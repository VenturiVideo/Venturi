//! Audio buffers already in the mixer's format, per `(path, audio_stream_index)`,
//! in blocks of `BLOCK_FRAMES` holding only what the timeline reads, within
//! a memory budget that keeps the blocks closest to the playhead. Decode +
//! resample on a dedicated thread, seeking to each job and taking the one
//! closest to the playhead first; every block plays as soon as it is decoded.

use std::collections::{HashMap, HashSet};
use std::ops::{ControlFlow, Range};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use vv_audio::{
    AnalysisSlot, AudioSource, BLOCK_FRAMES, ClipAudio, LevelAnalysis, LevelReading, SampleBlocks,
};
use vv_audio::{LevelSegment, Measured, PluginHandle, PluginRequest, StretchInput, StretchRequest};
use vv_core::{NormalizeMode, Project, Timeline};
use vv_session::audio_render::{LevelCache, measure_level, stretch_offline};
use vv_session::export::ExportError;

use crate::worker::Worker;

type Key = (PathBuf, usize);

/// A `StretchRequest` owning what it borrows, but the project (as
/// `OwnedAnalysis`).
struct OwnedStretch {
    key: u64,
    /// Set when its clip is no longer played: it stops being rendered.
    cancelled: Arc<AtomicBool>,
    input: OwnedInput,
    frames: Range<u64>,
    tempo: f64,
    format: (u32, u16),
}

enum OwnedInput {
    File(PathBuf, usize),
    Nested(Timeline, u32, u64),
}

impl OwnedStretch {
    fn request<'a>(&'a self, project: &'a Project) -> StretchRequest<'a> {
        StretchRequest {
            project,
            input: match &self.input {
                OwnedInput::File(path, stream) => StretchInput::File {
                    path,
                    stream: *stream,
                },
                OwnedInput::Nested(timeline, depth, scope) => StretchInput::Nested {
                    timeline,
                    depth: *depth,
                    scope: *scope,
                },
            },
            frames: self.frames.clone(),
            tempo: self.tempo,
            sample_rate: self.format.0,
            channels: self.format.1,
            key: self.key,
        }
    }
}

struct StretchJob {
    project: Arc<Project>,
    stretch: OwnedStretch,
}

enum Stretch {
    Pending(Arc<AtomicBool>),
    Ready(SampleBlocks),
    Failed,
}

/// A `LevelAnalysis` owning what it borrows, but the project: one copy of
/// it serves every analysis a pass asks for (`send_jobs`).
struct OwnedAnalysis {
    key: u64,
    slot: Option<AnalysisSlot>,
    timeline: Timeline,
    measured: Measured,
    mode: NormalizeMode,
    format: (u32, u16),
    segments: Vec<LevelSegment>,
    dry: Vec<u64>,
    scope: u64,
}

impl OwnedAnalysis {
    fn analysis<'a>(&'a self, project: &'a Project) -> LevelAnalysis<'a> {
        LevelAnalysis {
            slot: self.slot,
            project,
            timeline: &self.timeline,
            measured: self.measured,
            mode: self.mode,
            sample_rate: self.format.0,
            channels: self.format.1,
            segments: self.segments.clone(),
            dry: self.dry.clone(),
            scope: self.scope,
        }
    }
}

struct LevelJob {
    project: Arc<Project>,
    analysis: OwnedAnalysis,
}

/// `None`: dropped for a newer job of the same slot. Last: measured while
/// an installed plugin was not made, with the `retry` generation it ran under.
type LevelResult = (u64, Option<AnalysisSlot>, Option<f32>, Option<u64>);

/// Blocks kept on each side of what the clips read: a trim into them
/// decodes nothing.
const MARGIN_BLOCKS: u64 = 1;

/// Blocks per decode job: how far a decode goes before the worker looks
/// again for the job closest to the playhead.
const JOB_BLOCKS: u64 = 8;

/// New blocks rebuild the mix at most this often: a rebuild walks every
/// block of every clip.
const BLOCK_REBUILD_INTERVAL: Duration = Duration::from_millis(250);

/// Over budget, a playhead move this long chooses again what to keep.
const REBALANCE_FRAMES: u64 = 2 * vv_audio::mixer::PROJECT_SAMPLE_RATE as u64;

pub const DEFAULT_AUDIO_BUDGET_BYTES: usize = 512_000_000;

#[derive(Default)]
struct StreamCache {
    blocks: HashMap<u64, Arc<Vec<f32>>>,
    jobs: Vec<Job>,
    /// Frames of the stream, once a decode reached its end.
    end: Option<u64>,
    /// The stream ends before this: a decode seeked here found nothing.
    ends_before: Option<u64>,
    /// No such stream, or it does not decode.
    missing: bool,
    /// Read since the pass began.
    asked: bool,
    /// Blocks read since the pass began, with when they play.
    wanted: HashMap<u64, u64>,
    /// Read by the last timeline pass: a media preview keeps its blocks,
    /// outside the budget, for when the timeline plays again.
    timeline: bool,
}

impl StreamCache {
    /// Cancels and forgets the jobs left without a block `wanted` accepts.
    /// Takes the job list, not `&mut self`: callers hold other field borrows.
    fn retain_jobs(
        jobs: &mut Vec<Job>,
        job_keys: &mut HashMap<u64, Key>,
        wanted: impl Fn(u64) -> bool,
    ) {
        jobs.retain(|job| {
            let keep = job.blocks.clone().any(&wanted);
            if !keep {
                job.cancelled.store(true, Ordering::Relaxed);
                job_keys.remove(&job.id);
            }
            keep
        });
    }

