//! Editing operations on explicit targets. Selection and playhead belong to
//! the caller. Each operation is at most one undo step; a caller combining
//! several wraps them in `History::begin_group`/`end_group` (groups nest).

use std::collections::{BTreeSet, HashMap};

use crate::{
    AddTrack, Clip, ClipId, ClipSource, Command, CommandLabel, CompositeCommand, EffectStack,
    FrameIdx, History, Keyframed, LiftDelete, LinkClips, LinkGroupId, MediaId, MoveClips, Project,
    Rational, Rgba, RippleDeleteGap, SlipClip, SplitClip, TimelineId, TitleParams, TrackKind,
    TrimClip, TrimEdge,
};

pub type ClipRef = (usize, ClipId);

pub const DEFAULT_SOLID_COLOR: Rgba = Rgba {
    r: 1.0,
    g: 1.0,
    b: 0.0,
    a: 1.0,
};

/// Clips generated without a source media.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Generator {
    SolidColor,
    Text,
    Adjustment,
}

impl Generator {
    pub const ALL: [Generator; 3] = [
        Generator::SolidColor,
        Generator::Text,
        Generator::Adjustment,
    ];

    const DEFAULT_SECS: f64 = 5.0;

    pub fn default_len(self, timeline_fps: Rational) -> FrameIdx {
        (timeline_fps.as_f64() * Self::DEFAULT_SECS).round() as FrameIdx
    }
}

/// Where an insertion puts its clips. `extra_audio` is a track created for
/// this insertion: the first audio stream goes there.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TargetTracks {
    pub video: Option<usize>,
    pub extra_audio: Option<usize>,
}

/// A portion of a media to put on a timeline, in media frames.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MediaInsert {
    pub media_id: MediaId,
    pub source_in: FrameIdx,
    pub source_out: FrameIdx,
    pub video: bool,
    pub audio: bool,
}

/// Appends a track and returns its index.
pub fn add_track(
    project: &mut Project,
    history: &mut History,
    timeline_id: TimelineId,
    kind: TrackKind,
) -> usize {
    let index = project.timelines[timeline_id].tracks.len();
    history.do_command(project, Box::new(AddTrack::new(timeline_id, kind)));
    index
}

/// Splits at `frame` the clips of the unlocked tracks strictly covering it,
/// restricted to `only` when given. The right halves of a link group are
/// linked to each other. Returns the split clips (now the left halves) with
/// the id of their right half.
pub fn split_clips(
    project: &mut Project,
    history: &mut History,
    timeline_id: TimelineId,
    frame: FrameIdx,
    only: Option<&BTreeSet<ClipRef>>,
) -> Vec<(ClipRef, ClipId)> {
    split_where(project, history, timeline_id, frame, |track_index, clip| {
        only.is_none_or(|only| only.contains(&(track_index, clip.id)))
    })
}

fn split_where(
    project: &mut Project,
    history: &mut History,
    timeline_id: TimelineId,
    frame: FrameIdx,
    keep: impl Fn(usize, &Clip) -> bool,
) -> Vec<(ClipRef, ClipId)> {
    let targets: Vec<(usize, ClipId, Option<LinkGroupId>)> = project.timelines[timeline_id]
        .tracks
        .iter()
        .enumerate()
        .filter(|(_, track)| !track.locked)
        .flat_map(|(track_index, track)| {
            track
                .clips
                .iter()
                .filter(move |c| frame > c.timeline_start && frame < c.timeline_end())
                .map(move |c| (track_index, c))
        })
        .filter(|&(track_index, c)| keep(track_index, c))
        .map(|(track_index, c)| (track_index, c.id, c.linked_group))
        .collect();
    if targets.is_empty() {
        return Vec::new();
    }

    // Pre-allocated so the relinking commands can name the right halves.
    let new_ids: HashMap<ClipId, ClipId> = targets
        .iter()
        .map(|(_, id, _)| (*id, project.alloc_clip_id()))
        .collect();

    let mut commands: Vec<Box<dyn Command>> = targets
        .iter()
        .map(|(track_index, clip_id, _)| {
            Box::new(
                SplitClip::new(timeline_id, *track_index, *clip_id, frame)
                    .with_new_clip_id(new_ids[clip_id]),
            ) as Box<dyn Command>
        })
        .collect();

    let mut right_halves_by_group: HashMap<LinkGroupId, Vec<ClipRef>> = HashMap::new();
    for (track_index, clip_id, group) in &targets {
        if let Some(g) = group {
            right_halves_by_group
                .entry(*g)
                .or_default()
                .push((*track_index, new_ids[clip_id]));
        }
    }
    for right_halves in right_halves_by_group.into_values() {
        if right_halves.len() >= 2 {
            commands.push(Box::new(LinkClips::new(timeline_id, right_halves)));
        }
    }

    history.do_command(
        project,
        Box::new(CompositeCommand::new(CommandLabel::SplitClips, commands)),
    );
    targets
        .into_iter()
        .map(|(track_index, clip_id, _)| ((track_index, clip_id), new_ids[&clip_id]))
        .collect()
}

