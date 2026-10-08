//! Media import, file dialogs, saving/opening the project, OTIO,
//! export and relink: the UI over the `vv_session::Session` jobs.

use super::*;
use std::sync::Arc;

/// Linux dialog filters are case-sensitive globs, so `mp4` becomes `[mM][pP]4`
/// to show `.MP4` (GoPro). macOS and Windows are already case-insensitive and
/// do not take globs here.
fn case_insensitive(exts: &[&str]) -> Vec<String> {
    if !cfg!(target_os = "linux") {
        return exts.iter().map(ToString::to_string).collect();
    }
    exts.iter()
        .map(|e| {
            e.chars()
                .map(|c| match c.is_ascii_alphabetic() {
                    true => format!("[{}{}]", c.to_ascii_lowercase(), c.to_ascii_uppercase()),
                    false => c.to_string(),
                })
                .collect()
        })
        .collect()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ProjectSwitch {
    New,
    Open,
    OpenRecent(PathBuf),
    Quit,
}

pub(crate) enum UnsavedChoice {
    Save,
    Discard,
    Cancel,
}

/// What to do with the result of a background file dialog. `RelinkMedia`
/// carries the pool selection as it was when it opened.
pub(crate) enum DialogKind {
    ImportMedia,
    SaveProjectAs,
    ExportOtio(TimelineId),
    ImportOtio,
    OpenProject,
    RelinkMedia(Vec<MediaId>),
    RecordingFolder,
}

pub(crate) enum DialogOutcome {
    File(Option<PathBuf>),
    Files(Option<Vec<PathBuf>>),
}

pub(crate) struct PendingDialog {
    pub(crate) kind: DialogKind,
    pub(crate) rx: mpsc::Receiver<DialogOutcome>,
}

/// The export window: open from the start of an export until closed, so the
/// outcome stays readable after the job is over.
pub(crate) struct ExportUiState {
    pub(crate) job: vv_session::JobId,
    pub(crate) progress: Arc<Mutex<export::ExportProgress>>,
}

fn export_error_message(e: &export::ExportError) -> String {
    match e {
        export::ExportError::TimelineNotFound => t!("export.error_timeline_not_found").into_owned(),
        export::ExportError::MediaNotFound => t!("export.error_media_not_found").into_owned(),
        export::ExportError::Cancelled => t!("export.cancelled").into_owned(),
        export::ExportError::Failed(e) => e.clone(),
    }
}

impl VenturiApp {
    pub(crate) fn import_media(&mut self, path: PathBuf) {
        if let Some(existing) = self.session.media_with_path(&path) {
            self.import_warnings.clear();
            self.preview_media(existing);
            self.media_pool_state.select_only([existing]);
            return;
        }
        match self.add_media_to_pool(path) {
            Ok(media_id) => {
                self.import_warnings.clear();
                self.preview_media(media_id);
                self.media_pool_state.select_only([media_id]);
            }
            Err(e) => self.import_warnings = vec![e],
        }
    }

    /// Multiple import: probing the files goes to worker threads (tens of
    /// ms each), the UI stays alive and shows the progress.
    pub(crate) fn import_media_files(&mut self, paths: Vec<PathBuf>) {
        if !paths.is_empty() {
            self.session.import_media(paths);
        }
    }

    /// Collects the outcome of the session's background jobs.
    pub(crate) fn poll_session(&mut self, ctx: &egui::Context) {
        self.process_session_events();
        // Without a new event egui would not redraw: the progress bars
        // would stay frozen.
        if self.session.has_running_jobs() {
            ctx.request_repaint_after(std::time::Duration::from_millis(50));
        }
    }

    fn process_session_events(&mut self) {
        for event in self.session.tick() {
            self.mcp_session_event(&event);
            self.handle_session_event(event);
        }
    }

    pub(crate) fn handle_session_event(&mut self, event: vv_session::SessionEvent) {
        use vv_session::{RelinkEnd, SessionEvent as E};
        match event {
            // The agent gets the outcome of its own jobs; the user's view and
            // warnings stay as they are.
            E::ImportStarted { job } if !self.is_agent_job(job) => self.import_warnings.clear(),
            E::ImportStarted { .. } => {}
            E::MediaAdded { job, media_id } => {
                if !self.is_agent_job(job) {
                    let meta = self.session.project.media_pool[media_id].meta.clone();
                    if meta.has_video {
                        self.ensure_timeline_for(&meta, Some(job));
                    } else {
                        self.ensure_timeline_audio_only(Some(job));
                    }
                }
                self.enqueue_media_background_jobs(media_id);
            }
            E::ImportFinished {
                job,
                imported,
                errors,
            } => {
                // Everything was already in the pool: nothing started.
                if (imported.is_empty() && errors.is_empty()) || self.is_agent_job(job) {
                    return;
                }
                self.import_warnings = errors;
                if let Some(&last) = imported.last() {
                    self.preview_media(last);
                }
                if !imported.is_empty() {
                    self.media_pool_state.select_only(imported);
                }
            }
            E::OtioNeedsDecision { .. } | E::OtioCancelled { .. } => {}
            E::OtioImported { job, result } => {
                let by_agent = self.is_agent_job(job);
                self.apply_otio_merged(result, by_agent)
            }
            E::OtioFailed { job, .. } if self.is_agent_job(job) => {}
            E::OtioFailed { error, .. } => {
                self.project_error =
                    Some(t!("project.otio_import_failed", error = error).into_owned())
            }
            E::RelinkFinished { end, .. } => match end {
                RelinkEnd::Done {
                    relinked,
                    not_found,
                } => self.finish_relink(relinked, not_found),
                RelinkEnd::Cancelled => {}
                RelinkEnd::Failed => {
                    self.relink_message = Some(t!("project.relink_none").into_owned())
                }
            },
            E::ExportFinished { .. } => self.resume_proxies_after_export(),
        }
    }

    /// Blocks until the imports in progress are finished: the tests
    /// have no event loop calling `poll_session`.
    #[cfg(test)]
    pub(crate) fn wait_for_import(&mut self) {
        let ctx = egui::Context::default();
        while self.session.is_importing() {
            self.poll_session(&ctx);
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }

    pub(crate) fn import_progress(&self) -> Option<(usize, usize)> {
        self.session.import_progress()
    }

    pub(crate) fn add_media_to_pool(&mut self, path: PathBuf) -> Result<MediaId, String> {
        match vv_media::probe_media(&path) {
            Ok(meta) => Ok(self.insert_media(path, meta)),
            Err(e) => Err(e.to_string()),
        }
    }

    fn insert_media(&mut self, path: PathBuf, meta: vv_core::MediaMeta) -> MediaId {
        let group = self.session.history.begin_group();
        if meta.has_video {
            self.ensure_timeline_for(&meta, None);
        } else {
            self.ensure_timeline_audio_only(None);
        }
        let media_id = self.session.add_media(path, meta);
        self.session
            .history
            .end_group_as(group, vv_core::CommandLabel::ImportMedia);
        self.enqueue_media_background_jobs(media_id);
        media_id
    }

    /// Proxy, thumbnail and waveform of a media in the pool, whether just
    /// imported or from an opened project: what is already in the on-disk
    /// cache is skipped by the workers.
    pub(crate) fn enqueue_media_background_jobs(&mut self, media_id: MediaId) {
        let Some(item) = self.session.project.media_pool.get(media_id) else {
            return;
        };
        // A compound clip has no file on disk to proxy/thumbnail/
        // analyze: its content is `item.compound`, not `item.path`.
        if item.compound.is_some() {
            return;
        }
        // An image has nothing to gain from a proxy, and its duration is
        // the `IMAGE_DURATION_FRAMES` sentinel.
        if self.settings.proxy_enabled && item.meta.has_video && !item.meta.is_image() {
            let frames = item.meta.duration_frames.max(0) as u64;
            let quality = self.settings.proxy_quality;
            self.proxy_worker
                .get_or_insert_with(|| proxy_worker::ProxyWorker::spawn(quality))
                .enqueue(item.path.clone(), item.content_hash, frames);
        }
        if item.meta.has_video && !self.thumbnails.contains_key(&item.content_hash) {
            self.thumbnails.insert(item.content_hash, None);
            self.thumbnail_worker
                .get_or_insert_with(thumbnail_worker::ThumbnailWorker::spawn)
                .enqueue(
                    item.path.clone(),
                    item.content_hash,
                    item.meta.duration_frames as f64 / item.meta.fps.as_f64(),
                );
        }
        // Only media with audio: the timeline draws the waveform only
        // on audio clips.
        if item.meta.has_audio {
            let secs = item.meta.duration_frames as f64 / item.meta.fps.as_f64();
            let num_peaks = vv_media::recommended_num_peaks(secs);
            self.waveform_worker
                .get_or_insert_with(waveform_worker::WaveformWorker::spawn)
                .enqueue(
                    item.path.clone(),
                    item.content_hash,
                    item.meta.audio_stream_count(),
                    num_peaks,
                );
        }
    }

    pub(crate) fn poll_thumbnails(&mut self, ctx: &egui::Context) {
        let Some(worker) = &mut self.thumbnail_worker else {
            return;
        };
        for (content_hash, thumb) in worker.drain() {
            let texture = thumb.map(|t| {
                ctx.load_texture(
                    format!("thumbnail-{content_hash:016x}"),
                    egui::ColorImage::from_rgba_unmultiplied(
                        [t.width as usize, t.height as usize],
                        &t.rgba,
                    ),
                    egui::TextureOptions::LINEAR,
                )
            });
            self.thumbnails.insert(content_hash, texture);
        }
    }

    /// Opens the native file dialog on a separate thread: on the
    /// GNOME/Wayland event loop thread it marks the app as unresponsive. One at
    /// a time.
    pub(crate) fn spawn_dialog(
        &mut self,
        kind: DialogKind,
        run: impl FnOnce(rfd::FileDialog) -> DialogOutcome + Send + 'static,
    ) {
        if self.pending_dialog.is_some() {
            return;
        }
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(run(rfd::FileDialog::new()));
        });
        self.pending_dialog = Some(PendingDialog { kind, rx });
    }

    pub(crate) fn spawn_file_dialog(
        &mut self,
        kind: DialogKind,
        build: impl FnOnce(rfd::FileDialog) -> Option<PathBuf> + Send + 'static,
    ) {
        self.spawn_dialog(kind, |dlg| DialogOutcome::File(build(dlg)));
    }

    /// Applies the result of the background dialog, if it arrived.
    pub(crate) fn poll_pending_dialog(&mut self, ctx: &egui::Context) {
        let Some(pending) = &self.pending_dialog else {
            return;
        };
        let Ok(outcome) = pending.rx.try_recv() else {
            // Still waiting: without a new event (mouse, keyboard)
            // egui would not redraw, so the result would arrive
            // only at the user's next input.
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
            return;
        };
        let Some(PendingDialog { kind, .. }) = self.pending_dialog.take() else {
            return;
        };
        match (kind, outcome) {
            (DialogKind::ImportMedia, DialogOutcome::Files(Some(paths))) => {
                self.import_media_files(paths);
            }
            (DialogKind::SaveProjectAs, DialogOutcome::File(Some(path))) => {
                self.save_project_to(&path);
            }
            (DialogKind::RecordingFolder, DialogOutcome::File(Some(path))) => {
                self.settings.recording_dir = Some(path);
                self.persist_settings();
            }
            (DialogKind::ExportOtio(timeline_id), DialogOutcome::File(Some(path))) => {
                self.export_otio_to(timeline_id, &path);
            }
            (DialogKind::ImportOtio, DialogOutcome::File(Some(path))) => {
                self.import_otio_from(&path);
            }
            (DialogKind::OpenProject, DialogOutcome::File(Some(path))) => {
                self.load_project_from(path);
            }
            (DialogKind::RelinkMedia(targets), DialogOutcome::File(Some(base_dir))) => {
                self.relink_media(&base_dir, &targets);
            }
            _ => {} // dialog cancelled by the user
        }
    }

    /// Files dropped by the file manager onto the window: they are imported into
    /// the pool, wherever they land.
    pub(crate) fn poll_dropped_files(&mut self, ctx: &egui::Context) {
        let paths: Vec<PathBuf> = ctx.input(|i| {
            i.raw
                .dropped_files
                .iter()
                .map(|f| f.path().to_path_buf())
                .collect()
        });
        if !paths.is_empty() {
            self.import_media_files(paths);
        }
    }

    // Opens the file dialog and imports the chosen files (used by the toolbar
    // button and by the Ctrl+I shortcut).
    pub(crate) fn import_media_dialog(&mut self) {
        const VIDEO: &[&str] = &["mp4", "mov", "mkv", "avi"];
        const AUDIO: &[&str] = &["wav", "mp3", "flac", "m4a", "aac", "ogg", "opus"];
        let media: Vec<&str> = [VIDEO, AUDIO, vv_media::IMAGE_EXTENSIONS].concat();
        self.spawn_dialog(DialogKind::ImportMedia, move |dlg| {
            DialogOutcome::Files(
                dlg.add_filter("media", &case_insensitive(&media))
                    .add_filter("video", &case_insensitive(VIDEO))
                    .add_filter("audio", &case_insensitive(AUDIO))
                    .add_filter(
                        t!("file_filter.images"),
                        &case_insensitive(vv_media::IMAGE_EXTENSIONS),
                    )
                    .pick_files(),
            )
        });
    }

    /// Saves to the current file (`Session::path`), or as a "save as"
    /// if the project has not been saved/opened yet.
    pub(crate) fn save_project(&mut self) {
        match self.session.path().map(Path::to_path_buf) {
            Some(path) => self.save_project_to(&path),
            None => self.save_project_as(),
        }
    }

    /// Always opens the save file dialog, even if the project already has
    /// a current file (used by the "Save as..." button and by
    /// Ctrl+Shift+S).
    pub(crate) fn save_project_as(&mut self) {
        self.spawn_file_dialog(DialogKind::SaveProjectAs, |dlg| {
            dlg.set_file_name(format!("{}.vvproj", t!("project.default_file_name")))
                .add_filter(t!("file_filter.project"), &["vvproj"])
                .save_file()
        });
    }

    pub(crate) fn save_project_to(&mut self, path: &Path) {
        match self.session.save_to(path) {
            Ok(()) => {
                self.project_error = None;
                self.remember_recent_project(path.to_path_buf());
            }
            Err(e) => self.project_error = Some(t!("project.save_failed", error = e).into_owned()),
        }
    }

    pub(crate) fn export_otio_dialog(&mut self) {
        let Some(timeline_id) = self.timeline_id else {
            return;
        };
        let file_name = format!("{}.otio", self.session.project.timelines[timeline_id].name);
        self.spawn_file_dialog(DialogKind::ExportOtio(timeline_id), move |dlg| {
            dlg.set_file_name(file_name)
                .add_filter("OpenTimelineIO", &["otio"])
                .save_file()
        });
    }

    pub(crate) fn export_otio_to(&mut self, timeline_id: TimelineId, path: &Path) {
        let measure = |title: &vv_core::TitleParams| vv_render::text::title_metrics(title);
        self.project_error =
            vv_core::export_otio(&self.session.project, timeline_id, path, Some(&measure))
                .err()
                .map(|e| t!("project.otio_export_failed", error = e).into_owned());
    }

    pub(crate) fn has_unsaved_changes(&self) -> bool {
        self.session.has_unsaved_changes()
    }

    /// Name of the current project, without extension.
    pub(crate) fn project_label(&self) -> String {
        self.session
            .path()
            .and_then(|p| p.file_stem())
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| t!("project.untitled").into_owned())
    }

    pub(crate) fn sync_window_title(&mut self, ctx: &egui::Context) {
        let title = format!(
            "{}{} — Venturi",
            self.project_label(),
            if self.has_unsaved_changes() { "*" } else { "" },
        );
        if title != self.window_title {
            self.window_title = title.clone();
            ctx.send_viewport_cmd(egui::ViewportCommand::Title(title));
        }
    }

    /// Opens or imports a project, asking first whether to save the changes.
    pub(crate) fn request_project_switch(&mut self, switch: ProjectSwitch) {
        if self.has_unsaved_changes() {
            self.pending_project_switch = Some(switch);
        } else {
            self.run_project_switch(switch);
        }
    }

    pub(crate) fn run_project_switch(&mut self, switch: ProjectSwitch) {
        match switch {
            ProjectSwitch::New => self.new_project(),
            ProjectSwitch::Open => self.open_project_dialog(),
            ProjectSwitch::OpenRecent(path) => self.load_project_from(path),
            ProjectSwitch::Quit => self.quit_confirmed = true,
        }
    }

    /// Closing the window is suspended until the user answers
    /// "save the changes?"; then it is requested again.
    pub(crate) fn handle_close_request(&mut self, ctx: &egui::Context) {
        if self.quit_confirmed {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            return;
        }
        if ctx.input(|i| i.viewport().close_requested()) && self.has_unsaved_changes() {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            self.pending_project_switch = Some(ProjectSwitch::Quit);
            ctx.request_repaint();
        }
    }

    pub(crate) fn show_unsaved_changes_dialog(&mut self, ui: &mut egui::Ui) {
        let Some(switch) = self.pending_project_switch.clone() else {
            return;
        };
        let mut choice = None;
        let modal = egui::Modal::new(egui::Id::new("unsaved_changes")).show(ui.ctx(), |ui| {
            ui.heading(if switch == ProjectSwitch::Quit {
                t!("project.save_before_quit")
            } else {
                t!("project.save_changes")
            });
            ui.label(t!("project.unsaved_changes"));
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                if ui.button(t!("common.save")).clicked() {
                    choice = Some(UnsavedChoice::Save);
                }
                if ui.button(t!("project.dont_save")).clicked() {
                    choice = Some(UnsavedChoice::Discard);
                }
                if ui.button(t!("common.cancel")).clicked() {
                    choice = Some(UnsavedChoice::Cancel);
                }
            });
        });
        if choice.is_none() && modal.should_close() {
            choice = Some(UnsavedChoice::Cancel);
        }
        if let Some(choice) = choice {
            self.resolve_unsaved_changes(choice);
            ui.ctx().request_repaint();
        }
    }

    pub(crate) fn resolve_unsaved_changes(&mut self, choice: UnsavedChoice) {
        let Some(switch) = self.pending_project_switch.take() else {
            return;
        };
        match choice {
            UnsavedChoice::Save => {
                self.save_project();
                // Save cancelled or failed: better not to lose anything.
                if !self.has_unsaved_changes() {
                    self.run_project_switch(switch);
                }
            }
            UnsavedChoice::Discard => self.run_project_switch(switch),
            UnsavedChoice::Cancel => {}
        }
    }

    pub(crate) fn import_otio_dialog(&mut self) {
        self.spawn_file_dialog(DialogKind::ImportOtio, |dlg| {
            dlg.add_filter("OpenTimelineIO", &["otio"]).pick_file()
        });
    }

    /// Adds the timelines of an `.otio` to the project, named after the
    /// file. What was not imported ends up in `import_warnings`.
    pub(crate) fn import_otio_from(&mut self, path: &Path) {
        self.session.import_otio(path);
    }

    /// Completes the import waiting for the reuse-media dialog (`None`:
    /// cancelled).
    pub(crate) fn finish_otio_import(&mut self, reuse_existing: Option<bool>) {
        self.session.finish_otio_import(reuse_existing);
        self.process_session_events();
    }

    /// The first imported timeline is opened, unless the agent imported it.
    fn apply_otio_merged(&mut self, result: vv_session::OtioMerged, by_agent: bool) {
        for &media_id in &result.added_media {
            self.enqueue_media_background_jobs(media_id);
        }
        if let Some(folder) = result.folder {
            self.media_pool_state.expanded.insert(folder);
        }
        if by_agent {
            return;
        }
        self.import_warnings = result.warnings.iter().map(otio_warning_text).collect();
        if let Some(&timeline_id) = result.timelines.first() {
            self.open_timeline(timeline_id);
            self.spawn_render_ahead_if_needed(timeline_id);
        }
    }

    pub(crate) fn show_otio_merge_dialog(&mut self, ui: &mut egui::Ui) {
        let Some((name, matches)) = self.session.otio_awaiting_decision() else {
            return;
        };
        let name = name.to_owned();
        let mut choice = None;
        let modal = egui::Modal::new(egui::Id::new("otio_merge")).show(ui.ctx(), |ui| {
            ui.heading(t!("project.otio_media_exist"));
            ui.label(t!("project.otio_media_exist_detail", count = matches));
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                if ui
                    .button(t!("project.otio_import_into_folder", folder = name))
                    .clicked()
                {
                    choice = Some(Some(false));
                }
                if ui.button(t!("project.otio_use_existing")).clicked() {
                    choice = Some(Some(true));
                }
                if ui.button(t!("common.cancel")).clicked() {
                    choice = Some(None);
                }
            });
        });
        if choice.is_none() && modal.should_close() {
            choice = Some(None);
        }
        if let Some(choice) = choice {
            self.finish_otio_import(choice);
        }
    }

    #[cfg(test)]
    pub(crate) fn wait_for_otio_import(&mut self) {
        let ctx = egui::Context::default();
        while self.session.otio_progress().is_some() {
            self.poll_session(&ctx);
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }

    pub(crate) fn show_otio_import_progress(&mut self, ui: &mut egui::Ui) {
        let Some((done, total)) = self.session.otio_progress() else {
            return;
        };
        egui::Modal::new(egui::Id::new("otio_import_progress")).show(ui.ctx(), |ui| {
            ui.heading(t!("project.otio_importing"));
            ui.add_space(8.0);
            let bar = if total == 0 {
                egui::ProgressBar::new(0.0).animate(true)
            } else {
                egui::ProgressBar::new(done as f32 / total as f32).text(t!(
                    "project.otio_import_progress",
                    done = done,
                    total = total
                ))
            };
            ui.add(bar.desired_width(320.0));
        });
    }

    pub(crate) fn open_project_dialog(&mut self) {
        self.spawn_file_dialog(DialogKind::OpenProject, |dlg| {
            dlg.add_filter(t!("file_filter.project"), &["vvproj"])
                .pick_file()
        });
    }

    pub(crate) fn new_project(&mut self) {
        self.session.new_project();
        self.reset_for_replaced_project();
    }

    /// Replaces the project and resets the UI state tied to the old one.
    pub(crate) fn load_project_from(&mut self, path: PathBuf) {
        match self.session.open(&path) {
            Ok(()) => {
                self.remember_recent_project(path);
                self.reset_for_replaced_project();
            }
            Err(e) => self.project_error = Some(t!("project.open_failed", error = e).into_owned()),
        }
    }

    /// Updates the "Recent projects" list and persists it immediately, so it
    /// survives even a crash before a clean shutdown.
    pub(crate) fn remember_recent_project(&mut self, path: PathBuf) {
        self.settings.add_recent_project(path);
        if let Some(settings_path) = &self.settings_path {
            let _ = self.settings.save(settings_path);
        }
    }

    /// After the session switched project: the UI state tied to the old
    /// one goes, the first timeline opens.
    pub(crate) fn reset_for_replaced_project(&mut self) {
        self.seen_epoch = self.session.epoch();
        self.timeline_id = self.session.project.timelines.keys().next();
        self.timeline_state = timeline_ui::TimelineState::default();
        self.import_warnings.clear();
        self.media_pool_state.invalidate_offline();
        self.last_export_settings = None;
        self.preview_meta = None;
        self.preview_error = None;
        self.last_viewer_frame_kind = None;
        self.browsing_render_ahead = None;
        self.active_clip = None;
        self.last_synced_playhead = 0;
        self.browsing_media = None;
        if let Some(audio) = &mut self.timeline_audio {
            audio.pause();
            audio.invalidate();
        }
        self.reset_playback_speed_to_normal();
        if let Some(fps) = self
            .timeline_id
            .map(|id| self.session.project.timelines[id].fps.as_f64())
        {
            self.timeline_audio().seek_frame(0, fps);
        }
        self.project_error = None;
        // Project replaced outside the history: `sync_render_ahead` would not
        // notice.
        if let Some(timeline_id) = self.timeline_id {
            self.spawn_render_ahead_if_needed(timeline_id);
            if let Some(render_ahead) = &self.render_ahead {
                render_ahead.update_project(&self.session.project, timeline_id);
            }
        }
        self.render_ahead_generation = self.session.history.generation();
        let media_ids: Vec<MediaId> = self.session.project.media_pool.keys().collect();
        for media_id in media_ids {
            self.enqueue_media_background_jobs(media_id);
        }
    }

    /// Opens the settings window: the export starts only from there
    /// (`run_export`).
    pub(crate) fn start_export(&mut self) {
        // The button is disabled during an export, but Ctrl+Shift+E is not.
        let Some(timeline_id) = self.timeline_id else {
            return;
        };
        if self.export.is_some() || self.export_dialog.is_some() {
            return;
        }
        let dialog = match self.last_export_settings.clone() {
            Some(settings) => {
                export_dialog::ExportDialog::new(settings, crate::hw_decode::export_choices())
            }
            None => export_dialog::ExportDialog::preferred(
                export_dialog::default_output_path(
                    self.session.path(),
                    &self.project_label(),
                    &self.session.project.timelines[timeline_id].name,
                ),
                crate::hw_decode::export_choices(),
            ),
        };
        self.export_dialog = Some(dialog);
    }

    pub(crate) fn show_export_dialog(&mut self, ui: &mut egui::Ui) {
        let (Some(dialog), Some(timeline_id)) = (&mut self.export_dialog, self.timeline_id) else {
            return;
        };
        let timeline = &self.session.project.timelines[timeline_id];
        let total_frames = timeline.total_frames();
        let marks = &self.timeline_state.export_marks;
        let info = export_dialog::TimelineInfo {
            resolution: timeline.resolution,
            fps: timeline.fps,
            total_frames,
            marks: (!marks.is_full(total_frames)).then(|| marks.resolve(total_frames)),
            has_audio: timeline
                .tracks_of_kind(TrackKind::Audio)
                .any(|(_, t)| !t.clips.is_empty()),
        };
        match dialog.show(ui.ctx(), &info) {
            export_dialog::ExportDialogAction::None => {}
            export_dialog::ExportDialogAction::Cancel => self.export_dialog = None,
            export_dialog::ExportDialogAction::Export { settings, range } => {
                self.export_dialog = None;
                self.last_export_settings = Some(settings.clone());
                self.run_export(timeline_id, settings, range);
            }
        }
    }

    /// Exports on a thread using a copy of the project: editing can
    /// continue.
    pub(crate) fn run_export(
        &mut self,
        timeline_id: TimelineId,
        settings: export::ExportSettings,
        range: std::ops::Range<FrameIdx>,
    ) {
        self.pause_proxies_for_export();
        match self.session.export(timeline_id, settings, range) {
            Some((job, progress)) => self.export = Some(ExportUiState { job, progress }),
            None => {
                self.resume_proxies_after_export();
                self.project_error = Some(t!("export.already_running").into_owned());
            }
        }
    }

    pub(crate) fn show_import_warnings(&mut self, ui: &mut egui::Ui) {
        if self.import_warnings.is_empty() {
            return;
        }
        let mut close = false;
        egui::Window::new(t!(
            "project.import_warnings",
            count = self.import_warnings.len()
        ))
        .id(egui::Id::new("import_warnings"))
        .collapsible(true)
        .default_width(480.0)
        .show(ui.ctx(), |ui| {
            egui::ScrollArea::vertical()
                .max_height(320.0)
                .show(ui, |ui| {
                    for warning in &self.import_warnings {
                        ui.label(warning);
                    }
                });
            ui.separator();
            ui.horizontal(|ui| {
                if ui.button(t!("project.copy_all")).clicked() {
                    ui.ctx().copy_text(self.import_warnings.join("\n"));
                }
                close = ui.button(t!("common.close")).clicked();
            });
        });
        if close {
            self.import_warnings.clear();
        }
    }

    pub(crate) fn show_relink_message(&mut self, ui: &mut egui::Ui) {
        let Some(message) = &self.relink_message else {
            return;
        };
        let (mut close, mut force) = (false, false);
        egui::Window::new(t!("relink.title"))
            .id(egui::Id::new("relink_message"))
            .collapsible(false)
            .default_width(360.0)
            .show(ui.ctx(), |ui| {
                ui.label(message.as_str());
                ui.separator();
                ui.horizontal(|ui| {
                    close = ui.button(t!("common.close")).clicked();
                    if self.forced_relink_offer.is_some() {
                        force = ui
                            .button(t!("relink.force_button"))
                            .on_hover_text(t!("relink.force_hint"))
                            .clicked();
                    }
                });
            });
        if force && let Some((base_dir, failed)) = self.forced_relink_offer.take() {
            self.open_forced_relink(base_dir, &failed);
        }
        if close || force {
            self.relink_message = None;
            self.forced_relink_offer = None;
        }
    }

    pub(crate) fn show_export_progress(&mut self, ui: &mut egui::Ui) {
        let Some(state) = &self.export else {
            return;
        };

        let (current, total, done, error, elapsed, fps, file_name, pipeline) = {
            let p = state.progress.lock().unwrap();
            let stage = |name, accelerator, stats| StageRow {
                name,
                accelerator,
                fps: export::StageStats::fps(stats),
                busy_share: p.busy_share(stats),
            };
            let pipeline = PipelineRows {
                stages: [
                    stage(
                        t!("project.export_stage_decode"),
                        decoders_label(&p.decoders),
                        &p.decode,
                    ),
                    stage(
                        t!("project.export_stage_compose"),
                        p.compositor.clone(),
                        &p.compose,
                    ),
                    stage(
                        t!("project.export_stage_encode"),
                        p.encoder.map(|codec| encoder_label(codec).to_owned()),
                        &p.encode,
                    ),
                ],
                output_fps: p.fps(),
                startup: p.startup,
                finalize: p.finalize,
            };
            (
                p.current_frame,
                p.total_frames,
                p.done,
                p.error.as_ref().map(export_error_message),
                p.elapsed,
                p.fps(),
                p.output_path
                    .file_name()
                    .map(|name| name.to_string_lossy().into_owned()),
                pipeline,
            )
        };

        let mut should_close = false;
        let screen_center = ui.ctx().content_rect().center();
        egui::Window::new(t!("project.export_window_title").into_owned())
            .id(egui::Id::new("export_progress"))
            .collapsible(false)
            .resizable(false)
            .pivot(egui::Align2::CENTER_CENTER)
            .default_pos(screen_center)
            .show(ui.ctx(), |ui| {
                ui.set_width(360.0);
                if let Some(name) = &file_name {
                    ui.label(egui::RichText::new(name).strong());
                    ui.add_space(4.0);
                }

                let fraction = if total > 0 {
                    (current as f32 / total as f32).clamp(0.0, 1.0)
                } else {
                    0.0
                };
                ui.add(
                    egui::ProgressBar::new(fraction)
                        .text(format!(
                            "{:.0}% · {}",
                            fraction * 100.0,
                            t!("project.export_progress", current = current, total = total)
                        ))
                        .animate(!done),
                );
                if !done && let Some(fps) = fps {
                    let remaining =
                        std::time::Duration::from_secs_f64((total - current).max(0) as f64 / fps);
                    ui.vertical_centered(|ui| {
                        ui.weak(t!(
                            "project.export_time",
                            elapsed = format_elapsed(elapsed),
                            remaining = format_elapsed(remaining)
                        ));
                    });
                }

                ui.add_space(6.0);
                egui::CollapsingHeader::new(t!("project.export_stages").into_owned())
                    .id_salt("export_stages")
                    .show(ui, |ui| pipeline.show(ui))
                    .header_response
                    .on_hover_text(t!("project.export_stage_hint"));

                ui.add_space(6.0);
                if let Some(err) = &error {
                    ui.colored_label(ui.visuals().error_fg_color, format!("✖ {err}"));
                } else if done {
                    let elapsed = format_elapsed(elapsed);
                    let text = match fps {
                        Some(fps) => t!(
                            "project.export_done_fps",
                            elapsed = elapsed,
                            fps = format_fps(fps)
                        ),
                        None => t!("project.export_done", elapsed = elapsed),
                    };
                    ui.colored_label(egui::Color32::from_rgb(110, 205, 120), format!("✔ {text}"));
                }

                // `horizontal` first: a bare right-to-left layout takes all the
                // height left and stretches the window.
                ui.horizontal(|ui| {
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if !done && ui.button(t!("common.cancel")).clicked() {
                            self.session.cancel_export(state.job);
                        }
                        if done && ui.button(t!("common.close")).clicked() {
                            should_close = true;
                        }
                    });
                });
            });

        // Without input egui does not redraw: the bar would stay frozen.
        if !done {
            ui.ctx().request_repaint();
        }

        if should_close {
            self.export = None;
        }
    }

    /// Applies the "use proxy" toggle and the quality: off (or a different
    /// quality) also stops the generation in progress (the worker is thrown
    /// away, the partial encode discarded), on restarts it for the media that
    /// do not have a proxy yet.
    pub(crate) fn apply_proxy_settings(&mut self) {
        let proxy = self.settings.proxy();
        for render_ahead in self.render_aheads() {
            render_ahead.set_proxy(proxy);
        }
        if self.proxy_worker.as_ref().map(|w| w.quality()) != proxy {
            self.proxy_worker = None;
            self.proxy_paused_for_export = false;
        }
        let Some(quality) = proxy else {
            return;
        };
        let media: Vec<(PathBuf, u64, u64)> = self
            .session
            .project
            .media_pool
            .iter()
            .filter(|(_, item)| {
                item.compound.is_none() && item.meta.has_video && !item.meta.is_image()
            })
            .map(|(_, item)| {
                (
                    item.path.clone(),
                    item.content_hash,
                    item.meta.duration_frames.max(0) as u64,
                )
            })
            .collect();
        if media.is_empty() {
            return;
        }
        let worker = self
            .proxy_worker
            .get_or_insert_with(|| proxy_worker::ProxyWorker::spawn(quality));
        for (path, content_hash, frames) in media {
            worker.enqueue(path, content_hash, frames);
        }
    }

    /// Pauses the proxies during the export: encoding a proxy slows it down a lot.
    /// A pause already chosen by the user must not be cancelled at the end of the export.
    pub(crate) fn pause_proxies_for_export(&mut self) {
        let Some(worker) = &self.proxy_worker else {
            return;
        };
        if worker.is_paused() {
            return;
        }
        worker.set_paused(true);
        self.proxy_paused_for_export = true;
    }

    /// Resumes the proxies if the export paused them. Idempotent.
    pub(crate) fn resume_proxies_after_export(&mut self) {
        if !self.proxy_paused_for_export {
            return;
        }
        self.proxy_paused_for_export = false;
        if let Some(worker) = &self.proxy_worker {
            worker.set_paused(false);
        }
    }

    /// Asks for a base directory and relinks the selected media. The selection
    /// is fixed now: the dialog comes back whenever it wants.
    pub(crate) fn relink_media_dialog(&mut self) {
        if self.media_pool_state.selected.is_empty() {
            return;
        }
        let targets: Vec<MediaId> = self.media_pool_state.selected.iter().copied().collect();
        self.spawn_file_dialog(DialogKind::RelinkMedia(targets), |dlg| dlg.pick_folder());
    }

    pub(crate) fn relink_folder_dialog(&mut self, folder: vv_core::FolderId) {
        let targets = self.session.project.media_in_folder(folder);
        if targets.is_empty() {
            return;
        }
        self.spawn_file_dialog(DialogKind::RelinkMedia(targets), |dlg| dlg.pick_folder());
    }

    /// Relinks the media of `targets` that no longer exist at their path to
    /// a file with the same name under `base_dir`, in the background. The
    /// ones not found are offered to the forced relink.
    pub(crate) fn relink_media(&mut self, base_dir: &Path, targets: &[MediaId]) {
        let targets = targets
            .iter()
            .filter_map(|&id| {
                let item = self.session.project.media_pool.get(id)?;
                item.compound.is_none().then(|| (id, item.path.clone()))
            })
            .collect();
        self.session.relink(relink_job::RelinkRequest::ByName {
            base_dir: base_dir.to_path_buf(),
            targets,
        });
    }

    /// Points each media at its new file, probing it in the background
    /// when `meta` is `None` (an offline media imported from OTIO only has
    /// a guessed one).
    pub(crate) fn apply_relinks(
        &mut self,
        relinks: Vec<(MediaId, PathBuf, Option<vv_core::MediaMeta>)>,
    ) {
        self.session
            .relink(relink_job::RelinkRequest::Chosen(relinks));
    }

    fn finish_relink(
        &mut self,
        relinked: Vec<MediaId>,
        not_found: Option<(PathBuf, Vec<MediaId>)>,
    ) {
        for &media_id in &relinked {
            self.enqueue_media_background_jobs(media_id);
        }
        self.media_pool_state.invalidate_offline();
        let relinked = relinked.len();
        self.relink_message = Some(match (&not_found, relinked) {
            (_, 0) => t!("project.relink_none").into_owned(),
            (None, _) => t!("project.relink_done", count = relinked).into_owned(),
            (Some((_, missing)), _) => t!(
                "project.relink_partial",
                count = relinked,
                missing = missing.len()
            )
            .into_owned(),
        });
        self.forced_relink_offer = not_found;
    }

    #[cfg(test)]
    pub(crate) fn wait_for_relink(&mut self) {
        let ctx = egui::Context::default();
        while self.session.relink_progress().is_some() {
            self.poll_session(&ctx);
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }

    pub(crate) fn show_relink_progress(&mut self, ui: &mut egui::Ui) {
        let Some(progress) = self.session.relink_progress() else {
            return;
        };
        let total = progress.total.load(Ordering::Relaxed);
        let done = progress.done.load(Ordering::Relaxed).min(total);
        let mut cancel = false;
        egui::Modal::new(egui::Id::new("relink_progress")).show(ui.ctx(), |ui| {
            ui.heading(t!("relink.title"));
            ui.add_space(8.0);
            let bar = if total == 0 {
                egui::ProgressBar::new(0.0)
                    .animate(true)
                    .text(t!("relink.scanning"))
            } else {
                egui::ProgressBar::new(done as f32 / total as f32).text(t!(
                    "relink.progress",
                    done = done,
                    total = total
                ))
            };
            ui.add(bar.desired_width(320.0));
            ui.add_space(8.0);
            cancel = ui.button(t!("common.cancel")).clicked();
        });
        if cancel {
            self.session.cancel_relink();
        }
    }
}

