//! Pointer gestures driven through `show_timeline` with synthetic input,
//! frame by frame: they pin down what each drag commits.

use super::*;
use vv_core::{CrossTransition, Ease, PushDirection, Rational, Track, Transition, TransitionKind};

/// 25 fps at the default 60 px/s: 2.4 px per frame.
const PX_PER_FRAME: f32 = 2.4;

struct Harness {
    ctx: egui::Context,
    project: Project,
    history: History,
    timeline_id: TimelineId,
    state: TimelineState,
    time: f64,
    /// `clip_rects` of the frame the button was released in.
    release_rects: std::collections::HashMap<ClipId, egui::Rect>,
    /// Held down during every following frame.
    modifiers: egui::Modifiers,
    /// The last compound clip `show_timeline` asked to open.
    entered: Option<(TimelineId, ClipId, FrameIdx)>,
}

impl Harness {
    fn new(tracks: Vec<Track>) -> Self {
        let mut project = Project::default();
        let timeline_id = project.timelines.insert(vv_core::Timeline {
            name: "T".into(),
            fps: Rational::new(25, 1),
            resolution: (1920, 1080),
            tracks,
            markers: Vec::new(),
            master: Default::default(),
        });
        let mut harness = Self {
            ctx: egui::Context::default(),
            project,
            history: History::default(),
            timeline_id,
            state: TimelineState::default(),
            time: 0.0,
            release_rects: std::collections::HashMap::new(),
            modifiers: egui::Modifiers::NONE,
            entered: None,
        };
        // Two frames: the first one only lays the panes out.
        harness.frame(Vec::new());
        harness.frame(Vec::new());
        harness
    }

    fn frame(&mut self, events: Vec<egui::Event>) {
        let mut input = egui::RawInput::default();
        input.screen_rect = Some(egui::Rect::from_min_size(
            egui::Pos2::ZERO,
            egui::vec2(1200.0, 700.0),
        ));
        input.time = Some(self.time);
        self.time += 1.0 / 60.0;
        input.events = std::iter::once(egui::Event::ModifiersChanged(self.modifiers))
            .chain(events)
            .collect();
        let mut entered = None;
        let mut output = self.ctx.run_ui(input, |ui| {
            egui::CentralPanel::default().show(ui, |ui| {
                (_, entered) = show_timeline(
                    ui,
                    &mut self.project,
                    &mut self.history,
                    self.timeline_id,
                    &|_id| "media".to_string(),
                    &std::collections::HashMap::new(),
                    &mut self.state,
                    false,
                    false,
                    &[],
                    &[],
                    &[],
                    &std::collections::HashMap::new(),
                    false,
                );
            });
        });
        output.textures_delta.clear();
        self.entered = entered.or(self.entered);
    }

    fn rect(&self, clip: ClipId) -> egui::Rect {
        self.state.clip_rects[&clip]
    }

    /// Presses at `from` and drags by `delta`. The movement that makes egui
    /// start the drag is not part of it (the timeline accumulates only from
    /// the next frame): a pickup step past egui's threshold comes first, in
    /// the same direction, so the gesture itself moves by exactly `delta`.
    fn drag(&mut self, from: egui::Pos2, delta: egui::Vec2) {
        let modifiers = self.modifiers;
        let button = |pos, pressed| egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers,
        };
        let picked_up = from + delta.normalized() * 12.0;
        self.frame(vec![egui::Event::PointerMoved(from)]);
        self.frame(vec![button(from, true)]);
        self.frame(vec![egui::Event::PointerMoved(picked_up)]);
        for step in 1..=5 {
            self.frame(vec![egui::Event::PointerMoved(
                picked_up + delta * (step as f32 / 5.0),
            )]);
        }
        self.frame(vec![button(picked_up + delta, false)]);
        self.release_rects = self.state.clip_rects.clone();
        self.frame(Vec::new());
    }

    fn click(&mut self, pos: egui::Pos2) {
        let modifiers = self.modifiers;
        let button = |pressed| egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers,
        };
        self.frame(vec![egui::Event::PointerMoved(pos)]);
        self.frame(vec![button(true)]);
        self.frame(vec![button(false)]);
    }

    fn with_markers(mut self, markers: Vec<vv_core::Marker>) -> Self {
        self.project.timelines[self.timeline_id].markers = markers;
        self.frame(Vec::new());
        self
    }

    /// Point in the marker lane over `frame`; needs a clip starting at 0.
    fn marker_pos(&self, frame: FrameIdx) -> egui::Pos2 {
        let origin_x = self
            .state
            .clip_rects
            .values()
            .map(|r| r.left())
            .fold(f32::MAX, f32::min);
        egui::pos2(
            origin_x + frame as f32 * PX_PER_FRAME,
            self.state.marker_lane.center().y,
        )
    }

    /// Markers follow the pointer from the press, pickup step included:
    /// drags by `frames` in total.
    fn drag_marker(&mut self, from: egui::Pos2, frames: f32) {
        let delta = frames * PX_PER_FRAME;
        self.drag(from, egui::vec2(delta - 12.0 * delta.signum(), 0.0));
    }

    fn markers(&self) -> Vec<(FrameIdx, FrameIdx)> {
        self.project.timelines[self.timeline_id]
            .markers
            .iter()
            .map(|m| (m.start, m.duration))
            .collect()
    }

    fn clip(&self, track_index: usize, id: ClipId) -> &Clip {
        self.project.timelines[self.timeline_id]
            .clip(track_index, id)
            .unwrap()
    }

    fn no_gesture(&self) -> bool {
        self.state.gesture.is_none()
    }
}

