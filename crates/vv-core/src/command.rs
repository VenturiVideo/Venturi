//! Command pattern for undo/redo. Every command captures by itself the state
//! needed to invert itself at the moment it is applied.

use crate::grade::GradeParam;
use crate::mask::{ClipMask, MaskParam};
use crate::model::{
    AudioEffect, BlurDirection, ChannelStrip, Clip, ClipAttributes, ClipColor, ClipFilter, ClipId,
    ClipSource, CrossTransition, EffectStack, FilterKind, FrameIdx, GAIN_DB_MAX, GAIN_DB_MIN,
    Interpolation, Keyframed, LinkGroupId, Marker, MarkerId, MediaId, MediaItem, MediaMeta,
    ProcessingPrecision, Project, Rational, Rgba, Timeline, TimelineId, TitleParams, Track,
    TrackKind, Transform, TransformParam, Transition,
};
use std::cell::RefCell;
use std::collections::{BTreeSet, HashMap};
use std::path::PathBuf;

/// `Send`: an editor serving MCP runs commands from another thread too.
pub trait Command: std::fmt::Debug + Send + std::any::Any {
    fn apply(&mut self, project: &mut Project);
    fn undo(&self, project: &mut Project);
    fn label(&self) -> CommandLabel;
}

/// Name of a history step; the translated text is chosen by the app.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandLabel {
    AddTrack,
    RemoveTrack,
    MuteTrack,
    SoloTrack,
    LockTrack,
    ArmTrack,
    RecordVoiceover,
    MixerGain,
    MixerPan,
    AddAudioEffect,
    RemoveAudioEffect,
    MoveAudioEffect,
    EditAudioEffect,
    ToggleAudioEffect,
    ToggleClipsDisabled,
    InsertClips,
    PasteClips,
    DuplicateClips,
    DeleteClips,
    RippleDelete,
    MoveClips,
    TrimClips,
    SlipClips,
    UnlinkClips,
    LinkClips,
    SplitClips,
    Fade,
    Transform,
    Flip,
    Gain,
    ResetGain,
    Title,
    ResetTransform,
    ClipColor,
    ClipDisplayColor,
    SetKeyframe,
    RemoveKeyframe,
    RemoveMedia,
    RelinkMedia,
    Filters,
    Masks,
    BlendMode,
    Transition,
    MakeCompoundClip,
    MoveKeyframes,
    SetInterpolation,
    PasteAttributes,
    ClipSpeed,
    RemoveSilences,
    AddMarker,
    EditMarker,
    MoveMarker,
    DeleteMarker,
    ImportMedia,
    ImportOtio,
    NewTimeline,
    DuplicateTimeline,
    RenameTimeline,
    NewFolder,
    RenameFolder,
    MoveToFolder,
    DeleteFolder,
    ProcessingPrecision,
}

/// Several commands in a single history step.
#[derive(Debug)]
pub struct CompositeCommand {
    commands: Vec<Box<dyn Command>>,
    label: CommandLabel,
}

impl CompositeCommand {
    pub fn new(label: CommandLabel, commands: Vec<Box<dyn Command>>) -> Self {
        Self { commands, label }
    }
}

impl Command for CompositeCommand {
    fn label(&self) -> CommandLabel {
        self.label
    }

    fn apply(&mut self, project: &mut Project) {
        for cmd in &mut self.commands {
            cmd.apply(project);
        }
    }

    fn undo(&self, project: &mut Project) {
        for cmd in self.commands.iter().rev() {
            cmd.undo(project);
        }
    }
}

/// Start point of an undo group, see `History::begin_group`.
#[derive(Debug, Clone, Copy)]
pub struct GroupMark(usize);

/// A history step that later commands can join, see `History::join`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct JoinableStep(u64);

#[derive(Default)]
pub struct History {
    undo_stack: Vec<Box<dyn Command>>,
    redo_stack: Vec<Box<dyn Command>>,
    generation: u64,
    /// Bumped by every change to the stacks: a `JoinableStep` is valid
    /// only while it is unchanged.
    revision: u64,
}

impl History {
    pub fn do_command(&mut self, project: &mut Project, mut cmd: Box<dyn Command>) {
        cmd.apply(project);
        self.undo_stack.push(cmd);
        self.redo_stack.clear();
        self.generation += 1;
        self.revision += 1;
    }

    /// Applies `cmd` as part of `step` if nothing else touched the history
    /// since `step` was returned; otherwise as a new step named `label`.
    /// Returns the step to join next. For work that lands piece by piece
    /// but is undone as one (an import).
    pub fn join(
        &mut self,
        project: &mut Project,
        step: Option<JoinableStep>,
        label: CommandLabel,
        mut cmd: Box<dyn Command>,
    ) -> JoinableStep {
        cmd.apply(project);
        self.generation += 1;
        let top = (step == Some(JoinableStep(self.revision)))
            .then(|| self.undo_stack.last_mut())
            .flatten()
            .and_then(|top| {
                (top.as_mut() as &mut dyn std::any::Any).downcast_mut::<CompositeCommand>()
            });
        match top {
            Some(composite) => composite.commands.push(cmd),
            None => {
                self.undo_stack
                    .push(Box::new(CompositeCommand::new(label, vec![cmd])));
                self.redo_stack.clear();
            }
        }
        self.revision += 1;
        JoinableStep(self.revision)
    }

    /// From here to `end_group` the commands become a single undo step: for
    /// an action that decomposes into several commands (e.g. a drop of several media).
    pub fn begin_group(&mut self) -> GroupMark {
        GroupMark(self.undo_stack.len())
    }

    /// The group takes the name of its first command.
    pub fn end_group(&mut self, mark: GroupMark) {
        self.close_group(mark, None);
    }

    /// For groups whose first command is incidental (e.g. the track created
    /// on the fly by a drop) and would not give the right name.
    pub fn end_group_as(&mut self, mark: GroupMark, label: CommandLabel) {
        self.close_group(mark, Some(label));
    }

    fn close_group(&mut self, mark: GroupMark, label: Option<CommandLabel>) {
        self.revision += 1;
        let commands = self.undo_stack.split_off(mark.0.min(self.undo_stack.len()));
        // A single step is wrapped only to rename it: it may be the one an
        // edit operation already grouped.
        let renames = |label: CommandLabel| commands[0].label() != label;
        if commands.len() > 1 || (commands.len() == 1 && label.is_some_and(renames)) {
            let label = label.unwrap_or_else(|| commands[0].label());
            self.undo_stack
                .push(Box::new(CompositeCommand::new(label, commands)));
        } else {
            self.undo_stack.extend(commands);
        }
    }

    pub fn undo(&mut self, project: &mut Project) {
        if let Some(cmd) = self.undo_stack.pop() {
            cmd.undo(project);
            self.redo_stack.push(cmd);
            self.generation += 1;
            self.revision += 1;
        }
    }

    pub fn redo(&mut self, project: &mut Project) {
        if let Some(mut cmd) = self.redo_stack.pop() {
            cmd.apply(project);
            self.undo_stack.push(cmd);
            self.generation += 1;
            self.revision += 1;
        }
    }

    /// All the steps, oldest first: the first `position()` are applied,
    /// the others can be redone.
    pub fn labels(&self) -> impl Iterator<Item = CommandLabel> + '_ {
        self.undo_stack
            .iter()
            .chain(self.redo_stack.iter().rev())
            .map(|cmd| cmd.label())
    }

    pub fn position(&self) -> usize {
        self.undo_stack.len()
    }

    /// Undoes or redoes until the first `position` steps are left applied.
    pub fn go_to(&mut self, project: &mut Project, position: usize) {
        while self.undo_stack.len() > position {
            self.undo(project);
        }
        while self.undo_stack.len() < position && !self.redo_stack.is_empty() {
            self.redo(project);
        }
    }

    /// Changes on every effective modification: comparing it is enough to know whether the
    /// project changed, without diffing it.
    pub fn generation(&self) -> u64 {
        self.generation
    }
}

/// Appends an empty track: for compositing only the relative order
/// between video tracks matters.
#[derive(Debug)]
pub struct AddTrack {
    pub timeline: TimelineId,
    pub kind: TrackKind,
    /// Index assigned by `apply` (the last of `tracks` at that moment),
    /// known only afterwards.
    index: Option<usize>,
}

impl AddTrack {
    pub fn new(timeline: TimelineId, kind: TrackKind) -> Self {
        Self {
            timeline,
            kind,
            index: None,
        }
    }

    /// The index of the just created track, known only after `apply`.
    pub fn track_index(&self) -> Option<usize> {
        self.index
    }
}

impl Command for AddTrack {
    fn label(&self) -> CommandLabel {
        CommandLabel::AddTrack
    }

    fn apply(&mut self, project: &mut Project) {
        let tl = &mut project.timelines[self.timeline];
        tl.tracks.push(Track::new(self.kind));
        self.index = Some(tl.tracks.len() - 1);
    }

    fn undo(&self, project: &mut Project) {
        let Some(index) = self.index else {
            return;
        };
        let tl = &mut project.timelines[self.timeline];
        if index < tl.tracks.len() {
            tl.tracks.remove(index);
        }
    }
}

/// Removes a track with its clips. Keeping at least one per kind is a
/// rule of the UI, not of the model.
#[derive(Debug)]
pub struct RemoveTrack {
    pub timeline: TimelineId,
    pub track_index: usize,
    removed: Option<Track>,
}

impl RemoveTrack {
    pub fn new(timeline: TimelineId, track_index: usize) -> Self {
        Self {
            timeline,
            track_index,
            removed: None,
        }
    }
}

impl Command for RemoveTrack {
    fn label(&self) -> CommandLabel {
        CommandLabel::RemoveTrack
    }

    fn apply(&mut self, project: &mut Project) {
        let tl = &mut project.timelines[self.timeline];
        if self.track_index >= tl.tracks.len() {
            return;
        }
        self.removed = Some(tl.tracks.remove(self.track_index));
    }

    fn undo(&self, project: &mut Project) {
        let Some(track) = self.removed.clone() else {
            return;
        };
        let tl = &mut project.timelines[self.timeline];
        let index = self.track_index.min(tl.tracks.len());
        tl.tracks.insert(index, track);
    }
}

/// State of a track editable from its header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrackFlag {
    Muted,
    Solo,
    Locked,
    Armed,
}

impl TrackFlag {
    fn field(self, track: &mut Track) -> &mut bool {
        match self {
            TrackFlag::Muted => &mut track.muted,
            TrackFlag::Solo => &mut track.solo,
            TrackFlag::Locked => &mut track.locked,
            TrackFlag::Armed => &mut track.armed,
        }
    }
}

#[derive(Debug)]
pub struct SetTrackFlag {
    pub timeline: TimelineId,
    pub track_index: usize,
    pub flag: TrackFlag,
    pub value: bool,
    old: Option<bool>,
}

impl SetTrackFlag {
    pub fn new(timeline: TimelineId, track_index: usize, flag: TrackFlag, value: bool) -> Self {
        Self {
            timeline,
            track_index,
            flag,
            value,
            old: None,
        }
    }
}

impl Command for SetTrackFlag {
    fn label(&self) -> CommandLabel {
        match self.flag {
            TrackFlag::Muted => CommandLabel::MuteTrack,
            TrackFlag::Solo => CommandLabel::SoloTrack,
            TrackFlag::Locked => CommandLabel::LockTrack,
            TrackFlag::Armed => CommandLabel::ArmTrack,
        }
    }

    fn apply(&mut self, project: &mut Project) {
        let Some(track) = project.timelines[self.timeline]
            .tracks
            .get_mut(self.track_index)
        else {
            return;
        };
        let field = self.flag.field(track);
        self.old = Some(*field);
        *field = self.value;
    }

    fn undo(&self, project: &mut Project) {
        let (Some(old), Some(track)) = (
            self.old,
            project.timelines[self.timeline]
                .tracks
                .get_mut(self.track_index),
        ) else {
            return;
        };
        *self.flag.field(track) = old;
    }
}

/// A strip of the audio mixer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MixerChannel {
    Track(usize),
    Master,
}

