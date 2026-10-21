use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use vv_core::{EffectId, PluginState, Vst3Effect, canonical_class_id};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PluginInfo {
    #[serde(deserialize_with = "vv_core::deserialize_class_id")]
    pub class_id: String,
    pub name: String,
    pub vendor: String,
    pub version: String,
    pub path: PathBuf,
}

impl PluginInfo {
    /// A new effect for this plugin, with no saved state.
    pub fn effect(&self) -> Vst3Effect {
        Vst3Effect {
            id: EffectId::fresh(),
            class_id: canonical_class_id(&self.class_id),
            name: self.name.clone(),
            vendor: self.vendor.clone(),
            version: self.version.clone(),
            component_state: PluginState::default(),
            controller_state: PluginState::default(),
        }
    }
}

pub fn default_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    let home = std::env::var_os("HOME").map(PathBuf::from);
    if cfg!(target_os = "macos") {
        // User dirs first: the first copy of a class id wins the scan dedupe.
        dirs.extend(home.map(|h| h.join("Library/Audio/Plug-Ins/VST3")));
        dirs.push(PathBuf::from("/Library/Audio/Plug-Ins/VST3"));
    } else {
        dirs.extend(home.map(|h| h.join(".vst3")));
        dirs.extend(["/usr/lib/vst3", "/usr/lib64/vst3", "/usr/local/lib/vst3"].map(PathBuf::from));
    }
    dirs
}

/// Effects only (category "Audio Module Class", subcategories without "Instrument").
pub fn scan(dirs: &[PathBuf]) -> Vec<PluginInfo> {
    unique_classes(bundles_in(dirs).iter().flat_map(|b| scan_bundle(b, None)))
}

/// Like `scan`, but a bundle that has to be loaded is first named in `marker`,
/// so a crash inside it is blamed on the next run. The caller consumes a
/// leftover marker with `take_crashed` and passes the result in `blocklist`.
pub fn scan_with_guard(dirs: &[PathBuf], marker: &Path, blocklist: &[PathBuf]) -> Vec<PluginInfo> {
    let infos = unique_classes(
        bundles_in(dirs)
            .iter()
            .filter(|b| !blocklist.contains(b))
            .take_while(|_| !is_abandoned(marker))
            .flat_map(|b| scan_bundle(b, Some(marker))),
    );
    if !is_abandoned(marker) {
        let _ = std::fs::remove_file(marker);
    }
    infos
}

/// A load this long at quit is a hang, not a slow plugin: it stays blamed.
const HANG_AFTER: Duration = Duration::from_secs(30);

#[derive(Default)]
struct ScanState {
    abandoned: Vec<PathBuf>,
    loading: Vec<(PathBuf, Instant)>,
}

/// One lock makes "write the marker" and "abandon" atomic, so a quitting app
/// cannot be left with a marker written after it was removed.
static STATE: Mutex<ScanState> = Mutex::new(ScanState {
    abandoned: Vec::new(),
    loading: Vec::new(),
});

fn lock_state() -> std::sync::MutexGuard<'static, ScanState> {
    STATE.lock().unwrap_or_else(|e| e.into_inner())
}

fn is_abandoned(marker: &Path) -> bool {
    lock_state().abandoned.iter().any(|m| m == marker)
}

/// For an orderly quit while a scan runs: the process is not crashing, so a
/// bundle still loading must not be blamed by the next launch. A real crash
/// never gets here; a load that has hung for 30 s keeps its marker.
pub fn abandon_scan(marker: &Path) {
    abandon_scan_at(marker, Instant::now(), HANG_AFTER);
}

fn abandon_scan_at(marker: &Path, now: Instant, hang_after: Duration) {
    let mut state = lock_state();
    state.abandoned.push(marker.to_path_buf());
    let hung = state
        .loading
        .iter()
        .any(|(m, since)| m == marker && now.saturating_duration_since(*since) >= hang_after);
    if !hung {
        let _ = std::fs::remove_file(marker);
    }
}

/// The bundle a previous scan died in, if any; consumes the marker. The app
/// calls it before scanning to persist the block even if this scan crashes too.
pub fn take_crashed(marker: &Path) -> Option<PathBuf> {
    let bytes = std::fs::read(marker).ok()?;
    let _ = std::fs::remove_file(marker);
    let crashed = path_from_bytes(bytes);
    eprintln!("VST3 scan crashed in {}, skipping it", crashed.display());
    Some(crashed)
}

#[cfg(unix)]
fn path_to_bytes(path: &Path) -> Vec<u8> {
    use std::os::unix::ffi::OsStrExt;
    path.as_os_str().as_bytes().to_vec()
}

#[cfg(unix)]
fn path_from_bytes(bytes: Vec<u8>) -> PathBuf {
    use std::os::unix::ffi::OsStringExt;
    PathBuf::from(std::ffi::OsString::from_vec(bytes))
}

#[cfg(not(unix))]
fn path_to_bytes(path: &Path) -> Vec<u8> {
    path.to_string_lossy().into_owned().into_bytes()
}

#[cfg(not(unix))]
fn path_from_bytes(bytes: Vec<u8>) -> PathBuf {
    PathBuf::from(String::from_utf8_lossy(&bytes).into_owned())
}

