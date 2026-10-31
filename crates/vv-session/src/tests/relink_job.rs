use super::*;
use vv_media::test_support::TempDir;

fn media_ids(n: usize) -> Vec<MediaId> {
    let mut project = vv_core::Project::default();
    (0..n)
        .map(|i| {
            let timeline = project.timelines.insert(vv_core::Timeline {
                name: i.to_string(),
                fps: vv_core::Rational::new(25, 1),
                resolution: (64, 48),
                tracks: Vec::new(),
                markers: Vec::new(),
                master: Default::default(),
            });
            project.insert_timeline_item(timeline, None)
        })
        .collect()
}

#[test]
fn by_name_prepares_the_offline_media_and_reports_the_missing_ones() {
    let temp = TempDir::new("vv-app-relink-job");
    let dir = temp.to_path_buf();
    std::fs::create_dir_all(dir.join("sub")).unwrap();
    let found = dir.join("sub/a.mp4");
    std::fs::write(&found, b"a").unwrap();
    let reachable = dir.join("b.mp4");
    std::fs::write(&reachable, b"b").unwrap();
    let ids = media_ids(3);
    let progress = RelinkProgress::default();

    let outcome = run(
        RelinkRequest::ByName {
            base_dir: dir.clone(),
            targets: vec![
                (ids[0], "/missing/a.mp4".into()),
                (ids[1], reachable),
                (ids[2], "/missing/ghost.mp4".into()),
            ],
        },
        &progress,
    )
    .expect("not cancelled");

    assert_eq!(outcome.relinks.len(), 1);
    assert_eq!(outcome.relinks[0].media_id, ids[0]);
    assert_eq!(outcome.relinks[0].path, found);
    assert_eq!(outcome.not_found, Some((dir.clone(), vec![ids[2]])));
    assert_eq!(progress.done.load(Ordering::Relaxed), 1);
    assert_eq!(progress.total.load(Ordering::Relaxed), 1);
}

#[test]
fn a_cancelled_relink_returns_nothing() {
    let progress = RelinkProgress::default();
    progress.cancel.store(true, Ordering::Relaxed);
    let ids = media_ids(1);
    let outcome = run(
        RelinkRequest::Chosen(vec![(ids[0], "/x.mp4".into(), None)]),
        &progress,
    );
    assert!(outcome.is_none());
}
