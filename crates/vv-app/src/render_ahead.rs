//! Video buffer at timeline level: a thread walks forward from the playhead
//! (and a little backwards) crossing cuts, gaps and tracks without special
//! cases, and fills a single `SharedFrameCache` on a global budget (see
//! plans/REFACTOR_PIPELINE.md §2). Decode only: the compositing stays on the UI thread,
//! the audio is played by the mixer.
//!
//! A compound clip is not decoded and not cached: its
//! content is a timeline, composed on the fly by whoever composes the outer
//! frame (`frame_provider::GpuCompounds`). Here its nested timeline is walked
//! anyway, at any depth, to keep warm the
//! real media that composition will read.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::ops::ControlFlow;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError, TryRecvError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use vv_core::{ClipSource, FrameIdx, MediaId, Project, Timeline, TimelineId};
use vv_media::proxy::ProxyQuality;
use vv_media::{Decoder, FrameYuv420, SharedFrameCache, WantedRange};

/// Seconds buffered ahead of the playhead, by default.
pub const DEFAULT_LOOKAHEAD_SECS: f64 = 3.0;

/// Seconds buffered behind the playhead, by default. Few on purpose:
/// they only serve a close back-and-forth scrub; the forward window
/// always has priority on the budget.
pub const DEFAULT_BEHIND_SECS: f64 = 2.0;

/// Minimum lookahead/behind margin even if configured to 0: without a
/// cushion the playback stutters from the normal timing variability.
const MIN_MARGIN_FRAMES: FrameIdx = 4;

/// Chunks the forward window is split into, per media: see
/// `chunk_forward_segments_near_to_far`.
const FORWARD_CHUNK_FRAMES: FrameIdx = 15;

/// Chunks the window behind the playhead is split into. Decoding it in
/// a single seek would produce last the frames near the playhead, and a
/// continuous backwards scrub would never reach them: from the nearest
/// chunk to the farthest the hole stays at most one chunk wide.
const BEHIND_CHUNK_FRAMES: FrameIdx = 15;

/// Cap on the transit (frames decoded before the wanted stretch) in frames,
/// not in budget bytes: a tight budget says nothing about the GOP
/// length. Generous: it only guards against pathological GOPs.
const TRANSIT_SAFETY_CAP_FRAMES: FrameIdx = 3000;

/// Whether the value changed.
fn store_secs(atomic: &AtomicU64, secs: f64) -> bool {
    let bits = secs.max(0.0).to_bits();
    atomic.swap(bits, Ordering::Relaxed) != bits
}

fn load_secs(atomic: &AtomicU64) -> f64 {
    f64::from_bits(atomic.load(Ordering::Relaxed))
}

/// Frames skipped per step of a transit (`Decoder::skip_some`) between two
/// checks of the live playhead.
const SKIP_STEP_FRAMES: usize = 8;

/// A cycle finding everything cached returns immediately: a short poll costs little.
const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// Decoding demand over the throughput measured at the same stride past
/// which fast playback of a media shows keyframes only: well over it, or the
/// normal playback would still beat a keyframe every GOP.
const KEYFRAMES_ABOVE_LOAD: f64 = 1.5;

/// The frame at the playhead missing at least this long during fast playback
/// (and longer than keyframes only would take to show the next one): the
/// decoder has fallen behind.
const FELL_BEHIND_AFTER: Duration = Duration::from_millis(250);

/// Share of the playback so far with the frame at the playhead missing past
/// which a freeze is no one-off.
const SUSTAINED_MISSING: f64 = 0.3;

/// Playback of a media watched at least this long before a freeze counts:
/// the first frame of a clip coming late is no sign of anything.
const SUSTAINED_OVER: Duration = Duration::from_secs(2);

/// GOP assumed until the decoder has seen one: the longest it estimates.
const UNKNOWN_GOP_FRAMES: FrameIdx = MAX_SEEK_THRESHOLD_FRAMES;

/// A seek while paused whose way to the frame is predicted longer than this
/// shows its keyframe first.
const KEYFRAME_FIRST_AFTER_SECS: f64 = 0.1;

/// A playhead jump this recent while paused means a scrub: a frame not cached
/// shows its keyframe first.
const SCRUBBING_FOR: Duration = Duration::from_millis(300);

/// Source frames per second assumed until one is measured.
const DEFAULT_THROUGHPUT_FPS: f64 = 120.0;

/// Frames decoded in sequence before a throughput sample is taken.
const THROUGHPUT_SAMPLE_FRAMES: FrameIdx = 60;

/// Seek threshold until the GOP of the media has been observed. Low: one
/// seek too many costs little, it reuses the open decoder.
const DEFAULT_SEEK_THRESHOLD_FRAMES: FrameIdx = 30;

/// Cap on the GOP estimate, until a tighter observation arrives.
const MAX_SEEK_THRESHOLD_FRAMES: FrameIdx = 300;

/// Seek threshold on the proxies: they are all-intra, seeking costs almost nothing.
/// The adaptive estimate there would stay high during a fast scrub and would
/// make dozens of useless frames be decoded in sequence.
const PROXY_SEEK_THRESHOLD_FRAMES: FrameIdx = 1;

enum Command {
    UpdateProject(Box<Project>, TimelineId),
    /// Goes through the commands and not through an atomic: the worker must react to the change
    /// by emptying caches and decoders (the frames have the wrong resolution).
    SetProxy(Option<ProxyQuality>),
    /// Like `SetProxy`: the cached frames have the wrong size.
    SetPreviewDivisor(u32),
    /// The HW decode setting changed: the decoders reopen with it, the
    /// frames already cached stay (same pixels).
    ReopenDecoders,
    /// Wakes the worker immediately instead of waiting for `POLL_INTERVAL`.
    Wake,
    Stop,
}

/// Diagnostic log on stderr with `VV_DEBUG_RENDER_AHEAD=1`.
fn debug_enabled() -> bool {
    static ENABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ENABLED.get_or_init(|| std::env::var("VV_DEBUG_RENDER_AHEAD").is_ok())
}

/// The playhead as last reported by the UI.
struct LiveTarget {
    frame: AtomicI64,
    /// Bumped every time the user moves the playhead (not playback): the
    /// cycle in progress serves a position no longer wanted.
    jumps: AtomicU64,
    /// See `RenderAhead::set_frame_stride`.
    frame_stride: AtomicI64,
    /// See `RenderAhead::set_playback_speed`: bits of an `f64`.
    speed: AtomicU64,
    /// See `RenderAhead::set_keyframes_when_behind`.
    keyframes_when_behind: AtomicBool,
}

impl LiveTarget {
    fn still(frame: FrameIdx) -> Self {
        Self {
            frame: AtomicI64::new(frame),
            jumps: AtomicU64::new(0),
            frame_stride: AtomicI64::new(1),
            speed: AtomicU64::new(0),
            keyframes_when_behind: AtomicBool::new(false),
        }
    }

    fn speed(&self) -> f64 {
        load_secs(&self.speed)
    }

    fn keyframes_when_behind(&self) -> bool {
        self.keyframes_when_behind.load(Ordering::Relaxed)
    }

    fn frame_stride(&self) -> FrameIdx {
        self.frame_stride.load(Ordering::Relaxed).max(1)
    }

    fn frame(&self) -> FrameIdx {
        self.frame.load(Ordering::Relaxed)
    }

    fn jumps(&self) -> u64 {
        self.jumps.load(Ordering::Relaxed)
    }
}

/// State shared between `RenderAhead` (UI thread) and the worker.
struct SharedState {
    caches: SharedFrameCache,
    target: LiveTarget,
    cache_budget_bytes: AtomicUsize,
    /// The worker's last cycle found the whole window cached: the
    /// UI stops asking for repaints for the "buffered" strip.
    caught_up: AtomicBool,
    /// Seconds (bits of an `f64`, see `store_secs`) to buffer ahead of
    /// and behind the playhead. Atomics and not `Command`s: the worker re-reads them on
    /// every cycle, there is no transition to react to.
    lookahead_secs: AtomicU64,
    behind_secs: AtomicU64,
}

/// See the module docs. One per `VenturiApp`.
pub struct RenderAhead {
    shared: Arc<SharedState>,
    tx: mpsc::Sender<Command>,
    handle: Option<JoinHandle<()>>,
    /// `frame_for` stood a keyframe in since `take_served_keyframe`.
    served_keyframe: bool,
}

impl RenderAhead {
    pub fn spawn(
        project: Project,
        timeline_id: TimelineId,
        cache_budget_bytes: usize,
        proxy: Option<ProxyQuality>,
        lookahead_secs: f64,
        behind_secs: f64,
    ) -> Self {
        let shared = Arc::new(SharedState {
            caches: SharedFrameCache::new(),
            target: LiveTarget::still(0),
            cache_budget_bytes: AtomicUsize::new(cache_budget_bytes),
            caught_up: AtomicBool::new(false),
            lookahead_secs: AtomicU64::new(lookahead_secs.max(0.0).to_bits()),
            behind_secs: AtomicU64::new(behind_secs.max(0.0).to_bits()),
        });
        let (tx, rx) = mpsc::channel();
        let thread_shared = shared.clone();
        let handle = std::thread::spawn(move || {
            worker_loop(rx, &thread_shared, project, timeline_id, proxy);
        });
        Self {
            shared,
            tx,
            handle: Some(handle),
            served_keyframe: false,
        }
    }

