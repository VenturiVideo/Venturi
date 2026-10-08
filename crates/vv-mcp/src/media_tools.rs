//! Tools that decode or encode media: they run on a worker thread over a
//! snapshot of the project, so the host keeps answering meanwhile.

use std::path::PathBuf;
use std::sync::{OnceLock, RwLock, mpsc};

use serde_json::json;
use vv_core::{FrameIdx, ProcessingPrecision, Project, TimelineId};
use vv_session::Session;
use vv_session::analysis::{FLOOR_DB, Level};
use vv_session::export::{ExportError, ExportSettings};

use crate::dispatch::{Dispatch, Pending};
use crate::ids;
use crate::tools::*;

const MAX_WINDOWS: i64 = 20_000;
const DEFAULT_RENDER_WIDTH: u32 = 960;

fn fail(message: impl Into<String>) -> Dispatch {
    Dispatch::Handled(Err(ToolError(message.into())))
}

/// Runs `work` on its own thread; the result comes back through `Pending`.
fn on_worker(session: &Session, work: impl FnOnce() -> ToolResult + Send + 'static) -> Dispatch {
    let (tx, rx) = mpsc::channel();
    let waker = session.waker();
    std::thread::spawn(move || {
        let _ = tx.send(work());
        waker.wake();
    });
    Dispatch::Deferred(Pending::Worker(rx))
}

/// Runs `render` on the one compositor, set to `precision`: creating a GPU
/// device takes long, so it is not one per render nor per precision.
fn with_compositor<R>(
    precision: ProcessingPrecision,
    render: impl FnOnce(&vv_render::Compositor) -> R,
) -> R {
    static COMPOSITOR: OnceLock<RwLock<vv_render::Compositor>> = OnceLock::new();
    let lock = COMPOSITOR.get_or_init(|| {
        RwLock::new(vv_render::Compositor::new_headless_with_precision(
            precision,
        ))
    });
    loop {
        let compositor = lock.read().unwrap();
        if compositor.precision() == precision {
            return render(&compositor);
        }
        drop(compositor);
        lock.write().unwrap().set_precision(precision);
    }
}

fn export_error(e: ExportError) -> ToolError {
    ToolError(e.to_string())
}

pub fn encode_png(rgba: &[u8], (width, height): (u32, u32)) -> Result<Vec<u8>, ToolError> {
    let mut png = Vec::new();
    let mut encoder = png::Encoder::new(&mut png, width, height);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    encoder
        .write_header()
        .and_then(|mut writer| writer.write_image_data(rgba))
        .map_err(|e| ToolError(format!("cannot encode the PNG: {e}")))?;
    Ok(png)
}

pub(crate) fn render_frame(session: &Session, args: RenderFrameArgs) -> Dispatch {
    let project = &session.project;
    let timeline = match ids::timeline_id(project, &args.timeline_id) {
        Ok(id) => id,
        Err(e) => return Dispatch::Handled(Err(e)),
    };
    let tl = &project.timelines[timeline];
    let length = tl.total_frames();
    if !(0..length).contains(&args.frame) {
        return fail(format!(
            "frame {} is outside the timeline, 0 to {}",
            args.frame,
            length - 1
        ));
    }
    let (full_width, full_height) = tl.resolution;
    let width = args
        .max_width
        .unwrap_or(DEFAULT_RENDER_WIDTH)
        .clamp(1, full_width);
    let height = ((full_height as u64 * width as u64) / full_width as u64).max(1) as u32;
    let snapshot = project.clone();
    let frame = args.frame;
    on_worker(session, move || {
        let rgba = with_compositor(snapshot.precision, |compositor| {
            vv_session::export::render_frame_rgba(
                &snapshot,
                timeline,
                frame,
                (width, height),
                compositor,
            )
        })
        .map_err(export_error)?;
        Ok(ToolOutput {
            value: json!({ "frame": frame, "width": width, "height": height }),
            image_png: Some(encode_png(&rgba, (width, height))?),
        })
    })
}

fn round_db(value: f32) -> f64 {
    (value as f64 * 10.0).round() / 10.0
}

fn levels_output(
    levels: Vec<Level>,
    fps: vv_core::Rational,
    start: i64,
    end: i64,
    window: i64,
) -> ToolOutput {
    ToolOutput::json(json!({
        "fps": crate::json::fps_json(fps),
        "start": start,
        "end": end,
        "window_frames": window,
        "floor_db": FLOOR_DB,
        "rms_db": levels.iter().map(|l| round_db(l.rms_db)).collect::<Vec<_>>(),
        "peak_db": levels.iter().map(|l| round_db(l.peak_db)).collect::<Vec<_>>(),
    }))
}

