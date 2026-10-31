use super::*;

#[test]
fn a_keyboard_reveal_discards_wheel_smoothing_until_fresh_input() {
    let ctx = egui::Context::default();
    ctx.options_mut(|options| {
        options.input_options.horizontal_scroll_modifier = egui::Modifiers::CTRL;
        options.input_options.vertical_scroll_modifier = egui::Modifiers::ALT;
        options.input_options.zoom_modifier = egui::Modifiers::SHIFT;
    });
    let scroll_id = egui::Id::new("keyboard_wheel_smoothing");
    let mut state = TimelineState::default();
    state.pixels_per_sec = 25.0;
    state.last_rendered_pps = 25.0;
    state.playhead = 100;
    let panel = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(800.0, 400.0));
    let mut scroll = egui::containers::scroll_area::State::default();
    scroll.offset.x = 100.0;
    scroll.store(&ctx, scroll_id);
    let wheel = || egui::Event::MouseWheel {
        unit: egui::MouseWheelUnit::Line,
        delta: egui::vec2(-5.0, 0.0),
        modifiers: egui::Modifiers::NONE,
        phase: egui::TouchPhase::Move,
    };
    for frame in 0..9 {
        let mut input = egui::RawInput::default();
        input.screen_rect = Some(panel);
        input.time = Some(frame as f64 / 60.0);
        input.events = match frame {
            0 | 7 => vec![egui::Event::PointerMoved(panel.center()), wheel()],
            2 | 5 | 8 => vec![egui::Event::MouseWheel {
                unit: egui::MouseWheelUnit::Line,
                delta: egui::vec2(0.0, -5.0),
                modifiers: if frame == 8 {
                    egui::Modifiers::CTRL
                } else {
                    egui::Modifiers::NONE
                },
                phase: egui::TouchPhase::Move,
            }],
            3 => vec![egui::Event::MouseWheel {
                unit: egui::MouseWheelUnit::Line,
                delta: egui::vec2(-5.0, 0.0),
                modifiers: egui::Modifiers::ALT,
                phase: egui::TouchPhase::Move,
            }],
            4 => vec![egui::Event::MouseWheel {
                unit: egui::MouseWheelUnit::Line,
                delta: egui::vec2(-5.0, 0.0),
                modifiers: egui::Modifiers::SHIFT,
                phase: egui::TouchPhase::Move,
            }],
            _ => Vec::new(),
        };
        let mut output = ctx.run_ui(input, |ui| {
            if frame == 0 {
                return;
            }
            if frame == 1 {
                state.reveal_playhead = true;
                assert!(ui.input(|i| i.smooth_scroll_delta.x != 0.0));
            }
            if frame == 2 || frame == 3 || frame == 5 {
                assert!(ui.input(|i| i.smooth_scroll_delta.y != 0.0));
            }
            sync_timeline_scroll(
                &ctx, scroll_id, &mut state, 25.0, 1.0, 200.0, 2000.0, false, panel, true,
            );
            let offset = egui::containers::scroll_area::State::load(&ctx, scroll_id)
                .unwrap()
                .offset
                .x;
            if frame < 7 {
                assert_eq!(
                    offset, 100.0,
                    "wheel smoothing undid the reveal in frame {frame}"
                );
                assert_eq!(state.hscroll_vel, 0.0);
            } else {
                assert!(offset > 100.0, "fresh wheel input must still scroll");
                assert!(state.hscroll_vel > 0.0);
            }
        });
        output.textures_delta.clear();
    }
}

#[test]
fn the_timeline_reveals_keyboard_steps_beyond_its_last_clip() {
    let mut app = crate::VenturiApp::default();
    let timeline = app.ensure_timeline();
    let id = app.session.project.alloc_clip_id();
    app.session.project.timelines[timeline].tracks[0]
        .clips
        .push(Clip::from_source_range(
            id,
            vv_core::ClipSource::SolidColor,
            0,
            100,
            0,
            vv_core::Rational::one(),
        ));
    app.scrub_audio = false;
    let fps = app.session.project.timelines[timeline].fps.as_f64() as f32;
    app.timeline_state.pixels_per_sec = fps;
    app.timeline_state.last_rendered_pps = fps;
    let ctx = egui::Context::default();
    let mut scroll_id = None;
    for frame in 0..5 {
        let mut input = egui::RawInput::default();
        input.screen_rect = Some(egui::Rect::from_min_size(
            egui::Pos2::ZERO,
            egui::vec2(800.0, 600.0),
        ));
        if frame == 2 {
            app.timeline_state.playhead = 5000;
            app.step_playhead_with_arrows(Some(1), 0.0);
        }
        let mut output = ctx.run_ui(input, |ui| {
            egui::CentralPanel::default().show(ui, |ui| {
                if frame == 1 {
                    ui.horizontal_top(|ui| {
                        scroll_id =
                            Some(ui.make_persistent_id(egui::IdSalt::new("timeline_scroll")));
                    });
                } else {
                    show_timeline(
                        ui,
                        &mut app.session.project,
                        &mut app.session.history,
                        timeline,
                        &|_| "media".to_string(),
                        &mut app.timeline_state,
                        true,
                        true,
                        &[],
                        &[],
                        &[],
                        &std::collections::HashMap::new(),
                        false,
                    );
                }
            });
        });
        output.textures_delta.clear();
        if frame >= 2 {
            let offset = egui::containers::scroll_area::State::load(&ctx, scroll_id.unwrap())
                .unwrap()
                .offset
                .x;
            let x = app.timeline_state.playhead as f32;
            assert!(
                x >= offset && x <= offset + 800.0 - TRACK_HEADER_WIDTH,
                "playhead {x} beyond last clip is outside viewport at {offset}"
            );
        }
    }
}

#[test]
fn keyboard_steps_keep_the_playhead_visible_after_pan_inertia() {
    let mut app = crate::VenturiApp::default();
    let timeline = app.ensure_timeline();
    let id = app.session.project.alloc_clip_id();
    app.session.project.timelines[timeline].tracks[0]
        .clips
        .push(vv_core::Clip::from_source_range(
            id,
            vv_core::ClipSource::SolidColor,
            0,
            1000,
            0,
            vv_core::Rational::one(),
        ));
    app.scrub_audio = false;
    let fps = app.session.project.timelines[timeline].fps.as_f64();
    let ctx = egui::Context::default();
    ctx.input_mut(|input| input.stable_dt = 0.05);
    let scroll_id = egui::Id::new("keyboard_follow_regression");
    let viewport = 200.0;
    for (direction, playhead, offset) in [(1, 600, 0.0), (-1, 300, 500.0)] {
        let mut scroll = egui::containers::scroll_area::State::default();
        scroll.offset.x = offset;
        scroll.store(&ctx, scroll_id);
        app.arrow_hold = None;
        app.timeline_state.playhead = playhead;
        app.timeline_state.pixels_per_sec = fps as f32;
        app.timeline_state.last_rendered_pps = fps as f32;
        app.timeline_state.hscroll_vel = direction as f32 * 20_000.0;
        app.step_playhead_with_arrows(Some(direction), 0.0);
        sync_timeline_scroll(
            &ctx,
            scroll_id,
            &mut app.timeline_state,
            fps,
            1.0,
            viewport,
            1000.0,
            false,
            egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(viewport, 100.0)),
            true,
        );
        let scroll = egui::containers::scroll_area::State::load(&ctx, scroll_id).unwrap();
        let x = app.timeline_state.playhead as f32;
        assert!(
            x >= scroll.offset.x && x <= scroll.offset.x + viewport,
            "playhead {x} outside {}..{}",
            scroll.offset.x,
            scroll.offset.x + viewport
        );
    }
}

/// Reported bug: splitting an audio clip (`SplitClip`) visibly
/// moved the drawn waveform exactly at the cut
/// point, because each half rounded its own bins to its own edges.
/// With `waveform_bin_for_column` computed from the *absolute* position
/// in the audio time, the same instant must always map to the same
/// bin both before and after the split.
#[test]
fn waveform_bin_for_column_is_continuous_across_a_clip_split() {
    // Realistic values from a real case (bbb_sunflower): video fps and
    // audio duration of the *media* slightly different from the duration of the
    // video, the cause of the rounding the bug exposed.
    let media_fps = 60.0_f64;
    let full_source_out: FrameIdx = 38074;
    let audio_duration_secs = 634.144; // slightly < 38074/60.0
    let num_peaks = 63457;

    let split_at: FrameIdx = 3120; // 52s at 60fps

    for probe_secs in [51.5, 51.9, 52.0, 52.1, 52.5, 53.0] {
        // Bin according to the whole (unsplit) clip: source_in=0,
        // source_out=full_source_out.
        let whole_clip_start = 0.0_f64;
        let whole_clip_end = full_source_out as f64 / media_fps;
        let frac_whole = probe_secs / whole_clip_end;
        let bin_whole = waveform_bin_for_column(
            frac_whole,
            whole_clip_start,
            whole_clip_end,
            audio_duration_secs,
            num_peaks,
        );

        // Same instant, but from the half (left or right) produced by
        // a split at `split_at`.
        let (clip_start_frame, clip_end_frame) = if probe_secs * media_fps < split_at as f64 {
            (0, split_at)
        } else {
            (split_at, full_source_out)
        };
        let clip_start_secs = clip_start_frame as f64 / media_fps;
        let clip_end_secs = clip_end_frame as f64 / media_fps;
        let frac_half = (probe_secs - clip_start_secs) / (clip_end_secs - clip_start_secs);
        let bin_half = waveform_bin_for_column(
            frac_half,
            clip_start_secs,
            clip_end_secs,
            audio_duration_secs,
            num_peaks,
        );

        assert_eq!(
            bin_whole, bin_half,
            "at t={probe_secs}s the whole clip picks bin {bin_whole} but the half after the split picks {bin_half}: the waveform would shift at the cut"
        );
    }
}

