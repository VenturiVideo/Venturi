//! Voiceover: while the timeline plays at 1x, the microphone is recorded
//! onto the armed tracks, over whatever is there.

use std::path::{Path, PathBuf};

use vv_audio::recorder::{Recorder, Take};
use vv_core::{Clip, ClipSource, CommandLabel, FrameIdx, Rational, TimelineId, TrackKind};
use vv_media::audio_file::{AudioFileFormat, write_audio_file};

use crate::VenturiApp;
use crate::timeline_ui::{RECORDING_PEAKS_PER_SEC, RecordingView};

/// Folder of the takes, next to the project file.
pub(crate) const RECORDINGS_DIR: &str = "Recordings";

pub(crate) struct ActiveTake {
    recorder: Recorder,
    timeline: TimelineId,
    start: FrameIdx,
    tracks: Vec<usize>,
    /// For the live waveform, see `extend_peaks`.
    peaks: Vec<f32>,
    peaked_frames: usize,
}

/// Adds to `peaks` one peak (over every channel) per full block of
/// `1 / RECORDING_PEAKS_PER_SEC` s of `recorded` past `peaked_frames`.
pub(crate) fn extend_peaks(peaks: &mut Vec<f32>, peaked_frames: &mut usize, recorded: &Take) {
    let ch = recorded.channels.max(1) as usize;
    let block = (recorded.sample_rate as f64 / RECORDING_PEAKS_PER_SEC).max(1.0) as usize;
    while *peaked_frames + block <= recorded.frames() {
        let samples = &recorded.samples[*peaked_frames * ch..(*peaked_frames + block) * ch];
        peaks.push(samples.iter().fold(0.0f32, |p, s| p.max(s.abs())));
        *peaked_frames += block;
    }
}

/// The first `Voiceover NNN` not in `dir` yet, in any format: a take
/// number is never reused when the format changes.
pub(crate) fn next_take_path(dir: &Path, format: AudioFileFormat) -> PathBuf {
    (1..)
        .map(|n| format!("Voiceover {n:03}"))
        .find(|name| {
            AudioFileFormat::ALL
                .iter()
                .all(|f| !dir.join(format!("{name}.{}", f.extension())).exists())
        })
        .map(|name| dir.join(format!("{name}.{}", format.extension())))
        .expect("an unbounded range")
}

/// Past this distance between the playhead and the recorded time the
/// playback jumped (a seek, a scrub): the take ends there.
const JUMP_SECS: f64 = 0.5;

impl VenturiApp {
    /// The armed tracks of the open timeline a take can land on.
    fn armed_tracks(&self) -> Vec<usize> {
        let Some(timeline_id) = self.timeline_id else {
            return Vec::new();
        };
        self.session.project.timelines[timeline_id]
            .tracks_of_kind(TrackKind::Audio)
            .filter(|(_, t)| t.armed && !t.locked)
            .map(|(i, _)| i)
            .collect()
    }

    /// The folder chosen in the settings, or `Recordings` next to the
    /// project file once it has one.
    fn recordings_dir(&self) -> Option<PathBuf> {
        if let Some(dir) = &self.settings.recording_dir {
            return Some(dir.clone());
        }
        Some(self.session.path()?.parent()?.join(RECORDINGS_DIR))
    }

    /// Before playback starts: `false` if it must not, because armed
    /// tracks are waiting for a project folder to record into.
    pub(crate) fn prepare_take(&mut self) -> bool {
        if self.armed_tracks().is_empty() {
            return true;
        }
        if self.recordings_dir().is_none() {
            self.timeline_state.record_needs_save = true;
            return false;
        }
        true
    }

    /// Just before playback starts at 1x from `start`.
    pub(crate) fn start_take(&mut self, start: FrameIdx) {
        let tracks = self.armed_tracks();
        let Some(timeline) = self.timeline_id.filter(|_| !tracks.is_empty()) else {
            return;
        };
        match Recorder::start(self.settings.input_device.as_deref()) {
            Ok(recorder) => {
                self.take = Some(ActiveTake {
                    recorder,
                    timeline,
                    start,
                    tracks,
                    peaks: Vec::new(),
                    peaked_frames: 0,
                });
            }
            Err(e) => {
                self.project_error = Some(t!("voiceover.no_input", error = e).into_owned());
            }
        }
    }

