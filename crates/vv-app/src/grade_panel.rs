//! Color correction section of the properties panel: the four wheels, the
//! saturations and the ranges of `vv_core::GradeParam`.

use vv_core::{
    FrameIdx, GradeParam, GradePreset, GradeValue, GradeWheel, KeyframeTarget, KeyframeValue,
};

use crate::PanelTarget;
use crate::properties_panel::{
    BoxedCommand, remove_keyframe, set_filters, target_effects, upsert_keyframe,
};
use crate::properties_panel::{
    KeyframeEdit, RowKeyframe, drag_field, keyframe_arrow, keyframe_button, param_row, slider_field,
};

fn wheel_label(wheel: GradeWheel) -> std::borrow::Cow<'static, str> {
    match wheel {
        GradeWheel::Shadows => t!("props.grade_shadows"),
        GradeWheel::Midtones => t!("props.grade_midtones"),
        GradeWheel::Highlights => t!("props.grade_highlights"),
        GradeWheel::Offset => t!("props.grade_offset"),
    }
}

fn preset_label(preset: GradePreset) -> std::borrow::Cow<'static, str> {
    match preset {
        GradePreset::Neutral => t!("props.grade_neutral"),
        GradePreset::BlackAndWhite => t!("filter.black_and_white"),
    }
}

pub(crate) fn grade_param_label(param: GradeParam) -> String {
    let wheel = GradeWheel::ALL
        .into_iter()
        .find(|w| [w.x(), w.y(), w.luma(), w.saturation()].contains(&param));
    match (param, wheel) {
        (GradeParam::LowRange, _) => t!("props.grade_low_range").to_string(),
        (GradeParam::HighRange, _) => t!("props.grade_high_range").to_string(),
        (GradeParam::Saturation, _) => t!("props.saturation").to_string(),
        (_, Some(wheel)) => {
            let part = if param == wheel.x() {
                "X".into()
            } else if param == wheel.y() {
                "Y".into()
            } else if param == wheel.luma() {
                t!("props.grade_luminance")
            } else {
                t!("props.saturation")
            };
            format!("{} {part}", wheel_label(wheel))
        }
        (_, None) => format!("{param:?}"),
    }
}

/// The color the wheel shows at angle `angle`: what a push that way adds,
/// over a mid grey.
fn hue_at(angle: f32) -> egui::Color32 {
    let shift = vv_core::chroma_shift(angle.cos(), angle.sin());
    let [r, g, b] = shift
        .map(|c| ((0.5 + c / vv_core::WHEEL_CHROMA_STRENGTH * 0.3).clamp(0.0, 1.0) * 255.0) as u8);
    egui::Color32::from_rgb(r, g, b)
}

