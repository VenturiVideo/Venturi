//! The tools that change a timeline. Each one validates everything first,
//! then applies as a single undo step (`one_step`).

use std::collections::{BTreeSet, HashSet};

use serde_json::{Value, json};
use vv_core::edit::{self, ClipRef, Generator, MediaInsert, RangeDelete, TargetTracks};
use vv_core::{
    ClipColor, ClipSource, CommandLabel, Ease, FadeEdge, FrameIdx, LinkClips, Marker, Project,
    PushDirection, Rgba, SetClipColor, SetClipFade, SetClipsDisabled, SetClipsDisplayColor,
    SetMarker, SetTrackFlag, TimelineId, TrackFlag, TrackKind, TransformParam, Transition,
    TransitionKind, TrimEdge, UnlinkClip,
};
use vv_session::Session;

use crate::ids;
use crate::json::{clip_json, marker_json, timeline_json, track_name};
use crate::tools::*;

type Result<T> = std::result::Result<T, ToolError>;

fn fail<T>(message: impl Into<String>) -> Result<T> {
    Err(ToolError(message.into()))
}

/// `apply` runs as one undo step, named `label` (or after its first
/// command). It should not fail once it started changing things; if it
/// does, what it applied is undone.
fn one_step(
    session: &mut Session,
    timeline: TimelineId,
    label: Option<CommandLabel>,
    apply: impl FnOnce(&mut Session) -> Result<Value>,
) -> ToolResult {
    let before = session.history.position();
    let mark = session.history.begin_group();
    let result = apply(session);
    match label {
        Some(label) => session.history.end_group_as(mark, label),
        None => session.history.end_group(mark),
    }
    match result {
        Ok(mut value) => {
            session.sync_timeline_media(timeline);
            // Lets the agent chain edits without reading the timeline again.
            if let Some(object) = value.as_object_mut() {
                object.insert(
                    "revision".into(),
                    json!(session.timeline_revision(timeline)),
                );
            }
            Ok(ToolOutput::json(value))
        }
        Err(e) => {
            if session.history.position() > before {
                session.history.undo(&mut session.project);
            }
            Err(e)
        }
    }
}

fn ensure_unlocked(project: &Project, timeline: TimelineId, track_index: usize) -> Result<()> {
    if project.timelines[timeline].tracks[track_index].locked {
        return fail(format!(
            "track {} is locked",
            track_name(project, timeline, track_index)
        ));
    }
    Ok(())
}

fn ensure_kind(
    project: &Project,
    timeline: TimelineId,
    track_index: usize,
    kind: TrackKind,
) -> Result<()> {
    if project.timelines[timeline].tracks[track_index].kind != kind {
        return fail(format!(
            "track {} is not a {} track",
            track_name(project, timeline, track_index),
            match kind {
                TrackKind::Video => "video",
                TrackKind::Audio => "audio",
            }
        ));
    }
    Ok(())
}

fn clips_json(project: &Project, timeline: TimelineId, refs: &[ClipRef]) -> Vec<Value> {
    refs.iter()
        .filter_map(|&(track, id)| {
            // A clip may have changed track, e.g. after a move.
            let track = if project.timelines[timeline].clip(track, id).is_some() {
                track
            } else {
                ids::clip_ref(project, timeline, &id.0.to_string()).ok()?.0
            };
            let clip = project.timelines[timeline].clip(track, id)?;
            Some(clip_json(project, timeline, track, clip))
        })
        .collect()
}

fn rgba([r, g, b, a]: [f32; 4]) -> Result<Rgba> {
    if [r, g, b, a].iter().any(|c| !(0.0..=1.0).contains(c)) {
        return fail("color components go from 0 to 1");
    }
    Ok(Rgba { r, g, b, a })
}

pub(crate) fn add_track(session: &mut Session, args: AddTrackArgs) -> ToolResult {
    let timeline = ids::timeline_id(&session.project, &args.timeline_id)?;
    ids::check_revision(session, timeline, args.if_revision.as_deref())?;
    let kind = match args.kind {
        TrackKindArg::Video => TrackKind::Video,
        TrackKindArg::Audio => TrackKind::Audio,
    };
    one_step(session, timeline, Some(CommandLabel::AddTrack), |s| {
        let index = edit::add_track(&mut s.project, &mut s.history, timeline, kind);
        Ok(json!({ "track": track_name(&s.project, timeline, index) }))
    })
}