    fn in_flight(&self, block: u64) -> bool {
        self.jobs.iter().any(|job| job.blocks.contains(&block))
    }
}

struct Job {
    id: u64,
    blocks: Range<u64>,
    cancelled: Arc<AtomicBool>,
}

struct DecodeJob {
    id: u64,
    path: PathBuf,
    stream: usize,
    blocks: Range<u64>,
    /// When its blocks play, for the order of the jobs.
    plays: Range<u64>,
    cancelled: Arc<AtomicBool>,
}

/// 0 while `plays` holds the playhead: a job ranked by its first block
/// would lose to the next one, whose start is closer.
fn distance(plays: &Range<u64>, playhead: u64) -> u64 {
    if playhead < plays.start {
        plays.start - playhead
    } else {
        playhead.saturating_sub(plays.end - 1)
    }
}

enum Decoded {
    Block {
        job: u64,
        index: u64,
        samples: Arc<Vec<f32>>,
    },
    Done {
        job: u64,
        /// Frames of the stream, if the decode reached its end.
        end: Option<u64>,
        /// Where it seeked, if it reached the end with nothing decoded.
        ends_before: Option<u64>,
        missing: bool,
    },
}

pub struct MixBufferCache {
    channels: u16,
    budget_bytes: usize,
    streams: HashMap<Key, StreamCache>,
    job_keys: HashMap<u64, Key>,
    next_id: u64,
    /// Where the mix plays, in samples of what is mixed (a timeline, or a
    /// media in the preview): the worker reads it to pick a job.
    playhead: Arc<AtomicU64>,
    /// Between `begin_*pass` and `sweep_unused`, which plans what was asked.
    in_pass: bool,
    /// Blocks asked for outside a pass, or a playhead move over budget:
    /// `poll` runs `rebalance`.
    unplanned: bool,
    /// The last `rebalance` left blocks out, and where the playhead was.
    over_budget: bool,
    rebalanced_at: u64,
    /// A media preview is mixed, not a timeline: its pass keeps the
    /// timeline's mixdowns, stretches and levels.
    media_pass: bool,
    worker: Worker<DecodeJob>,
    decoded_rx: mpsc::Receiver<Decoded>,
    /// Blocks arrived and not yet reported by `poll`, and when it last did.
    unreported_blocks: bool,
    last_block_report: Option<Instant>,
    /// Pitch-corrected clip audio by `StretchRequest::key`.
    stretched: HashMap<u64, Stretch>,
    /// Keys asked for since the last `sweep_unused`.
    stretch_used: HashSet<u64>,
    queued_stretches: Vec<OwnedStretch>,
    /// Jobs sent and not done: the queued ones wait until it is 0, so a
    /// drag clones the project as often as the worker takes a job, not
    /// every frame.
    stretch_busy: Arc<AtomicUsize>,
    stretch_worker: Worker<StretchJob>,
    stretch_rx: mpsc::Receiver<(u64, Result<SampleBlocks, String>, Option<u64>)>,
    /// Normalization levels by `LevelAnalysis::key`; `None` while measuring.
    levels: HashMap<u64, Option<f32>>,
    last_level: HashMap<AnalysisSlot, f32>,
    level_used: HashSet<u64>,
    queued_levels: Vec<OwnedAnalysis>,
    level_busy: Arc<AtomicUsize>,
    level_worker: Worker<LevelJob>,
    level_rx: mpsc::Receiver<LevelResult>,
    /// Keys of the levels and stretches rendered while a plugin was not
    /// made: a rescan measures them again.
    degraded: HashSet<u64>,
    /// Bumped by a rescan: the workers' pools forget their failures.
    retry: Arc<AtomicU64>,
    /// Stops the level worker's measures when the cache drops.
    closing: Arc<AtomicBool>,
    /// What the level worker measures, and the flag that stops it: set by
    /// `abort_outdated_level` under the lock.
    level_inflight: Arc<std::sync::Mutex<Option<(AnalysisSlot, u64)>>>,
    level_abort: Arc<AtomicBool>,
    /// Created on the first plugin asked for, so projects without plugins
    /// never touch the host.
    plugins: Option<vv_plugin::Instances>,
}

