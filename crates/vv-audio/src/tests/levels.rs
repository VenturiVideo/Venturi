use super::*;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use vv_core::{
    AudioEffect, AudioEffectKind, Clip, MediaItem, MediaMeta, MultibandCompressor, Rational,
    SetLevel, Track, TrackKind,
};

use crate::SampleBlocks;
use crate::mixer::{ClipAudio, LevelReading};

const RATE: u32 = 100;

fn project() -> (Project, vv_core::MediaId) {
    let mut project = Project::default();
    let media = project.media_pool.insert(MediaItem {
        path: PathBuf::from("a.wav"),
        meta: MediaMeta {
            duration_frames: 1000,
            fps: Rational::new(10, 1),
            width: 0,
            height: 0,
            has_video: false,
            has_audio: true,
            sample_rate: RATE,
            channels: 1,
            audio_streams: 1,
            file: Default::default(),
        },
        content_hash: 7,
        compound: None,
        folder: None,
    });
    (project, media)
}

fn clip(id: u64, media: vv_core::MediaId, start: i64, source_in: i64, len: i64) -> Clip {
    Clip::from_source_range(
        ClipId(id),
        ClipSource::Media(media),
        source_in,
        source_in + len,
        start,
        Rational::one(),
    )
}

fn track(clips: Vec<Clip>) -> Track {
    Track {
        clips,
        ..Track::new(TrackKind::Audio)
    }
}

fn timeline(tracks: Vec<Track>) -> Timeline {
    Timeline {
        name: "t".into(),
        fps: Rational::new(10, 1),
        resolution: (4, 2),
        tracks,
        markers: Vec::new(),
        master: Default::default(),
    }
}

fn normalize() -> AudioEffect {
    AudioEffect::new(AudioEffectKind::Normalize {
        target_db: 0.0,
        mode: NormalizeMode::SamplePeak,
        set_level: SetLevel::Relative,
    })
}

fn compressor() -> AudioEffect {
    AudioEffect::new(AudioEffectKind::MultibandCompressor(
        MultibandCompressor::DEFAULT,
    ))
}

/// The segments of every normalization, in the order they are resolved.
#[derive(Default)]
struct Segments {
    segments: Vec<Vec<LevelSegment>>,
    /// The live pool makes every plugin.
    live: bool,
}

struct Silent;

impl crate::PluginProcessor for Silent {
    fn process(&self, _: &mut [f32], _: usize) -> bool {
        true
    }
    fn reset(&self) {}
    fn latency(&self) -> u32 {
        0
    }
}

impl AudioSource for Segments {
    fn file(&mut self, _: &Path, _: usize) -> ClipAudio {
        ClipAudio::Ready(SampleBlocks::contiguous(Arc::new(vec![0.5; 10_000]), 0, 1))
    }

    fn plugin(&mut self, _: crate::PluginRequest<'_>) -> Option<crate::PluginHandle> {
        self.live.then(|| Arc::new(Silent) as _)
    }

    fn level(&mut self, analysis: LevelAnalysis) -> LevelReading {
        self.segments.push(analysis.segments);
        LevelReading::Ready(1.0)
    }
}

fn segments(project: &Project, tl: &Timeline) -> Vec<LevelSegment> {
    segments_through(project, tl, false)
}

fn segments_through(project: &Project, tl: &Timeline, live: bool) -> Vec<LevelSegment> {
    let mut source = Segments {
        live,
        ..Segments::default()
    };
    MixSnapshot::from_timeline(project, tl, RATE, 1, &mut source);
    source.segments.into_iter().next().expect("a normalization")
}

fn plugin() -> AudioEffect {
    AudioEffect::new(AudioEffectKind::Vst3(Vst3Effect {
        id: vv_core::EffectId(1),
        class_id: "0".repeat(32),
        name: String::new(),
        vendor: String::new(),
        version: String::new(),
        component_state: Default::default(),
        controller_state: Default::default(),
    }))
}

