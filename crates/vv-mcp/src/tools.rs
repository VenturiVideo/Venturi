//! The tools and their arguments. Argument structs derive `JsonSchema`: the
//! MCP layer publishes them as the tools' input schemas.

use schemars::JsonSchema;
use serde::Deserialize;

#[derive(Debug, Clone, PartialEq)]
pub enum ToolCall {
    GetProject,
    GetTimeline(TimelineArgs),
    GetClip(ClipArgs),
    NewProject,
    OpenProject(OpenProjectArgs),
    SaveProject(SaveProjectArgs),
    ImportMedia(ImportMediaArgs),
    ImportOtio(ImportOtioArgs),
    CreateTimeline(CreateTimelineArgs),
    AddTrack(AddTrackArgs),
    SetTrack(SetTrackArgs),
    InsertClip(InsertClipArgs),
    Split(SplitArgs),
    DeleteClips(DeleteClipsArgs),
    DeleteRanges(DeleteRangesArgs),
    MoveClips(MoveClipsArgs),
    TrimClip(TrimClipArgs),
    SetClipProperties(SetClipPropertiesArgs),
    AddTitle(AddTitleArgs),
    AddSolidColor(AddSolidColorArgs),
    AddAdjustmentClip(AddAdjustmentClipArgs),
    LinkClips(ClipsArgs),
    UnlinkClips(ClipsArgs),
    AddMarker(AddMarkerArgs),
    EditMarker(EditMarkerArgs),
    DeleteMarker(MarkerArgs),
    GetMarkers(TimelineArgs),
    SetClipColor(SetClipColorArgs),
    SetClipMasks(SetClipMasksArgs),
    SetTransition(SetTransitionArgs),
    RenderFrame(RenderFrameArgs),
    GetAudioLevels(AudioLevelsArgs),
    Export(ExportArgs),
    ExportStatus(JobArgs),
    CancelExport(JobArgs),
    Undo,
    Redo,
    GetState,
    ScreenshotUi,
    SetActiveTimeline(TimelineArgs),
}

impl ToolCall {
    pub fn name(&self) -> &'static str {
        match self {
            ToolCall::GetProject => "get_project",
            ToolCall::GetTimeline(_) => "get_timeline",
            ToolCall::GetClip(_) => "get_clip",
            ToolCall::NewProject => "new_project",
            ToolCall::OpenProject(_) => "open_project",
            ToolCall::SaveProject(_) => "save_project",
            ToolCall::ImportMedia(_) => "import_media",
            ToolCall::ImportOtio(_) => "import_otio",
            ToolCall::CreateTimeline(_) => "create_timeline",
            ToolCall::AddTrack(_) => "add_track",
            ToolCall::SetTrack(_) => "set_track",
            ToolCall::InsertClip(_) => "insert_clip",
            ToolCall::Split(_) => "split",
            ToolCall::DeleteClips(_) => "delete_clips",
            ToolCall::DeleteRanges(_) => "delete_ranges",
            ToolCall::MoveClips(_) => "move_clips",
            ToolCall::TrimClip(_) => "trim_clip",
            ToolCall::SetClipProperties(_) => "set_clip_properties",
            ToolCall::AddTitle(_) => "add_title",
            ToolCall::AddSolidColor(_) => "add_solid_color",
            ToolCall::AddAdjustmentClip(_) => "add_adjustment_clip",
            ToolCall::LinkClips(_) => "link_clips",
            ToolCall::UnlinkClips(_) => "unlink_clips",
            ToolCall::AddMarker(_) => "add_marker",
            ToolCall::EditMarker(_) => "edit_marker",
            ToolCall::DeleteMarker(_) => "delete_marker",
            ToolCall::GetMarkers(_) => "get_markers",
            ToolCall::SetClipColor(_) => "set_clip_color",
            ToolCall::SetClipMasks(_) => "set_clip_masks",
            ToolCall::SetTransition(_) => "set_transition",
            ToolCall::RenderFrame(_) => "render_frame",
            ToolCall::GetAudioLevels(_) => "get_audio_levels",
            ToolCall::Export(_) => "export",
            ToolCall::ExportStatus(_) => "export_status",
            ToolCall::CancelExport(_) => "cancel_export",
            ToolCall::Undo => "undo",
            ToolCall::Redo => "redo",
            ToolCall::GetState => "get_state",
            ToolCall::ScreenshotUi => "screenshot_ui",
            ToolCall::SetActiveTimeline(_) => "set_active_timeline",
        }
    }

    /// Whether the call changes the project (or what the editor shows): an
    /// editor host holds these while the user is in the middle of a gesture.
    pub fn mutates(&self) -> bool {
        !matches!(
            self,
            ToolCall::GetProject
                | ToolCall::GetTimeline(_)
                | ToolCall::GetClip(_)
                | ToolCall::GetMarkers(_)
                | ToolCall::RenderFrame(_)
                | ToolCall::GetAudioLevels(_)
                | ToolCall::ExportStatus(_)
                | ToolCall::CancelExport(_)
                | ToolCall::GetState
                | ToolCall::ScreenshotUi
        )
    }
}