impl MixBufferCache {
    pub fn spawn(sample_rate: u32, channels: u16) -> Self {
        let (decoded_tx, decoded_rx) = mpsc::channel::<Decoded>();
        let playhead = Arc::new(AtomicU64::new(0));
        let worker_playhead = playhead.clone();
        let worker = Worker::spawn(move |job_rx: mpsc::Receiver<DecodeJob>| {
            let mut queue: Vec<DecodeJob> = Vec::new();
            loop {
                if queue.is_empty() {
                    match job_rx.recv() {
                        Ok(job) => queue.push(job),
                        Err(_) => break,
                    }
                }
                queue.extend(job_rx.try_iter());
                queue.retain(|job| !job.cancelled.load(Ordering::Relaxed));
                let playhead = worker_playhead.load(Ordering::Relaxed);
                let batch = take_batch(&mut queue, playhead);
                let Some(path) = batch.first().map(|job| job.path.clone()) else {
                    continue;
                };
                if !decode_blocks(&path, &batch, sample_rate, channels, &decoded_tx) {
                    break;
                }
            }
        });
        let (stretch_tx, stretch_rx) = mpsc::channel();
        let stretch_busy = Arc::new(AtomicUsize::new(0));
        let worker_stretch_busy = stretch_busy.clone();
        let retry = Arc::new(AtomicU64::new(0));
        let stretch_retry = retry.clone();
        let stretch_worker = Worker::spawn(move |jobs: mpsc::Receiver<StretchJob>| {
            let mut cache = worker_levels();
            cache.threads = 1;
            let mut retried = 0;
            for StretchJob { project, stretch } in jobs {
                retry_failures(&mut cache, &stretch_retry, &mut retried);
                cache.cancel = Some(stretch.cancelled.clone());
                cache.degraded = false;
                let result = match stretch_offline(&stretch.request(&project), &mut cache) {
                    Err(ExportError::Cancelled) => None,
                    result => Some(result.map_err(|e| e.to_string())),
                };
                worker_stretch_busy.fetch_sub(1, Ordering::Relaxed);
                if let Some(result) = result
                    && stretch_tx
                        .send((stretch.key, result, cache.degraded.then_some(retried)))
                        .is_err()
                {
                    break;
                }
            }
        });
        let (level_tx, level_rx) = mpsc::channel::<LevelResult>();
        let level_busy = Arc::new(AtomicUsize::new(0));
        let worker_level_busy = level_busy.clone();
        let closing = Arc::new(AtomicBool::new(false));
        let worker_closing = closing.clone();
        let level_inflight = Arc::new(std::sync::Mutex::new(None));
        let worker_inflight = level_inflight.clone();
        let level_abort = Arc::new(AtomicBool::new(false));
        let worker_abort = level_abort.clone();
        let level_retry = retry.clone();
        let level_worker = Worker::spawn(move |jobs: mpsc::Receiver<LevelJob>| {
            // Half the cores: the playback's decodes need the rest.
            let mut cache = worker_levels();
            cache.threads = cache.threads.div_ceil(2);
            cache.cancel = Some(worker_abort.clone());
            let mut retried = 0;
            while let Ok(first) = jobs.recv() {
                retry_failures(&mut cache, &level_retry, &mut retried);
                let mut batch = vec![first];
                // Only the newest job of each slot is worth measuring.
                while let Ok(job) = jobs.try_recv() {
                    let slot = job.analysis.slot;
                    let older =
                        slot.and_then(|s| batch.iter().position(|j| j.analysis.slot == Some(s)));
                    if let Some(older) = older {
                        let dropped = std::mem::replace(&mut batch[older], job).analysis;
                        worker_level_busy.fetch_sub(1, Ordering::Relaxed);
                        if level_tx.send((dropped.key, slot, None, None)).is_err() {
                            return;
                        }
                    } else {
                        batch.push(job);
                    }
                }
                // Short measures first: a continuous one (plugins) can take long.
                batch.sort_by_key(|job| {
                    (job.analysis.segments.iter())
                        .map(|s| s.frames.end - s.frames.start)
                        .sum::<u64>()
                });
                for LevelJob { project, analysis } in batch {
                    cache.degraded = false;
                    if worker_closing.load(Ordering::Relaxed) {
                        return;
                    }
                    {
                        let mut inflight = worker_inflight.lock().unwrap();
                        *inflight = analysis.slot.map(|slot| (slot, analysis.key));
                        worker_abort.store(worker_closing.load(Ordering::SeqCst), Ordering::SeqCst);
                    }
                    if worker_closing.load(Ordering::SeqCst) {
                        return;
                    }
                    let measured = measure_level(&analysis.analysis(&project), &mut cache);
                    *worker_inflight.lock().unwrap() = None;
                    worker_level_busy.fetch_sub(1, Ordering::Relaxed);
                    if worker_closing.load(Ordering::Relaxed) {
                        return;
                    }
                    // Superseded: asked again if still wanted.
                    if matches!(measured, Err(ExportError::Cancelled)) {
                        if level_tx
                            .send((analysis.key, analysis.slot, None, None))
                            .is_err()
                        {
                            return;
                        }
                        continue;
                    }
                    // Unity gain rather than a failed measure.
                    let level = measured.unwrap_or_else(|e| {
                        eprintln!("[mix_buffers] level: {e}");
                        0.0
                    });
                    if level_tx
                        .send((
                            analysis.key,
                            analysis.slot,
                            Some(level),
                            cache.degraded.then_some(retried),
                        ))
                        .is_err()
                    {
                        return;
                    }
                }
            }
        });
        Self {
            channels,
            budget_bytes: DEFAULT_AUDIO_BUDGET_BYTES,
            streams: HashMap::new(),
            job_keys: HashMap::new(),
            next_id: 0,
            playhead,
            in_pass: false,
            unplanned: false,
            over_budget: false,
            rebalanced_at: 0,
            media_pass: false,
            worker,
            decoded_rx,
            unreported_blocks: false,
            last_block_report: None,
            stretched: HashMap::new(),
            stretch_used: HashSet::new(),
            queued_stretches: Vec::new(),
            stretch_busy,
            stretch_worker,
            stretch_rx,
            levels: HashMap::new(),
            last_level: HashMap::new(),
            level_used: HashSet::new(),
            queued_levels: Vec::new(),
            level_busy,
            level_worker,
            level_rx,
            degraded: HashSet::new(),
            retry,
            closing,
            level_inflight,
            level_abort,
            plugins: None,
        }
    }

    pub fn set_budget_bytes(&mut self, bytes: usize) {
        if bytes != self.budget_bytes {
            self.budget_bytes = bytes;
            self.unplanned = true;
        }
    }

