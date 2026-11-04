use serde::{Deserialize, Serialize};

use crate::{FrameIdx, Keyframed, Transform};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ShakeParam {
    MotionScale,
    Speed,
    Pan,
    Tilt,
    Rotation,
    Zoom,
    Smoothness,
}

impl ShakeParam {
    pub const ALL: [Self; 7] = [
        Self::MotionScale,
        Self::Speed,
        Self::Pan,
        Self::Tilt,
        Self::Rotation,
        Self::Zoom,
        Self::Smoothness,
    ];

    pub const fn index(self) -> usize {
        self as usize
    }

    pub fn range(self) -> std::ops::RangeInclusive<f32> {
        match self {
            Self::MotionScale => 0.0..=2.0,
            Self::Speed => 0.0..=20.0,
            Self::Pan | Self::Tilt | Self::Zoom => 0.0..=10.0,
            Self::Rotation => 0.0..=15.0,
            Self::Smoothness => 0.0..=1.0,
        }
    }

    pub const fn default_value(self) -> f32 {
        match self {
            Self::MotionScale => 1.0,
            Self::Speed => 2.0,
            Self::Pan | Self::Tilt | Self::Rotation => 0.5,
            Self::Zoom => 0.2,
            Self::Smoothness => 0.8,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct CameraShakeTracks {
    #[serde(with = "param_tracks")]
    params: [Keyframed<f32>; 7],
    pub seed: u32,
    pub auto_zoom: bool,
}

impl Default for CameraShakeTracks {
    fn default() -> Self {
        Self {
            params: ShakeParam::ALL.map(|p| Keyframed::constant(p.default_value())),
            seed: 0,
            auto_zoom: true,
        }
    }
}

impl CameraShakeTracks {
    pub fn track(&self, param: ShakeParam) -> &Keyframed<f32> {
        &self.params[param.index()]
    }

    pub fn track_mut(&mut self, param: ShakeParam) -> &mut Keyframed<f32> {
        &mut self.params[param.index()]
    }

    pub fn tracks_mut(&mut self) -> &mut [Keyframed<f32>; 7] {
        &mut self.params
    }

    pub fn apply(&self, transform: &mut Transform, frame: FrameIdx, fps: f64, size: (u32, u32)) {
        self.apply_at(transform, frame as f64, fps, size);
    }

    pub fn apply_at(&self, transform: &mut Transform, frame: f64, fps: f64, size: (u32, u32)) {
        if !frame.is_finite() || !fps.is_finite() || fps <= 0.0 || size.0 == 0 || size.1 == 0 {
            return;
        }
        let value = |p: ShakeParam| {
            let v = track_value(self.track(p), frame);
            if v.is_finite() {
                v.clamp(*p.range().start(), *p.range().end())
            } else {
                0.0
            }
        };
        let amount = value(ShakeParam::MotionScale);
        if amount == 0.0 {
            return;
        }
        let phase = speed_phase(self.track(ShakeParam::Speed), frame) / fps;
        let smoothness = value(ShakeParam::Smoothness);
        let noise = |channel: u32| {
            let seed = self.seed.wrapping_add(channel.wrapping_mul(0x9e3779b9));
            let slow = value_noise(phase, seed);
            let fast = value_noise(phase * 4.0, seed.wrapping_add(0x85ebca6b));
            slow * (0.5 + 0.5 * smoothness) + fast * (0.5 - 0.5 * smoothness)
        };
        let (w, h) = (size.0 as f32, size.1 as f32);
        let pan = w * value(ShakeParam::Pan) * amount / 100.0;
        let tilt = h * value(ShakeParam::Tilt) * amount / 100.0;
        let angle = value(ShakeParam::Rotation) * amount;
        let zoom = value(ShakeParam::Zoom) * amount / 100.0;
        let rotation = angle * noise(2);
        let scale = (zoom * noise(3)).exp();
        let compensation = if self.auto_zoom {
            // A fixed envelope avoids pumping as the random motion changes.
            let sin = angle.to_radians().sin().abs();
            let margin = (1.0 + 2.0 * (pan + sin * tilt) / w + sin * h / w)
                .max(1.0 + 2.0 * (tilt + sin * pan) / h + sin * w / h);
            margin * zoom.exp()
        } else {
            1.0
        };
        let (sin, cos) = rotation.to_radians().sin_cos();
        let x = transform.position[0] + transform.anchor[0];
        let y = transform.position[1] + transform.anchor[1];
        // Transform positions are Y up; positive rotation is clockwise.
        transform.position = [
            scale * (cos * x + sin * y) + pan * noise(0) - transform.anchor[0],
            scale * (-sin * x + cos * y) + tilt * noise(1) - transform.anchor[1],
        ];
        transform.zoom = transform.zoom.map(|z| z * scale);
        transform.rotation += rotation;
        // Overscan grows around the clip's centre, including with a custom anchor.
        let (sin, cos) = transform.rotation.to_radians().sin_cos();
        let x = transform.anchor[0] * transform.zoom[0];
        let y = transform.anchor[1] * transform.zoom[1];
        transform.position[0] += (compensation - 1.0) * (cos * x + sin * y);
        transform.position[1] += (compensation - 1.0) * (-sin * x + cos * y);
        transform.zoom = transform.zoom.map(|z| z * compensation);
    }
}

mod param_tracks {
    use super::*;
    use serde::de::{MapAccess, SeqAccess, Visitor};

    pub fn serialize<S: serde::Serializer>(
        params: &[Keyframed<f32>; 7],
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        serializer.collect_map(ShakeParam::ALL.map(|p| (p, &params[p.index()])))
    }

    pub fn deserialize<'de, D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> Result<[Keyframed<f32>; 7], D::Error> {
        struct TracksVisitor;
        impl<'de> Visitor<'de> for TracksVisitor {
            type Value = [Keyframed<f32>; 7];

            fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
                formatter.write_str("named camera shake tracks or a legacy track array")
            }

            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
                let mut params = CameraShakeTracks::default().params;
                while let Some((param, track)) = map.next_entry::<ShakeParam, Keyframed<f32>>()? {
                    params[param.index()] = track;
                }
                Ok(params)
            }

            fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
                let mut params = CameraShakeTracks::default().params;
                // Preserve the original positional format independently of enum order.
                for param in [
                    ShakeParam::MotionScale,
                    ShakeParam::Speed,
                    ShakeParam::Pan,
                    ShakeParam::Tilt,
                    ShakeParam::Rotation,
                    ShakeParam::Zoom,
                    ShakeParam::Smoothness,
                ] {
                    let Some(track) = seq.next_element()? else {
                        break;
                    };
                    params[param.index()] = track;
                }
                Ok(params)
            }
        }
        deserializer.deserialize_any(TracksVisitor)
    }
}