fn keys(segments: &[LevelSegment]) -> Vec<u64> {
    segments.iter().map(|s| s.key).collect()
}

#[test]
fn pitch_and_formant_edits_invalidate_level_measurements() {
    let (project, media) = project();
    let mut tl = timeline(vec![track(vec![clip(1, media, 0, 0, 10)])]);
    let body = |tl: &Timeline| input_clips(&project, tl, RATE)[0].body;
    let original = body(&tl);
    tl.tracks[0].clips[0].effects.pitch_shift.semitones = 7.0;
    let shifted = body(&tl);
    assert_ne!(shifted, original);
    tl.tracks[0].clips[0].effects.pitch_shift.preserve_formants = true;
    assert_ne!(body(&tl), shifted);
    tl.tracks[0].clips[0].effects.pitch_shift.enabled = false;
    assert_eq!(body(&tl), original);
}

fn three_clips(media: vv_core::MediaId, second_start: i64, third_start: i64) -> Track {
    track(vec![
        clip(1, media, 0, 0, 10),
        clip(2, media, second_start, 20, 10),
        clip(3, media, third_start, 40, 10),
    ])
}

#[test]
fn a_ripple_keeps_the_keys_of_what_it_moves() {
    let (project, media) = project();
    let mut before = timeline(vec![three_clips(media, 15, 30)]);
    before.master.effects = vec![normalize()];
    let mut after = timeline(vec![three_clips(media, 10, 25)]);
    after.master.effects = vec![normalize()];

    let (before, after) = (segments(&project, &before), segments(&project, &after));
    assert_eq!(before.len(), 3);
    assert_eq!(keys(&before), keys(&after));
    assert_ne!(before[1].frames, after[1].frames);
}

#[test]
fn an_edit_changes_only_the_segments_it_touches() {
    let (project, media) = project();
    let mut tl = timeline(vec![three_clips(media, 15, 30)]);
    tl.master.effects = vec![normalize()];
    let before = keys(&segments(&project, &tl));
    tl.tracks[0].clips[1].effects.gain_db = vv_core::Keyframed::constant(-6.0);
    let after = keys(&segments(&project, &tl));
    assert_eq!(
        before
            .iter()
            .zip(&after)
            .map(|(a, b)| a != b)
            .collect::<Vec<_>>(),
        [false, true, false]
    );
}

/// The compressor carries the edit into the next segment, rendered with a
/// preroll from it.
#[test]
fn with_a_state_before_the_measure_an_edit_reaches_the_next_segment() {
    let (project, media) = project();
    let mut tl = timeline(vec![three_clips(media, 10, 20)]);
    tl.master.effects = vec![compressor(), normalize()];
    let before = segments(&project, &tl);
    assert!(before[1].preroll > 0);
    tl.tracks[0].clips[1].effects.gain_db = vv_core::Keyframed::constant(-6.0);
    let after = keys(&segments(&project, &tl));
    assert_eq!(
        keys(&before)
            .iter()
            .zip(&after)
            .map(|(a, b)| a != b)
            .collect::<Vec<_>>(),
        [false, true, true]
    );
}

/// Clips of 10 frames at 0, 15 and 30; a frame is 10 samples.
fn plugin_timeline(media: vv_core::MediaId, on_track: bool) -> Timeline {
    let mut tl = timeline(vec![three_clips(media, 15, 30)]);
    let strip = if on_track {
        &mut tl.tracks[0].mix
    } else {
        &mut tl.master
    };
    strip.effects.push(plugin());
    tl.master.effects.push(normalize());
    tl
}