/// It must stay among the first candidates (1-2-5) at very low zoom, and
/// rise enough to keep the ticks readable even at very
/// high zoom — not a fixed value whatever `pixels_per_sec` is.
#[test]
fn nice_tick_interval_secs_grows_with_zoom_to_keep_ticks_readable() {
    assert_eq!(nice_tick_interval_secs(800.0), 1.0);
    assert_eq!(nice_tick_interval_secs(60.0), 2.0);
    assert_eq!(nice_tick_interval_secs(10.0), 10.0);
    assert_eq!(nice_tick_interval_secs(0.1), 900.0);
}

#[test]
fn format_timecode_includes_frames_and_hours() {
    // 25 fps: 5 seconds = frame 125, shows HH:MM:SS:FF
    assert_eq!(format_timecode(5.0, 25.0), "00:00:05:00");
    // 65 seconds = 1 min 5 sec
    assert_eq!(format_timecode(65.0, 25.0), "00:01:05:00");
    // 3665 seconds = 1h 1m 5s
    assert_eq!(format_timecode(3665.0, 25.0), "01:01:05:00");
    // With a fraction of a second: 0.4s at 25fps = frame 10
    assert_eq!(format_timecode(5.4, 25.0), "00:00:05:10");
    // 29.97: 30 frames per nominal second, not 29.
    assert_eq!(
        format_timecode(1799.0 / (30_000.0 / 1001.0), 30_000.0 / 1001.0),
        "00:00:59:29"
    );
}

#[test]
fn format_duration_is_seconds_and_leftover_frames() {
    assert_eq!(format_duration(0, 10.0), "0:00");
    // 10 frames at 10fps = exactly 1s, no remaining frames.
    assert_eq!(format_duration(10, 10.0), "1:00");
    // 14 frames at 10fps = 1s + 4 frames.
    assert_eq!(format_duration(14, 10.0), "1:04");
    assert_eq!(format_duration(93, 30.0), "3:03");
}

#[test]
fn fade_zone_at_only_matches_the_top_band_near_the_handle() {
    let clip_rect = egui::Rect::from_min_size(egui::pos2(100.0, 20.0), egui::vec2(200.0, 40.0));
    let fade_in_x = 120.0; // fade-in handle 20px from the left edge
    let fade_out_x = 280.0; // fade-out handle 20px from the right edge
    assert_eq!(
        fade_zone_at(egui::pos2(120.0, 22.0), clip_rect, fade_in_x, fade_out_x),
        Some(FadeEdge::In)
    );
    assert_eq!(
        fade_zone_at(egui::pos2(280.0, 22.0), clip_rect, fade_in_x, fade_out_x),
        Some(FadeEdge::Out)
    );
    // Same X as the fade-in handle, but below the top band: it is trim/roll, not fade.
    assert_eq!(
        fade_zone_at(egui::pos2(120.0, 50.0), clip_rect, fade_in_x, fade_out_x),
        None
    );
    // Far from both handles.
    assert_eq!(
        fade_zone_at(egui::pos2(200.0, 22.0), clip_rect, fade_in_x, fade_out_x),
        None
    );
}

#[test]
fn edge_drag_value_moves_in_opposite_screen_directions_for_in_and_out() {
    let px_per_frame = 2.0;
    let drag = |edge, accum_px| EdgeDragState {
        clip_id: ClipId(1),
        track_index: 0,
        edge,
        original_value: 10,
        accum_px,
    };
    // 20px to the right = 10 frames: the fade-in grows, the fade-out (its
    // handle approaching the corner) shrinks.
    assert_eq!(
        edge_drag_value(&drag(FadeEdge::In, 20.0), 100, px_per_frame, 0),
        20
    );
    assert_eq!(
        edge_drag_value(&drag(FadeEdge::Out, 20.0), 100, px_per_frame, 0),
        0
    );
    // Clamped to the duration of the clip.
    assert_eq!(
        edge_drag_value(&drag(FadeEdge::In, 1000.0), 30, px_per_frame, 0),
        30
    );
}

/// A fade can be dragged away to nothing, a transition keeps a frame.
#[test]
fn edge_drag_value_never_goes_below_min() {
    let drag = EdgeDragState {
        clip_id: ClipId(1),
        track_index: 0,
        edge: FadeEdge::In,
        original_value: 10,
        accum_px: -1000.0,
    };
    assert_eq!(edge_drag_value(&drag, 100, 2.0, 0), 0);
    assert_eq!(edge_drag_value(&drag, 100, 2.0, 1), 1);
}

#[test]
fn crossing_drag_value_grows_when_the_grabbed_extremity_moves_away_from_the_cut() {
    let px_per_frame = 2.0;
    // Left end dragged further left (away from the
    // cut, towards the inside of the left clip): the crossing
    // lengthens, by twice the frames moved (it grows on both
    // sides together).
    let left_extends = CrossingDragState {
        track_index: 0,
        left_clip: ClipId(1),
        grabbed_left_side: true,
        original_duration: 10,
        max_duration: 100,
        accum_px: -20.0, // 20px to the left = 10 frames
    };
    assert_eq!(crossing_drag_value(&left_extends, px_per_frame), 30);
    // Same displacement in pixels but on the right side, to the right:
    // same effect, it moves away from the cut in the opposite direction.
    let right_extends = CrossingDragState {
        track_index: 0,
        left_clip: ClipId(1),
        grabbed_left_side: false,
        original_duration: 10,
        max_duration: 100,
        accum_px: 20.0,
    };
    assert_eq!(crossing_drag_value(&right_extends, px_per_frame), 30);
    // Dragging the left end to the right (towards the cut)
    // shortens it, clamped to a minimum of 1 frame.
    let shrinking = CrossingDragState {
        track_index: 0,
        left_clip: ClipId(1),
        grabbed_left_side: true,
        original_duration: 10,
        max_duration: 100,
        accum_px: 20.0,
    };
    assert_eq!(crossing_drag_value(&shrinking, px_per_frame), 1);
    // Clamped to the maximum allowed by the two clips involved.
    let far = CrossingDragState {
        track_index: 0,
        left_clip: ClipId(1),
        grabbed_left_side: true,
        original_duration: 10,
        max_duration: 40,
        accum_px: -1000.0,
    };
    assert_eq!(crossing_drag_value(&far, px_per_frame), 40);
}

#[test]
fn gain_offset_is_zero_at_zero_db_and_reaches_the_edges_at_the_range_extremes() {
    assert_eq!(gain_offset(0.0), 0.0);
    assert_eq!(gain_offset(vv_core::GAIN_DB_MAX), 1.0);
    assert_eq!(gain_offset(vv_core::GAIN_DB_MIN), -1.0);
    // Past the extremes it stays clamped, it does not exceed [-1, 1].
    assert_eq!(gain_offset(vv_core::GAIN_DB_MAX + 10.0), 1.0);
    assert_eq!(gain_offset(vv_core::GAIN_DB_MIN - 10.0), -1.0);
}

#[test]
fn gain_from_offset_is_the_inverse_of_gain_offset() {
    for db in [
        vv_core::GAIN_DB_MIN,
        -50.0,
        -6.0,
        0.0,
        6.0,
        vv_core::GAIN_DB_MAX,
    ] {
        assert!(
            (gain_from_offset(gain_offset(db)) - db).abs() < 1e-4,
            "db={db}"
        );
    }
}

#[test]
fn volume_drag_value_follows_the_pointer_and_clamps_at_the_range_extremes() {
    let half_height = 18.0; // (ROW_HEIGHT - 4.0) / 2.0
    let group = History::default().begin_group();
    let drag = |accum_px| VolumeDragState {
        clip_id: ClipId(1),
        track_index: 0,
        original_db: 0.0,
        accum_px,
        group,
    };
    // At 0 dB the line is at the center: dragging upwards (negative
    // accum_px) raises the gain, downwards lowers it.
    assert!(volume_drag_value(&drag(-half_height), half_height) > 0.0);
    assert!(volume_drag_value(&drag(half_height), half_height) < 0.0);
    // Past the available run it clamps to the extremes of the range.
    assert_eq!(
        volume_drag_value(&drag(-half_height * 10.0), half_height),
        vv_core::GAIN_DB_MAX
    );
    assert_eq!(
        volume_drag_value(&drag(half_height * 10.0), half_height),
        vv_core::GAIN_DB_MIN
    );
}

/// "Identity" `row_y` (no grouping/margin) for the tests.
fn test_row_y(n: usize) -> Vec<f32> {
    (0..n)
        .map(|i| RULER_HEIGHT + i as f32 * ROW_HEIGHT)
        .collect()
}

fn visual(track_index: usize, id: u64, start: FrameIdx, len: FrameIdx) -> ClipVisual<'static> {
    ClipVisual {
        track_index,
        clip: std::borrow::Cow::Owned(Clip::from_source_range(
            ClipId(id),
            vv_core::ClipSource::SolidColor,
            0,
            len,
            start,
            vv_core::Rational::one(),
        )),
        label: String::new(),
        color: egui::Color32::WHITE,
        locked: false,
        muted: false,
    }
}

fn visual_linked(
    track_index: usize,
    id: u64,
    start: FrameIdx,
    len: FrameIdx,
    group: u64,
) -> ClipVisual<'static> {
    let mut v = visual(track_index, id, start, len);
    v.clip.to_mut().linked_group = Some(vv_core::LinkGroupId(group));
    v
}

