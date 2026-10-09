//! Multi-track timeline widget: clips, selection (click, ctrl, shift,
//! rubber band, always extended to the linked groups), drag, trim, playhead.
//! Changes to the project go through `History::do_command`; only the
//! selection mutates `TimelineState`. Drawn with the painter: for a dense
//! grid of rectangles it costs less than nested widgets.

use egui::emath::GuiRounding as _;
use std::borrow::Cow;
use std::collections::BTreeSet;

use vv_core::{
    Clip, ClipId, ClipSource, FadeEdge, FrameIdx, History, Keyframed, Project, TimelineId, Track,
    TrackFlag, TrackKind, TrimEdge,
};

#[path = "timeline_markers.rs"]
mod markers;
pub(crate) use markers::add_marker_at_playhead;

const ROW_HEIGHT: f32 = 40.0;
/// Maximum vertical zoom (Shift+wheel); the minimum is `ROW_HEIGHT`.
const MAX_ROW_HEIGHT: f32 = ROW_HEIGHT * 4.0;
/// Vertical zoom gain per pixel of wheel.
const ROW_ZOOM_SPEED: f32 = 1.0 / 200.0;
/// The ruler is the tick band plus the marker lane below it.
const RULER_TICKS_HEIGHT: f32 = 20.0;
const MARKER_LANE_HEIGHT: f32 = 14.0;
const RULER_HEIGHT: f32 = RULER_TICKS_HEIGHT + MARKER_LANE_HEIGHT;
const MIN_TIMELINE_SECS: f64 = 20.0;
const TRAILING_MARGIN_SECS: f64 = 5.0;
/// Height of the draggable separator between the Video group and the Audio group.
const GROUP_DIVIDER_HEIGHT: f32 = 8.0;
/// Fixed column on the left of the timeline (track label + remove),
/// not involved in the horizontal scroll — see `draw_track_headers`.
const TRACK_HEADER_WIDTH: f32 = 140.0;
const MIN_PANE_HEIGHT: f32 = 20.0;
/// Minimum "new track" zone past the last track when the box
/// scrolls: without it, with many tracks there would be nowhere to drag a new one.
const NEW_TRACK_ZONE_HEIGHT: f32 = 24.0;
const PANE_SCROLLBAR_WIDTH: f32 = 8.0;

/// (track, id): the id is a global counter, the track serves to find it.
type ClipKey = (usize, ClipId);

/// What the properties panel shows when a transition is selected
/// instead of a clip — an alternative to `TimelineState::selected`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TransitionSelection {
    /// Transition on a single edge (`EffectStack::transition_in`/`_out`).
    Edge(ClipKey, FadeEdge),
    /// Straddling transition, identified by its `left_clip` (a clip has
    /// at most one crossing on its own right edge) and by the track.
    Crossing(usize, ClipId),
}

/// A take in progress, as the timeline draws it.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct RecordingView {
    pub start: FrameIdx,
    pub tracks: Vec<usize>,
    /// Peak of every `1 / RECORDING_PEAKS_PER_SEC` s recorded so far.
    pub peaks: Vec<f32>,
}

pub const RECORDING_PEAKS_PER_SEC: f64 = 100.0;

/// What a drag on a clip does; picked from the toolbar under the viewer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TimelineTool {
    /// Move, trim, roll, fades: the edge or handle under the pointer decides.
    #[default]
    Select,
    /// Slides the media under the clip, which keeps its place and length.
    Slip,
}

pub struct TimelineState {
    pub tool: TimelineTool,
    /// Selected clips. Empty if no clip is selected (it must not
    /// be confused with "no timeline": here it is only the state of the
    /// selection inside an existing timeline).
    pub selected: BTreeSet<ClipKey>,
    /// The project has a folder to record into: set by the app every frame.
    pub can_record: bool,
    /// Arming was refused for that: the app shows why.
    pub record_needs_save: bool,
    pub recording: Option<RecordingView>,
    /// Origin of the shift+click. A shift+click does not move it, as in
    /// file managers.
    selection_anchor: Option<ClipKey>,
    pub playhead: FrameIdx,
    pixels_per_sec: f32,
    /// Track height (vertical zoom): `ROW_HEIGHT` is the minimum.
    row_height: f32,
    /// `pixels_per_sec` of the last drawing: if it changed there was a zoom
    /// in this frame, and the scroll must be corrected to anchor it to the playhead.
    last_rendered_pps: f32,
    /// The pointer gesture in progress, if any.
    gesture: Option<Gesture>,
    /// Selected gap (track, start, end), an alternative to `selected`:
    /// it is closed with a ripple delete. The space at the end is not a gap.
    pub selected_gap: Option<(usize, FrameIdx, FrameIdx)>,
    /// Selected transition (single edge or crossing), an alternative to
    /// `selected`: a click on it replaces any clip selection,
    /// even a multiple one — the properties panel shows its
    /// controls instead of the clip's.
    pub selected_transition: Option<TransitionSelection>,
    /// Copied clips (Ctrl+C in `main.rs`), ready to be pasted
    /// (Ctrl+V) at the playhead position. Empty if nothing has ever
    /// been copied in this session yet.
    pub clipboard: Vec<ClipboardEntry>,
    /// Height of the Video box if the user dragged the separator
    /// (see `GROUP_DIVIDER_HEIGHT`); `None` = groups centered by default.
    video_pane_height: Option<f32>,
    /// Vertical scroll of the Video box, measured from the bottom: the video
    /// tracks rest against the separator, as in an NLE.
    video_scroll: f32,
    audio_scroll: f32,
    /// Residual speed (px/s) of the kinetic touchpad scroll: once the gesture
    /// is over the view keeps scrolling and brakes with friction until it
    /// reaches `KINETIC_STOP_SPEED`, instead of stopping abruptly.
    hscroll_vel: f32,
    video_scroll_vel: f32,
    audio_scroll_vel: f32,
    /// In/out of the timeline: the exported portion.
    pub export_marks: crate::transport::MarkRange,
    /// "Paste attributes" asked from the context menu: the dialog lives in
    /// the app, which opens it and clears the flag.
    pub paste_attributes_requested: bool,
    /// The playhead jumped programmatically (e.g. ripple delete): the next
    /// frame brings it into view if it left, without waiting for playback.
    pub reveal_playhead: bool,
    /// Clips showing the retime bar (Ctrl+R): dragging their right edge
    /// changes the speed instead of trimming.
    pub retime_controls: BTreeSet<ClipId>,
    /// "Change Clip Speed…" asked for these clips: the dialog lives in the
    /// app, which opens it and clears this.
    pub speed_dialog_requested: Option<Vec<ClipKey>>,
    /// "Remove Silences…" asked for these clips: handled by the app.
    pub silence_dialog_requested: Option<Vec<ClipKey>>,
    /// Timeline ranges the open silence dialog would remove, painted red.
    pub silence_preview: Vec<(FrameIdx, FrameIdx)>,
    /// "Show in Media Pool" asked for this media: handled by the app.
    pub reveal_in_pool_requested: Option<vv_core::MediaId>,
    marker_drag: Option<markers::MarkerDrag>,
    marker_editor: Option<markers::MarkerEditor>,
    /// Where each clip was drawn in the last frame: the tests aim their
    /// synthetic pointer events with it.
    #[cfg(test)]
    clip_rects: std::collections::HashMap<ClipId, egui::Rect>,
    #[cfg(test)]
    marker_lane: egui::Rect,
}

/// A copied clip. Id and group are reassigned on paste; the position
/// is relative to the leftmost of the copied clips.
#[derive(Clone)]
pub struct ClipboardEntry {
    /// Origin track as V/A + number, not an absolute index: pasting
    /// from a compound clip with video tracks only into a
    /// video+audio timeline must land on V2, not on the track of index 2 (which there
    /// is audio).
    pub track_kind: TrackKind,
    pub track_number: usize,
    pub relative_start: FrameIdx,
    /// The clip as it was at copy time; id, position and group are reassigned
    /// on paste.
    pub clip: Clip,
    /// Fps of the origin timeline, in which `clip` and
    /// `relative_start` are expressed.
    pub timeline_fps: vv_core::Rational,
    /// Clips with the same tag were in the same group at copy time.
    pub link_tag: Option<u64>,
}

/// A pointer gesture on the timeline: at most one at a time.
enum Gesture {
    Move(DragState),
    /// Selection rectangle, in content coordinates (it stays valid if the
    /// scroll changes).
    Marquee(MarqueeDrag),
    Trim(TrimState),
    Retime(RetimeState),
    Fade(EdgeDragState),
    TransitionLength(EdgeDragState),
    CrossingLength(CrossingDragState),
    /// Alt+drag on the body (not on the handle) of a transition marker of
    /// this clip: it duplicates instead of resizing. The DnD payload set by
    /// `begin_transition_duplicate_drag` does the work; this only marks when
    /// the gesture ends.
    DuplicateTransition(ClipId),
    Volume(VolumeDragState),
    Slip(SlipState),
}

struct MarqueeDrag {
    start: egui::Pos2,
    current: egui::Pos2,
}

struct DragState {
    /// The pressed clip: its position drives the snapping, the others
    /// follow it with the initial offset.
    clip_id: ClipId,
    /// Starting track: the candidate track (see `track_drag_target`)
    /// may differ during the drag, the bounds are recomputed every frame.
    track_index: usize,
    original_start: FrameIdx,
    accum_px: f32,
    /// (clip_id, track_index, offset) of the clips following the primary one:
    /// the selection at the start of the drag, linked groups included.
    followers: Vec<(ClipId, usize, FrameIdx)>,
    /// Drag started with ALT: on release copies are inserted, the
    /// originals stay where they are.
    duplicate: bool,
}

/// Dragging the handle of a fade or of a single-edge transition: always
/// local to the single clip, no followers nor neighbors.
struct EdgeDragState {
    clip_id: ClipId,
    track_index: usize,
    edge: FadeEdge,
    /// Length (frames) of the fade or transition before the drag.
    original_value: FrameIdx,
    accum_px: f32,
}

/// Dragging the end of a crossing transition: unlike a single-edge
/// transition (`Gesture::TransitionLength`), it always touches both sides equally (see
/// `CrossTransition::split`) — here a `FadeEdge` is not needed, only knowing whether
/// the end being grabbed is the one inside the left clip or the one
/// inside the right clip, for the sign of the displacement.
struct CrossingDragState {
    track_index: usize,
    left_clip: ClipId,
    grabbed_left_side: bool,
    /// Total duration (frames) before the drag.
    original_duration: FrameIdx,
    /// It cannot exceed the duration of the two clips involved: computed once
    /// at the start of the drag, the clips do not change length in the
    /// meantime.
    max_duration: FrameIdx,
    accum_px: f32,
}

/// Vertical dragging of the volume line on an audio clip: like
/// a fade (`EdgeDragState`), always local to the single clip, never a multiple
/// selection. Unlike fade/trim, the gain is really applied (via
/// `PendingAction::SetGain`) on every drag frame instead of only on
/// release — the same chain of events as the properties panel slider,
/// so the waveform and the clip color follow live. `group` keeps
/// all those commits together in a single undo step (see
/// `History::begin_group`).
struct VolumeDragState {
    clip_id: ClipId,
    track_index: usize,
    /// Gain (dB) before the drag.
    original_db: f32,
    accum_px: f32,
    group: vv_core::GroupMark,
}

/// (clip_id, track_index, offset, edge) of a clip trimmed along with the primary one.
type TrimFollower = (ClipId, usize, FrameIdx, TrimEdge);

/// Trim of an edge, separate from an actual drag.
struct TrimState {
    clip_id: ClipId,
    track_index: usize,
    edge: TrimEdge,
    /// Original value (frames, timeline space) of the trimmed
    /// coordinate: `timeline_start` for `Start`, `timeline_end()` for `End`.
    original_value: FrameIdx,
    accum_px: f32,
    /// Valid range for the *new* value of `original_value`, already
    /// combined with that of all the `followers` (see
    /// `combined_trim_range`).
    min_value: FrameIdx,
    max_value: FrameIdx,
    /// The other clips trimmed together; in a roll, the neighbor too, with
    /// the opposite edge.
    followers: Vec<TrimFollower>,
    /// Roll edit between two adjacent clips (for the cursor only).
    roll: bool,
}

/// Dragging the right edge of a clip with the retime bar: `source_in` stays,
/// the length changes and the speed with it; ripple on release.
struct RetimeState {
    clip_id: ClipId,
    start: FrameIdx,
    original_len: FrameIdx,
    /// Length of the source range at 100%, exact: the speed for a dragged
    /// length is this over it, not a correction of the current (rounded) one.
    len_at_100: FrameIdx,
    pitch_correction: bool,
    accum_px: f32,
    /// The linked group gets the same speed.
    group: Vec<ClipKey>,
}

/// Slip drag: the pressed clip and its linked group slide their content by
/// the same amount.
struct SlipState {
    clip_id: ClipId,
    accum_px: f32,
    /// Each slipped clip with its own `slip_range`, for the overlay of the
    /// media left on each side.
    group: Vec<(ClipKey, FrameIdx, FrameIdx)>,
    /// `slip_range` shared by the whole group.
    min_delta: FrameIdx,
    max_delta: FrameIdx,
    /// Recomputed every frame from `accum_px`.
    delta: FrameIdx,
    /// The video clip of the group as it was at the press, for the viewer.
    video: Option<Box<Clip>>,
}

impl SlipState {
    /// Dragging right brings earlier media into the clip.
    fn delta_for(&self, px_per_frame: f32) -> FrameIdx {
        let frames = (self.accum_px / px_per_frame).round() as FrameIdx;
        (-frames).clamp(self.min_delta, self.max_delta)
    }
}

/// What the viewer shows during a slip: the first and last source frame the
/// video clip would play.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SlipPreview {
    pub media_id: vv_core::MediaId,
    pub first_frame: FrameIdx,
    pub last_frame: FrameIdx,
}

/// Speed limits of the retime drag and the dialog, in percent.
pub(crate) const SPEED_PERCENT_RANGE: std::ops::RangeInclusive<f64> = 1.0..=10_000.0;

impl RetimeState {
    /// Lengths reachable within `SPEED_PERCENT_RANGE`.
    fn len_range(&self) -> (FrameIdx, FrameIdx) {
        let len =
            |percent: f64| (self.len_at_100 as f64 * 100.0 / percent).round().max(1.0) as FrameIdx;
        (
            len(*SPEED_PERCENT_RANGE.end()),
            len(*SPEED_PERCENT_RANGE.start()),
        )
    }

    fn speed_for(&self, len: FrameIdx) -> vv_core::Rational {
        vv_core::Rational::new(self.len_at_100.max(1) as i32, 1)
            .divided_by(vv_core::Rational::new(len.max(1) as i32, 1))
    }
}

/// What a drag started near the edge of a clip does.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum EdgeZone {
    Trim(TrimEdge),
    /// On the contact point with the adjacent clip `neighbor`: `edge` is the
    /// edge of this clip, the neighbor moves with the opposite one.
    Roll {
        edge: TrimEdge,
        neighbor: ClipKey,
    },
}

impl EdgeZone {
    fn edge(self) -> TrimEdge {
        match self {
            EdgeZone::Trim(edge) | EdgeZone::Roll { edge, .. } => edge,
        }
    }
}

/// Distance (in screen pixels) from the edge of a clip within which a drag
/// starts as a trim instead of as a move; reduced for very
/// narrow clips, otherwise the whole clip would be "only edges".
const TRIM_HANDLE_PX: f32 = 8.0;
/// Half-width of the roll zone around the contact point between two
/// adjacent clips; the trim zone starts right after, towards the inside.
const ROLL_HANDLE_PX: f32 = 4.0;
/// Radius of the dot drawn for the fade-in/fade-out handle.
const FADE_HANDLE_RADIUS: f32 = 4.0;
/// Half-width of the clickable zone around the handle: wider than the
/// drawn dot, so it can be grabbed without aiming at the pixel.
const FADE_HANDLE_HIT_RADIUS: f32 = 9.0;
/// Band at the top of the clip reserved for the fade handles: below stays the
/// trim/roll of the edge, as in the other NLEs.
const FADE_HANDLE_ZONE_HEIGHT: f32 = 14.0;
/// Below this width the clip shows no fade handles: there would be no
/// room to grab them without colliding with the trim.
const MIN_FADE_CLIP_WIDTH_PX: f32 = 20.0;
/// Band at the bottom of the clip reserved for the transition marker: mirroring
/// the fade band at the top, so the two do not contend for the same hover.
const TRANSITION_HANDLE_ZONE_HEIGHT: f32 = 14.0;
/// Half-width of the clickable zone around the draggable end (duration)
/// of a transition, like `FADE_HANDLE_HIT_RADIUS`.
const TRANSITION_HANDLE_HIT_RADIUS: f32 = 9.0;
/// Within how many pixels of a clip edge a transition drop is
/// accepted; past that, releasing in the middle of the clip does nothing.
const TRANSITION_DROP_ZONE_PX: f32 = 40.0;
/// Color of a transition marker: also the highlight of the edge
/// during the drag, so the color anticipates what will appear on release.
const TRANSITION_COLOR: egui::Color32 = egui::Color32::from_rgb(120, 130, 235);
const TRANSITION_SELECTED_COLOR: egui::Color32 = egui::Color32::from_rgb(190, 197, 255);
/// Color of a crossing transition marker: a different hue from that
/// of a single edge, to signal at a glance that this one "eats"
/// the neighboring clip too instead of staying against transparency.
const CROSSING_COLOR: egui::Color32 = egui::Color32::from_rgb(230, 150, 90);
const CROSSING_SELECTED_COLOR: egui::Color32 = egui::Color32::from_rgb(255, 195, 150);
/// Vertical distance (px) within which the pointer grabs the volume
/// line of an audio clip.
const VOLUME_LINE_HIT_PX: f32 = 5.0;

/// At 0.1 px/s an hour fits in 360 px.
const MIN_PIXELS_PER_SEC: f32 = 0.1;
const MAX_PIXELS_PER_SEC: f32 = 800.0;

/// Below this speed the kinetic scroll stops instead of creeping
/// forever. Lower friction than egui's native one (1000 px/s²):
/// at that value the swipe was barely felt, here it glides longer.
const KINETIC_STOP_SPEED: f32 = 15.0; // px/s
const KINETIC_FRICTION: f32 = 500.0; // px/s^2
/// Amplifies the speed captured from the swipe: the native sensitivity
/// felt weak, a normal gesture barely set the inertia in motion.
pub(crate) const KINETIC_VELOCITY_GAIN: f32 = 1.6;

impl Default for TimelineState {
    fn default() -> Self {
        Self {
            tool: TimelineTool::default(),
            selected: BTreeSet::new(),
            can_record: false,
            record_needs_save: false,
            recording: None,
            selection_anchor: None,
            playhead: 0,
            pixels_per_sec: 60.0,
            row_height: ROW_HEIGHT,
            // Same initial value as `pixels_per_sec`: on the first frame there
            // is no zoom to compensate yet.
            last_rendered_pps: 60.0,
            gesture: None,
            selected_gap: None,
            selected_transition: None,
            clipboard: Vec::new(),
            video_pane_height: None,
            video_scroll: 0.0,
            audio_scroll: 0.0,
            hscroll_vel: 0.0,
            video_scroll_vel: 0.0,
            audio_scroll_vel: 0.0,
            export_marks: crate::transport::MarkRange::default(),
            paste_attributes_requested: false,
            reveal_playhead: false,
            retime_controls: BTreeSet::new(),
            speed_dialog_requested: None,
            silence_dialog_requested: None,
            silence_preview: Vec::new(),
            reveal_in_pool_requested: None,
            marker_drag: None,
            marker_editor: None,
            #[cfg(test)]
            clip_rects: std::collections::HashMap::new(),
            #[cfg(test)]
            marker_lane: egui::Rect::NOTHING,
        }
    }
}

impl TimelineState {
    /// A gesture whose undo group stays open across frames.
    pub fn holds_undo_group(&self) -> bool {
        matches!(self.gesture, Some(Gesture::Volume(_)))
    }

    fn moving(&self) -> Option<&DragState> {
        match &self.gesture {
            Some(Gesture::Move(d)) => Some(d),
            _ => None,
        }
    }

    fn trimming(&self) -> Option<&TrimState> {
        match &self.gesture {
            Some(Gesture::Trim(t)) => Some(t),
            _ => None,
        }
    }

    fn retiming(&self) -> Option<&RetimeState> {
        match &self.gesture {
            Some(Gesture::Retime(r)) => Some(r),
            _ => None,
        }
    }

    fn slipping(&self) -> Option<&SlipState> {
        match &self.gesture {
            Some(Gesture::Slip(s)) => Some(s),
            _ => None,
        }
    }

    pub fn slip_preview(&self) -> Option<SlipPreview> {
        let slip = self.slipping()?;
        let mut clip = *slip.video.clone()?;
        let ClipSource::Media(media_id) = clip.source else {
            return None;
        };
        clip.source_offset += slip.delta;
        Some(SlipPreview {
            media_id,
            first_frame: clip.source_in(),
            last_frame: clip.source_out() - 1,
        })
    }

    fn fading(&self) -> Option<&EdgeDragState> {
        match &self.gesture {
            Some(Gesture::Fade(d)) => Some(d),
            _ => None,
        }
    }

    fn sizing_transition(&self) -> Option<&EdgeDragState> {
        match &self.gesture {
            Some(Gesture::TransitionLength(d)) => Some(d),
            _ => None,
        }
    }

    fn sizing_crossing(&self) -> Option<&CrossingDragState> {
        match &self.gesture {
            Some(Gesture::CrossingLength(d)) => Some(d),
            _ => None,
        }
    }

    /// Sets selection and anchor from outside (e.g. after a cut).
    /// A drag on the timeline is in progress.
    pub fn gesture_active(&self) -> bool {
        self.gesture.is_some()
    }

    pub fn set_selection(&mut self, selected: BTreeSet<ClipKey>, anchor: Option<ClipKey>) {
        self.selected = selected;
        self.selection_anchor = anchor;
        self.selected_gap = None;
        self.selected_transition = None;
    }

    /// A single clip (or none), for "selection follows playhead".
    pub fn set_single_selection(&mut self, clip: Option<ClipKey>) {
        self.set_selection(clip.into_iter().collect(), clip);
    }

    /// Removes from the selection whatever is on locked tracks.
    pub fn drop_locked(&mut self, timeline: &vv_core::Timeline) {
        self.selected
            .retain(|&(track_index, _)| !timeline.is_locked(track_index));
        if self
            .selection_anchor
            .is_some_and(|(track_index, _)| timeline.is_locked(track_index))
        {
            self.selection_anchor = None;
        }
        if self
            .selected_gap
            .is_some_and(|(track_index, _, _)| timeline.is_locked(track_index))
        {
            self.selected_gap = None;
        }
        if self.selected_transition.is_some_and(|sel| {
            let track_index = match sel {
                TransitionSelection::Edge((track_index, _), _) => track_index,
                TransitionSelection::Crossing(track_index, _) => track_index,
            };
            timeline.is_locked(track_index)
        }) {
            self.selected_transition = None;
        }
    }

    /// Empties the selection (clips, gap and transition).
    pub fn clear_selection(&mut self) {
        self.selected.clear();
        self.selection_anchor = None;
        self.selected_gap = None;
        self.selected_transition = None;
    }

    /// Horizontal zoom anchored to the playhead.
    pub fn zoom_in(&mut self) {
        self.set_pixels_per_sec(self.pixels_per_sec * ZOOM_STEP);
    }

    pub fn zoom_out(&mut self) {
        self.set_pixels_per_sec(self.pixels_per_sec / ZOOM_STEP);
    }

    fn set_pixels_per_sec(&mut self, value: f32) {
        self.pixels_per_sec = value.clamp(MIN_PIXELS_PER_SEC, MAX_PIXELS_PER_SEC);
    }

    fn set_row_height(&mut self, value: f32) {
        self.row_height = value.clamp(ROW_HEIGHT, MAX_ROW_HEIGHT);
    }
}

const RETIME_BAR_HEIGHT: f32 = 14.0;

/// Zoom factor per step of `zoom_in`/`zoom_out`.
const ZOOM_STEP: f32 = 1.25;

struct ClipVisual<'a> {
    track_index: usize,
    /// Borrowed from the project; `Owned` only in the tests.
    clip: std::borrow::Cow<'a, Clip>,
    label: String,
    color: egui::Color32,
    /// The track is locked: the clip is untouchable.
    locked: bool,
    /// Excluded from the output: it or its video track is disabled.
    muted: bool,
}

/// Command collected during the drawing (which borrows `project`) and
/// applied afterwards.
enum PendingAction {
    Marker(markers::MarkerChange),
    /// Moves a dragged group. The new tracks must be created before
    /// resolving the `EffectiveTrack::New` of `moves`.
    Move {
        new_video_tracks: usize,
        new_audio_tracks: usize,
        moves: Vec<(ClipId, usize, EffectiveTrack, FrameIdx)>,
        duplicate: bool,
    },
    /// (clip_id, track, edge, new position) for every clip, plus the stretches
    /// they take by lengthening: whatever was there gets overwritten.
    Trim {
        trims: Vec<(ClipId, usize, TrimEdge, FrameIdx)>,
        overwritten: Vec<(usize, FrameIdx, FrameIdx)>,
    },
    /// New duration (in frames) of the fade in or out.
    SetFade {
        track_index: usize,
        clip_id: ClipId,
        edge: FadeEdge,
        new_value: FrameIdx,
    },
    /// New constant gain (dB), from dragging the volume line on the timeline.
    SetGain {
        track_index: usize,
        clip_id: ClipId,
        new_value: f32,
    },
    /// A filter of the Effects panel was dropped on this clip:
    /// appended to its list (or re-enabled if already present),
    /// active by default.
    ApplyFilter {
        track_index: usize,
        clip_id: ClipId,
        filter: FilterEntry,
    },
    /// A transition of the Effects panel was dropped near an
    /// edge of this clip: it replaces the one already present on that edge
    /// (dropping again shortens/restores the default duration), never appended to
    /// a list like the filters — an edge has at most one.
    ApplyTransition {
        track_index: usize,
        clip_id: ClipId,
        edge: FadeEdge,
        kind: vv_core::TransitionKind,
    },
    /// New duration (in frames) of the transition of an edge, from dragging
    /// its end on the timeline.
    SetTransitionDuration {
        track_index: usize,
        clip_id: ClipId,
        edge: FadeEdge,
        new_value: FrameIdx,
    },
    /// An existing transition was duplicated (Alt+drag from its
    /// body) and dropped near an edge: unlike
    /// `ApplyTransition`, it does not start from the defaults — it keeps the same
    /// parameters as the original.
    DuplicateTransition {
        track_index: usize,
        clip_id: ClipId,
        edge: FadeEdge,
        transition: vv_core::Transition,
    },
    /// New duration (in frames) of a crossing transition, from the symmetric
    /// drag of its end — see `CrossingDragState`.
    SetCrossingDuration {
        track_index: usize,
        left_clip: ClipId,
        new_value: FrameIdx,
    },
    Unlink(usize, ClipId),
    /// Links all the listed clips (track_index, clip_id) into a single
    /// new group — at least 2.
    Link(Vec<ClipKey>),
    /// Removes the track at this index (and its clips).
    RemoveTrack(usize),
    AddTrack(TrackKind),
    /// Arming for recording asked on a project with no folder yet.
    ArmNeedsSave,
    SetTrackFlag(usize, TrackFlag, bool),
    /// Replaces the listed clips with a compound clip (see
    /// `make_compound_clip`).
    MakeCompound(Vec<ClipKey>),
    /// Timeline color of the listed clips; `None` goes back to the default one.
    SetDisplayColor(Vec<ClipKey>, Option<vv_core::ClipColor>),
    /// Constant speed of the listed clips (see `vv_core::SetClipSpeed`).
    /// Slides the content of the listed clips (see `vv_core::SlipClip`).
    Slip {
        clips: Vec<ClipKey>,
        delta: FrameIdx,
    },
    /// See `vv_core::SetClipFreeze`.
    SetFreeze {
        clips: Vec<ClipKey>,
        at: Option<FrameIdx>,
    },
    SetSpeed {
        clips: Vec<ClipKey>,
        speed: vv_core::Rational,
        pitch_correction: bool,
        /// A dragged end: from the old end to the released one, exactly.
        resize_to: Option<(FrameIdx, FrameIdx)>,
    },
}

#[derive(Clone, Copy)]
struct TrackFlags {
    muted: bool,
    solo: bool,
    locked: bool,
    armed: bool,
}

/// Drawing order: Video from the highest index (the new one is at the top),
/// then Audio in order. Independent of the order in `tracks`.
fn track_row_order(track_kinds: &[TrackKind]) -> Vec<usize> {
    let mut order: Vec<usize> = (0..track_kinds.len())
        .filter(|&i| track_kinds[i] == TrackKind::Video)
        .collect();
    order.reverse();
    order.extend((0..track_kinds.len()).filter(|&i| track_kinds[i] == TrackKind::Audio));
    order
}

/// Vertical geometry (`y` local to the content) of the two boxes, Video
/// above and Audio below the separator, each with its own scroll.
#[derive(Clone, Copy, Debug)]
struct PaneLayout {
    video_pane: egui::Rangef,
    audio_pane: egui::Rangef,
    video_rows_top: f32,
    audio_rows_top: f32,
    video_count: usize,
    audio_count: usize,
    divider_height: f32,
    video_max_scroll: f32,
    audio_max_scroll: f32,
    /// Limits of the Video box height when dragging the separator.
    video_height_range: egui::Rangef,
    row_height: f32,
}

impl PaneLayout {
    /// It also clamps the scrolls in `state` to the current limits.
    fn new(
        avail_below_ruler: f32,
        video_count: usize,
        audio_count: usize,
        state: &mut TimelineState,
    ) -> Self {
        let divider_height = if video_count > 0 && audio_count > 0 {
            GROUP_DIVIDER_HEIGHT
        } else {
            0.0
        };
        let rows_avail = (avail_below_ruler - divider_height).max(0.0);
        let row_height = state.row_height;
        let video_rows = video_count as f32 * row_height;
        let audio_rows = audio_count as f32 * row_height;
        let default_video_height = if video_rows + audio_rows <= rows_avail {
            (rows_avail - video_rows - audio_rows) / 2.0 + video_rows
        } else {
            rows_avail * video_count as f32 / (video_count + audio_count) as f32
        };
        let min_height = if divider_height > 0.0 {
            MIN_PANE_HEIGHT.min(rows_avail / 2.0)
        } else {
            0.0
        };
        let video_height_range = egui::Rangef::new(min_height, rows_avail - min_height);
        let video_height = if divider_height > 0.0 {
            state
                .video_pane_height
                .unwrap_or(default_video_height)
                .clamp(video_height_range.min, video_height_range.max)
        } else {
            default_video_height
        };
        let audio_height = rows_avail - video_height;

        let max_scroll = |rows: f32, pane: f32| {
            if rows > pane {
                rows + NEW_TRACK_ZONE_HEIGHT - pane
            } else {
                0.0
            }
        };
        let video_max_scroll = max_scroll(video_rows, video_height);
        let audio_max_scroll = max_scroll(audio_rows, audio_height);
        state.video_scroll = state.video_scroll.clamp(0.0, video_max_scroll);
        state.audio_scroll = state.audio_scroll.clamp(0.0, audio_max_scroll);

        let video_bottom = RULER_HEIGHT + video_height;
        Self {
            video_pane: egui::Rangef::new(RULER_HEIGHT, video_bottom),
            audio_pane: egui::Rangef::new(
                video_bottom + divider_height,
                RULER_HEIGHT + avail_below_ruler.max(divider_height),
            ),
            video_rows_top: video_bottom - video_rows + state.video_scroll,
            audio_rows_top: video_bottom + divider_height - state.audio_scroll,
            video_count,
            audio_count,
            divider_height,
            video_max_scroll,
            audio_max_scroll,
            video_height_range,
            row_height,
        }
    }

