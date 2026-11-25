//! Settings of the multiband compressor: on top its frequency response,
//! live, with the crossovers and the band gains to drag; below one strip
//! per band.

use vv_audio::dynamics::{BandActivity, CrossoverResponse};
use vv_audio::mixer::PROJECT_SAMPLE_RATE;
use vv_audio::spectrum::{SpectrumTap, magnitudes_db};
use vv_core::{AudioEffectKind, CompressorBand, MultibandCompressor};

use crate::mixer_panel::{knob, paint_fader_handle};
use crate::properties_panel::drag_field;
use crate::settings::Preset;

/// As wide as the three band strips below it, frames and gaps included.
const GRAPH_SIZE: egui::Vec2 = egui::vec2(3.0 * (STRIP_WIDTH + 18.0) + 16.0, 230.0);
const FREQ_MIN: f32 = 20.0;
const FREQ_MAX: f32 = 20_000.0;
const DB_TOP: f32 = 24.0;
const DB_BOTTOM: f32 = -30.0;
pub(crate) const CURVE_POINTS: usize = 240;
const BAND_COLORS: [egui::Color32; 3] = [
    egui::Color32::from_rgb(90, 160, 255),
    egui::Color32::from_rgb(110, 205, 120),
    egui::Color32::from_rgb(235, 190, 70),
];
const FADER_COLUMN_WIDTH: f32 = 76.0;
const STRIP_WIDTH: f32 = FADER_COLUMN_WIDTH + 2.0 * KNOB_CELL_WIDTH + 16.0;
const FADER_HEIGHT: f32 = 150.0;
const KNOB_RADIUS: f32 = 15.0;
pub(crate) const KNOB_CELL_WIDTH: f32 = 60.0;
/// The crossovers never get closer than this ratio.
const MIN_CROSSOVER_RATIO: f32 = 1.2;

/// Scale of the spectrum, in dBFS per bin.
const SPECTRUM_TOP_DB: f32 = 0.0;
const SPECTRUM_FLOOR_DB: f32 = -96.0;
/// How fast a peak of the spectrum falls back, per repaint.
const SPECTRUM_FALL_DB: f32 = 1.5;
const GRID_COLOR: egui::Color32 = egui::Color32::from_gray(40);
const LABEL_COLOR: egui::Color32 = egui::Color32::from_gray(110);

/// Frequency of point `i` of the curves.
pub(crate) fn curve_freq(i: usize) -> f32 {
    fraction_freq(i as f32 / CURVE_POINTS as f32)
}

/// The spectrum `bins_db` (from 0 Hz to Nyquist) at each point of the
/// curves: the loudest bin around the point, or between the two nearest
/// bins where the points are closer than the bins (the low end).
pub(crate) fn spectrum_at_points(bins_db: &[f32], sample_rate: u32) -> Vec<f32> {
    let bin_hz = sample_rate as f32 / (2.0 * (bins_db.len() - 1) as f32);
    let bin_at = |freq: f32| freq / bin_hz;
    (0..=CURVE_POINTS)
        .map(|i| {
            let half_step = 0.5 / CURVE_POINTS as f32;
            let t = i as f32 / CURVE_POINTS as f32;
            let from = bin_at(fraction_freq(t - half_step)).ceil() as usize;
            let to = bin_at(fraction_freq(t + half_step)).floor() as usize;
            let last = bins_db.len() - 1;
            if from <= to && from <= last {
                bins_db[from..=to.min(last)]
                    .iter()
                    .copied()
                    .fold(f32::MIN, f32::max)
            } else {
                let at = bin_at(curve_freq(i)).min(last as f32);
                let below = at.floor() as usize;
                let above = (below + 1).min(last);
                let w = at - below as f32;
                bins_db[below] * (1.0 - w) + bins_db[above] * w
            }
        })
        .collect()
}

/// Each point averaged with its neighbours, about a fifth of an octave: the
/// bins of real audio are too jagged to read. In power, not dB, or a lone
/// tone would sink.
pub(crate) fn smooth(points: &[f32]) -> Vec<f32> {
    const REACH: usize = 2;
    (0..points.len())
        .map(|i| {
            let around = &points[i.saturating_sub(REACH)..(i + REACH + 1).min(points.len())];
            let power = around.iter().map(|db| 10f32.powf(db / 10.0)).sum::<f32>();
            10.0 * (power / around.len() as f32).log10()
        })
        .collect()
}

