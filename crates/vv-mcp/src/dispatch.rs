use std::path::{Path, PathBuf};

use serde_json::{Value, json};
use vv_core::{Rational, Timeline, Track, TrackKind};
use vv_session::{JobId, OtioMerged, Session, SessionEvent};

use crate::ids::{self, key_to_string};
use crate::json::*;
use crate::tools::*;
use crate::{edit_tools, media_tools};

pub enum Dispatch {
    Handled(ToolResult),
    /// Waits for background work: the host feeds every `SessionEvent` to
    /// `Pending::resolve` until it returns the result.
    Deferred(Pending),
}

pub enum Pending {
    Import {
        job: JobId,
        paths: Vec<PathBuf>,
    },
    Otio {
        job: JobId,
        reuse_existing_media: bool,
    },
    /// Work on a thread of its own: the host checks it with `poll`.
    Worker(std::sync::mpsc::Receiver<ToolResult>),
}

impl Pending {
    /// The result of a `Worker`, once there.
    pub fn poll(&self) -> Option<ToolResult> {
        let Pending::Worker(result) = self else {
            return None;
        };
        match result.try_recv() {
            Ok(result) => Some(result),
            Err(std::sync::mpsc::TryRecvError::Empty) => None,
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                Some(Err(ToolError("the work failed unexpectedly".into())))
            }
        }
    }

    pub fn resolve(&self, session: &mut Session, event: &SessionEvent) -> Option<ToolResult> {
        match (self, event) {
            (
                Pending::Import { job, paths },
                SessionEvent::ImportFinished {
                    job: finished,
                    errors,
                    ..
                },
            ) if job == finished => {
                // Files already in the pool are reported too: the agent
                // needs their ids either way.
                let media: Vec<Value> = paths
                    .iter()
                    .filter_map(|path| session.media_with_path(path))
                    .map(|id| media_json(&session.project, id))
                    .collect();
                Some(Ok(ToolOutput::json(
                    json!({ "media": media, "errors": errors }),
                )))
            }
            (Pending::Otio { job, .. }, SessionEvent::OtioImported { job: done, result })
                if job == done =>
            {
                Some(Ok(ToolOutput::json(otio_json(session, result))))
            }
            (
                Pending::Otio {
                    job,
                    reuse_existing_media,
                },
                SessionEvent::OtioNeedsDecision { job: waiting },
            ) if job == waiting => {
                // Resolved by the `OtioImported` this produces.
                session.finish_otio_import(Some(*reuse_existing_media));
                None
            }
            (Pending::Otio { job, .. }, SessionEvent::OtioFailed { job: failed, error })
                if job == failed =>
            {
                Some(Err(ToolError(format!("OTIO import failed: {error}"))))
            }
            (Pending::Otio { job, .. }, SessionEvent::OtioCancelled { job: dropped })
                if job == dropped =>
            {
                Some(Err(ToolError("the OTIO import was cancelled".into())))
            }
            _ => None,
        }
    }
}

pub fn dispatch(session: &mut Session, call: ToolCall) -> Dispatch {
    match call {
        ToolCall::ImportMedia(args) => import_media(session, args),
        ToolCall::ImportOtio(args) => import_otio(session, args),
        ToolCall::RenderFrame(args) => media_tools::render_frame(session, args),
        ToolCall::GetAudioLevels(args) => media_tools::audio_levels(session, args),
        call => Dispatch::Handled(run(session, call)),
    }
}

