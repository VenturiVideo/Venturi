//! Positions arrive in seconds at an arbitrary rate and are quantized to
//! the timeline frame. Tracks are read by accumulating the seconds and
//! rounding the start and end of each element, so no drift accumulates
//! along the track. A file exported by Venturi comes back identical thanks
//! to `metadata.venturi`.

use super::{MeasureTitle, OtioError, generator, resolve};
use crate::model::{
    Clip, ClipId, ClipSource, CrossTransition, Ease, EffectStack, FrameIdx, IMAGE_DURATION_FRAMES,
    Interpolation, Keyframed, LinkGroupId, MAX_COMPOUND_DEPTH, MediaId, MediaItem, MediaMeta,
    Project, PushDirection, Rational, Rgba, Timeline, TimelineId, TitleParams, Track, TrackKind,
    TransformParam, Transition, TransitionKind,
};
use serde_json::Value;
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

enum PendingTransition {
    In(Transition),
    Crossing {
        left_clip: ClipId,
        transition: Transition,
        into_previous: FrameIdx,
    },
}

/// Metadata of a media and its `content_hash`, or a readable error.
pub type ProbeResult = Result<(MediaMeta, u64), String>;

pub struct OtioImport {
    pub project: Project,
    /// What was not imported or was approximated, for the user.
    pub warnings: Vec<OtioWarning>,
}

/// The text is composed by the UI, in its own language.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OtioWarning {
    EffectIgnored {
        effect: String,
        clips: usize,
    },
    EffectPartlyIgnored {
        effect: String,
        clips: usize,
    },
    SpeedNotApplied {
        clip: String,
        percent: i64,
    },
    UnsupportedInStack {
        schema: String,
    },
    TrackKindIgnored {
        kind: Option<String>,
    },
    TransitionIgnored,
    UnsupportedItem {
        schema: String,
    },
    ClipWithoutDuration {
        clip: String,
    },
    /// Content not in the file (Resolve's Text+, Fusion...): a disabled
    /// title with the clip's name holds its place.
    Placeholder {
        clip: String,
    },
    ClipShorterThanAFrame {
        clip: String,
    },
    AudioOnlyOnVideoTrack {
        clip: String,
    },
    UnsupportedReference {
        clip: String,
        schema: String,
    },
    UnsupportedUrl {
        url: String,
    },
    /// Imported offline, to relink.
    MediaUnreadable {
        path: PathBuf,
        error: String,
    },
}

pub fn import_otio(
    path: &Path,
    mut probe: impl FnMut(&Path) -> ProbeResult,
    measure: Option<MeasureTitle>,
) -> Result<OtioImport, OtioError> {
    let value: Value = serde_json::from_str(&std::fs::read_to_string(path)?)?;
    let base_dir = path.parent().unwrap_or(Path::new("."));
    project_from_otio(&value, base_dir, &mut probe, measure)
}

/// Distinct `target_url`s in the file: an upper bound on the probes
/// `project_from_otio` will make, for a progress bar.
pub fn media_url_count(value: &Value) -> usize {
    fn collect<'v>(value: &'v Value, urls: &mut std::collections::HashSet<&'v str>) {
        match value {
            Value::Object(map) => {
                if let Some(url) = map.get("target_url").and_then(Value::as_str) {
                    urls.insert(url);
                }
                map.values().for_each(|v| collect(v, urls));
            }
            Value::Array(items) => items.iter().for_each(|v| collect(v, urls)),
            _ => {}
        }
    }
    let mut urls = std::collections::HashSet::new();
    collect(value, &mut urls);
    urls.len()
}

/// `base_dir` resolves relative `target_url`s.
pub fn project_from_otio(
    value: &Value,
    base_dir: &Path,
    probe: &mut dyn FnMut(&Path) -> ProbeResult,
    measure: Option<MeasureTitle>,
) -> Result<OtioImport, OtioError> {
    let mut timelines = Vec::new();
    collect_timelines(value, &mut timelines);
    if timelines.is_empty() {
        return Err(OtioError::Format("no timeline in the OTIO file".into()));
    }
    let mut importer = Importer {
        project: Project::default(),
        warnings: Vec::new(),
        media: HashMap::new(),
        offline: Default::default(),
        ignored_effects: BTreeMap::new(),
        base_dir,
        probe,
        measure,
        compounds: HashMap::new(),
        new_compounds: Vec::new(),
        depth: 0,
    };
    for timeline in timelines {
        importer.timeline(timeline);
    }
    for ((effect, partial), clips) in std::mem::take(&mut importer.ignored_effects) {
        importer.warn(match partial {
            true => OtioWarning::EffectPartlyIgnored { effect, clips },
            false => OtioWarning::EffectIgnored { effect, clips },
        });
    }
    Ok(OtioImport {
        project: importer.project,
        warnings: importer.warnings,
    })
}

struct Importer<'a> {
    project: Project,
    warnings: Vec<OtioWarning>,
    media: HashMap<PathBuf, MediaId>,
    offline: std::collections::HashSet<MediaId>,
    /// Effect name → how many clips had it: one warning per effect,
    /// not one per clip. `partial`: something of the effect was translated.
    ignored_effects: BTreeMap<(String, bool), usize>,
    base_dir: &'a Path,
    probe: &'a mut dyn FnMut(&Path) -> ProbeResult,
    measure: Option<MeasureTitle<'a>>,
    /// Resolve's `Sequence ID` → pool item: the pieces of a split compound
    /// clip share one nested timeline.
    compounds: HashMap<String, MediaId>,
    /// Built while reading the current top-level timeline, which lends them
    /// its resolution.
    new_compounds: Vec<(TimelineId, MediaId)>,
    depth: u32,
}

