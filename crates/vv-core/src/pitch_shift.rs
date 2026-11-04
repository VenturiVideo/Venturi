//! Constant clip pitch, independent of its timeline duration.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct PitchShift {
    pub enabled: bool,
    pub semitones: f32,
    pub cents: f32,
    pub preserve_formants: bool,
}

impl Default for PitchShift {
    fn default() -> Self {
        Self {
            enabled: true,
            semitones: 0.0,
            cents: 0.0,
            preserve_formants: false,
        }
    }
}

impl PitchShift {
    pub const SEMITONE_RANGE: (f32, f32) = (-24.0, 24.0);
    pub const CENT_RANGE: (f32, f32) = (-100.0, 100.0);

    pub fn sanitized(mut self) -> Self {
        let finite = |value: f32, (min, max): (f32, f32)| {
            if value.is_finite() {
                value.clamp(min, max)
            } else {
                0.0
            }
        };
        self.semitones = finite(self.semitones, Self::SEMITONE_RANGE);
        self.cents = finite(self.cents, Self::CENT_RANGE);
        self
    }

    pub fn ratio(self) -> f64 {
        if !self.enabled {
            return 1.0;
        }
        let params = self.sanitized();
        2.0f64.powf((params.semitones as f64 + params.cents as f64 / 100.0) / 12.0)
    }

    pub fn is_active(self) -> bool {
        self.ratio() != 1.0
    }
}

#[cfg(test)]
#[path = "tests/pitch_shift.rs"]
mod tests;