fn solid(id: u64, start: FrameIdx, len: FrameIdx) -> Clip {
    Clip::from_source_range(
        ClipId(id),
        ClipSource::SolidColor,
        0,
        len,
        start,
        Rational::one(),
    )
}

fn track(kind: TrackKind, clips: Vec<Clip>) -> Track {
    Track {
        clips,
        ..Track::new(kind)
    }
}

fn push(duration: FrameIdx) -> Transition {
    Transition {
        kind: TransitionKind::Push,
        duration,
        direction: PushDirection::Left,
        ease: Ease::None,
        curve: 0.0,
    }
}

#[test]
fn dragging_a_clip_body_moves_it() {
    let mut h = Harness::new(vec![track(TrackKind::Video, vec![solid(1, 0, 100)])]);
    let from = h.rect(ClipId(1)).center();
    h.drag(from, egui::vec2(50.0 * PX_PER_FRAME, 0.0));
    assert_eq!(h.clip(0, ClipId(1)).timeline_start, 50);
    assert_eq!(h.history.position(), 1);
    assert!(h.no_gesture());
}

#[test]
fn dragging_a_clip_edge_trims_it() {
    let mut h = Harness::new(vec![track(TrackKind::Video, vec![solid(1, 0, 100)])]);
    let rect = h.rect(ClipId(1));
    let from = egui::pos2(rect.right() - 2.0, rect.center().y);
    h.drag(from, egui::vec2(-25.0 * PX_PER_FRAME, 0.0));
    let clip = h.clip(0, ClipId(1));
    assert_eq!((clip.timeline_start, clip.timeline_len), (0, 75));
    assert!(h.no_gesture());
}

#[test]
fn dragging_the_fade_handle_sets_the_fade() {
    let mut h = Harness::new(vec![track(TrackKind::Video, vec![solid(1, 0, 100)])]);
    let rect = h.rect(ClipId(1));
    let from = egui::pos2(rect.left() + 1.0, rect.top() + 5.0);
    h.drag(from, egui::vec2(20.0 * PX_PER_FRAME, 0.0));
    assert_eq!(h.clip(0, ClipId(1)).fade_in, 20);
    assert!(h.no_gesture());
}

#[test]
fn dragging_a_transition_handle_sets_its_duration() {
    let mut clip = solid(1, 0, 100);
    clip.effects.transition_in = Some(push(10));
    let mut h = Harness::new(vec![track(TrackKind::Video, vec![clip])]);
    let rect = h.rect(ClipId(1));
    let from = egui::pos2(rect.left() + 10.0 * PX_PER_FRAME, rect.bottom() - 5.0);
    h.drag(from, egui::vec2(20.0 * PX_PER_FRAME, 0.0));
    let duration = h
        .clip(0, ClipId(1))
        .effects
        .transition_in
        .as_ref()
        .unwrap()
        .duration;
    assert_eq!(duration, 30);
    assert!(h.no_gesture());
}