#[test]
fn expand_to_linked_groups_includes_the_whole_group() {
    let visuals = vec![
        visual_linked(0, 1, 0, 10, 100),
        visual_linked(1, 2, 0, 10, 100),
    ];
    assert_eq!(
        expand_to_linked_groups(&visuals, [(0, ClipId(1))]),
        BTreeSet::from([(0, ClipId(1)), (1, ClipId(2))]),
        "selecting the video must extend to the linked audio too"
    );
    assert_eq!(
        expand_to_linked_groups(&visuals, [(1, ClipId(2))]),
        BTreeSet::from([(0, ClipId(1)), (1, ClipId(2))]),
        "and vice versa, starting from the audio"
    );
}

#[test]
fn expand_to_linked_groups_is_a_noop_for_unlinked_clips() {
    let visuals = vec![visual(0, 1, 0, 10)];
    assert_eq!(
        expand_to_linked_groups(&visuals, [(0, ClipId(1))]),
        BTreeSet::from([(0, ClipId(1))])
    );
    assert_eq!(expand_to_linked_groups(&visuals, []), BTreeSet::new());
}

#[test]
fn expand_to_linked_groups_handles_independent_groups_and_groups_larger_than_two() {
    // A group of 2 and one of 3, independent, both starting
    // points: the whole group of each must show up in the
    // result, not just one partner.
    let visuals = vec![
        visual_linked(0, 1, 0, 10, 100),
        visual_linked(1, 2, 0, 10, 100),
        visual_linked(0, 3, 20, 10, 200),
        visual_linked(1, 4, 20, 10, 200),
        visual_linked(2, 5, 20, 10, 200),
    ];
    let result = expand_to_linked_groups(&visuals, [(0, ClipId(1)), (0, ClipId(3))]);
    assert_eq!(
        result,
        BTreeSet::from([
            (0, ClipId(1)),
            (1, ClipId(2)),
            (0, ClipId(3)),
            (1, ClipId(4)),
            (2, ClipId(5)),
        ])
    );
}

/// Reported bug: with CTRL+click/rubber band I selected 2+ clips *not*
/// linked to each other, then dragging one the others did not follow —
/// the drag looked only at the linked group of the clicked clip,
/// ignoring the rest of the selection.
#[test]
fn drag_group_for_follows_the_whole_multi_selection_even_without_a_link() {
    let visuals = vec![
        visual(0, 1, 0, 10),
        visual(1, 2, 30, 10),
        visual(2, 3, 60, 10),
    ];
    // 3 clips not linked to each other, all selected by hand (CTRL+click).
    let selected = BTreeSet::from([(0, ClipId(1)), (1, ClipId(2)), (2, ClipId(3))]);

    // Dragging any one of them, the drag must follow the whole
    // selection — not just it.
    assert_eq!(
        drag_group_for(&selected, &visuals, (1, ClipId(2))),
        selected
    );
}

#[test]
fn drag_group_for_replaces_the_selection_when_dragging_an_unselected_clip() {
    let visuals = vec![visual(0, 1, 0, 10), visual(1, 2, 30, 10)];
    // Previous, unrelated selection: dragging a clip outside
    // it must not drag it along.
    let selected = BTreeSet::from([(0, ClipId(1))]);
    assert_eq!(
        drag_group_for(&selected, &visuals, (1, ClipId(2))),
        BTreeSet::from([(1, ClipId(2))])
    );
}

#[test]
fn drag_group_for_expands_to_the_link_group_when_dragging_an_unselected_linked_clip() {
    let visuals = vec![
        visual_linked(0, 1, 0, 10, 100),
        visual_linked(1, 2, 0, 10, 100),
    ];
    let selected = BTreeSet::new();
    assert_eq!(
        drag_group_for(&selected, &visuals, (0, ClipId(1))),
        BTreeSet::from([(0, ClipId(1)), (1, ClipId(2))])
    );
}

#[test]
fn apply_click_selection_plain_replaces_selection() {
    let visuals = vec![visual(0, 1, 0, 10), visual(0, 2, 20, 10)];
    let current = BTreeSet::from([(0, ClipId(1))]);
    let (selected, anchor) = apply_click_selection(
        &current,
        Some((0, ClipId(1))),
        (0, ClipId(2)),
        ClickModifiers::Plain,
        &visuals,
        10.0,
        &test_row_y(2),
        ROW_HEIGHT,
    );
    assert_eq!(selected, BTreeSet::from([(0, ClipId(2))]));
    assert_eq!(anchor, Some((0, ClipId(2))));
}

#[test]
fn apply_click_selection_toggle_adds_and_removes() {
    let visuals = vec![visual(0, 1, 0, 10), visual(0, 2, 20, 10)];
    let current = BTreeSet::from([(0, ClipId(1))]);
    let (selected, _) = apply_click_selection(
        &current,
        Some((0, ClipId(1))),
        (0, ClipId(2)),
        ClickModifiers::Toggle,
        &visuals,
        10.0,
        &test_row_y(2),
        ROW_HEIGHT,
    );
    assert_eq!(selected, BTreeSet::from([(0, ClipId(1)), (0, ClipId(2))]));

    // Ctrl+click on an already selected clip removes it.
    let (selected2, _) = apply_click_selection(
        &selected,
        Some((0, ClipId(2))),
        (0, ClipId(1)),
        ClickModifiers::Toggle,
        &visuals,
        10.0,
        &test_row_y(2),
        ROW_HEIGHT,
    );
    assert_eq!(selected2, BTreeSet::from([(0, ClipId(2))]));
}

#[test]
fn apply_click_selection_range_selects_bounding_box_from_anchor() {
    // Three clips on the same track: [0,10) [20,30) [40,50). Anchor=1,
    // shift+click on 3 must select the 2 in between too.
    let visuals = vec![
        visual(0, 1, 0, 10),
        visual(0, 2, 20, 10),
        visual(0, 3, 40, 10),
    ];
    let current = BTreeSet::from([(0, ClipId(1))]);
    let (selected, anchor) = apply_click_selection(
        &current,
        Some((0, ClipId(1))),
        (0, ClipId(3)),
        ClickModifiers::Range,
        &visuals,
        1.0,
        &test_row_y(1),
        ROW_HEIGHT,
    );
    assert_eq!(
        selected,
        BTreeSet::from([(0, ClipId(1)), (0, ClipId(2)), (0, ClipId(3))])
    );
    // The anchor does not change with shift+click.
    assert_eq!(anchor, Some((0, ClipId(1))));
}

#[test]
fn apply_click_selection_range_spans_multiple_tracks() {
    let visuals = vec![
        visual(0, 1, 0, 10),  // video, anchor
        visual(1, 2, 0, 10),  // audio, inside the range (same column)
        visual(0, 3, 20, 10), // outside the horizontal range
    ];
    let current = BTreeSet::from([(0, ClipId(1))]);
    let (selected, _) = apply_click_selection(
        &current,
        Some((0, ClipId(1))),
        (1, ClipId(2)),
        ClickModifiers::Range,
        &visuals,
        1.0,
        &test_row_y(2),
        ROW_HEIGHT,
    );
    assert_eq!(selected, BTreeSet::from([(0, ClipId(1)), (1, ClipId(2))]));
}

#[test]
fn apply_click_selection_range_without_prior_anchor_uses_clicked_as_anchor() {
    let visuals = vec![visual(0, 1, 0, 10)];
    let current = BTreeSet::new();
    let (selected, anchor) = apply_click_selection(
        &current,
        None,
        (0, ClipId(1)),
        ClickModifiers::Range,
        &visuals,
        1.0,
        &test_row_y(1),
        ROW_HEIGHT,
    );
    assert_eq!(selected, BTreeSet::from([(0, ClipId(1))]));
    assert_eq!(anchor, Some((0, ClipId(1))));
}

#[test]
fn clips_intersecting_rect_finds_overlapping_clips_only() {
    let visuals = vec![
        visual(0, 1, 0, 10),
        visual(0, 2, 20, 10),
        visual(1, 3, 0, 10),
    ];
    // Rectangle covering only the area of clips 1 and 3 (starting
    // column, both tracks), not 2.
    let row_y = test_row_y(2);
    let rect = clip_local_rect(&visuals[0], 1.0, &row_y, ROW_HEIGHT).union(clip_local_rect(
        &visuals[2],
        1.0,
        &row_y,
        ROW_HEIGHT,
    ));
    let hits: BTreeSet<_> = clips_intersecting_rect(&visuals, 1.0, &row_y, ROW_HEIGHT, rect)
        .into_iter()
        .collect();
    assert_eq!(hits, BTreeSet::from([(0, ClipId(1)), (1, ClipId(3))]));
}

#[test]
fn neighbor_bounds_no_neighbors_is_unbounded() {
    let visuals = vec![visual(0, 1, 10, 5)];
    assert_eq!(
        neighbor_bounds_at(&visuals, 0, &[ClipId(1)], 10, 5),
        (0, FrameIdx::MAX)
    );
}

#[test]
fn neighbor_bounds_clamped_by_prev_and_next_on_same_track() {
    let visuals = vec![
        visual(0, 1, 0, 10),  // ends at 10
        visual(0, 2, 20, 30), // the moving one
        visual(0, 3, 50, 5),  // starts at 50
        visual(1, 4, 15, 3),  // other track: ignored
    ];
    assert_eq!(
        neighbor_bounds_at(&visuals, 0, &[ClipId(2)], 20, 30),
        (10, 50)
    );
}