/// A plugin's memory has no bound: the strip is measured in one run from the
/// start, gaps included, and any edit reaches it.
#[test]
fn a_measure_after_a_plugin_is_one_continuous_run() {
    let (project, media) = project();
    for on_track in [false, true] {
        let mut tl = plugin_timeline(media, on_track);
        let before = segments_through(&project, &tl, true);
        assert_eq!(before.len(), 1, "on track {on_track}");
        assert_eq!(before[0].frames, 0..400, "on track {on_track}");
        assert_eq!(before[0].preroll, 0);
        tl.tracks[0].clips[0].effects.gain_db = vv_core::Keyframed::constant(-6.0);
        let after = segments_through(&project, &tl, true);
        assert_ne!(after[0].key, before[0].key, "on track {on_track}");
        tl.tracks[0].clips[0].effects.gain_db = vv_core::Keyframed::constant(0.0);
        assert_eq!(segments_through(&project, &tl, true), before);
    }
}

/// A plugin the live pool did not make passes through: nothing to remember.
#[test]
fn a_plugin_that_is_not_made_keeps_the_segments() {
    let (project, media) = project();
    let tl = plugin_timeline(media, false);
    let dry = segments_through(&project, &tl, false);
    assert_eq!(dry.len(), 3);
    let mut bare = timeline(vec![three_clips(media, 15, 30)]);
    bare.master.effects = vec![normalize()];
    assert_ne!(keys(&dry), keys(&segments(&project, &bare)));
    assert_eq!(keys(&dry), keys(&segments_through(&project, &tl, false)));
}

#[test]
fn the_master_measure_follows_what_plays() {
    let (project, media) = project();
    let mut tl = timeline(vec![
        track(vec![clip(1, media, 0, 0, 10)]),
        track(vec![clip(2, media, 0, 50, 10)]),
    ]);
    tl.master.effects = vec![normalize()];
    let both = keys(&segments(&project, &tl));
    tl.tracks[1].muted = true;
    assert_ne!(keys(&segments(&project, &tl)), both);
    tl.tracks[1].muted = false;
    tl.tracks[1].mix.gain_db = -6.0;
    assert_ne!(
        keys(&segments(&project, &tl)),
        both,
        "after the track fader"
    );
}

/// The keys go to disk: a change here makes every cached level stale.
#[test]
fn the_stable_hash_does_not_change() {
    assert_eq!(stable_hash((1u64, 2u32, "levels")), 0x1dc1_4506_b088_1bce);
}

fn key_of(project: &Project, tl: &Timeline) -> u64 {
    timeline_key(project, tl, RATE, 0, &mut |_| Some(5))
}

fn plugin_compound(media: vv_core::MediaId, id: u64, name: &str) -> Timeline {
    let mut tl = timeline(vec![track(vec![clip(1, media, 0, 0, 10)])]);
    let mut effect = plugin();
    if let AudioEffectKind::Vst3(p) = &mut effect.kind {
        p.id = vv_core::EffectId(id);
        p.name = name.into();
        p.vendor = name.into();
    }
    tl.tracks[0].mix.effects.push(effect);
    tl.master.effects.push(normalize());
    tl
}

#[test]
fn a_fresh_id_copy_of_a_compound_shares_its_key() {
    let (project, media) = project();
    let a = plugin_compound(media, 1, "One");
    let b = plugin_compound(media, 2, "Two");
    assert_eq!(key_of(&project, &a), key_of(&project, &b));
    let mut c = plugin_compound(media, 3, "Three");
    if let AudioEffectKind::Vst3(p) = &mut c.tracks[0].mix.effects[0].kind {
        p.version = "2".into();
    }
    assert_ne!(key_of(&project, &a), key_of(&project, &c));
    let mut d = plugin_compound(media, 4, "Four");
    d.tracks[0].mix.effects[0].enabled = false;
    assert_ne!(key_of(&project, &a), key_of(&project, &d));
}

/// The keys go to disk: strips without plugins must keep hashing as before.
#[test]
fn a_plugin_free_timeline_key_does_not_change() {
    let (project, media) = project();
    let mut tl = timeline(vec![track(vec![clip(1, media, 0, 0, 10)])]);
    tl.tracks[0].mix.gain_db = -3.0;
    tl.tracks[0].mix.effects.push(compressor());
    tl.master.effects.push(normalize());
    assert_eq!(key_of(&project, &tl), 9_592_979_620_447_192_468);
}