#[test]
fn dragging_a_crossing_handle_sets_its_duration_on_both_sides() {
    let mut video = track(TrackKind::Video, vec![solid(1, 0, 100), solid(2, 100, 100)]);
    video.crossings.push(CrossTransition {
        left_clip: ClipId(1),
        right_clip: ClipId(2),
        transition: push(20),
    });
    let mut h = Harness::new(vec![video]);
    let rect = h.rect(ClipId(1));
    // The left half of the window (10 frames) ends inside the left clip.
    let from = egui::pos2(rect.right() - 10.0 * PX_PER_FRAME, rect.bottom() - 5.0);
    h.drag(from, egui::vec2(-10.0 * PX_PER_FRAME, 0.0));
    let crossing = &h.project.timelines[h.timeline_id].tracks[0].crossings[0];
    assert_eq!(crossing.transition.duration, 40);
    assert!(h.no_gesture());
}

#[test]
fn dragging_the_volume_line_sets_the_gain_in_one_undo_step() {
    let mut h = Harness::new(vec![
        track(TrackKind::Video, Vec::new()),
        track(TrackKind::Audio, vec![solid(1, 0, 100)]),
    ]);
    let from = h.rect(ClipId(1)).center();
    h.drag(from, egui::vec2(0.0, -10.0));
    assert!(h.clip(1, ClipId(1)).effects.gain_db.default > 0.0);
    assert!(h.no_gesture());
    h.history.undo(&mut h.project);
    assert_eq!(h.clip(1, ClipId(1)).effects.gain_db.default, 0.0);
}

#[test]
fn a_marquee_from_an_empty_area_selects_the_clips_it_touches() {
    let mut h = Harness::new(vec![track(
        TrackKind::Video,
        vec![solid(1, 0, 40), solid(2, 60, 40)],
    )]);
    let first = h.rect(ClipId(1));
    let second = h.rect(ClipId(2));
    let from = egui::pos2(second.right() + 30.0, second.center().y);
    h.drag(
        from,
        egui::pos2(second.center().x, first.center().y + 1.0) - from,
    );
    assert_eq!(h.state.selected, BTreeSet::from([(0, ClipId(2))]));
    assert!(h.no_gesture());
}

/// The gesture state must outlive the whole draw loop of the release frame:
/// cleared as soon as the dragged clip handles its release, a linked clip
/// drawn after it would flash back to its old place for one frame.
#[test]
fn a_linked_clip_drawn_after_the_dragged_one_does_not_flash_back_on_release() {
    let group = Some(vv_core::LinkGroupId(0));
    let mut video = solid(1, 0, 100);
    video.linked_group = group;
    let mut audio = solid(2, 0, 100);
    audio.linked_group = group;
    let mut h = Harness::new(vec![
        track(TrackKind::Video, vec![video]),
        track(TrackKind::Audio, vec![audio]),
    ]);
    let from = h.rect(ClipId(1)).center();
    h.drag(from, egui::vec2(50.0 * PX_PER_FRAME, 0.0));
    assert_eq!(h.clip(1, ClipId(2)).timeline_start, 50);
    assert_eq!(h.release_rects[&ClipId(2)].left(), h.rect(ClipId(2)).left());
}

/// A video track with a 25 fps media clip of 100 frames at 0 and a solid
/// right after it.
fn media_harness() -> Harness {
    let mut h = Harness::new(vec![track(TrackKind::Video, Vec::new())]);
    let media = h.project.media_pool.insert(vv_core::MediaItem {
        path: "a.mp4".into(),
        meta: vv_core::MediaMeta {
            duration_frames: 1000,
            fps: Rational::new(25, 1),
            width: 1920,
            height: 1080,
            has_video: true,
            has_audio: false,
            sample_rate: 0,
            channels: 0,
            audio_streams: 0,
            file: Default::default(),
        },
        content_hash: 1,
        compound: None,
        folder: None,
    });
    let tracks = &mut h.project.timelines[h.timeline_id].tracks;
    tracks[0].clips = vec![
        Clip::from_source_range(
            ClipId(1),
            ClipSource::Media(media),
            0,
            100,
            0,
            Rational::one(),
        ),
        solid(2, 100, 20),
    ];
    h.frame(Vec::new());
    h
}