pub(crate) fn set_track(session: &mut Session, args: SetTrackArgs) -> ToolResult {
    let timeline = ids::timeline_id(&session.project, &args.timeline_id)?;
    ids::check_revision(session, timeline, args.if_revision.as_deref())?;
    let index = ids::track_index(&session.project, timeline, &args.track)?;
    one_step(session, timeline, None, |s| {
        for (flag, value) in [
            (TrackFlag::Muted, args.muted),
            (TrackFlag::Solo, args.solo),
            (TrackFlag::Locked, args.locked),
        ] {
            if let Some(value) = value {
                s.history.do_command(
                    &mut s.project,
                    Box::new(SetTrackFlag::new(timeline, index, flag, value)),
                );
            }
        }
        let track = &s.project.timelines[timeline].tracks[index];
        Ok(json!({
            "track": track_name(&s.project, timeline, index),
            "muted": track.muted,
            "solo": track.solo,
            "locked": track.locked,
        }))
    })
}

pub(crate) fn insert_clip(session: &mut Session, args: InsertClipArgs) -> ToolResult {
    let project = &session.project;
    let timeline = ids::timeline_id(project, &args.timeline_id)?;
    ids::check_revision(session, timeline, args.if_revision.as_deref())?;
    let media_id = ids::media_id(project, &args.media_id)?;
    if project.would_create_a_cycle(media_id, timeline) {
        return fail("that timeline contains this one: inserting it would nest it in itself");
    }
    let meta = &project.media_pool[media_id].meta;
    let default_out = if meta.is_image() {
        (meta.fps.as_f64() * 5.0).round() as FrameIdx
    } else {
        meta.duration_frames
    };
    let source_in = args.source_in.unwrap_or(0);
    let source_out = args.source_out.unwrap_or(default_out);
    if source_in < 0 || source_in >= source_out {
        return fail(format!("empty source range {source_in}..{source_out}"));
    }
    if !meta.is_image() && source_out > meta.duration_frames {
        return fail(format!(
            "source_out {source_out} is past the end of the media ({} frames)",
            meta.duration_frames
        ));
    }
    if args.at < 0 {
        return fail("`at` must not be negative");
    }
    let video = args.video && meta.has_video;
    let audio = args.audio && meta.has_audio;
    if !video && !audio {
        return fail("nothing to insert: no video or audio selected from this media");
    }
    let video_track = match (&args.video_track, video) {
        (Some(name), true) => {
            let index = ids::track_index(project, timeline, name)?;
            ensure_kind(project, timeline, index, TrackKind::Video)?;
            ensure_unlocked(project, timeline, index)?;
            Some(index)
        }
        _ => None,
    };
    let audio_track = match (&args.audio_track, audio) {
        (Some(name), true) => {
            let index = ids::track_index(project, timeline, name)?;
            ensure_kind(project, timeline, index, TrackKind::Audio)?;
            ensure_unlocked(project, timeline, index)?;
            Some(index)
        }
        _ => None,
    };
    one_step(session, timeline, Some(CommandLabel::InsertClips), |s| {
        let tl = &s.project.timelines[timeline];
        let video_track = match (
            video,
            video_track,
            tl.first_unlocked_track_index(TrackKind::Video),
        ) {
            (false, _, _) => None,
            (true, Some(index), _) | (true, None, Some(index)) => Some(index),
            (true, None, None) => Some(edit::add_track(
                &mut s.project,
                &mut s.history,
                timeline,
                TrackKind::Video,
            )),
        };
        // `insert_media` drops a video's audio without audio tracks.
        let has_audio_track = s.project.timelines[timeline]
            .first_unlocked_track_index(TrackKind::Audio)
            .is_some();
        let extra_audio = match (audio, audio_track, has_audio_track) {
            (true, Some(index), _) => Some(index),
            (true, None, false) => Some(edit::add_track(
                &mut s.project,
                &mut s.history,
                timeline,
                TrackKind::Audio,
            )),
            _ => None,
        };
        let refs = edit::insert_media(
            &mut s.project,
            &mut s.history,
            timeline,
            MediaInsert {
                media_id,
                source_in,
                source_out,
                video,
                audio,
            },
            args.at,
            TargetTracks {
                video: video_track,
                extra_audio,
            },
        )
        .unwrap_or_default();
        Ok(json!({ "clips": clips_json(&s.project, timeline, &refs) }))
    })
}

pub(crate) fn split(session: &mut Session, args: SplitArgs) -> ToolResult {
    let timeline = ids::timeline_id(&session.project, &args.timeline_id)?;
    ids::check_revision(session, timeline, args.if_revision.as_deref())?;
    let only: Option<BTreeSet<ClipRef>> = match &args.clip_ids {
        Some(clip_ids) => Some(
            ids::clip_refs(&session.project, timeline, clip_ids)?
                .into_iter()
                .collect(),
        ),
        None => None,
    };
    one_step(session, timeline, Some(CommandLabel::SplitClips), |s| {
        let pieces = edit::split_clips(
            &mut s.project,
            &mut s.history,
            timeline,
            args.frame,
            only.as_ref(),
        );
        if pieces.is_empty() {
            return fail(format!(
                "no clip of an unlocked track crosses frame {}",
                args.frame
            ));
        }
        let pieces: Vec<Value> = pieces
            .iter()
            .map(|&((track, left), right)| {
                json!({
                    "track": track_name(&s.project, timeline, track),
                    "left": left.0.to_string(),
                    "right": right.0.to_string(),
                })
            })
            .collect();
        Ok(json!({ "split": pieces }))
    })
}

