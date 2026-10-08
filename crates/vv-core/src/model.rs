//! Data model of the project. No node graph: every Clip has a fixed
//! `EffectStack`. The transforms are evaluated at runtime from
//! the keyframes, never "baked" into the cached frames (see ARCHITECTURE.md).

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

pub use crate::id_map::{FolderId, Id, IdMap, MediaId, TimelineId};

/// A folder of the media pool: only a way to group the items, it has no
/// effect on rendering.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MediaFolder {
    pub name: String,
    pub parent: Option<FolderId>,
}

/// Id of a group of linked clips: a counter, membership is only
/// the `Clip::linked_group` field.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct LinkGroupId(pub u64);

/// Counter: the clips live in ordered `Vec`s inside the tracks, not in
/// an arena.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct ClipId(pub u64);

/// Unique within its timeline, see `Timeline::alloc_marker_id`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct MarkerId(pub u64);

/// A note pinned to the timeline ruler, on one frame or over a range.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Marker {
    pub id: MarkerId,
    pub start: FrameIdx,
    /// 0 = a single frame.
    #[serde(default)]
    pub duration: FrameIdx,
    #[serde(default)]
    pub note: String,
    #[serde(default = "Marker::default_color")]
    pub color: ClipColor,
}

impl Marker {
    pub fn default_color() -> ClipColor {
        ClipColor::Yellow
    }

    pub fn end(&self) -> FrameIdx {
        self.start + self.duration
    }
}

/// Frame rate as an exact fraction (e.g. 30000/1001 for 29.97fps).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rational {
    pub num: i32,
    pub den: i32,
}

impl Rational {
    pub const fn new(num: i32, den: i32) -> Self {
        Self { num, den }
    }

    pub const fn one() -> Self {
        Self { num: 1, den: 1 }
    }

    /// From a floating point fps (as in OTIO): recognizes the NTSC fps
    /// (`n * 1000/1001`), otherwise approximates to the thousandth.
    pub fn from_fps(fps: f64) -> Self {
        if !(fps.is_finite() && fps > 0.0) {
            return Self::new(30, 1);
        }
        let whole = fps.round();
        if (fps - whole).abs() < 1e-3 {
            return Self::new(whole as i32, 1);
        }
        let ntsc = (fps * 1.001).round();
        if (fps - ntsc * 1000.0 / 1001.0).abs() < 1e-3 {
            return Self::new(ntsc as i32 * 1000, 1001);
        }
        let num = (fps * 1000.0).round() as i64;
        let g = gcd(num, 1000);
        Self::new((num / g) as i32, (1000 / g) as i32)
    }

    pub fn as_f64(self) -> f64 {
        self.num as f64 / self.den as f64
    }

    pub fn is_one(self) -> bool {
        self.den != 0 && self.num == self.den
    }

    /// Timeline frames per source frame of a media at `media_fps` on
    /// a timeline at `timeline_fps`, reduced to lowest terms (see
    /// `Clip::rate`).
    pub fn conform_rate(timeline_fps: Rational, media_fps: Rational) -> Self {
        Self::reduced(
            timeline_fps.num as i64 * media_fps.den as i64,
            timeline_fps.den as i64 * media_fps.num as i64,
        )
    }

    /// `self / other`, reduced (see `reduced`).
    pub fn divided_by(self, other: Rational) -> Self {
        Self::reduced(
            self.num as i64 * other.den as i64,
            self.den as i64 * other.num as i64,
        )
    }

    /// A clip speed from a percentage, to the hundredth of a percent.
    pub fn from_percent(percent: f64) -> Self {
        Self::reduced((percent * 100.0).round() as i64, 10_000)
    }

    pub fn as_percent(self) -> f64 {
        self.as_f64() * 100.0
    }

    /// `num/den` in lowest terms; `1/1` if not positive.
    fn reduced(mut num: i64, mut den: i64) -> Self {
        if num <= 0 || den <= 0 {
            return Self::one();
        }
        let g = gcd(num, den);
        num /= g;
        den /= g;
        // An irreducible ratio that does not fit in i32 (exotic fps on
        // both sides) is approximated: better a millionth of
        // error than an overflow.
        if num > i32::MAX as i64 || den > i32::MAX as i64 {
            let approx = (num as f64 / den as f64 * 1_000_000.0).round() as i64;
            return Self::new(approx.clamp(1, i32::MAX as i64) as i32, 1_000_000);
        }
        Self::new(num as i32, den as i32)
    }

    /// `round(frames * self)`, halves up. Exact identity for
    /// `1/1`, so an unconformed clip stays bit-for-bit as before.
    pub fn scale_round(self, frames: FrameIdx) -> FrameIdx {
        if self.is_one() || self.num <= 0 || self.den <= 0 {
            return frames;
        }
        let (num, den) = (self.num as i128, self.den as i128);
        let v = frames as i128;
        ((2 * v * num + den).div_euclid(2 * den)) as FrameIdx
    }

    /// The largest `n` such that `scale_round(n) <= scaled`: the inverse of
    /// `scale_round`, i.e. "which source frame covers this position".
    pub fn unscale_round(self, scaled: FrameIdx) -> FrameIdx {
        if self.is_one() || self.num <= 0 || self.den <= 0 {
            return scaled;
        }
        let (num, den) = (self.num as i128, self.den as i128);
        let a = (2 * scaled as i128 + 1) * den;
        let b = 2 * num;
        (-((-a).div_euclid(b)) - 1) as FrameIdx // ceil(a/b) - 1
    }
}

fn gcd(a: i64, b: i64) -> i64 {
    let (mut a, mut b) = (a.abs(), b.abs());
    while b != 0 {
        (a, b) = (b, a % b);
    }
    a.max(1)
}

/// YUV→RGB matrix of a decoded frame. BT.2020 only if the source
/// signals it: never guessed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColorMatrix {
    Bt601,
    Bt709,
    Bt2020,
}

/// Frame index, always relative to the context it is used in: source
/// frame of a media (native fps) or Timeline frame (fps of the
/// Timeline containing it). The two spaces must never be confused.
pub type FrameIdx = i64;

/// `duration_frames` of an image: ~463 days at `IMAGE_FPS`, no real
/// duration reaches it, so it acts as an "it is an image" marker without an
/// extra field and does not limit the trim.
pub const IMAGE_DURATION_FRAMES: FrameIdx = 1_000_000_000;

/// Name prefix of the compound clips in the media pool.
pub const COMPOUND_NAME_PREFIX: &str = "Compound Clip ";
pub const TIMELINE_NAME_PREFIX: &str = "Timeline ";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MediaMeta {
    pub duration_frames: FrameIdx,
    pub fps: Rational,
    /// `width`/`height` at zero and nominal `fps` if `false`: an audio-only
    /// media goes only on audio tracks.
    pub width: u32,
    pub height: u32,
    pub has_video: bool,
    pub has_audio: bool,
    pub sample_rate: u32,
    pub channels: u16,
    /// Audio streams in the container. 0 in projects saved before the
    /// field existed: the app recomputes it on opening.
    #[serde(default)]
    pub audio_streams: u16,
    #[serde(default)]
    pub file: MediaFileInfo,
}

/// What identifies the file beyond its path, recorded while it is still
/// reachable: once it goes missing it is all a forced relink can compare
/// candidates against.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct MediaFileInfo {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size_bytes: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub video_codec: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub audio_codec: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub artist: Option<String>,
}

impl MediaMeta {
    /// Audio streams to use: at least one if the media has audio.
    pub fn audio_stream_count(&self) -> usize {
        if self.has_audio {
            usize::from(self.audio_streams).max(1)
        } else {
            0
        }
    }

