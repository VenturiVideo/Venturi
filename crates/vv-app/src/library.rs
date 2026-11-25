//! User library shared by all projects, in `$XDG_DATA_HOME/venturi/library`
//! (`%APPDATA%` on Windows):
//! assets (pool items, their files copied in) and attribute presets, one
//! JSON file each so they can be synced or handed around one by one, and the
//! folders the assets are sorted in.

use std::path::{Path, PathBuf};
use std::sync::mpsc;

use serde::{Deserialize, Serialize};
use vv_core::clipboard::{ClipboardOrigin, ClipboardPayload};
use vv_core::{Clip, MediaItem, Project, Rational};
use vv_session::MediaTree;

use crate::paste_attributes::{Attribute, KeyframeAnchor, TimelineFormat};
use crate::settings::Shortcut;

const VERSION: u32 = 1;

#[derive(Serialize, Deserialize)]
struct AssetFile {
    version: u32,
    name: String,
    #[serde(default)]
    folder: Option<String>,
    /// `ClipboardPayload::to_text`: it already carries compound clips with
    /// their nested timelines.
    payload: String,
}

#[derive(Clone)]
pub(crate) struct Asset {
    pub id: String,
    pub name: String,
    pub folder: Option<String>,
    pub payload: ClipboardPayload,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct LibraryFolder {
    pub id: String,
    pub name: String,
    pub parent: Option<String>,
}

#[derive(Serialize, Deserialize)]
struct FoldersFile {
    version: u32,
    folders: Vec<LibraryFolder>,
}

impl Asset {
    pub fn item(&self) -> Option<&MediaItem> {
        self.payload.media_items().next()
    }

