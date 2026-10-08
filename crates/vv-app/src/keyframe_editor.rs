//! Keyframe editor: floating window with, at the bottom, one row per
//! animated parameter of the selected clip and, at the top, the curve of the
//! chosen row. Drawn with the painter and with hand-written hit-testing, like
//! the timeline: the points are too many and too small for widgets.
//!
//! Keyframes live in *source* frames of the clip: all the coordinates here
//! are in that space, the conversion with the playhead happens at the edges.

use std::collections::HashSet;

use vv_core::{
    Clip, ClipId, EffectStack, FrameIdx, Interpolation, KeyframePick, KeyframeTarget, Project,
    TimelineId, TransformParam,
};

use crate::properties_panel::BoxedCommand;

const LABEL_WIDTH: f32 = 110.0;
const ROW_HEIGHT: f32 = 22.0;
const RULER_HEIGHT: f32 = 18.0;
const CURVE_HEIGHT: f32 = 170.0;
/// Hit radius of a keyframe: wider than the drawn point, which would
/// otherwise be nearly impossible to grab.
const PICK_RADIUS: f32 = 11.0;
/// Half diagonal of a keyframe diamond.
const DIAMOND_HALF: f32 = 7.0;
const HANDLE_RADIUS: f32 = 5.0;
/// The handles sit on the curve, between the points: their hit area is more
/// generous, or grabbing them with the mouse is a lottery.
const HANDLE_PICK_RADIUS: f32 = 10.0;
const PLAYHEAD_COLOR: egui::Color32 = crate::theme::PLAYHEAD;
const SELECTED_COLOR: egui::Color32 = crate::theme::ACCENT;
const CURVE_COLOR: egui::Color32 = egui::Color32::from_gray(200);
const PRESET_ICON_SIZE: egui::Vec2 = egui::vec2(28.0, 20.0);

/// What the pointer is dragging.
#[derive(Debug, Clone, Copy)]
enum Drag {
    /// Moving the selection in time. `applied` is the delta already sent
    /// to the history: each frame only the difference is sent.
    Time {
        origin_frame: FrameIdx,
        applied: FrameIdx,
    },
    /// Point of the curve: time as above, plus the value.
    Point {
        pick: KeyframePick,
        origin_frame: FrameIdx,
        applied: FrameIdx,
    },
    /// Handle of a bezier: `outgoing` tells the one of the segment's start
    /// keyframe from the one of its end keyframe.
    Handle { pick: KeyframePick, outgoing: bool },
}

#[derive(Debug, Default)]
pub(crate) struct KeyframeEditorState {
    /// The clip whose keyframes are being shown.
    clip: Option<(TimelineId, usize, ClipId)>,
    /// Row whose curve is visible.
    row: Option<KeyframeTarget>,
    selection: HashSet<KeyframePick>,
    drag: Option<Drag>,
    /// Selection rectangle in progress: origin, current corner and whether it
    /// started in the curve (the two areas must not steal it from each other).
    box_select: Option<(egui::Pos2, egui::Pos2, bool)>,
    /// Visible portion of the clip (first frame, duration) when zoomed;
    /// `None` = the whole clip.
    view: Option<(FrameIdx, FrameIdx)>,
    /// The last click landed in here: as for the media pool, it decides
    /// who takes Del.
    focused: bool,
}

impl KeyframeEditorState {
    /// Del deletes the selected keyframes only if the editor has the last
    /// click and something selected; otherwise it stays with the timeline,
    /// which deletes the clip.
    pub(crate) fn owns_delete(&self) -> bool {
        self.focused && !self.selection.is_empty()
    }

    /// The commands to remove the selected keyframes, which stop being
    /// selected.
    pub(crate) fn remove_selected(&mut self, zoom_link: bool) -> Vec<BoxedCommand> {
        let Some((timeline, track_index, clip_id)) = self.clip else {
            return Vec::new();
        };
        let picks = with_zoom_link(self.selection.drain().collect(), zoom_link);
        picks
            .into_iter()
            .map(|(target, frame)| {
                Box::new(vv_core::RemoveKeyframe::new(
                    timeline,
                    track_index,
                    clip_id,
                    target,
                    frame,
                )) as BoxedCommand
            })
            .collect()
    }

    /// The clip changed underneath (another selection, undo): what was
    /// selected no longer exists.
    fn reset_for(&mut self, clip: (TimelineId, usize, ClipId)) {
        if self.clip != Some(clip) {
            self.clip = Some(clip);
            self.row = None;
            self.selection.clear();
            self.drag = None;
            self.box_select = None;
            self.view = None;
        }
    }
}

/// The rows to show: the parameters with at least one keyframe.
fn rows(effects: &EffectStack) -> Vec<KeyframeTarget> {
    let mut rows: Vec<KeyframeTarget> = TransformParam::ALL
        .iter()
        .filter(|p| !effects.transform.track(**p).is_constant())
        .map(|p| KeyframeTarget::TransformParam(*p))
        .collect();
    if !effects.gain_db.is_constant() {
        rows.push(KeyframeTarget::Gain);
    }
    if effects.color.as_ref().is_some_and(|c| !c.is_constant()) {
        rows.push(KeyframeTarget::Color);
    }
    for filter in &effects.filters {
        if !filter.radius.is_constant() {
            rows.push(KeyframeTarget::FilterRadius(filter.kind));
        }
        if !filter.direction.is_constant() {
            rows.push(KeyframeTarget::FilterDirection(filter.kind));
        }
        if !filter.amount.is_constant() {
            rows.push(KeyframeTarget::FilterAmount(filter.kind));
        }
    }
    rows
}

