//! Viewer split view during a slip: the first frame of the slipped clip on
//! the left, the last one on the right.

use std::sync::Arc;

use vv_core::{FrameIdx, MediaId};
use vv_media::FrameYuv420;
use vv_session::frame_provider::{OwnedContent, OwnedLayer};

use crate::render_ahead::RenderAhead;
use crate::timeline_ui::SlipPreview;

/// One buffer per end: they seek independently, often far apart.
pub struct SlipViewer {
    media_id: MediaId,
    /// Each buffer on its own single-media timeline, with the id the media
    /// has in there.
    ends: [(RenderAhead, MediaId); 2],
    requested: [Option<FrameIdx>; 2],
    /// Last frame shown per side: kept while the new one decodes.
    shown: [Option<Arc<FrameYuv420>>; 2],
}

impl SlipViewer {
    pub fn new(media_id: MediaId, ends: [(RenderAhead, MediaId); 2]) -> Self {
        Self {
            media_id,
            ends,
            requested: [None; 2],
            shown: [None, None],
        }
    }

    pub fn media_id(&self) -> MediaId {
        self.media_id
    }

    /// Both frames, once each side has shown at least one.
    pub fn frames(&mut self, preview: SlipPreview) -> Option<[Arc<FrameYuv420>; 2]> {
        for (side, frame) in [preview.first_frame, preview.last_frame]
            .into_iter()
            .enumerate()
        {
            let (render_ahead, media_id) = &self.ends[side];
            if self.requested[side] != Some(frame) {
                render_ahead.jump_to(frame);
                self.requested[side] = Some(frame);
            }
            if let Some(decoded) = render_ahead.get_frame(*media_id, frame) {
                self.shown[side] = Some(decoded);
            }
        }
        Some([self.shown[0].clone()?, self.shown[1].clone()?])
    }
}

/// The two frames at half size, side by side in a frame of `timeline_size`.
pub fn split_layers(
    frames: [Arc<FrameYuv420>; 2],
    source_size: (u32, u32),
    timeline_size: (u32, u32),
) -> Vec<OwnedLayer> {
    let quarter = timeline_size.0 as f32 / 4.0;
    frames
        .into_iter()
        .zip([-quarter, quarter])
        .map(|(frame, x)| OwnedLayer {
            content: OwnedContent::Video { frame, source_size },
            transform: vv_core::Transform {
                zoom: [0.5, 0.5],
                position: [x, 0.0],
                ..Default::default()
            },
            opacity: 1.0,
            filters: Vec::new(),
            blend: vv_core::BlendMode::Normal,
            masks: Vec::new(),
        })
        .collect()
}

#[cfg(test)]
#[path = "tests/slip_viewer.rs"]
mod tests;