pub(crate) fn delete_clips(session: &mut Session, args: DeleteClipsArgs) -> ToolResult {
    let timeline = ids::timeline_id(&session.project, &args.timeline_id)?;
    ids::check_revision(session, timeline, args.if_revision.as_deref())?;
    let refs = ids::clip_refs(&session.project, timeline, &args.clip_ids)?;
    for &(track, _) in &refs {
        ensure_unlocked(&session.project, timeline, track)?;
    }
    one_step(
        session,
        timeline,
        Some(if args.ripple {
            CommandLabel::RippleDelete
        } else {
            CommandLabel::DeleteClips
        }),
        |s| {
            if args.ripple {
                edit::ripple_delete_clips(&mut s.project, &mut s.history, timeline, &refs);
            } else {
                edit::delete_clips(&mut s.project, &mut s.history, timeline, &refs);
            }
            Ok(json!({ "timeline": timeline_json(s, timeline) }))
        },
    )
}

pub(crate) fn delete_ranges(session: &mut Session, args: DeleteRangesArgs) -> ToolResult {
    let project = &session.project;
    let timeline = ids::timeline_id(project, &args.timeline_id)?;
    ids::check_revision(session, timeline, args.if_revision.as_deref())?;
    if args.ranges.is_empty() {
        return fail("no ranges given");
    }
    if let Some([start, end]) = args.ranges.iter().find(|[s, e]| *s < 0 || e <= s) {
        return fail(format!("invalid range [{start}, {end})"));
    }
    let mode = match (args.ripple, &args.tracks) {
        (true, Some(_)) => {
            return fail(
                "`tracks` only applies without `ripple`: rippling shifts every unlocked track",
            );
        }
        (true, None) => RangeDelete::Ripple,
        (false, None) => RangeDelete::Lift { tracks: None },
        (false, Some(names)) => {
            let mut tracks = Vec::new();
            for name in names {
                let index = ids::track_index(project, timeline, name)?;
                ensure_unlocked(project, timeline, index)?;
                tracks.push(index);
            }
            RangeDelete::Lift {
                tracks: Some(tracks),
            }
        }
    };
    let ranges: Vec<(FrameIdx, FrameIdx)> = args.ranges.iter().map(|&[s, e]| (s, e)).collect();
    let media = match &args.media_id {
        Some(id) => {
            let media = ids::media_id(project, id)?;
            let tracks = match &mode {
                RangeDelete::Lift { tracks } => tracks.as_deref(),
                RangeDelete::Ripple => None,
            };
            if edit::media_ranges_on_timeline(project, timeline, media, &ranges, tracks).is_empty()
            {
                return fail(
                    "none of those frames of the media are on this timeline (on unlocked tracks)",
                );
            }
            Some(media)
        }
        None => None,
    };
    one_step(
        session,
        timeline,
        Some(if args.ripple {
            CommandLabel::RippleDelete
        } else {
            CommandLabel::DeleteClips
        }),
        |s| {
            let removed = match media {
                Some(media) => edit::delete_media_ranges(
                    &mut s.project,
                    &mut s.history,
                    timeline,
                    media,
                    &ranges,
                    &mode,
                ),
                None => {
                    edit::delete_ranges(&mut s.project, &mut s.history, timeline, &ranges, &mode);
                    Vec::new()
                }
            };
            let mut value = json!({ "timeline": timeline_json(s, timeline) });
            if media.is_some() {
                value["removed"] = removed
                    .iter()
                    .map(|&(track, start, end)| {
                        json!({
                            "track": track_name(&s.project, timeline, track),
                            "start": start,
                            "end": end,
                        })
                    })
                    .collect();
            }
            Ok(value)
        },
    )
}

