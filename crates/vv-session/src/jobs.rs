//! Background work of a session: media import, OTIO import, relink, export.
//! Each start returns a `JobId`; the outcome comes back as a
//! `SessionEvent` from `Session::tick`.

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use vv_core::{
    AddEntities, Command, CommandLabel, CompositeCommand, FolderId, FrameIdx, JoinableStep,
    MediaFolder, MediaId, MediaItem, MediaMeta, OtioImport, OtioWarning, SetMediaPath, TimelineId,
};

use crate::Session;
use crate::export::{ExportError, ExportProgress, ExportSettings};
use crate::import_worker::ImportWorker;
use crate::relink_job::{self, RelinkOutcome, RelinkProgress, RelinkRequest};

pub type JobId = u64;

/// Called from worker threads when there is something for `tick` to collect.
#[derive(Clone)]
pub struct Waker(Arc<dyn Fn() + Send + Sync>);

impl Waker {
    pub fn new(wake: impl Fn() + Send + Sync + 'static) -> Self {
        Self(Arc::new(wake))
    }

    pub fn wake(&self) {
        (self.0)()
    }
}

impl Default for Waker {
    fn default() -> Self {
        Self::new(|| {})
    }
}

#[derive(Debug, Clone)]
pub enum SessionEvent {
    ImportStarted {
        job: JobId,
    },
    /// A probed media entered the pool.
    MediaAdded {
        job: JobId,
        media_id: MediaId,
    },
    /// `errors` are "file name: reason" for the files that did not import.
    ImportFinished {
        job: JobId,
        imported: Vec<MediaId>,
        errors: Vec<String>,
    },
    /// Some imported media share a file name with pool media: the host
    /// decides with `finish_otio_import`.
    OtioNeedsDecision {
        job: JobId,
    },
    OtioImported {
        job: JobId,
        result: OtioMerged,
    },
    OtioFailed {
        job: JobId,
        error: String,
    },
    /// The host dropped an import waiting for a decision.
    OtioCancelled {
        job: JobId,
    },
    RelinkFinished {
        job: JobId,
        end: RelinkEnd,
    },
    ExportFinished {
        job: JobId,
        result: Result<(), ExportError>,
    },
}

#[derive(Debug, Clone)]
pub struct OtioMerged {
    /// The first one is the natural one to open.
    pub timelines: Vec<TimelineId>,
    pub added_media: Vec<MediaId>,
    pub folder: Option<FolderId>,
    pub warnings: Vec<OtioWarning>,
}

#[derive(Debug, Clone)]
pub enum RelinkEnd {
    Done {
        relinked: Vec<MediaId>,
        /// Not found by name, with the folder searched.
        not_found: Option<(PathBuf, Vec<MediaId>)>,
    },
    Cancelled,
    Failed,
}

#[derive(Default)]
pub(crate) struct Jobs {
    next_id: JobId,
    events: Vec<SessionEvent>,
    queued_imports: VecDeque<(JobId, Vec<PathBuf>)>,
    import: Option<RunningImport>,
    /// The undo step of the last import, while it can still grow.
    import_step: Option<(JobId, JoinableStep)>,
    otio: Option<RunningOtio>,
    otio_merge: Option<(JobId, PendingOtioMerge)>,
    relink: Option<(JobId, relink_job::RelinkJob)>,
    export: Option<RunningExport>,
    /// Every export of the session, readable after it ended.
    export_progress: HashMap<JobId, Arc<Mutex<ExportProgress>>>,
}

struct RunningImport {
    job: JobId,
    worker: ImportWorker,
    imported: Vec<MediaId>,
    errors: Vec<String>,
}

struct RunningOtio {
    job: JobId,
    name: String,
    probed: Arc<AtomicUsize>,
    total: Arc<AtomicUsize>,
    handle: std::thread::JoinHandle<Result<OtioImport, String>>,
}

struct PendingOtioMerge {
    imported: OtioImport,
    name: String,
    /// Imported media → the pool media with the same file name.
    matches: HashMap<MediaId, MediaId>,
}

struct RunningExport {
    job: JobId,
    cancel: Arc<AtomicBool>,
    handle: std::thread::JoinHandle<Result<(), ExportError>>,
}

impl Jobs {
    fn alloc(&mut self) -> JobId {
        self.next_id += 1;
        self.next_id
    }
}

