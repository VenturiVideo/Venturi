//! Audio and playback clock of the timeline: the mixer plays all the audio
//! tracks and its position is the playhead. In fast forward it plays windows
//! of the mix already stretched in the background. Without an audio device
//! the clock is wall time and nothing plays.

use std::path::{Path, PathBuf};
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};

use vv_audio::mixer::{
    Channel, ClipInput, MixClip, MixMeters, MixSnapshot, Mixer, MixerState, PROJECT_SAMPLE_RATE,
    StretchedWindow, mix_range, sample_to_timeline_frame, timeline_frame_to_sample,
};
use vv_core::{FrameIdx, Keyframed, Project, TimelineId};

use crate::mix_buffers::MixBufferCache;

const SCRUB_SNIPPET: Duration = Duration::from_millis(80);

/// Duration (timeline audio frames) of each stretched window: stretching
/// everything in one go would delay the first sound by seconds, 8s come back
/// in ~150-200ms. A multiple of every tempo (2/4/8) so the stretched window
/// has an exact length and the queued windows do not drift.
const WINDOW_FRAMES: u64 = 8 * PROJECT_SAMPLE_RATE as u64;

/// A new window starts this short, and the next ones double up to
/// `WINDOW_FRAMES`: after a jump the sound is back in ~80ms instead of the
/// ~350ms of a whole window, and each extension still lands before the
/// previous one has played, at 8x too.
const FIRST_WINDOW_FRAMES: u64 = 2 * PROJECT_SAMPLE_RATE as u64;

/// Unplayed audio below which the next window is queued: at 8x it is 0.5s
/// real time against ~200ms of stretching.
const EXTEND_MARGIN_FRAMES: u64 = 4 * PROJECT_SAMPLE_RATE as u64;

#[derive(Clone, Copy)]
struct StretchRequest {
    id: u64,
    tempo: u64,
    start: u64,
    frames: u64,
    /// `false`: extends the current window instead of replacing it.
    new_window: bool,
    /// All the audio it is stretched from was decoded (`MixSnapshot::is_ready`).
    complete: bool,
}

struct StretchResult {
    id: u64,
    samples: Result<Vec<f32>, String>,
}

pub struct TimelineAudio {
    mixer: Option<Mixer>,
    buffers: MixBufferCache,
    mix: Arc<MixSnapshot>,
    stretched: Option<StretchedWindow>,
    /// Every chunk of `stretched` was stretched from complete audio.
    window_complete: bool,
    playing: bool,
    /// Effective speed: a fast forward request applies only when its first
    /// window is ready.
    speed: f64,
    wall_base_sample: u64,
    wall_started_at: Option<Instant>,
    /// End of the scrub fragment and position to restore.
    scrub_snippet: Option<(Instant, u64)>,
    synced: Option<(TimelineId, u64)>,
    /// Media of the media pool preview currently in the snapshot.
    synced_media: Option<PathBuf>,
    /// At most one stretch in flight; a result with another id is stale.
    stretch_request: Option<StretchRequest>,
    /// The tempo of a new window wanted from where the playhead will be,
    /// once the one in flight lands or playback resumes: a drag would start
    /// one stretch per frame.
    restretch: Option<u64>,
    /// The last stretch failed: not retried every frame, only after a
    /// change of content, position or speed.
    stretch_failed: bool,
    next_request_id: u64,
    stretch_tx: mpsc::Sender<StretchResult>,
    stretch_rx: mpsc::Receiver<StretchResult>,
}

impl TimelineAudio {
    pub fn new() -> Self {
        let mixer = Mixer::new()
            .map_err(|e| eprintln!("vv-app: audio mixer unavailable: {e}"))
            .ok();
        let channels = mixer.as_ref().map_or(2, Mixer::channels);
        let (stretch_tx, stretch_rx) = mpsc::channel();
        Self {
            mixer,
            buffers: MixBufferCache::spawn(PROJECT_SAMPLE_RATE, channels),
            mix: Arc::new(MixSnapshot::empty(PROJECT_SAMPLE_RATE, channels)),
            stretched: None,
            window_complete: true,
            playing: false,
            speed: 1.0,
            wall_base_sample: 0,
            wall_started_at: None,
            scrub_snippet: None,
            synced: None,
            synced_media: None,
            stretch_request: None,
            restretch: None,
            stretch_failed: false,
            next_request_id: 0,
            stretch_tx,
            stretch_rx,
        }
    }

