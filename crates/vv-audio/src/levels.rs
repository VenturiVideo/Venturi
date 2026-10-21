//! What a normalization measures, cut into segments at the edit points of
//! its input and keyed by their content in their own time, so that an edit
//! measures again only the segments it touches (plans/AUDIO_LIVE_MIX.md
//! §3.3). The segments are measured on their own and merged
//! (`LevelParts::merge`).

use std::hash::{Hash, Hasher};
use std::ops::Range;

use vv_core::{ClipId, ClipSource, MixerChannel, NormalizeMode, Project, Timeline};

use crate::loudness::LevelMeter;
use crate::mixer::{
    AnalysisSlot, AudioSource, Channel, ClipSpan, Insert, MixSnapshot, MixState, hash_insert,
    mix_range_from,
};

/// Rendered before a segment when the effects before the measure keep a
/// state: they reach about the state they would have in a render from the
/// start.
const STATE_PREROLL_FRAMES: u64 = 2 * crate::mixer::PROJECT_SAMPLE_RATE as u64;

/// FNV-1a: the same hash in every build, for the keys kept on disk.
#[derive(Default)]
pub struct StableHasher(Option<u64>);

impl Hasher for StableHasher {
    fn write(&mut self, bytes: &[u8]) {
        let mut hash = self.0.unwrap_or(0xcbf2_9ce4_8422_2325);
        for byte in bytes {
            hash = (hash ^ u64::from(*byte)).wrapping_mul(0x0100_0000_01b3);
        }
        self.0 = Some(hash);
    }

    fn finish(&self) -> u64 {
        self.0.unwrap_or(0xcbf2_9ce4_8422_2325)
    }
}

pub(crate) fn stable_hash(value: impl Hash) -> u64 {
    let mut h = StableHasher::default();
    value.hash(&mut h);
    h.finish()
}

/// What a normalization measures: its strip before it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Measured {
    pub channel: MixerChannel,
    /// Position of the normalization in the chain: the effects before it are
    /// measured through.
    pub insert: usize,
    /// One clip of the track alone.
    pub clip: Option<ClipId>,
}

/// A part of what a normalization measures, between two edit points.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LevelSegment {
    /// Timeline audio frames.
    pub frames: Range<u64>,
    /// Rendered before `frames` for the effects' state, not measured.
    pub preroll: u64,
    /// Same key, same measure, in every build.
    pub key: u64,
}

/// A normalization to measure, described rather than mixed: it is rendered
/// from the files, whatever the preview holds.
pub struct LevelAnalysis<'a> {
    pub slot: Option<AnalysisSlot>,
    pub project: &'a Project,
    pub timeline: &'a Timeline,
    pub measured: Measured,
    pub mode: NormalizeMode,
    pub sample_rate: u32,
    pub channels: u16,
    pub segments: Vec<LevelSegment>,
}

impl LevelAnalysis<'_> {
    /// Same key, same level: the keys of the segments, not where they are.
    pub fn key(&self) -> u64 {
        let keys: Vec<u64> = self.segments.iter().map(|s| s.key).collect();
        stable_hash((self.mode as u8, keys))
    }

    /// What the measure mixes over `range` (all of it with `None`):
    /// the strip up to the normalization, before its fader, at
    /// `sample_rate`/`channels`.
    pub fn snapshot(
        &self,
        source: &mut impl AudioSource,
        range: Option<Range<u64>>,
        (sample_rate, channels): (u32, u16),
    ) -> MixSnapshot {
        MixSnapshot::measured(
            self.project,
            self.timeline,
            self.measured,
            sample_rate,
            channels,
            source,
            range,
        )
    }

    /// Rendered whole and in order from `source`, without segments: the
    /// reference they approximate. Slow.
    pub fn measure_with(&self, source: &mut impl AudioSource) -> f32 {
        const CHUNK_FRAMES: u64 = 1 << 16;
        let (Some(first), Some(last)) = (self.segments.first(), self.segments.last()) else {
            return 0.0;
        };
        let snapshot = self.snapshot(source, None, (self.sample_rate, self.channels));
        let ch = self.channels.max(1) as usize;
        let mut out = vec![0.0f32; CHUNK_FRAMES as usize * ch];
        let mut state = MixState::new(&snapshot);
        let mut meter = LevelMeter::new(self.mode, self.sample_rate, self.channels);
        let mut f = first.frames.start;
        while f < last.frames.end {
            let frames = (last.frames.end - f).min(CHUNK_FRAMES);
            let chunk = &mut out[..frames as usize * ch];
            mix_range_from(&snapshot, &mut state, f, chunk);
            meter.push(chunk);
            f += frames;
        }
        meter.finish()
    }
}

/// A clip a strip receives, as the segment keys see it.
pub(crate) struct InputClip {
    pub track: usize,
    pub id: ClipId,
    pub start: u64,
    pub len: u64,
    /// Everything about it but where it starts.
    body: u64,
    compound: bool,
}

/// The clips of the audible tracks of `timeline`, whether their audio is
/// decoded or not.
pub(crate) fn input_clips(
    project: &Project,
    timeline: &Timeline,
    sample_rate: u32,
) -> Vec<InputClip> {
    input_clips_at(project, timeline, sample_rate, 0)
}

