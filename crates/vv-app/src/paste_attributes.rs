//! "Paste attributes" (Alt+V): copies onto the selected clips only the
//! attributes chosen in the dialog, taken from the clips in the clipboard.

use super::*;
use std::collections::HashSet;
use vv_core::{Clip, ClipAttributes, FrameIdx, Keyframed, Lerp, Rational, TransformParam};

/// Serialized in the library presets: the names must never change.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Attribute {
    Fades,
    Speed,
    BlendMode,
    Matte,
    Opacity,
    Position,
    Rotation,
    AnchorPoint,
    Zoom,
    Crop,
    CropSoftness,
    Flip,
    Filters,
    Masks,
    Transitions,
    Volume,
}

/// The dialog's sections, in the order and with the names of the
/// Properties panel.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Section {
    Transform,
    Crop,
    Blending,
    Filters,
    Masks,
    Transitions,
    Audio,
    Clip,
}

impl Section {
    pub(crate) const ALL: [Section; 8] = [
        Section::Transform,
        Section::Crop,
        Section::Blending,
        Section::Filters,
        Section::Masks,
        Section::Transitions,
        Section::Audio,
        Section::Clip,
    ];

    fn title(self) -> std::borrow::Cow<'static, str> {
        match self {
            Section::Transform => t!("props.transform"),
            Section::Crop => t!("props.cropping"),
            Section::Blending => t!("props.composite"),
            Section::Filters => t!("props.filters"),
            Section::Masks => t!("props.masks"),
            Section::Transitions => t!("props.transition"),
            Section::Audio => t!("props.tab_audio"),
            Section::Clip => t!("paste_attr.clip"),
        }
    }

    /// The kind of clip the section applies to; `None` = any.
    pub(crate) fn kind(self) -> Option<TrackKind> {
        match self {
            Section::Clip => None,
            Section::Audio => Some(TrackKind::Audio),
            _ => Some(TrackKind::Video),
        }
    }
}

impl Attribute {
    pub(crate) const ALL: [Attribute; 16] = [
        Attribute::Zoom,
        Attribute::Position,
        Attribute::Rotation,
        Attribute::AnchorPoint,
        Attribute::Flip,
        Attribute::Crop,
        Attribute::CropSoftness,
        Attribute::BlendMode,
        Attribute::Matte,
        Attribute::Opacity,
        Attribute::Filters,
        Attribute::Masks,
        Attribute::Transitions,
        Attribute::Volume,
        Attribute::Fades,
        Attribute::Speed,
    ];

    pub(crate) fn section(self) -> Section {
        match self {
            Attribute::Zoom
            | Attribute::Position
            | Attribute::Rotation
            | Attribute::AnchorPoint
            | Attribute::Flip => Section::Transform,
            Attribute::Crop | Attribute::CropSoftness => Section::Crop,
            Attribute::BlendMode | Attribute::Matte | Attribute::Opacity => Section::Blending,
            Attribute::Filters => Section::Filters,
            Attribute::Masks => Section::Masks,
            Attribute::Transitions => Section::Transitions,
            Attribute::Volume => Section::Audio,
            Attribute::Fades | Attribute::Speed => Section::Clip,
        }
    }