pub(crate) fn move_clips(session: &mut Session, args: MoveClipsArgs) -> ToolResult {
    let project = &session.project;
    let timeline = ids::timeline_id(project, &args.timeline_id)?;
    ids::check_revision(session, timeline, args.if_revision.as_deref())?;
    if args.moves.is_empty() {
        return fail("no moves given");
    }
    let mut seen = HashSet::new();
    let mut moves = Vec::new();
    for m in &args.moves {
        let (from, clip_id) = ids::clip_ref(project, timeline, &m.clip_id)?;
        if !seen.insert(clip_id) {
            return fail(format!("clip {} is moved twice", m.clip_id));
        }
        let to = match &m.track {
            Some(name) => ids::track_index(project, timeline, name)?,
            None => from,
        };
        ensure_kind(
            project,
            timeline,
            to,
            project.timelines[timeline].tracks[from].kind,
        )?;
        ensure_unlocked(project, timeline, from)?;
        ensure_unlocked(project, timeline, to)?;
        if m.start < 0 {
            return fail("a clip cannot start before frame 0");
        }
        moves.push((clip_id, from, to, m.start));
    }
    let refs: Vec<ClipRef> = moves.iter().map(|&(id, _, to, _)| (to, id)).collect();
    one_step(session, timeline, Some(CommandLabel::MoveClips), |s| {
        edit::move_clips(&mut s.project, &mut s.history, timeline, moves);
        Ok(json!({ "clips": clips_json(&s.project, timeline, &refs) }))
    })
}

pub(crate) fn trim_clip(session: &mut Session, args: TrimClipArgs) -> ToolResult {
    let project = &session.project;
    let timeline = ids::timeline_id(project, &args.timeline_id)?;
    ids::check_revision(session, timeline, args.if_revision.as_deref())?;
    let clip_ref = ids::clip_ref(project, timeline, &args.clip_id)?;
    ensure_unlocked(project, timeline, clip_ref.0)?;
    let edge = match args.edge {
        EdgeArg::Start => TrimEdge::Start,
        EdgeArg::End => TrimEdge::End,
    };
    let clip = project.timelines[timeline]
        .clip(clip_ref.0, clip_ref.1)
        .expect("found above");
    let (min, max) = edit::trim_range(project, clip, edge);
    if !(min..=max).contains(&args.frame) {
        return fail(format!(
            "the {} edge can go from frame {min} to {}",
            match edge {
                TrimEdge::Start => "start",
                TrimEdge::End => "end",
            },
            if max == FrameIdx::MAX {
                "any later frame".to_string()
            } else {
                format!("frame {max}")
            }
        ));
    }
    one_step(session, timeline, Some(CommandLabel::TrimClips), |s| {
        edit::trim_clip(
            &mut s.project,
            &mut s.history,
            timeline,
            clip_ref,
            edge,
            args.frame,
        );
        Ok(json!({ "clips": clips_json(&s.project, timeline, &[clip_ref]) }))
    })
}

pub(crate) fn set_clip_properties(
    session: &mut Session,
    args: SetClipPropertiesArgs,
) -> ToolResult {
    let project = &session.project;
    let timeline = ids::timeline_id(project, &args.timeline_id)?;
    ids::check_revision(session, timeline, args.if_revision.as_deref())?;
    let refs = ids::clip_refs(project, timeline, &args.clip_ids)?;
    let tl = &project.timelines[timeline];
    for &(track, id) in &refs {
        ensure_unlocked(project, timeline, track)?;
        let clip = tl.clip(track, id).expect("found above");
        for (fade, name) in [(args.fade_in, "fade_in"), (args.fade_out, "fade_out")] {
            if fade.is_some_and(|f| !(0..=clip.timeline_len).contains(&f)) {
                return fail(format!(
                    "{name} of clip {} must be 0 to its length, {}",
                    id.0, clip.timeline_len
                ));
            }
        }
        if args.fill_color.is_some() && !matches!(clip.source, ClipSource::SolidColor) {
            return fail(format!("clip {} is not a solid color clip", id.0));
        }
    }
    if args.opacity.is_some_and(|o| !(0.0..=100.0).contains(&o)) {
        return fail("opacity goes from 0 to 100");
    }
    let color = args.fill_color.map(rgba).transpose()?;
    let mut params = Vec::new();
    if let Some(o) = args.opacity {
        params.push((TransformParam::Opacity, o));
    }
    if let Some([x, y]) = args.position {
        params.extend([
            (TransformParam::PositionX, x),
            (TransformParam::PositionY, y),
        ]);
    }
    if let Some([x, y]) = args.scale {
        params.extend([(TransformParam::ZoomX, x), (TransformParam::ZoomY, y)]);
    }
    if let Some(r) = args.rotation {
        params.push((TransformParam::Rotation, r));
    }
    let mut warnings = Vec::new();
    for &(track, id) in &refs {
        let clip = tl.clip(track, id).expect("found above");
        for &(param, _) in &params {
            if !clip.effects.transform.track(param).is_constant() {
                warnings.push(format!(
                    "clip {}: {param:?} has keyframes, which override the value set",
                    id.0
                ));
            }
        }
        if args.gain_db.is_some() && !clip.effects.gain_db.is_constant() {
            warnings.push(format!(
                "clip {}: gain has keyframes, which override the value set",
                id.0
            ));
        }
    }
    one_step(session, timeline, None, |s| {
        let do_command = |s: &mut Session, cmd: Box<dyn vv_core::Command>| {
            s.history.do_command(&mut s.project, cmd)
        };
        for &(track, id) in &refs {
            for &(param, value) in &params {
                do_command(
                    s,
                    Box::new(vv_core::set_clip_transform_param(
                        timeline, track, id, param, value,
                    )),
                );
            }
            if let Some(gain) = args.gain_db {
                do_command(
                    s,
                    Box::new(vv_core::set_clip_gain(timeline, track, id, gain)),
                );
            }
            for (fade, edge) in [(args.fade_in, FadeEdge::In), (args.fade_out, FadeEdge::Out)] {
                if let Some(fade) = fade {
                    do_command(
                        s,
                        Box::new(SetClipFade::new(timeline, track, id, edge, fade)),
                    );
                }
            }
            if let Some(color) = color {
                do_command(s, Box::new(SetClipColor::new(timeline, track, id, color)));
            }
        }
        if let Some(disabled) = args.disabled {
            do_command(
                s,
                Box::new(SetClipsDisabled::new(timeline, refs.clone(), disabled)),
            );
        }
        Ok(json!({
            "clips": clips_json(&s.project, timeline, &refs),
            "warnings": warnings,
        }))
    })
}