#[test]
fn max_start_in_slot_keeps_clip_inside_slot() {
    // slot [10, 50), clip of length 30: it can only sit between 10 and 20.
    assert_eq!(0.clamp(10, max_start_in_slot(10, 50, 30)), 10);
    assert_eq!(15.clamp(10, max_start_in_slot(10, 50, 30)), 15);
    assert_eq!(100.clamp(10, max_start_in_slot(10, 50, 30)), 20);
}

#[test]
fn max_start_in_slot_degenerate_slot_does_not_invert_range() {
    // slot smaller than the clip: it must not produce an inverted range.
    assert_eq!(max_start_in_slot(10, 15, 30), 10);
}

#[test]
fn drag_range_matches_neighbor_bounds_minus_own_length() {
    let visuals = vec![
        visual(0, 1, 0, 10),  // ends at 10
        visual(0, 2, 20, 30), // length 30: it can sit between 10 and 50-30=20
        visual(0, 3, 50, 5),
    ];
    assert_eq!(drag_range(&visuals, 0, ClipId(2)), (10, 20));
}

#[test]
fn combined_drag_range_with_no_others_matches_plain_drag_range() {
    let visuals = vec![visual(0, 1, 0, 10), visual(0, 2, 20, 30)];
    let (min, max, followers) = combined_drag_range(&visuals, 0, ClipId(2), &[]);
    assert_eq!((min, max), drag_range(&visuals, 0, ClipId(2)));
    assert!(followers.is_empty());
}

#[test]
fn combined_drag_range_intersects_both_clips_constraints() {
    // Track 0: [0,10) then clip 2 (video, [20,50)).
    // Track 1: its twin (audio, same [20,50)) but with a
    // narrower following neighbor: it ends at 55 instead of free.
    let visuals = vec![
        visual(0, 1, 0, 10),
        visual(0, 2, 20, 30), // video, linked to 3
        visual(1, 3, 20, 30), // audio, linked to 2
        visual(1, 4, 55, 5),  // constrains the audio twin to stay <= 55-30=25
    ];
    // On its own track 0 would allow [10, MAX-30]; the twin on
    // track 1 narrows it to max_start <= 25 (same offset, 0).
    let (min, max, followers) = combined_drag_range(&visuals, 0, ClipId(2), &[(1, ClipId(3))]);
    assert_eq!(min, 10);
    assert_eq!(max, 25);
    assert_eq!(followers, vec![(ClipId(3), 1, 0)]);
}

#[test]
fn combined_drag_range_respects_nonzero_offset_between_linked_clips() {
    // The twin is not aligned: it starts 5 frames after the primary.
    let visuals = vec![
        visual(0, 1, 10, 20), // primary, track 0, start=10
        visual(1, 2, 15, 20), // twin, track 1, start=15 (offset=5)
        visual(1, 3, 60, 5),  // constrains the twin: max_start <= 60-20=40
    ];
    let (_, max, followers) = combined_drag_range(&visuals, 0, ClipId(1), &[(1, ClipId(2))]);
    // twin constraint translated: primary.max_start <= 40 - offset(5) = 35
    assert_eq!(max, 35);
    assert_eq!(followers, vec![(ClipId(2), 1, 5)]);
}

#[test]
fn combined_drag_range_intersects_a_group_of_three() {
    // A group of 3 on 3 different tracks, each with a different constraint.
    let visuals = vec![
        visual(0, 1, 10, 10), // primary, track 0, start=10, no neighbor
        visual(1, 2, 10, 10), // same start, constrained by a neighbor at 25
        visual(1, 5, 35, 5),
        visual(2, 3, 10, 10), // same start, constrained by a neighbor at 22
        visual(2, 6, 32, 5),
    ];
    let (min, max, followers) =
        combined_drag_range(&visuals, 0, ClipId(1), &[(1, ClipId(2)), (2, ClipId(3))]);
    assert_eq!(min, 0);
    // track1: max_start <= 35-10=25; track2: max_start <= 32-10=22 (tighter)
    assert_eq!(max, 22);
    assert_eq!(
        followers,
        vec![(ClipId(2), 1, 0), (ClipId(3), 2, 0)],
        "both other clips of the group, with offset 0 (same start)"
    );
}

#[test]
fn drag_range_at_checks_neighbors_on_a_track_the_clip_isnt_on() {
    let visuals = vec![visual(0, 1, 0, 10), visual(1, 2, 20, 10)];
    // Clip 1 evaluated as if it were about to land on track 1: it must
    // respect the neighbor there (clip 2), not those of its real track.
    assert_eq!(drag_range_at(&visuals, 1, &[ClipId(1)], 5, 10), (0, 10));
}

#[test]
fn group_drag_bounds_intersects_primary_and_followers_on_their_own_targets() {
    let visuals = vec![
        visual(0, 1, 0, 10),
        visual(1, 2, 0, 10),
        visual(1, 3, 30, 10),
    ];
    let targets = vec![
        (ClipId(1), EffectiveTrack::Existing(0)),
        (ClipId(2), EffectiveTrack::Existing(1)),
    ];
    let followers = vec![(ClipId(2), 1, -5)];
    let (min_start, max_start) = group_drag_bounds(&visuals, 0, &targets, &followers);
    assert_eq!((min_start, max_start), (5, 25));
}

#[test]
fn group_drag_bounds_lets_group_members_land_on_the_same_track_without_blocking_each_other() {
    // Two clips of the group (1 and 2) both land on track 1: they must
    // not block each other, only the outside clip (9) counts as a
    // neighbor.
    let visuals = vec![
        visual(0, 1, 0, 10),
        visual(2, 2, 5, 10),
        visual(1, 9, 50, 5),
    ];
    let targets = vec![
        (ClipId(1), EffectiveTrack::Existing(1)),
        (ClipId(2), EffectiveTrack::Existing(1)),
    ];
    let followers = vec![(ClipId(2), 2, 5)];
    let (min_start, max_start) = group_drag_bounds(&visuals, 0, &targets, &followers);
    assert_eq!((min_start, max_start), (0, 35));
}

#[test]
fn dragging_from_the_middle_of_a_group_onto_an_empty_track_does_not_overflow() {
    let visuals = vec![
        visual(0, 1, 0, 10),
        visual(0, 2, 20, 10),
        visual(0, 3, 40, 10),
    ];
    let targets = vec![
        (ClipId(2), EffectiveTrack::Existing(1)),
        (ClipId(1), EffectiveTrack::Existing(1)),
        (ClipId(3), EffectiveTrack::New(1)),
    ];
    let followers = vec![(ClipId(1), 0, -20), (ClipId(3), 0, 20)];
    let (min_start, max_start) = group_drag_bounds(&visuals, 20, &targets, &followers);
    assert_eq!(min_start, 20);
    assert!(max_start > 1_000_000);

    let (min_start, _, _) =
        combined_drag_range(&visuals, 0, ClipId(2), &[(0, ClipId(1)), (0, ClipId(3))]);
    assert_eq!(min_start, 20);
}

/// 2 video tracks and 1 audio; 228px below the ruler = 50px of empty
/// zone above and below the groups, 0 = no empty zone.
fn test_layout(slack: f32) -> PaneLayout {
    let avail = 2.0 * slack + 3.0 * ROW_HEIGHT + GROUP_DIVIDER_HEIGHT;
    PaneLayout::new(avail, 2, 1, &mut TimelineState::default())
}

#[test]
fn track_drag_target_above_video_group_is_new_track_when_margin_exists() {
    let row_order = [1, 0, 2]; // 2 video tracks (descending), 1 audio
    let target = track_drag_target(
        RULER_HEIGHT + 20.0,
        TrackKind::Video,
        &row_order,
        &test_layout(50.0),
    );
    assert!(matches!(target, Some(TrackDragTarget::NewTrack)));
}

#[test]
fn track_drag_target_above_video_group_without_margin_is_the_top_track() {
    let row_order = [1, 0, 2];
    let target = track_drag_target(
        RULER_HEIGHT - 5.0,
        TrackKind::Video,
        &row_order,
        &test_layout(0.0),
    );
    assert!(matches!(target, Some(TrackDragTarget::Track(1))));
}

#[test]
fn track_drag_target_lands_on_the_right_video_row() {
    let row_order = [1, 0, 2];
    let layout = test_layout(50.0);
    let first_row = track_drag_target(RULER_HEIGHT + 60.0, TrackKind::Video, &row_order, &layout);
    assert!(matches!(first_row, Some(TrackDragTarget::Track(1))));
    let second_row = track_drag_target(RULER_HEIGHT + 100.0, TrackKind::Video, &row_order, &layout);
    assert!(matches!(second_row, Some(TrackDragTarget::Track(0))));
}

#[test]
fn track_drag_target_video_over_audio_group_is_none() {
    let row_order = [1, 0, 2];
    let target = track_drag_target(
        RULER_HEIGHT + 150.0,
        TrackKind::Video,
        &row_order,
        &test_layout(50.0),
    );
    assert!(target.is_none());
}

/// An effect (always video) or a media with video over a free
/// video track land exactly there, not on the first free one —
/// the very same criterion, regardless of which of the two
/// is being dragged.
#[test]
fn media_pool_drop_target_lands_on_the_hovered_video_track_for_a_generator_or_a_video_media() {
    for has_video in [true, false] {
        let target = media_pool_drop_target(2, has_video, TrackKind::Video, false);
        if has_video {
            assert_eq!(target, Some(MediaDropTarget::Track(2)));
        } else {
            // A media without video (audio-only) on a video track
            // does not force that track: it falls back on the usual
            // resolution, as it already did.
            assert_eq!(target, Some(MediaDropTarget::Default));
        }
    }
}