    fn label(self) -> std::borrow::Cow<'static, str> {
        match self {
            Attribute::Fades => t!("paste_attr.fades"),
            Attribute::Speed => t!("paste_attr.speed"),
            Attribute::BlendMode => t!("props.composite_mode"),
            Attribute::Matte => t!("props.matte"),
            Attribute::Opacity => t!("props.opacity"),
            Attribute::Position => t!("props.position"),
            Attribute::Rotation => t!("props.rotation"),
            Attribute::AnchorPoint => t!("props.anchor_point"),
            Attribute::Zoom => t!("props.zoom"),
            Attribute::Crop => t!("paste_attr.crop_edges"),
            Attribute::CropSoftness => t!("props.softness"),
            Attribute::Flip => t!("props.flip"),
            Attribute::Filters => t!("props.filters"),
            Attribute::Masks => t!("props.masks"),
            Attribute::Transitions => t!("props.transition"),
            Attribute::Volume => t!("props.volume"),
        }
    }

    /// Transform tracks the attribute is made of; empty for the ones that
    /// are not a transform parameter.
    fn transform_params(self) -> &'static [TransformParam] {
        match self {
            Attribute::Opacity => &[TransformParam::Opacity],
            Attribute::Position => &[TransformParam::PositionX, TransformParam::PositionY],
            Attribute::Rotation => &[TransformParam::Rotation],
            Attribute::AnchorPoint => &[TransformParam::AnchorX, TransformParam::AnchorY],
            Attribute::Zoom => &[TransformParam::ZoomX, TransformParam::ZoomY],
            Attribute::Crop => &[
                TransformParam::CropLeft,
                TransformParam::CropTop,
                TransformParam::CropRight,
                TransformParam::CropBottom,
            ],
            Attribute::CropSoftness => &[TransformParam::CropSoftness],
            _ => &[],
        }
    }
}

/// What to do with the keyframes of an attribute when source and
/// destination clips have different durations.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum KeyframeMode {
    MaintainTiming,
    StretchToFit,
}

pub(crate) struct PasteAttributesDialog {
    /// Name shown as the origin of the attributes.
    source_name: String,
    /// Clips whose attributes are read, one per track kind: a copied
    /// A/V group pastes the video attributes onto the video clips and the
    /// audio ones onto the audio clips.
    source_video: Option<Clip>,
    source_audio: Option<Clip>,
    targets: Vec<(usize, ClipId)>,
    video_targets: usize,
    audio_targets: usize,
    selected: HashSet<Attribute>,
    keyframe_mode: KeyframeMode,
}

impl PasteAttributesDialog {
    fn has_source_for(&self, kind: TrackKind) -> bool {
        match kind {
            TrackKind::Video => self.source_video.is_some(),
            TrackKind::Audio => self.source_audio.is_some(),
        }
    }

    /// A section is shown only if some target can receive it.
    fn section_applies(&self, section: Section) -> bool {
        match section.kind() {
            None => !self.targets.is_empty(),
            Some(TrackKind::Video) => {
                self.video_targets > 0 && self.has_source_for(TrackKind::Video)
            }
            Some(TrackKind::Audio) => {
                self.audio_targets > 0 && self.has_source_for(TrackKind::Audio)
            }
        }
    }
}

impl VenturiApp {
    /// Opens the dialog on the current selection; does nothing if there is
    /// nothing to copy from or onto.
    pub(crate) fn open_paste_attributes_dialog(&mut self) {
        let Some(timeline_id) = self.timeline_id else {
            return;
        };
        if self.timeline_state.clipboard.is_empty() || self.timeline_state.selected.is_empty() {
            return;
        }
        let tl = &self.session.project.timelines[timeline_id];
        let source_of = |kind: TrackKind| {
            self.timeline_state
                .clipboard
                .iter()
                .find(|e| e.track_kind == kind)
                .map(|e| e.clip.clone())
        };
        let source_video = source_of(TrackKind::Video);
        let source_audio = source_of(TrackKind::Audio);
        let source_name = source_video
            .as_ref()
            .or(source_audio.as_ref())
            .map(|clip| self.clip_display_name(clip))
            .unwrap_or_default();

        let targets: Vec<(usize, ClipId)> = self
            .timeline_state
            .selected
            .iter()
            .copied()
            .filter(|&(track_index, clip_id)| {
                !tl.is_locked(track_index) && tl.clip(track_index, clip_id).is_some()
            })
            .collect();
        if targets.is_empty() {
            return;
        }
        let count_of = |kind: TrackKind| {
            targets
                .iter()
                .filter(|&&(track_index, _)| tl.tracks[track_index].kind == kind)
                .count()
        };
        let (video_targets, audio_targets) =
            (count_of(TrackKind::Video), count_of(TrackKind::Audio));

        self.paste_attributes = Some(PasteAttributesDialog {
            source_name,
            source_video,
            source_audio,
            targets,
            video_targets,
            audio_targets,
            selected: self.paste_attributes_selection.clone(),
            keyframe_mode: self.paste_attributes_keyframe_mode,
        });
    }