    /// New playhead. If it changes it immediately marks "not buffered" (or the UI
    /// would stop asking for repaints on a stale state) and wakes the
    /// worker: with narrow windows 50 ms of waiting limit the playback.
    pub fn set_target(&self, frame: FrameIdx) {
        let previous = self.shared.target.frame.swap(frame, Ordering::Relaxed);
        if previous != frame {
            self.shared.caught_up.store(false, Ordering::Relaxed);
            let _ = self.tx.send(Command::Wake);
        }
    }

    /// The user moved the playhead to `frame`: unlike playback, whatever
    /// the worker is decoding is abandoned right away, however short the jump.
    pub fn jump_to(&self, frame: FrameIdx) {
        self.shared.target.jumps.fetch_add(1, Ordering::Relaxed);
        self.shared.target.frame.store(frame, Ordering::Relaxed);
        self.shared.caught_up.store(false, Ordering::Relaxed);
        let _ = self.tx.send(Command::Wake);
    }

    /// Buffers one timeline frame every `stride` (the playback speed: fast
    /// playback shows no others), 1 for all of them.
    pub fn set_frame_stride(&self, stride: FrameIdx) {
        let stride = stride.max(1);
        if self
            .shared
            .target
            .frame_stride
            .swap(stride, Ordering::Relaxed)
            != stride
        {
            self.shared.caught_up.store(false, Ordering::Relaxed);
            let _ = self.tx.send(Command::Wake);
        }
    }

    /// The speed the timeline is playing at, 0 when it is not.
    pub fn set_playback_speed(&self, speed: f64) {
        if store_secs(&self.shared.target.speed, speed) {
            self.shared.caught_up.store(false, Ordering::Relaxed);
            let _ = self.tx.send(Command::Wake);
        }
    }

    /// Whether a media the decoder cannot keep up with, in fast playback or
    /// in a scrub, shows its keyframes meanwhile.
    pub fn set_keyframes_when_behind(&self, on: bool) {
        self.shared
            .target
            .keyframes_when_behind
            .store(on, Ordering::Relaxed);
    }

    /// Whether a frame handed out since the last call was a keyframe standing
    /// in for the one asked.
    pub fn take_served_keyframe(&mut self) -> bool {
        std::mem::take(&mut self.served_keyframe)
    }

    pub fn set_cache_budget_bytes(&self, bytes: usize) {
        self.shared
            .cache_budget_bytes
            .store(bytes, Ordering::Relaxed);
    }

    /// Seconds buffered ahead (Settings > Playback), never below
    /// `MIN_MARGIN_FRAMES`.
    pub fn set_lookahead_secs(&self, secs: f64) {
        // Called on every frame: resetting `caught_up` unchanged kept the UI
        // repainting forever.
        if store_secs(&self.shared.lookahead_secs, secs) {
            self.shared.caught_up.store(false, Ordering::Relaxed);
        }
    }

    /// How many seconds of timeline to buffer *behind* the playhead too
    /// (Settings > Playback) — see the docs of `DEFAULT_BEHIND_SECS`.
    pub fn set_behind_secs(&self, secs: f64) {
        if store_secs(&self.shared.behind_secs, secs) {
            self.shared.caught_up.store(false, Ordering::Relaxed);
        }
    }

    /// The worker works on a copy of the project: it must be updated on every
    /// change of the history.
    pub fn update_project(&self, project: &Project, timeline_id: TimelineId) {
        self.shared.caught_up.store(false, Ordering::Relaxed);
        let _ = self.tx.send(Command::UpdateProject(
            Box::new(project.clone()),
            timeline_id,
        ));
    }

    /// "Use proxy" toggle (plans/REFACTOR_PIPELINE.md proxy): the worker
    /// empties the shared cache and reopens from scratch every decoder on the
    /// right path for the new state — see the docs of `Command::SetProxy`.
    pub fn reopen_decoders(&self) {
        let _ = self.tx.send(Command::ReopenDecoders);
    }

    pub fn set_proxy(&self, proxy: Option<ProxyQuality>) {
        self.shared.caught_up.store(false, Ordering::Relaxed);
        let _ = self.tx.send(Command::SetProxy(proxy));
    }

    /// Decodes the frames at most at their media's size divided by
    /// `divisor` (Playback > Preview resolution).
    pub fn set_preview_divisor(&self, divisor: u32) {
        self.shared.caught_up.store(false, Ordering::Relaxed);
        let _ = self.tx.send(Command::SetPreviewDivisor(divisor));
    }

    /// The decoded frame for `(media_id, source_frame)`, if already
    /// cached.
    pub fn get_frame(&self, media_id: MediaId, source_frame: FrameIdx) -> Option<Arc<FrameYuv420>> {
        self.shared.caches.get(media_id, source_frame)
    }

    /// Keyframes stand in for some media: a frame missing may just lie
    /// between two of them, it is not lost.
    pub fn is_standing_in(&self) -> bool {
        self.shared.caches.any_keyframes_only()
    }

    /// `false` while work remains for the current window: the UI keeps
    /// asking for repaints to advance the "buffered" strip.
    pub fn is_caught_up(&self) -> bool {
        self.shared.caught_up.load(Ordering::Relaxed)
    }

    /// Stretches of a media whose frames keyframes stand in for, in source
    /// frames: empty unless it is in keyframe mode.
    pub fn keyframe_ranges_for(&self, media_id: MediaId) -> Vec<(FrameIdx, FrameIdx)> {
        self.shared.caches.keyframe_ranges(media_id)
    }

    /// Cached intervals of a media, in source frames.
    pub fn cached_ranges_for(&self, media_id: MediaId) -> Vec<(FrameIdx, FrameIdx)> {
        self.shared.caches.cached_ranges(media_id)
    }
}