/// Removes `clips`, leaving gaps. Clips on locked tracks are kept.
pub fn delete_clips(
    project: &mut Project,
    history: &mut History,
    timeline_id: TimelineId,
    clips: &[ClipRef],
) {
    let tl = &project.timelines[timeline_id];
    let commands: Vec<Box<dyn Command>> = clips
        .iter()
        .filter(|&&(track_index, clip_id)| {
            !tl.is_locked(track_index) && tl.clip(track_index, clip_id).is_some()
        })
        .map(|&(track_index, clip_id)| {
            Box::new(LiftDelete::new(timeline_id, track_index, clip_id)) as Box<dyn Command>
        })
        .collect();
    if commands.is_empty() {
        return;
    }
    history.do_command(
        project,
        Box::new(CompositeCommand::new(CommandLabel::DeleteClips, commands)),
    );
}

/// Removes `clips` and their link groups, then closes the holes on every
/// unlocked track. Returns where the leftmost hole was.
pub fn ripple_delete_clips(
    project: &mut Project,
    history: &mut History,
    timeline_id: TimelineId,
    clips: &[ClipRef],
) -> Option<FrameIdx> {
    let tl = &project.timelines[timeline_id];
    let mut processed: BTreeSet<ClipRef> = BTreeSet::new();
    let mut removed: Vec<(usize, ClipId, FrameIdx, FrameIdx)> = Vec::new();
    for &(track_index, clip_id) in clips {
        for (member_track, member_id) in
            std::iter::once((track_index, clip_id)).chain(tl.linked_members(track_index, clip_id))
        {
            if tl.is_locked(member_track) || !processed.insert((member_track, member_id)) {
                continue;
            }
            let Some(clip) = tl.clip(member_track, member_id) else {
                continue;
            };
            removed.push((
                member_track,
                member_id,
                clip.timeline_start,
                clip.timeline_end(),
            ));
        }
    }
    if removed.is_empty() {
        return None;
    }
    // Closing overlapping holes one per clip would shift the rest twice.
    let merged = merge_ranges(removed.iter().map(|&(_, _, start, end)| (start, end)));

    let mut commands: Vec<Box<dyn Command>> = removed
        .iter()
        .map(|&(track_index, clip_id, _, _)| {
            Box::new(LiftDelete::new(timeline_id, track_index, clip_id)) as Box<dyn Command>
        })
        .collect();
    // Right to left: closing a hole only moves what comes after it.
    for &(start, end) in merged.iter().rev() {
        commands.push(Box::new(RippleDeleteGap::new(
            timeline_id,
            start,
            end - start,
        )));
    }
    let mark = history.begin_group();
    history.do_command(
        project,
        Box::new(CompositeCommand::new(CommandLabel::RippleDelete, commands)),
    );
    cut_overlaps(project, history, timeline_id);
    history.end_group(mark);
    merged.first().map(|&(start, _)| start)
}

/// Closes the empty interval `[start, end)` on every unlocked track.
pub fn ripple_delete_gap(
    project: &mut Project,
    history: &mut History,
    timeline_id: TimelineId,
    start: FrameIdx,
    end: FrameIdx,
) {
    let mark = history.begin_group();
    history.do_command(
        project,
        Box::new(RippleDeleteGap::new(timeline_id, start, end - start)),
    );
    cut_overlaps(project, history, timeline_id);
    history.end_group(mark);
}