    pub(crate) fn clip_display_name(&self, clip: &Clip) -> String {
        match &clip.source {
            vv_core::ClipSource::Media(media_id) => self
                .session
                .project
                .media_pool
                .get(*media_id)
                .map(|item| {
                    item.path
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_else(|| item.path.to_string_lossy().into_owned())
                })
                .unwrap_or_default(),
            vv_core::ClipSource::SolidColor => t!("generator.solid_color").into_owned(),
            vv_core::ClipSource::Text => t!("generator.text").into_owned(),
            vv_core::ClipSource::Adjustment => t!("generator.adjustment").into_owned(),
        }
    }

    pub(crate) fn show_paste_attributes_dialog(&mut self, ctx: &egui::Context) {
        let Some(dialog) = &mut self.paste_attributes else {
            return;
        };
        let mut open = true;
        let mut apply = false;
        let mut cancel = false;
        egui::Window::new(t!("paste_attr.title"))
            .id(egui::Id::new("paste_attributes_dialog"))
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .default_width(420.0)
            .show(ctx, |ui| {
                ui.label(t!(
                    "paste_attr.context",
                    source = dialog.source_name,
                    count = dialog.targets.len()
                ));

                let applies: Vec<Section> = Section::ALL
                    .into_iter()
                    .filter(|&s| dialog.section_applies(s))
                    .collect();
                attribute_checkboxes(ui, &mut dialog.selected, &applies);

                ui.separator();
                ui.label(egui::RichText::new(t!("paste_attr.keyframes")).strong());
                ui.radio_value(
                    &mut dialog.keyframe_mode,
                    KeyframeMode::MaintainTiming,
                    t!("paste_attr.maintain_timing"),
                );
                ui.radio_value(
                    &mut dialog.keyframe_mode,
                    KeyframeMode::StretchToFit,
                    t!("paste_attr.stretch_to_fit"),
                );

                ui.separator();
                ui.horizontal(|ui| {
                    if ui.button(t!("common.cancel")).clicked() {
                        cancel = true;
                    }
                    if ui
                        .add_enabled(
                            !dialog.selected.is_empty(),
                            egui::Button::new(t!("paste_attr.apply")),
                        )
                        .clicked()
                    {
                        apply = true;
                    }
                });
            });
        if !open {
            cancel = true;
        }
        if !(apply || cancel) {
            return;
        }
        let dialog = self.paste_attributes.take().expect("just checked");
        // The choices stay for the next time, as in the other NLEs.
        self.paste_attributes_selection = dialog.selected.clone();
        self.paste_attributes_keyframe_mode = dialog.keyframe_mode;
        if apply {
            self.apply_paste_attributes(&dialog);
        }
    }

    fn apply_paste_attributes(&mut self, dialog: &PasteAttributesDialog) {
        let source_ids =
            [&dialog.source_video, &dialog.source_audio].map(|c| c.as_ref().map(|c| c.id));
        let targets: Vec<(usize, ClipId)> = dialog
            .targets
            .iter()
            .copied()
            .filter(|(_, id)| !source_ids.contains(&Some(*id)))
            .collect();
        let mode = dialog.keyframe_mode;
        self.apply_attributes(
            &targets,
            dialog.source_video.as_ref(),
            dialog.source_audio.as_ref(),
            &dialog.selected,
            |source, target, _| Conversion::paste(source, target, mode),
            vv_core::CommandLabel::PasteAttributes,
        );
    }

