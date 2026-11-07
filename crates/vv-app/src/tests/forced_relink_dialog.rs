use super::*;

#[test]
fn a_finished_search_turns_into_rows_preselected_where_something_matched() {
    let dir = vv_media::test_support::TempDir::new("vv-app-forced-relink-dialog-test");
    let found = dir.join("rana.mp4");
    std::fs::write(&found, b"x").unwrap();

    let mut app = VenturiApp::default();
    let meta = vv_core::MediaMeta {
        duration_frames: 10,
        fps: vv_core::Rational::new(25, 1),
        width: 320,
        height: 240,
        has_video: true,
        has_audio: false,
        sample_rate: 0,
        channels: 0,
        audio_streams: 0,
        file: Default::default(),
    };
    let rana = app.session.project.media_pool.insert(vv_core::MediaItem {
        path: "/missing/rana.mov".into(),
        meta: meta.clone(),
        content_hash: 1,
        compound: None,
        folder: None,
    });
    let ghost = app.session.project.media_pool.insert(vv_core::MediaItem {
        path: "/missing/ghost.mov".into(),
        meta,
        content_hash: 2,
        compound: None,
        folder: None,
    });
    app.open_forced_relink(dir.to_path_buf(), &[rana, ghost]);
    let dialog = app.forced_relink.as_mut().unwrap();
    dialog.phase = Phase::Searching(forced_relink::spawn_search(
        dialog.references.clone(),
        dir.to_path_buf(),
        dialog.criteria.clone(),
    ));

    let ctx = egui::Context::default();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
            app.show_forced_relink_dialog(ui.ctx());
        });
        output.textures_delta.clear();
        if matches!(app.forced_relink.as_ref().unwrap().phase, Phase::Results(_)) {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "search never finished"
        );
        std::thread::sleep(std::time::Duration::from_millis(10));
    }

    let dialog = app.forced_relink.take().unwrap();
    let Phase::Results(rows) = dialog.phase else {
        unreachable!()
    };
    assert!(rows[0].selected);
    assert!(!rows[1].selected, "nothing to relink it to");
    assert_eq!(
        chosen_relinks(&dialog.references, rows)
            .into_iter()
            .map(|(id, path, _)| (id, path))
            .collect::<Vec<_>>(),
        [(rana, found)]
    );
}