    fn video_height(&self) -> f32 {
        self.video_pane.span()
    }

    fn video_rows_bottom(&self) -> f32 {
        self.video_rows_top + self.video_count as f32 * self.row_height
    }

    fn audio_rows_bottom(&self) -> f32 {
        self.audio_rows_top + self.audio_count as f32 * self.row_height
    }

    fn pane(&self, kind: TrackKind) -> egui::Rangef {
        match kind {
            TrackKind::Video => self.video_pane,
            TrackKind::Audio => self.audio_pane,
        }
    }

    /// `y` of a row of `track_row_order`.
    fn row_y(&self, row: usize) -> f32 {
        if row < self.video_count {
            self.video_rows_top + row as f32 * self.row_height
        } else {
            self.audio_rows_top + (row - self.video_count) as f32 * self.row_height
        }
    }

    /// Row (of `track_row_order`) nearest to `y`, inside the box
    /// containing `y`.
    fn row_at_y(&self, y: f32) -> usize {
        let in_video = self.video_count > 0 && (self.audio_count == 0 || y < self.audio_pane.min);
        if in_video {
            let row = ((y - self.video_rows_top) / self.row_height)
                .floor()
                .max(0.0) as usize;
            row.min(self.video_count - 1)
        } else {
            let row = ((y - self.audio_rows_top) / self.row_height)
                .floor()
                .max(0.0) as usize;
            self.video_count + row.min(self.audio_count.saturating_sub(1))
        }
    }

    /// `y` over a visible track (not in an empty zone nor hidden
    /// by the scroll).
    fn is_over_rows(&self, y: f32) -> bool {
        (self.video_pane.contains(y) && y >= self.video_rows_top && y < self.video_rows_bottom())
            || (self.audio_pane.contains(y)
                && y >= self.audio_rows_top
                && y < self.audio_rows_bottom())
    }
}

enum TrackDragTarget {
    Track(usize),
    NewTrack,
}

/// Candidate track of a drag: an existing one, or `New(depth)` to create on
/// release (`depth` 1-based: a follower may need several new
/// tracks to keep the spacing of the group).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EffectiveTrack {
    Existing(usize),
    New(usize),
}

/// Where a clip of kind `kind` dragged to `local_y` would land: a track or
/// a new one (zone above the Video ones / below the Audio ones). `None` in the box
/// of the other kind or on the separator.
fn track_drag_target(
    local_y: f32,
    kind: TrackKind,
    row_order: &[usize],
    layout: &PaneLayout,
) -> Option<TrackDragTarget> {
    match kind {
        TrackKind::Video => {
            if local_y >= layout.video_pane.max {
                return None;
            }
            let y = local_y.max(layout.video_pane.min);
            if y < layout.video_rows_top {
                return Some(TrackDragTarget::NewTrack);
            }
            if y < layout.video_rows_bottom() {
                return row_order
                    .get(layout.row_at_y(y))
                    .copied()
                    .map(TrackDragTarget::Track);
            }
            None
        }
        TrackKind::Audio => {
            if local_y < layout.audio_pane.min {
                return None;
            }
            let y = local_y.min(layout.audio_pane.max - 1.0);
            if y >= layout.audio_rows_bottom() {
                return Some(TrackDragTarget::NewTrack);
            }
            if y >= layout.audio_rows_top {
                return row_order
                    .get(layout.row_at_y(y))
                    .copied()
                    .map(TrackDragTarget::Track);
            }
            None
        }
    }
}

/// Drop on an existing track: if video is dragged onto a video
/// track it lands there, otherwise on the usual track. `None` if it is
/// locked.
fn media_pool_drop_target(
    track: usize,
    has_video: bool,
    track_kind: TrackKind,
    locked: bool,
) -> Option<MediaDropTarget> {
    if locked {
        None
    } else if has_video && track_kind == TrackKind::Video {
        Some(MediaDropTarget::Track(track))
    } else {
        Some(MediaDropTarget::Default)
    }
}

/// Target track of each clip of a dragged group: the followers
/// move by the same number of rows as the primary one (in the opposite direction if
/// of a different kind: video and audio grow in opposite directions). Past
/// the last track of their own kind they become `New(depth)`, in the opposite
/// direction they stop at the nearest one.
fn drag_group_row_targets(
    primary_id: ClipId,
    primary_track: usize,
    primary_target: EffectiveTrack,
    followers: &[(ClipId, usize, FrameIdx)],
    track_kinds: &[TrackKind],
    row_of_track: &[usize],
    row_order: &[usize],
    video_count: usize,
    track_count: usize,
) -> Vec<(ClipId, EffectiveTrack)> {
    let primary_kind = track_kinds[primary_track];
    let primary_original_row = row_of_track[primary_track] as isize;
    let primary_target_row = match primary_target {
        EffectiveTrack::Existing(track) => row_of_track[track] as isize,
        EffectiveTrack::New(_) => match primary_kind {
            TrackKind::Video => -1,
            TrackKind::Audio => track_count as isize,
        },
    };
    let delta_row = primary_target_row - primary_original_row;

    let mut targets = vec![(primary_id, primary_target)];
    for &(follower_id, follower_track, _) in followers {
        let follower_kind = track_kinds[follower_track];
        let signed_delta = if follower_kind == primary_kind {
            delta_row
        } else {
            -delta_row
        };
        let candidate_row = row_of_track[follower_track] as isize + signed_delta;
        let target = match follower_kind {
            TrackKind::Video if candidate_row < 0 => EffectiveTrack::New((-candidate_row) as usize),
            TrackKind::Audio if candidate_row > track_count as isize - 1 => {
                EffectiveTrack::New((candidate_row - (track_count as isize - 1)) as usize)
            }
            TrackKind::Video => EffectiveTrack::Existing(
                row_order[candidate_row.min(video_count as isize - 1) as usize],
            ),
            TrackKind::Audio => EffectiveTrack::Existing(
                row_order[candidate_row.max(video_count as isize) as usize],
            ),
        };
        targets.push((follower_id, target));
    }
    targets
}

/// Fixed column of the headers. Drawn by hand: an `add_space`
/// tied to the height would make the panel grow indefinitely.
fn draw_track_headers(
    ui: &mut egui::Ui,
    track_kinds: &[TrackKind],
    track_labels: &[String],
    track_flags: &[TrackFlags],
    row_order: &[usize],
    row_y: &[f32],
    layout: &PaneLayout,
    video_pane_height: &mut Option<f32>,
    pending: &mut Option<PendingAction>,
    playhead: FrameIdx,
    fps: f64,
    can_record: bool,
) {
    let (rect, _resp) = ui.allocate_exact_size(
        egui::vec2(TRACK_HEADER_WIDTH, RULER_HEIGHT),
        egui::Sense::hover(),
    );
    let origin = rect.min;
    let full_clip = ui.clip_rect();
    let text_color = ui.visuals().text_color();

    // Timestamp of the playhead position in HH:MM:SS:FF format, in the
    // ruler row.
    let playhead_secs = playhead as f64 / fps;
    ui.painter().text(
        egui::pos2(
            origin.x + TRACK_HEADER_WIDTH / 2.0,
            origin.y + RULER_HEIGHT / 2.0,
        ),
        egui::Align2::CENTER_CENTER,
        format_timecode(playhead_secs, fps),
        egui::FontId::monospace(14.0),
        egui::Color32::WHITE,
    );

    for &track_index in row_order {
        let kind = track_kinds[track_index];
        let pane = layout.pane(kind);
        ui.set_clip_rect(full_clip.intersect(egui::Rect::from_x_y_ranges(
            rect.x_range(),
            (origin.y + pane.min)..=(origin.y + pane.max),
        )));
        let row_rect = egui::Rect::from_min_size(
            egui::pos2(origin.x, origin.y + row_y[track_index]),
            egui::vec2(TRACK_HEADER_WIDTH, layout.row_height),
        );
        ui.painter().text(
            row_rect.left_center() + egui::vec2(6.0, 0.0),
            egui::Align2::LEFT_CENTER,
            &track_labels[track_index],
            egui::FontId::proportional(14.0),
            text_color,
        );

        let flags = track_flags[track_index];
        let toggle = |x: f32, id: &str, hover: &str, paint: &dyn Fn(&egui::Painter, egui::Rect)| {
            let rect = egui::Rect::from_center_size(
                egui::pos2(row_rect.left() + x, row_rect.center().y),
                egui::vec2(20.0, 20.0),
            );
            let resp = ui
                .interact(
                    rect,
                    ui.id().with(id).with(track_index),
                    egui::Sense::click(),
                )
                .on_hover_text(hover);
            if resp.hovered() {
                ui.painter()
                    .rect_filled(rect, 3.0, egui::Color32::from_gray(60));
            }
            paint(ui.painter(), rect);
            resp.clicked()
        };
        if toggle(38.0, "lock_track", &t!("timeline.lock_track"), &|p, r| {
            paint_lock_icon(p, r, flags.locked)
        }) {
            *pending = Some(PendingAction::SetTrackFlag(
                track_index,
                TrackFlag::Locked,
                !flags.locked,
            ));
        }
        match kind {
            TrackKind::Video => {
                if toggle(
                    60.0,
                    "mute_track",
                    &t!("timeline.disable_video_track"),
                    &|p, r| paint_film_icon(p, r, !flags.muted),
                ) {
                    *pending = Some(PendingAction::SetTrackFlag(
                        track_index,
                        TrackFlag::Muted,
                        !flags.muted,
                    ));
                }
            }
            TrackKind::Audio => {
                let solo_color = crate::theme::ACCENT;
                if toggle(60.0, "solo_track", &t!("timeline.solo"), &|p, r| {
                    paint_letter_button(p, r, "S", flags.solo.then_some(solo_color))
                }) {
                    *pending = Some(PendingAction::SetTrackFlag(
                        track_index,
                        TrackFlag::Solo,
                        !flags.solo,
                    ));
                }
                let mute_color = egui::Color32::from_rgb(120, 150, 190);
                if toggle(82.0, "mute_track", &t!("timeline.mute"), &|p, r| {
                    paint_letter_button(p, r, "M", flags.muted.then_some(mute_color))
                }) {
                    *pending = Some(PendingAction::SetTrackFlag(
                        track_index,
                        TrackFlag::Muted,
                        !flags.muted,
                    ));
                }
                let arm_color = crate::theme::ERROR;
                if toggle(104.0, "arm_track", &t!("timeline.arm"), &|p, r| {
                    paint_letter_button(p, r, "R", flags.armed.then_some(arm_color))
                }) {
                    *pending = Some(if flags.armed || can_record {
                        PendingAction::SetTrackFlag(track_index, TrackFlag::Armed, !flags.armed)
                    } else {
                        PendingAction::ArmNeedsSave
                    });
                }
            }
        }

        let is_last_of_kind = track_kinds.iter().filter(|k| **k == kind).count() <= 1;
        const REMOVE_BTN_SIZE: f32 = 18.0;
        let remove_rect = egui::Rect::from_center_size(
            egui::pos2(row_rect.right() - 14.0, row_rect.center().y),
            egui::vec2(REMOVE_BTN_SIZE, REMOVE_BTN_SIZE),
        );
        let sense = if is_last_of_kind {
            egui::Sense::hover()
        } else {
            egui::Sense::click()
        };
        let remove_resp = ui
            .interact(
                remove_rect,
                ui.id().with("remove_track").with(track_index),
                sense,
            )
            .on_hover_text(if is_last_of_kind {
                t!("timeline.cannot_remove_last_track")
            } else {
                t!("timeline.remove_track")
            });
        if !is_last_of_kind && remove_resp.hovered() {
            ui.painter()
                .rect_filled(remove_rect, 3.0, egui::Color32::from_gray(70));
        }
        ui.painter().text(
            remove_rect.center(),
            egui::Align2::CENTER_CENTER,
            "×",
            egui::FontId::proportional(14.0),
            if is_last_of_kind {
                egui::Color32::from_gray(90)
            } else {
                text_color
            },
        );
        if remove_resp.clicked() {
            *pending = Some(PendingAction::RemoveTrack(track_index));
        }
    }

    // The empty part of each box, above the video rows and below the audio
    // ones: right click adds a track there.
    let empty_parts = [
        (
            TrackKind::Video,
            layout.video_pane,
            layout.video_pane.min,
            layout.video_rows_top,
        ),
        (
            TrackKind::Audio,
            layout.audio_pane,
            layout.audio_rows_bottom(),
            layout.audio_pane.max,
        ),
    ];
    for (kind, pane, top, bottom) in empty_parts {
        let (top, bottom) = (top.max(pane.min), bottom.min(pane.max));
        if bottom - top < 1.0 {
            continue;
        }
        let empty =
            egui::Rect::from_x_y_ranges(rect.x_range(), (origin.y + top)..=(origin.y + bottom));
        ui.set_clip_rect(full_clip.intersect(empty));
        let resp = ui.interact(
            empty,
            ui.id().with(("header_empty", kind == TrackKind::Video)),
            egui::Sense::click(),
        );
        resp.context_menu(|ui| {
            let label = match kind {
                TrackKind::Video => t!("timeline.add_video_track"),
                TrackKind::Audio => t!("timeline.add_audio_track"),
            };
            if ui.button(label).clicked() {
                *pending = Some(PendingAction::AddTrack(kind));
            }
        });
    }

    ui.set_clip_rect(full_clip);

    if layout.divider_height > 0.0 {
        let divider_rect = egui::Rect::from_min_size(
            egui::pos2(origin.x, origin.y + layout.video_pane.max),
            egui::vec2(TRACK_HEADER_WIDTH, layout.divider_height),
        );
        interact_divider(
            ui,
            ui.painter(),
            divider_rect,
            ui.id().with("timeline_header_track_split"),
            layout,
            video_pane_height,
        );
    }
}

/// Draggable Video/Audio separator: present both in the header column
/// and in the scrollable area, with shared state.
fn interact_divider(
    ui: &egui::Ui,
    painter: &egui::Painter,
    rect: egui::Rect,
    id: egui::Id,
    layout: &PaneLayout,
    video_pane_height: &mut Option<f32>,
) {
    let resp = ui.interact(rect, id, egui::Sense::drag());
    let active = resp.hovered() || resp.dragged();
    if active {
        ui.ctx()
            .output_mut(|o| o.cursor_icon = egui::CursorIcon::ResizeVertical);
    }
    if resp.dragged() {
        let range = layout.video_height_range;
        *video_pane_height =
            Some((layout.video_height() + resp.drag_delta().y).clamp(range.min, range.max));
    }
    painter.hline(
        rect.x_range(),
        rect.center().y,
        egui::Stroke::new(1.0, egui::Color32::from_gray(if active { 160 } else { 80 })),
    );
}

/// Vertical scrollbar of a box; `offset` measured from the top.
/// Returns the new offset if the user drags it.
fn pane_scrollbar(
    ui: &egui::Ui,
    painter: &egui::Painter,
    track_rect: egui::Rect,
    id: egui::Id,
    offset: f32,
    max_offset: f32,
) -> Option<f32> {
    if max_offset <= 0.0 || track_rect.height() <= 0.0 {
        return None;
    }
    let content_height = track_rect.height() + max_offset;
    let thumb_height = (track_rect.height() * track_rect.height() / content_height)
        .max(16.0)
        .min(track_rect.height());
    let travel = track_rect.height() - thumb_height;
    let thumb_top = track_rect.top() + travel * offset / max_offset;
    let thumb_rect = egui::Rect::from_min_size(
        egui::pos2(track_rect.left(), thumb_top),
        egui::vec2(track_rect.width(), thumb_height),
    );
    let resp = ui.interact(track_rect, id, egui::Sense::click_and_drag());
    let active = resp.hovered() || resp.dragged();
    painter.rect_filled(track_rect, 4.0, egui::Color32::from_black_alpha(90));
    painter.rect_filled(
        thumb_rect.shrink2(egui::vec2(1.0, 1.0)),
        4.0,
        egui::Color32::from_gray(if active { 170 } else { 120 }),
    );
    if resp.dragged() && travel > 0.0 {
        return Some((offset + resp.drag_delta().y * max_offset / travel).clamp(0.0, max_offset));
    }
    if resp.clicked()
        && let Some(pos) = resp.interact_pointer_pos()
        && travel > 0.0
    {
        let target = (pos.y - track_rect.top() - thumb_height / 2.0) / travel * max_offset;
        return Some(target.clamp(0.0, max_offset));
    }
    None
}

fn paint_lock_icon(painter: &egui::Painter, rect: egui::Rect, locked: bool) {
    let color = if locked {
        egui::Color32::from_gray(235)
    } else {
        egui::Color32::from_gray(110)
    };
    let c = rect.center();
    let body = egui::Rect::from_min_max(c + egui::vec2(-5.0, -1.0), c + egui::vec2(5.0, 6.0));
    painter.rect_filled(body, 1.5, color);
    // When open, the right leg of the arc does not reach the body.
    let right_leg_end = if locked { -1.0 } else { -4.0 };
    let mut points = vec![c + egui::vec2(-3.5, -1.0), c + egui::vec2(-3.5, -3.5)];
    points.extend((0..=8).map(|i| {
        let a = std::f32::consts::PI * (1.0 + i as f32 / 8.0);
        c + egui::vec2(3.5 * a.cos(), -3.5 + 3.5 * a.sin())
    }));
    points.push(c + egui::vec2(3.5, right_leg_end));
    painter.add(egui::Shape::line(points, egui::Stroke::new(1.6, color)));
}

/// Film strip; crossed out if the track is disabled.
fn paint_film_icon(painter: &egui::Painter, rect: egui::Rect, enabled: bool) {
    let color = egui::Color32::from_gray(if enabled { 200 } else { 100 });
    let film = egui::Rect::from_center_size(rect.center(), egui::vec2(14.0, 11.0));
    painter.rect_stroke(
        film,
        1.0,
        egui::Stroke::new(1.3, color),
        egui::StrokeKind::Inside,
    );
    for i in 0..4 {
        let x = film.left() + 2.5 + i as f32 * 3.0;
        for y in [film.top() + 2.0, film.bottom() - 2.0] {
            painter.rect_filled(
                egui::Rect::from_center_size(egui::pos2(x, y), egui::vec2(1.4, 1.4)),
                0.0,
                color,
            );
        }
    }
    if !enabled {
        painter.line_segment(
            [
                film.left_bottom() + egui::vec2(-1.0, 1.0),
                film.right_top() + egui::vec2(1.0, -1.0),
            ],
            egui::Stroke::new(1.6, crate::theme::ACCENT),
        );
    }
}

/// Hand-drawn gear (ring + radial teeth): no Unicode glyph, which
/// on some platforms (Asahi) is missing from egui's fonts (see the comment
/// on the link icon in `paint_clip_overlay`).
pub(crate) fn paint_gear_icon(
    painter: &egui::Painter,
    center: egui::Pos2,
    radius: f32,
    color: egui::Color32,
) {
    let stroke = egui::Stroke::new(1.6, color);
    painter.circle_stroke(center, radius * 0.55, stroke);
    painter.circle_filled(center, radius * 0.16, color);
    const TEETH: usize = 8;
    for i in 0..TEETH {
        let angle = std::f32::consts::TAU * i as f32 / TEETH as f32;
        let dir = egui::vec2(angle.cos(), angle.sin());
        painter.line_segment(
            [center + dir * radius * 0.55, center + dir * radius],
            stroke,
        );
    }
}

/// "S"/"M" button: filled with `active` when it is on.
fn paint_letter_button(
    painter: &egui::Painter,
    rect: egui::Rect,
    letter: &str,
    active: Option<egui::Color32>,
) {
    let button = rect.shrink(2.0);
    let text_color = match active {
        Some(fill) => {
            painter.rect_filled(button, 3.0, fill);
            egui::Color32::BLACK
        }
        None => {
            painter.rect_stroke(
                button,
                3.0,
                egui::Stroke::new(1.0, egui::Color32::from_gray(90)),
                egui::StrokeKind::Inside,
            );
            egui::Color32::from_gray(150)
        }
    };
    painter.text(
        button.center(),
        egui::Align2::CENTER_CENTER,
        letter,
        egui::FontId::proportional(11.0),
        text_color,
    );
}

/// Interval between major ticks from the 1-2-5 sequence, the first one that
/// keeps them at least `MIN_MAJOR_TICK_PX` apart.
fn nice_tick_interval_secs(pixels_per_sec: f32) -> f64 {
    const MIN_MAJOR_TICK_PX: f32 = 70.0;
    const CANDIDATES: &[f64] = &[
        1.0, 2.0, 5.0, 10.0, 15.0, 30.0, 60.0, 120.0, 300.0, 600.0, 900.0, 1800.0, 3600.0, 7200.0,
        14400.0,
    ];
    CANDIDATES
        .iter()
        .copied()
        .find(|&c| c as f32 * pixels_per_sec >= MIN_MAJOR_TICK_PX)
        .unwrap_or(*CANDIDATES.last().unwrap())
}

/// `true` if the pointer is on the rectangle *and* nothing is above it: a
/// floating window (the keyframe editor) keeps its own scroll,
/// the timeline underneath must not react.
pub(crate) fn pointer_over(ctx: &egui::Context, rect: egui::Rect) -> bool {
    ctx.input(|i| i.pointer.hover_pos()).is_some_and(|p| {
        rect.contains(p)
            && ctx
                .layer_id_at(p)
                .is_none_or(|l| l.order == egui::Order::Background)
    })
}

/// Non-drop-frame HH:MM:SS:FF timecode: with non-integer fps (29.97) the
/// seconds are counted on the rounded nominal fps, as in NLEs.
pub(crate) fn format_timecode(total_secs: f64, fps: f64) -> String {
    let nominal = (fps.round() as i64).max(1);
    let frame = (total_secs.max(0.0) * fps).round() as i64;
    let (h, m) = (frame / (nominal * 3600), frame / (nominal * 60) % 60);
    let (s, f) = (frame / nominal % 60, frame % nominal);
    format!("{h:02}:{m:02}:{s:02}:{f:02}")
}

/// Ticks on three levels: major ones with timecode, medium ones every N frames,
/// one per frame; a level denser than `MIN_TICK_SPACING_PX` is not
/// drawn.
fn draw_ruler_ticks(
    painter: &egui::Painter,
    origin: egui::Pos2,
    visible_x: egui::Rect,
    pixels_per_sec: f32,
    fps: f64,
) {
    if !visible_x.is_positive() {
        return; // ruler completely outside the scrolled viewport
    }

    const MIN_TICK_SPACING_PX: f32 = 8.0;
    let tick_color = egui::Color32::from_gray(110);
    let label_color = egui::Color32::from_gray(200);
    let minor_color = egui::Color32::from_gray(70);
    let medium_color = egui::Color32::from_gray(90);

    // Heights of the three tick levels (from bottom to top).
    const FRAME_TICK_HEIGHT: f32 = 5.0;
    const MEDIUM_TICK_HEIGHT: f32 = 10.0;
    const MAJOR_TICK_HEIGHT: f32 = RULER_TICKS_HEIGHT;

    // Range in seconds actually visible, not the whole duration of the
    // timeline (thousands of off-screen ticks otherwise).
    let visible_start_secs = ((visible_x.min.x - origin.x) / pixels_per_sec.max(1e-6)) as f64;
    let visible_end_secs = ((visible_x.max.x - origin.x) / pixels_per_sec.max(1e-6)) as f64;

    // Level 2: major ticks with an HH:MM:SS:FF label, adaptive
    // "clean" interval (1-2-5 sequence) — always visible.
    let major_secs = nice_tick_interval_secs(pixels_per_sec);
    let first_major = (visible_start_secs / major_secs).floor() as i64;
    let last_major = (visible_end_secs / major_secs).ceil() as i64;
    for i in first_major..=last_major {
        let secs = i as f64 * major_secs;
        if secs < 0.0 {
            continue;
        }
        let x = origin.x + (secs * pixels_per_sec as f64) as f32;
        painter.line_segment(
            [
                egui::pos2(x, origin.y),
                egui::pos2(x, origin.y + MAJOR_TICK_HEIGHT),
            ],
            egui::Stroke::new(1.0, tick_color),
        );
        painter.text(
            egui::pos2(x + 3.0, origin.y + 2.0),
            egui::Align2::LEFT_TOP,
            format_timecode(secs, fps),
            egui::FontId::proportional(10.0),
            label_color,
        );
    }

    // Medium ticks, only if denser than the major ones.
    let px_per_frame = pixels_per_sec / fps.max(1e-9) as f32;

    // Computes the interval in frames for the medium ticks: the smallest
    // "clean" multiple (1, 2, 5, 10, 25, 50...) that keeps the ticks at
    // least MIN_TICK_SPACING_PX apart.
    let medium_interval_frames = {
        const MEDIUM_CANDIDATES: &[i64] = &[1, 2, 5, 10, 25, 50, 100, 250, 500];
        MEDIUM_CANDIDATES
            .iter()
            .copied()
            .find(|&c| c as f32 * px_per_frame >= MIN_TICK_SPACING_PX)
            .unwrap_or(*MEDIUM_CANDIDATES.last().unwrap())
    };

    // The medium ticks are useful only if they are closer than the major ones and
    // do not overlap them exactly (otherwise they would be redundant).
    let major_interval_frames = (major_secs * fps) as i64;
    if medium_interval_frames < major_interval_frames {
        let first_frame = (visible_start_secs * fps).floor().max(0.0) as i64;
        let last_frame = (visible_end_secs * fps).ceil().max(0.0) as i64;
        for frame in (first_frame..=last_frame).step_by(medium_interval_frames as usize) {
            // Skips the positions where there is already a major tick (redundant).
            let secs = frame as f64 / fps;
            let major_at_this_pos = ((secs / major_secs).round() * major_secs - secs).abs() < 1e-9;
            if major_at_this_pos {
                continue;
            }
            let x = origin.x + frame as f32 * px_per_frame;
            painter.line_segment(
                [
                    egui::pos2(x, origin.y + RULER_TICKS_HEIGHT - MEDIUM_TICK_HEIGHT),
                    egui::pos2(x, origin.y + RULER_TICKS_HEIGHT),
                ],
                egui::Stroke::new(1.0, medium_color),
            );
        }
    }

    // Level 0: ticks for every single frame — the shortest ones, visible only
    // when the zoom is high enough not to make them touch.
    if px_per_frame >= MIN_TICK_SPACING_PX {
        let first_frame = (visible_start_secs * fps).floor().max(0.0) as i64;
        let last_frame = (visible_end_secs * fps).ceil().max(0.0) as i64;
        for frame in first_frame..=last_frame {
            // Skips the positions where there is already a medium or major tick.
            let is_medium_pos = frame % medium_interval_frames == 0;
            let secs = frame as f64 / fps;
            let is_major_pos = ((secs / major_secs).round() * major_secs - secs).abs() < 1e-9;
            if is_medium_pos || is_major_pos {
                continue;
            }
            let x = origin.x + frame as f32 * px_per_frame;
            painter.line_segment(
                [
                    egui::pos2(x, origin.y + RULER_TICKS_HEIGHT - FRAME_TICK_HEIGHT),
                    egui::pos2(x, origin.y + RULER_TICKS_HEIGHT),
                ],
                egui::Stroke::new(1.0, minor_color),
            );
        }
    }
}

/// Payload of a media drag&drop towards the timeline: from the media pool
/// (the whole media) or from the viewer (the portion between the in/out markers).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MediaDrag {
    pub media_id: vv_core::MediaId,
    pub source_in: FrameIdx,
    pub source_out: FrameIdx,
    pub streams: DragStreams,
}

/// Which streams of the media end up on the timeline.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DragStreams {
    #[default]
    All,
    VideoOnly,
    AudioOnly,
}

impl MediaDrag {
    /// Initial duration of an image from the pool (its `duration_frames` is a
    /// sentinel), like the generators.
    const DEFAULT_IMAGE_SECS: f64 = 5.0;

    pub fn whole(media_id: vv_core::MediaId, meta: &vv_core::MediaMeta) -> Self {
        let source_out = if meta.is_image() {
            (meta.fps.as_f64() * Self::DEFAULT_IMAGE_SECS).round() as FrameIdx
        } else {
            meta.duration_frames
        };
        Self {
            media_id,
            source_in: 0,
            source_out,
            streams: DragStreams::All,
        }
    }

    pub fn takes_video(&self, meta: &vv_core::MediaMeta) -> bool {
        meta.has_video && self.streams != DragStreams::AudioOnly
    }

    pub fn takes_audio(&self, meta: &vv_core::MediaMeta) -> bool {
        meta.has_audio && self.streams != DragStreams::VideoOnly
    }

    /// Duration in *source* frames (media fps): the in/out markers
    /// of the preview live in that space.
    pub fn source_len(&self) -> FrameIdx {
        self.source_out - self.source_in
    }

    /// How much it will occupy on the timeline, conformed to `rate` (see
    /// `Clip::rate`): what matters for the drop ghost and for the
    /// snapping, which work in timeline frames.
    pub fn timeline_len(&self, rate: vv_core::Rational) -> FrameIdx {
        rate.scale_round(self.source_out) - rate.scale_round(self.source_in)
    }
}

/// Actual payload of the drag&drop: several media selected together in the
/// media pool are appended onto the timeline in the order they appear
/// there, so the payload is an ordered list, not a single media.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MediaDragSet {
    pub items: Vec<MediaDrag>,
}

impl MediaDragSet {
    pub fn one(drag: MediaDrag) -> Self {
        Self { items: vec![drag] }
    }
}

pub use vv_core::edit::{Generator, add_track};

pub fn generator_label(generator: Generator) -> std::borrow::Cow<'static, str> {
    match generator {
        Generator::SolidColor => t!("generator.solid_color"),
        Generator::Text => t!("generator.text"),
        Generator::Adjustment => t!("generator.adjustment"),
    }
}