fn filter(effects: &EffectStack, kind: vv_core::FilterKind) -> Option<&vv_core::ClipFilter> {
    effects.filters.iter().find(|f| f.kind == kind)
}

/// The frames of the keyframes of a row.
fn frames_of(effects: &EffectStack, target: KeyframeTarget) -> Vec<FrameIdx> {
    match target {
        KeyframeTarget::TransformParam(p) => effects
            .transform
            .track(p)
            .keyframes()
            .iter()
            .map(|k| k.0)
            .collect(),
        KeyframeTarget::Gain => effects.gain_db.keyframes().iter().map(|k| k.0).collect(),
        KeyframeTarget::Color => effects
            .color
            .as_ref()
            .map(|c| c.keyframes().iter().map(|k| k.0).collect())
            .unwrap_or_default(),
        KeyframeTarget::FilterRadius(kind) => filter(effects, kind)
            .map(|f| f.radius.keyframes().iter().map(|k| k.0).collect())
            .unwrap_or_default(),
        KeyframeTarget::FilterDirection(kind) => filter(effects, kind)
            .map(|f| f.direction.keyframes().iter().map(|k| k.0).collect())
            .unwrap_or_default(),
        KeyframeTarget::FilterAmount(kind) => filter(effects, kind)
            .map(|f| f.amount.keyframes().iter().map(|k| k.0).collect())
            .unwrap_or_default(),
    }
}

/// Value and interpolation of a scalar keyframe; `None` for the color and
/// the blur direction, which have no curve to draw.
fn scalar_at(
    effects: &EffectStack,
    target: KeyframeTarget,
    frame: FrameIdx,
) -> Option<(f32, Interpolation)> {
    match target {
        KeyframeTarget::TransformParam(p) => effects.transform.track(p).keyframe_at(frame),
        KeyframeTarget::Gain => effects.gain_db.keyframe_at(frame),
        KeyframeTarget::FilterRadius(kind) => filter(effects, kind)?.radius.keyframe_at(frame),
        KeyframeTarget::FilterAmount(kind) => filter(effects, kind)?.amount.keyframe_at(frame),
        KeyframeTarget::Color | KeyframeTarget::FilterDirection(_) => None,
    }
}

/// The scalar keyframes of a row, value and interpolation included.
fn scalar_keyframes(
    effects: &EffectStack,
    target: KeyframeTarget,
) -> Vec<(FrameIdx, f32, Interpolation)> {
    match target {
        KeyframeTarget::TransformParam(p) => effects.transform.track(p).keyframes().to_vec(),
        KeyframeTarget::Gain => effects.gain_db.keyframes().to_vec(),
        KeyframeTarget::FilterRadius(kind) => filter(effects, kind)
            .map(|f| f.radius.keyframes().to_vec())
            .unwrap_or_default(),
        KeyframeTarget::FilterAmount(kind) => filter(effects, kind)
            .map(|f| f.amount.keyframes().to_vec())
            .unwrap_or_default(),
        KeyframeTarget::Color | KeyframeTarget::FilterDirection(_) => Vec::new(),
    }
}

/// The continuous-time curve: `Keyframed::value_at` works on whole frames,
/// and up close its staircase is the frames', not the curve's.
fn sample(keyframes: &[(FrameIdx, f32, Interpolation)], at: f32) -> Option<f32> {
    let first = keyframes.first()?;
    let last = keyframes.last()?;
    if at <= first.0 as f32 {
        return Some(first.1);
    }
    if at >= last.0 as f32 {
        return Some(last.1);
    }
    let i = keyframes.partition_point(|(f, _, _)| (*f as f32) <= at) - 1;
    let ((f0, v0, interp), (f1, v1, _)) = (keyframes[i], keyframes[i + 1]);
    let t = (at - f0 as f32) / (f1 - f0) as f32;
    Some(v0 + (v1 - v0) * interp.ease(t))
}

fn scalar_value_at(effects: &EffectStack, target: KeyframeTarget, frame: FrameIdx) -> Option<f32> {
    match target {
        KeyframeTarget::TransformParam(p) => Some(effects.transform.track(p).value_at(frame)),
        KeyframeTarget::Gain => Some(effects.gain_db.value_at(frame)),
        KeyframeTarget::FilterRadius(kind) => Some(filter(effects, kind)?.radius.value_at(frame)),
        KeyframeTarget::FilterAmount(kind) => Some(filter(effects, kind)?.amount.value_at(frame)),
        KeyframeTarget::Color | KeyframeTarget::FilterDirection(_) => None,
    }
}

