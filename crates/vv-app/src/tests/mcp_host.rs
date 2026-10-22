use super::*;
use tokio::sync::oneshot::{Receiver, error::TryRecvError};
use vv_mcp::{ImportMediaArgs, McpHandle, SetClipPropertiesArgs, TimelineArgs};

/// An editor serving MCP without a socket; the UI has the session.
fn attached() -> (VenturiApp, McpHandle, egui::Context) {
    let mut app = VenturiApp::default();
    let ctx = egui::Context::default();
    let (handle, inbox) = vv_mcp::channel(|| {});
    app.mcp = Some(McpHost::for_test(inbox, &mut app.session, &ctx));
    app.seen_epoch = app.session.epoch();
    (app, handle, ctx)
}

fn wait(reply: &mut Receiver<ToolResult>, within: Duration) -> Option<ToolResult> {
    let deadline = Instant::now() + within;
    loop {
        match reply.try_recv() {
            Ok(result) => return Some(result),
            Err(TryRecvError::Closed) => panic!("the host dropped the call"),
            Err(TryRecvError::Empty) if Instant::now() > deadline => return None,
            Err(TryRecvError::Empty) => std::thread::sleep(Duration::from_millis(5)),
        }
    }
}

/// With the window not drawn at all: no frame runs until the answer.
fn call_hidden(
    app: &mut VenturiApp,
    handle: &McpHandle,
    ctx: &egui::Context,
    call: ToolCall,
) -> ToolResult {
    app.mcp_checkin(ctx);
    let mut reply = handle.submit(call);
    let result = wait(&mut reply, Duration::from_secs(30)).expect("answered without frames");
    app.mcp_checkout();
    result
}

fn timeline_id_string(id: TimelineId) -> String {
    use vv_core::Id;
    id.raw().to_string()
}

fn opacity(timeline_id: TimelineId, clip: ClipId, value: f32) -> ToolCall {
    ToolCall::SetClipProperties(SetClipPropertiesArgs {
        timeline_id: timeline_id_string(timeline_id),
        if_revision: None,
        clip_ids: vec![clip.0.to_string()],
        opacity: Some(value),
        position: None,
        scale: None,
        rotation: None,
        gain_db: None,
        disabled: None,
        fade_in: None,
        fade_out: None,
        fill_color: None,
        matte: None,
    })
}

fn solid_clip(app: &mut VenturiApp) -> (TimelineId, ClipId) {
    let timeline = app.ensure_timeline();
    let clip = vv_core::edit::insert_generator(
        &mut app.session.project,
        &mut app.session.history,
        timeline,
        vv_core::edit::Generator::SolidColor,
        0,
        0,
        Some(50),
    );
    (timeline, clip)
}

fn clip_opacity(app: &VenturiApp, timeline: TimelineId, clip: ClipId) -> f32 {
    app.session.project.timelines[timeline]
        .clip(0, clip)
        .unwrap()
        .effects
        .transform
        .track(vv_core::TransformParam::Opacity)
        .default
}

#[test]
fn calls_are_answered_while_the_window_is_not_drawn() {
    let (mut app, handle, ctx) = attached();
    let (timeline, clip) = solid_clip(&mut app);

    assert!(call_hidden(&mut app, &handle, &ctx, opacity(timeline, clip, 40.0)).is_ok());

    assert_eq!(
        clip_opacity(&app, timeline, clip),
        40.0,
        "the UI sees the edit"
    );
}

#[test]
fn get_state_reports_what_the_user_saw_last() {
    let (mut app, handle, ctx) = attached();
    let (timeline, clip) = solid_clip(&mut app);
    app.timeline_state.playhead = 12;
    app.timeline_state
        .set_selection(BTreeSet::from([(0, clip)]), Some((0, clip)));

    let state = call_hidden(&mut app, &handle, &ctx, ToolCall::GetState)
        .unwrap()
        .value;

    assert_eq!(state["active_timeline"]["id"], timeline_id_string(timeline));
    assert_eq!(state["playhead"], 12);
    assert_eq!(
        state["selected_clips"],
        serde_json::json!([clip.0.to_string()])
    );
}

