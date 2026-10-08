//! Masks of a clip: shapes limiting where its layer (or, for an adjustment
//! clip, its filters) shows.
//!
//! Coordinates are *layer* pixels: timeline pixels from the center of the
//! clip as fitted at zoom 1, Y up — the same space as the viewer overlay's
//! handles. The transform moves, zooms and rotates the mask with the
//! layer; the flip does not mirror it.

use serde::{Deserialize, Serialize};

use crate::model::{FrameIdx, Keyframed, Lerp};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum MaskShape {
    Rectangle,
    Ellipse,
    /// Closed bezier path (`ClipMask::path`).
    Path,
}

impl MaskShape {
    pub const ALL: [Self; 3] = [Self::Rectangle, Self::Ellipse, Self::Path];
}

/// How a mask combines with the ones above it in the list; the first one
/// combines with an empty mask (nothing shows), except `Subtract`, which
/// cuts from a full one.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum MaskMode {
    #[default]
    Add,
    Subtract,
    Intersect,
}

impl MaskMode {
    pub const ALL: [Self; 3] = [Self::Add, Self::Subtract, Self::Intersect];
}

/// An animatable scalar of a mask.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum MaskParam {
    CenterX,
    CenterY,
    /// Rectangle/ellipse only.
    Width,
    Height,
    /// Degrees, clockwise.
    Rotation,
    /// Rectangle only: corner radius in layer pixels.
    Roundness,
    /// Width of the soft edge, centered on the outline.
    Feather,
    /// Grows (positive) or shrinks the shape.
    Expansion,
    /// 0-100.
    Opacity,
}

impl MaskParam {
    pub const ALL: [Self; 9] = [
        Self::CenterX,
        Self::CenterY,
        Self::Width,
        Self::Height,
        Self::Rotation,
        Self::Roundness,
        Self::Feather,
        Self::Expansion,
        Self::Opacity,
    ];

    pub fn index(self) -> usize {
        self as usize
    }

    pub fn applies_to(self, shape: MaskShape) -> bool {
        match self {
            Self::Width | Self::Height => shape != MaskShape::Path,
            Self::Roundness => shape == MaskShape::Rectangle,
            _ => true,
        }
    }
}

/// Upper bound of the feather and corner radius sliders, in layer pixels.
pub const MASK_SOFTNESS_MAX: f32 = 1000.0;

/// A vertex of a path with its bezier handles, relative to `point`.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct PathPoint {
    pub point: [f32; 2],
    #[serde(default)]
    pub in_handle: [f32; 2],
    #[serde(default)]
    pub out_handle: [f32; 2],
}

impl PathPoint {
    pub const fn corner(point: [f32; 2]) -> Self {
        Self {
            point,
            in_handle: [0.0; 2],
            out_handle: [0.0; 2],
        }
    }
}

/// Closed path, relative to the mask's center and rotation.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct MaskPath {
    pub points: Vec<PathPoint>,
}

/// Point by point when the two paths have the same vertices; otherwise
/// there is no correspondence and the path holds until the next keyframe.
impl Lerp for MaskPath {
    fn lerp(a: &Self, b: &Self, t: f32) -> Self {
        if a.points.len() != b.points.len() {
            return if t >= 1.0 { b.clone() } else { a.clone() };
        }
        let mix =
            |p: [f32; 2], q: [f32; 2]| [f32::lerp(&p[0], &q[0], t), f32::lerp(&p[1], &q[1], t)];
        Self {
            points: a
                .points
                .iter()
                .zip(&b.points)
                .map(|(p, q)| PathPoint {
                    point: mix(p.point, q.point),
                    in_handle: mix(p.in_handle, q.in_handle),
                    out_handle: mix(p.out_handle, q.out_handle),
                })
                .collect(),
        }
    }
}

/// Segments per bezier curve when flattening a path.
const CURVE_SEGMENTS: usize = 12;

impl MaskPath {
    /// Point at `t` (0..1) of the segment from vertex `i` to the next one.
    pub fn segment_point(&self, i: usize, t: f32) -> [f32; 2] {
        let a = self.points[i];
        let b = self.points[(i + 1) % self.points.len()];
        cubic(
            a.point,
            add(a.point, a.out_handle),
            add(b.point, b.in_handle),
            b.point,
            t,
        )
    }

    /// The outline as a polygon, in the path's own coordinates.
    pub fn flatten(&self) -> Vec<[f32; 2]> {
        let n = self.points.len();
        let mut out = Vec::new();
        for i in 0..n {
            let a = self.points[i];
            let b = self.points[(i + 1) % n];
            out.push(a.point);
            if a.out_handle == [0.0; 2] && b.in_handle == [0.0; 2] {
                continue;
            }
            for s in 1..CURVE_SEGMENTS {
                out.push(self.segment_point(i, s as f32 / CURVE_SEGMENTS as f32));
            }
        }
        out
    }
}

