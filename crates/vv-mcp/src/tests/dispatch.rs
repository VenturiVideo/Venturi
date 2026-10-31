use super::*;
use std::path::Path;
use vv_core::edit;
use vv_media::test_support::TempDir;

pub(crate) fn test_dir(name: &str) -> TempDir {
    TempDir::new(&format!("vv-mcp-{name}"))
}

/// 1 s of 64x48 video at 30000/1001 fps with a sine track.
pub(crate) fn clip_file(dir: &Path, name: &str) -> PathBuf {
    let path = dir.join(name);
    vv_media::test_support::ffmpeg(
        &[
            "-f",
            "lavfi",
            "-i",
            "testsrc=size=64x48:rate=30000/1001:duration=1",
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=440:sample_rate=48000:duration=1",
            "-c:v",
            "libx264",
            "-pix_fmt",
            "yuv420p",
            "-c:a",
            "aac",
        ],
        &path,
    );
    path
}

pub(crate) fn handled(session: &mut Session, call: ToolCall) -> ToolResult {
    match dispatch(session, call) {
        Dispatch::Handled(result) => result,
        Dispatch::Deferred(_) => panic!("expected an immediate result"),
    }
}

pub(crate) fn ok(session: &mut Session, call: ToolCall) -> Value {
    handled(session, call).unwrap().value
}

pub(crate) fn error(session: &mut Session, call: ToolCall) -> String {
    handled(session, call).unwrap_err().0
}

