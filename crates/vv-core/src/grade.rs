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
/// is Cr, as on a vectorscope, so the wheel reads like one. The strength
/// grows with the square of the distance from the center, for fine control
/// where the small corrections are.
pub fn chroma_shift(x: f32, y: f32) -> [f32; 3] {
    let [cb, cr] = wheel_chroma(x, y);
    [1.5748 * cr, -0.187_324 * cb - 0.468_124 * cr, 1.8556 * cb]
}

/// The Cb/Cr a puck at `(x, y)` adds.
pub fn wheel_chroma(x: f32, y: f32) -> [f32; 2] {
    let gain = (x * x + y * y).sqrt() * WHEEL_CHROMA_STRENGTH;
    [x * gain, y * gain]
}

/// The puck adding `[cb, cr]`, the inverse of `wheel_chroma`: at the edge of
/// the disc if that much is out of its reach.
pub fn wheel_for_chroma([cb, cr]: [f32; 2]) -> (f32, f32) {
    let length = (cb * cb + cr * cr).sqrt();
    if length < 1e-9 {
        return (0.0, 0.0);
    }
    let radius = (length / WHEEL_CHROMA_STRENGTH).sqrt().min(1.0);
    (cb / length * radius, cr / length * radius)
}

/// Shadows, midtones and highlights weights of a pixel of luma `luma`: soft,
/// summing to 1. Mirrors `range_weights` in transform.wgsl.
pub fn range_weights(luma: f32, low: f32, high: f32) -> [f32; 3] {
    let high = high.max(low);
    let soft = (0.5 * low.min(high - low).min(1.0 - high)).max(0.001);
    let smoothstep = |e0: f32, e1: f32, x: f32| {
        let t = ((x - e0) / (e1 - e0)).clamp(0.0, 1.0);
        t * t * (3.0 - 2.0 * t)
    };
    let shadows = 1.0 - smoothstep(low - soft, low + soft, luma);
    let highlights = smoothstep(high - soft, high + soft, luma);
    [shadows, 1.0 - shadows - highlights, highlights]
}

fn luma(rgb: [f32; 3]) -> f32 {
    rgb.iter().zip(LUMA_WEIGHTS).map(|(c, w)| c * w).sum()
}

/// `grade` applied to one pixel on the CPU: the same as `apply_grade` in
/// transform.wgsl, which the compositor tests check it against.
pub fn apply_grade(rgb: [f32; 3], grade: &GradeValue) -> [f32; 3] {
    let [shadows, midtones, highlights] = range_weights(
        luma(rgb),
        grade.get(GradeParam::LowRange),
        grade.get(GradeParam::HighRange),
    );
    let weights = [shadows, midtones, highlights, 1.0];
    let mut c = rgb;
    for (wheel, weight) in GradeWheel::ALL.iter().zip(weights) {
        let shift = grade.wheel_shift(*wheel);
        for i in 0..3 {
            c[i] += weight * shift[i];
        }
    }
    let saturation = (0..3)
        .map(|i| weights[i] * grade.get(GradeWheel::ALL[i].saturation()))
        .sum::<f32>()
        * grade.get(GradeParam::Saturation);
    let y = luma(c);
    c.map(|v| (y + (v - y) * saturation).clamp(0.0, 1.0))
}

