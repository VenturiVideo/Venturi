//! Resolve reads back only its own `metadata.Resolve_OTIO` namespace, so the
//! transform of a clip has to travel as a list of `Effect.1` shaped exactly
//! like the ones its exporter writes. Parameter IDs, types and units come
//! from a file exported by Resolve: values normalized on the frame,
//! keyframes indexed by timeline frame relative to the start of the clip.
//!
//! Resolve omits every parameter left at its default and we do the same:
//! a parameter it does not find keeps the value of the imported clip.

use super::export::rational_time;
use crate::model::{
    BlendMode, Clip, ClipSource, Ease, FrameIdx, Keyframed, Rational, TrackKind, TransformParam,
    TransformTracks, Transition,
};
use serde_json::{Map, Value, json};

/// Denominators of the normalized values. Resolve expresses pan, tilt and
/// anchor as fractions of the clip *as fitted into the frame*, not of the
/// frame: on a 1080x2400 source in a 1920x1080 timeline a shift of half the
/// visible width is 0.5 of 486 px, and its inspector then shows it
/// multiplied by the width of the frame. The crop is in media pixels, like
/// ours, because Resolve applies it before the transform.
pub(super) struct Scale {
    pub display: (f32, f32),
    pub media: (f32, f32),
}

impl Scale {
    /// `media` letterboxed into `frame`, which is how both compositors place
    /// a clip whose aspect ratio differs from the timeline's.
    pub fn new(media: (f32, f32), frame: (f32, f32)) -> Self {
        let fit = (frame.0 / media.0).min(frame.1 / media.1);
        Self {
            display: (media.0 * fit, media.1 * fit),
            media,
        }
    }
}

pub(super) fn clip_effects(clip: &Clip, kind: TrackKind, scale: &Scale) -> Vec<Value> {
    time_warp(clip)
        .into_iter()
        .chain(resolve_effects(clip, kind, scale))
        .collect()
}

/// The only effect of Resolve's that is not in its own namespace: the speed
/// travels in the standard schema, first in the list like it writes it.
fn time_warp(clip: &Clip) -> Option<Value> {
    if clip.freeze.is_some() {
        return Some(json!({
            "OTIO_SCHEMA": "FreezeFrame.1",
            "name": "",
            "effect_name": "FreezeFrame",
            "time_scalar": 0.0,
            "metadata": {},
        }));
    }
    (!clip.speed().is_one()).then(|| {
        json!({
            "OTIO_SCHEMA": "LinearTimeWarp.1",
            "name": "",
            "effect_name": "",
            "time_scalar": clip.speed().as_f64(),
            "metadata": {},
        })
    })
}

/// The parameterless effect that makes a `MissingReference` clip an
/// adjustment clip for Resolve: it has no other marker (the name can be changed).
const ADJUSTMENT_EFFECT_TYPE: u64 = 74;

pub(super) fn is_adjustment_clip<'a>(effects: impl IntoIterator<Item = &'a Value>) -> bool {
    effects
        .into_iter()
        .any(|e| e["metadata"]["Resolve_OTIO"]["Type"].as_u64() == Some(ADJUSTMENT_EFFECT_TYPE))
}

fn adjustment_marker(clip: &Clip) -> Option<Value> {
    matches!(clip.source, ClipSource::Adjustment).then(|| {
        json!({
            "OTIO_SCHEMA": "Effect.1",
            "name": "",
            "effect_name": "Resolve Effect",
            "metadata": {
                "Resolve_OTIO": {
                    "Effect Name": "Effect",
                    "Name": "Effect",
                    "Type": ADJUSTMENT_EFFECT_TYPE,
                    "Display Type": 0,
                    "Enabled": true,
                    "Parameters": [],
                }
            },
        })
    })
}

fn resolve_effects(clip: &Clip, kind: TrackKind, scale: &Scale) -> Vec<Value> {
    match kind {
        TrackKind::Video => [
            adjustment_marker(clip),
            transform(clip, scale),
            cropping(clip, scale),
            composite(clip),
            video_faders(clip),
        ],
        TrackKind::Audio => [volume_and_fades(clip), None, None, None, None],
    }
    .into_iter()
    .flatten()
    .collect()
}