    pub fn set_budget_bytes(&mut self, bytes: usize) {
        self.buffers.set_budget_bytes(bytes);
    }

    fn channels(&self) -> u16 {
        self.mix.channels
    }

    /// To be called every frame: rebuilds the mix snapshot if the project
    /// changed or new decoded buffers arrived.
    pub fn sync(&mut self, project: &Project, timeline_id: TimelineId, generation: u64) {
        self.buffers.set_playhead(self.position_sample());
        let buffers_arrived = self.buffers.poll();
        self.buffers.send_jobs(project);
        let content_changed = self.synced != Some((timeline_id, generation));
        if !buffers_arrived && !content_changed {
            return;
        }
        self.synced = Some((timeline_id, generation));
        self.synced_media = None;
        if self.mixer.is_none() {
            return;
        }
        let Some(timeline) = project.timelines.get(timeline_id) else {
            return;
        };
        let (rate, channels) = (self.mix.sample_rate, self.mix.channels);
        self.buffers.begin_pass();
        let mut mix =
            MixSnapshot::from_timeline(project, timeline, rate, channels, &mut self.buffers);
        mix.meters.keep_spectra_of(&self.mix.meters);
        self.mix = Arc::new(mix);
        self.buffers.sweep_unused();
        self.buffers.send_jobs(project);
        self.refresh_window(content_changed);
        self.publish();
    }

    /// Like `sync`, but for the preview of a media: plays all its
    /// `audio_streams` from the start of the file, without a timeline.
    pub fn sync_media(&mut self, path: &Path, audio_streams: usize, fps: f64, duration_secs: f64) {
        self.buffers.set_playhead(self.position_sample());
        let buffers_arrived = self.buffers.poll();
        let content_changed = self.synced_media.as_deref() != Some(path);
        if !buffers_arrived && !content_changed {
            return;
        }
        self.synced_media = Some(path.to_path_buf());
        self.synced = None;
        if self.mixer.is_none() {
            return;
        }
        let rate = self.mix.sample_rate;
        let channels = self.mix.channels;
        let frames = 0..(duration_secs.max(0.0) * rate as f64).ceil() as u64;
        self.buffers.begin_media_pass();
        let buffers: Vec<_> = (0..audio_streams)
            .filter_map(|stream| self.buffers.media_frames(path, stream, frames.clone()))
            .collect();
        self.buffers.sweep_unused();
        let clips: Vec<MixClip> = buffers
            .into_iter()
            .map(|buffer| MixClip {
                clip: vv_core::ClipId(0),
                start: 0,
                len: buffer.end(),
                source_offset: 0,
                step: 1.0,
                input: ClipInput::Blocks(buffer),
                gain_db: Keyframed::constant(0.0),
                track: 0,
                clip_fps: fps,
                media_offset: 0,
                media_step: 1.0,
                fade_in: 0,
                fade_out: 0,
            })
            .collect();
        self.mix = Arc::new(MixSnapshot::new(
            rate,
            channels,
            clips,
            Vec::new(),
            Channel::UNITY,
        ));
        self.refresh_window(content_changed);
        self.publish();
    }

