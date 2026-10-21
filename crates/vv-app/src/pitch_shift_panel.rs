use vv_core::PitchShift;

pub(crate) fn builtin_presets() -> [(&'static str, PitchShift); 5] {
    [
        ("pitch_shift.default", 0.0),
        ("pitch_shift.octave_down", -12.0),
        ("pitch_shift.fifth_down", -7.0),
        ("pitch_shift.fifth_up", 7.0),
        ("pitch_shift.octave_up", 12.0),
    ]
    .map(|(name, semitones)| {
        (
            name,
            PitchShift {
                semitones,
                ..PitchShift::default()
            },
        )
    })
}

pub(crate) fn show(ui: &mut egui::Ui, current: PitchShift) -> Option<PitchShift> {
    let mut params = current;
    let mut changed = false;
    egui::CollapsingHeader::new(t!("pitch_shift.title"))
        .default_open(true)
        .show(ui, |ui| {
            changed |= ui
                .checkbox(&mut params.enabled, t!("pitch_shift.enabled"))
                .changed();
            ui.add_enabled_ui(params.enabled, |ui| {
                let presets = builtin_presets();
                let selected = presets
                    .iter()
                    .find(|(_, preset)| *preset == params)
                    .map_or_else(|| t!("pitch_shift.custom"), |(name, _)| t!(*name));
                egui::ComboBox::from_id_salt("pitch_shift_preset")
                    .selected_text(selected)
                    .show_ui(ui, |ui| {
                        for (name, preset) in presets {
                            if ui.selectable_label(params == preset, t!(name)).clicked() {
                                params = preset;
                                changed = true;
                            }
                        }
                    });
                changed |= ui
                    .add(
                        egui::Slider::new(
                            &mut params.semitones,
                            PitchShift::SEMITONE_RANGE.0..=PitchShift::SEMITONE_RANGE.1,
                        )
                        .step_by(1.0)
                        .text(t!("pitch_shift.semitones")),
                    )
                    .changed();
                changed |= ui
                    .add(
                        egui::Slider::new(
                            &mut params.cents,
                            PitchShift::CENT_RANGE.0..=PitchShift::CENT_RANGE.1,
                        )
                        .step_by(1.0)
                        .text(t!("pitch_shift.cents")),
                    )
                    .changed();
                changed |= ui
                    .checkbox(&mut params.preserve_formants, t!("pitch_shift.formants"))
                    .on_hover_text(t!("pitch_shift.formants_hint"))
                    .changed();
            });
            if ui.button(t!("pitch_shift.reset")).clicked() {
                params = PitchShift::default();
                changed = true;
            }
            ui.weak(t!("pitch_shift.hint"));
        });
    changed.then_some(params.sanitized())
}

#[cfg(test)]
#[path = "tests/pitch_shift_panel.rs"]
mod tests;