    /// Where the mix plays now, in samples of what is mixed.
    pub fn set_playhead(&mut self, sample: u64) {
        self.playhead.store(sample, Ordering::Relaxed);
        if self.over_budget && sample.abs_diff(self.rebalanced_at) > REBALANCE_FRAMES {
            self.unplanned = true;
        }
    }

    /// From here to `sweep_unused`, a full mix of the timeline: what it
    /// does not read is dropped.
    pub fn begin_pass(&mut self) {
        self.begin(false);
    }

    /// Like `begin_pass`, for the preview of a media: the timeline's
    /// blocks, stretches and levels are kept for when it plays again.
    pub fn begin_media_pass(&mut self) {
        self.begin(true);
    }

    fn begin(&mut self, media_pass: bool) {
        self.in_pass = true;
        self.media_pass = media_pass;
        if let (false, Some(plugins)) = (media_pass, &mut self.plugins) {
            plugins.begin_pass();
        }
        for stream in self.streams.values_mut() {
            stream.asked = false;
            stream.wanted.clear();
        }
    }

    /// Drops the blocks, mixdowns, stretches and levels the pass did not
    /// ask for, cancels the decodes no longer wanted and plans the others.
    pub fn sweep_unused(&mut self) {
        self.in_pass = false;
        let media_pass = self.media_pass;
        let job_keys = &mut self.job_keys;
        self.streams.retain(|_, stream| {
            let StreamCache {
                blocks,
                jobs,
                asked,
                wanted,
                timeline,
                ..
            } = stream;
            if !media_pass {
                *timeline = *asked;
            }
            let parked = media_pass && !*asked && *timeline;
            if !parked {
                blocks.retain(|index, _| wanted.contains_key(index));
            }
            StreamCache::retain_jobs(jobs, job_keys, |index| wanted.contains_key(&index));
            *asked || parked
        });
        self.rebalance();
        if self.media_pass {
            return;
        }
        // The instances die here, on the UI thread: the mixer thread only
        // ever holds clones of them.
        if let Some(plugins) = &mut self.plugins {
            plugins.sweep();
        }
        let used = std::mem::take(&mut self.stretch_used);
        self.stretched.retain(|key, stretch| {
            let keep = used.contains(key);
            if let (false, Stretch::Pending(cancelled)) = (keep, stretch) {
                cancelled.store(true, Ordering::Relaxed);
            }
            keep
        });
        self.queued_stretches
            .retain(|stretch| !stretch.cancelled.load(Ordering::Relaxed));
        let used = std::mem::take(&mut self.level_used);
        self.levels.retain(|key, _| used.contains(key));
        self.degraded
            .retain(|key| self.levels.contains_key(key) || self.stretched.contains_key(key));
        let levels = &self.levels;
        self.queued_levels
            .retain(|analysis| levels.contains_key(&analysis.key));
    }

    /// Adopts the installed list in the live pool: see `Instances::refresh`.
    /// Returns the instances dropped.
    pub fn refresh_plugins(
        &mut self,
        known: Arc<[vv_plugin::PluginInfo]>,
    ) -> Vec<Arc<dyn vv_plugin::HostedPlugin>> {
        let dropped = self
            .plugins
            .as_mut()
            .map(|plugins| plugins.refresh(known))
            .unwrap_or_default();
        self.retry.fetch_add(1, Ordering::Relaxed);
        self.remeasure_degraded();
        dropped
    }

    /// Lets plugins that failed be tried again, keeping the live ones.
    pub fn clear_plugin_failures(&mut self) {
        if let Some(plugins) = &mut self.plugins {
            plugins.clear_failures();
        }
        self.retry.fetch_add(1, Ordering::Relaxed);
        self.remeasure_degraded();
    }

    /// Drops what the workers rendered while a plugin was not made; the
    /// next pass asks for it again (`last_level` plays meanwhile). Once per
    /// rescan, so a plugin that keeps failing cannot loop.
    fn remeasure_degraded(&mut self) {
        for key in std::mem::take(&mut self.degraded) {
            self.levels.remove(&key);
            if let Some(Stretch::Ready(_) | Stretch::Failed) = self.stretched.get(&key) {
                self.stretched.remove(&key);
            }
        }
    }

    fn plugins(&mut self) -> &mut vv_plugin::Instances {
        self.plugins
            .get_or_insert_with(|| vv_plugin::Instances::realtime(vv_plugin::installed()))
    }

    /// The instance playing under `live`, else one made for `aside` that no
    /// mix uses: the pool's sweep drops its own copy at the next pass.
    pub fn plugin_instance(
        &mut self,
        live: Option<u64>,
        aside: u64,
        effect: &vv_core::Vst3Effect,
        sample_rate: u32,
    ) -> Option<Arc<dyn vv_plugin::HostedPlugin>> {
        let plugins = self.plugins();
        if let Some(instance) = live.and_then(|key| plugins.instance(key)) {
            return Some(instance);
        }
        plugins.get(PluginRequest {
            key: aside,
            effect,
            sample_rate,
            process: true,
        });
        plugins.instance(aside)
    }

    /// The instances open editors drive, by live key; see `Instances::set_held`.
    pub fn hold_plugins(&mut self, held: Vec<(u64, Arc<dyn vv_plugin::HostedPlugin>)>) {
        if let Some(plugins) = &mut self.plugins {
            plugins.set_held(held);
        }
    }

    pub fn service_plugin_restarts(&self) {
        if let Some(plugins) = &self.plugins {
            plugins.service_restarts();
        }
    }

    pub fn plugin_latency(&self, key: u64) -> Option<u32> {
        self.plugins.as_ref()?.instance(key).map(|p| p.latency())
    }