fn otio_warning_text(warning: &vv_core::OtioWarning) -> String {
    use vv_core::OtioWarning as W;
    match warning {
        W::EffectIgnored { effect, clips } => {
            t!("otio.effect_ignored", effect = effect, clips = clips)
        }
        W::SpeedNotApplied { clip, percent } => {
            t!("otio.speed_not_applied", clip = clip, percent = percent)
        }
        W::EffectPartlyIgnored { effect, clips } => {
            t!("otio.effect_partly_ignored", effect = effect, clips = clips)
        }
        W::UnsupportedInStack { schema } => t!("otio.unsupported_in_stack", schema = schema),
        W::TrackKindIgnored { kind } => {
            t!(
                "otio.track_kind_ignored",
                kind = kind.as_deref().unwrap_or("?")
            )
        }
        W::TransitionIgnored => t!("otio.transition_ignored"),
        W::UnsupportedItem { schema } => t!("otio.unsupported_item", schema = schema),
        W::ClipWithoutDuration { clip } => t!("otio.clip_without_duration", clip = clip),
        W::Placeholder { clip } => t!("otio.placeholder", clip = clip),
        W::ClipShorterThanAFrame { clip } => t!("otio.clip_too_short", clip = clip),
        W::AudioOnlyOnVideoTrack { clip } => t!("otio.audio_only_on_video_track", clip = clip),
        W::UnsupportedReference { clip, schema } => {
            t!("otio.unsupported_reference", clip = clip, schema = schema)
        }
        W::UnsupportedUrl { url } => t!("otio.unsupported_url", url = url),
        W::MediaUnreadable { path, error } => {
            t!(
                "otio.media_unreadable",
                path = path.display(),
                error = error
            )
        }
    }
    .into_owned()
}

