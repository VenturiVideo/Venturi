//! Masks section of the properties panel. Edits only the primary clip: a
//! mask is geometry drawn on one picture, copying it onto the other
//! selected clips would rarely be what is wanted.

use std::borrow::Cow;

use vv_core::{ClipMask, FrameIdx, Interpolation, Keyframed, MaskMode, MaskParam, MaskShape};

use crate::properties_panel::{
    RowKeyframe, drag_field, param_row, preview_combo, section_header, slider_field, toggle_switch,
};

pub(crate) fn mask_param_label(param: MaskParam) -> Cow<'static, str> {
    match param {
        MaskParam::CenterX => t!("mask.center_x"),
        MaskParam::CenterY => t!("mask.center_y"),
        MaskParam::Width => t!("mask.width"),
        MaskParam::Height => t!("mask.height"),
        MaskParam::Rotation => t!("props.rotation"),
        MaskParam::Roundness => t!("mask.roundness"),
        MaskParam::Feather => t!("mask.feather"),
        MaskParam::Expansion => t!("mask.expansion"),
        MaskParam::Opacity => t!("props.opacity"),
    }
}

pub(crate) fn mask_shape_label(shape: MaskShape) -> Cow<'static, str> {
    match shape {
        MaskShape::Rectangle => t!("mask.rectangle"),
        MaskShape::Ellipse => t!("mask.ellipse"),
        MaskShape::Path => t!("mask.path"),
    }
}

fn mask_mode_label(mode: MaskMode) -> Cow<'static, str> {
    match mode {
        MaskMode::Add => t!("mask.mode_add"),
        MaskMode::Subtract => t!("mask.mode_subtract"),
        MaskMode::Intersect => t!("mask.mode_intersect"),
    }
}

/// Size in timeline pixels of a `source_size` clip fitted into the
/// timeline at zoom 1: the extent of the masks' layer space.
pub(crate) fn layer_size(source_size: (u32, u32), timeline_size: (u32, u32)) -> (u32, u32) {
    let (sw, sh) = (source_size.0.max(1) as f32, source_size.1.max(1) as f32);
    let (tw, th) = (timeline_size.0 as f32, timeline_size.1 as f32);
    let scale = (tw / sw).min(th / sh);
    ((sw * scale).round() as u32, (sh * scale).round() as u32)
}

/// Sets `track` to `value` at `frame`: its default while it is not animated.
pub(crate) fn set_track_value<T: Clone>(track: &mut Keyframed<T>, frame: FrameIdx, value: T) {
    if track.is_constant() {
        track.default = value;
    } else {
        track.upsert(frame, value, Interpolation::Linear);
    }
}

fn toggle_keyframe<T: Clone + vv_core::Lerp>(track: &mut Keyframed<T>, frame: FrameIdx) {
    if track.remove_at(frame).is_none() {
        let value = track.value_at(frame);
        track.upsert(frame, value, Interpolation::Linear);
    }
}

#[derive(Default)]
pub(crate) struct MaskSectionResponse {
    /// The new list, if anything changed.
    pub(crate) masks: Option<Vec<ClipMask>>,
    /// Source frame to move the playhead to (keyframe arrow clicked).
    pub(crate) goto: Option<FrameIdx>,
    /// The mask the viewer handles should edit, if the user picked one.
    pub(crate) focus: Option<Option<usize>>,
    /// The focused mask is a new path to draw with the pen.
    pub(crate) draw: bool,
}