    /// Whether the pool has scanned `effect`'s plugin; `None` without a pool.
    pub fn plugin_known(&self, effect: &vv_core::Vst3Effect) -> Option<bool> {
        self.plugins.as_ref().map(|p| p.knows(effect))
    }

    pub fn plugin_failed(&self, key: u64) -> bool {
        self.plugins.as_ref().is_some_and(|p| p.failed(key))
    }

    /// Frames `frames` of `stream` of `path`, as far as decoded; for the
    /// preview of a media, so they play at their own position.
    pub fn media_frames(
        &mut self,
        path: &Path,
        stream: usize,
        frames: Range<u64>,
    ) -> Option<SampleBlocks> {
        let plays_at = frames.start;
        match self.lookup(path, stream, frames, plays_at) {
            ClipAudio::Ready(buffer) | ClipAudio::Partial(buffer) => Some(buffer),
            ClipAudio::Pending | ClipAudio::Missing => None,
        }
    }

    /// Non-blocking: the blocks holding `needed`, `Partial` while some are
    /// not decoded. Asks for them, with `MARGIN_BLOCKS` around:
    /// `rebalance` decides which to decode and keep.
    fn lookup(
        &mut self,
        path: &Path,
        stream: usize,
        needed: Range<u64>,
        plays_at: u64,
    ) -> ClipAudio {
        let playhead = self.playhead.load(Ordering::Relaxed);
        let entry = self
            .streams
            .entry((path.to_path_buf(), stream))
            .or_default();
        entry.asked = true;
        if entry.missing {
            return ClipAudio::Missing;
        }
        let first_block = needed.start / BLOCK_FRAMES;
        let limit = entry.end.or(entry.ends_before).unwrap_or(u64::MAX);
        let last_block = needed
            .end
            .min(limit)
            .div_ceil(BLOCK_FRAMES)
            .max(first_block);
        let blocks_end = limit.div_ceil(BLOCK_FRAMES);
        let wanted = first_block.saturating_sub(MARGIN_BLOCKS)
            ..(last_block + MARGIN_BLOCKS).min(blocks_end).max(last_block);
        for index in wanted {
            let plays = (plays_at + index * BLOCK_FRAMES).saturating_sub(needed.start);
            let closest = entry.wanted.get(&index).is_none_or(|&other| {
                distance(&(plays..plays + BLOCK_FRAMES), playhead)
                    < distance(&(other..other + BLOCK_FRAMES), playhead)
            });
            if closest {
                entry.wanted.insert(index, plays);
            }
            if !self.in_pass && !entry.blocks.contains_key(&index) && !entry.in_flight(index) {
                self.unplanned = true;
            }
        }

        let blocks: Vec<Option<Arc<Vec<f32>>>> = (first_block..last_block)
            .map(|index| entry.blocks.get(&index).cloned())
            .collect();

        let view_end = entry.end.unwrap_or(u64::MAX).min(last_block * BLOCK_FRAMES);
        let complete = blocks.iter().all(Option::is_some);
        let view = SampleBlocks::new(
            first_block * BLOCK_FRAMES,
            BLOCK_FRAMES,
            self.channels,
            blocks,
            view_end,
        );
        // Even with no block yet: the clip stays in the snapshot, silent,
        // and `MixSnapshot::is_ready` sees what it misses.
        if complete {
            ClipAudio::Ready(view)
        } else {
            ClipAudio::Partial(view)
        }
    }

    /// Keeps the wanted blocks closest to the playhead that fit the
    /// budget: drops the others, plans the missing ones, cancels the jobs
    /// left without one. `true` if it dropped blocks.
    fn rebalance(&mut self) -> bool {
        self.unplanned = false;
        let playhead = self.playhead.load(Ordering::Relaxed);
        self.rebalanced_at = playhead;
        let mut wanted: Vec<(u64, &Key, u64)> = self
            .streams
            .iter()
            .flat_map(|(key, stream)| {
                stream
                    .wanted
                    .iter()
                    .map(move |(&index, &plays)| (plays, key, index))
            })
            .collect();
        wanted.sort_unstable_by_key(|&(plays, key, index)| {
            (
                distance(&(plays..plays + BLOCK_FRAMES), playhead),
                key,
                index,
            )
        });
        let block_bytes = BLOCK_FRAMES as usize * self.channels.max(1) as usize * 4;
        let fit = (self.budget_bytes / block_bytes).min(wanted.len());
        self.over_budget = fit < wanted.len();
        let mut kept: HashMap<Key, Vec<(u64, u64)>> = HashMap::new();
        for &(plays, key, index) in &wanted[..fit] {
            kept.entry(key.clone()).or_default().push((index, plays));
        }

        let mut dropped = false;
        let mut plans: Vec<(Key, Range<u64>, Range<u64>)> = Vec::new();
        for (key, stream) in &mut self.streams {
            let keep = kept.remove(key).unwrap_or_default();
            let keep_set: HashSet<u64> = keep.iter().map(|&(index, _)| index).collect();
            let before = stream.blocks.len();
            let wanted = &stream.wanted;
            stream
                .blocks
                .retain(|index, _| keep_set.contains(index) || !wanted.contains_key(index));
            dropped |= stream.blocks.len() < before;
            let job_keys = &mut self.job_keys;
            StreamCache::retain_jobs(&mut stream.jobs, job_keys, |index| {
                keep_set.contains(&index)
            });
            let mut missing: Vec<(u64, u64)> = keep
                .into_iter()
                .filter(|&(index, _)| {
                    !stream.blocks.contains_key(&index) && !stream.in_flight(index)
                })
                .collect();
            missing.sort_unstable();
            for (index, plays) in missing {
                match plans.last_mut() {
                    Some((k, run, run_plays))
                        if k == key && run.end == index && run.end - run.start < JOB_BLOCKS =>
                    {
                        run.end += 1;
                        run_plays.start = run_plays.start.min(plays);
                        run_plays.end = run_plays.end.max(plays + BLOCK_FRAMES);
                    }
                    _ => plans.push((key.clone(), index..index + 1, plays..plays + BLOCK_FRAMES)),
                }
            }
        }
        // The worker may start on the first before the others arrive.
        plans.sort_by_key(|(_, _, plays)| distance(plays, playhead));
        for (key, blocks, plays) in plans {
            let id = self.next_id;
            self.next_id += 1;
            let cancelled = Arc::new(AtomicBool::new(false));
            self.worker.send(DecodeJob {
                id,
                path: key.0.clone(),
                stream: key.1,
                blocks: blocks.clone(),
                plays,
                cancelled: cancelled.clone(),
            });
            self.job_keys.insert(id, key.clone());
            if let Some(stream) = self.streams.get_mut(&key) {
                stream.jobs.push(Job {
                    id,
                    blocks,
                    cancelled,
                });
            }
        }
        dropped
    }