/// `grade` with the shadows, midtones and highlights wheels moved so that
/// the mean color of each range of `pixels` (RGB 0–1, measured with `grade`
/// already applied) becomes a neutral grey: gray world, per range. The
/// ranges overlap, so a wheel also moves its neighbours' means: the three
/// moves are solved together. Near-black and near-white pixels do not
/// count; a range without pixels keeps its wheel. Measured after the grade,
/// it corrects what is left, so applying it again refines what clipping and
/// the wheels' reach leave.
pub fn auto_balance(grade: &GradeValue, pixels: impl Iterator<Item = [f32; 3]>) -> GradeValue {
    const CLIPPED: f32 = 1.0 / 255.0;
    const RANGES: [GradeWheel; 3] = [
        GradeWheel::Shadows,
        GradeWheel::Midtones,
        GradeWheel::Highlights,
    ];
    let (low, high) = (
        grade.get(GradeParam::LowRange),
        grade.get(GradeParam::HighRange),
    );
    let saturations = RANGES.map(|w| grade.get(w.saturation()));
    let global_saturation = grade.get(GradeParam::Saturation);
    // Per range r: Σ w_r, Σ w_r·Cb, Σ w_r·Cr, and how much a move of each
    // wheel s shows in it: Σ w_r·w_s·saturation.
    let mut weight = [0.0f64; 3];
    let mut chroma = [[0.0f64; 2]; 3];
    let mut effect = [[0.0f64; 3]; 3];
    for rgb in pixels {
        let y = luma(rgb);
        if y <= CLIPPED || y >= 1.0 - CLIPPED {
            continue;
        }
        let cb = ((rgb[2] - y) / 1.8556) as f64;
        let cr = ((rgb[0] - y) / 1.5748) as f64;
        let w = range_weights(y, low, high).map(|w| w as f64);
        let saturation = w
            .iter()
            .zip(saturations)
            .map(|(w, s)| w * s as f64)
            .sum::<f64>()
            * global_saturation as f64;
        for r in 0..3 {
            weight[r] += w[r];
            chroma[r][0] += w[r] * cb;
            chroma[r][1] += w[r] * cr;
            for s in 0..3 {
                effect[r][s] += w[r] * w[s] * saturation;
            }
        }
    }
    // Ranges with (almost) no pixels, or desaturated, are left alone.
    let solved: Vec<usize> = (0..3)
        .filter(|&r| weight[r] >= 1.0 && effect[r][r] / weight[r] >= 0.01)
        .collect();
    let mut balanced = *grade;
    if solved.is_empty() {
        return balanced;
    }
    let matrix: Vec<Vec<f64>> = solved
        .iter()
        .map(|&r| solved.iter().map(|&s| effect[r][s] / weight[r]).collect())
        .collect();
    for component in 0..2 {
        let target: Vec<f64> = solved
            .iter()
            .map(|&r| -chroma[r][component] / weight[r])
            .collect();
        let Some(moves) = solve(matrix.clone(), target) else {
            return *grade;
        };
        for (&r, delta) in solved.iter().zip(moves) {
            let wheel = RANGES[r];
            let [cb, cr] = wheel_chroma(balanced.get(wheel.x()), balanced.get(wheel.y()));
            let mut current = [cb, cr];
            current[component] += delta as f32;
            // Out of the wheel's reach, `wheel_for_chroma` stops at its edge.
            let (x, y) = wheel_for_chroma(current);
            balanced.set(wheel.x(), x);
            balanced.set(wheel.y(), y);
        }
    }
    balanced
}

/// `matrix · x = target` by Gaussian elimination with partial pivoting;
/// `None` if singular.
fn solve(mut matrix: Vec<Vec<f64>>, mut target: Vec<f64>) -> Option<Vec<f64>> {
    let n = target.len();
    for col in 0..n {
        let pivot =
            (col..n).max_by(|&a, &b| matrix[a][col].abs().total_cmp(&matrix[b][col].abs()))?;
        if matrix[pivot][col].abs() < 1e-9 {
            return None;
        }
        matrix.swap(col, pivot);
        target.swap(col, pivot);
        for row in col + 1..n {
            let factor = matrix[row][col] / matrix[col][col];
            let pivot_row = matrix[col].clone();
            for (cell, pivot) in matrix[row].iter_mut().zip(pivot_row).skip(col) {
                *cell -= factor * pivot;
            }
            target[row] -= factor * target[col];
        }
    }
    let mut x = vec![0.0; n];
    for row in (0..n).rev() {
        let rest: f64 = (row + 1..n).map(|k| matrix[row][k] * x[k]).sum();
        x[row] = (target[row] - rest) / matrix[row][row];
    }
    Some(x)
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