/// What is being dragged towards the timeline.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TimelineDrag {
    Media(MediaDragSet),
    Generator(Generator),
}

impl TimelineDrag {
    pub fn hovered(resp: &egui::Response) -> Option<Self> {
        resp.dnd_hover_payload::<MediaDragSet>()
            .map(|set| Self::Media((*set).clone()))
            .or_else(|| {
                resp.dnd_hover_payload::<Generator>()
                    .map(|g| Self::Generator(*g))
            })
    }

    pub fn released(resp: &egui::Response) -> Option<Self> {
        // `take_payload` discards the payload even if the type does not match:
        // the right type must be chosen before taking it. A `FilterEntry`, a
        // `TransitionKind` or a whole `Transition` (duplication via
        // Alt+drag) is never a `TimelineDrag` (they are dropped only on
        // a clip, handled in the clip loop): if one of those is in
        // progress, exit immediately, otherwise the `MediaDragSet` branch below would
        // take and destroy it without being able to interpret it, and the
        // drop on the clip would no longer see anything (see the same oversight
        // already made and repaired for the filters).
        if egui::DragAndDrop::has_payload_of_type::<FilterEntry>(&resp.ctx)
            || egui::DragAndDrop::has_payload_of_type::<vv_core::TransitionKind>(&resp.ctx)
            || egui::DragAndDrop::has_payload_of_type::<vv_core::Transition>(&resp.ctx)
        {
            return None;
        }
        if egui::DragAndDrop::has_payload_of_type::<Generator>(&resp.ctx) {
            resp.dnd_release_payload::<Generator>()
                .map(|g| Self::Generator(*g))
        } else {
            resp.dnd_release_payload::<MediaDragSet>()
                .map(|set| Self::Media((*set).clone()))
        }
    }

    fn is_media(&self) -> bool {
        matches!(self, Self::Media(_))
    }
}

/// Filters of the Effects panel, in the order they appear there: unlike the
/// `Generator`s, they apply to an existing video clip instead of
/// generating a new one, and for this reason they stay outside `TimelineDrag`
/// (no ghost on the empty zones, no new tracks). The type shared
/// with `EffectStack::filters` (`vv_core::FilterKind`) remains the single source
/// of truth on "which filters exist": here their entries and labels.
pub const FILTER_ENTRIES: [FilterEntry; 5] = [
    FilterEntry::plain(vv_core::FilterKind::ColorCorrection),
    FilterEntry {
        kind: vv_core::FilterKind::ColorCorrection,
        preset: Some(vv_core::GradePreset::BlackAndWhite),
    },
    FilterEntry::plain(vv_core::FilterKind::Exposure),
    FilterEntry::plain(vv_core::FilterKind::BoxBlur),
    FilterEntry::plain(vv_core::FilterKind::GaussianBlur),
];

/// A filter of the Effects panel, the drag payload: a kind and, for the
/// color correction, possibly a preset (the one-click black and white).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FilterEntry {
    pub kind: vv_core::FilterKind,
    pub preset: Option<vv_core::GradePreset>,
}

impl FilterEntry {
    const fn plain(kind: vv_core::FilterKind) -> Self {
        Self { kind, preset: None }
    }

    pub fn label(self) -> std::borrow::Cow<'static, str> {
        match self.preset {
            Some(vv_core::GradePreset::BlackAndWhite) => t!("filter.black_and_white"),
            _ => filter_label(self.kind),
        }
    }
}

pub fn filter_label(kind: vv_core::FilterKind) -> std::borrow::Cow<'static, str> {
    match kind {
        vv_core::FilterKind::ColorCorrection => t!("filter.color_correction"),
        vv_core::FilterKind::BoxBlur => t!("filter.box_blur"),
        vv_core::FilterKind::GaussianBlur => t!("filter.gaussian_blur"),
        vv_core::FilterKind::Exposure => t!("filter.exposure"),
    }
}

/// How much the dragged media occupies on the timeline: its duration in
/// source frames conformed to the timeline fps (see `Clip::rate`).
/// `1/1` if the media is not (any longer) in the pool.
fn drag_timeline_len(
    project: &Project,
    timeline_fps: vv_core::Rational,
    drag: &MediaDrag,
) -> FrameIdx {
    let rate = project
        .media_pool
        .get(drag.media_id)
        .map(|item| vv_core::Rational::conform_rate(timeline_fps, item.meta.fps))
        .unwrap_or_else(vv_core::Rational::one);
    drag.timeline_len(rate)
}

/// Total length of the drop: the media appended one after the other.
fn drag_set_timeline_len(
    project: &Project,
    timeline_fps: vv_core::Rational,
    drag: &TimelineDrag,
) -> FrameIdx {
    match drag {
        TimelineDrag::Media(set) => set
            .items
            .iter()
            .map(|d| drag_timeline_len(project, timeline_fps, d))
            .sum(),
        TimelineDrag::Generator(g) => g.default_len(timeline_fps),
    }
}

/// A segment of the drop ghost: the media are appended, so the ghost
/// shows them separated.
struct DragSegment {
    offset: FrameIdx,
    len: FrameIdx,
    has_video: bool,
    has_audio: bool,
}

fn drag_set_segments(
    project: &Project,
    timeline_fps: vv_core::Rational,
    drag: &TimelineDrag,
) -> Vec<DragSegment> {
    let set = match drag {
        TimelineDrag::Media(set) => set,
        TimelineDrag::Generator(g) => {
            return vec![DragSegment {
                offset: 0,
                len: g.default_len(timeline_fps),
                has_video: true,
                has_audio: false,
            }];
        }
    };
    let mut offset = 0;
    set.items
        .iter()
        .filter(|d| project.media_pool.contains_key(d.media_id))
        .map(|d| {
            let len = drag_timeline_len(project, timeline_fps, d);
            let seg = DragSegment {
                offset,
                len,
                has_video: d.takes_video(&project.media_pool[d.media_id].meta),
                has_audio: d.takes_audio(&project.media_pool[d.media_id].meta),
            };
            offset += len;
            seg
        })
        .collect()
}

/// Where a drop goes: the usual track, a new one (band above the Video ones or
/// below the Audio ones) or a precise video track.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaDropTarget {
    Default,
    NewVideoTrack,
    NewAudioTrack,
    /// Existing video track under the pointer (drop of an effect or of
    /// a media with video, when the pointer is on a video track already
    /// present).
    Track(usize),
}

