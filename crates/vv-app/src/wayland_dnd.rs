//! Dropping files from the file manager on Wayland, where winit 0.30 does not
//! handle it (X11 only), and reading images from the clipboard, which
//! egui-winit reads only as text. Native panel drags also use pointer focus
//! and surface-local coordinates after the implicit grab ends. A thread with
//! its own queue on winit's connection, like smithay-clipboard does; files reach egui in
//! `raw_input_hook`.

use std::ffi::OsString;
use std::io::Read;
use std::os::unix::ffi::OsStringExt;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use raw_window_handle::{HasDisplayHandle, RawDisplayHandle};
use sctk::data_device_manager::data_device::{DataDevice, DataDeviceHandler};
use sctk::data_device_manager::data_offer::{DataOfferHandler, DragOffer, SelectionOffer};
use sctk::data_device_manager::data_source::DataSourceHandler;
use sctk::data_device_manager::{DataDeviceManagerState, ReadPipe, WritePipe};
use sctk::reexports::client::globals::registry_queue_init;
use sctk::reexports::client::protocol::wl_callback::{self, WlCallback};
use sctk::reexports::client::protocol::wl_data_device::WlDataDevice;
use sctk::reexports::client::protocol::wl_data_device_manager::DndAction;
use sctk::reexports::client::protocol::wl_data_source::WlDataSource;
use sctk::reexports::client::protocol::wl_pointer::WlPointer;
use sctk::reexports::client::protocol::wl_seat::WlSeat;
use sctk::reexports::client::protocol::wl_surface::WlSurface;
use sctk::reexports::client::{Connection, Dispatch, Proxy, QueueHandle, backend::Backend};
use sctk::registry::{ProvidesRegistryState, RegistryState};
use sctk::seat::pointer::{BTN_LEFT, PointerEvent, PointerEventKind, PointerHandler};
use sctk::seat::{Capability, SeatHandler, SeatState};
use sctk::{
    delegate_data_device, delegate_pointer, delegate_registry, delegate_seat, registry_handlers,
};

use crate::paste_image::IMAGE_MIME_TYPES;

const URI_LIST: &str = "text/uri-list";

#[derive(Clone, Copy)]
struct SurfacePoint {
    surface: u32,
    point: egui::Pos2,
}

#[derive(Default)]
struct PointerState {
    generation: u64,
    owner: Option<u32>,
    origin: Option<u32>,
    focus: Option<SurfacePoint>,
    held: bool,
    cancelled: bool,
    binding: Option<(u64, u64)>,
    released: Option<Option<SurfacePoint>>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum PointerDrop {
    Pending,
    Cancelled,
    Dropped(egui::Pos2),
}

impl PointerState {
    fn press(&mut self, pointer: u32, position: SurfacePoint) {
        self.generation = self.generation.wrapping_add(1);
        self.owner = Some(pointer);
        self.origin = Some(position.surface);
        self.focus = Some(position);
        self.held = true;
        self.cancelled = false;
        self.released = None;
    }

    fn enter(&mut self, pointer: u32, position: SurfacePoint) {
        if self.owner == Some(pointer) {
            self.focus = Some(position);
        }
    }

    fn leave(&mut self, pointer: u32, surface: u32) {
        if self.owner == Some(pointer) && self.focus.is_some_and(|p| p.surface == surface) {
            self.focus = None;
            if self.released.is_some() {
                self.released = Some(None);
            }
        }
    }

    fn release(&mut self, pointer: u32) -> Option<u64> {
        if self.owner != Some(pointer) {
            return None;
        }
        self.held = false;
        Some(self.generation)
    }

    fn finish_release(&mut self, generation: u64) {
        if generation == self.generation && !self.held {
            self.released = Some(self.focus.filter(|p| Some(p.surface) != self.origin));
        }
    }

    fn bind(&mut self, token: u64) {
        self.binding = Some((token, self.generation));
    }

    fn cancel(&mut self) {
        self.cancelled = true;
        self.held = false;
        self.released = Some(None);
    }

    fn drop_position(&self, token: u64) -> PointerDrop {
        if self.cancelled {
            return PointerDrop::Cancelled;
        }
        let Some((bound, generation)) = self.binding else {
            return PointerDrop::Pending;
        };
        if bound != token {
            return PointerDrop::Pending;
        }
        if generation == 0 || generation != self.generation {
            return PointerDrop::Cancelled;
        }
        match self.released {
            None => PointerDrop::Pending,
            Some(Some(position)) if self.focus.is_some_and(|p| p.surface == position.surface) => {
                PointerDrop::Dropped(position.point)
            }
            Some(_) => PointerDrop::Cancelled,
        }
    }
}

#[derive(Clone)]
pub(crate) struct PointerBridge {
    state: Arc<Mutex<PointerState>>,
    bind: Arc<dyn Fn(u64) + Send + Sync>,
    token: Arc<AtomicU64>,
}

impl PointerBridge {
    pub(crate) fn begin_drag(&self) -> u64 {
        let token = self.token.fetch_add(1, Ordering::Relaxed);
        (self.bind)(token);
        token
    }

