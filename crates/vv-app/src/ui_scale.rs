//! Application zoom; eframe persists it with egui's memory.

const SCALE_RANGE: std::ops::RangeInclusive<f32> = 0.2..=5.0;
const PRESETS: [u32; 5] = [75, 100, 125, 150, 200];

pub(crate) fn configure(ctx: &egui::Context) {
    // Ctrl/Cmd +/- belongs to the timeline; reset stays in the app keymap.
    ctx.options_mut(|options| {
        options.zoom_with_keyboard = false;
        if !options.zoom_factor.is_finite() || options.zoom_factor <= 0.0 {
            // The first input frame cannot be rescaled from an invalid factor.
            options.zoom_factor = 1.0;
        }
    });
    ctx.set_zoom_factor(
        ctx.zoom_factor()
            .clamp(*SCALE_RANGE.start(), *SCALE_RANGE.end()),
    );
}

pub(crate) fn show(ui: &mut egui::Ui) {
    let mut percent = ui.ctx().zoom_factor() * 100.0;
    let mut changed = false;
    ui.horizontal_wrapped(|ui| {
        ui.label(t!("settings.ui_scale"));
        changed |= ui
            .add(
                egui::DragValue::new(&mut percent)
                    .range(*SCALE_RANGE.start() * 100.0..=*SCALE_RANGE.end() * 100.0)
                    .speed(1.0)
                    .suffix("%"),
            )
            .changed();
    });
    ui.horizontal_wrapped(|ui| {
        for preset in PRESETS {
            if ui
                .selectable_label((percent - preset as f32).abs() < 0.01, format!("{preset}%"))
                .clicked()
            {
                percent = preset as f32;
                changed = true;
            }
        }
    });
    if ui.button(t!("settings.ui_scale_reset")).clicked() {
        percent = 100.0;
        changed = true;
    }
    ui.label(t!("settings.ui_scale_hint"));
    if changed {
        ui.ctx().set_zoom_factor(percent / 100.0);
    }
}

#[cfg(test)]
#[path = "tests/ui_scale.rs"]
mod tests;