    /// Collects the decoded blocks; `true` when the mixer snapshot must be
    /// rebuilt.
    pub fn poll(&mut self) -> bool {
        let mut changed = self.unplanned && self.rebalance();
        while let Ok(decoded) = self.decoded_rx.try_recv() {
            match decoded {
                Decoded::Block {
                    job,
                    index,
                    samples,
                } => {
                    let Some(stream) = self
                        .job_keys
                        .get(&job)
                        .and_then(|k| self.streams.get_mut(k))
                    else {
                        continue;
                    };
                    stream.blocks.entry(index).or_insert(samples);
                    self.unreported_blocks = true;
                }
                Decoded::Done {
                    job,
                    end,
                    ends_before,
                    missing,
                } => {
                    let Some(key) = self.job_keys.remove(&job) else {
                        continue;
                    };
                    if let Some(stream) = self.streams.get_mut(&key) {
                        stream.jobs.retain(|j| j.id != job);
                        stream.end = end.or(stream.end);
                        stream.ends_before = match (stream.ends_before, ends_before) {
                            (Some(a), Some(b)) => Some(a.min(b)),
                            (a, b) => a.or(b),
                        };
                        stream.missing |= missing;
                    }
                    changed = true;
                }
            }
        }
        let now = Instant::now();
        let report_due = self
            .last_block_report
            .is_none_or(|last| now.duration_since(last) >= BLOCK_REBUILD_INTERVAL);
        if changed || (self.unreported_blocks && report_due) {
            changed |= self.unreported_blocks;
            self.unreported_blocks = false;
            self.last_block_report = Some(now);
        }
        while let Ok((key, result, degraded)) = self.stretch_rx.try_recv() {
            if let Some(entry) = self.stretched.get_mut(&key) {
                if let Some(started) = degraded {
                    if started != self.retry.load(Ordering::Relaxed) {
                        // Rendered on the pool a rescan replaced: asked again.
                        self.stretched.remove(&key);
                        changed = true;
                        continue;
                    }
                    self.degraded.insert(key);
                }
                *entry = match result {
                    Ok(stretched) => Stretch::Ready(stretched),
                    Err(e) => {
                        eprintln!("[mix_buffers] stretch failed: {e}");
                        Stretch::Failed
                    }
                };
                changed = true;
            }
        }
        while let Ok((key, slot, level, degraded)) = self.level_rx.try_recv() {
            match level {
                Some(level) => {
                    let stale = degraded.is_some_and(|s| s != self.retry.load(Ordering::Relaxed));
                    if stale {
                        self.levels.remove(&key);
                    } else if let Some(entry) = self.levels.get_mut(&key) {
                        *entry = Some(level);
                        if degraded.is_some() {
                            self.degraded.insert(key);
                        }
                        changed = true;
                    }
                    if let Some(slot) = slot {
                        self.last_level.insert(slot, level);
                    }
                }
                // Asked again, it is measured again.
                None => {
                    self.levels.remove(&key);
                }
            }
        }
        changed
    }

    /// Hands the levels and stretches asked for to their workers once they
    /// are idle, with a copy of `project` to render them from. Every frame:
    /// a worker becoming idle takes the jobs waiting.
    pub fn send_jobs(&mut self, project: &Project) {
        self.abort_outdated_level();
        let idle = |busy: &AtomicUsize| busy.load(Ordering::Relaxed) == 0;
        let levels = !self.queued_levels.is_empty() && idle(&self.level_busy);
        let stretches = !self.queued_stretches.is_empty() && idle(&self.stretch_busy);
        if !levels && !stretches {
            return;
        }
        let project = Arc::new(project.clone());
        if levels {
            self.level_busy
                .fetch_add(self.queued_levels.len(), Ordering::Relaxed);
            for analysis in self.queued_levels.drain(..) {
                self.level_worker.send(LevelJob {
                    project: project.clone(),
                    analysis,
                });
            }
        }
        if stretches {
            self.stretch_busy
                .fetch_add(self.queued_stretches.len(), Ordering::Relaxed);
            for stretch in self.queued_stretches.drain(..) {
                self.stretch_worker.send(StretchJob {
                    project: project.clone(),
                    stretch,
                });
            }
        }
    }