/// A color wheel: the puck at `(x, y)` in the unit disc (Y up), dragged
/// relative to where it is, finer with Shift. The pointer is locked and
/// hidden meanwhile, so only the puck moves. `true` if it moved; a double
/// click asks for a reset.
fn wheel(
    ui: &mut egui::Ui,
    id: egui::Id,
    size: f32,
    x: &mut f32,
    y: &mut f32,
    reset: &mut bool,
) -> bool {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(size, size), egui::Sense::hover());
    let response = ui
        .interact(rect, id, egui::Sense::click_and_drag())
        .on_hover_text(t!("props.grade_wheel_hint"));
    let center = rect.center();
    let radius = size / 2.0 - 6.0;
    let painter = ui.painter_at(rect);
    let visuals = ui.visuals();
    painter.circle_filled(center, radius, visuals.extreme_bg_color);
    const SEGMENTS: usize = 72;
    for i in 0..SEGMENTS {
        let a0 = i as f32 / SEGMENTS as f32 * std::f32::consts::TAU;
        let a1 = (i + 1) as f32 / SEGMENTS as f32 * std::f32::consts::TAU;
        let point = |a: f32| center + egui::vec2(a.cos(), -a.sin()) * radius;
        painter.line_segment(
            [point(a0), point(a1)],
            egui::Stroke::new(4.0, hue_at((a0 + a1) / 2.0)),
        );
    }
    let faint = egui::Stroke::new(1.0, visuals.widgets.noninteractive.bg_stroke.color);
    painter.line_segment(
        [
            center - egui::vec2(radius, 0.0),
            center + egui::vec2(radius, 0.0),
        ],
        faint,
    );
    painter.line_segment(
        [
            center - egui::vec2(0.0, radius),
            center + egui::vec2(0.0, radius),
        ],
        faint,
    );

    // Pointer travel from the center to the edge, in radii.
    const TRAVEL: f32 = 2.5;
    if response.drag_started() {
        ui.ctx()
            .send_viewport_cmd(egui::ViewportCommand::CursorGrab(
                egui::viewport::CursorGrab::Locked,
            ));
        ui.ctx()
            .send_viewport_cmd(egui::ViewportCommand::CursorVisible(false));
    }
    if response.drag_stopped() {
        ui.ctx()
            .send_viewport_cmd(egui::ViewportCommand::CursorGrab(
                egui::viewport::CursorGrab::None,
            ));
        ui.ctx()
            .send_viewport_cmd(egui::ViewportCommand::CursorVisible(true));
    }
    let mut changed = false;
    if response.double_clicked() {
        *reset = true;
    } else if response.dragged() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::None);
        // Locked, the pointer reports only its motion (see `drag_field`).
        let (motion, precise) = ui.input(|i| (i.pointer.motion(), i.modifiers.shift));
        let fine = if precise { 0.2 } else { 1.0 };
        let delta = motion.unwrap_or_default() / (radius * TRAVEL) * fine;
        let (mut nx, mut ny) = (*x + delta.x, *y - delta.y);
        let length = (nx * nx + ny * ny).sqrt();
        if length > 1.0 {
            nx /= length;
            ny /= length;
        }
        if (nx, ny) != (*x, *y) {
            (*x, *y) = (nx, ny);
            changed = true;
        }
    }
    let puck = center + egui::vec2(*x, -*y) * radius;
    let accent = if response.dragged() || response.hovered() {
        crate::theme::ACCENT
    } else {
        visuals.strong_text_color()
    };
    painter.circle_stroke(puck, 5.0, egui::Stroke::new(2.0, accent));
    changed
}

/// A wheel's params act as one keyframe row: the diamond and the arrows
/// follow whichever of them has keyframes.
fn joint_keyframe(keys: impl IntoIterator<Item = RowKeyframe>) -> RowKeyframe {
    let nearest = |p: Option<FrameIdx>,
                   q: Option<FrameIdx>,
                   pick: fn(FrameIdx, FrameIdx) -> FrameIdx| {
        match (p, q) {
            (Some(p), Some(q)) => Some(pick(p, q)),
            (p, q) => p.or(q),
        }
    };
    keys.into_iter()
        .reduce(|a, b| RowKeyframe {
            on_keyframe: a.on_keyframe || b.on_keyframe,
            prev: nearest(a.prev, b.prev, FrameIdx::max),
            next: nearest(a.next, b.next, FrameIdx::min),
        })
        .expect("a wheel has params")
}

/// Width of one wheel with its controls; the grid fits as many per row as
/// the panel allows.
/// Minimum width of one wheel with its controls: enough for the longest name
/// next to the keyframe group. The blocks of a row share the panel's width.
const WHEEL_BLOCK_WIDTH: f32 = 140.0;
const MAX_WHEEL_SIZE: f32 = 150.0;

/// Wheels per row for `width`: 1, 2 or all 4 (never 3 + 1).
fn wheel_columns(width: f32, spacing: f32) -> usize {
    match ((width + spacing) / (WHEEL_BLOCK_WIDTH + spacing)) as usize {
        0 | 1 => 1,
        2 | 3 => 2,
        _ => 4,
    }
}

#[derive(Default)]
pub(crate) struct GradeSectionResponse {
    pub(crate) keyframes: Vec<(KeyframeEdit, KeyframeTarget)>,
    /// Params back to neutral, keyframes dropped.
    pub(crate) reset: Vec<GradeParam>,
    pub(crate) preset: Option<GradePreset>,
    /// Balance on the viewer's frame asked: see `balance_edits`.
    pub(crate) auto_balance: bool,
    /// Source frame to move the playhead to (keyframe arrow clicked).
    pub(crate) goto: Option<FrameIdx>,
}

impl GradeSectionResponse {
    fn set(&mut self, param: GradeParam, value: f32) {
        self.keyframes.push((
            KeyframeEdit::Set(KeyframeValue::Grade(param, value)),
            KeyframeTarget::Grade(param),
        ));
    }

