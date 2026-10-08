//! The Color window: scopes of the selected clip above its color
//! correction, a small "Color page".

use vv_core::FrameIdx;
use vv_render::ScopeKind;

use crate::grade_panel::{GradeInfo, GradeSectionResponse};

/// The scopes as shown, and what the window asks of them for the next frame.
#[derive(Default)]
pub(crate) struct ScopeView {
    /// Per slot, the egui id and the texture registered under it.
    pub(crate) textures: [Option<(egui::TextureId, vv_render::wgpu::Texture)>; 2],
    /// Pixel size wanted per slot; zero for a slot not shown.
    pub(crate) sizes: [(u32, u32); 2],
    pub(crate) drawn: [Option<DrawnScope>; 2],
    /// Whether there is a clip to measure: the selected one, under the playhead.
    pub(crate) has_source: bool,
}

/// What a scope texture holds: scope, pixel size and the viewer frame it
/// measured (`VenturiApp::scope_generation`).
pub(crate) type DrawnScope = (ScopeKind, (u32, u32), u64);

/// What happened in the window this frame.
#[derive(Default)]
pub(crate) struct ColorWindowResponse {
    pub(crate) grade: Option<GradeSectionResponse>,
    pub(crate) add_grade: bool,
    /// Source frame to move the playhead to (keyframe arrow clicked).
    pub(crate) goto: Option<FrameIdx>,
}

fn scope_label(kind: ScopeKind) -> std::borrow::Cow<'static, str> {
    match kind {
        ScopeKind::Waveform => t!("color.waveform"),
        ScopeKind::Parade => t!("color.parade"),
        ScopeKind::Vectorscope => t!("color.vectorscope"),
        ScopeKind::Histogram => t!("color.histogram"),
    }
}

/// Narrower than this, a single scope slot.
const MIN_SCOPE_WIDTH: f32 = 260.0;
const SCOPE_HEIGHT: f32 = 240.0;

/// `grade`: `None` without a selected video clip, `Some(None)` for a clip
/// without color correction. `can_balance`: see `grade_section`.
pub(crate) fn show_color_window(
    ctx: &egui::Context,
    open: &mut bool,
    kinds: &mut [ScopeKind; 2],
    view: &mut ScopeView,
    grade: Option<Option<GradeInfo>>,
    can_balance: bool,
) -> ColorWindowResponse {
    let mut response = ColorWindowResponse::default();
    egui::Window::new(t!("color.title"))
        .id(egui::Id::new("color_window"))
        .open(open)
        .default_size([760.0, 640.0])
        .resizable(true)
        .show(ctx, |ui| {
            let spacing = ui.spacing().item_spacing.x;
            let width = ui.available_width();
            let slots = if width >= 2.0 * MIN_SCOPE_WIDTH + spacing {
                2
            } else {
                1
            };
            let slot_width = ((width - spacing * (slots - 1) as f32) / slots as f32).floor();
            let pixels = ui.ctx().pixels_per_point();
            view.sizes = [(0, 0); 2];
            ui.horizontal_top(|ui| {
                for (slot, kind_slot) in kinds.iter_mut().enumerate().take(slots) {
                    ui.vertical(|ui| {
                        ui.set_width(slot_width);
                        egui::ComboBox::from_id_salt(("scope_kind", slot))
                            .selected_text(scope_label(*kind_slot))
                            .show_ui(ui, |ui| {
                                for kind in ScopeKind::ALL {
                                    ui.selectable_value(kind_slot, kind, scope_label(kind));
                                }
                            });
                        let (rect, _) = ui.allocate_exact_size(
                            egui::vec2(slot_width, SCOPE_HEIGHT),
                            egui::Sense::hover(),
                        );
                        view.sizes[slot] = (
                            (rect.width() * pixels).round() as u32,
                            (rect.height() * pixels).round() as u32,
                        );
                        let painter = ui.painter_at(rect);
                        painter.rect_filled(rect, 2.0, egui::Color32::BLACK);
                        if !view.has_source {
                            painter.text(
                                rect.center(),
                                egui::Align2::CENTER_CENTER,
                                t!("color.no_scope_source"),
                                egui::FontId::proportional(12.0),
                                egui::Color32::from_white_alpha(110),
                            );
                        } else if let Some((id, _)) = &view.textures[slot]
                            && view.drawn[slot].is_some_and(|(kind, ..)| kind == *kind_slot)
                        {
                            painter.image(
                                *id,
                                rect,
                                egui::Rect::from_min_max(
                                    egui::pos2(0.0, 0.0),
                                    egui::pos2(1.0, 1.0),
                                ),
                                egui::Color32::WHITE,
                            );
                        }
                        if view.has_source {
                            graticule(&painter, rect, *kind_slot);
                        }
                    });
                }
            });
            ui.separator();
            egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                .show(ui, |ui| match grade {
                    None => {
                        ui.label(egui::RichText::new(t!("color.no_clip")).weak());
                    }
                    Some(None) => {
                        if ui.button(t!("color.add_correction")).clicked() {
                            response.add_grade = true;
                        }
                    }
                    Some(Some(info)) => {
                        let section = crate::grade_panel::grade_section(ui, &info, can_balance);
                        response.goto = section.goto;
                        response.grade = Some(section);
                    }
                });
        });
    response
}

