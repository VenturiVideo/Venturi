//! `source_range` is expressed at the timeline fps, like `source_offset`/
//! `timeline_len` (see `Clip`): exact even for conformed clips, where in
//! the media's fps the start would fall mid-frame. What OTIO cannot
//! represent (linked groups, audio streams, the parts of the transform
//! Resolve does not read) ends up in `metadata.venturi`; the transform and
//! the transitions also travel in Resolve's own namespace (see `resolve`).

use super::generator;
use super::resolve::{self, Scale};
use super::{MeasureTitle, OtioError};
use crate::model::{
    Clip, ClipSource, FrameIdx, Project, Rational, Rgba, TimelineId, Track, TrackKind,
};
use serde_json::{Value, json};
use std::path::Path;

pub fn export_otio(
    project: &Project,
    timeline: TimelineId,
    path: &Path,
    measure: Option<MeasureTitle>,
) -> Result<(), OtioError> {
    let contents = serde_json::to_string_pretty(&timeline_to_otio(project, timeline, measure))?;
    std::fs::write(path, contents)?;
    Ok(())
}

pub fn timeline_to_otio(
    project: &Project,
    timeline_id: TimelineId,
    measure: Option<MeasureTitle>,
) -> Value {
    let timeline = &project.timelines[timeline_id];
    let fps = timeline.fps;
    let tracks: Vec<Value> = timeline
        .tracks
        .iter()
        .enumerate()
        .map(|(i, track)| {
            track_to_otio(
                project,
                track,
                timeline.track_label(i),
                fps,
                timeline.resolution,
                measure,
            )
        })
        .collect();

    json!({
        "OTIO_SCHEMA": "Timeline.1",
        "name": timeline.name,
        "global_start_time": rational_time(0, fps),
        // Resolve stamps its own version on a file it wrote; without it it
        // does not trust the `Resolve_OTIO` namespace of the generators.
        "metadata": {
            "Resolve_OTIO": { "Resolve OTIO Meta Version": "1.0" },
            "venturi": {
                "fps": timeline.fps,
                "resolution": timeline.resolution,
            }
        },
        "tracks": {
            "OTIO_SCHEMA": "Stack.1",
            "name": "tracks",
            "source_range": null,
            "effects": [],
            "markers": [],
            "enabled": true,
            "metadata": {},
            "children": tracks,
        },
    })
}

/// OTIO wants sequential tracks: the holes between clips become `Gap`s.
fn track_to_otio(
    project: &Project,
    track: &Track,
    name: String,
    fps: Rational,
    resolution: (u32, u32),
    measure: Option<MeasureTitle>,
) -> Value {
    let mut children = Vec::new();
    let mut cursor = 0;
    for (i, clip) in track.clips.iter().enumerate() {
        if clip.timeline_start > cursor {
            children.push(gap(clip.timeline_start - cursor, fps));
        }
        if let Some(transition) = &clip.effects.transition_in {
            children.push(resolve::transition_to_otio(
                transition,
                (0, transition.duration),
                fps,
            ));
        }
        children.push(clip_to_otio(
            project, clip, track.kind, fps, resolution, measure,
        ));
        if let Some(transition) = &clip.effects.transition_out {
            children.push(resolve::transition_to_otio(
                transition,
                (transition.duration, 0),
                fps,
            ));
        }
        let next = track.clips.get(i + 1);
        if let Some(crossing) = track.crossing_from(clip.id)
            && next.is_some_and(|n| {
                n.id == crossing.right_clip && n.timeline_start == clip.timeline_end()
            })
        {
            children.push(resolve::transition_to_otio(
                &crossing.transition,
                crossing.split(),
                fps,
            ));
        }
        cursor = clip.timeline_end();
    }
    json!({
        "OTIO_SCHEMA": "Track.1",
        "name": name,
        "kind": match track.kind {
            TrackKind::Video => "Video",
            TrackKind::Audio => "Audio",
        },
        "source_range": null,
        "effects": [],
        "markers": [],
        "enabled": !track.muted,
        "metadata": { "Resolve_OTIO": { "Locked": false } },
        "children": children,
    })
}

