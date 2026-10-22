//! A plugin's own GUI in a native window. UI thread only: the plugin's view
//! and the window system both require it.
use std::cell::{Cell, RefCell};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::rc::Rc;
use std::sync::Arc;

use vst3::Steinberg::Vst::{IEditControllerTrait, ViewType};
use vst3::Steinberg::{
    IPlugFrame, IPlugFrameTrait, IPlugView, IPlugViewTrait, ViewRect, kInternalError,
    kInvalidArgument, kResultFalse, kResultOk, kResultTrue, tresult,
};
use vst3::{Class, ComPtr, ComRef, ComWrapper};

use crate::{HostedPlugin, Vst3Instance, Waker};

#[cfg(unix)]
mod fd_watcher;
#[cfg(unix)]
mod run_loop;

#[cfg(target_os = "macos")]
#[path = "editor/macos.rs"]
mod platform;
#[cfg(target_os = "linux")]
#[path = "editor/x11.rs"]
mod platform;
#[cfg(not(any(target_os = "macos", target_os = "linux")))]
mod platform {
    use std::ffi::c_void;
    use std::rc::Rc;

    use vst3::Steinberg::FIDString;

    use super::Resizer;
    use crate::Waker;

    pub(super) const VIEW_TYPE: FIDString = std::ptr::null();

    const UNSUPPORTED: &str = "plugin editors are not supported on this platform";

    pub(super) fn check_thread() -> Result<(), String> {
        Err(UNSUPPORTED.into())
    }

    pub(super) struct Window;

    impl Window {
        pub fn new(_: &str, _: (i32, i32), _: bool, _: Waker) -> Result<Self, String> {
            Err(UNSUPPORTED.into())
        }

        pub fn parent(&self) -> *mut c_void {
            std::ptr::null_mut()
        }

        pub fn resizer(&self) -> Resizer {
            Rc::new(|_, _| {})
        }

        pub fn idle(&mut self) -> Option<(i32, i32)> {
            None
        }

        pub fn raise(&self) {}

        pub fn is_open(&self) -> bool {
            false
        }
    }
}

const METER_POLL: std::time::Duration = std::time::Duration::from_millis(33);

type Resizer = Rc<dyn Fn(i32, i32)>;

/// What the plugin calls back on: resizes the window, then tells the view.
struct Frame {
    resize: RefCell<Option<Resizer>>,
    resizing: Cell<bool>,
    #[cfg(target_os = "linux")]
    run_loop: run_loop::RunLoop,
}

impl Class for Frame {
    #[cfg(target_os = "linux")]
    type Interfaces = (IPlugFrame, vst3::Steinberg::Linux::IRunLoop);
    #[cfg(not(target_os = "linux"))]
    type Interfaces = (IPlugFrame,);
}

impl IPlugFrameTrait for Frame {
    unsafe fn resizeView(&self, view: *mut IPlugView, new_size: *mut ViewRect) -> tresult {
        if view.is_null() || new_size.is_null() {
            return kInvalidArgument;
        }
        // A resize caused by our own onSize must not start another.
        if self.resizing.replace(true) {
            return kResultFalse;
        }
        let res = catch_unwind(AssertUnwindSafe(|| {
            // SAFETY: non-null, and the plugin owns the rect for this call.
            let rect = unsafe { &mut *new_size };
            let resize = self.resize.borrow().clone();
            if let Some(resize) = resize {
                resize(rect.right - rect.left, rect.bottom - rect.top);
            }
            // SAFETY: the plugin passes its own live view.
            if let Some(view) = unsafe { ComRef::from_raw(view) } {
                unsafe { view.onSize(rect) };
            }
            kResultOk
        }))
        .unwrap_or(kInternalError);
        self.resizing.set(false);
        res
    }
}

pub struct Editor {
    // Order matters: the view goes before the window it lives in, and the
    // instance (the plugin's code) last.
    view: ComPtr<IPlugView>,
    frame: ComWrapper<Frame>,
    window: platform::Window,
    resize: Resizer,
    // Only Linux window managers refuse a size.
    #[cfg(target_os = "linux")]
    requested: Option<(i32, i32)>,
    #[cfg(target_os = "linux")]
    waker: Waker,
    instance: Arc<Vst3Instance>,
}

#[derive(Debug, PartialEq, Eq)]
struct ResizeDecision {
    request: Option<(i32, i32)>,
    notify: (i32, i32),
}

/// A window manager can keep `window` instead of `constrained`; asking again would loop, and
/// telling the plugin the rejected `window` can make it resize back. `requested` is the size
/// asked for last, only until the next report.
fn resize_decision(
    window: (i32, i32),
    constrained: (i32, i32),
    requested: &mut Option<(i32, i32)>,
) -> ResizeDecision {
    let repeat = requested.take() == Some(constrained);
    let request = (constrained != window && !repeat).then_some(constrained);
    *requested = request;
    ResizeDecision {
        request,
        notify: constrained,
    }
}

/// The window manager closed the window during `idle`, after the app last
/// looked for closed editors: ask for another look.
#[cfg(any(target_os = "linux", test))]
fn wake_if_closed(was_open: bool, is_open: bool, waker: &Waker) {
    if was_open && !is_open {
        waker();
    }
}

fn size_of(rect: &ViewRect) -> (i32, i32) {
    (rect.right - rect.left, rect.bottom - rect.top)
}

