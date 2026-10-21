//! The editor's top-level X11 window (the plugin embeds into it with XEmbed)
//! and the run loop plugins expect from a Linux host.
use std::cell::Cell;
use std::ffi::c_void;
use std::rc::Rc;
use std::time::{Duration, Instant};

use vst3::ComRef;
use vst3::Steinberg::Linux::{
    FileDescriptor, IEventHandler, IEventHandlerTrait, IRunLoopTrait, ITimerHandler,
    ITimerHandlerTrait, TimerInterval,
};
use vst3::Steinberg::{FIDString, kInvalidArgument, kPlatformTypeX11EmbedWindowID, tresult};
use x11rb::connection::Connection;
use x11rb::properties::WmSizeHints;
use x11rb::protocol::Event;
use x11rb::protocol::xproto::{
    AtomEnum, ClientMessageEvent, ConfigureWindowAux, ConnectionExt, CreateWindowAux, EventMask,
    PropMode, StackMode, WindowClass,
};
use x11rb::rust_connection::RustConnection;
use x11rb::wrapper::ConnectionExt as _;
use x11rb::{COPY_DEPTH_FROM_PARENT, COPY_FROM_PARENT};

use super::run_loop::RunLoop;
use super::{Frame, Resizer};
use crate::Waker;

pub(super) fn check_thread() -> Result<(), String> {
    Ok(())
}

pub(super) const VIEW_TYPE: FIDString = kPlatformTypeX11EmbedWindowID;

pub(super) struct Window {
    conn: Rc<RustConnection>,
    id: u32,
    delete: u32,
    resizable: bool,
    size: Rc<Cell<(i32, i32)>>,
    closed: bool,
}

fn clamp(v: i32) -> u16 {
    v.clamp(1, i32::from(u16::MAX)) as u16
}

// Without hints a window manager would let the user resize a fixed-size editor.
fn pin_size(conn: &RustConnection, id: u32, (width, height): (i32, i32)) {
    let mut hints = WmSizeHints::new();
    hints.min_size = Some((width, height));
    hints.max_size = Some((width, height));
    let _ = hints.set_normal_hints(conn, id);
}

impl Window {
    pub fn new(
        title: &str,
        size: (i32, i32),
        resizable: bool,
        _waker: Waker,
    ) -> Result<Self, String> {
        let fail = |e: &dyn std::fmt::Display| format!("X11: {e}");
        let (conn, screen_num) = x11rb::connect(None).map_err(|e| fail(&e))?;
        let screen = &conn.setup().roots[screen_num];
        let id = conn.generate_id().map_err(|e| fail(&e))?;
        let aux = CreateWindowAux::new()
            .background_pixel(screen.black_pixel)
            .event_mask(EventMask::STRUCTURE_NOTIFY);
        conn.create_window(
            COPY_DEPTH_FROM_PARENT,
            id,
            screen.root,
            0,
            0,
            clamp(size.0),
            clamp(size.1),
            0,
            WindowClass::INPUT_OUTPUT,
            COPY_FROM_PARENT,
            &aux,
        )
        .map_err(|e| fail(&e))?;
        let atom = |name: &str| -> Result<u32, String> {
            let cookie = conn
                .intern_atom(false, name.as_bytes())
                .map_err(|e| fail(&e))?;
            Ok(cookie.reply().map_err(|e| fail(&e))?.atom)
        };
        let (protocols, delete) = (atom("WM_PROTOCOLS")?, atom("WM_DELETE_WINDOW")?);
        let (net_name, utf8) = (atom("_NET_WM_NAME")?, atom("UTF8_STRING")?);
        let _ = conn.change_property8(
            PropMode::REPLACE,
            id,
            AtomEnum::WM_NAME,
            AtomEnum::STRING,
            title.as_bytes(),
        );
        let _ = conn.change_property8(PropMode::REPLACE, id, net_name, utf8, title.as_bytes());
        let _ = conn.change_property32(PropMode::REPLACE, id, protocols, AtomEnum::ATOM, &[delete]);
        if !resizable {
            pin_size(&conn, id, size);
        }
        conn.map_window(id).map_err(|e| fail(&e))?;
        // The plugin creates its child on another connection: the window must exist by then.
        conn.get_input_focus()
            .map_err(|e| fail(&e))?
            .reply()
            .map_err(|e| fail(&e))?;
        Ok(Self {
            conn: Rc::new(conn),
            id,
            delete,
            resizable,
            size: Rc::new(Cell::new(size)),
            closed: false,
        })
    }

    /// The X connection's socket, so window events wake the app.
    pub fn fd(&self) -> std::os::fd::RawFd {
        use std::os::fd::AsRawFd;
        self.conn.stream().as_raw_fd()
    }

    /// XEmbed takes the window id as the parent pointer.
    pub fn parent(&self) -> *mut c_void {
        self.id as usize as *mut c_void
    }

