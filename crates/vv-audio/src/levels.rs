//! What a normalization measures, cut into segments at the edit points of
//! its input and keyed by their content in their own time, so that an edit
//! measures again only the segments it touches (plans/AUDIO_LIVE_MIX.md
//! §3.3). The segments are measured on their own and merged
//! (`LevelParts::merge`).

use std::hash::{Hash, Hasher};
use std::ops::Range;

use vv_core::{
    AudioEffectKind, ChannelStrip, ClipId, ClipSource, MixerChannel, NormalizeMode, Project,
    Timeline, Vst3Effect,
};

use crate::loudness::LevelMeter;
use crate::mixer::{
    AnalysisSlot, AudioSource, Channel, ClipSpan, Insert, MixSnapshot, MixState, hash_insert,
    mix_range_from,
};

/// Rendered before a segment when the effects before the measure keep a
/// state: they reach about the state they would have in a render from the
/// start.
const STATE_PREROLL_FRAMES: u64 = 2 * crate::mixer::PROJECT_SAMPLE_RATE as u64;

pub use vv_core::StableHasher;
pub(crate) use vv_core::stable_hash;

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
    /// Placements the live pool did not make when `segments` were keyed:
    /// the measure plays them pass-through to match its keys.
    pub dry: Vec<u64>,
    /// Of the timeline the strip is in: part of its plugins' keys.
    pub scope: u64,
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
            self.scope,
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
    /// A compound clip whose timeline holds an installed plugin.
    pub plugin: bool,
}

/// The clips of the audible tracks of `timeline`, whether their audio is
/// decoded or not.
pub(crate) fn input_clips(
    project: &Project,
    timeline: &Timeline,
    sample_rate: u32,
    available: &mut dyn FnMut(&Vst3Effect) -> Option<u64>,
) -> Vec<InputClip> {
    input_clips_at(project, timeline, sample_rate, 0, available)
}

fn input_clips_at(
    project: &Project,
    timeline: &Timeline,
    sample_rate: u32,
    depth: u32,
    available: &mut dyn FnMut(&Vst3Effect) -> Option<u64>,
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
                Some(nested) => timeline_key(project, nested, sample_rate, depth + 1, available),
                None => stable_hash((item.content_hash, clip.audio_stream_index)),
            };
            let body = stable_hash((
                (span.len, span.source_offset, span.step.to_bits()),
                (span.fade_in, span.fade_out, clip.pitch_correction),
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
                plugin: nested
                    .is_some_and(|n| holds_available_plugin(project, n, depth + 1, available)),
            });
        }
    }
    clips
}

/// Whether `timeline` holds an enabled installed plugin, in itself or in
/// the compounds it plays. Conservative: it also counts muted tracks and
/// disabled clips.
fn holds_available_plugin(
    project: &Project,
    timeline: &Timeline,
    depth: u32,
    available: &mut dyn FnMut(&Vst3Effect) -> Option<u64>,
) -> bool {
    if depth >= vv_core::MAX_COMPOUND_DEPTH {
        return false;
    }
    for strip in timeline.strips() {
        for effect in &strip.effects {
            if let AudioEffectKind::Vst3(plugin) = &effect.kind
                && effect.enabled
                && available(plugin).is_some()
            {
                return true;
            }
        }
    }
    timeline
        .tracks
        .iter()
        .flat_map(|track| &track.clips)
        .filter_map(|clip| match &clip.source {
            ClipSource::Media(id) => project.media_pool.get(*id)?.compound,
            _ => None,
        })
        .filter_map(|id| project.timelines.get(id))
        .any(|nested| holds_available_plugin(project, nested, depth + 1, available))
}

/// The content of a compound clip's timeline, in its own time.
pub(crate) fn timeline_key(
    project: &Project,
    timeline: &Timeline,
    sample_rate: u32,
    depth: u32,
    available: &mut dyn FnMut(&Vst3Effect) -> Option<u64>,
) -> u64 {
    if depth >= vv_core::MAX_COMPOUND_DEPTH {
        return 0;
    }
    let mut h = StableHasher::default();
    for clip in input_clips_at(project, timeline, sample_rate, depth, available) {
        (clip.track, clip.start, clip.body).hash(&mut h);
    }
    for strip in timeline.strips() {
        hash_strip(strip, available, &mut h);
    }
    h.finish()
}