fn clip_to_otio(
    project: &Project,
    clip: &Clip,
    kind: TrackKind,
    fps: Rational,
    resolution: (u32, u32),
    measure: Option<MeasureTitle>,
) -> Value {
    let frame = (resolution.0 as f32, resolution.1 as f32);
    let (name, media_reference) = match &clip.source {
        ClipSource::Media(media_id) => match project.media_pool.get(*media_id) {
            Some(item) => (
                item.path
                    .file_name()
                    .map_or_else(String::new, |n| n.to_string_lossy().into_owned()),
                json!({
                    "OTIO_SCHEMA": "ExternalReference.1",
                    "name": "",
                    "target_url": file_url(&item.path),
                    "available_range": time_range(0, item.meta.duration_frames, item.meta.fps),
                    "available_image_bounds": null,
                    "metadata": {},
                }),
            ),
            None => (String::new(), missing_reference()),
        },
        // Resolve names a generator clip after its kind: ours used to carry
        // the colour or the text, which its importer does not expect.
        ClipSource::SolidColor => {
            let color = clip
                .effects
                .color
                .as_ref()
                .map_or(Rgba::BLACK, |c| c.default);
            ("Solid Color".to_owned(), generator::solid_color(color))
        }
        ClipSource::Text => {
            let title = clip.effects.title.clone().unwrap_or_default();
            ("Text".to_owned(), generator::text(&title, frame, measure))
        }
        ClipSource::Adjustment => ("Adjustment Clip".to_owned(), missing_reference()),
    };

    let media = match &clip.source {
        ClipSource::Media(id) => project
            .media_pool
            .get(*id)
            .map_or(frame, |m| (m.meta.width as f32, m.meta.height as f32)),
        ClipSource::SolidColor | ClipSource::Text | ClipSource::Adjustment => frame,
    };
    let scale = Scale::new(media, frame);

    json!({
        "OTIO_SCHEMA": "Clip.2",
        "name": name,
        // OTIO's start is in media time, the duration in timeline time.
        "source_range": {
            "OTIO_SCHEMA": "TimeRange.1",
            "start_time": {
                "OTIO_SCHEMA": "RationalTime.1",
                "rate": fps.as_f64(),
                "value": match clip.freeze {
                    Some(frame) => clip.conform_rate().scale_round(frame) as f64,
                    None => clip.source_offset as f64 * clip.speed().as_f64(),
                },
            },
            "duration": rational_time(clip.timeline_len, fps),
        },
        "effects": resolve::clip_effects(clip, kind, &scale),
        "markers": [],
        "enabled": !clip.disabled,
        "metadata": {
            "Resolve_OTIO": {},
            "venturi": {
                "effects": clip.effects,
                "linked_group": clip.linked_group,
                "audio_stream_index": clip.audio_stream_index,
                "fade_in": clip.fade_in,
                "fade_out": clip.fade_out,
                "display_color": clip.display_color,
                "speed": clip.speed(),
                "pitch_correction": clip.pitch_correction,
                "freeze": clip.freeze.map(|frame| json!({
                    "frame": frame,
                    "source_offset": clip.source_offset,
                })),
            }
        },
        "media_references": { "DEFAULT_MEDIA": media_reference },
        "active_media_reference_key": "DEFAULT_MEDIA",
    })
}

fn missing_reference() -> Value {
    json!({
        "OTIO_SCHEMA": "MissingReference.1",
        "name": "",
        "available_range": null,
        "available_image_bounds": null,
        "metadata": {},
    })
}

fn gap(len: FrameIdx, fps: Rational) -> Value {
    json!({
        "OTIO_SCHEMA": "Gap.1",
        "name": "",
        "source_range": time_range(0, len, fps),
        "effects": [],
        "markers": [],
        "enabled": true,
        "metadata": {},
    })
}

pub(super) fn rational_time(value: FrameIdx, fps: Rational) -> Value {
    json!({ "OTIO_SCHEMA": "RationalTime.1", "rate": fps.as_f64(), "value": value as f64 })
}

fn time_range(start: FrameIdx, duration: FrameIdx, fps: Rational) -> Value {
    json!({
        "OTIO_SCHEMA": "TimeRange.1",
        "start_time": rational_time(start, fps),
        "duration": rational_time(duration, fps),
    })
}

fn file_url(path: &Path) -> String {
    let absolute = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
    let mut url = String::from("file://");
    for byte in absolute.to_string_lossy().bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' | b'/' => {
                url.push(byte as char)
            }
            _ => url.push_str(&format!("%{byte:02X}")),
        }
    }
    url
}

#[cfg(test)]
#[path = "../tests/otio/export.rs"]
mod tests;