    /// Sets the `selected` attributes of the source of each target's kind
    /// onto the targets, as one undo step.
    pub(crate) fn apply_attributes(
        &mut self,
        targets: &[(usize, ClipId)],
        source_video: Option<&Clip>,
        source_audio: Option<&Clip>,
        selected: &HashSet<Attribute>,
        conversion: impl Fn(&Clip, &Clip, &vv_core::Timeline) -> Conversion,
        label: vv_core::CommandLabel,
    ) {
        let Some(timeline_id) = self.timeline_id else {
            return;
        };
        let mut commands: Vec<(usize, ClipId, ClipAttributes)> = Vec::new();
        let mut speeds: SpeedTargets = Vec::new();
        {
            let tl = &self.session.project.timelines[timeline_id];
            for &(track_index, clip_id) in targets {
                let (Some(track), Some(target)) =
                    (tl.tracks.get(track_index), tl.clip(track_index, clip_id))
                else {
                    continue;
                };
                if tl.is_locked(track_index) {
                    continue;
                }
                let source = match track.kind {
                    TrackKind::Video => source_video,
                    TrackKind::Audio => source_audio,
                };
                let Some(source) = source else {
                    continue;
                };
                commands.push((
                    track_index,
                    clip_id,
                    merge(source, target, selected, &conversion(source, target, tl)),
                ));
                if selected.contains(&Attribute::Speed)
                    && matches!(target.source, vv_core::ClipSource::Media(_))
                    && (target.speed(), target.pitch_correction)
                        != (source.speed(), source.pitch_correction)
                {
                    let key = (source.speed(), source.pitch_correction);
                    match speeds.iter_mut().find(|(k, _)| *k == key) {
                        Some((_, clips)) => clips.push((track_index, clip_id)),
                        None => speeds.push((key, vec![(track_index, clip_id)])),
                    }
                }
            }
        }
        if commands.is_empty() {
            return;
        }
        let mark = self.session.history.begin_group();
        for (track_index, clip_id, attributes) in commands {
            self.session.history.do_command(
                &mut self.session.project,
                Box::new(vv_core::SetClipAttributes::new(
                    timeline_id,
                    track_index,
                    clip_id,
                    attributes,
                )),
            );
        }
        // Ripple: several pasted clips can be adjacent, and each one's new
        // length must not overwrite the next.
        for ((speed, pitch_correction), clips) in speeds {
            self.session.history.do_command(
                &mut self.session.project,
                Box::new(vv_core::SetClipSpeed::new(
                    timeline_id,
                    clips,
                    speed,
                    pitch_correction,
                    vv_core::SpeedFit::Ripple,
                )),
            );
        }
        self.session.history.end_group_as(mark, label);
    }
}

/// One checkbox per section of `sections`, plus one per attribute when the
/// section has more than one.
pub(crate) fn attribute_checkboxes(
    ui: &mut egui::Ui,
    selected: &mut HashSet<Attribute>,
    sections: &[Section],
) {
    for &section in sections {
        let members: Vec<Attribute> = Attribute::ALL
            .into_iter()
            .filter(|a| a.section() == section)
            .collect();
        ui.separator();
        let mut all = members.iter().all(|a| selected.contains(a));
        if ui
            .checkbox(&mut all, egui::RichText::new(section.title()).strong())
            .changed()
        {
            for attribute in &members {
                if all {
                    selected.insert(*attribute);
                } else {
                    selected.remove(attribute);
                }
            }
        }
        if members.len() < 2 {
            continue;
        }
        ui.indent(("attribute_section", section as usize), |ui| {
            ui.horizontal_wrapped(|ui| {
                for attribute in &members {
                    let mut on = selected.contains(attribute);
                    if ui.checkbox(&mut on, attribute.label()).changed() {
                        if on {
                            selected.insert(*attribute);
                        } else {
                            selected.remove(attribute);
                        }
                    }
                }
            });
        });
    }
}

/// Targets grouped by the speed (and pitch correction) they get.
type SpeedTargets = Vec<((vv_core::Rational, bool), Vec<(usize, ClipId)>)>;

#[cfg(test)]
fn merged_attributes(
    source: &Clip,
    target: &Clip,
    selected: &HashSet<Attribute>,
    mode: KeyframeMode,
) -> ClipAttributes {
    merge(
        source,
        target,
        selected,
        &Conversion::paste(source, target, mode),
    )
}

