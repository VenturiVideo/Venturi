//! Settings of the equalizer: on top its frequency response over the live
//! spectrum, with a point per band to drag; below one strip per band.

use vv_audio::eq::EqResponse;
use vv_audio::mixer::PROJECT_SAMPLE_RATE;
use vv_core::{AudioEffectKind, EqBand, EqShape, Equalizer};

use crate::compressor_panel::{
    CURVE_POINTS, EffectPanelState, KNOB_CELL_WIDTH, KnobScale, curve_freq, format_freq,
    fraction_freq, freq_fraction, paint_freq_grid, paint_spectrum, paint_spectrum_scale,
    param_knob, preset_bar,
};
use crate::settings::Preset;

const BANDS: usize = 6;
const STRIP_WIDTH: f32 = 96.0;
/// As wide as the band strips below it, frames and gaps included.
const GRAPH_SIZE: egui::Vec2 = egui::vec2(
    BANDS as f32 * (STRIP_WIDTH + 18.0) + (BANDS - 1) as f32 * 8.0,
    230.0,
);
/// A little past the gain range, so the points at its ends stay whole.
const DB_SPAN: f32 = 27.0;
const BAND_COLORS: [egui::Color32; BANDS] = [
    egui::Color32::from_rgb(200, 110, 240),
    egui::Color32::from_rgb(90, 160, 255),
    egui::Color32::from_rgb(80, 205, 200),
    egui::Color32::from_rgb(110, 205, 120),
    egui::Color32::from_rgb(235, 190, 70),
    egui::Color32::from_rgb(240, 120, 110),
];
/// Q multiplier per notch of the mouse wheel.
const Q_WHEEL_STEP: f32 = 1.15;