#[derive(Debug, Clone, PartialEq, Deserialize, JsonSchema)]
pub struct TimelineArgs {
    pub timeline_id: String,
}

#[derive(Debug, Clone, PartialEq, Deserialize, JsonSchema)]
pub struct ClipArgs {
    pub timeline_id: String,
    pub clip_id: String,
}

#[derive(Debug, Clone, PartialEq, Deserialize, JsonSchema)]
pub struct ClipsArgs {
    pub timeline_id: String,
    /// The timeline's `revision` from your last read: the call is refused
    /// if the timeline changed since (e.g. the user edited it meanwhile).
    #[serde(default)]
    pub if_revision: Option<String>,
    pub clip_ids: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Deserialize, JsonSchema)]
pub struct ImportOtioArgs {
    /// Path of an `.otio` file.
    pub path: String,
    /// When some of its media have the same file name as media already in
    /// the pool: `true` points the clips at the existing media, `false`
    /// (default) imports them again into a new folder.
    #[serde(default)]
    pub reuse_existing_media: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum TrackKindArg {
    Video,
    Audio,
}

#[derive(Debug, Clone, PartialEq, Deserialize, JsonSchema)]
pub struct AddTrackArgs {
    pub timeline_id: String,
    /// The timeline's `revision` from your last read: the call is refused
    /// if the timeline changed since (e.g. the user edited it meanwhile).
    #[serde(default)]
    pub if_revision: Option<String>,
    pub kind: TrackKindArg,
}

#[derive(Debug, Clone, PartialEq, Deserialize, JsonSchema)]
pub struct SetTrackArgs {
    pub timeline_id: String,
    /// The timeline's `revision` from your last read: the call is refused
    /// if the timeline changed since (e.g. the user edited it meanwhile).
    #[serde(default)]
    pub if_revision: Option<String>,
    /// Track name as in `get_timeline`: "V1", "A2", ...
    pub track: String,
    #[serde(default)]
    pub muted: Option<bool>,
    #[serde(default)]
    pub solo: Option<bool>,
    /// Locked tracks are left alone by every edit tool.
    #[serde(default)]
    pub locked: Option<bool>,
}

fn yes() -> bool {
    true
}

#[derive(Debug, Clone, PartialEq, Deserialize, JsonSchema)]
pub struct InsertClipArgs {
    pub timeline_id: String,
    /// The timeline's `revision` from your last read: the call is refused
    /// if the timeline changed since (e.g. the user edited it meanwhile).
    #[serde(default)]
    pub if_revision: Option<String>,
    pub media_id: String,
    /// Timeline frame where the clip starts. What is already there is
    /// overwritten (shortened, split or removed).
    pub at: i64,
    /// First media frame used (media frames, not timeline frames). Default 0.
    #[serde(default)]
    pub source_in: Option<i64>,
    /// Media frame after the last one used. Default: the end of the media
    /// (5 s for an image).
    #[serde(default)]
    pub source_out: Option<i64>,
    /// Video track name ("V1", ...). Default: the first unlocked one, created
    /// if there is none.
    #[serde(default)]
    pub video_track: Option<String>,
    /// Audio track for the first audio stream ("A1", ...); the other streams
    /// go on the following unlocked audio tracks, created as needed.
    #[serde(default)]
    pub audio_track: Option<String>,
    /// Put the media's video on the timeline. Default true.
    #[serde(default = "yes")]
    pub video: bool,
    /// Put the media's audio streams on the timeline. Default true.
    #[serde(default = "yes")]
    pub audio: bool,
}

#[derive(Debug, Clone, PartialEq, Deserialize, JsonSchema)]
pub struct SplitArgs {
    pub timeline_id: String,
    /// The timeline's `revision` from your last read: the call is refused
    /// if the timeline changed since (e.g. the user edited it meanwhile).
    #[serde(default)]
    pub if_revision: Option<String>,
    /// Timeline frame of the cut: the right halves start here.
    pub frame: i64,
    /// Only these clips; default every clip of the unlocked tracks crossing
    /// `frame`. Linked clips are not added automatically.
    #[serde(default)]
    pub clip_ids: Option<Vec<String>>,
}

#[derive(Debug, Clone, PartialEq, Deserialize, JsonSchema)]
pub struct DeleteClipsArgs {
    pub timeline_id: String,
    /// The timeline's `revision` from your last read: the call is refused
    /// if the timeline changed since (e.g. the user edited it meanwhile).
    #[serde(default)]
    pub if_revision: Option<String>,
    pub clip_ids: Vec<String>,
    /// Close the gaps, shifting everything after them on every unlocked
    /// track (their linked clips are deleted too). Default false.
    #[serde(default)]
    pub ripple: bool,
}

#[derive(Debug, Clone, PartialEq, Deserialize, JsonSchema)]
pub struct DeleteRangesArgs {
    pub timeline_id: String,
    /// The timeline's `revision` from your last read: the call is refused
    /// if the timeline changed since (e.g. the user edited it meanwhile).
    #[serde(default)]
    pub if_revision: Option<String>,
    /// `[start, end)` timeline frame ranges; clips crossing an edge are cut
    /// there. Overlapping or touching ranges are merged.
    pub ranges: Vec<[i64; 2]>,
    /// Close the gaps on every unlocked track, keeping audio and video in
    /// sync. Default false.
    #[serde(default)]
    pub ripple: bool,
    /// Without `ripple`: only these tracks ("V1", "A1", ...). Default all
    /// unlocked tracks.
    #[serde(default)]
    pub tracks: Option<Vec<String>>,
    /// The ranges are frames of this media, not of the timeline: they are
    /// removed wherever that material is on the timeline (every use of it,
    /// wherever earlier cuts moved it). Without `ripple`, only the media's
    /// own clips are cut. The result lists the timeline ranges removed.
    #[serde(default)]
    pub media_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Deserialize, JsonSchema)]