    pub fn resizer(&self) -> Resizer {
        let (conn, id, size, resizable) = (
            self.conn.clone(),
            self.id,
            self.size.clone(),
            self.resizable,
        );
        Rc::new(move |width, height| {
            size.set((width, height));
            if !resizable {
                pin_size(&conn, id, (width, height));
            }
            let aux = ConfigureWindowAux::new()
                .width(u32::from(clamp(width)))
                .height(u32::from(clamp(height)));
            let _ = conn.configure_window(id, &aux);
            let _ = conn.flush();
        })
    }

    /// The new size when the user resized the window.
    pub fn idle(&mut self) -> Option<(i32, i32)> {
        let mut resized = None;
        loop {
            match self.conn.poll_for_event() {
                Ok(Some(Event::ClientMessage(m)))
                    if m.window == self.id
                        && m.format == 32
                        && m.data.as_data32()[0] == self.delete =>
                {
                    self.closed = true
                }
                Ok(Some(Event::DestroyNotify(e))) if e.window == self.id => self.closed = true,
                Ok(Some(Event::ConfigureNotify(e))) if e.window == self.id => {
                    let now = (i32::from(e.width), i32::from(e.height));
                    if now != self.size.replace(now) {
                        resized = Some(now);
                    }
                }
                Ok(Some(_)) => {}
                Ok(None) => break,
                Err(_) => {
                    self.closed = true;
                    break;
                }
            }
        }
        resized
    }

    pub fn is_open(&self) -> bool {
        !self.closed
    }

    pub fn raise(&self) {
        // Mapping again de-iconifies; the message asks the WM for focus too.
        let _ = self.conn.map_window(self.id);
        let _ = self.conn.configure_window(
            self.id,
            &ConfigureWindowAux::new().stack_mode(StackMode::ABOVE),
        );
        let root = self.conn.setup().roots.first().map(|s| s.root);
        let active = self
            .conn
            .intern_atom(false, b"_NET_ACTIVE_WINDOW")
            .ok()
            .and_then(|c| c.reply().ok());
        if let (Some(root), Some(active)) = (root, active) {
            // Source 2: a pager, so focus-stealing prevention lets a user click through.
            let event = ClientMessageEvent::new(32, self.id, active.atom, [2, 0, 0, 0, 0]);
            let mask = EventMask::SUBSTRUCTURE_REDIRECT | EventMask::SUBSTRUCTURE_NOTIFY;
            let _ = self.conn.send_event(false, root, mask, event);
        }
        let _ = self.conn.flush();
    }
}

impl Drop for Window {
    fn drop(&mut self) {
        let _ = self.conn.destroy_window(self.id);
        let _ = self.conn.flush();
    }
}

/// Keeps `Instant` arithmetic far from overflow.
const MAX_TIMER: Duration = Duration::from_secs(3600);

impl RunLoop {
    /// Calls back what is ready or due; the handlers may register more meanwhile.
    pub fn pump(&self) {
        let handlers = self.handlers();
        let mut polls: Vec<libc::pollfd> = handlers
            .iter()
            .map(|(_, fd)| libc::pollfd {
                fd: *fd,
                events: libc::POLLIN,
                revents: 0,
            })
            .collect();
        // SAFETY: `polls` is a valid array of the length passed; a zero timeout never blocks.
        if !polls.is_empty() && unsafe { libc::poll(polls.as_mut_ptr(), polls.len() as _, 0) } > 0 {
            for (poll, (handler, fd)) in polls.iter().zip(&handlers) {
                if poll.revents != 0 && self.has_handler(handler) {
                    // SAFETY: a live handler the plugin registered.
                    unsafe { handler.onFDIsSet(*fd) };
                }
            }
        }
        for handler in self.take_due(Instant::now()) {
            if self.has_timer(&handler) {
                // SAFETY: a live handler the plugin registered.
                unsafe { handler.onTimer() };
            }
        }
    }
}

impl IRunLoopTrait for Frame {
    unsafe fn registerEventHandler(
        &self,
        handler: *mut IEventHandler,
        fd: FileDescriptor,
    ) -> tresult {
        // SAFETY: a live handler from the plugin; the clone takes our own reference.
        let Some(handler) = (unsafe { ComRef::from_raw(handler) }) else {
            return kInvalidArgument;
        };
        self.run_loop.add_handler(handler.to_com_ptr(), fd)
    }

    unsafe fn unregisterEventHandler(&self, handler: *mut IEventHandler) -> tresult {
        self.run_loop.remove_handler(handler)
    }

    unsafe fn registerTimer(
        &self,
        handler: *mut ITimerHandler,
        milliseconds: TimerInterval,
    ) -> tresult {
        // SAFETY: as in registerEventHandler.
        let Some(handler) = (unsafe { ComRef::from_raw(handler) }) else {
            return kInvalidArgument;
        };
        let interval =
            Duration::from_millis(milliseconds).clamp(Duration::from_millis(1), MAX_TIMER);
        self.run_loop.add_timer(handler.to_com_ptr(), interval)
    }

    unsafe fn unregisterTimer(&self, handler: *mut ITimerHandler) -> tresult {
        self.run_loop.remove_timer(handler)
    }
}