fn hash(mut value: u64) -> u64 {
    value = (value ^ (value >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
    value = (value ^ (value >> 27)).wrapping_mul(0x94d049bb133111eb);
    value ^ (value >> 31)
}

fn value_noise(time: f64, seed: u32) -> f32 {
    let cell = time.floor() as i64;
    let random = |i: i64| {
        let bits = hash((i as u64).wrapping_add(u64::from(seed).wrapping_mul(0x9e3779b9)));
        (bits >> 40) as f32 / 8_388_607.5 - 1.0
    };
    let t = (time - time.floor()) as f32;
    let t = t * t * t * (t * (t * 6.0 - 15.0) + 10.0);
    random(cell) * (1.0 - t) + random(cell.wrapping_add(1)) * t
}

fn track_value(track: &Keyframed<f32>, frame: f64) -> f32 {
    let keys = track.keyframes();
    let i = keys.partition_point(|k| k.0 as f64 <= frame);
    match (i, keys.len()) {
        (_, 0) => track.default,
        (0, _) => keys[0].1,
        (i, n) if i == n => keys[n - 1].1,
        _ => {
            let (a, b) = (keys[i - 1], keys[i]);
            let t = ((frame - a.0 as f64) / (b.0 as f64 - a.0 as f64)) as f32;
            a.1 + (b.1 - a.1) * a.2.ease(t)
        }
    }
}

fn speed_value(track: &Keyframed<f32>, frame: f64) -> f64 {
    let v = track_value(track, frame);
    if v.is_finite() {
        f64::from(v.clamp(0.0, 20.0))
    } else {
        0.0
    }
}

fn speed_phase(track: &Keyframed<f32>, frame: f64) -> f64 {
    if track.is_constant() {
        return frame * speed_value(track, 0.0);
    }
    let (start, end) = (frame.min(0.0), frame.max(0.0));
    let mut a = start;
    let mut area = 0.0;
    // Integrating speed keeps animated speed changes from jumping in phase.
    for b in track
        .keyframes()
        .iter()
        .map(|k| k.0 as f64)
        .filter(|&f| f > start && f < end)
        .chain(std::iter::once(end))
    {
        let half = (b - a) * 0.5;
        let mid = (a + b) * 0.5;
        for (node, weight) in [
            (0.1834346424956498, 0.362683783378362),
            (0.525532409916329, 0.3137066458778873),
            (0.7966664774136267, 0.2223810344533745),
            (0.9602898564975363, 0.1012285362903763),
        ] {
            area += half
                * weight
                * (speed_value(track, mid - half * node) + speed_value(track, mid + half * node));
        }
        a = b;
    }
    if frame < 0.0 { -area } else { area }
}

#[cfg(test)]
#[path = "tests/camera_shake.rs"]
mod tests;