/// The attributes of `target` with the ones selected replaced by those of
/// `source`. Durations (fades, transitions) are clamped to the target clip,
/// which can be shorter than the source one.
fn merge(
    source: &Clip,
    target: &Clip,
    selected: &HashSet<Attribute>,
    conversion: &Conversion,
) -> ClipAttributes {
    let mut attributes = ClipAttributes::of(target);
    let remap = &conversion.remap;
    let len = target.timeline_len.max(1);
    let duration = |frames: FrameIdx| conversion.duration(frames);
    let [sx, sy] = conversion.space_scale;
    let scaled = |track: &Keyframed<f32>, k: f32| {
        let mut track = remap.apply(track);
        if k != 1.0 {
            track.map_values(|v| v * k);
        }
        track
    };

    for attribute in selected {
        for param in attribute.transform_params() {
            let k = match param {
                TransformParam::PositionX | TransformParam::AnchorX => sx,
                TransformParam::PositionY | TransformParam::AnchorY => sy,
                _ => 1.0,
            };
            *attributes.effects.transform.track_mut(*param) =
                scaled(source.effects.transform.track(*param), k);
        }
        match attribute {
            Attribute::Fades => {
                attributes.fade_in = duration(source.fade_in).clamp(0, len);
                attributes.fade_out = duration(source.fade_out).clamp(0, len);
            }
            Attribute::BlendMode => attributes.effects.blend_mode = source.effects.blend_mode,
            Attribute::Matte => attributes.effects.matte = source.effects.matte,
            Attribute::Flip => attributes.effects.transform.flip = source.effects.transform.flip,
            Attribute::Filters => {
                let size = (sx * sy).sqrt();
                let filters = source.effects.filters.iter().map(|f| {
                    let mut filter = vv_core::ClipFilter {
                        radius: scaled(&f.radius, size),
                        direction: remap.apply(&f.direction),
                        amount: remap.apply(&f.amount),
                        offset: [scaled(&f.offset[0], sx), scaled(&f.offset[1], sy)],
                        grow: scaled(&f.grow, size),
                        ..f.clone()
                    };
                    for track in filter.shake.tracks_mut() {
                        *track = remap.apply(track);
                    }
                    for track in filter.grade.tracks_mut() {
                        *track = remap.apply(track);
                    }
                    filter
                });
                if conversion.append_filters {
                    attributes.effects.filters.extend(filters);
                } else {
                    attributes.effects.filters = filters.collect();
                }
            }
            Attribute::Masks => {
                attributes.effects.masks = source.effects.masks.clone();
                for mask in &mut attributes.effects.masks {
                    for track in mask.tracks_mut() {
                        *track = remap.apply(track);
                    }
                    mask.path = remap.apply(&mask.path);
                }
            }
            Attribute::Transitions => {
                let clamped = |t: &Option<vv_core::Transition>| {
                    t.clone().map(|mut t| {
                        t.duration = duration(t.duration).clamp(1, len);
                        t
                    })
                };
                attributes.effects.transition_in = clamped(&source.effects.transition_in);
                attributes.effects.transition_out = clamped(&source.effects.transition_out);
            }
            Attribute::Volume => {
                attributes.effects.gain_db = remap.apply(&source.effects.gain_db);
            }
            _ => {}
        }
    }
    attributes
}

/// Where a preset's keyframes are pinned on a clip of another length.
#[derive(Clone, Copy, PartialEq, Eq, Debug, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum KeyframeAnchor {
    Start,
    End,
    Stretch,
}

/// How the values of a source clip are brought onto a target clip.
pub(crate) struct Conversion {
    remap: KeyframeRemap,
    /// Target fps over source fps, for the durations in timeline frames.
    time_scale: f64,
    /// Target resolution over source resolution, per axis, for the values
    /// in timeline pixels.
    space_scale: [f32; 2],
    append_filters: bool,
}

/// The timeline a preset's source clip lived in.
#[derive(Clone, Copy)]
pub(crate) struct TimelineFormat {
    pub fps: Rational,
    pub resolution: (u32, u32),
}

impl Conversion {
    fn paste(source: &Clip, target: &Clip, mode: KeyframeMode) -> Self {
        Self {
            remap: KeyframeRemap::source_frames(source, target, mode),
            time_scale: 1.0,
            space_scale: [1.0, 1.0],
            append_filters: false,
        }
    }