fn channel_strip(
    project: &mut Project,
    timeline: TimelineId,
    channel: MixerChannel,
) -> Option<&mut ChannelStrip> {
    let tl = project.timelines.get_mut(timeline)?;
    match channel {
        MixerChannel::Track(index) => tl.tracks.get_mut(index).map(|t| &mut t.mix),
        MixerChannel::Master => Some(&mut tl.master),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MixerParam {
    GainDb,
    Pan,
}

impl MixerParam {
    fn field(self, strip: &mut ChannelStrip) -> &mut f32 {
        match self {
            MixerParam::GainDb => &mut strip.gain_db,
            MixerParam::Pan => &mut strip.pan,
        }
    }

    pub fn range(self) -> std::ops::RangeInclusive<f32> {
        match self {
            MixerParam::GainDb => GAIN_DB_MIN..=GAIN_DB_MAX,
            MixerParam::Pan => -1.0..=1.0,
        }
    }
}

#[derive(Debug)]
pub struct SetMixerParam {
    pub timeline: TimelineId,
    pub channel: MixerChannel,
    pub param: MixerParam,
    pub value: f32,
    old: Option<f32>,
}

impl SetMixerParam {
    pub fn new(timeline: TimelineId, channel: MixerChannel, param: MixerParam, value: f32) -> Self {
        let range = param.range();
        Self {
            timeline,
            channel,
            param,
            value: value.clamp(*range.start(), *range.end()),
            old: None,
        }
    }
}

impl Command for SetMixerParam {
    fn label(&self) -> CommandLabel {
        match self.param {
            MixerParam::GainDb => CommandLabel::MixerGain,
            MixerParam::Pan => CommandLabel::MixerPan,
        }
    }

    fn apply(&mut self, project: &mut Project) {
        if let Some(strip) = channel_strip(project, self.timeline, self.channel) {
            let field = self.param.field(strip);
            self.old = Some(*field);
            *field = self.value;
        }
    }

    fn undo(&self, project: &mut Project) {
        if let (Some(old), Some(strip)) = (
            self.old,
            channel_strip(project, self.timeline, self.channel),
        ) {
            *self.param.field(strip) = old;
        }
    }
}

#[derive(Debug)]
pub struct AddAudioEffect {
    pub timeline: TimelineId,
    pub channel: MixerChannel,
    pub effect: AudioEffect,
    added_at: Option<usize>,
}

impl AddAudioEffect {
    /// Appended at the end of the chain.
    pub fn new(timeline: TimelineId, channel: MixerChannel, effect: AudioEffect) -> Self {
        Self {
            timeline,
            channel,
            effect,
            added_at: None,
        }
    }
}

impl Command for AddAudioEffect {
    fn label(&self) -> CommandLabel {
        CommandLabel::AddAudioEffect
    }

    fn apply(&mut self, project: &mut Project) {
        if let Some(strip) = channel_strip(project, self.timeline, self.channel) {
            strip.effects.push(self.effect.clone());
            self.added_at = Some(strip.effects.len() - 1);
        }
    }

    fn undo(&self, project: &mut Project) {
        if let (Some(index), Some(strip)) = (
            self.added_at,
            channel_strip(project, self.timeline, self.channel),
        ) && index < strip.effects.len()
        {
            strip.effects.remove(index);
        }
    }
}

#[derive(Debug)]
pub struct RemoveAudioEffect {
    pub timeline: TimelineId,
    pub channel: MixerChannel,
    pub index: usize,
    removed: Option<AudioEffect>,
}

impl RemoveAudioEffect {
    pub fn new(timeline: TimelineId, channel: MixerChannel, index: usize) -> Self {
        Self {
            timeline,
            channel,
            index,
            removed: None,
        }
    }
}

impl Command for RemoveAudioEffect {
    fn label(&self) -> CommandLabel {
        CommandLabel::RemoveAudioEffect
    }

    fn apply(&mut self, project: &mut Project) {
        self.removed = channel_strip(project, self.timeline, self.channel)
            .filter(|strip| self.index < strip.effects.len())
            .map(|strip| strip.effects.remove(self.index));
    }

    fn undo(&self, project: &mut Project) {
        if let (Some(effect), Some(strip)) = (
            self.removed.clone(),
            channel_strip(project, self.timeline, self.channel),
        ) {
            let index = self.index.min(strip.effects.len());
            strip.effects.insert(index, effect);
        }
    }
}

/// Moves an effect of the chain from `from` to `to` (its index afterwards).
#[derive(Debug)]
pub struct MoveAudioEffect {
    pub timeline: TimelineId,
    pub channel: MixerChannel,
    pub from: usize,
    pub to: usize,
}

impl MoveAudioEffect {
    pub fn new(timeline: TimelineId, channel: MixerChannel, from: usize, to: usize) -> Self {
        Self {
            timeline,
            channel,
            from,
            to,
        }
    }

    fn shift(&self, project: &mut Project, from: usize, to: usize) {
        if let Some(strip) = channel_strip(project, self.timeline, self.channel)
            && from < strip.effects.len()
            && to < strip.effects.len()
        {
            let effect = strip.effects.remove(from);
            strip.effects.insert(to, effect);
        }
    }
}

impl Command for MoveAudioEffect {
    fn label(&self) -> CommandLabel {
        CommandLabel::MoveAudioEffect
    }

    fn apply(&mut self, project: &mut Project) {
        self.shift(project, self.from, self.to);
    }

    fn undo(&self, project: &mut Project) {
        self.shift(project, self.to, self.from);
    }
}

/// Replaces an effect of the chain: its parameters, or whether it is on.
#[derive(Debug)]
pub struct SetAudioEffect {
    pub timeline: TimelineId,
    pub channel: MixerChannel,
    pub index: usize,
    pub effect: AudioEffect,
    label: CommandLabel,
    old: Option<AudioEffect>,
}

impl SetAudioEffect {
    pub fn new(
        timeline: TimelineId,
        channel: MixerChannel,
        index: usize,
        effect: AudioEffect,
        label: CommandLabel,
    ) -> Self {
        Self {
            timeline,
            channel,
            index,
            effect,
            label,
            old: None,
        }
    }
}

impl Command for SetAudioEffect {
    fn label(&self) -> CommandLabel {
        self.label
    }

    fn apply(&mut self, project: &mut Project) {
        if let Some(slot) = channel_strip(project, self.timeline, self.channel)
            .and_then(|strip| strip.effects.get_mut(self.index))
        {
            self.old = Some(std::mem::replace(slot, self.effect.clone()));
        }
    }

    fn undo(&self, project: &mut Project) {
        if let (Some(old), Some(slot)) = (
            self.old.clone(),
            channel_strip(project, self.timeline, self.channel)
                .and_then(|strip| strip.effects.get_mut(self.index)),
        ) {
            *slot = old;
        }
    }
}

/// Enables or disables (key D) a set of clips.
#[derive(Debug)]
pub struct SetClipsDisabled {
    pub timeline: TimelineId,
    pub clips: Vec<(usize, ClipId)>,
    pub disabled: bool,
    old: Vec<(usize, ClipId, bool)>,
}

impl SetClipsDisabled {
    pub fn new(timeline: TimelineId, clips: Vec<(usize, ClipId)>, disabled: bool) -> Self {
        Self {
            timeline,
            clips,
            disabled,
            old: Vec::new(),
        }
    }
}

impl Command for SetClipsDisabled {
    fn label(&self) -> CommandLabel {
        CommandLabel::ToggleClipsDisabled
    }

    fn apply(&mut self, project: &mut Project) {
        let tl = &mut project.timelines[self.timeline];
        self.old.clear();
        for &(track_index, clip_id) in &self.clips {
            if let Some(clip) = tl.clip_mut(track_index, clip_id) {
                self.old.push((track_index, clip_id, clip.disabled));
                clip.disabled = self.disabled;
            }
        }
    }

    fn undo(&self, project: &mut Project) {
        let tl = &mut project.timelines[self.timeline];
        for &(track_index, clip_id, old) in &self.old {
            if let Some(clip) = tl.clip_mut(track_index, clip_id) {
                clip.disabled = old;
            }
        }
    }
}

/// Timeline color of several clips at once; `None` goes back to the
/// color derived from the source kind.
#[derive(Debug)]
pub struct SetClipsDisplayColor {
    pub timeline: TimelineId,
    pub clips: Vec<(usize, ClipId)>,
    pub color: Option<ClipColor>,
    old: Vec<(usize, ClipId, Option<ClipColor>)>,
}

impl SetClipsDisplayColor {
    pub fn new(
        timeline: TimelineId,
        clips: Vec<(usize, ClipId)>,
        color: Option<ClipColor>,
    ) -> Self {
        Self {
            timeline,
            clips,
            color,
            old: Vec::new(),
        }
    }
}

impl Command for SetClipsDisplayColor {
    fn label(&self) -> CommandLabel {
        CommandLabel::ClipDisplayColor
    }

    fn apply(&mut self, project: &mut Project) {
        let tl = &mut project.timelines[self.timeline];
        self.old.clear();
        for &(track_index, clip_id) in &self.clips {
            if let Some(clip) = tl.clip_mut(track_index, clip_id) {
                self.old.push((track_index, clip_id, clip.display_color));
                clip.display_color = self.color;
            }
        }
    }

    fn undo(&self, project: &mut Project) {
        let tl = &mut project.timelines[self.timeline];
        for &(track_index, clip_id, old) in &self.old {
            if let Some(clip) = tl.clip_mut(track_index, clip_id) {
                clip.display_color = old;
            }
        }
    }
}

/// Inserts a clip into a track at a position. If it overlaps existing clips,
/// those underneath are moved right (insert, not overwrite).
#[derive(Debug)]
pub struct InsertClip {
    pub timeline: TimelineId,
    pub track_index: usize,
    pub clip: Clip,
}

impl Command for InsertClip {
    fn label(&self) -> CommandLabel {
        CommandLabel::InsertClips
    }

    fn apply(&mut self, project: &mut Project) {
        project.timelines[self.timeline].tracks[self.track_index].insert_sorted(self.clip.clone());
    }

    fn undo(&self, project: &mut Project) {
        let track = &mut project.timelines[self.timeline].tracks[self.track_index];
        track.clips.retain(|c| c.id != self.clip.id);
    }
}

/// Normal delete ("lift"): removes the clip, leaves a gap in its place.
#[derive(Debug)]
pub struct LiftDelete {
    pub timeline: TimelineId,
    pub track_index: usize,
    pub clip_id: ClipId,
    removed: Option<Clip>,
    /// Crossing transitions removed together with the clip (see the docs of
    /// `Track::crossings`), to be restored on undo.
    removed_crossings: Vec<CrossTransition>,
}

impl LiftDelete {
    pub fn new(timeline: TimelineId, track_index: usize, clip_id: ClipId) -> Self {
        Self {
            timeline,
            track_index,
            clip_id,
            removed: None,
            removed_crossings: Vec::new(),
        }
    }
}

impl Command for LiftDelete {
    fn label(&self) -> CommandLabel {
        CommandLabel::DeleteClips
    }

    fn apply(&mut self, project: &mut Project) {
        let track = &mut project.timelines[self.timeline].tracks[self.track_index];
        self.removed = track.remove_clip(self.clip_id);
        self.removed_crossings = if self.removed.is_some() {
            track.take_crossings_for(self.clip_id)
        } else {
            Vec::new()
        };
    }

    fn undo(&self, project: &mut Project) {
        let track = &mut project.timelines[self.timeline].tracks[self.track_index];
        if let Some(clip) = &self.removed {
            track.insert_sorted(clip.clone());
        }
        track
            .crossings
            .extend(self.removed_crossings.iter().cloned());
    }
}

fn resort(track: &mut Track) {
    track.clips.sort_by_key(|c| c.timeline_start);
}

/// Closes a gap (no clip to remove) on all the tracks,
/// shifting back by `gap_len` every clip starting at `gap_start` or
/// later — preserves the global A/V sync.
#[derive(Debug)]
pub struct RippleDeleteGap {
    pub timeline: TimelineId,
    pub gap_start: FrameIdx,
    pub gap_len: FrameIdx,
    shifted: Vec<(usize, ClipId, FrameIdx)>,
    markers_before: Vec<Marker>,
}

impl RippleDeleteGap {
    pub fn new(timeline: TimelineId, gap_start: FrameIdx, gap_len: FrameIdx) -> Self {
        Self {
            timeline,
            gap_start,
            gap_len,
            shifted: Vec::new(),
            markers_before: Vec::new(),
        }
    }
}

impl Command for RippleDeleteGap {
    fn label(&self) -> CommandLabel {
        CommandLabel::RippleDelete
    }

    fn apply(&mut self, project: &mut Project) {
        let tl = &mut project.timelines[self.timeline];
        let mut shifted = Vec::new();
        for (track_index, track) in tl.tracks.iter_mut().enumerate() {
            if track.locked {
                continue;
            }
            for c in &mut track.clips {
                if c.timeline_start >= self.gap_start {
                    shifted.push((track_index, c.id, c.timeline_start));
                    c.timeline_start -= self.gap_len;
                }
            }
            resort(track);
        }
        self.shifted = shifted;
        self.markers_before = tl.markers.clone();
        // A marker inside the closed gap lands on its start instead of
        // vanishing with the removed material.
        let gap_end = self.gap_start + self.gap_len;
        for marker in &mut tl.markers {
            if marker.start >= gap_end {
                marker.start -= self.gap_len;
            } else if marker.start >= self.gap_start {
                marker.start = self.gap_start;
            }
        }
        tl.markers.sort_by_key(|m| m.start);
    }

    fn undo(&self, project: &mut Project) {
        let tl = &mut project.timelines[self.timeline];
        for (track_index, clip_id, original_start) in &self.shifted {
            if let Some(c) = tl.clip_mut(*track_index, *clip_id) {
                c.timeline_start = *original_start;
            }
        }
        for track in &mut tl.tracks {
            resort(track);
        }
        tl.markers = self.markers_before.clone();
    }
}

/// Adds, replaces or removes (`marker: None`) the marker `id`.
#[derive(Debug)]
pub struct SetMarker {
    pub timeline: TimelineId,
    pub id: MarkerId,
    pub marker: Option<Marker>,
    label: CommandLabel,
    old: Option<Marker>,
}

impl SetMarker {
    pub fn add(timeline: TimelineId, marker: Marker) -> Self {
        Self::new(timeline, marker.id, Some(marker), CommandLabel::AddMarker)
    }

    pub fn edit(timeline: TimelineId, marker: Marker) -> Self {
        Self::new(timeline, marker.id, Some(marker), CommandLabel::EditMarker)
    }

    pub fn moved(timeline: TimelineId, marker: Marker) -> Self {
        Self::new(timeline, marker.id, Some(marker), CommandLabel::MoveMarker)
    }

    pub fn remove(timeline: TimelineId, id: MarkerId) -> Self {
        Self::new(timeline, id, None, CommandLabel::DeleteMarker)
    }

    fn new(
        timeline: TimelineId,
        id: MarkerId,
        marker: Option<Marker>,
        label: CommandLabel,
    ) -> Self {
        Self {
            timeline,
            id,
            marker,
            label,
            old: None,
        }
    }

    fn put(markers: &mut Vec<Marker>, id: MarkerId, marker: Option<&Marker>) -> Option<Marker> {
        let old = markers
            .iter()
            .position(|m| m.id == id)
            .map(|i| markers.remove(i));
        if let Some(marker) = marker {
            markers.push(marker.clone());
            markers.sort_by_key(|m| m.start);
        }
        old
    }
}

impl Command for SetMarker {
    fn label(&self) -> CommandLabel {
        self.label
    }

    fn apply(&mut self, project: &mut Project) {
        let markers = &mut project.timelines[self.timeline].markers;
        self.old = Self::put(markers, self.id, self.marker.as_ref());
    }

    fn undo(&self, project: &mut Project) {
        let markers = &mut project.timelines[self.timeline].markers;
        Self::put(markers, self.id, self.old.as_ref());
    }
}

/// What a speed change does to the length of the clip and to its neighbours.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpeedFit {
    /// The length follows the speed; whatever starts at or after the old end
    /// moves by the difference on all the unlocked tracks, as
    /// `RippleDeleteGap` does.
    Ripple,
    /// The clip keeps its length (capped at the end of the media) and shows
    /// more or less of the source.
    KeepLength,
    /// The length follows the speed and nothing else moves: the caller frees
    /// the stretch gained first (`make_room_for_ranges`), as for a trim.
    Resize,
    /// Like `Resize`, but the clips ending at `from` end exactly at `to`
    /// (a dragged edge), within the media: the length recomputed from the
    /// source range could miss it by the rounding.
    ResizeTo { from: FrameIdx, to: FrameIdx },
}

/// Constant speed of `Media` clips (the caller passes whole linked groups):
/// `source_in` and `timeline_start` stay put, see `SpeedFit` for the rest.
#[derive(Debug)]
pub struct SetClipSpeed {
    pub timeline: TimelineId,
    pub clips: Vec<(usize, ClipId)>,
    pub speed: Rational,
    pub pitch_correction: bool,
    pub fit: SpeedFit,
    before: Vec<Track>,
}

impl SetClipSpeed {
    pub fn new(
        timeline: TimelineId,
        clips: Vec<(usize, ClipId)>,
        speed: Rational,
        pitch_correction: bool,
        fit: SpeedFit,
    ) -> Self {
        Self {
            timeline,
            clips,
            speed,
            pitch_correction,
            fit,
            before: Vec::new(),
        }
    }
}

impl Command for SetClipSpeed {
    fn label(&self) -> CommandLabel {
        CommandLabel::ClipSpeed
    }

    fn apply(&mut self, project: &mut Project) {
        let media_pool = &project.media_pool;
        let tl = &mut project.timelines[self.timeline];
        self.before = tl.tracks.clone();
        let mut pending: Vec<(usize, ClipId)> = self
            .clips
            .iter()
            .copied()
            .filter(|&(track, id)| {
                tl.clip(track, id)
                    .is_some_and(|c| matches!(c.source, ClipSource::Media(_)))
            })
            .collect();
        // Leftmost first: every ripple moves the ends of the ones after it.
        while !pending.is_empty() {
            let end_of =
                |&(track, id): &(usize, ClipId)| tl.clip(track, id).unwrap().timeline_end();
            let old_end = pending.iter().map(end_of).min().unwrap();
            let (batch, rest): (Vec<_>, Vec<_>) =
                pending.into_iter().partition(|key| end_of(key) == old_end);
            pending = rest;
            let mut new_end = FrameIdx::MIN;
            for &(track, id) in &batch {
                let clip = tl.tracks[track].clip_mut(id).unwrap();
                let ClipSource::Media(media_id) = clip.source else {
                    continue;
                };
                let Some(media) = media_pool.get(media_id) else {
                    continue;
                };
                let (old_len, old_end) = (clip.timeline_len, clip.timeline_end());
                clip.pitch_correction = self.pitch_correction;
                clip.set_speed(self.speed, Rational::conform_rate(tl.fps, media.meta.fps));
                let wanted_len = match self.fit {
                    SpeedFit::KeepLength => Some(old_len),
                    SpeedFit::ResizeTo { from, to } if from == old_end => {
                        Some(to - clip.timeline_start)
                    }
                    _ => None,
                };
                if let Some(len) = wanted_len {
                    let available =
                        clip.rate().scale_round(media.meta.duration_frames) - clip.source_offset;
                    clip.timeline_len = len.min(available).max(1);
                    clip.fade_in = clip.fade_in.min(clip.timeline_len);
                    clip.fade_out = clip.fade_out.min(clip.timeline_len);
                }
                new_end = new_end.max(clip.timeline_end());
            }
            if self.fit == SpeedFit::Ripple && new_end != FrameIdx::MIN {
                ripple_from(&mut tl.tracks, old_end, new_end - old_end);
            }
        }
    }

    fn undo(&self, project: &mut Project) {
        project.timelines[self.timeline].tracks = self.before.clone();
    }
}

/// Moves by `delta` whatever starts at `from` or later, on the unlocked
/// tracks. Backwards it stops where a clip that stays (it starts before
/// `from`) would be overlapped.
fn ripple_from(tracks: &mut [Track], from: FrameIdx, delta: FrameIdx) {
    let moves = |c: &Clip| c.timeline_start >= from;
    let unlocked = || tracks.iter().filter(|t| !t.locked);
    let room = unlocked()
        .filter_map(|t| {
            let first_moved = t
                .clips
                .iter()
                .filter(|c| moves(c))
                .map(|c| c.timeline_start)
                .min()?;
            let staying_end = t
                .clips
                .iter()
                .filter(|c| !moves(c))
                .map(Clip::timeline_end)
                .max()
                .unwrap_or(0);
            Some(first_moved - staying_end)
        })
        .min()
        .unwrap_or(FrameIdx::MAX);
    let delta = delta.max(-room.max(0));
    if delta == 0 {
        return;
    }
    for track in tracks.iter_mut().filter(|t| !t.locked) {
        for clip in track.clips.iter_mut().filter(|c| moves(c)) {
            clip.timeline_start += delta;
        }
    }
}

/// Moves several clips in a single history step (e.g. a dragged group).
#[derive(Debug)]
pub struct MoveClips {
    pub timeline: TimelineId,
    /// (clip_id, from_track, to_track, new_start)
    pub moves: Vec<(ClipId, usize, usize, FrameIdx)>,
    old_starts: Vec<Option<FrameIdx>>,
    /// Crossings removed from `from_track` for every move changing track (see
    /// the docs of `Track::crossings`); empty for a move on the same track,
    /// where adjacency can break but the clip stays there and the crossing
    /// simply stays inert until it becomes adjacent again.
    removed_crossings: Vec<Vec<CrossTransition>>,
}

impl MoveClips {
    pub fn new(timeline: TimelineId, moves: Vec<(ClipId, usize, usize, FrameIdx)>) -> Self {
        Self {
            timeline,
            moves,
            old_starts: Vec::new(),
            removed_crossings: Vec::new(),
        }
    }
}

impl Command for MoveClips {
    fn label(&self) -> CommandLabel {
        CommandLabel::MoveClips
    }

    fn apply(&mut self, project: &mut Project) {
        self.old_starts.clear();
        self.removed_crossings.clear();
        for &(clip_id, from_track, to_track, new_start) in &self.moves {
            let tl = &mut project.timelines[self.timeline];
            let Some(mut clip) = tl.tracks[from_track].remove_clip(clip_id) else {
                self.old_starts.push(None);
                self.removed_crossings.push(Vec::new());
                continue;
            };
            self.old_starts.push(Some(clip.timeline_start));
            let removed = if from_track != to_track {
                tl.tracks[from_track].take_crossings_for(clip_id)
            } else {
                Vec::new()
            };
            self.removed_crossings.push(removed);
            clip.timeline_start = new_start;
            tl.tracks[to_track].insert_sorted(clip);
        }
    }

    fn undo(&self, project: &mut Project) {
        for ((&(clip_id, from_track, to_track, _new_start), old_start), removed) in self
            .moves
            .iter()
            .zip(&self.old_starts)
            .zip(&self.removed_crossings)
        {
            let Some(old_start) = old_start else {
                continue;
            };
            let tl = &mut project.timelines[self.timeline];
            let Some(mut clip) = tl.tracks[to_track].remove_clip(clip_id) else {
                continue;
            };
            clip.timeline_start = *old_start;
            tl.tracks[from_track].insert_sorted(clip);
            tl.tracks[from_track]
                .crossings
                .extend(removed.iter().cloned());
        }
    }
}

/// Which edge of a clip is trimmed: `Start` moves the beginning
/// together with the content (the end on the timeline stays put), `End` moves
/// only the end.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrimEdge {
    Start,
    End,
}