/// Space of linked group numbers in the file: ours and Resolve's must not
/// be mixed.
#[derive(PartialEq, Eq, Hash)]
enum GroupKey {
    Venturi(u64),
    Resolve(u64),
}

/// A clip read from a file not our own, a candidate for automatic
/// video+audio linking.
struct ForeignClip {
    track: usize,
    index: usize,
}

impl Importer<'_> {
    fn timeline(&mut self, otio: &Value) {
        let venturi = &otio["metadata"]["venturi"];
        let fps = serde_json::from_value::<Rational>(venturi["fps"].clone())
            .ok()
            .or_else(|| {
                otio["global_start_time"]["rate"]
                    .as_f64()
                    .map(Rational::from_fps)
            })
            .or_else(|| first_item_rate(otio).map(Rational::from_fps))
            .unwrap_or(Rational::new(30, 1));
        let tracks = self.tracks(&otio["tracks"], fps);
        let resolution = serde_json::from_value::<(u32, u32)>(venturi["resolution"].clone())
            .ok()
            .or_else(|| self.first_video_resolution(&tracks))
            .unwrap_or((1920, 1080));
        // Resolve renders a compound clip at the resolution of the timeline.
        for (timeline_id, media_id) in std::mem::take(&mut self.new_compounds) {
            let nested = &mut self.project.timelines[timeline_id];
            nested.resolution = resolution;
            self.project.media_pool[media_id].meta = nested.compound_meta();
        }
        self.project.timelines.insert(Timeline {
            name: otio["name"].as_str().unwrap_or("Timeline").to_owned(),
            fps,
            resolution,
            tracks,
            markers: Vec::new(),
            master: Default::default(),
        });
    }

    /// The tracks of a timeline's or a compound clip's `stack`.
    fn tracks(&mut self, stack: &Value, fps: Rational) -> Vec<Track> {
        let mut groups: HashMap<GroupKey, LinkGroupId> = HashMap::new();
        let mut foreign = Vec::new();
        let mut tracks = Vec::new();
        for otio_track in children(stack) {
            if schema(otio_track) != "Track" {
                self.warn(OtioWarning::UnsupportedInStack {
                    schema: schema(otio_track).to_owned(),
                });
                continue;
            }
            let kind = match otio_track["kind"].as_str() {
                Some("Video") => TrackKind::Video,
                Some("Audio") => TrackKind::Audio,
                other => {
                    self.warn(OtioWarning::TrackKindIgnored {
                        kind: other.map(str::to_owned),
                    });
                    continue;
                }
            };
            let mut track = Track::new(kind);
            track.muted = otio_track["enabled"] == false;
            let mut cursor = 0.0;
            let mut pending = None;
            for item in children(otio_track) {
                let start = to_frames(cursor, fps);
                let duration = match schema(item) {
                    // Takes no time on the track: it overlaps its neighbours.
                    "Transition" => {
                        pending = self.transition(item, fps, start, &mut track);
                        continue;
                    }
                    "Gap" => item_duration(item),
                    "Clip" | "Stack" => {
                        let (duration, clip) =
                            self.clip(item, kind, fps, start, cursor, &mut groups);
                        if let Some((mut clip, is_foreign)) = clip {
                            match pending.take() {
                                Some(PendingTransition::In(transition)) => {
                                    clip.effects.transition_in = Some(transition);
                                }
                                Some(PendingTransition::Crossing {
                                    left_clip,
                                    transition,
                                    ..
                                }) => {
                                    let left_len =
                                        track.clip(left_clip).map_or(1, |c| c.timeline_len);
                                    let max_duration = (2 * left_len.min(clip.timeline_len)).max(1);
                                    track.crossings.push(CrossTransition {
                                        left_clip,
                                        right_clip: clip.id,
                                        transition: Transition {
                                            duration: transition.duration.clamp(1, max_duration),
                                            ..transition
                                        },
                                    });
                                }
                                None => {}
                            }
                            if is_foreign {
                                foreign.push(ForeignClip {
                                    track: tracks.len(),
                                    index: track.clips.len(),
                                });
                            }
                            track.clips.push(clip);
                        }
                        duration
                    }
                    other => {
                        self.warn(OtioWarning::UnsupportedItem {
                            schema: other.to_owned(),
                        });
                        item_duration(item)
                    }
                };
                // No clip right after the cut: only the half before it stays.
                if let Some(PendingTransition::Crossing {
                    left_clip,
                    transition,
                    into_previous,
                }) = pending.take_if(|p| matches!(p, PendingTransition::Crossing { .. }))
                    && let Some(clip) = track.clip_mut(left_clip)
                {
                    clip.effects.transition_out = Some(Transition {
                        duration: into_previous,
                        ..transition
                    });
                }
                cursor += duration;
            }
            tracks.push(track);
        }
        if !tracks.iter().any(|t| t.kind == TrackKind::Video) {
            tracks.insert(0, Track::new(TrackKind::Video));
            for clip in &mut foreign {
                clip.track += 1;
            }
        }
        self.link_foreign_clips(&mut tracks, &foreign);
        tracks
    }

    /// The pool item of the compound clip a `Stack` inside a track stands
    /// for, built the first time its sequence is met.
    fn compound(&mut self, stack: &Value, fps: Rational) -> Option<MediaId> {
        let resolve = &stack["metadata"]["Resolve_OTIO"];
        let sequence = resolve["Sequence ID"].as_str();
        if let Some(&media) = sequence.and_then(|id| self.compounds.get(id)) {
            return Some(media);
        }
        if self.depth >= MAX_COMPOUND_DEPTH {
            self.warn(OtioWarning::UnsupportedItem {
                schema: schema(stack).to_owned(),
            });
            return None;
        }
        let fps = resolve["Sequence Fps"]
            .as_f64()
            .filter(|fps| *fps > 0.0)
            .map_or(fps, Rational::from_fps);
        self.depth += 1;
        let tracks = self.tracks(stack, fps);
        self.depth -= 1;
        let resolution = self.first_video_resolution(&tracks).unwrap_or((1920, 1080));
        let name = match stack["name"].as_str() {
            Some(name) if !name.is_empty() => name.to_owned(),
            _ => self.project.alloc_compound_name(),
        };
        let timeline_id = self.project.timelines.insert(Timeline {
            name,
            fps,
            resolution,
            tracks,
            markers: Vec::new(),
            master: Default::default(),
        });
        let media = self.project.insert_timeline_item(timeline_id, None);
        self.new_compounds.push((timeline_id, media));
        if let Some(id) = sequence {
            self.compounds.insert(id.to_owned(), media);
        }
        Some(media)
    }

    /// A transition straddles the cut: `in_offset` extends into the clip
    /// before it and `out_offset` into the one after. Reaching into both
    /// adjacent clips it is one crossing, settled by the caller once the next
    /// clip is known; on one side only it belongs to that clip's edge.
    fn transition(
        &mut self,
        item: &Value,
        fps: Rational,
        start: FrameIdx,
        track: &mut Track,
    ) -> Option<PendingTransition> {
        let venturi = &item["metadata"]["venturi"];
        let saved = serde_json::from_value::<Transition>(venturi["transition"].clone()).ok();
        let offset = |key: &str| to_frames(seconds(&item[key]).unwrap_or(0.0), fps);
        let (into_previous, into_next) = (offset("in_offset"), offset("out_offset"));
        if into_previous <= 0 && into_next <= 0 {
            return None;
        }
        let build = |duration: FrameIdx| match &saved {
            Some(transition) => Transition {
                duration,
                ..transition.clone()
            },
            None => {
                let effect = &item["metadata"]["Resolve_OTIO"]["Effects"];
                Transition {
                    kind: TransitionKind::Push,
                    duration,
                    direction: PushDirection::Left,
                    ease: resolve_ease(effect),
                    curve: resolve_curve(effect, duration),
                }
            }
        };
        if into_previous > 0
            && into_next > 0
            && let Some(left) = track.clips.last().filter(|c| c.timeline_end() == start)
        {
            return Some(PendingTransition::Crossing {
                left_clip: left.id,
                transition: build(into_previous + into_next),
                into_previous,
            });
        }
        if into_previous > 0 {
            match track.clips.last_mut() {
                Some(clip) => clip.effects.transition_out = Some(build(into_previous)),
                None => self.warn(OtioWarning::TransitionIgnored),
            }
        }
        (into_next > 0).then(|| PendingTransition::In(build(into_next)))
    }

    /// Duration occupied on the track (even if the clip is not imported)
    /// and the clip, with `true` if it does not come from Venturi.
    #[allow(clippy::too_many_arguments)]
    fn clip(
        &mut self,
        item: &Value,
        kind: TrackKind,
        fps: Rational,
        timeline_start: FrameIdx,
        cursor: f64,
        groups: &mut HashMap<GroupKey, LinkGroupId>,
    ) -> (f64, Option<(Clip, bool)>) {
        let name = item["name"].as_str().unwrap_or("");
        let reference = match item.get("media_references") {
            Some(refs) => {
                let key = item["active_media_reference_key"]
                    .as_str()
                    .unwrap_or("DEFAULT_MEDIA");
                &refs[key]
            }
            None => &item["media_reference"],
        };
        let Some((source_start, duration)) =
            time_range(&item["source_range"]).or_else(|| time_range(&reference["available_range"]))
        else {
            self.warn(OtioWarning::ClipWithoutDuration {
                clip: name.to_owned(),
            });
            return (0.0, None);
        };
        let timeline_len = to_frames(cursor + duration, fps) - timeline_start;
        if timeline_len < 1 {
            self.warn(OtioWarning::ClipShorterThanAFrame {
                clip: name.to_owned(),
            });
            return (duration, None);
        }
        let venturi = &item["metadata"]["venturi"];
        let ours = !venturi["effects"].is_null();
        let mut effects =
            serde_json::from_value::<EffectStack>(venturi["effects"].clone()).unwrap_or_default();
        let mut fades = (
            venturi["fade_in"].as_i64().unwrap_or(0) as FrameIdx,
            venturi["fade_out"].as_i64().unwrap_or(0) as FrameIdx,
        );
        let (speed, frozen) = match serde_json::from_value::<Rational>(venturi["speed"].clone()) {
            Ok(speed) => (speed, false),
            Err(_) => self.time_warp(item, name),
        };
        // `(media, start of its available range)`.
        let media = match (schema(item), schema(reference)) {
            ("Stack", _) => Some((self.compound(item, fps), 0.0)),
            (_, "ExternalReference") => {
                let url = reference["target_url"].as_str().unwrap_or("");
                let available_start =
                    time_range(&reference["available_range"]).map_or(0.0, |(start, _)| start);
                Some((self.media(url, reference, kind), available_start))
            }
            _ => None,
        };
        // Seconds into the media at the start of the clip and the media fps, to
        // translate the effect keyframes of other editors.
        let (source, conform_rate, source_offset, media_start) = match (media, schema(reference)) {
            (Some((None, _)), _) => return (duration, None),
            (Some((Some(media_id), available_start)), _) => {
                let meta = &self.project.media_pool[media_id].meta;
                if kind == TrackKind::Video && !meta.has_video {
                    self.warn(OtioWarning::AudioOnlyOnVideoTrack {
                        clip: name.to_owned(),
                    });
                    return (duration, None);
                }
                let media_fps = meta.fps;
                let conform_rate = Rational::conform_rate(fps, media_fps);
                let rate = conform_rate.divided_by(speed);
                let secs = source_start - available_start;
                let source_frame = secs * media_fps.as_f64();
                let offset = if (source_frame - source_frame.round()).abs() < 1e-6 {
                    rate.scale_round(source_frame.round() as FrameIdx)
                } else {
                    to_frames(secs / speed.as_f64(), fps)
                };
                (
                    ClipSource::Media(media_id),
                    conform_rate,
                    offset.max(0),
                    Some((secs, media_fps)),
                )
            }
            (None, "MissingReference")
                if resolve::is_adjustment_clip(children_of(item, "effects")) =>
            {
                (ClipSource::Adjustment, Rational::one(), 0, None)
            }
            (None, "GeneratorReference") if reference["generator_kind"] == "Solid Color" => {
                if effects.color.is_none() {
                    let color = generator::read_solid_color(reference).unwrap_or(Rgba::BLACK);
                    effects.color = Some(Keyframed::constant(color));
                }
                (ClipSource::SolidColor, Rational::one(), 0, None)
            }
            (None, "GeneratorReference") if reference["generator_kind"] == "Rich" => {
                if effects.title.is_none() {
                    let frame = self.timeline_resolution(venturi, &ClipSource::Text);
                    effects.title = Some(generator::read_text(reference, frame, self.measure));
                }
                (ClipSource::Text, Rational::one(), 0, None)
            }
            // Adjustment clips were matched above by their effect; one without
            // it is broken, not a title to rebuild.
            (None, "MissingReference") if kind == TrackKind::Video && name != "Adjustment Clip" => {
                self.warn(OtioWarning::Placeholder {
                    clip: name.to_owned(),
                });
                effects.title = Some(TitleParams {
                    content: name.to_owned(),
                    ..TitleParams::default()
                });
                (ClipSource::Text, Rational::one(), 0, None)
            }
            (None, other) => {
                self.warn(OtioWarning::UnsupportedReference {
                    clip: name.to_owned(),
                    schema: other.to_owned(),
                });
                return (duration, None);
            }
        };

        let resolve = &item["metadata"]["Resolve_OTIO"];
        // Our own file already carries everything in `venturi`: translating
        // Resolve's effects again would fight with it.
        if !ours {
            let frame = self.timeline_resolution(venturi, &source);
            let media = self.media_resolution(&source).unwrap_or(frame);
            let fit = (frame.0 / media.0).min(frame.1 / media.1);
            let clip_rate = match source {
                ClipSource::Media(_) => conform_rate.divided_by(speed),
                _ => Rational::one(),
            };
            let context = ResolveContext {
                span: (
                    clip_rate.unscale_round(source_offset),
                    clip_rate.unscale_round(source_offset + timeline_len - 1),
                ),
                media_start,
                rate: item["source_range"]["start_time"]["rate"]
                    .as_f64()
                    .unwrap_or(fps.as_f64()),
                display: (media.0 * fit, media.1 * fit),
                media,
                speed: speed.as_f64(),
            };
            for effect in children_of(item, "effects") {
                self.effect(effect, &mut effects, &mut fades, &context);
            }
        }
        let group_key = venturi["linked_group"]
            .as_u64()
            .map(GroupKey::Venturi)
            .or_else(|| resolve["Link Group ID"].as_u64().map(GroupKey::Resolve));
        let is_foreign = group_key.is_none();
        let linked_group = group_key.map(|key| {
            *groups
                .entry(key)
                .or_insert_with(|| self.project.alloc_link_group_id())
        });
        let audio_stream_index = venturi["audio_stream_index"]
            .as_u64()
            .or_else(|| resolve_source_track(resolve))
            .unwrap_or(0) as usize;
        let speed = if matches!(source, ClipSource::Media(_)) {
            speed
        } else {
            Rational::one()
        };
        // Placeholders stand in for content that is not in the file: off
        // until the user rebuilds it. Adjustment clips are complete.
        let disabled = item["enabled"] == false
            || (matches!(schema(reference), "MissingReference")
                && !matches!(source, ClipSource::Adjustment));
        let mut clip = Clip::new(
            self.project.alloc_clip_id(),
            source,
            source_offset,
            timeline_start,
            timeline_len,
            conform_rate,
            speed,
        );
        clip.effects = effects;
        clip.linked_group = linked_group;
        clip.audio_stream_index = audio_stream_index;
        clip.pitch_correction = venturi["pitch_correction"].as_bool().unwrap_or(true);
        clip.disabled = disabled;
        if let ClipSource::Media(_) = clip.source {
            let saved = &venturi["freeze"];
            match saved["frame"].as_i64() {
                Some(frame) => {
                    clip.freeze = Some(frame as FrameIdx);
                    clip.source_offset = saved["source_offset"].as_i64().unwrap_or(0) as FrameIdx;
                }
                None => clip.freeze = frozen.then(|| clip.source_in()),
            }
        }
        clip.fade_in = fades.0.clamp(0, timeline_len);
        clip.fade_out = fades.1.clamp(0, timeline_len);
        clip.display_color =
            serde_json::from_value(venturi["display_color"].clone()).unwrap_or(None);
        (duration, Some((clip, is_foreign)))
    }

    /// The clip speed from a `LinearTimeWarp`, and `true` for a freeze frame
    /// (`time_scalar` 0, also its own `FreezeFrame` schema): it freezes on
    /// the frame at the start of `source_range`. Reverse plays at 100%, with
    /// a warning.
    fn time_warp(&mut self, item: &Value, name: &str) -> (Rational, bool) {
        let Some(scalar) = children_of(item, "effects")
            .filter(|e| is_time_warp(e))
            .find_map(|e| e["time_scalar"].as_f64())
        else {
            return (Rational::one(), false);
        };
        if scalar > 0.0 {
            return (Rational::from_percent(scalar * 100.0), false);
        }
        if scalar == 0.0 {
            return (Rational::one(), true);
        }
        self.warn(OtioWarning::SpeedNotApplied {
            clip: name.to_owned(),
            percent: (scalar * 100.0).round() as i64,
        });
        (Rational::one(), false)
    }

    /// Brings into `effects` and `fades` what it knows how to translate; the
    /// rest ends up in `ignored_effects`, except for effects that are off or
    /// at their defaults (Resolve exports them all).
    fn effect(
        &mut self,
        effect: &Value,
        effects: &mut EffectStack,
        fades: &mut (FrameIdx, FrameIdx),
        context: &ResolveContext,
    ) {
        // Read beforehand by `time_warp`.
        if is_time_warp(effect) {
            return;
        }
        let resolve = &effect["metadata"]["Resolve_OTIO"];
        if resolve.is_null() {
            let name = effect["effect_name"]
                .as_str()
                .unwrap_or_else(|| schema(effect));
            *self
                .ignored_effects
                .entry((name.to_owned(), false))
                .or_default() += 1;
            return;
        }
        if resolve["Enabled"] == false {
            return;
        }
        let name = resolve["Effect Name"].as_str().unwrap_or("Resolve Effect");
        if name == "Dynamic Zoom" {
            if !resolve_dynamic_zoom(resolve, effects, context) {
                *self
                    .ignored_effects
                    .entry((name.to_owned(), false))
                    .or_default() += 1;
            }
            return;
        }
        let (mut translated, mut untranslated) = (false, false);
        for parameter in children_of(resolve, "Parameters") {
            let id = parameter["Parameter ID"].as_str().unwrap_or("");
            if resolve_parameter(name, id, parameter, effects, fades, context) {
                translated = true;
            } else if !is_default_parameter(parameter) {
                untranslated = true;
            }
        }
        if untranslated {
            *self
                .ignored_effects
                .entry((name.to_owned(), translated))
                .or_default() += 1;
        }
    }

    /// Resolution of the timeline, which the clip gets fitted into.
    fn timeline_resolution(&self, venturi: &Value, source: &ClipSource) -> (f32, f32) {
        serde_json::from_value::<(u32, u32)>(venturi["resolution"].clone())
            .ok()
            .map(|(w, h)| (w as f32, h as f32))
            .or_else(|| self.media_resolution(source))
            .unwrap_or((1920.0, 1080.0))
    }

    fn media_resolution(&self, source: &ClipSource) -> Option<(f32, f32)> {
        match source {
            ClipSource::Media(id) => self
                .project
                .media_pool
                .get(*id)
                .filter(|m| m.meta.width > 0)
                .map(|m| (m.meta.width as f32, m.meta.height as f32)),
            ClipSource::SolidColor | ClipSource::Text | ClipSource::Adjustment => None,
        }
    }

    /// The media at `target_url`, probed once per file. An unreadable one
    /// goes in the pool offline, described by the file, so a relink can
    /// bring its clips back.
    fn media(&mut self, url: &str, reference: &Value, kind: TrackKind) -> Option<MediaId> {
        let Some(path) = url_to_path(url, self.base_dir) else {
            self.warn(OtioWarning::UnsupportedUrl {
                url: url.to_owned(),
            });
            return None;
        };
        if let Some(&media) = self.media.get(&path) {
            if let Some(item) = self
                .offline
                .contains(&media)
                .then(|| &mut self.project.media_pool[media])
            {
                item.meta.has_video |= kind == TrackKind::Video;
                item.meta.has_audio |= kind == TrackKind::Audio;
            }
            return Some(media);
        }
        let (meta, content_hash, offline) = match (self.probe)(&path) {
            Ok((meta, content_hash)) => (meta, content_hash, false),
            Err(e) => {
                self.warn(OtioWarning::MediaUnreadable {
                    path: path.clone(),
                    error: e,
                });
                (offline_meta(reference, kind), 0, true)
            }
        };
        let media = self.project.media_pool.insert(MediaItem {
            path: path.clone(),
            meta,
            content_hash,
            compound: None,
            folder: None,
        });
        if offline {
            self.offline.insert(media);
        }
        self.media.insert(path, media);
        Some(media)
    }

    /// Other editors export video and audio of the same media as separate
    /// clips: the ones that coincide in everything get relinked.
    fn link_foreign_clips(&mut self, tracks: &mut [Track], foreign: &[ForeignClip]) {
        let mut by_span: HashMap<(MediaId, FrameIdx, FrameIdx, FrameIdx), Vec<&ForeignClip>> =
            HashMap::new();
        for clip_ref in foreign {
            let clip = &tracks[clip_ref.track].clips[clip_ref.index];
            if let ClipSource::Media(media_id) = clip.source {
                let key = (
                    media_id,
                    clip.timeline_start,
                    clip.source_offset,
                    clip.timeline_len,
                );
                by_span.entry(key).or_default().push(clip_ref);
            }
        }
        for members in by_span.into_values() {
            let kinds = |kind| members.iter().any(|m| tracks[m.track].kind == kind);
            if !(kinds(TrackKind::Video) && kinds(TrackKind::Audio)) {
                continue;
            }
            let group = self.project.alloc_link_group_id();
            for member in members {
                tracks[member.track].clips[member.index].linked_group = Some(group);
            }
        }
    }

    fn first_video_resolution(&self, tracks: &[Track]) -> Option<(u32, u32)> {
        tracks
            .iter()
            .filter(|t| t.kind == TrackKind::Video)
            .flat_map(|t| &t.clips)
            .find_map(|clip| match clip.source {
                ClipSource::Media(id) => self
                    .project
                    .media_pool
                    .get(id)
                    .filter(|m| m.meta.width > 0)
                    .map(|m| (m.meta.width, m.meta.height)),
                ClipSource::SolidColor | ClipSource::Text | ClipSource::Adjustment => None,
            })
    }

    fn warn(&mut self, warning: OtioWarning) {
        self.warnings.push(warning);
    }
}