#[test]
fn edits_wait_for_the_users_gesture_and_stay_a_separate_undo_step() {
    let (mut app, handle, ctx) = attached();
    let (timeline, clip) = solid_clip(&mut app);
    // The user is dragging a slider: their edits so far form one group.
    app.edit_drag_group = Some(app.session.history.begin_group());
    let before = app.session.history.position();
    app.mcp_checkin(&ctx);

    let mut edit = handle.submit(opacity(timeline, clip, 40.0));
    assert!(
        wait(&mut edit, Duration::from_millis(300)).is_none(),
        "held"
    );
    let mut read = handle.submit(ToolCall::GetProject);
    assert!(
        wait(&mut read, Duration::from_secs(5)).is_some(),
        "reading is not held"
    );

    app.mcp_checkout();
    let mark = app.edit_drag_group.take().unwrap();
    app.session.history.end_group(mark);
    app.mcp_checkin(&ctx);
    assert!(wait(&mut edit, Duration::from_secs(5)).unwrap().is_ok());
    app.mcp_checkout();

    assert_eq!(app.session.history.position(), before + 1);
    assert_eq!(clip_opacity(&app, timeline, clip), 40.0);
}

#[test]
fn edits_are_refused_while_a_dialog_waits_for_the_user() {
    let (mut app, handle, ctx) = attached();
    let (timeline, clip) = solid_clip(&mut app);
    app.pending_project_switch = Some(ProjectSwitch::Quit);

    let refused = call_hidden(&mut app, &handle, &ctx, opacity(timeline, clip, 40.0)).unwrap_err();
    assert!(refused.0.contains("dialog open"), "{refused}");
    assert!(call_hidden(&mut app, &handle, &ctx, ToolCall::GetProject).is_ok());
}

#[test]
fn a_project_replaced_by_the_agent_resets_the_editor() {
    let (mut app, handle, ctx) = attached();
    solid_clip(&mut app);

    let refused = call_hidden(&mut app, &handle, &ctx, ToolCall::NewProject).unwrap_err();
    assert!(refused.0.contains("unsaved changes"), "{refused}");
    assert!(app.timeline_id.is_some());

    app.session.mark_saved();
    assert!(call_hidden(&mut app, &handle, &ctx, ToolCall::NewProject).is_ok());
    assert_eq!(app.timeline_id, None, "the editor follows the new project");
}

fn video_file(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join("vv-app-mcp-host");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(name);
    vv_media::test_support::ffmpeg(
        &[
            "-f",
            "lavfi",
            "-i",
            "testsrc=size=64x48:rate=25:duration=1",
            "-c:v",
            "libx264",
            "-pix_fmt",
            "yuv420p",
        ],
        &path,
    );
    path
}

fn import(path: &Path) -> ToolCall {
    ToolCall::ImportMedia(ImportMediaArgs {
        paths: vec![path.display().to_string()],
    })
}

#[test]
fn an_agent_import_does_not_move_the_users_view() {
    let path = video_file("hidden.mp4");
    let (mut app, handle, ctx) = attached();
    app.import_warnings = vec!["the user's".into()];

    let result = call_hidden(&mut app, &handle, &ctx, import(&path)).unwrap();

    assert_eq!(result.value["media"][0]["name"], "hidden.mp4");
    assert_eq!(app.timeline_id, None, "no timeline created for the agent");
    assert_eq!(app.browsing, None);
    assert!(app.media_pool_state.selected.is_empty());
    assert_eq!(app.import_warnings, ["the user's"]);
}

#[test]
fn an_agent_import_is_answered_while_the_window_keeps_drawing() {
    let path = video_file("drawn.mp4");
    let (mut app, handle, ctx) = attached();
    let mut reply = handle.submit(import(&path));
    let deadline = Instant::now() + Duration::from_secs(30);
    // Frames go on: the UI ticks the session too, and its events must reach
    // the waiting call.
    let result = loop {
        app.poll_session(&ctx);
        app.mcp_checkin(&ctx);
        let answer = wait(&mut reply, Duration::from_millis(10));
        app.mcp_checkout();
        if let Some(result) = answer {
            break result;
        }
        assert!(Instant::now() < deadline, "never answered");
    };
    assert_eq!(result.unwrap().value["media"][0]["name"], "drawn.mp4");
}