struct StageRow {
    name: std::borrow::Cow<'static, str>,
    accelerator: Option<String>,
    fps: Option<f64>,
    busy_share: Option<f64>,
}

/// The "pipeline speed" details of the export window.
struct PipelineRows {
    stages: [StageRow; 3],
    output_fps: Option<f64>,
    startup: Option<std::time::Duration>,
    finalize: Option<std::time::Duration>,
}

impl PipelineRows {
    fn show(&self, ui: &mut egui::Ui) {
        let bottleneck = self
            .stages
            .iter()
            .enumerate()
            .filter_map(|(i, stage)| stage.fps.map(|fps| (i, fps)))
            .min_by(|a, b| a.1.total_cmp(&b.1))
            .map(|(i, _)| i);
        let fps_text = |fps: Option<f64>| {
            fps.map_or_else(|| "—".to_owned(), |fps| format!("{} fps", format_fps(fps)))
        };
        let right = |ui: &mut egui::Ui, text: egui::RichText| {
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.label(text.monospace())
            });
        };

        ui.set_width(ui.available_width());
        egui::Grid::new("export_stages")
            .num_columns(2)
            .spacing([16.0, 4.0])
            .show(ui, |ui| {
                for (i, stage) in self.stages.iter().enumerate() {
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 4.0;
                        ui.label(stage.name.as_ref());
                        if let Some(accelerator) = &stage.accelerator {
                            ui.weak(format!("({accelerator})"));
                        }
                    });
                    // One monospace label, so the two figures line up in columns.
                    let share = stage
                        .busy_share
                        .map_or_else(String::new, |share| format!("{:.0}%", share * 100.0));
                    let mut text =
                        egui::RichText::new(format!("{}  {share:>4}", fps_text(stage.fps)));
                    if bottleneck == Some(i) {
                        text = text.color(ui.visuals().warn_fg_color);
                    }
                    right(ui, text);
                    ui.end_row();
                }

                ui.strong(t!("project.export_stage_output"));
                right(
                    ui,
                    egui::RichText::new(format!("{}      ", fps_text(self.output_fps))).strong(),
                );
                ui.end_row();

                for (label, duration) in [
                    (t!("project.export_startup"), self.startup),
                    (t!("project.export_finalize"), self.finalize),
                ] {
                    if let Some(duration) = duration {
                        ui.weak(label);
                        right(
                            ui,
                            egui::RichText::new(format!("{}      ", format_elapsed(duration)))
                                .weak(),
                        );
                        ui.end_row();
                    }
                }
            });
    }
}

