use super::*;

fn temp_dir(name: &str) -> crate::test_support::TempDir {
    crate::test_support::TempDir::new(&format!("vv-app-library-ui-test-{name}"))
}

/// A library holding one asset, `music.mp3`.
fn library_with_asset(root: &Path) -> library::Library {
    let file = root.join("music.mp3");
    std::fs::write(&file, b"not really music").unwrap();
    let mut project = vv_core::Project::default();
    let media = project.media_pool.insert(vv_core::MediaItem {
        path: file,
        meta: vv_core::MediaMeta {
            duration_frames: 10,
            fps: vv_core::Rational::new(30, 1),
            width: 0,
            height: 0,
            has_video: false,
            has_audio: true,
            sample_rate: 48000,
            channels: 2,
            audio_streams: 1,
            file: Default::default(),
        },
        content_hash: 0x56,
        compound: None,
        folder: None,
    });
    let origin = vv_core::clipboard::ClipboardOrigin {
        instance: 0,
        epoch: 0,
    };
    let payload = vv_core::clipboard::ClipboardPayload::new(origin, &project, &[media], &[]);
    let mut library = library::Library::open(root.join("library"));
    library.add_asset("Music".into(), None, payload);
    while library.assets.is_empty() {
        assert!(library.poll().is_empty());
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    library
}

#[test]
fn library_media_are_copied_into_the_project_folder() {
    let dir = temp_dir("saved");
    let mut app = VenturiApp::default();
    app.library = library_with_asset(&dir);
    let project_file = dir.join("project").join("show.vvproj");
    std::fs::create_dir_all(project_file.parent().unwrap()).unwrap();
    app.session.save_to(&project_file).unwrap();
    let asset = app.library.assets[0].id.clone();

    let media = app.import_library_asset(&asset);
    app.wait_for_library_copies();

    let path = &app.session.project.media_pool[media[0]].path;
    assert_eq!(*path, dir.join("project").join("library").join("music.mp3"));
    assert!(path.is_file());
}

#[test]
fn an_unsaved_project_gets_its_copies_when_saved() {
    let dir = temp_dir("unsaved");
    let mut app = VenturiApp::default();
    app.library = library_with_asset(&dir);
    let asset = app.library.assets[0].id.clone();
    let media = app.import_library_asset(&asset)[0];
    assert!(
        app.session.project.media_pool[media]
            .path
            .starts_with(dir.join("library"))
    );

    let project_file = dir.join("project").join("show.vvproj");
    std::fs::create_dir_all(project_file.parent().unwrap()).unwrap();
    app.save_project_to(&project_file);
    app.wait_for_library_copies();

    let copy = dir.join("project").join("library").join("music.mp3");
    assert_eq!(app.session.project.media_pool[media].path, copy);
    let saved = vv_core::load_project(&project_file).unwrap();
    assert_eq!(
        saved.media_pool[media].path, copy,
        "with nothing else unsaved, the file is written again"
    );
}