impl Drop for RenderAhead {
    fn drop(&mut self) {
        let _ = self.tx.send(Command::Stop);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

/// Non-blocking and never an error: a media that does not decode is
/// skipped by the worker.
impl crate::frame_provider::FrameProvider for RenderAhead {
    fn frame_for(
        &mut self,
        _project: &Project,
        clip: &vv_core::Clip,
        timeline_frame: FrameIdx,
    ) -> Result<Option<Arc<FrameYuv420>>, crate::export::ExportError> {
        let Some((media_id, source_frame)) =
            crate::frame_provider::media_source_frame(clip, timeline_frame)
        else {
            return Ok(None);
        };
        let near = self.shared.caches.get_near(
            media_id,
            source_frame,
            clip.source_in(),
            clip.source_out() - 1,
        );
        self.served_keyframe |= near
            .as_ref()
            .is_some_and(|(_, how)| *how == vv_media::Served::Keyframe);
        Ok(near.map(|(frame, _)| frame))
    }
}

/// Decoder open for a media, with the next frame it will produce: it says whether
/// it is better to decode in sequence or to seek. One per media, so a
/// cut between two media does not make them reopen.
struct OpenDecoder {
    decoder: Decoder,
    /// Open path: if the proxy becomes available or the toggle changes, the
    /// decoder must be reopened on the new one.
    resolved_path: std::path::PathBuf,
    /// `true` when `resolved_path` is a proxy (plans/REFACTOR_PIPELINE.md
    /// proxy) — see `PROXY_SEEK_THRESHOLD_FRAMES` on why it bypasses the
    /// adaptive GOP estimate instead of merely initializing it.
    is_all_intra: bool,
    next_frame: FrameIdx,
    /// Last seek landing and GOP estimated from the distance between landings.
    last_keyframe_landed: Option<FrameIdx>,
    estimated_gop: Option<FrameIdx>,
    /// The longest stretch a seek had to cross from its landing to its
    /// target: no keyframe in it, so the GOP is at least this long.
    min_gop: FrameIdx,
}

impl OpenDecoder {
    fn fresh(decoder: Decoder, resolved_path: std::path::PathBuf, is_all_intra: bool) -> Self {
        Self {
            decoder,
            resolved_path,
            is_all_intra,
            next_frame: 0,
            last_keyframe_landed: None,
            estimated_gop: None,
            min_gop: 0,
        }
    }

    /// The GOP as far as seeks have shown it, `None` before any.
    fn known_gop(&self) -> Option<FrameIdx> {
        match (self.estimated_gop, self.min_gop) {
            (None, 0) => None,
            (gop, min) => Some(gop.unwrap_or(0).max(min)),
        }
    }

    /// About one observed GOP, a fallback while there is none; fixed on the proxies.
    fn seek_threshold_frames(&self) -> FrameIdx {
        if self.is_all_intra {
            return PROXY_SEEK_THRESHOLD_FRAMES;
        }
        // Below `min_gop` an interrupted transit would seek again, landing on
        // the same keyframe and starting over: at 2x on a long GOP, forever.
        self.estimated_gop
            .unwrap_or(DEFAULT_SEEK_THRESHOLD_FRAMES)
            .max(self.min_gop)
    }

    /// Updates the GOP estimate with a new landing of a seek to `target`. It
    /// keeps the minimum: a jump of several GOPs would overestimate it.
    fn record_keyframe_landing(&mut self, idx: FrameIdx, target: FrameIdx) {
        self.min_gop = self
            .min_gop
            .max((target - idx).min(MAX_SEEK_THRESHOLD_FRAMES));
        if let Some(prev) = self.last_keyframe_landed
            && idx > prev
        {
            let observed = (idx - prev).min(MAX_SEEK_THRESHOLD_FRAMES);
            self.estimated_gop = Some(match self.estimated_gop {
                Some(g) => g.min(observed),
                None => observed,
            });
        }
        if debug_enabled() {
            eprintln!(
                "[render_ahead] LANDING idx={idx} target={target} last_keyframe_landed_before={:?} estimated_gop_after={:?} min_gop={}",
                self.last_keyframe_landed, self.estimated_gop, self.min_gop
            );
        }
        self.last_keyframe_landed = Some(idx);
    }
}

fn worker_loop(
    rx: mpsc::Receiver<Command>,
    shared: &SharedState,
    mut project: Project,
    mut timeline_id: TimelineId,
    mut proxy: Option<ProxyQuality>,
) {
    let mut open: HashMap<MediaId, OpenDecoder> = HashMap::new();
    // Separate decoders for the window behind the playhead: see `walk_and_fill`.
    let mut open_behind: HashMap<MediaId, OpenDecoder> = HashMap::new();
    // Separate decoders for keyframes: seeking the forward ones back to a
    // keyframe would throw away their way towards the frame wanted.
    let mut open_keys: HashMap<MediaId, OpenDecoder> = HashMap::new();
    // `from` of the previous cycle (only that, never a historical min/max):
    // it says whether the playhead went backwards.
    let mut last_from_frame: Option<FrameIdx> = None;
    // Previous cycle interrupted by a playhead jump: it restarts
    // immediately instead of waiting for `POLL_INTERVAL`.
    let mut retry_immediately = false;
    let mut preview_divisor = 1;
    let pacing = RefCell::new(Pacing::default());
    loop {
        let first = if retry_immediately {
            match rx.try_recv() {
                Ok(cmd) => Some(cmd),
                Err(TryRecvError::Empty) => None,
                Err(TryRecvError::Disconnected) => return,
            }
        } else {
            match rx.recv_timeout(POLL_INTERVAL) {
                Ok(cmd) => Some(cmd),
                Err(RecvTimeoutError::Timeout) => None,
                Err(RecvTimeoutError::Disconnected) => return,
            }
        };
        // The commands already queued too: only the final state counts.
        for cmd in first
            .into_iter()
            .chain(std::iter::from_fn(|| rx.try_recv().ok()))
        {
            match cmd {
                Command::Stop => return,
                Command::UpdateProject(p, id) => {
                    forget_changed_media(
                        &project,
                        &p,
                        &shared.caches,
                        &mut [&mut open, &mut open_behind, &mut open_keys],
                    );
                    project = *p;
                    timeline_id = id;
                }
                Command::SetProxy(v) => {
                    proxy = v;
                    shared.caches.clear();
                    open.clear();
                    open_behind.clear();
                    open_keys.clear();
                }
                Command::SetPreviewDivisor(divisor) => {
                    if divisor != preview_divisor {
                        preview_divisor = divisor;
                        shared.caches.clear();
                        open.clear();
                        open_behind.clear();
                        open_keys.clear();
                    }
                }
                Command::ReopenDecoders => {
                    open.clear();
                    open_behind.clear();
                    open_keys.clear();
                }
                Command::Wake => {}
            }
        }

        let from = shared.target.frame();
        let went_backward = last_from_frame.is_some_and(|last| from < last);
        last_from_frame = Some(from);
        let outcome = walk_and_fill_paced(
            &project,
            timeline_id,
            &shared.caches,
            &mut open,
            &mut open_behind,
            &mut open_keys,
            from,
            shared.cache_budget_bytes.load(Ordering::Relaxed),
            went_backward,
            proxy,
            preview_divisor,
            load_secs(&shared.lookahead_secs),
            load_secs(&shared.behind_secs),
            &shared.target,
            &pacing,
        );
        retry_immediately = outcome.interrupted;
        shared.caught_up.store(outcome.caught_up, Ordering::Relaxed);
    }
}

/// Decoders survive an edit: reopening costs a seek back to a keyframe. A
/// media gone from `new` or naming another file there loses its decoders and
/// its frames — `MediaId`s restart from 1 in every project.
fn forget_changed_media(
    old: &Project,
    new: &Project,
    caches: &SharedFrameCache,
    decoders: &mut [&mut HashMap<MediaId, OpenDecoder>],
) {
    for (media_id, item) in &old.media_pool {
        let same_file = new
            .media_pool
            .get(media_id)
            .is_some_and(|n| n.path == item.path && n.content_hash == item.content_hash);
        if !same_file {
            caches.remove_media(media_id);
            for map in decoders.iter_mut() {
                map.remove(&media_id);
            }
        }
    }
}

/// What `position_decoder` did to satisfy the request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Positioned {
    /// Decoder already open and already well positioned: no seek, the front
    /// of the buffer advances in sequence at no extra cost.
    Reused,
    /// Decoder already open, repositioned with a seek on the existing
    /// decoder (`seek_to_time`, not a reopen).
    Seeked,
    /// No decoder open for this media: opened from scratch.
    Opened,
    /// Opening the media failed: the caller skips the segment.
    Failed,
}

/// Segments of `[from_frame, end_frame)`, one per Media clip of every video
/// track (including those below: they show in the letterbox bars),
/// from the nearest to the playhead and, at equal position, from the highest track.
/// Real media only: a compound clip is not decoded, it is walked
/// into (at any nesting depth).
fn collect_media_segments(
    project: &Project,
    timeline: &Timeline,
    from_frame: FrameIdx,
    end_frame: FrameIdx,
) -> Vec<WantedRange> {
    let mut real = clipped_media_segments(project, timeline, from_frame, end_frame, 0);
    real.sort_by_key(|(track, s)| (s.timeline_start, std::cmp::Reverse(*track)));
    strip_track(real)
}

/// Like `collect_media_segments` for the window behind: from the nearest
/// to the playhead, i.e. from the farthest ahead on the timeline.
fn collect_media_segments_behind(
    project: &Project,
    timeline: &Timeline,
    from_frame: FrameIdx,
    start_frame: FrameIdx,
) -> Vec<WantedRange> {
    let mut real = clipped_media_segments(project, timeline, start_frame, from_frame, 0);
    real.sort_by_key(|(track, s)| {
        (
            std::cmp::Reverse(s.timeline_start),
            std::cmp::Reverse(*track),
        )
    });
    strip_track(real)
}

fn strip_track(segments: Vec<(usize, WantedRange)>) -> Vec<WantedRange> {
    segments.into_iter().map(|(_, s)| s).collect()
}

/// The part common to the two functions above: every Media clip of every video
/// track clipped to `[from_frame, end_frame)`, with the index of its
/// track so they can be sorted afterwards. Not sorted. Recursive: a clip
/// referencing a compound clip (`MediaItem::compound`) does not generate a
/// segment to decode, but — on the same range, translated into frames
/// of its nested timeline by `Clip::picture_frame_at` — the segments
/// collected inside that timeline, at any depth (within
/// `MAX_COMPOUND_DEPTH`).
fn clipped_media_segments(
    project: &Project,
    timeline: &Timeline,
    from_frame: FrameIdx,
    end_frame: FrameIdx,
    depth: u32,
) -> Vec<(usize, WantedRange)> {
    let mut real = Vec::new();
    if depth >= vv_core::MAX_COMPOUND_DEPTH {
        return real;
    }
    for (track_index, clip) in timeline.visible_video_clips() {
        let ClipSource::Media(media_id) = &clip.source else {
            continue;
        };
        let segment_start = clip.timeline_start.max(from_frame);
        let segment_end = clip.timeline_end().min(end_frame);
        if segment_end <= segment_start {
            continue;
        }
        // clip→source-frame mapping shared with the export
        // (`vv_core::Clip::picture_frame_at`, see the docs there for the
        // reason — plans/REFACTOR_PIPELINE.md B1).
        let source_start = clip.picture_frame_at(segment_start);
        let source_end = clip.picture_frame_at(segment_end - 1);
        let segment = WantedRange {
            media_id: *media_id,
            source_start,
            source_end,
            timeline_start: segment_start,
            rate: clip.rate(),
        };
        push_or_recurse(project, track_index, segment, depth, &mut real);
    }
    real
}

/// Adds `segment` to the media to decode, or — if `segment.media_id` is
/// a compound clip, which is never decoded — recurses into its
/// nested timeline on the same range (in source frames, i.e. already in the
/// space of that timeline).
fn push_or_recurse(
    project: &Project,
    track_index: usize,
    segment: WantedRange,
    depth: u32,
    real: &mut Vec<(usize, WantedRange)>,
) {
    match project
        .media_pool
        .get(segment.media_id)
        .and_then(|m| m.compound)
    {
        Some(nested_id) => {
            if let Some(nested) = project.timelines.get(nested_id) {
                real.extend(
                    clipped_media_segments(
                        project,
                        nested,
                        segment.source_start,
                        segment.source_end + 1,
                        depth + 1,
                    )
                    .into_iter()
                    .map(|(track, s)| (track, s.seen_through(&segment))),
                );
            }
        }
        None => real.push((track_index, segment)),
    }
}

/// Segments "lent" to the crossing transitions active in
/// `[from_frame, end_frame)`: each side plays past its clip's declared edge
/// (the footage the trim discarded, held on the media's real edges by
/// `held_timeline_frame`), which no `clipped_media_segments` segment covers —
/// without these, `reconcile` evicts it as soon as the playhead passes the
/// cut. Real media only: a compound clip is walked into.
fn crossing_borrowed_segments(
    project: &Project,
    timeline: &Timeline,
    from_frame: FrameIdx,
    end_frame: FrameIdx,
) -> Vec<WantedRange> {
    let mut real = Vec::new();
    for (_, track) in timeline.visible_video_tracks() {
        for crossing in &track.crossings {
            let (Some(left), Some(right)) = (
                track.clip(crossing.left_clip),
                track.clip(crossing.right_clip),
            ) else {
                continue;
            };
            let window = crossing.window(left, right);
            if window.end <= from_frame || window.start >= end_frame {
                continue;
            }
            // `left` lends the stretch after its own declared edge,
            // `right` the one before: the other half of each is already covered
            // by its own normal segment.
            if window.end > left.timeline_end() {
                push_borrowed_segment(
                    project,
                    left,
                    left.timeline_end(),
                    window.end - 1,
                    &mut real,
                );
            }
            if window.start < right.timeline_start {
                push_borrowed_segment(
                    project,
                    right,
                    window.start,
                    right.timeline_start - 1,
                    &mut real,
                );
            }
        }
    }
    real
}

/// A source segment for the stretch of `clip` lent to a crossing,
/// `[from_timeline, to_timeline]` (inclusive) in timeline frames,
/// clamped to the same edges of `media.meta.duration_frames` that
/// `held_timeline_frame` would use — otherwise it would ask to buffer a
/// source frame the decoder will never produce.
fn push_borrowed_segment(
    project: &Project,
    clip: &vv_core::Clip,
    from_timeline: FrameIdx,
    to_timeline: FrameIdx,
    real: &mut Vec<WantedRange>,
) {
    let ClipSource::Media(media_id) = &clip.source else {
        return;
    };
    let Some(item) = project.media_pool.get(*media_id) else {
        return;
    };
    let last = (item.meta.duration_frames - 1).max(0);
    let a = clip.picture_frame_at(from_timeline).clamp(0, last);
    let b = clip.picture_frame_at(to_timeline).clamp(0, last);
    let segment = WantedRange {
        media_id: *media_id,
        source_start: a.min(b),
        source_end: a.max(b),
        timeline_start: from_timeline,
        rate: clip.rate(),
    };
    match item.compound {
        Some(nested_id) => {
            if let Some(nested) = project.timelines.get(nested_id) {
                real.extend(
                    strip_track(clipped_media_segments(
                        project,
                        nested,
                        segment.source_start,
                        segment.source_end + 1,
                        1,
                    ))
                    .into_iter()
                    .map(|s| s.seen_through(&segment)),
                );
            }
        }
        None => real.push(segment),
    }
}

/// Splits the forward segments into chunks of `FORWARD_CHUNK_FRAMES` and orders
/// them by timeline position, so clips playing together (two video tracks) are
/// filled interleaved. Whole segment at a time, the playhead interrupts the
/// cycle before the lower track is ever reached and the compositing shows it
/// black.
fn chunk_forward_segments_near_to_far(segments: &[WantedRange]) -> Vec<WantedRange> {
    let mut chunks: Vec<WantedRange> = Vec::new();
    for segment in segments {
        let mut chunk_start = segment.source_start;
        loop {
            let chunk_end = (chunk_start + FORWARD_CHUNK_FRAMES - 1).min(segment.source_end);
            chunks.push(segment.sub_range(chunk_start, chunk_end));
            if chunk_end == segment.source_end {
                break;
            }
            chunk_start = chunk_end + 1;
        }
    }
    // Stable: at equal position the order of the segments (topmost track first) holds.
    chunks.sort_by_key(|c| c.timeline_start);
    chunks
}

/// Splits the segments behind the playhead into chunks of `BEHIND_CHUNK_FRAMES`,
/// from the edge near the playhead (`source_end`) towards the far one.
fn chunk_behind_segments_near_to_far(segments: &[WantedRange]) -> Vec<WantedRange> {
    let mut chunks = Vec::new();
    for segment in segments {
        let mut chunk_end = segment.source_end;
        loop {
            let chunk_start = (chunk_end - BEHIND_CHUNK_FRAMES + 1).max(segment.source_start);
            chunks.push(segment.sub_range(chunk_start, chunk_end));
            if chunk_start == segment.source_start {
                break;
            }
            chunk_end = chunk_start - 1;
        }
    }
    chunks
}

/// Removes the chunks already cached. In the window behind, the decoder stays
/// parked on the farthest chunk of the previous cycle: without the filter every chunk
/// would look like it needs reseeking on every cycle, even with the playhead still.
fn without_already_cached_chunks(
    caches: &SharedFrameCache,
    chunks: Vec<WantedRange>,
) -> Vec<WantedRange> {
    chunks
        .into_iter()
        .filter(|chunk| {
            !caches.covers_segment(chunk.media_id, chunk.source_start, chunk.source_end)
        })
        .collect()
}

/// Brings `open[media_id]` where it can cover `segment_start` by decoding
/// forward. A decoder already past the segment is the normal state and is reused.
/// It seeks (on the open decoder, never reopening: reparsing the container can
/// cost seconds) only if the segment is too far ahead, if the playhead went
/// back and the decoder passed it, or if the cache has a hole
/// where the decoder thinks it already went through. `went_backward` is decided
/// once per cycle: per media the order of the segments would dirty it.
fn position_decoder(
    caches: &SharedFrameCache,
    open: &mut HashMap<MediaId, OpenDecoder>,
    media_id: MediaId,
    path: &Path,
    segment_start: FrameIdx,
    went_backward: bool,
    is_all_intra: bool,
    is_image: bool,
    hw_priority: vv_media::HwPriority,
    max_size: Option<(u32, u32)>,
) -> Positioned {
    if open.get(&media_id).is_some_and(|o| o.resolved_path != path) {
        open.remove(&media_id);
    }
    if let Some(o) = open.get_mut(&media_id) {
        let needs_seek = segment_start > o.next_frame + o.seek_threshold_frames()
            || (went_backward && segment_start < o.next_frame)
            // An eviction may have removed the tail the decoder thinks it already
            // produced: it is checked against the cache.
            || (o.next_frame > segment_start
                && !caches.covers(media_id, segment_start, o.next_frame - 1));
        if !needs_seek {
            return Positioned::Reused;
        }
        let secs = segment_start as f64 / o.decoder.fps().as_f64().max(1e-9);
        let debug_start = debug_enabled().then(std::time::Instant::now);
        let _ = o.decoder.seek_to_time(secs);
        if let Some(t) = debug_start {
            eprintln!(
                "[render_ahead] seek (decoder reused) media={media_id:?} target={segment_start} elapsed={:?}",
                t.elapsed()
            );
        }
        // Placeholder: the next frame will say where the seek really landed.
        o.next_frame = 0;
        return Positioned::Seeked;
    }
    // No decoder open for this media: here the real open is
    // unavoidable (first time, or a media different from the one open so far).
    let debug_start = debug_enabled().then(std::time::Instant::now);
    let Some(mut decoder) = open_decoder(path, is_image, hw_priority, max_size) else {
        return Positioned::Failed;
    };
    let secs = segment_start as f64 / decoder.fps().as_f64().max(1e-9);
    let _ = decoder.seek_to_time(secs);
    if let Some(t) = debug_start {
        eprintln!(
            "[render_ahead] OPEN (new decoder) media={media_id:?} path={} hw={} target={segment_start} elapsed={:?}",
            path.display(),
            decoder.is_hw(),
            t.elapsed()
        );
    }
    open.insert(
        media_id,
        OpenDecoder::fresh(decoder, path.to_path_buf(), is_all_intra),
    );
    Positioned::Opened
}

fn open_decoder(
    path: &Path,
    is_image: bool,
    hw_priority: vv_media::HwPriority,
    max_size: Option<(u32, u32)>,
) -> Option<Decoder> {
    // An image must be opened with `open_image`, or it would hit EOF after the
    // first frame.
    let opened = if is_image {
        Decoder::open_image(path)
    } else {
        Decoder::open_with(path, &crate::hw_decode::devices(), hw_priority)
    };
    let mut decoder = opened.ok()?;
    decoder.set_max_size(max_size);
    Some(decoder)
}

/// The first frame from `landing` on that the transit towards `segment` must
/// hand out: the start of `segment`, or earlier where another wanted range of
/// the same media (the window behind, another clip of the same file) overlaps
/// the transit.
fn first_wanted_frame(
    window: &[WantedRange],
    segment: &WantedRange,
    landing: FrameIdx,
) -> FrameIdx {
    window
        .iter()
        .filter(|r| r.media_id == segment.media_id && r.source_end >= landing)
        .map(|r| r.source_start.max(landing))
        .fold(segment.source_start, FrameIdx::min)
}

/// Source frames of a media spanned by `timeline_stride` frames of the
/// timeline, given the clip's `rate` (timeline frames per source frame); never
/// below 1, so a slowed down clip keeps every frame. 1 without a stride, even
/// on a sped up clip: its multiples need not be the frames it shows, and paused
/// a neighbour would stand in for the exact frame.
fn source_stride(timeline_stride: FrameIdx, rate: vv_core::Rational) -> FrameIdx {
    if timeline_stride <= 1 {
        return 1;
    }
    if rate.num <= 0 || rate.den <= 0 {
        return timeline_stride;
    }
    (timeline_stride * rate.den as FrameIdx / rate.num as FrameIdx).max(1)
}

/// Outcome of one `walk_and_fill` cycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct WalkOutcome {
    /// The playhead moved enough to make the remaining work obsolete:
    /// it restarts immediately with the new target.
    interrupted: bool,
    /// The whole window is cached: the UI can stop asking for repaints.
    caught_up: bool,
}