/// Video track for a new generator clip: the named one, else the first
/// unlocked one (created if needed, inside the undo step).
fn generator_track(
    project: &Project,
    timeline: TimelineId,
    name: Option<&str>,
) -> Result<Option<usize>> {
    let Some(name) = name else {
        return Ok(None);
    };
    let index = ids::track_index(project, timeline, name)?;
    ensure_kind(project, timeline, index, TrackKind::Video)?;
    ensure_unlocked(project, timeline, index)?;
    Ok(Some(index))
}

fn add_generator(
    session: &mut Session,
    timeline_id: &str,
    if_revision: Option<&str>,
    generator: Generator,
    at: FrameIdx,
    duration: Option<FrameIdx>,
    track: Option<&str>,
    customize: impl FnOnce(&mut Session, TimelineId, ClipRef),
) -> ToolResult {
    let timeline = ids::timeline_id(&session.project, timeline_id)?;
    ids::check_revision(session, timeline, if_revision)?;
    if at < 0 {
        return fail("`at` must not be negative");
    }
    if duration.is_some_and(|d| d <= 0) {
        return fail("the duration must be at least one frame");
    }
    let track = generator_track(&session.project, timeline, track)?;
    one_step(session, timeline, Some(CommandLabel::InsertClips), |s| {
        let track = match track
            .or_else(|| s.project.timelines[timeline].first_unlocked_track_index(TrackKind::Video))
        {
            Some(index) => index,
            None => edit::add_track(&mut s.project, &mut s.history, timeline, TrackKind::Video),
        };
        let id = edit::insert_generator(
            &mut s.project,
            &mut s.history,
            timeline,
            generator,
            track,
            at,
            duration,
        );
        customize(s, timeline, (track, id));
        Ok(json!({ "clips": clips_json(&s.project, timeline, &[(track, id)]) }))
    })
}

pub(crate) fn add_title(session: &mut Session, args: AddTitleArgs) -> ToolResult {
    let color = args.color.map(rgba).transpose()?;
    if args.size.is_some_and(|size| size <= 0.0) {
        return fail("the size must be positive");
    }
    add_generator(
        session,
        &args.timeline_id,
        args.if_revision.as_deref(),
        Generator::Text,
        args.at,
        args.duration,
        args.track.as_deref(),
        |s, timeline, (track, id)| {
            let clip = s.project.timelines[timeline]
                .clip(track, id)
                .expect("just inserted");
            let mut title = clip.effects.title.clone().unwrap_or_default();
            title.content = args.text;
            if let Some(size) = args.size {
                title.size = size;
            }
            if let Some(color) = color {
                title.color = color;
            }
            if let Some(position) = args.position {
                title.position = position;
            }
            s.history.do_command(
                &mut s.project,
                Box::new(vv_core::set_clip_title(timeline, track, id, title)),
            );
        },
    )
}

pub(crate) fn add_solid_color(session: &mut Session, args: AddSolidColorArgs) -> ToolResult {
    let color = args.color.map(rgba).transpose()?;
    add_generator(
        session,
        &args.timeline_id,
        args.if_revision.as_deref(),
        Generator::SolidColor,
        args.at,
        args.duration,
        args.track.as_deref(),
        |s, timeline, (track, id)| {
            if let Some(color) = color {
                s.history.do_command(
                    &mut s.project,
                    Box::new(SetClipColor::new(timeline, track, id, color)),
                );
            }
        },
    )
}