/// The spectrum drawn under the curves, before and after the compressor,
/// with its peaks falling back slowly.
#[derive(Default)]
pub(crate) struct SpectrumView {
    pre: Vec<f32>,
    post: Vec<f32>,
    written: usize,
}

impl SpectrumView {
    /// Reads `tap`; while the audio is stopped the spectrum falls away.
    /// `true` while something is still shown.
    pub(crate) fn update(&mut self, tap: Option<&SpectrumTap>) -> bool {
        let floor = vec![SPECTRUM_FLOOR_DB; CURVE_POINTS + 1];
        let fresh = tap.filter(|tap| tap.written() != self.written);
        let (pre, post) = match fresh {
            Some(tap) => {
                self.written = tap.written();
                let (pre, post) = tap.read();
                let points = |samples: &[f32]| {
                    smooth(&spectrum_at_points(
                        &magnitudes_db(samples),
                        PROJECT_SAMPLE_RATE,
                    ))
                };
                (points(&pre), points(&post))
            }
            None => (floor.clone(), floor.clone()),
        };
        for (shown, new) in [(&mut self.pre, pre), (&mut self.post, post)] {
            if shown.len() != new.len() {
                *shown = floor.clone();
            }
            for (value, new) in shown.iter_mut().zip(new) {
                *value = new.max(*value - SPECTRUM_FALL_DB).max(SPECTRUM_FLOOR_DB);
            }
        }
        self.pre
            .iter()
            .chain(&self.post)
            .any(|db| *db > SPECTRUM_FLOOR_DB)
    }
}

/// Position of `freq` on the logarithmic axis, 0 at 20 Hz, 1 at 20 kHz.
pub(crate) fn freq_fraction(freq: f32) -> f32 {
    (freq / FREQ_MIN).ln() / (FREQ_MAX / FREQ_MIN).ln()
}

pub(crate) fn fraction_freq(t: f32) -> f32 {
    FREQ_MIN * (FREQ_MAX / FREQ_MIN).powf(t)
}

/// Knob position of `value` in `(min, max)`, logarithmic when `log`.
pub(crate) fn knob_fraction(value: f32, (min, max): (f32, f32), log: bool) -> f32 {
    let t = if log {
        (value / min).ln() / (max / min).ln()
    } else {
        (value - min) / (max - min)
    };
    t.clamp(0.0, 1.0)
}

pub(crate) fn knob_value(t: f32, (min, max): (f32, f32), log: bool) -> f32 {
    if log {
        min * (max / min).powf(t)
    } else {
        min + (max - min) * t
    }
}

fn band_edges(params: &MultibandCompressor) -> [f32; 4] {
    let [low, high] = params.crossovers_hz;
    [FREQ_MIN, low, high, FREQ_MAX]
}

pub(crate) fn format_freq(freq: f32) -> String {
    if freq >= 1000.0 {
        let k = freq / 1000.0;
        if k.fract() < 0.05 {
            format!("{k:.0}k")
        } else {
            format!("{k:.1}k")
        }
    } else {
        format!("{freq:.0}")
    }
}

/// What the window keeps between repaints.
#[derive(Default)]
pub(crate) struct EffectPanelState {
    pub(crate) spectrum: SpectrumView,
    /// Being typed in the save dialog.
    pub(crate) preset_name: String,
}