/// Runs a deferred call to completion, as a host does.
pub(crate) fn run_deferred(session: &mut Session, call: ToolCall) -> ToolResult {
    let Dispatch::Deferred(pending) = dispatch(session, call) else {
        panic!("expected a deferred call");
    };
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    loop {
        for event in session.tick() {
            if let Some(result) = pending.resolve(session, &event) {
                return result;
            }
        }
        if let Some(result) = pending.poll() {
            return result;
        }
        assert!(std::time::Instant::now() < deadline, "never resolved");
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
}

pub(crate) fn import(session: &mut Session, paths: &[&Path]) -> Value {
    let args = ImportMediaArgs {
        paths: paths.iter().map(|p| p.display().to_string()).collect(),
    };
    run_deferred(session, ToolCall::ImportMedia(args))
        .unwrap()
        .value
}

pub(crate) fn create(name: &str) -> CreateTimelineArgs {
    CreateTimelineArgs {
        name: name.into(),
        from_media: None,
        fps: None,
        resolution: None,
    }
}

#[test]
fn an_empty_project_has_nothing_to_report() {
    let mut session = Session::default();
    let project = ok(&mut session, ToolCall::GetProject);
    assert_eq!(project["media"], json!([]));
    assert_eq!(project["timelines"], json!([]));
    assert_eq!(project["unsaved"], json!(false));
    assert_eq!(project["path"], Value::Null);
}

#[test]
fn import_reports_new_and_existing_media_in_the_given_order() {
    let dir = test_dir("import");
    let a = clip_file(&dir, "a.mp4");
    let b = clip_file(&dir, "b.mp4");
    let mut session = Session::default();
    import(&mut session, &[&a]);

    let result = import(&mut session, &[&b, &dir.join("missing.mp4"), &a]);

    let names: Vec<&str> = result["media"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["b.mp4", "a.mp4"]);
    let b = &result["media"][0];
    assert_eq!(b["kind"], "video");
    assert_eq!(b["fps"]["num"], 30000);
    assert_eq!(b["fps"]["den"], 1001);
    assert_eq!(b["resolution"], json!([64, 48]));
    assert_eq!(b["audio_streams"], 1);
    assert_eq!(b["offline"], false);
    let errors = result["errors"].as_array().unwrap();
    assert_eq!(errors.len(), 1);
    assert!(errors[0].as_str().unwrap().starts_with("missing.mp4: "));
}

#[test]
fn import_without_paths_is_an_error() {
    let mut session = Session::default();
    let call = ToolCall::ImportMedia(ImportMediaArgs { paths: Vec::new() });
    assert_eq!(error(&mut session, call), "no paths given");
}

#[test]
fn create_timeline_takes_the_media_format_unless_overridden() {
    let dir = test_dir("create-timeline");
    let a = clip_file(&dir, "a.mp4");
    let mut session = Session::default();
    let media_id = import(&mut session, &[&a])["media"][0]["id"].clone();

    let default = ok(&mut session, ToolCall::CreateTimeline(create("Plain")));
    assert_eq!(default["fps"]["value"], 25.0);
    assert_eq!(default["resolution"], json!([1920, 1080]));
    assert_eq!(default["video_tracks"], 1);
    assert_eq!(default["audio_tracks"], 1);

    let mut args = create("From media");
    args.from_media = Some(media_id.as_str().unwrap().into());
    let from_media = ok(&mut session, ToolCall::CreateTimeline(args.clone()));
    assert_eq!(from_media["fps"]["num"], 30000);
    assert_eq!(from_media["resolution"], json!([64, 48]));

    args.fps = Some([50, 1]);
    args.resolution = Some([1280, 720]);
    let overridden = ok(&mut session, ToolCall::CreateTimeline(args));
    assert_eq!(overridden["fps"]["value"], 50.0);
    assert_eq!(overridden["resolution"], json!([1280, 720]));

    let project = ok(&mut session, ToolCall::GetProject);
    assert_eq!(project["timelines"].as_array().unwrap().len(), 3);
    assert_eq!(project["unsaved"], true);
    let timeline_items: Vec<&Value> = project["media"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|m| m["kind"] == "timeline")
        .collect();
    assert_eq!(timeline_items.len(), 3);
    assert!(
        timeline_items
            .iter()
            .any(|m| m["timeline_id"] == overridden["id"])
    );
}

#[test]
fn create_timeline_rejects_bad_arguments() {
    let mut session = Session::default();
    let mut args = create("T");
    args.fps = Some([0, 1]);
    assert_eq!(
        error(&mut session, ToolCall::CreateTimeline(args)),
        "invalid fps 0/1"
    );
    let mut args = create("T");
    args.from_media = Some("12345".into());
    assert_eq!(
        error(&mut session, ToolCall::CreateTimeline(args)),
        "unknown media id \"12345\""
    );
    let mut args = create("T");
    args.from_media = Some("abc".into());
    assert_eq!(
        error(&mut session, ToolCall::CreateTimeline(args)),
        "malformed media id \"abc\""
    );
    assert!(session.project.timelines.is_empty());
}

#[test]
fn save_open_and_new_project() {
    let dir = test_dir("save");
    let mut session = Session::default();
    ok(&mut session, ToolCall::CreateTimeline(create("T")));
    let save = |path: Option<&Path>| {
        ToolCall::SaveProject(SaveProjectArgs {
            path: path.map(|p| p.display().to_string()),
        })
    };
    assert_eq!(
        error(&mut session, save(None)),
        "the project has no file yet: pass `path`"
    );
    let path = dir.join("p.vvproj");
    ok(&mut session, save(Some(&path)));
    ok(&mut session, save(None));
    assert_eq!(ok(&mut session, ToolCall::GetProject)["unsaved"], false);

    let project = ok(&mut session, ToolCall::NewProject);
    assert_eq!(project["timelines"], json!([]));
    let open = |path: &Path| {
        ToolCall::OpenProject(OpenProjectArgs {
            path: path.display().to_string(),
        })
    };
    let project = ok(&mut session, open(&path));
    assert_eq!(project["timelines"][0]["name"], "T");
    assert_eq!(project["path"], json!(path));
    let missing = dir.join("nope.vvproj");
    assert!(error(&mut session, open(&missing)).starts_with("cannot open "));
}

#[test]
fn undo_and_redo_name_the_step() {
    let mut session = Session::default();
    assert_eq!(error(&mut session, ToolCall::Undo), "nothing to undo");
    ok(&mut session, ToolCall::CreateTimeline(create("T")));
    let timeline = session.project.timelines.keys().next().unwrap();
    edit::insert_generator(
        &mut session.project,
        &mut session.history,
        timeline,
        edit::Generator::Text,
        0,
        0,
        None,
    );

    assert_eq!(ok(&mut session, ToolCall::Undo)["undone"], "InsertClips");
    assert!(
        session.project.timelines[timeline].tracks[0]
            .clips
            .is_empty()
    );
    assert_eq!(ok(&mut session, ToolCall::Redo)["redone"], "InsertClips");
    assert_eq!(error(&mut session, ToolCall::Redo), "nothing to redo");
}
