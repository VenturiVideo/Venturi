//! Continuous mixer of the timeline audio tracks: an immutable snapshot
//! of the clips (built by the UI thread) + `mix_range`, a pure function used
//! both by the cpal callback and by the export, so preview and export
//! produce the same mix.

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use vv_core::{
    AudioEffectKind, ChannelStrip, ClipId, ClipSource, EffectId, Equalizer, FrameIdx, Keyframed,
    MediaId, MixerChannel, MultibandCompressor, NormalizeMode, Project, SetLevel, Timeline,
    TimelineId, Vst3Effect,
};

use crate::blocks::{Run, SampleBlocks};
use crate::dynamics::{BandActivity, MultibandProcessor};
use crate::effect::{Effect, PluginEffect, PluginHandle, PluginRequest};
use crate::eq::EqProcessor;
use crate::levels::{
    InputClip, LevelAnalysis, Measured, Memory, StableHasher, has_processing_plugin, has_state,
    hash_channel, hash_inserts, input_clips, inputs_have_state, level_segments, stable_hash,
    timeline_key,
};
use crate::spectrum::SpectrumTap;
use crate::stretched::{StretchInput, StretchRequest};

pub const PROJECT_SAMPLE_RATE: u32 = 48_000;
/// Control-rate (~60Hz) granularity of the keyframed gain, not sample-accurate.
pub const GAIN_BLOCK_FRAMES: u64 = 800;
/// The mix goes track by track through buffers of this many frames.
pub const MIX_BLOCK_FRAMES: usize = 512;

#[derive(Clone)]
pub struct MixClip {
    /// Matches the mix state of a compound clip across snapshots
    /// (`MixState::continue_from`), and a clip measured alone.
    pub clip: ClipId,
    /// In timeline audio frames (one sample per channel).
    pub start: u64,
    pub len: u64,
    /// Audio frame of `buffer` corresponding to `start`.
    pub source_offset: u64,
    /// Frames of `buffer` per timeline frame: the clip speed (varispeed),
    /// 1 when `buffer` is already stretched.
    pub step: f64,
    pub input: ClipInput,
    pub gain_db: Keyframed<f32>,
    /// Timeline index of the track: its entry in `MixSnapshot::tracks`.
    pub track: usize,
    pub clip_fps: f64,
    /// Audio frame of the media at `start` and media frames per timeline
    /// frame: where the gain keyframes are read, even from a stretched copy.
    pub media_offset: u64,
    pub media_step: f64,
    /// Fades, in timeline audio frames from the start/end of
    /// `len` (see `Clip::fade_multiplier_at`, same semantics on samples).
    pub fade_in: u64,
    pub fade_out: u64,
}

/// What a clip plays, its frames counted like `MixClip::source_offset`.
#[derive(Clone)]
pub enum ClipInput {
    /// Interleaved at the snapshot's `sample_rate`/`channels`: it may hold
    /// only the part read.
    Blocks(SampleBlocks),
    /// A compound clip's timeline, mixed live in its own time.
    Nested(NestedMix),
}

#[derive(Clone)]
pub struct NestedMix {
    pub mix: Arc<MixSnapshot>,
}

/// A mixer strip, ready for the mix.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Bus {
    pub gain: f32,
    /// Multipliers of the left and right channel.
    pub balance: [f32; 2],
}

impl Bus {
    pub const UNITY: Bus = Bus {
        gain: 1.0,
        balance: [1.0, 1.0],
    };

    pub fn from_strip(strip: &ChannelStrip) -> Self {
        Self {
            gain: db_to_linear(strip.gain_db),
            balance: balance_gains(strip.pan),
        }
    }

    /// Multiplier of output channel `c` of `channels`: the balance only
    /// touches the first two, and nothing in mono.
    fn channel_gain(&self, c: usize, channels: usize) -> f32 {
        if channels < 2 || c >= 2 {
            self.gain
        } else {
            self.gain * self.balance[c]
        }
    }
}

/// Balance, not a pan law: the centre keeps both channels at unity, moving
/// towards a side only attenuates the other one.
pub fn balance_gains(pan: f32) -> [f32; 2] {
    let pan = pan.clamp(-1.0, 1.0);
    [(1.0 - pan).min(1.0), (1.0 + pan).min(1.0)]
}

/// Peak of a left/right pair since the last `take`. Non-negative floats
/// order like their bits, so `fetch_max` works on them.
#[derive(Debug, Default)]
pub struct StereoPeak([AtomicU32; 2]);

impl StereoPeak {
    fn raise(&self, peak: [f32; 2]) {
        for (slot, value) in self.0.iter().zip(peak) {
            slot.fetch_max(value.to_bits(), Ordering::Relaxed);
        }
    }

    pub fn take(&self) -> (f32, f32) {
        let [l, r] = &self.0;
        (
            f32::from_bits(l.swap(0, Ordering::Relaxed)),
            f32::from_bits(r.swap(0, Ordering::Relaxed)),
        )
    }
}

/// Post-fader peaks of what the output stream played.
#[derive(Debug, Default)]
pub struct MixMeters {
    /// Indexed like `MixSnapshot::tracks`.
    tracks: Vec<StereoPeak>,
    master: StereoPeak,
    /// Per channel and insert, like the chains of `MixSnapshot::tracks`.
    track_inserts: Vec<Vec<InsertMeter>>,
    master_inserts: Vec<InsertMeter>,
}

#[derive(Debug, Default)]
struct InsertMeter {
    bands: BandMeter,
    /// Compressors and equalizers only.
    spectrum: Option<Arc<SpectrumTap>>,
}

impl MixMeters {
    fn new(tracks: &[Channel], master: &Channel) -> Self {
        let inserts = |channel: &Channel| {
            channel
                .inserts
                .iter()
                .map(|insert| InsertMeter {
                    bands: BandMeter::default(),
                    spectrum: matches!(insert, Insert::Compressor(_) | Insert::Eq(_))
                        .then(|| Arc::new(SpectrumTap::default())),
                })
                .collect()
        };
        Self {
            tracks: tracks.iter().map(|_| StereoPeak::default()).collect(),
            master: StereoPeak::default(),
            track_inserts: tracks.iter().map(inserts).collect(),
            master_inserts: inserts(master),
        }
    }

    fn insert(&self, channel: MixerChannel, index: usize) -> Option<&InsertMeter> {
        match channel {
            MixerChannel::Track(track) => self.track_inserts.get(track)?.get(index),
            MixerChannel::Master => self.master_inserts.get(index),
        }
    }

    /// Takes over the spectra of `previous` where an effect is still there:
    /// a new snapshot for every edit would otherwise empty them all the time.
    pub fn keep_spectra_of(&mut self, previous: &MixMeters) {
        let pairs = self
            .track_inserts
            .iter_mut()
            .zip(&previous.track_inserts)
            .chain([(&mut self.master_inserts, &previous.master_inserts)]);
        for (chain, old_chain) in pairs {
            for (meter, old) in chain.iter_mut().zip(old_chain) {
                if let (Some(spectrum), Some(old)) = (&mut meter.spectrum, &old.spectrum) {
                    *spectrum = old.clone();
                }
            }
        }
    }

    /// Before and after the effect at `index` in the chain of `channel`.
    pub fn spectrum(&self, channel: MixerChannel, index: usize) -> Option<&SpectrumTap> {
        self.insert(channel, index)?.spectrum.as_deref()
    }

    pub fn track(&self, index: usize) -> Option<&StereoPeak> {
        self.tracks.get(index)
    }

    pub fn master(&self) -> &StereoPeak {
        &self.master
    }

    /// Of the effect at `index` in the chain of `channel`.
    pub fn band_meter(&self, channel: MixerChannel, index: usize) -> Option<&BandMeter> {
        Some(&self.insert(channel, index)?.bands)
    }

    fn track_inserts(&self, track: usize) -> &[InsertMeter] {
        self.track_inserts.get(track).map_or(&[], Vec::as_slice)
    }
}

/// Level and gain reduction of each band of a compressor since the last
/// `take`.
#[derive(Debug, Default)]
pub struct BandMeter {
    level: [AtomicU32; 3],
    reduction_db: [AtomicU32; 3],
}

impl BandMeter {
    fn raise(&self, activity: BandActivity) {
        let pairs = [
            (&self.level, activity.level),
            (&self.reduction_db, activity.reduction_db),
        ];
        for (slots, values) in pairs {
            for (slot, value) in slots.iter().zip(values) {
                slot.fetch_max(value.max(0.0).to_bits(), Ordering::Relaxed);
            }
        }
    }

    pub fn take(&self) -> BandActivity {
        let take = |slots: &[AtomicU32; 3]| {
            slots
                .each_ref()
                .map(|slot| f32::from_bits(slot.swap(0, Ordering::Relaxed)))
        };
        BandActivity {
            level: take(&self.level),
            reduction_db: take(&self.reduction_db),
        }
    }
}

/// An effect of a chain, ready for the mix: a normalization is the gain it
/// measured.
#[derive(Debug, Clone, PartialEq)]
pub enum Insert {
    /// A disabled effect, or one with nothing to act on: it keeps the
    /// inserts aligned with the effects of the strip.
    Off,
    Gain(f32),
    /// By timeline audio frame each gain starts at, ascending: one per clip
    /// of the track, held until the next one (the first also before it).
    ClipGains(Vec<(u64, f32)>),
    Compressor(MultibandCompressor),
    Mono,
    Eq(Equalizer),
    Plugin(PluginInsert),
}

/// A hosted plugin in a chain. Equality is by what it plays, not by the
/// instance, but whether it processes counts: `handle` is `None` when the
/// source has none (it passes through).
#[derive(Clone)]
pub struct PluginInsert {
    pub key: u64,
    pub content: u64,
    pub handle: Option<PluginHandle>,
}

impl PartialEq for PluginInsert {
    fn eq(&self, other: &Self) -> bool {
        (self.key, self.content, self.handle.is_some())
            == (other.key, other.content, other.handle.is_some())
    }
}

impl std::fmt::Debug for PluginInsert {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PluginInsert")
            .field("key", &self.key)
            .field("content", &self.content)
            .field("processes", &self.handle.is_some())
            .finish()
    }
}

/// A mixer strip, ready for the mix: its inserts, then fader and balance.
#[derive(Debug, Clone, PartialEq)]
pub struct Channel {
    pub inserts: Vec<Insert>,
    pub bus: Bus,
}