    /// Every frame: collects the microphone and ends the take once the
    /// playback stopped, left 1x or jumped.
    pub(crate) fn drive_take(&mut self) {
        self.timeline_state.can_record = self.recordings_dir().is_some();
        let playing = self.is_timeline_playing();
        let Some(take) = &mut self.take else {
            self.timeline_state.recording = None;
            return;
        };
        let fps = self.session.project.timelines[take.timeline].fps.as_f64();
        let recorded = take.recorder.poll();
        extend_peaks(&mut take.peaks, &mut take.peaked_frames, recorded);
        let recorded = recorded.seconds();
        let expected = take.start as f64 + recorded * fps;
        let jumped = (self.timeline_state.playhead as f64 - expected).abs() > JUMP_SECS * fps;
        let still = playing
            && self.playback_speed == 1.0
            && self.timeline_id == Some(take.timeline)
            && !jumped;
        if still {
            self.timeline_state.recording = Some(RecordingView {
                start: take.start,
                tracks: take.tracks.clone(),
                peaks: take.peaks.clone(),
            });
        } else {
            self.finish_take();
        }
    }

    fn finish_take(&mut self) {
        self.timeline_state.recording = None;
        let Some(take) = self.take.take() else {
            return;
        };
        let recorded = take.recorder.finish();
        if recorded.frames() == 0 {
            return;
        }
        if let Err(e) = self.place_take(take.timeline, take.start, &take.tracks, &recorded) {
            self.project_error = Some(t!("voiceover.save_failed", error = e).into_owned());
        }
    }

    /// Writes `recorded` into the project folder and puts it on `tracks`
    /// from `start`, overwriting what it meets, as one history step.
    fn place_take(
        &mut self,
        timeline: TimelineId,
        start: FrameIdx,
        tracks: &[usize],
        recorded: &Take,
    ) -> Result<(), String> {
        let dir = self.recordings_dir().ok_or("the project has no folder")?;
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        let format = AudioFileFormat::resolve(self.settings.recording_format);
        let path = next_take_path(&dir, format);
        write_audio_file(
            &path,
            format,
            &recorded.samples,
            recorded.sample_rate,
            recorded.channels,
        )
        .map_err(|e| e.to_string())?;
        let mut meta = vv_media::probe_media(&path).map_err(|e| e.to_string())?;
        // Ignore codec padding but retain the media tail beyond the rounded clip.
        meta.duration_frames = ((recorded.seconds() * meta.fps.as_f64()).ceil() as FrameIdx).max(1);

        let group = self.session.history.begin_group();
        let media_id = self.session.add_media(path, meta.clone());
        let project = &mut self.session.project;
        let rate = Rational::conform_rate(project.timelines[timeline].fps, meta.fps);
        let take_len = ((recorded.seconds() * project.timelines[timeline].fps.as_f64()).round()
            as FrameIdx)
            .max(1);
        let clips = tracks
            .iter()
            .map(|&track| {
                let clip = Clip::new(
                    project.alloc_clip_id(),
                    ClipSource::Media(media_id),
                    0,
                    start,
                    take_len,
                    rate,
                    Rational::one(),
                );
                (track, clip, None::<u64>)
            })
            .collect();
        vv_core::edit::insert_clips(
            project,
            &mut self.session.history,
            timeline,
            clips,
            CommandLabel::RecordVoiceover,
        );
        // One take per arming: the next playback must not record over it.
        for &track in tracks {
            self.session.history.do_command(
                &mut self.session.project,
                Box::new(vv_core::SetTrackFlag::new(
                    timeline,
                    track,
                    vv_core::TrackFlag::Armed,
                    false,
                )),
            );
        }
        self.session
            .history
            .end_group_as(group, CommandLabel::RecordVoiceover);
        self.enqueue_media_background_jobs(media_id);
        Ok(())
    }

    /// Why recording cannot start yet, with a way out.
    pub(crate) fn show_record_needs_save(&mut self, ctx: &egui::Context) {
        if !self.timeline_state.record_needs_save {
            return;
        }
        let (mut save, mut close) = (false, false);
        egui::Window::new(t!("voiceover.needs_save_title"))
            .id(egui::Id::new("record_needs_save"))
            .collapsible(false)
            .resizable(false)
            .default_width(380.0)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                ui.label(t!("voiceover.needs_save", dir = RECORDINGS_DIR));
                ui.separator();
                ui.horizontal(|ui| {
                    save = ui.button(t!("voiceover.save_project")).clicked();
                    close = ui.button(t!("common.cancel")).clicked();
                });
            });
        if save {
            self.save_project();
        }
        if save || close {
            self.timeline_state.record_needs_save = false;
        }
    }
}

#[cfg(test)]
#[path = "tests/voiceover.rs"]
mod tests;