/// `Some((drag, frame, target))` if in this frame something dragged from the
/// media pool or from the Effects panel was released; the caller
/// takes care of it.
pub fn show_timeline(
    ui: &mut egui::Ui,
    project: &mut Project,
    history: &mut History,
    timeline_id: TimelineId,
    media_labels: &dyn Fn(vv_core::MediaId) -> String,
    state: &mut TimelineState,
    snapping_enabled: bool,
    kinetic_scroll_enabled: bool,
    // Timeline intervals already cached: "buffered" strip in the ruler.
    buffered_ranges: &[(FrameIdx, FrameIdx)],
    // Timeline intervals of the clips served by the proxy: strip on the clip.
    proxy_ranges: &[(FrameIdx, FrameIdx)],
    // Loaded waveforms, per `(content_hash, stream_index)`.
    waveform_cache: &std::collections::HashMap<(u64, usize), vv_media::Waveform>,
    // The timeline is playing: during playback the playhead must
    // always stay visible, so the view "turns the page" to
    // follow it when it leaves the visible area (see below).
    playback_active: bool,
) -> (
    Option<(TimelineDrag, FrameIdx, MediaDropTarget)>,
    Option<TimelineId>,
) {
    let mut media_drop = None;
    // Double click on a compound clip: the caller (main.rs) opens it
    // as a timeline of its own (see `VenturiApp::enter_compound_timeline`).
    let mut enter_compound = None;

    // Alt+scroll/pinch zooms only with the pointer over the timeline.
    let panel_rect = ui.available_rect_before_wrap();
    let pointer_over_panel = pointer_over(ui.ctx(), panel_rect);
    if pointer_over_panel {
        let zoom = ui.input(|i| i.zoom_delta());
        if zoom != 1.0 {
            state.set_pixels_per_sec(state.pixels_per_sec * zoom);
        }
    }

    // A new touch immediately interrupts any residual inertia, as on
    // a real touchpad: a click/tap (1 finger), the start of a scroll
    // gesture (2 fingers, `TouchPhase::Start`) before it even produces a
    // delta, or even just the pointer moving — resting the fingers
    // on the touchpad without lifting them generates no dedicated event (the
    // system reports only changes, not "fingers at rest"), but a real touch
    // is never perfectly still: a micro-movement of the pointer is
    // the signal left to us to notice it.
    let touch_started = ui.input(|i| {
        i.pointer.delta() != egui::Vec2::ZERO
            || (pointer_over_panel
                && (i.pointer.any_pressed()
                    || i.events.iter().any(|e| {
                        matches!(
                            e,
                            egui::Event::MouseWheel {
                                phase: egui::TouchPhase::Start,
                                ..
                            }
                        )
                    })))
    });
    if touch_started {
        state.video_scroll_vel = 0.0;
        state.audio_scroll_vel = 0.0;
        state.hscroll_vel = 0.0;
    }

    let timeline_fps = project.timelines[timeline_id].fps;
    let fps = timeline_fps.as_f64();
    let px_per_frame = state.pixels_per_sec / fps.max(1.0) as f32;

    // --- pass 1: collect the data to draw (immutable borrow) ---
    let (track_count, track_kinds, visuals, max_end_frames) = {
        let tl = &project.timelines[timeline_id];
        let mut visuals = Vec::new();
        let mut max_end: FrameIdx = 0;
        for (track_index, track) in tl.tracks.iter().enumerate() {
            for clip in &track.clips {
                max_end = max_end.max(clip.timeline_end());
                let offline = matches!(
                    &clip.source,
                    ClipSource::Media(id) if !project.media_pool.contains_key(*id)
                );
                let (label, color) = clip_label_and_color(clip, track, offline, media_labels);
                visuals.push(ClipVisual {
                    track_index,
                    clip: std::borrow::Cow::Borrowed(clip),
                    label,
                    color,
                    locked: track.locked,
                    muted: clip.disabled || (track.kind == TrackKind::Video && track.muted),
                });
            }
        }
        let track_kinds: Vec<TrackKind> = tl.tracks.iter().map(|t| t.kind).collect();
        (tl.tracks.len(), track_kinds, visuals, max_end)
    };
    if let Some(Gesture::Slip(slip)) = &mut state.gesture {
        slip.delta = slip.delta_for(px_per_frame);
    }
    // The slipped clips are drawn with the content they will show.
    let mut visuals = visuals;
    if let Some(slip) = state.slipping() {
        for v in &mut visuals {
            if slip
                .group
                .iter()
                .any(|&(key, _, _)| key == (v.track_index, v.clip.id))
            {
                v.clip.to_mut().source_offset += slip.delta;
            }
        }
    }
    let track_labels: Vec<String> = (0..track_count)
        .map(|i| project.timelines[timeline_id].track_label(i))
        .collect();
    let track_flags: Vec<TrackFlags> = project.timelines[timeline_id]
        .tracks
        .iter()
        .map(|t| TrackFlags {
            muted: t.muted,
            solo: t.solo,
            locked: t.locked,
            armed: t.armed,
        })
        .collect();
    let track_locked = |track_index: usize| track_flags.get(track_index).is_some_and(|f| f.locked);
    state.drop_locked(&project.timelines[timeline_id]);

    let total_secs = (max_end_frames as f64 / fps + TRAILING_MARGIN_SECS).max(MIN_TIMELINE_SECS);
    // At low zoom the natural content is narrower than the panel: we force
    // at least `viewport_width` so the ruler always reaches the edge.
    let viewport_width = (panel_rect.width() - TRACK_HEADER_WIDTH).max(1.0);
    let content_width = ((total_secs * state.pixels_per_sec as f64) as f32).max(viewport_width);

    let row_order = track_row_order(&track_kinds);
    let mut row_of_track = vec![0usize; track_count];
    for (row, &track_index) in row_order.iter().enumerate() {
        row_of_track[track_index] = row;
    }
    let video_count = track_kinds
        .iter()
        .filter(|k| **k == TrackKind::Video)
        .count();
    let audio_count = track_count - video_count;
    let avail_below_ruler = (panel_rect.height() - RULER_HEIGHT).max(0.0);
    let mut layout = PaneLayout::new(avail_below_ruler, video_count, audio_count, state);
    let dt = ui.input(|i| i.stable_dt).min(0.1);
    if !kinetic_scroll_enabled {
        state.video_scroll_vel = 0.0;
        state.audio_scroll_vel = 0.0;
    }

    // Residual inertia from a touchpad swipe just finished: it keeps
    // scrolling and braking, even if the pointer moved in the meantime.
    let video_coasted = apply_kinetic_scroll(
        &mut state.video_scroll,
        &mut state.video_scroll_vel,
        layout.video_max_scroll,
        dt,
    );
    let audio_coasted = apply_kinetic_scroll(
        &mut state.audio_scroll,
        &mut state.audio_scroll_vel,
        layout.audio_max_scroll,
        dt,
    );
    if video_coasted || audio_coasted {
        layout = PaneLayout::new(avail_below_ruler, video_count, audio_count, state);
        ui.ctx().request_repaint();
    }

    // Wheel: vertical scroll of the box under the pointer; with Shift it
    // is the vertical zoom of the tracks (the horizontal scroll is on
    // Ctrl+wheel, see `horizontal_scroll_modifier`).
    if let Some(pos) = ui.input(|i| i.pointer.hover_pos())
        && pointer_over(ui.ctx(), panel_rect)
    {
        let local_y = pos.y - panel_rect.top();
        let (wheel, shift) = ui.input(|i| (i.smooth_scroll_delta.y, i.modifiers.shift));
        if wheel != 0.0 && shift {
            state.set_row_height(state.row_height * (wheel * ROW_ZOOM_SPEED).exp());
            ui.input_mut(|i| i.smooth_scroll_delta.y = 0.0);
            layout = PaneLayout::new(avail_below_ruler, video_count, audio_count, state);
        } else if wheel != 0.0 {
            let scrolled = if layout.video_pane.contains(local_y) && layout.video_max_scroll > 0.0 {
                state.video_scroll += wheel;
                state.video_scroll_vel = if kinetic_scroll_enabled && dt > 0.0 {
                    KINETIC_VELOCITY_GAIN * wheel / dt
                } else {
                    0.0
                };
                true
            } else if layout.audio_pane.contains(local_y) && layout.audio_max_scroll > 0.0 {
                state.audio_scroll -= wheel;
                state.audio_scroll_vel = if kinetic_scroll_enabled && dt > 0.0 {
                    -KINETIC_VELOCITY_GAIN * wheel / dt
                } else {
                    0.0
                };
                true
            } else {
                false
            };
            if scrolled {
                ui.input_mut(|i| i.smooth_scroll_delta.y = 0.0);
                layout = PaneLayout::new(avail_below_ruler, video_count, audio_count, state);
            }
        }
    }
    let row_height = layout.row_height;
    let divider_height = layout.divider_height;
    let visual_height = layout.audio_pane.max;
    let pane_of = |track_index: usize| layout.pane(track_kinds[track_index]);

    // Local `y` of every track (indexed by `track_index`), consistent with
    // `clip_local_rect`.
    let row_y: Vec<f32> = (0..track_count)
        .map(|track_index| layout.row_y(row_of_track[track_index]))
        .collect();
    let track_at_y = |local_y: f32| -> usize { row_order[layout.row_at_y(local_y)] };

    let mut pending: Option<PendingAction> = None;
    // Taken from the volume gesture before the reset further below clears it, to
    // close the undo group after `apply_pending_action` has applied
    // the last `SetGain` of the drag (see `VolumeDragState::group`).
    let mut volume_drag_group: Option<vv_core::GroupMark> = None;

    ui.horizontal_top(|ui| {
        // `Id::with(IdSalt)` and `Id::with(&str)` give different ids: the form
        // the ScrollArea uses in `begin` is needed.
        let scroll_id = ui.make_persistent_id(egui::IdSalt::new("timeline_scroll"));
        let scroll_viewport_width =
            (ui.available_rect_before_wrap().width() - TRACK_HEADER_WIDTH).max(1.0);
        sync_timeline_scroll(
            ui.ctx(),
            scroll_id,
            state,
            fps,
            px_per_frame,
            scroll_viewport_width,
            content_width,
            playback_active,
            panel_rect,
            kinetic_scroll_enabled,
        );

        draw_track_headers(
            ui,
            &track_kinds,
            &track_labels,
            &track_flags,
            &row_order,
            &row_y,
            &layout,
            &mut state.video_pane_height,
            &mut pending,
            state.playhead,
            fps,
            state.can_record,
        );

        egui::ScrollArea::horizontal()
            .id_salt("timeline_scroll")
            // `auto_shrink` off: otherwise the panel closes back to the content and
            // its resize springs back.
            .auto_shrink([false, false])
            .show(ui, |ui| {
                let (rect, _resp) = ui.allocate_exact_size(
                    egui::vec2(content_width, RULER_HEIGHT),
                    egui::Sense::hover(),
                );
                let origin = rect.min;
                // Not allocated: the height of the boxes depends on the panel,
                // and allocating it would make `Panel::bottom` grow indefinitely.
                let visual_rect =
                    egui::Rect::from_min_size(origin, egui::vec2(content_width, visual_height));
                let painter = ui.painter_at(visual_rect);
                let to_local = |pos: egui::Pos2| egui::pos2(pos.x - origin.x, pos.y - origin.y);
                let pane_rect = |pane: egui::Rangef| {
                    egui::Rect::from_x_y_ranges(
                        visual_rect.x_range(),
                        (origin.y + pane.min)..=(origin.y + pane.max),
                    )
                };
                let track_pane_rect = |track_index: usize| pane_rect(pane_of(track_index));
                let track_painter = |track_index: usize| {
                    painter
                        .with_clip_rect(painter.clip_rect().intersect(track_pane_rect(track_index)))
                };
                let local_pane_rect =
                    |track_index: usize| track_pane_rect(track_index).translate(-origin.to_vec2());
                // Only the part of the clip visible in its box.
                let visible_clip_rect = |v: &ClipVisual| {
                    clip_local_rect(v, px_per_frame, &row_y, row_height)
                        .intersect(local_pane_rect(v.track_index))
                };
                let press_over_a_clip = |pos: egui::Pos2| {
                    let local = to_local(pos);
                    visuals.iter().any(|v| visible_clip_rect(v).contains(local))
                };

                show_ruler(
                    ui,
                    &painter,
                    origin,
                    content_width,
                    state,
                    &visuals,
                    fps,
                    px_per_frame,
                    snapping_enabled,
                    buffered_ranges,
                    max_end_frames,
                    &mut pending,
                );
                markers::show_markers(
                    ui,
                    &painter,
                    origin,
                    content_width,
                    timeline_id,
                    &project.timelines[timeline_id].markers,
                    state,
                    &visuals,
                    px_per_frame,
                    snapping_enabled,
                    &mut pending,
                );

                // Track backgrounds and, above them, an interactable area for clicks and
                // a rubber band from the empty space. The clips are interacted with afterwards and win the hit-test.
                for (row, &track_index) in row_order.iter().enumerate() {
                    let y = origin.y + row_y[track_index];
                    let track_rect = egui::Rect::from_min_size(
                        egui::pos2(origin.x, y),
                        egui::vec2(content_width, row_height),
                    );
                    let bg = match (track_locked(track_index), row % 2 == 0) {
                        (true, _) => egui::Color32::from_gray(42),
                        (false, true) => egui::Color32::from_gray(32),
                        (false, false) => egui::Color32::from_gray(27),
                    };
                    track_painter(track_index).rect_filled(track_rect, 0.0, bg);
                }
                let over_rows = |pos: egui::Pos2| {
                    visual_rect.x_range().contains(pos.x) && layout.is_over_rows(pos.y - origin.y)
                };
                // The empty zones too: the selection rectangle can
                // start from there.
                let marquee_area_rect = egui::Rect::from_min_max(
                    egui::pos2(origin.x, origin.y + RULER_HEIGHT),
                    egui::pos2(origin.x + content_width, origin.y + visual_height),
                );
                let pointer_over_tracks =
                    ui.input(|i| i.pointer.hover_pos()).is_some_and(over_rows);
                let marquee_resp = ui.interact(
                    marquee_area_rect,
                    ui.id().with("timeline_marquee"),
                    egui::Sense::click_and_drag(),
                );

                // Interacted with *after* `marquee_resp` to win the hit-test on
                // this thin band (same pattern as the clips below).
                if divider_height > 0.0 {
                    let divider_rect = egui::Rect::from_min_size(
                        egui::pos2(origin.x, origin.y + layout.video_pane.max),
                        egui::vec2(content_width, divider_height),
                    );
                    interact_divider(
                        ui,
                        &painter,
                        divider_rect,
                        ui.id().with("timeline_track_split"),
                        &layout,
                        &mut state.video_pane_height,
                    );
                }

                // `dnd_*_payload` look at `contains_pointer`: they work even if the
                // drag started from another widget. The media with video or the effect
                // land on the video track under the pointer.
                let drop_target = |drag: &TimelineDrag, pos: egui::Pos2| {
                    let track = track_at_y(pos.y - origin.y);
                    let has_video = match drag {
                        TimelineDrag::Generator(_) => true,
                        TimelineDrag::Media(set) => set
                            .items
                            .iter()
                            .any(|d| d.takes_video(&project.media_pool[d.media_id].meta)),
                    };
                    media_pool_drop_target(
                        track,
                        has_video,
                        track_kinds[track],
                        track_locked(track),
                    )
                };
                // Where a drop falls: the frame under the pointer, with the snapping.
                let playhead = state.playhead;
                let drop_frame = |drag: &TimelineDrag, pos: egui::Pos2| {
                    let raw = (((pos.x - origin.x) / px_per_frame).round() as FrameIdx).max(0);
                    snap_frame(
                        raw,
                        drag_set_timeline_len(project, timeline_fps, drag),
                        &visuals,
                        &[],
                        &[playhead],
                        px_per_frame,
                        snapping_enabled,
                    )
                    .max(0)
                };
                // Layer above the clips, painted further on.
                let ghost_painter = painter.clone().with_layer_id(egui::LayerId::new(
                    egui::Order::Foreground,
                    ui.id().with("timeline_drop_ghost"),
                ));
                if pointer_over_tracks
                    && let Some(drag) = TimelineDrag::hovered(&marquee_resp)
                    && let Some(pos) = ui.input(|i| i.pointer.hover_pos())
                    && let Some(target) = drop_target(&drag, pos)
                    && !drag_set_segments(project, timeline_fps, &drag).is_empty()
                {
                    let frame = drop_frame(&drag, pos);
                    // The tracks where `vv_core::edit::insert_media` puts video and audio.
                    let first_row = |kind| {
                        (0..track_count)
                            .find(|&t| track_kinds[t] == kind && !track_locked(t))
                            .map(|t| origin.y + row_y[t])
                    };
                    let video_y = match target {
                        MediaDropTarget::Track(track) => Some(origin.y + row_y[track]),
                        _ => first_row(TrackKind::Video),
                    };
                    let audio_y = first_row(TrackKind::Audio);
                    for seg in drag_set_segments(project, timeline_fps, &drag) {
                        let x = origin.x + (frame + seg.offset) as f32 * px_per_frame;
                        let rows = [
                            (seg.has_video, video_y, layout.video_pane),
                            (seg.has_audio, audio_y, layout.audio_pane),
                        ];
                        for (_, y, pane) in rows.into_iter().filter(|(present, _, _)| *present) {
                            let Some(y) = y else { continue };
                            let ghost_painter = ghost_painter.with_clip_rect(
                                ghost_painter.clip_rect().intersect(pane_rect(pane)),
                            );
                            let rect = egui::Rect::from_min_size(
                                egui::pos2(x, y),
                                egui::vec2(seg.len as f32 * px_per_frame, row_height),
                            )
                            // A sliver of margin between one segment and the
                            // next: without it the edges meet and the appended
                            // clips look like a single block.
                            .shrink2(egui::vec2(1.0, 0.0));
                            ghost_painter.rect_filled(
                                rect,
                                4.0,
                                egui::Color32::from_rgba_unmultiplied(120, 220, 120, 90),
                            );
                            ghost_painter.rect_stroke(
                                rect,
                                4.0,
                                egui::Stroke::new(2.0, egui::Color32::from_rgb(120, 220, 120)),
                                egui::StrokeKind::Inside,
                            );
                        }
                    }
                }
                if pointer_over_tracks
                    && let Some(drag) = TimelineDrag::released(&marquee_resp)
                    && let Some(pos) = ui.input(|i| i.pointer.interact_pos())
                    && let Some(target) = drop_target(&drag, pos)
                {
                    let frame = drop_frame(&drag, pos);
                    media_drop = Some((drag, frame, target));
                }

                // Ghost of a filter: a gear instead of the green rectangle,
                // anywhere on the timeline (not only on the tracks, as above: a
                // filter never lands on an empty space, but the cursor
                // stays consistent anyway while passing over it).
                if pointer_over_panel
                    && egui::DragAndDrop::has_payload_of_type::<FilterEntry>(ui.ctx())
                    && let Some(pos) = ui.input(|i| i.pointer.hover_pos())
                {
                    paint_gear_icon(
                        &ghost_painter,
                        pos + egui::vec2(14.0, 14.0),
                        10.0,
                        egui::Color32::WHITE,
                    );
                }

                // Same gear ghost as the filters: a transition
                // too (from the panel or duplicated with Alt+drag)
                // lands only on an existing clip, never on an empty
                // space.
                if pointer_over_panel
                    && (egui::DragAndDrop::has_payload_of_type::<vv_core::TransitionKind>(ui.ctx())
                        || egui::DragAndDrop::has_payload_of_type::<vv_core::Transition>(ui.ctx()))
                    && let Some(pos) = ui.input(|i| i.pointer.hover_pos())
                {
                    paint_gear_icon(
                        &ghost_painter,
                        pos + egui::vec2(14.0, 14.0),
                        10.0,
                        egui::Color32::WHITE,
                    );
                }

                // "Add a new track" zones: margins above/below the
                // groups (zero height if there is no margin, see above).
                let above_video_rect = egui::Rect::from_min_max(
                    egui::pos2(origin.x, origin.y + layout.video_pane.min),
                    egui::pos2(
                        origin.x + content_width,
                        origin.y + layout.video_rows_top.max(layout.video_pane.min),
                    ),
                );
                let above_video_resp = ui.interact(
                    above_video_rect,
                    ui.id().with("timeline_new_video_track_zone"),
                    egui::Sense::hover(),
                );
                if TimelineDrag::hovered(&above_video_resp).is_some() {
                    paint_drop_zone(
                        &painter,
                        above_video_rect,
                        Some(&t!("timeline.new_video_track")),
                    );
                }
                if let Some(drag) = TimelineDrag::released(&above_video_resp)
                    && let Some(pos) = ui.input(|i| i.pointer.interact_pos())
                {
                    let frame = drop_frame(&drag, pos);
                    media_drop = Some((drag, frame, MediaDropTarget::NewVideoTrack));
                }

                let below_audio_rect = egui::Rect::from_min_max(
                    egui::pos2(
                        origin.x,
                        origin.y + layout.audio_rows_bottom().min(layout.audio_pane.max),
                    ),
                    egui::pos2(origin.x + content_width, origin.y + layout.audio_pane.max),
                );
                let below_audio_resp = ui.interact(
                    below_audio_rect,
                    ui.id().with("timeline_new_audio_track_zone"),
                    egui::Sense::hover(),
                );
                if TimelineDrag::hovered(&below_audio_resp).is_some_and(|d| d.is_media()) {
                    paint_drop_zone(
                        &painter,
                        below_audio_rect,
                        Some(&t!("timeline.new_audio_track")),
                    );
                }
                if let Some(drag) = TimelineDrag::released(&below_audio_resp)
                    && drag.is_media()
                    && let Some(pos) = ui.input(|i| i.pointer.interact_pos())
                {
                    let frame = drop_frame(&drag, pos);
                    media_drop = Some((drag, frame, MediaDropTarget::NewAudioTrack));
                }

                marquee_resp.context_menu(|ui| {
                    if ui.button(t!("timeline.add_marker")).clicked() {
                        pending = Some(PendingAction::Marker(markers::MarkerChange::AddAtPlayhead));
                        ui.close();
                    }
                });
                if marquee_resp.drag_started() {
                    if let Some(pos) = marquee_resp.interact_pointer_pos()
                        && !press_over_a_clip(pos)
                    {
                        let local = to_local(pos);
                        state.gesture = Some(Gesture::Marquee(MarqueeDrag {
                            start: local,
                            current: local,
                        }));
                    }
                } else if marquee_resp.dragged() {
                    if let (Some(Gesture::Marquee(m)), Some(pos)) =
                        (&mut state.gesture, marquee_resp.interact_pointer_pos())
                    {
                        m.current = to_local(pos);
                    }
                } else if marquee_resp.drag_stopped() {
                    if let Some(Gesture::Marquee(m)) =
                        state.gesture.take_if(|g| matches!(g, Gesture::Marquee(_)))
                    {
                        let rect = egui::Rect::from_two_pos(m.start, m.current);
                        let hits: Vec<ClipKey> = visuals
                            .iter()
                            .filter(|v| !v.locked && visible_clip_rect(v).intersects(rect))
                            .map(|v| (v.track_index, v.clip.id))
                            .collect();
                        state.selected = expand_to_linked_groups(&visuals, hits.iter().copied());
                        state.selection_anchor = hits.first().copied();
                        state.selected_gap = None;
                        state.selected_transition = None;
                    }
                } else if marquee_resp.clicked()
                    && let Some(pos) = marquee_resp.interact_pointer_pos()
                    && !press_over_a_clip(pos)
                {
                    if row_order.is_empty() || !over_rows(pos) {
                        state.clear_selection();
                    } else {
                        // Click on a gap followed by a clip: it gets selected.
                        let local = to_local(pos);
                        let frame = ((local.x / px_per_frame).round() as FrameIdx).max(0);
                        let track_index = track_at_y(local.y);
                        match gap_at(&visuals, track_index, frame)
                            .filter(|_| !track_locked(track_index))
                        {
                            Some((gap_start, gap_end)) => {
                                state.selected.clear();
                                state.selection_anchor = None;
                                state.selected_gap = Some((track_index, gap_start, gap_end));
                                state.selected_transition = None;
                            }
                            None => state.clear_selection(),
                        }
                    }
                }
                if let Some(Gesture::Marquee(m)) = &state.gesture {
                    let marquee_rect = egui::Rect::from_two_pos(
                        origin + m.start.to_vec2(),
                        origin + m.current.to_vec2(),
                    );
                    painter.rect_filled(marquee_rect, 0.0, crate::theme::ACCENT_TRANSLUCENT);
                    painter.rect_stroke(
                        marquee_rect,
                        0.0,
                        egui::Stroke::new(1.0, crate::theme::ACCENT),
                        egui::StrokeKind::Inside,
                    );
                }

                // Selected gap: same frame as a selected clip.
                if let Some((track_index, gap_start, gap_end)) = state.selected_gap
                    && let Some(&row_y_val) = row_y.get(track_index)
                {
                    let y = origin.y + row_y_val;
                    let gap_rect = egui::Rect::from_min_size(
                        egui::pos2(origin.x + gap_start as f32 * px_per_frame, y + 2.0),
                        egui::vec2(
                            (gap_end - gap_start) as f32 * px_per_frame,
                            row_height - 4.0,
                        ),
                    );
                    let painter = track_painter(track_index);
                    painter.rect_filled(
                        gap_rect,
                        4.0,
                        egui::Color32::from_rgba_unmultiplied(255, 255, 255, 30),
                    );
                    painter.rect_stroke(
                        gap_rect,
                        4.0,
                        egui::Stroke::new(2.0, egui::Color32::WHITE),
                        egui::StrokeKind::Inside,
                    );
                }

                // Candidate track of the drag, from the current pointer position.
                let drag_effective_track = state.moving().map(|d| {
                    let kind = track_kinds[d.track_index];
                    let target = ui.input(|i| i.pointer.interact_pos()).and_then(|pos| {
                        track_drag_target(to_local(pos).y, kind, &row_order, &layout)
                    });
                    match target {
                        Some(TrackDragTarget::Track(idx)) if !track_locked(idx) => {
                            EffectiveTrack::Existing(idx)
                        }
                        Some(TrackDragTarget::NewTrack) => EffectiveTrack::New(1),
                        _ => EffectiveTrack::Existing(d.track_index),
                    }
                });
                if let (Some(d), Some(EffectiveTrack::New(_))) =
                    (state.moving(), drag_effective_track)
                {
                    let rect = match track_kinds[d.track_index] {
                        TrackKind::Video => above_video_rect,
                        TrackKind::Audio => below_audio_rect,
                    };
                    paint_drop_zone(&painter, rect, None);
                }

                // See `drag_group_row_targets`.
                // If any clip of the group would land on a locked
                // track, the group stays on its own tracks.
                let drag_group_targets: Option<Vec<(ClipId, EffectiveTrack)>> =
                    state.moving().map(|d| {
                        let targets_for = |primary_target| {
                            drag_group_row_targets(
                                d.clip_id,
                                d.track_index,
                                primary_target,
                                &d.followers,
                                &track_kinds,
                                &row_of_track,
                                &row_order,
                                video_count,
                                track_count,
                            )
                        };
                        let targets = targets_for(drag_effective_track.unwrap());
                        if targets.iter().any(|(_, t)| {
                            matches!(t, EffectiveTrack::Existing(track) if track_locked(*track))
                        }) {
                            targets_for(EffectiveTrack::Existing(d.track_index))
                        } else {
                            targets
                        }
                    });

                // Position of the dragged primary (clamped and snapped), once
                // for the whole group and for the preview during the drag.
                let dragged_primary_new_start = state.moving().map(|d| {
                    let raw = d.original_start as f32 + d.accum_px / px_per_frame;
                    let raw_rounded = raw.round() as FrameIdx;
                    let len = visuals
                        .iter()
                        .find(|v| v.clip.id == d.clip_id)
                        .map(|v| v.clip.timeline_len)
                        .unwrap_or(0);
                    let (min_start, max_start) = group_drag_bounds(
                        &visuals,
                        raw_rounded,
                        drag_group_targets.as_deref().unwrap(),
                        &d.followers,
                    );
                    let candidate = raw_rounded.clamp(min_start, max_start);
                    let mut exclude = vec![d.clip_id];
                    exclude.extend(d.followers.iter().map(|(id, _, _)| *id));
                    snap_frame(
                        candidate,
                        len,
                        &visuals,
                        &exclude,
                        &[state.playhead],
                        px_per_frame,
                        snapping_enabled,
                    )
                    .clamp(min_start, max_start)
                });

                // As above for the edge of a trim, which changes the length too.
                let trimmed_primary_new_value = state.trimming().map(|t| {
                    let raw = t.original_value as f32 + t.accum_px / px_per_frame;
                    let exclude: Vec<ClipId> = std::iter::once(t.clip_id)
                        .chain(t.followers.iter().map(|&(id, _, _, _)| id))
                        .collect();
                    let snapped = snap_frame(
                        raw.round() as FrameIdx,
                        0,
                        &visuals,
                        &exclude,
                        &[state.playhead],
                        px_per_frame,
                        snapping_enabled,
                    );
                    snapped.clamp(t.min_value, t.max_value)
                });

                // The retimed end snaps like a trimmed one.
                let retimed_new_len = state.retiming().map(|r| {
                    let raw = (r.start + r.original_len) as f32 + r.accum_px / px_per_frame;
                    let exclude: Vec<ClipId> = r.group.iter().map(|&(_, id)| id).collect();
                    let snapped = snap_frame(
                        raw.round() as FrameIdx,
                        0,
                        &visuals,
                        &exclude,
                        &[state.playhead],
                        px_per_frame,
                        snapping_enabled,
                    );
                    let (min_len, max_len) = r.len_range();
                    (snapped - r.start).clamp(min_len, max_len)
                });

                // The gesture is cleared only after the loop: the clips of the group
                // drawn after the primary would go back for one frame to the initial
                // position.
                let mut gesture_finished = false;
                let mut edge_cursor: Option<(egui::Pos2, EdgeCursor)> = None;
                // The mirror marker on the neighbor must be drawn after the whole loop,
                // not during the iteration of the clip under the pointer: if the
                // neighbor comes later in `draw_order` (the common case, more recent
                // clips have higher ids), its own `paint_clip_box` would
                // cover it immediately — see the comment on `gesture_finished` above
                // for the same structural reason.
                let mut pending_crossing_previews: Vec<(usize, ClipId, FadeEdge)> = Vec::new();

                // The moving clips are drawn last: they invade the others.
                let trimmed_keys: Vec<ClipKey> = state
                    .trimming()
                    .map(|t| {
                        std::iter::once((t.track_index, t.clip_id))
                            .chain(t.followers.iter().map(|&(id, track, _, _)| (track, id)))
                            .collect()
                    })
                    .unwrap_or_default();
                let mut moving_keys = trimmed_keys.clone();
                if let Some(d) = state.moving() {
                    moving_keys.push((d.track_index, d.clip_id));
                    moving_keys.extend(d.followers.iter().map(|&(id, track, _)| (track, id)));
                }
                let draw_order: Vec<&ClipVisual> = visuals
                    .iter()
                    .filter(|v| !moving_keys.contains(&(v.track_index, v.clip.id)))
                    .chain(
                        visuals
                            .iter()
                            .filter(|v| moving_keys.contains(&(v.track_index, v.clip.id))),
                    )
                    .collect();
                // When duplicating, the originals stay visible in their place.
                if state.moving().is_some_and(|d| d.duplicate) {
                    for visual in visuals
                        .iter()
                        .filter(|v| moving_keys.contains(&(v.track_index, v.clip.id)))
                    {
                        let clip_rect = egui::Rect::from_min_size(
                            egui::pos2(
                                origin.x + visual.clip.timeline_start as f32 * px_per_frame,
                                origin.y + row_y[visual.track_index] + 2.0,
                            ),
                            egui::vec2(
                                (visual.clip.timeline_len as f32 * px_per_frame).max(2.0),
                                row_height - 4.0,
                            ),
                        )
                        .round_to_pixels(ui.pixels_per_point());
                        let painter = track_painter(visual.track_index);
                        let corner = clip_corner_radius(clip_rect);
                        painter.rect_filled(clip_rect, corner, visual.color);
                        if clip_rect.width() >= MIN_WIDTH_FOR_STROKE {
                            painter.rect_stroke(
                                clip_rect,
                                corner,
                                egui::Stroke::new(1.0, egui::Color32::from_gray(15)),
                                egui::StrokeKind::Inside,
                            );
                        } else {
                            paint_clip_separator(&painter, clip_rect);
                        }
                        let label_pos = clip_rect.left_top() + egui::vec2(4.0, 2.0);
                        paint_clip_label(
                            &painter,
                            &visual.label,
                            label_pos,
                            clip_rect.right() - 4.0 - label_pos.x,
                            egui::Color32::BLACK,
                        );
                    }
                }
                for visual in draw_order {
                    let painter = track_painter(visual.track_index);
                    let is_trimming_this =
                        trimmed_keys.contains(&(visual.track_index, visual.clip.id));
                    let (display_start, display_len) = display_range(
                        visual,
                        state,
                        is_trimming_this,
                        trimmed_primary_new_value,
                        dragged_primary_new_start,
                        retimed_new_len,
                    );

                    let x = origin.x + display_start as f32 * px_per_frame;
                    // During a drag changing track, the preview of every
                    // clip of the group follows its own target (see
                    // `drag_group_targets`) instead of the starting track.
                    let this_target = drag_group_targets
                        .as_ref()
                        .and_then(|targets| targets.iter().find(|(id, _)| *id == visual.clip.id));
                    let y = match this_target {
                        Some((_, EffectiveTrack::Existing(track))) => origin.y + row_y[*track],
                        // Every "depth" stacks another row past the
                        // current edge (see `EffectiveTrack::New`).
                        Some((_, EffectiveTrack::New(depth))) => {
                            match track_kinds[visual.track_index] {
                                TrackKind::Video => {
                                    origin.y + layout.video_rows_top - *depth as f32 * row_height
                                }
                                TrackKind::Audio => {
                                    origin.y
                                        + layout.audio_rows_bottom()
                                        + (*depth - 1) as f32 * row_height
                                }
                            }
                        }
                        None => origin.y + row_y[visual.track_index],
                    };
                    let w = (display_len as f32 * px_per_frame).max(2.0);
                    // Pixel snap: without it, the edges of dense clips straddle
                    // two pixels and antialiasing makes them unevenly thick
                    // (jagged look).
                    let clip_rect = egui::Rect::from_min_size(
                        egui::pos2(x, y + 2.0),
                        egui::vec2(w, row_height - 4.0),
                    )
                    .round_to_pixels(ui.pixels_per_point());
                    #[cfg(test)]
                    state.clip_rects.insert(visual.clip.id, clip_rect);

                    let id = ui.id().with("clip").with(visual.clip.id.0);
                    let sense = if visual.locked {
                        egui::Sense::hover()
                    } else {
                        egui::Sense::click_and_drag()
                    };
                    let resp = ui.interact(
                        clip_rect.intersect(track_pane_rect(visual.track_index)),
                        id,
                        sense,
                    );

                    let is_selected = state
                        .selected
                        .contains(&(visual.track_index, visual.clip.id));
                    paint_clip_box(&painter, clip_rect, visual, is_selected);

                    let retime_bar_shown = state.retime_controls.contains(&visual.clip.id)
                        && matches!(visual.clip.source, ClipSource::Media(_));

                    // Filter dragged from the Effects panel: only video clips,
                    // not locked, accept it (no empty spaces or new
                    // tracks, unlike Generator/Media). The guard
                    // `has_payload_of_type` before `dnd_release_payload` is not
                    // redundant: the latter discards the global payload even
                    // when the type does not match (egui side effect, see
                    // `TimelineDrag::released`) — without it, dragging a
                    // `TransitionKind` onto the same clip would lose it here,
                    // before the block below even sees it.
                    if !visual.locked
                        && track_kinds[visual.track_index] == TrackKind::Video
                        && egui::DragAndDrop::has_payload_of_type::<FilterEntry>(ui.ctx())
                    {
                        if resp.dnd_hover_payload::<FilterEntry>().is_some() {
                            painter.rect_stroke(
                                clip_rect,
                                4.0,
                                egui::Stroke::new(3.0, FILTER_HIGHLIGHT_COLOR),
                                egui::StrokeKind::Inside,
                            );
                        }
                        if let Some(filter) = resp.dnd_release_payload::<FilterEntry>() {
                            pending = Some(PendingAction::ApplyFilter {
                                track_index: visual.track_index,
                                clip_id: visual.clip.id,
                                filter: *filter,
                            });
                        }
                    }

                    // Transition dragged from the Effects panel: like the filters,
                    // only unlocked video clips — but in addition only near an
                    // edge (never at the center, never on an empty space or a new
                    // track): the side nearest to the pointer decides whether it becomes
                    // `transition_in` or `transition_out`. Same guard as
                    // above, same reason.
                    if !visual.locked
                        && track_kinds[visual.track_index] == TrackKind::Video
                        && egui::DragAndDrop::has_payload_of_type::<vv_core::TransitionKind>(
                            ui.ctx(),
                        )
                    {
                        let drop_zone_px = TRANSITION_DROP_ZONE_PX.min(clip_rect.width() / 2.0);
                        let hover_edge = ui
                            .input(|i| i.pointer.hover_pos())
                            .and_then(|pos| transition_drop_edge(pos, clip_rect, drop_zone_px))
                            .filter(|&edge| accepts_transition(&visuals, visual, edge));
                        if resp
                            .dnd_hover_payload::<vv_core::TransitionKind>()
                            .is_some()
                            && let Some(edge) = hover_edge
                        {
                            let x = match edge {
                                FadeEdge::In => clip_rect.left() + drop_zone_px,
                                FadeEdge::Out => clip_rect.right() - drop_zone_px,
                            };
                            let is_crossing =
                                has_neighbor(&visuals, visual.track_index, visual.clip.id, edge);
                            paint_transition_marker(
                                &painter,
                                clip_rect,
                                edge,
                                x,
                                false,
                                is_crossing,
                            );
                            if is_crossing {
                                pending_crossing_previews.push((
                                    visual.track_index,
                                    visual.clip.id,
                                    edge,
                                ));
                            }
                        }
                        if let Some(kind) = resp.dnd_release_payload::<vv_core::TransitionKind>()
                            && let Some(edge) = hover_edge
                        {
                            pending = Some(PendingAction::ApplyTransition {
                                track_index: visual.track_index,
                                clip_id: visual.clip.id,
                                edge,
                                kind: *kind,
                            });
                        }
                    }

                    // Alt+drag of an existing transition (duplication, see
                    // `begin_transition_duplicate_drag`): same payload as a
                    // drop from the Effects panel but with a whole `vv_core::Transition`
                    // instead of a `TransitionKind`, to keep its
                    // parameters (duration, direction, ease, curve) instead of
                    // starting from the defaults. Same guard, same reason.
                    if !visual.locked
                        && track_kinds[visual.track_index] == TrackKind::Video
                        && egui::DragAndDrop::has_payload_of_type::<vv_core::Transition>(ui.ctx())
                    {
                        let drop_zone_px = TRANSITION_DROP_ZONE_PX.min(clip_rect.width() / 2.0);
                        let hover_edge = ui
                            .input(|i| i.pointer.hover_pos())
                            .and_then(|pos| transition_drop_edge(pos, clip_rect, drop_zone_px))
                            .filter(|&edge| accepts_transition(&visuals, visual, edge));
                        if resp.dnd_hover_payload::<vv_core::Transition>().is_some()
                            && let Some(edge) = hover_edge
                        {
                            let x = match edge {
                                FadeEdge::In => clip_rect.left() + drop_zone_px,
                                FadeEdge::Out => clip_rect.right() - drop_zone_px,
                            };
                            let is_crossing =
                                has_neighbor(&visuals, visual.track_index, visual.clip.id, edge);
                            paint_transition_marker(
                                &painter,
                                clip_rect,
                                edge,
                                x,
                                false,
                                is_crossing,
                            );
                            if is_crossing {
                                pending_crossing_previews.push((
                                    visual.track_index,
                                    visual.clip.id,
                                    edge,
                                ));
                            }
                        }
                        if let Some(transition) = resp.dnd_release_payload::<vv_core::Transition>()
                            && let Some(edge) = hover_edge
                        {
                            pending = Some(PendingAction::DuplicateTransition {
                                track_index: visual.track_index,
                                clip_id: visual.clip.id,
                                edge,
                                transition: (*transition).clone(),
                            });
                        }
                    }

                    // Waveform: maximum of the bins per column, shape independent of the zoom.
                    if track_kinds[visual.track_index] == TrackKind::Audio
                        && visual.clip.freeze.is_none()
                        && let ClipSource::Media(media_id) = &visual.clip.source
                        && let Some(item) = project.media_pool.get(*media_id)
                        && let Some(wf) =
                            waveform_cache.get(&(item.content_hash, visual.clip.audio_stream_index))
                    {
                        // During a trim the drawn clip covers another
                        // band of the source: without remapping it the
                        // waveform would stretch instead of being cut.
                        let (wave_start, wave_end) = if is_trimming_this {
                            (display_start, display_start + display_len)
                        } else {
                            (visual.clip.timeline_start, visual.clip.timeline_end())
                        };
                        let fps = timeline_fps.as_f64();
                        draw_clip_waveform(
                            &painter,
                            clip_rect,
                            &wf.peaks,
                            visual.clip.media_secs_at(wave_start, fps),
                            visual.clip.media_secs_at(wave_end, fps),
                            item.meta.fps.as_f64(),
                            wf.audio_duration_secs,
                            painter.clip_rect(),
                            &visual.clip.effects.gain_db,
                        );
                    }

                    if !visual.locked {
                        paint_silence_preview(
                            &painter,
                            clip_rect,
                            &visual.clip,
                            px_per_frame,
                            &state.silence_preview,
                        );
                    }

                    let is_proxy_backed = proxy_ranges.iter().any(|&(s, e)| {
                        s < visual.clip.timeline_end() && e >= visual.clip.timeline_start
                    });
                    let top_inset = if retime_bar_shown {
                        retime_bar_height(clip_rect)
                    } else {
                        0.0
                    };
                    paint_clip_overlay(&painter, clip_rect, visual, is_proxy_backed, top_inset);
                    if retime_bar_shown {
                        let group: Vec<ClipKey> = expand_to_linked_groups(
                            &visuals,
                            [(visual.track_index, visual.clip.id)],
                        )
                        .into_iter()
                        .collect();
                        let dragged = state
                            .retiming()
                            .filter(|r| r.group.contains(&(visual.track_index, visual.clip.id)))
                            .zip(retimed_new_len)
                            .map(|(r, len)| r.speed_for(len));
                        match retime_bar(
                            ui,
                            &painter,
                            clip_rect,
                            id,
                            dragged.unwrap_or(visual.clip.speed()),
                            !visual.locked,
                        ) {
                            Some(RetimeBarAction::Close) => {
                                state.retime_controls.remove(&visual.clip.id);
                            }
                            Some(RetimeBarAction::Percent(percent)) => {
                                pending = Some(PendingAction::SetSpeed {
                                    clips: group,
                                    speed: vv_core::Rational::from_percent(percent),
                                    pitch_correction: visual.clip.pitch_correction,
                                    resize_to: None,
                                });
                            }
                            Some(RetimeBarAction::Dialog) => {
                                state.speed_dialog_requested = Some(group);
                            }
                            None => {}
                        }
                    }

                    // Volume line: a thin horizontal line draggable
                    // vertically, centered at 0 dB (see `gain_offset`). Only
                    // if the gain is not keyframed: a flat line would lie
                    // about the real curve, which is edited from the properties panel.
                    if track_kinds[visual.track_index] == TrackKind::Audio
                        && visual.clip.effects.gain_db.is_constant()
                    {
                        let dragging = matches!(
                            &state.gesture,
                            Some(Gesture::Volume(d)) if d.clip_id == visual.clip.id
                        );
                        paint_gain_line(
                            &painter,
                            clip_rect,
                            gain_line_y(visual.clip.effects.gain_db.default, clip_rect),
                            dragging,
                        );
                    }

                    // Fade-in/fade-out handles: always present if the
                    // fade is already set, otherwise only while the
                    // clip is under the mouse (to grab them from the corner).
                    let (fade_in_preview, fade_out_preview) =
                        fade_preview(state, visual, px_per_frame);
                    let fading_edge = |edge| {
                        state
                            .fading()
                            .is_some_and(|d| d.clip_id == visual.clip.id && d.edge == edge)
                    };
                    let fade_in_dragging = fading_edge(FadeEdge::In);
                    let fade_out_dragging = fading_edge(FadeEdge::Out);
                    // Their handles would sit on the retime bar.
                    let show_fades = !visual.locked
                        && !retime_bar_shown
                        && clip_rect.width() >= MIN_FADE_CLIP_WIDTH_PX;
                    let fade_in_x = clip_rect.left()
                        + (fade_in_preview as f32 * px_per_frame).min(clip_rect.width());
                    let fade_out_x = clip_rect.right()
                        - (fade_out_preview as f32 * px_per_frame).min(clip_rect.width());
                    // The slip tool can't grab them: no reveal on hover, nor while slipping.
                    let reveal_fades = resp.hovered() && state.tool == TimelineTool::Select;
                    if show_fades && (visual.clip.fade_in > 0 || fade_in_dragging || reveal_fades) {
                        paint_fade_wedge(
                            &painter,
                            clip_rect,
                            clip_rect.left(),
                            fade_in_x,
                            fade_in_dragging,
                        );
                    }
                    if show_fades && (visual.clip.fade_out > 0 || fade_out_dragging || reveal_fades)
                    {
                        paint_fade_wedge(
                            &painter,
                            clip_rect,
                            clip_rect.right(),
                            fade_out_x,
                            fade_out_dragging,
                        );
                    }
                    if let Some(pos) = ui.input(|i| i.pointer.latest_pos())
                        && (fade_in_dragging || fade_out_dragging)
                    {
                        let frames = if fade_in_dragging {
                            fade_in_preview
                        } else {
                            fade_out_preview
                        };
                        paint_duration_overlay(ui.ctx(), pos, frames, timeline_fps.as_f64());
                    }

                    // Markers of the already set transitions (single edge or
                    // crossing): the drop from the Effects panel (above) makes them
                    // persistent, from here on they live like the fade handle —
                    // always visible, the end (duration) draggable.
                    let track = &project.timelines[timeline_id].tracks[visual.track_index];
                    let left_marker = edge_marker(track, state, visual, px_per_frame, FadeEdge::In);
                    let right_marker =
                        edge_marker(track, state, visual, px_per_frame, FadeEdge::Out);
                    let transition_in_x = left_marker.as_ref().map(|m| {
                        clip_rect.left() + (m.duration as f32 * px_per_frame).min(clip_rect.width())
                    });
                    let transition_out_x = right_marker.as_ref().map(|m| {
                        clip_rect.right()
                            - (m.duration as f32 * px_per_frame).min(clip_rect.width())
                    });
                    if let (Some(m), Some(x)) = (&left_marker, transition_in_x) {
                        let selected = state.selected_transition == Some(m.selection);
                        paint_transition_marker(
                            &painter,
                            clip_rect,
                            FadeEdge::In,
                            x,
                            selected,
                            m.is_crossing,
                        );
                    }
                    if let (Some(m), Some(x)) = (&right_marker, transition_out_x) {
                        let selected = state.selected_transition == Some(m.selection);
                        paint_transition_marker(
                            &painter,
                            clip_rect,
                            FadeEdge::Out,
                            x,
                            selected,
                            m.is_crossing,
                        );
                    }
                    // Overlay with the duration during the drag of its
                    // end: general behavior (see
                    // `paint_duration_overlay`), not only for the fade above.
                    if let Some(pos) = ui.input(|i| i.pointer.latest_pos()) {
                        if let Some(d) = state
                            .sizing_transition()
                            .filter(|d| d.clip_id == visual.clip.id)
                        {
                            let frames =
                                edge_drag_value(d, visual.clip.timeline_len, px_per_frame, 1);
                            paint_duration_overlay(ui.ctx(), pos, frames, timeline_fps.as_f64());
                        } else if let Some(d) = state.sizing_crossing().filter(|d| {
                            [&left_marker, &right_marker]
                                .into_iter()
                                .flatten()
                                .any(|m| {
                                    m.selection
                                        == TransitionSelection::Crossing(d.track_index, d.left_clip)
                                })
                        }) {
                            let frames = crossing_drag_value(d, px_per_frame);
                            paint_duration_overlay(ui.ctx(), pos, frames, timeline_fps.as_f64());
                        }
                    }

                    // Reduced zones for very narrow clips, otherwise
                    // the whole clip would be "only edges" and it could no longer
                    // be moved (Move) with a normal drag from the center.
                    let adjacent = |at: FrameIdx, edge: TrimEdge| {
                        visuals
                            .iter()
                            .find(|v| {
                                v.track_index == visual.track_index
                                    && v.clip.id != visual.clip.id
                                    && match edge {
                                        TrimEdge::Start => v.clip.timeline_end() == at,
                                        TrimEdge::End => v.clip.timeline_start == at,
                                    }
                            })
                            .map(|v| (v.track_index, v.clip.id))
                    };
                    let zones = edge_zones(
                        clip_rect.width(),
                        adjacent(visual.clip.timeline_start, TrimEdge::Start),
                        adjacent(visual.clip.timeline_end(), TrimEdge::End),
                    );
                    let edge_at = |pos: egui::Pos2| zones.at(pos.x - clip_rect.left());
                    let volume_hit = |pos: egui::Pos2| {
                        track_kinds[visual.track_index] == TrackKind::Audio
                            && visual.clip.effects.gain_db.is_constant()
                            && volume_line_hit(
                                pos,
                                clip_rect,
                                gain_line_y(visual.clip.effects.gain_db.default, clip_rect),
                            )
                    };
                    let idle_hover = resp.hovered() && state.gesture.is_none();
                    let slip_tool = state.tool == TimelineTool::Slip;
                    let select_hover = idle_hover && !slip_tool;
                    if idle_hover
                        && slip_tool
                        && !visual.locked
                        && matches!(visual.clip.source, ClipSource::Media(_))
                        && let Some(pos) = resp.hover_pos()
                    {
                        edge_cursor = Some((pos, EdgeCursor::Slip));
                    } else if select_hover
                        && let Some(pos) = resp.hover_pos()
                        && transition_handle_at(pos, clip_rect, transition_in_x, transition_out_x)
                            .is_some()
                    {
                        ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
                    } else if select_hover
                        && show_fades
                        && let Some(pos) = resp.hover_pos()
                        && fade_zone_at(pos, clip_rect, fade_in_x, fade_out_x).is_some()
                    {
                        ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
                    } else if select_hover
                        && !visual.locked
                        && let Some(pos) = resp.hover_pos()
                        && let Some(zone) = edge_at(pos)
                    {
                        let cursor = if retime_bar_shown && zone.edge() == TrimEdge::End {
                            EdgeCursor::Retime
                        } else {
                            EdgeCursor::from_zone(zone)
                        };
                        edge_cursor = Some((pos, cursor));
                    } else if select_hover
                        && !visual.locked
                        && let Some(pos) = resp.hover_pos()
                        && volume_hit(pos)
                    {
                        ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeVertical);
                    }

                    let marker_at = |edge: FadeEdge| match edge {
                        FadeEdge::In => left_marker.as_ref(),
                        FadeEdge::Out => right_marker.as_ref(),
                    };
                    let owns_crossing = |d: &CrossingDragState| {
                        [&left_marker, &right_marker]
                            .into_iter()
                            .flatten()
                            .any(|m| {
                                m.selection
                                    == TransitionSelection::Crossing(d.track_index, d.left_clip)
                            })
                    };
                    if resp.drag_started() && slip_tool {
                        begin_slip(state, &visuals, project, &track_kinds, visual);
                    } else if resp.drag_started() {
                        // `press_origin` and not the current position: egui declares the drag after a
                        // small movement, and towards the inside one would already have left the zone
                        // of the edge.
                        let press_pos = ui.input(|i| i.pointer.press_origin());
                        let transition_handle = press_pos.and_then(|p| {
                            transition_handle_at(p, clip_rect, transition_in_x, transition_out_x)
                        });
                        // Alt+drag on the body (not on the handle) of an already
                        // present marker duplicates instead of resizing — the handle
                        // keeps priority, as the fade ignores Alt on its own.
                        let transition_duplicate_edge = if ui.input(|i| i.modifiers.alt) {
                            press_pos.and_then(|p| {
                                if transition_in_x.is_some_and(|x| {
                                    transition_body_hit(p, clip_rect, FadeEdge::In, x)
                                }) {
                                    Some(FadeEdge::In)
                                } else if transition_out_x.is_some_and(|x| {
                                    transition_body_hit(p, clip_rect, FadeEdge::Out, x)
                                }) {
                                    Some(FadeEdge::Out)
                                } else {
                                    None
                                }
                            })
                        } else {
                            None
                        };
                        let fade_zone = if show_fades {
                            press_pos
                                .and_then(|p| fade_zone_at(p, clip_rect, fade_in_x, fade_out_x))
                        } else {
                            None
                        };
                        match transition_handle.and_then(|edge| marker_at(edge).map(|m| (edge, m)))
                        {
                            Some((edge, m)) => match m.selection {
                                TransitionSelection::Crossing(track_index, left_clip) => {
                                    begin_crossing_drag(
                                        state,
                                        project,
                                        timeline_id,
                                        track_index,
                                        left_clip,
                                        edge,
                                    );
                                }
                                TransitionSelection::Edge(..) => {
                                    begin_transition_drag(state, visual, edge)
                                }
                            },
                            None => match transition_duplicate_edge.and_then(marker_at) {
                                Some(m) => begin_transition_duplicate_drag(
                                    state,
                                    &resp,
                                    project,
                                    timeline_id,
                                    visual,
                                    m,
                                ),
                                None => match fade_zone {
                                    Some(edge) => begin_fade_drag(state, visual, edge),
                                    None => match press_pos.and_then(edge_at) {
                                        Some(zone)
                                            if zone.edge() == TrimEdge::End && retime_bar_shown =>
                                        {
                                            begin_retime(
                                                state,
                                                &visuals,
                                                project,
                                                timeline_fps,
                                                visual,
                                            )
                                        }
                                        Some(zone) => {
                                            begin_trim(state, &visuals, project, visual, zone)
                                        }
                                        None if press_pos.is_some_and(volume_hit) => {
                                            begin_volume_drag(state, history, visual)
                                        }
                                        None => begin_drag(
                                            state,
                                            &visuals,
                                            visual,
                                            ui.input(|i| i.modifiers.alt),
                                        ),
                                    },
                                },
                            },
                        }
                    } else if resp.dragged() {
                        let delta = resp.drag_delta();
                        match &mut state.gesture {
                            Some(Gesture::TransitionLength(d)) if d.clip_id == visual.clip.id => {
                                d.accum_px += delta.x;
                            }
                            Some(Gesture::CrossingLength(d)) if owns_crossing(d) => {
                                d.accum_px += delta.x
                            }
                            Some(Gesture::Fade(d)) if d.clip_id == visual.clip.id => {
                                d.accum_px += delta.x
                            }
                            Some(Gesture::Volume(d)) if d.clip_id == visual.clip.id => {
                                d.accum_px += delta.y;
                                // Unlike fade/trim/move, it is applied right here,
                                // on every drag frame (see `VolumeDragState`).
                                pending = Some(PendingAction::SetGain {
                                    track_index: d.track_index,
                                    clip_id: d.clip_id,
                                    new_value: volume_drag_value(d, clip_rect.height() / 2.0),
                                });
                            }
                            Some(Gesture::Trim(t)) if t.clip_id == visual.clip.id => {
                                t.accum_px += delta.x
                            }
                            Some(Gesture::Retime(r)) if r.clip_id == visual.clip.id => {
                                r.accum_px += delta.x
                            }
                            Some(Gesture::Move(d)) if d.clip_id == visual.clip.id => {
                                d.accum_px += delta.x
                            }
                            Some(Gesture::Slip(s)) if s.clip_id == visual.clip.id => {
                                s.accum_px += delta.x
                            }
                            _ => {}
                        }
                    } else if resp.drag_stopped() {
                        gesture_finished = true;
                        match &state.gesture {
                            Some(Gesture::TransitionLength(d)) if d.clip_id == visual.clip.id => {
                                pending = Some(PendingAction::SetTransitionDuration {
                                    track_index: d.track_index,
                                    clip_id: d.clip_id,
                                    edge: d.edge,
                                    new_value: edge_drag_value(
                                        d,
                                        visual.clip.timeline_len,
                                        px_per_frame,
                                        1,
                                    ),
                                });
                            }
                            Some(Gesture::CrossingLength(d)) if owns_crossing(d) => {
                                pending = Some(PendingAction::SetCrossingDuration {
                                    track_index: d.track_index,
                                    left_clip: d.left_clip,
                                    new_value: crossing_drag_value(d, px_per_frame),
                                });
                            }
                            // The real drop, if any, is handled by
                            // `dnd_release_payload` in the clip it landed on.
                            Some(Gesture::DuplicateTransition(clip_id))
                                if *clip_id == visual.clip.id => {}
                            Some(Gesture::Fade(d)) if d.clip_id == visual.clip.id => {
                                pending = Some(PendingAction::SetFade {
                                    track_index: d.track_index,
                                    clip_id: d.clip_id,
                                    edge: d.edge,
                                    new_value: edge_drag_value(
                                        d,
                                        visual.clip.timeline_len,
                                        px_per_frame,
                                        0,
                                    ),
                                });
                            }
                            Some(Gesture::Volume(d)) if d.clip_id == visual.clip.id => {
                                pending = Some(PendingAction::SetGain {
                                    track_index: d.track_index,
                                    clip_id: d.clip_id,
                                    new_value: volume_drag_value(d, clip_rect.height() / 2.0),
                                });
                                volume_drag_group = Some(d.group);
                            }
                            Some(Gesture::Retime(r)) if r.clip_id == visual.clip.id => {
                                let len = retimed_new_len.unwrap_or(r.original_len);
                                pending = Some(PendingAction::SetSpeed {
                                    clips: r.group.clone(),
                                    speed: r.speed_for(len),
                                    pitch_correction: r.pitch_correction,
                                    resize_to: Some((r.start + r.original_len, r.start + len)),
                                });
                            }
                            Some(Gesture::Trim(t)) if t.clip_id == visual.clip.id => {
                                pending = Some(finish_trim(
                                    t,
                                    visual,
                                    &visuals,
                                    trimmed_primary_new_value,
                                ));
                            }
                            Some(Gesture::Move(d)) if d.clip_id == visual.clip.id => {
                                pending = Some(finish_drag(
                                    d,
                                    drag_group_targets.as_deref().unwrap(),
                                    &track_kinds,
                                    dragged_primary_new_start,
                                ));
                            }
                            Some(Gesture::Slip(s)) if s.clip_id == visual.clip.id => {
                                pending = Some(PendingAction::Slip {
                                    clips: s.group.iter().map(|&(key, _, _)| key).collect(),
                                    delta: s.delta_for(px_per_frame),
                                });
                            }
                            _ => gesture_finished = false,
                        }
                    } else if resp.double_clicked() {
                        if let ClipSource::Media(media_id) = visual.clip.source
                            && let Some(nested_id) =
                                project.media_pool.get(media_id).and_then(|m| m.compound)
                        {
                            enter_compound = Some(nested_id);
                        }
                    } else if resp.clicked() {
                        let clicked_transition = resp.interact_pointer_pos().and_then(|pos| {
                            if transition_in_x.is_some_and(|x| {
                                transition_body_hit(pos, clip_rect, FadeEdge::In, x)
                            }) {
                                left_marker.as_ref()
                            } else if transition_out_x.is_some_and(|x| {
                                transition_body_hit(pos, clip_rect, FadeEdge::Out, x)
                            }) {
                                right_marker.as_ref()
                            } else {
                                None
                            }
                        });
                        if let Some(m) = clicked_transition {
                            // Replaces any clip selection, even a multiple one.
                            state.selected.clear();
                            state.selection_anchor = None;
                            state.selected_gap = None;
                            state.selected_transition = Some(m.selection);
                        } else {
                            let modifiers = click_modifiers(ui.input(|i| i.modifiers));
                            let (selected, anchor) = apply_click_selection(
                                &state.selected,
                                state.selection_anchor,
                                (visual.track_index, visual.clip.id),
                                modifiers,
                                &visuals,
                                px_per_frame,
                                &row_y,
                                row_height,
                            );
                            state.selected = expand_to_linked_groups(&visuals, selected);
                            state.selection_anchor = anchor;
                            state.selected_gap = None;
                            state.selected_transition = None;
                        }
                    }

                    resp.context_menu(|ui| {
                        if visual.clip.linked_group.is_some() {
                            if ui.button(t!("timeline.unlink")).clicked() {
                                pending =
                                    Some(PendingAction::Unlink(visual.track_index, visual.clip.id));
                                ui.close();
                            }
                        } else if state.selected.len() >= 2 {
                            if ui.button(t!("timeline.link")).clicked() {
                                pending = Some(PendingAction::Link(
                                    state.selected.iter().copied().collect(),
                                ));
                                ui.close();
                            }
                        } else {
                            ui.label(t!("timeline.link_hint"));
                        }
                        ui.separator();
                        if ui
                            .add_enabled(
                                !state.clipboard.is_empty() && !state.selected.is_empty(),
                                egui::Button::new(t!("timeline.paste_attributes")),
                            )
                            .clicked()
                        {
                            state.paste_attributes_requested = true;
                            ui.close();
                        }
                        ui.menu_button(t!("timeline.clip_color"), |ui| {
                            let targets = || {
                                color_targets(&visuals, state, visual.track_index, visual.clip.id)
                            };
                            if let Some(color) = clip_color_grid(ui, visual.clip.display_color) {
                                pending = Some(PendingAction::SetDisplayColor(targets(), color));
                                ui.close();
                            }
                        });
                        if let ClipSource::Media(media) = visual.clip.source {
                            ui.separator();
                            if ui
                                .add_enabled(
                                    project.media_pool.contains_key(media),
                                    egui::Button::new(t!("timeline.show_in_media_pool")),
                                )
                                .clicked()
                            {
                                state.reveal_in_pool_requested = Some(media);
                                ui.ctx().request_repaint();
                                ui.close();
                            }
                            let key = (visual.track_index, visual.clip.id);
                            let targets = || -> Vec<ClipKey> {
                                let base = if state.selected.contains(&key) {
                                    state.selected.clone()
                                } else {
                                    BTreeSet::from([key])
                                };
                                expand_to_linked_groups(&visuals, base)
                                    .into_iter()
                                    .filter(|&(track, id)| {
                                        visuals.iter().any(|v| {
                                            v.track_index == track
                                                && v.clip.id == id
                                                && matches!(v.clip.source, ClipSource::Media(_))
                                        })
                                    })
                                    .collect()
                            };
                            let mut shown = state.retime_controls.contains(&visual.clip.id);
                            if ui
                                .checkbox(&mut shown, t!("timeline.retime_controls"))
                                .clicked()
                            {
                                for (_, id) in targets() {
                                    if shown {
                                        state.retime_controls.insert(id);
                                    } else {
                                        state.retime_controls.remove(&id);
                                    }
                                }
                                ui.close();
                            }
                            ui.menu_button(t!("timeline.clip_speed"), |ui| {
                                let current = match visual.clip.freeze {
                                    Some(_) => SpeedChoice::Freeze,
                                    None => SpeedChoice::of(visual.clip.speed()),
                                };
                                let choices = [
                                    (
                                        SpeedChoice::Freeze,
                                        format!("{} (0%)", t!("timeline.freeze_frame")),
                                    ),
                                    (SpeedChoice::Percent(50), "50%".to_owned()),
                                    (SpeedChoice::Percent(100), "100%".to_owned()),
                                    (SpeedChoice::Percent(200), "200%".to_owned()),
                                    (SpeedChoice::Advanced, t!("timeline.speed_advanced").into()),
                                ];
                                for (choice, label) in choices {
                                    if !ui.radio(current == choice, label).clicked() {
                                        continue;
                                    }
                                    ui.close();
                                    match choice {
                                        SpeedChoice::Advanced => {
                                            state.speed_dialog_requested = Some(targets());
                                        }
                                        _ if choice == current => {}
                                        SpeedChoice::Freeze => {
                                            pending = Some(PendingAction::SetFreeze {
                                                clips: targets(),
                                                at: Some(state.playhead),
                                            });
                                        }
                                        SpeedChoice::Percent(percent) => {
                                            pending = Some(PendingAction::SetSpeed {
                                                clips: targets(),
                                                speed: vv_core::Rational::from_percent(f64::from(
                                                    percent,
                                                )),
                                                pitch_correction: visual.clip.pitch_correction,
                                                resize_to: None,
                                            });
                                        }
                                    }
                                }
                            });
                            let has_audio = targets()
                                .iter()
                                .any(|&(track, _)| track_kinds[track] == TrackKind::Audio);
                            if ui
                                .add_enabled(
                                    has_audio,
                                    egui::Button::new(t!("timeline.remove_silences")),
                                )
                                .clicked()
                            {
                                state.silence_dialog_requested = Some(targets());
                                ui.close();
                            }
                        }
                        ui.separator();
                        if ui.button(t!("timeline.make_compound_clip")).clicked() {
                            // A right-click on an unselected clip acts only
                            // on it, not on the stale previous selection.
                            let base = if state.selected.is_empty() {
                                BTreeSet::from([(visual.track_index, visual.clip.id)])
                            } else {
                                state.selected.clone()
                            };
                            let selection = expand_to_linked_groups(&visuals, base);
                            pending =
                                Some(PendingAction::MakeCompound(selection.into_iter().collect()));
                            ui.close();
                        }
                    });
                }
                // See the docs of `pending_crossing_previews`: only now, with the
                // drawing of all the clips finished, can no later `paint_clip_box`
                // cover it any more.
                for (track_index, clip_id, edge) in pending_crossing_previews {
                    paint_mirrored_marker_on_neighbor(
                        &track_painter(track_index),
                        &visuals,
                        origin,
                        &row_y,
                        row_height,
                        px_per_frame,
                        track_index,
                        clip_id,
                        edge,
                    );
                }
                // Cleared only now, not with a `.take()` halfway through the loop
                // above — see the comment on `gesture_finished`.
                if gesture_finished {
                    state.gesture = None;
                }
                if let Some(t) = state.trimming()
                    && let Some(pos) = ui.input(|i| i.pointer.latest_pos())
                {
                    let cursor = match (t.roll, t.edge) {
                        (true, _) => EdgeCursor::Roll,
                        (false, TrimEdge::Start) => EdgeCursor::TrimStart,
                        (false, TrimEdge::End) => EdgeCursor::TrimEnd,
                    };
                    edge_cursor = Some((pos, cursor));
                }
                if state.retiming().is_some()
                    && let Some(pos) = ui.input(|i| i.pointer.latest_pos())
                {
                    edge_cursor = Some((pos, EdgeCursor::Retime));
                }
                if let Some(slip) = state.slipping() {
                    for &((track_index, clip_id), own_min, own_max) in &slip.group {
                        let Some(v) = visuals
                            .iter()
                            .find(|v| v.track_index == track_index && v.clip.id == clip_id)
                        else {
                            continue;
                        };
                        let media_start = v.clip.timeline_start + own_min - slip.delta;
                        let media_end = v.clip.timeline_end() + own_max - slip.delta;
                        let y = origin.y + row_y[track_index];
                        let extent = egui::Rect::from_min_max(
                            egui::pos2(origin.x + media_start as f32 * px_per_frame, y + 1.0),
                            egui::pos2(
                                origin.x + media_end as f32 * px_per_frame,
                                y + row_height - 1.0,
                            ),
                        );
                        track_painter(track_index).rect_stroke(
                            extent,
                            4.0,
                            egui::Stroke::new(1.5, egui::Color32::WHITE),
                            egui::StrokeKind::Inside,
                        );
                    }
                    if let Some(pos) = ui.input(|i| i.pointer.latest_pos()) {
                        edge_cursor = Some((pos, EdgeCursor::Slip));
                        let shift = -slip.delta;
                        let sign = if shift < 0 { "-" } else { "+" };
                        paint_pointer_overlay(
                            ui.ctx(),
                            pos,
                            format!("{sign}{}", format_duration(shift.abs(), fps)),
                        );
                    }
                }
                if let Some((pos, cursor)) = edge_cursor {
                    paint_edge_cursor(ui.ctx(), pos, cursor);
                }
                if let (Some(r), Some(len)) = (state.retiming(), retimed_new_len)
                    && let Some(pos) = ui.input(|i| i.pointer.latest_pos())
                {
                    paint_pointer_overlay(ui.ctx(), pos, format_speed(r.speed_for(len)));
                }

                if let Some(take) = &state.recording {
                    for &track_index in take.tracks.iter().filter(|t| **t < track_count) {
                        let x0 = origin.x + take.start as f32 * px_per_frame;
                        let x1 = origin.x + state.playhead as f32 * px_per_frame;
                        let y = origin.y + row_y[track_index];
                        let area = egui::Rect::from_min_max(
                            egui::pos2(x0, y + 1.0),
                            egui::pos2(x1.max(x0), y + row_height - 1.0),
                        );
                        let painter = track_painter(track_index);
                        painter.rect_filled(area, 3.0, crate::theme::ERROR.gamma_multiply(0.35));
                        paint_recording_waveform(
                            &painter,
                            area,
                            &take.peaks,
                            px_per_frame as f64 * fps,
                        );
                        painter.rect_stroke(
                            area,
                            3.0,
                            egui::Stroke::new(1.5, crate::theme::ERROR),
                            egui::StrokeKind::Inside,
                        );
                        let label = painter.layout_no_wrap(
                            "REC".to_owned(),
                            egui::FontId::proportional(10.0),
                            egui::Color32::WHITE,
                        );
                        let badge = egui::Rect::from_min_size(
                            area.left_top() + egui::vec2(3.0, 3.0),
                            label.size() + egui::vec2(6.0, 2.0),
                        );
                        painter.rect_filled(badge, 2.0, crate::theme::ERROR);
                        painter.galley(
                            badge.min + egui::vec2(3.0, 1.0),
                            label,
                            egui::Color32::WHITE,
                        );
                    }
                }
                paint_playhead(
                    &painter,
                    origin,
                    state.playhead as f32 * px_per_frame,
                    visual_height,
                );

                // On the visible right edge, not on the content's one.
                let scrollbar_x = egui::Rangef::new(
                    ui.clip_rect().right() - PANE_SCROLLBAR_WIDTH - 2.0,
                    ui.clip_rect().right() - 2.0,
                );
                let scrollbar_rect = |pane: egui::Rangef| {
                    egui::Rect::from_x_y_ranges(
                        scrollbar_x,
                        (origin.y + pane.min + 2.0)..=(origin.y + pane.max - 2.0),
                    )
                };
                let video_max = layout.video_max_scroll;
                if let Some(offset) = pane_scrollbar(
                    ui,
                    &painter,
                    scrollbar_rect(layout.video_pane),
                    ui.id().with("timeline_video_vscroll"),
                    video_max - state.video_scroll,
                    video_max,
                ) {
                    state.video_scroll = video_max - offset;
                }
                if let Some(offset) = pane_scrollbar(
                    ui,
                    &painter,
                    scrollbar_rect(layout.audio_pane),
                    ui.id().with("timeline_audio_vscroll"),
                    state.audio_scroll,
                    layout.audio_max_scroll,
                ) {
                    state.audio_scroll = offset;
                }
            });
    });

    if let Some(action) = pending {
        apply_pending_action(project, history, state, timeline_id, action);
    }
    if let Some(mark) = volume_drag_group {
        history.end_group(mark);
    }

    (media_drop, enter_compound)
}