/// `media_harness` with the clip showing source frames 200..300 and the
/// slip tool active.
fn slip_harness() -> Harness {
    let mut h = media_harness();
    h.project.timelines[h.timeline_id].tracks[0].clips[0].source_offset = 200;
    h.state.tool = TimelineTool::Slip;
    h.frame(Vec::new());
    h
}

#[test]
fn the_slip_tool_slides_the_content_and_keeps_the_clip_in_place() {
    let mut h = slip_harness();
    let from = h.rect(ClipId(1)).center();
    h.drag(from, egui::vec2(-50.0 * PX_PER_FRAME, 0.0));
    let clip = h.clip(0, ClipId(1));
    assert_eq!(
        (clip.timeline_start, clip.timeline_len, clip.source_in()),
        (0, 100, 250)
    );
    assert_eq!(h.history.position(), 1);
    assert!(h.no_gesture());
}

#[test]
fn the_slip_tool_slips_from_the_edges_too_and_stops_at_the_first_frame() {
    let mut h = slip_harness();
    let rect = h.rect(ClipId(1));
    h.drag(
        egui::pos2(rect.left() + 2.0, rect.center().y),
        egui::vec2(300.0 * PX_PER_FRAME, 0.0),
    );
    let clip = h.clip(0, ClipId(1));
    assert_eq!(
        (clip.timeline_start, clip.timeline_len, clip.source_in()),
        (0, 100, 0)
    );
}

#[test]
fn the_slip_tool_leaves_generators_alone() {
    let mut h = slip_harness();
    let from = h.rect(ClipId(2)).center();
    h.drag(from, egui::vec2(-10.0 * PX_PER_FRAME, 0.0));
    assert_eq!(h.clip(0, ClipId(2)).timeline_start, 100);
    assert_eq!(h.history.position(), 0);
}

#[test]
fn with_the_retime_bar_the_right_edge_changes_the_speed() {
    let mut h = media_harness();
    h.state.retime_controls.insert(ClipId(1));
    h.frame(Vec::new());
    let rect = h.rect(ClipId(1));
    h.drag(
        egui::pos2(rect.right() - 2.0, rect.bottom() - 8.0),
        egui::vec2(-50.0 * PX_PER_FRAME, 0.0),
    );
    let clip = h.clip(0, ClipId(1));
    assert_eq!((clip.speed(), clip.timeline_len), (Rational::new(2, 1), 50));
    assert_eq!(clip.source_in(), 0);
    assert_eq!(
        h.clip(0, ClipId(2)).timeline_start,
        100,
        "a gap, as for a trim"
    );
    assert!(h.no_gesture());
}

/// Lengthening over the next clip cuts it, as a trim does.
#[test]
fn slowing_down_with_the_retime_bar_overwrites_the_next_clip() {
    let mut h = media_harness();
    h.state.retime_controls.insert(ClipId(1));
    h.frame(Vec::new());
    let rect = h.rect(ClipId(1));
    h.drag(
        egui::pos2(rect.right() - 2.0, rect.bottom() - 8.0),
        egui::vec2(10.0 * PX_PER_FRAME, 0.0),
    );
    let clip = h.clip(0, ClipId(1));
    assert_eq!(
        (clip.speed(), clip.timeline_len),
        (Rational::new(10, 11), 110)
    );
    let next = h.clip(0, ClipId(2));
    assert_eq!((next.timeline_start, next.timeline_len), (110, 10));
    h.history.undo(&mut h.project);
    assert_eq!(h.clip(0, ClipId(2)).timeline_start, 100, "one undo step");
    assert_eq!(h.clip(0, ClipId(1)).timeline_len, 100);
}

#[test]
fn the_retime_bar_closes_with_its_x() {
    let mut h = media_harness();
    h.state.retime_controls.insert(ClipId(1));
    h.frame(Vec::new());
    let rect = h.rect(ClipId(1));
    let x = egui::pos2(rect.right() - 7.0, rect.top() + 7.0);
    let button = |pressed| egui::Event::PointerButton {
        pos: x,
        button: egui::PointerButton::Primary,
        pressed,
        modifiers: egui::Modifiers::NONE,
    };
    h.frame(vec![egui::Event::PointerMoved(x)]);
    h.frame(vec![button(true)]);
    h.frame(vec![button(false)]);
    assert!(h.state.retime_controls.is_empty());
    assert_eq!(h.clip(0, ClipId(1)).speed(), Rational::one());
}

