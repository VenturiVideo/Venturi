//! Color correction section of the properties panel: the four wheels, the
//! saturations and the ranges of `vv_core::GradeParam`.

use vv_core::{
    FrameIdx, GradeParam, GradePreset, GradeValue, GradeWheel, KeyframeTarget, KeyframeValue,
};

use crate::properties_panel::{KeyframeEdit, RowKeyframe, param_row, slider_field};

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
fn wheel(ui: &mut egui::Ui, id: egui::Id, x: &mut f32, y: &mut f32, reset: &mut bool) -> bool {
    const SIZE: f32 = 104.0;
    let (rect, _) = ui.allocate_exact_size(egui::vec2(SIZE, SIZE), egui::Sense::hover());
    let response = ui
        .interact(rect, id, egui::Sense::click_and_drag())
        .on_hover_text(t!("props.grade_wheel_hint"));
    let center = rect.center();
    let radius = SIZE / 2.0 - 6.0;
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

/// The X and Y rows of a wheel act as one: the diamond and the arrows follow
/// whichever of the two has keyframes.
fn joint_keyframe(a: RowKeyframe, b: RowKeyframe) -> RowKeyframe {
    let pick =
        |p: Option<FrameIdx>, q: Option<FrameIdx>, nearest: fn(FrameIdx, FrameIdx) -> FrameIdx| {
            match (p, q) {
                (Some(p), Some(q)) => Some(nearest(p, q)),
                (p, q) => p.or(q),
            }
        };
    RowKeyframe {
        on_keyframe: a.on_keyframe || b.on_keyframe,
        prev: pick(a.prev, b.prev, FrameIdx::max),
        next: pick(a.next, b.next, FrameIdx::min),
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

    for wheel_kind in GradeWheel::ALL {
        ui.add_space(4.0);
        let (px, py) = (wheel_kind.x(), wheel_kind.y());
        let joint = joint_keyframe(key(px), key(py));
        let (mut x, mut y) = (grade.get(px), grade.get(py));
        let mut reset_wheel = false;
        let row = param_row(ui, &wheel_label(wheel_kind), Some(joint), |ui| {
            wheel(
                ui,
                ui.id().with(("grade_wheel", px)),
                &mut x,
                &mut y,
                &mut reset_wheel,
            )
        });
        if row.changed {
            response.set(px, x);
            response.set(py, y);
        }
        if row.toggled_keyframe {
            response.toggle(px, joint.on_keyframe);
            response.toggle(py, joint.on_keyframe);
        }
        if row.reset || reset_wheel {
            response.reset.extend([px, py]);
        }
        response.goto = response.goto.or(row.goto);

        let luma = wheel_kind.luma();
        scalar_row(
            ui,
            &mut response,
            grade,
            luma,
            key(luma),
            &t!("props.grade_luminance"),
        );
        let saturation = wheel_kind.saturation();
        scalar_row(
            ui,
            &mut response,
            grade,
            saturation,
            key(saturation),
            &t!("props.saturation"),
        );
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