fn target_label(target: KeyframeTarget) -> String {
    use TransformParam as P;
    match target {
        KeyframeTarget::TransformParam(p) => match p {
            P::ZoomX => format!("{} X", t!("props.zoom")),
            P::ZoomY => format!("{} Y", t!("props.zoom")),
            P::PositionX => format!("{} X", t!("props.position")),
            P::PositionY => format!("{} Y", t!("props.position")),
            P::Rotation => t!("props.rotation").to_string(),
            P::AnchorX => format!("{} X", t!("props.anchor_point")),
            P::AnchorY => format!("{} Y", t!("props.anchor_point")),
            P::CropLeft => t!("props.crop_left").to_string(),
            P::CropTop => t!("props.crop_top").to_string(),
            P::CropRight => t!("props.crop_right").to_string(),
            P::CropBottom => t!("props.crop_bottom").to_string(),
            P::CropSoftness => t!("props.softness").to_string(),
            P::Opacity => t!("props.opacity").to_string(),
        },
        KeyframeTarget::Gain => t!("keyframes.gain").to_string(),
        KeyframeTarget::Color => t!("keyframes.color").to_string(),
        KeyframeTarget::FilterRadius(kind) => format!(
            "{} {}",
            crate::timeline_ui::filter_label(kind),
            t!("props.blur_radius")
        ),
        KeyframeTarget::FilterDirection(kind) => format!(
            "{} {}",
            crate::timeline_ui::filter_label(kind),
            t!("props.blur_direction")
        ),
        KeyframeTarget::FilterAmount(kind) => crate::timeline_ui::filter_label(kind).to_string(),
    }
}

fn preset_label(interpolation: Interpolation) -> String {
    match interpolation {
        Interpolation::Hold => t!("keyframes.hold"),
        Interpolation::Linear => t!("keyframes.linear"),
        Interpolation::EaseInOut => t!("keyframes.ease_in_out"),
        Interpolation::EaseIn => t!("keyframes.ease_in"),
        Interpolation::EaseOut => t!("keyframes.ease_out"),
        Interpolation::Bezier { .. } => t!("keyframes.bezier"),
    }
    .to_string()
}

/// Button drawing the easing curve of `preset`, its name in the tooltip.
fn preset_button(ui: &mut egui::Ui, preset: Interpolation) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(PRESET_ICON_SIZE, egui::Sense::click());
    let response = response.on_hover_text(preset_label(preset));
    let visuals = ui.style().interact(&response);
    ui.painter().rect_filled(rect, 3.0, visuals.weak_bg_fill);
    let icon = rect.shrink2(egui::vec2(7.0, 5.0));
    const STEPS: usize = 16;
    let points = (0..=STEPS)
        .map(|i| {
            let t = i as f32 / STEPS as f32;
            egui::pos2(
                egui::lerp(icon.left()..=icon.right(), t),
                egui::lerp(icon.bottom()..=icon.top(), preset.ease(t)),
            )
        })
        .collect();
    ui.painter().add(egui::Shape::line(
        points,
        egui::Stroke::new(1.5, visuals.fg_stroke.color),
    ));
    response
}

/// Mapping between source frames of the clip and x on screen.
#[derive(Clone, Copy)]
struct TimeAxis {
    left: f32,
    width: f32,
    first: FrameIdx,
    span: FrameIdx,
}

impl TimeAxis {
    /// `view` is the visible portion of the clip: first frame and duration.
    fn new(rect: egui::Rect, view: (FrameIdx, FrameIdx)) -> Self {
        Self {
            left: rect.left(),
            width: rect.width().max(1.0),
            first: view.0,
            span: view.1.max(1),
        }
    }

    /// How many frames a horizontal movement of `dx` pixels is worth.
    fn delta(&self, dx: f32) -> FrameIdx {
        (dx / self.width * self.span as f32).round() as FrameIdx
    }

    fn x(&self, frame: FrameIdx) -> f32 {
        self.left + (frame - self.first) as f32 / self.span as f32 * self.width
    }

    fn frame(&self, x: f32) -> FrameIdx {
        self.time(x).round() as FrameIdx
    }

    /// Like `frame`, but without rounding to the frame: for drawing.
    fn time(&self, x: f32) -> f32 {
        self.first as f32 + (x - self.left) / self.width * self.span as f32
    }
}

pub(crate) struct KeyframeEditorResponse {
    pub(crate) commands: Vec<BoxedCommand>,
    /// Timeline frame to move the playhead to.
    pub(crate) playhead: Option<FrameIdx>,
}