/// What is needed to bring a Resolve parameter back into our units:
/// `media_start` (seconds into the media at the start of the clip and the
/// media fps) and `rate` (fps of Resolve's keyframes) place the keyframes,
/// `frame` and `media` denormalize the values.
struct ResolveContext {
    media_start: Option<(f64, Rational)>,
    rate: f64,
    display: (f32, f32),
    media: (f32, f32),
    speed: f64,
    /// Source frames of the clip's first and last frame.
    span: (FrameIdx, FrameIdx),
}

/// `true` if the parameter was translated. The `multiplier` is the inverse
/// of the one used on export: see `otio::resolve`.
fn resolve_parameter(
    effect: &str,
    id: &str,
    parameter: &Value,
    effects: &mut EffectStack,
    fades: &mut (FrameIdx, FrameIdx),
    context: &ResolveContext,
) -> bool {
    use TransformParam::*;
    let mut track = |param, multiplier| {
        resolve_track(parameter, effects, param, multiplier, context);
        true
    };
    match (effect, id) {
        ("Transform", "transformationZoomX") => track(ZoomX, 1.0),
        ("Transform", "transformationZoomY") => track(ZoomY, 1.0),
        ("Transform", "transformationPan") => track(PositionX, context.display.0),
        ("Transform", "transformationTilt") => track(PositionY, context.display.1),
        ("Transform", "transformationRotationAngle") => track(Rotation, -1.0),
        ("Transform", "transformationAnchorPoint") => {
            resolve_point(
                parameter,
                effects,
                [AnchorX, AnchorY],
                context.display,
                context,
            );
            true
        }
        ("Transform", "transformationFlipX") => resolve_flip(parameter, effects, 0),
        ("Transform", "transformationFlipY") => resolve_flip(parameter, effects, 1),
        ("Cropping", "cropLeft") => track(CropLeft, context.media.0),
        ("Cropping", "cropRight") => track(CropRight, context.media.0),
        ("Cropping", "cropTop") => track(CropTop, context.media.1),
        ("Cropping", "cropBottom") => track(CropBottom, context.media.1),
        ("Cropping", "cropSoftness") => track(CropSoftness, 1.0),
        ("Composite", "opacity") => track(Opacity, 1.0),
        ("Composite", "composite mode") => match parameter["Parameter Value"]
            .as_u64()
            .and_then(resolve::blend_mode)
        {
            Some(blend) => {
                effects.blend_mode = blend;
                true
            }
            // Resolve has modes we do not: let the warning through.
            None => false,
        },
        ("Video Faders", "videoFaderIn") | ("Fairlight Clip Volume and Fades", "faderIn") => {
            fades.0 = resolve_frames(parameter);
            true
        }
        ("Video Faders", "videoFaderOut") | ("Fairlight Clip Volume and Fades", "faderOut") => {
            fades.1 = resolve_frames(parameter);
            true
        }
        ("Fairlight Clip Volume and Fades", "volume") => {
            resolve_volume(parameter, effects, context);
            true
        }
        _ => false,
    }
}

