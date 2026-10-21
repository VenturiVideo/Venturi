use std::sync::Arc;

use super::*;
use crate::HostedPlugin;
use crate::testing::{fixture_effect, fixture_instance};

#[test]
fn editor_open_fails_cleanly_without_view() {
    let instance: Arc<dyn HostedPlugin> = fixture_instance(&fixture_effect(&[]));
    // The fixture's createView returns null: nothing to attach, no window made.
    let err = Editor::open(&instance, "Venturi Test Gain", Arc::new(|| {})).err();
    // On macOS the test thread is refused first: only the main thread may open windows.
    let expected = if cfg!(target_os = "macos") {
        "plugin editors open on the main thread"
    } else {
        "the plugin has no editor"
    };
    assert_eq!(err.as_deref(), Some(expected));
}

#[test]
fn a_refused_constrained_size_is_requested_once_and_the_plugin_hears_the_constrained_one() {
    let mut requested = None;
    let first = resize_decision((1000, 700), (900, 700), &mut requested);
    assert_eq!(first.request, Some((900, 700)));
    assert_eq!(first.notify, (900, 700));
    let refused = resize_decision((1000, 700), (900, 700), &mut requested);
    assert_eq!(refused.request, None);
    assert_eq!(refused.notify, (900, 700));
}

#[test]
fn a_user_resize_after_a_refusal_is_constrained_again() {
    let mut requested = None;
    resize_decision((1000, 700), (900, 700), &mut requested);
    resize_decision((1000, 700), (900, 700), &mut requested);
    let user = resize_decision((1100, 700), (900, 700), &mut requested);
    assert_eq!(user.request, Some((900, 700)));
}

#[test]
fn a_different_answer_to_a_request_is_not_asked_for_again() {
    let mut requested = None;
    resize_decision((1000, 700), (900, 700), &mut requested);
    let answer = resize_decision((1200, 700), (900, 700), &mut requested);
    assert_eq!(answer.request, None);
}

#[test]
fn a_report_that_matches_the_constraint_requests_nothing() {
    let mut requested = Some((900, 700));
    let ok = resize_decision((900, 700), (900, 700), &mut requested);
    assert_eq!(ok.request, None);
    assert_eq!(requested, None);
}

#[test]
fn a_window_closed_while_idling_wakes_the_app_once() {
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let counter = calls.clone();
    let waker: Waker = Arc::new(move || {
        counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    });
    let woke = || calls.load(std::sync::atomic::Ordering::SeqCst);
    wake_if_closed(true, true, &waker);
    wake_if_closed(false, false, &waker);
    assert_eq!(woke(), 0);
    wake_if_closed(true, false, &waker);
    assert_eq!(woke(), 1);
}
