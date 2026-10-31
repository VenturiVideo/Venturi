//! RGBA thumbnail of a media for the media pool: a single frame,
//! converted on CPU (too small to justify the compositor).

use crate::decode::{ColorMatrix, Decoder, FrameYuv420};
use std::path::Path;

pub struct Thumbnail {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

/// Frame at ~1s (or at the midpoint, for shorter clips): the very first
/// frame is often black because of a fade-in.
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
    let (kr, kb) = match frame.matrix {
        ColorMatrix::Bt601 => (0.299, 0.114),
        ColorMatrix::Bt709 => (0.2126, 0.0722),
        ColorMatrix::Bt2020 => (0.2627, 0.0593),
    };
    let kg = 1.0 - kr - kb;
    let mut rgba = Vec::with_capacity((width * height * 4) as usize);
    for ty in 0..height {
        let sy = (ty as u64 * frame.height as u64 / height as u64) as usize;
        let cy = (sy * frame.chroma_height as usize / frame.height as usize)
            .min(frame.chroma_height as usize - 1);
        for tx in 0..width {
            let sx = (tx as u64 * frame.width as u64 / width as u64) as usize;
            let cx = (sx * frame.chroma_width as usize / frame.width as usize)
                .min(frame.chroma_width as usize - 1);
            let y = frame.y[sy * frame.width as usize + sx] as f32;
            let (u, v) = frame.chroma_at(cx, cy);
            let (u, v) = (u as f32 - 128.0, v as f32 - 128.0);
            let (y, u, v) = if frame.full_range {
                (y, u, v)
            } else {
                (
                    (y - 16.0) * 255.0 / 219.0,
                    u * 255.0 / 224.0,
                    v * 255.0 / 224.0,
                )
            };
            let r = y + 2.0 * (1.0 - kr) * v;
            let b = y + 2.0 * (1.0 - kb) * u;
            let g = (y - kr * r - kb * b) / kg;
            rgba.extend_from_slice(&[
                r.clamp(0.0, 255.0) as u8,
                g.clamp(0.0, 255.0) as u8,
                b.clamp(0.0, 255.0) as u8,
                255,
            ]);
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