/// Plugin-free strips keep their Debug hash (the keys are on disk). With a
/// plugin, Debug would include its id and name: a fresh-id copy must match.
fn hash_strip(
    strip: &ChannelStrip,
    available: &mut dyn FnMut(&Vst3Effect) -> Option<u64>,
    h: &mut StableHasher,
) {
    let has_plugin = strip
        .effects
        .iter()
        .any(|e| matches!(e.kind, AudioEffectKind::Vst3(_)));
    if !has_plugin {
        format!("{strip:?}").hash(h);
        return;
    }
    // Destructured so a new strip field cannot be left out of the key.
    let ChannelStrip {
        gain_db,
        pan,
        effects,
    } = strip;
    (gain_db.to_bits(), pan.to_bits()).hash(h);
    for effect in effects {
        effect.enabled.hash(h);
        let AudioEffectKind::Vst3(plugin) = &effect.kind else {
            format!("{:?}", effect.kind).hash(h);
            continue;
        };
        plugin.content_hash().hash(h);
        let build = available(plugin);
        build.is_some().hash(h);
        if let Some(build) = build {
            build.hash(h);
        }
    }
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
    inserts.iter().any(|insert| {
        matches!(
            insert,
            Insert::Compressor(_) | Insert::Eq(_) | Insert::Plugin(_)
        )
    })
}

/// How long what is before a measure remembers its input.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Memory {
    /// The compressor and the equalizer settle within a preroll; `true`: one
    /// is before the measure.
    Bounded(bool),
    /// A plugin (delay, reverb, AGC) may remember all of it, and its tails
    /// ring through the gaps.
    Unbounded,
}

impl Memory {
    pub(crate) fn of(plugin: bool, bounded_state: bool) -> Self {
        if plugin {
            Self::Unbounded
        } else {
            Self::Bounded(bounded_state)
        }
    }
}

/// Whether one of `inserts` is a plugin that processes.
pub(crate) fn has_processing_plugin(inserts: &[Insert]) -> bool {
    inserts
        .iter()
        .any(|insert| matches!(insert, Insert::Plugin(plugin) if plugin.handle.is_some()))
}

/// The segments of `inputs` between their edit points; the silence
/// between them is not measured. `base`: what every segment shares (the
/// mode, the format, the chain measured through); `track_key`: what the
/// track of a clip adds to it before the measure. `memory`: what is before
/// the measure keeps a state across the edit points, so each segment is
/// rendered with a preroll and keyed with the one before it; or it keeps
/// all of it, and there is one segment from the start through the gaps,
/// keyed by every input. `mix_end`: where the last clip of the timeline ends.
pub(crate) fn level_segments(
    inputs: &[&InputClip],
    base: u64,
    track_key: impl Fn(usize) -> u64,
    memory: Memory,
    mix_end: u64,
) -> Vec<LevelSegment> {
    let stateful = match memory {
        Memory::Unbounded => return continuous_segment(inputs, base, track_key, mix_end),
        Memory::Bounded(stateful) => stateful,
    };
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

/// Rendered whole from frame 0 to the end of the mix (`mix_end`: the tail
/// of a strip rings until then), as the export does: any edit anywhere is a
/// new key.
fn continuous_segment(
    inputs: &[&InputClip],
    base: u64,
    track_key: impl Fn(usize) -> u64,
    mix_end: u64,
) -> Vec<LevelSegment> {
    if inputs.is_empty() {
        return Vec::new();
    }
    let end = mix_end;
    let mut sorted = inputs.to_vec();
    sorted.sort_by_key(|clip| (clip.track, clip.start, clip.id));
    let mut h = StableHasher::default();
    (base, end, "continuous").hash(&mut h);
    for clip in sorted {
        (
            track_key(clip.track),
            clip.track,
            clip.start,
            clip.len,
            clip.body,
        )
            .hash(&mut h);
    }
    vec![LevelSegment {
        frames: 0..end,
        preroll: 0,
        key: h.finish(),
    }]
}

/// Whether a measure needs a preroll: an effect before it, or a compound
/// clip (whose own effects may keep a state).
pub(crate) fn inputs_have_state(inputs: &[&InputClip]) -> bool {
    inputs.iter().any(|clip| clip.compound)
}

#[cfg(test)]
#[path = "tests/levels.rs"]
mod tests;
