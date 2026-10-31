//! Playback bar under the viewer: playhead, in/out markers and play/pause
//! button. It serves both the preview of a media pool item (in/out = the
//! portion to bring onto the timeline) and the timeline (in/out = the
//! portion to export).

use vv_core::FrameIdx;

/// Stores optional In and Out endpoints for preview trimming or timeline export.
///
/// An unset endpoint follows its content edge, so cleared marks expand the range.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct MarkRange {
    /// Inclusive first frame, or `None` to start at frame zero.
    pub mark_in: Option<FrameIdx>,
    /// Exclusive: "o" on frame `p` includes it.
    pub mark_out: Option<FrameIdx>,
}

impl MarkRange {
    /// Resolves the selected range within `0..=total`, keeping `in <= out`.
    ///
    /// Unset endpoints use the content edges; out-of-range values are clamped.
    pub fn resolve(&self, total: FrameIdx) -> (FrameIdx, FrameIdx) {
        let total = total.max(0);
        let out = self.mark_out.unwrap_or(total).clamp(0, total);
        let mark_in = self.mark_in.unwrap_or(0).clamp(0, out);
        (mark_in, out)
    }

    /// Sets the inclusive In point and clamps it to the content range.
    ///
    /// Clears Out if the new In point would otherwise follow it.
    pub fn set_in(&mut self, frame: FrameIdx, total: FrameIdx) {
        let frame = frame.clamp(0, total.max(0));
        self.mark_in = Some(frame);
        if self.mark_out.is_some_and(|out| out <= frame) {
            self.mark_out = None;
        }
    }

    /// Sets Out after the inclusive frame and clamps it to the content range.
    ///
    /// Clears In if the new Out point would otherwise precede or touch it.
    pub fn set_out(&mut self, frame: FrameIdx, total: FrameIdx) {
        let out = (frame + 1).clamp(0, total.max(0));
        self.mark_out = Some(out);
        if self.mark_in.is_some_and(|mark_in| mark_in >= out) {
            self.mark_in = None;
        }
    }

    pub fn clear_in(&mut self) {
        self.mark_in = None;
    }

    pub fn clear_out(&mut self) {
        self.mark_out = None;
    }

    pub fn clear_all(&mut self) {
        *self = Self::default();
    }

    /// Reports whether these marks resolve to the full content range.
    pub fn is_full(&self, total: FrameIdx) -> bool {
        self.resolve(total) == (0, total.max(0))
    }
}

#[derive(Default)]
pub struct TransportResponse {
    pub seek: Option<FrameIdx>,
    pub toggle_play: bool,
}

const BAR_HEIGHT: f32 = 30.0;
const TRACK_Y: f32 = 12.0;
const SIDE_PADDING: f32 = 10.0;
const PLAY_BUTTON_SIZE: f32 = 26.0;

/// `total` frames of content; the playhead can sit on `0..total`.
pub fn show_transport(
    ui: &mut egui::Ui,
    total: FrameIdx,
    playhead: FrameIdx,
    marks: (FrameIdx, FrameIdx),
    playing: bool,
) -> TransportResponse {
    let mut response = TransportResponse::default();
    let (rect, bar_resp) = ui.allocate_exact_size(
        egui::vec2(ui.available_width(), BAR_HEIGHT),
        egui::Sense::click_and_drag(),
    );
    let track = egui::Rect::from_min_max(
        egui::pos2(rect.left() + SIDE_PADDING, rect.top() + TRACK_Y - 2.0),
        egui::pos2(rect.right() - SIDE_PADDING, rect.top() + TRACK_Y + 2.0),
    );
    let span = total.max(1) as f32;
    let x_of =
        |frame: FrameIdx| track.left() + track.width() * (frame as f32 / span).clamp(0.0, 1.0);

    if total > 0
        && let Some(pos) = bar_resp.interact_pointer_pos()
    {
        let frac = ((pos.x - track.left()) / track.width().max(1.0)).clamp(0.0, 1.0);
        let frame = ((frac * span).round() as FrameIdx).min(total - 1);
        if frame != playhead {
            response.seek = Some(frame);
        }
    }

    let painter = ui.painter_at(rect);
    painter.rect_filled(track, 2.0, egui::Color32::from_gray(60));
    let (mark_in, mark_out) = marks;
    let region = egui::Rect::from_min_max(
        egui::pos2(x_of(mark_in), track.top()),
        egui::pos2(x_of(mark_out), track.bottom()),
    );
    painter.rect_filled(region, 2.0, egui::Color32::from_gray(150));

    let marker_color = egui::Color32::from_gray(185);
    for (frame, is_in) in [(mark_in, true), (mark_out, false)] {
        let x = x_of(frame);
        painter.vline(
            x,
            track.top()..=track.bottom() + 6.0,
            egui::Stroke::new(1.0, marker_color),
        );
        // Flag pointing towards the inside of the region.
        let flag_w = if is_in { 7.0 } else { -7.0 };
        let top = track.bottom() + 6.0;
        let mut points = vec![
            egui::pos2(x, top),
            egui::pos2(x + flag_w, top + 3.0),
            egui::pos2(x + flag_w, top + 10.0),
            egui::pos2(x, top + 10.0),
        ];
        // egui's tessellator wants the vertices in clockwise order.
        if !is_in {
            points.reverse();
        }
        painter.add(egui::Shape::convex_polygon(
            points,
            marker_color,
            egui::Stroke::NONE,
        ));
    }

    let playhead_color = crate::theme::PLAYHEAD;
    let px = x_of(playhead);
    painter.vline(
        px,
        rect.top() + 2.0..=track.bottom() + 4.0,
        egui::Stroke::new(2.0, playhead_color),
    );
    painter.rect_filled(
        egui::Rect::from_center_size(egui::pos2(px, rect.top() + 6.0), egui::vec2(10.0, 10.0)),
        2.0,
        playhead_color,
    );

    ui.vertical_centered(|ui| {
        let (button_rect, button_resp) = ui.allocate_exact_size(
            egui::vec2(PLAY_BUTTON_SIZE, PLAY_BUTTON_SIZE),
            egui::Sense::click(),
        );
        let visuals = ui.style().interact(&button_resp);
        ui.painter()
            .rect_filled(button_rect, 4.0, visuals.weak_bg_fill);
        let color = visuals.fg_stroke.color;
        let c = button_rect.center();
        // Icons drawn by hand: the ▶/⏸ glyphs are not guaranteed in egui's fonts.
        if playing {
            for dx in [-4.0, 4.0] {
                ui.painter().rect_filled(
                    egui::Rect::from_center_size(c + egui::vec2(dx, 0.0), egui::vec2(4.0, 12.0)),
                    1.0,
                    color,
                );
            }
        } else {
            ui.painter().add(egui::Shape::convex_polygon(
                vec![
                    c + egui::vec2(-4.0, -7.0),
                    c + egui::vec2(7.0, 0.0),
                    c + egui::vec2(-4.0, 7.0),
                ],
                color,
                egui::Stroke::NONE,
            ));
        }
        if button_resp
            .on_hover_text(t!("transport.play_hint"))
            .clicked()
        {
            response.toggle_play = true;
        }
    });

    response
}

#[cfg(test)]
#[path = "tests/transport.rs"]
mod tests;