/// Resolve moves a framing rectangle (`dynamicZoomScale` of the frame, centered
/// at `dynamicZoomCenter`) from the start to the end of the clip, wherever its
/// two keyframes are exported: it becomes a zoom on that rectangle, on top of
/// the transform. `false` if the transform is animated already.
fn resolve_dynamic_zoom(
    resolve: &Value,
    effects: &mut EffectStack,
    context: &ResolveContext,
) -> bool {
    use TransformParam::*;
    let ends = |id: &str| -> Option<[&Value; 2]> {
        let parameter = children_of(resolve, "Parameters").find(|p| p["Parameter ID"] == id)?;
        let mut keys: Vec<(f64, &Value)> = parameter["Key Frames"]
            .as_object()
            .into_iter()
            .flatten()
            .filter_map(|(frame, key)| Some((frame.parse().ok()?, &key["Value"])))
            .collect();
        keys.sort_by(|a, b| a.0.total_cmp(&b.0));
        match (keys.first(), keys.last()) {
            (Some(first), Some(last)) => Some([first.1, last.1]),
            _ => Some([&parameter["Parameter Value"]; 2]),
        }
    };
    let scales = ends("dynamicZoomScale").map(|e| e.map(|v| v.as_f64().unwrap_or(1.0)));
    let centers = ends("dynamicZoomCenter")
        .map(|e| e.map(|v| [0, 1].map(|i| v.get(i).and_then(Value::as_f64).unwrap_or(0.0))));
    let scales = scales.unwrap_or([1.0; 2]);
    let centers = centers.unwrap_or([[0.0; 2]; 2]);
    if scales.iter().all(|s| *s == 1.0) && centers.iter().flatten().all(|c| *c == 0.0) {
        return true;
    }
    let params = [ZoomX, ZoomY, PositionX, PositionY];
    if params
        .iter()
        .any(|p| !effects.transform.track(*p).is_constant())
    {
        return false;
    }
    let base = params.map(|p| effects.transform.track(p).default);
    let display = [context.display.0, context.display.1];
    let at = |end: usize| {
        let zoom = 1.0 / scales[end].max(0.01) as f32;
        let position =
            |axis: usize| zoom * (base[2 + axis] - centers[end][axis] as f32 * display[axis]);
        [base[0] * zoom, base[1] * zoom, position(0), position(1)]
    };
    let (start, end) = (at(0), at(1));
    for (i, param) in params.into_iter().enumerate() {
        let track = effects.transform.track_mut(param);
        if start[i] == end[i] || context.span.0 == context.span.1 {
            track.default = start[i];
        } else {
            track.upsert(context.span.0, start[i], Interpolation::Linear);
            track.upsert(context.span.1, end[i], Interpolation::Linear);
        }
    }
    true
}