    /// A still image imported into the pool: it has video but no audio, and
    /// `duration_frames` is the `IMAGE_DURATION_FRAMES` sentinel (see its
    /// docs on why a dedicated field is not needed).
    pub fn is_image(&self) -> bool {
        self.has_video && !self.has_audio && self.duration_frames == IMAGE_DURATION_FRAMES
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MediaItem {
    /// For a compound clip (`compound.is_some()`) it is only the name
    /// shown in the media pool ("Compound Clip N"), not a real file.
    pub path: PathBuf,
    pub meta: MediaMeta,
    /// Stable key for the frame cache and the proxies: hash of the content
    /// (not of the path), so moving/renaming the file invalidates nothing.
    /// For a compound clip, it changes every time its nested timeline
    /// changes (see `Project::touch_compound`): it invalidates the cache
    /// of composited frames without having to compare the whole timeline.
    pub content_hash: u64,
    /// `Some` if this item is a compound clip: its content is
    /// `Project::timelines[_]` instead of a file on disk. `meta` stays
    /// valid anyway (recomputed by `Project::sync_compound_meta`), so
    /// the rest of the program (probe, drag&drop, duration on the timeline) can
    /// treat it like any other media.
    #[serde(default)]
    pub compound: Option<TimelineId>,
    /// `None`, or a folder no longer in `Project::folders`: the pool root.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub folder: Option<FolderId>,
}

/// Nesting limit for whoever walks into compound clips: a safety net
/// against a cycle (`Project::would_create_a_cycle` prevents creating one),
/// not a design limit.
pub const MAX_COMPOUND_DEPTH: u32 = 16;

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum Interpolation {
    Hold,
    Linear,
    EaseInOut,
    EaseIn,
    EaseOut,
    /// Free curve: control points of a cubic bezier normalized
    /// on the segment (from `(0,0)` to `(1,1)`), like CSS's `cubic-bezier`.
    Bezier {
        c1: [f32; 2],
        c2: [f32; 2],
    },
}

impl Interpolation {
    /// The presets offered by the keyframe editor, in bar order.
    pub const PRESETS: [Self; 5] = [
        Self::Hold,
        Self::Linear,
        Self::EaseInOut,
        Self::EaseIn,
        Self::EaseOut,
    ];

    /// Weight of the arrival keyframe at `t` (0 = the starting one).
    pub fn ease(self, t: f32) -> f32 {
        match self {
            Self::Hold => {
                if t >= 1.0 {
                    1.0
                } else {
                    0.0
                }
            }
            Self::Linear => t,
            Self::EaseInOut => smoothstep(t),
            Self::EaseIn => t * t,
            Self::EaseOut => t * (2.0 - t),
            Self::Bezier { c1, c2 } => bezier_ease(c1, c2, t),
        }
    }

    /// The equivalent control points, for the editor handles: `Hold`
    /// has no curve to manipulate.
    pub fn control_points(self) -> Option<([f32; 2], [f32; 2])> {
        match self {
            Self::Hold => None,
            Self::Linear => Some(([1.0 / 3.0, 1.0 / 3.0], [2.0 / 3.0, 2.0 / 3.0])),
            Self::EaseInOut => Some(([0.5, 0.0], [0.5, 1.0])),
            Self::EaseIn => Some(([0.42, 0.0], [1.0, 1.0])),
            Self::EaseOut => Some(([0.0, 0.0], [0.58, 1.0])),
            Self::Bezier { c1, c2 } => Some((c1, c2)),
        }
    }
}

/// The `x` of a normalized cubic bezier is not `t`: it is inverted by
/// bisection (monotonic as long as the control abscissas stay in `0..=1`).
fn bezier_ease(c1: [f32; 2], c2: [f32; 2], x: f32) -> f32 {
    let axis = |a: f32, b: f32, t: f32| {
        let u = 1.0 - t;
        3.0 * u * u * t * a + 3.0 * u * t * t * b + t * t * t
    };
    let (mut lo, mut hi) = (0.0f32, 1.0f32);
    for _ in 0..24 {
        let mid = 0.5 * (lo + hi);
        if axis(c1[0], c2[0], mid) < x {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    axis(c1[1], c2[1], 0.5 * (lo + hi))
}

/// Parameter animatable via keyframes. Always sorted by increasing time.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Keyframed<T> {
    /// If empty, the parameter is constant and must be read from `default`.
    keyframes: Vec<(FrameIdx, T, Interpolation)>,
    pub default: T,
}

impl<T: Clone> Keyframed<T> {
    pub const fn constant(value: T) -> Self {
        Self {
            keyframes: Vec::new(),
            default: value,
        }
    }

    pub fn is_constant(&self) -> bool {
        self.keyframes.is_empty()
    }

    pub fn keyframes(&self) -> &[(FrameIdx, T, Interpolation)] {
        &self.keyframes
    }

    /// Inserts or replaces the keyframe at `frame`, preserving the order.
    pub fn upsert(&mut self, frame: FrameIdx, value: T, interpolation: Interpolation) {
        match self.keyframes.binary_search_by_key(&frame, |(f, _, _)| *f) {
            Ok(idx) => self.keyframes[idx] = (frame, value, interpolation),
            Err(idx) => self.keyframes.insert(idx, (frame, value, interpolation)),
        }
    }

    /// Removes the keyframe exactly at `frame`, if it exists. Returns the
    /// removed value (useful for the undo).
    pub fn remove_at(&mut self, frame: FrameIdx) -> Option<(T, Interpolation)> {
        let idx = self
            .keyframes
            .binary_search_by_key(&frame, |(f, _, _)| *f)
            .ok()?;
        let (_, value, interp) = self.keyframes.remove(idx);
        Some((value, interp))
    }

    /// Frame of the nearest keyframe before `frame`.
    pub fn keyframe_before(&self, frame: FrameIdx) -> Option<FrameIdx> {
        self.keyframes
            .iter()
            .rev()
            .map(|k| k.0)
            .find(|&f| f < frame)
    }

    /// Frame of the nearest keyframe after `frame`.
    pub fn keyframe_after(&self, frame: FrameIdx) -> Option<FrameIdx> {
        self.keyframes.iter().map(|k| k.0).find(|&f| f > frame)
    }

    /// Changes the outgoing interpolation of the keyframe at `frame`, if there is one, and
    /// returns the one it had.
    pub fn set_interpolation(
        &mut self,
        frame: FrameIdx,
        interpolation: Interpolation,
    ) -> Option<Interpolation> {
        let idx = self
            .keyframes
            .binary_search_by_key(&frame, |(f, _, _)| *f)
            .ok()?;
        Some(std::mem::replace(&mut self.keyframes[idx].2, interpolation))
    }

    /// Re-expresses the keyframe times from `from` fps to `to` fps, keeping
    /// their seconds. Keyframes rounding onto the same frame keep the first.
    pub fn rescale_times(&mut self, from: Rational, to: Rational) {
        for keyframe in &mut self.keyframes {
            keyframe.0 = convert_frames(keyframe.0, from, to);
        }
        self.keyframes.dedup_by_key(|k| k.0);
    }

    /// Applies `f` to the default and to every keyframe value.
    pub fn map_values(&mut self, f: impl Fn(&T) -> T) {
        self.default = f(&self.default);
        for keyframe in &mut self.keyframes {
            keyframe.1 = f(&keyframe.1);
        }
    }

    /// Moves every keyframe `delta` frames later.
    pub fn shift(&mut self, delta: FrameIdx) {
        for keyframe in &mut self.keyframes {
            keyframe.0 += delta;
        }
    }

    /// The keyframe exactly at `frame`, if it exists.
    pub fn keyframe_at(&self, frame: FrameIdx) -> Option<(T, Interpolation)> {
        let idx = self
            .keyframes
            .binary_search_by_key(&frame, |(f, _, _)| *f)
            .ok()?;
        let (_, value, interp) = &self.keyframes[idx];
        Some((value.clone(), *interp))
    }
}

/// Component-wise linear interpolation between two values of an
/// animatable parameter. `Keyframed::value_at` needs it to compute
/// the value at an arbitrary frame between two keyframes.
pub trait Lerp {
    fn lerp(a: &Self, b: &Self, t: f32) -> Self;
}

impl Lerp for f32 {
    fn lerp(a: &Self, b: &Self, t: f32) -> Self {
        a + (b - a) * t
    }
}

impl Lerp for Transform {
    fn lerp(a: &Self, b: &Self, t: f32) -> Self {
        let mut crop = [0.0; 4];
        for ((c, ca), cb) in crop.iter_mut().zip(a.crop).zip(b.crop) {
            *c = f32::lerp(&ca, &cb, t);
        }
        Self {
            crop,
            crop_softness: f32::lerp(&a.crop_softness, &b.crop_softness, t),
            opacity: f32::lerp(&a.opacity, &b.opacity, t),
            zoom: [
                f32::lerp(&a.zoom[0], &b.zoom[0], t),
                f32::lerp(&a.zoom[1], &b.zoom[1], t),
            ],
            position: [
                f32::lerp(&a.position[0], &b.position[0], t),
                f32::lerp(&a.position[1], &b.position[1], t),
            ],
            rotation: f32::lerp(&a.rotation, &b.rotation, t),
            anchor: [
                f32::lerp(&a.anchor[0], &b.anchor[0], t),
                f32::lerp(&a.anchor[1], &b.anchor[1], t),
            ],
            // A flip has no middle ground: it snaps at half the interpolation.
            flip: if t < 0.5 { a.flip } else { b.flip },
        }
    }
}

fn smoothstep(t: f32) -> f32 {
    t * t * (3.0 - 2.0 * t)
}

impl<T: Lerp + Clone> Keyframed<T> {
    /// `default` without keyframes; before the first and after the last the extreme
    /// value; in between the interpolation of the starting keyframe.
    pub fn value_at(&self, frame: FrameIdx) -> T {
        if self.keyframes.is_empty() {
            return self.default.clone();
        }
        match self.keyframes.binary_search_by_key(&frame, |(f, _, _)| *f) {
            Ok(idx) => self.keyframes[idx].1.clone(),
            Err(0) => self.keyframes[0].1.clone(),
            Err(idx) if idx == self.keyframes.len() => {
                self.keyframes[self.keyframes.len() - 1].1.clone()
            }
            Err(idx) => {
                let (f0, v0, interp) = &self.keyframes[idx - 1];
                let (f1, v1, _) = &self.keyframes[idx];
                let t = (frame - f0) as f32 / (f1 - f0) as f32;
                if interp == &Interpolation::Hold {
                    v0.clone()
                } else {
                    T::lerp(v0, v1, interp.ease(t))
                }
            }
        }
    }

    /// Discards the keyframes before `start` preserving the value from `start`
    /// on: a joining keyframe remains only if the animation depends on it.
    pub fn drop_before(&mut self, start: FrameIdx)
    where
        T: PartialEq,
    {
        let Some(lead) = self.keyframes.iter().rev().find(|(f, _, _)| *f < start) else {
            return;
        };
        let lead_interp = lead.2;
        let at_start = self.value_at(start);
        self.keyframes.retain(|(f, _, _)| *f >= start);
        if self.keyframes.is_empty() {
            self.default = at_start;
        } else if self.value_at(start) != at_start {
            self.upsert(start, at_start, lead_interp);
        }
    }

    /// Mirror of `drop_before`: keeps only the keyframes before `end`.
    pub fn drop_from(&mut self, end: FrameIdx)
    where
        T: PartialEq,
    {
        if self.keyframes.last().is_none_or(|(f, _, _)| *f < end) {
            return;
        }
        let last = end - 1;
        let at_last = self.value_at(last);
        self.keyframes.retain(|(f, _, _)| *f < end);
        if self.keyframes.is_empty() {
            self.default = at_last;
        } else if self.value_at(last) != at_last {
            self.upsert(last, at_last, Interpolation::Linear);
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Transform {
    /// Pixels cut per side (left, top, right, bottom) at the native
    /// resolution of the media, not of the proxy. The rest is not recentered.
    pub crop: [f32; 4], // left, top, right, bottom
    /// Softness of the crop edge in media pixels: negative towards the inside,
    /// positive towards the outside, 0 hard.
    pub crop_softness: f32,
    /// Magnification per axis around the `anchor`, relative to the output frame.
    pub zoom: [f32; 2],
    /// Displacement in timeline pixels, Y upwards.
    pub position: [f32; 2],
    /// Rotation in degrees, clockwise, around the `anchor`.
    pub rotation: f32,
    /// Zoom and rotation pivot, in timeline pixels from the center
    /// of the clip (`[0, 0]` = its center), with the same directions as
    /// `position` (Y positive upwards).
    pub anchor: [f32; 2],
    /// Horizontal (X) and vertical (Y) mirroring.
    pub flip: [bool; 2],
    /// Layer opacity as a percentage, 0-100.
    pub opacity: f32,
}

impl Default for Transform {
    fn default() -> Self {
        Self {
            crop: [0.0; 4],
            crop_softness: 0.0,
            zoom: [1.0, 1.0],
            position: [0.0, 0.0],
            rotation: 0.0,
            anchor: [0.0, 0.0],
            flip: [false, false],
            opacity: 100.0,
        }
    }
}

/// A transform parameter, each with its own keyframes. `flip` is not one: it
/// does not interpolate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum TransformParam {
    ZoomX,
    ZoomY,
    PositionX,
    PositionY,
    Rotation,
    AnchorX,
    AnchorY,
    CropLeft,
    CropTop,
    CropRight,
    CropBottom,
    CropSoftness,
    Opacity,
}

impl TransformParam {
    pub const ALL: [Self; 13] = [
        Self::ZoomX,
        Self::ZoomY,
        Self::PositionX,
        Self::PositionY,
        Self::Rotation,
        Self::AnchorX,
        Self::AnchorY,
        Self::CropLeft,
        Self::CropTop,
        Self::CropRight,
        Self::CropBottom,
        Self::CropSoftness,
        Self::Opacity,
    ];

    /// Position in `TransformTracks::params` — the order of `ALL`, which is
    /// the declaration order.
    pub fn index(self) -> usize {
        self as usize
    }

    /// The value it has in an already evaluated `Transform`.
    pub fn of(self, t: &Transform) -> f32 {
        match self {
            Self::ZoomX => t.zoom[0],
            Self::ZoomY => t.zoom[1],
            Self::PositionX => t.position[0],
            Self::PositionY => t.position[1],
            Self::Rotation => t.rotation,
            Self::AnchorX => t.anchor[0],
            Self::AnchorY => t.anchor[1],
            Self::CropLeft => t.crop[0],
            Self::CropTop => t.crop[1],
            Self::CropRight => t.crop[2],
            Self::CropBottom => t.crop[3],
            Self::CropSoftness => t.crop_softness,
            Self::Opacity => t.opacity,
        }
    }
}

/// The transform of a clip: one `Keyframed<f32>` per parameter, so every
/// parameter animates on its own, plus the flip (not animatable).
#[derive(Debug, Clone, Serialize)]
pub struct TransformTracks {
    /// One per `TransformParam`, in the order of `TransformParam::ALL`.
    params: Vec<Keyframed<f32>>,
    pub flip: [bool; 2],
}

/// Projects saved before a parameter existed have fewer tracks than
/// `TransformParam::ALL`: the missing tail takes the default value.
impl<'de> Deserialize<'de> for TransformTracks {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        struct Repr {
            params: Vec<Keyframed<f32>>,
            flip: [bool; 2],
        }
        let mut repr = Repr::deserialize(deserializer)?;
        let default = Transform::default();
        for p in TransformParam::ALL.iter().skip(repr.params.len()) {
            repr.params.push(Keyframed::constant(p.of(&default)));
        }
        Ok(Self {
            params: repr.params,
            flip: repr.flip,
        })
    }
}

impl Default for TransformTracks {
    fn default() -> Self {
        Self::constant(Transform::default())
    }
}

impl TransformTracks {
    pub fn constant(t: Transform) -> Self {
        Self {
            params: TransformParam::ALL
                .iter()
                .map(|p| Keyframed::constant(p.of(&t)))
                .collect(),
            flip: t.flip,
        }
    }

    pub fn track(&self, param: TransformParam) -> &Keyframed<f32> {
        &self.params[param.index()]
    }

    pub fn track_mut(&mut self, param: TransformParam) -> &mut Keyframed<f32> {
        &mut self.params[param.index()]
    }

    /// `true` if no parameter has keyframes.
    pub fn is_constant(&self) -> bool {
        self.params.iter().all(|k| k.is_constant())
    }

    /// `true` if the transform is still the default one, keyframes included.
    pub fn is_pristine(&self) -> bool {
        let d = Transform::default();
        self.flip == d.flip
            && TransformParam::ALL
                .iter()
                .all(|p| self.track(*p).is_constant() && self.track(*p).default == p.of(&d))
    }

    pub fn value_at(&self, frame: FrameIdx) -> Transform {
        let v = |p: TransformParam| self.track(p).value_at(frame);
        Transform {
            crop: [
                v(TransformParam::CropLeft),
                v(TransformParam::CropTop),
                v(TransformParam::CropRight),
                v(TransformParam::CropBottom),
            ],
            crop_softness: v(TransformParam::CropSoftness),
            opacity: v(TransformParam::Opacity),
            zoom: [v(TransformParam::ZoomX), v(TransformParam::ZoomY)],
            position: [v(TransformParam::PositionX), v(TransformParam::PositionY)],
            rotation: v(TransformParam::Rotation),
            anchor: [v(TransformParam::AnchorX), v(TransformParam::AnchorY)],
            flip: self.flip,
        }
    }

    /// The keyframe nearest to `frame` *before* it, among those of the
    /// given parameters: used by the panel's navigation arrows.
    pub fn previous_keyframe(
        &self,
        params: &[TransformParam],
        frame: FrameIdx,
    ) -> Option<FrameIdx> {
        params
            .iter()
            .filter_map(|p| self.track(*p).keyframe_before(frame))
            .max()
    }

    /// Mirror of `previous_keyframe`, forwards.
    pub fn next_keyframe(&self, params: &[TransformParam], frame: FrameIdx) -> Option<FrameIdx> {
        params
            .iter()
            .filter_map(|p| self.track(*p).keyframe_after(frame))
            .min()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Rgba {
    pub r: f32,
    pub g: f32,
    pub b: f32,
    pub a: f32,
}

impl Rgba {
    pub const BLACK: Self = Self::gray(0.0);
    pub const WHITE: Self = Self::gray(1.0);

    /// Opaque grey.
    pub const fn gray(v: f32) -> Self {
        Self {
            r: v,
            g: v,
            b: v,
            a: 1.0,
        }
    }
}

impl From<[f32; 4]> for Rgba {
    fn from([r, g, b, a]: [f32; 4]) -> Self {
        Self { r, g, b, a }
    }
}

impl From<Rgba> for [f32; 4] {
    fn from(c: Rgba) -> Self {
        [c.r, c.g, c.b, c.a]
    }
}

impl Lerp for Rgba {
    fn lerp(a: &Self, b: &Self, t: f32) -> Self {
        Self {
            r: f32::lerp(&a.r, &b.r, t),
            g: f32::lerp(&a.g, &b.g, t),
            b: f32::lerp(&a.b, &b.b, t),
            a: f32::lerp(&a.a, &b.a, t),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum TextAlign {
    Left,
    Center,
    Right,
    Justify,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum HAnchor {
    Left,
    Center,
    Right,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum VAnchor {
    Top,
    Middle,
    Bottom,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum FontCase {
    Mixed,
    Upper,
    Lower,
    Title,
}

/// Parameters of a `ClipSource::Text` clip. The measures are in timeline
/// pixels, like those of the `Transform`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TitleParams {
    pub content: String,
    /// Empty = system sans-serif.
    pub font_family: String,
    /// 100-900, as in CSS.
    pub font_weight: u16,
    pub italic: bool,
    pub color: Rgba,
    pub size: f32,
    /// Letter spacing, in thousandths of an em.
    pub tracking: f32,
    /// Extra space between lines, in pixels.
    pub line_spacing: f32,
    pub underline: bool,
    pub strikethrough: bool,
    pub case: FontCase,
    pub align: TextAlign,
    /// Which point of the text block falls on `position`.
    pub anchor: (HAnchor, VAnchor),
    /// From the center of the frame, Y upwards.
    pub position: [f32; 2],
    #[serde(default)]
    pub shadow: TitleShadow,
    #[serde(default)]
    pub background: TitleBackground,
}

/// Shadow of the text alone.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TitleShadow {
    pub enabled: bool,
    pub color: Rgba,
    /// In timeline pixels, Y upwards.
    pub offset: [f32; 2],
    /// Blur radius, in timeline pixels.
    pub blur: f32,
    /// 0-100.
    pub opacity: f32,
}

impl Default for TitleShadow {
    fn default() -> Self {
        Self {
            enabled: false,
            color: Rgba::BLACK,
            offset: [8.0, -8.0],
            blur: 6.0,
            opacity: 75.0,
        }
    }
}

/// Rectangle behind the text.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TitleBackground {
    pub enabled: bool,
    pub color: Rgba,
    pub outline_color: Rgba,
    /// In timeline pixels, towards the inside of the rectangle.
    pub outline_width: f32,
    /// Fraction of the width/height of the frame; 0 = around the text.
    pub width: f32,
    pub height: f32,
    /// Fraction of the shorter side of the rectangle, up to 0.5.
    pub corner_radius: f32,
    /// Displacement from the center of the text, in timeline pixels, Y upwards.
    pub center: [f32; 2],
    /// 0-100.
    pub opacity: f32,
}

impl Default for TitleBackground {
    fn default() -> Self {
        Self {
            enabled: false,
            color: Rgba::BLACK,
            outline_color: Rgba::WHITE,
            outline_width: 0.0,
            width: 0.0,
            height: 0.0,
            corner_radius: 0.1,
            center: [0.0, 0.0],
            opacity: 100.0,
        }
    }
}

impl Default for TitleParams {
    fn default() -> Self {
        Self {
            content: "Title".into(),
            font_family: String::new(),
            font_weight: 400,
            italic: false,
            color: Rgba::WHITE,
            size: 96.0,
            tracking: 0.0,
            line_spacing: 0.0,
            underline: false,
            strikethrough: false,
            case: FontCase::Mixed,
            align: TextAlign::Center,
            anchor: (HAnchor::Center, VAnchor::Middle),
            position: [0.0, 0.0],
            shadow: TitleShadow::default(),
            background: TitleBackground::default(),
        }
    }
}

impl TitleParams {
    /// The text to draw, with `case` already applied.
    pub fn display_text(&self) -> String {
        match self.case {
            FontCase::Mixed => self.content.clone(),
            FontCase::Upper => self.content.to_uppercase(),
            FontCase::Lower => self.content.to_lowercase(),
            FontCase::Title => {
                let mut out = String::with_capacity(self.content.len());
                let mut word_start = true;
                for c in self.content.chars() {
                    if word_start {
                        out.extend(c.to_uppercase());
                    } else {
                        out.extend(c.to_lowercase());
                    }
                    word_start = c.is_whitespace();
                }
                out
            }
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ClipSource {
    Media(MediaId),
    SolidColor,
    /// Parameters in `EffectStack::title`.
    Text,
    /// No content: its effects apply to the composite of the tracks below.
    Adjustment,
}

/// Bounds of `gain_db`: shared between the properties panel slider and the
/// volume line on the timeline, so they always stay a single one.
pub const GAIN_DB_MIN: f32 = -100.0;
pub const GAIN_DB_MAX: f32 = 30.0;

/// A filter of the Effects panel: the variety is open (new variants for
/// new filters), the rendering translates it into a shader id — see
/// `vv_render`, which does not know the meaning of each one, only its id.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum FilterKind {
    Grayscale,
    BoxBlur,
    GaussianBlur,
    Exposure,
}

impl FilterKind {
    /// Whether `ClipFilter::radius` and `direction` apply to it.
    pub fn is_blur(self) -> bool {
        matches!(self, Self::BoxBlur | Self::GaussianBlur)
    }

    /// Whether `ClipFilter::amount` applies to it.
    pub fn has_amount(self) -> bool {
        self == Self::Exposure
    }
}

/// Bounds of `ClipFilter::amount` for `Exposure`, in stops.
pub const EXPOSURE_MIN: f32 = -5.0;
pub const EXPOSURE_MAX: f32 = 5.0;

/// Bounds of `ClipFilter::radius`, in timeline pixels.
pub const BLUR_RADIUS_MAX: f32 = 250.0;
pub const DEFAULT_BLUR_RADIUS: f32 = 10.0;

fn default_blur_radius() -> Keyframed<f32> {
    Keyframed::constant(DEFAULT_BLUR_RADIUS)
}

fn default_blur_direction() -> Keyframed<BlurDirection> {
    Keyframed::constant(BlurDirection::Both)
}

/// The axes a blur spreads along.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum BlurDirection {
    #[default]
    Both,
    Horizontal,
    Vertical,
}

impl BlurDirection {
    pub const ALL: [Self; 3] = [Self::Both, Self::Horizontal, Self::Vertical];

    pub fn horizontal(self) -> bool {
        self != Self::Vertical
    }

    pub fn vertical(self) -> bool {
        self != Self::Horizontal
    }
}

/// No values in between: a keyframe holds until the next one.
impl Lerp for BlurDirection {
    fn lerp(a: &Self, b: &Self, t: f32) -> Self {
        if t >= 1.0 { *b } else { *a }
    }
}

/// A filter applied to a clip. The order in the `Vec` of
/// `EffectStack::filters` is the order of application, configurable
/// by the user (several filters on the same clip, in a sequence they choose);
/// `enabled` suspends it without removing it from the sequence.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ClipFilter {
    pub kind: FilterKind,
    pub enabled: bool,
    /// Blurs only: in timeline pixels, as seen with the clip at zoom 1.
    #[serde(default = "default_blur_radius")]
    pub radius: Keyframed<f32>,
    #[serde(default = "default_blur_direction")]
    pub direction: Keyframed<BlurDirection>,
    /// Exposure only: in stops.
    #[serde(default = "default_filter_amount")]
    pub amount: Keyframed<f32>,
}

fn default_filter_amount() -> Keyframed<f32> {
    Keyframed::constant(0.0)
}

/// A `ClipFilter` evaluated at one frame: what the rendering needs.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FilterValue {
    pub kind: FilterKind,
    pub radius: f32,
    pub direction: BlurDirection,
    pub amount: f32,
}

impl FilterValue {
    pub const fn new(kind: FilterKind) -> Self {
        Self {
            kind,
            radius: DEFAULT_BLUR_RADIUS,
            direction: BlurDirection::Both,
            amount: 0.0,
        }
    }
}

impl ClipFilter {
    pub const fn new(kind: FilterKind) -> Self {
        Self {
            kind,
            enabled: true,
            radius: Keyframed::constant(DEFAULT_BLUR_RADIUS),
            direction: Keyframed::constant(BlurDirection::Both),
            amount: Keyframed::constant(0.0),
        }
    }

    pub fn value_at(&self, frame: FrameIdx) -> FilterValue {
        FilterValue {
            kind: self.kind,
            radius: self.radius.value_at(frame),
            direction: self.direction.value_at(frame),
            amount: self.amount.value_at(frame),
        }
    }
}

/// A transition of the Effects panel, "Transitions" section: like
/// `FilterKind`, an open variety for future transitions. Unlike the
/// filters, it applies only to one edge of the clip (`EffectStack::transition_in`
/// or `transition_out`), not to the whole clip.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TransitionKind {
    Push,
}

/// Slide direction of a Push transition. Independent of the clip edge
/// it is attached to (`In`/`Out`): it describes only the direction of the
/// movement on screen during the transition.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PushDirection {
    Left,
    Right,
    Up,
    Down,
}

impl PushDirection {
    pub const ALL: [Self; 4] = [Self::Left, Self::Right, Self::Up, Self::Down];

    /// Direction of the movement in `Transform.position` units (X, Y): `Y`
    /// positive upwards, like the rest of the transform.
    fn vector(self) -> [f32; 2] {
        match self {
            Self::Left => [-1.0, 0.0],
            Self::Right => [1.0, 0.0],
            Self::Up => [0.0, 1.0],
            Self::Down => [0.0, -1.0],
        }
    }
}

/// What "off screen" really means for a clip zoomed `zoom` times
/// on the axis of `vec` (`direction` is always axial, so only one of the
/// two components matters). In the shader the zoom is applied *after* the
/// position (see `transform.wgsl`): the center of the image moves by
/// `position` on screen whatever the zoom, but its real edge is
/// `zoom` times farther from the center — at `push_clearance == 1` (zoom 1)
/// one unit of push is enough to clear the whole screen, at a higher zoom
/// more is needed or one would still see the inside of the image instead of the
/// transparent underneath for the whole transition. It does not account for
/// anchor/rotation: a case rare enough not to justify the exact
/// computation, here clearing the screen for the zoom (the common case) is enough.
fn push_clearance(vec: [f32; 2], zoom: [f32; 2]) -> f32 {
    let z = vec[0].abs() * zoom[0] + vec[1].abs() * zoom[1];
    0.5 * (z + 1.0)
}

/// Acceleration curve of a transition, applied to the 0..1 progression
/// before translating it into an offset. The same four options as an NLE.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Ease {
    None,
    In,
    Out,
    InOut,
}

impl Ease {
    pub const ALL: [Self; 4] = [Self::None, Self::In, Self::Out, Self::InOut];
}

/// `t` (0..1) reshaped according to `ease`; `curve` (0..1, "Transition Curve"
/// in the inspector) controls its intensity: 0 nearly linear, 1 more
/// pronounced. No claim to match the exact curve of a specific
/// NLE, only a monotonic progression, symmetric in InOut.
fn eased(t: f32, ease: Ease, curve: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    let exponent = 1.0 + curve.clamp(0.0, 1.0) * 4.0;
    match ease {
        Ease::None => t,
        Ease::In => t.powf(exponent),
        Ease::Out => 1.0 - (1.0 - t).powf(exponent),
        Ease::InOut => {
            if t < 0.5 {
                0.5 * (2.0 * t).powf(exponent)
            } else {
                1.0 - 0.5 * (2.0 * (1.0 - t)).powf(exponent)
            }
        }
    }
}

/// A transition applied to one edge of a clip (see
/// `EffectStack::transition_in`/`transition_out`). `duration` in timeline
/// frames from the edge, like `Clip::fade_in`/`fade_out`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Transition {
    pub kind: TransitionKind,
    pub duration: FrameIdx,
    pub direction: PushDirection,
    pub ease: Ease,
    /// "Transition Curve" in the inspector, 0..1.
    pub curve: f32,
}

/// Compositing method of a layer onto those below ("Composite Mode"
/// in the inspector). Separable modes only, computed channel by channel.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum BlendMode {
    #[default]
    Normal,
    Add,
    Multiply,
    Screen,
    Overlay,
    Darken,
    Lighten,
    ColorDodge,
    ColorBurn,
    HardLight,
    SoftLight,
    Difference,
    Exclusion,
    Subtract,
    Divide,
}

impl BlendMode {
    pub const ALL: [Self; 15] = [
        Self::Normal,
        Self::Add,
        Self::Multiply,
        Self::Screen,
        Self::Overlay,
        Self::Darken,
        Self::Lighten,
        Self::ColorDodge,
        Self::ColorBurn,
        Self::HardLight,
        Self::SoftLight,
        Self::Difference,
        Self::Exclusion,
        Self::Subtract,
        Self::Divide,
    ];
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EffectStack {
    pub transform: TransformTracks,
    pub gain_db: Keyframed<f32>,
    pub color: Option<Keyframed<Rgba>>,
    #[serde(default)]
    pub title: Option<TitleParams>,
    #[serde(default)]
    pub filters: Vec<ClipFilter>,
    /// Transition attached to the starting/ending edge of the clip, from the
    /// Effects panel. A pair of fields like `Clip::fade_in`/`fade_out`,
    /// not a map: at most one per edge.
    #[serde(default)]
    pub transition_in: Option<Transition>,
    #[serde(default)]
    pub transition_out: Option<Transition>,
    #[serde(default)]
    pub blend_mode: BlendMode,
}

impl Default for EffectStack {
    fn default() -> Self {
        Self {
            transform: TransformTracks::default(),
            gain_db: Keyframed::constant(0.0),
            color: None,
            title: None,
            filters: Vec::new(),
            transition_in: None,
            transition_out: None,
            blend_mode: BlendMode::default(),
        }
    }
}

impl EffectStack {
    /// See `Keyframed::drop_before`, on every animatable parameter.
    pub fn drop_keyframes_before(&mut self, start: FrameIdx) {
        self.for_each_f32_track(|k| k.drop_before(start));
        if let Some(c) = &mut self.color {
            c.drop_before(start);
        }
        for f in &mut self.filters {
            f.direction.drop_before(start);
        }
    }

    /// See `Keyframed::drop_from`, on every animatable parameter.
    pub fn drop_keyframes_from(&mut self, end: FrameIdx) {
        self.for_each_f32_track(|k| k.drop_from(end));
        if let Some(c) = &mut self.color {
            c.drop_from(end);
        }
        for f in &mut self.filters {
            f.direction.drop_from(end);
        }
    }

    /// See `Keyframed::shift`, on every animatable parameter.
    pub fn shift_keyframes(&mut self, delta: FrameIdx) {
        self.for_each_f32_track(|k| k.shift(delta));
        if let Some(c) = &mut self.color {
            c.shift(delta);
        }
        for f in &mut self.filters {
            f.direction.shift(delta);
        }
    }

    /// The crop is in source pixels: same cut for a source of another
    /// resolution. The softness follows the mean of the two scales.
    pub fn rescale_crop(&mut self, scale_x: f32, scale_y: f32) {
        let scales = [
            (TransformParam::CropLeft, scale_x),
            (TransformParam::CropRight, scale_x),
            (TransformParam::CropTop, scale_y),
            (TransformParam::CropBottom, scale_y),
            (TransformParam::CropSoftness, (scale_x * scale_y).sqrt()),
        ];
        for (param, scale) in scales {
            self.transform.track_mut(param).map_values(|v| v * scale);
        }
    }

    /// See `Keyframed::rescale_times`, on every animatable parameter.
    pub fn rescale_keyframe_times(&mut self, from: Rational, to: Rational) {
        self.for_each_f32_track(|k| k.rescale_times(from, to));
        if let Some(c) = &mut self.color {
            c.rescale_times(from, to);
        }
        for f in &mut self.filters {
            f.direction.rescale_times(from, to);
        }
    }

    fn for_each_f32_track(&mut self, mut f: impl FnMut(&mut Keyframed<f32>)) {
        for p in TransformParam::ALL {
            f(self.transform.track_mut(p));
        }
        f(&mut self.gain_db);
        for filter in &mut self.filters {
            f(&mut filter.radius);
            f(&mut filter.amount);
        }
    }

    /// `true` if no property was touched relative to the default: the
    /// timeline draws the clips for which it is `false` darker. The title does not
    /// count: it is the content of the clip, not an effect.
    pub fn is_pristine(&self) -> bool {
        self.transform.is_pristine()
            && self.gain_db.is_constant()
            && self.gain_db.default == 0.0
            && self.color.is_none()
            && self.blend_mode == BlendMode::Normal
    }
}

/// Hues picked in OKLCH at near-constant lightness and chroma, plus two
/// neutrals.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ClipColor {
    Red,
    Orange,
    Yellow,
    Green,
    Cyan,
    Blue,
    Indigo,
    Purple,
    Magenta,
    Rose,
    Slate,
    Gray,
}

/// A color name this version does not know (older palettes) loads as no
/// color instead of failing the whole file.
pub(crate) fn lenient_clip_color<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<ClipColor>, D::Error> {
    use serde::de::{EnumAccess, VariantAccess, Visitor};
    struct Lenient;
    impl<'de> Visitor<'de> for Lenient {
        type Value = Option<ClipColor>;
        fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
            f.write_str("a clip color")
        }
        fn visit_none<E>(self) -> Result<Self::Value, E> {
            Ok(None)
        }
        fn visit_unit<E>(self) -> Result<Self::Value, E> {
            Ok(None)
        }
        fn visit_some<D: serde::Deserializer<'de>>(self, d: D) -> Result<Self::Value, D::Error> {
            d.deserialize_enum("ClipColor", &[], self)
        }
        fn visit_str<E>(self, name: &str) -> Result<Self::Value, E> {
            Ok(ClipColor::from_name(name))
        }
        fn visit_enum<A: EnumAccess<'de>>(self, data: A) -> Result<Self::Value, A::Error> {
            let (Name(name), variant) = data.variant()?;
            variant.unit_variant()?;
            Ok(ClipColor::from_name(&name))
        }
    }
    /// RON hands variant names over only as identifiers, not as strings.
    struct Name(String);
    impl<'de> serde::Deserialize<'de> for Name {
        fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
            struct NameVisitor;
            impl Visitor<'_> for NameVisitor {
                type Value = Name;
                fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                    f.write_str("a color name")
                }
                fn visit_str<E>(self, name: &str) -> Result<Name, E> {
                    Ok(Name(name.to_owned()))
                }
            }
            d.deserialize_identifier(NameVisitor)
        }
    }
    deserializer.deserialize_option(Lenient)
}