#[test]
fn media_pool_drop_target_over_an_audio_track_falls_back_to_default() {
    assert_eq!(
        media_pool_drop_target(0, true, TrackKind::Audio, false),
        Some(MediaDropTarget::Default)
    );
}

#[test]
fn media_pool_drop_target_over_a_locked_track_refuses_the_drop() {
    assert_eq!(
        media_pool_drop_target(2, true, TrackKind::Video, true),
        None
    );
}

#[test]
fn track_drag_target_below_audio_group_is_new_track_when_margin_exists() {
    let row_order = [1, 0, 2];
    let target = track_drag_target(
        RULER_HEIGHT + 180.0,
        TrackKind::Audio,
        &row_order,
        &test_layout(50.0),
    );
    assert!(matches!(target, Some(TrackDragTarget::NewTrack)));
}

#[test]
fn track_drag_target_below_audio_group_without_margin_is_the_bottom_track() {
    let row_order = [1, 0, 2];
    let target = track_drag_target(
        RULER_HEIGHT + 180.0,
        TrackKind::Audio,
        &row_order,
        &test_layout(0.0),
    );
    assert!(matches!(target, Some(TrackDragTarget::Track(2))));
}

/// Reported bug: with many video tracks the separator did not rise past
/// the topmost track, so room could not be made for the audio.
#[test]
fn divider_can_shrink_an_overflowing_video_pane_which_then_scrolls() {
    let mut state = TimelineState::default();
    let avail = 200.0;
    let unconstrained = PaneLayout::new(avail, 6, 1, &mut state);
    assert_eq!(unconstrained.video_height_range.min, MIN_PANE_HEIGHT);

    state.video_pane_height = Some(60.0);
    let layout = PaneLayout::new(avail, 6, 1, &mut state);
    assert_eq!(layout.video_height(), 60.0);
    // Resting against the separator until it scrolls.
    assert_eq!(layout.video_rows_bottom(), layout.video_pane.max);
    assert_eq!(
        layout.video_max_scroll,
        6.0 * ROW_HEIGHT + NEW_TRACK_ZONE_HEIGHT - 60.0
    );

    state.video_scroll = 10_000.0;
    let scrolled = PaneLayout::new(avail, 6, 1, &mut state);
    assert_eq!(state.video_scroll, layout.video_max_scroll);
    assert_eq!(
        scrolled.video_rows_top,
        scrolled.video_pane.min + NEW_TRACK_ZONE_HEIGHT
    );
}

#[test]
fn audio_pane_scrolls_when_its_tracks_overflow() {
    let mut state = TimelineState::default();
    state.audio_scroll = 10_000.0;
    let layout = PaneLayout::new(200.0, 1, 8, &mut state);
    assert!(layout.audio_max_scroll > 0.0);
    assert_eq!(
        layout.audio_rows_bottom() + NEW_TRACK_ZONE_HEIGHT,
        layout.audio_pane.max
    );
    let last_row = layout.row_at_y(layout.audio_pane.max - NEW_TRACK_ZONE_HEIGHT - 1.0);
    assert_eq!(last_row, 8);
}

#[test]
fn drag_group_row_targets_shifts_a_same_kind_follower_by_the_same_amount() {
    // track_kinds: [Video, Audio, Video, Video] -> row_order [3,2,0,1]
    // (video descending, audio ascending), row_of_track [2,3,1,0].
    let track_kinds = [
        TrackKind::Video,
        TrackKind::Audio,
        TrackKind::Video,
        TrackKind::Video,
    ];
    let row_of_track = [2, 3, 1, 0];
    let row_order = [3, 2, 0, 1];
    // Primary (track 2, row 1) rises one row -> track 3 (row 0).
    // Video follower (track 0, row 2, "below" the primary) must
    // snap into the row just freed by the primary (row
    // 1 -> track 2), exactly as C1 follows C2 in the user's
    // example.
    let followers = vec![(ClipId(9), 0, 0)];
    let targets = drag_group_row_targets(
        ClipId(1),
        2,
        EffectiveTrack::Existing(3),
        &followers,
        &track_kinds,
        &row_of_track,
        &row_order,
        3,
        4,
    );
    assert_eq!(
        targets,
        vec![
            (ClipId(1), EffectiveTrack::Existing(3)),
            (ClipId(9), EffectiveTrack::Existing(2)),
        ]
    );
}

#[test]
fn drag_group_row_targets_moves_an_audio_follower_in_the_opposite_row_direction() {
    // track_kinds: [Video, Audio, Audio, Video] -> row_order [3,0,1,2]
    // (2 video tracks, 2 audio), row_of_track [1,2,3,0].
    let track_kinds = [
        TrackKind::Video,
        TrackKind::Audio,
        TrackKind::Audio,
        TrackKind::Video,
    ];
    let row_of_track = [1, 2, 3, 0];
    let row_order = [3, 0, 1, 2];
    // Video primary (track 0, row 1) rises one row -> track 3
    // (row 0). The audio follower (track 1, row 2) must go down
    // one row (track 1 -> track 2), not up: video and audio
    // number the tracks in opposite directions.
    let followers = vec![(ClipId(9), 1, 0)];
    let targets = drag_group_row_targets(
        ClipId(1),
        0,
        EffectiveTrack::Existing(3),
        &followers,
        &track_kinds,
        &row_of_track,
        &row_order,
        2,
        4,
    );
    assert_eq!(
        targets,
        vec![
            (ClipId(1), EffectiveTrack::Existing(3)),
            (ClipId(9), EffectiveTrack::Existing(2)),
        ]
    );
}

#[test]
fn drag_group_row_targets_creates_a_new_track_for_a_follower_that_would_overflow() {
    let track_kinds = [
        TrackKind::Video,
        TrackKind::Audio,
        TrackKind::Video,
        TrackKind::Video,
    ];
    let row_of_track = [2, 3, 1, 0];
    let row_order = [3, 2, 0, 1];
    // Single audio track: the audio follower has nowhere to go among
    // the existing ones, so (reported bug) it must ask for a new
    // track instead of staying stuck on its own — exactly as
    // it would if it were the grabbed clip.
    let followers = vec![(ClipId(9), 1, 0)];
    let targets = drag_group_row_targets(
        ClipId(1),
        2,
        EffectiveTrack::Existing(3),
        &followers,
        &track_kinds,
        &row_of_track,
        &row_order,
        3,
        4,
    );
    assert_eq!(targets[1], (ClipId(9), EffectiveTrack::New(1)));
}

#[test]
fn drag_group_row_targets_can_need_more_than_one_new_track_for_a_follower() {
    // The primary is not the nearest to the edge of its own group: if
    // it jumps directly into a new track, the follower that was already
    // at the edge must "break through" by more than one track to keep
    // the relative spacing.
    let track_kinds = [TrackKind::Video, TrackKind::Video, TrackKind::Video];
    let row_of_track = [2, 1, 0]; // 3 video tracks, descending rows
    let row_order = [2, 1, 0];
    // Primary on track 0 (row 2, the farthest from the edge) goes to
    // New(1); the follower on track 2 (row 0, already at the edge) follows
    // with the same delta (-3) -> New(3).
    let followers = vec![(ClipId(9), 2, 0)];
    let targets = drag_group_row_targets(
        ClipId(1),
        0,
        EffectiveTrack::New(1),
        &followers,
        &track_kinds,
        &row_of_track,
        &row_order,
        3,
        3,
    );
    assert_eq!(targets[1], (ClipId(9), EffectiveTrack::New(3)));
}

fn media_clip_visual(
    track_index: usize,
    id: u64,
    start: FrameIdx,
    source_in: FrameIdx,
    source_out: FrameIdx,
    media_id: vv_core::MediaId,
) -> ClipVisual<'static> {
    ClipVisual {
        track_index,
        clip: std::borrow::Cow::Owned(Clip::from_source_range(
            ClipId(id),
            ClipSource::Media(media_id),
            source_in,
            source_out,
            start,
            vv_core::Rational::one(),
        )),
        label: String::new(),
        color: egui::Color32::WHITE,
        locked: false,
        muted: false,
    }
}

fn project_with_media(duration_frames: FrameIdx) -> (Project, vv_core::MediaId) {
    let mut project = Project::default();
    let media_id = project.media_pool.insert(vv_core::MediaItem {
        path: "/tmp/x.mp4".into(),
        meta: vv_core::MediaMeta {
            duration_frames,
            fps: vv_core::Rational::new(25, 1),
            width: 100,
            height: 100,
            has_video: true,
            has_audio: false,
            sample_rate: 0,
            channels: 0,
            audio_streams: 0,
            file: Default::default(),
        },
        content_hash: 0,
        compound: None,
        folder: None,
    });
    (project, media_id)
}

#[test]
fn drag_set_segments_are_queued_one_after_the_other() {
    let (mut project, a) = project_with_media(30);
    let b = project.media_pool.insert(vv_core::MediaItem {
        path: "/tmp/y.mp4".into(),
        meta: vv_core::MediaMeta {
            duration_frames: 50,
            fps: vv_core::Rational::new(25, 1),
            width: 100,
            height: 100,
            has_video: true,
            has_audio: true,
            sample_rate: 48000,
            channels: 2,
            audio_streams: 1,
            file: Default::default(),
        },
        content_hash: 1,
        compound: None,
        folder: None,
    });
    let fps = vv_core::Rational::new(25, 1);
    let set = MediaDragSet {
        items: vec![
            MediaDrag::whole(a, &project.media_pool[a].meta),
            MediaDrag::whole(b, &project.media_pool[b].meta),
        ],
    };
    let segments = drag_set_segments(&project, fps, &TimelineDrag::Media(set));
    assert_eq!(
        segments
            .iter()
            .map(|s| (s.offset, s.len, s.has_audio))
            .collect::<Vec<_>>(),
        vec![(0, 30, false), (30, 50, true)]
    );
}

