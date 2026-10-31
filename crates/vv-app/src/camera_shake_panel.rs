use vv_core::{CameraShakeTracks, FrameIdx, KeyframeTarget, KeyframeValue, ShakeParam};

use crate::properties_panel::{
    BoxedCommand, ClipRef, KeyframeEdit, RowKeyframe, param_row, remove_keyframe, slider_field,
    upsert_keyframe,
};

pub(crate) fn param_label(param: ShakeParam) -> std::borrow::Cow<'static, str> {
    match param {
        ShakeParam::MotionScale => t!("shake.motion_scale"),
        ShakeParam::Speed => t!("shake.speed"),
        ShakeParam::Pan => t!("shake.pan"),
        ShakeParam::Tilt => t!("shake.tilt"),
        ShakeParam::Rotation => t!("shake.rotation"),
        ShakeParam::Zoom => t!("shake.zoom"),
        ShakeParam::Smoothness => t!("shake.smoothness"),
    }
}

#[derive(Debug, Clone)]
pub(crate) struct ShakeInfo {
    values: [f32; 7],
    keys: [RowKeyframe; 7],
    seed: u32,
    auto_zoom: bool,
}

impl ShakeInfo {
    pub(crate) fn of(
        tracks: &CameraShakeTracks,
        frame: FrameIdx,
        reachable: impl Fn(&FrameIdx) -> bool,
    ) -> Self {
        Self {
            values: ShakeParam::ALL.map(|p| tracks.track(p).value_at(frame)),
            keys: ShakeParam::ALL.map(|p| RowKeyframe::of(tracks.track(p), frame, &reachable)),
            seed: tracks.seed,
            auto_zoom: tracks.auto_zoom,
        }
    }
}

#[derive(Default)]
pub(crate) struct ShakeResponse {
    edits: Vec<(ShakeParam, KeyframeEdit)>,
    resets: Vec<ShakeParam>,
    reset_all: bool,
    seed: Option<u32>,
    auto_zoom: Option<bool>,
    pub(crate) goto: Option<FrameIdx>,
}

impl ShakeResponse {
    pub(crate) fn changed(&self) -> bool {
        self.reset_all
            || !self.edits.is_empty()
            || !self.resets.is_empty()
            || self.seed.is_some()
            || self.auto_zoom.is_some()
    }

    pub(crate) fn commands(
        &self,
        clip: ClipRef,
        frame: FrameIdx,
        tracks: &mut CameraShakeTracks,
    ) -> Vec<BoxedCommand> {
        if self.reset_all {
            *tracks = CameraShakeTracks::default();
        }
        for &param in &self.resets {
            *tracks.track_mut(param) = vv_core::Keyframed::constant(param.default_value());
        }
        if let Some(seed) = self.seed {
            tracks.seed = seed;
        }
        if let Some(auto_zoom) = self.auto_zoom {
            tracks.auto_zoom = auto_zoom;
        }
        let mut commands = Vec::new();
        for (param, edit) in &self.edits {
            let track = tracks.track_mut(*param);
            match edit {
                KeyframeEdit::Set(KeyframeValue::CameraShake(_, value)) if track.is_constant() => {
                    track.default = *value;
                }
                KeyframeEdit::Set(value) => {
                    commands.push(upsert_keyframe(clip, frame, value.clone()))
                }
                KeyframeEdit::Toggle(true) => commands.push(remove_keyframe(
                    clip,
                    frame,
                    KeyframeTarget::CameraShake(*param),
                )),
                KeyframeEdit::Toggle(false) => commands.push(upsert_keyframe(
                    clip,
                    frame,
                    KeyframeValue::CameraShake(*param, track.value_at(frame)),
                )),
            }
        }
        commands
    }
}

pub(crate) fn section(ui: &mut egui::Ui, info: &ShakeInfo, reset: bool) -> ShakeResponse {
    let mut response = ShakeResponse {
        reset_all: reset,
        ..Default::default()
    };
    for param in ShakeParam::ALL {
        let i = param.index();
        let mut value = info.values[i];
        let row = param_row(ui, &param_label(param), Some(info.keys[i]), |ui| {
            slider_field(ui, &mut value, param.range(), 0.01, 2)
        });
        if row.changed {
            response.edits.push((
                param,
                KeyframeEdit::Set(KeyframeValue::CameraShake(param, value)),
            ));
        }
        if row.toggled_keyframe {
            response
                .edits
                .push((param, KeyframeEdit::Toggle(info.keys[i].on_keyframe)));
        }
        if row.reset {
            response.resets.push(param);
        }
        response.goto = response.goto.or(row.goto);
    }
    let mut auto_zoom = info.auto_zoom;
    if ui
        .checkbox(&mut auto_zoom, t!("shake.auto_zoom"))
        .on_hover_text(t!("shake.auto_zoom_hint"))
        .changed()
    {
        response.auto_zoom = Some(auto_zoom);
    }
    ui.horizontal(|ui| {
        ui.label(t!("shake.seed"));
        let mut seed = info.seed;
        if ui.add(egui::DragValue::new(&mut seed)).changed() {
            response.seed = Some(seed);
        }
        if ui.button(t!("shake.new_seed")).clicked() {
            response.seed = Some(info.seed.wrapping_add(1));
        }
    });
    ui.label(egui::RichText::new(t!("shake.hint")).weak().small());
    response
}

#[cfg(test)]
#[path = "tests/camera_shake_panel.rs"]
mod tests;