/// Scale lines, targets and labels over a scope's picture.
fn graticule(painter: &egui::Painter, rect: egui::Rect, kind: ScopeKind) {
    let line = egui::Stroke::new(1.0, egui::Color32::from_white_alpha(40));
    let text = egui::Color32::from_white_alpha(110);
    let font = egui::FontId::proportional(10.0);
    match kind {
        ScopeKind::Waveform | ScopeKind::Parade => {
            for percent in [0, 25, 50, 75, 100] {
                let y = rect.bottom() - rect.height() * percent as f32 / 100.0;
                painter.hline(rect.x_range(), y, line);
                painter.text(
                    egui::pos2(rect.left() + 2.0, y),
                    egui::Align2::LEFT_BOTTOM,
                    percent.to_string(),
                    font.clone(),
                    text,
                );
            }
            if kind == ScopeKind::Parade {
                for third in [1.0, 2.0] {
                    painter.vline(
                        rect.left() + rect.width() * third / 3.0,
                        rect.y_range(),
                        line,
                    );
                }
            }
        }
        ScopeKind::Histogram => {
            for quarter in [1.0, 2.0, 3.0] {
                painter.vline(
                    rect.left() + rect.width() * quarter / 4.0,
                    rect.y_range(),
                    line,
                );
            }
        }
        ScopeKind::Vectorscope => {
            let side = rect.width().min(rect.height());
            let center = rect.center();
            // Cb/Cr ±0.5 span the square, as in scopes.wgsl.
            let at = |cb: f32, cr: f32| center + egui::vec2(cb, -cr) * side;
            painter.circle_stroke(center, side / 2.0, line);
            painter.hline(
                center.x - side / 2.0..=center.x + side / 2.0,
                center.y,
                line,
            );
            painter.vline(
                center.x,
                center.y - side / 2.0..=center.y + side / 2.0,
                line,
            );
            // Skin tones fall along this line, about 123° from +Cb.
            let skin = 123f32.to_radians();
            painter.line_segment(
                [center, at(skin.cos() * 0.5, skin.sin() * 0.5)],
                egui::Stroke::new(
                    1.0,
                    egui::Color32::from_rgba_unmultiplied(255, 200, 150, 70),
                ),
            );
            // 75 % color bars.
            for (name, rgb) in [
                ("R", [0.75, 0.0, 0.0]),
                ("Mg", [0.75, 0.0, 0.75]),
                ("B", [0.0, 0.0, 0.75]),
                ("Cy", [0.0, 0.75, 0.75]),
                ("G", [0.0, 0.75, 0.0]),
                ("Yl", [0.75, 0.75, 0.0]),
            ] {
                let [cb, cr] = vv_core::cb_cr(rgb);
                let point = at(cb, cr);
                painter.rect_stroke(
                    egui::Rect::from_center_size(point, egui::vec2(8.0, 8.0)),
                    0.0,
                    egui::Stroke::new(1.0, egui::Color32::from_white_alpha(90)),
                    egui::StrokeKind::Middle,
                );
                painter.text(
                    point + egui::vec2(6.0, -6.0),
                    egui::Align2::LEFT_BOTTOM,
                    name,
                    font.clone(),
                    text,
                );
            }
        }
    }
}

/// The color correction of `clip` at the panel's frame, if it has one.
pub(crate) fn grade_info(clip: &vv_core::Clip, frame: FrameIdx) -> Option<GradeInfo> {
    let filter = clip.effects.filters.iter().find(|f| f.kind.has_grade())?;
    let in_clip = |f: &FrameIdx| (clip.source_in()..clip.source_out()).contains(f);
    Some(GradeInfo::of(&filter.grade, frame, in_clip))
}