    fn toggle(&mut self, param: GradeParam, on_keyframe: bool) {
        self.keyframes.push((
            KeyframeEdit::Toggle(on_keyframe),
            KeyframeTarget::Grade(param),
        ));
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.keyframes.is_empty() && self.reset.is_empty() && self.preset.is_none()
    }
}

/// The commands applying `section` to the color correction of every target
/// that has one: a value is the track's default while it has no keyframes,
/// a keyframe at the target's frame otherwise.
pub(crate) fn grade_commands(
    tl: Option<&vv_core::Timeline>,
    targets: &[PanelTarget],
    section: &GradeSectionResponse,
) -> Vec<BoxedCommand> {
    let mut commands = Vec::new();
    if section.is_empty() {
        return commands;
    }
    for t in targets {
        let Some(effects) = target_effects(tl, t) else {
            continue;
        };
        let Some(pos) = effects.filters.iter().position(|f| f.kind.has_grade()) else {
            continue;
        };
        let clip_ref = (t.timeline, t.track_index, t.clip_id);
        let mut filters = effects.filters.clone();
        let grade = &mut filters[pos].grade;
        if let Some(preset) = section.preset {
            grade.apply_preset(preset);
        }
        for param in &section.reset {
            *grade.track_mut(*param) = vv_core::Keyframed::constant(param.neutral());
        }
        let mut keyframes = Vec::new();
        for (edit, target) in &section.keyframes {
            match *edit {
                KeyframeEdit::Set(KeyframeValue::Grade(param, v))
                    if grade.track(param).is_constant() =>
                {
                    grade.track_mut(param).default = v;
                }
                KeyframeEdit::Set(value) => {
                    keyframes.push(upsert_keyframe(clip_ref, t.source_frame, value));
                }
                KeyframeEdit::Toggle(true) => {
                    keyframes.push(remove_keyframe(clip_ref, t.source_frame, *target));
                }
                KeyframeEdit::Toggle(false) => {
                    if let KeyframeTarget::Grade(param) = *target {
                        let value = grade.track(param).value_at(t.source_frame);
                        keyframes.push(upsert_keyframe(
                            clip_ref,
                            t.source_frame,
                            KeyframeValue::Grade(param, value),
                        ));
                    }
                }
            }
        }
        if filters != effects.filters {
            commands.push(set_filters(clip_ref, filters));
        }
        commands.append(&mut keyframes);
    }
    commands
}

/// The wheel moves of an auto balance of `grade` on the RGBA `pixels` of
/// the viewer's frame, as edits of `section` (keyframes where animated).
pub(crate) fn balance_edits(section: &mut GradeSectionResponse, grade: &GradeValue, pixels: &[u8]) {
    // A quarter of a million pixels measure a frame's neutral surfaces well
    // enough, whatever its resolution.
    let pixels = pixels.as_chunks::<4>().0;
    let step = (pixels.len() / 250_000).max(1);
    let balanced = vv_core::auto_balance(
        grade,
        pixels
            .iter()
            .step_by(step)
            .map(|p| [p[0], p[1], p[2]].map(|c| c as f32 / 255.0)),
    );
    for param in GradeParam::ALL {
        if balanced.get(param) != grade.get(param) {
            section.set(param, balanced.get(param));
        }
    }
}

/// Adds a neutral color correction to the targets that have none.
pub(crate) fn add_grade_commands(
    tl: Option<&vv_core::Timeline>,
    targets: &[PanelTarget],
) -> Vec<BoxedCommand> {
    targets
        .iter()
        .filter_map(|t| {
            let effects = target_effects(tl, t)?;
            if effects.filters.iter().any(|f| f.kind.has_grade()) {
                return None;
            }
            let mut filters = effects.filters.clone();
            filters.push(vv_core::ClipFilter::new(
                vv_core::FilterKind::ColorCorrection,
            ));
            Some(set_filters((t.timeline, t.track_index, t.clip_id), filters))
        })
        .collect()
}

#[derive(Clone, Copy)]
enum PresetIcon {
    Neutral,
    BlackAndWhite,
    AutoBalance,
}