/// Built-in presets: translation key and settings.
pub(crate) fn builtin_presets() -> [(&'static str, Equalizer); 7] {
    let band = |shape, freq_hz, gain_db, q| EqBand {
        enabled: true,
        shape,
        freq_hz,
        gain_db,
        q,
    };
    let preset = |changes: &[(usize, EqBand)]| {
        let mut eq = Equalizer::DEFAULT;
        for &(index, band) in changes {
            eq.bands[index] = band;
        }
        eq
    };
    let q = EqBand::Q_DEFAULT;
    [
        ("mixer.eq_preset_flat", Equalizer::DEFAULT),
        (
            "mixer.eq_preset_voice",
            preset(&[
                (0, band(EqShape::HighPass, 80.0, 0.0, q)),
                (1, band(EqShape::LowShelf, 150.0, -2.0, q)),
                (2, band(EqShape::Peak, 300.0, -3.0, 1.0)),
                (3, band(EqShape::Peak, 3000.0, 3.0, 1.0)),
                (4, band(EqShape::HighShelf, 10_000.0, 2.0, q)),
            ]),
        ),
        (
            "mixer.eq_preset_warm",
            preset(&[
                (1, band(EqShape::LowShelf, 200.0, 3.0, q)),
                (3, band(EqShape::Peak, 3000.0, -1.5, 0.8)),
                (4, band(EqShape::HighShelf, 8000.0, -2.0, q)),
            ]),
        ),
        (
            "mixer.eq_preset_bright",
            preset(&[
                (0, band(EqShape::HighPass, 40.0, 0.0, q)),
                (3, band(EqShape::Peak, 4000.0, 1.5, 0.8)),
                (4, band(EqShape::HighShelf, 10_000.0, 4.0, q)),
            ]),
        ),
        (
            "mixer.eq_preset_mud",
            preset(&[
                (0, band(EqShape::HighPass, 60.0, 0.0, q)),
                (2, band(EqShape::Peak, 350.0, -5.0, 1.4)),
            ]),
        ),
        (
            "mixer.eq_preset_rumble",
            preset(&[(0, band(EqShape::HighPass, 100.0, 0.0, q))]),
        ),
        (
            "mixer.eq_preset_telephone",
            preset(&[
                (0, band(EqShape::HighPass, 400.0, 0.0, q)),
                (3, band(EqShape::Peak, 1500.0, 4.0, 1.0)),
                (5, band(EqShape::LowPass, 3400.0, 0.0, q)),
            ]),
        ),
    ]
}

pub(crate) fn shape_name(shape: EqShape) -> String {
    match shape {
        EqShape::HighPass => t!("mixer.eq_high_pass"),
        EqShape::LowShelf => t!("mixer.eq_low_shelf"),
        EqShape::Peak => t!("mixer.eq_peak"),
        EqShape::HighShelf => t!("mixer.eq_high_shelf"),
        EqShape::LowPass => t!("mixer.eq_low_pass"),
    }
    .into_owned()
}

/// Where the point of `band` sits: the passes have no gain, they stay on
/// the 0 dB line.
fn point_db(band: &EqBand) -> f32 {
    if band.shape.has_gain() {
        band.gain_db
    } else {
        0.0
    }
}

/// `q` scaled by `notches` of the wheel, kept in range.
pub(crate) fn wheel_q(q: f32, notches: f32) -> f32 {
    let (min, max) = EqBand::Q_RANGE;
    let q = (q * Q_WHEEL_STEP.powf(notches)).clamp(min, max);
    (q * 100.0).round() / 100.0
}

/// The edited parameters, if changed. `presets`: the user's own, saved and
/// deleted here.
pub(crate) fn show(
    ui: &mut egui::Ui,
    params: &Equalizer,
    state: &mut EffectPanelState,
    presets: &mut Vec<Preset<Equalizer>>,
) -> Option<AudioEffectKind> {
    let mut params = params.clone();
    let builtin = builtin_presets();
    let mut changed = preset_bar(ui, "eq_preset", &mut params, &builtin, state, presets);
    ui.add_space(6.0);
    changed |= graph(ui, &mut params, state);
    ui.weak(t!("mixer.eq_graph_hint"));
    ui.add_space(6.0);
    ui.horizontal_top(|ui| {
        for (index, band) in params.bands.iter_mut().enumerate() {
            changed |= band_strip(ui, index, band);
        }
    });
    changed.then_some(AudioEffectKind::Equalizer(params))
}

fn graph(ui: &mut egui::Ui, params: &mut Equalizer, state: &EffectPanelState) -> bool {
    let (rect, _) = ui.allocate_exact_size(GRAPH_SIZE, egui::Sense::hover());
    let x_of = |freq: f32| rect.left() + rect.width() * freq_fraction(freq);
    let y_of = |db: f32| {
        let t = (DB_SPAN - db.clamp(-DB_SPAN, DB_SPAN)) / (2.0 * DB_SPAN);
        rect.top() + rect.height() * t
    };
    let freq_at = |x: f32| fraction_freq(((x - rect.left()) / rect.width()).clamp(0.0, 1.0));
    let db_at = |y: f32| DB_SPAN - (y - rect.top()) / rect.height() * 2.0 * DB_SPAN;

    let mut changed = false;
    let pointer = ui.ctx().pointer_interact_pos();
    let mut active = None;
    for (index, band) in params.bands.iter_mut().enumerate() {
        let spot = egui::pos2(x_of(band.freq_hz), y_of(point_db(band)));
        let response = ui
            .interact(
                egui::Rect::from_center_size(spot, egui::Vec2::splat(18.0)),
                ui.id().with(("eq_point", index)),
                egui::Sense::click_and_drag(),
            )
            .on_hover_cursor(egui::CursorIcon::Grab);
        if response.double_clicked() {
            band.enabled = !band.enabled;
            changed = true;
        } else if response.dragged()
            && let Some(pointer) = pointer
        {
            let (min, max) = EqBand::FREQ_RANGE_HZ;
            band.freq_hz = freq_at(pointer.x).clamp(min, max).round();
            if band.shape.has_gain() {
                let (min, max) = EqBand::GAIN_RANGE_DB;
                band.gain_db = (db_at(pointer.y).clamp(min, max) * 10.0).round() / 10.0;
            }
            // Dragging a band that is off would change nothing heard.
            band.enabled = true;
            changed = true;
        }
        if response.hovered() {
            let notches: f32 = ui.input(|i| {
                i.raw
                    .events
                    .iter()
                    .map(|e| match e {
                        egui::Event::MouseWheel { delta, .. } => delta.y.signum(),
                        _ => 0.0,
                    })
                    .sum()
            });
            if notches != 0.0 {
                band.q = wheel_q(band.q, notches);
                band.enabled = true;
                changed = true;
            }
        }
        if response.hovered() || response.dragged() {
            active = Some(index);
        }
    }

    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 4.0, egui::Color32::from_gray(14));
    paint_spectrum(&painter, rect, &state.spectrum);
    let font = egui::FontId::proportional(9.0);
    for db in (-24..=24).step_by(6) {
        let y = y_of(db as f32);
        let gray = if db == 0 { 70 } else { 40 };
        painter.hline(
            rect.x_range(),
            y,
            egui::Stroke::new(1.0, egui::Color32::from_gray(gray)),
        );
        // Not +24: the reset button sits there.
        if db != 24 {
            let text = if db > 0 {
                format!("+{db}")
            } else {
                db.to_string()
            };
            painter.text(
                egui::pos2(rect.right() - 4.0, y - 1.0),
                egui::Align2::RIGHT_BOTTOM,
                text,
                font.clone(),
                egui::Color32::from_gray(110),
            );
        }
    }
    paint_freq_grid(&painter, rect);
    paint_spectrum_scale(&painter, rect);

    let response = EqResponse::new(params, PROJECT_SAMPLE_RATE);
    let freqs: Vec<f32> = (0..=CURVE_POINTS).map(curve_freq).collect();
    for (index, band) in params.bands.iter().enumerate() {
        if !band.enabled {
            continue;
        }
        let alpha = if active == Some(index) { 0.9 } else { 0.45 };
        painter.add(egui::Shape::line(
            freqs
                .iter()
                .map(|&f| egui::pos2(x_of(f), y_of(response.band_gain_db(index, f))))
                .collect(),
            egui::Stroke::new(1.0, BAND_COLORS[index].gamma_multiply(alpha)),
        ));
    }
    let total: Vec<egui::Pos2> = freqs
        .iter()
        .map(|&f| egui::pos2(x_of(f), y_of(response.gain_db(f))))
        .collect();
    let zero = y_of(0.0);
    let fill = crate::theme::ACCENT.gamma_multiply(0.18);
    for pair in total.windows(2) {
        let [a, b] = [pair[0], pair[1]];
        if (a.y - zero).abs() > 0.5 || (b.y - zero).abs() > 0.5 {
            // Split where the curve crosses 0 dB, or the quad would twist.
            let polygon = if (a.y - zero) * (b.y - zero) < 0.0 {
                let t = (zero - a.y) / (b.y - a.y);
                let cross = egui::pos2(a.x + (b.x - a.x) * t, zero);
                vec![
                    vec![egui::pos2(a.x, zero), a, cross],
                    vec![cross, b, egui::pos2(b.x, zero)],
                ]
            } else {
                vec![vec![egui::pos2(a.x, zero), a, b, egui::pos2(b.x, zero)]]
            };
            for points in polygon {
                painter.add(egui::Shape::convex_polygon(
                    points,
                    fill,
                    egui::Stroke::NONE,
                ));
            }
        }
    }
    painter.add(egui::Shape::line(
        total,
        egui::Stroke::new(2.0, crate::theme::ACCENT),
    ));

    for (index, band) in params.bands.iter().enumerate() {
        let spot = egui::pos2(x_of(band.freq_hz), y_of(point_db(band)));
        let lit = active == Some(index);
        let radius = if lit { 7.0 } else { 5.5 };
        let outline = egui::Stroke::new(1.5, egui::Color32::from_gray(if lit { 255 } else { 20 }));
        if band.enabled {
            painter.circle(spot, radius, BAND_COLORS[index], outline);
        } else {
            painter.circle(
                spot,
                radius,
                egui::Color32::from_gray(30),
                egui::Stroke::new(1.5, BAND_COLORS[index].gamma_multiply(0.6)),
            );
        }
        painter.text(
            spot,
            egui::Align2::CENTER_CENTER,
            (index + 1).to_string(),
            egui::FontId::proportional(8.0),
            egui::Color32::from_gray(if band.enabled { 10 } else { 140 }),
        );
        if lit {
            let mut text = format!("{} Hz", format_freq(band.freq_hz));
            if band.shape.has_gain() {
                text += &format!("  {:+.1} dB", band.gain_db);
            }
            text += &format!("  Q {:.2}", band.q);
            // Towards the middle, so it does not leave the graph.
            let (align, offset) = if spot.x > rect.center().x {
                (egui::Align2::RIGHT_BOTTOM, egui::vec2(-10.0, -10.0))
            } else {
                (egui::Align2::LEFT_BOTTOM, egui::vec2(10.0, -10.0))
            };
            let below = spot.y < rect.top() + 24.0;
            let offset = if below {
                egui::vec2(offset.x, 24.0)
            } else {
                offset
            };
            painter.text(
                spot + offset,
                align,
                text,
                egui::FontId::proportional(11.0),
                egui::Color32::WHITE,
            );
        }
    }

    let reset = egui::Rect::from_min_size(
        rect.right_top() + egui::vec2(-84.0, 6.0),
        egui::vec2(78.0, 18.0),
    );
    let reset_all = egui::Button::new(egui::RichText::new(t!("mixer.reset_all")).small());
    // A detached child: `put` would move the layout past the graph's top.
    let mut overlay = ui.new_child(egui::UiBuilder::new().max_rect(reset));
    if overlay
        .add_sized(reset.size(), reset_all)
        .on_hover_text(t!("mixer.eq_reset_all_hint"))
        .clicked()
    {
        *params = Equalizer::DEFAULT;
        changed = true;
    }
    painter.rect_stroke(
        rect,
        4.0,
        egui::Stroke::new(1.0, egui::Color32::from_gray(60)),
        egui::StrokeKind::Inside,
    );
    changed
}

