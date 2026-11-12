//! The editor's Win32 window, parent of the plugin's child HWND.
use std::cell::Cell;
use std::ffi::c_void;
use std::rc::Rc;
use std::sync::Once;

use vst3::Steinberg::{FIDString, kPlatformTypeHWND};
use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    AdjustWindowRectEx, CW_USEDEFAULT, CreateWindowExW, DefWindowProcW, DestroyWindow,
    GWLP_USERDATA, GetClientRect, GetWindowLongPtrW, IDC_ARROW, IsIconic, LoadCursorW,
    RegisterClassExW, SW_HIDE, SW_RESTORE, SW_SHOW, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOZORDER,
    SetForegroundWindow, SetWindowLongPtrW, SetWindowPos, ShowWindow, WINDOW_STYLE, WM_CLOSE,
    WM_SIZE, WNDCLASSEXW, WS_CAPTION, WS_CLIPCHILDREN, WS_MAXIMIZEBOX, WS_MINIMIZEBOX,
    WS_OVERLAPPED, WS_SYSMENU, WS_THICKFRAME,
};

use super::Resizer;
use crate::Waker;

pub(super) const VIEW_TYPE: FIDString = kPlatformTypeHWND;

// The window belongs to the thread that makes it, whose event loop dispatches
// its messages: there is no single UI thread to insist on.
pub(super) fn check_thread() -> Result<(), String> {
    Ok(())
}

struct State {
    closed: Cell<bool>,
    resized: Cell<bool>,
    waker: Waker,
}

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

const CLASS_NAME: &str = "VenturiPluginEditor";

unsafe extern "system" fn wnd_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    // SAFETY: null, or the `State` its `Window` keeps alive until it clears this slot.
    let state = unsafe { (GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *const State).as_ref() };
    match (msg, state) {
        (WM_CLOSE, Some(state)) => {
            // `Window::drop` destroys it, after the plugin's view is removed.
            unsafe { ShowWindow(hwnd, SW_HIDE) };
            state.closed.set(true);
            (state.waker)();
            0
        }
        (WM_SIZE, Some(state)) => {
            state.resized.set(true);
            (state.waker)();
            0
        }
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

fn register_class() {
    static REGISTER: Once = Once::new();
    REGISTER.call_once(|| {
        let name = wide(CLASS_NAME);
        // SAFETY: a fully initialised class whose strings outlive the call.
        unsafe {
            let class = WNDCLASSEXW {
                cbSize: size_of::<WNDCLASSEXW>() as u32,
                lpfnWndProc: Some(wnd_proc),
                hInstance: GetModuleHandleW(std::ptr::null()),
                hCursor: LoadCursorW(std::ptr::null_mut(), IDC_ARROW),
                lpszClassName: name.as_ptr(),
                ..std::mem::zeroed()
            };
            RegisterClassExW(&class);
        }
    });
}

/// The window size whose client area is `client`.
fn outer_size(style: WINDOW_STYLE, client: (i32, i32)) -> (i32, i32) {
    let mut rect = RECT {
        left: 0,
        top: 0,
        right: client.0,
        bottom: client.1,
    };
    // SAFETY: a valid rect, left as it is on failure.
    unsafe { AdjustWindowRectEx(&mut rect, style, 0, 0) };
    (rect.right - rect.left, rect.bottom - rect.top)
}

pub(super) struct Window {
    hwnd: HWND,
    style: WINDOW_STYLE,
    state: Rc<State>,
    size: Rc<Cell<(i32, i32)>>,
}

impl Window {
    pub fn new(
        title: &str,
        size: (i32, i32),
        resizable: bool,
        waker: Waker,
    ) -> Result<Self, String> {
        register_class();
        let mut style = WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU | WS_MINIMIZEBOX | WS_CLIPCHILDREN;
        if resizable {
            style |= WS_THICKFRAME | WS_MAXIMIZEBOX;
        }
        let (width, height) = outer_size(style, size);
        let class = wide(CLASS_NAME);
        let title = wide(title);
        // SAFETY: the class is registered and the strings outlive the call.
        let hwnd = unsafe {
            CreateWindowExW(
                0,
                class.as_ptr(),
                title.as_ptr(),
                style,
                CW_USEDEFAULT,
                CW_USEDEFAULT,
                width,
                height,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                GetModuleHandleW(std::ptr::null()),
                std::ptr::null(),
            )
        };
        if hwnd.is_null() {
            return Err(format!(
                "the editor window was not created ({})",
                std::io::Error::last_os_error()
            ));
        }
        let state = Rc::new(State {
            closed: Cell::new(false),
            resized: Cell::new(false),
            waker,
        });
        // SAFETY: `state` outlives the slot, cleared in Drop.
        unsafe {
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, Rc::as_ptr(&state) as isize);
            ShowWindow(hwnd, SW_SHOW);
        }
        Ok(Self {
            hwnd,
            style,
            state,
            size: Rc::new(Cell::new(size)),
        })
    }

    pub fn parent(&self) -> *mut c_void {
        self.hwnd
    }

    pub fn resizer(&self) -> Resizer {
        let hwnd = self.hwnd;
        let style = self.style;
        let size = self.size.clone();
        Rc::new(move |width, height| {
            size.set((width, height));
            let (width, height) = outer_size(style, (width, height));
            let flags = SWP_NOMOVE | SWP_NOZORDER | SWP_NOACTIVATE;
            // SAFETY: a stale handle only makes the call fail.
            unsafe { SetWindowPos(hwnd, std::ptr::null_mut(), 0, 0, width, height, flags) };
        })
    }

    /// The new size when the user resized the window.
    pub fn idle(&mut self) -> Option<(i32, i32)> {
        if !self.state.resized.take() {
            return None;
        }
        let mut rect = RECT {
            left: 0,
            top: 0,
            right: 0,
            bottom: 0,
        };
        // SAFETY: a live window. Minimized, its client area is empty.
        let known = unsafe { IsIconic(self.hwnd) == 0 && GetClientRect(self.hwnd, &mut rect) != 0 };
        if !known {
            return None;
        }
        let now = (rect.right - rect.left, rect.bottom - rect.top);
        (now != self.size.replace(now)).then_some(now)
    }

    pub fn raise(&self) {
        // SAFETY: a live window.
        unsafe {
            let show = if IsIconic(self.hwnd) != 0 {
                SW_RESTORE
            } else {
                SW_SHOW
            };
            ShowWindow(self.hwnd, show);
            SetForegroundWindow(self.hwnd);
        }
    }

    pub fn is_open(&self) -> bool {
        !self.state.closed.get()
    }
}

impl Drop for Window {
    fn drop(&mut self) {
        // SAFETY: the window is ours; the slot is cleared before `state` goes.
        unsafe {
            SetWindowLongPtrW(self.hwnd, GWLP_USERDATA, 0);
            DestroyWindow(self.hwnd);
        }
    }
}

#[cfg(test)]
#[path = "../tests/editor/windows.rs"]
mod tests;