    /// Keyframes keep their *time* from the anchor, so they survive a
    /// different fps, speed or media fps.
    pub(crate) fn preset(
        source: &Clip,
        from: TimelineFormat,
        target: &Clip,
        to: TimelineFormat,
        anchor: KeyframeAnchor,
        append_filters: bool,
    ) -> Self {
        let time_scale = to.fps.as_f64() / from.fps.as_f64().max(1e-9);
        let axis = |to: u32, from: u32| to as f32 / from.max(1) as f32;
        Self {
            remap: KeyframeRemap::Anchored {
                source: Box::new(source.clone()),
                target: Box::new(target.clone()),
                time_scale,
                anchor,
            },
            time_scale,
            space_scale: [
                axis(to.resolution.0, from.resolution.0),
                axis(to.resolution.1, from.resolution.1),
            ],
            append_filters,
        }
    }

    fn duration(&self, frames: FrameIdx) -> FrameIdx {
        (frames as f64 * self.time_scale).round() as FrameIdx
    }
}

/// Source frames of the keyframes of `source` brought into the space of
/// `target`: the clips can start at a different point of their media and
/// last a different time.
enum KeyframeRemap {
    SourceFrames {
        source_in: FrameIdx,
        target_in: FrameIdx,
        target_out: FrameIdx,
        /// `None` keeps the distances from the start of the clip (Maintain Timing).
        scale: Option<f64>,
    },
    Anchored {
        source: Box<Clip>,
        target: Box<Clip>,
        time_scale: f64,
        anchor: KeyframeAnchor,
    },
}

impl KeyframeRemap {
    fn source_frames(source: &Clip, target: &Clip, mode: KeyframeMode) -> Self {
        let scale = match mode {
            KeyframeMode::MaintainTiming => None,
            KeyframeMode::StretchToFit => {
                Some(target.source_len().max(1) as f64 / source.source_len().max(1) as f64)
            }
        };
        Self::SourceFrames {
            source_in: source.source_in(),
            target_in: target.source_in(),
            target_out: target.source_out(),
            scale,
        }
    }

    fn target_range(&self) -> std::ops::Range<FrameIdx> {
        match self {
            Self::SourceFrames {
                target_in,
                target_out,
                ..
            } => *target_in..*target_out,
            Self::Anchored { target, .. } => target.source_in()..target.source_out(),
        }
    }

    fn frame(&self, frame: FrameIdx) -> FrameIdx {
        match self {
            Self::SourceFrames {
                source_in,
                target_in,
                scale,
                ..
            } => {
                let offset = frame - source_in;
                let offset = match scale {
                    Some(scale) => (offset as f64 * scale).round() as FrameIdx,
                    None => offset,
                };
                target_in + offset
            }
            Self::Anchored {
                source,
                target,
                time_scale,
                anchor,
            } => {
                let offset = source.timeline_frame_at(frame) - source.timeline_start;
                let rescale = |frames: FrameIdx| (frames as f64 * time_scale).round() as FrameIdx;
                let offset = match anchor {
                    KeyframeAnchor::Start => rescale(offset),
                    KeyframeAnchor::End => {
                        target.timeline_len + rescale(offset - source.timeline_len)
                    }
                    KeyframeAnchor::Stretch => (offset as f64 * target.timeline_len as f64
                        / source.timeline_len.max(1) as f64)
                        .round() as FrameIdx,
                };
                target.source_frame_at(target.timeline_start + offset)
            }
        }
    }

    /// Keyframes outside the target clip are cut as a trim would: the
    /// animation keeps the value it has at the edges of the clip.
    fn apply<T: Lerp + Clone + PartialEq>(&self, track: &Keyframed<T>) -> Keyframed<T> {
        let range = self.target_range();
        let mut out = Keyframed::constant(track.default.clone());
        for (frame, value, interpolation) in track.keyframes() {
            out.upsert(self.frame(*frame), value.clone(), *interpolation);
        }
        out.drop_before(range.start);
        out.drop_from(range.end);
        out
    }
}

#[cfg(test)]
#[path = "tests/paste_attributes.rs"]
mod tests;