fn band_strip(ui: &mut egui::Ui, index: usize, band: &mut EqBand) -> bool {
    let mut changed = false;
    egui::Frame::group(ui.style())
        .fill(ui.visuals().faint_bg_color)
        .inner_margin(egui::Margin::same(8))
        .show(ui, |ui| {
            ui.set_width(STRIP_WIDTH);
            ui.push_id(index, |ui| {
                ui.vertical_centered(|ui| {
                    ui.horizontal(|ui| {
                        let (dot, _) =
                            ui.allocate_exact_size(egui::Vec2::splat(10.0), egui::Sense::hover());
                        ui.painter()
                            .circle_filled(dot.center(), 4.5, BAND_COLORS[index]);
                        ui.strong((index + 1).to_string());
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            changed |= ui
                                .checkbox(&mut band.enabled, "")
                                .on_hover_text(t!("mixer.eq_band_on"))
                                .changed();
                        });
                    });
                    egui::ComboBox::from_id_salt("eq_shape")
                        .width(STRIP_WIDTH)
                        .selected_text(shape_name(band.shape))
                        .show_ui(ui, |ui| {
                            for shape in EqShape::ALL {
                                if ui
                                    .selectable_label(band.shape == shape, shape_name(shape))
                                    .clicked()
                                {
                                    band.shape = shape;
                                    changed = true;
                                }
                            }
                        });
                    ui.add_space(4.0);
                    ui.add_enabled_ui(band.enabled, |ui| {
                        let default = Equalizer::DEFAULT.bands[index];
                        changed |= param_knob(
                            ui,
                            &t!("mixer.eq_freq"),
                            &mut band.freq_hz,
                            KnobScale::new(EqBand::FREQ_RANGE_HZ, true, default.freq_hz),
                            (5.0, 0, " Hz"),
                        );
                        ui.add_enabled_ui(band.shape.has_gain(), |ui| {
                            changed |= param_knob(
                                ui,
                                &t!("mixer.eq_gain"),
                                &mut band.gain_db,
                                KnobScale::new(EqBand::GAIN_RANGE_DB, false, 0.0),
                                (0.1, 1, " dB"),
                            );
                        });
                        changed |= param_knob(
                            ui,
                            "Q",
                            &mut band.q,
                            KnobScale::new(EqBand::Q_RANGE, true, EqBand::Q_DEFAULT),
                            (0.01, 2, ""),
                        );
                    });
                    ui.add_space(4.0);
                    let reset = ui
                        .add_sized(
                            egui::vec2(KNOB_CELL_WIDTH, 0.0),
                            egui::Button::new(egui::RichText::new(t!("mixer.reset")).small()),
                        )
                        .on_hover_text(t!("mixer.reset_band_hint"));
                    if reset.clicked() {
                        *band = Equalizer::DEFAULT.bands[index];
                        changed = true;
                    }
                });
            });
        });
    changed
}

#[cfg(test)]
#[path = "tests/eq_panel.rs"]
mod tests;