/// Start and length to draw `visual` with: during a trim or a drag
/// the preview position, otherwise the real one.
fn display_range(
    visual: &ClipVisual,
    state: &TimelineState,
    is_trimming_this: bool,
    trimmed_primary_new_value: Option<FrameIdx>,
    dragged_primary_new_start: Option<FrameIdx>,
    retimed_new_len: Option<FrameIdx>,
) -> (FrameIdx, FrameIdx) {
    if let (Some(r), Some(len)) = (state.retiming(), retimed_new_len)
        && r.group.contains(&(visual.track_index, visual.clip.id))
    {
        let scaled = visual.clip.timeline_len as f64 * len as f64 / r.original_len.max(1) as f64;
        return (
            visual.clip.timeline_start,
            (scaled.round() as FrameIdx).max(1),
        );
    }
    if is_trimming_this
        && let (Some(t), Some(primary_value)) = (state.trimming(), trimmed_primary_new_value)
    {
        let (offset, edge) = t
            .followers
            .iter()
            .find(|&&(id, track, _, _)| id == visual.clip.id && track == visual.track_index)
            .map_or((0, t.edge), |&(_, _, offset, edge)| (offset, edge));
        let new_value = primary_value + offset;
        match edge {
            TrimEdge::Start => (new_value, (visual.clip.timeline_end() - new_value).max(1)),
            TrimEdge::End => (
                visual.clip.timeline_start,
                (new_value - visual.clip.timeline_start).max(1),
            ),
        }
    } else {
        let start = match (state.moving(), dragged_primary_new_start) {
            (Some(d), Some(new_start)) if d.clip_id == visual.clip.id => new_start,
            (Some(d), Some(new_start)) => match d
                .followers
                .iter()
                .find(|(id, track, _)| *id == visual.clip.id && *track == visual.track_index)
            {
                Some((_, _, offset)) => new_start + offset,
                None => visual.clip.timeline_start,
            },
            _ => visual.clip.timeline_start,
        };
        (start, visual.clip.timeline_len)
    }
}

/// Fill and border of a clip.
fn paint_clip_box(
    painter: &egui::Painter,
    clip_rect: egui::Rect,
    visual: &ClipVisual,
    is_selected: bool,
) {
    let stroke = if is_selected {
        egui::Stroke::new(2.0, egui::Color32::WHITE)
    } else {
        egui::Stroke::new(1.0, egui::Color32::from_gray(15))
    };
    let fill = if visual.muted {
        egui::Color32::from_gray(58)
    } else {
        visual.color
    };
    let corner = clip_corner_radius(clip_rect);
    painter.rect_filled(clip_rect, corner, fill);
    // On very narrow clips (zoomed far out) the stroke would eat all the
    // colour: only the more informative selection stroke is kept.
    if is_selected || clip_rect.width() >= MIN_WIDTH_FOR_STROKE {
        painter.rect_stroke(clip_rect, corner, stroke, egui::StrokeKind::Inside);
    } else {
        paint_clip_separator(painter, clip_rect);
    }
}

/// Dark line exactly one pixel wide on the right edge: on clips narrower
/// than the full stroke it only shows where the clip ends.
fn paint_clip_separator(painter: &egui::Painter, clip_rect: egui::Rect) {
    let px = 1.0 / painter.pixels_per_point();
    painter.rect_filled(
        egui::Rect::from_min_max(
            egui::pos2(clip_rect.right() - px, clip_rect.top()),
            clip_rect.max,
        ),
        0.0,
        egui::Color32::from_gray(15),
    );
}

/// Rounded corners only while the clip is wide enough: below that, the
/// radius would deform the whole rectangle.
fn clip_corner_radius(clip_rect: egui::Rect) -> f32 {
    (clip_rect.width() / 2.0).min(4.0)
}

const MIN_WIDTH_FOR_STROKE: f32 = 4.0;
const LABEL_FONT_SIZE: f32 = 12.0;
const LINK_ICON_WIDTH: f32 = 16.0;
/// Below this many visible characters the name does not help recognise
/// the clip: better not to draw it at all.
const LABEL_MIN_CHARS: usize = 4;

/// Name truncated to `max_width`, omitted if fewer than
/// `LABEL_MIN_CHARS` characters fit.
fn paint_clip_label(
    painter: &egui::Painter,
    label: &str,
    pos: egui::Pos2,
    max_width: f32,
    color: egui::Color32,
) {
    let font = egui::FontId::proportional(LABEL_FONT_SIZE);
    let min_text: String = label.chars().take(LABEL_MIN_CHARS).collect();
    if min_text.is_empty() {
        return;
    }
    let min_width = painter
        .layout_no_wrap(min_text, font.clone(), color)
        .size()
        .x;
    if max_width < min_width {
        return;
    }
    let mut job = egui::text::LayoutJob::simple_singleline(label.to_owned(), font, color);
    job.wrap = egui::text::TextWrapping::truncate_at_width(max_width);
    let galley = painter.layout_job(job);
    painter.galley(pos, galley, color);
}

/// Label, "disabled" badge, link icon and veil of the locked
/// tracks, on top of the waveform.
fn paint_clip_overlay(
    painter: &egui::Painter,
    clip_rect: egui::Rect,
    visual: &ClipVisual,
    is_proxy_backed: bool,
    top_inset: f32,
) {
    let veil_rect = clip_rect;
    let clip_rect = clip_rect.with_min_y(clip_rect.top() + top_inset);
    let label_offset_y = if is_proxy_backed {
        paint_proxy_strip(painter, clip_rect);
        2.0 + PROXY_STRIP_HEIGHT
    } else {
        2.0
    };
    let mut label_pos = clip_rect.left_top() + egui::vec2(4.0, label_offset_y);
    if visual.clip.disabled {
        paint_disabled_badge(painter, label_pos);
        label_pos.x += DISABLED_BADGE_SIZE + 4.0;
    }
    let label_color = if visual.muted {
        egui::Color32::from_gray(185)
    } else {
        egui::Color32::BLACK
    };
    let show_link_icon = visual.clip.linked_group.is_some() && clip_rect.width() > LINK_ICON_WIDTH;
    let reserved_right = if show_link_icon { LINK_ICON_WIDTH } else { 4.0 };
    paint_clip_label(
        painter,
        &visual.label,
        label_pos,
        clip_rect.right() - reserved_right - label_pos.x,
        label_color,
    );
    if show_link_icon {
        // Two hand-drawn rings: on some platforms (Asahi) egui's fonts do not
        // have 🔗.
        let center = clip_rect.right_top() + egui::vec2(-9.0, 8.0);
        let ring_color = if visual.muted {
            egui::Color32::from_gray(185)
        } else {
            egui::Color32::BLACK
        };
        let ring_stroke = egui::Stroke::new(1.3, ring_color);
        painter.circle_stroke(center + egui::vec2(-2.5, 0.0), 3.5, ring_stroke);
        painter.circle_stroke(center + egui::vec2(2.5, 0.0), 3.5, ring_stroke);
    }
    if visual.locked {
        painter.rect_filled(
            veil_rect,
            4.0,
            egui::Color32::from_rgba_unmultiplied(70, 70, 70, 140),
        );
    }
}

/// Triangular shadow of the fade (from the corner towards the handle) plus the
/// dot of the handle itself. `x` is the current position of the handle,
/// `corner_x` the corner (left for the fade-in, right for the fade-out) the
/// triangle starts from.
fn paint_fade_wedge(
    painter: &egui::Painter,
    clip_rect: egui::Rect,
    corner_x: f32,
    x: f32,
    dragging: bool,
) {
    let top = clip_rect.top();
    painter.add(egui::Shape::convex_polygon(
        vec![
            egui::pos2(corner_x, top),
            egui::pos2(x, top),
            egui::pos2(corner_x, clip_rect.bottom()),
        ],
        egui::Color32::from_black_alpha(110),
        egui::Stroke::NONE,
    ));
    let center = egui::pos2(x, top + FADE_HANDLE_ZONE_HEIGHT * 0.5);
    let radius = if dragging {
        FADE_HANDLE_RADIUS + 1.0
    } else {
        FADE_HANDLE_RADIUS
    };
    painter.circle_filled(center, radius, egui::Color32::WHITE);
    painter.circle_stroke(
        center,
        radius,
        egui::Stroke::new(1.0, egui::Color32::from_gray(40)),
    );
}

/// Overlay with the duration (fade, single transition or crossing)
/// near the pointer, during the drag of its end: same "always on top"
/// layer as the trim cursor. General behavior, not only
/// for the fades: any draggable end shows it.
fn paint_duration_overlay(ctx: &egui::Context, pos: egui::Pos2, frames: FrameIdx, fps: f64) {
    paint_pointer_overlay(ctx, pos, format!("+{}", format_duration(frames, fps)));
}

fn paint_pointer_overlay(ctx: &egui::Context, pos: egui::Pos2, text: String) {
    let painter = ctx.layer_painter(egui::LayerId::new(
        egui::Order::Tooltip,
        egui::Id::new("timeline_duration_overlay"),
    ));
    let text_pos = pos + egui::vec2(12.0, 14.0);
    let galley =
        painter.layout_no_wrap(text, egui::FontId::proportional(12.0), egui::Color32::WHITE);
    let bg = egui::Rect::from_min_size(text_pos, galley.size()).expand(3.0);
    painter.rect_filled(bg, 3.0, egui::Color32::from_black_alpha(200));
    painter.galley(text_pos, galley, egui::Color32::WHITE);
}

/// `S:FF`: duration in seconds and remaining frames, not a timeline
/// position (no hours/minutes, these durations are always short).
fn format_duration(frames: FrameIdx, fps: f64) -> String {
    let nominal = (fps.round() as i64).max(1);
    let frames = frames.max(0);
    let (secs, f) = (frames / nominal, frames % nominal);
    format!("{secs}:{f:02}")
}

fn begin_retime(
    state: &mut TimelineState,
    visuals: &[ClipVisual],
    project: &Project,
    timeline_fps: vv_core::Rational,
    visual: &ClipVisual,
) {
    let clip = &visual.clip;
    let conform = match clip.source {
        ClipSource::Media(id) => project.media_pool.get(id).map(|m| m.meta.fps),
        _ => None,
    }
    .map_or(vv_core::Rational::one(), |fps| {
        vv_core::Rational::conform_rate(timeline_fps, fps)
    });
    state.gesture = Some(Gesture::Retime(RetimeState {
        clip_id: clip.id,
        start: clip.timeline_start,
        original_len: clip.timeline_len,
        len_at_100: conform.scale_round(clip.source_out()) - conform.scale_round(clip.source_in()),
        pitch_correction: visual.clip.pitch_correction,
        accum_px: 0.0,
        group: expand_to_linked_groups(visuals, [(visual.track_index, visual.clip.id)])
            .into_iter()
            .collect(),
    }));
}

enum RetimeBarAction {
    Close,
    Percent(f64),
    Dialog,
}

fn retime_bar_height(clip_rect: egui::Rect) -> f32 {
    RETIME_BAR_HEIGHT.min(clip_rect.height() / 2.0)
}

/// Presets of the speed menu of the retime bar, in percent.
/// The radio entries of the clip menu's speed submenu.
#[derive(Clone, Copy, PartialEq)]
enum SpeedChoice {
    Freeze,
    Percent(u32),
    /// Any speed without its own entry; opens the speed dialog.
    Advanced,
}

impl SpeedChoice {
    fn of(speed: vv_core::Rational) -> Self {
        [50, 100, 200]
            .into_iter()
            .find(|&p| speed == vv_core::Rational::from_percent(f64::from(p)))
            .map_or(Self::Advanced, Self::Percent)
    }
}

const SPEED_PRESETS: [f64; 10] = [
    10.0, 25.0, 50.0, 75.0, 100.0, 125.0, 150.0, 200.0, 400.0, 1000.0,
];

fn format_speed(speed: vv_core::Rational) -> String {
    let percent = speed.as_percent();
    if (percent - percent.round()).abs() < 0.005 {
        format!("{percent:.0}%")
    } else {
        format!("{percent:.2}%")
    }
}

/// The bar on top of a clip with its speed handles shown: the speed, with
/// the presets menu, and `×` to hide it.
fn retime_bar(
    ui: &egui::Ui,
    painter: &egui::Painter,
    clip_rect: egui::Rect,
    id: egui::Id,
    speed: vv_core::Rational,
    interactive: bool,
) -> Option<RetimeBarAction> {
    let height = retime_bar_height(clip_rect);
    let bar = egui::Rect::from_min_size(clip_rect.min, egui::vec2(clip_rect.width(), height));
    painter.rect_filled(bar, 2.0, crate::theme::ACCENT_FILL);
    let font = egui::FontId::proportional(11.0);
    let galley = painter.layout_no_wrap(
        format!("{} ⏷", format_speed(speed)),
        font.clone(),
        egui::Color32::WHITE,
    );
    // Centered on the visible part of the clip, however long or zoomed in.
    let visible = bar.intersect(painter.clip_rect());
    if visible.width() <= 0.0 {
        return None;
    }
    let menu_width = (galley.size().x + 8.0).min(visible.width());
    let menu_left = (visible.center().x - menu_width / 2.0).max(visible.left());
    let menu_rect = egui::Rect::from_min_size(
        egui::pos2(menu_left, bar.top()),
        egui::vec2(menu_width, height),
    );
    let close_rect = egui::Rect::from_min_size(
        egui::pos2(visible.right() - height, bar.top()),
        egui::vec2(height, height),
    );
    painter.with_clip_rect(visible).galley(
        menu_rect.min + egui::vec2(4.0, (height - galley.size().y) / 2.0),
        galley,
        egui::Color32::WHITE,
    );
    let show_close = visible.width() >= menu_width + 2.0 * close_rect.width();
    if show_close {
        painter.text(
            close_rect.center(),
            egui::Align2::CENTER_CENTER,
            "×",
            font,
            egui::Color32::WHITE,
        );
    }
    if !interactive {
        return None;
    }
    let mut action = None;
    if show_close
        && ui
            .interact(close_rect, id.with("retime_close"), egui::Sense::click())
            .clicked()
    {
        action = Some(RetimeBarAction::Close);
    }
    let menu = ui.interact(menu_rect, id.with("retime_menu"), egui::Sense::click());
    egui::Popup::menu(&menu).show(|ui| {
        for percent in SPEED_PRESETS {
            if ui.button(format!("{percent:.0}%")).clicked() {
                action = Some(RetimeBarAction::Percent(percent));
            }
        }
        ui.separator();
        if ui.button(t!("timeline.change_clip_speed")).clicked() {
            action = Some(RetimeBarAction::Dialog);
        }
    });
    action
}