impl WalkOutcome {
    const SETTLED: Self = Self {
        interrupted: false,
        caught_up: true,
    };
    /// Stopped by the budget or by transit: there is still work, but not right away.
    const UNFINISHED: Self = Self {
        interrupted: false,
        caught_up: false,
    };
}

/// What the worker learns, cycle after cycle, about keeping up.
#[derive(Default)]
struct Pacing {
    /// Source frames per second each media advanced decoding in sequence, by
    /// stride: skipping the frames in between the decoder goes faster.
    throughput: HashMap<(MediaId, FrameIdx), f64>,
    /// A media fell behind at this speed (bits of the `f64`): the next time
    /// it plays at it, keyframes only from the start.
    fell_behind: HashSet<(MediaId, u64)>,
    /// Media in keyframe mode for the fast playback in progress, with its speed.
    keyframes_only: HashMap<MediaId, u64>,
    /// Since when the frame at the playhead is missing, in fast playback.
    missing_since: HashMap<MediaId, Instant>,
    /// How much of the fast playback in progress had the frame missing.
    presence: HashMap<MediaId, Presence>,
    /// Per keyframe a seek landed on, the farthest target that landed there:
    /// no keyframe in between.
    keyframe_spans: HashMap<MediaId, std::collections::BTreeMap<FrameIdx, FrameIdx>>,
    /// Media showing a keyframe while paused, until their frames are there.
    scrub_keyframes: HashSet<MediaId>,
    /// `LiveTarget::jumps` last seen, and when it last changed.
    last_jumps: u64,
    last_jump_at: Option<Instant>,
}