    /// Stops the measure in flight when nothing asks for its key any more
    /// and a newer analysis of its slot waits: a wanted measure is never
    /// stopped.
    fn abort_outdated_level(&self) {
        let inflight = self.level_inflight.lock().unwrap();
        if let Some((slot, key)) = *inflight
            && !self.levels.contains_key(&key)
            && self.queued_levels.iter().any(|a| a.slot == Some(slot))
        {
            self.level_abort.store(true, Ordering::SeqCst);
        }
    }

    #[cfg(test)]
    pub fn has_pending(&mut self) -> bool {
        self.poll();
        self.streams.values().any(|stream| !stream.jobs.is_empty())
            || self.levels.values().any(Option::is_none)
            || self
                .stretched
                .values()
                .any(|stretch| matches!(stretch, Stretch::Pending(_)))
    }
}

/// In memory in this crate's tests: they would fill the user's cache.
fn retry_failures(cache: &mut LevelCache, retry: &AtomicU64, retried: &mut u64) {
    let now = retry.load(Ordering::Relaxed);
    if std::mem::replace(retried, now) != now {
        cache.clear_plugin_failures();
    }
}

fn worker_levels() -> LevelCache {
    if cfg!(test) {
        LevelCache::default()
    } else {
        LevelCache::on_disk()
    }
}

/// The workers join on drop: what they have queued or running is cancelled
/// first, or closing the app would wait for it.
impl Drop for MixBufferCache {
    fn drop(&mut self) {
        for job in self.streams.values().flat_map(|stream| &stream.jobs) {
            job.cancelled.store(true, Ordering::Relaxed);
        }
        for stretch in self.stretched.values() {
            if let Stretch::Pending(cancelled) = stretch {
                cancelled.store(true, Ordering::Relaxed);
            }
        }
        self.closing.store(true, Ordering::SeqCst);
        self.level_abort.store(true, Ordering::SeqCst);
    }
}

/// Every buffer is at the `sample_rate`/`channels` given to `spawn`.
impl AudioSource for MixBufferCache {
    fn plugin(&mut self, request: PluginRequest<'_>) -> Option<PluginHandle> {
        self.plugins().get(request)
    }

    fn plugin_build(&mut self, effect: &vv_core::Vst3Effect) -> Option<u64> {
        self.plugins().build(effect)
    }

    /// The mixer always asks for frames: a whole file is never held.
    fn file(&mut self, _path: &Path, _stream: usize) -> ClipAudio {
        ClipAudio::Missing
    }

    fn file_frames(
        &mut self,
        path: &Path,
        stream: usize,
        frames: Range<u64>,
        plays_at: u64,
    ) -> ClipAudio {
        self.lookup(path, stream, frames, plays_at)
    }

    fn stretched(&mut self, request: StretchRequest) -> ClipAudio {
        self.stretch_used.insert(request.key);
        if let Some(entry) = self.stretched.get(&request.key) {
            return match entry {
                Stretch::Pending(_) => ClipAudio::Pending,
                Stretch::Ready(samples) => ClipAudio::Ready(samples.clone()),
                Stretch::Failed => ClipAudio::Missing,
            };
        }
        let cancelled = Arc::new(AtomicBool::new(false));
        self.queued_stretches.push(OwnedStretch {
            key: request.key,
            cancelled: cancelled.clone(),
            input: match request.input {
                StretchInput::File { path, stream } => OwnedInput::File(path.to_path_buf(), stream),
                StretchInput::Nested {
                    timeline,
                    depth,
                    scope,
                } => OwnedInput::Nested(timeline.clone(), depth, scope),
            },
            frames: request.frames,
            tempo: request.tempo,
            format: (request.sample_rate, request.channels),
        });
        self.stretched
            .insert(request.key, Stretch::Pending(cancelled));
        ClipAudio::Pending
    }

    fn level(&mut self, analysis: LevelAnalysis) -> LevelReading {
        let key = analysis.key();
        self.level_used.insert(key);
        let last = analysis
            .slot
            .and_then(|slot| self.last_level.get(&slot).copied());
        match self.levels.get(&key) {
            Some(Some(level)) => LevelReading::Ready(*level),
            Some(None) => LevelReading::Pending(last),
            None => {
                self.queued_levels.push(OwnedAnalysis {
                    key,
                    slot: analysis.slot,
                    timeline: analysis.timeline.clone(),
                    measured: analysis.measured,
                    mode: analysis.mode,
                    format: (analysis.sample_rate, analysis.channels),
                    segments: analysis.segments,
                    dry: analysis.dry,
                    scope: analysis.scope,
                });
                self.levels.insert(key, None);
                LevelReading::Pending(last)
            }
        }
    }
}

/// The next decode: the job closest to the playhead, with the queued jobs one
/// read of its file serves too, i.e. the other streams over the same blocks.
fn take_batch(queue: &mut Vec<DecodeJob>, playhead: u64) -> Vec<DecodeJob> {
    let Some(next) = queue
        .iter()
        .enumerate()
        .min_by_key(|(_, job)| (distance(&job.plays, playhead), job.id))
        .map(|(i, _)| i)
    else {
        return Vec::new();
    };
    let job = queue.swap_remove(next);
    let (mut batch, rest): (Vec<_>, Vec<_>) = std::mem::take(queue)
        .into_iter()
        .partition(|other| other.path == job.path && other.blocks == job.blocks);
    *queue = rest;
    batch.insert(0, job);
    batch
}