fn decoders_label(decoders: &[Option<vv_media::HwDevice>]) -> Option<String> {
    let labels: Vec<String> = decoders
        .iter()
        .map(|device| {
            device
                .as_ref()
                .map_or("CPU".into(), crate::hw_decode::device_label)
        })
        .collect();
    (!labels.is_empty()).then(|| labels.join(" + "))
}

fn encoder_label(codec: vv_media::VideoCodec) -> &'static str {
    match codec {
        vv_media::VideoCodec::X264 => "x264, CPU",
        vv_media::VideoCodec::Nvenc => "NVENC",
        vv_media::VideoCodec::VideoToolbox => "VideoToolbox",
        vv_media::VideoCodec::Vulkan => "Vulkan",
    }
}

fn format_fps(fps: f64) -> String {
    if fps < 10.0 {
        format!("{fps:.1}")
    } else {
        format!("{fps:.0}")
    }
}

fn format_elapsed(d: std::time::Duration) -> String {
    let secs = d.as_secs();
    match secs {
        0..60 => format!("{:.1} s", d.as_secs_f32()),
        60..3600 => format!("{}m {:02}s", secs / 60, secs % 60),
        _ => format!("{}h {:02}m {:02}s", secs / 3600, secs / 60 % 60, secs % 60),
    }
}

#[cfg(test)]
#[path = "tests/project_io.rs"]
mod tests;