/// Spans remembered per media: past it the oldest go.
const MAX_KEYFRAME_SPANS: usize = 256;

/// The frame at the playhead in a fast playback, checked once per cycle.
struct Presence {
    since: Instant,
    checked: Instant,
    was_missing: bool,
    missing: Duration,
}

impl Presence {
    fn new(now: Instant) -> Self {
        Self {
            since: now,
            checked: now,
            was_missing: true,
            missing: Duration::ZERO,
        }
    }

    /// Share of the time so far with the frame missing, `missing` the state now.
    fn missing_share(&mut self, now: Instant, missing: bool) -> f64 {
        if self.was_missing {
            self.missing += now.duration_since(self.checked);
        }
        self.checked = now;
        self.was_missing = missing;
        let elapsed = now.duration_since(self.since).as_secs_f64();
        if elapsed <= 0.0 {
            return if missing { 1.0 } else { 0.0 };
        }
        self.missing.as_secs_f64() / elapsed
    }
}

impl Pacing {
    /// The keyframe at or before `frame` with no other one up to it, as far as
    /// seeks proved, and how far that proof goes.
    fn keyframe_span_at(&self, media_id: MediaId, frame: FrameIdx) -> Option<(FrameIdx, FrameIdx)> {
        let (&keyframe, &until) = self
            .keyframe_spans
            .get(&media_id)?
            .range(..=frame)
            .next_back()?;
        (frame <= until).then_some((keyframe, until))
    }

    /// The first keyframe a seek landed on past `frame`.
    fn keyframe_after(&self, media_id: MediaId, frame: FrameIdx) -> Option<FrameIdx> {
        let spans = self.keyframe_spans.get(&media_id)?;
        spans
            .range(frame + 1..)
            .next()
            .map(|(&keyframe, _)| keyframe)
    }

    fn record_keyframe_span(&mut self, media_id: MediaId, keyframe: FrameIdx, target: FrameIdx) {
        let spans = self.keyframe_spans.entry(media_id).or_default();
        let until = spans.entry(keyframe).or_insert(target);
        *until = (*until).max(target);
        if spans.len() > MAX_KEYFRAME_SPANS {
            spans.pop_first();
        }
    }

    /// Whether the playhead was moved by hand within `SCRUBBING_FOR`.
    fn scrubbing(&mut self, jumps: u64) -> bool {
        let now = Instant::now();
        if jumps != self.last_jumps {
            self.last_jumps = jumps;
            self.last_jump_at = Some(now);
        }
        self.last_jump_at
            .is_some_and(|at| now.duration_since(at) < SCRUBBING_FOR)
    }

    fn record_throughput(&mut self, media_id: MediaId, stride: FrameIdx, fps: f64) {
        let rate = self.throughput.entry((media_id, stride)).or_insert(fps);
        *rate = 0.7 * *rate + 0.3 * fps;
    }
}

/// `walk_and_fill_paced` with nothing learned yet.
#[cfg(test)]
fn walk_and_fill(
    project: &Project,
    timeline_id: TimelineId,
    caches: &SharedFrameCache,
    open: &mut HashMap<MediaId, OpenDecoder>,
    open_behind: &mut HashMap<MediaId, OpenDecoder>,
    from_frame: FrameIdx,
    cache_budget_bytes: usize,
    went_backward: bool,
    proxy: Option<ProxyQuality>,
    preview_divisor: u32,
    lookahead_secs: f64,
    behind_secs: f64,
    target: &LiveTarget,
) -> WalkOutcome {
    walk_and_fill_paced(
        project,
        timeline_id,
        caches,
        open,
        open_behind,
        &mut HashMap::new(),
        from_frame,
        cache_budget_bytes,
        went_backward,
        proxy,
        preview_divisor,
        lookahead_secs,
        behind_secs,
        target,
        &RefCell::new(Pacing::default()),
    )
}