    /// After a new snapshot, the fast forward stretched from the old one may
    /// be stale. Another timeline or an edit throws it away at once: it
    /// would play the old audio. Audio that only arrived replaces a window
    /// stretched while some of it was missing, once the snapshot has all of
    /// it; meanwhile that window keeps playing.
    fn refresh_window(&mut self, content_changed: bool) {
        let tempo = match (&self.stretched, self.stretch_request, self.restretch) {
            (_, Some(request), _) if request.new_window => request.tempo,
            (_, _, Some(tempo)) => tempo,
            (Some(window), _, _) => window.tempo,
            (None, _, None) => return,
        };
        let pos = self.position_sample();
        if content_changed {
            self.stretch_failed = false;
            if let Some(window) = &mut self.stretched {
                window.origin = pos;
                window.chunks.clear();
            }
            self.want_window(tempo, pos);
            return;
        }
        if !self.playing || self.stretch_request.is_some() || self.stretch_failed {
            return;
        }
        if self.restretch.is_some()
            || !self.window_complete && self.mix.is_ready(pos..pos + FIRST_WINDOW_FRAMES)
        {
            self.request_window(tempo, pos, true);
        }
    }

    /// A new window from `start`, or from where the playhead is once the
    /// new window in flight lands or playback resumes. An extension in
    /// flight is not waited for: the window it extends is being replaced.
    fn want_window(&mut self, tempo: u64, start: u64) {
        if self.playing && self.stretch_request.is_none_or(|r| !r.new_window) {
            self.request_window(tempo, start, true);
        } else {
            self.restretch = Some(tempo);
        }
    }

    fn publish(&mut self) {
        if let Some(mixer) = &mut self.mixer {
            mixer.set_state(Arc::new(MixerState::new(
                self.mix.clone(),
                self.stretched.clone(),
            )));
        }
    }

    /// The mix the callback would play from `frame` at 1x, for the tests.
    #[cfg(test)]
    pub fn render(&self, frame: FrameIdx, fps: f64, len_frames: FrameIdx) -> Vec<f32> {
        let rate = self.mix.sample_rate;
        let start = timeline_frame_to_sample(frame, fps, rate);
        let end = timeline_frame_to_sample(frame + len_frames, fps, rate);
        let mut out = vec![0.0; (end - start) as usize * self.channels() as usize];
        mix_range(&self.mix, start, &mut out);
        out
    }

    /// Peak of the loaded stretched audio, for the tests.
    #[cfg(test)]
    pub fn stretched_peak(&self) -> Option<f32> {
        let window = self.stretched.as_ref()?;
        let peak = window.chunks.iter().flat_map(|c| c.iter());
        Some(peak.fold(0.0f32, |m, s| m.max(s.abs())))
    }

    #[cfg(test)]
    pub fn has_pending_buffers(&mut self) -> bool {
        self.buffers.has_pending()
    }

    /// See `MixBufferCache::next_missing_retry`.
    pub fn next_missing_retry(&self) -> Option<Instant> {
        self.buffers.next_missing_retry()
    }

    /// Forces a snapshot rebuild on the next `sync` (e.g. project replaced
    /// without going through the history).
    pub fn invalidate(&mut self) {
        self.synced = None;
    }

    pub fn is_playing(&self) -> bool {
        self.playing
    }

    pub fn play(&mut self) {
        self.stop_scrub_snippet();
        if self.playing {
            return;
        }
        self.playing = true;
        match &self.mixer {
            Some(mixer) => mixer.play(),
            None => self.wall_started_at = Some(Instant::now()),
        }
    }

    pub fn pause(&mut self) {
        self.stop_scrub_snippet();
        if !self.playing {
            return;
        }
        let pos = self.position_sample();
        self.playing = false;
        if let Some(mixer) = &self.mixer {
            mixer.pause();
        }
        self.seek_sample(pos);
    }

    fn seek_sample(&mut self, sample: u64) {
        if let Some((stop_at, _)) = self.scrub_snippet {
            self.scrub_snippet = Some((stop_at, sample));
        }
        if let Some(mixer) = &self.mixer {
            mixer.seek(sample);
        }
        self.wall_base_sample = sample;
        self.wall_started_at = self.playing.then(Instant::now);
        self.stretch_failed = false;

        let channels = self.channels();
        // A click seeks a few times to the same place: the window already
        // wanted from there must not be thrown away and stretched again.
        if let Some(window) = &self.stretched
            && !(window.origin..window.covered_until(channels)).contains(&sample)
            && !(window.chunks.is_empty() && window.origin == sample)
        {
            // Outside the window: restart from there. While waiting the clock
            // advances silently at the same speed, without reopening the stream.
            let tempo = match self.stretch_request {
                Some(req) if req.new_window => req.tempo,
                _ => window.tempo,
            };
            self.stretched = Some(StretchedWindow {
                tempo,
                origin: sample,
                chunks: Vec::new(),
            });
            self.speed = tempo as f64;
            self.publish();
            self.want_window(tempo, sample);
        }
    }