/// The neighbor no longer limits the trim: lengthening over it
/// overwrites it (see `PendingAction::Trim`), so the only limit
/// left is the start of the source.
#[test]
fn single_trim_range_start_is_not_clamped_by_the_previous_neighbor() {
    let project = Project::default();
    let visuals = [
        visual(0, 1, 0, 5), // ends at 5
        media_clip_visual(
            0,
            2,
            10,
            8,
            20,
            <vv_core::MediaId as vv_core::Id>::from_raw(0),
        ),
    ];
    let (min_value, _) = vv_core::edit::trim_range(&project, &visuals[1].clip, TrimEdge::Start);
    assert_eq!(min_value, 2, "10 - source_in 8, not the neighbour's edge");
}

#[test]
fn single_trim_range_start_is_clamped_by_source_in() {
    let project = Project::default();
    // No neighbor, but source_in=3: one cannot go back past
    // the start of the source, so timeline_start cannot go
    // below 10-3=7.
    let visuals = [media_clip_visual(
        0,
        1,
        10,
        3,
        20,
        <vv_core::MediaId as vv_core::Id>::from_raw(0),
    )];
    let (min_value, max_value) =
        vv_core::edit::trim_range(&project, &visuals[0].clip, TrimEdge::Start);
    assert_eq!(min_value, 7);
    assert_eq!(
        max_value, 26,
        "timeline_end() - 1 (timeline_end = 10 + (20-3) = 27)"
    );
}

#[test]
fn single_trim_range_end_is_not_clamped_by_the_next_neighbor() {
    let project = Project::default();
    let visuals = [
        visual(0, 1, 0, 10),  // being trimmed: [0,10)
        visual(0, 2, 15, 10), // following neighbor: it can be overwritten
    ];
    let (_, max_value) = vv_core::edit::trim_range(&project, &visuals[0].clip, TrimEdge::End);
    assert_eq!(max_value, FrameIdx::MAX);
}

#[test]
fn single_trim_range_end_is_clamped_by_media_duration() {
    let (project, media_id) = project_with_media(25);
    // source_out starts at 20 on a media 25 frames long: the end cannot
    // be extended past timeline_start + (25 - source_in) = 25.
    let visuals = [media_clip_visual(0, 1, 0, 0, 20, media_id)];
    let (_, max_value) = vv_core::edit::trim_range(&project, &visuals[0].clip, TrimEdge::End);
    assert_eq!(max_value, 25);
}

/// Conformed clip (media at 29.97 on a timeline at 30): the trim limits
/// are in *timeline* frames, so the duration of the source must be
/// converted with the `rate` — 1000 source frames are 1001 timeline ones.
/// With the previous 1:1 conversion it would give 1000 and 5000.
#[test]
fn single_trim_range_of_a_conformed_clip_is_in_timeline_frames() {
    let (project, media_id) = project_with_media(4000);
    let mut visuals = [media_clip_visual(0, 1, 3000, 2000, 2400, media_id)];
    visuals[0].clip = std::borrow::Cow::Owned(Clip::from_source_range(
        visuals[0].clip.id,
        visuals[0].clip.source.clone(),
        2000,
        2400,
        3000,
        vv_core::Rational::conform_rate(
            vv_core::Rational::new(30, 1),
            vv_core::Rational::new(30_000, 1001),
        ),
    ));

    let (min_value, _) = vv_core::edit::trim_range(&project, &visuals[0].clip, TrimEdge::Start);
    assert_eq!(
        min_value, 998,
        "2000 source frames before = 2002 timeline frames before 3000"
    );

    let (_, max_value) = vv_core::edit::trim_range(&project, &visuals[0].clip, TrimEdge::End);
    assert_eq!(
        max_value, 5002,
        "2000 remaining source frames = 2002 timeline frames after 3000"
    );
}

#[test]
fn single_trim_range_end_is_unbounded_for_solid_color() {
    let project = Project::default();
    let visuals = [visual(0, 1, 0, 10)];
    let (_, max_value) = vv_core::edit::trim_range(&project, &visuals[0].clip, TrimEdge::End);
    assert_eq!(max_value, FrameIdx::MAX);
}

#[test]
fn combined_trim_range_intersects_both_clips_constraints() {
    // Video [10,30) generator (unlimited end), linked to the audio
    // [10,30) on track 1, whose media ends at source frame 25:
    // the twin's limit applies to the video too.
    let (project, media_id) = project_with_media(25);
    let group = Some(vv_core::LinkGroupId(9));
    let mut video = visual(0, 1, 10, 20);
    video.clip.to_mut().linked_group = group;
    let mut audio = media_clip_visual(1, 2, 10, 0, 20, media_id);
    audio.clip.to_mut().linked_group = group;
    let visuals = vec![video, audio];

    let (_, max_value, followers) = combined_trim_range(
        &visuals,
        &project,
        (0, ClipId(1)),
        TrimEdge::End,
        &[((1, ClipId(2)), TrimEdge::End)],
    );
    assert_eq!(
        max_value, 35,
        "the twin's constraint applies to the video too"
    );
    assert_eq!(followers, vec![(ClipId(2), 1, 0, TrimEdge::End)]);
}

#[test]
fn combined_trim_range_shifts_each_selected_clip_by_its_offset() {
    // End of the primary at 10, of the other (track 1) at 25: same
    // delta for both, and the other cannot go below 21.
    let project = Project::default();
    let visuals = vec![visual(0, 1, 0, 10), visual(1, 2, 20, 5)];
    let (min_value, max_value, followers) = combined_trim_range(
        &visuals,
        &project,
        (0, ClipId(1)),
        TrimEdge::End,
        &[((1, ClipId(2)), TrimEdge::End)],
    );
    assert_eq!(followers, vec![(ClipId(2), 1, 15, TrimEdge::End)]);
    assert_eq!(min_value, 6, "21 - 15");
    assert_eq!(max_value, FrameIdx::MAX - 15);
}

#[test]
fn combined_trim_range_stops_before_another_trimmed_clip_on_the_same_track() {
    let project = Project::default();
    let visuals = vec![visual(0, 1, 0, 10), visual(0, 2, 20, 5)];
    let (_, max_value, _) = combined_trim_range(
        &visuals,
        &project,
        (0, ClipId(1)),
        TrimEdge::End,
        &[((0, ClipId(2)), TrimEdge::End)],
    );
    assert_eq!(max_value, 20);
}

#[test]
fn combined_trim_range_rolls_between_two_adjacent_clips() {
    // [0,10) and [10,25) in contact, the second with 5 frames of source
    // before its start: the contact point goes from 5 to 24 (the
    // second stays at least 1 long).
    let project = Project::default();
    let mut second = visual(0, 2, 10, 15);
    second.clip = std::borrow::Cow::Owned(Clip::from_source_range(
        ClipId(2),
        vv_core::ClipSource::Media(<vv_core::MediaId as vv_core::Id>::from_raw(1)),
        5,
        20,
        10,
        vv_core::Rational::one(),
    ));
    let visuals = vec![visual(0, 1, 0, 10), second];
    let (min_value, max_value, followers) = combined_trim_range(
        &visuals,
        &project,
        (0, ClipId(2)),
        TrimEdge::Start,
        &[((0, ClipId(1)), TrimEdge::End)],
    );
    assert_eq!((min_value, max_value), (5, 24));
    assert_eq!(followers, vec![(ClipId(1), 0, 0, TrimEdge::End)]);
}

#[test]
fn edge_zones_roll_at_the_contact_point_and_trim_just_inside() {
    let neighbor = Some((0, ClipId(9)));
    let zones = edge_zones(100.0, None, neighbor);
    assert_eq!(zones.at(2.0), Some(EdgeZone::Trim(TrimEdge::Start)));
    assert_eq!(zones.at(50.0), None);
    assert_eq!(zones.at(90.0), Some(EdgeZone::Trim(TrimEdge::End)));
    assert_eq!(
        zones.at(98.0),
        Some(EdgeZone::Roll {
            edge: TrimEdge::End,
            neighbor: (0, ClipId(9))
        })
    );
}

/// Reported bug: with snapping on, the edge stopped one frame before
/// or after the playhead, which was not a snap point.
#[test]
fn snap_frame_of_an_edge_snaps_to_the_playhead() {
    let visuals = vec![visual(0, 1, 0, 10), visual(0, 2, 20, 10)];
    assert_eq!(
        snap_frame(14, 0, &visuals, &[ClipId(1)], &[15], 5.0, true),
        15
    );
    assert_eq!(
        snap_frame(4, 10, &visuals, &[ClipId(1)], &[15], 5.0, true),
        5
    );
}

#[test]
fn snap_frame_of_an_edge_snaps_the_trimmed_edge_to_a_nearby_clip_edge() {
    // Neighboring clip [20,30): the edge dragged to 18, within the threshold
    // (10px / 5px per frame = 2 frames), snaps to 20.
    let visuals = vec![visual(0, 1, 0, 10), visual(0, 2, 20, 10)];
    assert_eq!(
        snap_frame(18, 0, &visuals, &[ClipId(1)], &[], 5.0, true),
        20
    );
}