/// How `delete_ranges` treats the removed material.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RangeDelete {
    /// Closes the gaps. Acts on every unlocked track: closing a gap shifts
    /// them all, so leaving material on some would make it overlap.
    Ripple,
    /// Leaves gaps, on the given tracks (all unlocked ones with `None`).
    Lift { tracks: Option<Vec<usize>> },
}

/// Removes the timeline intervals `[start, end)`: clips crossing an edge
/// are split there, the pieces inside are removed. Overlapping or touching
/// ranges are merged first.
pub fn delete_ranges(
    project: &mut Project,
    history: &mut History,
    timeline_id: TimelineId,
    ranges: &[(FrameIdx, FrameIdx)],
    mode: &RangeDelete,
) {
    let on_track = |track_index: usize| match mode {
        RangeDelete::Ripple | RangeDelete::Lift { tracks: None } => true,
        RangeDelete::Lift {
            tracks: Some(tracks),
        } => tracks.contains(&track_index),
    };
    let merged = merge_ranges(ranges.iter().copied().filter(|&(s, e)| e > s));
    let mark = history.begin_group();
    // Last to first, so rippling one range does not move the earlier ones.
    for &(start, end) in merged.iter().rev() {
        for edge in [end, start] {
            split_where(project, history, timeline_id, edge, |track_index, _| {
                on_track(track_index)
            });
        }
        let tl = &project.timelines[timeline_id];
        let inside: Vec<ClipRef> = tl
            .tracks
            .iter()
            .enumerate()
            .filter(|&(track_index, track)| !track.locked && on_track(track_index))
            .flat_map(|(track_index, track)| {
                track
                    .clips
                    .iter()
                    .filter(|c| c.timeline_start >= start && c.timeline_end() <= end)
                    .map(move |c| (track_index, c.id))
            })
            .collect();
        delete_clips(project, history, timeline_id, &inside);
        if *mode == RangeDelete::Ripple {
            ripple_delete_gap(project, history, timeline_id, start, end);
        }
    }
    history.end_group(mark);
}

/// Where the source frames `media_ranges` (`[start, end)`, frames of the
/// media) of `media_id` are on the timeline, as `(track, start, end)`: every
/// clip of that media on the unlocked tracks (only `tracks` when given),
/// wherever earlier edits moved it. A media used twice maps twice.
pub fn media_ranges_on_timeline(
    project: &Project,
    timeline_id: TimelineId,
    media_id: MediaId,
    media_ranges: &[(FrameIdx, FrameIdx)],
    tracks: Option<&[usize]>,
) -> Vec<(usize, FrameIdx, FrameIdx)> {
    let mut mapped = Vec::new();
    for (track_index, track) in project.timelines[timeline_id].tracks.iter().enumerate() {
        if track.locked || tracks.is_some_and(|t| !t.contains(&track_index)) {
            continue;
        }
        for clip in &track.clips {
            if !matches!(clip.source, ClipSource::Media(id) if id == media_id) {
                continue;
            }
            for &(start, end) in media_ranges {
                let (start, end) = (start.max(clip.source_in()), end.min(clip.source_out()));
                if start >= end {
                    continue;
                }
                let span = clip.timeline_start..=clip.timeline_end();
                let from = clip
                    .timeline_frame_at(start)
                    .clamp(*span.start(), *span.end());
                let to = clip
                    .timeline_frame_at(end)
                    .clamp(*span.start(), *span.end());
                if from < to {
                    mapped.push((track_index, from, to));
                }
            }
        }
    }
    mapped
}

