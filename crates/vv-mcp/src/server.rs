//! The MCP protocol side: each tool translates its arguments into a
//! `ToolCall`, submits it to the host and waits for the result.

use base64::Engine;
use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{CallToolResult, ContentBlock, Implementation, ServerCapabilities, ServerConfig};
use rmcp::{ErrorData, ServerHandler, ServiceExt, tool, tool_handler, tool_router};
use vv_session::Session;

use crate::tools::*;
use crate::{McpHandle, channel, run_headless};

const INSTRUCTIONS: &str = "\
Venturi is a video editor. Ids (media, timelines, clips, markers) are strings. \
Positions and lengths are integer frames of the timeline (fps in `get_timeline`); \
only `source_in`/`source_out` are frames of the media. Tracks are named V1, V2... \
and A1, A2... Every editing call is one undo step (`undo` reverts it); locked tracks \
are never touched. A typical session: import_media, create_timeline with from_media, \
insert_clip, then edit and save_project. To cut pauses or bad takes, delete_ranges \
with ripple keeps audio and video in sync. \
Every result describing a timeline carries its `revision`; pass it back as `if_revision` \
when editing, so an edit computed on a timeline that changed meanwhile (the user may be \
editing it too) is refused instead of landing in the wrong place. \
Venturi does not transcribe. To cut by words (repeated takes, a script), use a \
speech-to-text tool installed once (Venturi's repository has one in container/whisper/), \
transcribe only the media ranges the timeline uses (clips' source_in/source_out, about \
2 s of margin each side), not whole files, keep the times as media frames and cut with \
delete_ranges and media_id. Details in docs/MCP.md, section Transcribing.";

type ToolReturn = Result<CallToolResult, ErrorData>;

#[derive(Clone)]
pub struct VenturiServer {
    handle: McpHandle,
    tool_router: ToolRouter<Self>,
}

#[tool_router]
impl VenturiServer {
    pub fn new(handle: McpHandle) -> Self {
        Self {
            handle,
            tool_router: Self::tool_router(),
        }
    }

    async fn call(&self, call: ToolCall) -> ToolReturn {
        let result = self
            .handle
            .submit(call)
            .await
            .unwrap_or_else(|_| Err(ToolError("Venturi stopped before answering".into())));
        Ok(match result {
            Ok(output) => {
                let text = serde_json::to_string_pretty(&output.value)
                    .unwrap_or_else(|e| format!("unserializable result: {e}"));
                let mut content = vec![ContentBlock::text(text)];
                if let Some(png) = output.image_png {
                    content.push(ContentBlock::image(
                        base64::engine::general_purpose::STANDARD.encode(png),
                        "image/png",
                    ));
                }
                CallToolResult::success(content)
            }
            Err(e) => CallToolResult::error(vec![ContentBlock::text(e.0)]),
        })
    }