pub struct ClipMove {
    pub clip_id: String,
    /// New timeline start frame.
    pub start: i64,
    /// Destination track, of the same kind; default the current one.
    #[serde(default)]
    pub track: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Deserialize, JsonSchema)]
pub struct MoveClipsArgs {
    pub timeline_id: String,
    /// The timeline's `revision` from your last read: the call is refused
    /// if the timeline changed since (e.g. the user edited it meanwhile).
    #[serde(default)]
    pub if_revision: Option<String>,
    /// The moved clips overwrite what is at their destination. Linked clips
    /// are not moved along: list them too.
    pub moves: Vec<ClipMove>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum EdgeArg {
    Start,
    End,
}

#[derive(Debug, Clone, PartialEq, Deserialize, JsonSchema)]
pub struct TrimClipArgs {
    pub timeline_id: String,
    /// The timeline's `revision` from your last read: the call is refused
    /// if the timeline changed since (e.g. the user edited it meanwhile).
    #[serde(default)]
    pub if_revision: Option<String>,
    pub clip_id: String,
    pub edge: EdgeArg,
    /// New timeline frame of that edge (`end` is exclusive). Growing over
    /// a neighbour overwrites it; the media's length is the limit.
    pub frame: i64,
}

#[derive(Debug, Clone, PartialEq, Deserialize, JsonSchema)]
pub struct SetClipPropertiesArgs {
    pub timeline_id: String,
    /// The timeline's `revision` from your last read: the call is refused
    /// if the timeline changed since (e.g. the user edited it meanwhile).
    #[serde(default)]
    pub if_revision: Option<String>,
    pub clip_ids: Vec<String>,
    /// Percent, 0-100.
    #[serde(default)]
    pub opacity: Option<f32>,
    /// `[x, y]` displacement in timeline pixels from the center, Y up.
    #[serde(default)]
    pub position: Option<[f32; 2]>,
    /// `[x, y]` magnification, 1 = original size.
    #[serde(default)]
    pub scale: Option<[f32; 2]>,
    /// Degrees, clockwise.
    #[serde(default)]
    pub rotation: Option<f32>,
    /// Audio gain in dB, 0 = unchanged.
    #[serde(default)]
    pub gain_db: Option<f32>,
    /// A disabled clip is neither seen nor heard.
    #[serde(default)]
    pub disabled: Option<bool>,
    /// Fade in length, in timeline frames.
    #[serde(default)]
    pub fade_in: Option<i64>,
    /// Fade out length, in timeline frames.
    #[serde(default)]
    pub fade_out: Option<i64>,
    /// `[r, g, b, a]` in 0-1: the fill of a solid color clip. Not the clip's
    /// color on the timeline: that is `set_clip_color`.
    #[serde(default)]
    pub fill_color: Option<[f32; 4]>,
}

#[derive(Debug, Clone, PartialEq, Deserialize, JsonSchema)]
pub struct AddTitleArgs {
    pub timeline_id: String,
    /// The timeline's `revision` from your last read: the call is refused
    /// if the timeline changed since (e.g. the user edited it meanwhile).
    #[serde(default)]
    pub if_revision: Option<String>,
    pub text: String,
    /// Timeline start frame; what is there on the track is overwritten.
    pub at: i64,
    /// In timeline frames. Default 5 s.
    #[serde(default)]
    pub duration: Option<i64>,
    /// Video track ("V1", ...). Default: the first unlocked one.
    #[serde(default)]
    pub track: Option<String>,
    /// Font size in timeline pixels.
    #[serde(default)]
    pub size: Option<f32>,
    /// `[r, g, b, a]` in 0-1.
    #[serde(default)]
    pub color: Option<[f32; 4]>,
    /// `[x, y]` from the center of the frame in timeline pixels, Y up.
    #[serde(default)]
    pub position: Option<[f32; 2]>,
}

#[derive(Debug, Clone, PartialEq, Deserialize, JsonSchema)]
pub struct AddSolidColorArgs {
    pub timeline_id: String,
    /// The timeline's `revision` from your last read: the call is refused
    /// if the timeline changed since (e.g. the user edited it meanwhile).
    #[serde(default)]
    pub if_revision: Option<String>,
    /// Timeline start frame; what is there on the track is overwritten.
    pub at: i64,
    /// In timeline frames. Default 5 s.
    #[serde(default)]
    pub duration: Option<i64>,
    /// Video track ("V1", ...). Default: the first unlocked one.
    #[serde(default)]
    pub track: Option<String>,
    /// `[r, g, b, a]` in 0-1.
    #[serde(default)]
    pub color: Option<[f32; 4]>,
}

#[derive(Debug, Clone, PartialEq, Deserialize, JsonSchema)]
pub struct AddAdjustmentClipArgs {
    pub timeline_id: String,
    /// The timeline's `revision` from your last read: the call is refused
    /// if the timeline changed since (e.g. the user edited it meanwhile).
    #[serde(default)]
    pub if_revision: Option<String>,
    /// Timeline start frame; what is there on the track is overwritten.
    pub at: i64,
    /// In timeline frames. Default 5 s.
    #[serde(default)]
    pub duration: Option<i64>,
    /// Video track ("V1", ...). Default: the first unlocked one.
    #[serde(default)]
    pub track: Option<String>,
}

/// The editor's palette for clips and markers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum PaletteColor {
    Red,
    Orange,
    Yellow,
    Green,
    Cyan,
    Blue,
    Indigo,
    Purple,
    Magenta,
    Rose,
    Slate,
    Gray,
}