fn transform(clip: &Clip, scale: &Scale) -> Option<Value> {
    use TransformParam::*;
    let t = &clip.effects.transform;
    let parameters = [
        scalar(
            clip,
            t,
            ZoomX,
            "transformationZoomX",
            1.0,
            1.0,
            [0.01, 100.0],
        ),
        scalar(
            clip,
            t,
            ZoomY,
            "transformationZoomY",
            1.0,
            1.0,
            [0.01, 100.0],
        ),
        scalar(
            clip,
            t,
            PositionX,
            "transformationPan",
            scale.display.0,
            0.0,
            [-4.0, 4.0],
        ),
        scalar(
            clip,
            t,
            PositionY,
            "transformationTilt",
            scale.display.1,
            0.0,
            [-4.0, 4.0],
        ),
        // Resolve turns counter-clockwise on a positive angle, we turn clockwise.
        scalar(
            clip,
            t,
            Rotation,
            "transformationRotationAngle",
            -1.0,
            0.0,
            [-100_000.0, 100_000.0],
        ),
        point(
            clip,
            t,
            [AnchorX, AnchorY],
            "transformationAnchorPoint",
            scale.display,
        ),
        boolean("transformationFlipX", t.flip[0]),
        boolean("transformationFlipY", t.flip[1]),
    ];
    effect("Transform", "Transform", 2, 1, parameters)
}

fn cropping(clip: &Clip, scale: &Scale) -> Option<Value> {
    use TransformParam::*;
    let t = &clip.effects.transform;
    let parameters = [
        scalar(
            clip,
            t,
            CropLeft,
            "cropLeft",
            scale.media.0,
            0.0,
            [0.0, 1.0],
        ),
        scalar(
            clip,
            t,
            CropRight,
            "cropRight",
            scale.media.0,
            0.0,
            [0.0, 1.0],
        ),
        scalar(clip, t, CropTop, "cropTop", scale.media.1, 0.0, [0.0, 1.0]),
        scalar(
            clip,
            t,
            CropBottom,
            "cropBottom",
            scale.media.1,
            0.0,
            [0.0, 1.0],
        ),
        scalar(
            clip,
            t,
            CropSoftness,
            "cropSoftness",
            1.0,
            0.0,
            [-100.0, 100.0],
        ),
        None,
    ];
    effect("Cropping", "Cropping", 3, 1, parameters)
}

fn composite(clip: &Clip) -> Option<Value> {
    let t = &clip.effects.transform;
    let opacity = scalar(
        clip,
        t,
        TransformParam::Opacity,
        "opacity",
        1.0,
        100.0,
        [0.0, 100.0],
    );
    let mode = composite_mode(clip.effects.blend_mode);
    let mode = (mode != 0).then(|| {
        json!({
            "Parameter ID": "composite mode",
            "Parameter Value": mode,
            "Default Parameter Value": 0,
            "Variant Type": "UInt",
        })
    });
    effect(
        "Composite",
        "Composite",
        1,
        1,
        [mode, opacity, None, None, None, None],
    )
}

/// Values of Resolve's `composite mode`, read off a file with one clip per
/// entry of its Composite Mode menu. It has more of them than we do; those
/// stay out and the import warns about them.
const COMPOSITE_MODES: [(BlendMode, u64); 15] = [
    (BlendMode::Normal, 0),
    (BlendMode::Add, 1),
    (BlendMode::Subtract, 2),
    (BlendMode::Difference, 3),
    (BlendMode::Multiply, 4),
    (BlendMode::Screen, 5),
    (BlendMode::Overlay, 6),
    (BlendMode::HardLight, 7),
    (BlendMode::SoftLight, 8),
    (BlendMode::Darken, 9),
    (BlendMode::Lighten, 10),
    (BlendMode::ColorDodge, 11),
    (BlendMode::ColorBurn, 12),
    (BlendMode::Exclusion, 13),
    (BlendMode::Divide, 18),
];

pub(super) fn composite_mode(blend: BlendMode) -> u64 {
    COMPOSITE_MODES
        .iter()
        .find(|(b, _)| *b == blend)
        .map_or(0, |(_, mode)| *mode)
}

pub(super) fn blend_mode(mode: u64) -> Option<BlendMode> {
    COMPOSITE_MODES
        .iter()
        .find(|(_, m)| *m == mode)
        .map(|(blend, _)| *blend)
}