fn resolve_flip(parameter: &Value, effects: &mut EffectStack, axis: usize) -> bool {
    if let Some(flipped) = parameter["Parameter Value"].as_bool() {
        effects.transform.flip[axis] = flipped;
    }
    true
}

fn resolve_frames(parameter: &Value) -> FrameIdx {
    parameter["Parameter Value"]
        .as_f64()
        .unwrap_or(0.0)
        .round()
        .max(0.0) as FrameIdx
}

fn resolve_track(
    parameter: &Value,
    effects: &mut EffectStack,
    param: TransformParam,
    multiplier: f32,
    context: &ResolveContext,
) {
    let track = effects.transform.track_mut(param);
    if let Some(value) = parameter["Parameter Value"].as_f64() {
        track.default = value as f32 * multiplier;
    }
    for (frame, value) in resolve_keyframes(parameter, context) {
        let Some(value) = value.as_f64() else {
            continue;
        };
        track.upsert(frame, value as f32 * multiplier, Interpolation::Linear);
    }
}

/// A `POINTF` feeds two tracks at once.
fn resolve_point(
    parameter: &Value,
    effects: &mut EffectStack,
    params: [TransformParam; 2],
    multiplier: (f32, f32),
    context: &ResolveContext,
) {
    let axis = |value: &Value, i: usize| value.get(i).and_then(Value::as_f64);
    let multiplier = [multiplier.0, multiplier.1];
    for (i, param) in params.into_iter().enumerate() {
        let track = effects.transform.track_mut(param);
        if let Some(value) = axis(&parameter["Parameter Value"], i) {
            track.default = value as f32 * multiplier[i];
        }
        for (frame, value) in resolve_keyframes(parameter, context) {
            let Some(value) = axis(value, i) else {
                continue;
            };
            track.upsert(frame, value as f32 * multiplier[i], Interpolation::Linear);
        }
    }
}