/// Starts the trim of `visual` from the edge zone `zone`, with the clips
/// following it.
fn begin_trim(
    state: &mut TimelineState,
    visuals: &[ClipVisual],
    project: &Project,
    visual: &ClipVisual,
    zone: EdgeZone,
) {
    let edge = zone.edge();
    let key = (visual.track_index, visual.clip.id);
    let others: Vec<(ClipKey, TrimEdge)> = match zone {
        EdgeZone::Trim(_) => drag_group_for(&state.selected, visuals, key)
            .into_iter()
            .filter(|k| *k != key)
            .map(|k| (k, edge))
            .collect(),
        // Only the two clips in contact, each with its own linked group.
        EdgeZone::Roll { neighbor, .. } => {
            let opposite = match edge {
                TrimEdge::Start => TrimEdge::End,
                TrimEdge::End => TrimEdge::Start,
            };
            expand_to_linked_groups(visuals, [key])
                .into_iter()
                .filter(|k| *k != key)
                .map(|k| (k, edge))
                .chain(
                    expand_to_linked_groups(visuals, [neighbor])
                        .into_iter()
                        .map(|k| (k, opposite)),
                )
                .collect()
        }
    };
    let (min_value, max_value, followers) =
        combined_trim_range(visuals, project, key, edge, &others);
    let original_value = match edge {
        TrimEdge::Start => visual.clip.timeline_start,
        TrimEdge::End => visual.clip.timeline_end(),
    };
    state.gesture = Some(Gesture::Trim(TrimState {
        clip_id: visual.clip.id,
        track_index: visual.track_index,
        edge,
        original_value,
        accum_px: 0.0,
        min_value,
        max_value: max_value.max(min_value),
        followers,
        roll: matches!(zone, EdgeZone::Roll { .. }),
    }));
}

/// Starts slipping `visual` with its linked group. Generators have no media
/// to slide: the drag does nothing.
fn begin_slip(
    state: &mut TimelineState,
    visuals: &[ClipVisual],
    project: &Project,
    track_kinds: &[TrackKind],
    visual: &ClipVisual,
) {
    let key = (visual.track_index, visual.clip.id);
    if vv_core::edit::slip_range(project, &visual.clip).is_none() {
        return;
    }
    let mut group = Vec::new();
    let (mut min_delta, mut max_delta) = (FrameIdx::MIN, FrameIdx::MAX);
    let mut video: Option<&Clip> = None;
    for k in expand_to_linked_groups(visuals, [key]) {
        let Some(v) = visuals.iter().find(|v| (v.track_index, v.clip.id) == k) else {
            continue;
        };
        let Some((lo, hi)) = vv_core::edit::slip_range(project, &v.clip) else {
            continue;
        };
        min_delta = min_delta.max(lo);
        max_delta = max_delta.min(hi);
        if track_kinds[k.0] == TrackKind::Video && (video.is_none() || k == key) {
            video = Some(&v.clip);
        }
        group.push((k, lo, hi));
    }
    state.selected = group.iter().map(|&(k, _, _)| k).collect();
    state.selection_anchor = Some(key);
    state.selected_gap = None;
    state.selected_transition = None;
    state.gesture = Some(Gesture::Slip(SlipState {
        clip_id: visual.clip.id,
        accum_px: 0.0,
        group,
        min_delta,
        max_delta,
        delta: 0,
        video: video.cloned().map(Box::new),
    }));
}

/// Starts dragging `visual` together with its selection group.
fn begin_drag(
    state: &mut TimelineState,
    visuals: &[ClipVisual],
    visual: &ClipVisual,
    duplicate: bool,
) {
    let drag_group = drag_group_for(
        &state.selected,
        visuals,
        (visual.track_index, visual.clip.id),
    );
    state.selected = drag_group.clone();
    state.selection_anchor = Some((visual.track_index, visual.clip.id));

    let others: Vec<ClipKey> = drag_group.into_iter().collect();
    let (_, _, followers) =
        combined_drag_range(visuals, visual.track_index, visual.clip.id, &others);

    state.gesture = Some(Gesture::Move(DragState {
        clip_id: visual.clip.id,
        track_index: visual.track_index,
        original_start: visual.clip.timeline_start,
        accum_px: 0.0,
        followers,
        duplicate,
    }));
}

/// Starts dragging the fade-in/fade-out handle of `visual`: no
/// group nor neighbor involved, it is always local to the clip.
fn begin_fade_drag(state: &mut TimelineState, visual: &ClipVisual, edge: FadeEdge) {
    let original_value = match edge {
        FadeEdge::In => visual.clip.fade_in,
        FadeEdge::Out => visual.clip.fade_out,
    };
    state.gesture = Some(Gesture::Fade(EdgeDragState {
        clip_id: visual.clip.id,
        track_index: visual.track_index,
        edge,
        original_value,
        accum_px: 0.0,
    }));
}

/// Length (in frames, clamped to `min..=clip_len`) of the preview of a fade
/// or transition drag in progress: dragging the handle towards the inside of
/// the clip lengthens it — opposite directions on the X axis for `In` and
/// `Out`. `min` is 0 for a fade, which can be dragged away, 1 for a
/// transition, which cannot.
fn edge_drag_value(
    d: &EdgeDragState,
    clip_len: FrameIdx,
    px_per_frame: f32,
    min: FrameIdx,
) -> FrameIdx {
    let signed_delta = match d.edge {
        FadeEdge::In => d.accum_px,
        FadeEdge::Out => -d.accum_px,
    };
    (d.original_value as f32 + signed_delta / px_per_frame)
        .round()
        .clamp(min as f32, clip_len.max(min) as f32) as FrameIdx
}

/// `(fade_in, fade_out)` to show for `visual`: the preview of the drag in
/// progress if it concerns it, otherwise the already saved values.
fn fade_preview(
    state: &TimelineState,
    visual: &ClipVisual,
    px_per_frame: f32,
) -> (FrameIdx, FrameIdx) {
    let mut fade_in = visual.clip.fade_in;
    let mut fade_out = visual.clip.fade_out;
    if let Some(d) = state.fading()
        && d.clip_id == visual.clip.id
    {
        let value = edge_drag_value(d, visual.clip.timeline_len, px_per_frame, 0);
        match d.edge {
            FadeEdge::In => fade_in = value,
            FadeEdge::Out => fade_out = value,
        }
    }
    (fade_in, fade_out)
}

/// Fade handle under `pos`, only in the band at the top of the clip: below
/// stays the trim/roll of the existing edge.
fn fade_zone_at(
    pos: egui::Pos2,
    clip_rect: egui::Rect,
    fade_in_x: f32,
    fade_out_x: f32,
) -> Option<FadeEdge> {
    if pos.y > clip_rect.top() + FADE_HANDLE_ZONE_HEIGHT {
        return None;
    }
    if (pos.x - fade_in_x).abs() <= FADE_HANDLE_HIT_RADIUS {
        return Some(FadeEdge::In);
    }
    if (pos.x - fade_out_x).abs() <= FADE_HANDLE_HIT_RADIUS {
        return Some(FadeEdge::Out);
    }
    None
}

/// Starts dragging the end (duration) of an already present
/// transition: like `begin_fade_drag`, always local to the single clip.
fn begin_transition_drag(state: &mut TimelineState, visual: &ClipVisual, edge: FadeEdge) {
    let original_value = match edge {
        FadeEdge::In => visual.clip.effects.transition_in.as_ref(),
        FadeEdge::Out => visual.clip.effects.transition_out.as_ref(),
    }
    .map_or(0, |t| t.duration);
    state.gesture = Some(Gesture::TransitionLength(EdgeDragState {
        clip_id: visual.clip.id,
        track_index: visual.track_index,
        edge,
        original_value,
        accum_px: 0.0,
    }));
}

/// The clip adjacent to `clip_id` on the side `edge`, on the same track: `In`
/// looks for whoever touches its start, `Out` for whoever touches its end. `None` if
/// `clip_id` does not exist or has no neighbors on that side.
fn adjacent_clip(track: &vv_core::Track, clip_id: ClipId, edge: FadeEdge) -> Option<ClipId> {
    let clip = track.clip(clip_id)?;
    let at = match edge {
        FadeEdge::In => clip.timeline_start,
        FadeEdge::Out => clip.timeline_end(),
    };
    track
        .clips
        .iter()
        .find(|c| {
            c.id != clip_id
                && match edge {
                    FadeEdge::In => c.timeline_end() == at,
                    FadeEdge::Out => c.timeline_start == at,
                }
        })
        .map(|c| c.id)
}

/// Creates (or replaces) the crossing transition between `left_id` and `right_id`,
/// clamping `transition.duration` to what the two clips can really
/// "lend" it (twice the shorter of the two, see
/// `CrossTransition::split`), and selects it.
fn apply_new_crossing(
    project: &mut Project,
    history: &mut History,
    state: &mut TimelineState,
    timeline_id: TimelineId,
    track_index: usize,
    left_id: ClipId,
    right_id: ClipId,
    mut transition: vv_core::Transition,
) {
    let track = &project.timelines[timeline_id].tracks[track_index];
    let (Some(left), Some(right)) = (track.clip(left_id), track.clip(right_id)) else {
        return;
    };
    let max_duration = (2 * left.timeline_len.min(right.timeline_len)).max(1);
    transition.duration = transition.duration.clamp(1, max_duration);
    let crossing = vv_core::CrossTransition {
        left_clip: left_id,
        right_clip: right_id,
        transition,
    };
    history.do_command(
        project,
        Box::new(vv_core::SetCrossTransition::new(
            timeline_id,
            track_index,
            left_id,
            Some(crossing),
        )),
    );
    state.selected.clear();
    state.selection_anchor = None;
    state.selected_gap = None;
    state.selected_transition = Some(TransitionSelection::Crossing(track_index, left_id));
}

/// Starts a symmetric drag of the duration of a crossing transition:
/// `edge` is the edge of *this* clip the drag starts from — `Out` means
/// that this clip is the `left_clip` of the crossing (the end grabbed
/// is the one inside it), `In` that it is the `right_clip` — consistent with
/// `Track::crossing_from`/`crossing_into` used by `edge_marker`.
fn begin_crossing_drag(
    state: &mut TimelineState,
    project: &Project,
    timeline_id: TimelineId,
    track_index: usize,
    left_clip: ClipId,
    edge: FadeEdge,
) {
    let track = &project.timelines[timeline_id].tracks[track_index];
    let Some(crossing) = track.crossing_from(left_clip) else {
        return;
    };
    let (Some(left), Some(right)) = (
        track.clip(crossing.left_clip),
        track.clip(crossing.right_clip),
    ) else {
        return;
    };
    let max_duration = (2 * left.timeline_len.min(right.timeline_len)).max(1);
    state.gesture = Some(Gesture::CrossingLength(CrossingDragState {
        track_index,
        left_clip,
        grabbed_left_side: edge == FadeEdge::Out,
        original_duration: crossing.transition.duration,
        max_duration,
        accum_px: 0.0,
    }));
}

/// Starts an Alt+drag duplication from the body of a transition
/// marker (single edge or crossing): the DnD payload must be set
/// right here, not in `resp.dragged()` as one might think by
/// analogy with the rest of the file — `Response::dnd_set_drag_payload` acts
/// only if `drag_started()`, not on every drag frame (the library then
/// keeps it alive by itself for the duration of the drag, see `egui::DragAndDrop`).
fn begin_transition_duplicate_drag(
    state: &mut TimelineState,
    resp: &egui::Response,
    project: &Project,
    timeline_id: TimelineId,
    visual: &ClipVisual,
    marker: &EdgeMarker,
) {
    let transition = match marker.selection {
        TransitionSelection::Edge(_, edge) => match edge {
            FadeEdge::In => visual.clip.effects.transition_in.clone(),
            FadeEdge::Out => visual.clip.effects.transition_out.clone(),
        },
        TransitionSelection::Crossing(track_index, left_clip) => project.timelines[timeline_id]
            .tracks[track_index]
            .crossing_from(left_clip)
            .map(|c| c.transition.clone()),
    };
    if let Some(transition) = transition {
        resp.dnd_set_drag_payload(transition);
    }
    state.gesture = Some(Gesture::DuplicateTransition(visual.clip.id));
}

/// Total duration (in frames) of the preview of a crossing drag in progress:
/// symmetric, dragging the left end to the left lengthens it
/// (and the right one to the right in the same way), always by twice
/// the displacement in frames — it grows/shrinks on the two sides equally.
fn crossing_drag_value(d: &CrossingDragState, px_per_frame: f32) -> FrameIdx {
    // Like `edge_drag_value`: the left end is an "Out" edge
    // (inside the left clip, it grows by dragging it to the left,
    // away from the cut), the right one an "In" edge (inside the right
    // clip, it grows by dragging it to the right) — here doubled in addition
    // on the other side, see above.
    let signed_delta = if d.grabbed_left_side {
        -d.accum_px
    } else {
        d.accum_px
    };
    (d.original_duration as f32 + 2.0 * signed_delta / px_per_frame)
        .round()
        .clamp(1.0, d.max_duration.max(1) as f32) as FrameIdx
}

/// What to show/select on the edge `edge` of `visual.clip`: a single
/// edge (`EffectStack::transition_in`/`_out`), or — if that edge is
/// shared with a valid crossing transition — its own half of
/// that. The duration reflects the preview of a resize drag
/// in progress on this edge, if there is one.
struct EdgeMarker {
    duration: FrameIdx,
    selection: TransitionSelection,
    is_crossing: bool,
}

fn edge_marker(
    track: &vv_core::Track,
    state: &TimelineState,
    visual: &ClipVisual,
    px_per_frame: f32,
    edge: FadeEdge,
) -> Option<EdgeMarker> {
    let crossing = match edge {
        FadeEdge::In => track.crossing_into(visual.clip.id),
        FadeEdge::Out => track.crossing_from(visual.clip.id),
    };
    if let Some(crossing) = crossing {
        let total = if let Some(d) = state.sizing_crossing()
            && d.track_index == visual.track_index
            && d.left_clip == crossing.left_clip
        {
            crossing_drag_value(d, px_per_frame)
        } else {
            crossing.transition.duration
        };
        let (split_left, split_right) = vv_core::CrossTransition::split_duration(total);
        let duration = match edge {
            FadeEdge::Out => split_left,
            FadeEdge::In => split_right,
        };
        return Some(EdgeMarker {
            duration,
            selection: TransitionSelection::Crossing(visual.track_index, crossing.left_clip),
            is_crossing: true,
        });
    }
    let transition = match edge {
        FadeEdge::In => visual.clip.effects.transition_in.as_ref(),
        FadeEdge::Out => visual.clip.effects.transition_out.as_ref(),
    }?;
    let mut duration = transition.duration;
    if let Some(d) = state.sizing_transition()
        && d.clip_id == visual.clip.id
        && d.edge == edge
    {
        duration = edge_drag_value(d, visual.clip.timeline_len, px_per_frame, 1);
    }
    Some(EdgeMarker {
        duration,
        selection: TransitionSelection::Edge((visual.track_index, visual.clip.id), edge),
        is_crossing: false,
    })
}

/// End (duration) of a transition under `pos`, only in the band at the
/// bottom of the clip — mirroring `fade_zone_at`. `None` for an edge that
/// has no transition yet: nothing to drag there.
fn transition_handle_at(
    pos: egui::Pos2,
    clip_rect: egui::Rect,
    in_x: Option<f32>,
    out_x: Option<f32>,
) -> Option<FadeEdge> {
    if pos.y < clip_rect.bottom() - TRANSITION_HANDLE_ZONE_HEIGHT {
        return None;
    }
    if let Some(x) = in_x
        && (pos.x - x).abs() <= TRANSITION_HANDLE_HIT_RADIUS
    {
        return Some(FadeEdge::In);
    }
    if let Some(x) = out_x
        && (pos.x - x).abs() <= TRANSITION_HANDLE_HIT_RADIUS
    {
        return Some(FadeEdge::Out);
    }
    None
}

/// `true` if `pos` falls in the body of a transition marker (from the edge
/// of the clip to its end `x`), not only on its handle: a
/// click anywhere there selects it, no need to aim at the end.
fn transition_body_hit(pos: egui::Pos2, clip_rect: egui::Rect, edge: FadeEdge, x: f32) -> bool {
    if pos.y < clip_rect.bottom() - TRANSITION_HANDLE_ZONE_HEIGHT {
        return false;
    }
    match edge {
        FadeEdge::In => pos.x >= clip_rect.left() && pos.x <= x,
        FadeEdge::Out => pos.x <= clip_rect.right() && pos.x >= x,
    }
}

/// Edge of `clip_rect` nearest to `pos`, within `drop_zone_px` — shared
/// by the drop of a `TransitionKind` from the Effects panel and by the drop of a
/// whole `Transition` (duplication via Alt+drag): the same "only
/// near an edge" rule in both cases.
fn transition_drop_edge(
    pos: egui::Pos2,
    clip_rect: egui::Rect,
    drop_zone_px: f32,
) -> Option<FadeEdge> {
    if pos.x - clip_rect.left() <= drop_zone_px {
        Some(FadeEdge::In)
    } else if clip_rect.right() - pos.x <= drop_zone_px {
        Some(FadeEdge::Out)
    } else {
        None
    }
}

/// The `ClipVisual` adjacent to `clip_id` on the side `edge`, on the same
/// track — `None` if there is none. Used during the drag of a
/// transition to anticipate, through the marker color, whether releasing there
/// will create a crossing or stay a single edge (see `CROSSING_COLOR`),
/// and to draw the mirror marker on the neighboring clip too (see
/// `paint_mirrored_marker_on_neighbor`).
fn neighbor_visual<'a, 'b>(
    visuals: &'a [ClipVisual<'b>],
    track_index: usize,
    clip_id: ClipId,
    edge: FadeEdge,
) -> Option<&'a ClipVisual<'b>> {
    let visual = visuals
        .iter()
        .find(|v| v.track_index == track_index && v.clip.id == clip_id)?;
    let at = match edge {
        FadeEdge::In => visual.clip.timeline_start,
        FadeEdge::Out => visual.clip.timeline_end(),
    };
    visuals.iter().find(|v| {
        v.track_index == track_index
            && v.clip.id != clip_id
            && match edge {
                FadeEdge::In => v.clip.timeline_end() == at,
                FadeEdge::Out => v.clip.timeline_start == at,
            }
    })
}

/// Neither the clip nor the neighbor it would cross with may be an
/// adjustment clip (see `Track::crossing_at`).
fn accepts_transition(visuals: &[ClipVisual], visual: &ClipVisual, edge: FadeEdge) -> bool {
    !visual.clip.is_adjustment()
        && neighbor_visual(visuals, visual.track_index, visual.clip.id, edge)
            .is_none_or(|n| !n.clip.is_adjustment())
}

fn has_neighbor(
    visuals: &[ClipVisual],
    track_index: usize,
    clip_id: ClipId,
    edge: FadeEdge,
) -> bool {
    neighbor_visual(visuals, track_index, clip_id, edge).is_some()
}

/// If there is an adjacent clip on the side `edge`, it draws the mirror
/// marker on it too (opposite edge): once released, a crossing already shows
/// like that on both clips (see the docs of `paint_transition_marker`) —
/// showing it only on the clip under the pointer during the drag would be
/// misleading. It must be called AFTER the loop drawing all the clips (see
/// `pending_crossing_previews`): the neighbor may come later in `draw_order`,
/// and its own `paint_clip_box` would cover it if drawn during
/// the iteration of the clip under the pointer. The neighboring clip is never in
/// a drag/trim when this function is called (one drag at a
/// time, and this is the drag of a `TransitionKind`/`Transition` from the
/// Effects panel), so its static rectangle is enough.
fn paint_mirrored_marker_on_neighbor(
    painter: &egui::Painter,
    visuals: &[ClipVisual],
    origin: egui::Pos2,
    row_y: &[f32],
    row_height: f32,
    px_per_frame: f32,
    track_index: usize,
    clip_id: ClipId,
    edge: FadeEdge,
) {
    let Some(neighbor) = neighbor_visual(visuals, track_index, clip_id, edge) else {
        return;
    };
    let x = origin.x + neighbor.clip.timeline_start as f32 * px_per_frame;
    let y = origin.y + row_y[track_index];
    let w = (neighbor.clip.timeline_len as f32 * px_per_frame).max(2.0);
    let neighbor_rect =
        egui::Rect::from_min_size(egui::pos2(x, y + 2.0), egui::vec2(w, row_height - 4.0));
    let drop_zone_px = TRANSITION_DROP_ZONE_PX.min(neighbor_rect.width() / 2.0);
    let opposite = match edge {
        FadeEdge::In => FadeEdge::Out,
        FadeEdge::Out => FadeEdge::In,
    };
    let nx = match opposite {
        FadeEdge::In => neighbor_rect.left() + drop_zone_px,
        FadeEdge::Out => neighbor_rect.right() - drop_zone_px,
    };
    paint_transition_marker(painter, neighbor_rect, opposite, nx, false, true);
}

/// The marker of a transition: a colored band at the bottom of the clip from the
/// edge to `x`. A single edge (`is_crossing: false`) shows a
/// gear on the fixed side (the real edge of the clip, against
/// transparency) and a bracket on the end `x` (the draggable side) —
/// "X]" for `In`, "[X" for `Out`. A crossing (`is_crossing: true`) has no
/// "fixed" side: the edge shared with the neighbor shows another
/// bracket, opening towards the inside of its own half — the two halves,
/// drawn one per clip, sit side by side there as "][".
fn paint_transition_marker(
    painter: &egui::Painter,
    clip_rect: egui::Rect,
    edge: FadeEdge,
    x: f32,
    selected: bool,
    is_crossing: bool,
) {
    let x = x.clamp(clip_rect.left(), clip_rect.right());
    let band = egui::Rect::from_min_max(
        egui::pos2(
            clip_rect.left(),
            clip_rect.bottom() - TRANSITION_HANDLE_ZONE_HEIGHT,
        ),
        clip_rect.max,
    );
    let body = match edge {
        FadeEdge::In => egui::Rect::from_min_max(band.min, egui::pos2(x, band.bottom())),
        FadeEdge::Out => egui::Rect::from_min_max(egui::pos2(x, band.top()), band.max),
    };
    let color = match (is_crossing, selected) {
        (false, false) => TRANSITION_COLOR,
        (false, true) => TRANSITION_SELECTED_COLOR,
        (true, false) => CROSSING_COLOR,
        (true, true) => CROSSING_SELECTED_COLOR,
    };
    painter.rect_filled(body, 0.0, color);
    let edge_x = match edge {
        FadeEdge::In => clip_rect.left(),
        FadeEdge::Out => clip_rect.right(),
    };
    if is_crossing {
        let opposite = match edge {
            FadeEdge::In => FadeEdge::Out,
            FadeEdge::Out => FadeEdge::In,
        };
        paint_bracket_icon(
            painter,
            egui::pos2(edge_x, band.center().y),
            band.height() * 0.7,
            opposite,
            egui::Color32::WHITE,
        );
    } else {
        paint_gear_icon(
            painter,
            egui::pos2(edge_x, band.center().y),
            band.height() * 0.4,
            egui::Color32::WHITE,
        );
    }
    paint_bracket_icon(
        painter,
        egui::pos2(x, band.center().y),
        band.height() * 0.7,
        edge,
        egui::Color32::WHITE,
    );
}

/// Hand-drawn bracket (no Unicode glyph, see `paint_gear_icon`):
/// a vertical line with two notches opening towards the fixed edge of the
/// clip (`In`: notches on the left, towards the gear; `Out`: on the right).
pub(crate) fn paint_bracket_icon(
    painter: &egui::Painter,
    center: egui::Pos2,
    height: f32,
    edge: FadeEdge,
    color: egui::Color32,
) {
    let stroke = egui::Stroke::new(1.6, color);
    let half = height / 2.0;
    let tick = height * 0.3;
    let tick_dir = match edge {
        FadeEdge::In => -1.0,
        FadeEdge::Out => 1.0,
    };
    painter.line_segment(
        [
            egui::pos2(center.x, center.y - half),
            egui::pos2(center.x, center.y + half),
        ],
        stroke,
    );
    painter.line_segment(
        [
            egui::pos2(center.x, center.y - half),
            egui::pos2(center.x + tick * tick_dir, center.y - half),
        ],
        stroke,
    );
    painter.line_segment(
        [
            egui::pos2(center.x, center.y + half),
            egui::pos2(center.x + tick * tick_dir, center.y + half),
        ],
        stroke,
    );
}

/// Transitions of the Effects panel, in the order they appear there — see
/// `FILTER_ENTRIES`, same idea.
pub const ALL_TRANSITION_KINDS: [vv_core::TransitionKind; 1] = [vv_core::TransitionKind::Push];

pub fn transition_kind_label(kind: vv_core::TransitionKind) -> std::borrow::Cow<'static, str> {
    match kind {
        vv_core::TransitionKind::Push => t!("transition.push"),
    }
}

pub fn push_direction_label(direction: vv_core::PushDirection) -> std::borrow::Cow<'static, str> {
    match direction {
        vv_core::PushDirection::Left => t!("transition.direction_left"),
        vv_core::PushDirection::Right => t!("transition.direction_right"),
        vv_core::PushDirection::Up => t!("transition.direction_up"),
        vv_core::PushDirection::Down => t!("transition.direction_down"),
    }
}

pub fn ease_label(ease: vv_core::Ease) -> std::borrow::Cow<'static, str> {
    match ease {
        vv_core::Ease::None => t!("transition.ease_none"),
        vv_core::Ease::In => t!("transition.ease_in"),
        vv_core::Ease::Out => t!("transition.ease_out"),
        vv_core::Ease::InOut => t!("transition.ease_in_out"),
    }
}

/// Normalized vertical offset (-1 at the bottom, +1 at the top) of the volume
/// line for a gain in dB: centered at 0 dB, the two branches use different scales
/// because `GAIN_DB_MIN`/`GAIN_DB_MAX` are not symmetric.
fn gain_offset(db: f32) -> f32 {
    if db >= 0.0 {
        (db / vv_core::GAIN_DB_MAX).clamp(0.0, 1.0)
    } else {
        -(db / vv_core::GAIN_DB_MIN).clamp(0.0, 1.0)
    }
}

/// Inverse of `gain_offset`.
fn gain_from_offset(offset: f32) -> f32 {
    let offset = offset.clamp(-1.0, 1.0);
    if offset >= 0.0 {
        offset * vv_core::GAIN_DB_MAX
    } else {
        -offset * vv_core::GAIN_DB_MIN
    }
}

/// Y coordinate of the volume line for a gain in dB.
fn gain_line_y(db: f32, clip_rect: egui::Rect) -> f32 {
    clip_rect.center().y - gain_offset(db) * clip_rect.height() / 2.0
}

fn paint_gain_line(painter: &egui::Painter, clip_rect: egui::Rect, line_y: f32, dragging: bool) {
    let alpha = if dragging { 220 } else { 130 };
    let stroke = egui::Stroke::new(
        1.0,
        egui::Color32::from_rgba_unmultiplied(255, 255, 255, alpha),
    );
    painter.line_segment(
        [
            egui::pos2(clip_rect.left(), line_y),
            egui::pos2(clip_rect.right(), line_y),
        ],
        stroke,
    );
}

/// The volume line is as wide as the clip but thin: it is grabbed within
/// `VOLUME_LINE_HIT_PX` vertically, no narrow horizontal test is needed.
fn volume_line_hit(pos: egui::Pos2, clip_rect: egui::Rect, line_y: f32) -> bool {
    clip_rect.x_range().contains(pos.x) && (pos.y - line_y).abs() <= VOLUME_LINE_HIT_PX
}

/// Starts dragging the volume line: like the fade, local to the
/// single clip. It opens the undo group that will collect the `SetGain`s of
/// every drag frame (see `VolumeDragState::group`).
fn begin_volume_drag(state: &mut TimelineState, history: &mut History, visual: &ClipVisual) {
    state.gesture = Some(Gesture::Volume(VolumeDragState {
        clip_id: visual.clip.id,
        track_index: visual.track_index,
        original_db: visual.clip.effects.gain_db.default,
        accum_px: 0.0,
        group: history.begin_group(),
    }));
}

/// Gain (dB) of the preview of a volume line drag in progress: the drag is
/// vertical and linear in the drawn "offset" space, not in dB, so the
/// line follows the pointer exactly along the whole run.
fn volume_drag_value(d: &VolumeDragState, half_height: f32) -> f32 {
    if half_height <= 0.0 {
        return d.original_db;
    }
    let offset = gain_offset(d.original_db) - d.accum_px / half_height;
    gain_from_offset(offset)
}

/// Trim to apply on release: the same (already clamped) value shown
/// in the preview.
fn finish_trim(
    t: &TrimState,
    visual: &ClipVisual,
    visuals: &[ClipVisual],
    trimmed_primary_new_value: Option<FrameIdx>,
) -> PendingAction {
    let new_value = trimmed_primary_new_value.unwrap_or(t.original_value);
    let mut trims = vec![(t.clip_id, t.track_index, t.edge, new_value)];
    let mut overwritten: Vec<_> =
        vv_core::edit::grown_range(&visual.clip, visual.track_index, t.edge, new_value)
            .into_iter()
            .collect();
    for &(other_id, other_track, offset, edge) in &t.followers {
        let Some(other) = visuals
            .iter()
            .find(|v| v.clip.id == other_id && v.track_index == other_track)
        else {
            continue;
        };
        let other_value = new_value + offset;
        trims.push((other_id, other_track, edge, other_value));
        overwritten.extend(vv_core::edit::grown_range(
            &other.clip,
            other_track,
            edge,
            other_value,
        ));
    }
    PendingAction::Trim { trims, overwritten }
}