fn input_clips_at(
    project: &Project,
    timeline: &Timeline,
    sample_rate: u32,
    depth: u32,
) -> Vec<InputClip> {
    let fps = timeline.fps.as_f64().max(1e-9);
    let mut clips = Vec::new();
    for (track_index, track) in timeline.audible_tracks() {
        for clip in track
            .clips
            .iter()
            .filter(|c| !c.disabled && c.freeze.is_none())
        {
            let ClipSource::Media(media_id) = &clip.source else {
                continue;
            };
            let Some(item) = project.media_pool.get(*media_id) else {
                continue;
            };
            let span = ClipSpan::new(clip, fps, sample_rate);
            let nested = item.compound.and_then(|id| project.timelines.get(id));
            let source = match nested {
                Some(nested) => timeline_key(project, nested, sample_rate, depth + 1),
                None => stable_hash((item.content_hash, clip.audio_stream_index)),
            };
            let body = stable_hash((
                (span.len, span.source_offset, span.step.to_bits()),
                (span.fade_in, span.fade_out, clip.pitch_correction),
                (
                    clip.effects.pitch_shift.ratio().to_bits(),
                    clip.effects.pitch_shift.is_active()
                        && clip.effects.pitch_shift.preserve_formants,
                ),
                format!("{:?}", clip.effects.gain_db),
                item.meta.fps.as_f64().to_bits(),
                source,
            ));
            clips.push(InputClip {
                track: track_index,
                id: clip.id,
                start: span.start,
                len: span.len,
                body,
                compound: nested.is_some(),
            });
        }
    }
    clips
}

/// The content of a compound clip's timeline, in its own time.
pub(crate) fn timeline_key(
    project: &Project,
    timeline: &Timeline,
    sample_rate: u32,
    depth: u32,
) -> u64 {
    if depth >= vv_core::MAX_COMPOUND_DEPTH {
        return 0;
    }
    let mut h = StableHasher::default();
    for clip in input_clips_at(project, timeline, sample_rate, depth) {
        (clip.track, clip.start, clip.body).hash(&mut h);
    }
    for track in &timeline.tracks {
        format!("{:?}", track.mix).hash(&mut h);
    }
    format!("{:?}", timeline.master).hash(&mut h);
    h.finish()
}

pub(crate) fn hash_channel(channel: &Channel, h: &mut impl Hasher) {
    let bus = &channel.bus;
    (bus.gain.to_bits(), bus.balance.map(f32::to_bits)).hash(h);
    hash_inserts(&channel.inserts, h);
}

pub(crate) fn hash_inserts(inserts: &[Insert], h: &mut impl Hasher) {
    for insert in inserts {
        hash_insert(insert, h);
    }
}

pub(crate) fn has_state(inserts: &[Insert]) -> bool {
    inserts
        .iter()
        .any(|insert| matches!(insert, Insert::Compressor(_) | Insert::Eq(_)))
}

/// The segments of `inputs` between their edit points; the silence
/// between them is not measured. `base`: what every segment shares (the
/// mode, the format, the chain measured through); `track_key`: what the
/// track of a clip adds to it before the measure. `stateful`: something
/// before the measure keeps a state across the edit points, so each
/// segment is rendered with a preroll and keyed with the one before it.
pub(crate) fn level_segments(
    inputs: &[&InputClip],
    base: u64,
    track_key: impl Fn(usize) -> u64,
    stateful: bool,
) -> Vec<LevelSegment> {
    let mut edges: Vec<u64> = inputs
        .iter()
        .flat_map(|clip| [clip.start, clip.start + clip.len])
        .collect();
    edges.sort_unstable();
    edges.dedup();
    let mut segments = Vec::new();
    let mut previous: Option<u64> = None;
    for pair in edges.windows(2) {
        let (start, end) = (pair[0], pair[1]);
        let mut playing: Vec<&&InputClip> = inputs
            .iter()
            .filter(|clip| clip.start < end && clip.start + clip.len > start)
            .collect();
        if playing.is_empty() {
            continue;
        }
        playing.sort_by_key(|clip| (clip.track, clip.start, clip.id));
        let mut h = StableHasher::default();
        (base, end - start).hash(&mut h);
        for clip in playing {
            let from_start = clip.start as i64 - start as i64;
            (track_key(clip.track), from_start, clip.body).hash(&mut h);
        }
        let content = h.finish();
        let key = if stateful {
            stable_hash((content, previous))
        } else {
            content
        };
        segments.push(LevelSegment {
            frames: start..end,
            preroll: if stateful {
                STATE_PREROLL_FRAMES.min(start)
            } else {
                0
            },
            key,
        });
        previous = Some(content);
    }
    segments
}

/// Whether a measure needs a preroll: an effect before it, or a compound
/// clip (whose own effects may keep a state).
pub(crate) fn inputs_have_state(inputs: &[&InputClip]) -> bool {
    inputs.iter().any(|clip| clip.compound)
}

#[cfg(test)]
#[path = "tests/levels.rs"]
mod tests;