/// Trim of one edge of a clip. No clamping nor collision avoidance: the
/// caller computes `new_value`.
#[derive(Debug)]
pub struct TrimClip {
    pub timeline: TimelineId,
    pub track_index: usize,
    pub clip_id: ClipId,
    pub edge: TrimEdge,
    /// New timeline position of the edge.
    pub new_value: FrameIdx,
    /// Previous `(timeline_start, source_offset, timeline_len)`.
    old: Option<(FrameIdx, FrameIdx, FrameIdx)>,
    /// How far the keyframes moved to keep a generator's `source_offset` at 0.
    keyframe_shift: FrameIdx,
}

impl TrimClip {
    pub fn new(
        timeline: TimelineId,
        track_index: usize,
        clip_id: ClipId,
        edge: TrimEdge,
        new_value: FrameIdx,
    ) -> Self {
        Self {
            timeline,
            track_index,
            clip_id,
            edge,
            new_value,
            old: None,
            keyframe_shift: 0,
        }
    }
}

impl Command for TrimClip {
    fn label(&self) -> CommandLabel {
        CommandLabel::TrimClips
    }

    fn apply(&mut self, project: &mut Project) {
        let track = &mut project.timelines[self.timeline].tracks[self.track_index];
        let Some(clip) = track.clip_mut(self.clip_id) else {
            return;
        };
        self.old = Some((clip.timeline_start, clip.source_offset, clip.timeline_len));
        self.keyframe_shift = 0;
        match self.edge {
            TrimEdge::Start => {
                let delta = self.new_value - clip.timeline_start;
                clip.timeline_start = self.new_value;
                clip.source_offset += delta;
                clip.timeline_len -= delta;
                // A generator grows before its first frame by moving its
                // origin: the keyframes follow, so they stay on the same
                // content.
                if clip.is_generator() && clip.source_offset < 0 {
                    self.keyframe_shift = -clip.source_offset;
                    clip.source_offset = 0;
                    clip.effects.shift_keyframes(self.keyframe_shift);
                }
            }
            TrimEdge::End => {
                clip.timeline_len = self.new_value - clip.timeline_start;
            }
        }
        debug_assert!(
            clip.timeline_len >= 1 && clip.source_offset >= 0,
            "trim out of bounds"
        );
        resort(track);
    }