/// Move to apply on release, at the same position shown
/// during the drag.
fn finish_drag(
    d: &DragState,
    targets: &[(ClipId, EffectiveTrack)],
    track_kinds: &[TrackKind],
    dragged_primary_new_start: Option<FrameIdx>,
) -> PendingAction {
    let new_start = dragged_primary_new_start.unwrap_or(d.original_start);
    let original_tracks =
        std::iter::once(d.track_index).chain(d.followers.iter().map(|(_, t, _)| *t));
    let starts = std::iter::once(new_start)
        .chain(d.followers.iter().map(|(_, _, offset)| new_start + offset));

    let mut new_video_tracks = 0usize;
    let mut new_audio_tracks = 0usize;
    let moves: Vec<(ClipId, usize, EffectiveTrack, FrameIdx)> = targets
        .iter()
        .zip(original_tracks)
        .zip(starts)
        .map(|(((id, target), from_track), start)| {
            if let EffectiveTrack::New(depth) = *target {
                let count = match track_kinds[from_track] {
                    TrackKind::Video => &mut new_video_tracks,
                    TrackKind::Audio => &mut new_audio_tracks,
                };
                *count = (*count).max(depth);
            }
            (*id, from_track, *target, start)
        })
        .collect();
    PendingAction::Move {
        new_video_tracks,
        new_audio_tracks,
        moves,
        duplicate: d.duplicate,
    }
}

/// Applies one kinetic scroll step to `scroll_val`: damps `vel` (px/s)
/// with the same friction physics as egui's native drag-to-scroll, and
/// zeroes the speed if the resulting scroll hits a limit.
/// Returns `true` if `scroll_val` was updated (a repaint is needed).
pub(crate) fn apply_kinetic_scroll(
    scroll_val: &mut f32,
    vel: &mut f32,
    max_scroll: f32,
    dt: f32,
) -> bool {
    if *vel == 0.0 {
        return false;
    }
    let friction = KINETIC_FRICTION * dt;
    if friction > vel.abs() || vel.abs() < KINETIC_STOP_SPEED {
        *vel = 0.0;
        return false;
    }
    *vel -= friction * vel.signum();
    let raw = *scroll_val + *vel * dt;
    let clamped = raw.clamp(0.0, max_scroll);
    if clamped != raw {
        *vel = 0.0;
    }
    *scroll_val = clamped;
    true
}

/// Corrects the saved scroll of the ScrollArea `scroll_id` before its `show`:
/// on zoom the playhead stays still on screen, during playback it stays visible;
/// it also handles the horizontal kinetic touchpad scroll (swipe + inertia
/// after release), since the vertical wheel scroll is already consumed
/// elsewhere for the Video/Audio boxes.
fn sync_timeline_scroll(
    ctx: &egui::Context,
    scroll_id: egui::Id,
    state: &mut TimelineState,
    fps: f64,
    px_per_frame: f32,
    viewport_width: f32,
    content_width: f32,
    playback_active: bool,
    panel_rect: egui::Rect,
    kinetic_scroll_enabled: bool,
) {
    // Zoom changed in this frame: the saved scroll of the
    // ScrollArea (same id) is corrected before the `show`, so the playhead stays still on
    // screen.
    if state.pixels_per_sec != state.last_rendered_pps
        && let Some(mut scroll_state) = egui::containers::scroll_area::State::load(ctx, scroll_id)
    {
        let playhead_secs = state.playhead as f64 / fps;
        scroll_state.offset.x +=
            (playhead_secs as f32) * (state.pixels_per_sec - state.last_rendered_pps);
        scroll_state.store(ctx, scroll_id);
    }
    state.last_rendered_pps = state.pixels_per_sec;

    // During playback (or after a programmatic jump) the playhead stays visible:
    // if it leaves, the view "turns the page" bringing it to a third from the
    // left. Clamp like egui's in `begin`.
    let reveal = std::mem::take(&mut state.reveal_playhead);
    if playback_active || reveal {
        let playhead_x = state.playhead as f32 * px_per_frame;
        let visible_start = match egui::containers::scroll_area::State::load(ctx, scroll_id) {
            Some(st) => st.offset.x,
            None => 0.0,
        };
        let visible_end = visible_start + viewport_width;
        const FOLLOW_MARGIN_FRAC: f32 = 1.0 / 3.0;
        if playhead_x < visible_start || playhead_x > visible_end {
            let target = (playhead_x - viewport_width * FOLLOW_MARGIN_FRAC)
                .clamp(0.0, (content_width - viewport_width).max(0.0));
            if let Some(mut scroll_state) =
                egui::containers::scroll_area::State::load(ctx, scroll_id)
            {
                scroll_state.offset.x = target;
                scroll_state.store(ctx, scroll_id);
            }
        }
    }

    let max_offset_x = (content_width - viewport_width).max(0.0);
    let dt = ctx.input(|i| i.stable_dt).min(0.1);
    // During playback the view follows the playhead: a residual inertia would
    // make it slide away from the point it just centered it on above.
    if !kinetic_scroll_enabled || playback_active {
        state.hscroll_vel = 0.0;
    }
    let hovering_panel = pointer_over(ctx, panel_rect);
    // Horizontal swipe in progress: applied immediately (as the
    // ScrollArea would), and its instantaneous speed becomes the inertia to
    // damp when the gesture ends.
    let wheel_x = if hovering_panel {
        ctx.input(|i| i.smooth_scroll_delta.x)
    } else {
        0.0
    };
    if wheel_x != 0.0 {
        if let Some(mut scroll_state) = egui::containers::scroll_area::State::load(ctx, scroll_id) {
            scroll_state.offset.x = (scroll_state.offset.x - wheel_x).clamp(0.0, max_offset_x);
            scroll_state.store(ctx, scroll_id);
        }
        state.hscroll_vel = if kinetic_scroll_enabled && !playback_active && dt > 0.0 {
            -KINETIC_VELOCITY_GAIN * wheel_x / dt
        } else {
            0.0
        };
        // Consumed here: the ScrollArea must not reapply it in its `show`.
        ctx.input_mut(|i| i.smooth_scroll_delta.x = 0.0);
    } else if let Some(mut scroll_state) =
        egui::containers::scroll_area::State::load(ctx, scroll_id)
    {
        let mut offset_x = scroll_state.offset.x;
        if apply_kinetic_scroll(&mut offset_x, &mut state.hscroll_vel, max_offset_x, dt) {
            scroll_state.offset.x = offset_x;
            scroll_state.store(ctx, scroll_id);
            ctx.request_repaint();
        }
    }
}

/// Ruler: ticks, cached frames strip and export markers; click and
/// drag move the playhead.
fn show_ruler(
    ui: &mut egui::Ui,
    painter: &egui::Painter,
    origin: egui::Pos2,
    content_width: f32,
    state: &mut TimelineState,
    visuals: &[ClipVisual],
    fps: f64,
    px_per_frame: f32,
    snapping_enabled: bool,
    buffered_ranges: &[(FrameIdx, FrameIdx)],
    max_end_frames: FrameIdx,
    pending: &mut Option<PendingAction>,
) {
    let ruler_rect = egui::Rect::from_min_size(origin, egui::vec2(content_width, RULER_HEIGHT));
    painter.rect_filled(ruler_rect, 0.0, egui::Color32::from_gray(45));
    painter.rect_filled(
        markers::lane_rect(origin, content_width),
        0.0,
        egui::Color32::from_gray(38),
    );
    let ruler_resp = ui.interact(
        ruler_rect,
        ui.id().with("timeline_ruler"),
        egui::Sense::click_and_drag(),
    );
    ruler_resp.context_menu(|ui| {
        if ui.button(t!("timeline.add_marker")).clicked() {
            *pending = Some(PendingAction::Marker(markers::MarkerChange::AddAtPlayhead));
            ui.close();
        }
    });
    // On a click what counts is where it was released: if the frame
    // arrived late, `interact_pointer_pos` is already
    // the last mouse position after the release.
    let ruler_pos = if ruler_resp.clicked() {
        ui.input(|i| {
            i.events.iter().rev().find_map(|e| match e {
                egui::Event::PointerButton {
                    pos,
                    button: egui::PointerButton::Primary,
                    pressed: false,
                    ..
                } => Some(*pos),
                _ => None,
            })
        })
        .or_else(|| ruler_resp.interact_pointer_pos())
    } else {
        ruler_resp.interact_pointer_pos()
    };
    if let Some(pos) = ruler_pos {
        let raw_frame = (((pos.x - origin.x) / px_per_frame).round() as FrameIdx).max(0);
        state.playhead = snap_frame(
            raw_frame,
            0,
            visuals,
            &[],
            &[],
            px_per_frame,
            snapping_enabled,
        );
    }

    // Only the visible ticks: a long timeline zoomed to the frame would have
    // thousands of them off screen.
    let visible_x = ui.clip_rect().intersect(ruler_rect);
    draw_ruler_ticks(painter, origin, visible_x, state.pixels_per_sec, fps);

    // Below the playhead line, so it stays visible.
    const BUFFERED_STRIP_HEIGHT: f32 = 4.0;
    let buffered_color = egui::Color32::from_rgba_unmultiplied(120, 190, 255, 140);
    for &(start, end) in buffered_ranges {
        let x0 = origin.x + start as f32 * px_per_frame;
        let x1 = origin.x + (end + 1) as f32 * px_per_frame;
        let strip_rect = egui::Rect::from_min_max(
            egui::pos2(x0, origin.y + RULER_TICKS_HEIGHT - BUFFERED_STRIP_HEIGHT),
            egui::pos2(x1, origin.y + RULER_TICKS_HEIGHT),
        );
        painter.rect_filled(strip_rect, 0.0, buffered_color);
    }

    if !state.export_marks.is_full(max_end_frames) {
        let (mark_in, mark_out) = state.export_marks.resolve(max_end_frames);
        let band = egui::Rect::from_min_max(
            egui::pos2(origin.x + mark_in as f32 * px_per_frame, origin.y),
            egui::pos2(
                origin.x + mark_out as f32 * px_per_frame,
                origin.y + RULER_TICKS_HEIGHT - BUFFERED_STRIP_HEIGHT,
            ),
        );
        painter.rect_filled(
            band,
            0.0,
            egui::Color32::from_rgba_unmultiplied(255, 255, 255, 28),
        );
        for x in [band.left(), band.right()] {
            painter.vline(
                x,
                band.y_range(),
                egui::Stroke::new(1.0, egui::Color32::from_gray(185)),
            );
        }
    }
}

/// The peaks recorded so far across `area`, from its left edge, at
/// `px_per_sec`.
fn paint_recording_waveform(
    painter: &egui::Painter,
    area: egui::Rect,
    peaks: &[f32],
    px_per_sec: f64,
) {
    let peaks_per_px = RECORDING_PEAKS_PER_SEC / px_per_sec.max(1e-9);
    let half = (area.height() - 6.0).max(0.0) / 2.0;
    let mid = area.center().y;
    let stroke = egui::Stroke::new(1.0, egui::Color32::from_white_alpha(210));
    let mut x = area.left();
    while x < area.right() {
        let column = (x - area.left()) as f64;
        let from = (column * peaks_per_px) as usize;
        let to = (((column + 1.0) * peaks_per_px) as usize)
            .max(from + 1)
            .min(peaks.len());
        if from >= peaks.len() {
            break;
        }
        let peak = peaks[from..to]
            .iter()
            .copied()
            .fold(0.0f32, f32::max)
            .min(1.0);
        let h = (peak * half).max(0.5);
        painter.line_segment([egui::pos2(x, mid - h), egui::pos2(x, mid + h)], stroke);
        x += 1.0;
    }
}

/// Playhead line plus a triangular head in the ruler.
fn paint_playhead(painter: &egui::Painter, origin: egui::Pos2, x_offset: f32, visual_height: f32) {
    let px = origin.x + x_offset;
    let playhead_color = crate::theme::PLAYHEAD;
    painter.line_segment(
        [
            egui::pos2(px, origin.y),
            egui::pos2(px, origin.y + visual_height),
        ],
        egui::Stroke::new(2.0, playhead_color),
    );
    const PLAYHEAD_HEAD_HALF_WIDTH: f32 = 6.0;
    painter.add(egui::Shape::convex_polygon(
        vec![
            egui::pos2(px - PLAYHEAD_HEAD_HALF_WIDTH, origin.y),
            egui::pos2(px + PLAYHEAD_HEAD_HALF_WIDTH, origin.y),
            egui::pos2(px, origin.y + RULER_TICKS_HEIGHT),
        ],
        playhead_color,
        egui::Stroke::NONE,
    ));
}

fn apply_pending_action(
    project: &mut Project,
    history: &mut History,
    state: &mut TimelineState,
    timeline_id: TimelineId,
    action: PendingAction,
) {
    match action {
        PendingAction::Marker(change) => {
            markers::apply_change(project, history, timeline_id, state.playhead, change);
        }
        PendingAction::Move {
            new_video_tracks,
            new_audio_tracks,
            moves,
            duplicate,
        } => {
            // Creating in increasing depth order is enough: both for
            // video and for audio, the depth-th created ends up
            // by itself on the right row (see `EffectiveTrack::New`).
            let mut video_tracks = Vec::with_capacity(new_video_tracks);
            for _ in 0..new_video_tracks {
                video_tracks.push(add_track(project, history, timeline_id, TrackKind::Video));
            }
            let mut audio_tracks = Vec::with_capacity(new_audio_tracks);
            for _ in 0..new_audio_tracks {
                audio_tracks.push(add_track(project, history, timeline_id, TrackKind::Audio));
            }
            let moves: Vec<(ClipId, usize, usize, FrameIdx)> = moves
                .into_iter()
                .map(|(id, from_track, dest, start)| {
                    let to_track = match dest {
                        EffectiveTrack::Existing(track) => track,
                        // The kind of the new track is the starting one.
                        EffectiveTrack::New(depth) => {
                            match project.timelines[timeline_id].tracks[from_track].kind {
                                TrackKind::Video => video_tracks[depth - 1],
                                TrackKind::Audio => audio_tracks[depth - 1],
                            }
                        }
                    };
                    (id, from_track, to_track, start)
                })
                .collect();
            if duplicate {
                duplicate_clips(project, history, state, timeline_id, &moves);
                return;
            }
            vv_core::edit::move_clips(project, history, timeline_id, moves);
        }
        PendingAction::Slip { clips, delta } => {
            vv_core::edit::slip_clips(project, history, timeline_id, &clips, delta);
        }
        PendingAction::Trim { trims, overwritten } => {
            // The stretch gained by lengthening overwrites what was there; the trimmed
            // clips stay out, or they would cut themselves.
            let exclude: Vec<(usize, ClipId)> = trims
                .iter()
                .map(|&(clip_id, track_index, _, _)| (track_index, clip_id))
                .collect();
            let mut commands: Vec<Box<dyn vv_core::Command>> = Vec::new();
            vv_core::make_room_for_ranges(
                project,
                timeline_id,
                &overwritten,
                &exclude,
                &mut commands,
            );
            commands.extend(
                trims
                    .into_iter()
                    .map(|(clip_id, track_index, edge, new_value)| {
                        Box::new(vv_core::TrimClip::new(
                            timeline_id,
                            track_index,
                            clip_id,
                            edge,
                            new_value,
                        )) as Box<dyn vv_core::Command>
                    }),
            );
            history.do_command(
                project,
                Box::new(vv_core::CompositeCommand::new(
                    vv_core::CommandLabel::TrimClips,
                    commands,
                )),
            );
        }
        PendingAction::SetFade {
            track_index,
            clip_id,
            edge,
            new_value,
        } => {
            history.do_command(
                project,
                Box::new(vv_core::SetClipFade::new(
                    timeline_id,
                    track_index,
                    clip_id,
                    edge,
                    new_value,
                )),
            );
        }
        PendingAction::SetGain {
            track_index,
            clip_id,
            new_value,
        } => {
            history.do_command(
                project,
                Box::new(vv_core::set_clip_gain(
                    timeline_id,
                    track_index,
                    clip_id,
                    new_value,
                )),
            );
        }
        PendingAction::ApplyFilter {
            track_index,
            clip_id,
            filter,
        } => {
            // If it is already present (dropping it again) it is only re-enabled,
            // instead of being duplicated at the end.
            let new_filters = project.timelines[timeline_id]
                .clip(track_index, clip_id)
                .map(|clip| {
                    let mut filters = clip.effects.filters.clone();
                    let own = match filters.iter().position(|f| f.kind == filter.kind) {
                        Some(pos) => &mut filters[pos],
                        None => {
                            filters.push(vv_core::ClipFilter::new(filter.kind));
                            filters.last_mut().unwrap()
                        }
                    };
                    own.enabled = true;
                    if let Some(preset) = filter.preset {
                        own.grade.apply_preset(preset);
                    }
                    filters
                });
            if let Some(filters) = new_filters {
                history.do_command(
                    project,
                    Box::new(vv_core::set_clip_filters(
                        timeline_id,
                        track_index,
                        clip_id,
                        filters,
                    )),
                );
            }
        }
        PendingAction::ApplyTransition {
            track_index,
            clip_id,
            edge,
            kind,
        } => {
            let default_duration =
                (project.timelines[timeline_id].fps.as_f64() * 0.45).round() as FrameIdx;
            let transition = vv_core::Transition {
                kind,
                duration: default_duration.max(1),
                direction: vv_core::PushDirection::Right,
                ease: vv_core::Ease::InOut,
                curve: 0.5,
            };
            let neighbor = adjacent_clip(
                &project.timelines[timeline_id].tracks[track_index],
                clip_id,
                edge,
            );
            if let Some(neighbor_id) = neighbor {
                let (left_id, right_id) = match edge {
                    FadeEdge::In => (neighbor_id, clip_id),
                    FadeEdge::Out => (clip_id, neighbor_id),
                };
                apply_new_crossing(
                    project,
                    history,
                    state,
                    timeline_id,
                    track_index,
                    left_id,
                    right_id,
                    transition,
                );
            } else if let Some(clip) = project.timelines[timeline_id].clip(track_index, clip_id) {
                let mut transition = transition;
                transition.duration = transition.duration.clamp(1, clip.timeline_len.max(1));
                history.do_command(
                    project,
                    Box::new(vv_core::set_clip_transition(
                        timeline_id,
                        track_index,
                        clip_id,
                        edge,
                        Some(transition),
                    )),
                );
                state.selected.clear();
                state.selection_anchor = None;
                state.selected_gap = None;
                state.selected_transition =
                    Some(TransitionSelection::Edge((track_index, clip_id), edge));
            }
        }
        PendingAction::SetTransitionDuration {
            track_index,
            clip_id,
            edge,
            new_value,
        } => {
            if let Some(clip) = project.timelines[timeline_id].clip(track_index, clip_id) {
                let mut transition = match edge {
                    FadeEdge::In => clip.effects.transition_in.clone(),
                    FadeEdge::Out => clip.effects.transition_out.clone(),
                };
                if let Some(t) = &mut transition {
                    t.duration = new_value.clamp(1, clip.timeline_len.max(1));
                }
                if let Some(transition) = transition {
                    history.do_command(
                        project,
                        Box::new(vv_core::set_clip_transition(
                            timeline_id,
                            track_index,
                            clip_id,
                            edge,
                            Some(transition),
                        )),
                    );
                }
            }
        }
        PendingAction::SetCrossingDuration {
            track_index,
            left_clip,
            new_value,
        } => {
            let track = &project.timelines[timeline_id].tracks[track_index];
            if let Some(mut crossing) = track.crossing_from(left_clip).cloned() {
                let max_duration = match (
                    track.clip(crossing.left_clip),
                    track.clip(crossing.right_clip),
                ) {
                    (Some(left), Some(right)) => {
                        (2 * left.timeline_len.min(right.timeline_len)).max(1)
                    }
                    _ => new_value.max(1),
                };
                crossing.transition.duration = new_value.clamp(1, max_duration);
                history.do_command(
                    project,
                    Box::new(vv_core::SetCrossTransition::new(
                        timeline_id,
                        track_index,
                        left_clip,
                        Some(crossing),
                    )),
                );
            }
        }
        PendingAction::DuplicateTransition {
            track_index,
            clip_id,
            edge,
            transition,
        } => {
            let neighbor = adjacent_clip(
                &project.timelines[timeline_id].tracks[track_index],
                clip_id,
                edge,
            );
            if let Some(neighbor_id) = neighbor {
                let (left_id, right_id) = match edge {
                    FadeEdge::In => (neighbor_id, clip_id),
                    FadeEdge::Out => (clip_id, neighbor_id),
                };
                apply_new_crossing(
                    project,
                    history,
                    state,
                    timeline_id,
                    track_index,
                    left_id,
                    right_id,
                    transition,
                );
            } else if let Some(clip) = project.timelines[timeline_id].clip(track_index, clip_id) {
                let mut transition = transition;
                transition.duration = transition.duration.clamp(1, clip.timeline_len.max(1));
                history.do_command(
                    project,
                    Box::new(vv_core::set_clip_transition(
                        timeline_id,
                        track_index,
                        clip_id,
                        edge,
                        Some(transition),
                    )),
                );
                state.selected.clear();
                state.selection_anchor = None;
                state.selected_gap = None;
                state.selected_transition =
                    Some(TransitionSelection::Edge((track_index, clip_id), edge));
            }
        }
        PendingAction::Unlink(track_index, clip_id) => {
            history.do_command(
                project,
                Box::new(vv_core::UnlinkClip::new(timeline_id, track_index, clip_id)),
            );
        }
        PendingAction::Link(targets) => {
            history.do_command(
                project,
                Box::new(vv_core::LinkClips::new(timeline_id, targets)),
            );
        }
        PendingAction::SetTrackFlag(track_index, flag, value) => {
            history.do_command(
                project,
                Box::new(vv_core::SetTrackFlag::new(
                    timeline_id,
                    track_index,
                    flag,
                    value,
                )),
            );
            if flag == TrackFlag::Locked && value {
                state.drop_locked(&project.timelines[timeline_id]);
            }
        }
        PendingAction::AddTrack(kind) => {
            add_track(project, history, timeline_id, kind);
        }
        PendingAction::ArmNeedsSave => state.record_needs_save = true,
        PendingAction::RemoveTrack(track_index) => {
            history.do_command(
                project,
                Box::new(vv_core::RemoveTrack::new(timeline_id, track_index)),
            );
            // The track indices of the selection are no longer valid: it is cleared.
            state.clear_selection();
        }
        PendingAction::MakeCompound(clips) => {
            make_compound_clip(project, history, timeline_id, clips);
            state.clear_selection();
        }
        PendingAction::SetDisplayColor(clips, color) => {
            history.do_command(
                project,
                Box::new(vv_core::SetClipsDisplayColor::new(
                    timeline_id,
                    clips,
                    color,
                )),
            );
        }
        PendingAction::SetFreeze { clips, at } => {
            history.do_command(
                project,
                Box::new(vv_core::SetClipFreeze::new(timeline_id, clips, at)),
            );
        }
        // As a trim: the stretch gained overwrites what was there, a shorter
        // clip leaves a gap.
        PendingAction::SetSpeed {
            clips,
            speed,
            pitch_correction,
            resize_to,
        } => {
            let tl = &project.timelines[timeline_id];
            let overwritten: Vec<(usize, FrameIdx, FrameIdx)> = clips
                .iter()
                .filter_map(|&(track_index, clip_id)| {
                    let clip = tl.clip(track_index, clip_id)?;
                    let ClipSource::Media(media_id) = clip.source else {
                        return None;
                    };
                    let media = project.media_pool.get(media_id)?;
                    let new_end = match resize_to {
                        Some((from, to)) if from == clip.timeline_end() => to,
                        _ => {
                            let mut retimed = clip.clone();
                            retimed.unfreeze(media.meta.duration_frames);
                            retimed.set_speed(
                                speed,
                                vv_core::Rational::conform_rate(tl.fps, media.meta.fps),
                            );
                            retimed.timeline_end()
                        }
                    };
                    vv_core::edit::grown_range(clip, track_index, TrimEdge::End, new_end)
                })
                .collect();
            let mut commands: Vec<Box<dyn vv_core::Command>> = Vec::new();
            vv_core::make_room_for_ranges(
                project,
                timeline_id,
                &overwritten,
                &clips,
                &mut commands,
            );
            commands.push(Box::new(vv_core::SetClipSpeed::new(
                timeline_id,
                clips,
                speed,
                pitch_correction,
                match resize_to {
                    Some((from, to)) => vv_core::SpeedFit::ResizeTo { from, to },
                    None => vv_core::SpeedFit::Resize,
                },
            )));
            history.do_command(
                project,
                Box::new(vv_core::CompositeCommand::new(
                    vv_core::CommandLabel::ClipSpeed,
                    commands,
                )),
            );
        }
    }
}

/// Removes `clips` from the timeline (several clips too, video and audio together, on
/// several tracks) and puts a compound clip in their place, with its nested
/// timeline and pool item, as one undo step.
fn make_compound_clip(
    project: &mut Project,
    history: &mut History,
    timeline_id: TimelineId,
    clips: Vec<ClipKey>,
) {
    let Some(plan) = vv_core::plan_compound_clip(project, timeline_id, &clips) else {
        return;
    };
    let (_, commands) = vv_core::compound_clip_commands(project, timeline_id, &clips, &plan);
    history.do_command(
        project,
        Box::new(vv_core::CompositeCommand::new(
            vv_core::CommandLabel::MakeCompoundClip,
            commands,
        )),
    );
}

/// Inserts a copy of every clip of `moves` at the destination,
/// overwriting like a move. The copies become the selection.
fn duplicate_clips(
    project: &mut Project,
    history: &mut History,
    state: &mut TimelineState,
    timeline_id: TimelineId,
    moves: &[(ClipId, usize, usize, FrameIdx)],
) {
    let copies: Vec<(usize, Clip, Option<vv_core::LinkGroupId>)> = moves
        .iter()
        .filter_map(|&(id, from_track, to_track, start)| {
            let original = project.timelines[timeline_id].clip(from_track, id)?;
            let mut clip = original.clone();
            clip.timeline_start = start;
            clip.linked_group = None;
            Some((to_track, clip, original.linked_group))
        })
        .collect();
    let copies: Vec<_> = copies
        .into_iter()
        .map(|(track, mut clip, group)| {
            clip.id = project.alloc_clip_id();
            (track, clip, group)
        })
        .collect();
    let new_selection: BTreeSet<ClipKey> = copies
        .iter()
        .map(|(track, clip, _)| (*track, clip.id))
        .collect();
    let commands = vv_core::insert_overwriting(project, timeline_id, copies);
    history.do_command(
        project,
        Box::new(vv_core::CompositeCommand::new(
            vv_core::CommandLabel::DuplicateClips,
            commands,
        )),
    );

    let anchor = new_selection.iter().next().copied();
    state.set_selection(new_selection, anchor);
}

/// `offline`: the media of the clip is no longer in the media pool (deleted from
/// there, see `vv_core::RemoveMedia`) — the clip stays on the timeline but turns
/// red, and the player shows "Media offline".
fn clip_label_and_color(
    clip: &Clip,
    track: &Track,
    offline: bool,
    media_labels: &dyn Fn(vv_core::MediaId) -> String,
) -> (String, egui::Color32) {
    match &clip.source {
        vv_core::ClipSource::Media(media_id) => {
            if offline {
                return (t!("timeline.media_offline").into_owned(), OFFLINE_COLOR);
            }
            let mut label = media_labels(*media_id);
            if clip.freeze.is_some() {
                label = format!("{label} ({})", t!("timeline.freeze_frame"));
            } else if !clip.speed().is_one() {
                label = format!("{label} ({})", format_speed(clip.speed()));
            }
            let color = if track.kind == TrackKind::Video {
                egui::Color32::from_rgb(90, 140, 200)
            } else {
                egui::Color32::from_rgb(90, 190, 140)
            };
            (label, clip_box_color(color, clip))
        }
        vv_core::ClipSource::SolidColor => (
            t!("generator.solid_color").into_owned(),
            clip_box_color(palette_color(vv_core::ClipColor::Yellow), clip),
        ),
        vv_core::ClipSource::Text => (
            clip.effects
                .title
                .as_ref()
                .and_then(|t| t.content.lines().next())
                .map_or_else(|| t!("generator.text").into_owned(), str::to_string),
            clip_box_color(palette_color(vv_core::ClipColor::Magenta), clip),
        ),
        vv_core::ClipSource::Adjustment => (
            t!("generator.adjustment").into_owned(),
            clip_box_color(palette_color(vv_core::ClipColor::Gray), clip),
        ),
    }
}

/// The color chosen by the user wins over the one of the source kind; the
/// darkening for an edited clip applies to both.
fn clip_box_color(default_color: egui::Color32, clip: &Clip) -> egui::Color32 {
    let color = clip.display_color.map_or(default_color, palette_color);
    darken_if_edited(color, clip)
}

fn palette_color(color: vv_core::ClipColor) -> egui::Color32 {
    let (r, g, b) = color.rgb();
    egui::Color32::from_rgb(r, g, b)
}

/// Clips with some effect changed from the default stand out
/// at a glance on the timeline: same color, darker shade.
fn darken_if_edited(color: egui::Color32, clip: &Clip) -> egui::Color32 {
    if clip.effects.is_pristine() {
        return color;
    }
    const F: f32 = 0.62;
    egui::Color32::from_rgb(
        (color.r() as f32 * F) as u8,
        (color.g() as f32 * F) as u8,
        (color.b() as f32 * F) as u8,
    )
}

/// A right-click on a clip of the selection colors the whole selection;
/// on a clip outside it, only that clip.
fn color_targets(
    visuals: &[ClipVisual],
    state: &TimelineState,
    track_index: usize,
    clip_id: ClipId,
) -> Vec<ClipKey> {
    let clicked = (track_index, clip_id);
    if state.selected.contains(&clicked) {
        expand_to_linked_groups(visuals, state.selected.clone())
            .into_iter()
            .collect()
    } else {
        expand_to_linked_groups(visuals, BTreeSet::from([clicked]))
            .into_iter()
            .collect()
    }
}

const COLOR_GRID_COLUMNS: usize = 6;
const COLOR_SWATCH_CELL: f32 = 26.0;
const COLOR_SWATCH_RADIUS: f32 = 6.0;

/// Swatch grid of the clip color menu. `Some(None)` = "no color" chosen.
fn clip_color_grid(
    ui: &mut egui::Ui,
    current: Option<vv_core::ClipColor>,
) -> Option<Option<vv_core::ClipColor>> {
    let mut chosen = None;
    egui::Grid::new("clip_color_grid")
        .spacing(egui::vec2(2.0, 2.0))
        .show(ui, |ui| {
            for (i, color) in vv_core::ClipColor::ALL.into_iter().enumerate() {
                if clip_color_swatch(ui, color, current == Some(color)).clicked() {
                    chosen = Some(Some(color));
                }
                if (i + 1) % COLOR_GRID_COLUMNS == 0 {
                    ui.end_row();
                }
            }
        });
    ui.separator();
    if ui
        .add_enabled(
            current.is_some(),
            egui::Button::new(t!("timeline.clear_clip_color")),
        )
        .clicked()
    {
        chosen = Some(None);
    }
    chosen
}