/// A small button with a hand-drawn icon before its label.
fn icon_button(ui: &mut egui::Ui, icon: PresetIcon, label: &str) -> egui::Response {
    const ICON: f32 = 10.0;
    const PADDING: f32 = 5.0;
    let galley = ui.painter().layout_no_wrap(
        label.to_owned(),
        egui::TextStyle::Button.resolve(ui.style()),
        egui::Color32::PLACEHOLDER,
    );
    let size = egui::vec2(
        PADDING * 3.0 + ICON + galley.size().x,
        galley.size().y + 4.0,
    );
    let (rect, response) = ui.allocate_exact_size(size, egui::Sense::click());
    let visuals = ui.style().interact(&response);
    let painter = ui.painter();
    painter.rect(
        rect,
        3.0,
        visuals.weak_bg_fill,
        visuals.bg_stroke,
        egui::StrokeKind::Inside,
    );
    let color = visuals.text_color();
    let c = egui::pos2(rect.left() + PADDING + ICON / 2.0, rect.center().y);
    let stroke = egui::Stroke::new(1.2, color);
    let r = ICON / 2.0;
    match icon {
        // A wheel with its puck at the center.
        PresetIcon::Neutral => {
            painter.circle_stroke(c, r, stroke);
            painter.circle_filled(c, 1.6, color);
        }
        // Half black, half white.
        PresetIcon::BlackAndWhite => {
            painter.circle_filled(c, r, egui::Color32::from_gray(235));
            let left: Vec<egui::Pos2> = (0..=16)
                .map(|i| {
                    let a = std::f32::consts::FRAC_PI_2 + i as f32 / 16.0 * std::f32::consts::PI;
                    c + egui::vec2(a.cos(), -a.sin()) * r
                })
                .collect();
            painter.add(egui::Shape::convex_polygon(
                left,
                egui::Color32::from_gray(20),
                egui::Stroke::NONE,
            ));
            painter.circle_stroke(c, r, stroke);
        }
        // A wand with a spark at its tip.
        PresetIcon::AutoBalance => {
            let tip = c + egui::vec2(2.5, -2.5);
            painter.line_segment([c + egui::vec2(-r, r), tip], egui::Stroke::new(1.6, color));
            for (dx, dy) in [(0.0, -2.5), (0.0, 2.5), (-2.5, 0.0), (2.5, 0.0)] {
                painter.line_segment([tip, tip + egui::vec2(dx, dy)], stroke);
            }
        }
    }
    painter.galley(
        egui::pos2(
            rect.left() + PADDING * 2.0 + ICON,
            rect.center().y - galley.size().y / 2.0,
        ),
        galley,
        color,
    );
    response
}

/// `keys` holds one `RowKeyframe` per `GradeParam`, in the order of `ALL`.
/// `can_balance`: a single clip is selected and the playhead is on it.
pub(crate) fn grade_section(
    ui: &mut egui::Ui,
    grade: &GradeValue,
    keys: &[RowKeyframe],
    can_balance: bool,
) -> GradeSectionResponse {
    let mut response = GradeSectionResponse::default();
    let key = |param: GradeParam| keys[param.index()];

    // Wrapped: in a narrow inspector a single row would widen it.
    ui.horizontal_wrapped(|ui| {
        ui.label(t!("props.grade_preset"));
        for preset in GradePreset::ALL {
            let icon = match preset {
                GradePreset::Neutral => PresetIcon::Neutral,
                GradePreset::BlackAndWhite => PresetIcon::BlackAndWhite,
            };
            if icon_button(ui, icon, &preset_label(preset)).clicked() {
                response.preset = Some(preset);
            }
        }
        response.auto_balance = ui
            .add_enabled_ui(can_balance, |ui| {
                icon_button(ui, PresetIcon::AutoBalance, &t!("props.grade_auto_balance"))
            })
            .inner
            .on_hover_text(t!("props.grade_auto_balance_hint"))
            .on_disabled_hover_text(t!("props.grade_auto_balance_unavailable"))
            .clicked();
    });

    ui.add_space(4.0);
    let spacing = ui.spacing().item_spacing.x;
    let available = ui.available_width();
    let columns = wheel_columns(available, spacing);
    // Rounded down: a row a fraction wider than the panel would widen it.
    let block_width = ((available - spacing * (columns - 1) as f32) / columns as f32).floor() - 1.0;
    for row in GradeWheel::ALL.chunks(columns) {
        ui.horizontal_top(|ui| {
            for wheel_kind in row {
                ui.allocate_ui_with_layout(
                    egui::vec2(block_width, 0.0),
                    egui::Layout::top_down(egui::Align::Center),
                    |ui| wheel_block(ui, &mut response, grade, *wheel_kind, block_width, &key),
                );
            }
        });
        ui.add_space(4.0);
    }
    ui.add_space(4.0);
    for param in [GradeParam::LowRange, GradeParam::HighRange] {
        scalar_row(
            ui,
            &mut response,
            grade,
            param,
            key(param),
            &grade_param_label(param),
        );
    }
    response
}