    fn position_sample(&self) -> u64 {
        if let Some((_, restore)) = self.scrub_snippet {
            return restore;
        }
        if let Some(mixer) = &self.mixer {
            return mixer.position();
        }
        let elapsed = self
            .wall_started_at
            .map_or(0.0, |t| t.elapsed().as_secs_f64());
        self.wall_base_sample + (elapsed * self.speed * PROJECT_SAMPLE_RATE as f64) as u64
    }

    pub fn seek_frame(&mut self, frame: FrameIdx, fps: f64) {
        self.seek_sample(timeline_frame_to_sample(
            frame.max(0),
            fps,
            PROJECT_SAMPLE_RATE,
        ));
    }

    pub fn position_frame(&self, fps: f64) -> FrameIdx {
        sample_to_timeline_frame(self.position_sample(), fps, PROJECT_SAMPLE_RATE)
    }

    pub fn speed(&self) -> f64 {
        self.speed
    }

    /// 1x applies immediately; 2x/4x/8x when the first stretched window is
    /// ready (meanwhile playback continues at the current speed).
    pub fn request_speed(&mut self, speed: f64) {
        let tempo = speed.round().max(1.0) as u64;
        self.stretch_failed = false;
        if self.mixer.is_none() {
            let pos = self.position_sample();
            self.speed = tempo as f64;
            self.seek_sample(pos);
            return;
        }
        if tempo == 1 {
            self.stretch_request = None;
            self.restretch = None;
            self.speed = 1.0;
            if self.stretched.take().is_some() {
                self.publish();
            }
            return;
        }
        let already_requested = match self.stretch_request {
            Some(req) if req.new_window => req.tempo == tempo,
            _ => self.stretched.as_ref().is_some_and(|w| w.tempo == tempo),
        };
        if !already_requested {
            let pos = self.position_sample();
            self.request_window(tempo, pos, true);
        }
    }

    fn request_window(&mut self, tempo: u64, start: u64, new_window: bool) {
        let channels = self.channels().max(1) as usize;
        let last_chunk = self
            .stretched
            .as_ref()
            .filter(|_| !new_window)
            .and_then(|w| {
                w.chunks
                    .last()
                    .map(|c| (c.len() / channels) as u64 * w.tempo)
            });
        let frames = last_chunk.map_or(FIRST_WINDOW_FRAMES, |last| {
            (last * 2).clamp(FIRST_WINDOW_FRAMES, WINDOW_FRAMES)
        });
        let request = StretchRequest {
            id: self.next_request_id,
            tempo,
            start,
            frames,
            new_window,
            complete: self.mix.is_ready(start..start + frames),
        };
        self.next_request_id += 1;
        self.stretch_request = Some(request);
        if new_window {
            self.restretch = None;
        }

        let mix = self.mix.clone();
        let tx = self.stretch_tx.clone();
        std::thread::spawn(move || {
            let ch = mix.channels as usize;
            let mut samples = vec![0.0; frames as usize * ch];
            mix_range(&mix, start, &mut samples);
            let expected = (frames / tempo) as usize * ch;
            let samples =
                vv_audio::stretch_samples(&samples, mix.sample_rate, mix.channels, tempo as f64)
                    .map(|mut stretched| {
                        stretched.resize(expected, 0.0);
                        stretched
                    });
            let _ = tx.send(StretchResult {
                id: request.id,
                samples,
            });
        });
    }