    pub(crate) fn drop_position(&self, token: u64) -> PointerDrop {
        self.state.lock().unwrap().drop_position(token)
    }
}

enum PointerSync {
    Bind(u64),
    Release(u64),
}

#[derive(Default)]
struct Shared {
    hovering: bool,
    /// Where the drag is, in surface coordinates: egui gets no pointer
    /// events during it, and the drop target depends on it.
    position: Option<(f64, f64)>,
    dropped: Vec<PathBuf>,
    selection: Option<SelectionOffer>,
}

pub struct WaylandDnd {
    shared: Arc<Mutex<Shared>>,
    conn: Connection,
    pointer: PointerBridge,
}

impl WaylandDnd {
    /// `None` outside Wayland or if the compositor does not offer DnD.
    pub fn start(cc: &eframe::CreationContext<'_>) -> Option<Self> {
        let RawDisplayHandle::Wayland(handle) = cc.display_handle().ok()?.as_raw() else {
            return None;
        };
        // The display stays valid for the whole process lifetime: it is winit's.
        let backend = unsafe { Backend::from_foreign_display(handle.display.as_ptr().cast()) };
        let conn = Connection::from_backend(backend);
        let (globals, mut queue) = registry_queue_init::<State>(&conn).ok()?;
        let qh = queue.handle();
        let shared = Arc::new(Mutex::new(Shared::default()));
        let pointer_state = Arc::new(Mutex::new(PointerState::default()));
        let bind_conn = conn.clone();
        let bind_queue = qh.clone();
        let pointer = PointerBridge {
            state: pointer_state.clone(),
            bind: Arc::new(move |token| {
                bind_conn
                    .display()
                    .sync(&bind_queue, PointerSync::Bind(token));
                let _ = bind_conn.flush();
            }),
            token: Arc::new(AtomicU64::new(1)),
        };
        let mut state = State {
            registry: RegistryState::new(&globals),
            seats: SeatState::new(&globals, &qh),
            manager: DataDeviceManagerState::bind(&globals, &qh).ok()?,
            devices: Vec::new(),
            pointers: Vec::new(),
            pointer_state,
            conn: conn.clone(),
            shared: shared.clone(),
            ctx: cc.egui_ctx.clone(),
        };
        for seat in state.seats.seats() {
            state.add_device(&qh, &seat);
        }
        std::thread::Builder::new()
            .name("wayland-dnd".into())
            .spawn(move || {
                loop {
                    if let Err(e) = queue.blocking_dispatch(&mut state) {
                        state.pointer_state.lock().unwrap().cancel();
                        state.ctx.request_repaint();
                        eprintln!("wayland-dnd: {e}");
                        return;
                    }
                }
            })
            .ok()?;
        Some(Self {
            shared,
            conn,
            pointer,
        })
    }

    pub(crate) fn pointer_bridge(&self) -> PointerBridge {
        self.pointer.clone()
    }

    /// The pipe the clipboard's image arrives from, and its extension;
    /// `None` if the clipboard holds no image.
    pub fn clipboard_image(&self) -> Option<(ReadPipe, &'static str)> {
        let offer = self.shared.lock().unwrap().selection.clone()?;
        let (mime, extension) = offer.with_mime_types(|mimes| {
            IMAGE_MIME_TYPES
                .into_iter()
                .find(|(mime, _)| mimes.iter().any(|m| m == mime))
        })?;
        let pipe = offer.receive(mime.to_string()).ok()?;
        let _ = self.conn.flush();
        Some((pipe, extension))
    }