    /// Whether a file it needs, also inside a compound clip, is gone.
    pub fn is_offline(&self) -> bool {
        self.payload.files().any(|f| !f.path.is_file())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct AttributePreset {
    pub version: u32,
    pub name: String,
    pub attributes: Vec<Attribute>,
    pub anchor: KeyframeAnchor,
    /// Added after the target's filters instead of replacing them.
    #[serde(default)]
    pub append_filters: bool,
    /// Applies the preset to the selected clips; unique among presets.
    #[serde(default)]
    pub shortcut: Option<Shortcut>,
    pub fps: Rational,
    pub resolution: (u32, u32),
    pub video: Option<Clip>,
    pub audio: Option<Clip>,
}

impl AttributePreset {
    pub fn new(
        name: String,
        format: TimelineFormat,
        video: Option<Clip>,
        audio: Option<Clip>,
    ) -> Self {
        Self {
            version: VERSION,
            name,
            attributes: Vec::new(),
            anchor: KeyframeAnchor::Start,
            append_filters: false,
            shortcut: None,
            fps: format.fps,
            resolution: format.resolution,
            video,
            audio,
        }
    }

    pub fn format(&self) -> TimelineFormat {
        TimelineFormat {
            fps: self.fps,
            resolution: self.resolution,
        }
    }
}

/// An asset whose files are still being copied.
struct PendingAsset {
    name: String,
    /// `Ok(None)`: the file was already there, nothing added.
    done: mpsc::Receiver<Result<Option<Asset>, String>>,
}

#[derive(Default)]
pub(crate) struct Library {
    /// `None`: no home directory, the library is in memory only.
    root: Option<PathBuf>,
    pub assets: Vec<Asset>,
    pub folders: Vec<LibraryFolder>,
    pub presets: Vec<(String, AttributePreset)>,
    pending: Vec<PendingAsset>,
}

impl Library {
    pub fn default_root() -> Option<PathBuf> {
        Some(vv_core::dirs::data_dir()?.join("venturi").join("library"))
    }

    /// Unreadable entries are skipped with a message, never fatal.
    pub fn open(root: PathBuf) -> Self {
        let mut assets: Vec<Asset> = json_files(&root.join("assets"))
            .filter_map(|(id, text)| {
                let file: AssetFile = parse(&id, &text)?;
                let payload = match ClipboardPayload::from_text(&file.payload) {
                    Some(Ok(payload)) => payload,
                    _ => {
                        eprintln!("library asset {id}: unreadable payload");
                        return None;
                    }
                };
                Some(Asset {
                    id,
                    name: file.name,
                    folder: file.folder,
                    payload,
                })
            })
            .collect();
        assets.sort_by_cached_key(|a| a.name.to_lowercase());
        let folders = std::fs::read_to_string(root.join("folders.json"))
            .ok()
            .and_then(|text| parse::<FoldersFile>("folders", &text))
            .map_or_else(Vec::new, |f| f.folders);
        let mut presets: Vec<(String, AttributePreset)> = json_files(&root.join("presets"))
            .filter_map(|(id, text)| Some((id.clone(), parse(&id, &text)?)))
            .collect();
        presets.sort_by_cached_key(|(_, p)| p.name.to_lowercase());
        Self {
            root: Some(root),
            assets,
            folders,
            presets,
            pending: Vec::new(),
        }
    }

    /// Names of the assets still copying their files.
    pub fn pending(&self) -> impl Iterator<Item = &str> {
        self.pending.iter().map(|p| p.name.as_str())
    }

    /// Copies the files of `payload` into the library in the background.
    pub fn add_asset(&mut self, name: String, folder: Option<String>, payload: ClipboardPayload) {
        let (tx, done) = mpsc::channel();
        let root = self.root.clone();
        let asset = Asset {
            id: new_id(),
            name: name.clone(),
            folder,
            payload,
        };
        std::thread::spawn(move || {
            let result = match root {
                Some(root) => store_asset(&root, asset),
                None => Ok(asset),
            };
            let _ = tx.send(result.map(Some));
        });
        self.pending.push(PendingAsset { name, done });
    }

    /// Each media file of `tree` becomes an asset in `parent`, its folders
    /// library folders: one with the same name and place as an existing
    /// one is merged into it, and a file already in its folder is skipped.
    /// Probed and copied in the background, one file at a time.
    pub fn import_tree(
        &mut self,
        tree: MediaTree,
        parent: Option<String>,
        origin: ClipboardOrigin,
    ) -> Result<(), String> {
        let mut ids: Vec<String> = Vec::with_capacity(tree.folders.len());
        for (name, inside) in tree.folders {
            let parent = inside.map(|i| ids[i].clone()).or_else(|| parent.clone());
            let existing = self
                .folders
                .iter()
                .find(|f| f.parent == parent && f.name == name)
                .map(|f| f.id.clone());
            let id = match existing {
                Some(id) => id,
                None => self.add_folder(name, parent)?,
            };
            ids.push(id);
        }
        let present: Vec<(u64, Option<String>)> = self
            .assets
            .iter()
            .filter_map(|a| Some((a.item()?.content_hash, a.folder.clone())))
            .collect();
        let mut jobs = Vec::with_capacity(tree.files.len());
        for (path, inside) in tree.files {
            let (tx, done) = mpsc::channel();
            self.pending.push(PendingAsset {
                name: vv_session::file_label(&path),
                done,
            });
            let folder = inside.map(|i| ids[i].clone()).or_else(|| parent.clone());
            jobs.push((path, folder, tx));
        }
        let root = self.root.clone();
        std::thread::spawn(move || {
            for (path, folder, tx) in jobs {
                let result = file_asset(&path, folder, origin).and_then(|asset| {
                    let hash = asset.item().map(|i| i.content_hash);
                    if present
                        .iter()
                        .any(|(h, f)| Some(*h) == hash && *f == asset.folder)
                    {
                        return Ok(None);
                    }
                    match &root {
                        Some(root) => store_asset(root, asset).map(Some),
                        None => Ok(Some(asset)),
                    }
                });
                let _ = tx.send(result);
            }
        });
        Ok(())
    }

    /// Collects the finished copies; returns the errors.
    pub fn poll(&mut self) -> Vec<String> {
        let mut errors = Vec::new();
        let mut i = 0;
        while i < self.pending.len() {
            match self.pending[i].done.try_recv() {
                Ok(result) => {
                    let pending = self.pending.remove(i);
                    match result {
                        Ok(Some(asset)) => self.insert_asset(asset),
                        Ok(None) => {}
                        Err(e) => errors.push(format!("{}: {e}", pending.name)),
                    }
                }
                Err(mpsc::TryRecvError::Empty) => i += 1,
                Err(mpsc::TryRecvError::Disconnected) => {
                    self.pending.remove(i);
                }
            }
        }
        errors
    }

    fn insert_asset(&mut self, asset: Asset) {
        let at = self
            .assets
            .partition_point(|a| a.name.to_lowercase() <= asset.name.to_lowercase());
        self.assets.insert(at, asset);
    }

    pub fn rename_asset(&mut self, id: &str, name: String) -> Result<(), String> {
        let Some(index) = self.assets.iter().position(|a| a.id == id) else {
            return Ok(());
        };
        let mut asset = self.assets.remove(index);
        asset.name = name;
        let result = self.save_asset(&asset);
        self.insert_asset(asset);
        result
    }

    pub fn move_assets(&mut self, ids: &[String], folder: Option<String>) -> Result<(), String> {
        for index in 0..self.assets.len() {
            if ids.contains(&self.assets[index].id) && self.assets[index].folder != folder {
                self.assets[index].folder = folder.clone();
                self.save_asset(&self.assets[index])?;
            }
        }
        Ok(())
    }

    fn save_asset(&self, asset: &Asset) -> Result<(), String> {
        match &self.root {
            Some(root) => write_asset(root, asset),
            None => Ok(()),
        }
    }

    pub fn media_dir(&self) -> Option<PathBuf> {
        self.root.as_ref().map(|root| root.join("media"))
    }

    /// Looks for the missing files of the asset under `base_dir`, by name,
    /// and copies them in. Returns how many are still missing.
    pub fn relink_asset(&mut self, id: &str, base_dir: &Path) -> Result<usize, String> {
        let Some(media_dir) = self.media_dir() else {
            return Ok(0);
        };
        let Some(index) = self.assets.iter().position(|a| a.id == id) else {
            return Ok(0);
        };
        let mut missing = 0;
        for item in self.assets[index].payload.files_mut() {
            if item.path.is_file() {
                continue;
            }
            match find_file(base_dir, &original_name(&item.path)?) {
                Some(found) => {
                    item.path = copy_to_media(&media_dir, &found, item.content_hash)?;
                }
                None => missing += 1,
            }
        }
        self.save_asset(&self.assets[index])?;
        Ok(missing)
    }

    pub fn folder(&self, id: &str) -> Option<&LibraryFolder> {
        self.folders.iter().find(|f| f.id == id)
    }

    pub fn add_folder(&mut self, name: String, parent: Option<String>) -> Result<String, String> {
        let id = new_id();
        self.folders.push(LibraryFolder {
            id: id.clone(),
            name,
            parent,
        });
        self.save_folders()?;
        Ok(id)
    }

    pub fn rename_folder(&mut self, id: &str, name: String) -> Result<(), String> {
        if let Some(folder) = self.folders.iter_mut().find(|f| f.id == id) {
            folder.name = name;
        }
        self.save_folders()
    }

    /// The caller checks that `parent` is not inside `id`.
    pub fn move_folder(&mut self, id: &str, parent: Option<String>) -> Result<(), String> {
        if let Some(folder) = self.folders.iter_mut().find(|f| f.id == id) {
            folder.parent = parent;
        }
        self.save_folders()
    }

    /// As in the project's pool, its content moves up one level.
    pub fn remove_folder(&mut self, id: &str) -> Result<(), String> {
        let Some(index) = self.folders.iter().position(|f| f.id == id) else {
            return Ok(());
        };
        let removed = self.folders.remove(index);
        for folder in &mut self.folders {
            if folder.parent.as_deref() == Some(id) {
                folder.parent = removed.parent.clone();
            }
        }
        let contained: Vec<String> = self
            .assets
            .iter()
            .filter(|a| a.folder.as_deref() == Some(id))
            .map(|a| a.id.clone())
            .collect();
        self.move_assets(&contained, removed.parent)?;
        self.save_folders()
    }

    fn save_folders(&self) -> Result<(), String> {
        match &self.root {
            Some(root) => write_json(
                &root.join("folders.json"),
                &FoldersFile {
                    version: VERSION,
                    folders: self.folders.clone(),
                },
            ),
            None => Ok(()),
        }
    }

    /// Also deletes the copied files no other asset uses.
    pub fn remove_asset(&mut self, id: &str) -> Result<(), String> {
        let Some(index) = self.assets.iter().position(|a| a.id == id) else {
            return Ok(());
        };
        let removed = self.assets.remove(index);
        let Some(root) = &self.root else {
            return Ok(());
        };
        remove_file(&root.join("assets").join(format!("{id}.json")))?;
        let media_dir = root.join("media");
        for item in removed.payload.files() {
            let used = self
                .assets
                .iter()
                .any(|a| a.payload.files().any(|f| f.path == item.path));
            if !used && item.path.starts_with(&media_dir) {
                remove_file(&item.path)?;
                if let Some(dir) = item.path.parent()
                    && dir != media_dir
                {
                    let _ = std::fs::remove_dir(dir);
                }
            }
        }
        Ok(())
    }

    pub fn preset(&self, id: &str) -> Option<&AttributePreset> {
        self.presets.iter().find(|(i, _)| i == id).map(|(_, p)| p)
    }

    /// Id of the preset whose shortcut was pressed.
    pub fn preset_pressed(&self, input: &egui::InputState) -> Option<String> {
        self.presets
            .iter()
            .find(|(_, p)| p.shortcut.is_some_and(|s| s.pressed(input)))
            .map(|(id, _)| id.clone())
    }

    pub fn add_preset(&mut self, preset: AttributePreset) -> Result<String, String> {
        let id = new_id();
        self.presets.push((id.clone(), preset.clone()));
        self.update_preset(&id, |p| *p = preset)?;
        Ok(id)
    }

    /// Changes a preset and saves it. A shortcut taken by this preset is
    /// taken away from the others.
    pub fn update_preset(
        &mut self,
        id: &str,
        change: impl FnOnce(&mut AttributePreset),
    ) -> Result<(), String> {
        let Some(index) = self.presets.iter().position(|(i, _)| i == id) else {
            return Ok(());
        };
        change(&mut self.presets[index].1);
        let mut dirty = vec![index];
        if let Some(shortcut) = self.presets[index].1.shortcut {
            for (i, (_, other)) in self.presets.iter_mut().enumerate() {
                if i != index && other.shortcut == Some(shortcut) {
                    other.shortcut = None;
                    dirty.push(i);
                }
            }
        }
        let result = match &self.root {
            Some(root) => dirty.iter().try_for_each(|&i| {
                let (id, preset) = &self.presets[i];
                write_json(&root.join("presets").join(format!("{id}.json")), preset)
            }),
            None => Ok(()),
        };
        self.presets
            .sort_by_cached_key(|(_, p)| p.name.to_lowercase());
        result
    }

    pub fn remove_preset(&mut self, id: &str) -> Result<(), String> {
        self.presets.retain(|(i, _)| i != id);
        match &self.root {
            Some(root) => remove_file(&root.join("presets").join(format!("{id}.json"))),
            None => Ok(()),
        }
    }
}

/// Copies each file into `media/`. A missing file fails the whole asset:
/// stored, it would only come back offline.
/// An asset of the media file at `path`, not copied yet.
fn file_asset(
    path: &Path,
    folder: Option<String>,
    origin: ClipboardOrigin,
) -> Result<Asset, String> {
    let meta = vv_media::probe_media(path).map_err(|e| e.to_string())?;
    let mut project = Project::default();
    let id = project.media_pool.insert(MediaItem {
        path: path.to_path_buf(),
        meta,
        // `0` only if the file vanished meanwhile: the copy then fails anyway.
        content_hash: vv_media::content_fingerprint(path).unwrap_or(0),
        compound: None,
        folder: None,
    });
    Ok(Asset {
        id: new_id(),
        name: vv_session::file_label(path),
        folder,
        payload: ClipboardPayload::new(origin, &project, &[id], &[]),
    })
}

fn store_asset(root: &Path, mut asset: Asset) -> Result<Asset, String> {
    let media_dir = root.join("media");
    for item in asset.payload.files_mut() {
        if !item.path.is_file() {
            return Err(t!("library.file_missing", path = item.path.display()).into_owned());
        }
        if !item.path.starts_with(&media_dir) {
            item.path = copy_to_media(&media_dir, &item.path, item.content_hash)?;
        }
    }
    write_asset(root, &asset)?;
    Ok(asset)
}

/// Into `media/<content hash>/`, so two assets sharing a file share the
/// copy and the file keeps its name.
fn copy_to_media(media_dir: &Path, src: &Path, content_hash: u64) -> Result<PathBuf, String> {
    let dir = media_dir.join(format!("{content_hash:016x}"));
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let dest = dir.join(original_name(src)?);
    if !dest.exists() {
        copy_file(src, &dest, &|_| true)?;
    }
    Ok(dest)
}

/// Through a temporary name: a copy cut short must not look done.
/// `progress` gets the bytes of each chunk and returns `false` to stop.
fn copy_file(src: &Path, dest: &Path, progress: &dyn Fn(u64) -> bool) -> Result<(), String> {
    use std::io::{Read, Write};
    let mut partial_name = dest.file_name().unwrap_or_default().to_owned();
    partial_name.push(".partial");
    let partial = dest.with_file_name(partial_name);
    let src_error = |e: std::io::Error| format!("{}: {e}", src.display());
    let dest_error = |e: std::io::Error| format!("{}: {e}", partial.display());
    let mut reader = std::fs::File::open(src).map_err(src_error)?;
    let mut writer = std::fs::File::create(&partial).map_err(dest_error)?;
    let mut buffer = vec![0; 1 << 20];
    loop {
        let read = reader.read(&mut buffer).map_err(src_error)?;
        if read == 0 {
            break;
        }
        writer.write_all(&buffer[..read]).map_err(dest_error)?;
        if !progress(read as u64) {
            drop(writer);
            let _ = std::fs::remove_file(&partial);
            return Err(format!("{}: cancelled", src.display()));
        }
    }
    drop(writer);
    std::fs::rename(&partial, dest).map_err(|e| e.to_string())
}

/// The file name without the `<content hash>-` prefix the first library
/// copies were given.
fn original_name(path: &Path) -> Result<String, String> {
    let name = path
        .file_name()
        .ok_or_else(|| format!("{}: not a file", path.display()))?
        .to_string_lossy();
    let hashed = name.len() > 17
        && name.as_bytes()[16] == b'-'
        && name[..16].bytes().all(|b| b.is_ascii_hexdigit());
    Ok(if hashed { &name[17..] } else { &name }.to_owned())
}

/// A copy of a library file in `dir`, for a project: an identical file
/// already there is reused, a different one with the same name gets
/// " (2)", " (3)", ... appended. `progress` as in `copy_file`.
pub(crate) fn copy_for_project(
    src: &Path,
    dir: &Path,
    progress: &dyn Fn(u64) -> bool,
) -> Result<PathBuf, String> {
    let name = original_name(src)?;
    let size = std::fs::metadata(src)
        .map_err(|e| format!("{}: {e}", src.display()))?
        .len();
    std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let (stem, ext) = match name.rsplit_once('.') {
        Some((stem, ext)) if !stem.is_empty() => (stem.to_owned(), format!(".{ext}")),
        _ => (name.clone(), String::new()),
    };
    for n in 1.. {
        let dest = match n {
            1 => dir.join(&name),
            n => dir.join(format!("{stem} ({n}){ext}")),
        };
        match std::fs::metadata(&dest) {
            Ok(meta) if meta.len() == size => return Ok(dest),
            Ok(_) => continue,
            Err(_) => {
                copy_file(src, &dest, progress)?;
                return Ok(dest);
            }
        }
    }
    unreachable!("infinite candidates")
}

/// The first file named `name` under `dir`, at any depth.
fn find_file(dir: &Path, name: &str) -> Option<PathBuf> {
    let mut dirs = vec![dir.to_owned()];
    while let Some(dir) = dirs.pop() {
        for entry in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
            let path = entry.path();
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            if kind.is_dir() {
                dirs.push(path);
            } else if kind.is_file() && entry.file_name().to_string_lossy() == name {
                return Some(path);
            }
        }
    }
    None
}

fn write_asset(root: &Path, asset: &Asset) -> Result<(), String> {
    write_json(
        &root.join("assets").join(format!("{}.json", asset.id)),
        &AssetFile {
            version: VERSION,
            name: asset.name.clone(),
            folder: asset.folder.clone(),
            payload: asset.payload.to_text(),
        },
    )
}

fn write_json(path: &Path, value: &impl Serialize) -> Result<(), String> {
    let text = serde_json::to_string_pretty(value).map_err(|e| e.to_string())?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    std::fs::write(path, text).map_err(|e| format!("{}: {e}", path.display()))
}

fn remove_file(path: &Path) -> Result<(), String> {
    match std::fs::remove_file(path) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
            Err(format!("{}: {e}", path.display()))
        }
        _ => Ok(()),
    }
}