    fn apply_stretch_results(&mut self) {
        while let Ok(result) = self.stretch_rx.try_recv() {
            let Some(request) = self.stretch_request.filter(|r| r.id == result.id) else {
                continue;
            };
            self.stretch_request = None;
            if let Some(tempo) = self.restretch {
                // Stretched from what changed since.
                self.want_window(tempo, self.position_sample());
                continue;
            }
            let samples = match result.samples {
                Ok(samples) => Arc::new(samples),
                Err(e) => {
                    eprintln!("vv-app: audio stretch at {}x failed: {e}", request.tempo);
                    self.stretch_failed = true;
                    continue;
                }
            };
            let channels = self.channels();
            if request.new_window {
                let pos = self.position_sample();
                // While stretching, the playhead moved past it, or was moved.
                if !(request.start..request.start + request.frames).contains(&pos) {
                    self.want_window(request.tempo, pos);
                    continue;
                }
                self.stretched = Some(StretchedWindow {
                    tempo: request.tempo,
                    origin: request.start,
                    chunks: vec![samples],
                });
                self.window_complete = request.complete;
                self.speed = request.tempo as f64;
            } else {
                let Some(window) = &mut self.stretched else {
                    continue;
                };
                if window.tempo != request.tempo || window.covered_until(channels) != request.start
                {
                    continue;
                }
                window.chunks.push(samples);
                self.window_complete &= request.complete;
                self.drop_played_chunks();
            }
            self.publish();
        }
    }

    /// Discards the windows already played: in a long fast forward they
    /// would pile up.
    fn drop_played_chunks(&mut self) {
        let channels = self.channels().max(1) as usize;
        let pos = self.position_sample();
        let Some(window) = &mut self.stretched else {
            return;
        };
        while window.chunks.len() > 1 {
            let end = window.origin + (window.chunks[0].len() / channels) as u64 * window.tempo;
            if end > pos {
                break;
            }
            window.chunks.remove(0);
            window.origin = end;
        }
    }

    fn extend_window_if_needed(&mut self) {
        if !self.playing || self.stretch_request.is_some() || self.stretch_failed {
            return;
        }
        let Some(window) = &self.stretched else {
            return;
        };
        let covered_until = window.covered_until(self.channels());
        if covered_until.saturating_sub(self.position_sample()) < EXTEND_MARGIN_FRAMES {
            self.request_window(window.tempo, covered_until, false);
        }
    }

    /// Short fragment from the current position without entering playback.
    /// No-op if already playing or without a device.
    pub fn play_scrub_snippet(&mut self) {
        if self.playing {
            return;
        }
        let Some(mixer) = &self.mixer else {
            return;
        };
        let pos = self.position_sample();
        self.scrub_snippet = None;
        mixer.seek(pos);
        mixer.play();
        self.scrub_snippet = Some((Instant::now() + SCRUB_SNIPPET, pos));
    }

    pub fn is_scrub_snippet_active(&self) -> bool {
        self.scrub_snippet.is_some()
    }

    fn stop_scrub_snippet(&mut self) {
        if let Some((_, pos)) = self.scrub_snippet.take()
            && let Some(mixer) = &self.mixer
        {
            mixer.pause();
            mixer.seek(pos);
        }
    }

    pub fn tick(&mut self) {
        if self
            .scrub_snippet
            .is_some_and(|(stop_at, _)| Instant::now() >= stop_at)
        {
            self.stop_scrub_snippet();
        }
        self.apply_stretch_results();
        // A window may land incomplete after the snapshot that completes it.
        self.refresh_window(false);
        self.extend_window_if_needed();
    }

    pub fn peak_linear_stereo(&self) -> (f32, f32) {
        self.mixer
            .as_ref()
            .map_or((0.0, 0.0), Mixer::peak_linear_stereo)
    }

    /// Per-track and master peaks of what is playing.
    pub fn meters(&self) -> &MixMeters {
        &self.mix.meters
    }
}

#[cfg(test)]
#[path = "tests/timeline_audio.rs"]
mod tests;