/// A clip's color on the timeline: a palette color, or `none` for the
/// default one of its kind (blue video, green audio...).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum ClipColorArg {
    Red,
    Orange,
    Yellow,
    Green,
    Cyan,
    Blue,
    Indigo,
    Purple,
    Magenta,
    Rose,
    Slate,
    Gray,
    None,
}

#[derive(Debug, Clone, PartialEq, Deserialize, JsonSchema)]
pub struct SetClipColorArgs {
    pub timeline_id: String,
    /// The timeline's `revision` from your last read: the call is refused
    /// if the timeline changed since (e.g. the user edited it meanwhile).
    #[serde(default)]
    pub if_revision: Option<String>,
    pub clip_ids: Vec<String>,
    /// How the clips look on the timeline, to tag them (e.g. takes to
    /// review). It does not change the picture.
    pub color: ClipColorArg,
}

#[derive(Debug, Clone, PartialEq, Deserialize, JsonSchema)]
pub struct SetClipMasksArgs {
    pub timeline_id: String,
    /// The timeline's `revision` from your last read: the call is refused
    /// if the timeline changed since (e.g. the user edited it meanwhile).
    #[serde(default)]
    pub if_revision: Option<String>,
    /// A video clip. On an adjustment clip the masks limit where its
    /// filters apply; on any other clip, where the clip shows.
    pub clip_id: String,
    /// Replaces the clip's masks; empty removes them. They combine in
    /// order, each by its `mode`.
    pub masks: Vec<MaskArg>,
}