/// Resolve indexes its keyframes by timeline frame from the start of the
/// clip; ours go on the source frame shown there.
fn resolve_keyframes<'v>(
    parameter: &'v Value,
    context: &ResolveContext,
) -> Vec<(FrameIdx, &'v Value)> {
    let (Some(keyframes), Some((start_secs, media_fps))) =
        (parameter["Key Frames"].as_object(), context.media_start)
    else {
        return Vec::new();
    };
    keyframes
        .iter()
        .filter_map(|(frame, keyframe)| {
            let frame = frame.parse::<f64>().ok()?;
            let secs = start_secs + frame / context.rate * context.speed;
            Some((
                (secs * media_fps.as_f64()).round() as FrameIdx,
                &keyframe["Value"],
            ))
        })
        .collect()
}

fn is_default_parameter(parameter: &Value) -> bool {
    let no_keyframes = parameter["Key Frames"]
        .as_object()
        .is_none_or(|k| k.is_empty());
    no_keyframes && parameter["Parameter Value"] == parameter["Default Parameter Value"]
}

/// Resolve's volume is in dB, like `gain_db`; the keyframes become linear
/// keyframes on the corresponding source frame.
fn resolve_volume(parameter: &Value, effects: &mut EffectStack, context: &ResolveContext) {
    if let Some(db) = parameter["Parameter Value"].as_f64() {
        effects.gain_db.default = db as f32;
    }
    for (frame, value) in resolve_keyframes(parameter, context) {
        let Some(db) = value.as_f64() else { continue };
        effects
            .gain_db
            .upsert(frame, db as f32, Interpolation::Linear);
    }
}