    fn undo(&self, project: &mut Project) {
        let Some((timeline_start, source_offset, timeline_len)) = self.old else {
            return;
        };
        let track = &mut project.timelines[self.timeline].tracks[self.track_index];
        if let Some(clip) = track.clip_mut(self.clip_id) {
            clip.timeline_start = timeline_start;
            clip.source_offset = source_offset;
            clip.timeline_len = timeline_len;
            clip.effects.shift_keyframes(-self.keyframe_shift);
        }
        resort(track);
    }
}

/// Slides the content under a clip by `delta` timeline frames, leaving its
/// place on the timeline untouched. No clamping: the caller keeps it inside
/// `edit::slip_range`.
#[derive(Debug)]
pub struct SlipClip {
    pub timeline: TimelineId,
    pub track_index: usize,
    pub clip_id: ClipId,
    pub delta: FrameIdx,
    applied: bool,
}

impl SlipClip {
    pub fn new(timeline: TimelineId, track_index: usize, clip_id: ClipId, delta: FrameIdx) -> Self {
        Self {
            timeline,
            track_index,
            clip_id,
            delta,
            applied: false,
        }
    }
}

impl Command for SlipClip {
    fn label(&self) -> CommandLabel {
        CommandLabel::SlipClips
    }

    fn apply(&mut self, project: &mut Project) {
        let track = &mut project.timelines[self.timeline].tracks[self.track_index];
        self.applied = false;
        if let Some(clip) = track.clip_mut(self.clip_id) {
            clip.source_offset += self.delta;
            debug_assert!(clip.source_offset >= 0, "slip out of bounds");
            self.applied = true;
        }
    }

    fn undo(&self, project: &mut Project) {
        if !self.applied {
            return;
        }
        let track = &mut project.timelines[self.timeline].tracks[self.track_index];
        if let Some(clip) = track.clip_mut(self.clip_id) {
            clip.source_offset -= self.delta;
        }
    }
}

/// Which fade a `SetClipFade` concerns.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FadeEdge {
    In,
    Out,
}

/// Sets the duration (in timeline frames) of the fade in or
/// out of a clip, dragged from the handle on the timeline.
#[derive(Debug)]
pub struct SetClipFade {
    pub timeline: TimelineId,
    pub track_index: usize,
    pub clip_id: ClipId,
    pub edge: FadeEdge,
    pub new_value: FrameIdx,
    old: Option<FrameIdx>,
}

impl SetClipFade {
    pub fn new(
        timeline: TimelineId,
        track_index: usize,
        clip_id: ClipId,
        edge: FadeEdge,
        new_value: FrameIdx,
    ) -> Self {
        Self {
            timeline,
            track_index,
            clip_id,
            edge,
            new_value,
            old: None,
        }
    }

    fn field(edge: FadeEdge, clip: &mut Clip) -> &mut FrameIdx {
        match edge {
            FadeEdge::In => &mut clip.fade_in,
            FadeEdge::Out => &mut clip.fade_out,
        }
    }
}

impl Command for SetClipFade {
    fn label(&self) -> CommandLabel {
        CommandLabel::Fade
    }

    fn apply(&mut self, project: &mut Project) {
        let track = &mut project.timelines[self.timeline].tracks[self.track_index];
        let Some(clip) = track.clip_mut(self.clip_id) else {
            return;
        };
        let clamped = self.new_value.clamp(0, clip.timeline_len);
        let field = Self::field(self.edge, clip);
        self.old = Some(*field);
        *field = clamped;
    }

    fn undo(&self, project: &mut Project) {
        let Some(old) = self.old else {
            return;
        };
        let track = &mut project.timelines[self.timeline].tracks[self.track_index];
        if let Some(clip) = track.clip_mut(self.clip_id) {
            *Self::field(self.edge, clip) = old;
        }
    }
}

/// Dissolves the whole linked group of the clip, not just the clip itself.
#[derive(Debug)]
pub struct UnlinkClip {
    pub timeline: TimelineId,
    pub track_index: usize,
    pub clip_id: ClipId,
    /// Dissolved group and its members, for the undo.
    dissolved: Option<(LinkGroupId, Vec<(usize, ClipId)>)>,
}

impl UnlinkClip {
    pub fn new(timeline: TimelineId, track_index: usize, clip_id: ClipId) -> Self {
        Self {
            timeline,
            track_index,
            clip_id,
            dissolved: None,
        }
    }
}

impl Command for UnlinkClip {
    fn label(&self) -> CommandLabel {
        CommandLabel::UnlinkClips
    }

    fn apply(&mut self, project: &mut Project) {
        let tl = &mut project.timelines[self.timeline];
        let Some(group) = tl
            .clip(self.track_index, self.clip_id)
            .and_then(|c| c.linked_group)
        else {
            return;
        };
        let members = tl.clips_in_group(group);
        for &(track_index, clip_id) in &members {
            if let Some(c) = tl.clip_mut(track_index, clip_id) {
                c.linked_group = None;
            }
        }
        self.dissolved = Some((group, members));
    }

    fn undo(&self, project: &mut Project) {
        let Some((group, members)) = &self.dissolved else {
            return;
        };
        let tl = &mut project.timelines[self.timeline];
        for &(track_index, clip_id) in members {
            if let Some(c) = tl.clip_mut(track_index, clip_id) {
                c.linked_group = Some(*group);
            }
        }
    }
}

/// Links the clips into a new group, overwriting the previous ones.
/// No-op with fewer than 2 clips.
#[derive(Debug)]
pub struct LinkClips {
    pub timeline: TimelineId,
    pub targets: Vec<(usize, ClipId)>,
    /// Allocated the first time `apply` runs, then reused unchanged on the
    /// later redos — never a new id on every redo.
    group_id: Option<LinkGroupId>,
    previous: Option<Vec<Option<LinkGroupId>>>,
}

impl LinkClips {
    pub fn new(timeline: TimelineId, targets: Vec<(usize, ClipId)>) -> Self {
        Self {
            timeline,
            targets,
            group_id: None,
            previous: None,
        }
    }
}

impl Command for LinkClips {
    fn label(&self) -> CommandLabel {
        CommandLabel::LinkClips
    }

    fn apply(&mut self, project: &mut Project) {
        if self.targets.len() < 2 {
            return;
        }
        let group_id = self
            .group_id
            .unwrap_or_else(|| project.alloc_link_group_id());
        self.group_id = Some(group_id);

        let tl = &project.timelines[self.timeline];
        let previous: Vec<Option<LinkGroupId>> = self
            .targets
            .iter()
            .map(|(track_index, clip_id)| {
                tl.clip(*track_index, *clip_id).and_then(|c| c.linked_group)
            })
            .collect();
        self.previous = Some(previous);

        let tl = &mut project.timelines[self.timeline];
        for (track_index, clip_id) in &self.targets {
            if let Some(c) = tl.clip_mut(*track_index, *clip_id) {
                c.linked_group = Some(group_id);
            }
        }
    }

    fn undo(&self, project: &mut Project) {
        let Some(previous) = &self.previous else {
            return;
        };
        let tl = &mut project.timelines[self.timeline];
        for ((track_index, clip_id), old) in self.targets.iter().zip(previous.iter()) {
            if let Some(c) = tl.clip_mut(*track_index, *clip_id) {
                c.linked_group = *old;
            }
        }
    }
}

/// Splits a clip at `split_at`; the right half gets a new id. On a conformed
/// clip the two halves can share the source frame straddling
/// the cut.
#[derive(Debug)]
pub struct SplitClip {
    pub timeline: TimelineId,
    pub track_index: usize,
    pub clip_id: ClipId,
    pub split_at: FrameIdx,
    original_len: Option<FrameIdx>,
    original_effects: Option<EffectStack>,
    new_clip_id: Option<ClipId>,
    /// Id of the right half decided by the caller, to relink the halves of
    /// several splits with a later command.
    preallocated_new_clip_id: Option<ClipId>,
    /// Crossing transitions removed from `clip_id` during the split (neither
    /// of the two halves remains the geometric counterpart expected by
    /// the transition), to be restored on undo.
    removed_crossings: Vec<CrossTransition>,
}

impl SplitClip {
    pub fn new(
        timeline: TimelineId,
        track_index: usize,
        clip_id: ClipId,
        split_at: FrameIdx,
    ) -> Self {
        Self {
            timeline,
            track_index,
            clip_id,
            split_at,
            original_len: None,
            original_effects: None,
            new_clip_id: None,
            preallocated_new_clip_id: None,
            removed_crossings: Vec::new(),
        }
    }

    pub fn with_new_clip_id(mut self, id: ClipId) -> Self {
        self.preallocated_new_clip_id = Some(id);
        self
    }

    /// The id assigned to the right half, known only after `apply`.
    pub fn new_clip_id(&self) -> Option<ClipId> {
        self.new_clip_id
    }
}

impl Command for SplitClip {
    fn label(&self) -> CommandLabel {
        CommandLabel::SplitClips
    }

    fn apply(&mut self, project: &mut Project) {
        let new_id = self
            .preallocated_new_clip_id
            .unwrap_or_else(|| project.alloc_clip_id());
        let track = &mut project.timelines[self.timeline].tracks[self.track_index];
        let Some(clip) = track.clip_mut(self.clip_id) else {
            return;
        };
        if self.split_at <= clip.timeline_start || self.split_at >= clip.timeline_end() {
            return; // outside the body of the clip: nothing to split
        }

        self.original_len = Some(clip.timeline_len);
        let mut second_half = clip.clone();
        let left_len = self.split_at - clip.timeline_start;
        clip.timeline_len = left_len;
        // The left half is the same clip and stays in its group; the right one is
        // new and starts unlinked.
        second_half.id = new_id;
        second_half.timeline_start = self.split_at;
        second_half.source_offset += left_len;
        second_half.timeline_len -= left_len;
        second_half.linked_group = None;
        self.new_clip_id = Some(new_id);

        // Each half keeps only the keyframes on its own side of the cut:
        // otherwise the right one would interpolate from the left one's keyframes.
        self.original_effects = Some(clip.effects.clone());
        clip.effects.drop_keyframes_from(clip.source_out());
        second_half
            .effects
            .drop_keyframes_before(second_half.source_in());

        track.insert_sorted(second_half);

        // A crossing on `clip_id` pointed at an edge that now belongs to
        // one of the two halves but no longer to the whole clip: instead of leaving it
        // dangling (see the docs of `Track::crossings`) it is removed here, valid
        // for any kind of crossing transition present or future.
        self.removed_crossings = track.take_crossings_for(self.clip_id);
    }