pub(crate) fn add_adjustment_clip(
    session: &mut Session,
    args: AddAdjustmentClipArgs,
) -> ToolResult {
    add_generator(
        session,
        &args.timeline_id,
        args.if_revision.as_deref(),
        Generator::Adjustment,
        args.at,
        args.duration,
        args.track.as_deref(),
        |_, _, _| {},
    )
}

pub(crate) fn link_clips(session: &mut Session, args: ClipsArgs) -> ToolResult {
    let timeline = ids::timeline_id(&session.project, &args.timeline_id)?;
    ids::check_revision(session, timeline, args.if_revision.as_deref())?;
    let refs = ids::clip_refs(&session.project, timeline, &args.clip_ids)?;
    if refs.len() < 2 {
        return fail("linking needs at least two clips");
    }
    one_step(session, timeline, Some(CommandLabel::LinkClips), |s| {
        s.history.do_command(
            &mut s.project,
            Box::new(LinkClips::new(timeline, refs.clone())),
        );
        Ok(json!({ "clips": clips_json(&s.project, timeline, &refs) }))
    })
}

pub(crate) fn unlink_clips(session: &mut Session, args: ClipsArgs) -> ToolResult {
    let timeline = ids::timeline_id(&session.project, &args.timeline_id)?;
    ids::check_revision(session, timeline, args.if_revision.as_deref())?;
    let refs = ids::clip_refs(&session.project, timeline, &args.clip_ids)?;
    let tl = &session.project.timelines[timeline];
    let mut groups = HashSet::new();
    let targets: Vec<ClipRef> = refs
        .iter()
        .copied()
        .filter(|&(track, id)| {
            tl.clip(track, id)
                .and_then(|c| c.linked_group)
                .is_some_and(|group| groups.insert(group))
        })
        .collect();
    if targets.is_empty() {
        return fail("none of these clips is linked");
    }
    one_step(session, timeline, Some(CommandLabel::UnlinkClips), |s| {
        for (track, id) in targets {
            s.history.do_command(
                &mut s.project,
                Box::new(UnlinkClip::new(timeline, track, id)),
            );
        }
        Ok(json!({ "clips": clips_json(&s.project, timeline, &refs) }))
    })
}

fn palette(color: PaletteColor) -> ClipColor {
    match color {
        PaletteColor::Red => ClipColor::Red,
        PaletteColor::Orange => ClipColor::Orange,
        PaletteColor::Yellow => ClipColor::Yellow,
        PaletteColor::Green => ClipColor::Green,
        PaletteColor::Cyan => ClipColor::Cyan,
        PaletteColor::Blue => ClipColor::Blue,
        PaletteColor::Indigo => ClipColor::Indigo,
        PaletteColor::Purple => ClipColor::Purple,
        PaletteColor::Magenta => ClipColor::Magenta,
        PaletteColor::Rose => ClipColor::Rose,
        PaletteColor::Slate => ClipColor::Slate,
        PaletteColor::Gray => ClipColor::Gray,
    }
}

fn clip_color(color: ClipColorArg) -> Option<ClipColor> {
    Some(match color {
        ClipColorArg::Red => ClipColor::Red,
        ClipColorArg::Orange => ClipColor::Orange,
        ClipColorArg::Yellow => ClipColor::Yellow,
        ClipColorArg::Green => ClipColor::Green,
        ClipColorArg::Cyan => ClipColor::Cyan,
        ClipColorArg::Blue => ClipColor::Blue,
        ClipColorArg::Indigo => ClipColor::Indigo,
        ClipColorArg::Purple => ClipColor::Purple,
        ClipColorArg::Magenta => ClipColor::Magenta,
        ClipColorArg::Rose => ClipColor::Rose,
        ClipColorArg::Slate => ClipColor::Slate,
        ClipColorArg::Gray => ClipColor::Gray,
        ClipColorArg::None => return None,
    })
}

pub(crate) fn set_clip_color(session: &mut Session, args: SetClipColorArgs) -> ToolResult {
    let timeline = ids::timeline_id(&session.project, &args.timeline_id)?;
    ids::check_revision(session, timeline, args.if_revision.as_deref())?;
    let refs = ids::clip_refs(&session.project, timeline, &args.clip_ids)?;
    for &(track, _) in &refs {
        ensure_unlocked(&session.project, timeline, track)?;
    }
    one_step(
        session,
        timeline,
        Some(CommandLabel::ClipDisplayColor),
        |s| {
            s.history.do_command(
                &mut s.project,
                Box::new(SetClipsDisplayColor::new(
                    timeline,
                    refs.clone(),
                    clip_color(args.color),
                )),
            );
            Ok(json!({ "clips": clips_json(&s.project, timeline, &refs) }))
        },
    )
}

