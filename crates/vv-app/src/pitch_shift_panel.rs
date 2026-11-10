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

#[derive(Debug, Clone, Copy)]
pub(crate) enum PitchShiftEdit {
    Enabled(bool),
    Semitones(f32),
    Cents(f32),
    PreserveFormants(bool),
    Replace(PitchShift),
}

impl PitchShiftEdit {
    pub(crate) fn apply_to(self, mut current: PitchShift) -> PitchShift {
        match self {
            Self::Enabled(value) => current.enabled = value,
            Self::Semitones(value) => current.semitones = value,
            Self::Cents(value) => current.cents = value,
            Self::PreserveFormants(value) => current.preserve_formants = value,
            Self::Replace(value) => current = value,
        }
        current.sanitized()
    }
}

pub(crate) fn show(ui: &mut egui::Ui, current: PitchShift) -> Vec<PitchShiftEdit> {
    let mut params = current;
    let mut edits = Vec::new();
    egui::CollapsingHeader::new(t!("pitch_shift.title"))
        .default_open(true)
        .show(ui, |ui| {
            if ui
                .checkbox(&mut params.enabled, t!("pitch_shift.enabled"))
                .changed()
            {
                edits.push(PitchShiftEdit::Enabled(params.enabled));
            }
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
                                edits.push(PitchShiftEdit::Replace(preset));
                            }
                        }
                    });
                if ui
                    .add(
                        egui::Slider::new(
                            &mut params.semitones,
                            PitchShift::SEMITONE_RANGE.0..=PitchShift::SEMITONE_RANGE.1,
                        )
                        .step_by(1.0)
                        .text(t!("pitch_shift.semitones")),
                    )
                    .changed()
                {
                    edits.push(PitchShiftEdit::Semitones(params.semitones));
                }
                if ui
                    .add(
                        egui::Slider::new(
                            &mut params.cents,
                            PitchShift::CENT_RANGE.0..=PitchShift::CENT_RANGE.1,
                        )
                        .step_by(1.0)
                        .text(t!("pitch_shift.cents")),
                    )
                    .changed()
                {
                    edits.push(PitchShiftEdit::Cents(params.cents));
                }
                if ui
                    .checkbox(&mut params.preserve_formants, t!("pitch_shift.formants"))
                    .on_hover_text(t!("pitch_shift.formants_hint"))
                    .changed()
                {
                    edits.push(PitchShiftEdit::PreserveFormants(params.preserve_formants));
                }
            });
            if ui.button(t!("pitch_shift.reset")).clicked() {
                edits.push(PitchShiftEdit::Replace(PitchShift::default()));
            }
            ui.weak(t!("pitch_shift.hint"));
        });
    edits
}

#[cfg(test)]
#[path = "tests/pitch_shift_panel.rs"]
mod tests;