fn video_faders(clip: &Clip) -> Option<Value> {
    effect(
        "Video Faders",
        "Video Faders",
        36,
        3,
        [
            frames("videoFaderIn", clip.fade_in),
            frames("videoFaderOut", clip.fade_out),
            None,
            None,
            None,
            None,
        ],
    )
}

fn volume_and_fades(clip: &Clip) -> Option<Value> {
    let gain = parameter(
        "volume",
        clip.effects.gain_db.default,
        0.0,
        [-100.0, 30.0],
        key_frames(clip, &clip.effects.gain_db, 1.0),
    );
    effect(
        "Volume",
        "Fairlight Clip Volume and Fades",
        62,
        1,
        [
            gain,
            frames("faderIn", clip.fade_in),
            frames("faderOut", clip.fade_out),
            None,
            None,
            None,
        ],
    )
}

fn effect<const N: usize>(
    name: &str,
    effect_name: &str,
    type_id: u32,
    display_type: u32,
    parameters: [Option<Value>; N],
) -> Option<Value> {
    let parameters: Vec<Value> = parameters.into_iter().flatten().collect();
    if parameters.is_empty() {
        return None;
    }
    Some(json!({
        "OTIO_SCHEMA": "Effect.1",
        "name": "",
        "effect_name": "Resolve Effect",
        "metadata": {
            "Resolve_OTIO": {
                "Effect Name": effect_name,
                "Name": name,
                "Type": type_id,
                "Display Type": display_type,
                "Enabled": true,
                "Parameters": parameters,
            }
        },
    }))
}

/// `divisor` brings our unit into Resolve's: the size of the frame for the
/// normalized ones, `-1` for the angle, `1` for those already shared.
fn scalar(
    clip: &Clip,
    tracks: &TransformTracks,
    param: TransformParam,
    id: &str,
    divisor: f32,
    default: f32,
    range: [f32; 2],
) -> Option<Value> {
    let track = tracks.track(param);
    parameter(
        id,
        track.default / divisor,
        default,
        range,
        key_frames(clip, track, divisor),
    )
}

/// A `POINTF`: one keyframe per instant at which either axis has one, with
/// the other axis sampled there.
fn point(
    clip: &Clip,
    tracks: &TransformTracks,
    params: [TransformParam; 2],
    id: &str,
    divisor: (f32, f32),
) -> Option<Value> {
    let axes = params.map(|p| tracks.track(p));
    let value = |t: FrameIdx| {
        json!([
            axes[0].value_at(t) / divisor.0,
            axes[1].value_at(t) / divisor.1
        ])
    };
    let mut instants: Vec<FrameIdx> = axes
        .iter()
        .flat_map(|a| a.keyframes())
        .map(|(f, _, _)| *f)
        .collect();
    instants.sort_unstable();
    instants.dedup();
    let keys: Map<String, Value> = instants
        .into_iter()
        .map(|t| {
            (
                local_frame(clip, t).to_string(),
                json!({ "Value": value(t), "Variant Type": "POINTF" }),
            )
        })
        .collect();

    let default = json!([axes[0].default / divisor.0, axes[1].default / divisor.1]);
    if keys.is_empty() && default == json!([0.0, 0.0]) {
        return None;
    }
    Some(json!({
        "Parameter ID": id,
        "Parameter Value": default,
        "Default Parameter Value": [0.0, 0.0],
        "Variant Type": "POINTF",
        "Key Frames": keys,
    }))
}

fn parameter(
    id: &str,
    value: f32,
    default: f32,
    range: [f32; 2],
    keys: Map<String, Value>,
) -> Option<Value> {
    if keys.is_empty() && value == default {
        return None;
    }
    Some(json!({
        "Parameter ID": id,
        "Parameter Value": value,
        "Default Parameter Value": default,
        "Variant Type": "Double",
        "minValue": range[0],
        "maxValue": range[1],
        "Key Frames": keys,
    }))
}

fn boolean(id: &str, value: bool) -> Option<Value> {
    value.then(|| {
        json!({
            "Parameter ID": id,
            "Parameter Value": true,
            "Default Parameter Value": false,
            "Variant Type": "Bool",
        })
    })
}