/// Decodes the blocks of `jobs` (all of `path`) in one read,
/// seeking to them, sending each block as soon as it is complete and
/// stopping once the last job is. `false` if the receiver is gone (the
/// worker must exit).
fn decode_blocks(
    path: &Path,
    jobs: &[DecodeJob],
    sample_rate: u32,
    channels: u16,
    decoded_tx: &mpsc::Sender<Decoded>,
) -> bool {
    let mut streams: Vec<usize> = jobs.iter().map(|job| job.stream).collect();
    streams.sort_unstable();
    streams.dedup();
    let slot_of = |stream: usize| streams.iter().position(|&s| s == stream).unwrap();
    let ch = channels.max(1) as u64;
    let windows: Vec<Range<u64>> = jobs
        .iter()
        .map(|job| job.blocks.start * BLOCK_FRAMES..job.blocks.end * BLOCK_FRAMES)
        .collect();
    let from = windows.iter().map(|w| w.start).min().unwrap_or(0);
    let mut filling: Vec<Vec<f32>> = jobs.iter().map(|_| new_block(ch)).collect();
    let mut filled_to: Vec<u64> = windows.iter().map(|w| w.start).collect();
    let mut decoded = vec![0_u64; streams.len()];
    let mut remixed = Vec::new();
    let mut receiver_gone = false;
    let mut stopped_early = false;
    let result = vv_media::decode_audio_streams_from(
        path,
        &streams,
        sample_rate,
        from,
        |slot, src_channels, at, chunk| {
            remixed.clear();
            vv_audio::mixer::remix_channels_into(chunk, src_channels, channels, &mut remixed);
            let to = at + remixed.len() as u64 / ch;
            decoded[slot] = decoded[slot].max(to);
            let mut finished = true;
            for (i, job) in jobs.iter().enumerate() {
                if job.cancelled.load(Ordering::Relaxed) {
                    continue;
                }
                let window = &windows[i];
                if job.stream == streams[slot] {
                    // A gap in the stream plays as silence.
                    let gap_end = at.min(window.end);
                    let until = window.end.min(to);
                    let sent = (filled_to[i] >= gap_end
                        || fill(
                            &mut filling[i],
                            &mut filled_to[i],
                            gap_end,
                            None,
                            ch,
                            job,
                            decoded_tx,
                        ))
                        && (filled_to[i] >= until || {
                            let skip = ((filled_to[i] - at) * ch) as usize;
                            let samples = &remixed[skip..((until - at) * ch) as usize];
                            fill(
                                &mut filling[i],
                                &mut filled_to[i],
                                until,
                                Some(samples),
                                ch,
                                job,
                                decoded_tx,
                            )
                        });
                    if !sent {
                        receiver_gone = true;
                        return ControlFlow::Break(());
                    }
                }
                if filled_to[i] < window.end {
                    finished = false;
                }
            }
            if finished {
                stopped_early = true;
                ControlFlow::Break(())
            } else {
                ControlFlow::Continue(())
            }
        },
    );
    if receiver_gone {
        return false;
    }
    let formats = result.unwrap_or_else(|e| {
        eprintln!("[mix_buffers] decode failed for {}: {e}", path.display());
        vec![None; streams.len()]
    });
    for ((job, last), filled_to) in jobs.iter().zip(filling).zip(filled_to) {
        let slot = slot_of(job.stream);
        let missing = formats[slot].is_none();
        let at_end = !stopped_early && !missing;
        let end = (at_end && decoded[slot] > 0).then_some(decoded[slot]);
        let ends_before = (at_end && decoded[slot] == 0).then_some(from);
        // The block the file ends in, shorter than the others.
        if end.is_some() && !last.is_empty() {
            let block = Decoded::Block {
                job: job.id,
                index: (filled_to - 1) / BLOCK_FRAMES,
                samples: Arc::new(last),
            };
            if decoded_tx.send(block).is_err() {
                return false;
            }
        }
        let done = Decoded::Done {
            job: job.id,
            end,
            ends_before,
            missing,
        };
        if decoded_tx.send(done).is_err() {
            return false;
        }
    }
    true
}

fn new_block(ch: u64) -> Vec<f32> {
    Vec::with_capacity((BLOCK_FRAMES * ch) as usize)
}

/// Appends frames up to `until` to the block being filled (`samples`, or
/// silence), sending every block it completes. `false` if the receiver is gone.
fn fill(
    filling: &mut Vec<f32>,
    filled_to: &mut u64,
    until: u64,
    samples: Option<&[f32]>,
    ch: u64,
    job: &DecodeJob,
    decoded_tx: &mpsc::Sender<Decoded>,
) -> bool {
    let mut offset = 0;
    while *filled_to < until {
        let block_end = (*filled_to / BLOCK_FRAMES + 1) * BLOCK_FRAMES;
        let next = until.min(block_end);
        let count = ((next - *filled_to) * ch) as usize;
        match samples {
            Some(samples) => filling.extend_from_slice(&samples[offset..offset + count]),
            None => filling.resize(filling.len() + count, 0.0),
        }
        offset += count;
        *filled_to = next;
        if next == block_end {
            let block = Decoded::Block {
                job: job.id,
                index: next / BLOCK_FRAMES - 1,
                samples: Arc::new(std::mem::replace(filling, new_block(ch))),
            };
            if decoded_tx.send(block).is_err() {
                return false;
            }
        }
    }
    true
}

#[cfg(test)]
#[path = "tests/mix_buffers.rs"]
mod tests;