/// Name and keyframe group, the wheel, then luminance and saturation.
fn wheel_block(
    ui: &mut egui::Ui,
    response: &mut GradeSectionResponse,
    grade: &GradeValue,
    wheel_kind: GradeWheel,
    width: f32,
    key: &dyn Fn(GradeParam) -> RowKeyframe,
) {
    let params = [
        wheel_kind.x(),
        wheel_kind.y(),
        wheel_kind.luma(),
        wheel_kind.saturation(),
    ];
    let joint = joint_keyframe(params.map(key));
    ui.horizontal(|ui| {
        ui.set_width(width);
        let prev = keyframe_arrow(ui, true, joint.prev, &t!("props.prev_keyframe"));
        if keyframe_button(ui, joint.on_keyframe).clicked() {
            for param in params {
                response.toggle(param, joint.on_keyframe);
            }
        }
        let next = keyframe_arrow(ui, false, joint.next, &t!("props.next_keyframe"));
        response.goto = response.goto.or(prev.or(next));
        let name = wheel_label(wheel_kind);
        let label = egui::Label::new(egui::RichText::new(name.as_ref()).strong()).truncate();
        if reset_label(ui, label, &name) {
            response.reset.extend(params);
        }
    });

    let (px, py) = (wheel_kind.x(), wheel_kind.y());
    let (mut x, mut y) = (grade.get(px), grade.get(py));
    let mut reset_wheel = false;
    if wheel(
        ui,
        ui.id().with(("grade_wheel", px)),
        (width - 20.0).min(MAX_WHEEL_SIZE),
        &mut x,
        &mut y,
        &mut reset_wheel,
    ) {
        response.set(px, x);
        response.set(py, y);
    }
    if reset_wheel {
        response.reset.extend([px, py]);
    }

    ui.horizontal(|ui| {
        // Narrow enough for both on one row of the narrowest block.
        ui.spacing_mut().interact_size.x = 36.0;
        for (param, label) in [
            (wheel_kind.luma(), t!("props.grade_luminance_short")),
            (wheel_kind.saturation(), t!("props.grade_saturation_short")),
        ] {
            if reset_label(ui, egui::Label::new(label), &grade_param_label(param)) {
                response.reset.push(param);
            }
            let mut value = grade.get(param) as f64;
            let range = param.range();
            if drag_field(
                ui,
                &mut value,
                0.005,
                (*range.start() as f64)..=(*range.end() as f64),
                2,
                "",
            ) {
                response.set(param, value as f32);
            }
        }
    });
}

/// A label that resets its control like `param_row`'s: double click, or the
/// context menu.
fn reset_label(ui: &mut egui::Ui, label: egui::Label, name: &str) -> bool {
    let response = ui
        .add(label.sense(egui::Sense::click()))
        .on_hover_text(format!("{name}\n{}", t!("props.reset_hint")));
    let mut reset = response.double_clicked();
    response.context_menu(|ui| {
        if ui.button(t!("props.reset_param")).clicked() {
            reset = true;
            ui.close();
        }
    });
    reset
}

fn scalar_row(
    ui: &mut egui::Ui,
    response: &mut GradeSectionResponse,
    grade: &GradeValue,
    param: GradeParam,
    key: RowKeyframe,
    label: &str,
) {
    let mut value = grade.get(param);
    let row = param_row(ui, label, Some(key), |ui| {
        slider_field(ui, &mut value, param.range(), 0.005, 3)
    });
    if row.changed {
        response.set(param, value);
    }
    if row.toggled_keyframe {
        response.toggle(param, key.on_keyframe);
    }
    if row.reset {
        response.reset.push(param);
    }
    response.goto = response.goto.or(row.goto);
}

#[cfg(test)]
#[path = "tests/grade_panel.rs"]
mod tests;
