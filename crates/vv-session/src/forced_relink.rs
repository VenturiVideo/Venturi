//! Forced relink: for the media a relink by name could not find, looks for
//! files under the chosen folder that match on the properties the user
//! picks (duration, size, codec, waveform, ...), whatever their name.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use vv_core::{MediaId, MediaMeta};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Criterion {
    Name,
    Extension,
    Duration,
    Frames,
    Size,
    Codec,
    AspectRatio,
    Title,
    Artist,
    Waveform,
    Tag,
}

impl Criterion {
    pub const ALL: [Criterion; 11] = [
        Criterion::Name,
        Criterion::Extension,
        Criterion::Duration,
        Criterion::Frames,
        Criterion::Size,
        Criterion::Codec,
        Criterion::AspectRatio,
        Criterion::Title,
        Criterion::Artist,
        Criterion::Waveform,
        Criterion::Tag,
    ];

    /// Whether the project recorded what this criterion compares for
    /// `reference`: without it no candidate can match.
    pub fn known_for(self, reference: &Reference) -> bool {
        let meta = &reference.meta;
        match self {
            Criterion::Name => reference.path.file_stem().is_some(),
            Criterion::Extension => reference.path.extension().is_some(),
            Criterion::Duration | Criterion::Frames | Criterion::Tag => true,
            Criterion::Size => meta.file.size_bytes.is_some(),
            Criterion::Codec => meta.file.video_codec.is_some() || meta.file.audio_codec.is_some(),
            Criterion::AspectRatio => meta.width > 0 && meta.height > 0,
            Criterion::Title => meta.file.title.is_some(),
            Criterion::Artist => meta.file.artist.is_some(),
            Criterion::Waveform => reference.waveform.is_some(),
        }
    }