    /// `zoom`: egui's zoom factor, between surface coordinates and points.
    pub fn feed(&self, raw_input: &mut egui::RawInput, zoom: f32) {
        let mut shared = self.shared.lock().unwrap();
        if shared.hovering {
            raw_input.hovered_files.push(egui::HoveredFile::default());
        }
        if let Some((x, y)) = shared.position.take() {
            raw_input.events.push(egui::Event::PointerMoved(
                egui::pos2(x as f32, y as f32) / zoom,
            ));
        }
        raw_input.dropped_files.extend(
            shared.dropped.drain(..).map(|path| {
                Arc::new(DroppedPath(path)) as Arc<dyn egui::DroppedFile + Send + Sync>
            }),
        );
    }
}

#[derive(Debug)]
struct DroppedPath(PathBuf);

impl egui::DroppedFile for DroppedPath {
    fn path(&self) -> &std::path::Path {
        &self.0
    }
    fn bytes(&self) -> Result<Vec<u8>, String> {
        std::fs::read(&self.0).map_err(|e| e.to_string())
    }
}

struct State {
    registry: RegistryState,
    seats: SeatState,
    manager: DataDeviceManagerState,
    devices: Vec<DataDevice>,
    pointers: Vec<(WlSeat, WlPointer)>,
    pointer_state: Arc<Mutex<PointerState>>,
    conn: Connection,
    shared: Arc<Mutex<Shared>>,
    ctx: egui::Context,
}

impl State {
    fn add_device(&mut self, qh: &QueueHandle<Self>, seat: &WlSeat) {
        self.devices.push(self.manager.get_data_device(qh, seat));
        self.add_pointer(qh, seat);
    }

    fn add_pointer(&mut self, qh: &QueueHandle<Self>, seat: &WlSeat) {
        if !self.pointers.iter().any(|(s, _)| s == seat)
            && let Ok(pointer) = self.seats.get_pointer(qh, seat)
        {
            self.pointers.push((seat.clone(), pointer));
        }
    }

    fn remove_pointer(&mut self, seat: &WlSeat) {
        self.pointers.retain(|(s, pointer)| {
            if s != seat {
                return true;
            }
            let mut state = self.pointer_state.lock().unwrap();
            if state.owner == Some(pointer.id().protocol_id()) {
                state.cancel();
            }
            if pointer.version() >= 3 {
                pointer.release();
            }
            false
        });
        self.ctx.request_repaint();
    }

    fn drag_offer(&self, device: &WlDataDevice) -> Option<DragOffer> {
        self.devices
            .iter()
            .find(|d| d.inner() == device)?
            .data()
            .drag_offer()
    }

    fn set_hovering(&self, hovering: bool) {
        self.shared.lock().unwrap().hovering = hovering;
        self.ctx.request_repaint();
    }
}

impl PointerHandler for State {
    fn pointer_frame(
        &mut self,
        conn: &Connection,
        qh: &QueueHandle<Self>,
        pointer: &WlPointer,
        events: &[PointerEvent],
    ) {
        let id = pointer.id().protocol_id();
        let mut state = self.pointer_state.lock().unwrap();
        for event in events {
            let position = SurfacePoint {
                surface: event.surface.id().protocol_id(),
                point: egui::pos2(event.position.0 as f32, event.position.1 as f32),
            };
            match event.kind {
                PointerEventKind::Press {
                    button: BTN_LEFT, ..
                } => state.press(id, position),
                PointerEventKind::Enter { .. } | PointerEventKind::Motion { .. } => {
                    state.enter(id, position);
                }
                PointerEventKind::Leave { .. } => state.leave(id, position.surface),
                PointerEventKind::Release {
                    button: BTN_LEFT, ..
                } => {
                    if let Some(generation) = state.release(id) {
                        // The implicit grab ends before the compositor sends the destination enter.
                        conn.display().sync(qh, PointerSync::Release(generation));
                    }
                }
                _ => {}
            }
        }
        let _ = conn.flush();
        self.ctx.request_repaint();
    }
}

impl Dispatch<WlCallback, PointerSync> for State {
    fn event(
        state: &mut Self,
        _: &WlCallback,
        _: wl_callback::Event,
        action: &PointerSync,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let mut pointer = state.pointer_state.lock().unwrap();
        match action {
            PointerSync::Bind(token) => pointer.bind(*token),
            PointerSync::Release(generation) => pointer.finish_release(*generation),
        }
        state.ctx.request_repaint();
    }
}

impl DataDeviceHandler for State {
    fn enter(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        device: &WlDataDevice,
        x: f64,
        y: f64,
        _: &WlSurface,
    ) {
        self.shared.lock().unwrap().position = Some((x, y));
        let Some(offer) = self.drag_offer(device) else {
            return;
        };
        let has_files = offer.with_mime_types(|mimes| mimes.iter().any(|m| m == URI_LIST));
        offer.accept_mime_type(offer.serial, has_files.then(|| URI_LIST.to_string()));
        if has_files {
            offer.set_actions(DndAction::Copy, DndAction::Copy);
            self.set_hovering(true);
        }
    }