/// `delete_ranges` for source material: removes the frames `media_ranges`
/// of `media_id` wherever they are on the timeline. With `Ripple` the
/// timeline closes up on every unlocked track; with `Lift` only that media's
/// clips are cut. Returns the timeline ranges it removed.
pub fn delete_media_ranges(
    project: &mut Project,
    history: &mut History,
    timeline_id: TimelineId,
    media_id: MediaId,
    media_ranges: &[(FrameIdx, FrameIdx)],
    mode: &RangeDelete,
) -> Vec<(usize, FrameIdx, FrameIdx)> {
    let tracks = match mode {
        RangeDelete::Lift { tracks } => tracks.as_deref(),
        RangeDelete::Ripple => None,
    };
    let mapped = media_ranges_on_timeline(project, timeline_id, media_id, media_ranges, tracks);
    match mode {
        RangeDelete::Ripple => {
            let ranges: Vec<(FrameIdx, FrameIdx)> =
                mapped.iter().map(|&(_, start, end)| (start, end)).collect();
            delete_ranges(project, history, timeline_id, &ranges, mode);
        }
        RangeDelete::Lift { .. } => {
            let touched: BTreeSet<usize> = mapped.iter().map(|&(track, _, _)| track).collect();
            let mark = history.begin_group();
            for track in touched {
                let ranges: Vec<(FrameIdx, FrameIdx)> = mapped
                    .iter()
                    .filter(|&&(t, _, _)| t == track)
                    .map(|&(_, start, end)| (start, end))
                    .collect();
                delete_ranges(
                    project,
                    history,
                    timeline_id,
                    &ranges,
                    &RangeDelete::Lift {
                        tracks: Some(vec![track]),
                    },
                );
            }
            history.end_group(mark);
        }
    }
    mapped
}

fn merge_ranges(ranges: impl Iterator<Item = (FrameIdx, FrameIdx)>) -> Vec<(FrameIdx, FrameIdx)> {
    let mut ranges: Vec<(FrameIdx, FrameIdx)> = ranges.collect();
    ranges.sort();
    let mut merged: Vec<(FrameIdx, FrameIdx)> = Vec::new();
    for (start, end) in ranges {
        match merged.last_mut() {
            Some((_, last_end)) if start <= *last_end => *last_end = (*last_end).max(end),
            _ => merged.push((start, end)),
        }
    }
    merged
}

/// Cuts the overlaps left by a bulk move (see `crate::cut_overlaps`).
pub fn cut_overlaps(project: &mut Project, history: &mut History, timeline_id: TimelineId) {
    let mut commands: Vec<Box<dyn Command>> = Vec::new();
    crate::cut_overlaps(project, timeline_id, &mut commands);
    if commands.is_empty() {
        return;
    }
    history.do_command(
        project,
        Box::new(CompositeCommand::new(CommandLabel::TrimClips, commands)),
    );
}

/// Inserts `clips` (track, clip, link key) overwriting what is underneath.
pub fn insert_clips<K: PartialEq>(
    project: &mut Project,
    history: &mut History,
    timeline_id: TimelineId,
    clips: Vec<(usize, Clip, Option<K>)>,
    label: CommandLabel,
) {
    let commands = crate::insert_overwriting(project, timeline_id, clips);
    history.do_command(project, Box::new(CompositeCommand::new(label, commands)));
}