/// Draws the window. `target` is the clip to show (the first selected one)
/// and `playhead` the current timeline frame.
#[allow(clippy::too_many_arguments)]
pub(crate) fn show_keyframe_editor(
    ctx: &egui::Context,
    open: &mut bool,
    state: &mut KeyframeEditorState,
    project: &Project,
    target: Option<(TimelineId, usize, ClipId)>,
    playhead: FrameIdx,
    // Zoom locked to proportions in the Properties panel: here X and Y move
    // together, otherwise the editor would break the constraint.
    zoom_link: bool,
) -> KeyframeEditorResponse {
    let mut response = KeyframeEditorResponse {
        commands: Vec::new(),
        playhead: None,
    };
    let mut window_open = *open;
    let window = egui::Window::new(t!("keyframes.title"))
        .id(egui::Id::new("keyframe_editor"))
        .open(&mut window_open)
        .default_size([720.0, 340.0])
        .min_width(420.0)
        .show(ctx, |ui| {
            let clip = target.and_then(|(tl, track, id)| {
                project.timelines.get(tl).and_then(|t| t.clip(track, id))
            });
            let (Some((timeline, track_index, clip_id)), Some(clip)) = (target, clip) else {
                ui.label(t!("keyframes.no_clip"));
                return;
            };
            state.reset_for((timeline, track_index, clip_id));
            let rows = rows(&clip.effects);
            if rows.is_empty() {
                ui.label(t!("keyframes.no_keyframes"));
                return;
            }
            // A row that disappeared (undo, keyframes removed) must not stay open.
            if !state.row.is_some_and(|r| rows.contains(&r)) {
                state.row = Some(rows[0]);
            }
            state
                .selection
                .retain(|(t, f)| rows.contains(t) && frames_of(&clip.effects, *t).contains(f));

            show_toolbar(
                ui,
                state,
                zoom_link,
                (timeline, track_index, clip_id),
                &mut response,
            );
            ui.separator();

            let axis_rect = |rect: egui::Rect| rect.with_min_x(rect.left() + LABEL_WIDTH);
            let head = clip.source_frame_at(playhead);
            let full = (
                clip.source_in(),
                (clip.source_out() - clip.source_in()).max(1),
            );
            handle_zoom(ui, state, axis_rect(ui.available_rect_before_wrap()), full);
            let view = state.view.unwrap_or(full);

            if let Some(row) = state.row {
                let (rect, _) = ui.allocate_exact_size(
                    egui::vec2(ui.available_width(), CURVE_HEIGHT),
                    egui::Sense::hover(),
                );
                let axis = TimeAxis::new(axis_rect(rect), view);
                draw_curve(
                    ui,
                    rect,
                    axis,
                    state,
                    clip,
                    (timeline, track_index, clip_id),
                    row,
                    head,
                    zoom_link,
                    &mut response,
                );
            }

            let ruler = ui
                .allocate_exact_size(
                    egui::vec2(ui.available_width(), RULER_HEIGHT),
                    egui::Sense::hover(),
                )
                .0;
            let axis = TimeAxis::new(axis_rect(ruler), view);
            draw_ruler(ui, ruler, axis, clip, project, timeline, head);
            let ruler_response = ui.interact(
                axis_rect(ruler),
                ui.id().with("kf_ruler"),
                egui::Sense::click_and_drag(),
            );
            if let Some(pos) = ruler_response.interact_pointer_pos() {
                response.playhead = Some(clip.timeline_frame_at(
                    axis.frame(pos.x).clamp(clip.source_in(), clip.source_out()),
                ));
            }

            egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    draw_rows(
                        ui,
                        state,
                        clip,
                        (timeline, track_index, clip_id),
                        &rows,
                        view,
                        head,
                        zoom_link,
                        &mut response,
                    );
                });
        });
    *open = window_open;
    // Whoever got the last click decides who takes Del, as between
    // media pool and timeline.
    if let Some(pos) = ctx.input(|i| {
        i.pointer
            .any_pressed()
            .then(|| i.pointer.interact_pos())
            .flatten()
    }) {
        state.focused = *open && window.is_some_and(|w| w.response.rect.contains(pos));
    }
    response
}

/// Alt+scroll (or pinch) zooms around the pointer as on the timeline;
/// horizontal scroll pans the visible portion.
fn handle_zoom(
    ui: &egui::Ui,
    state: &mut KeyframeEditorState,
    rect: egui::Rect,
    full: (FrameIdx, FrameIdx),
) {
    // Only if the pointer is really over this window: underneath there is the
    // timeline, which zooms with the same gesture.
    let Some(pos) = ui
        .ctx()
        .pointer_hover_pos()
        .filter(|p| rect.contains(*p) && ui.ctx().layer_id_at(*p) == Some(ui.layer_id()))
    else {
        return;
    };
    let (zoom, pan) = ui.input(|i| (i.zoom_delta(), i.smooth_scroll_delta.x));
    if zoom == 1.0 && pan == 0.0 {
        return;
    }
    let axis = TimeAxis::new(rect, state.view.unwrap_or(full));
    let anchor = axis.frame(pos.x);
    let span = ((axis.span as f32 / zoom).round() as FrameIdx).clamp(2, full.1);
    // The frame under the pointer stays where it is.
    let first = anchor
        - ((anchor - axis.first) as f32 * span as f32 / axis.span as f32).round() as FrameIdx
        - axis.delta(pan);
    state.view = (span < full.1).then(|| (first.clamp(full.0, full.0 + full.1 - span), span));
}

/// With zoom locked to proportions, X and Y are twins: whatever is done
/// to the keyframe of one must be done to the other's at the same frame.
fn with_zoom_link(picks: Vec<KeyframePick>, zoom_link: bool) -> Vec<KeyframePick> {
    if !zoom_link {
        return picks;
    }
    let mut all = picks.clone();
    for (target, frame) in picks {
        let KeyframeTarget::TransformParam(param) = target else {
            continue;
        };
        let twin = match param {
            TransformParam::ZoomX => TransformParam::ZoomY,
            TransformParam::ZoomY => TransformParam::ZoomX,
            _ => continue,
        };
        let pick = (KeyframeTarget::TransformParam(twin), frame);
        if !all.contains(&pick) {
            all.push(pick);
        }
    }
    all
}