pub(crate) fn audio_levels(session: &Session, args: AudioLevelsArgs) -> Dispatch {
    let window = args.window.unwrap_or(1);
    if window < 1 {
        return fail("`window` must be at least one frame");
    }
    if args.start < 0 || args.end <= args.start {
        return fail(format!("empty range {}..{}", args.start, args.end));
    }
    let windows = (args.end - args.start + window - 1) / window;
    if windows > MAX_WINDOWS {
        return fail(format!(
            "{windows} windows asked, at most {MAX_WINDOWS}: use a larger `window` or a shorter range"
        ));
    }
    let project = &session.project;
    let (start, end) = (args.start, args.end);
    match (&args.media_id, &args.timeline_id) {
        (Some(media), None) => {
            let media = match ids::media_id(project, media) {
                Ok(id) => id,
                Err(e) => return Dispatch::Handled(Err(e)),
            };
            let item = &project.media_pool[media];
            if item.compound.is_some() {
                return fail("that media is a timeline: pass it as `timeline_id`");
            }
            let stream = args.stream.unwrap_or(0);
            let streams = if item.meta.has_audio {
                item.meta.audio_stream_count()
            } else {
                0
            };
            if stream >= streams {
                return fail(format!("the media has {streams} audio streams"));
            }
            let (path, fps) = (item.path.clone(), item.meta.fps);
            on_worker(session, move || {
                vv_session::analysis::media_audio_levels(&path, stream, fps, start..end, window)
                    .map(|levels| levels_output(levels, fps, start, end, window))
                    .map_err(ToolError)
            })
        }
        (None, Some(timeline)) => {
            let timeline = match ids::timeline_id(project, timeline) {
                Ok(id) => id,
                Err(e) => return Dispatch::Handled(Err(e)),
            };
            let fps = project.timelines[timeline].fps;
            let snapshot = project.clone();
            on_worker(session, move || {
                vv_session::analysis::timeline_audio_levels(&snapshot, timeline, start..end, window)
                    .map(|levels| levels_output(levels, fps, start, end, window))
                    .map_err(export_error)
            })
        }
        _ => fail("pass either `media_id` or `timeline_id`"),
    }
}

fn export_range(
    project: &Project,
    timeline: TimelineId,
    range: Option<[i64; 2]>,
) -> Result<std::ops::Range<FrameIdx>, ToolError> {
    let length = project.timelines[timeline].total_frames();
    let [start, end] = range.unwrap_or([0, length]);
    if start < 0 || end <= start || end > length {
        return Err(ToolError(format!(
            "range [{start}, {end}) is not inside the timeline, [0, {length})"
        )));
    }
    Ok(start..end)
}

pub(crate) fn export(session: &mut Session, args: ExportArgs) -> ToolResult {
    let timeline = ids::timeline_id(&session.project, &args.timeline_id)?;
    let range = export_range(&session.project, timeline, args.range)?;
    let path = PathBuf::from(&args.path);
    if path.file_name().is_none() || path.is_dir() {
        return Err(ToolError(format!("{} is not a file path", args.path)));
    }
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty())
        && !parent.is_dir()
    {
        return Err(ToolError(format!(
            "folder {} does not exist",
            parent.display()
        )));
    }
    let scale = args.scale_percent.unwrap_or(100);
    if !(1..=100).contains(&scale) {
        return Err(ToolError("scale_percent goes from 1 to 100".into()));
    }
    let mut settings = ExportSettings::preferred(path);
    settings.scale_percent = scale;
    if !args.audio {
        settings.audio = None;
    }
    let total = range.end - range.start;
    let (job, _) = session
        .export(timeline, settings, range)
        .ok_or_else(|| ToolError("another export is running".into()))?;
    Ok(ToolOutput::json(json!({
        "job_id": job.to_string(),
        "total_frames": total,
    })))
}

fn job_id(args: &JobArgs) -> Result<vv_session::JobId, ToolError> {
    args.job_id
        .trim()
        .parse()
        .map_err(|_| ToolError(format!("malformed job id \"{}\"", args.job_id)))
}

pub(crate) fn export_status(session: &Session, args: JobArgs) -> ToolResult {
    let job = job_id(&args)?;
    let progress = session
        .export_progress(job)
        .ok_or_else(|| ToolError(format!("no export \"{}\"", args.job_id)))?;
    let p = progress.lock().unwrap();
    let state = match (&p.error, p.done) {
        (Some(ExportError::Cancelled), _) => "cancelled",
        (Some(_), _) => "failed",
        (None, true) => "done",
        (None, false) => "running",
    };
    Ok(ToolOutput::json(json!({
        "state": state,
        "current_frame": p.current_frame,
        "total_frames": p.total_frames,
        "elapsed_secs": p.elapsed.as_secs_f64(),
        "fps": p.fps(),
        "stage_fps": {
            "decode": p.decode.fps(),
            "compose": p.compose.fps(),
            "encode": p.encode.fps(),
        },
        "error": p.error.as_ref().map(ToString::to_string),
    })))
}

pub(crate) fn cancel_export(session: &Session, args: JobArgs) -> ToolResult {
    let job = job_id(&args)?;
    if !session.cancel_export(job) {
        return Err(ToolError(format!(
            "export \"{}\" is not running",
            args.job_id
        )));
    }
    Ok(ToolOutput::json(json!({ "cancelling": args.job_id })))
}

#[cfg(test)]
#[path = "tests/media_tools.rs"]
mod tests;