pub(crate) fn masks_section(
    ui: &mut egui::Ui,
    masks: &[ClipMask],
    frame: FrameIdx,
    in_clip: impl Fn(&FrameIdx) -> bool,
    layer_size: (u32, u32),
    focused: Option<usize>,
) -> MaskSectionResponse {
    let mut response = MaskSectionResponse::default();
    let mut new = masks.to_vec();
    ui.add_space(6.0);
    let (open, reset_all) = section_header(ui, &t!("props.masks"), !masks.is_empty());
    if reset_all {
        new.clear();
        response.focus = Some(None);
    }
    if open && !reset_all {
        ui.horizontal(|ui| {
            ui.label(t!("mask.add"));
            for shape in MaskShape::ALL {
                if ui.small_button(mask_shape_label(shape)).clicked() {
                    let mut mask = ClipMask::new(shape, layer_size);
                    if shape == MaskShape::Path {
                        mask.path.default.points.clear();
                        response.draw = true;
                    }
                    new.push(mask);
                    response.focus = Some(Some(new.len() - 1));
                }
            }
        });
        let mut removed = None;
        let mut swap = None;
        for (index, mask) in masks.iter().enumerate() {
            let own = &mut new[index];
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                let mut enabled = own.enabled;
                if toggle_switch(ui, &mut enabled).changed() {
                    own.enabled = enabled;
                }
                let title = format!(
                    "{} {} · {}",
                    t!("props.mask"),
                    index + 1,
                    mask_shape_label(mask.shape)
                );
                let text = egui::RichText::new(title).strong();
                let text = if focused == Some(index) {
                    text.color(crate::theme::ACCENT)
                } else {
                    text
                };
                let label = ui
                    .add(egui::Label::new(text).sense(egui::Sense::click()))
                    .on_hover_text(t!("mask.focus_hint"));
                if label.clicked() {
                    response.focus = Some((focused != Some(index)).then_some(index));
                }
                label.context_menu(|ui| {
                    if index > 0 && ui.button(t!("mask.move_up")).clicked() {
                        swap = Some(index - 1);
                        ui.close();
                    }
                    if index + 1 < masks.len() && ui.button(t!("mask.move_down")).clicked() {
                        swap = Some(index);
                        ui.close();
                    }
                    if ui.button(t!("props.reset_section")).clicked() {
                        let enabled = own.enabled;
                        *own = ClipMask::new(mask.shape, layer_size);
                        own.enabled = enabled;
                        ui.close();
                    }
                });
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui
                        .small_button("×")
                        .on_hover_text(t!("mask.delete"))
                        .clicked()
                    {
                        removed = Some(index);
                    }
                    ui.checkbox(&mut own.invert, t!("mask.invert"));
                });
            });

            let row = param_row(ui, &t!("mask.mode"), None, |ui| {
                let items: Vec<_> = MaskMode::ALL
                    .iter()
                    .map(|m| (*m, mask_mode_label(*m).to_string(), true))
                    .collect();
                preview_combo(
                    ui,
                    &format!("mask_mode_{index}"),
                    &mut own.mode,
                    &items,
                    Some(ui.available_width()),
                    None,
                    None,
                )
            });
            if row.reset {
                own.mode = MaskMode::Add;
            }

            if mask.shape == MaskShape::Path {
                let row = param_row(
                    ui,
                    &t!("mask.shape"),
                    Some(RowKeyframe::of(&mask.path, frame, &in_clip)),
                    |ui| {
                        let count = mask.path.value_at(frame).points.len();
                        ui.label(t!("mask.points", count = count));
                        false
                    },
                );
                if row.toggled_keyframe {
                    toggle_keyframe(&mut own.path, frame);
                }
                if row.reset {
                    own.path = ClipMask::new(MaskShape::Path, layer_size).path;
                }
                response.goto = response.goto.or(row.goto);
            }

            for param in MaskParam::ALL
                .into_iter()
                .filter(|p| p.applies_to(mask.shape))
            {
                let track = mask.track(param);
                let mut value = track.value_at(frame);
                let row = param_row(
                    ui,
                    &mask_param_label(param),
                    Some(RowKeyframe::of(track, frame, &in_clip)),
                    |ui| param_field(ui, param, &mut value),
                );
                let own_track = own.track_mut(param);
                if row.changed {
                    set_track_value(own_track, frame, value);
                }
                if row.toggled_keyframe {
                    toggle_keyframe(own_track, frame);
                }
                if row.reset {
                    *own_track = ClipMask::new(mask.shape, layer_size).track(param).clone();
                }
                response.goto = response.goto.or(row.goto);
            }
        }
        if let Some(index) = swap {
            new.swap(index, index + 1);
            if let Some(f) = focused.filter(|f| *f == index || *f == index + 1) {
                response.focus = Some(Some(if f == index { index + 1 } else { index }));
            }
        }
        if let Some(index) = removed {
            new.remove(index);
            response.focus = Some(None);
        }
    }
    if new != masks {
        response.masks = Some(new);
    }
    response
}

fn param_field(ui: &mut egui::Ui, param: MaskParam, value: &mut f32) -> bool {
    let (range, speed, decimals) = match param {
        MaskParam::CenterX | MaskParam::CenterY | MaskParam::Expansion => {
            return pixel_field(ui, value, -100_000.0..=100_000.0);
        }
        MaskParam::Width | MaskParam::Height => return pixel_field(ui, value, 0.0..=100_000.0),
        MaskParam::Rotation => (-360.0..=360.0, 0.5, 1),
        MaskParam::Roundness | MaskParam::Feather => (0.0..=vv_core::MASK_SOFTNESS_MAX, 0.5, 1),
        MaskParam::Opacity => (0.0..=100.0, 0.5, 1),
    };
    slider_field(ui, value, range, speed, decimals)
}

fn pixel_field(ui: &mut egui::Ui, value: &mut f32, range: std::ops::RangeInclusive<f64>) -> bool {
    let mut v = *value as f64;
    let changed = drag_field(ui, &mut v, 1.0, range, 1, " px");
    if changed {
        *value = v as f32;
    }
    changed
}