    fn undo(&self, project: &mut Project) {
        let (Some(original_len), Some(new_clip_id)) = (self.original_len, self.new_clip_id) else {
            return;
        };
        let track = &mut project.timelines[self.timeline].tracks[self.track_index];
        track.clips.retain(|c| c.id != new_clip_id);
        if let Some(clip) = track.clip_mut(self.clip_id) {
            clip.timeline_len = original_len;
            if let Some(effects) = &self.original_effects {
                clip.effects = effects.clone();
            }
        }
        track
            .crossings
            .extend(self.removed_crossings.iter().cloned());
    }
}

/// Replaces a value of a clip (chosen by `access`) remembering the
/// previous one for the undo.
pub struct SetClipValue<T> {
    pub timeline: TimelineId,
    pub track_index: usize,
    pub clip_id: ClipId,
    value: T,
    access: Box<dyn Fn(&mut Clip) -> &mut T + Send>,
    old: Option<T>,
    label: CommandLabel,
}

impl<T> SetClipValue<T> {
    pub fn new(
        timeline: TimelineId,
        track_index: usize,
        clip_id: ClipId,
        label: CommandLabel,
        value: T,
        access: impl Fn(&mut Clip) -> &mut T + Send + 'static,
    ) -> Self {
        Self {
            timeline,
            track_index,
            clip_id,
            value,
            access: Box::new(access),
            old: None,
            label,
        }
    }
}

impl<T> std::fmt::Debug for SetClipValue<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SetClipValue")
            .field("track_index", &self.track_index)
            .field("clip_id", &self.clip_id)
            .finish_non_exhaustive()
    }
}

impl<T: Clone + Send + 'static> Command for SetClipValue<T> {
    fn label(&self) -> CommandLabel {
        self.label
    }

    fn apply(&mut self, project: &mut Project) {
        if let Some(clip) =
            project.timelines[self.timeline].clip_mut(self.track_index, self.clip_id)
        {
            self.old = Some(std::mem::replace((self.access)(clip), self.value.clone()));
        }
    }

    fn undo(&self, project: &mut Project) {
        if let (Some(old), Some(clip)) = (
            &self.old,
            project.timelines[self.timeline].clip_mut(self.track_index, self.clip_id),
        ) {
            *(self.access)(clip) = old.clone();
        }
    }
}

/// Replaces the whole attribute block of a clip (effects and fades):
/// the "paste attributes" of the NLEs, whose per-attribute merge the
/// caller has already done.
#[derive(Debug)]
pub struct SetClipAttributes {
    pub timeline: TimelineId,
    pub track_index: usize,
    pub clip_id: ClipId,
    value: ClipAttributes,
    old: Option<ClipAttributes>,
}

impl SetClipAttributes {
    pub fn new(
        timeline: TimelineId,
        track_index: usize,
        clip_id: ClipId,
        value: ClipAttributes,
    ) -> Self {
        Self {
            timeline,
            track_index,
            clip_id,
            value,
            old: None,
        }
    }
}

impl Command for SetClipAttributes {
    fn label(&self) -> CommandLabel {
        CommandLabel::PasteAttributes
    }

    fn apply(&mut self, project: &mut Project) {
        if let Some(clip) =
            project.timelines[self.timeline].clip_mut(self.track_index, self.clip_id)
        {
            self.old = Some(ClipAttributes::of(clip));
            self.value.clone().apply_to(clip);
        }
    }

    fn undo(&self, project: &mut Project) {
        if let (Some(old), Some(clip)) = (
            self.old.clone(),
            project.timelines[self.timeline].clip_mut(self.track_index, self.clip_id),
        ) {
            old.apply_to(clip);
        }
    }
}

/// Static value (the `default`) of a transform parameter.
pub fn set_clip_transform_param(
    timeline: TimelineId,
    track_index: usize,
    clip_id: ClipId,
    param: TransformParam,
    value: f32,
) -> SetClipValue<f32> {
    SetClipValue::new(
        timeline,
        track_index,
        clip_id,
        CommandLabel::Transform,
        value,
        move |c| &mut c.effects.transform.track_mut(param).default,
    )
}

pub fn set_clip_flip(
    timeline: TimelineId,
    track_index: usize,
    clip_id: ClipId,
    value: [bool; 2],
) -> SetClipValue<[bool; 2]> {
    SetClipValue::new(
        timeline,
        track_index,
        clip_id,
        CommandLabel::Flip,
        value,
        |c| &mut c.effects.transform.flip,
    )
}

/// Static gain (dB).
pub fn set_clip_gain(
    timeline: TimelineId,
    track_index: usize,
    clip_id: ClipId,
    value: f32,
) -> SetClipValue<f32> {
    SetClipValue::new(
        timeline,
        track_index,
        clip_id,
        CommandLabel::Gain,
        value,
        |c| &mut c.effects.gain_db.default,
    )
}

/// Gain at 0 dB, keyframes included.
pub fn reset_clip_gain(
    timeline: TimelineId,
    track_index: usize,
    clip_id: ClipId,
) -> SetClipValue<Keyframed<f32>> {
    SetClipValue::new(
        timeline,
        track_index,
        clip_id,
        CommandLabel::ResetGain,
        Keyframed::constant(0.0),
        |c| &mut c.effects.gain_db,
    )
}

pub fn set_clip_title(
    timeline: TimelineId,
    track_index: usize,
    clip_id: ClipId,
    value: TitleParams,
) -> SetClipValue<Option<TitleParams>> {
    SetClipValue::new(
        timeline,
        track_index,
        clip_id,
        CommandLabel::Title,
        Some(value),
        |c| &mut c.effects.title,
    )
}

/// Replaces the whole filter list, order included: adding one,
/// removing it, enabling/disabling it or reordering them are all "write the
/// new list" — the caller computes it from the current one.
pub fn set_clip_filters(
    timeline: TimelineId,
    track_index: usize,
    clip_id: ClipId,
    value: Vec<ClipFilter>,
) -> SetClipValue<Vec<ClipFilter>> {
    SetClipValue::new(
        timeline,
        track_index,
        clip_id,
        CommandLabel::Filters,
        value,
        |c| &mut c.effects.filters,
    )
}

/// Replaces the whole mask list, like `set_clip_filters`.
pub fn set_clip_masks(
    timeline: TimelineId,
    track_index: usize,
    clip_id: ClipId,
    value: Vec<ClipMask>,
) -> SetClipValue<Vec<ClipMask>> {
    SetClipValue::new(
        timeline,
        track_index,
        clip_id,
        CommandLabel::Masks,
        value,
        |c| &mut c.effects.masks,
    )
}

/// Compositing method of the clip's layer.
pub fn set_clip_blend_mode(
    timeline: TimelineId,
    track_index: usize,
    clip_id: ClipId,
    value: crate::model::BlendMode,
) -> SetClipValue<crate::model::BlendMode> {
    SetClipValue::new(
        timeline,
        track_index,
        clip_id,
        CommandLabel::BlendMode,
        value,
        |c| &mut c.effects.blend_mode,
    )
}

/// Sets (or removes, with `None`) the transition of one edge of the clip.
pub fn set_clip_transition(
    timeline: TimelineId,
    track_index: usize,
    clip_id: ClipId,
    edge: FadeEdge,
    value: Option<Transition>,
) -> SetClipValue<Option<Transition>> {
    SetClipValue::new(
        timeline,
        track_index,
        clip_id,
        CommandLabel::Transition,
        value,
        move |c| match edge {
            FadeEdge::In => &mut c.effects.transition_in,
            FadeEdge::Out => &mut c.effects.transition_out,
        },
    )
}

/// Adds, replaces or removes (`value: None`) the crossing transition whose
/// `left_clip` is `left_clip`: a clip has at most one transition on its
/// right edge, so that identifies it on its own, without needing the id of
/// `right_clip`.
#[derive(Debug)]
pub struct SetCrossTransition {
    pub timeline: TimelineId,
    pub track_index: usize,
    pub left_clip: ClipId,
    pub value: Option<CrossTransition>,
    /// `None` until applied; then the previous value (which in turn
    /// can be `None` if there was no crossing there).
    old: Option<Option<CrossTransition>>,
}

impl SetCrossTransition {
    pub fn new(
        timeline: TimelineId,
        track_index: usize,
        left_clip: ClipId,
        value: Option<CrossTransition>,
    ) -> Self {
        Self {
            timeline,
            track_index,
            left_clip,
            value,
            old: None,
        }
    }

    fn write(
        track: &mut Track,
        left_clip: ClipId,
        value: &Option<CrossTransition>,
    ) -> Option<CrossTransition> {
        let pos = track
            .crossings
            .iter()
            .position(|c| c.left_clip == left_clip);
        let old = pos.map(|i| track.crossings[i].clone());
        match (pos, value) {
            (Some(i), Some(new)) => track.crossings[i] = new.clone(),
            (Some(i), None) => {
                track.crossings.remove(i);
            }
            (None, Some(new)) => track.crossings.push(new.clone()),
            (None, None) => {}
        }
        old
    }
}

impl Command for SetCrossTransition {
    fn label(&self) -> CommandLabel {
        CommandLabel::Transition
    }

    fn apply(&mut self, project: &mut Project) {
        let track = &mut project.timelines[self.timeline].tracks[self.track_index];
        self.old = Some(Self::write(track, self.left_clip, &self.value));
    }

    fn undo(&self, project: &mut Project) {
        let Some(old) = &self.old else {
            return;
        };
        let track = &mut project.timelines[self.timeline].tracks[self.track_index];
        Self::write(track, self.left_clip, old);
    }
}

/// Brings a group of transform parameters back to their default value,
/// keyframes included: it is the reset of a section of the properties panel
/// (Transform or Cropping), not of a single parameter.
#[derive(Debug)]
pub struct ResetTransformParams {
    pub timeline: TimelineId,
    pub track_index: usize,
    pub clip_id: ClipId,
    pub params: Vec<TransformParam>,
    /// `true` for the Transform group, which includes the flip too.
    pub reset_flip: bool,
    previous: Option<(Vec<Keyframed<f32>>, [bool; 2])>,
}

impl ResetTransformParams {
    pub fn new(
        timeline: TimelineId,
        track_index: usize,
        clip_id: ClipId,
        params: Vec<TransformParam>,
        reset_flip: bool,
    ) -> Self {
        Self {
            timeline,
            track_index,
            clip_id,
            params,
            reset_flip,
            previous: None,
        }
    }
}

impl Command for ResetTransformParams {
    fn label(&self) -> CommandLabel {
        CommandLabel::ResetTransform
    }

    fn apply(&mut self, project: &mut Project) {
        let Some(clip) = project.timelines[self.timeline].clip_mut(self.track_index, self.clip_id)
        else {
            return;
        };
        let defaults = Transform::default();
        let previous = self
            .params
            .iter()
            .map(|p| clip.effects.transform.track(*p).clone())
            .collect();
        self.previous = Some((previous, clip.effects.transform.flip));
        for param in &self.params {
            *clip.effects.transform.track_mut(*param) = Keyframed::constant(param.of(&defaults));
        }
        if self.reset_flip {
            clip.effects.transform.flip = defaults.flip;
        }
    }

    fn undo(&self, project: &mut Project) {
        let Some((previous, flip)) = &self.previous else {
            return;
        };
        let Some(clip) = project.timelines[self.timeline].clip_mut(self.track_index, self.clip_id)
        else {
            return;
        };
        for (param, track) in self.params.iter().zip(previous) {
            *clip.effects.transform.track_mut(*param) = track.clone();
        }
        if self.reset_flip {
            clip.effects.transform.flip = *flip;
        }
    }
}