fn show_toolbar(
    ui: &mut egui::Ui,
    state: &mut KeyframeEditorState,
    zoom_link: bool,
    clip: (TimelineId, usize, ClipId),
    response: &mut KeyframeEditorResponse,
) {
    ui.horizontal(|ui| {
        ui.add_enabled_ui(!state.selection.is_empty(), |ui| {
            for preset in Interpolation::PRESETS {
                if preset_button(ui, preset).clicked() {
                    let picks =
                        with_zoom_link(state.selection.iter().copied().collect(), zoom_link);
                    response
                        .commands
                        .push(Box::new(vv_core::SetKeyframeInterpolation::new(
                            clip.0, clip.1, clip.2, picks, preset,
                        )));
                }
            }
        });
    });
}

/// Ticks with the timecode of the corresponding *timeline* frame: that is
/// what one reads on the playback bar.
fn draw_ruler(
    ui: &egui::Ui,
    rect: egui::Rect,
    axis: TimeAxis,
    clip: &Clip,
    project: &Project,
    timeline: TimelineId,
    head: FrameIdx,
) {
    let painter = ui.painter_at(rect);
    let visuals = ui.visuals();
    painter.rect_filled(rect, 0.0, visuals.extreme_bg_color);
    let fps = project.timelines[timeline].fps.as_f64().max(1.0);
    // A tick every ~90 px, rounded to a round number of frames.
    let per_tick = ((axis.span as f32 * 90.0 / axis.width.max(1.0)).ceil() as FrameIdx).max(1);
    let step = round_step(per_tick);
    let mut frame = axis.first - axis.first.rem_euclid(step);
    while frame <= axis.first + axis.span {
        if frame >= axis.first {
            let x = axis.x(frame);
            painter.line_segment(
                [
                    egui::pos2(x, rect.bottom() - 5.0),
                    egui::pos2(x, rect.bottom()),
                ],
                egui::Stroke::new(1.0, visuals.weak_text_color()),
            );
            painter.text(
                egui::pos2(x + 3.0, rect.top()),
                egui::Align2::LEFT_TOP,
                crate::timeline_ui::format_timecode(
                    clip.timeline_frame_at(frame) as f64 / fps,
                    fps,
                ),
                egui::FontId::proportional(10.0),
                visuals.weak_text_color(),
            );
        }
        frame += step;
    }
    let x = axis.x(head);
    painter.line_segment(
        [egui::pos2(x, rect.top()), egui::pos2(x, rect.bottom())],
        egui::Stroke::new(1.0, PLAYHEAD_COLOR),
    );
}

/// Nearest "round" step (1, 2, 5, 10, 20, 50, …), rounding up.
fn round_step(minimum: FrameIdx) -> FrameIdx {
    let mut step = 1;
    while step < minimum {
        let digits = [1, 2, 5];
        let next = digits
            .iter()
            .map(|d| d * step)
            .find(|s| *s > step && *s >= minimum);
        step = next.unwrap_or(step * 10);
    }
    step
}