fn add(a: [f32; 2], b: [f32; 2]) -> [f32; 2] {
    [a[0] + b[0], a[1] + b[1]]
}

fn cubic(p0: [f32; 2], p1: [f32; 2], p2: [f32; 2], p3: [f32; 2], t: f32) -> [f32; 2] {
    let u = 1.0 - t;
    let w = [u * u * u, 3.0 * u * u * t, 3.0 * u * t * t, t * t * t];
    [
        w[0] * p0[0] + w[1] * p1[0] + w[2] * p2[0] + w[3] * p3[0],
        w[0] * p0[1] + w[1] * p1[1] + w[2] * p2[1] + w[3] * p3[1],
    ]
}

/// A mask of `EffectStack::masks`; their order is the order in which they
/// combine.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ClipMask {
    pub shape: MaskShape,
    pub enabled: bool,
    pub invert: bool,
    #[serde(default)]
    pub mode: MaskMode,
    /// One per `MaskParam`, in the order of `MaskParam::ALL`.
    params: Vec<Keyframed<f32>>,
    #[serde(default = "empty_path")]
    pub path: Keyframed<MaskPath>,
}

fn empty_path() -> Keyframed<MaskPath> {
    Keyframed::constant(MaskPath::default())
}

impl ClipMask {
    /// Centered on the layer, half of `layer_size` (timeline pixels) wide
    /// and high; a path starts as that rectangle's corners.
    pub fn new(shape: MaskShape, layer_size: (u32, u32)) -> Self {
        let (w, h) = (layer_size.0 as f32 / 2.0, layer_size.1 as f32 / 2.0);
        let default = |p: MaskParam| match p {
            MaskParam::Width => w,
            MaskParam::Height => h,
            MaskParam::Opacity => 100.0,
            _ => 0.0,
        };
        let path = if shape == MaskShape::Path {
            let (x, y) = (w / 2.0, h / 2.0);
            MaskPath {
                points: [[-x, y], [x, y], [x, -y], [-x, -y]]
                    .map(PathPoint::corner)
                    .to_vec(),
            }
        } else {
            MaskPath::default()
        };
        Self {
            shape,
            enabled: true,
            invert: false,
            mode: MaskMode::Add,
            params: MaskParam::ALL
                .iter()
                .map(|p| Keyframed::constant(default(*p)))
                .collect(),
            path: Keyframed::constant(path),
        }
    }

    /// Whether it limits anything: a path with fewer than three vertices
    /// (still being drawn) does not.
    pub fn is_active(&self) -> bool {
        self.enabled
            && (self.shape != MaskShape::Path
                || self
                    .path
                    .keyframes()
                    .iter()
                    .map(|k| &k.1)
                    .chain([&self.path.default])
                    .any(|p| p.points.len() >= 3))
    }

    pub fn track(&self, param: MaskParam) -> &Keyframed<f32> {
        &self.params[param.index()]
    }

    pub fn track_mut(&mut self, param: MaskParam) -> &mut Keyframed<f32> {
        &mut self.params[param.index()]
    }

    pub fn tracks_mut(&mut self) -> impl Iterator<Item = &mut Keyframed<f32>> {
        self.params.iter_mut()
    }

    pub fn value_at(&self, frame: FrameIdx) -> MaskValue {
        let v = |p: MaskParam| self.track(p).value_at(frame);
        let center = [v(MaskParam::CenterX), v(MaskParam::CenterY)];
        let rotation = v(MaskParam::Rotation);
        let polygon = if self.shape == MaskShape::Path {
            let (sin, cos) = (-rotation.to_radians()).sin_cos();
            self.path
                .value_at(frame)
                .flatten()
                .into_iter()
                .map(|[x, y]| [center[0] + x * cos - y * sin, center[1] + x * sin + y * cos])
                .collect()
        } else {
            Vec::new()
        };
        MaskValue {
            shape: self.shape,
            invert: self.invert,
            mode: self.mode,
            center,
            size: [v(MaskParam::Width).max(0.0), v(MaskParam::Height).max(0.0)],
            rotation,
            roundness: v(MaskParam::Roundness).max(0.0),
            feather: v(MaskParam::Feather).max(0.0),
            expansion: v(MaskParam::Expansion),
            opacity: (v(MaskParam::Opacity) / 100.0).clamp(0.0, 1.0),
            polygon,
        }
    }
}

/// A `ClipMask` evaluated at one frame, in layer pixels.
#[derive(Debug, Clone, PartialEq)]
pub struct MaskValue {
    pub shape: MaskShape,
    pub invert: bool,
    pub mode: MaskMode,
    pub center: [f32; 2],
    pub size: [f32; 2],
    pub rotation: f32,
    pub roundness: f32,
    pub feather: f32,
    pub expansion: f32,
    /// 0-1.
    pub opacity: f32,
    /// `Path` only: the outline, center and rotation applied.
    pub polygon: Vec<[f32; 2]>,
}

#[cfg(test)]
#[path = "tests/mask.rs"]
mod tests;