fn walk_and_fill_paced(
    project: &Project,
    timeline_id: TimelineId,
    caches: &SharedFrameCache,
    open: &mut HashMap<MediaId, OpenDecoder>,
    open_behind: &mut HashMap<MediaId, OpenDecoder>,
    open_keys: &mut HashMap<MediaId, OpenDecoder>,
    from_frame: FrameIdx,
    cache_budget_bytes: usize,
    went_backward: bool,
    proxy: Option<ProxyQuality>,
    preview_divisor: u32,
    lookahead_secs: f64,
    behind_secs: f64,
    target: &LiveTarget,
    pacing: &RefCell<Pacing>,
) -> WalkOutcome {
    let jumps = target.jumps();
    let Some(timeline) = project.timelines.get(timeline_id) else {
        return WalkOutcome::SETTLED;
    };
    let fps = timeline.fps.as_f64().max(1e-9);
    let lookahead_frames = ((lookahead_secs * fps).round() as FrameIdx).max(MIN_MARGIN_FRAMES);
    let behind_frames = ((behind_secs * fps).round() as FrameIdx).max(MIN_MARGIN_FRAMES);
    let end_frame = from_frame + lookahead_frames;
    let start_frame = (from_frame - behind_frames).max(0);

    let mut forward_segments = collect_media_segments(project, timeline, from_frame, end_frame);
    let mut behind_segments =
        collect_media_segments_behind(project, timeline, from_frame, start_frame);
    forward_segments.extend(crossing_borrowed_segments(
        project, timeline, from_frame, end_frame,
    ));
    behind_segments.extend(crossing_borrowed_segments(
        project,
        timeline,
        start_frame,
        from_frame,
    ));
    if forward_segments.is_empty() && behind_segments.is_empty() {
        return WalkOutcome::SETTLED;
    }
    let forward_media: HashSet<MediaId> = forward_segments.iter().map(|s| s.media_id).collect();
    let behind_media: HashSet<MediaId> = behind_segments.iter().map(|s| s.media_id).collect();
    open.retain(|id, _| forward_media.contains(id));
    open_behind.retain(|id, _| behind_media.contains(id));
    open_keys.retain(|id, _| forward_media.contains(id));

    let mut window: Vec<WantedRange> = forward_segments
        .iter()
        .chain(&behind_segments)
        .copied()
        .collect();
    let mut strides: HashMap<MediaId, FrameIdx> = HashMap::new();
    for range in &window {
        let stride = source_stride(target.frame_stride(), range.rate);
        strides
            .entry(range.media_id)
            .and_modify(|s| *s = (*s).min(stride))
            .or_insert(stride);
    }
    for (media_id, stride) in strides {
        caches.set_stride(media_id, stride);
    }
    let speed = target.speed();
    let paused = speed <= 0.0;
    let gops: HashMap<MediaId, FrameIdx> = open
        .iter()
        .chain(open_keys.iter())
        .filter_map(|(id, o)| Some((*id, o.known_gop()?)))
        .collect();
    let keyframe_media = keyframes_only_media(
        project,
        fps,
        &forward_segments,
        caches,
        proxy,
        &gops,
        from_frame,
        speed,
        target.keyframes_when_behind(),
        &mut pacing.borrow_mut(),
    );
    let mut standing_in_media = keyframe_media.clone();
    // Scrubbing over frames not cached: the keyframe before each first, then
    // the frame itself.
    let scrub_segments: Vec<WantedRange> =
        if paused && target.keyframes_when_behind() && pacing.borrow_mut().scrubbing(jumps) {
            forward_segments
                .iter()
                .filter(|s| s.timeline_start == from_frame)
                .filter(|s| {
                    project
                        .media_pool
                        .get(s.media_id)
                        .is_some_and(|item| keyframes_help(item, proxy))
                })
                // A frame a GOP or less before stands in already: the cycle goes to
                // decoding the frame itself instead.
                .filter(|s| {
                    let gop = open
                        .get(&s.media_id)
                        .map_or(DEFAULT_SEEK_THRESHOLD_FRAMES, |o| o.seek_threshold_frames());
                    !(s.source_start - gop..=s.source_start).any(|i| caches.contains(s.media_id, i))
                })
                // A decoder about to reach the frame keeps going: seeking it back
                // to the keyframe would throw that way away.
                .filter(|s| {
                    !open.get(&s.media_id).is_some_and(|o| {
                        let ahead = s.source_start - o.next_frame;
                        let throughput = pacing
                            .borrow()
                            .throughput
                            .get(&(s.media_id, caches.stride(s.media_id)))
                            .copied()
                            .unwrap_or(DEFAULT_THROUGHPUT_FPS);
                        o.next_frame > 0
                            && ahead >= 0
                            && ahead as f64 / throughput <= KEYFRAME_FIRST_AFTER_SECS
                    })
                })
                .map(|s| s.sub_range(s.source_start, s.source_start))
                .collect()
        } else {
            Vec::new()
        };
    {
        let mut pacing = pacing.borrow_mut();
        pacing
            .scrub_keyframes
            .extend(scrub_segments.iter().map(|s| s.media_id));
        standing_in_media.extend(pacing.scrub_keyframes.iter().copied());
        window.extend(standing_in_ranges(
            &forward_segments,
            &standing_in_media,
            &pacing,
        ));
        if !paused || !target.keyframes_when_behind() {
            pacing.scrub_keyframes.clear();
        }
        // Off for the media out of the window too: a flag left on would keep
        // the viewer waiting for frames that never come.
        caches.set_keyframes_only_media(
            forward_media
                .iter()
                .chain(&behind_media)
                .filter(|m| keyframe_media.contains(m) || pacing.scrub_keyframes.contains(m))
                .copied()
                .collect(),
        );
    }
    // Discards what is outside both windows and, past the budget, the
    // farthest from the playhead.
    caches.reconcile(from_frame, &window, cache_budget_bytes);
    if debug_enabled() {
        for id in forward_media
            .iter()
            .chain(behind_media.iter())
            .collect::<HashSet<_>>()
        {
            eprintln!(
                "[render_ahead] AFTER-RECONCILE media={id:?} cached_ranges={:?}",
                caches.cached_ranges(*id)
            );
        }
    }

    let ctx = FillContext {
        project,
        caches,
        went_backward,
        cache_budget_bytes,
        from_frame,
        proxy,
        preview_divisor,
        target,
        jumps,
        window: &forward_segments,
        hw_priority: vv_media::HwPriority::Normal,
        keyframe_first: paused && target.keyframes_when_behind(),
        pacing,
        // In fast playback the decision on keyframes is taken between cycles:
        // a cycle decoding for a GOP would take it a second late.
        decide_by: (speed > 1.0 && target.keyframes_when_behind())
            .then(|| Instant::now() + FELL_BEHIND_AFTER),
    };
    let (keyframe_segments, frame_segments): (Vec<WantedRange>, Vec<WantedRange>) =
        forward_segments
            .iter()
            .partition(|s| keyframe_media.contains(&s.media_id));
    let keys_ctx = FillContext {
        hw_priority: vv_media::HwPriority::Low,
        ..ctx
    };
    if let ControlFlow::Break(outcome) = fill_keyframes(&keyframe_segments, &keys_ctx, open_keys) {
        return outcome;
    }
    if let ControlFlow::Break(outcome) = fill_keyframes(&scrub_segments, &keys_ctx, open_keys) {
        return outcome;
    }
    // The forward window first: behind gets only the budget left over.
    let forward_chunks = chunk_forward_segments_near_to_far(&frame_segments);
    if let ControlFlow::Break(outcome) = fill_segments(&forward_chunks, &ctx, open) {
        return outcome;
    }
    behind_segments.retain(|s| !keyframe_media.contains(&s.media_id));
    // Decoders separate from the forward ones: a single decoder would stay
    // past every segment behind and would skip them all. For the remaining chunks
    // a seek is always needed, so `went_backward` is forced.
    let all_behind_chunks = chunk_behind_segments_near_to_far(&behind_segments);
    let behind_chunks = without_already_cached_chunks(caches, all_behind_chunks.clone());
    if debug_enabled() {
        eprintln!(
            "[render_ahead] BEHIND from_frame={from_frame} behind_secs={behind_secs} behind_frames={behind_frames} total_chunks={} chunks_to_process={} bytes_used={} budget={cache_budget_bytes}",
            all_behind_chunks.len(),
            behind_chunks.len(),
            caches.bytes_used(),
        );
        for c in &all_behind_chunks {
            let processed = behind_chunks
                .iter()
                .any(|b| b.source_start == c.source_start && b.source_end == c.source_end);
            eprintln!(
                "[render_ahead]   chunk media={:?} [{},{}] {}",
                c.media_id,
                c.source_start,
                c.source_end,
                if processed {
                    "TO PROCESS"
                } else {
                    "already cached, skipped"
                }
            );
        }
    }
    let behind_ctx = FillContext {
        went_backward: true,
        window: &window,
        hw_priority: vv_media::HwPriority::Low,
        keyframe_first: false,
        ..ctx
    };
    if let ControlFlow::Break(outcome) = fill_segments(&behind_chunks, &behind_ctx, open_behind) {
        return outcome;
    }
    // Immediately removes the leftover transit frames: the UI reads the state just
    // declared `caught_up` and must not see a strip wider than the truth.
    window.extend(standing_in_ranges(
        &forward_segments,
        &standing_in_media,
        &pacing.borrow(),
    ));
    caches.reconcile(from_frame, &window, cache_budget_bytes);
    if paused {
        // Every frame is there: no keyframe stands in any more.
        pacing.borrow_mut().scrub_keyframes.clear();
    }
    WalkOutcome::SETTLED
}

/// The media playback above 1x shows by keyframes only: those whose decoding
/// demand exceeds the throughput measured at the same stride by
/// `KEYFRAMES_ABOVE_LOAD`, or that already fell behind at this speed, or whose
/// frame at the playhead has been missing longer than keyframes only would
/// leave between two (`gops`, by media) and at least `FELL_BEHIND_AFTER`, with
/// the frame missing for `SUSTAINED_MISSING` of the playback so far, watched
/// for `SUSTAINED_OVER`: else the
/// freeze was a one-off (a cut, the disk) and keyframes for the whole playback
/// would be worse.
/// Decided once per playback and speed: switching back and forth would cost a
/// seek each time.
fn keyframes_only_media(
    project: &Project,
    fps: f64,
    forward_segments: &[WantedRange],
    caches: &SharedFrameCache,
    proxy: Option<ProxyQuality>,
    gops: &HashMap<MediaId, FrameIdx>,
    from_frame: FrameIdx,
    speed: f64,
    enabled: bool,
    pacing: &mut Pacing,
) -> HashSet<MediaId> {
    let mut chosen = HashSet::new();
    if speed <= 1.0 || !enabled {
        pacing.keyframes_only.clear();
        pacing.missing_since.clear();
        pacing.presence.clear();
        return chosen;
    }
    let speed_key = speed.to_bits();
    let now = Instant::now();
    let mut seen = HashSet::new();
    for segment in forward_segments {
        let media_id = segment.media_id;
        if !seen.insert(media_id) {
            continue;
        }
        let Some(item) = project.media_pool.get(media_id) else {
            continue;
        };
        if !keyframes_help(item, proxy) {
            continue;
        }
        if pacing.keyframes_only.get(&media_id) == Some(&speed_key) {
            chosen.insert(media_id);
            continue;
        }
        if pacing.keyframes_only.remove(&media_id).is_some() {
            // Another speed: what was missing at the last one says nothing.
            pacing.presence.remove(&media_id);
        }
        let rate = segment.rate;
        let demand = if rate.num > 0 && rate.den > 0 {
            speed * fps * rate.den as f64 / rate.num as f64
        } else {
            speed * fps
        };
        let predicted = pacing.fell_behind.contains(&(media_id, speed_key))
            || pacing
                .throughput
                .get(&(media_id, caches.stride(media_id)))
                .is_some_and(|&fps| demand > fps * KEYFRAMES_ABOVE_LOAD);
        let under_playhead = segment.timeline_start == from_frame;
        let shown = caches
            .get_near(
                media_id,
                segment.source_start,
                segment.source_start,
                segment.source_end,
            )
            .is_some();
        let between_keyframes = Duration::from_secs_f64(
            gops.get(&media_id).copied().unwrap_or(UNKNOWN_GOP_FRAMES) as f64 / demand.max(1e-9),
        );
        let sustained = under_playhead && {
            let presence = pacing
                .presence
                .entry(media_id)
                .or_insert_with(|| Presence::new(now));
            presence.missing_share(now, !shown) >= SUSTAINED_MISSING
                && now.duration_since(presence.since) >= SUSTAINED_OVER
        };
        let stalled = under_playhead && !shown && sustained && {
            let since = *pacing.missing_since.entry(media_id).or_insert(now);
            now.duration_since(since) >= FELL_BEHIND_AFTER.max(between_keyframes)
        };
        if shown {
            pacing.missing_since.remove(&media_id);
        }
        if stalled {
            pacing.fell_behind.insert((media_id, speed_key));
        }
        if predicted || stalled {
            if debug_enabled() {
                eprintln!(
                    "[render_ahead] KEYFRAMES-ONLY media={media_id:?} speed={speed} remembered={} throughput={:?} demand={demand:.0} stalled={stalled}",
                    pacing.fell_behind.contains(&(media_id, speed_key)),
                    pacing.throughput.get(&(media_id, caches.stride(media_id))),
                );
            }
            pacing.keyframes_only.insert(media_id, speed_key);
            chosen.insert(media_id);
        }
    }
    chosen
}

