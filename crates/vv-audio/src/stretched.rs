//! A time-stretched or pitch-shifted clip's audio, described rather than read: rendered from
//! the files and stretched apart from the mix, like a level is measured
//! (plans/AUDIO_LIVE_MIX.md step 4).

use std::ops::Range;
use std::path::Path;
use std::sync::Arc;

use vv_core::{Project, Timeline};

use crate::SampleBlocks;
use crate::mixer::{AudioSource, ClipAudio, MixSnapshot, mix_range};

/// What a clip processes before mixing.
#[derive(Clone, Copy)]
pub enum StretchInput<'a> {
    File {
        path: &'a Path,
        stream: usize,
    },
    /// A compound clip's timeline, in its own time, mixed at `depth` of
    /// nesting.
    Nested {
        timeline: &'a Timeline,
        depth: u32,
    },
}

pub struct StretchRequest<'a> {
    pub project: &'a Project,
    pub input: StretchInput<'a>,
    /// Of the input: the clip's, from its first frame.
    pub frames: Range<u64>,
    pub tempo: f64,
    /// Frequency multiplier, independently of `tempo`.
    pub pitch: f64,
    pub preserve_formants: bool,
    pub sample_rate: u32,
    pub channels: u16,
    /// Same key, same stretch, in every build.
    pub key: u64,
}

impl StretchRequest<'_> {
    /// The input over `range` (all of `frames` for the stretch), mixed
    /// from `source` at `rate`/`channels` on a timeline whose time is the
    /// input's.
    pub fn snapshot(
        &self,
        source: &mut impl AudioSource,
        range: Range<u64>,
        (rate, channels): (u32, u16),
    ) -> MixSnapshot {
        match self.input {
            StretchInput::File { path, stream } => {
                MixSnapshot::of_file(rate, channels, source, path, stream, range)
            }
            StretchInput::Nested { timeline, depth } => MixSnapshot::nested_range(
                self.project,
                timeline,
                depth,
                rate,
                channels,
                source,
                range,
            ),
        }
    }

    /// Stretched from `source` whole and at once: the reference, and what
    /// a source without a better way does. `Pending` while it decodes.
    pub fn stretch_with(&self, source: &mut impl AudioSource) -> ClipAudio {
        let input = match self.input {
            StretchInput::File { path, stream } => {
                match source.file_frames(path, stream, self.frames.clone(), 0) {
                    ClipAudio::Ready(buffer) => buffer,
                    ClipAudio::Partial(_) | ClipAudio::Pending => return ClipAudio::Pending,
                    ClipAudio::Missing => return ClipAudio::Missing,
                }
            }
            StretchInput::Nested { .. } => {
                let format = (self.sample_rate, self.channels);
                let snapshot = self.snapshot(source, self.frames.clone(), format);
                let frames = (self.frames.end - self.frames.start) as usize;
                let mut mixed = vec![0.0; frames * self.channels.max(1) as usize];
                mix_range(&snapshot, self.frames.start, &mut mixed);
                SampleBlocks::contiguous(Arc::new(mixed), self.frames.start, self.channels)
            }
        };
        let mut stretcher = match crate::Stretcher::with_pitch(
            self.sample_rate,
            self.channels,
            self.tempo,
            self.pitch,
            self.preserve_formants,
        ) {
            Ok(stretcher) => stretcher,
            Err(_) => return ClipAudio::Missing,
        };
        let mut out = Vec::new();
        let range = self.frames.start.max(input.start())..self.frames.end.min(input.end());
        if stretcher.push(&input.to_vec(range), &mut out).is_err()
            || stretcher.finish(&mut out).is_err()
        {
            return ClipAudio::Missing;
        }
        ClipAudio::Ready(SampleBlocks::contiguous(Arc::new(out), 0, self.channels))
    }
}