#[allow(clippy::too_many_arguments)]
fn draw_curve(
    ui: &mut egui::Ui,
    rect: egui::Rect,
    axis: TimeAxis,
    state: &mut KeyframeEditorState,
    clip: &Clip,
    clip_ref: (TimelineId, usize, ClipId),
    row: KeyframeTarget,
    head: FrameIdx,
    zoom_link: bool,
    response: &mut KeyframeEditorResponse,
) {
    let plot = rect.with_min_x(axis.left);
    let painter = ui.painter_at(rect);
    let visuals = ui.visuals().clone();
    painter.rect_filled(plot, 0.0, visuals.faint_bg_color);

    let frames = frames_of(&clip.effects, row);
    if scalar_value_at(&clip.effects, row, axis.first).is_none() {
        painter.text(
            plot.center(),
            egui::Align2::CENTER_CENTER,
            t!("keyframes.no_curve"),
            egui::FontId::proportional(12.0),
            visuals.weak_text_color(),
        );
        return;
    }

    let values: Vec<f32> = frames
        .iter()
        .filter_map(|f| scalar_value_at(&clip.effects, row, *f))
        .collect();
    let (mut low, mut high) = values
        .iter()
        .fold((f32::MAX, f32::MIN), |(lo, hi), v| (lo.min(*v), hi.max(*v)));
    if (high - low).abs() < 1e-6 {
        low -= 1.0;
        high += 1.0;
    }
    let margin = (high - low) * 0.15;
    let (low, high) = (low - margin, high + margin);
    let y = |v: f32| plot.bottom() - (v - low) / (high - low) * plot.height();
    let value_at_y = |py: f32| low + (plot.bottom() - py) / plot.height() * (high - low);

    // Grid and value scale in the left column.
    for i in 0..=4 {
        let value = low + (high - low) * i as f32 / 4.0;
        let py = y(value);
        painter.line_segment(
            [egui::pos2(plot.left(), py), egui::pos2(plot.right(), py)],
            visuals.widgets.noninteractive.bg_stroke,
        );
        painter.text(
            egui::pos2(plot.left() - 6.0, py),
            egui::Align2::RIGHT_CENTER,
            format!("{value:.2}"),
            egui::FontId::proportional(10.0),
            visuals.weak_text_color(),
        );
    }

    // One pixel column per sample, in continuous time: at high zoom one
    // sample per frame would give a staircase.
    let curve = scalar_keyframes(&clip.effects, row);
    let mut points = Vec::new();
    let mut x = plot.left();
    while x <= plot.right() {
        if let Some(v) = sample(&curve, axis.time(x)) {
            points.push(egui::pos2(x, y(v)));
        }
        x += 1.0;
    }
    painter.add(egui::Shape::line(
        points,
        egui::Stroke::new(1.5, CURVE_COLOR),
    ));

    let head_x = axis.x(head);
    painter.line_segment(
        [
            egui::pos2(head_x, plot.top()),
            egui::pos2(head_x, plot.bottom()),
        ],
        egui::Stroke::new(1.0, PLAYHEAD_COLOR),
    );

    let point_pos = |frame: FrameIdx| {
        scalar_value_at(&clip.effects, row, frame).map(|v| egui::pos2(axis.x(frame), y(v)))
    };

    // Segments to show the handles on: the one leaving a selected keyframe
    // and the one entering it, so even a single selected keyframe has one
    // attached.
    let mut segments: Vec<usize> = Vec::new();
    for (i, &frame) in frames.iter().enumerate() {
        if !state.selection.contains(&(row, frame)) {
            continue;
        }
        if i + 1 < frames.len() {
            segments.push(i);
        }
        if i > 0 {
            segments.push(i - 1);
        }
    }
    segments.sort_unstable();
    segments.dedup();

    let mut handles: Vec<(KeyframePick, bool, egui::Pos2)> = Vec::new();
    for i in segments {
        let (frame, next) = (frames[i], frames[i + 1]);
        let (Some((_, interp)), Some(from), Some(to)) = (
            scalar_at(&clip.effects, row, frame),
            point_pos(frame),
            point_pos(next),
        ) else {
            continue;
        };
        let Some((c1, c2)) = interp.control_points() else {
            continue;
        };
        let control = |c: [f32; 2]| {
            egui::pos2(
                from.x + (to.x - from.x) * c[0],
                from.y + (to.y - from.y) * c[1],
            )
        };
        for (outgoing, pos) in [(true, control(c1)), (false, control(c2))] {
            let anchor = if outgoing { from } else { to };
            painter.line_segment(
                [anchor, pos],
                egui::Stroke::new(1.0, visuals.weak_text_color()),
            );
            painter.rect_filled(
                egui::Rect::from_center_size(pos, egui::Vec2::splat(HANDLE_RADIUS * 1.6)),
                1.0,
                visuals.weak_text_color(),
            );
            handles.push(((row, frame), outgoing, pos));
        }
    }

    for &frame in &frames {
        let Some(pos) = point_pos(frame) else {
            continue;
        };
        let selected = state.selection.contains(&(row, frame));
        paint_keyframe_diamond(&painter, pos, DIAMOND_HALF, selected, CURVE_COLOR);
    }

    let interaction = ui.interact(
        plot,
        ui.id().with("kf_curve"),
        egui::Sense::click_and_drag(),
    );
    let additive = ui.input(|i| i.modifiers.shift || i.modifiers.command);

    if interaction.drag_started()
        && let Some(pos) = interaction.interact_pointer_pos()
    {
        let handle = handles
            .iter()
            .filter(|(_, _, p)| p.distance(pos) <= HANDLE_PICK_RADIUS)
            .min_by(|a, b| a.2.distance(pos).total_cmp(&b.2.distance(pos)))
            .map(|(pick, outgoing, _)| (*pick, *outgoing));
        let point = frames
            .iter()
            .filter_map(|f| point_pos(*f).map(|p| (*f, p)))
            .filter(|(_, p)| p.distance(pos) <= PICK_RADIUS)
            .min_by(|a, b| a.1.distance(pos).total_cmp(&b.1.distance(pos)))
            .map(|(f, _)| f);
        state.drag = match (handle, point) {
            (Some((pick, outgoing)), _) => Some(Drag::Handle { pick, outgoing }),
            (None, Some(frame)) => {
                if !additive && !state.selection.contains(&(row, frame)) {
                    state.selection.clear();
                }
                state.selection.insert((row, frame));
                Some(Drag::Point {
                    pick: (row, frame),
                    origin_frame: axis.frame(pos.x),
                    applied: 0,
                })
            }
            (None, None) => {
                if !additive {
                    state.selection.clear();
                }
                state.box_select = Some((pos, pos, true));
                None
            }
        };
    }

    if interaction.dragged()
        && let Some(pos) = interaction.interact_pointer_pos()
    {
        match state.drag {
            Some(Drag::Handle { pick, outgoing }) => {
                let next = frames.iter().find(|f| **f > pick.1).copied();
                if let (Some(next), Some((_, interp))) =
                    (next, scalar_at(&clip.effects, row, pick.1))
                    && let (Some(from), Some(to)) = (point_pos(pick.1), point_pos(next))
                    && let Some((c1, c2)) = interp.control_points()
                {
                    let span_x = to.x - from.x;
                    let (v0, v1) = (value_at_y(from.y), value_at_y(to.y));
                    let nx = if span_x.abs() > 1e-3 {
                        ((pos.x - from.x) / span_x).clamp(0.0, 1.0)
                    } else {
                        0.5
                    };
                    // On a flat segment the normalized y does not exist:
                    // the handle stays where it is and moves only horizontally.
                    let ny = if (v1 - v0).abs() > 1e-6 {
                        (value_at_y(pos.y) - v0) / (v1 - v0)
                    } else if outgoing {
                        c1[1]
                    } else {
                        c2[1]
                    };
                    let (c1, c2) = if outgoing {
                        ([nx, ny], c2)
                    } else {
                        (c1, [nx, ny])
                    };
                    response
                        .commands
                        .push(Box::new(vv_core::SetKeyframeInterpolation::new(
                            clip_ref.0,
                            clip_ref.1,
                            clip_ref.2,
                            with_zoom_link(vec![pick], zoom_link),
                            Interpolation::Bezier { c1, c2 },
                        )));
                }
            }
            Some(Drag::Point {
                pick,
                origin_frame,
                applied,
            }) => {
                let wanted = axis.frame(pos.x) - origin_frame;
                let step = wanted - applied;
                let (tl, track, id) = clip_ref;
                if step != 0 {
                    let picks =
                        with_zoom_link(state.selection.iter().copied().collect(), zoom_link);
                    response.commands.push(Box::new(vv_core::MoveKeyframes::new(
                        tl, track, id, picks, step,
                    )));
                    state.selection = state
                        .selection
                        .iter()
                        .map(|(t, f)| (*t, f + step))
                        .collect();
                    state.drag = Some(Drag::Point {
                        pick: (pick.0, pick.1 + step),
                        origin_frame,
                        applied: wanted,
                    });
                }
                // The value follows the pointer on the dragged keyframe,
                // not on the whole selection: moving them all vertically
                // would mean different scales for different parameters.
                let frame = pick.1 + step;
                // `clip` is still the one from before this frame's commands:
                // the interpolation is read at the old position.
                if let Some((_, interp)) = scalar_at(&clip.effects, row, pick.1) {
                    let value = value_at_y(pos.y);
                    // The value must be replicated on the twin too, or the locked
                    // zoom would stay locked in name only.
                    for (target, _) in with_zoom_link(vec![(row, pick.1)], zoom_link) {
                        if scalar_at(&clip.effects, target, pick.1).is_none() {
                            continue;
                        }
                        response
                            .commands
                            .push(Box::new(vv_core::UpsertKeyframe::new(
                                tl,
                                track,
                                id,
                                frame,
                                keyframe_value(target, value),
                                interp,
                            )));
                    }
                }
            }
            _ => {}
        }
        if let Some((origin, _, true)) = state.box_select {
            state.box_select = Some((origin, pos, true));
        }
    }

    if let Some((origin, current, true)) = state.box_select {
        let rect = egui::Rect::from_two_pos(origin, current);
        painter.rect_stroke(
            rect,
            0.0,
            egui::Stroke::new(1.0, visuals.selection.bg_fill),
            egui::StrokeKind::Inside,
        );
        if interaction.drag_stopped() {
            for frame in &frames {
                if point_pos(*frame).is_some_and(|p| rect.contains(p)) {
                    state.selection.insert((row, *frame));
                }
            }
            state.box_select = None;
        }
    }
    if interaction.drag_stopped() {
        state.drag = None;
    }
}

