//! Picks the chroma key color from the viewer. It samples the clip's own
//! decoded frame, not the composed preview: once keyed, the preview shows
//! what lies beneath the key instead of the key itself.

use vv_core::{Rgba, Transform};
use vv_media::FrameYuv420;

use crate::viewer_overlay::frame_to_clip;

/// Half side of the square averaged around the picked pixel, in frame
/// pixels: a single pixel of a compressed source is noisy.
const SAMPLE_RADIUS: i64 = 1;
const SWATCH: f32 = 18.0;

pub(crate) enum Outcome {
    Picked(Rgba),
    Cancelled,
}

/// The source pixel under a point of the timeline frame (timeline pixels
/// from the center, Y up), as the shader maps it; `None` where the clip does
/// not show (outside the source or cropped away).
pub(crate) fn source_point(
    transform: &Transform,
    timeline_size: (u32, u32),
    source_size: (u32, u32),
    frame_point: [f32; 2],
) -> Option<[f32; 2]> {
    let (tw, th) = (timeline_size.0.max(1) as f32, timeline_size.1.max(1) as f32);
    let (sw, sh) = (source_size.0.max(1) as f32, source_size.1.max(1) as f32);
    let fit = (tw / sw).min(th / sh);
    let p = frame_to_clip(transform, frame_point);
    let flip = transform.flip.map(|f| if f { -1.0 } else { 1.0 });
    let x = p[0] * flip[0] / fit + sw / 2.0;
    let y = -p[1] * flip[1] / fit + sh / 2.0;
    let crop = transform.crop;
    let inside = x >= crop[0].max(0.0)
        && x <= sw - crop[2].max(0.0)
        && y >= crop[1].max(0.0)
        && y <= sh - crop[3].max(0.0);
    inside.then_some([x, y])
}

/// The average color around `source_point` (pixels of a `source_size`
/// media) in `frame`, which can be a smaller proxy of it.
pub(crate) fn sample(frame: &FrameYuv420, source_size: (u32, u32), source_point: [f32; 2]) -> Rgba {
    let scale = [
        frame.width as f32 / source_size.0.max(1) as f32,
        frame.height as f32 / source_size.1.max(1) as f32,
    ];
    let cx = (source_point[0] * scale[0]) as i64;
    let cy = (source_point[1] * scale[1]) as i64;
    let mut sum = [0u32; 3];
    let mut count = 0;
    for dy in -SAMPLE_RADIUS..=SAMPLE_RADIUS {
        for dx in -SAMPLE_RADIUS..=SAMPLE_RADIUS {
            let x = (cx + dx).clamp(0, frame.width as i64 - 1) as u32;
            let y = (cy + dy).clamp(0, frame.height as i64 - 1) as u32;
            for (s, c) in sum.iter_mut().zip(frame.rgb_at(x, y)) {
                *s += c as u32;
            }
            count += 1;
        }
    }
    let channel = |i: usize| (sum[i] as f32 / count as f32).round() / 255.0;
    Rgba {
        r: channel(0),
        g: channel(1),
        b: channel(2),
        a: 1.0,
    }
}

/// Crosshair over `area` with a swatch of the color under the pointer;
/// a click picks it, Esc or a right click cancels.
pub(crate) fn show(
    ui: &egui::Ui,
    frame_rect: egui::Rect,
    area: egui::Rect,
    timeline_size: (u32, u32),
    source_size: (u32, u32),
    transform: &Transform,
    frame: Option<&FrameYuv420>,
) -> Option<Outcome> {
    if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
        return Some(Outcome::Cancelled);
    }
    let response = ui.interact(area, ui.id().with("eyedropper"), egui::Sense::click());
    if response.secondary_clicked() {
        return Some(Outcome::Cancelled);
    }
    let pos = response.hover_pos()?;
    ui.ctx().set_cursor_icon(egui::CursorIcon::Crosshair);
    let scale = frame_rect.width() / timeline_size.0.max(1) as f32;
    let offset = pos - frame_rect.center();
    let frame_point = [offset.x / scale, -offset.y / scale];
    let point = source_point(transform, timeline_size, source_size, frame_point)?;
    let color = sample(frame?, source_size, point);
    paint_swatch(ui, pos, color);
    response.clicked().then_some(Outcome::Picked(color))
}

fn paint_swatch(ui: &egui::Ui, pos: egui::Pos2, color: Rgba) {
    let painter = ui.painter();
    let fill = egui::Color32::from_rgb(
        (color.r * 255.0).round() as u8,
        (color.g * 255.0).round() as u8,
        (color.b * 255.0).round() as u8,
    );
    let swatch =
        egui::Rect::from_min_size(pos + egui::vec2(14.0, 14.0), egui::vec2(SWATCH, SWATCH));
    painter.rect_filled(swatch, 2.0, fill);
    painter.rect_stroke(
        swatch,
        2.0,
        egui::Stroke::new(1.0, egui::Color32::WHITE),
        egui::StrokeKind::Outside,
    );
    let text_pos = swatch.right_center() + egui::vec2(6.0, 0.0);
    let font = egui::FontId::monospace(12.0);
    let galley = painter.layout_no_wrap(color.to_hex(), font, egui::Color32::WHITE);
    let backing = egui::Rect::from_min_size(
        text_pos - egui::vec2(3.0, galley.size().y / 2.0 + 1.0),
        galley.size() + egui::vec2(6.0, 2.0),
    );
    painter.rect_filled(backing, 2.0, egui::Color32::from_black_alpha(180));
    painter.galley(
        text_pos - egui::vec2(0.0, galley.size().y / 2.0),
        galley,
        egui::Color32::WHITE,
    );
}

#[cfg(test)]
#[path = "tests/eyedropper.rs"]
mod tests;