impl Channel {
    pub const UNITY: Channel = Channel {
        inserts: Vec::new(),
        bus: Bus::UNITY,
    };
}

impl Default for Channel {
    fn default() -> Self {
        Self::UNITY
    }
}

pub struct MixSnapshot {
    pub sample_rate: u32,
    pub channels: u16,
    /// Sorted by track.
    pub clips: Vec<MixClip>,
    /// Where a pitch-corrected clip is left out while its stretch is still
    /// coming: silent, and not in `clips` for `is_ready` to see.
    pending: Vec<std::ops::Range<u64>>,
    /// Track and range of `clips` of each track with clips.
    groups: Vec<(usize, std::ops::Range<usize>)>,
    /// Indexed by timeline track index; a clip whose track is missing plays
    /// at unity.
    pub tracks: Vec<Channel>,
    pub master: Channel,
    pub meters: MixMeters,
}

impl MixSnapshot {
    pub fn empty(sample_rate: u32, channels: u16) -> Self {
        Self::new(
            sample_rate,
            channels,
            Vec::new(),
            Vec::new(),
            Channel::UNITY,
        )
    }

    pub fn new(
        sample_rate: u32,
        channels: u16,
        mut clips: Vec<MixClip>,
        tracks: Vec<Channel>,
        master: Channel,
    ) -> Self {
        clips.sort_by_key(|c| c.track);
        let mut groups: Vec<(usize, std::ops::Range<usize>)> = Vec::new();
        for (i, clip) in clips.iter().enumerate() {
            match groups.last_mut() {
                Some((track, range)) if *track == clip.track => range.end = i + 1,
                _ => groups.push((clip.track, i..i + 1)),
            }
        }
        let meters = MixMeters::new(&tracks, &master);
        Self {
            sample_rate,
            channels,
            clips,
            pending: Vec::new(),
            groups,
            tracks,
            master,
            meters,
        }
    }

    /// Runs the chains of `tracks` that have no clip here on silence, so
    /// their tails ring through gaps and across offline chunks.
    fn keep_running(&mut self, tracks: &[usize]) {
        for &track in tracks {
            if !self.groups.iter().any(|(t, _)| *t == track) {
                self.groups.push((track, 0..0));
            }
        }
        self.groups.sort_by_key(|(track, _)| *track);
    }

    /// Clips whose audio is `Pending` are silent.
    pub fn from_timeline(
        project: &Project,
        timeline: &Timeline,
        sample_rate: u32,
        channels: u16,
        source: &mut impl AudioSource,
    ) -> Self {
        collect_clips(
            project,
            timeline,
            sample_rate,
            channels,
            source,
            None,
            0,
            0,
            0,
            None,
        )
        .0
    }

    /// Only for mixing the timeline audio frames `range`: the clips outside
    /// it are left out, and the others ask `AudioSource::file_frames` for
    /// the part they read.
    pub fn from_timeline_range(
        project: &Project,
        timeline: &Timeline,
        sample_rate: u32,
        channels: u16,
        source: &mut impl AudioSource,
        range: std::ops::Range<u64>,
    ) -> Self {
        collect_clips(
            project,
            timeline,
            sample_rate,
            channels,
            source,
            Some(range),
            0,
            0,
            0,
            None,
        )
        .0
    }

    /// `timeline` as a compound clip at `depth` of nesting mixes it, over
    /// `range` of its own time.
    pub fn nested_range(
        project: &Project,
        timeline: &Timeline,
        depth: u32,
        scope: u64,
        sample_rate: u32,
        channels: u16,
        source: &mut impl AudioSource,
        range: std::ops::Range<u64>,
    ) -> Self {
        collect_clips(
            project,
            timeline,
            sample_rate,
            channels,
            source,
            Some(range),
            depth,
            0,
            scope,
            None,
        )
        .0
    }

    /// Frames `range` of a file's `stream`, at their own time.
    pub fn of_file(
        sample_rate: u32,
        channels: u16,
        source: &mut impl AudioSource,
        path: &Path,
        stream: usize,
        range: std::ops::Range<u64>,
    ) -> Self {
        let buffer = match source.file_frames(path, stream, range.clone(), range.start) {
            ClipAudio::Ready(buffer) | ClipAudio::Partial(buffer) => buffer,
            ClipAudio::Pending | ClipAudio::Missing => return Self::empty(sample_rate, channels),
        };
        let clip = MixClip {
            clip: ClipId(0),
            start: range.start,
            len: range.end - range.start,
            source_offset: range.start,
            step: 1.0,
            input: ClipInput::Blocks(buffer),
            gain_db: Keyframed::constant(0.0),
            track: 0,
            clip_fps: 1.0,
            media_offset: range.start,
            media_step: 1.0,
            fade_in: 0,
            fade_out: 0,
        };
        Self::new(
            sample_rate,
            channels,
            vec![clip],
            Vec::new(),
            Channel::UNITY,
        )
    }

    /// What `measured` measures over `range` (all of it with `None`): its
    /// strip up to the normalization, alone and before the fader.
    pub fn measured(
        project: &Project,
        timeline: &Timeline,
        measured: Measured,
        sample_rate: u32,
        channels: u16,
        source: &mut impl AudioSource,
        range: Option<std::ops::Range<u64>>,
        scope: u64,
    ) -> Self {
        collect_clips(
            project,
            timeline,
            sample_rate,
            channels,
            source,
            range,
            0,
            0,
            scope,
            Some(measured),
        )
        .0
    }

    /// Whether every clip playing in `range` has its audio there: a render
    /// of it now equals the render once everything is decoded. The preview
    /// hands in a clip still decoding as `ClipAudio::Partial`, even with no
    /// block yet, for it to be seen here.
    pub fn is_ready(&self, range: std::ops::Range<u64>) -> bool {
        if self
            .pending
            .iter()
            .any(|p| p.start < range.end && p.end > range.start)
        {
            return false;
        }
        self.clips
            .iter()
            .filter(|clip| clip.start < range.end && clip.start + clip.len > range.start)
            .all(|clip| {
                let frames = range.start.max(clip.start)..range.end.min(clip.start + clip.len);
                let read = nested_frames(clip, frames);
                match &clip.input {
                    ClipInput::Blocks(buffer) => buffer.holds(read),
                    ClipInput::Nested(nested) => nested.mix.is_ready(read),
                }
            })
    }

    fn has_plugins(&self) -> bool {
        let (tracks, master) = self.skip_from();
        master != usize::MAX || tracks.iter().any(|&t| t != usize::MAX)
    }

    /// Per track and for the master, the insert from which a normalize gain
    /// was measured through a processing plugin, or `usize::MAX`.
    fn skip_from(&self) -> (Vec<usize>, usize) {
        let own = |channel: &Channel| {
            channel
                .inserts
                .iter()
                .position(|i| matches!(i, Insert::Plugin(p) if p.handle.is_some()))
                .unwrap_or(usize::MAX)
        };
        let mut tracks: Vec<usize> = self.tracks.iter().map(own).collect();
        for (track, range) in &self.groups {
            let nested = self.clips[range.clone()]
                .iter()
                .any(|clip| matches!(&clip.input, ClipInput::Nested(n) if n.mix.has_plugins()));
            if let (true, Some(t)) = (nested, tracks.get_mut(*track)) {
                *t = 0;
            }
        }
        let master = if tracks.iter().any(|&t| t != usize::MAX) {
            0
        } else {
            own(&self.master)
        };
        (tracks, master)
    }

    /// Whether the chain of `track` may output something once its input is
    /// silent: a processing plugin, here or in a compound clip. The
    /// compressor and the equalizer give silence back once they have settled.
    fn track_rings(&self, track: usize) -> bool {
        has_processing_plugin(&self.track(track).inserts)
            || self.groups.iter().any(|(t, range)| {
                *t == track
                    && self.clips[range.clone()].iter().any(
                        |clip| matches!(&clip.input, ClipInput::Nested(n) if n.mix.has_plugins()),
                    )
            })
    }

    fn settle_frames(&self) -> u64 {
        2 * self.sample_rate as u64
    }

    fn track(&self, index: usize) -> &Channel {
        static UNITY: Channel = Channel::UNITY;
        self.tracks.get(index).unwrap_or(&UNITY)
    }
}

impl Insert {
    /// What runs it, if it keeps a state from one block to the next.
    fn effect(&self, sample_rate: u32, channels: u16, plugins: bool) -> Option<Box<dyn Effect>> {
        match self {
            Insert::Off | Insert::Gain(_) | Insert::ClipGains(_) | Insert::Mono => None,
            Insert::Compressor(params) => Some(Box::new(MultibandProcessor::new(
                params,
                sample_rate,
                channels,
            ))),
            Insert::Eq(params) => Some(Box::new(EqProcessor::new(params, sample_rate, channels))),
            Insert::Plugin(plugin) => plugin.handle.clone().filter(|_| plugins).map(|handle| {
                Box::new(PluginEffect {
                    handle,
                    channels: channels as usize,
                }) as Box<dyn Effect>
            }),
        }
    }
}

/// Below this a chain's output counts as silence.
const SILENCE: f32 = 1e-6;

/// Whether a track without a plugin may skip its blocks: its last clip
/// ended `last_end` and the output of its chain has gone quiet.
#[derive(Clone, Copy)]
struct Idle {
    last_end: u64,
    settled: bool,
    ringing: bool,
}

impl Idle {
    const SETTLED: Self = Self {
        last_end: 0,
        settled: true,
        ringing: false,
    };
}

/// The memory of the mix between two calls: the state of the effects that
/// have one, and where the previous call ended. A call that does not follow
/// on starts them over.
pub struct MixState {
    /// Aligned with the inserts of `MixSnapshot::tracks`.
    tracks: Vec<Vec<Option<Box<dyn Effect>>>>,
    master: Vec<Option<Box<dyn Effect>>>,
    /// Aligned with `MixSnapshot::clips`: the compound clips' own.
    nested: Vec<Option<NestedState>>,
    scratch: Vec<f32>,
    peaks: Vec<[f32; 2]>,
    next: Option<u64>,
    /// First insert of each track (and of the master) whose gains were
    /// measured through a plugin that passes through here: they are dropped.
    track_skip: Vec<usize>,
    master_skip: usize,
    /// Per track, for those not in `rings`.
    idle: Vec<Idle>,
    /// Per track: a plugin may ring on after the input stops, so the chain
    /// runs through the gaps.
    rings: Vec<bool>,
    #[cfg(test)]
    skipped_blocks: usize,
}