/// A duration in frames: no keyframes and no range, like Resolve's faders.
fn frames(id: &str, value: FrameIdx) -> Option<Value> {
    (value > 0).then(|| {
        json!({
            "Parameter ID": id,
            "Parameter Value": value as f64,
            "Default Parameter Value": 0.0,
            "Variant Type": "Double",
            "minValue": 0.0,
            "maxValue": 2_147_483_647.0,
        })
    })
}

/// Our keyframes are indexed by source frame, Resolve's by timeline frame
/// from the start of the clip. The interpolation is lost: Resolve reads it
/// from bezier handles we do not write.
fn key_frames(clip: &Clip, track: &Keyframed<f32>, divisor: f32) -> Map<String, Value> {
    track
        .keyframes()
        .iter()
        .map(|(source_frame, value, _)| {
            (
                local_frame(clip, *source_frame).to_string(),
                json!({ "Value": value / divisor, "Variant Type": "Double" }),
            )
        })
        .collect()
}

fn local_frame(clip: &Clip, source_frame: FrameIdx) -> FrameIdx {
    clip.timeline_frame_at(source_frame) - clip.timeline_start
}

/// A transition takes no time on the track: it straddles the cut, extending
/// `in_offset` into what precedes it and `out_offset` into what follows.
pub(super) fn transition_to_otio(
    transition: &Transition,
    (into_previous, into_next): (FrameIdx, FrameIdx),
    fps: Rational,
) -> Value {
    let edge = match (into_previous, into_next) {
        (0, _) => "in",
        (_, 0) => "out",
        _ => "crossing",
    };
    let name = match transition.kind {
        crate::model::TransitionKind::Push => "Push",
    };
    json!({
        "OTIO_SCHEMA": "Transition.1",
        "name": name,
        "transition_type": "Custom_Transition",
        "in_offset": rational_time(into_previous, fps),
        "out_offset": rational_time(into_next, fps),
        "metadata": {
            "Resolve_OTIO": {
                "Transition Type": name,
                "Effects": {
                    "Effect Name": name,
                    "Name": name,
                    "Type": 50,
                    "Display Type": 1,
                    "Enabled": true,
                    "Parameters": [
                        {
                            "Parameter ID": "ease",
                            "Parameter Value": ease_index(transition.ease),
                            "Default Parameter Value": 0,
                            "Variant Type": "UInt",
                        },
                        transition_curve(transition),
                    ],
                },
            },
            // Direction and curve have no counterpart in Resolve's export.
            "venturi": { "transition": transition, "edge": edge },
        },
    })
}

/// Resolve drives a transition with a curve keyframed from 0 to 1 over its
/// whole length; without it the progress stays at 0 and nothing moves. The
/// ease lives in the bezier handles, horizontal and `HANDLE` of the length
/// at our `curve` 0.5, which is what Resolve writes by default.
fn transition_curve(transition: &Transition) -> Value {
    const HANDLE: f32 = 0.6;
    let length = transition.curve.clamp(0.0, 1.0) * HANDLE * transition.duration as f32;
    let (start, end) = match transition.ease {
        Ease::None => (0.0, 0.0),
        Ease::In => (length, 0.0),
        Ease::Out => (0.0, length),
        Ease::InOut => (length, length),
    };
    let mut keys = Map::new();
    let mut key = |at: FrameIdx, value: f64, handle: &str, x: f32| {
        let mut entry = json!({ "Value": value, "Variant Type": "Double" });
        if x != 0.0 {
            entry[handle] = json!({ handle: [x, 0.0], "Variant Type": "POINTF" });
        }
        keys.insert(at.to_string(), entry);
    };
    // The outgoing handle points forwards, the incoming one backwards.
    key(0, 0.0, "OutBez", start);
    key(transition.duration, 1.0, "InBez", -end);
    json!({
        "Parameter ID": "transitionCustomCurvesKeyframes",
        "Parameter Value": 0.0,
        "Default Parameter Value": 0.0,
        "Variant Type": "Double",
        "minValue": 0.0,
        "maxValue": 1.0,
        "Key Frames": keys,
    })
}

fn ease_index(ease: Ease) -> u32 {
    Ease::ALL.iter().position(|e| *e == ease).unwrap_or(0) as u32
}

#[cfg(test)]
#[path = "../tests/otio/resolve.rs"]
mod tests;