fn run(session: &mut Session, call: ToolCall) -> ToolResult {
    match call {
        ToolCall::GetProject => Ok(ToolOutput::json(project_json(session))),
        ToolCall::GetTimeline(args) => {
            let timeline = ids::timeline_id(&session.project, &args.timeline_id)?;
            Ok(ToolOutput::json(timeline_detail_json(session, timeline)))
        }
        ToolCall::GetClip(args) => {
            let project = &session.project;
            let timeline = ids::timeline_id(project, &args.timeline_id)?;
            let (track, id) = ids::clip_ref(project, timeline, &args.clip_id)?;
            let clip = project.timelines[timeline]
                .clip(track, id)
                .expect("found above");
            Ok(ToolOutput::json(clip_detail_json(
                project, timeline, track, clip,
            )))
        }
        ToolCall::NewProject => {
            session.new_project();
            Ok(ToolOutput::json(project_json(session)))
        }
        ToolCall::OpenProject(args) => {
            session
                .open(Path::new(&args.path))
                .map_err(|e| ToolError(format!("cannot open {}: {e}", args.path)))?;
            Ok(ToolOutput::json(project_json(session)))
        }
        ToolCall::SaveProject(args) => {
            let path = match (args.path, session.path()) {
                (Some(path), _) => PathBuf::from(path),
                (None, Some(current)) => current.to_path_buf(),
                (None, None) => {
                    return Err(ToolError("the project has no file yet: pass `path`".into()));
                }
            };
            session
                .save_to(&path)
                .map_err(|e| ToolError(format!("cannot save to {}: {e}", path.display())))?;
            Ok(ToolOutput::json(json!({ "path": path })))
        }
        ToolCall::CreateTimeline(args) => create_timeline(session, args),
        ToolCall::AddTrack(args) => edit_tools::add_track(session, args),
        ToolCall::SetTrack(args) => edit_tools::set_track(session, args),
        ToolCall::InsertClip(args) => edit_tools::insert_clip(session, args),
        ToolCall::Split(args) => edit_tools::split(session, args),
        ToolCall::DeleteClips(args) => edit_tools::delete_clips(session, args),
        ToolCall::DeleteRanges(args) => edit_tools::delete_ranges(session, args),
        ToolCall::MoveClips(args) => edit_tools::move_clips(session, args),
        ToolCall::TrimClip(args) => edit_tools::trim_clip(session, args),
        ToolCall::SetClipProperties(args) => edit_tools::set_clip_properties(session, args),
        ToolCall::AddTitle(args) => edit_tools::add_title(session, args),
        ToolCall::AddSolidColor(args) => edit_tools::add_solid_color(session, args),
        ToolCall::AddAdjustmentClip(args) => edit_tools::add_adjustment_clip(session, args),
        ToolCall::LinkClips(args) => edit_tools::link_clips(session, args),
        ToolCall::UnlinkClips(args) => edit_tools::unlink_clips(session, args),
        ToolCall::AddMarker(args) => edit_tools::add_marker(session, args),
        ToolCall::EditMarker(args) => edit_tools::edit_marker(session, args),
        ToolCall::DeleteMarker(args) => edit_tools::delete_marker(session, args),
        ToolCall::GetMarkers(args) => edit_tools::get_markers(session, args),
        ToolCall::SetClipColor(args) => edit_tools::set_clip_color(session, args),
        ToolCall::SetClipMasks(args) => edit_tools::set_clip_masks(session, args),
        ToolCall::SetTransition(args) => edit_tools::set_transition(session, args),
        ToolCall::Export(args) => media_tools::export(session, args),
        ToolCall::ExportStatus(args) => media_tools::export_status(session, args),
        ToolCall::CancelExport(args) => media_tools::cancel_export(session, args),
        ToolCall::Undo => {
            let position = session.history.position();
            let Some(label) = position
                .checked_sub(1)
                .and_then(|last| session.history.labels().nth(last))
            else {
                return Err(ToolError("nothing to undo".into()));
            };
            session.history.undo(&mut session.project);
            Ok(ToolOutput::json(json!({ "undone": format!("{label:?}") })))
        }
        ToolCall::Redo => {
            let position = session.history.position();
            let Some(label) = session.history.labels().nth(position) else {
                return Err(ToolError("nothing to redo".into()));
            };
            session.history.redo(&mut session.project);
            Ok(ToolOutput::json(json!({ "redone": format!("{label:?}") })))
        }
        ToolCall::GetState | ToolCall::ScreenshotUi | ToolCall::SetActiveTimeline(_) => {
            Err(ToolError(
                "only available when attached to the Venturi window (vv-app mcp --attach)".into(),
            ))
        }
        ToolCall::ImportMedia(_)
        | ToolCall::ImportOtio(_)
        | ToolCall::RenderFrame(_)
        | ToolCall::GetAudioLevels(_) => Err(ToolError("deferred call run as immediate".into())),
    }
}

fn import_media(session: &mut Session, args: ImportMediaArgs) -> Dispatch {
    if args.paths.is_empty() {
        return Dispatch::Handled(Err(ToolError("no paths given".into())));
    }
    let paths: Vec<PathBuf> = args.paths.iter().map(PathBuf::from).collect();
    let job = session.import_media(paths.clone());
    Dispatch::Deferred(Pending::Import { job, paths })
}

fn import_otio(session: &mut Session, args: ImportOtioArgs) -> Dispatch {
    match session.import_otio(Path::new(&args.path)) {
        Some(job) => Dispatch::Deferred(Pending::Otio {
            job,
            reuse_existing_media: args.reuse_existing_media,
        }),
        None => Dispatch::Handled(Err(ToolError(
            "another OTIO import is still running".into(),
        ))),
    }
}

fn otio_json(session: &Session, result: &OtioMerged) -> Value {
    let project = &session.project;
    json!({
        "timelines": result.timelines.iter().map(|&id| timeline_json(session, id)).collect::<Vec<_>>(),
        "added_media": result.added_media.iter().map(|&id| media_json(project, id)).collect::<Vec<_>>(),
        "folder": result.folder.map(key_to_string),
        "warnings": result.warnings.iter().map(|w| format!("{w:?}")).collect::<Vec<_>>(),
    })
}

fn create_timeline(session: &mut Session, args: CreateTimelineArgs) -> ToolResult {
    let mut fps = Rational::new(25, 1);
    let mut resolution = (1920, 1080);
    if let Some(media) = &args.from_media {
        let meta = &session.project.media_pool[ids::media_id(&session.project, media)?].meta;
        fps = meta.fps;
        if meta.has_video {
            resolution = (meta.width, meta.height);
        }
    }
    if let Some([num, den]) = args.fps {
        if num <= 0 || den <= 0 {
            return Err(ToolError(format!("invalid fps {num}/{den}")));
        }
        fps = Rational::new(num, den);
    }
    if let Some([width, height]) = args.resolution {
        if width == 0 || height == 0 {
            return Err(ToolError(format!("invalid resolution {width}x{height}")));
        }
        resolution = (width, height);
    }
    let id = session.create_timeline(Timeline {
        name: args.name,
        fps,
        resolution,
        tracks: vec![Track::new(TrackKind::Video), Track::new(TrackKind::Audio)],
        markers: Vec::new(),
        master: Default::default(),
    });
    Ok(ToolOutput::json(timeline_json(session, id)))
}

#[cfg(test)]
#[path = "tests/dispatch.rs"]
pub(crate) mod tests;