impl MixState {
    pub fn new(snapshot: &MixSnapshot) -> Self {
        Self::build(snapshot, true)
    }

    /// Without the hosted plugins, which pass through: for a thread other
    /// than the one playing them, as an instance takes one caller at a time.
    pub fn without_plugins(snapshot: &MixSnapshot) -> Self {
        Self::build(snapshot, false)
    }

    fn build(snapshot: &MixSnapshot, plugins: bool) -> Self {
        let (rate, channels) = (snapshot.sample_rate, snapshot.channels);
        let (track_skip, master_skip) = if plugins {
            (vec![usize::MAX; snapshot.tracks.len()], usize::MAX)
        } else {
            snapshot.skip_from()
        };
        let processors = |channel: &Channel| -> Vec<Option<Box<dyn Effect>>> {
            channel
                .inserts
                .iter()
                .map(|insert| insert.effect(rate, channels, plugins))
                .collect()
        };
        let rings = (0..snapshot.tracks.len())
            .map(|track| snapshot.track_rings(track))
            .collect();
        Self {
            tracks: snapshot.tracks.iter().map(processors).collect(),
            master: processors(&snapshot.master),
            nested: snapshot
                .clips
                .iter()
                .map(|clip| match &clip.input {
                    ClipInput::Nested(nested) => Some(NestedState::new(clip, nested, plugins)),
                    ClipInput::Blocks(_) => None,
                })
                .collect(),
            scratch: vec![0.0; MIX_BLOCK_FRAMES * snapshot.channels.max(1) as usize],
            peaks: vec![[0.0; 2]; snapshot.tracks.len()],
            track_skip,
            master_skip,
            idle: vec![Idle::SETTLED; snapshot.tracks.len()],
            rings,
            #[cfg(test)]
            skipped_blocks: 0,
            next: None,
        }
    }

    /// Carries on from the state of the previous snapshot: the effects
    /// found at the same place keep their memory, so an edit does not
    /// restart them.
    pub fn continue_from(&mut self, previous: &MixState) {
        let pairs = self
            .tracks
            .iter_mut()
            .zip(&previous.tracks)
            .chain([(&mut self.master, &previous.master)]);
        for (chain, old_chain) in pairs {
            for (processor, old) in chain.iter_mut().zip(old_chain) {
                if let (Some(processor), Some(old)) = (processor, old) {
                    processor.take_memory_from(old.as_ref());
                }
            }
        }
        for nested in self.nested.iter_mut().flatten() {
            let old = previous
                .nested
                .iter()
                .flatten()
                .find(|old| old.key == nested.key);
            if let Some(old) = old {
                nested.continue_from(old);
            }
        }
        self.next = previous.next;
        for (idle, old) in self.idle.iter_mut().zip(&previous.idle) {
            *idle = *old;
        }
    }

    fn reset(&mut self) {
        for processor in self
            .tracks
            .iter_mut()
            .chain([&mut self.master])
            .flatten()
            .flatten()
        {
            processor.reset();
        }
        for nested in self.nested.iter_mut().flatten() {
            nested.rendered.clear();
        }
        self.idle.fill(Idle::SETTLED);
    }
}

/// The live mix of a compound clip: its timeline's state, and the frames
/// rendered last, which a varispeed read interpolates across calls.
struct NestedState {
    /// To find it again in the next snapshot. Unique in the project: not
    /// with the track, which a move or a track inserted above changes.
    key: ClipId,
    state: MixState,
    rendered: Vec<f32>,
    /// Nested frame of `rendered[0]`.
    from: u64,
    channels: usize,
}

impl NestedState {
    fn new(clip: &MixClip, nested: &NestedMix, plugins: bool) -> Self {
        let ch = nested.mix.channels.max(1) as usize;
        // What one mixer block reads, so the callback never allocates.
        let frames = (MIX_BLOCK_FRAMES as f64 * clip.step).ceil() as usize + clip.step as usize + 3;
        Self {
            key: clip.clip,
            state: MixState::build(&nested.mix, plugins),
            rendered: Vec::with_capacity(frames * ch),
            from: 0,
            channels: ch,
        }
    }

    /// Within the capacity: no allocations, it runs in the callback.
    fn continue_from(&mut self, previous: &NestedState) {
        self.state.continue_from(&previous.state);
        if self.channels != previous.channels {
            return;
        }
        let ch = self.channels;
        let kept = previous
            .rendered
            .len()
            .min(self.rendered.capacity() / ch * ch);
        self.rendered.clear();
        self.rendered.extend_from_slice(&previous.rendered[..kept]);
        self.from = previous.from;
        self.state.next = Some(self.from + (kept / ch) as u64);
    }

    /// The nested mix over `frames`, rendered in order: what was already
    /// rendered of them is kept, so the state stays sequential.
    fn render(&mut self, mix: &MixSnapshot, frames: std::ops::Range<u64>) -> Rendered<'_> {
        let ch = self.channels;
        let end = self.from + (self.rendered.len() / ch) as u64;
        if frames.start < self.from || frames.start > end {
            self.rendered.clear();
            self.from = frames.start;
        } else {
            let drop = (frames.start - self.from) as usize * ch;
            self.rendered.copy_within(drop.., 0);
            self.rendered.truncate(self.rendered.len() - drop);
            self.from = frames.start;
        }
        let have = self.from + (self.rendered.len() / ch) as u64;
        if frames.end > have {
            let old = self.rendered.len();
            self.rendered
                .resize(old + (frames.end - have) as usize * ch, 0.0);
            mix_into(mix, &mut self.state, have, &mut self.rendered[old..], false);
        }
        Rendered {
            samples: &self.rendered[..(frames.end - self.from) as usize * ch],
            from: self.from,
            channels: ch,
        }
    }
}

/// The frames a clip reads, from a file's blocks or a nested mix.
trait Frames {
    fn start(&self) -> u64;
    fn end(&self) -> u64;
    fn run(&self, frame: u64) -> Run<'_>;
    fn frame(&self, frame: u64) -> Option<&[f32]>;
}

impl Frames for SampleBlocks {
    fn start(&self) -> u64 {
        SampleBlocks::start(self)
    }

    fn end(&self) -> u64 {
        SampleBlocks::end(self)
    }

    fn run(&self, frame: u64) -> Run<'_> {
        SampleBlocks::run(self, frame)
    }

    fn frame(&self, frame: u64) -> Option<&[f32]> {
        SampleBlocks::frame(self, frame)
    }
}

struct Rendered<'a> {
    samples: &'a [f32],
    from: u64,
    channels: usize,
}

impl Frames for Rendered<'_> {
    fn start(&self) -> u64 {
        self.from
    }

    fn end(&self) -> u64 {
        self.from + (self.samples.len() / self.channels) as u64
    }

    fn run(&self, frame: u64) -> Run<'_> {
        if frame < self.from || frame >= self.end() {
            return Run::End;
        }
        Run::Samples(&self.samples[(frame - self.from) as usize * self.channels..])
    }

    fn frame(&self, frame: u64) -> Option<&[f32]> {
        match self.run(frame) {
            Run::Samples(samples) => Some(&samples[..self.channels]),
            Run::Silence(_) | Run::End => None,
        }
    }
}

/// What a normalization measures: while a new reading is on its way, the
/// same slot keeps playing with its last one. The channel, the position of
/// the normalization in its chain and the clip of the track (0 for the
/// whole master).
pub type AnalysisSlot = (TimelineId, MixerChannel, usize, usize);

pub(crate) fn hash_insert(insert: &Insert, h: &mut impl Hasher) {
    match insert {
        Insert::Off => 2u8.hash(h),
        Insert::Mono => 3u8.hash(h),
        Insert::Gain(gain) => (0u8, gain.to_bits()).hash(h),
        Insert::ClipGains(gains) => {
            5u8.hash(h);
            for (start, gain) in gains {
                (start, gain.to_bits()).hash(h);
            }
        }
        Insert::Compressor(params) => {
            (1u8, params.crossovers_hz.map(f32::to_bits)).hash(h);
            for band in &params.bands {
                let fields = [
                    band.threshold_db,
                    band.ratio,
                    band.attack_ms,
                    band.release_ms,
                    band.makeup_db,
                ];
                fields.map(f32::to_bits).hash(h);
            }
        }
        Insert::Plugin(plugin) => (6u8, plugin.content, plugin.handle.is_some()).hash(h),
        Insert::Eq(params) => {
            4u8.hash(h);
            for band in &params.bands {
                (band.enabled, band.shape as u8).hash(h);
                [band.freq_hz, band.gain_db, band.q]
                    .map(f32::to_bits)
                    .hash(h);
            }
        }
    }
}

pub enum LevelReading {
    Ready(f32),
    /// Being measured; the last reading of the same slot, if any.
    Pending(Option<f32>),
}

/// A clip's audio, as an `AudioSource` answers it.
pub enum ClipAudio {
    /// Already at the snapshot's `sample_rate`/`channels`.
    Ready(SampleBlocks),
    /// Like `Ready`, but with blocks still decoding: they play as silence.
    Partial(SampleBlocks),
    /// Still decoding, nothing yet: silent for now.
    Pending,
    /// No audio there (e.g. a stream index past the file's streams).
    Missing,
}

/// The decoded audio a mix is built from. Compound clips never reach `file`:
/// they are mixed live from their nested timeline.
pub trait AudioSource {
    fn file(&mut self, path: &Path, stream: usize) -> ClipAudio;

    /// `file` when only its frames `frames` are read: the buffer holds at
    /// least those, or stops at the end of the file. `plays_at`: about when
    /// the first of them plays, in samples of the timeline being mixed (of
    /// the outermost one, for a compound clip's).
    fn file_frames(
        &mut self,
        path: &Path,
        stream: usize,
        _frames: std::ops::Range<u64>,
        _plays_at: u64,
    ) -> ClipAudio {
        self.file(path, stream)
    }