#[test]
fn snap_frame_of_an_edge_ignores_the_clip_being_trimmed_and_far_edges() {
    let visuals = vec![visual(0, 1, 0, 10), visual(0, 2, 20, 10)];
    // Its own edge (10) is not a valid snap.
    assert_eq!(
        snap_frame(11, 0, &visuals, &[ClipId(1)], &[], 5.0, true),
        11
    );
    // Outside the threshold: no snap.
    assert_eq!(
        snap_frame(15, 0, &visuals, &[ClipId(1)], &[], 5.0, true),
        15
    );
    // Snapping off: no snap even within the threshold.
    assert_eq!(
        snap_frame(18, 0, &visuals, &[ClipId(1)], &[], 5.0, false),
        18
    );
}

#[test]
fn snap_frame_snaps_start_to_nearby_clip_end() {
    // Existing clip [0,10): its end edge is 10. A candidate at
    // 12 (within the threshold) must snap exactly there.
    let visuals = vec![visual(0, 1, 0, 10)];
    let px_per_frame = 5.0; // threshold 10px / 5px_per_frame = 2 frames
    let snapped = snap_frame(12, 20, &visuals, &[], &[], px_per_frame, true);
    assert_eq!(snapped, 10);
}

#[test]
fn snap_frame_snaps_end_of_dragged_clip_to_nearby_clip_start() {
    // Existing clip [50,60): the dragged clip (length 20) must
    // snap its own *end* to 50, i.e. candidate_start=30.
    let visuals = vec![visual(0, 1, 50, 10)];
    let snapped = snap_frame(32, 20, &visuals, &[], &[], 5.0, true);
    assert_eq!(snapped, 30);
}

#[test]
fn snap_frame_ignores_clips_beyond_threshold() {
    let visuals = vec![visual(0, 1, 0, 10)];
    // 20 frames away from the edge (10): at px_per_frame=5.0 the threshold
    // is only 2 frames, so it stays unchanged.
    let snapped = snap_frame(30, 5, &visuals, &[], &[], 5.0, true);
    assert_eq!(snapped, 30);
}

#[test]
fn snap_frame_disabled_is_a_no_op() {
    let visuals = vec![visual(0, 1, 0, 10)];
    let snapped = snap_frame(12, 20, &visuals, &[], &[], 5.0, false);
    assert_eq!(snapped, 12);
}

#[test]
fn snap_frame_excludes_given_clip_ids() {
    // Clip 1 would be a valid snap, but it is excluded (it is the clip
    // being dragged itself, or its linked twin).
    let visuals = vec![visual(0, 1, 0, 10)];
    let snapped = snap_frame(12, 20, &visuals, &[ClipId(1)], &[], 5.0, true);
    assert_eq!(snapped, 12);
}

#[test]
fn duplicate_clips_keeps_the_originals_relinks_the_copies_and_cuts_what_they_cover() {
    let mut project = Project::default();
    let timeline_id = project.timelines.insert(vv_core::Timeline {
        name: "T".into(),
        fps: vv_core::Rational::new(25, 1),
        resolution: (1920, 1080),
        tracks: vec![
            vv_core::Track::new(TrackKind::Video),
            vv_core::Track::new(TrackKind::Audio),
        ],
        markers: Vec::new(),
        master: Default::default(),
    });
    let mut history = History::default();
    let solid = |id, start, len| {
        Clip::from_source_range(
            id,
            vv_core::ClipSource::SolidColor,
            0,
            len,
            start,
            vv_core::Rational::one(),
        )
    };
    // Video [0,20) linked to the audio [0,20); further along on the video
    // another clip [30,60) that the copy will partly cover.
    let (v, a, other) = (
        project.alloc_clip_id(),
        project.alloc_clip_id(),
        project.alloc_clip_id(),
    );
    for (track_index, clip) in [
        (0, solid(v, 0, 20)),
        (1, solid(a, 0, 20)),
        (0, solid(other, 30, 30)),
    ] {
        history.do_command(
            &mut project,
            Box::new(vv_core::InsertClip {
                timeline: timeline_id,
                track_index,
                clip,
            }),
        );
    }
    history.do_command(
        &mut project,
        Box::new(vv_core::LinkClips::new(timeline_id, vec![(0, v), (1, a)])),
    );

    let mut state = TimelineState::default();
    duplicate_clips(
        &mut project,
        &mut history,
        &mut state,
        timeline_id,
        &[(v, 0, 0, 25), (a, 1, 1, 25)],
    );

    let tl = &project.timelines[timeline_id];
    let spans = |track: usize| -> Vec<(FrameIdx, FrameIdx)> {
        tl.tracks[track]
            .clips
            .iter()
            .map(|c| (c.timeline_start, c.timeline_end()))
            .collect()
    };
    assert_eq!(
        spans(0),
        vec![(0, 20), (25, 45), (45, 60)],
        "the other clip is cut"
    );
    assert_eq!(spans(1), vec![(0, 20), (25, 45)]);
    let copy_v = &tl.tracks[0].clips[1];
    let copy_a = &tl.tracks[1].clips[1];
    assert!(copy_v.linked_group.is_some());
    assert_eq!(copy_v.linked_group, copy_a.linked_group);
    assert_ne!(copy_v.linked_group, tl.tracks[0].clips[0].linked_group);
    assert_eq!(
        state.selected,
        BTreeSet::from([(0, copy_v.id), (1, copy_a.id)])
    );

    history.undo(&mut project);
    assert_eq!(
        project.timelines[timeline_id].tracks[0].clips.len(),
        2,
        "a single undo step"
    );
}

#[test]
fn make_compound_clip_replaces_the_selection_and_names_it_in_order() {
    let mut project = Project::default();
    let timeline_id = project.timelines.insert(vv_core::Timeline {
        name: "T".into(),
        fps: vv_core::Rational::new(25, 1),
        resolution: (1920, 1080),
        tracks: vec![Track::new(TrackKind::Video), Track::new(TrackKind::Audio)],
        markers: Vec::new(),
        master: Default::default(),
    });
    let mut history = History::default();
    let solid = |id, start, len| {
        Clip::from_source_range(
            id,
            vv_core::ClipSource::SolidColor,
            0,
            len,
            start,
            vv_core::Rational::one(),
        )
    };
    let (v, a) = (project.alloc_clip_id(), project.alloc_clip_id());
    for (track_index, clip) in [(0, solid(v, 10, 30)), (1, solid(a, 20, 30))] {
        history.do_command(
            &mut project,
            Box::new(vv_core::InsertClip {
                timeline: timeline_id,
                track_index,
                clip,
            }),
        );
    }

    make_compound_clip(
        &mut project,
        &mut history,
        timeline_id,
        vec![(0, v), (1, a)],
    );

    let video_track = &project.timelines[timeline_id].tracks[0];
    assert_eq!(
        video_track.clips.len(),
        1,
        "the two originals become a single video clip"
    );
    let vv_core::ClipSource::Media(media_id) = video_track.clips[0].source else {
        panic!("the resulting compound clip must point to the media pool");
    };
    assert_eq!(video_track.clips[0].timeline_start, 10);
    assert_eq!(video_track.clips[0].timeline_len, 40);
    let audio_track = &project.timelines[timeline_id].tracks[1];
    assert_eq!(audio_track.clips.len(), 1);
    assert_eq!(
        video_track.clips[0].linked_group,
        audio_track.clips[0].linked_group
    );

    let item = project.media_pool.get(media_id).expect("added to the pool");
    assert_eq!(item.path.to_str(), Some("Compound Clip 1"));
    assert!(item.meta.has_video && item.meta.has_audio);
    let nested_id = item.compound.expect("it is a compound clip");
    let nested = &project.timelines[nested_id];
    assert_eq!(
        nested.tracks[0].clips[0].timeline_start, 0,
        "re-offset to the start of the selection"
    );
    assert_eq!(nested.tracks[1].clips[0].timeline_start, 10);

    // An undo gives back the original clips and takes the compound clip
    // out of the pool, with its nested timeline.
    history.undo(&mut project);
    assert_eq!(project.timelines[timeline_id].tracks[0].clips.len(), 1);
    assert_eq!(project.timelines[timeline_id].tracks[0].clips[0].id, v);
    assert!(project.media_pool.is_empty());
    assert!(!project.timelines.contains_key(nested_id));
    history.redo(&mut project);
    assert!(project.timelines.contains_key(nested_id));

    // A second compound clip continues the numbering.
    let (v2, a2) = (project.alloc_clip_id(), project.alloc_clip_id());
    for (track_index, clip) in [(0, solid(v2, 100, 10)), (1, solid(a2, 100, 10))] {
        history.do_command(
            &mut project,
            Box::new(vv_core::InsertClip {
                timeline: timeline_id,
                track_index,
                clip,
            }),
        );
    }
    make_compound_clip(
        &mut project,
        &mut history,
        timeline_id,
        vec![(0, v2), (1, a2)],
    );
    let second = project
        .media_pool
        .values()
        .find(|m| m.path.to_str() != Some("Compound Clip 1"))
        .unwrap();
    assert_eq!(second.path.to_str(), Some("Compound Clip 2"));
}

