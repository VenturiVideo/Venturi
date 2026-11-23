//! RGBA thumbnail of a media for the media pool: a single frame,
//! converted on CPU (too small to justify the compositor).

use crate::decode::{Decoder, FrameYuv420};
use std::path::Path;

pub struct Thumbnail {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

/// Frame at ~1s (or at the midpoint, for shorter clips): the very first
/// frame is often black because of a fade-in. An image has a single frame:
/// `duration_secs` is ignored.
pub fn generate_thumbnail(
    path: &Path,
    duration_secs: f64,
    max_width: u32,
) -> Result<Thumbnail, crate::MediaError> {
    crate::probe::ensure_init();
    let is_image = crate::probe::is_image_path(path);
    let mut decoder = if is_image {
        Decoder::open_image(path)?
    } else {
        Decoder::open(path)?
    };
    let target = (duration_secs / 2.0).min(1.0);
    if !is_image && target > 0.0 {
        decoder.seek_to_time(target)?;
    }
    let (_, frame) = decoder
        .next_frame()?
        .ok_or_else(|| crate::MediaError::NoStream(path.display().to_string()))?;
    Ok(downscale_to_rgba(&frame, max_width))
}

fn downscale_to_rgba(frame: &FrameYuv420, max_width: u32) -> Thumbnail {
    let width = frame.width.min(max_width).max(1);
    let height = ((frame.height as u64 * width as u64) / frame.width.max(1) as u64).max(1) as u32;
    let mut rgba = Vec::with_capacity((width * height * 4) as usize);
    for ty in 0..height {
        let sy = (ty as u64 * frame.height as u64 / height as u64) as u32;
        for tx in 0..width {
            let sx = (tx as u64 * frame.width as u64 / width as u64) as u32;
            let [r, g, b] = frame.rgb_at(sx, sy);
            rgba.extend_from_slice(&[r, g, b, 255]);
        }
    }
    Thumbnail {
        width,
        height,
        rgba,
    }
}

#[cfg(test)]
#[path = "tests/thumbnail.rs"]
mod tests;