fn keyframe_value(target: KeyframeTarget, value: f32) -> vv_core::KeyframeValue {
    match target {
        KeyframeTarget::TransformParam(p) => vv_core::KeyframeValue::TransformParam(p, value),
        KeyframeTarget::Gain => vv_core::KeyframeValue::Gain(value),
        KeyframeTarget::FilterRadius(kind) => vv_core::KeyframeValue::FilterRadius(kind, value),
        KeyframeTarget::FilterAmount(kind) => vv_core::KeyframeValue::FilterAmount(kind, value),
        // Without a curve one does not get here: they are edited from the Properties panel.
        KeyframeTarget::Color | KeyframeTarget::FilterDirection(_) => {
            vv_core::KeyframeValue::Gain(value)
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn draw_rows(
    ui: &mut egui::Ui,
    state: &mut KeyframeEditorState,
    clip: &Clip,
    clip_ref: (TimelineId, usize, ClipId),
    rows: &[KeyframeTarget],
    view: (FrameIdx, FrameIdx),
    head: FrameIdx,
    zoom_link: bool,
    response: &mut KeyframeEditorResponse,
) {
    // The space left over under the last row is part of the area too:
    // one must be able to start a selection box there.
    let height = rows.len() as f32 * ROW_HEIGHT;
    let (rect, _) = ui.allocate_exact_size(
        egui::vec2(ui.available_width(), height.max(ui.available_height())),
        egui::Sense::hover(),
    );
    let track_rect = rect.with_min_x(rect.left() + LABEL_WIDTH);
    let axis = TimeAxis::new(track_rect, view);
    let painter = ui.painter_at(rect);
    let visuals = ui.visuals().clone();

    let row_rect = |i: usize| {
        egui::Rect::from_min_size(
            egui::pos2(rect.left(), rect.top() + i as f32 * ROW_HEIGHT),
            egui::vec2(rect.width(), ROW_HEIGHT),
        )
    };

    for (i, &row) in rows.iter().enumerate() {
        let r = row_rect(i);
        if state.row == Some(row) {
            painter.rect_filled(r, 0.0, visuals.faint_bg_color);
        }
        painter.text(
            egui::pos2(r.left() + LABEL_WIDTH - 6.0, r.center().y),
            egui::Align2::RIGHT_CENTER,
            target_label(row),
            egui::FontId::proportional(11.0),
            if state.row == Some(row) {
                visuals.strong_text_color()
            } else {
                visuals.text_color()
            },
        );
        let strip = r.with_min_x(track_rect.left());
        painter.line_segment(
            [
                egui::pos2(strip.left(), strip.center().y),
                egui::pos2(strip.right(), strip.center().y),
            ],
            egui::Stroke::new(1.0, visuals.faint_bg_color),
        );
        for frame in frames_of(&clip.effects, row) {
            let pos = egui::pos2(axis.x(frame), strip.center().y);
            paint_keyframe_diamond(
                &painter,
                pos,
                DIAMOND_HALF,
                state.selection.contains(&(row, frame)),
                visuals.text_color(),
            );
        }
    }

    let head_x = axis.x(head);
    painter.line_segment(
        [
            egui::pos2(head_x, rect.top()),
            egui::pos2(head_x, rect.bottom()),
        ],
        egui::Stroke::new(1.0, PLAYHEAD_COLOR),
    );

    let interaction = ui.interact(rect, ui.id().with("kf_rows"), egui::Sense::click_and_drag());
    let additive = ui.input(|i| i.modifiers.shift || i.modifiers.command);
    let row_at = |pos: egui::Pos2| {
        let i = ((pos.y - rect.top()) / ROW_HEIGHT).floor() as isize;
        (i >= 0).then(|| rows.get(i as usize).copied()).flatten()
    };
    let hit = |pos: egui::Pos2| -> Option<KeyframePick> {
        let row = row_at(pos)?;
        frames_of(&clip.effects, row)
            .into_iter()
            .map(|f| (f, (axis.x(f) - pos.x).abs()))
            .filter(|(_, d)| *d <= PICK_RADIUS)
            .min_by(|a, b| a.1.total_cmp(&b.1))
            .map(|(f, _)| (row, f))
    };

    if interaction.drag_started()
        && let Some(pos) = interaction.interact_pointer_pos()
    {
        if pos.x < track_rect.left()
            && let Some(row) = row_at(pos)
        {
            state.row = Some(row);
        } else if let Some(pick) = hit(pos) {
            state.row = Some(pick.0);
            if !additive && !state.selection.contains(&pick) {
                state.selection.clear();
            }
            state.selection.insert(pick);
            state.drag = Some(Drag::Time {
                origin_frame: axis.frame(pos.x),
                applied: 0,
            });
        } else {
            if !additive {
                state.selection.clear();
            }
            state.box_select = Some((pos, pos, false));
        }
    }

    if interaction.dragged()
        && let Some(pos) = interaction.interact_pointer_pos()
        && let Some(Drag::Time {
            origin_frame,
            applied,
        }) = state.drag
    {
        let wanted = axis.frame(pos.x) - origin_frame;
        let step = wanted - applied;
        if step != 0 {
            let picks = with_zoom_link(state.selection.iter().copied().collect(), zoom_link);
            response.commands.push(Box::new(vv_core::MoveKeyframes::new(
                clip_ref.0, clip_ref.1, clip_ref.2, picks, step,
            )));
            state.selection = state
                .selection
                .iter()
                .map(|(t, f)| (*t, f + step))
                .collect();
            state.drag = Some(Drag::Time {
                origin_frame,
                applied: wanted,
            });
        }
    }

    if let Some((origin, current, false)) = state.box_select {
        let current = interaction.interact_pointer_pos().unwrap_or(current);
        state.box_select = Some((origin, current, false));
        let select = egui::Rect::from_two_pos(origin, current);
        painter.rect_stroke(
            select,
            0.0,
            egui::Stroke::new(1.0, visuals.selection.bg_fill),
            egui::StrokeKind::Inside,
        );
        if interaction.drag_stopped() {
            for (i, &row) in rows.iter().enumerate() {
                let y = row_rect(i).center().y;
                for frame in frames_of(&clip.effects, row) {
                    if select.contains(egui::pos2(axis.x(frame), y)) {
                        state.selection.insert((row, frame));
                    }
                }
            }
            state.box_select = None;
        }
    }

    if interaction.clicked()
        && let Some(pos) = interaction.interact_pointer_pos()
    {
        if pos.x < track_rect.left()
            && let Some(row) = row_at(pos)
        {
            state.row = Some(row);
        } else if let Some(pick) = hit(pos) {
            state.row = Some(pick.0);
            if !additive {
                state.selection.clear();
            }
            state.selection.insert(pick);
        }
    }

    if interaction.drag_stopped() {
        state.drag = None;
    }
}

#[cfg(test)]
#[path = "tests/keyframe_editor.rs"]
mod tests;

/// Keyframe mark: a diamond, outlined when unselected, filled with the
/// accent when selected.
pub(crate) fn paint_keyframe_diamond(
    painter: &egui::Painter,
    pos: egui::Pos2,
    r: f32,
    selected: bool,
    outline: egui::Color32,
) {
    let points = vec![
        pos + egui::vec2(0.0, -r),
        pos + egui::vec2(r, 0.0),
        pos + egui::vec2(0.0, r),
        pos + egui::vec2(-r, 0.0),
    ];
    if selected {
        painter.add(egui::Shape::convex_polygon(
            points,
            SELECTED_COLOR,
            egui::Stroke::NONE,
        ));
    } else {
        painter.add(egui::Shape::closed_line(
            points,
            egui::Stroke::new(1.5, outline),
        ));
    }
}
