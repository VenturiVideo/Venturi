use super::*;
use std::time::{Duration, Instant};

fn wait_until(mut cond: impl FnMut() -> bool, what: &str) {
    let deadline = Instant::now() + Duration::from_secs(30);
    while !cond() {
        assert!(Instant::now() < deadline, "timeout: {what}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn pause_holds_the_queue_and_resume_completes_it() {
    let dir = vv_media::test_support::TempDir::new("vv-app-proxy-worker-test");
    let path = dir.join("clip.mp4");
    vv_media::test_support::ffmpeg(
        &[
            "-f",
            "lavfi",
            "-i",
            "testsrc=size=320x240:rate=25:duration=1",
            "-c:v",
            "libx264",
            "-pix_fmt",
            "yuv420p",
        ],
        &path,
    );
    let content_hash = 0x5EED_0001;
    let _ = std::fs::remove_file(vv_media::proxy::proxy_path_for(
        content_hash,
        ProxyQuality::Low,
    ));

    let worker = ProxyWorker::spawn(ProxyQuality::Low);
    worker.set_paused(true);
    worker.enqueue(path.clone(), content_hash, 25);
    worker.enqueue(path, content_hash, 25);
    assert_eq!(
        worker.progress().total,
        1,
        "a requeued media does not count twice"
    );

    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(worker.state(content_hash), Some(ProxyState::Queued));
    assert_eq!(worker.progress().finished, 0);

    worker.set_paused(false);
    wait_until(
        || worker.state(content_hash) == Some(ProxyState::Ready),
        "proxy never ready after resuming",
    );
    let progress = worker.progress();
    assert_eq!((progress.finished, progress.total), (1, 1));
    assert_eq!(progress.fraction, 1.0);
}
