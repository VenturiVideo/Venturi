//! Parameters of the color correction filter (`FilterKind::ColorCorrection`):
//! one `Keyframed<f32>` each, like the transform, so every control animates
//! on its own and the generic keyframe code covers them all.

use serde::{Deserialize, Serialize};

use crate::{FrameIdx, Keyframed};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum GradeParam {
    ShadowsX,
    ShadowsY,
    ShadowsLuma,
    ShadowsSat,
    MidtonesX,
    MidtonesY,
    MidtonesLuma,
    MidtonesSat,
    HighlightsX,
    HighlightsY,
    HighlightsLuma,
    HighlightsSat,
    OffsetX,
    OffsetY,
    OffsetLuma,
    Saturation,
    /// Luma where the shadows give way to the midtones.
    LowRange,
    /// Luma where the midtones give way to the highlights.
    HighRange,
}

impl GradeParam {
    pub const COUNT: usize = 18;
    pub const ALL: [Self; Self::COUNT] = [
        Self::ShadowsX,
        Self::ShadowsY,
        Self::ShadowsLuma,
        Self::ShadowsSat,
        Self::MidtonesX,
        Self::MidtonesY,
        Self::MidtonesLuma,
        Self::MidtonesSat,
        Self::HighlightsX,
        Self::HighlightsY,
        Self::HighlightsLuma,
        Self::HighlightsSat,
        Self::OffsetX,
        Self::OffsetY,
        Self::OffsetLuma,
        Self::Saturation,
        Self::LowRange,
        Self::HighRange,
    ];

    /// Position in `GradeTracks::params` and `GradeValue`: the declaration
    /// order, which is that of `ALL`.
    pub const fn index(self) -> usize {
        self as usize
    }

    /// The value that leaves the image unchanged.
    pub const fn neutral(self) -> f32 {
        match self {
            Self::ShadowsSat | Self::MidtonesSat | Self::HighlightsSat | Self::Saturation => 1.0,
            Self::LowRange => 1.0 / 3.0,
            Self::HighRange => 2.0 / 3.0,
            _ => 0.0,
        }
    }

    /// Bounds of the control. The wheel X/Y are further kept inside the
    /// unit disc by the wheel itself.
    pub fn range(self) -> std::ops::RangeInclusive<f32> {
        match self {
            Self::ShadowsSat | Self::MidtonesSat | Self::HighlightsSat | Self::Saturation => {
                0.0..=2.0
            }
            Self::LowRange | Self::HighRange => 0.0..=1.0,
            _ => -1.0..=1.0,
        }
    }
}

/// The four wheels: a color shift (X/Y on the disc) and a luminance shift
/// each, plus the saturation shown with them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GradeWheel {
    Shadows,
    Midtones,
    Highlights,
    /// Applies to the whole image, whatever its luma.
    Offset,
}

impl GradeWheel {
    pub const ALL: [Self; 4] = [
        Self::Shadows,
        Self::Midtones,
        Self::Highlights,
        Self::Offset,
    ];

    pub fn x(self) -> GradeParam {
        match self {
            Self::Shadows => GradeParam::ShadowsX,
            Self::Midtones => GradeParam::MidtonesX,
            Self::Highlights => GradeParam::HighlightsX,
            Self::Offset => GradeParam::OffsetX,
        }
    }

    pub fn y(self) -> GradeParam {
        match self {
            Self::Shadows => GradeParam::ShadowsY,
            Self::Midtones => GradeParam::MidtonesY,
            Self::Highlights => GradeParam::HighlightsY,
            Self::Offset => GradeParam::OffsetY,
        }
    }

    pub fn luma(self) -> GradeParam {
        match self {
            Self::Shadows => GradeParam::ShadowsLuma,
            Self::Midtones => GradeParam::MidtonesLuma,
            Self::Highlights => GradeParam::HighlightsLuma,
            Self::Offset => GradeParam::OffsetLuma,
        }
    }

    /// For `Offset`, the global saturation.
    pub fn saturation(self) -> GradeParam {
        match self {
            Self::Shadows => GradeParam::ShadowsSat,
            Self::Midtones => GradeParam::MidtonesSat,
            Self::Highlights => GradeParam::HighlightsSat,
            Self::Offset => GradeParam::Saturation,
        }
    }
}