impl Editor {
    /// Err if the plugin has no editor or the platform view type is unsupported.
    /// `waker` is called, from any thread, when the editor needs `idle` or the
    /// app should look at the instance (an edit ended, the window changed).
    pub fn open(
        instance: &Arc<dyn HostedPlugin>,
        title: &str,
        waker: Waker,
    ) -> Result<Self, String> {
        platform::check_thread()?;
        let instance = instance
            .clone()
            .into_vst3()
            .ok_or("not a VST3 plugin instance")?;
        let controller = instance
            .controller()
            .ok_or("the plugin has no controller")?;
        // SAFETY: a live controller; the view comes back with one reference for us.
        let view = unsafe { controller.createView(ViewType::kEditor) };
        // SAFETY: owns the reference createView returned.
        let view = unsafe { ComPtr::from_raw(view) }.ok_or("the plugin has no editor")?;
        // SAFETY: calls on the live view.
        let (size, resizable) = unsafe {
            if view.isPlatformTypeSupported(platform::VIEW_TYPE) != kResultTrue {
                return Err("the plugin editor does not support this platform".into());
            }
            let mut rect = ViewRect {
                left: 0,
                top: 0,
                right: 0,
                bottom: 0,
            };
            if view.getSize(&mut rect) != kResultOk {
                return Err("the plugin editor reports no size".into());
            }
            (size_of(&rect), view.canResize() == kResultTrue)
        };
        let window = platform::Window::new(title, size, resizable, waker.clone())?;
        let resize = window.resizer();
        let frame = ComWrapper::new(Frame {
            resize: RefCell::new(Some(resize.clone())),
            resizing: Cell::new(false),
            #[cfg(target_os = "linux")]
            run_loop: run_loop::RunLoop::new(waker.clone(), Some(window.fd())),
        });
        let frame_ptr = frame.to_com_ptr::<IPlugFrame>().ok_or("plugin frame")?;
        // SAFETY: the frame outlives the view's use of it (cleared in Drop).
        unsafe { view.setFrame(frame_ptr.as_ptr()) };
        // SAFETY: the parent is a live native view/window of the platform's type.
        let attached = unsafe { view.attached(window.parent(), platform::VIEW_TYPE) };
        if attached != kResultOk {
            // SAFETY: detaches the frame set above; the view was never attached.
            unsafe { view.setFrame(std::ptr::null_mut()) };
            frame.resize.take();
            // A failed attach can leave handlers and timers that reference the frame.
            #[cfg(target_os = "linux")]
            frame.run_loop.clear();
            return Err(format!("the plugin editor did not attach ({attached})"));
        }
        instance.set_waker(Some(waker.clone()));
        Ok(Self {
            view,
            frame,
            window,
            resize,
            #[cfg(target_os = "linux")]
            requested: None,
            #[cfg(target_os = "linux")]
            waker,
            instance,
        })
    }

    pub fn instance(&self) -> &Arc<Vst3Instance> {
        &self.instance
    }

    /// Pumps the window and, on Linux, the plugin's registered fds and timers.
    pub fn idle(&mut self) {
        self.instance.forward_outputs();
        #[cfg(target_os = "linux")]
        self.frame.run_loop.pump();
        #[cfg(target_os = "linux")]
        let was_open = self.window.is_open();
        let resized = self.window.idle();
        #[cfg(target_os = "linux")]
        {
            // The app drops closed editors before it pumps them.
            wake_if_closed(was_open, self.window.is_open(), &self.waker);
            self.frame.run_loop.rearm();
        }
        if let Some((width, height)) = resized {
            let mut rect = ViewRect {
                left: 0,
                top: 0,
                right: width,
                bottom: height,
            };
            // SAFETY: the view is live until Drop.
            unsafe {
                self.view.checkSizeConstraint(&mut rect);
                #[cfg(target_os = "linux")]
                let requested = &mut self.requested;
                #[cfg(not(target_os = "linux"))]
                let requested = &mut None;
                let decision = resize_decision((width, height), size_of(&rect), requested);
                if let Some((w, h)) = decision.request {
                    (self.resize)(w, h);
                }
                rect.right = rect.left + decision.notify.0;
                rect.bottom = rect.top + decision.notify.1;
                self.view.onSize(&mut rect);
            }
        }
    }

    /// When `idle` is next needed besides the waker's calls; `None` to sleep.
    pub fn next_wake(&self) -> Option<std::time::Duration> {
        // Playback already repaints; this only keeps a meter moving when a plugin's output changes
        // while nothing else wakes the app.
        let meters = self.instance.outputs_fresh().then_some(METER_POLL);
        #[cfg(target_os = "linux")]
        return match (meters, self.frame.run_loop.next_wake()) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        };
        #[cfg(not(target_os = "linux"))]
        meters
    }

    pub fn is_open(&self) -> bool {
        self.window.is_open()
    }

    /// Shows the window again, in front and focused, even if covered or minimized.
    pub fn raise(&self) {
        self.window.raise();
    }
}

impl Drop for Editor {
    fn drop(&mut self) {
        // SAFETY: the view was attached in `open`; the frame is released first
        // so the plugin cannot call back into a closing window.
        unsafe {
            self.view.removed();
            self.view.setFrame(std::ptr::null_mut());
        }
        self.frame.resize.take();
        #[cfg(target_os = "linux")]
        self.frame.run_loop.clear();
        self.instance.set_waker(None);
    }
}

#[cfg(test)]
#[path = "tests/editor.rs"]
mod tests;