/// Static color of a SolidColor clip; the first one initializes it. The
/// keyframe commands assume a color is already present.
#[derive(Debug)]
pub struct SetClipColor {
    pub timeline: TimelineId,
    pub track_index: usize,
    pub clip_id: ClipId,
    pub new_value: Rgba,
    old_value: Option<Option<Rgba>>,
}

impl SetClipColor {
    pub fn new(timeline: TimelineId, track_index: usize, clip_id: ClipId, new_value: Rgba) -> Self {
        Self {
            timeline,
            track_index,
            clip_id,
            new_value,
            old_value: None,
        }
    }
}

impl Command for SetClipColor {
    fn label(&self) -> CommandLabel {
        CommandLabel::ClipColor
    }

    fn apply(&mut self, project: &mut Project) {
        let Some(clip) = project.timelines[self.timeline].clip_mut(self.track_index, self.clip_id)
        else {
            return;
        };
        self.old_value = Some(clip.effects.color.as_ref().map(|k| k.default));
        match &mut clip.effects.color {
            Some(k) => k.default = self.new_value,
            None => clip.effects.color = Some(Keyframed::constant(self.new_value)),
        }
    }

    fn undo(&self, project: &mut Project) {
        let Some(old) = self.old_value else {
            return;
        };
        let Some(clip) = project.timelines[self.timeline].clip_mut(self.track_index, self.clip_id)
        else {
            return;
        };
        match old {
            Some(v) => {
                if let Some(k) = &mut clip.effects.color {
                    k.default = v;
                }
            }
            None => clip.effects.color = None,
        }
    }
}

/// Animatable parameter of `UpsertKeyframe`/`RemoveKeyframe`.
#[derive(Debug, Clone, Copy)]
pub enum KeyframeValue {
    TransformParam(TransformParam, f32),
    Gain(f32),
    Color(Rgba),
    /// Of the clip's filter of that kind (there is at most one per kind).
    FilterRadius(FilterKind, f32),
    FilterDirection(FilterKind, BlurDirection),
    FilterAmount(FilterKind, f32),
    /// Of the clip's color correction.
    Grade(GradeParam, f32),
    /// Of the clip's mask at that index.
    Mask(usize, MaskParam, f32),
}

/// Inserts or replaces a keyframe of an animatable parameter.
#[derive(Debug)]
pub struct UpsertKeyframe {
    pub timeline: TimelineId,
    pub track_index: usize,
    pub clip_id: ClipId,
    pub frame: FrameIdx,
    pub value: KeyframeValue,
    pub interpolation: Interpolation,
    /// Value/interpolation that was there before at this same frame, if
    /// there was one: `None` means the frame had no keyframe, so
    /// the undo must remove it instead of restoring an old one.
    previous: Option<(KeyframeValue, Interpolation)>,
}

impl UpsertKeyframe {
    pub fn new(
        timeline: TimelineId,
        track_index: usize,
        clip_id: ClipId,
        frame: FrameIdx,
        value: KeyframeValue,
        interpolation: Interpolation,
    ) -> Self {
        Self {
            timeline,
            track_index,
            clip_id,
            frame,
            value,
            interpolation,
            previous: None,
        }
    }
}

impl Command for UpsertKeyframe {
    fn label(&self) -> CommandLabel {
        CommandLabel::SetKeyframe
    }

    fn apply(&mut self, project: &mut Project) {
        let Some(clip) = project.timelines[self.timeline].clip_mut(self.track_index, self.clip_id)
        else {
            return;
        };
        let target = self.value.target();
        // Without a color initialized nothing is put: see the docs of SetClipColor.
        self.previous = take_keyframe(&mut clip.effects, target, self.frame);
        put_keyframe(
            &mut clip.effects,
            self.value,
            self.frame,
            self.interpolation,
        );
    }