/// Reported bug: an already retimed clip stopped a few frames short of
/// where the edge was released.
#[test]
fn a_retimed_clip_ends_exactly_where_its_edge_is_released() {
    for frames in [3, 7, 13, 20, 31, -9, -17] {
        let mut h = media_harness();
        {
            let clip = h.project.timelines[h.timeline_id].tracks[0]
                .clip_mut(ClipId(1))
                .unwrap();
            clip.set_speed(Rational::from_percent(198.97), Rational::one());
        }
        let tracks = &mut h.project.timelines[h.timeline_id].tracks;
        tracks[0].clips.retain(|c| c.id == ClipId(1));
        h.state.retime_controls.insert(ClipId(1));
        h.frame(Vec::new());
        let original_end = h.clip(0, ClipId(1)).timeline_end();
        let rect = h.rect(ClipId(1));
        h.drag(
            egui::pos2(rect.right() - 2.0, rect.bottom() - 8.0),
            egui::vec2(frames as f32 * PX_PER_FRAME, 0.0),
        );
        assert_eq!(
            h.clip(0, ClipId(1)).timeline_end(),
            original_end + frames,
            "dragged by {frames} frames"
        );
    }
}

/// As above with a conformed clip (60 fps media on the 25 fps timeline)
/// starting deep into its source.
#[test]
fn a_conformed_retimed_clip_ends_exactly_where_its_edge_is_released() {
    for frames in [3, 7, 13, 20, 31, -9, -17] {
        let mut h = media_harness();
        let conform = Rational::conform_rate(Rational::new(25, 1), Rational::new(60, 1));
        {
            let media = match h.clip(0, ClipId(1)).source {
                ClipSource::Media(id) => id,
                _ => unreachable!(),
            };
            let item = &mut h.project.media_pool[media];
            item.meta.fps = Rational::new(60, 1);
            item.meta.duration_frames = 40_000;
            let tracks = &mut h.project.timelines[h.timeline_id].tracks;
            let mut clip = Clip::from_source_range(
                ClipId(1),
                ClipSource::Media(media),
                20_011,
                20_251,
                0,
                conform,
            );
            clip.set_speed(Rational::from_percent(198.97), conform);
            tracks[0].clips = vec![clip];
        }
        h.state.retime_controls.insert(ClipId(1));
        h.frame(Vec::new());
        let original_end = h.clip(0, ClipId(1)).timeline_end();
        let rect = h.rect(ClipId(1));
        h.drag(
            egui::pos2(rect.right() - 2.0, rect.bottom() - 8.0),
            egui::vec2(frames as f32 * PX_PER_FRAME, 0.0),
        );
        assert_eq!(
            h.clip(0, ClipId(1)).timeline_end(),
            original_end + frames,
            "dragged by {frames} frames"
        );
    }
}

fn marker(id: u64, start: FrameIdx, duration: FrameIdx) -> vv_core::Marker {
    vv_core::Marker {
        id: vv_core::MarkerId(id),
        start,
        duration,
        note: String::new(),
        color: vv_core::Marker::default_color(),
    }
}

fn marker_harness(markers: Vec<vv_core::Marker>) -> Harness {
    Harness::new(vec![track(TrackKind::Video, vec![solid(1, 0, 200)])]).with_markers(markers)
}

#[test]
fn dragging_a_marker_moves_it_in_one_undo_step() {
    let mut h = marker_harness(vec![marker(0, 20, 0)]);
    let from = h.marker_pos(20);
    h.drag_marker(from, 30.0);
    assert_eq!(h.markers(), [(50, 0)]);
    assert_eq!(h.history.position(), 1);
    assert!(h.state.marker_drag.is_none());
    h.history.undo(&mut h.project);
    assert_eq!(h.markers(), [(20, 0)]);
}

#[test]
fn alt_dragging_a_marker_stretches_it_into_a_range() {
    let mut h = marker_harness(vec![marker(0, 40, 0)]);
    h.modifiers = egui::Modifiers::ALT;
    let from = h.marker_pos(40);
    h.drag_marker(from, 25.0);
    assert_eq!(h.markers(), [(40, 25)]);

    // Leftwards the dragged end becomes the start.
    let mut h = marker_harness(vec![marker(0, 40, 0)]);
    h.modifiers = egui::Modifiers::ALT;
    let from = h.marker_pos(40);
    h.drag_marker(from, -15.0);
    assert_eq!(h.markers(), [(25, 15)]);
}