/// `(file stem, content)` of the `.json` files in `dir`.
fn json_files(dir: &Path) -> impl Iterator<Item = (String, String)> {
    std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|entry| {
            let path = entry.path();
            if path.extension()? != "json" {
                return None;
            }
            let id = path.file_stem()?.to_string_lossy().into_owned();
            Some((id, std::fs::read_to_string(&path).ok()?))
        })
}

/// Entries written by a newer Venturi are skipped, not misread.
fn parse<T: serde::de::DeserializeOwned>(id: &str, text: &str) -> Option<T> {
    #[derive(Deserialize)]
    struct Versioned {
        version: u32,
    }
    match serde_json::from_str::<Versioned>(text) {
        Ok(v) if v.version > VERSION => {
            eprintln!("library entry {id}: version {} is newer", v.version);
            return None;
        }
        Err(e) => {
            eprintln!("library entry {id}: {e}");
            return None;
        }
        Ok(_) => {}
    }
    serde_json::from_str(text)
        .map_err(|e| eprintln!("library entry {id}: {e}"))
        .ok()
}

fn new_id() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos() as u64);
    format!("{nanos:x}-{:x}", COUNTER.fetch_add(1, Ordering::Relaxed))
}

#[cfg(test)]
#[path = "tests/library.rs"]
mod tests;