fn clip_color_swatch(
    ui: &mut egui::Ui,
    color: vv_core::ClipColor,
    checked: bool,
) -> egui::Response {
    let (rect, resp) =
        ui.allocate_exact_size(egui::Vec2::splat(COLOR_SWATCH_CELL), egui::Sense::click());
    let resp = resp.on_hover_text(clip_color_label(color));
    let painter = ui.painter();
    if resp.hovered() {
        painter.rect_filled(rect, 3.0, ui.visuals().widgets.hovered.weak_bg_fill);
    }
    let (r, g, b) = color.rgb();
    let stroke = if checked {
        egui::Stroke::new(2.0, ui.visuals().strong_text_color())
    } else {
        egui::Stroke::NONE
    };
    painter.add(egui::Shape::convex_polygon(
        drop_outline(rect.center(), COLOR_SWATCH_RADIUS),
        egui::Color32::from_rgb(r, g, b),
        stroke,
    ));
    resp
}

/// A drop: circle of `radius` whose top tapers to a tip. `center` is the
/// center of the bounding box.
fn drop_outline(center: egui::Pos2, radius: f32) -> Vec<egui::Pos2> {
    use std::f32::consts::{FRAC_PI_2, PI};
    const TIP_DISTANCE: f32 = 1.8;
    let tip_distance = radius * TIP_DISTANCE;
    let circle_center = center + egui::vec2(0.0, (tip_distance - radius) / 2.0);
    let tangent = (radius / tip_distance).acos();
    let start = -FRAC_PI_2 + tangent;
    let sweep = 2.0 * PI - 2.0 * tangent;
    const STEPS: usize = 20;
    let mut points = vec![circle_center - egui::vec2(0.0, tip_distance)];
    points.extend((0..=STEPS).map(|i| {
        let angle = start + sweep * i as f32 / STEPS as f32;
        circle_center + radius * egui::vec2(angle.cos(), angle.sin())
    }));
    points
}

fn clip_color_label(color: vv_core::ClipColor) -> Cow<'static, str> {
    use vv_core::ClipColor as C;
    match color {
        C::Red => t!("timeline.color_red"),
        C::Orange => t!("timeline.color_orange"),
        C::Yellow => t!("timeline.color_yellow"),
        C::Green => t!("timeline.color_green"),
        C::Cyan => t!("timeline.color_cyan"),
        C::Blue => t!("timeline.color_blue"),
        C::Indigo => t!("timeline.color_indigo"),
        C::Purple => t!("timeline.color_purple"),
        C::Magenta => t!("timeline.color_magenta"),
        C::Rose => t!("timeline.color_rose"),
        C::Slate => t!("timeline.color_slate"),
        C::Gray => t!("timeline.color_gray"),
    }
}

const DISABLED_BADGE_SIZE: f32 = 12.0;

/// Small crossed-out square in front of the name of a disabled clip.
fn paint_disabled_badge(painter: &egui::Painter, top_left: egui::Pos2) {
    let rect = egui::Rect::from_min_size(
        top_left + egui::vec2(0.0, 1.0),
        egui::vec2(DISABLED_BADGE_SIZE, DISABLED_BADGE_SIZE),
    );
    painter.rect_filled(rect, 2.0, egui::Color32::from_gray(60));
    let inner = rect.shrink(3.0);
    painter.line_segment(
        [inner.left_bottom(), inner.right_top()],
        egui::Stroke::new(1.5, egui::Color32::WHITE),
    );
}

/// Rectangle of a clip in content-local coordinates: same
/// geometry for drawing, selection rectangle and shift+click.
fn clip_local_rect(
    visual: &ClipVisual,
    px_per_frame: f32,
    row_y: &[f32],
    row_height: f32,
) -> egui::Rect {
    let x = visual.clip.timeline_start as f32 * px_per_frame;
    let y = row_y[visual.track_index];
    let w = (visual.clip.timeline_len as f32 * px_per_frame).max(2.0);
    egui::Rect::from_min_size(egui::pos2(x, y + 2.0), egui::vec2(w, row_height - 4.0))
}

/// The ranges a silence removal would cut, over the part of the clip they
/// cover; anchored to the clip so they follow it while it is dragged.
fn paint_silence_preview(
    painter: &egui::Painter,
    clip_rect: egui::Rect,
    clip: &Clip,
    px_per_frame: f32,
    ranges: &[(FrameIdx, FrameIdx)],
) {
    let x_at =
        |frame: FrameIdx| clip_rect.left() + (frame - clip.timeline_start) as f32 * px_per_frame;
    for &(start, end) in ranges {
        let (start, end) = (start.max(clip.timeline_start), end.min(clip.timeline_end()));
        if start >= end {
            continue;
        }
        let rect = egui::Rect::from_x_y_ranges(x_at(start)..=x_at(end), clip_rect.y_range())
            .intersect(clip_rect);
        painter.rect_filled(rect, 0.0, SILENCE_PREVIEW_COLOR);
    }
}

/// Waveform of an audio clip, one line per visible column. The bin of
/// each column comes from the absolute time in the audio: splitting the clip does not
/// move the waveform.
fn draw_clip_waveform(
    painter: &egui::Painter,
    clip_rect: egui::Rect,
    peaks: &[f32],
    clip_start_secs: f64,
    clip_end_secs: f64,
    media_fps: f64,
    audio_duration_secs: f64,
    visible_rect: egui::Rect,
    gain_db: &Keyframed<f32>,
) {
    if peaks.is_empty() || audio_duration_secs <= 0.0 || media_fps <= 0.0 {
        return;
    }
    if clip_end_secs <= clip_start_secs {
        return;
    }

    // Visible portion of the clip (no column outside the viewport).
    let vis = clip_rect.intersect(visible_rect);
    if !vis.is_positive() {
        return;
    }

    let center_y = clip_rect.center().y;
    let half_height = clip_rect.height() / 2.0;
    let stroke = egui::Stroke::new(
        1.0,
        egui::Color32::from_rgba_unmultiplied(255, 255, 255, 140),
    );
    let clipped_stroke =
        egui::Stroke::new(1.0, egui::Color32::from_rgba_unmultiplied(255, 90, 90, 200));

    let width = clip_rect.width();
    let mut x = vis.min.x;
    while x < vis.max.x {
        let frac = ((x - clip_rect.min.x) / width) as f64;
        let bin = waveform_bin_for_column(
            frac,
            clip_start_secs,
            clip_end_secs,
            audio_duration_secs,
            peaks.len(),
        );
        // The gain lives in *source* frames, as in the mixer: the shape
        // drawn is the one that will really be heard, clipping included.
        let secs = clip_start_secs + frac * (clip_end_secs - clip_start_secs);
        let source_frame = (secs * media_fps).floor() as FrameIdx;
        let amplified = peaks[bin] * vv_audio::mixer::db_to_linear(gain_db.value_at(source_frame));
        let h = (half_height * amplified.min(1.0)).max(0.5);
        painter.line_segment(
            [egui::pos2(x, center_y - h), egui::pos2(x, center_y + h)],
            if amplified > 1.0 {
                clipped_stroke
            } else {
                stroke
            },
        );
        x += 1.0;
    }
}

/// Bin of the column at `frac` of the clip, from the absolute time in the audio.
fn waveform_bin_for_column(
    frac: f64,
    clip_start_secs: f64,
    clip_end_secs: f64,
    audio_duration_secs: f64,
    num_peaks: usize,
) -> usize {
    let t_secs = clip_start_secs + frac * (clip_end_secs - clip_start_secs);
    ((t_secs / audio_duration_secs * num_peaks as f64) as usize).min(num_peaks.saturating_sub(1))
}

/// The clips whose rectangle intersects `rect` (local coordinates): the core
/// shared by marquee-select and shift+click (which uses the rectangle
/// joining the anchor and the clicked clip).
fn clips_intersecting_rect(
    visuals: &[ClipVisual],
    px_per_frame: f32,
    row_y: &[f32],
    row_height: f32,
    rect: egui::Rect,
) -> Vec<ClipKey> {
    visuals
        .iter()
        .filter(|v| {
            !v.locked && clip_local_rect(v, px_per_frame, row_y, row_height).intersects(rect)
        })
        .map(|v| (v.track_index, v.clip.id))
        .collect()
}

/// The gap covering `frame` on the track, if followed by another clip.
fn gap_at(
    visuals: &[ClipVisual],
    track_index: usize,
    frame: FrameIdx,
) -> Option<(FrameIdx, FrameIdx)> {
    let mut track_clips: Vec<&Clip> = visuals
        .iter()
        .filter(|v| v.track_index == track_index)
        .map(|v| v.clip.as_ref())
        .collect();
    track_clips.sort_by_key(|c| c.timeline_start);

    if track_clips.iter().any(|c| c.contains(frame)) {
        return None; // `frame` is inside a clip, not in a gap.
    }
    let next = track_clips.iter().find(|c| c.timeline_start > frame)?;
    let gap_start = track_clips
        .iter()
        .filter(|c| c.timeline_end() <= frame)
        .map(|c| c.timeline_end())
        .max()
        .unwrap_or(0);
    Some((gap_start, next.timeline_start))
}

/// Plain click, ctrl (adds/removes), shift (range with the rectangle
/// between anchor and clip).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ClickModifiers {
    Plain,
    Toggle,
    Range,
}

fn click_modifiers(modifiers: egui::Modifiers) -> ClickModifiers {
    if modifiers.shift {
        ClickModifiers::Range
    } else if modifiers.command {
        ClickModifiers::Toggle
    } else {
        ClickModifiers::Plain
    }
}

/// Applies a click (with any modifiers) on the clip `clicked`,
/// given the current selection and anchor. A pure function, without any
/// `egui::Ui`: testable with simple data.
fn apply_click_selection(
    current: &BTreeSet<ClipKey>,
    anchor: Option<ClipKey>,
    clicked: ClipKey,
    modifiers: ClickModifiers,
    visuals: &[ClipVisual],
    px_per_frame: f32,
    row_y: &[f32],
    row_height: f32,
) -> (BTreeSet<ClipKey>, Option<ClipKey>) {
    match modifiers {
        ClickModifiers::Plain => (BTreeSet::from([clicked]), Some(clicked)),
        ClickModifiers::Toggle => {
            let mut set = current.clone();
            if !set.remove(&clicked) {
                set.insert(clicked);
            }
            (set, Some(clicked))
        }
        ClickModifiers::Range => {
            let effective_anchor = anchor.unwrap_or(clicked);
            let anchor_rect = visuals
                .iter()
                .find(|v| (v.track_index, v.clip.id) == effective_anchor)
                .map(|v| clip_local_rect(v, px_per_frame, row_y, row_height));
            let clicked_rect = visuals
                .iter()
                .find(|v| (v.track_index, v.clip.id) == clicked)
                .map(|v| clip_local_rect(v, px_per_frame, row_y, row_height));
            let set = match (anchor_rect, clicked_rect) {
                (Some(a), Some(c)) => {
                    clips_intersecting_rect(visuals, px_per_frame, row_y, row_height, a.union(c))
                        .into_iter()
                        .collect()
                }
                _ => BTreeSet::from([clicked]),
            };
            (set, Some(effective_anchor))
        }
    }
}

/// Bounds imposed by the neighbors on `track_index` for a clip of length `len`
/// positioned (only to decide who comes before/after) at `reference_start` —
/// it does not have to already be there: also used for a track change mid-drag.
fn neighbor_bounds_at(
    visuals: &[ClipVisual],
    track_index: usize,
    exclude: &[ClipId],
    reference_start: FrameIdx,
    len: FrameIdx,
) -> (FrameIdx, FrameIdx) {
    let mut lower_bound: FrameIdx = 0;
    let mut upper_bound: FrameIdx = FrameIdx::MAX;
    let reference_end = reference_start + len;

    for v in visuals {
        if v.track_index != track_index || exclude.contains(&v.clip.id) {
            continue;
        }
        if v.clip.timeline_end() <= reference_start {
            lower_bound = lower_bound.max(v.clip.timeline_end());
        }
        if v.clip.timeline_start >= reference_end {
            upper_bound = upper_bound.min(v.clip.timeline_start);
        }
    }

    (lower_bound, upper_bound)
}

fn max_start_in_slot(lower: FrameIdx, upper: FrameIdx, len: FrameIdx) -> FrameIdx {
    upper.saturating_sub(len).max(lower)
}

/// Like `drag_range`, for a clip of length `len` evaluated at `reference_start`
/// on `track_index` (even if it does not fit there yet) ignoring `exclude`.
fn drag_range_at(
    visuals: &[ClipVisual],
    track_index: usize,
    exclude: &[ClipId],
    reference_start: FrameIdx,
    len: FrameIdx,
) -> (FrameIdx, FrameIdx) {
    let (lower, upper) = neighbor_bounds_at(visuals, track_index, exclude, reference_start, len);
    (lower, max_start_in_slot(lower, upper, len))
}

/// Valid range (min/max) for the new `timeline_start` of a clip on its
/// own, already resolved (not a raw "upper"): used both directly and
/// as a base to combine with that of a linked twin.
fn drag_range(visuals: &[ClipVisual], track_index: usize, clip_id: ClipId) -> (FrameIdx, FrameIdx) {
    let Some(v) = visuals
        .iter()
        .find(|v| v.track_index == track_index && v.clip.id == clip_id)
    else {
        return (0, FrameIdx::MAX);
    };
    drag_range_at(
        visuals,
        track_index,
        &[clip_id],
        v.clip.timeline_start,
        v.clip.timeline_len,
    )
}

/// Extends the clips to their linked groups. The single place doing it for the
/// selections made on the timeline.
fn expand_to_linked_groups(
    visuals: &[ClipVisual],
    keys: impl IntoIterator<Item = ClipKey>,
) -> BTreeSet<ClipKey> {
    let mut result: BTreeSet<ClipKey> = BTreeSet::new();
    for (track_index, clip_id) in keys {
        let Some(visual) = visuals
            .iter()
            .find(|v| v.track_index == track_index && v.clip.id == clip_id && !v.locked)
        else {
            continue;
        };
        result.insert((track_index, clip_id));
        if let Some(group) = visual.clip.linked_group {
            for v in visuals
                .iter()
                .filter(|v| v.clip.linked_group == Some(group) && !v.locked)
            {
                result.insert((v.track_index, v.clip.id));
            }
        }
    }
    result
}

/// Clips moving with `clicked`: the selection if it contains it,
/// otherwise it and its group.
fn drag_group_for(
    selected: &BTreeSet<ClipKey>,
    visuals: &[ClipVisual],
    clicked: ClipKey,
) -> BTreeSet<ClipKey> {
    if selected.contains(&clicked) {
        selected.clone()
    } else {
        expand_to_linked_groups(visuals, [clicked])
    }
}

/// Range of the `timeline_start` of `clip_id` respecting the constraints of all
/// the `others`, and their offsets for `DragState::followers`.
fn combined_drag_range(
    visuals: &[ClipVisual],
    track_index: usize,
    clip_id: ClipId,
    others: &[ClipKey],
) -> (FrameIdx, FrameIdx, Vec<(ClipId, usize, FrameIdx)>) {
    let (mut min_start, mut max_start) = drag_range(visuals, track_index, clip_id);

    let Some(this_start) = visuals
        .iter()
        .find(|v| v.track_index == track_index && v.clip.id == clip_id)
        .map(|v| v.clip.timeline_start)
    else {
        return (min_start, max_start, Vec::new());
    };

    let mut followers = Vec::new();
    for &(other_track, other_id) in others {
        if other_track == track_index && other_id == clip_id {
            continue;
        }
        let Some(other) = visuals
            .iter()
            .find(|v| v.track_index == other_track && v.clip.id == other_id)
        else {
            continue;
        };
        let offset = other.clip.timeline_start - this_start;
        let (o_min, o_max) = drag_range(visuals, other_track, other_id);
        // Saturating: without neighbors `o_max` is ~`FrameIdx::MAX`.
        min_start = min_start.max(o_min.saturating_sub(offset));
        max_start = max_start.min(o_max.saturating_sub(offset));
        followers.push((other_id, other_track, offset));
    }
    (min_start, max_start, followers)
}

/// Like `combined_drag_range`, with every clip on its own target track.
/// The neighbors exclude the whole group, or two clips headed for the same
/// track would block each other.
fn group_drag_bounds(
    visuals: &[ClipVisual],
    reference_start: FrameIdx,
    targets: &[(ClipId, EffectiveTrack)],
    followers: &[(ClipId, usize, FrameIdx)],
) -> (FrameIdx, FrameIdx) {
    let exclude: Vec<ClipId> = targets.iter().map(|(id, _)| *id).collect();
    let bound_for =
        |id: ClipId, target: EffectiveTrack, reference: FrameIdx| -> (FrameIdx, FrameIdx) {
            let len = visuals
                .iter()
                .find(|v| v.clip.id == id)
                .map(|v| v.clip.timeline_len)
                .unwrap_or(0);
            match target {
                EffectiveTrack::Existing(track) => {
                    drag_range_at(visuals, track, &exclude, reference, len)
                }
                EffectiveTrack::New(_) => (0, max_start_in_slot(0, FrameIdx::MAX, len)),
            }
        };

    let (primary_id, primary_target) = targets[0];
    let (mut min_start, mut max_start) = bound_for(primary_id, primary_target, reference_start);

    for (i, &(follower_id, _, offset)) in followers.iter().enumerate() {
        let (_, follower_target) = targets[i + 1];
        let (o_min, o_max) = bound_for(follower_id, follower_target, reference_start + offset);
        // Saturating: without neighbors `o_max` is ~`FrameIdx::MAX`.
        min_start = min_start.max(o_min.saturating_sub(offset));
        max_start = max_start.min(o_max.saturating_sub(offset));
    }
    (min_start, max_start.max(min_start))
}

/// Range of the trimmed edge combined with that of the `others`, and their
/// offsets for `TrimState::followers`.
fn combined_trim_range(
    visuals: &[ClipVisual],
    project: &Project,
    primary: ClipKey,
    edge: TrimEdge,
    others: &[(ClipKey, TrimEdge)],
) -> (FrameIdx, FrameIdx, Vec<TrimFollower>) {
    let find = |(track, id): ClipKey| {
        visuals
            .iter()
            .find(|v| v.track_index == track && v.clip.id == id)
    };
    let Some(primary_visual) = find(primary) else {
        return (0, FrameIdx::MAX, Vec::new());
    };
    let edge_value = |clip: &Clip, edge: TrimEdge| match edge {
        TrimEdge::Start => clip.timeline_start,
        TrimEdge::End => clip.timeline_end(),
    };
    let primary_value = edge_value(&primary_visual.clip, edge);
    let trimmed: Vec<(&ClipVisual, TrimEdge)> = std::iter::once((primary_visual, edge))
        .chain(others.iter().filter_map(|&(k, e)| find(k).map(|v| (v, e))))
        .collect();

    let mut min_value = FrameIdx::MIN;
    let mut max_value = FrameIdx::MAX;
    let mut followers = Vec::new();
    for &(v, v_edge) in &trimmed {
        let offset = edge_value(&v.clip, v_edge) - primary_value;
        let (mut o_min, mut o_max) = vv_core::edit::trim_range(project, &v.clip, v_edge);
        // Two clips trimmed together on the same track must not
        // lengthen over each other; in a roll, instead, the edge of the
        // neighbor moves with this one.
        for &(w, w_edge) in trimmed.iter().filter(|(w, w_edge)| {
            w.track_index == v.track_index && w.clip.id != v.clip.id && *w_edge == v_edge
        }) {
            match w_edge {
                TrimEdge::End if w.clip.timeline_start >= v.clip.timeline_end() => {
                    o_max = o_max.min(w.clip.timeline_start);
                }
                TrimEdge::Start if w.clip.timeline_end() <= v.clip.timeline_start => {
                    o_min = o_min.max(w.clip.timeline_end());
                }
                _ => {}
            }
        }
        min_value = min_value.max(o_min.saturating_sub(offset));
        max_value = max_value.min(o_max.saturating_sub(offset));
        if v.clip.id != primary_visual.clip.id || v.track_index != primary_visual.track_index {
            followers.push((v.clip.id, v.track_index, offset, v_edge));
        }
    }
    (min_value, max_value, followers)
}

/// Edge-sensitive zones of a clip `width` pixels wide, given who is in
/// contact with it on the left (`start_neighbor`) and on the right (`end_neighbor`).
struct EdgeZones {
    width: f32,
    roll_px: f32,
    trim_px: f32,
    start_neighbor: Option<ClipKey>,
    end_neighbor: Option<ClipKey>,
}

fn edge_zones(
    width: f32,
    start_neighbor: Option<ClipKey>,
    end_neighbor: Option<ClipKey>,
) -> EdgeZones {
    EdgeZones {
        width,
        roll_px: ROLL_HANDLE_PX.min(width / 6.0),
        trim_px: TRIM_HANDLE_PX.min(width / 3.0),
        start_neighbor,
        end_neighbor,
    }
}

impl EdgeZones {
    /// `local_x`: distance from the left edge of the clip.
    fn at(&self, local_x: f32) -> Option<EdgeZone> {
        let sides = [
            (local_x, TrimEdge::Start, self.start_neighbor),
            (self.width - local_x, TrimEdge::End, self.end_neighbor),
        ];
        for (distance, edge, neighbor) in sides {
            match neighbor {
                Some(neighbor) if distance < self.roll_px => {
                    return Some(EdgeZone::Roll { edge, neighbor });
                }
                Some(_) if distance < self.roll_px + self.trim_px => {
                    return Some(EdgeZone::Trim(edge));
                }
                None if distance < self.trim_px => return Some(EdgeZone::Trim(edge)),
                _ => {}
            }
        }
        None
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum EdgeCursor {
    TrimStart,
    TrimEnd,
    Roll,
    /// End of a clip with the retime bar: it changes the speed, not the trim.
    Retime,
    Slip,
}

impl EdgeCursor {
    fn from_zone(zone: EdgeZone) -> Self {
        match zone {
            EdgeZone::Roll { .. } => EdgeCursor::Roll,
            EdgeZone::Trim(TrimEdge::Start) => EdgeCursor::TrimStart,
            EdgeZone::Trim(TrimEdge::End) => EdgeCursor::TrimEnd,
        }
    }
}

/// egui has no custom cursors: the system one is hidden and
/// this is drawn in its place. Brackets "[" / "]" like the edges of a
/// clip, with the drag arrows.
fn paint_edge_cursor(ctx: &egui::Context, pos: egui::Pos2, cursor: EdgeCursor) {
    ctx.set_cursor_icon(egui::CursorIcon::None);
    let painter = ctx.layer_painter(egui::LayerId::new(
        egui::Order::Tooltip,
        egui::Id::new("timeline_edge_cursor"),
    ));
    if cursor == EdgeCursor::Retime {
        paint_retime_cursor(&painter, pos);
        return;
    }
    const HALF_H: f32 = 8.0;
    const TICK: f32 = 4.0;
    let bracket = |x: f32, towards: f32| {
        vec![
            egui::pos2(x + towards * TICK, pos.y - HALF_H),
            egui::pos2(x, pos.y - HALF_H),
            egui::pos2(x, pos.y + HALF_H),
            egui::pos2(x + towards * TICK, pos.y + HALF_H),
        ]
    };
    // (tip x, direction)
    let arrow = |tip: f32, dir: f32| {
        vec![
            egui::pos2(tip, pos.y),
            egui::pos2(tip - dir * 5.0, pos.y - 4.5),
            egui::pos2(tip - dir * 5.0, pos.y + 4.5),
        ]
    };
    let (brackets, arrows) = match cursor {
        EdgeCursor::TrimEnd => (
            vec![bracket(pos.x, -1.0)],
            vec![arrow(pos.x - 11.0, -1.0), arrow(pos.x + 8.0, 1.0)],
        ),
        EdgeCursor::TrimStart => (
            vec![bracket(pos.x, 1.0)],
            vec![arrow(pos.x - 8.0, -1.0), arrow(pos.x + 11.0, 1.0)],
        ),
        EdgeCursor::Roll => (
            vec![bracket(pos.x - 2.0, -1.0), bracket(pos.x + 2.0, 1.0)],
            vec![arrow(pos.x - 10.0, -1.0), arrow(pos.x + 10.0, 1.0)],
        ),
        // "[ ]" with the arrows inside: the clip stays, its content moves.
        EdgeCursor::Slip => (
            vec![bracket(pos.x - 10.0, 1.0), bracket(pos.x + 10.0, -1.0)],
            vec![arrow(pos.x - 6.0, -1.0), arrow(pos.x + 6.0, 1.0)],
        ),
        EdgeCursor::Retime => unreachable!("drawn by paint_retime_cursor"),
    };
    for points in &brackets {
        painter.line(points.clone(), egui::Stroke::new(4.0, egui::Color32::BLACK));
    }
    for points in brackets {
        painter.line(points, egui::Stroke::new(2.0, egui::Color32::WHITE));
    }
    for points in arrows {
        painter.add(egui::Shape::convex_polygon(
            points,
            egui::Color32::WHITE,
            egui::Stroke::new(1.0, egui::Color32::BLACK),
        ));
    }
}

/// A white double arrow with "%" over it: unlike the brackets of the trim and
/// the black system arrow of the fades.
fn paint_retime_cursor(painter: &egui::Painter, pos: egui::Pos2) {
    const HALF_W: f32 = 11.0;
    let outline = egui::Stroke::new(1.0, egui::Color32::BLACK);
    painter.line_segment(
        [
            pos - egui::vec2(HALF_W - 4.0, 0.0),
            pos + egui::vec2(HALF_W - 4.0, 0.0),
        ],
        egui::Stroke::new(5.0, egui::Color32::BLACK),
    );
    painter.line_segment(
        [
            pos - egui::vec2(HALF_W - 4.0, 0.0),
            pos + egui::vec2(HALF_W - 4.0, 0.0),
        ],
        egui::Stroke::new(3.0, egui::Color32::WHITE),
    );
    for dir in [-1.0f32, 1.0] {
        let tip = pos.x + dir * HALF_W;
        painter.add(egui::Shape::convex_polygon(
            vec![
                egui::pos2(tip, pos.y),
                egui::pos2(tip - dir * 6.0, pos.y - 5.5),
                egui::pos2(tip - dir * 6.0, pos.y + 5.5),
            ],
            egui::Color32::WHITE,
            outline,
        ));
    }
    let label = pos - egui::vec2(0.0, 10.0);
    for offset in [
        egui::vec2(-1.0, 0.0),
        egui::vec2(1.0, 0.0),
        egui::vec2(0.0, -1.0),
        egui::vec2(0.0, 1.0),
    ] {
        painter.text(
            label + offset,
            egui::Align2::CENTER_BOTTOM,
            "%",
            egui::FontId::proportional(11.0),
            egui::Color32::BLACK,
        );
    }
    painter.text(
        label,
        egui::Align2::CENTER_BOTTOM,
        "%",
        egui::FontId::proportional(11.0),
        egui::Color32::WHITE,
    );
}

/// Snapping threshold, in screen pixels (not in frames:
/// it stays the same visual distance at any zoom level, converted
/// into frames by `snap_frame` based on `px_per_frame`).
const SNAP_THRESHOLD_PX: f32 = 10.0;

/// Edges of the non-excluded clips plus `extra_targets` (the playhead).
fn snap_targets<'a>(
    visuals: &'a [ClipVisual],
    exclude: &'a [ClipId],
    extra_targets: &'a [FrameIdx],
) -> impl Iterator<Item = FrameIdx> + 'a {
    visuals
        .iter()
        .filter(|v| !exclude.contains(&v.clip.id))
        .flat_map(|v| [v.clip.timeline_start, v.clip.timeline_end()])
        .chain(extra_targets.iter().copied())
}

/// With snapping on, it snaps the start or the end of the clip of length
/// `len` to the nearest edge within `SNAP_THRESHOLD_PX`. `exclude` does not count.
fn snap_frame(
    candidate_start: FrameIdx,
    len: FrameIdx,
    visuals: &[ClipVisual],
    exclude: &[ClipId],
    extra_targets: &[FrameIdx],
    px_per_frame: f32,
    enabled: bool,
) -> FrameIdx {
    if !enabled {
        return candidate_start;
    }
    let threshold = (SNAP_THRESHOLD_PX / px_per_frame).round() as FrameIdx;
    if threshold <= 0 {
        return candidate_start;
    }
    let candidate_end = candidate_start + len;

    let mut best: Option<(FrameIdx, FrameIdx)> = None; // (|gap|, new candidate_start)
    for edge in snap_targets(visuals, exclude, extra_targets) {
        // (point of the dragged clip to compare with the edge, new
        // candidate_start if this is the chosen snap)
        for (point, new_start) in [(candidate_start, edge), (candidate_end, edge - len)] {
            let delta = (point - edge).abs();
            if delta > threshold {
                continue;
            }
            if best.is_none_or(|(best_delta, _)| delta < best_delta) {
                best = Some((delta, new_start));
            }
        }
    }
    best.map_or(candidate_start, |(_, new_start)| new_start)
}

/// Highlights a "new track" zone under a drag in progress.
fn paint_drop_zone(painter: &egui::Painter, rect: egui::Rect, label: Option<&str>) {
    let green = egui::Color32::from_rgb(120, 220, 120);
    painter.rect_filled(
        rect,
        4.0,
        egui::Color32::from_rgba_unmultiplied(120, 220, 120, 60),
    );
    painter.rect_stroke(
        rect,
        4.0,
        egui::Stroke::new(2.0, green),
        egui::StrokeKind::Inside,
    );
    if let Some(label) = label {
        painter.text(
            rect.center(),
            egui::Align2::CENTER_CENTER,
            label,
            egui::FontId::proportional(13.0),
            egui::Color32::from_rgb(200, 255, 200),
        );
    }
}

const PROXY_STRIP_HEIGHT: f32 = 4.0;
/// Clips whose media is no longer in the media pool.
pub const OFFLINE_COLOR: egui::Color32 = egui::Color32::from_rgb(170, 50, 50);

/// "Proxy available" indicator, shared with the media pool.
pub const PROXY_COLOR: egui::Color32 = egui::Color32::from_rgba_premultiplied(220, 151, 52, 220);

/// Border of the clip while a filter from the Effects panel is dragged over it.
const FILTER_HIGHLIGHT_COLOR: egui::Color32 = egui::Color32::from_rgb(255, 190, 60);

const SILENCE_PREVIEW_COLOR: egui::Color32 =
    egui::Color32::from_rgba_premultiplied(170, 25, 25, 150);

fn paint_proxy_strip(painter: &egui::Painter, rect: egui::Rect) {
    let strip_rect = egui::Rect::from_min_size(
        rect.left_top() + egui::vec2(1.0, 1.0),
        egui::vec2(rect.width() - 2.0, PROXY_STRIP_HEIGHT),
    );
    painter.rect_filled(strip_rect, 2.0, PROXY_COLOR);
}

#[cfg(test)]
#[path = "tests/timeline_ui.rs"]
mod tests;

#[cfg(test)]
#[path = "tests/timeline_ui_gestures.rs"]
mod gesture_tests;