/// Absolute, symlink-free form of `path`, or `path` itself if the file is
/// not reachable (removed media must still compare equal to itself).
fn canonical_path(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

pub fn file_label(path: &Path) -> String {
    path.file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("?")
        .to_string()
}

impl Session {
    pub fn set_waker(&mut self, waker: Waker) {
        self.waker = waker;
    }

    /// For work the host runs on its own threads.
    pub fn waker(&self) -> Waker {
        self.waker.clone()
    }

    /// Collects what the background work produced since the last call.
    /// Imports and relinks become history steps: not while the caller has
    /// an undo group open, or they would end up inside it.
    pub fn tick(&mut self) -> Vec<SessionEvent> {
        self.poll_import();
        self.poll_otio();
        self.poll_relink();
        self.poll_export();
        std::mem::take(&mut self.jobs.events)
    }

    pub fn has_running_jobs(&self) -> bool {
        self.jobs.import.is_some()
            || !self.jobs.queued_imports.is_empty()
            || self.jobs.otio.is_some()
            || self.jobs.relink.is_some()
            || self.jobs.export.is_some()
    }

    /// The media in the pool that points at `path`, if any. Paths are
    /// canonicalized so that symlinks and `..` do not import a duplicate.
    pub fn media_with_path(&self, path: &Path) -> Option<MediaId> {
        let target = canonical_path(path);
        self.project
            .media_pool
            .iter()
            .find(|(_, item)| item.compound.is_none() && canonical_path(&item.path) == target)
            .map(|(id, _)| id)
    }

    /// Adds an already probed media to the pool, as its own undo step.
    pub fn add_media(&mut self, path: PathBuf, meta: MediaMeta) -> MediaId {
        let (media_id, add) = self.media_entity(path, meta);
        self.history.do_command(&mut self.project, Box::new(add));
        media_id
    }

    fn media_entity(&mut self, path: PathBuf, meta: MediaMeta) -> (MediaId, AddEntities) {
        // `0` only if the file vanished in the meantime: at worst a proxy is
        // regenerated.
        let content_hash = vv_media::content_fingerprint(&path).unwrap_or(0);
        let mut add = AddEntities::new(CommandLabel::ImportMedia);
        let media_id = add.media(
            &mut self.project,
            MediaItem {
                path,
                meta,
                content_hash,
                compound: None,
                folder: None,
            },
        );
        (media_id, add)
    }

    /// Applies `cmd` in the undo step of import `job`, so that undoing the
    /// import undoes it too (e.g. the timeline created for the first media).
    /// A new step if something else entered the history in between.
    pub fn join_import(&mut self, job: JobId, cmd: Box<dyn Command>) {
        let step = self
            .jobs
            .import_step
            .filter(|(step_job, _)| *step_job == job)
            .map(|(_, step)| step);
        let step = self
            .history
            .join(&mut self.project, step, CommandLabel::ImportMedia, cmd);
        self.jobs.import_step = Some((job, step));
    }

    /// Probes the files on worker threads and adds them to the pool in the
    /// given order; files already in the pool are skipped. Runs after the
    /// imports started before it.
    pub fn import_media(&mut self, paths: Vec<PathBuf>) -> JobId {
        let job = self.jobs.alloc();
        self.jobs.queued_imports.push_back((job, paths));
        self.start_next_import();
        job
    }

    pub fn is_importing(&self) -> bool {
        self.jobs.import.is_some() || !self.jobs.queued_imports.is_empty()
    }

    /// `(probed, total)` over the running and the queued imports.
    pub fn import_progress(&self) -> Option<(usize, usize)> {
        let running = self.jobs.import.as_ref()?;
        let (done, total) = running.worker.progress();
        let queued: usize = self.jobs.queued_imports.iter().map(|(_, p)| p.len()).sum();
        Some((done, total + queued))
    }

    fn start_next_import(&mut self) {
        while self.jobs.import.is_none() {
            let Some((job, paths)) = self.jobs.queued_imports.pop_front() else {
                return;
            };
            let mut seen: HashSet<PathBuf> = self
                .project
                .media_pool
                .values()
                .filter(|item| item.compound.is_none())
                .map(|item| canonical_path(&item.path))
                .collect();
            let paths: Vec<PathBuf> = paths
                .into_iter()
                .filter(|path| seen.insert(canonical_path(path)))
                .collect();
            if paths.is_empty() {
                self.jobs.events.push(SessionEvent::ImportFinished {
                    job,
                    imported: Vec::new(),
                    errors: Vec::new(),
                });
                continue;
            }
            self.jobs.events.push(SessionEvent::ImportStarted { job });
            self.jobs.import = Some(RunningImport {
                job,
                worker: ImportWorker::spawn(paths, self.waker.clone()),
                imported: Vec::new(),
                errors: Vec::new(),
            });
        }
    }

    fn poll_import(&mut self) {
        let Some(mut running) = self.jobs.import.take() else {
            return;
        };
        for (path, result) in running.worker.drain_ready() {
            match result {
                Ok(meta) => {
                    let (media_id, add) = self.media_entity(path, meta);
                    self.join_import(running.job, Box::new(add));
                    running.imported.push(media_id);
                    self.jobs.events.push(SessionEvent::MediaAdded {
                        job: running.job,
                        media_id,
                    });
                }
                Err(e) => running.errors.push(format!("{}: {e}", file_label(&path))),
            }
        }
        if !running.worker.is_finished() {
            self.jobs.import = Some(running);
            return;
        }
        self.jobs.events.push(SessionEvent::ImportFinished {
            job: running.job,
            imported: running.imported,
            errors: running.errors,
        });
        self.start_next_import();
    }

    /// Adds the timelines of an `.otio` file, named after it. `None` while
    /// another OTIO import is running or waiting for a decision.
    pub fn import_otio(&mut self, path: &Path) -> Option<JobId> {
        if self.jobs.otio.is_some() || self.jobs.otio_merge.is_some() {
            return None;
        }
        let job = self.jobs.alloc();
        let name = path
            .file_stem()
            .map_or_else(|| "OTIO".into(), |s| s.to_string_lossy().into_owned());
        let probed = Arc::new(AtomicUsize::new(0));
        let total = Arc::new(AtomicUsize::new(0));
        let path = path.to_path_buf();
        let waker = self.waker.clone();
        let handle = std::thread::spawn({
            let (probed, total) = (probed.clone(), total.clone());
            move || {
                let result = read_otio(&path, &probed, &total);
                waker.wake();
                result
            }
        });
        self.jobs.otio = Some(RunningOtio {
            job,
            name,
            probed,
            total,
            handle,
        });
        Some(job)
    }

    /// `(probed, total)` of the media of the running OTIO import; `total` is
    /// 0 while the file is being read.
    pub fn otio_progress(&self) -> Option<(usize, usize)> {
        let running = self.jobs.otio.as_ref()?;
        let total = running.total.load(Ordering::Relaxed);
        Some((running.probed.load(Ordering::Relaxed).min(total), total))
    }

    /// Name of the import waiting for `finish_otio_import`, and how many of
    /// its media match pool media by file name.
    pub fn otio_awaiting_decision(&self) -> Option<(&str, usize)> {
        let (_, merge) = self.jobs.otio_merge.as_ref()?;
        Some((&merge.name, merge.matches.len()))
    }

    /// Completes the import waiting for a decision: `Some(true)` reuses the
    /// pool media with the same file name, `Some(false)` imports them all,
    /// `None` drops the import. The outcome comes from the next `tick`.
    pub fn finish_otio_import(&mut self, reuse_existing: Option<bool>) {
        let Some((job, merge)) = self.jobs.otio_merge.take() else {
            return;
        };
        let event = match reuse_existing {
            Some(reuse) => SessionEvent::OtioImported {
                job,
                result: self.merge_otio(merge, reuse),
            },
            None => SessionEvent::OtioCancelled { job },
        };
        self.jobs.events.push(event);
    }

    fn poll_otio(&mut self) {
        if !self
            .jobs
            .otio
            .as_ref()
            .is_some_and(|r| r.handle.is_finished())
        {
            return;
        }
        let Some(running) = self.jobs.otio.take() else {
            return;
        };
        let job = running.job;
        match running.handle.join() {
            Ok(Ok(imported)) => {
                let matches = self.media_with_same_name(&imported.project);
                let merge = PendingOtioMerge {
                    imported,
                    name: running.name,
                    matches,
                };
                if merge.matches.is_empty() {
                    let result = self.merge_otio(merge, false);
                    self.jobs
                        .events
                        .push(SessionEvent::OtioImported { job, result });
                } else {
                    self.jobs.otio_merge = Some((job, merge));
                    self.jobs
                        .events
                        .push(SessionEvent::OtioNeedsDecision { job });
                }
            }
            Ok(Err(error)) => self
                .jobs
                .events
                .push(SessionEvent::OtioFailed { job, error }),
            Err(_) => self.jobs.events.push(SessionEvent::OtioFailed {
                job,
                error: "panic".into(),
            }),
        }
    }

    fn media_with_same_name(&self, imported: &vv_core::Project) -> HashMap<MediaId, MediaId> {
        let existing: HashMap<String, MediaId> = self
            .project
            .media_pool
            .iter()
            .filter(|(_, item)| item.compound.is_none())
            .map(|(id, item)| (file_label(&item.path), id))
            .collect();
        imported
            .media_pool
            .iter()
            .filter(|(_, item)| item.compound.is_none())
            .filter_map(|(id, item)| Some((id, *existing.get(&file_label(&item.path))?)))
            .collect()
    }

    /// The new media go in a folder named after the file, the timelines at
    /// the root of the pool.
    fn merge_otio(&mut self, merge: PendingOtioMerge, reuse_existing: bool) -> OtioMerged {
        let PendingOtioMerge {
            imported,
            name,
            matches,
        } = merge;
        let mut project = imported.project;
        // Compound clips keep their own name, matching their pool item's.
        let nested: HashSet<TimelineId> = project
            .media_pool
            .values()
            .filter_map(|item| item.compound)
            .collect();
        let top_level: Vec<bool> = project
            .timelines
            .keys()
            .map(|id| !nested.contains(&id))
            .collect();
        let single = top_level.iter().filter(|top| **top).count() == 1;
        for (id, timeline) in project.timelines.iter_mut() {
            if nested.contains(&id) {
                continue;
            }
            timeline.name = if single {
                name.clone()
            } else {
                format!("{name} - {}", timeline.name)
            };
        }
        let reuse: HashMap<MediaId, MediaId> = if reuse_existing {
            // A match may have been deleted while the decision was pending.
            matches
                .into_iter()
                .filter(|(_, existing)| self.project.media_pool.contains_key(*existing))
                .collect()
        } else {
            HashMap::new()
        };
        let mut add = AddEntities::new(CommandLabel::ImportOtio);
        let folder = (project.media_pool.len() > reuse.len()).then(|| {
            add.folder(
                &mut self.project,
                MediaFolder {
                    name: name.clone(),
                    parent: None,
                },
            )
        });
        let timelines = vv_core::pool::absorb(&mut self.project, &mut add, project, &reuse, folder)
            .into_iter()
            .zip(top_level)
            .filter_map(|(id, top)| top.then_some(id))
            .collect();
        let added_media = add.media_ids().collect();
        self.history.do_command(&mut self.project, Box::new(add));
        OtioMerged {
            timelines,
            added_media,
            folder,
            warnings: imported.warnings,
        }
    }

    /// Relinks media in the background; all the relinks end up in a single
    /// undo step. `None` while another relink is running.
    pub fn relink(&mut self, request: RelinkRequest) -> Option<JobId> {
        if self.jobs.relink.is_some() {
            return None;
        }
        let job = self.jobs.alloc();
        self.jobs.relink = Some((job, relink_job::spawn(request, self.waker.clone())));
        Some(job)
    }

    pub fn relink_progress(&self) -> Option<&RelinkProgress> {
        self.jobs.relink.as_ref().map(|(_, job)| &*job.progress)
    }

    pub fn cancel_relink(&self) {
        if let Some(progress) = self.relink_progress() {
            progress.cancel.store(true, Ordering::Relaxed);
        }
    }

    fn poll_relink(&mut self) {
        if !self
            .jobs
            .relink
            .as_ref()
            .is_some_and(|(_, j)| j.handle.is_finished())
        {
            return;
        }
        let Some((job, running)) = self.jobs.relink.take() else {
            return;
        };
        let end = match running.handle.join() {
            Ok(Some(outcome)) => self.apply_relink(outcome),
            Ok(None) => RelinkEnd::Cancelled,
            Err(_) => RelinkEnd::Failed,
        };
        self.jobs
            .events
            .push(SessionEvent::RelinkFinished { job, end });
    }

    fn apply_relink(&mut self, outcome: RelinkOutcome) -> RelinkEnd {
        // Media deleted while the job was running are skipped.
        let relinks: Vec<_> = outcome
            .relinks
            .into_iter()
            .filter(|r| self.project.media_pool.contains_key(r.media_id))
            .collect();
        let relinked: Vec<MediaId> = relinks.iter().map(|r| r.media_id).collect();
        if !relinks.is_empty() {
            let commands: Vec<Box<dyn vv_core::Command>> = relinks
                .into_iter()
                .map(|r| {
                    Box::new(SetMediaPath::new(
                        r.media_id,
                        r.path,
                        r.content_hash,
                        r.meta,
                    )) as Box<dyn vv_core::Command>
                })
                .collect();
            self.history.do_command(
                &mut self.project,
                Box::new(CompositeCommand::new(CommandLabel::RelinkMedia, commands)),
            );
        }
        RelinkEnd::Done {
            relinked,
            not_found: outcome.not_found,
        }
    }

    /// Exports a snapshot of the project on a thread: editing can go on.
    /// The progress stays readable after the end. `None` while another
    /// export is running.
    pub fn export(
        &mut self,
        timeline_id: TimelineId,
        settings: ExportSettings,
        range: std::ops::Range<FrameIdx>,
    ) -> Option<(JobId, Arc<Mutex<ExportProgress>>)> {
        if self.jobs.export.is_some() {
            return None;
        }
        let job = self.jobs.alloc();
        let project = self.project.clone();
        let progress = Arc::new(Mutex::new(ExportProgress::default()));
        let cancel = Arc::new(AtomicBool::new(false));
        let waker = self.waker.clone();
        let handle = std::thread::spawn({
            let (progress, cancel) = (progress.clone(), cancel.clone());
            move || {
                let result = crate::export::export_timeline(
                    &project,
                    timeline_id,
                    &settings,
                    range,
                    &progress,
                    &cancel,
                );
                // Errors and cancellation close `progress` too.
                if let Err(e) = &result {
                    let mut p = progress.lock().unwrap();
                    p.error = Some(e.clone());
                    p.done = true;
                }
                waker.wake();
                result
            }
        });
        self.jobs.export = Some(RunningExport {
            job,
            cancel,
            handle,
        });
        self.jobs.export_progress.insert(job, progress.clone());
        Some((job, progress))
    }

    pub fn is_exporting(&self) -> bool {
        self.jobs.export.is_some()
    }

    pub fn running_export(&self) -> Option<JobId> {
        self.jobs.export.as_ref().map(|running| running.job)
    }

    pub fn export_progress(&self, job: JobId) -> Option<Arc<Mutex<ExportProgress>>> {
        self.jobs.export_progress.get(&job).cloned()
    }

    /// `false` if `job` is not the running export.
    pub fn cancel_export(&self, job: JobId) -> bool {
        // Done but not collected by `tick` yet: nothing left to cancel.
        let done = self
            .export_progress(job)
            .is_some_and(|p| p.lock().unwrap().done);
        match &self.jobs.export {
            Some(running) if running.job == job && !done => {
                running.cancel.store(true, Ordering::Relaxed);
                true
            }
            _ => false,
        }
    }

    fn poll_export(&mut self) {
        if !self
            .jobs
            .export
            .as_ref()
            .is_some_and(|r| r.handle.is_finished())
        {
            return;
        }
        let Some(running) = self.jobs.export.take() else {
            return;
        };
        let result = running
            .handle
            .join()
            .unwrap_or_else(|_| Err(ExportError::Failed("panic".into())));
        self.jobs.events.push(SessionEvent::ExportFinished {
            job: running.job,
            result,
        });
    }
}

fn read_otio(path: &Path, probed: &AtomicUsize, total: &AtomicUsize) -> Result<OtioImport, String> {
    let value: serde_json::Value = std::fs::read_to_string(path)
        .map_err(vv_core::OtioError::from)
        .and_then(|text| Ok(serde_json::from_str(&text)?))
        .map_err(|e| e.to_string())?;
    total.store(vv_core::media_url_count(&value), Ordering::Relaxed);
    let measure = |title: &vv_core::TitleParams| vv_render::text::title_metrics(title);
    let mut probe = |media_path: &Path| {
        let result = vv_media::probe_media(media_path)
            .map_err(|e| e.to_string())
            .map(|meta| (meta, vv_media::content_fingerprint(media_path).unwrap_or(0)));
        probed.fetch_add(1, Ordering::Relaxed);
        result
    };
    let base_dir = path.parent().unwrap_or(Path::new("."));
    vv_core::project_from_otio(&value, base_dir, &mut probe, Some(&measure))
        .map_err(|e| e.to_string())
}