impl ClipColor {
    pub const ALL: [ClipColor; 12] = [
        ClipColor::Red,
        ClipColor::Orange,
        ClipColor::Yellow,
        ClipColor::Green,
        ClipColor::Cyan,
        ClipColor::Blue,
        ClipColor::Indigo,
        ClipColor::Purple,
        ClipColor::Magenta,
        ClipColor::Rose,
        ClipColor::Slate,
        ClipColor::Gray,
    ];

    fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|c| format!("{c:?}") == name)
    }

    pub fn rgb(self) -> (u8, u8, u8) {
        match self {
            ClipColor::Red => (228, 124, 117),
            ClipColor::Orange => (218, 136, 68),
            ClipColor::Yellow => (216, 189, 81),
            ClipColor::Green => (103, 179, 106),
            ClipColor::Cyan => (43, 179, 185),
            ClipColor::Blue => (90, 163, 236),
            ClipColor::Indigo => (117, 124, 211),
            ClipColor::Purple => (177, 136, 223),
            ClipColor::Magenta => (211, 125, 184),
            ClipColor::Rose => (243, 175, 184),
            ClipColor::Slate => (114, 130, 149),
            ClipColor::Gray => (146, 146, 146),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Clip {
    pub id: ClipId,
    pub source: ClipSource,
    /// Start in the media in *timeline* frames from source frame 0: a conformed
    /// clip can start in the middle of a source frame.
    pub source_offset: FrameIdx,
    /// Position in the space of the Timeline containing this clip.
    pub timeline_start: FrameIdx,
    pub timeline_len: FrameIdx,
    pub effects: EffectStack,
    /// Linked group (usually the video and all the audio streams of one
    /// import): selection, drag and deletion treat it as a unit.
    #[serde(default)]
    pub linked_group: Option<LinkGroupId>,
    /// Audio clip: which stream of the container (order of
    /// `vv_media::audio_streams`).
    #[serde(default)]
    pub audio_stream_index: usize,
    /// Timeline frames per source frame: `Rational::conform_rate` divided
    /// by `speed`. Private, like `speed`, so the two cannot drift apart:
    /// written only through `conform`.
    #[serde(default = "Rational::one")]
    rate: Rational,
    /// Source time per timeline time: `2/1` plays the media twice as fast.
    /// Only on `Media` clips.
    #[serde(default = "Rational::one")]
    speed: Rational,
    /// Audio of a clip with `speed != 1`: pitch preserved (time-stretch)
    /// instead of following the speed.
    #[serde(default = "pitch_correction_default")]
    pub pitch_correction: bool,
    /// Excluded from compositing and mixing, but stays on the timeline.
    #[serde(default)]
    pub disabled: bool,
    /// Duration of the fade in, in timeline frames from the start
    /// of the clip. 0 = none.
    #[serde(default)]
    pub fade_in: FrameIdx,
    /// Duration of the fade out, in timeline frames from the end
    /// of the clip. 0 = none.
    #[serde(default)]
    pub fade_out: FrameIdx,
    /// Hand-picked timeline color; `None` = the one derived from the source kind.
    #[serde(default, deserialize_with = "lenient_clip_color")]
    pub display_color: Option<ClipColor>,
}

fn pitch_correction_default() -> bool {
    true
}

impl Clip {
    /// Clip showing `source_in..source_out()` of the source starting from
    /// `timeline_start`, without effects or links.
    pub fn from_source_range(
        id: ClipId,
        source: ClipSource,
        source_in: FrameIdx,
        source_out: FrameIdx,
        timeline_start: FrameIdx,
        rate: Rational,
    ) -> Self {
        let source_offset = rate.scale_round(source_in);
        Self {
            id,
            source,
            source_offset,
            timeline_start,
            timeline_len: rate.scale_round(source_out) - source_offset,
            effects: EffectStack::default(),
            linked_group: None,
            audio_stream_index: 0,
            rate,
            speed: Rational::one(),
            pitch_correction: pitch_correction_default(),
            disabled: false,
            fade_in: 0,
            fade_out: 0,
            display_color: None,
        }
    }

    /// A clip at `speed` with its timeline geometry already computed, without
    /// effects or links.
    pub fn new(
        id: ClipId,
        source: ClipSource,
        source_offset: FrameIdx,
        timeline_start: FrameIdx,
        timeline_len: FrameIdx,
        conform_rate: Rational,
        speed: Rational,
    ) -> Self {
        let mut clip = Self::from_source_range(id, source, 0, 0, timeline_start, conform_rate);
        clip.source_offset = source_offset;
        clip.timeline_len = timeline_len;
        clip.speed = speed;
        clip.conform(conform_rate);
        clip
    }

    pub fn rate(&self) -> Rational {
        self.rate
    }

    pub fn speed(&self) -> Rational {
        self.speed
    }

    /// `rate` without the speed: timeline fps against source fps.
    pub fn conform_rate(&self) -> Rational {
        Rational::reduced(
            self.rate.num as i64 * self.speed.num as i64,
            self.rate.den as i64 * self.speed.den as i64,
        )
    }

    /// Solid color, title or adjustment: no media bounds its length.
    pub fn is_generator(&self) -> bool {
        !matches!(self.source, ClipSource::Media(_))
    }

    pub fn is_adjustment(&self) -> bool {
        matches!(self.source, ClipSource::Adjustment)
    }

    /// `rate` for `conform_rate` (timeline fps against media fps) at this
    /// clip's `speed`.
    pub fn conform(&mut self, conform_rate: Rational) {
        self.rate = conform_rate.divided_by(self.speed);
    }

    /// New `speed` keeping `source_in` and `timeline_start`: the length
    /// follows it.
    pub fn set_speed(&mut self, speed: Rational, conform_rate: Rational) {
        let (source_in, source_out) = (self.source_in(), self.source_out());
        self.speed = speed;
        self.conform(conform_rate);
        self.source_offset = self.rate.scale_round(source_in);
        self.timeline_len = (self.rate.scale_round(source_out) - self.source_offset).max(1);
        self.fade_in = self.fade_in.min(self.timeline_len);
        self.fade_out = self.fade_out.min(self.timeline_len);
    }

    /// First source frame shown.
    pub fn source_in(&self) -> FrameIdx {
        self.rate.unscale_round(self.source_offset)
    }

    /// Exclusive: the last source frame shown, plus one.
    pub fn source_out(&self) -> FrameIdx {
        self.rate
            .unscale_round(self.source_offset + self.timeline_len - 1)
            + 1
    }

    /// Duration in *source* frames, for the computations living in that
    /// space (how many frames of the media need decoding).
    pub fn source_len(&self) -> FrameIdx {
        self.source_out() - self.source_in()
    }

    pub fn timeline_end(&self) -> FrameIdx {
        self.timeline_start + self.timeline_len
    }

    /// `frame` (of the timeline) falls inside the clip.
    pub fn contains(&self, frame: FrameIdx) -> bool {
        frame >= self.timeline_start && frame < self.timeline_end()
    }

    /// Source frame shown at timeline position `timeline_frame`
    /// (inside the clip). The single point of the mapping, shared by preview
    /// and export: a future time-remap must be applied here.
    pub fn source_frame_at(&self, timeline_frame: FrameIdx) -> FrameIdx {
        self.rate
            .unscale_round(timeline_frame - self.timeline_start + self.source_offset)
    }

    /// Inverse of `source_frame_at`, even outside the trim (needed by the trim
    /// limits).
    pub fn timeline_frame_at(&self, source_frame: FrameIdx) -> FrameIdx {
        self.timeline_start + self.rate.scale_round(source_frame) - self.source_offset
    }

    /// Seconds from the start of the media at `timeline_frame`, without going through the
    /// source frames: the audio follows the cut to the sample.
    pub fn media_secs_at(&self, timeline_frame: FrameIdx, timeline_fps: f64) -> f64 {
        (timeline_frame - self.timeline_start + self.source_offset) as f64 * self.speed.as_f64()
            / timeline_fps
    }

    /// Moves the clip from a timeline at `from` fps to one at `to` fps,
    /// preserving the seconds; `conform_rate` is the one of its source on
    /// the new timeline.
    pub fn retime(&mut self, from: Rational, to: Rational, conform_rate: Rational) {
        let end = convert_frames(self.timeline_end(), from, to);
        self.timeline_start = convert_frames(self.timeline_start, from, to);
        self.timeline_len = (end - self.timeline_start).max(1);
        self.source_offset = convert_frames(self.source_offset, from, to);
        self.conform(conform_rate);
    }

    /// Opacity/volume multiplier at `timeline_frame` for the
    /// fades: linear ramp 0→1 over `fade_in` frames from the start,
    /// ramp 1→0 over `fade_out` frames from the end, product of the two (if they
    /// overlap they subtract from each other, as in any NLE).
    pub fn fade_multiplier_at(&self, timeline_frame: FrameIdx) -> f32 {
        let len = self.timeline_len.max(1);
        let fade_in = self.fade_in.clamp(0, len);
        let fade_out = self.fade_out.clamp(0, len);
        let pos = (timeline_frame - self.timeline_start).clamp(0, len);
        let in_ramp = if fade_in > 0 {
            (pos as f32 / fade_in as f32).clamp(0.0, 1.0)
        } else {
            1.0
        };
        let out_ramp = if fade_out > 0 {
            ((len - pos) as f32 / fade_out as f32).clamp(0.0, 1.0)
        } else {
            1.0
        };
        in_ramp * out_ramp
    }

    /// Position offset (same pixel units as `Transform.position`,
    /// `frame_size` = resolution of the timeline) due to the
    /// push transitions in/out at `timeline_frame`. Added to the `position` already
    /// sampled from the transform keyframes, it does not replace it: this way a
    /// push transition coexists with a manual pan on the same clip. Outside
    /// the transition window the offset is zero; at its extremes it
    /// always coincides with "off screen" or "in place", independently
    /// of `direction`/`ease`, because the release gives alpha 0 outside the edges
    /// of the source_uv (see `transform.wgsl`): using it also to reveal
    /// what is below (lower third, overlay) is the same mechanism.
    /// `zoom` is the already sampled one of `effects.transform` at the same
    /// frame (see `push_clearance`): without it, a zoomed clip would be seen
    /// popping in halfway through the transition instead of sliding in from off
    /// screen, because its real edge stays past the displacement
    /// computed for zoom 1.
    pub fn transition_offset_at(
        &self,
        timeline_frame: FrameIdx,
        frame_size: (f32, f32),
        zoom: [f32; 2],
    ) -> [f32; 2] {
        let len = self.timeline_len.max(1);
        let pos = timeline_frame - self.timeline_start;
        let mut offset = [0.0f32; 2];
        if self.is_adjustment() {
            return offset;
        }
        if let Some(t) = &self.effects.transition_in {
            let d = t.duration.clamp(1, len);
            if pos >= 0 && pos < d {
                let progress = eased(pos as f32 / d as f32, t.ease, t.curve);
                let vec = t.direction.vector();
                let amount = -(1.0 - progress) * push_clearance(vec, zoom);
                offset[0] += vec[0] * frame_size.0 * amount;
                offset[1] += vec[1] * frame_size.1 * amount;
            }
        }
        if let Some(t) = &self.effects.transition_out {
            let d = t.duration.clamp(1, len);
            let from_end = len - pos;
            if from_end > 0 && from_end <= d {
                let progress = eased(1.0 - from_end as f32 / d as f32, t.ease, t.curve);
                let vec = t.direction.vector();
                let amount = progress * push_clearance(vec, zoom);
                offset[0] += vec[0] * frame_size.0 * amount;
                offset[1] += vec[1] * frame_size.1 * amount;
            }
        }
        offset
    }
}

/// The attribute part of a clip: everything the "paste attributes" of
/// the NLEs copies, i.e. all but source, position and links.
#[derive(Debug, Clone)]
pub struct ClipAttributes {
    pub effects: EffectStack,
    pub fade_in: FrameIdx,
    pub fade_out: FrameIdx,
}

impl ClipAttributes {
    pub fn of(clip: &Clip) -> Self {
        Self {
            effects: clip.effects.clone(),
            fade_in: clip.fade_in,
            fade_out: clip.fade_out,
        }
    }

    pub fn apply_to(self, clip: &mut Clip) {
        clip.effects = self.effects;
        clip.fade_in = self.fade_in;
        clip.fade_out = self.fade_out;
    }
}

/// `frames` at `from` fps expressed at `to` fps, rounded.
pub fn convert_frames(frames: FrameIdx, from: Rational, to: Rational) -> FrameIdx {
    if from == to || from.num <= 0 || to.den <= 0 {
        return frames;
    }
    let num = frames as i128 * to.num as i128 * from.den as i128;
    let den = to.den as i128 * from.num as i128;
    (2 * num + den).div_euclid(2 * den) as FrameIdx
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum TrackKind {
    Video,
    Audio,
}

/// Mixer settings of an audio track or of the master bus.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ChannelStrip {
    pub gain_db: f32,
    /// Left/right balance, -1 (left) to 1 (right).
    pub pan: f32,
    /// Insert chain before the fader, in order of application.
    pub effects: Vec<AudioEffect>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AudioEffect {
    pub enabled: bool,
    pub kind: AudioEffectKind,
}

impl AudioEffect {
    pub fn new(kind: AudioEffectKind) -> Self {
        Self {
            enabled: true,
            kind,
        }
    }
}

/// An audio effect of the mixer. The variety is open: new variants for new
/// effects.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum AudioEffectKind {
    /// Gain that brings the level of the channel to `target_db`: dBFS for
    /// the peak modes, LUFS for loudness.
    Normalize {
        target_db: f32,
        #[serde(default)]
        mode: NormalizeMode,
        #[serde(default)]
        set_level: SetLevel,
    },
    MultibandCompressor(MultibandCompressor),
    /// Every channel gets their average: a voice recorded on one side
    /// comes out of both.
    Mono,
    Equalizer(Equalizer),
}

impl AudioEffectKind {
    pub const ALL: [AudioEffectKind; 4] = [
        AudioEffectKind::Normalize {
            target_db: NORMALIZE_TARGET_DEFAULT,
            mode: NormalizeMode::SamplePeak,
            set_level: SetLevel::Relative,
        },
        AudioEffectKind::MultibandCompressor(MultibandCompressor::DEFAULT),
        AudioEffectKind::Equalizer(Equalizer::DEFAULT),
        AudioEffectKind::Mono,
    ];
}

/// Parametric equalizer: filters in series, one per band.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Equalizer {
    pub bands: [EqBand; 6],
}

impl Equalizer {
    /// Low and high cut off, the four in between flat.
    pub const DEFAULT: Self = Self {
        bands: [
            EqBand::new(EqShape::HighPass, 80.0, false),
            EqBand::new(EqShape::LowShelf, 120.0, true),
            EqBand::new(EqShape::Peak, 500.0, true),
            EqBand::new(EqShape::Peak, 2500.0, true),
            EqBand::new(EqShape::HighShelf, 8000.0, true),
            EqBand::new(EqShape::LowPass, 18_000.0, false),
        ],
    };
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum EqShape {
    HighPass,
    LowShelf,
    Peak,
    HighShelf,
    LowPass,
}

impl EqShape {
    pub const ALL: [EqShape; 5] = [
        EqShape::HighPass,
        EqShape::LowShelf,
        EqShape::Peak,
        EqShape::HighShelf,
        EqShape::LowPass,
    ];

    /// The cuts have no gain: `q` is their resonance.
    pub fn has_gain(self) -> bool {
        !matches!(self, EqShape::HighPass | EqShape::LowPass)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct EqBand {
    pub enabled: bool,
    pub shape: EqShape,
    pub freq_hz: f32,
    pub gain_db: f32,
    pub q: f32,
}

impl EqBand {
    pub const FREQ_RANGE_HZ: (f32, f32) = (20.0, 20_000.0);
    pub const GAIN_RANGE_DB: (f32, f32) = (-24.0, 24.0);
    pub const Q_RANGE: (f32, f32) = (0.1, 18.0);
    pub const Q_DEFAULT: f32 = std::f32::consts::FRAC_1_SQRT_2;

    pub const fn new(shape: EqShape, freq_hz: f32, enabled: bool) -> Self {
        Self {
            enabled,
            shape,
            freq_hz,
            gain_db: 0.0,
            q: Self::Q_DEFAULT,
        }
    }
}

/// Three bands split at `crossovers_hz`, each with its own compressor.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MultibandCompressor {
    /// Low/mid and mid/high, ascending.
    pub crossovers_hz: [f32; 2],
    /// Low, mid, high.
    pub bands: [CompressorBand; 3],
}

impl MultibandCompressor {
    pub const DEFAULT: Self = Self {
        crossovers_hz: [200.0, 2000.0],
        bands: [CompressorBand::DEFAULT; 3],
    };
    pub const CROSSOVER_RANGE_HZ: (f32, f32) = (20.0, 20_000.0);
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct CompressorBand {
    pub threshold_db: f32,
    pub ratio: f32,
    pub attack_ms: f32,
    pub release_ms: f32,
    pub makeup_db: f32,
}

impl CompressorBand {
    pub const DEFAULT: Self = Self {
        threshold_db: -24.0,
        ratio: 3.0,
        attack_ms: 10.0,
        release_ms: 150.0,
        makeup_db: 0.0,
    };
    pub const THRESHOLD_RANGE_DB: (f32, f32) = (-60.0, 0.0);
    pub const RATIO_RANGE: (f32, f32) = (1.0, 20.0);
    pub const ATTACK_RANGE_MS: (f32, f32) = (0.1, 200.0);
    pub const RELEASE_RANGE_MS: (f32, f32) = (5.0, 2000.0);
    pub const MAKEUP_RANGE_DB: (f32, f32) = (-12.0, 24.0);
}

pub const NORMALIZE_TARGET_DEFAULT: f32 = -1.0;

/// How a normalization measures the level.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum NormalizeMode {
    #[default]
    SamplePeak,
    /// Also the peaks between the samples (ITU-R BS.1770 Annex 2), which
    /// a conversion or a lossy encode can bring out.
    TruePeak,
    /// Integrated loudness, EBU R128 / ITU-R BS.1770: K-weighted and gated.
    Loudness,
}

impl NormalizeMode {
    pub const ALL: [NormalizeMode; 3] = [
        NormalizeMode::SamplePeak,
        NormalizeMode::TruePeak,
        NormalizeMode::Loudness,
    ];

    pub fn is_loudness(self) -> bool {
        self == NormalizeMode::Loudness
    }

    /// dBFS for the peaks, LUFS for loudness.
    pub fn target_range(self) -> std::ops::RangeInclusive<f32> {
        if self.is_loudness() {
            -40.0..=-5.0
        } else {
            -30.0..=0.0
        }
    }

    pub fn default_target(self) -> f32 {
        if self.is_loudness() {
            -23.0
        } else {
            NORMALIZE_TARGET_DEFAULT
        }
    }
}

/// How the clips of a track share a normalization.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum SetLevel {
    /// Each clip gets its own gain: all of them reach the target.
    Independent,
    /// One gain for all of them, so the loudest one reaches the target and
    /// the differences between them stay.
    #[default]
    Relative,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Track {
    pub kind: TrackKind,
    /// Always sorted by `timeline_start`, never overlapping.
    pub clips: Vec<Clip>,
    /// On a video track: excluded from compositing.
    pub muted: bool,
    /// Audio only: if at least one track is soloed, only those play.
    #[serde(default)]
    pub solo: bool,
    /// Its clips cannot be selected or edited, and nothing can
    /// land on it.
    #[serde(default)]
    pub locked: bool,
    /// Audio only: armed for recording, a take lands on it.
    #[serde(default)]
    pub armed: bool,
    /// Transitions straddling two adjacent clips. It never touches
    /// `timeline_start`/`timeline_len` of the clips involved (they stay non-
    /// overlapping, invariant intact): it is the rendering that "lends" for
    /// its window the tail of one and the head of the other, see
    /// `Track::crossing_at`. Whoever splits a clip, deletes it or moves it to
    /// another track must call `take_crossings_for` on its id,
    /// otherwise the entry stays in the data as a dangling reference (see
    /// `crossing_from`/`crossing_into`, which unlike `crossing_at`
    /// do not check that the clips still exist); moving it on the same
    /// track does not, it simply stays inert until it becomes
    /// adjacent again (`SplitClip`, `LiftDelete`, `MoveClips` in `command.rs`
    /// are the places to imitate for a new command touching the clips).
    #[serde(default)]
    pub crossings: Vec<CrossTransition>,
    /// Audio only.
    #[serde(default)]
    pub mix: ChannelStrip,
}

impl Track {
    pub fn new(kind: TrackKind) -> Self {
        Self {
            kind,
            clips: Vec::new(),
            muted: false,
            solo: false,
            locked: false,
            armed: false,
            crossings: Vec::new(),
            mix: ChannelStrip::default(),
        }
    }

    pub fn clip(&self, id: ClipId) -> Option<&Clip> {
        self.clips.iter().find(|c| c.id == id)
    }

    pub fn clip_mut(&mut self, id: ClipId) -> Option<&mut Clip> {
        self.clips.iter_mut().find(|c| c.id == id)
    }

    pub fn remove_clip(&mut self, id: ClipId) -> Option<Clip> {
        let pos = self.clips.iter().position(|c| c.id == id)?;
        Some(self.clips.remove(pos))
    }

    /// Inserts preserving the order by `timeline_start`.
    pub fn insert_sorted(&mut self, clip: Clip) {
        let pos = self
            .clips
            .partition_point(|c| c.timeline_start < clip.timeline_start);
        self.clips.insert(pos, clip);
    }

    /// The crossing transition whose pair is still valid (both
    /// clips exist and are still adjacent) and whose window covers
    /// `frame`, with the two clips involved.
    pub fn crossing_at(&self, frame: FrameIdx) -> Option<(&Clip, &Clip, &CrossTransition)> {
        self.crossings.iter().find_map(|c| {
            let left = self.clip(c.left_clip)?;
            let right = self.clip(c.right_clip)?;
            // An adjustment half would adjust a stack already holding the other half.
            if left.timeline_end() != right.timeline_start
                || left.is_adjustment()
                || right.is_adjustment()
            {
                return None;
            }
            c.window(left, right)
                .contains(&frame)
                .then_some((left, right, c))
        })
    }

    /// The crossing transition (valid or not) whose `left_clip` is `id`.
    pub fn crossing_from(&self, left_clip: ClipId) -> Option<&CrossTransition> {
        self.crossings.iter().find(|c| c.left_clip == left_clip)
    }

    /// The crossing transition (valid or not) whose `right_clip` is `id`.
    pub fn crossing_into(&self, right_clip: ClipId) -> Option<&CrossTransition> {
        self.crossings.iter().find(|c| c.right_clip == right_clip)
    }

    /// Removes and returns all the crossing transitions (of any
    /// kind, present or future: it does not filter on `transition.kind`)
    /// involving `id` as `left_clip` or `right_clip`. To be called from
    /// every command that splits or deletes a clip, so as not to leave
    /// dangling references in `crossings`.
    pub fn take_crossings_for(&mut self, id: ClipId) -> Vec<CrossTransition> {
        let mut removed = Vec::new();
        self.crossings.retain(|c| {
            if c.left_clip == id || c.right_clip == id {
                removed.push(c.clone());
                false
            } else {
                true
            }
        });
        removed
    }
}

/// A transition straddling two adjacent clips on the same track:
/// it "eats" the last `duration/2` frames of `left_clip` and the first
/// `duration/2` of `right_clip`, showing them overlapped instead of in
/// sequence. Unlike `EffectStack::transition_in`/`transition_out`
/// (one edge only, against transparency) here the sides are always two and
/// real: neither clip changes `timeline_start`/`timeline_len`,
/// the window is always computed from their current position.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CrossTransition {
    pub left_clip: ClipId,
    pub right_clip: ClipId,
    pub transition: Transition,
}

impl CrossTransition {
    /// How many frames of the window fall before the cut (inside
    /// `left_clip`) and how many after (inside `right_clip`): always halved,
    /// the odd frame, if any, goes to the left side. Dragging the duration
    /// is symmetric (see timeline_ui), so the rounding choice
    /// matters only for odd durations.
    pub fn split(&self) -> (FrameIdx, FrameIdx) {
        Self::split_duration(self.transition.duration)
    }

    /// Like `split`, but for a given duration instead of `self.transition.duration`
    /// — needed by the live preview of a resize drag, where
    /// the shown duration is not the saved one yet.
    pub fn split_duration(duration: FrameIdx) -> (FrameIdx, FrameIdx) {
        let left = duration - duration / 2;
        (left, duration - left)
    }

    /// The window (in timeline frames) of the transition, given the current
    /// edges of `left`/`right` — never stored, always recomputed: if
    /// a clip moves the window follows it.
    pub fn window(&self, left: &Clip, right: &Clip) -> std::ops::Range<FrameIdx> {
        let (split_left, split_right) = self.split();
        (left.timeline_end() - split_left)..(right.timeline_start + split_right)
    }

    /// Progress 0..1 of `frame` in the window: 0 at the start (left
    /// still entirely in place, right entirely out), 1 at the end
    /// (the opposite). It already applies `ease`/`curve`.
    pub fn eased_progress_at(&self, frame: FrameIdx, left: &Clip, right: &Clip) -> f32 {
        let window = self.window(left, right);
        let len = (window.end - window.start).max(1);
        let raw = (frame - window.start) as f32 / len as f32;
        eased(
            raw.clamp(0.0, 1.0),
            self.transition.ease,
            self.transition.curve,
        )
    }

    /// Position offset (same pixel units as `Transform.position`) of the
    /// left and right sides at the already "eased" progress `progress`: the
    /// left one exits, the right one enters, in the same direction — same
    /// mathematics as `Clip::transition_offset_at`, applied here to a
    /// progress shared by the whole window instead of local to the edge
    /// of a single clip.
    /// `left_zoom`/`right_zoom` are the already sampled ones of the transform of
    /// each clip at the same frame (see `push_clearance` and the docs of
    /// `Clip::transition_offset_at`): each can have its own, the push
    /// of the more zoomed one must reach farther to really
    /// clear the screen.
    pub fn offsets(
        &self,
        progress: f32,
        frame_size: (f32, f32),
        left_zoom: [f32; 2],
        right_zoom: [f32; 2],
    ) -> ([f32; 2], [f32; 2]) {
        let vec = self.transition.direction.vector();
        let left_amount = progress * push_clearance(vec, left_zoom);
        let right_amount = -(1.0 - progress) * push_clearance(vec, right_zoom);
        let left = [
            vec[0] * frame_size.0 * left_amount,
            vec[1] * frame_size.1 * left_amount,
        ];
        let right = [
            vec[0] * frame_size.0 * right_amount,
            vec[1] * frame_size.1 * right_amount,
        ];
        (left, right)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Timeline {
    pub name: String,
    pub fps: Rational,
    pub resolution: (u32, u32),
    /// Compositing order: bottom -> top.
    pub tracks: Vec<Track>,
    /// Sorted by `start`.
    #[serde(default)]
    pub markers: Vec<Marker>,
    /// Applied to the sum of the audio tracks.
    #[serde(default)]
    pub master: ChannelStrip,
}

impl Timeline {
    pub fn marker(&self, id: MarkerId) -> Option<&Marker> {
        self.markers.iter().find(|m| m.id == id)
    }

    pub fn alloc_marker_id(&self) -> MarkerId {
        MarkerId(self.markers.iter().map(|m| m.id.0 + 1).max().unwrap_or(0))
    }

    pub fn clip(&self, track_index: usize, id: ClipId) -> Option<&Clip> {
        self.tracks.get(track_index)?.clip(id)
    }

    pub fn clip_mut(&mut self, track_index: usize, id: ClipId) -> Option<&mut Clip> {
        self.tracks.get_mut(track_index)?.clip_mut(id)
    }

    /// Last frame (exclusive) covered by any clip of the
    /// timeline, on any track.
    pub fn total_frames(&self) -> FrameIdx {
        self.tracks
            .iter()
            .flat_map(|t| t.clips.iter())
            .map(Clip::timeline_end)
            .max()
            .unwrap_or(0)
    }

    /// The `meta` of the pool item that uses this timeline as a clip.
    pub fn compound_meta(&self) -> MediaMeta {
        let has_clips = |kind| self.tracks_of_kind(kind).any(|(_, t)| !t.clips.is_empty());
        MediaMeta {
            duration_frames: self.total_frames(),
            fps: self.fps,
            width: self.resolution.0,
            height: self.resolution.1,
            has_video: has_clips(TrackKind::Video),
            has_audio: has_clips(TrackKind::Audio),
            sample_rate: 48_000,
            channels: 2,
            audio_streams: 1,
            file: Default::default(),
        }
    }

    /// Track name as in an NLE: V1, V2… A1, A2…, per kind.
    pub fn track_label(&self, track_index: usize) -> String {
        let number = self.track_number(track_index);
        match self.tracks[track_index].kind {
            TrackKind::Video => format!("V{number}"),
            TrackKind::Audio => format!("A{number}"),
        }
    }

    /// Position of the track among those of its kind, from 1 (the V/A of
    /// `track_label`).
    pub fn track_number(&self, track_index: usize) -> usize {
        let kind = self.tracks[track_index].kind;
        self.tracks[..=track_index]
            .iter()
            .filter(|t| t.kind == kind)
            .count()
    }

    /// Absolute index of the `number`-th track (from 1) of kind `kind`.
    pub fn track_of_kind_numbered(&self, kind: TrackKind, number: usize) -> Option<usize> {
        self.tracks_of_kind(kind)
            .map(|(i, _)| i)
            .nth(number.checked_sub(1)?)
    }

    /// The tracks of kind `kind`, with their absolute index in `tracks`.
    pub fn tracks_of_kind(
        &self,
        kind: TrackKind,
    ) -> impl DoubleEndedIterator<Item = (usize, &Track)> {
        self.tracks
            .iter()
            .enumerate()
            .filter(move |(_, t)| t.kind == kind)
    }

    /// First track of kind `kind`.
    pub fn first_track_index(&self, kind: TrackKind) -> Option<usize> {
        self.tracks_of_kind(kind).map(|(i, _)| i).next()
    }

    /// The video tracks that get composited: not muted.
    pub fn visible_video_tracks(&self) -> impl DoubleEndedIterator<Item = (usize, &Track)> {
        self.tracks_of_kind(TrackKind::Video)
            .filter(|(_, t)| !t.muted)
    }

    /// Every clip that gets composited somewhere, with its track.
    pub fn visible_video_clips(&self) -> impl Iterator<Item = (usize, &Clip)> {
        self.visible_video_tracks()
            .flat_map(|(i, t)| t.clips.iter().filter(|c| !c.disabled).map(move |c| (i, c)))
    }

    /// The video clips covering `frame`, one per track, from bottom to
    /// top (compositing order). Excludes disabled clips and tracks.
    pub fn active_video_clips_at(&self, frame: FrameIdx) -> Vec<(usize, &Clip)> {
        self.visible_video_tracks()
            .filter_map(|(i, t)| visible_clip_at(t, frame).map(|c| (i, c)))
            .collect()
    }

    /// The audio tracks ending up in the mix: not muted and, if any is
    /// soloed, only those.
    pub fn audible_tracks(&self) -> impl Iterator<Item = (usize, &Track)> {
        let any_solo = self.tracks_of_kind(TrackKind::Audio).any(|(_, t)| t.solo);
        self.tracks_of_kind(TrackKind::Audio)
            .filter(move |(_, t)| !t.muted && (!any_solo || t.solo))
    }

    pub fn is_locked(&self, track_index: usize) -> bool {
        self.tracks.get(track_index).is_some_and(|t| t.locked)
    }

    /// The first unlocked track of kind `kind`.
    pub fn first_unlocked_track_index(&self, kind: TrackKind) -> Option<usize> {
        self.tracks_of_kind(kind)
            .find(|(_, t)| !t.locked)
            .map(|(i, _)| i)
    }

    /// The topmost of `active_video_clips_at`: the clip the viewer shows.
    pub fn active_video_clip_at(&self, frame: FrameIdx) -> Option<(usize, &Clip)> {
        self.visible_video_tracks()
            .rev()
            .find_map(|(i, t)| visible_clip_at(t, frame).map(|c| (i, c)))
    }

    /// The clips of `group` on all the tracks. One scan: it is called only on
    /// user interactions.
    pub fn clips_in_group(&self, group: LinkGroupId) -> Vec<(usize, ClipId)> {
        self.tracks
            .iter()
            .enumerate()
            .flat_map(|(i, t)| {
                t.clips
                    .iter()
                    .filter(move |c| c.linked_group == Some(group))
                    .map(move |c| (i, c.id))
            })
            .collect()
    }

    /// The other members of the linked group of a clip, excluding it.
    pub fn linked_members(&self, track_index: usize, clip_id: ClipId) -> Vec<(usize, ClipId)> {
        let Some(group) = self.clip(track_index, clip_id).and_then(|c| c.linked_group) else {
            return Vec::new();
        };
        self.clips_in_group(group)
            .into_iter()
            .filter(|&(_, id)| id != clip_id)
            .collect()
    }
}

/// The clip of `track` covering `frame`, unless it is disabled.
fn visible_clip_at(track: &Track, frame: FrameIdx) -> Option<&Clip> {
    track
        .clips
        .iter()
        .find(|c| c.contains(frame))
        .filter(|c| !c.disabled)
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Project {
    pub media_pool: IdMap<MediaId, MediaItem>,
    pub timelines: IdMap<TimelineId, Timeline>,
    #[serde(default)]
    pub folders: IdMap<FolderId, MediaFolder>,
    next_clip_id: u64,
    #[serde(default)]
    next_link_group_id: u64,
    /// Highest number ever assigned to a compound clip: kept only for
    /// compatibility with saved projects, the new name is chosen by
    /// `alloc_compound_name`.
    #[serde(default)]
    next_compound_id: u64,
    /// Counter for `MediaItem::content_hash` of the compound clips: see
    /// `Project::touch_compound`.
    #[serde(default)]
    next_compound_generation: u64,
}

impl Project {
    pub fn alloc_clip_id(&mut self) -> ClipId {
        let id = ClipId(self.next_clip_id);
        self.next_clip_id += 1;
        id
    }

    pub fn alloc_link_group_id(&mut self) -> LinkGroupId {
        let id = LinkGroupId(self.next_link_group_id);
        self.next_link_group_id += 1;
        id
    }

    /// Name for a new compound clip in the media pool: the lowest free
    /// number, so deleting one frees its name.
    pub fn alloc_compound_name(&mut self) -> String {
        let used: std::collections::HashSet<u64> = self
            .media_pool
            .values()
            .filter(|item| item.compound.is_some())
            .filter_map(|item| {
                item.path
                    .to_str()?
                    .strip_prefix(COMPOUND_NAME_PREFIX)?
                    .parse()
                    .ok()
            })
            .collect();
        let number = (1..).find(|n| !used.contains(n)).unwrap_or(1);
        self.next_compound_id = self.next_compound_id.max(number);
        format!("{COMPOUND_NAME_PREFIX}{number}")
    }

    /// Name for a new timeline: the lowest free "Timeline N", so deleting
    /// one frees its name.
    pub fn alloc_timeline_name(&self) -> String {
        let used: std::collections::HashSet<u64> = self
            .timelines
            .values()
            .filter_map(|tl| tl.name.strip_prefix(TIMELINE_NAME_PREFIX)?.parse().ok())
            .collect();
        let number = (1..).find(|n| !used.contains(n)).unwrap_or(1);
        format!("{TIMELINE_NAME_PREFIX}{number}")
    }

    /// Clip and link group ids are unique project-wide: a timeline coming
    /// from elsewhere needs fresh ones.
    pub(crate) fn reallocate_clip_ids(&mut self, timeline: &mut Timeline) {
        let mut groups = std::collections::HashMap::new();
        for track in &mut timeline.tracks {
            let mut clip_ids = std::collections::HashMap::new();
            for clip in &mut track.clips {
                let id = self.alloc_clip_id();
                clip_ids.insert(clip.id, id);
                clip.id = id;
                if let Some(group) = clip.linked_group {
                    let new_group = *groups
                        .entry(group)
                        .or_insert_with(|| self.alloc_link_group_id());
                    clip.linked_group = Some(new_group);
                }
            }
            track.crossings.retain_mut(|crossing| {
                match (
                    clip_ids.get(&crossing.left_clip),
                    clip_ids.get(&crossing.right_clip),
                ) {
                    (Some(&left), Some(&right)) => {
                        crossing.left_clip = left;
                        crossing.right_clip = right;
                        true
                    }
                    _ => false,
                }
            });
        }
    }

    /// The media pool entry of `timeline_id`, named after the timeline.
    pub fn insert_timeline_item(
        &mut self,
        timeline_id: TimelineId,
        folder: Option<FolderId>,
    ) -> MediaId {
        let item = self.timeline_item(timeline_id, &self.timelines[timeline_id].clone(), folder);
        self.media_pool.insert(item)
    }

    /// The pool item through which `timeline` (under `timeline_id`) is used
    /// as a clip.
    pub fn timeline_item(
        &mut self,
        timeline_id: TimelineId,
        timeline: &Timeline,
        folder: Option<FolderId>,
    ) -> MediaItem {
        MediaItem {
            path: timeline.name.clone().into(),
            meta: timeline.compound_meta(),
            content_hash: self.alloc_compound_generation(),
            compound: Some(timeline_id),
            folder,
        }
    }

    /// The media of `folder` and of its subfolders.
    pub fn media_in_folder(&self, folder: FolderId) -> Vec<MediaId> {
        let mut folders = vec![folder];
        let mut i = 0;
        while let Some(&current) = folders.get(i) {
            for (id, f) in &self.folders {
                if f.parent == Some(current) && !folders.contains(&id) {
                    folders.push(id);
                }
            }
            i += 1;
        }
        self.media_pool
            .iter()
            .filter(|(_, item)| item.folder.is_some_and(|f| folders.contains(&f)))
            .map(|(id, _)| id)
            .collect()
    }

    /// `false` if `parent` is `folder` itself or inside it, or if `folder`
    /// does not exist.
    pub fn can_move_folder(&self, folder: FolderId, parent: Option<FolderId>) -> bool {
        let mut ancestor = parent;
        for _ in 0..=self.folders.len() {
            match ancestor {
                Some(f) if f == folder => return false,
                Some(f) => ancestor = self.folders.get(f).and_then(|f| f.parent),
                None => break,
            }
        }
        self.folders.contains_key(folder)
    }

    /// New value for `MediaItem::content_hash` of a compound clip: to be
    /// assigned on creation and every time its nested timeline
    /// changes, to invalidate the cache of composited frames depending
    /// on its content.
    pub fn alloc_compound_generation(&mut self) -> u64 {
        self.next_compound_generation += 1;
        self.next_compound_generation
    }

    /// Recomputes `meta`/`content_hash` of a compound clip from its
    /// current nested timeline. To be called after every command touching
    /// that timeline, not only on creation: `meta.duration_frames`
    /// (hence the `Clip::timeline_len` available in a drag from the media pool)
    /// must reflect the real content, not the one at the time of
    /// creation.
    pub fn sync_compound_meta(&mut self, media_id: MediaId) {
        let Some(item) = self.media_pool.get(media_id) else {
            return;
        };
        let Some(timeline_id) = item.compound else {
            return;
        };
        let Some(timeline) = self.timelines.get(timeline_id) else {
            return;
        };
        let meta = timeline.compound_meta();
        let generation = self.alloc_compound_generation();
        let item = self.media_pool.get_mut(media_id).expect("checked above");
        item.meta = meta;
        item.content_hash = generation;
    }

    /// `true` if a clip referencing `media_id` (a compound clip, or the
    /// project timeline itself: see `MediaItem::compound`) cannot
    /// land on `destination` without closing a cycle — that is, if
    /// `destination` is reachable from the nested timeline of
    /// `media_id`, following in turn the compound clips it contains, at
    /// any depth (direct import of a timeline inside itself,
    /// or indirect through one of its compound clips). The rendering
    /// has a depth limit anyway as a safety net (see
    /// `MAX_COMPOUND_DEPTH` in `vv_app::render_ahead`), but a cycle must
    /// not be creatable in the first place.
    pub fn would_create_a_cycle(&self, media_id: MediaId, destination: TimelineId) -> bool {
        let Some(start) = self.media_pool.get(media_id).and_then(|m| m.compound) else {
            return false;
        };
        if start == destination {
            return true;
        }
        let mut visited: std::collections::HashSet<TimelineId> = std::collections::HashSet::new();
        let mut stack = vec![start];
        while let Some(current) = stack.pop() {
            if !visited.insert(current) {
                continue;
            }
            let Some(timeline) = self.timelines.get(current) else {
                continue;
            };
            for clip in timeline.tracks.iter().flat_map(|t| t.clips.iter()) {
                let ClipSource::Media(id) = &clip.source else {
                    continue;
                };
                let Some(nested) = self.media_pool.get(*id).and_then(|m| m.compound) else {
                    continue;
                };
                if nested == destination {
                    return true;
                }
                stack.push(nested);
            }
        }
        false
    }

    /// Recomputes `Clip::rate` from the fps. `source_offset`/`timeline_len` are
    /// in timeline frames and do not change.
    pub fn refresh_clip_rates(&mut self) {
        self.conform_clips(|_| true);
    }

    /// After the fps of `media` changed.
    pub fn conform_clips_of(&mut self, media: MediaId) {
        self.conform_clips(|id| id == media);
    }

    fn conform_clips(&mut self, of: impl Fn(MediaId) -> bool) {
        let media_pool = &self.media_pool;
        for timeline in self.timelines.values_mut() {
            let timeline_fps = timeline.fps;
            for clip in timeline.tracks.iter_mut().flat_map(|t| t.clips.iter_mut()) {
                let ClipSource::Media(media_id) = clip.source else {
                    continue;
                };
                if of(media_id)
                    && let Some(item) = media_pool.get(media_id)
                {
                    clip.conform(Rational::conform_rate(timeline_fps, item.meta.fps));
                }
            }
        }
    }
}

#[cfg(test)]
#[path = "tests/model_keyframe.rs"]
mod keyframe_tests;

#[cfg(test)]
#[path = "tests/model_timeline.rs"]
mod timeline_tests;