/// Every `GradeParam` evaluated at one frame.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GradeValue(pub [f32; GradeParam::COUNT]);

impl GradeValue {
    pub const NEUTRAL: Self = {
        let mut values = [0.0; GradeParam::COUNT];
        let mut i = 0;
        while i < GradeParam::COUNT {
            values[i] = GradeParam::ALL[i].neutral();
            i += 1;
        }
        Self(values)
    };

    pub fn get(&self, param: GradeParam) -> f32 {
        self.0[param.index()]
    }

    pub fn set(&mut self, param: GradeParam, value: f32) {
        self.0[param.index()] = value;
    }
}

/// RGB shift of a wheel's puck at the edge of the disc, and of its
/// luminance slider at the end of its travel.
pub const WHEEL_CHROMA_STRENGTH: f32 = 0.2;
pub const WHEEL_LUMA_STRENGTH: f32 = 0.5;

/// Rec.709 luma weights, the luma the ranges and the saturation are measured on.
pub const LUMA_WEIGHTS: [f32; 3] = [0.2126, 0.7152, 0.0722];

/// The color a puck at `(x, y)` adds, with no change in luma: X is Cb and Y
/// is Cr, as on a vectorscope, so the wheel reads like one.
pub fn chroma_shift(x: f32, y: f32) -> [f32; 3] {
    let (cb, cr) = (x * WHEEL_CHROMA_STRENGTH, y * WHEEL_CHROMA_STRENGTH);
    [1.5748 * cr, -0.187_324 * cb - 0.468_124 * cr, 1.8556 * cb]
}

impl GradeValue {
    /// What `wheel` adds to a pixel fully in its range.
    pub fn wheel_shift(&self, wheel: GradeWheel) -> [f32; 3] {
        let chroma = chroma_shift(self.get(wheel.x()), self.get(wheel.y()));
        let luma = self.get(wheel.luma()) * WHEEL_LUMA_STRENGTH;
        chroma.map(|c| c + luma)
    }
}

impl Default for GradeValue {
    fn default() -> Self {
        Self::NEUTRAL
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GradePreset {
    Neutral,
    BlackAndWhite,
}

impl GradePreset {
    pub const ALL: [Self; 2] = [Self::Neutral, Self::BlackAndWhite];

    pub const fn value(self) -> GradeValue {
        let mut value = GradeValue::NEUTRAL;
        if matches!(self, Self::BlackAndWhite) {
            value.0[GradeParam::Saturation.index()] = 0.0;
        }
        value
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct GradeTracks {
    /// One per `GradeParam`, in the order of `GradeParam::ALL`.
    params: Vec<Keyframed<f32>>,
}

/// Files saved before a parameter existed have fewer tracks: the missing
/// tail is neutral.
impl<'de> Deserialize<'de> for GradeTracks {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        struct Repr {
            params: Vec<Keyframed<f32>>,
        }
        let mut params = Repr::deserialize(deserializer)?.params;
        params.truncate(GradeParam::COUNT);
        for p in GradeParam::ALL.iter().skip(params.len()) {
            params.push(Keyframed::constant(p.neutral()));
        }
        Ok(Self { params })
    }
}

impl Default for GradeTracks {
    fn default() -> Self {
        Self::constant(GradeValue::NEUTRAL)
    }
}

impl GradeTracks {
    pub fn constant(value: GradeValue) -> Self {
        Self {
            params: value.0.iter().map(|v| Keyframed::constant(*v)).collect(),
        }
    }

    pub fn track(&self, param: GradeParam) -> &Keyframed<f32> {
        &self.params[param.index()]
    }

    pub fn track_mut(&mut self, param: GradeParam) -> &mut Keyframed<f32> {
        &mut self.params[param.index()]
    }

    pub fn tracks_mut(&mut self) -> impl Iterator<Item = &mut Keyframed<f32>> {
        self.params.iter_mut()
    }

    pub fn value_at(&self, frame: FrameIdx) -> GradeValue {
        let mut value = GradeValue::NEUTRAL;
        for (slot, track) in value.0.iter_mut().zip(&self.params) {
            *slot = track.value_at(frame);
        }
        value
    }
}

#[cfg(test)]
#[path = "tests/grade.rs"]
mod tests;