/// Really runs `show_timeline` inside a headless `egui::Context`,
/// with real clips on several tracks: it catches panics/bugs in the
/// drawing code (indices, borrows) that the purely logical tests
/// above do not touch.
#[test]
fn show_timeline_renders_without_panicking_with_real_clips() {
    let mut project = Project::default();
    let timeline_id = project.timelines.insert(vv_core::Timeline {
        name: "T".into(),
        fps: vv_core::Rational::new(25, 1),
        resolution: (1920, 1080),
        tracks: vec![
            vv_core::Track::new(TrackKind::Video),
            vv_core::Track::new(TrackKind::Audio),
        ],
        markers: Vec::new(),
        master: Default::default(),
    });
    let mut history = History::default();

    for (track_index, start, len) in [(0usize, 0i64, 50i64), (0, 50, 30), (1, 0, 50)] {
        let clip = Clip::from_source_range(
            project.alloc_clip_id(),
            vv_core::ClipSource::SolidColor,
            0,
            len,
            start,
            vv_core::Rational::one(),
        );
        history.do_command(
            &mut project,
            Box::new(vv_core::InsertClip {
                timeline: timeline_id,
                track_index,
                clip,
            }),
        );
    }

    let mut state = TimelineState {
        selected: BTreeSet::from([(0, ClipId(0))]),
        ..TimelineState::default()
    };

    let ctx = egui::Context::default();
    let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
        egui::CentralPanel::default().show(ui, |ui| {
            show_timeline(
                ui,
                &mut project,
                &mut history,
                timeline_id,
                &|_id| "media".to_string(),
                &mut state,
                true,
                true,
                &[],
                &[],
                &[],
                &std::collections::HashMap::new(),
                false,
            );
        });
    });
    // The font atlas generates a texture delta: it must be consumed explicitly
    // or egui panics on drop (diagnostics meant for a real renderer).
    output.textures_delta.clear();

    assert_eq!(project.timelines[timeline_id].tracks[0].clips.len(), 2);
}

/// Runs `show_timeline` with more than one video track (plans/REFACTOR_PIPELINE.md
/// B4): the header column (labels + add/remove track buttons,
/// `draw_track_headers`) must hold any N tracks, not
/// only the fixed video/audio pair of before.
#[test]
fn show_timeline_renders_without_panicking_with_more_than_two_tracks() {
    let mut project = Project::default();
    let timeline_id = project.timelines.insert(vv_core::Timeline {
        name: "T".into(),
        fps: vv_core::Rational::new(25, 1),
        resolution: (1920, 1080),
        tracks: vec![
            vv_core::Track::new(TrackKind::Video),
            vv_core::Track::new(TrackKind::Video),
            vv_core::Track::new(TrackKind::Audio),
        ],
        markers: Vec::new(),
        master: Default::default(),
    });
    let mut history = History::default();
    let mut state = TimelineState::default();

    let ctx = egui::Context::default();
    let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
        egui::CentralPanel::default().show(ui, |ui| {
            show_timeline(
                ui,
                &mut project,
                &mut history,
                timeline_id,
                &|_id| "media".to_string(),
                &mut state,
                true,
                true,
                &[],
                &[],
                &[],
                &std::collections::HashMap::new(),
                false,
            );
        });
    });
    output.textures_delta.clear();

    assert_eq!(project.timelines[timeline_id].tracks.len(), 3);
}

/// Bug: the zoom "grew" from the *visible* left edge (or from 0) instead
/// of from the playhead. Fix: when `pixels_per_sec` changes,
/// `show_timeline` corrects the horizontal scroll offset so the
/// playhead stays at the same position on screen — `offset' = offset +
/// t_playhead * (pps' - pps)` — and a repeated zoom in/out does not move it.
#[test]
fn zoom_keeps_playhead_at_same_screen_position() {
    let mut project = Project::default();
    let timeline_id = project.timelines.insert(vv_core::Timeline {
        name: "T".into(),
        fps: vv_core::Rational::new(25, 1),
        resolution: (1920, 1080),
        tracks: vec![
            vv_core::Track::new(TrackKind::Video),
            vv_core::Track::new(TrackKind::Audio),
        ],
        markers: Vec::new(),
        master: Default::default(),
    });
    let mut history = History::default();
    // Clip long enough to make the timeline scrollable (content
    // wider than the viewport): without it the offset would be clamped to 0 and the
    // test would say nothing.
    let clip = Clip::from_source_range(
        project.alloc_clip_id(),
        vv_core::ClipSource::SolidColor,
        0,
        2500, // 100s at 25fps
        0,
        vv_core::Rational::one(),
    );
    history.do_command(
        &mut project,
        Box::new(vv_core::InsertClip {
            timeline: timeline_id,
            track_index: 0,
            clip,
        }),
    );

    let mut state = TimelineState::default();
    state.playhead = 125; // t = 5s at 25fps

    let ctx = egui::Context::default();
    // Realistic viewport (800x600): with `RawInput::default()` the
    // headless viewport is enormous (10000x10000) and the content would
    // not be scrollable — the offset would be clamped to 0 and the test
    // would say nothing.
    let frame_input = || {
        let mut input = egui::RawInput::default();
        input.screen_rect = Some(egui::Rect::from_min_max(
            egui::pos2(0.0, 0.0),
            egui::pos2(800.0, 600.0),
        ));
        input
    };
    let mut render_frame = |state: &mut TimelineState| {
        let mut output = ctx.run_ui(frame_input(), |ui| {
            egui::CentralPanel::default().show(ui, |ui| {
                show_timeline(
                    ui,
                    &mut project,
                    &mut history,
                    timeline_id,
                    &|_id| "media".to_string(),
                    state,
                    true,
                    true,
                    &[],
                    &[],
                    &[],
                    &std::collections::HashMap::new(),
                    false,
                );
            });
        });
        output.textures_delta.clear();
    };

    // Frame 1: initializes the state of the ScrollArea.
    render_frame(&mut state);
    let scroll_id = {
        // Same id `show_timeline` uses for its ScrollArea:
        // `make_persistent_id` uses the *stable* id of the Ui (not the
        // auto-id counter), so it is enough to replicate the same
        // nesting structure — CentralPanel -> `horizontal_top`,
        // where inside `show_timeline` the ScrollArea lives. Careful to
        // use the `IdSalt` form and not the string: `Id::with(IdSalt)` and
        // `Id::with(&str)` give different ids for the same string.
        let mut captured = None;
        let mut output = ctx.run_ui(frame_input(), |ui| {
            egui::CentralPanel::default().show(ui, |ui| {
                ui.horizontal_top(|ui| {
                    captured = Some(ui.make_persistent_id(egui::IdSalt::new("timeline_scroll")));
                });
            });
        });
        output.textures_delta.clear();
        captured.expect("ScrollArea id")
    };

    // Simulates the user having already scrolled: offset 30px.
    {
        let mut st = egui::containers::scroll_area::State::load(&ctx, scroll_id)
            .expect("ScrollArea state after frame 1");
        assert_eq!(st.offset.x, 0.0);
        st.offset.x = 30.0;
        st.store(&ctx, scroll_id);
    }

    // Zoom in: pps 60 -> 75. The playhead (t=5s) must stay at the same
    // position on screen: offset' = 30 + 5 * (75 - 60) = 105.
    state.zoom_in();
    render_frame(&mut state);
    let st = egui::containers::scroll_area::State::load(&ctx, scroll_id).unwrap();
    assert_eq!(st.offset.x, 105.0);

    // Zoom out: pps 75 -> 60. The playhead did not move, so the offset
    // goes back exactly to 30.
    state.zoom_out();
    render_frame(&mut state);
    let st = egui::containers::scroll_area::State::load(&ctx, scroll_id).unwrap();
    assert_eq!(st.offset.x, 30.0);
}

/// Bug: the timeline panel (`Panel::bottom` with `show_timeline`
/// inside, see `main.rs`) went back to the size of the
/// content instead of staying at the one the user had
/// resized it to, as soon as a frame passed without interaction.
/// Cause: `ScrollArea` by default shrinks to the content instead
/// of filling the space assigned by the `Panel` (`auto_shrink` is
/// `true` on both axes by default). With few short clips
/// (real content much shorter than 240px) the panel, over several
/// consecutive frames without any interaction, must not shrink
/// below the requested size.
#[test]
fn show_timeline_panel_does_not_shrink_to_short_content() {
    let mut project = Project::default();
    let timeline_id = project.timelines.insert(vv_core::Timeline {
        name: "T".into(),
        fps: vv_core::Rational::new(25, 1),
        resolution: (1920, 1080),
        tracks: vec![vv_core::Track::new(TrackKind::Video)],
        markers: Vec::new(),
        master: Default::default(),
    });
    let mut history = History::default();
    let clip = Clip::from_source_range(
        project.alloc_clip_id(),
        vv_core::ClipSource::SolidColor,
        0,
        10,
        0,
        vv_core::Rational::one(),
    );
    history.do_command(
        &mut project,
        Box::new(vv_core::InsertClip {
            timeline: timeline_id,
            track_index: 0,
            clip,
        }),
    );
    let mut state = TimelineState::default();

    let ctx = egui::Context::default();
    let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(800.0, 600.0));
    let mut last_height = 0.0_f32;
    for _ in 0..4 {
        let raw = egui::RawInput {
            screen_rect: Some(screen),
            ..Default::default()
        };
        let mut output = ctx.run_ui(raw, |ui| {
            let panel_resp = egui::Panel::bottom("timeline_repro")
                .default_size(240.0)
                .resizable(true)
                .show(ui, |ui| {
                    show_timeline(
                        ui,
                        &mut project,
                        &mut history,
                        timeline_id,
                        &|_id| "media".to_string(),
                        &mut state,
                        true,
                        true,
                        &[],
                        &[],
                        &[],
                        &std::collections::HashMap::new(),
                        false,
                    );
                });
            last_height = panel_resp.response.rect.height();
        });
        output.textures_delta.clear();
    }
    assert!(
        last_height > 200.0,
        "the panel shrank to its content ({last_height}px) instead of staying near the requested 240px"
    );
}