/// Inserts the video clip and one audio clip per stream at `start`, all in
/// one link group, creating the audio tracks that are missing. Returns the
/// new clips, or `None` for an unknown media.
pub fn insert_media(
    project: &mut Project,
    history: &mut History,
    timeline_id: TimelineId,
    insert: MediaInsert,
    start: FrameIdx,
    tracks: TargetTracks,
) -> Option<Vec<ClipRef>> {
    let meta = project.media_pool.get(insert.media_id)?.meta.clone();
    let mark = history.begin_group();
    let tl = &project.timelines[timeline_id];
    let rate = Rational::conform_rate(tl.fps, meta.fps);
    let has_audio_tracks = tl.first_track_index(TrackKind::Audio).is_some();
    let mut audio_track_indices: Vec<usize> = tl
        .tracks_of_kind(TrackKind::Audio)
        .filter(|(_, t)| !t.locked)
        .map(|(i, _)| i)
        .collect();
    if let Some(extra) = tracks.extra_audio {
        audio_track_indices.retain(|i| *i != extra);
        audio_track_indices.insert(0, extra);
    }

    // Without audio tracks the audio of a video is dropped, but an
    // audio-only insertion creates them.
    let takes_video = insert.video && meta.has_video;
    let num_audio_streams = if !(insert.audio && meta.has_audio) {
        0
    } else if has_audio_tracks || !takes_video {
        meta.audio_stream_count()
    } else {
        0
    };
    while audio_track_indices.len() < num_audio_streams {
        audio_track_indices.push(add_track(project, history, timeline_id, TrackKind::Audio));
    }

    let audio_clip_ids: Vec<ClipId> = (0..num_audio_streams)
        .map(|_| project.alloc_clip_id())
        .collect();
    let video_id = takes_video.then(|| project.alloc_clip_id());
    let new_clip = |id| {
        Clip::from_source_range(
            id,
            ClipSource::Media(insert.media_id),
            insert.source_in,
            insert.source_out,
            start,
            rate,
        )
    };
    let mut new_clips: Vec<(usize, Clip, Option<u64>)> = Vec::new();
    if let (Some(id), Some(video_track)) = (video_id, tracks.video) {
        new_clips.push((video_track, new_clip(id), Some(0)));
    }
    for (stream_index, (&track_index, &clip_id)) in
        audio_track_indices.iter().zip(&audio_clip_ids).enumerate()
    {
        let mut audio_clip = new_clip(clip_id);
        audio_clip.audio_stream_index = stream_index;
        new_clips.push((track_index, audio_clip, Some(0)));
    }
    let refs = new_clips
        .iter()
        .map(|(track, clip, _)| (*track, clip.id))
        .collect();
    insert_clips(
        project,
        history,
        timeline_id,
        new_clips,
        CommandLabel::InsertClips,
    );
    history.end_group(mark);
    Some(refs)
}

/// Inserts a generator clip, of its default length with `len: None`.
/// Returns its id.
pub fn insert_generator(
    project: &mut Project,
    history: &mut History,
    timeline_id: TimelineId,
    generator: Generator,
    track_index: usize,
    start: FrameIdx,
    len: Option<FrameIdx>,
) -> ClipId {
    let len = len.unwrap_or_else(|| generator.default_len(project.timelines[timeline_id].fps));
    let source = match generator {
        Generator::SolidColor => ClipSource::SolidColor,
        Generator::Text => ClipSource::Text,
        Generator::Adjustment => ClipSource::Adjustment,
    };
    let id = project.alloc_clip_id();
    let mut clip = Clip::from_source_range(id, source, 0, len, start, Rational::one());
    match generator {
        Generator::SolidColor => {
            clip.effects = EffectStack {
                color: Some(Keyframed::constant(DEFAULT_SOLID_COLOR)),
                ..Default::default()
            }
        }
        Generator::Text => clip.effects.title = Some(TitleParams::default()),
        Generator::Adjustment => {}
    }
    insert_clips(
        project,
        history,
        timeline_id,
        vec![(track_index, clip, None::<u64>)],
        CommandLabel::InsertClips,
    );
    id
}

/// Moves clips to `(track, start)`, overwriting what is at the destination:
/// `moves` is `(clip, from track, to track, new start)`.
pub fn move_clips(
    project: &mut Project,
    history: &mut History,
    timeline_id: TimelineId,
    moves: Vec<(ClipId, usize, usize, FrameIdx)>,
) {
    // The moved clips stay out of the room making, even at their old place.
    let ranges: Vec<(usize, FrameIdx, FrameIdx)> = moves
        .iter()
        .filter_map(|&(id, from_track, to_track, start)| {
            let len = project.timelines[timeline_id]
                .clip(from_track, id)?
                .timeline_len;
            Some((to_track, start, start + len))
        })
        .collect();
    let exclude: Vec<ClipRef> = moves
        .iter()
        .flat_map(|&(id, from_track, to_track, _)| [(from_track, id), (to_track, id)])
        .collect();
    let mut commands: Vec<Box<dyn Command>> = Vec::new();
    crate::make_room_for_ranges(project, timeline_id, &ranges, &exclude, &mut commands);
    commands.push(Box::new(MoveClips::new(timeline_id, moves)));
    history.do_command(
        project,
        Box::new(CompositeCommand::new(CommandLabel::MoveClips, commands)),
    );
}

