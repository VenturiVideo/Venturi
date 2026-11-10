use crate::{Chroma, FrameYuv420};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Orientation([i8; 4]);

impl Default for Orientation {
    fn default() -> Self {
        Self([1, 0, 0, 1])
    }
}

impl Orientation {
    pub(crate) fn from_stream(stream: &ffmpeg_next::Stream<'_>) -> Self {
        stream
            .side_data()
            .find(|side| side.kind() == ffmpeg_next::codec::packet::side_data::Type::DisplayMatrix)
            .and_then(|side| Self::from_bytes(side.data()))
            .unwrap_or_default()
    }

    pub(crate) fn from_bytes(bytes: &[u8]) -> Option<Self> {
        let bytes: &[u8; 36] = bytes.get(..36)?.try_into().ok()?;
        let matrix: [i32; 9] = std::array::from_fn(|i| {
            i32::from_ne_bytes(bytes[i * 4..i * 4 + 4].try_into().unwrap())
        });
        if matrix[2] != 0 || matrix[5] != 0 || matrix[8] == 0 {
            return None;
        }
        let sign = f64::from(matrix[8].signum());
        let [a, b, c, d] =
            [matrix[0], matrix[1], matrix[3], matrix[4]].map(|v| f64::from(v) * sign);
        let x = a.hypot(b);
        let y = c.hypot(d);
        if x == 0.0 || y == 0.0 {
            return None;
        }
        let axis = |v: f64| {
            if v.abs() < 0.001 {
                Some(0)
            } else if (v.abs() - 1.0).abs() < 0.001 {
                Some(if v > 0.0 { 1 } else { -1 })
            } else {
                None
            }
        };
        let axes = [axis(a / x)?, axis(b / x)?, axis(c / y)?, axis(d / y)?];
        let [a, b, c, d] = axes;
        (a * c + b * d == 0).then_some(Self(axes))
    }

    pub(crate) fn dimensions(self, width: u32, height: u32) -> (u32, u32) {
        if self.0[0] == 0 {
            (height, width)
        } else {
            (width, height)
        }
    }

    fn plane(self, source: &[u8], width: u32, height: u32, channels: usize) -> Vec<u8> {
        let (out_width, _) = self.dimensions(width, height);
        let [a, b, c, d] = self.0;
        let mut result = vec![0; source.len()];
        for y in 0..height {
            for x in 0..width {
                let dx = if a != 0 {
                    if a > 0 { x } else { width - 1 - x }
                } else if c > 0 {
                    y
                } else {
                    height - 1 - y
                };
                let dy = if b != 0 {
                    if b > 0 { x } else { width - 1 - x }
                } else if d > 0 {
                    y
                } else {
                    height - 1 - y
                };
                let from = (y as usize * width as usize + x as usize) * channels;
                let to = (dy as usize * out_width as usize + dx as usize) * channels;
                result[to..to + channels].copy_from_slice(&source[from..from + channels]);
            }
        }
        result
    }

    pub(crate) fn apply(self, mut frame: FrameYuv420) -> FrameYuv420 {
        if self == Self::default() {
            return frame;
        }
        let (w, h) = (frame.width, frame.height);
        let (cw, ch) = (frame.chroma_width, frame.chroma_height);
        frame.y = self.plane(&frame.y, w, h, 1);
        frame.chroma = match frame.chroma {
            Chroma::Planar { u, v } => Chroma::Planar {
                u: self.plane(&u, cw, ch, 1),
                v: self.plane(&v, cw, ch, 1),
            },
            Chroma::Interleaved(uv) => Chroma::Interleaved(self.plane(&uv, cw, ch, 2)),
        };
        frame.alpha = frame.alpha.map(|alpha| self.plane(&alpha, w, h, 1));
        (frame.width, frame.height) = self.dimensions(w, h);
        (frame.chroma_width, frame.chroma_height) = self.dimensions(cw, ch);
        frame
    }
}

#[cfg(test)]
#[path = "tests/orientation.rs"]
mod tests;
