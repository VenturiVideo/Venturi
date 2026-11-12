use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use windows_sys::Win32::UI::WindowsAndMessaging::{SendMessageW, WM_CLOSE};

use super::*;

fn window(resizable: bool) -> (Window, Arc<AtomicUsize>) {
    let wakes = Arc::new(AtomicUsize::new(0));
    let counter = wakes.clone();
    let waker: Waker = Arc::new(move || {
        counter.fetch_add(1, Ordering::SeqCst);
    });
    let window = Window::new("Venturi test editor", (400, 300), resizable, waker).unwrap();
    (window, wakes)
}

fn client_size(window: &Window) -> (i32, i32) {
    let mut rect = RECT {
        left: 0,
        top: 0,
        right: 0,
        bottom: 0,
    };
    // SAFETY: a live window.
    unsafe { GetClientRect(window.hwnd, &mut rect) };
    (rect.right, rect.bottom)
}

#[test]
fn the_client_area_has_the_size_the_plugin_asked_for() {
    let (window, _) = window(true);
    assert!(!window.parent().is_null());
    assert_eq!(client_size(&window), (400, 300));
}

#[test]
fn a_resize_by_the_plugin_is_not_reported_as_one_by_the_user() {
    let (mut window, _) = window(true);
    // Showing the window sized it once.
    window.idle();
    (window.resizer())(640, 480);
    assert_eq!(client_size(&window), (640, 480));
    assert_eq!(window.idle(), None);
}

#[test]
fn a_resize_by_the_user_is_reported_once_and_wakes_the_app() {
    let (mut window, wakes) = window(true);
    window.idle();
    let before = wakes.load(Ordering::SeqCst);
    let (width, height) = outer_size(window.style, (500, 350));
    let flags = SWP_NOMOVE | SWP_NOZORDER | SWP_NOACTIVATE;
    // SAFETY: a live window; what dragging its border ends up calling.
    unsafe {
        SetWindowPos(
            window.hwnd,
            std::ptr::null_mut(),
            0,
            0,
            width,
            height,
            flags,
        )
    };
    assert!(wakes.load(Ordering::SeqCst) > before);
    assert_eq!(window.idle(), Some((500, 350)));
    assert_eq!(window.idle(), None);
}

#[test]
fn closing_hides_the_window_and_leaves_it_to_drop() {
    let (window, wakes) = window(false);
    let before = wakes.load(Ordering::SeqCst);
    assert!(window.is_open());
    // SAFETY: a live window; sent messages are handled before the call returns.
    unsafe { SendMessageW(window.hwnd, WM_CLOSE, 0, 0) };
    assert!(!window.is_open());
    assert!(wakes.load(Ordering::SeqCst) > before);
    // Still a window: the plugin's view is removed from it before it goes.
    assert_eq!(client_size(&window), (400, 300));
}