/// Where `edge` of `clip` can go: at least one frame must remain, and the
/// source bounds the extension (not the neighbours, which get overwritten).
pub fn trim_range(project: &Project, clip: &Clip, edge: TrimEdge) -> (FrameIdx, FrameIdx) {
    match edge {
        TrimEdge::Start => {
            let min_value = if clip.is_generator() {
                0
            } else {
                clip.timeline_frame_at(0).max(0)
            };
            let max_value = clip.timeline_end() - 1;
            (min_value, max_value.max(min_value))
        }
        TrimEdge::End => {
            // Generators and freeze frames have no source length.
            let max_value = match &clip.source {
                ClipSource::Media(_) if clip.freeze.is_some() => None,
                ClipSource::Media(media_id) => project
                    .media_pool
                    .get(*media_id)
                    .map(|item| clip.timeline_frame_at(item.meta.duration_frames)),
                ClipSource::SolidColor | ClipSource::Text | ClipSource::Adjustment => None,
            }
            .unwrap_or(FrameIdx::MAX);
            let min_value = clip.timeline_start + 1;
            (min_value, max_value.max(min_value))
        }
    }
}

/// The stretch of timeline a clip takes by moving `edge` to `new_value`,
/// if it grows: `None` if it shrinks.
pub fn grown_range(
    clip: &Clip,
    track_index: usize,
    edge: TrimEdge,
    new_value: FrameIdx,
) -> Option<(usize, FrameIdx, FrameIdx)> {
    match edge {
        TrimEdge::Start if new_value < clip.timeline_start => {
            Some((track_index, new_value, clip.timeline_start))
        }
        TrimEdge::End if new_value > clip.timeline_end() => {
            Some((track_index, clip.timeline_end(), new_value))
        }
        _ => None,
    }
}

/// Moves one edge of a clip to `new_value`, overwriting what the clip grows
/// over. `new_value` must be inside `trim_range`.
pub fn trim_clip(
    project: &mut Project,
    history: &mut History,
    timeline_id: TimelineId,
    (track_index, clip_id): ClipRef,
    edge: TrimEdge,
    new_value: FrameIdx,
) {
    let Some(clip) = project.timelines[timeline_id].clip(track_index, clip_id) else {
        return;
    };
    let grown: Vec<_> = grown_range(clip, track_index, edge, new_value)
        .into_iter()
        .collect();
    let mut commands: Vec<Box<dyn Command>> = Vec::new();
    crate::make_room_for_ranges(
        project,
        timeline_id,
        &grown,
        &[(track_index, clip_id)],
        &mut commands,
    );
    commands.push(Box::new(TrimClip::new(
        timeline_id,
        track_index,
        clip_id,
        edge,
        new_value,
    )));
    history.do_command(
        project,
        Box::new(CompositeCommand::new(CommandLabel::TrimClips, commands)),
    );
}

/// How far the content of `clip` can slip, in timeline frames, before its
/// in or out point leaves the media. `None` for generators (no source to
/// slide) and offline media.
pub fn slip_range(project: &Project, clip: &Clip) -> Option<(FrameIdx, FrameIdx)> {
    let ClipSource::Media(media_id) = clip.source else {
        return None;
    };
    let item = project.media_pool.get(media_id)?;
    let media_end = clip.timeline_frame_at(item.meta.duration_frames);
    Some((
        -clip.source_offset,
        (media_end - clip.timeline_end()).max(0),
    ))
}

/// Slips every clip in `clips` by the same `delta`, so linked video and audio
/// stay in sync. `delta` must be inside the `slip_range` of each one.
pub fn slip_clips(
    project: &mut Project,
    history: &mut History,
    timeline_id: TimelineId,
    clips: &[ClipRef],
    delta: FrameIdx,
) {
    if delta == 0 || clips.is_empty() {
        return;
    }
    let commands: Vec<Box<dyn Command>> = clips
        .iter()
        .map(|&(track_index, clip_id)| {
            Box::new(SlipClip::new(timeline_id, track_index, clip_id, delta)) as Box<dyn Command>
        })
        .collect();
    history.do_command(
        project,
        Box::new(CompositeCommand::new(CommandLabel::SlipClips, commands)),
    );
}

#[cfg(test)]
#[path = "tests/edit.rs"]
mod tests;