    fn undo(&self, project: &mut Project) {
        let Some(clip) = project.timelines[self.timeline].clip_mut(self.track_index, self.clip_id)
        else {
            return;
        };
        take_keyframe(&mut clip.effects, self.value.target(), self.frame);
        if let Some((value, interpolation)) = self.previous {
            put_keyframe(&mut clip.effects, value, self.frame, interpolation);
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum KeyframeTarget {
    TransformParam(TransformParam),
    Gain,
    Color,
    FilterRadius(FilterKind),
    FilterDirection(FilterKind),
    FilterAmount(FilterKind),
    Grade(GradeParam),
    Mask(usize, MaskParam),
}

impl KeyframeValue {
    pub fn target(self) -> KeyframeTarget {
        match self {
            Self::TransformParam(param, _) => KeyframeTarget::TransformParam(param),
            Self::Gain(_) => KeyframeTarget::Gain,
            Self::Color(_) => KeyframeTarget::Color,
            Self::FilterRadius(kind, _) => KeyframeTarget::FilterRadius(kind),
            Self::FilterDirection(kind, _) => KeyframeTarget::FilterDirection(kind),
            Self::FilterAmount(kind, _) => KeyframeTarget::FilterAmount(kind),
            Self::Grade(param, _) => KeyframeTarget::Grade(param),
            Self::Mask(index, param, _) => KeyframeTarget::Mask(index, param),
        }
    }
}

fn filter_mut(effects: &mut EffectStack, kind: FilterKind) -> Option<&mut ClipFilter> {
    effects.filters.iter_mut().find(|f| f.kind == kind)
}

fn grade_track_mut(effects: &mut EffectStack, param: GradeParam) -> Option<&mut Keyframed<f32>> {
    filter_mut(effects, FilterKind::ColorCorrection).map(|f| f.grade.track_mut(param))
}

fn mask_track_mut(
    effects: &mut EffectStack,
    index: usize,
    param: MaskParam,
) -> Option<&mut Keyframed<f32>> {
    effects.masks.get_mut(index).map(|m| m.track_mut(param))
}

/// A keyframe named by parameter and frame: the currency with which the keyframe
/// editor passes a selection to the commands.
pub type KeyframePick = (KeyframeTarget, FrameIdx);

/// Removes from the parameter the keyframe at `frame`, returning it.
fn take_keyframe(
    effects: &mut EffectStack,
    target: KeyframeTarget,
    frame: FrameIdx,
) -> Option<(KeyframeValue, Interpolation)> {
    match target {
        KeyframeTarget::TransformParam(param) => effects
            .transform
            .track_mut(param)
            .remove_at(frame)
            .map(|(v, i)| (KeyframeValue::TransformParam(param, v), i)),
        KeyframeTarget::Gain => effects
            .gain_db
            .remove_at(frame)
            .map(|(v, i)| (KeyframeValue::Gain(v), i)),
        KeyframeTarget::Color => effects
            .color
            .as_mut()
            .and_then(|c| c.remove_at(frame))
            .map(|(v, i)| (KeyframeValue::Color(v), i)),
        KeyframeTarget::FilterRadius(kind) => filter_mut(effects, kind)
            .and_then(|f| f.radius.remove_at(frame))
            .map(|(v, i)| (KeyframeValue::FilterRadius(kind, v), i)),
        KeyframeTarget::FilterDirection(kind) => filter_mut(effects, kind)
            .and_then(|f| f.direction.remove_at(frame))
            .map(|(v, i)| (KeyframeValue::FilterDirection(kind, v), i)),
        KeyframeTarget::FilterAmount(kind) => filter_mut(effects, kind)
            .and_then(|f| f.amount.remove_at(frame))
            .map(|(v, i)| (KeyframeValue::FilterAmount(kind, v), i)),
        KeyframeTarget::Grade(param) => grade_track_mut(effects, param)
            .and_then(|k| k.remove_at(frame))
            .map(|(v, i)| (KeyframeValue::Grade(param, v), i)),
        KeyframeTarget::Mask(index, param) => mask_track_mut(effects, index, param)
            .and_then(|k| k.remove_at(frame))
            .map(|(v, i)| (KeyframeValue::Mask(index, param, v), i)),
    }
}

fn put_keyframe(
    effects: &mut EffectStack,
    value: KeyframeValue,
    frame: FrameIdx,
    interpolation: Interpolation,
) {
    match value {
        KeyframeValue::TransformParam(param, v) => {
            effects
                .transform
                .track_mut(param)
                .upsert(frame, v, interpolation)
        }
        KeyframeValue::Gain(v) => effects.gain_db.upsert(frame, v, interpolation),
        KeyframeValue::Color(v) => {
            if let Some(color) = &mut effects.color {
                color.upsert(frame, v, interpolation);
            }
        }
        KeyframeValue::FilterRadius(kind, v) => {
            if let Some(filter) = filter_mut(effects, kind) {
                filter.radius.upsert(frame, v, interpolation);
            }
        }
        KeyframeValue::FilterDirection(kind, v) => {
            if let Some(filter) = filter_mut(effects, kind) {
                filter.direction.upsert(frame, v, interpolation);
            }
        }
        KeyframeValue::FilterAmount(kind, v) => {
            if let Some(filter) = filter_mut(effects, kind) {
                filter.amount.upsert(frame, v, interpolation);
            }
        }
        KeyframeValue::Grade(param, v) => {
            if let Some(track) = grade_track_mut(effects, param) {
                track.upsert(frame, v, interpolation);
            }
        }
        KeyframeValue::Mask(index, param, v) => {
            if let Some(track) = mask_track_mut(effects, index, param) {
                track.upsert(frame, v, interpolation);
            }
        }
    }
}

fn set_keyframe_interpolation(
    effects: &mut EffectStack,
    target: KeyframeTarget,
    frame: FrameIdx,
    interpolation: Interpolation,
) -> Option<Interpolation> {
    match target {
        KeyframeTarget::TransformParam(param) => effects
            .transform
            .track_mut(param)
            .set_interpolation(frame, interpolation),
        KeyframeTarget::Gain => effects.gain_db.set_interpolation(frame, interpolation),
        KeyframeTarget::Color => effects
            .color
            .as_mut()
            .and_then(|c| c.set_interpolation(frame, interpolation)),
        KeyframeTarget::FilterRadius(kind) => {
            filter_mut(effects, kind).and_then(|f| f.radius.set_interpolation(frame, interpolation))
        }
        KeyframeTarget::FilterDirection(kind) => filter_mut(effects, kind)
            .and_then(|f| f.direction.set_interpolation(frame, interpolation)),
        KeyframeTarget::FilterAmount(kind) => {
            filter_mut(effects, kind).and_then(|f| f.amount.set_interpolation(frame, interpolation))
        }
        KeyframeTarget::Grade(param) => {
            grade_track_mut(effects, param).and_then(|k| k.set_interpolation(frame, interpolation))
        }
        KeyframeTarget::Mask(index, param) => mask_track_mut(effects, index, param)
            .and_then(|k| k.set_interpolation(frame, interpolation)),
    }
}

/// Moves a group of keyframes in time (even of different parameters)
/// all by the same `delta`.
#[derive(Debug)]
pub struct MoveKeyframes {
    pub timeline: TimelineId,
    pub track_index: usize,
    pub clip_id: ClipId,
    pub picks: Vec<KeyframePick>,
    pub delta: FrameIdx,
    /// The keyframes actually moved, at their starting position.
    moved: Vec<(KeyframeTarget, FrameIdx, KeyframeValue, Interpolation)>,
    /// Keyframes landed under the moved ones: the undo puts them back.
    overwritten: Vec<(FrameIdx, KeyframeValue, Interpolation)>,
}

impl MoveKeyframes {
    pub fn new(
        timeline: TimelineId,
        track_index: usize,
        clip_id: ClipId,
        picks: Vec<KeyframePick>,
        delta: FrameIdx,
    ) -> Self {
        Self {
            timeline,
            track_index,
            clip_id,
            picks,
            delta,
            moved: Vec::new(),
            overwritten: Vec::new(),
        }
    }
}

impl Command for MoveKeyframes {
    fn label(&self) -> CommandLabel {
        CommandLabel::MoveKeyframes
    }

    fn apply(&mut self, project: &mut Project) {
        let Some(clip) = project.timelines[self.timeline].clip_mut(self.track_index, self.clip_id)
        else {
            return;
        };
        // First they are all removed, then put back down: otherwise a
        // move of one frame would overwrite the neighbour that is about to
        // move in turn.
        let moved: Vec<_> = self
            .picks
            .iter()
            .filter_map(|&(target, frame)| {
                take_keyframe(&mut clip.effects, target, frame).map(|(v, i)| (target, frame, v, i))
            })
            .collect();
        let mut overwritten = Vec::new();
        for &(target, frame, value, interp) in &moved {
            let destination = frame + self.delta;
            if let Some((v, i)) = take_keyframe(&mut clip.effects, target, destination) {
                overwritten.push((destination, v, i));
            }
            put_keyframe(&mut clip.effects, value, destination, interp);
        }
        self.moved = moved;
        self.overwritten = overwritten;
    }

    fn undo(&self, project: &mut Project) {
        let Some(clip) = project.timelines[self.timeline].clip_mut(self.track_index, self.clip_id)
        else {
            return;
        };
        for &(target, frame, _, _) in &self.moved {
            take_keyframe(&mut clip.effects, target, frame + self.delta);
        }
        for &(frame, value, interp) in &self.overwritten {
            put_keyframe(&mut clip.effects, value, frame, interp);
        }
        for &(_, frame, value, interp) in &self.moved {
            put_keyframe(&mut clip.effects, value, frame, interp);
        }
    }
}

/// Changes the outgoing interpolation of a group of keyframes.
#[derive(Debug)]
pub struct SetKeyframeInterpolation {
    pub timeline: TimelineId,
    pub track_index: usize,
    pub clip_id: ClipId,
    pub picks: Vec<KeyframePick>,
    pub interpolation: Interpolation,
    previous: Vec<(KeyframeTarget, FrameIdx, Interpolation)>,
}

impl SetKeyframeInterpolation {
    pub fn new(
        timeline: TimelineId,
        track_index: usize,
        clip_id: ClipId,
        picks: Vec<KeyframePick>,
        interpolation: Interpolation,
    ) -> Self {
        Self {
            timeline,
            track_index,
            clip_id,
            picks,
            interpolation,
            previous: Vec::new(),
        }
    }
}

impl Command for SetKeyframeInterpolation {
    fn label(&self) -> CommandLabel {
        CommandLabel::SetInterpolation
    }

    fn apply(&mut self, project: &mut Project) {
        let Some(clip) = project.timelines[self.timeline].clip_mut(self.track_index, self.clip_id)
        else {
            return;
        };
        self.previous = self
            .picks
            .iter()
            .filter_map(|&(target, frame)| {
                set_keyframe_interpolation(&mut clip.effects, target, frame, self.interpolation)
                    .map(|old| (target, frame, old))
            })
            .collect();
    }

    fn undo(&self, project: &mut Project) {
        let Some(clip) = project.timelines[self.timeline].clip_mut(self.track_index, self.clip_id)
        else {
            return;
        };
        for &(target, frame, interp) in &self.previous {
            set_keyframe_interpolation(&mut clip.effects, target, frame, interp);
        }
    }
}

/// Removes the keyframe of `target` exactly at the given frame, if there is one.
#[derive(Debug)]
pub struct RemoveKeyframe {
    pub timeline: TimelineId,
    pub track_index: usize,
    pub clip_id: ClipId,
    pub target: KeyframeTarget,
    pub frame: FrameIdx,
    removed: Option<(KeyframeValue, Interpolation)>,
}

impl RemoveKeyframe {
    pub fn new(
        timeline: TimelineId,
        track_index: usize,
        clip_id: ClipId,
        target: KeyframeTarget,
        frame: FrameIdx,
    ) -> Self {
        Self {
            timeline,
            track_index,
            clip_id,
            target,
            frame,
            removed: None,
        }
    }
}

impl Command for RemoveKeyframe {
    fn label(&self) -> CommandLabel {
        CommandLabel::RemoveKeyframe
    }

    fn apply(&mut self, project: &mut Project) {
        let Some(clip) = project.timelines[self.timeline].clip_mut(self.track_index, self.clip_id)
        else {
            return;
        };
        self.removed = take_keyframe(&mut clip.effects, self.target, self.frame);
    }

    fn undo(&self, project: &mut Project) {
        let Some((value, interp)) = self.removed else {
            return;
        };
        let Some(clip) = project.timelines[self.timeline].clip_mut(self.track_index, self.clip_id)
        else {
            return;
        };
        put_keyframe(&mut clip.effects, value, self.frame, interp);
    }
}

/// Removes a media from the pool; its clips stay offline. For a compound
/// clip the nested timeline disappears too, otherwise it would stay orphaned
/// in the project (and its name taken, see `alloc_compound_name`).
#[derive(Debug)]
pub struct RemoveMedia {
    media: MediaId,
    removed: RefCell<Option<(MediaItem, Option<Timeline>)>>,
}

impl RemoveMedia {
    pub fn new(media: MediaId) -> Self {
        Self {
            media,
            removed: RefCell::new(None),
        }
    }
}

impl Command for RemoveMedia {
    fn label(&self) -> CommandLabel {
        CommandLabel::RemoveMedia
    }

    fn apply(&mut self, project: &mut Project) {
        let Some(item) = project.media_pool.remove(self.media) else {
            return;
        };
        let nested = item.compound.and_then(|id| project.timelines.remove(id));
        *self.removed.borrow_mut() = Some((item, nested));
    }

    fn undo(&self, project: &mut Project) {
        let Some((item, nested)) = self.removed.borrow_mut().take() else {
            return;
        };
        if let (Some(id), Some(nested)) = (item.compound, nested) {
            project.timelines.insert_at(id, nested);
        }
        project.media_pool.insert_at(self.media, item);
    }
}

/// Relink: new path (and `content_hash`) for a media of the pool.
#[derive(Debug)]
pub struct SetMediaPath {
    media: MediaId,
    new_path: PathBuf,
    new_content_hash: u64,
    /// `None` keeps the current one.
    new_meta: Option<MediaMeta>,
    old: RefCell<Option<(PathBuf, u64, MediaMeta)>>,
    /// Effects of the clips adapted to the new file, as they were: the
    /// rounding of the adaptation is not invertible.
    old_effects: RefCell<Vec<(TimelineId, usize, ClipId, EffectStack)>>,
}

impl SetMediaPath {
    pub fn new(
        media: MediaId,
        new_path: PathBuf,
        new_content_hash: u64,
        new_meta: Option<MediaMeta>,
    ) -> Self {
        Self {
            media,
            new_path,
            new_content_hash,
            new_meta,
            old: RefCell::new(None),
            old_effects: RefCell::new(Vec::new()),
        }
    }
}

impl Command for SetMediaPath {
    fn label(&self) -> CommandLabel {
        CommandLabel::RelinkMedia
    }

    fn apply(&mut self, project: &mut Project) {
        let Some(item) = project.media_pool.get_mut(self.media) else {
            return;
        };
        let old_meta = item.meta.clone();
        *self.old.borrow_mut() = Some((item.path.clone(), item.content_hash, old_meta.clone()));
        item.path = self.new_path.clone();
        item.content_hash = self.new_content_hash;
        let Some(meta) = &self.new_meta else {
            return;
        };
        item.meta = meta.clone();
        project.conform_clips_of(self.media);
        let mut old_effects = self.old_effects.borrow_mut();
        old_effects.clear();
        let fps_changed = meta.fps != old_meta.fps;
        // An offline media imported from OTIO may not know its size.
        let crop_scale = (old_meta.width > 0
            && old_meta.height > 0
            && (meta.width, meta.height) != (old_meta.width, old_meta.height))
            .then(|| {
                (
                    meta.width as f32 / old_meta.width as f32,
                    meta.height as f32 / old_meta.height as f32,
                )
            });
        if !fps_changed && crop_scale.is_none() {
            return;
        }
        for (timeline_id, timeline) in project.timelines.iter_mut() {
            for (track_index, track) in timeline.tracks.iter_mut().enumerate() {
                for clip in &mut track.clips {
                    if !matches!(clip.source, ClipSource::Media(id) if id == self.media) {
                        continue;
                    }
                    old_effects.push((timeline_id, track_index, clip.id, clip.effects.clone()));
                    if fps_changed {
                        clip.effects.rescale_keyframe_times(old_meta.fps, meta.fps);
                    }
                    if let Some((scale_x, scale_y)) = crop_scale {
                        clip.effects.rescale_crop(scale_x, scale_y);
                    }
                }
            }
        }
    }

    fn undo(&self, project: &mut Project) {
        let Some((path, hash, meta)) = self.old.borrow_mut().take() else {
            return;
        };
        if let Some(item) = project.media_pool.get_mut(self.media) {
            item.path = path;
            item.content_hash = hash;
            item.meta = meta;
        }
        if self.new_meta.is_some() {
            project.conform_clips_of(self.media);
        }
        for (timeline_id, track_index, clip_id, effects) in self.old_effects.borrow_mut().drain(..)
        {
            if let Some(clip) = project.timelines[timeline_id].clip_mut(track_index, clip_id) {
                clip.effects = effects;
            }
        }
    }
}

/// `(timeline_start, timeline_end)` of a clip, if it exists.
fn clip_bounds(
    project: &Project,
    timeline_id: TimelineId,
    track_index: usize,
    clip_id: ClipId,
) -> Option<(FrameIdx, FrameIdx)> {
    let clip = project.timelines[timeline_id].clip(track_index, clip_id)?;
    Some((clip.timeline_start, clip.timeline_end()))
}

/// Appends the commands freeing `[new_start, new_end)` from a clip invading
/// it: removed if covered, shortened if it sticks out on one side, split if
/// the stretch falls in the middle (returns the right half).
#[allow(clippy::too_many_arguments)]
fn resolve_overlap(
    project: &mut Project,
    timeline_id: TimelineId,
    track_index: usize,
    clip_id: ClipId,
    old_start: FrameIdx,
    old_end: FrameIdx,
    new_start: FrameIdx,
    new_end: FrameIdx,
    commands: &mut Vec<Box<dyn Command>>,
) -> Option<ClipId> {
    if old_start >= new_start && old_end <= new_end {
        commands.push(Box::new(LiftDelete::new(timeline_id, track_index, clip_id)));
        None
    } else if old_start < new_start && old_end > new_end {
        // The new interval falls in the middle: splits the clip in two,
        // then shortens the right half from its left edge up to
        // `new_end`.
        let right_id = project.alloc_clip_id();
        commands.push(Box::new(
            SplitClip::new(timeline_id, track_index, clip_id, new_start).with_new_clip_id(right_id),
        ));
        commands.push(Box::new(TrimClip::new(
            timeline_id,
            track_index,
            right_id,
            TrimEdge::Start,
            new_end,
        )));
        Some(right_id)
    } else if old_start < new_start {
        // The tail sticks out past `new_start`: shortens the right edge
        // (end) up to there.
        commands.push(Box::new(TrimClip::new(
            timeline_id,
            track_index,
            clip_id,
            TrimEdge::End,
            new_start,
        )));
        None
    } else {
        // The head sticks out before `new_end`: shortens the left
        // edge (start) up to there.
        commands.push(Box::new(TrimClip::new(
            timeline_id,
            track_index,
            clip_id,
            TrimEdge::Start,
            new_end,
        )));
        None
    }
}

/// Appends the commands freeing the stretches in `ranges` (track, start, end):
/// the clips underneath are shortened, split or removed, never left
/// overlapping. If a cut splits several members of the same group across the
/// tracks involved, their right halves get relinked.
pub fn make_room_for_ranges(
    project: &mut Project,
    timeline_id: TimelineId,
    ranges: &[(usize, FrameIdx, FrameIdx)],
    exclude: &[(usize, ClipId)],
    commands: &mut Vec<Box<dyn Command>>,
) {
    let mut processed: BTreeSet<(usize, ClipId)> = exclude.iter().copied().collect();
    let range_tracks: BTreeSet<usize> = ranges.iter().map(|(t, _, _)| *t).collect();

    for &(track_index, new_start, new_end) in ranges {
        if new_start >= new_end {
            continue;
        }
        type Overlapping = (ClipId, FrameIdx, FrameIdx, Option<LinkGroupId>);
        let overlapping: Vec<Overlapping> = project.timelines[timeline_id]
            .tracks
            .get(track_index)
            .map(|t| {
                t.clips
                    .iter()
                    .filter(|c| c.timeline_start < new_end && c.timeline_end() > new_start)
                    .map(|c| (c.id, c.timeline_start, c.timeline_end(), c.linked_group))
                    .collect()
            })
            .unwrap_or_default();

        for (clip_id, old_start, old_end, group) in overlapping {
            if !processed.insert((track_index, clip_id)) {
                continue;
            }
            let split_halves = resolve_overlap(
                project,
                timeline_id,
                track_index,
                clip_id,
                old_start,
                old_end,
                new_start,
                new_end,
                commands,
            );

            if group.is_none() {
                continue;
            }
            let mut new_rights: Vec<(usize, ClipId)> = split_halves
                .map(|right| (track_index, right))
                .into_iter()
                .collect();

            for (member_track, member_id) in
                project.timelines[timeline_id].linked_members(track_index, clip_id)
            {
                if !range_tracks.contains(&member_track)
                    || !processed.insert((member_track, member_id))
                {
                    continue;
                }
                let Some((m_start, m_end)) =
                    clip_bounds(project, timeline_id, member_track, member_id)
                else {
                    continue;
                };
                let member_split = resolve_overlap(
                    project,
                    timeline_id,
                    member_track,
                    member_id,
                    m_start,
                    m_end,
                    new_start,
                    new_end,
                    commands,
                );
                if let Some(right) = member_split {
                    new_rights.push((member_track, right));
                }
            }

            if new_rights.len() >= 2 {
                commands.push(Box::new(LinkClips::new(timeline_id, new_rights)));
            }
        }
    }
}

/// Commands inserting `clips` (track, clip, group) overwriting
/// what is underneath; the clips with the same group key end up in
/// a new linked group.
pub fn insert_overwriting<K: PartialEq>(
    project: &mut Project,
    timeline_id: TimelineId,
    clips: Vec<(usize, Clip, Option<K>)>,
) -> Vec<Box<dyn Command>> {
    let ranges: Vec<(usize, FrameIdx, FrameIdx)> = clips
        .iter()
        .map(|(track, clip, _)| (*track, clip.timeline_start, clip.timeline_end()))
        .collect();
    let mut commands: Vec<Box<dyn Command>> = Vec::new();
    make_room_for_ranges(project, timeline_id, &ranges, &[], &mut commands);
    let mut groups: Vec<(K, Vec<(usize, ClipId)>)> = Vec::new();
    for (track_index, clip, key) in clips {
        if let Some(key) = key {
            match groups.iter_mut().find(|(k, _)| *k == key) {
                Some((_, members)) => members.push((track_index, clip.id)),
                None => groups.push((key, vec![(track_index, clip.id)])),
            }
        }
        commands.push(Box::new(InsertClip {
            timeline: timeline_id,
            track_index,
            clip,
        }));
    }
    for (_, members) in groups {
        if members.len() >= 2 {
            commands.push(Box::new(LinkClips::new(timeline_id, members)));
        }
    }
    commands
}

/// Appends the cuts for the remaining overlaps: the clip starting later
/// wins. A safety net after the bulk moves.
pub fn cut_overlaps(
    project: &mut Project,
    timeline_id: TimelineId,
    commands: &mut Vec<Box<dyn Command>>,
) {
    for track_index in 0..project.timelines[timeline_id].tracks.len() {
        if project.timelines[timeline_id].is_locked(track_index) {
            continue;
        }
        let clips: Vec<(ClipId, FrameIdx, FrameIdx)> = project.timelines[timeline_id].tracks
            [track_index]
            .clips
            .iter()
            .map(|c| (c.id, c.timeline_start, c.timeline_end()))
            .collect();
        for (i, &(clip_id, start, end)) in clips.iter().enumerate() {
            let Some(&(_, cut_at, _)) = clips[i + 1..]
                .iter()
                .filter(|&&(_, other_start, _)| other_start > start && other_start < end)
                .min_by_key(|&&(_, other_start, _)| other_start)
            else {
                // No clip starting inside this one: either it does not
                // overlap anything, or it is the one fully covered.
                if clips[i + 1..]
                    .iter()
                    .any(|&(_, other_start, other_end)| other_start <= start && other_end >= end)
                {
                    commands.push(Box::new(LiftDelete::new(timeline_id, track_index, clip_id)));
                }
                continue;
            };
            commands.push(Box::new(TrimClip::new(
                timeline_id,
                track_index,
                clip_id,
                TrimEdge::End,
                cut_at,
            )));
        }
    }
}

/// The result of `plan_compound_clip`: the nested timeline ready to
/// insert into the pool, and where the resulting new clip goes in the outer
/// timeline. Pure reading: it does not touch `project`, the caller decides whether and
/// how to apply it (see `compound_clip_commands`).
pub struct CompoundPlan {
    pub nested_timeline: Timeline,
    pub range_start: FrameIdx,
    pub len: FrameIdx,
    pub has_video: bool,
    pub has_audio: bool,
    /// Track (in the outer timeline) to place the resulting video clip
    /// on: the lowest among those involved, `Some` only if
    /// `has_video`.
    pub video_track: Option<usize>,
    /// Like `video_track`, for the resulting audio clip.
    pub audio_track: Option<usize>,
}

/// Selection of clips (across several tracks, video and audio together) →
/// contents of a new nested timeline, with the relative order of the
/// tracks involved preserved. `None` if `clips` resolves to no real
/// clip. The caller inserts `nested_timeline`/a `MediaItem` referencing it
/// (`MediaItem::compound`) into the `Project`, then uses
/// `compound_clip_commands` for the part to send to the history.
pub fn plan_compound_clip(
    project: &Project,
    timeline_id: TimelineId,
    clips: &[(usize, ClipId)],
) -> Option<CompoundPlan> {
    let timeline = project.timelines.get(timeline_id)?;
    let mut originals: Vec<(usize, Clip)> = clips
        .iter()
        .filter_map(|&(track_index, id)| {
            Some((
                track_index,
                timeline.tracks.get(track_index)?.clip(id)?.clone(),
            ))
        })
        .collect();
    if originals.is_empty() {
        return None;
    }
    originals.sort_by_key(|(track_index, c)| (*track_index, c.timeline_start));

    let range_start = originals.iter().map(|(_, c)| c.timeline_start).min()?;
    let range_end = originals.iter().map(|(_, c)| c.timeline_end()).max()?;

    let mut distinct: Vec<usize> = originals
        .iter()
        .map(|(track_index, _)| *track_index)
        .collect();
    distinct.sort_unstable();
    distinct.dedup();

    let mut nested_tracks = Vec::new();
    let mut index_map = HashMap::new();
    for &old_index in &distinct {
        index_map.insert(old_index, nested_tracks.len());
        nested_tracks.push(Track::new(timeline.tracks[old_index].kind));
    }

    let mut has_video = false;
    let mut has_audio = false;
    for (old_index, mut clip) in originals.iter().cloned() {
        clip.timeline_start -= range_start;
        let new_index = index_map[&old_index];
        match nested_tracks[new_index].kind {
            TrackKind::Video => has_video = true,
            TrackKind::Audio => has_audio = true,
        }
        nested_tracks[new_index].insert_sorted(clip);
    }

    let video_track = distinct
        .iter()
        .copied()
        .filter(|&i| timeline.tracks[i].kind == TrackKind::Video)
        .min();
    let audio_track = distinct
        .iter()
        .copied()
        .filter(|&i| timeline.tracks[i].kind == TrackKind::Audio)
        .min();
    // The compound clip plays through the gain of its host track: the nested
    // tracks keep only the difference, so the mix sounds the same.
    let host_gain_db = audio_track.map_or(0.0, |i| timeline.tracks[i].mix.gain_db);
    for (&old_index, &new_index) in &index_map {
        if nested_tracks[new_index].kind == TrackKind::Audio {
            nested_tracks[new_index].mix.gain_db =
                timeline.tracks[old_index].mix.gain_db - host_gain_db;
        }
    }

    Some(CompoundPlan {
        nested_timeline: Timeline {
            name: "Compound".into(),
            fps: timeline.fps,
            resolution: timeline.resolution,
            tracks: nested_tracks,
            markers: Vec::new(),
            master: Default::default(),
        },
        range_start,
        len: range_end - range_start,
        has_video,
        has_audio,
        video_track,
        audio_track,
    })
}

/// Commands (to be sent to the history as a single group, e.g. inside a
/// `CompositeCommand`) that add the nested timeline of `plan` and its pool
/// item, remove `clips` from the outer timeline and put in their place the
/// resulting clip — or the two, video and audio, linked. `plan` is the one
/// from `plan_compound_clip` for the same `clips`. Returns the new pool item
/// too.
pub fn compound_clip_commands(
    project: &mut Project,
    timeline_id: TimelineId,
    clips: &[(usize, ClipId)],
    plan: &CompoundPlan,
) -> (MediaId, Vec<Box<dyn Command>>) {
    let mut add = crate::AddEntities::new(CommandLabel::MakeCompoundClip);
    let nested = add.bare_timeline(project, plan.nested_timeline.clone());
    let item = MediaItem {
        path: project.alloc_compound_name().into(),
        meta: MediaMeta {
            duration_frames: plan.len,
            fps: plan.nested_timeline.fps,
            width: plan.nested_timeline.resolution.0,
            height: plan.nested_timeline.resolution.1,
            has_video: plan.has_video,
            has_audio: plan.has_audio,
            sample_rate: 48_000,
            channels: 2,
            audio_streams: 1,
            file: Default::default(),
        },
        content_hash: project.alloc_compound_generation(),
        compound: Some(nested),
        folder: None,
    };
    let media_id = add.media(project, item);
    let mut commands: Vec<Box<dyn Command>> = vec![Box::new(add)];
    commands.extend(clips.iter().map(|&(track_index, clip_id)| {
        Box::new(LiftDelete::new(timeline_id, track_index, clip_id)) as Box<dyn Command>
    }));

    let mut new_members = Vec::new();
    let mut push_clip =
        |project: &mut Project, track_index: usize, commands: &mut Vec<Box<dyn Command>>| {
            let clip = Clip::from_source_range(
                project.alloc_clip_id(),
                ClipSource::Media(media_id),
                0,
                plan.len,
                plan.range_start,
                Rational::one(),
            );
            new_members.push((track_index, clip.id));
            commands.push(Box::new(InsertClip {
                timeline: timeline_id,
                track_index,
                clip,
            }));
        };
    if let Some(track_index) = plan.video_track.filter(|_| plan.has_video) {
        push_clip(project, track_index, &mut commands);
    }
    if let Some(track_index) = plan.audio_track.filter(|_| plan.has_audio) {
        push_clip(project, track_index, &mut commands);
    }
    if new_members.len() >= 2 {
        commands.push(Box::new(LinkClips::new(timeline_id, new_members)));
    }
    (media_id, commands)
}

#[derive(Debug)]
pub struct SetProcessingPrecision {
    precision: ProcessingPrecision,
    old: RefCell<Option<ProcessingPrecision>>,
}

impl SetProcessingPrecision {
    pub fn new(precision: ProcessingPrecision) -> Self {
        Self {
            precision,
            old: RefCell::new(None),
        }
    }
}

impl Command for SetProcessingPrecision {
    fn label(&self) -> CommandLabel {
        CommandLabel::ProcessingPrecision
    }

    fn apply(&mut self, project: &mut Project) {
        *self.old.borrow_mut() = Some(std::mem::replace(&mut project.precision, self.precision));
    }

    fn undo(&self, project: &mut Project) {
        if let Some(old) = self.old.borrow_mut().take() {
            project.precision = old;
        }
    }
}

#[cfg(test)]
#[path = "tests/command.rs"]
mod tests;