    #[tool(
        description = "The project: media pool (ids, kind, fps, duration in frames and seconds, resolution, audio streams, offline flag), timelines (id, fps, resolution, length), folders, file path and unsaved flag."
    )]
    async fn get_project(&self) -> ToolReturn {
        self.call(ToolCall::GetProject).await
    }

    #[tool(
        description = "A timeline in full: tracks (name, kind, muted/solo/locked) with their clips (id, start, end exclusive, source media and its in/out frames, link group, fades, which effects are set) and markers."
    )]
    async fn get_timeline(&self, Parameters(args): Parameters<TimelineArgs>) -> ToolReturn {
        self.call(ToolCall::GetTimeline(args)).await
    }

    #[tool(
        description = "One clip with all its effect values (transform, opacity, gain, color, title, filters, transitions, keyframes)."
    )]
    async fn get_clip(&self, Parameters(args): Parameters<ClipArgs>) -> ToolReturn {
        self.call(ToolCall::GetClip(args)).await
    }

    #[tool(description = "Replaces the open project with an empty one. Unsaved changes are lost.")]
    async fn new_project(&self) -> ToolReturn {
        self.call(ToolCall::NewProject).await
    }

    #[tool(
        description = "Opens a .vvproj file, replacing the open project. Unsaved changes are lost."
    )]
    async fn open_project(&self, Parameters(args): Parameters<OpenProjectArgs>) -> ToolReturn {
        self.call(ToolCall::OpenProject(args)).await
    }

    #[tool(description = "Saves the project, to `path` or to its current file.")]
    async fn save_project(&self, Parameters(args): Parameters<SaveProjectArgs>) -> ToolReturn {
        self.call(ToolCall::SaveProject(args)).await
    }

    #[tool(
        description = "Adds files to the media pool and answers once they are probed, with their ids and metadata (files already in the pool included) and the files that failed."
    )]
    async fn import_media(&self, Parameters(args): Parameters<ImportMediaArgs>) -> ToolReturn {
        self.call(ToolCall::ImportMedia(args)).await
    }

    #[tool(
        description = "Imports the timelines of an OpenTimelineIO file (e.g. from DaVinci Resolve) with their media, into a folder named after the file."
    )]
    async fn import_otio(&self, Parameters(args): Parameters<ImportOtioArgs>) -> ToolReturn {
        self.call(ToolCall::ImportOtio(args)).await
    }

    #[tool(
        description = "Creates a timeline with tracks V1 and A1; fps and resolution from `from_media` or given explicitly. Not an undo step."
    )]
    async fn create_timeline(
        &self,
        Parameters(args): Parameters<CreateTimelineArgs>,
    ) -> ToolReturn {
        self.call(ToolCall::CreateTimeline(args)).await
    }

    #[tool(description = "Appends a video or audio track; returns its name.")]
    async fn add_track(&self, Parameters(args): Parameters<AddTrackArgs>) -> ToolReturn {
        self.call(ToolCall::AddTrack(args)).await
    }

    #[tool(description = "Mutes, solos or locks a track.")]
    async fn set_track(&self, Parameters(args): Parameters<SetTrackArgs>) -> ToolReturn {
        self.call(ToolCall::SetTrack(args)).await
    }

    #[tool(
        description = "Puts a portion of a media on the timeline at frame `at`, overwriting what is there: a video clip plus one linked audio clip per audio stream. Returns the new clips."
    )]
    async fn insert_clip(&self, Parameters(args): Parameters<InsertClipArgs>) -> ToolReturn {
        self.call(ToolCall::InsertClip(args)).await
    }

    #[tool(
        description = "Cuts clips at a timeline frame. The right halves of linked clips are linked to each other. Returns left and right ids."
    )]
    async fn split(&self, Parameters(args): Parameters<SplitArgs>) -> ToolReturn {
        self.call(ToolCall::Split(args)).await
    }

    #[tool(description = "Deletes clips, leaving gaps, or with `ripple` closing them.")]
    async fn delete_clips(&self, Parameters(args): Parameters<DeleteClipsArgs>) -> ToolReturn {
        self.call(ToolCall::DeleteClips(args)).await
    }

    #[tool(
        description = "Removes ranges [start, end) from the tracks, cutting the clips at the edges; with `ripple` the later material slides back, keeping all tracks in sync. With `media_id` the ranges are frames of that media (e.g. from its audio levels or a transcript) and Venturi finds where they are on the timeline, even after earlier cuts: prefer it whenever the ranges come from the media. The tool for removing silences or bad takes in one step."
    )]
    async fn delete_ranges(&self, Parameters(args): Parameters<DeleteRangesArgs>) -> ToolReturn {
        self.call(ToolCall::DeleteRanges(args)).await
    }

    #[tool(
        description = "Moves clips to a new start frame and/or track of the same kind, overwriting what is at the destination."
    )]
    async fn move_clips(&self, Parameters(args): Parameters<MoveClipsArgs>) -> ToolReturn {
        self.call(ToolCall::MoveClips(args)).await
    }

    #[tool(
        description = "Moves the start or end edge of a clip; the content under the other edge stays where it is."
    )]
    async fn trim_clip(&self, Parameters(args): Parameters<TrimClipArgs>) -> ToolReturn {
        self.call(ToolCall::TrimClip(args)).await
    }

    #[tool(
        description = "Sets static values on clips: opacity, position, scale, rotation, gain, fades, enabled state, fill of solid color clips. Parameters with keyframes keep following them (reported as warnings)."
    )]
    async fn set_clip_properties(
        &self,
        Parameters(args): Parameters<SetClipPropertiesArgs>,
    ) -> ToolReturn {
        self.call(ToolCall::SetClipProperties(args)).await
    }

    #[tool(description = "Adds a text title clip on a video track.")]
    async fn add_title(&self, Parameters(args): Parameters<AddTitleArgs>) -> ToolReturn {
        self.call(ToolCall::AddTitle(args)).await
    }

    #[tool(description = "Adds a solid color clip on a video track.")]
    async fn add_solid_color(&self, Parameters(args): Parameters<AddSolidColorArgs>) -> ToolReturn {
        self.call(ToolCall::AddSolidColor(args)).await
    }

    #[tool(
        description = "Adds an adjustment clip on a video track: its effects apply to everything below it."
    )]
    async fn add_adjustment_clip(
        &self,
        Parameters(args): Parameters<AddAdjustmentClipArgs>,
    ) -> ToolReturn {
        self.call(ToolCall::AddAdjustmentClip(args)).await
    }

    #[tool(
        description = "Links clips into one group: they are selected, moved and deleted together in the editor."
    )]
    async fn link_clips(&self, Parameters(args): Parameters<ClipsArgs>) -> ToolReturn {
        self.call(ToolCall::LinkClips(args)).await
    }

    #[tool(
        description = "Dissolves the link groups of these clips (the whole group, not only the given clips)."
    )]
    async fn unlink_clips(&self, Parameters(args): Parameters<ClipsArgs>) -> ToolReturn {
        self.call(ToolCall::UnlinkClips(args)).await
    }

    #[tool(
        description = "Adds a marker on the timeline ruler, on one frame or over a range, with an optional note and color (default yellow)."
    )]
    async fn add_marker(&self, Parameters(args): Parameters<AddMarkerArgs>) -> ToolReturn {
        self.call(ToolCall::AddMarker(args)).await
    }

    #[tool(description = "The markers of a timeline: id, start frame, duration, note, color.")]
    async fn get_markers(&self, Parameters(args): Parameters<TimelineArgs>) -> ToolReturn {
        self.call(ToolCall::GetMarkers(args)).await
    }

    #[tool(
        description = "Colors clips on the timeline with a palette color (or `none` for their default), e.g. to tag takes. It does not change the picture. `get_timeline` and `get_clip` report it as `clip_color`."
    )]
    async fn set_clip_color(&self, Parameters(args): Parameters<SetClipColorArgs>) -> ToolReturn {
        self.call(ToolCall::SetClipColor(args)).await
    }

    #[tool(
        description = "Sets the masks of a video clip: rectangles, ellipses or polygons, soft-edged with `feather`, possibly inverted. On an adjustment clip they limit where its filters apply (e.g. blur or darken only a region, or everything but it); on any other clip, where the clip shows. Replaces the clip's masks; `get_clip` reports them under `effects.masks`."
    )]
    async fn set_clip_masks(&self, Parameters(args): Parameters<SetClipMasksArgs>) -> ToolReturn {
        self.call(ToolCall::SetClipMasks(args)).await
    }

    #[tool(
        description = "Sets or removes (`kind: none`) the transition on one edge of video clips: `push` slides the clip in from, or out to, the edge of the frame over what is below. Options not given keep the value already on that edge, else the defaults. `get_clip` reports it as `transition_in`/`transition_out`."
    )]
    async fn set_transition(&self, Parameters(args): Parameters<SetTransitionArgs>) -> ToolReturn {
        self.call(ToolCall::SetTransition(args)).await
    }

    #[tool(description = "Changes a marker's position, duration, note or color.")]
    async fn edit_marker(&self, Parameters(args): Parameters<EditMarkerArgs>) -> ToolReturn {
        self.call(ToolCall::EditMarker(args)).await
    }

    #[tool(description = "Deletes a marker.")]
    async fn delete_marker(&self, Parameters(args): Parameters<MarkerArgs>) -> ToolReturn {
        self.call(ToolCall::DeleteMarker(args)).await
    }

    #[tool(
        description = "Renders one timeline frame as a PNG image, decoded exactly (not from a preview cache) and composited as the export does. Use it to check an edit visually."
    )]
    async fn render_frame(&self, Parameters(args): Parameters<RenderFrameArgs>) -> ToolReturn {
        self.call(ToolCall::RenderFrame(args)).await
    }

    #[tool(
        description = "Audio loudness per window of frames: RMS and peak in dBFS (floored at -120, digital silence), from the decoded samples. Of one stream of a media (media frames) or of the timeline mix (timeline frames). Thresholds are yours: e.g. RMS below -45 dB for 10+ consecutive frames is usually a pause."
    )]
    async fn get_audio_levels(&self, Parameters(args): Parameters<AudioLevelsArgs>) -> ToolReturn {
        self.call(ToolCall::GetAudioLevels(args)).await
    }

    #[tool(
        description = "Starts exporting a timeline (or a range of it) to a video file in the background; returns a job id. Edits made afterwards do not affect it. One export at a time."
    )]
    async fn export(&self, Parameters(args): Parameters<ExportArgs>) -> ToolReturn {
        self.call(ToolCall::Export(args)).await
    }

    #[tool(
        description = "Progress of an export: state running/done/failed/cancelled, frames written of the total, elapsed time, error."
    )]
    async fn export_status(&self, Parameters(args): Parameters<JobArgs>) -> ToolReturn {
        self.call(ToolCall::ExportStatus(args)).await
    }

    #[tool(description = "Stops a running export; the partial file is left as is.")]
    async fn cancel_export(&self, Parameters(args): Parameters<JobArgs>) -> ToolReturn {
        self.call(ToolCall::CancelExport(args)).await
    }

    #[tool(
        description = "Editor window only: the open timeline, the playhead, what the user selected. Lets the user point at something and say \"this\"."
    )]
    async fn get_state(&self) -> ToolReturn {
        self.call(ToolCall::GetState).await
    }

    #[tool(description = "Editor window only: a PNG screenshot of the whole Venturi window.")]
    async fn screenshot_ui(&self) -> ToolReturn {
        self.call(ToolCall::ScreenshotUi).await
    }

    #[tool(description = "Editor window only: shows this timeline in the editor.")]
    async fn set_active_timeline(&self, Parameters(args): Parameters<TimelineArgs>) -> ToolReturn {
        self.call(ToolCall::SetActiveTimeline(args)).await
    }

    #[tool(
        description = "Undoes the last step: one editing call, or one action of the user in the editor."
    )]
    async fn undo(&self) -> ToolReturn {
        self.call(ToolCall::Undo).await
    }

    #[tool(description = "Redoes the last undone step.")]
    async fn redo(&self) -> ToolReturn {
        self.call(ToolCall::Redo).await
    }
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for VenturiServer {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new("venturi", env!("CARGO_PKG_VERSION")))
            .with_instructions(INSTRUCTIONS)
    }
}

/// Serves MCP on stdin/stdout until the client disconnects. Blocking.
pub fn serve_stdio(handle: McpHandle) -> std::io::Result<()> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    runtime.block_on(async {
        let service = VenturiServer::new(handle)
            .serve(rmcp::transport::stdio())
            .await
            .map_err(std::io::Error::other)?;
        service.waiting().await.map_err(std::io::Error::other)?;
        Ok(())
    })
}

/// `vv-app mcp`: the session is served on this thread, the protocol on
/// another; returns when the client disconnects.
pub fn serve_headless_stdio(session: Session) -> std::io::Result<()> {
    let (handle, inbox) = channel(|| {});
    let transport = std::thread::spawn(move || serve_stdio(handle));
    run_headless(session, inbox);
    transport
        .join()
        .unwrap_or_else(|_| Err(std::io::Error::other("MCP transport panicked")))
}

#[cfg(test)]
#[path = "tests/server.rs"]
mod tests;