/// The keyframe standing in at a segment's start, in keyframe mode or in a
/// scrub, often lies before the window:
/// its stretch joins the window, or each reconcile would evict it and each
/// cycle seek it again. Only where a seek proved no keyframe in between.
fn standing_in_ranges(
    forward_segments: &[WantedRange],
    media: &HashSet<MediaId>,
    pacing: &Pacing,
) -> Vec<WantedRange> {
    forward_segments
        .iter()
        .filter(|s| media.contains(&s.media_id))
        .filter_map(|s| {
            let (keyframe, _) = pacing.keyframe_span_at(s.media_id, s.source_start)?;
            (keyframe < s.source_start).then(|| s.sub_range(keyframe, s.source_start - 1))
        })
        .collect()
}

/// Whether showing keyframes alone saves anything: every frame of an image or
/// of an all-intra proxy already is one.
fn keyframes_help(item: &vv_core::MediaItem, proxy: Option<ProxyQuality>) -> bool {
    !item.meta.is_image() && !decode_path(item, proxy).1
}

/// The file to decode for `item`, and whether it is its all-intra proxy: the
/// proxy when on and already made, the source otherwise.
fn decode_path(
    item: &vv_core::MediaItem,
    proxy: Option<ProxyQuality>,
) -> (std::path::PathBuf, bool) {
    match proxy.filter(|&q| vv_media::proxy::proxy_exists(item.content_hash, q)) {
        Some(q) => (vv_media::proxy::proxy_path_for(item.content_hash, q), true),
        None => (item.path.clone(), false),
    }
}

/// While paused, puts the keyframe a seek landed on in the cache when decoding
/// the `transit` frames from it to the one wanted is predicted to take longer
/// than `KEYFRAME_FIRST_AFTER_SECS`: the viewer shows it meanwhile.
fn show_landing_first(
    ctx: &FillContext,
    od: &mut OpenDecoder,
    media_id: MediaId,
    transit: FrameIdx,
) {
    let throughput = ctx
        .pacing
        .borrow()
        .throughput
        .get(&(media_id, ctx.caches.stride(media_id)))
        .copied()
        .unwrap_or(DEFAULT_THROUGHPUT_FPS);
    if transit as f64 / throughput <= KEYFRAME_FIRST_AFTER_SECS {
        return;
    }
    let Ok(Some((keyframe, frame))) = od.decoder.landing_frame() else {
        return;
    };
    if !ctx.caches.contains(media_id, keyframe)
        && ctx.caches.bytes_used() + frame.byte_len() <= ctx.cache_budget_bytes
    {
        ctx.caches.insert(media_id, keyframe, frame);
    }
    ctx.caches.set_keyframes_only(media_id, true);
    ctx.pacing.borrow_mut().scrub_keyframes.insert(media_id);
}

/// Keyframes only: for each segment the keyframe before it and those inside,
/// one seek and one decode each, nothing in between. A seek landing again on the
/// keyframe already there proves none in between: the next one goes a GOP
/// further.
fn fill_keyframes(
    segments: &[WantedRange],
    ctx: &FillContext,
    open: &mut HashMap<MediaId, OpenDecoder>,
) -> ControlFlow<WalkOutcome> {
    for segment in segments {
        let media_id = segment.media_id;
        let Some(item) = ctx.project.media_pool.get(media_id) else {
            continue;
        };
        let (path, is_proxy) = decode_path(item, ctx.proxy);
        let max_size = (ctx.preview_divisor > 1).then(|| {
            (
                (item.meta.width / ctx.preview_divisor).max(1),
                (item.meta.height / ctx.preview_divisor).max(1),
            )
        });
        let mut t = segment.source_start;
        loop {
            let (span, next) = {
                let pacing = ctx.pacing.borrow();
                let span = pacing.keyframe_span_at(media_id, t);
                let next = span.and_then(|(_, until)| pacing.keyframe_after(media_id, until));
                (span, next)
            };
            let seek_to = match span {
                Some((keyframe, until)) if ctx.caches.contains(media_id, keyframe) => {
                    if until >= segment.source_end {
                        break;
                    }
                    let step = open
                        .get(&media_id)
                        .map_or(DEFAULT_SEEK_THRESHOLD_FRAMES, |o| o.seek_threshold_frames());
                    // The next keyframe already found and still there: no
                    // need to seek it again every cycle.
                    if let Some(next) = next
                        && next <= until + step
                        && ctx.caches.contains(media_id, next)
                    {
                        t = next;
                        continue;
                    }
                    (until + step).min(segment.source_end)
                }
                _ => t,
            };
            if open.get(&media_id).is_some_and(|o| o.resolved_path != path) {
                open.remove(&media_id);
            }
            let debug_start = debug_enabled().then(Instant::now);
            let od = match open.entry(media_id) {
                std::collections::hash_map::Entry::Occupied(e) => e.into_mut(),
                std::collections::hash_map::Entry::Vacant(e) => {
                    let Some(decoder) =
                        open_decoder(&path, item.meta.is_image(), ctx.hw_priority, max_size)
                    else {
                        break;
                    };
                    e.insert(OpenDecoder::fresh(decoder, path.clone(), is_proxy))
                }
            };
            let Ok(Some((keyframe, frame))) = od.decoder.keyframe_before(seek_to) else {
                break;
            };
            if let Some(t) = debug_start {
                eprintln!(
                    "[render_ahead] KEYFRAME media={media_id:?} target={seek_to} landed={keyframe} elapsed={:?}",
                    t.elapsed()
                );
            }
            od.record_keyframe_landing(keyframe, seek_to);
            if !ctx.caches.contains(media_id, keyframe) {
                if ctx.caches.bytes_used() + frame.byte_len() > ctx.cache_budget_bytes {
                    return ControlFlow::Break(WalkOutcome::UNFINISHED);
                }
                ctx.caches.insert(media_id, keyframe, frame);
            }
            ctx.pacing
                .borrow_mut()
                .record_keyframe_span(media_id, keyframe, seek_to);
            if ctx.target.jumps() != ctx.jumps {
                return ControlFlow::Break(WalkOutcome {
                    interrupted: true,
                    caught_up: false,
                });
            }
            // Within the span just proven: the next seek goes past it.
            t = seek_to;
        }
    }
    ControlFlow::Continue(())
}

/// Parameters of `fill_segments` common to both windows.
struct FillContext<'a> {
    project: &'a Project,
    caches: &'a SharedFrameCache,
    went_backward: bool,
    cache_budget_bytes: usize,
    from_frame: FrameIdx,
    /// Proxy quality in use, `None` if off (plans/REFACTOR_PIPELINE.md proxy), as read
    /// from the last `Command::SetProxy` — see `fill_segments`
    /// where it decides whether to resolve the source path or the proxy one.
    proxy: Option<ProxyQuality>,
    /// See `RenderAhead::set_preview_divisor`.
    preview_divisor: u32,
    target: &'a LiveTarget,
    /// `target.jumps()` when the cycle started.
    jumps: u64,
    /// The ranges the transit towards a segment must still hand out; the
    /// decoder skips the rest. Ahead only the window ahead counts: the first
    /// frame at the playhead comes first, the window behind is filled
    /// afterwards by its own decoders. Behind, the whole window: the transit
    /// of a chunk is the next chunks, farther back.
    window: &'a [WantedRange],
    /// Behind is `Low`: the HW surfaces go to the window ahead first.
    hw_priority: vv_media::HwPriority,
    /// Paused with keyframes allowed: a seek far from its frame shows the
    /// keyframe it landed on first.
    keyframe_first: bool,
    pacing: &'a RefCell<Pacing>,
    /// The cycle ends here, interrupted, to decide again.
    decide_by: Option<Instant>,
}