pub(crate) fn set_clip_masks(session: &mut Session, args: SetClipMasksArgs) -> ToolResult {
    let timeline = ids::timeline_id(&session.project, &args.timeline_id)?;
    ids::check_revision(session, timeline, args.if_revision.as_deref())?;
    let (track, id) = ids::clip_ref(&session.project, timeline, &args.clip_id)?;
    ensure_kind(&session.project, timeline, track, TrackKind::Video)?;
    ensure_unlocked(&session.project, timeline, track)?;
    let resolution = session.project.timelines[timeline].resolution;
    let masks = args
        .masks
        .iter()
        .map(|m| clip_mask(m, resolution))
        .collect::<Result<Vec<_>>>()?;
    let refs = [(track, id)];
    one_step(session, timeline, Some(CommandLabel::Masks), |s| {
        s.history.do_command(
            &mut s.project,
            Box::new(vv_core::set_clip_masks(timeline, track, id, masks)),
        );
        Ok(json!({ "clips": clips_json(&s.project, timeline, &refs) }))
    })
}

fn clip_mask(arg: &MaskArg, resolution: (u32, u32)) -> Result<vv_core::ClipMask> {
    use vv_core::{MaskParam as P, MaskShape};
    let shape = match arg.shape {
        MaskShapeArg::Rectangle => MaskShape::Rectangle,
        MaskShapeArg::Ellipse => MaskShape::Ellipse,
        MaskShapeArg::Path => MaskShape::Path,
    };
    let mut mask = vv_core::ClipMask::new(shape, resolution);
    mask.invert = arg.invert;
    mask.mode = match arg.mode.unwrap_or(MaskModeArg::Add) {
        MaskModeArg::Add => vv_core::MaskMode::Add,
        MaskModeArg::Subtract => vv_core::MaskMode::Subtract,
        MaskModeArg::Intersect => vv_core::MaskMode::Intersect,
    };
    if arg.size.is_some_and(|[w, h]| w < 0.0 || h < 0.0) {
        return fail("`size` cannot be negative");
    }
    if arg.feather.is_some_and(|f| f < 0.0) || arg.roundness.is_some_and(|r| r < 0.0) {
        return fail("`feather` and `roundness` cannot be negative");
    }
    if arg.opacity.is_some_and(|o| !(0.0..=100.0).contains(&o)) {
        return fail("`opacity` must be between 0 and 100");
    }
    let values = [
        (P::CenterX, arg.center.map(|c| c[0])),
        (P::CenterY, arg.center.map(|c| c[1])),
        (P::Width, arg.size.map(|s| s[0])),
        (P::Height, arg.size.map(|s| s[1])),
        (P::Rotation, arg.rotation),
        (P::Roundness, arg.roundness),
        (P::Feather, arg.feather),
        (P::Expansion, arg.expansion),
        (P::Opacity, arg.opacity),
    ];
    for (param, value) in values {
        if let Some(value) = value {
            mask.track_mut(param).default = value;
        }
    }
    match (&arg.points, shape) {
        (Some(points), MaskShape::Path) => {
            if points.len() < 3 {
                return fail("a path needs at least 3 points");
            }
            mask.path.default.points = points
                .iter()
                .map(|p| vv_core::PathPoint::corner(*p))
                .collect();
        }
        (None, MaskShape::Path) => return fail("a path mask needs `points`"),
        (Some(_), _) => return fail("`points` is only for a path mask"),
        (None, _) => {}
    }
    Ok(mask)
}

