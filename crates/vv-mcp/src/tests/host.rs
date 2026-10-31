use super::*;
use crate::{ImportMediaArgs, ToolCall};

#[test]
fn headless_host_answers_until_the_last_handle_goes() {
    let dir = vv_media::test_support::TempDir::new("vv-mcp-headless");
    let media = crate::dispatch::tests::clip_file(&dir, "a.mp4");
    let (handle, inbox) = channel(|| {});
    let host = std::thread::spawn(move || {
        run_headless(Session::default(), inbox)
            .project
            .media_pool
            .len()
    });

    let import = handle.submit(ToolCall::ImportMedia(ImportMediaArgs {
        paths: vec![media.display().to_string()],
    }));
    // Answered while the import is still running.
    let project = handle.submit(ToolCall::GetProject).blocking_recv().unwrap();
    assert!(project.is_ok());
    let imported = import.blocking_recv().unwrap().unwrap();
    assert_eq!(imported.value["media"][0]["name"], "a.mp4");

    drop(handle);
    assert_eq!(host.join().unwrap(), 1);
}

#[test]
fn a_call_after_the_host_is_gone_fails_cleanly() {
    let (handle, inbox) = channel(|| {});
    drop(inbox);
    let result = handle.submit(ToolCall::GetProject).blocking_recv().unwrap();
    assert_eq!(result.unwrap_err().0, "Venturi is shutting down");
}

#[test]
fn every_call_notifies_the_host() {
    let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let (handle, inbox) = channel({
        let calls = calls.clone();
        move || {
            calls.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
    });
    let _reply = handle.submit(ToolCall::GetProject);
    assert_eq!(calls.load(std::sync::atomic::Ordering::Relaxed), 1);
    assert!(inbox.try_recv().is_some());
    assert!(inbox.try_recv().is_none());
}