fn bundles_in(dirs: &[PathBuf]) -> Vec<PathBuf> {
    let mut bundles = Vec::new();
    let mut seen = HashSet::new();
    for dir in dirs {
        collect_bundles(dir, 0, &mut seen, &mut bundles);
    }
    bundles
}

const MAX_DEPTH: usize = 8;

// Vendor sub-folders are common; bundles are never descended into. `seen`
// holds canonical paths of directories and bundles, so aliased roots and
// symlink loops are walked once; the first-seen path is the one reported.
fn collect_bundles(dir: &Path, depth: usize, seen: &mut HashSet<PathBuf>, out: &mut Vec<PathBuf>) {
    if depth > MAX_DEPTH {
        return;
    }
    let Ok(canonical) = dir.canonicalize() else {
        return;
    };
    if !seen.insert(canonical) {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut paths: Vec<PathBuf> = entries.flatten().map(|e| e.path()).collect();
    paths.sort();
    for path in paths {
        if path.extension().is_some_and(|e| e == "vst3") {
            if path.canonicalize().is_ok_and(|c| seen.insert(c)) {
                out.push(path);
            }
        } else if path.is_dir() {
            collect_bundles(&path, depth + 1, seen, out);
        }
    }
}

/// The class id is the plugin's identity: keep the first of each.
fn unique_classes(infos: impl Iterator<Item = PluginInfo>) -> Vec<PluginInfo> {
    let mut seen = HashSet::new();
    infos.filter(|i| seen.insert(i.class_id.clone())).collect()
}

fn scan_bundle(bundle: &Path, marker: Option<&Path>) -> Vec<PluginInfo> {
    let info_path = bundle.join("Contents/Resources/moduleinfo.json");
    if let Some(infos) = std::fs::read_to_string(info_path)
        .ok()
        .and_then(|json| parse_module_info(&json, bundle))
    {
        return infos;
    }
    if let Some(marker) = marker {
        let mut state = lock_state();
        if state.abandoned.iter().any(|m| m == marker) {
            return Vec::new();
        }
        state.loading.retain(|(m, _)| m != marker);
        state.loading.push((marker.to_path_buf(), Instant::now()));
        if let Err(e) = std::fs::write(marker, path_to_bytes(bundle)) {
            static WARNED: std::sync::Once = std::sync::Once::new();
            WARNED.call_once(|| eprintln!("VST3 scan crash marker not writable ({e}): unguarded"));
        }
    }
    // A broken plugin must not hide the others.
    let infos = crate::module::module(bundle)
        .map(|m| m.effect_classes(bundle))
        .unwrap_or_default();
    if let Some(marker) = marker {
        let mut state = lock_state();
        state.loading.retain(|(m, _)| m != marker);
        if !state.abandoned.iter().any(|m| m == marker) {
            let _ = std::fs::remove_file(marker);
        }
    }
    infos
}

#[derive(Deserialize)]
struct ModuleInfo {
    #[serde(rename = "Version", default)]
    version: String,
    #[serde(rename = "Factory Info", default)]
    factory: FactoryInfo,
    #[serde(rename = "Classes", default)]
    classes: Vec<ClassInfo>,
}

#[derive(Deserialize, Default)]
struct FactoryInfo {
    #[serde(rename = "Vendor", default)]
    vendor: String,
}

#[derive(Deserialize)]
struct ClassInfo {
    #[serde(rename = "CID")]
    cid: String,
    #[serde(rename = "Category", default)]
    category: String,
    #[serde(rename = "Name", default)]
    name: String,
    #[serde(rename = "Vendor")]
    vendor: Option<String>,
    #[serde(rename = "Version")]
    version: Option<String>,
    #[serde(rename = "Sub Categories", default)]
    sub_categories: Vec<String>,
}

pub(crate) const AUDIO_CLASS: &str = "Audio Module Class";

pub(crate) fn is_effect(category: &str, sub_categories: &[String]) -> bool {
    category == AUDIO_CLASS && !sub_categories.iter().any(|s| s.contains("Instrument"))
}

pub(crate) fn parse_module_info(json: &str, path: &Path) -> Option<Vec<PluginInfo>> {
    let info = serde_json::from_str::<ModuleInfo>(&strip_trailing_commas(json)).ok()?;
    let infos = info
        .classes
        .into_iter()
        .filter(|c| is_effect(&c.category, &c.sub_categories))
        .map(|c| PluginInfo {
            class_id: canonical_class_id(&c.cid),
            name: c.name,
            vendor: c.vendor.unwrap_or_else(|| info.factory.vendor.clone()),
            version: c.version.unwrap_or_else(|| info.version.clone()),
            path: path.to_path_buf(),
        })
        .collect();
    Some(infos)
}

fn strip_trailing_commas(json: &str) -> String {
    let chars: Vec<char> = json.chars().collect();
    let mut out = String::with_capacity(json.len());
    let (mut in_str, mut escaped) = (false, false);
    for (i, &c) in chars.iter().enumerate() {
        if in_str {
            in_str = !(c == '"' && !escaped);
            escaped = c == '\\' && !escaped;
        } else if c == '"' {
            in_str = true;
        } else if c == ',' {
            let next = chars[i + 1..].iter().find(|c| !c.is_whitespace());
            if matches!(next, Some(']' | '}')) {
                continue;
            }
        }
        out.push(c);
    }
    out
}

#[cfg(test)]
#[path = "tests/scan.rs"]
mod tests;