/// Built-in presets: translation key and settings.
pub(crate) fn builtin_presets() -> [(&'static str, MultibandCompressor); 7] {
    let band = |threshold_db, ratio, attack_ms, release_ms, makeup_db| CompressorBand {
        threshold_db,
        ratio,
        attack_ms,
        release_ms,
        makeup_db,
    };
    let bypass = band(0.0, 1.0, 10.0, 150.0, 0.0);
    let preset = |crossovers_hz, bands| MultibandCompressor {
        crossovers_hz,
        bands,
    };
    [
        ("mixer.preset_default", MultibandCompressor::DEFAULT),
        (
            "mixer.preset_glue",
            preset([150.0, 3000.0], [band(-26.0, 2.0, 30.0, 250.0, 2.0); 3]),
        ),
        (
            "mixer.preset_voice",
            preset(
                [200.0, 5000.0],
                [
                    band(-26.0, 3.0, 15.0, 150.0, -2.0),
                    band(-28.0, 3.0, 8.0, 120.0, 4.0),
                    band(-26.0, 2.0, 3.0, 80.0, 2.0),
                ],
            ),
        ),
        (
            "mixer.preset_deesser",
            preset(
                [1000.0, 6000.0],
                [bypass, bypass, band(-18.0, 6.0, 0.5, 40.0, 0.0)],
            ),
        ),
        (
            "mixer.preset_low_end",
            preset(
                [120.0, 2000.0],
                [band(-26.0, 4.0, 20.0, 200.0, 0.0), bypass, bypass],
            ),
        ),
        (
            "mixer.preset_broadcast",
            preset(
                [200.0, 3000.0],
                [
                    band(-28.0, 4.0, 5.0, 120.0, 10.5),
                    band(-28.0, 4.0, 5.0, 120.0, 8.5),
                    band(-28.0, 4.0, 5.0, 120.0, 7.5),
                ],
            ),
        ),
        (
            "mixer.preset_master",
            preset(
                [120.0, 5000.0],
                [
                    band(-26.0, 2.0, 30.0, 200.0, 3.0),
                    band(-26.0, 2.0, 20.0, 180.0, 3.0),
                    band(-26.0, 2.0, 10.0, 120.0, 3.0),
                ],
            ),
        ),
    ]
}

/// Adds `params` as `name`, or replaces the preset already called so.
pub(crate) fn save_preset<P: Clone>(presets: &mut Vec<Preset<P>>, name: &str, params: &P) {
    let preset = Preset {
        name: name.to_owned(),
        params: params.clone(),
    };
    match presets.iter_mut().find(|p| p.name == name) {
        Some(existing) => *existing = preset,
        None => presets.push(preset),
    }
}

/// The edited parameters, if changed. `presets`: the user's own, saved and
/// deleted here.
pub(crate) fn show(
    ui: &mut egui::Ui,
    params: &MultibandCompressor,
    activity: &BandActivity,
    state: &mut EffectPanelState,
    presets: &mut Vec<Preset<MultibandCompressor>>,
) -> Option<AudioEffectKind> {
    let mut params = params.clone();
    let builtin = builtin_presets();
    let mut changed = preset_bar(
        ui,
        "compressor_preset",
        &mut params,
        &builtin,
        state,
        presets,
    );
    ui.add_space(6.0);
    changed |= graph(ui, &mut params, activity, &state.spectrum);
    ui.add_space(8.0);
    ui.horizontal_top(|ui| {
        for band in 0..3 {
            changed |= band_strip(ui, band, &mut params, activity);
        }
    });
    changed.then_some(AudioEffectKind::MultibandCompressor(params))
}

/// Choosing a preset, and saving the current settings as one. `builtin`:
/// translation key and settings.
pub(crate) fn preset_bar<P: Clone + PartialEq>(
    ui: &mut egui::Ui,
    id_salt: &str,
    params: &mut P,
    builtin: &[(&'static str, P)],
    state: &mut EffectPanelState,
    presets: &mut Vec<Preset<P>>,
) -> bool {
    let current = builtin
        .iter()
        .find(|(_, p)| p == params)
        .map(|(key, _)| t!(*key).into_owned())
        .or_else(|| {
            presets
                .iter()
                .find(|p| p.params == *params)
                .map(|p| p.name.clone())
        })
        .unwrap_or_else(|| t!("mixer.preset_custom").into_owned());
    let mut changed = false;
    ui.horizontal(|ui| {
        ui.label(t!("mixer.preset"));
        let mut delete = None;
        egui::ComboBox::from_id_salt(id_salt)
            .width(220.0)
            .selected_text(&current)
            .show_ui(ui, |ui| {
                for (key, preset) in builtin {
                    if ui.selectable_label(preset == params, t!(*key)).clicked() {
                        *params = preset.clone();
                        changed = true;
                    }
                }
                if !presets.is_empty() {
                    ui.separator();
                }
                for (index, preset) in presets.iter().enumerate() {
                    ui.horizontal(|ui| {
                        let delete_button = ui
                            .small_button("×")
                            .on_hover_text(t!("mixer.preset_delete"));
                        if delete_button.clicked() {
                            delete = Some(index);
                        }
                        let selected = preset.params == *params;
                        if ui.selectable_label(selected, &preset.name).clicked() {
                            *params = preset.params.clone();
                            changed = true;
                        }
                    });
                }
            });
        if let Some(index) = delete {
            presets.remove(index);
        }

        let save = ui.button(t!("mixer.preset_save"));
        if save.clicked() {
            state.preset_name = presets
                .iter()
                .find(|p| p.params == *params)
                .map(|p| p.name.clone())
                .unwrap_or_default();
        }
        egui::Popup::from_toggle_button_response(&save)
            .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
            .show(|ui| {
                ui.set_min_width(220.0);
                ui.label(t!("mixer.preset_name"));
                let edit = ui.text_edit_singleline(&mut state.preset_name);
                if save.clicked() {
                    edit.request_focus();
                }
                let name = state.preset_name.trim().to_owned();
                if presets.iter().any(|p| p.name == name) {
                    ui.weak(t!("mixer.preset_replaces"));
                }
                let entered = edit.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                let confirm = ui
                    .add_enabled(
                        !name.is_empty(),
                        egui::Button::new(t!("mixer.preset_save_confirm")),
                    )
                    .clicked();
                if (confirm || entered) && !name.is_empty() {
                    save_preset(presets, &name, params);
                    ui.close();
                }
            });
    });
    changed
}

fn graph(
    ui: &mut egui::Ui,
    params: &mut MultibandCompressor,
    activity: &BandActivity,
    spectrum: &SpectrumView,
) -> bool {
    let (rect, _) = ui.allocate_exact_size(GRAPH_SIZE, egui::Sense::hover());
    let x_of = |freq: f32| rect.left() + rect.width() * freq_fraction(freq);
    let y_of = |db: f32| {
        let t = (DB_TOP - db.clamp(DB_BOTTOM, DB_TOP)) / (DB_TOP - DB_BOTTOM);
        rect.top() + rect.height() * t
    };
    let freq_at = |x: f32| fraction_freq(((x - rect.left()) / rect.width()).clamp(0.0, 1.0));
    let db_at = |y: f32| DB_TOP - (y - rect.top()) / rect.height() * (DB_TOP - DB_BOTTOM);

    let mut changed = false;
    let pointer = ui.ctx().pointer_interact_pos();
    let mut active_crossover = None;
    for k in 0..2 {
        let x = x_of(params.crossovers_hz[k]);
        let hit = egui::Rect::from_x_y_ranges(x - 6.0..=x + 6.0, rect.y_range());
        let response = ui
            .interact(hit, ui.id().with(("crossover", k)), egui::Sense::drag())
            .on_hover_cursor(egui::CursorIcon::ResizeHorizontal);
        if response.dragged()
            && let Some(pointer) = pointer
        {
            let [low, high] = params.crossovers_hz;
            let (min, max) = if k == 0 {
                (FREQ_MIN, high / MIN_CROSSOVER_RATIO)
            } else {
                (low * MIN_CROSSOVER_RATIO, FREQ_MAX)
            };
            params.crossovers_hz[k] = freq_at(pointer.x).clamp(min, max).round();
            changed = true;
        }
        if response.hovered() || response.dragged() {
            active_crossover = Some(k);
        }
    }
    let mut active_band = None;
    for band in 0..3 {
        let edges = band_edges(params);
        let centre = (edges[band] * edges[band + 1]).sqrt();
        let spot = egui::pos2(x_of(centre), y_of(params.bands[band].makeup_db));
        let response = ui
            .interact(
                egui::Rect::from_center_size(spot, egui::Vec2::splat(18.0)),
                ui.id().with(("band_gain", band)),
                egui::Sense::drag(),
            )
            .on_hover_cursor(egui::CursorIcon::ResizeVertical);
        if response.dragged()
            && let Some(pointer) = pointer
        {
            let (min, max) = CompressorBand::MAKEUP_RANGE_DB;
            let db = (db_at(pointer.y).clamp(min, max) * 10.0).round() / 10.0;
            params.bands[band].makeup_db = db;
            changed = true;
        }
        if response.hovered() || response.dragged() {
            active_band = Some(band);
        }
    }

    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 4.0, egui::Color32::from_gray(14));
    let edges = band_edges(params);
    for band in 0..3 {
        let area =
            egui::Rect::from_x_y_ranges(x_of(edges[band])..=x_of(edges[band + 1]), rect.y_range());
        let alpha = if active_band == Some(band) {
            0.12
        } else {
            0.05
        };
        painter.rect_filled(area, 0.0, BAND_COLORS[band].gamma_multiply(alpha));
    }

    paint_spectrum(&painter, rect, spectrum);

    let (grid, label) = (GRID_COLOR, LABEL_COLOR);
    let font = egui::FontId::proportional(9.0);
    for db in (DB_BOTTOM as i32..=DB_TOP as i32).step_by(6) {
        let y = y_of(db as f32);
        let stroke = if db == 0 {
            egui::Stroke::new(1.0, egui::Color32::from_gray(70))
        } else {
            egui::Stroke::new(1.0, grid)
        };
        painter.hline(rect.x_range(), y, stroke);
    }
    paint_freq_grid(&painter, rect);

    // The static curve is the makeup alone; the live one takes the current
    // reduction off each band, and the space between them is what is being
    // squashed now.
    let response = CrossoverResponse::new(params, PROJECT_SAMPLE_RATE);
    let makeup = params.bands.map(|b| b.makeup_db);
    let live = [0, 1, 2].map(|b| makeup[b] - activity.reduction_db[b]);
    let points: Vec<(f32, f32, f32)> = (0..=CURVE_POINTS)
        .map(|i| {
            let freq = curve_freq(i);
            (
                x_of(freq),
                y_of(response.gain_db(makeup, freq)),
                y_of(response.gain_db(live, freq)),
            )
        })
        .collect();
    let squash = crate::theme::ACCENT.gamma_multiply(0.3);
    for pair in points.windows(2) {
        let [(x0, s0, l0), (x1, s1, l1)] = [pair[0], pair[1]];
        if l0 - s0 > 0.5 || l1 - s1 > 0.5 {
            painter.add(egui::Shape::convex_polygon(
                vec![
                    egui::pos2(x0, s0),
                    egui::pos2(x1, s1),
                    egui::pos2(x1, l1),
                    egui::pos2(x0, l0),
                ],
                squash,
                egui::Stroke::NONE,
            ));
        }
    }
    painter.add(egui::Shape::line(
        points.iter().map(|&(x, s, _)| egui::pos2(x, s)).collect(),
        egui::Stroke::new(1.5, egui::Color32::from_gray(150)),
    ));
    painter.add(egui::Shape::line(
        points.iter().map(|&(x, _, l)| egui::pos2(x, l)).collect(),
        egui::Stroke::new(2.0, crate::theme::ACCENT),
    ));

    paint_spectrum_scale(&painter, rect);
    // Not +18: the reset button sits there.
    for db in (DB_BOTTOM as i32 + 6..DB_TOP as i32 - 6).step_by(6) {
        let text = if db > 0 {
            format!("+{db}")
        } else {
            db.to_string()
        };
        painter.text(
            egui::pos2(rect.right() - 4.0, y_of(db as f32) - 1.0),
            egui::Align2::RIGHT_BOTTOM,
            text,
            font.clone(),
            label,
        );
    }
    for (k, &crossover_hz) in params.crossovers_hz.iter().enumerate() {
        let x = x_of(crossover_hz);
        let lit = active_crossover == Some(k);
        let color = if lit {
            egui::Color32::from_gray(230)
        } else {
            egui::Color32::from_gray(130)
        };
        painter.add(egui::Shape::dashed_line(
            &[
                egui::pos2(x, rect.top() + 8.0),
                egui::pos2(x, rect.bottom() - 14.0),
            ],
            egui::Stroke::new(1.0, color),
            4.0,
            3.0,
        ));
        let tip = egui::pos2(x, rect.top() + 10.0);
        painter.add(egui::Shape::convex_polygon(
            vec![
                tip,
                tip + egui::vec2(6.0, -9.0),
                tip + egui::vec2(-6.0, -9.0),
            ],
            color,
            egui::Stroke::NONE,
        ));
        if lit {
            painter.text(
                egui::pos2(x + 8.0, rect.top() + 2.0),
                egui::Align2::LEFT_TOP,
                format!("{} Hz", params.crossovers_hz[k].round()),
                egui::FontId::proportional(11.0),
                egui::Color32::WHITE,
            );
        }
    }
    for band in 0..3 {
        let centre = (edges[band] * edges[band + 1]).sqrt();
        let spot = egui::pos2(x_of(centre), y_of(params.bands[band].makeup_db));
        let lit = active_band == Some(band);
        painter.circle(
            spot,
            if lit { 7.0 } else { 5.5 },
            BAND_COLORS[band],
            egui::Stroke::new(1.5, egui::Color32::from_gray(if lit { 255 } else { 20 })),
        );
        if lit {
            painter.text(
                spot + egui::vec2(10.0, -10.0),
                egui::Align2::LEFT_BOTTOM,
                format!("{:+.1} dB", params.bands[band].makeup_db),
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
        .on_hover_text(t!("mixer.reset_all_hint"))
        .clicked()
    {
        *params = MultibandCompressor::DEFAULT;
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

/// Under everything: the input as a shade, the output as a line on it.
pub(crate) fn paint_spectrum(painter: &egui::Painter, rect: egui::Rect, spectrum: &SpectrumView) {
    if spectrum.pre.len() != CURVE_POINTS + 1 {
        return;
    }
    let x_of = |freq: f32| rect.left() + rect.width() * freq_fraction(freq);
    let y_of = |db: f32| spectrum_y(rect, db);
    let shade = egui::Color32::from_white_alpha(28);
    for i in 0..CURVE_POINTS {
        let (x0, x1) = (x_of(curve_freq(i)), x_of(curve_freq(i + 1)));
        let (y0, y1) = (y_of(spectrum.pre[i]), y_of(spectrum.pre[i + 1]));
        if y0 < rect.bottom() || y1 < rect.bottom() {
            painter.add(egui::Shape::convex_polygon(
                vec![
                    egui::pos2(x0, rect.bottom()),
                    egui::pos2(x0, y0),
                    egui::pos2(x1, y1),
                    egui::pos2(x1, rect.bottom()),
                ],
                shade,
                egui::Stroke::NONE,
            ));
        }
    }
    painter.add(egui::Shape::line(
        (0..=CURVE_POINTS)
            .map(|i| egui::pos2(x_of(curve_freq(i)), y_of(spectrum.post[i])))
            .collect(),
        egui::Stroke::new(1.0, egui::Color32::from_white_alpha(110)),
    ));
}

pub(crate) fn spectrum_y(rect: egui::Rect, db: f32) -> f32 {
    let t = (SPECTRUM_TOP_DB - db) / (SPECTRUM_TOP_DB - SPECTRUM_FLOOR_DB);
    rect.top() + rect.height() * t.clamp(0.0, 1.0)
}

/// The dBFS marks of the spectrum on the left edge.
pub(crate) fn paint_spectrum_scale(painter: &egui::Painter, rect: egui::Rect) {
    for db in [-30.0, -60.0, -90.0] {
        painter.text(
            egui::pos2(rect.left() + 4.0, spectrum_y(rect, db) - 1.0),
            egui::Align2::LEFT_BOTTOM,
            format!("{db}"),
            egui::FontId::proportional(9.0),
            egui::Color32::from_gray(80),
        );
    }
}

/// Vertical lines and labels at the round frequencies.
pub(crate) fn paint_freq_grid(painter: &egui::Painter, rect: egui::Rect) {
    for freq in [50.0, 100.0, 200.0, 500.0, 1000.0, 2000.0, 5000.0, 10_000.0] {
        let x = rect.left() + rect.width() * freq_fraction(freq);
        painter.vline(x, rect.y_range(), egui::Stroke::new(1.0, GRID_COLOR));
        painter.text(
            egui::pos2(x, rect.bottom() - 3.0),
            egui::Align2::CENTER_BOTTOM,
            format_freq(freq),
            egui::FontId::proportional(9.0),
            LABEL_COLOR,
        );
    }
}

fn band_strip(
    ui: &mut egui::Ui,
    band: usize,
    params: &mut MultibandCompressor,
    activity: &BandActivity,
) -> bool {
    let edges = band_edges(params);
    let range = format!(
        "{}–{} Hz",
        format_freq(edges[band]),
        format_freq(edges[band + 1])
    );
    let settings = &mut params.bands[band];
    let mut changed = false;
    egui::Frame::group(ui.style())
        .fill(ui.visuals().faint_bg_color)
        .inner_margin(egui::Margin::same(8))
        .show(ui, |ui| {
            ui.set_width(STRIP_WIDTH);
            ui.vertical(|ui| {
                ui.push_id(band, |ui| {
                    ui.horizontal(|ui| {
                        let (dot, _) =
                            ui.allocate_exact_size(egui::Vec2::splat(10.0), egui::Sense::hover());
                        ui.painter()
                            .circle_filled(dot.center(), 4.5, BAND_COLORS[band]);
                        ui.strong(
                            [
                                t!("mixer.band_low"),
                                t!("mixer.band_mid"),
                                t!("mixer.band_high"),
                            ][band]
                                .clone(),
                        );
                        ui.weak(range);
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            let reset = ui
                                .small_button(t!("mixer.reset"))
                                .on_hover_text(t!("mixer.reset_band_hint"));
                            if reset.clicked() {
                                *settings = CompressorBand::DEFAULT;
                                changed = true;
                            }
                        });
                    });
                    ui.add_space(4.0);
                    ui.horizontal_top(|ui| {
                        let column = egui::vec2(FADER_COLUMN_WIDTH, 0.0);
                        let layout = egui::Layout::top_down(egui::Align::Center);
                        ui.allocate_ui_with_layout(column, layout, |ui| {
                            ui.set_width(FADER_COLUMN_WIDTH);
                            ui.small(t!("mixer.threshold"));
                            let level = activity.level[band];
                            let reduction = activity.reduction_db[band];
                            if let Some(db) =
                                threshold_fader(ui, settings.threshold_db, level, reduction)
                            {
                                settings.threshold_db = db;
                                changed = true;
                            }
                            changed |= value_field(
                                ui,
                                &mut settings.threshold_db,
                                CompressorBand::THRESHOLD_RANGE_DB,
                                0.2,
                                1,
                                " dB",
                            );
                        });
                        let d = CompressorBand::DEFAULT;
                        ui.vertical(|ui| {
                            ui.horizontal_top(|ui| {
                                changed |= param_knob(
                                    ui,
                                    &t!("mixer.ratio"),
                                    &mut settings.ratio,
                                    KnobScale::new(CompressorBand::RATIO_RANGE, true, d.ratio),
                                    (0.05, 1, ":1"),
                                );
                                changed |= param_knob(
                                    ui,
                                    &t!("mixer.makeup_short"),
                                    &mut settings.makeup_db,
                                    KnobScale::new(
                                        CompressorBand::MAKEUP_RANGE_DB,
                                        false,
                                        d.makeup_db,
                                    ),
                                    (0.1, 1, " dB"),
                                );
                            });
                            ui.add_space(6.0);
                            ui.horizontal_top(|ui| {
                                changed |= param_knob(
                                    ui,
                                    &t!("mixer.attack"),
                                    &mut settings.attack_ms,
                                    KnobScale::new(
                                        CompressorBand::ATTACK_RANGE_MS,
                                        true,
                                        d.attack_ms,
                                    ),
                                    (0.2, 1, " ms"),
                                );
                                changed |= param_knob(
                                    ui,
                                    &t!("mixer.release"),
                                    &mut settings.release_ms,
                                    KnobScale::new(
                                        CompressorBand::RELEASE_RANGE_MS,
                                        true,
                                        d.release_ms,
                                    ),
                                    (2.0, 0, " ms"),
                                );
                            });
                        });
                    });
                });
            });
        });
    changed
}

pub(crate) struct KnobScale {
    range: (f32, f32),
    log: bool,
    default: f32,
}

impl KnobScale {
    pub(crate) fn new(range: (f32, f32), log: bool, default: f32) -> Self {
        Self {
            range,
            log,
            default,
        }
    }
}

/// Label, knob and the numeric field under it; `field` is speed, decimals
/// and suffix of the field.
pub(crate) fn param_knob(
    ui: &mut egui::Ui,
    label: &str,
    value: &mut f32,
    scale: KnobScale,
    (speed, decimals, suffix): (f64, usize, &str),
) -> bool {
    let cell = egui::vec2(KNOB_CELL_WIDTH, 0.0);
    ui.allocate_ui_with_layout(cell, egui::Layout::top_down(egui::Align::Center), |ui| {
        ui.set_width(KNOB_CELL_WIDTH);
        ui.small(label);
        let t = knob_fraction(*value, scale.range, scale.log);
        let default = knob_fraction(scale.default, scale.range, scale.log);
        let (_, turned) = knob(ui, t, default, KNOB_RADIUS);
        let mut changed = false;
        if let Some(t) = turned {
            *value = knob_value(t, scale.range, scale.log);
            changed = true;
        }
        changed |= value_field(ui, value, scale.range, speed, decimals, suffix);
        changed
    })
    .inner
}

/// A numeric field for an `f32` within `(min, max)`.
pub(crate) fn value_field(
    ui: &mut egui::Ui,
    value: &mut f32,
    (min, max): (f32, f32),
    speed: f64,
    decimals: usize,
    suffix: &str,
) -> bool {
    let mut field = *value as f64;
    let changed = drag_field(
        ui,
        &mut field,
        speed,
        min as f64..=max as f64,
        decimals,
        suffix,
    );
    if changed {
        *value = field as f32;
    }
    changed
}

/// Threshold fader over the band's level meter: the part of the level past
/// the threshold lights up, and the reduction it causes grows down beside
/// it. Moved by the drag like the mixer faders, double click for the default.
fn threshold_fader(
    ui: &mut egui::Ui,
    threshold_db: f32,
    level: f32,
    reduction_db: f32,
) -> Option<f32> {
    const SCALE_WIDTH: f32 = 22.0;
    const GROOVE_WIDTH: f32 = 26.0;
    const BAR_WIDTH: f32 = 7.0;
    const HANDLE: egui::Vec2 = egui::vec2(24.0, 14.0);
    const REDUCTION_RANGE_DB: f32 = 24.0;
    let (min, max) = CompressorBand::THRESHOLD_RANGE_DB;
    let size = egui::vec2(
        SCALE_WIDTH + GROOVE_WIDTH + 2.0 * BAR_WIDTH + 4.0,
        FADER_HEIGHT,
    );
    let (rect, response) = ui.allocate_exact_size(size, egui::Sense::click_and_drag());
    let response = response.on_hover_cursor(egui::CursorIcon::ResizeVertical);
    let top = rect.top() + HANDLE.y / 2.0;
    let bottom = rect.bottom() - HANDLE.y / 2.0;
    let y_of = |db: f32| bottom - (db.clamp(min, max) - min) / (max - min) * (bottom - top);

    let mut changed = None;
    if response.double_clicked() {
        changed = Some(CompressorBand::DEFAULT.threshold_db);
    } else if response.dragged() {
        let dy = response.drag_delta().y;
        if dy != 0.0 {
            let fine = if ui.input(|i| i.modifiers.shift) {
                0.1
            } else {
                1.0
            };
            let db = threshold_db - dy / (bottom - top) * (max - min) * fine;
            changed = Some(db.clamp(min, max));
        }
    }

    let painter = ui.painter();
    let weak = ui.visuals().weak_text_color();
    let font = egui::FontId::proportional(9.0);
    let groove_x = rect.left() + SCALE_WIDTH + GROOVE_WIDTH / 2.0;
    for db in [0.0, -12.0, -24.0, -36.0, -48.0, -60.0] {
        let y = y_of(db);
        painter.text(
            egui::pos2(rect.left() + SCALE_WIDTH - 3.0, y),
            egui::Align2::RIGHT_CENTER,
            format!("{db}"),
            font.clone(),
            weak,
        );
        painter.line_segment(
            [egui::pos2(groove_x - 9.0, y), egui::pos2(groove_x - 5.0, y)],
            egui::Stroke::new(1.0, weak),
        );
    }
    painter.rect_filled(
        egui::Rect::from_min_max(
            egui::pos2(groove_x - 2.0, top),
            egui::pos2(groove_x + 2.0, bottom),
        ),
        2.0,
        egui::Color32::from_gray(15),
    );

    let level_x = rect.right() - 2.0 * BAR_WIDTH - 2.0;
    let level_bar = egui::Rect::from_min_max(
        egui::pos2(level_x, top),
        egui::pos2(level_x + BAR_WIDTH, bottom),
    );
    painter.rect_filled(level_bar, 1.0, egui::Color32::from_gray(10));
    let level_db = 20.0 * level.max(1e-9).log10();
    if level_db > min {
        let threshold_y = y_of(threshold_db);
        let level_y = y_of(level_db);
        let under = egui::Rect::from_min_max(
            egui::pos2(level_bar.left(), level_y.max(threshold_y)),
            level_bar.right_bottom(),
        );
        painter.rect_filled(under, 0.0, egui::Color32::from_rgb(60, 200, 90));
        if level_y < threshold_y {
            let over = egui::Rect::from_x_y_ranges(level_bar.x_range(), level_y..=threshold_y);
            painter.rect_filled(over, 0.0, crate::theme::ACCENT);
        }
    }
    let reduction_bar = level_bar.translate(egui::vec2(BAR_WIDTH + 2.0, 0.0));
    painter.rect_filled(reduction_bar, 1.0, egui::Color32::from_gray(10));
    let depth = reduction_bar.height() * (reduction_db / REDUCTION_RANGE_DB).clamp(0.0, 1.0);
    if depth > 0.5 {
        let bar = egui::Rect::from_min_max(
            reduction_bar.left_top(),
            egui::pos2(reduction_bar.right(), reduction_bar.top() + depth),
        );
        painter.rect_filled(bar, 0.0, crate::theme::ERROR);
    }

    let handle = egui::Rect::from_center_size(egui::pos2(groove_x, y_of(threshold_db)), HANDLE);
    paint_fader_handle(painter, handle, response.hovered() || response.dragged());
    changed
}

#[cfg(test)]
#[path = "tests/compressor_panel.rs"]
mod tests;