#[test]
fn dragging_the_ends_of_a_range_marker_resizes_it() {
    let mut h = marker_harness(vec![marker(0, 20, 40)]);
    let from = h.marker_pos(60) - egui::vec2(1.0, 0.0);
    h.drag_marker(from, -10.0);
    assert_eq!(h.markers(), [(20, 30)]);

    let from = h.marker_pos(20) + egui::vec2(1.0, 0.0);
    h.drag_marker(from, 8.0);
    assert_eq!(h.markers(), [(28, 22)]);

    let from = h.marker_pos(39);
    h.drag_marker(from, 10.0);
    assert_eq!(h.markers(), [(38, 22)], "the middle moves it whole");
}

#[test]
fn clicking_a_marker_moves_the_playhead_to_it() {
    let mut h = marker_harness(vec![marker(0, 30, 0)]);
    let pos = h.marker_pos(30) + egui::vec2(3.0, 0.0);
    h.click(pos);
    assert_eq!(h.state.playhead, 30);
}

#[test]
fn the_marker_editor_saves_on_ctrl_enter_and_discards_on_escape() {
    let mut h = marker_harness(vec![marker(0, 30, 0)]);
    let pos = h.marker_pos(30);
    h.click(pos);
    h.click(pos);
    let editor = h
        .state
        .marker_editor
        .as_mut()
        .expect("double click opens it");
    assert_eq!(editor.id, vv_core::MarkerId(0));
    editor.note = "retake".into();
    editor.color = vv_core::ClipColor::Cyan;
    let key = |key, modifiers| egui::Event::Key {
        key,
        physical_key: None,
        pressed: true,
        repeat: false,
        modifiers,
    };
    h.frame(vec![key(egui::Key::Enter, egui::Modifiers::COMMAND)]);
    h.frame(Vec::new());
    assert!(h.state.marker_editor.is_none());
    let saved = &h.project.timelines[h.timeline_id].markers[0];
    assert_eq!(
        (saved.note.as_str(), saved.color),
        ("retake", vv_core::ClipColor::Cyan)
    );

    h.time += 1.0; // not a triple click
    h.click(pos);
    h.click(pos);
    h.state.marker_editor.as_mut().unwrap().note = "discarded".into();
    h.frame(vec![key(egui::Key::Escape, egui::Modifiers::NONE)]);
    assert!(h.state.marker_editor.is_none());
    assert_eq!(h.project.timelines[h.timeline_id].markers[0].note, "retake");
}

/// Double click on a compound clip: it opens at the frame of it under the
/// playhead, or at its nearest edge when the playhead is outside it.
#[test]
fn a_compound_clip_opens_at_the_frame_under_the_playhead() {
    let mut h = Harness::new(vec![track(TrackKind::Video, Vec::new())]);
    let nested = h.project.timelines.insert(vv_core::Timeline {
        name: "Nested".into(),
        fps: Rational::new(25, 1),
        resolution: (1920, 1080),
        tracks: vec![Track::new(TrackKind::Video)],
        markers: Vec::new(),
        master: Default::default(),
    });
    let media = h.project.media_pool.insert(vv_core::MediaItem {
        path: "Compound Clip 1".into(),
        meta: vv_core::MediaMeta {
            duration_frames: 1000,
            fps: Rational::new(25, 1),
            width: 1920,
            height: 1080,
            has_video: true,
            has_audio: false,
            sample_rate: 0,
            channels: 0,
            audio_streams: 0,
            file: Default::default(),
        },
        content_hash: 1,
        compound: Some(nested),
        folder: None,
    });
    let mut clip = Clip::from_source_range(
        ClipId(1),
        ClipSource::Media(media),
        0,
        100,
        0,
        Rational::one(),
    );
    clip.source_offset = 200;
    h.project.timelines[h.timeline_id].tracks[0].clips = vec![clip];
    h.frame(Vec::new());
    let center = h.rect(ClipId(1)).center();

    h.state.playhead = 40;
    h.click(center);
    h.click(center);
    assert_eq!(h.entered, Some((nested, ClipId(1), 240)));

    h.time += 1.0;
    h.state.playhead = 150;
    h.click(center);
    h.click(center);
    assert_eq!(h.entered, Some((nested, ClipId(1), 299)));
}