/// The intensity of the ease, read back from the longest bezier handle of
/// the progress curve (see `resolve::transition_curve`).
fn resolve_curve(effect: &Value, duration: FrameIdx) -> f32 {
    let handles = children_of(effect, "Parameters")
        .find(|p| p["Parameter ID"] == "transitionCustomCurvesKeyframes")
        .and_then(|p| p["Key Frames"].as_object())
        .into_iter()
        .flatten()
        .flat_map(|(_, key)| ["InBez", "OutBez"].map(|h| key[h][h][0].as_f64()))
        .flatten()
        .map(f64::abs);
    let longest = handles.fold(0.0, f64::max);
    match duration > 0 {
        true => (longest as f32 / (0.6 * duration as f32)).clamp(0.0, 1.0),
        false => 0.5,
    }
}

/// The `ease` of a Resolve transition, by position in `Ease::ALL`.
fn resolve_ease(effect: &Value) -> Ease {
    children_of(effect, "Parameters")
        .find(|p| p["Parameter ID"] == "ease")
        .and_then(|p| p["Parameter Value"].as_u64())
        .and_then(|i| Ease::ALL.get(i as usize).copied())
        .unwrap_or(Ease::InOut)
}

/// The audio stream of the media Resolve takes the clip's channels from,
/// if they all come from the same one.
fn resolve_source_track(resolve: &Value) -> Option<u64> {
    let mut tracks = children_of(resolve, "Channels").map(|c| c["Source Track ID"].as_u64());
    let first = tracks.next()??;
    tracks.all(|t| t == Some(first)).then_some(first)
}