    fn needs_probe(self) -> bool {
        !matches!(
            self,
            Criterion::Name | Criterion::Extension | Criterion::Size
        )
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Criteria {
    pub enabled: BTreeSet<Criterion>,
    pub duration_tolerance_ms: u32,
    pub frames_tolerance: u32,
    pub size_tolerance_kib: u32,
    /// Minimum waveform similarity, 0..=100.
    pub waveform_min_percent: u32,
    pub tag_key: String,
    pub tag_value: String,
}

impl Default for Criteria {
    fn default() -> Self {
        Self {
            enabled: BTreeSet::from([Criterion::Name]),
            duration_tolerance_ms: 100,
            frames_tolerance: 1,
            size_tolerance_kib: 0,
            waveform_min_percent: 90,
            tag_key: String::new(),
            tag_value: String::new(),
        }
    }
}

impl Criteria {
    pub fn is_enabled(&self, criterion: Criterion) -> bool {
        self.enabled.contains(&criterion)
    }

    /// A search needs at least one criterion, and the tag one a key.
    pub fn is_searchable(&self) -> bool {
        !self.enabled.is_empty()
            && (!self.is_enabled(Criterion::Tag) || !self.tag_key.trim().is_empty())
    }
}

/// An offline media, as the project remembers it.
#[derive(Debug, Clone)]
pub struct Reference {
    pub media_id: MediaId,
    pub path: PathBuf,
    pub meta: MediaMeta,
    /// Peaks of the first audio stream, from the waveform cache.
    pub waveform: Option<Vec<f32>>,
}

/// A file found under the base folder. Everything beyond the path and the
/// size is read only when an enabled criterion asks for it, once per file
/// however many references it is compared to.
pub struct Candidate {
    pub path: PathBuf,
    size_bytes: Option<u64>,
    meta: Option<Option<MediaMeta>>,
    tags: Option<Vec<(String, String)>>,
    waveform: Option<Option<Vec<f32>>>,
}

impl Candidate {
    pub fn new(path: PathBuf) -> Self {
        Self {
            size_bytes: std::fs::metadata(&path).ok().map(|m| m.len()),
            path,
            meta: None,
            tags: None,
            waveform: None,
        }
    }

    fn meta(&mut self) -> Option<&MediaMeta> {
        let path = &self.path;
        self.meta
            .get_or_insert_with(|| vv_media::probe_media(path).ok())
            .as_ref()
    }

    fn tags(&mut self) -> &[(String, String)] {
        let path = &self.path;
        self.tags
            .get_or_insert_with(|| vv_media::probe_tags(path).unwrap_or_default())
    }

    /// Generated into the waveform cache like the waveform worker does, so
    /// a relink to this file finds it ready.
    fn waveform(&mut self) -> Option<&[f32]> {
        if self.waveform.is_none() {
            let peaks = self.meta().cloned().and_then(|meta| {
                if !meta.has_audio {
                    return None;
                }
                let hash = vv_media::content_fingerprint(&self.path).ok()?;
                if let Some(cached) = vv_media::load_waveform(hash, 0) {
                    return Some(cached.peaks);
                }
                let secs = meta.duration_frames as f64 / meta.fps.as_f64();
                vv_media::generate_waveforms(
                    &self.path,
                    hash,
                    &[0],
                    vv_media::recommended_num_peaks(secs),
                )
                .ok()?
                .pop()
                .flatten()
                .map(|w| w.peaks)
            });
            self.waveform = Some(peaks);
        }
        self.waveform.as_ref().and_then(|w| w.as_deref())
    }
}

fn same_text(a: &Option<String>, b: &Option<String>) -> bool {
    match (a, b) {
        (Some(a), Some(b)) => a.trim().eq_ignore_ascii_case(b.trim()),
        _ => false,
    }
}

fn duration_secs(meta: &MediaMeta) -> f64 {
    meta.duration_frames as f64 / meta.fps.as_f64()
}

fn matches_one(
    criterion: Criterion,
    criteria: &Criteria,
    reference: &Reference,
    candidate: &mut Candidate,
) -> bool {
    if !criterion.known_for(reference) {
        return false;
    }
    let reference_meta = &reference.meta;
    match criterion {
        Criterion::Name => reference.path.file_stem() == candidate.path.file_stem(),
        Criterion::Extension => {
            let ext = |p: &Path| p.extension().map(|e| e.to_ascii_lowercase());
            ext(&reference.path) == ext(&candidate.path)
        }
        Criterion::Size => match (reference_meta.file.size_bytes, candidate.size_bytes) {
            (Some(a), Some(b)) => a.abs_diff(b) <= u64::from(criteria.size_tolerance_kib) * 1024,
            _ => false,
        },
        Criterion::Tag => {
            let (key, value) = (criteria.tag_key.trim(), criteria.tag_value.trim());
            candidate
                .tags()
                .iter()
                .any(|(k, v)| k.eq_ignore_ascii_case(key) && v.trim().eq_ignore_ascii_case(value))
        }
        Criterion::Waveform => {
            let min = criteria.waveform_min_percent as f32 / 100.0;
            let reference_peaks = reference.waveform.as_deref().unwrap_or_default();
            candidate
                .waveform()
                .is_some_and(|peaks| waveform_similarity(reference_peaks, peaks) >= min)
        }
        Criterion::Duration
        | Criterion::Frames
        | Criterion::Codec
        | Criterion::AspectRatio
        | Criterion::Title
        | Criterion::Artist => {
            let Some(meta) = candidate.meta() else {
                return false;
            };
            match criterion {
                Criterion::Duration => {
                    (duration_secs(reference_meta) - duration_secs(meta)).abs() * 1000.0
                        <= f64::from(criteria.duration_tolerance_ms)
                }
                Criterion::Frames => {
                    reference_meta
                        .duration_frames
                        .abs_diff(meta.duration_frames)
                        <= u64::from(criteria.frames_tolerance)
                }
                Criterion::Codec => {
                    let (a, b) = (&reference_meta.file, &meta.file);
                    (a.video_codec.is_none() || a.video_codec == b.video_codec)
                        && (a.audio_codec.is_none() || a.audio_codec == b.audio_codec)
                }
                Criterion::AspectRatio => {
                    if meta.width == 0 || meta.height == 0 {
                        return false;
                    }
                    let ratio = |m: &MediaMeta| f64::from(m.width) / f64::from(m.height);
                    (ratio(reference_meta) - ratio(meta)).abs() <= ratio(reference_meta) * 0.01
                }
                Criterion::Title => same_text(&reference_meta.file.title, &meta.file.title),
                Criterion::Artist => same_text(&reference_meta.file.artist, &meta.file.artist),
                _ => unreachable!(),
            }
        }
    }
}

/// All the enabled criteria hold. The cheap ones go first, so a file that
/// already fails on name or size is never probed or decoded.
pub fn matches(criteria: &Criteria, reference: &Reference, candidate: &mut Candidate) -> bool {
    let (cheap, costly): (Vec<Criterion>, Vec<Criterion>) =
        criteria.enabled.iter().partition(|c| !c.needs_probe());
    let waveform_last = costly
        .iter()
        .filter(|&&c| c != Criterion::Waveform)
        .chain(costly.iter().filter(|&&c| c == Criterion::Waveform));
    !criteria.enabled.is_empty()
        && cheap
            .iter()
            .chain(waveform_last)
            .all(|&c| matches_one(c, criteria, reference, candidate))
}

/// Pearson correlation of the two envelopes, resampled to the same number of
/// bins: the peak counts differ whenever the durations do. 0 when either is
/// flat.
pub fn waveform_similarity(a: &[f32], b: &[f32]) -> f32 {
    const BINS: usize = 256;
    if a.is_empty() || b.is_empty() {
        return 0.0;
    }
    let resample = |peaks: &[f32]| -> Vec<f64> {
        (0..BINS)
            .map(|i| {
                let start = i * peaks.len() / BINS;
                let end = ((i + 1) * peaks.len() / BINS)
                    .max(start + 1)
                    .min(peaks.len());
                peaks[start.min(peaks.len() - 1)..end]
                    .iter()
                    .fold(0.0f64, |m, &p| m.max(f64::from(p)))
            })
            .collect()
    };
    let (a, b) = (resample(a), resample(b));
    let mean = |v: &[f64]| v.iter().sum::<f64>() / v.len() as f64;
    let (ma, mb) = (mean(&a), mean(&b));
    let (mut cov, mut va, mut vb) = (0.0, 0.0, 0.0);
    for (x, y) in a.iter().zip(&b) {
        cov += (x - ma) * (y - mb);
        va += (x - ma) * (x - ma);
        vb += (y - mb) * (y - mb);
    }
    if va <= f64::EPSILON || vb <= f64::EPSILON {
        return 0.0;
    }
    (cov / (va * vb).sqrt()).max(0.0) as f32
}

/// All the regular files under `base_dir`, breadth-first. Unreadable
/// directories are skipped; symlinks are not followed. Hidden directories
/// below `base_dir` are skipped too, so a trashed copy
/// (`~/.local/share/Trash`) never wins a relink.
pub fn files_under(base_dir: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    let mut dirs = std::collections::VecDeque::from([base_dir.to_path_buf()]);
    while let Some(dir) = dirs.pop_front() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            if file_type.is_dir() {
                if !entry.file_name().as_encoded_bytes().starts_with(b".") {
                    dirs.push_back(entry.path());
                }
            } else if file_type.is_file() {
                files.push(entry.path());
            }
        }
    }
    files
}

