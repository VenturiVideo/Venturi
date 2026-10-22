//! The editor's NSWindow, hosting the plugin's NSView.
use std::cell::Cell;
use std::ffi::c_void;
use std::rc::Rc;

use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2::{DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send};
use objc2_app_kit::{
    NSApplication, NSBackingStoreType, NSView, NSWindow, NSWindowDelegate, NSWindowStyleMask,
};
use objc2_foundation::{
    NSNotification, NSObject, NSObjectProtocol, NSPoint, NSRect, NSSize, NSString,
};
use vst3::Steinberg::{FIDString, kPlatformTypeNSView};

use super::Resizer;
use crate::Waker;

pub(super) const VIEW_TYPE: FIDString = kPlatformTypeNSView;

pub(super) fn check_thread() -> Result<(), String> {
    MainThreadMarker::new()
        .map(|_| ())
        .ok_or_else(|| "plugin editors open on the main thread".into())
}

struct State {
    closed: Cell<bool>,
    resized: Cell<bool>,
    waker: Waker,
}

define_class!(
    // Close and resize are events: the app sleeps between them instead of polling.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "VenturiEditorDelegate"]
    #[ivars = State]
    struct Delegate;

    unsafe impl NSObjectProtocol for Delegate {}

    unsafe impl NSWindowDelegate for Delegate {
        #[unsafe(method(windowWillClose:))]
        fn will_close(&self, _: &NSNotification) {
            self.ivars().closed.set(true);
            (self.ivars().waker)();
        }

        #[unsafe(method(windowDidResize:))]
        fn did_resize(&self, _: &NSNotification) {
            self.ivars().resized.set(true);
            (self.ivars().waker)();
        }
    }
);

impl Delegate {
    fn new(mtm: MainThreadMarker, waker: Waker) -> Retained<Self> {
        let this = mtm.alloc().set_ivars(State {
            closed: Cell::new(false),
            resized: Cell::new(false),
            waker,
        });
        // SAFETY: the superclass initialiser.
        unsafe { msg_send![super(this), init] }
    }
}

pub(super) struct Window {
    window: Retained<NSWindow>,
    content: Retained<NSView>,
    delegate: Retained<Delegate>,
    size: Rc<Cell<(i32, i32)>>,
}

impl Window {
    pub fn new(
        title: &str,
        size: (i32, i32),
        resizable: bool,
        waker: Waker,
    ) -> Result<Self, String> {
        let mtm = MainThreadMarker::new().ok_or("plugin editors open on the main thread")?;
        // Makes sure AppKit is initialised before a window is made.
        let _ = NSApplication::sharedApplication(mtm);
        let mut style = NSWindowStyleMask::Titled | NSWindowStyleMask::Closable;
        if resizable {
            style |= NSWindowStyleMask::Resizable;
        }
        let rect = NSRect::new(
            NSPoint::new(0.0, 0.0),
            NSSize::new(f64::from(size.0), f64::from(size.1)),
        );
        // SAFETY: a valid rect and style, on the main thread.
        let window = unsafe {
            NSWindow::initWithContentRect_styleMask_backing_defer(
                mtm.alloc(),
                rect,
                style,
                NSBackingStoreType::Buffered,
                false,
            )
        };
        // The Retained is the only owner: AppKit must not free it on close.
        // SAFETY: a plain property setter.
        unsafe { window.setReleasedWhenClosed(false) };
        window.setTitle(&NSString::from_str(title));
        let content = NSView::initWithFrame(mtm.alloc(), rect);
        window.setContentView(Some(&content));
        let delegate = Delegate::new(mtm, waker);
        window.setDelegate(Some(ProtocolObject::from_ref(&*delegate)));
        window.center();
        window.makeKeyAndOrderFront(None);
        Ok(Self {
            window,
            content,
            delegate,
            size: Rc::new(Cell::new(size)),
        })
    }

    pub fn parent(&self) -> *mut c_void {
        Retained::as_ptr(&self.content).cast_mut().cast()
    }

    pub fn resizer(&self) -> Resizer {
        let window = self.window.clone();
        let size = self.size.clone();
        Rc::new(move |width, height| {
            size.set((width, height));
            window.setContentSize(NSSize::new(f64::from(width), f64::from(height)));
        })
    }

    /// The new size when the user resized the window.
    pub fn idle(&mut self) -> Option<(i32, i32)> {
        if !self.delegate.ivars().resized.take() {
            return None;
        }
        let frame = self.content.frame().size;
        let now = (frame.width.round() as i32, frame.height.round() as i32);
        (now != self.size.replace(now)).then_some(now)
    }

    pub fn raise(&self) {
        if self.window.isMiniaturized() {
            self.window.deminiaturize(None);
        }
        self.window.makeKeyAndOrderFront(None);
    }

    // Hiding the app or minimizing is not closing.
    pub fn is_open(&self) -> bool {
        !self.delegate.ivars().closed.get()
    }
}

impl Drop for Window {
    fn drop(&mut self) {
        self.window.setDelegate(None);
        self.window.close();
    }
}