    /// The audio a pitch-corrected clip plays, from its first frame.
    /// Synchronous here, whole and from this source; the preview and the
    /// export render it from the files.
    fn stretched(&mut self, request: StretchRequest<'_>) -> ClipAudio
    where
        Self: Sized,
    {
        request.stretch_with(self)
    }

    /// Synchronous here, whole and from this source; the preview and the
    /// export measure from the files, by segment.
    fn level(&mut self, analysis: LevelAnalysis<'_>) -> LevelReading
    where
        Self: Sized,
    {
        LevelReading::Ready(analysis.measure_with(self))
    }

    /// The processor of a plugin placement, `None` to play it as a
    /// pass-through. The same `key` asked again, in a later pass or in the
    /// same one (a level analysis through `self`), gets the same instance:
    /// the callers use it one after the other, never at once.
    fn plugin(&mut self, _: PluginRequest<'_>) -> Option<PluginHandle> {
        None
    }

    /// The installed build of `plugin`, `None` if it is not installed: part
    /// of every key of what it renders, so an update drops the cached
    /// measures. A nested timeline's keys cannot ask for its handles.
    fn plugin_build(&mut self, _: &Vst3Effect) -> Option<u64> {
        None
    }
}

pub fn stretch_range(
    buffer: &SampleBlocks,
    range: std::ops::Range<u64>,
    tempo: f64,
    sample_rate: u32,
    channels: u16,
) -> Result<Vec<f32>, String> {
    let range = range.start.max(buffer.start())..range.end.min(buffer.end());
    crate::stretch_samples(&buffer.to_vec(range), sample_rate, channels, tempo)
}

/// `None` is `Missing`; no compound cache.
impl<F: FnMut(&Path, usize) -> Option<SampleBlocks>> AudioSource for F {
    fn file(&mut self, path: &Path, stream: usize) -> ClipAudio {
        self(path, stream).map_or(ClipAudio::Missing, ClipAudio::Ready)
    }
}

/// The mix clips of `timeline`, and whether it contains itself through a
/// compound clip: the whole cycle stays silent instead of layering itself
/// `MAX_COMPOUND_DEPTH` times. `placement`: where its sample 0 plays in the
/// outermost timeline, for `plays_at`.
#[allow(clippy::too_many_arguments)]
fn collect_clips(
    project: &Project,
    timeline: &Timeline,
    sample_rate: u32,
    channels: u16,
    source: &mut impl AudioSource,
    range: Option<std::ops::Range<u64>>,
    depth: u32,
    placement: i64,
    scope: u64,
    measuring: Option<Measured>,
) -> (MixSnapshot, bool) {
    let fps = timeline.fps.as_f64().max(1e-9);
    let mut clips = Vec::new();
    let mut cyclic = false;
    let mut pending = Vec::new();
    for (track_index, track) in timeline.audible_tracks() {
        for clip in track
            .clips
            .iter()
            .filter(|c| !c.disabled && c.freeze.is_none())
        {
            if let Some(Measured {
                channel: MixerChannel::Track(measured_track),
                clip: measured_clip,
                ..
            }) = measuring
                && (track_index != measured_track || measured_clip.is_some_and(|id| id != clip.id))
            {
                continue;
            }
            let ClipSource::Media(media_id) = &clip.source else {
                continue;
            };
            let Some(item) = project.media_pool.get(*media_id) else {
                continue;
            };
            let pitch_corrected = clip.pitch_correction && !clip.speed().is_one();
            let span = ClipSpan::new(clip, fps, sample_rate);
            // The stretch starts from the start of the clip.
            let mixed = range.as_ref().filter(|_| !pitch_corrected);
            let Some(needed) = span.media_frames(mixed) else {
                continue;
            };
            let into_clip = needed.start.saturating_sub(span.source_offset) as f64 / span.step;
            let plays_at = (placement + span.start as i64 + into_clip as i64).max(0) as u64;
            let nested_placement = placement + span.start as i64 - span.source_offset as i64;
            let input = if pitch_corrected {
                let nested = item.compound.and_then(|id| project.timelines.get(id));
                if nested.is_some() && depth + 1 >= vv_core::MAX_COMPOUND_DEPTH {
                    cyclic = true;
                    continue;
                }
                let mut frames = span.source_offset
                    ..span.source_offset + (span.len as f64 * span.step).ceil() as u64;
                if let Some(timeline) = nested {
                    frames.end =
                        frames
                            .end
                            .min(audio_end(project, timeline, sample_rate, depth + 1));
                    if frames.is_empty() {
                        continue;
                    }
                }
                let nested_scope = stable_hash((scope, clip.id));
                let (input, content) = match nested {
                    Some(timeline) => (
                        StretchInput::Nested {
                            timeline,
                            depth: depth + 1,
                            scope: nested_scope,
                        },
                        timeline_key(project, timeline, sample_rate, depth + 1, &mut |effect| {
                            source.plugin_build(effect)
                        }),
                    ),
                    None => (
                        StretchInput::File {
                            path: &item.path,
                            stream: clip.audio_stream_index,
                        },
                        stable_hash((item.content_hash, clip.audio_stream_index)),
                    ),
                };
                let key = stable_hash((
                    content,
                    (frames.start, frames.end),
                    span.step.to_bits(),
                    (sample_rate, channels),
                ));
                let request = StretchRequest {
                    project,
                    input,
                    frames,
                    tempo: span.step,
                    sample_rate,
                    channels,
                    key,
                };
                match source.stretched(request) {
                    ClipAudio::Ready(stretched) => ClipInput::Blocks(stretched),
                    ClipAudio::Pending | ClipAudio::Partial(_) => {
                        pending.push(span.start..span.start + span.len);
                        continue;
                    }
                    ClipAudio::Missing => continue,
                }
            } else if item.compound.is_some() {
                let Some((mix, nested_cyclic)) = nested_mix(
                    project,
                    *media_id,
                    (sample_rate, channels),
                    source,
                    range.is_some().then(|| needed.clone()),
                    depth,
                    nested_placement,
                    stable_hash((scope, clip.id)),
                ) else {
                    continue;
                };
                if nested_cyclic {
                    cyclic = true;
                    continue;
                }
                ClipInput::Nested(NestedMix { mix: Arc::new(mix) })
            } else {
                match source.file_frames(
                    &item.path,
                    clip.audio_stream_index,
                    needed.clone(),
                    plays_at,
                ) {
                    ClipAudio::Ready(buffer) | ClipAudio::Partial(buffer) => {
                        ClipInput::Blocks(buffer)
                    }
                    ClipAudio::Pending | ClipAudio::Missing => continue,
                }
            };
            let clip_fps = item.meta.fps.as_f64().max(1e-9);
            let readable_to = match &input {
                ClipInput::Blocks(buffer) if !pitch_corrected => {
                    (buffer.end() < needed.end).then_some(buffer.end())
                }
                ClipInput::Nested(_) => item
                    .compound
                    .and_then(|id| project.timelines.get(id))
                    .and_then(|nested| {
                        compound_readable_to(project, nested, &span, sample_rate, depth)
                    }),
                ClipInput::Blocks(_) => None,
            };
            let Some(mut mix_clip) = mix_clip_from(clip, &span, clip_fps, input, readable_to)
            else {
                continue;
            };
            mix_clip.track = track_index;
            if pitch_corrected && let ClipInput::Blocks(stretched) = &mix_clip.input {
                // Already at the clip's speed, from its first frame.
                mix_clip.len = mix_clip.len.min(stretched.end());
                mix_clip.source_offset = 0;
                mix_clip.step = 1.0;
            }
            clips.push(mix_clip);
        }
    }
    let timeline_id = project
        .timelines
        .iter()
        .find(|(_, t)| std::ptr::eq(*t, timeline))
        .map(|(id, _)| id);
    let slot = |channel, insert, clip| timeline_id.map(|id| (id, channel, insert, clip));
    let inputs = input_clips(project, timeline, sample_rate, &mut |effect| {
        source.plugin_build(effect)
    });
    // Export renders to the timeline end, video included.
    let mix_end = inputs
        .iter()
        .map(|c| c.start + c.len)
        .max()
        .unwrap_or(0)
        .max(timeline_frame_to_sample(
            timeline.total_frames(),
            fps,
            sample_rate,
        ));
    let measured_channel = measuring.map(|m| m.channel);
    let format = (sample_rate, channels);
    let mut tracks: Vec<Channel> = Vec::with_capacity(timeline.tracks.len());
    let mut running = Vec::new();
    let mut plugin_keys = PluginKeys::new(scope, TRACK_STRIPS);
    for (index, track) in timeline.tracks.iter().enumerate() {
        let channel = MixerChannel::Track(index);
        if matches!(measured_channel, Some(MixerChannel::Track(t)) if t != index) {
            plugin_keys.count(&track.mix);
            tracks.push(Channel::UNITY);
            continue;
        }
        let stop = measuring.filter(|m| m.channel == channel).map(|m| m.insert);
        let track_inputs: Vec<&InputClip> = inputs.iter().filter(|c| c.track == index).collect();
        let mut track_clips: Vec<(ClipId, u64)> =
            track_inputs.iter().map(|c| (c.id, c.start)).collect();
        track_clips.sort_by_key(|&(_, start)| start);
        let plugins = request_plugins(&track.mix, stop, &mut plugin_keys, source, sample_rate);
        let analyze = |inserts: &[Insert], clip: Option<(usize, ClipId)>, mode| {
            let input: Vec<&InputClip> = match clip {
                Some((_, id)) => track_inputs
                    .iter()
                    .copied()
                    .filter(|c| c.id == id)
                    .collect(),
                None => track_inputs.clone(),
            };
            let memory = Memory::of(
                has_processing_plugin(inserts) || input.iter().any(|c| c.plugin),
                has_state(inserts) || inputs_have_state(&input),
            );
            let segments = level_segments(
                &input,
                chain_key(mode, format, inserts),
                |_| 0,
                memory,
                mix_end,
            );
            source.level(LevelAnalysis {
                slot: slot(channel, inserts.len(), clip.map_or(0, |(i, _)| i)),
                project,
                timeline,
                measured: Measured {
                    channel,
                    insert: inserts.len(),
                    clip: clip.map(|(_, id)| id),
                },
                mode,
                sample_rate,
                channels,
                segments,
                scope,
                dry: unmade_plugins(inserts),
            })
        };
        let inserts = resolve_chain(
            &track.mix,
            stop,
            !track_inputs.is_empty(),
            &track_clips,
            plugins,
            analyze,
        );
        if !track_inputs.is_empty() && has_state(&inserts) {
            running.push(index);
        }
        // Measured before the fader.
        let bus = match stop {
            Some(_) => Bus::UNITY,
            None => Bus::from_strip(&track.mix),
        };
        tracks.push(Channel { inserts, bus });
    }
    let master = if matches!(measured_channel, Some(MixerChannel::Track(_))) {
        Channel::UNITY
    } else {
        let stop = measuring.map(|m| m.insert);
        let all: Vec<&InputClip> = inputs.iter().collect();
        let tracks_have_state = all.iter().any(|clip| {
            tracks
                .get(clip.track)
                .is_some_and(|t| has_state(&t.inserts))
        });
        let tracks_have_plugin = all.iter().any(|clip| {
            tracks
                .get(clip.track)
                .is_some_and(|t| has_processing_plugin(&t.inserts))
        });
        let mut plugin_keys = PluginKeys::new(scope, MASTER_STRIPS);
        let plugins = request_plugins(
            &timeline.master,
            stop,
            &mut plugin_keys,
            source,
            sample_rate,
        );
        let analyze = |inserts: &[Insert], _: Option<(usize, ClipId)>, mode| {
            let memory = Memory::of(
                has_processing_plugin(inserts)
                    || tracks_have_plugin
                    || all.iter().any(|c| c.plugin),
                has_state(inserts) || tracks_have_state || inputs_have_state(&all),
            );
            let track_key = |track: usize| {
                let mut h = StableHasher::default();
                if let Some(channel) = tracks.get(track) {
                    hash_channel(channel, &mut h);
                }
                h.finish()
            };
            let segments = level_segments(
                &all,
                chain_key(mode, format, inserts),
                track_key,
                memory,
                mix_end,
            );
            source.level(LevelAnalysis {
                slot: slot(MixerChannel::Master, inserts.len(), 0),
                project,
                timeline,
                measured: Measured {
                    channel: MixerChannel::Master,
                    insert: inserts.len(),
                    clip: None,
                },
                mode,
                sample_rate,
                channels,
                segments,
                scope,
                dry: tracks
                    .iter()
                    .flat_map(|t| unmade_plugins(&t.inserts))
                    .chain(unmade_plugins(inserts))
                    .collect(),
            })
        };
        let inserts = resolve_chain(
            &timeline.master,
            stop,
            !all.is_empty(),
            &[],
            plugins,
            analyze,
        );
        let bus = match stop {
            Some(_) => Bus::UNITY,
            None => Bus::from_strip(&timeline.master),
        };
        Channel { inserts, bus }
    };
    let mut snapshot = MixSnapshot::new(sample_rate, channels, clips, tracks, master);
    snapshot.keep_running(&running);
    snapshot.pending = pending;
    (snapshot, cyclic)
}

/// What every segment of a normalization shares: the mode, the format and
/// the effects before it.
fn chain_key(mode: NormalizeMode, (sample_rate, channels): (u32, u16), inserts: &[Insert]) -> u64 {
    let mut h = StableHasher::default();
    (mode as u8, sample_rate, channels).hash(&mut h);
    hash_inserts(inserts, &mut h);
    h.finish()
}

/// The effects of `strip`, in order, one insert each, up to `stop`. A
/// normalization measures the channel through the effects before it
/// (`analyze`), so it stays right after a compressor too. `clips`: a
/// track's, by start, measured one by one when its normalization asks so;
/// the master is measured whole. `plugins`: `request_plugins` of the same
/// `strip` and `stop`, so the keys of the measures see what plays.
fn resolve_chain(
    strip: &ChannelStrip,
    stop: Option<usize>,
    has_audio: bool,
    clips: &[(ClipId, u64)],
    mut plugins: Vec<Option<PluginInsert>>,
    mut analyze: impl FnMut(&[Insert], Option<(usize, ClipId)>, NormalizeMode) -> LevelReading,
) -> Vec<Insert> {
    let mut inserts = Vec::new();
    for (i, effect) in strip
        .effects
        .iter()
        .enumerate()
        .take(stop.unwrap_or(usize::MAX))
    {
        if !effect.enabled {
            inserts.push(Insert::Off);
            continue;
        }
        let insert = match &effect.kind {
            AudioEffectKind::Normalize { .. } if !has_audio => Insert::Off,
            &AudioEffectKind::Normalize {
                target_db,
                mode,
                set_level,
            } => {
                let mut gain = |clip: Option<(usize, ClipId)>| {
                    let reading = analyze(&inserts, clip, mode);
                    normalize_gain(target_db, reading)
                };
                // The loudest peak of the clips is the peak of the track:
                // measured in one go.
                let per_clip =
                    !clips.is_empty() && (set_level == SetLevel::Independent || mode.is_loudness());
                if !per_clip {
                    Insert::Gain(gain(None).unwrap_or(1.0))
                } else {
                    let gains: Vec<Option<f32>> = clips
                        .iter()
                        .enumerate()
                        .map(|(i, &(id, _))| gain(Some((i, id))))
                        .collect();
                    match set_level {
                        SetLevel::Relative => Insert::Gain(
                            gains.into_iter().flatten().reduce(f32::min).unwrap_or(1.0),
                        ),
                        SetLevel::Independent => Insert::ClipGains(
                            clips
                                .iter()
                                .zip(gains)
                                .map(|(&(_, start), gain)| (start, gain.unwrap_or(1.0)))
                                .collect(),
                        ),
                    }
                }
            }
            AudioEffectKind::MultibandCompressor(params) => Insert::Compressor(params.clone()),
            AudioEffectKind::Mono => Insert::Mono,
            AudioEffectKind::Equalizer(params) => Insert::Eq(params.clone()),
            AudioEffectKind::Vst3(_) => plugins
                .get_mut(i)
                .and_then(Option::take)
                .map_or(Insert::Off, Insert::Plugin),
        };
        inserts.push(insert);
    }
    inserts
}

/// Keys of the plugin placements of the track strips (or of the master).
/// The strip's position is not part of the key, so removing a track keeps
/// the instances of the others; every strip is counted, also when a measure
/// skips it, so the occurrences match the live pass.
struct PluginKeys {
    scope: u64,
    group: u64,
    met: HashMap<EffectId, u32>,
}

const TRACK_STRIPS: u64 = 0;
const MASTER_STRIPS: u64 = 1;

pub fn plugin_key(scope: u64, group: u64, id: EffectId, occurrence: u32) -> u64 {
    stable_hash((scope, group, id.0, occurrence))
}

/// Key of the plugin at `index` of a strip, as the mixer asks it: the panel
/// finds the live instance of an effect with it.
pub fn plugin_key_in(
    scope: u64,
    timeline: &Timeline,
    channel: MixerChannel,
    index: usize,
) -> Option<u64> {
    let (group, before, strip) = match channel {
        MixerChannel::Track(i) => {
            let mix = &timeline.tracks.get(i)?.mix;
            (TRACK_STRIPS, &timeline.tracks[..i], mix)
        }
        MixerChannel::Master => (MASTER_STRIPS, &[][..], &timeline.master),
    };
    let AudioEffectKind::Vst3(this) = &strip.effects.get(index)?.kind else {
        return None;
    };
    let mut keys = PluginKeys::new(scope, group);
    for track in before {
        keys.count(&track.mix);
    }
    for effect in &strip.effects[..index] {
        if let AudioEffectKind::Vst3(plugin) = &effect.kind {
            keys.next(plugin);
        }
    }
    Some(keys.next(this))
}

/// The keys of every plugin of a timeline's strips, computed in one pass.
pub struct StripKeys {
    tracks: Vec<Vec<Option<u64>>>,
    master: Vec<Option<u64>>,
}

impl StripKeys {
    /// What `plugin_key_in` gives, without walking the preceding strips again.
    pub fn get(&self, channel: MixerChannel, index: usize) -> Option<u64> {
        let strip = match channel {
            MixerChannel::Track(i) => self.tracks.get(i)?,
            MixerChannel::Master => &self.master,
        };
        strip.get(index).copied().flatten()
    }
}

/// The keys of all the strips of `timeline`: for a panel that asks one per row.
pub fn plugin_keys_in(scope: u64, timeline: &Timeline) -> StripKeys {
    let mut keys = PluginKeys::new(scope, TRACK_STRIPS);
    let tracks = timeline
        .tracks
        .iter()
        .map(|track| keys.strip_keys(&track.mix))
        .collect();
    let master = PluginKeys::new(scope, MASTER_STRIPS).strip_keys(&timeline.master);
    StripKeys { tracks, master }
}

impl PluginKeys {
    fn strip_keys(&mut self, strip: &ChannelStrip) -> Vec<Option<u64>> {
        strip
            .effects
            .iter()
            .map(|effect| match &effect.kind {
                AudioEffectKind::Vst3(plugin) => Some(self.next(plugin)),
                _ => None,
            })
            .collect()
    }

    fn new(scope: u64, group: u64) -> Self {
        Self {
            scope,
            group,
            met: HashMap::new(),
        }
    }

    /// The same id twice in a pass (a duplicated strip) is two placements.
    fn next(&mut self, effect: &Vst3Effect) -> u64 {
        let seen = self.met.entry(effect.id).or_insert(0);
        let occurrence = *seen;
        *seen += 1;
        plugin_key(self.scope, self.group, effect.id, occurrence)
    }

    fn count(&mut self, strip: &ChannelStrip) {
        for effect in &strip.effects {
            if let AudioEffectKind::Vst3(plugin) = &effect.kind {
                self.next(plugin);
            }
        }
    }
}

/// Asks `source` for the processor of each plugin of `strip` up to `stop`,
/// once (a bypassed one only to keep its instance): aligned with its
/// effects, `None` for the bypassed and the others. Before the
/// chain is resolved, since the keys of its measures include whether each
/// plugin processes.
fn request_plugins(
    strip: &ChannelStrip,
    stop: Option<usize>,
    plugin_keys: &mut PluginKeys,
    source: &mut impl AudioSource,
    sample_rate: u32,
) -> Vec<Option<PluginInsert>> {
    if !strip
        .effects
        .iter()
        .any(|effect| matches!(effect.kind, AudioEffectKind::Vst3(_)))
    {
        return Vec::new();
    }
    let stop = stop.unwrap_or(usize::MAX);
    let mut plugins = Vec::with_capacity(strip.effects.len().min(stop));
    for (i, effect) in strip.effects.iter().enumerate() {
        let AudioEffectKind::Vst3(plugin) = &effect.kind else {
            if i < stop {
                plugins.push(None);
            }
            continue;
        };
        // Past the stop the key is still taken, to keep the occurrences.
        let key = plugin_keys.next(plugin);
        if i >= stop {
            continue;
        }
        let build = source.plugin_build(plugin);
        let handle = source.plugin(PluginRequest {
            key,
            effect: plugin,
            sample_rate,
            process: effect.enabled,
        });
        plugins.push(effect.enabled.then(|| PluginInsert {
            key,
            content: build.map_or(plugin.content_hash(), |b| {
                stable_hash((plugin.content_hash(), b))
            }),
            handle,
        }));
    }
    plugins
}

/// The keys of the plugins in `inserts` that process but were not made.
fn unmade_plugins(inserts: &[Insert]) -> Vec<u64> {
    inserts
        .iter()
        .filter_map(|insert| match insert {
            Insert::Plugin(plugin) if plugin.handle.is_none() => Some(plugin.key),
            _ => None,
        })
        .collect()
}

/// A reading still on its way plays the last one. `None` for silence,
/// which stays silence instead of getting the maximum gain.
fn normalize_gain(target_db: f32, reading: LevelReading) -> Option<f32> {
    let level = match reading {
        LevelReading::Ready(level) => Some(level),
        LevelReading::Pending(last) => last,
    };
    level
        .filter(|level| *level > 1e-6)
        .map(|level| (db_to_linear(target_db) / level).min(db_to_linear(vv_core::GAIN_DB_MAX)))
}

/// The nested timeline of compound clip `media_id`, ready to be mixed in
/// its own time; `frames`: the part of it read, `None` for all of it.
/// `None` if it has no timeline.
fn nested_mix(
    project: &Project,
    media_id: MediaId,
    (sample_rate, channels): (u32, u16),
    source: &mut impl AudioSource,
    frames: Option<std::ops::Range<u64>>,
    depth: u32,
    placement: i64,
    scope: u64,
) -> Option<(MixSnapshot, bool)> {
    if depth >= vv_core::MAX_COMPOUND_DEPTH {
        return Some((MixSnapshot::empty(sample_rate, channels), true));
    }
    let item = project.media_pool.get(media_id)?;
    let nested = item.compound.and_then(|id| project.timelines.get(id))?;
    Some(collect_clips(
        project,
        nested,
        sample_rate,
        channels,
        source,
        frames,
        depth + 1,
        placement,
        scope,
        None,
    ))
}

/// Where a clip plays, in audio frames, before its buffer is known.
pub(crate) struct ClipSpan {
    pub start: u64,
    pub len: u64,
    /// Frame of the media at `start`.
    pub source_offset: u64,
    pub step: f64,
    pub fade_in: u64,
    pub fade_out: u64,
}

impl ClipSpan {
    pub(crate) fn new(clip: &vv_core::Clip, timeline_fps: f64, sample_rate: u32) -> Self {
        let frames = |timeline_frames: FrameIdx| {
            seconds_to_frames(timeline_frames as f64 / timeline_fps, sample_rate)
        };
        Self {
            start: timeline_frame_to_sample(clip.timeline_start, timeline_fps, sample_rate),
            len: frames(clip.timeline_len),
            source_offset: seconds_to_frames(
                clip.media_secs_at(clip.timeline_start, timeline_fps),
                sample_rate,
            ),
            step: clip.speed().as_f64(),
            fade_in: frames(clip.fade_in),
            fade_out: frames(clip.fade_out),
        }
    }

    /// The media frames a mix of `range` (all of it without one) reads,
    /// `None` if none. Past the end of the file they are enough to place
    /// the fade-out where the whole file would.
    fn media_frames(&self, range: Option<&std::ops::Range<u64>>) -> Option<std::ops::Range<u64>> {
        let end = self.start + self.len;
        let (from, to) = match range {
            None => (self.start, end),
            Some(r) if r.end <= self.start || r.start >= end => return None,
            Some(r) => (
                r.start.max(self.start),
                r.end.saturating_add(self.fade_out).min(end),
            ),
        };
        if from >= to {
            return None;
        }
        let media = |f: u64| (f - self.start) as f64 * self.step;
        // A varispeed read looks up to `step` frames ahead.
        let lookahead = self.step.ceil() as u64 + 1;
        Some(
            self.source_offset + media(from).floor() as u64
                ..self.source_offset + media(to).ceil() as u64 + lookahead,
        )
    }
}

/// `None` if the buffer does not cover even one sample of the clip.
/// `readable_to`: where the file ends, if the buffer reaches it before the
/// frames the clip reads.
fn mix_clip_from(
    clip: &vv_core::Clip,
    span: &ClipSpan,
    clip_fps: f64,
    input: ClipInput,
    readable_to: Option<u64>,
) -> Option<MixClip> {
    if let ClipInput::Blocks(buffer) = &input
        && buffer.is_empty()
    {
        return None;
    }
    let step = span.step;
    let len = readable_to.map_or(span.len, |end| readable_len(span, end));
    if len == 0 {
        return None;
    }
    Some(MixClip {
        clip: clip.id,
        start: span.start,
        len,
        source_offset: span.source_offset,
        step,
        input,
        gain_db: clip.effects.gain_db.clone(),
        track: 0,
        clip_fps,
        media_offset: span.source_offset,
        media_step: step,
        fade_in: span.fade_in.min(len),
        fade_out: span.fade_out.min(len),
    })
}

fn readable_len(span: &ClipSpan, media_end: u64) -> u64 {
    let source_offset = span.source_offset.min(media_end);
    // The last frame read is interpolated with the one after it.
    let readable = ((media_end - source_offset) as f64 / span.step).floor() as u64;
    span.len
        .min(readable.saturating_sub(u64::from(span.step != 1.0)))
}

/// Where `timeline`'s audio ends: a compound clip reads no further, so its
/// fade-out ends with its audio as a file clip's ends with its file.
fn audio_end(project: &Project, timeline: &Timeline, sample_rate: u32, depth: u32) -> u64 {
    if depth >= vv_core::MAX_COMPOUND_DEPTH {
        return 0;
    }
    let fps = timeline.fps.as_f64().max(1e-9);
    timeline
        .audible_tracks()
        .flat_map(|(_, track)| track.clips.iter())
        .filter(|c| !c.disabled && c.freeze.is_none())
        .filter_map(|clip| {
            let ClipSource::Media(media_id) = &clip.source else {
                return None;
            };
            let item = project.media_pool.get(*media_id)?;
            let span = ClipSpan::new(clip, fps, sample_rate);
            let len = item
                .compound
                .and_then(|id| project.timelines.get(id))
                .and_then(|nested| compound_readable_to(project, nested, &span, sample_rate, depth))
                .map_or(span.len, |end| readable_len(&span, end));
            (len > 0).then_some(span.start + len)
        })
        .max()
        .unwrap_or(0)
}

/// `readable_to` of a clip of `nested`, a compound at `depth`.
fn compound_readable_to(
    project: &Project,
    nested: &Timeline,
    span: &ClipSpan,
    sample_rate: u32,
    depth: u32,
) -> Option<u64> {
    let end = audio_end(project, nested, sample_rate, depth + 1);
    span.media_frames(None)
        .is_some_and(|whole| end < whole.end)
        .then_some(end)
}

fn seconds_to_frames(secs: f64, sample_rate: u32) -> u64 {
    (secs.max(0.0) * sample_rate as f64).round() as u64
}

pub fn timeline_frame_to_sample(frame: FrameIdx, fps: f64, sample_rate: u32) -> u64 {
    seconds_to_frames(frame as f64 / fps.max(1e-9), sample_rate)
}

pub fn sample_to_timeline_frame(sample: u64, fps: f64, sample_rate: u32) -> FrameIdx {
    (sample as f64 / sample_rate as f64 * fps).floor() as FrameIdx
}

/// Writes into `out` (interleaved, `snapshot.channels` channels) the mix
/// starting from timeline audio frame `start`, with the effects starting
/// from scratch.
pub fn mix_range(snapshot: &MixSnapshot, start: u64, out: &mut [f32]) {
    mix_into(snapshot, &mut MixState::new(snapshot), start, out, false);
}

/// `mix_range` with the hosted plugins passing through.
pub fn mix_range_without_plugins(snapshot: &MixSnapshot, start: u64, out: &mut [f32]) {
    let mut state = MixState::without_plugins(snapshot);
    mix_into(snapshot, &mut state, start, out, false);
}

/// `mix_range` carrying on the effects from the previous call through
/// `state`: a mix split into calls equals a single one.
pub fn mix_range_from(snapshot: &MixSnapshot, state: &mut MixState, start: u64, out: &mut [f32]) {
    mix_into(snapshot, state, start, out, false);
}

/// `mix_range` for the output stream: the effects carry on from the
/// previous call through `state`, and `snapshot.meters` go up. No
/// allocations: it runs in the realtime callback.
pub fn mix_range_metered(
    snapshot: &MixSnapshot,
    state: &mut MixState,
    start: u64,
    out: &mut [f32],
) {
    mix_into(snapshot, state, start, out, true);
}

fn mix_into(
    snapshot: &MixSnapshot,
    state: &mut MixState,
    start: u64,
    out: &mut [f32],
    metered: bool,
) {
    out.fill(0.0);
    let ch = snapshot.channels as usize;
    if ch == 0 {
        return;
    }
    let frames = out.len() / ch;
    if state.next != Some(start) {
        state.reset();
    }
    state.next = Some(start + frames as u64);
    state.peaks.fill([0.0; 2]);
    let mut master_peak = [0.0f32; 2];
    for block_start in (0..frames).step_by(MIX_BLOCK_FRAMES) {
        let len = MIX_BLOCK_FRAMES.min(frames - block_start);
        let block = &mut out[block_start * ch..(block_start + len) * ch];
        let block_time = start + block_start as u64;
        for (track, range) in &snapshot.groups {
            if !state.rings.get(*track).copied().unwrap_or(true) {
                let end = block_time + len as u64;
                let mut plays = false;
                let mut first_start = u64::MAX;
                let idle = &mut state.idle[*track];
                let previous_end = idle.last_end;
                for clip in &snapshot.clips[range.clone()] {
                    if clip.start < end {
                        idle.last_end = idle.last_end.max(clip.start + clip.len);
                        if clip.start + clip.len > block_time {
                            plays = true;
                            first_start = first_start.min(clip.start);
                        }
                    }
                }
                let after_settle = |from: u64| from >= previous_end + snapshot.settle_frames();
                let restart = if plays {
                    !idle.settled && !idle.ringing && after_settle(first_start)
                } else {
                    !idle.settled && !idle.ringing && after_settle(block_time)
                };
                if let (true, Some(chain)) = (restart, state.tracks.get_mut(*track)) {
                    chain.iter_mut().flatten().for_each(|p| p.reset());
                }
                let idle = &mut state.idle[*track];
                idle.settled = !plays && (idle.settled || restart);
                if idle.settled {
                    #[cfg(test)]
                    {
                        state.skipped_blocks += 1;
                    }
                    continue;
                }
            }
            let channel = snapshot.track(*track);
            let scratch = &mut state.scratch[..len * ch];
            scratch.fill(0.0);
            for (index, clip) in range.clone().zip(&snapshot.clips[range.clone()]) {
                let nested = state.nested.get_mut(index).and_then(Option::as_mut);
                add_clip(snapshot, clip, nested, block_time, scratch);
            }
            let processors = state.tracks.get_mut(*track).map(Vec::as_mut_slice);
            let meters = metered.then(|| snapshot.meters.track_inserts(*track));
            apply_inserts(
                &channel.inserts,
                processors.unwrap_or_default(),
                scratch,
                ch,
                block_time,
                meters,
                state.track_skip.get(*track).copied().unwrap_or(usize::MAX),
            );
            let mut peak = [0.0f32; 2];
            let mut raw = 0.0f32;
            for (dst, src) in block.chunks_exact_mut(ch).zip(scratch.chunks_exact(ch)) {
                for (c, (d, s)) in dst.iter_mut().zip(src).enumerate() {
                    raw = raw.max(s.abs());
                    let value = s * channel.bus.channel_gain(c, ch);
                    *d += value;
                    if c < 2 {
                        peak[c] = peak[c].max(value.abs());
                    }
                }
            }
            if let Some(idle) = state.idle.get_mut(*track) {
                idle.ringing = raw > SILENCE;
            }
            if let Some(track_peak) = state.peaks.get_mut(*track) {
                *track_peak = [track_peak[0].max(peak[0]), track_peak[1].max(peak[1])];
            }
        }
        let meters = metered.then_some(snapshot.meters.master_inserts.as_slice());
        apply_inserts(
            &snapshot.master.inserts,
            &mut state.master,
            block,
            ch,
            block_time,
            meters,
            state.master_skip,
        );
        for frame in block.chunks_exact_mut(ch) {
            for (c, s) in frame.iter_mut().enumerate() {
                *s *= snapshot.master.bus.channel_gain(c, ch);
                if c < 2 {
                    master_peak[c] = master_peak[c].max(s.abs());
                }
            }
        }
    }
    if metered {
        for (track, peak) in state.peaks.iter().enumerate() {
            if let Some(meter) = snapshot.meters.track(track) {
                meter.raise(mono_as_stereo(*peak, ch));
            }
        }
        snapshot
            .meters
            .master
            .raise(mono_as_stereo(master_peak, ch));
    }
}

/// `samples` start at timeline audio frame `time`. `meters`: where the
/// compressors report, if metered.
fn apply_inserts(
    inserts: &[Insert],
    processors: &mut [Option<Box<dyn Effect>>],
    samples: &mut [f32],
    channels: usize,
    time: u64,
    meters: Option<&[InsertMeter]>,
    skip_from: usize,
) {
    for (index, insert) in inserts.iter().enumerate() {
        match insert {
            Insert::Off => {}
            Insert::Gain(_) | Insert::ClipGains(_) if index >= skip_from => {}
            Insert::Gain(gain) => samples.iter_mut().for_each(|s| *s *= gain),
            Insert::ClipGains(gains) => apply_clip_gains(gains, time, samples, channels),
            Insert::Mono => {
                for frame in samples.chunks_exact_mut(channels.max(1)) {
                    let average = frame.iter().sum::<f32>() / frame.len() as f32;
                    frame.fill(average);
                }
            }
            Insert::Compressor(_) | Insert::Eq(_) | Insert::Plugin(_) => {
                let Some(Some(processor)) = processors.get_mut(index) else {
                    continue;
                };
                let meter = meters.and_then(|m| m.get(index));
                let spectrum = meter.and_then(|m| m.spectrum.as_deref());
                if let Some(spectrum) = spectrum {
                    spectrum.record_pre(samples, channels);
                }
                let activity = processor.process(samples);
                if let (Some(meter), Some(activity)) = (meter, activity) {
                    meter.bands.raise(activity);
                }
                if let Some(spectrum) = spectrum {
                    spectrum.record_post(samples, channels);
                }
            }
        }
    }
}

fn apply_clip_gains(gains: &[(u64, f32)], time: u64, samples: &mut [f32], channels: usize) {
    if gains.is_empty() {
        return;
    }
    let ch = channels.max(1);
    let frames = samples.len() / ch;
    let mut from = 0;
    while from < frames {
        let next = gains.partition_point(|(start, _)| *start <= time + from as u64);
        let gain = gains[next.saturating_sub(1)].1;
        let to = gains
            .get(next)
            .map_or(frames, |(start, _)| ((start - time) as usize).min(frames));
        samples[from * ch..to * ch]
            .iter_mut()
            .for_each(|s| *s *= gain);
        from = to;
    }
}

/// Adds the part of `clip` from timeline audio frame `start` into `out`,
/// with its gain and fades.
fn add_clip(
    snapshot: &MixSnapshot,
    clip: &MixClip,
    nested: Option<&mut NestedState>,
    start: u64,
    out: &mut [f32],
) {
    let ch = snapshot.channels as usize;
    let end = start + (out.len() / ch) as u64;
    let to = end.min(clip.start + clip.len);
    let from = start.max(clip.start);
    if from >= to {
        return;
    }
    match (&clip.input, nested) {
        (ClipInput::Blocks(buffer), _) => add_frames(snapshot, clip, buffer, start, from..to, out),
        (ClipInput::Nested(mix), Some(nested)) => {
            let rendered = nested.render(&mix.mix, nested_frames(clip, from..to));
            add_frames(snapshot, clip, &rendered, start, from..to, out);
        }
        (ClipInput::Nested(_), None) => {}
    }
}

/// The nested frames a clip reads over the timeline frames `frames`.
fn nested_frames(clip: &MixClip, frames: std::ops::Range<u64>) -> std::ops::Range<u64> {
    if clip.step == 1.0 {
        let first = clip.source_offset + (frames.start - clip.start);
        return first..first + (frames.end - frames.start);
    }
    let pos = |f: u64| clip.source_offset as f64 + (f - clip.start) as f64 * clip.step;
    // `add_resampled` reads up to `step` frames from each position.
    pos(frames.start) as u64..pos(frames.end - 1) as u64 + clip.step as u64 + 2
}

/// Adds the timeline frames `frames` of `clip`, reading `input`, to `out`
/// (which starts at `start`).
fn add_frames(
    snapshot: &MixSnapshot,
    clip: &MixClip,
    input: &impl Frames,
    start: u64,
    frames: std::ops::Range<u64>,
    out: &mut [f32],
) {
    let ch = snapshot.channels as usize;
    let to = frames.end;
    let mut f = frames.start;
    while f < to {
        let in_clip = f - clip.start;
        let block = in_clip / GAIN_BLOCK_FRAMES;
        let block_end = (clip.start + (block + 1) * GAIN_BLOCK_FRAMES).min(to);
        // At the start of the block, not of the call: how the mix is split
        // into calls must not change it. The fade is not block granular:
        // it is applied per frame, at `add_scaled`.
        let gain = block_gain_linear(clip, block, snapshot.sample_rate);
        let dst = ((f - start) as usize) * ch;
        if clip.step == 1.0 {
            let mut from = f;
            while from < block_end {
                let src = clip.source_offset + (from - clip.start);
                let dst = (from - start) as usize * ch;
                let frames = match input.run(src) {
                    Run::Samples(samples) => {
                        let count = samples.len().min((block_end - from) as usize * ch);
                        let (dst, src) = (&mut out[dst..dst + count], &samples[..count]);
                        add_scaled(clip, gain, from - clip.start, dst, src, ch);
                        (count / ch) as u64
                    }
                    Run::Silence(frames) => frames,
                    Run::End => break,
                };
                from += frames.max(1);
            }
        } else {
            let frames = &mut out[dst..dst + (block_end - f) as usize * ch];
            for (i, frame) in frames.chunks_exact_mut(ch).enumerate() {
                let at = in_clip + i as u64;
                let pos = clip.source_offset as f64 + at as f64 * clip.step;
                add_resampled(
                    input,
                    pos,
                    clip.step,
                    gain * fade_multiplier(clip, at),
                    frame,
                );
            }
        }
        f = block_end;
    }
}

/// Adds `src` (interleaved, `in_clip` frames into the clip) to `dst`,
/// scaled by `gain` and by the fade of that instant.
fn add_scaled(clip: &MixClip, gain: f32, in_clip: u64, dst: &mut [f32], src: &[f32], ch: usize) {
    if clip.fade_in == 0 && clip.fade_out == 0 {
        for (d, s) in dst.iter_mut().zip(src) {
            *d += s * gain;
        }
        return;
    }
    for (i, (d, s)) in dst.chunks_mut(ch).zip(src.chunks(ch)).enumerate() {
        let gain = gain * fade_multiplier(clip, in_clip + i as u64);
        for (d, s) in d.iter_mut().zip(s) {
            *d += s * gain;
        }
    }
}

fn mono_as_stereo(peak: [f32; 2], channels: usize) -> [f32; 2] {
    if channels == 1 { [peak[0]; 2] } else { peak }
}

/// Varispeed read of the frame at fractional position `pos`: linear
/// interpolation slowing down; speeding up, the average of the `step`
/// frames skipped over, a crude low-pass against aliasing.
fn add_resampled(buffer: &impl Frames, pos: f64, step: f64, gain: f32, out: &mut [f32]) {
    let last = buffer.end().saturating_sub(1);
    let at = |frame: u64, c: usize| {
        buffer
            .frame(frame.min(last))
            .map_or(0.0, |samples| samples[c])
    };
    let pos = pos.max(buffer.start() as f64);
    let i = pos as u64;
    if step < 1.0 {
        let t = (pos - i as f64) as f32;
        for (c, o) in out.iter_mut().enumerate() {
            *o += (at(i, c) + (at(i + 1, c) - at(i, c)) * t) * gain;
        }
    } else {
        let n = (step as usize).max(1);
        for (c, o) in out.iter_mut().enumerate() {
            let sum: f32 = (i..i + n as u64).map(|frame| at(frame, c)).sum();
            *o += sum / n as f32 * gain;
        }
    }
}

/// Linear fade ramp at `in_clip` samples from the start of the
/// clip, same semantics as `Clip::fade_multiplier_at` but in audio samples.
fn fade_multiplier(clip: &MixClip, in_clip: u64) -> f32 {
    let in_ramp = if clip.fade_in > 0 {
        (in_clip as f32 / clip.fade_in as f32).clamp(0.0, 1.0)
    } else {
        1.0
    };
    let out_ramp = if clip.fade_out > 0 {
        let from_end = clip.len.saturating_sub(in_clip);
        (from_end as f32 / clip.fade_out as f32).clamp(0.0, 1.0)
    } else {
        1.0
    };
    in_ramp * out_ramp
}

fn block_gain_linear(clip: &MixClip, block: u64, sample_rate: u32) -> f32 {
    if clip.gain_db.is_constant() {
        return db_to_linear(clip.gain_db.default);
    }
    // The gain keyframes live in *source* frames: the frame covering
    // this instant of the media.
    let media_frame =
        clip.media_offset as f64 + (block * GAIN_BLOCK_FRAMES) as f64 * clip.media_step;
    let media_secs = media_frame / sample_rate as f64;
    let source_frame = (media_secs * clip.clip_fps).floor() as FrameIdx;
    db_to_linear(clip.gain_db.value_at(source_frame))
}

pub fn db_to_linear(db: f32) -> f32 {
    10f32.powf(db / 20.0)
}

/// From `from` to `to` channels, appending to `out`. Symmetric, not a broadcast
/// downmix: going down, channel `i` is averaged onto `i % to` (on 5.1
/// ffmpeg groups L,C,Ls and R,LFE,Rs), going up, channel `i` copies
/// `i % from`.
pub fn remix_channels_into(samples: &[f32], from: u16, to: u16, out: &mut Vec<f32>) {
    if from == 0 || to == 0 {
        return;
    }
    if from == to {
        out.extend_from_slice(samples);
        return;
    }
    let (from, to) = (from as usize, to as usize);
    out.reserve(samples.len() / from * to);
    for frame in samples.chunks_exact(from) {
        if from > to {
            for c in 0..to {
                let (sum, n) = frame
                    .iter()
                    .skip(c)
                    .step_by(to)
                    .fold((0.0, 0u32), |(sum, n), &v| (sum + v, n + 1));
                out.push(sum / n as f32);
            }
        } else {
            out.extend((0..to).map(|c| frame[c % from]));
        }
    }
}

/// Audio already stretched to `tempo` (fast forward): frame `i` of the
/// concatenation of `chunks` corresponds to timeline audio frame
/// `origin + i * tempo`.
#[derive(Clone)]
pub struct StretchedWindow {
    pub tempo: u64,
    pub origin: u64,
    pub chunks: Vec<Arc<Vec<f32>>>,
}

impl StretchedWindow {
    pub fn frames(&self, channels: u16) -> u64 {
        let ch = channels.max(1) as usize;
        self.chunks.iter().map(|c| (c.len() / ch) as u64).sum()
    }

    /// First uncovered timeline audio frame.
    pub fn covered_until(&self, channels: u16) -> u64 {
        self.origin + self.frames(channels) * self.tempo
    }
}

/// What the callback plays: the mix, or at speeds > 1x the stretched
/// window.
pub struct MixerState {
    pub mix: Arc<MixSnapshot>,
    pub stretched: Option<StretchedWindow>,
    /// Locked only by the callback; allocated here, on the UI thread.
    effects: Mutex<MixState>,
}

impl MixerState {
    pub fn new(mix: Arc<MixSnapshot>, stretched: Option<StretchedWindow>) -> Self {
        let effects = Mutex::new(MixState::new(&mix));
        Self {
            mix,
            stretched,
            effects,
        }
    }
}

/// Writes into `out` the stretched audio corresponding to timeline audio
/// frame `position`; silence outside the window. No allocations.
pub fn render_stretched(window: &StretchedWindow, channels: u16, position: u64, out: &mut [f32]) {
    out.fill(0.0);
    let ch = channels as usize;
    if ch == 0 || window.tempo == 0 || position < window.origin {
        return;
    }
    let mut skip = ((position - window.origin) / window.tempo) as usize * ch;
    let mut written = 0;
    for chunk in &window.chunks {
        if skip >= chunk.len() {
            skip -= chunk.len();
            continue;
        }
        let take = (chunk.len() - skip).min(out.len() - written);
        out[written..written + take].copy_from_slice(&chunk[skip..skip + take]);
        written += take;
        skip = 0;
        if written == out.len() {
            break;
        }
    }
}

/// Single cpal stream playing the current state. The position (timeline
/// audio frame) is the playback clock.
pub struct Mixer {
    _stream: cpal::Stream,
    playing: Arc<AtomicBool>,
    position: Arc<AtomicU64>,
    pending: Arc<Mutex<Option<Arc<MixerState>>>>,
    /// Published states still referenced by the callback: keeping them here
    /// guarantees that the last drop (with deallocation) happens on the UI
    /// thread, never on the audio one.
    retained: Vec<Arc<MixerState>>,
    peak_left_bits: Arc<AtomicU32>,
    peak_right_bits: Arc<AtomicU32>,
    sample_rate: u32,
    channels: u16,
}

impl Mixer {
    pub fn new() -> Result<Self, String> {
        let host = cpal::default_host();
        let device = host
            .default_output_device()
            .ok_or("no audio output device")?;
        // Native channels of the device: asking for others makes PipeWire insert a
        // remix that adds output latency, visible as audio lagging
        // behind playhead and waveform.
        let channels = device
            .default_output_config()
            .map(|c| c.channels())
            .unwrap_or(2);
        let sample_rate = PROJECT_SAMPLE_RATE;
        let config = cpal::StreamConfig {
            channels,
            sample_rate,
            buffer_size: cpal::BufferSize::Default,
        };

        let playing = Arc::new(AtomicBool::new(false));
        let position = Arc::new(AtomicU64::new(0));
        let initial = Arc::new(MixerState::new(
            Arc::new(MixSnapshot::empty(sample_rate, channels)),
            None,
        ));
        let pending: Arc<Mutex<Option<Arc<MixerState>>>> = Arc::new(Mutex::new(None));
        let peak_left_bits = Arc::new(AtomicU32::new(0.0f32.to_bits()));
        let peak_right_bits = Arc::new(AtomicU32::new(0.0f32.to_bits()));

        let cb_playing = playing.clone();
        let cb_position = position.clone();
        let cb_pending = pending.clone();
        let cb_peak_left = peak_left_bits.clone();
        let cb_peak_right = peak_right_bits.clone();
        let mut current = initial.clone();
        let ch = channels.max(1) as usize;

        let stream = device
            .build_output_stream(
                config,
                move |data: &mut [f32], _: &cpal::OutputCallbackInfo| {
                    if let Ok(mut slot) = cb_pending.try_lock()
                        && let Some(next) = slot.take()
                    {
                        if let (Ok(mut effects), Ok(previous)) =
                            (next.effects.try_lock(), current.effects.try_lock())
                        {
                            effects.continue_from(&previous);
                        }
                        current = next;
                    }
                    if cb_playing.load(Ordering::Relaxed) && current.mix.channels as usize == ch {
                        let pos = cb_position.load(Ordering::Relaxed);
                        let frames = (data.len() / ch) as u64;
                        let advance = match &current.stretched {
                            Some(window) => {
                                render_stretched(window, channels, pos, data);
                                frames * window.tempo
                            }
                            None => {
                                match current.effects.try_lock() {
                                    Ok(mut effects) => {
                                        mix_range_metered(&current.mix, &mut effects, pos, data)
                                    }
                                    Err(_) => data.fill(0.0),
                                }
                                frames
                            }
                        };
                        // A seek that arrived during the mix wins over the advance.
                        let _ = cb_position.compare_exchange(
                            pos,
                            pos + advance,
                            Ordering::Relaxed,
                            Ordering::Relaxed,
                        );
                    } else {
                        data.fill(0.0);
                    }
                    let (l, r) = stereo_peak(data, ch);
                    cb_peak_left.store(l.to_bits(), Ordering::Relaxed);
                    cb_peak_right.store(r.to_bits(), Ordering::Relaxed);
                },
                move |err| eprintln!("vv-audio: mixer stream error: {err}"),
                None,
            )
            .map_err(|e| e.to_string())?;
        stream.play().map_err(|e| e.to_string())?;

        Ok(Self {
            _stream: stream,
            playing,
            position,
            pending,
            retained: vec![initial],
            peak_left_bits,
            peak_right_bits,
            sample_rate,
            channels,
        })
    }

    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    pub fn channels(&self) -> u16 {
        self.channels
    }

    pub fn set_state(&mut self, state: Arc<MixerState>) {
        *self.pending.lock().unwrap_or_else(|e| e.into_inner()) = Some(state.clone());
        self.retained.retain(|s| Arc::strong_count(s) > 1);
        self.retained.push(state);
    }

    pub fn play(&self) {
        self.playing.store(true, Ordering::Relaxed);
    }

    pub fn pause(&self) {
        self.playing.store(false, Ordering::Relaxed);
    }

    pub fn is_playing(&self) -> bool {
        self.playing.load(Ordering::Relaxed)
    }

    pub fn seek(&self, timeline_sample: u64) {
        self.position.store(timeline_sample, Ordering::Relaxed);
    }

    pub fn position(&self) -> u64 {
        self.position.load(Ordering::Relaxed)
    }

    pub fn peak_linear_stereo(&self) -> (f32, f32) {
        (
            f32::from_bits(self.peak_left_bits.load(Ordering::Relaxed)),
            f32::from_bits(self.peak_right_bits.load(Ordering::Relaxed)),
        )
    }
}

fn stereo_peak(data: &[f32], channels: usize) -> (f32, f32) {
    let mut left = 0.0f32;
    let mut right = 0.0f32;
    for frame in data.chunks(channels.max(1)) {
        if let Some(&s) = frame.first() {
            left = left.max(s.abs());
        }
        match frame.get(1) {
            Some(&s) => right = right.max(s.abs()),
            None => right = left,
        }
    }
    (left, right)
}

#[cfg(test)]
#[path = "tests/mixer.rs"]
mod tests;