/// Decodes what is needed to cover `segments`, stopping if the budget is
/// saturated or if the live playhead moved too far. `Break` carries the final
/// outcome.
fn fill_segments(
    segments: &[WantedRange],
    ctx: &FillContext,
    open: &mut HashMap<MediaId, OpenDecoder>,
) -> ControlFlow<WalkOutcome> {
    for segment in segments {
        let Some(item) = ctx.project.media_pool.get(segment.media_id) else {
            continue;
        };
        // Already all cached: the position the decoder believes it has is not
        // trusted.
        if ctx
            .caches
            .covers_segment(segment.media_id, segment.source_start, segment.source_end)
        {
            continue;
        }
        // Without room for even one frame it does not start: it would pay the
        // transit for a frame then rejected, cycle after cycle. The transit never
        // counts against the budget of the wanted stretch.
        let max_size = (ctx.preview_divisor > 1).then(|| {
            (
                (item.meta.width / ctx.preview_divisor).max(1),
                (item.meta.height / ctx.preview_divisor).max(1),
            )
        });
        let (frame_w, frame_h) = max_size.unwrap_or((item.meta.width, item.meta.height));
        let estimated_frame_bytes = vv_media::yuv420_frame_bytes(frame_w, frame_h);
        if ctx.caches.bytes_used() + estimated_frame_bytes > ctx.cache_budget_bytes {
            if debug_enabled() {
                eprintln!(
                    "[render_ahead] BUDGET-ALREADY-FULL media={:?} segment=[{},{}] bytes_used={} budget={}",
                    segment.media_id,
                    segment.source_start,
                    segment.source_end,
                    ctx.caches.bytes_used(),
                    ctx.cache_budget_bytes
                );
            }
            return ControlFlow::Break(WalkOutcome::UNFINISHED);
        }
        // Proxy only if enabled and already generated; otherwise the source, until the
        // proxy shows up on disk.
        let (path, is_proxy) = decode_path(item, ctx.proxy);
        match position_decoder(
            ctx.caches,
            open,
            segment.media_id,
            &path,
            segment.source_start,
            ctx.went_backward,
            is_proxy,
            item.meta.is_image(),
            ctx.hw_priority,
            max_size,
        ) {
            Positioned::Failed => continue,
            // Going on in sequence towards a segment ahead (a jump shorter
            // than a GOP, or one that interrupted a skip) is a transit too.
            Positioned::Reused => {
                let od = open.get_mut(&segment.media_id).unwrap();
                let first_wanted = first_wanted_frame(ctx.window, segment, od.next_frame);
                if first_wanted > od.next_frame {
                    od.decoder.skip_before(first_wanted);
                }
            }
            Positioned::Seeked | Positioned::Opened => {
                let od = open.get_mut(&segment.media_id).unwrap();
                if let Some(landing) = od.decoder.landing() {
                    od.record_keyframe_landing(landing, segment.source_start);
                    let first_wanted = first_wanted_frame(ctx.window, segment, landing);
                    if ctx.keyframe_first && !is_proxy {
                        show_landing_first(ctx, od, segment.media_id, first_wanted - landing);
                    }
                    od.decoder.skip_before(first_wanted);
                }
            }
        }
        // Where the decoder stood when the throughput sample started.
        let mut sample: Option<(Instant, FrameIdx)> = None;

        // `next_frame` is a placeholder until the first frame after the seek
        // tells the real position.
        let mut resumed = false;
        // Transit bytes of this round: they must not prevent the segment from
        // reaching itself with a tight budget.
        let mut transit_bytes: usize = 0;
        let mut transit_frames: FrameIdx = 0;
        loop {
            let od = open.get_mut(&segment.media_id).unwrap();
            if od.next_frame > segment.source_end {
                break;
            }
            // Rejoined with a part already cached all the way to the end of the segment: the
            // rest is already there. A single contiguous interval is needed, not two points
            // present on different islands.
            if resumed
                && ctx
                    .caches
                    .covers(segment.media_id, od.next_frame, segment.source_end)
            {
                if debug_enabled() {
                    eprintln!(
                        "[render_ahead] RIAGGANCIO media={:?} segment=[{},{}] next_frame={}",
                        segment.media_id, segment.source_start, segment.source_end, od.next_frame
                    );
                }
                break;
            }
            match sample {
                None if od.next_frame > 0 => sample = Some((Instant::now(), od.next_frame)),
                Some((since, from)) if od.next_frame - from >= THROUGHPUT_SAMPLE_FRAMES => {
                    let secs = since.elapsed().as_secs_f64().max(1e-6);
                    ctx.pacing.borrow_mut().record_throughput(
                        segment.media_id,
                        ctx.caches.stride(segment.media_id),
                        (od.next_frame - from) as f64 / secs,
                    );
                    sample = Some((Instant::now(), od.next_frame));
                }
                _ => {}
            }
            let threshold_frames = od.seek_threshold_frames();
            if od.decoder.is_skipping() {
                match od.decoder.skip_some(SKIP_STEP_FRAMES) {
                    // Where the decoder is, should a jump restart the cycle.
                    Ok(skipped) => {
                        if let Some(idx) = skipped {
                            od.next_frame = idx + 1;
                        }
                    }
                    Err(_) => break,
                }
            } else {
                match od.decoder.next_frame() {
                    Ok(Some((idx, frame))) => {
                        let frame_bytes = frame.byte_len();
                        if idx >= segment.source_start {
                            if ctx.caches.bytes_used().saturating_sub(transit_bytes)
                                >= ctx.cache_budget_bytes
                            {
                                if debug_enabled() {
                                    eprintln!(
                                        "[render_ahead] BUDGET-SATURO media={:?} segment=[{},{}] next_frame={} bytes_used={} transit_bytes={transit_bytes} budget={}",
                                        segment.media_id,
                                        segment.source_start,
                                        segment.source_end,
                                        od.next_frame,
                                        ctx.caches.bytes_used(),
                                        ctx.cache_budget_bytes
                                    );
                                }
                                return ControlFlow::Break(WalkOutcome::UNFINISHED);
                            }
                        } else {
                            // Transit before the segment: kept anyway, a nearby segment in
                            // this same round reuses it instead of crossing the GOP again.
                            if transit_frames >= TRANSIT_SAFETY_CAP_FRAMES {
                                if debug_enabled() {
                                    eprintln!(
                                        "[render_ahead] EXCESSIVE-TRANSIT media={:?} segment=[{},{}] next_frame={} transit_frames={transit_frames}",
                                        segment.media_id,
                                        segment.source_start,
                                        segment.source_end,
                                        od.next_frame,
                                    );
                                }
                                return ControlFlow::Break(WalkOutcome::UNFINISHED);
                            }
                            transit_bytes += frame_bytes;
                            transit_frames += 1;
                        }
                        ctx.caches.insert(segment.media_id, idx, frame);
                        od.next_frame = idx + 1;
                        resumed = true;
                        let stride = ctx.caches.stride(segment.media_id);
                        if stride > 1 && idx >= segment.source_start {
                            od.decoder
                                .skip_before((idx.div_euclid(stride) + 1) * stride);
                        }
                    }
                    Ok(None) => {
                        if debug_enabled() {
                            eprintln!(
                                "[render_ahead] DECODE-FINE(EOF) media={:?} segment=[{},{}] next_frame={}",
                                segment.media_id,
                                segment.source_start,
                                segment.source_end,
                                od.next_frame
                            );
                        }
                        break;
                    }
                    Err(e) => {
                        if debug_enabled() {
                            eprintln!(
                                "[render_ahead] DECODE-FINE(ERR) media={:?} segment=[{},{}] next_frame={} error={e}",
                                segment.media_id,
                                segment.source_start,
                                segment.source_end,
                                od.next_frame
                            );
                        }
                        break;
                    }
                }
            }
            // Live target: if the playhead moved past the threshold the prefetch is
            // obsolete and it restarts immediately.
            let live = ctx.target.frame();
            if ctx.target.jumps() != ctx.jumps
                || (live - ctx.from_frame).abs() > threshold_frames
                || ctx.decide_by.is_some_and(|by| Instant::now() >= by)
            {
                if debug_enabled() {
                    eprintln!(
                        "[render_ahead] INTERROTTO media={:?} segment=[{},{}] live={live} from_frame={} threshold={threshold_frames} estimated_gop={:?} last_keyframe_landed={:?}",
                        segment.media_id,
                        segment.source_start,
                        segment.source_end,
                        ctx.from_frame,
                        open.get(&segment.media_id).unwrap().estimated_gop,
                        open.get(&segment.media_id).unwrap().last_keyframe_landed,
                    );
                }
                return ControlFlow::Break(WalkOutcome {
                    interrupted: true,
                    caught_up: false,
                });
            }
        }

        if debug_enabled() {
            let final_next_frame = open.get(&segment.media_id).unwrap().next_frame;
            let from_frame = ctx.from_frame;
            eprintln!(
                "[render_ahead] media={:?} target_frame={from_frame} segment=[{},{}] next_frame_after={final_next_frame} bytes_used={} cached_ranges={:?}",
                segment.media_id,
                segment.source_start,
                segment.source_end,
                ctx.caches.bytes_used(),
                ctx.caches.cached_ranges(segment.media_id)
            );
        }
    }
    ControlFlow::Continue(())
}

#[cfg(test)]
#[path = "tests/render_ahead.rs"]
mod tests;
