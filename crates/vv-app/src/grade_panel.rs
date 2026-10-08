//! Color correction section of the properties panel: the four wheels, the
//! saturations and the ranges of `vv_core::GradeParam`.

use vv_core::{
    FrameIdx, GradeParam, GradePreset, GradeValue, GradeWheel, KeyframeTarget, KeyframeValue,
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
/// relative to where it is, finer with Shift. `true` if it moved; a double
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

    let mut changed = false;
    if response.double_clicked() {
        *reset = true;
    } else if response.dragged() {
        let fine = if ui.input(|i| i.modifiers.shift) {
            0.2
        } else {
            1.0
        };
        let delta = response.drag_delta() / radius * fine;
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

/// `keys` holds one `RowKeyframe` per `GradeParam`, in the order of `ALL`.
pub(crate) fn grade_section(
    ui: &mut egui::Ui,
    grade: &GradeValue,
    keys: &[RowKeyframe],
) -> GradeSectionResponse {
    let mut response = GradeSectionResponse::default();
    let key = |param: GradeParam| keys[param.index()];

    ui.horizontal(|ui| {
        ui.label(t!("props.grade_preset"));
        for preset in GradePreset::ALL {
            if ui.small_button(preset_label(preset)).clicked() {
                response.preset = Some(preset);
            }
        }
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
        let label = ui
            .add(
                egui::Label::new(egui::RichText::new(wheel_label(wheel_kind)).strong())
                    .truncate()
                    .sense(egui::Sense::click()),
            )
            .on_hover_text(format!(
                "{}\n{}",
                wheel_label(wheel_kind),
                t!("props.reset_hint")
            ));
        if label.double_clicked() {
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

    for (param, label) in [
        (wheel_kind.luma(), t!("props.grade_luminance_short")),
        (wheel_kind.saturation(), t!("props.grade_saturation_short")),
    ] {
        ui.horizontal(|ui| {
            ui.label(label).on_hover_text(grade_param_label(param));
            let mut value = grade.get(param) as f64;
            let range = param.range();
            if drag_field(
                ui,
                &mut value,
                0.005,
                (*range.start() as f64)..=(*range.end() as f64),
                3,
                "",
            ) {
                response.set(param, value as f32);
            }
        });
    }
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