#[test]
fn set_active_timeline_applies_at_the_next_frame() {
    let (mut app, handle, ctx) = attached();
    let first = app.ensure_timeline();
    let second = app.create_timeline("Second".into(), vv_core::Rational::new(25, 1), (64, 48));
    assert_eq!(app.timeline_id, Some(first));

    let args = TimelineArgs {
        timeline_id: timeline_id_string(second),
    };
    call_hidden(&mut app, &handle, &ctx, ToolCall::SetActiveTimeline(args)).unwrap();

    assert_eq!(app.timeline_id, Some(second));
}

#[test]
fn screenshot_answers_with_the_next_drawn_frame() {
    let (mut app, handle, ctx) = attached();
    app.mcp_checkin(&ctx);
    let mut reply = handle.submit(ToolCall::ScreenshotUi);
    assert!(wait(&mut reply, Duration::from_millis(200)).is_none());
    app.mcp_checkout();

    let image = egui::ColorImage::new([4, 2], vec![egui::Color32::RED; 8]);
    app.mcp_screenshot(&image);

    let output = reply.try_recv().unwrap().unwrap();
    assert_eq!(output.value["width"], 4);
    assert!(output.image_png.is_some());
}

#[test]
fn an_agent_export_shows_in_the_progress_window() {
    let dir = std::env::temp_dir().join("vv-app-mcp-export");
    std::fs::create_dir_all(&dir).unwrap();
    let (mut app, handle, ctx) = attached();
    let (timeline, _) = solid_clip(&mut app);

    let started = call_hidden(
        &mut app,
        &handle,
        &ctx,
        ToolCall::Export(vv_mcp::ExportArgs {
            timeline_id: timeline_id_string(timeline),
            path: dir.join("out.mp4").display().to_string(),
            range: Some([0, 5]),
            scale_percent: Some(10),
            audio: false,
        }),
    )
    .unwrap();

    let job: vv_session::JobId = started.value["job_id"].as_str().unwrap().parse().unwrap();
    assert_eq!(app.export.as_ref().map(|e| e.job), Some(job));
}

#[test]
fn turning_mcp_off_gives_the_session_back_to_the_editor() {
    let (mut app, _handle, ctx) = attached();
    let (timeline, _) = solid_clip(&mut app);
    app.mcp_checkin(&ctx);
    app.mcp_checkout();
    app.settings.mcp_enabled = false;

    app.apply_mcp_setting(&ctx);

    assert!(app.mcp.is_none());
    assert!(app.session.project.timelines.contains_key(timeline));
}

/// MCP on with a project installed (epoch above zero), the session in the slot between frames.
fn attached_with_gesture() -> (VenturiApp, vv_core::TimelineId, egui::Context) {
    let (mut app, id) = crate::tests::app_with_pending_gesture();
    let ctx = egui::Context::default();
    let (_handle, inbox) = vv_mcp::channel(|| {});
    let project = std::mem::take(&mut app.session.project);
    app.session.install_project(project, None);
    app.mcp = Some(McpHost::for_test(inbox, &mut app.session, &ctx));
    app.seen_epoch = app.session.epoch();
    assert!(app.seen_epoch > 0);
    app.mcp_checkin(&ctx);
    (app, id, ctx)
}

#[test]
fn editors_are_pumped_by_logic_with_mcp_on_between_frames() {
    let (mut app, _, ctx) = attached_with_gesture();
    eframe::App::logic(&mut app, &ctx, &mut eframe::Frame::_new_kittest());
    let slot = app.mcp.as_ref().unwrap().shared.slot.lock().unwrap();
    assert!(!slot.in_ui, "the session went back to the slot");
    assert_eq!(slot.session.history.labels().count(), 1);
}

#[test]
fn editors_are_not_pumped_after_the_agent_replaced_the_project() {
    let (mut app, _, ctx) = attached_with_gesture();
    app.seen_epoch -= 1;
    eframe::App::logic(&mut app, &ctx, &mut eframe::Frame::_new_kittest());
    let slot = app.mcp.as_ref().unwrap().shared.slot.lock().unwrap();
    assert!(!slot.in_ui);
    assert_eq!(slot.session.history.labels().count(), 0);
}