/// What the file says of a media it could not probe. Resolution unknown (0)
/// until the relink probes it.
fn offline_meta(reference: &Value, kind: TrackKind) -> MediaMeta {
    let range = &reference["available_range"];
    let fps = range["duration"]["rate"]
        .as_f64()
        .filter(|rate| *rate > 0.0)
        .map_or(Rational::new(25, 1), Rational::from_fps);
    let duration_frames = range["duration"]["value"].as_f64().unwrap_or(0.0).round() as FrameIdx;
    let has_audio = kind == TrackKind::Audio;
    // Resolve gives stills a one-frame range.
    let is_image = !has_audio && duration_frames <= 1;
    MediaMeta {
        duration_frames: if is_image {
            IMAGE_DURATION_FRAMES
        } else {
            duration_frames.max(1)
        },
        fps,
        width: 0,
        height: 0,
        has_video: !has_audio,
        has_audio,
        sample_rate: if has_audio { 48_000 } else { 0 },
        channels: if has_audio { 2 } else { 0 },
        audio_streams: has_audio as u16,
        file: Default::default(),
    }
}

fn collect_timelines<'v>(value: &'v Value, out: &mut Vec<&'v Value>) {
    match schema(value) {
        "Timeline" => out.push(value),
        "SerializableCollection" => {
            for child in children(value) {
                collect_timelines(child, out);
            }
        }
        _ => {}
    }
}

/// The schema name without the version (`"Clip.2"` → `"Clip"`).
fn is_time_warp(effect: &Value) -> bool {
    matches!(schema(effect), "LinearTimeWarp" | "FreezeFrame")
}

fn schema(value: &Value) -> &str {
    let full = value["OTIO_SCHEMA"].as_str().unwrap_or("");
    full.split('.').next().unwrap_or(full)
}

fn children(value: &Value) -> impl Iterator<Item = &Value> {
    children_of(value, "children")
}

fn children_of<'v>(value: &'v Value, key: &str) -> impl Iterator<Item = &'v Value> {
    value[key].as_array().into_iter().flatten()
}

fn seconds(time: &Value) -> Option<f64> {
    let rate = time["rate"].as_f64()?;
    (rate > 0.0).then(|| time["value"].as_f64().unwrap_or(0.0) / rate)
}

/// `(start, duration)` in seconds.
fn time_range(range: &Value) -> Option<(f64, f64)> {
    Some((seconds(&range["start_time"])?, seconds(&range["duration"])?))
}

/// Duration of a non-imported element: `source_range`, or that of the
/// children (in sequence for a track, the longest for a stack).
fn item_duration(item: &Value) -> f64 {
    if let Some((_, duration)) = time_range(&item["source_range"]) {
        return duration;
    }
    let durations = children(item)
        .filter(|child| schema(child) != "Transition")
        .map(item_duration);
    match schema(item) {
        "Stack" => durations.fold(0.0, f64::max),
        _ => durations.sum(),
    }
}

fn first_item_rate(timeline: &Value) -> Option<f64> {
    children(&timeline["tracks"])
        .flat_map(children)
        .find_map(|item| item["source_range"]["duration"]["rate"].as_f64())
}

fn to_frames(secs: f64, fps: Rational) -> FrameIdx {
    (secs * fps.as_f64()).round() as FrameIdx
}

fn url_to_path(url: &str, base_dir: &Path) -> Option<PathBuf> {
    let Some(rest) = url.strip_prefix("file://") else {
        if url.contains("://") || url.is_empty() {
            return None;
        }
        return Some(base_dir.join(url));
    };
    let rest = rest.strip_prefix("localhost").unwrap_or(rest);
    let mut bytes = Vec::with_capacity(rest.len());
    let mut iter = rest.bytes();
    while let Some(byte) = iter.next() {
        if byte == b'%' {
            let hex = [iter.next()?, iter.next()?];
            bytes.push(u8::from_str_radix(std::str::from_utf8(&hex).ok()?, 16).ok()?);
        } else {
            bytes.push(byte);
        }
    }
    Some(PathBuf::from(String::from_utf8(bytes).ok()?))
}

#[cfg(test)]
#[path = "../tests/otio/import.rs"]
mod tests;