/// Coordinates are pixels of the timeline from the center of the clip as
/// it shows at zoom 1, Y up; the clip's transform moves the mask with it.
#[derive(Debug, Clone, PartialEq, Deserialize, JsonSchema)]
pub struct MaskArg {
    pub shape: MaskShapeArg,
    /// Show (or process) outside the shape instead of inside.
    #[serde(default)]
    pub invert: bool,
    /// How it combines with the masks before it (default `add`).
    #[serde(default)]
    pub mode: Option<MaskModeArg>,
    /// Default `[0, 0]`.
    #[serde(default)]
    pub center: Option<[f32; 2]>,
    /// Rectangle and ellipse: `[width, height]`. Default half the timeline.
    #[serde(default)]
    pub size: Option<[f32; 2]>,
    /// Degrees, clockwise.
    #[serde(default)]
    pub rotation: Option<f32>,
    /// Rectangle: corner radius in pixels.
    #[serde(default)]
    pub roundness: Option<f32>,
    /// Width in pixels of the soft edge.
    #[serde(default)]
    pub feather: Option<f32>,
    /// Pixels the shape grows (negative: shrinks).
    #[serde(default)]
    pub expansion: Option<f32>,
    /// 0-100, default 100.
    #[serde(default)]
    pub opacity: Option<f32>,
    /// Path: the vertices `[x, y]` of the closed polygon, relative to
    /// `center`. At least 3.
    #[serde(default)]
    pub points: Option<Vec<[f32; 2]>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum MaskShapeArg {
    Rectangle,
    Ellipse,
    Path,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum MaskModeArg {
    Add,
    Subtract,
    Intersect,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum TransitionKindArg {
    Push,
    None,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum DirectionArg {
    Left,
    Right,
    Up,
    Down,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum EaseArg {
    None,
    In,
    Out,
    InOut,
}

#[derive(Debug, Clone, PartialEq, Deserialize, JsonSchema)]
pub struct SetTransitionArgs {
    pub timeline_id: String,
    /// The timeline's `revision` from your last read: the call is refused
    /// if the timeline changed since (e.g. the user edited it meanwhile).
    #[serde(default)]
    pub if_revision: Option<String>,
    /// Video clips.
    pub clip_ids: Vec<String>,
    /// `start`: the clip enters; `end`: it leaves.
    pub edge: EdgeArg,
    /// `push` (default) slides the clip in or out of the frame; `none`
    /// removes the transition from that edge.
    #[serde(default)]
    pub kind: Option<TransitionKindArg>,
    /// Timeline frames from the edge, at most the clip's length. Default
    /// 0.45 s, or the value already set on that edge (as for the options
    /// below).
    #[serde(default)]
    pub duration: Option<i64>,
    /// Where the picture moves on screen. Default `right`.
    #[serde(default)]
    pub direction: Option<DirectionArg>,
    /// Default `in_out`.
    #[serde(default)]
    pub ease: Option<EaseArg>,
    /// Strength of the ease, 0-1. Default 0.5.
    #[serde(default)]
    pub curve: Option<f32>,
}

#[derive(Debug, Clone, PartialEq, Deserialize, JsonSchema)]
pub struct AddMarkerArgs {
    pub timeline_id: String,
    /// The timeline's `revision` from your last read: the call is refused
    /// if the timeline changed since (e.g. the user edited it meanwhile).
    #[serde(default)]
    pub if_revision: Option<String>,
    /// Timeline frame.
    pub at: i64,
    /// In timeline frames; 0 (default) marks a single frame.
    #[serde(default)]
    pub duration: Option<i64>,
    #[serde(default)]
    pub note: Option<String>,
    /// Default yellow.
    #[serde(default)]
    pub color: Option<PaletteColor>,
}

#[derive(Debug, Clone, PartialEq, Deserialize, JsonSchema)]
pub struct EditMarkerArgs {
    pub timeline_id: String,
    /// The timeline's `revision` from your last read: the call is refused
    /// if the timeline changed since (e.g. the user edited it meanwhile).
    #[serde(default)]
    pub if_revision: Option<String>,
    pub marker_id: String,
    #[serde(default)]
    pub at: Option<i64>,
    #[serde(default)]
    pub duration: Option<i64>,
    #[serde(default)]
    pub note: Option<String>,
    #[serde(default)]
    pub color: Option<PaletteColor>,
}

#[derive(Debug, Clone, PartialEq, Deserialize, JsonSchema)]
pub struct MarkerArgs {
    pub timeline_id: String,
    /// The timeline's `revision` from your last read: the call is refused
    /// if the timeline changed since (e.g. the user edited it meanwhile).
    #[serde(default)]
    pub if_revision: Option<String>,
    pub marker_id: String,
}

#[derive(Debug, Clone, PartialEq, Deserialize, JsonSchema)]
pub struct OpenProjectArgs {
    /// Path of a `.vvproj` file.
    pub path: String,
}

#[derive(Debug, Clone, PartialEq, Deserialize, JsonSchema)]
pub struct SaveProjectArgs {
    /// Where to save; omitted, the project's current file.
    #[serde(default)]
    pub path: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Deserialize, JsonSchema)]
pub struct ImportMediaArgs {
    /// Video, audio or image files. Files already in the media pool are
    /// skipped.
    pub paths: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Deserialize, JsonSchema)]
pub struct CreateTimelineArgs {
    pub name: String,
    /// Takes fps and resolution from this media (its video, if any).
    #[serde(default)]
    pub from_media: Option<String>,
    /// Frames per second as `[numerator, denominator]`, e.g. `[30000, 1001]`.
    /// Default 25, or the media's with `from_media`.
    #[serde(default)]
    pub fps: Option<[i32; 2]>,
    /// `[width, height]` in pixels. Default 1920x1080, or the media's with
    /// `from_media`.
    #[serde(default)]
    pub resolution: Option<[u32; 2]>,
}

/// What a tool returns: JSON for the agent, plus an image for the visual
/// tools.
#[derive(Debug, Clone, PartialEq)]
pub struct ToolOutput {
    pub value: serde_json::Value,
    pub image_png: Option<Vec<u8>>,
}

impl ToolOutput {
    pub fn json(value: serde_json::Value) -> Self {
        Self {
            value,
            image_png: None,
        }
    }
}

/// A failed call, reported to the agent as a tool error (not a protocol
/// error) with this message.
#[derive(Debug, Clone, PartialEq)]
pub struct ToolError(pub String);

impl std::fmt::Display for ToolError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

pub type ToolResult = Result<ToolOutput, ToolError>;

#[derive(Debug, Clone, PartialEq, Deserialize, JsonSchema)]
pub struct RenderFrameArgs {
    pub timeline_id: String,
    /// Timeline frame.
    pub frame: i64,
    /// Width of the image in pixels, at most the timeline's; the height
    /// keeps the aspect ratio. Default 960.
    #[serde(default)]
    pub max_width: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Deserialize, JsonSchema)]
pub struct AudioLevelsArgs {
    /// Measure a media file (its frames and fps)...
    #[serde(default)]
    pub media_id: Option<String>,
    /// ...which of its audio streams, default 0...
    #[serde(default)]
    pub stream: Option<usize>,
    /// ...or what a timeline plays, all audible tracks mixed (its frames).
    #[serde(default)]
    pub timeline_id: Option<String>,
    /// First frame measured.
    pub start: i64,
    /// Frame after the last one measured.
    pub end: i64,
    /// Frames per measurement window, default 1. At most 20000 windows per
    /// call.
    #[serde(default)]
    pub window: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Deserialize, JsonSchema)]
pub struct ExportArgs {
    pub timeline_id: String,
    /// Output file, e.g. `/home/me/out.mp4` (H.264 + AAC).
    pub path: String,
    /// `[start, end)` timeline frames; default the whole timeline.
    #[serde(default)]
    pub range: Option<[i64; 2]>,
    /// Output size as a percentage of the timeline resolution, default 100.
    #[serde(default)]
    pub scale_percent: Option<u32>,
    /// Include the audio. Default true.
    #[serde(default = "yes")]
    pub audio: bool,
}

#[derive(Debug, Clone, PartialEq, Deserialize, JsonSchema)]
pub struct JobArgs {
    pub job_id: String,
}