    fn leave(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataDevice) {
        self.set_hovering(false);
    }

    fn motion(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataDevice, x: f64, y: f64) {
        self.shared.lock().unwrap().position = Some((x, y));
        self.ctx.request_repaint();
    }

    fn selection(&mut self, _: &Connection, _: &QueueHandle<Self>, device: &WlDataDevice) {
        self.shared.lock().unwrap().selection = self
            .devices
            .iter()
            .find(|d| d.inner() == device)
            .and_then(|d| d.data().selection_offer());
    }

    fn drop_performed(&mut self, _: &Connection, _: &QueueHandle<Self>, device: &WlDataDevice) {
        self.set_hovering(false);
        let Some(offer) = self.drag_offer(device) else {
            return;
        };
        let Ok(mut pipe) = offer.receive(URI_LIST.to_string()) else {
            offer.destroy();
            return;
        };
        let _ = self.conn.flush();
        // The read waits for the source client: it must not block the queue.
        let conn = self.conn.clone();
        let shared = self.shared.clone();
        let ctx = self.ctx.clone();
        std::thread::spawn(move || {
            let mut uri_list = Vec::new();
            let _ = pipe.read_to_end(&mut uri_list);
            offer.finish();
            offer.destroy();
            let _ = conn.flush();
            shared
                .lock()
                .unwrap()
                .dropped
                .extend(parse_uri_list(&uri_list));
            ctx.request_repaint();
        });
    }
}

impl DataOfferHandler for State {
    fn source_actions(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        offer: &mut DragOffer,
        _: DndAction,
    ) {
        offer.set_actions(DndAction::Copy, DndAction::Copy);
    }

    fn selected_action(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &mut DragOffer,
        _: DndAction,
    ) {
    }
}

impl DataSourceHandler for State {
    fn accept_mime(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &WlDataSource,
        _: Option<String>,
    ) {
    }
    fn send_request(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &WlDataSource,
        _: String,
        _: WritePipe,
    ) {
    }
    fn cancelled(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataSource) {}
    fn dnd_dropped(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataSource) {}
    fn dnd_finished(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataSource) {}
    fn action(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataSource, _: DndAction) {}
}

impl SeatHandler for State {
    fn seat_state(&mut self) -> &mut SeatState {
        &mut self.seats
    }
    fn new_seat(&mut self, _: &Connection, qh: &QueueHandle<Self>, seat: WlSeat) {
        self.add_device(qh, &seat);
    }
    fn new_capability(
        &mut self,
        _: &Connection,
        qh: &QueueHandle<Self>,
        seat: WlSeat,
        capability: Capability,
    ) {
        if capability == Capability::Pointer {
            self.add_pointer(qh, &seat);
        }
    }
    fn remove_capability(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        seat: WlSeat,
        capability: Capability,
    ) {
        if capability == Capability::Pointer {
            self.remove_pointer(&seat);
        }
    }
    fn remove_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, seat: WlSeat) {
        self.devices.retain(|d| d.data().seat() != &seat);
        self.remove_pointer(&seat);
    }
}

impl ProvidesRegistryState for State {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registry
    }
    registry_handlers![SeatState];
}

delegate_data_device!(State);
delegate_pointer!(State);
delegate_seat!(State);
delegate_registry!(State);

/// Local paths from a `text/uri-list` (RFC 2483): comments and non-`file:`
/// URIs are discarded.
fn parse_uri_list(bytes: &[u8]) -> Vec<PathBuf> {
    bytes
        .split(|&b| b == b'\n')
        .map(|line| line.strip_suffix(b"\r").unwrap_or(line))
        .filter(|line| !line.starts_with(b"#"))
        .filter_map(|line| line.strip_prefix(b"file://"))
        .filter_map(|rest| {
            // After `file://` comes the host, usually empty or `localhost`.
            let path = &rest[rest.iter().position(|&b| b == b'/')?..];
            Some(PathBuf::from(OsString::from_vec(percent_decode(path))))
        })
        .collect()
}

fn percent_decode(bytes: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let hex = bytes
            .get(i + 1..i + 3)
            .and_then(|h| std::str::from_utf8(h).ok())
            .and_then(|h| u8::from_str_radix(h, 16).ok());
        match (bytes[i], hex) {
            (b'%', Some(byte)) => {
                out.push(byte);
                i += 3;
            }
            (b, _) => {
                out.push(b);
                i += 1;
            }
        }
    }
    out
}

#[cfg(test)]
#[path = "tests/wayland_dnd.rs"]
mod tests;