#[derive(Default)]
pub struct SearchProgress {
    pub done: AtomicUsize,
    pub total: AtomicUsize,
    pub cancel: AtomicBool,
}

/// A candidate accepted for a reference, with the metadata read while
/// comparing, so applying the relink need not probe it again.
#[derive(Debug, Clone)]
pub struct Match {
    pub path: PathBuf,
    pub meta: Option<MediaMeta>,
}

/// The matches of each reference (same order), best first: same name, then
/// closest length, then least nested. `None` if cancelled.
pub fn search(
    references: &[Reference],
    base_dir: &Path,
    criteria: &Criteria,
    progress: &SearchProgress,
) -> Option<Vec<Vec<Match>>> {
    let files = files_under(base_dir);
    progress.total.store(files.len(), Ordering::Relaxed);
    let mut found: Vec<Vec<Match>> = vec![Vec::new(); references.len()];
    for path in files {
        if progress.cancel.load(Ordering::Relaxed) {
            return None;
        }
        let mut candidate = Candidate::new(path);
        for (reference, found) in references.iter().zip(&mut found) {
            if matches(criteria, reference, &mut candidate) {
                found.push(Match {
                    path: candidate.path.clone(),
                    meta: candidate.meta.clone().flatten(),
                });
            }
        }
        progress.done.fetch_add(1, Ordering::Relaxed);
    }
    for (reference, found) in references.iter().zip(&mut found) {
        found.sort_by_key(|m| {
            (
                m.path.file_stem() != reference.path.file_stem(),
                m.meta.as_ref().map_or(0, |meta| {
                    meta.duration_frames
                        .abs_diff(reference.meta.duration_frames)
                }),
                m.path.components().count(),
            )
        });
    }
    Some(found)
}

pub struct RunningSearch {
    pub progress: Arc<SearchProgress>,
    pub handle: std::thread::JoinHandle<Option<Vec<Vec<Match>>>>,
}

pub fn spawn_search(
    references: Arc<Vec<Reference>>,
    base_dir: PathBuf,
    criteria: Criteria,
) -> RunningSearch {
    let progress = Arc::new(SearchProgress::default());
    let handle = {
        let progress = progress.clone();
        std::thread::Builder::new()
            .name("forced-relink".into())
            .spawn(move || search(&references, &base_dir, &criteria, &progress))
            .expect("cannot spawn the forced relink thread")
    };
    RunningSearch { progress, handle }
}

#[cfg(test)]
#[path = "tests/forced_relink.rs"]
mod tests;