pub(crate) fn set_transition(session: &mut Session, args: SetTransitionArgs) -> ToolResult {
    let timeline = ids::timeline_id(&session.project, &args.timeline_id)?;
    ids::check_revision(session, timeline, args.if_revision.as_deref())?;
    let refs = ids::clip_refs(&session.project, timeline, &args.clip_ids)?;
    for &(track, _) in &refs {
        ensure_kind(&session.project, timeline, track, TrackKind::Video)?;
        ensure_unlocked(&session.project, timeline, track)?;
    }
    if args.duration.is_some_and(|d| d <= 0) {
        return fail("the duration must be at least one frame");
    }
    if args.curve.is_some_and(|c| !(0.0..=1.0).contains(&c)) {
        return fail("`curve` must be between 0 and 1");
    }
    let edge = match args.edge {
        EdgeArg::Start => FadeEdge::In,
        EdgeArg::End => FadeEdge::Out,
    };
    let tl = &session.project.timelines[timeline];
    // Same defaults as a transition dropped from the Effects panel.
    let default = Transition {
        kind: TransitionKind::Push,
        duration: ((tl.fps.as_f64() * 0.45).round() as FrameIdx).max(1),
        direction: PushDirection::Right,
        ease: Ease::InOut,
        curve: 0.5,
    };
    let mut warnings = Vec::new();
    let mut values = Vec::with_capacity(refs.len());
    for &(track, id) in &refs {
        if args.kind == Some(TransitionKindArg::None) {
            values.push(None);
            continue;
        }
        let clip = tl.clip(track, id).expect("resolved above");
        let current = match edge {
            FadeEdge::In => &clip.effects.transition_in,
            FadeEdge::Out => &clip.effects.transition_out,
        };
        let mut transition = current.clone().unwrap_or_else(|| default.clone());
        if let Some(duration) = args.duration {
            transition.duration = duration;
        }
        if let Some(direction) = args.direction {
            transition.direction = match direction {
                DirectionArg::Left => PushDirection::Left,
                DirectionArg::Right => PushDirection::Right,
                DirectionArg::Up => PushDirection::Up,
                DirectionArg::Down => PushDirection::Down,
            };
        }
        if let Some(ease) = args.ease {
            transition.ease = match ease {
                EaseArg::None => Ease::None,
                EaseArg::In => Ease::In,
                EaseArg::Out => Ease::Out,
                EaseArg::InOut => Ease::InOut,
            };
        }
        if let Some(curve) = args.curve {
            transition.curve = curve;
        }
        if transition.duration > clip.timeline_len {
            warnings.push(format!(
                "clip {}: duration cut to the clip's length, {} frames",
                id.0, clip.timeline_len
            ));
            transition.duration = clip.timeline_len;
        }
        values.push(Some(transition));
    }
    one_step(session, timeline, Some(CommandLabel::Transition), |s| {
        for (&(track, id), value) in refs.iter().zip(values) {
            s.history.do_command(
                &mut s.project,
                Box::new(vv_core::set_clip_transition(
                    timeline, track, id, edge, value,
                )),
            );
        }
        Ok(json!({
            "clips": clips_json(&s.project, timeline, &refs),
            "warnings": warnings,
        }))
    })
}

pub(crate) fn get_markers(session: &Session, args: TimelineArgs) -> ToolResult {
    let timeline = ids::timeline_id(&session.project, &args.timeline_id)?;
    let markers: Vec<Value> = session.project.timelines[timeline]
        .markers
        .iter()
        .map(marker_json)
        .collect();
    Ok(ToolOutput::json(json!({
        "markers": markers,
        "revision": session.timeline_revision(timeline),
    })))
}

fn check_marker_span(start: FrameIdx, duration: FrameIdx) -> Result<()> {
    if start < 0 || duration < 0 {
        return fail("marker start and duration must not be negative");
    }
    Ok(())
}

pub(crate) fn add_marker(session: &mut Session, args: AddMarkerArgs) -> ToolResult {
    let timeline = ids::timeline_id(&session.project, &args.timeline_id)?;
    ids::check_revision(session, timeline, args.if_revision.as_deref())?;
    let duration = args.duration.unwrap_or(0);
    check_marker_span(args.at, duration)?;
    let marker = Marker {
        id: session.project.timelines[timeline].alloc_marker_id(),
        start: args.at,
        duration,
        note: args.note.unwrap_or_default(),
        color: args.color.map_or_else(Marker::default_color, palette),
    };
    one_step(session, timeline, None, |s| {
        let value = marker_json(&marker);
        s.history
            .do_command(&mut s.project, Box::new(SetMarker::add(timeline, marker)));
        Ok(value)
    })
}

pub(crate) fn edit_marker(session: &mut Session, args: EditMarkerArgs) -> ToolResult {
    let timeline = ids::timeline_id(&session.project, &args.timeline_id)?;
    ids::check_revision(session, timeline, args.if_revision.as_deref())?;
    let id = ids::marker_id(&session.project, timeline, &args.marker_id)?;
    let mut marker = session.project.timelines[timeline]
        .marker(id)
        .expect("found above")
        .clone();
    if let Some(at) = args.at {
        marker.start = at;
    }
    if let Some(duration) = args.duration {
        marker.duration = duration;
    }
    if let Some(note) = args.note {
        marker.note = note;
    }
    if let Some(color) = args.color {
        marker.color = palette(color);
    }
    check_marker_span(marker.start, marker.duration)?;
    one_step(session, timeline, None, |s| {
        let value = marker_json(&marker);
        s.history
            .do_command(&mut s.project, Box::new(SetMarker::edit(timeline, marker)));
        Ok(value)
    })
}

pub(crate) fn delete_marker(session: &mut Session, args: MarkerArgs) -> ToolResult {
    let timeline = ids::timeline_id(&session.project, &args.timeline_id)?;
    ids::check_revision(session, timeline, args.if_revision.as_deref())?;
    let id = ids::marker_id(&session.project, timeline, &args.marker_id)?;
    one_step(session, timeline, None, |s| {
        s.history
            .do_command(&mut s.project, Box::new(SetMarker::remove(timeline, id)));
        Ok(json!({ "deleted": args.marker_id }))
    })
}

#[cfg(test)]
#[path = "tests/edit_tools.rs"]
mod tests;
