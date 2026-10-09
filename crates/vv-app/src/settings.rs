//! User settings of the program (not of the project), saved in
//! `~/.config/venturi/settings.json`.

use std::borrow::Cow;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use vv_media::audio_file::AudioFileFormat;
use vv_media::proxy::ProxyQuality;

use crate::i18n::Language;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Action {
    TogglePlayback,
    FastPlayback,
    StepBackward,
    StepForward,
    MarkIn,
    MarkOut,
    FullscreenViewer,
    ViewerZoomFit,
    ViewerZoomActual,
    Undo,
    Redo,
    Copy,
    Cut,
    Paste,
    PasteAttributes,
    Delete,
    RippleDelete,
    Split,
    ToggleDisabled,
    RetimeControls,
    SelectAll,
    SelectFromPlayhead,
    Rename,
    NewProject,
    OpenProject,
    SaveProject,
    SaveProjectAs,
    ImportMedia,
    Export,
    OpenSettings,
    ZoomIn,
    ZoomOut,
    AddMarker,
    SelectTool,
    SlipTool,
}

impl Action {
    pub const ALL: [Action; 35] = [
        Action::TogglePlayback,
        Action::FastPlayback,
        Action::StepBackward,
        Action::StepForward,
        Action::MarkIn,
        Action::MarkOut,
        Action::FullscreenViewer,
        Action::ViewerZoomFit,
        Action::ViewerZoomActual,
        Action::Undo,
        Action::Redo,
        Action::Copy,
        Action::Cut,
        Action::Paste,
        Action::PasteAttributes,
        Action::Delete,
        Action::RippleDelete,
        Action::Split,
        Action::ToggleDisabled,
        Action::RetimeControls,
        Action::SelectAll,
        Action::SelectFromPlayhead,
        Action::Rename,
        Action::NewProject,
        Action::OpenProject,
        Action::SaveProject,
        Action::SaveProjectAs,
        Action::ImportMedia,
        Action::Export,
        Action::OpenSettings,
        Action::ZoomIn,
        Action::ZoomOut,
        Action::AddMarker,
        Action::SelectTool,
        Action::SlipTool,
    ];

    /// Key in the settings file: must never be changed.
    pub fn id(self) -> &'static str {
        match self {
            Action::TogglePlayback => "toggle_playback",
            Action::FastPlayback => "fast_playback",
            Action::StepBackward => "step_backward",
            Action::StepForward => "step_forward",
            Action::MarkIn => "mark_in",
            Action::MarkOut => "mark_out",
            Action::FullscreenViewer => "fullscreen_viewer",
            Action::ViewerZoomFit => "viewer_zoom_fit",
            Action::ViewerZoomActual => "viewer_zoom_actual",
            Action::Undo => "undo",
            Action::Redo => "redo",
            Action::Copy => "copy",
            Action::Cut => "cut",
            Action::Paste => "paste",
            Action::PasteAttributes => "paste_attributes",
            Action::Delete => "delete",
            Action::RippleDelete => "ripple_delete",
            Action::Split => "split",
            Action::ToggleDisabled => "toggle_disabled",
            Action::RetimeControls => "retime_controls",
            Action::SelectAll => "select_all",
            Action::SelectFromPlayhead => "select_from_playhead",
            Action::Rename => "rename",
            Action::NewProject => "new_project",
            Action::OpenProject => "open_project",
            Action::SaveProject => "save_project",
            Action::SaveProjectAs => "save_project_as",
            Action::ImportMedia => "import_media",
            Action::Export => "export",
            Action::OpenSettings => "open_settings",
            Action::ZoomIn => "zoom_in",
            Action::ZoomOut => "zoom_out",
            Action::AddMarker => "add_marker",
            Action::SelectTool => "select_tool",
            Action::SlipTool => "slip_tool",
        }
    }

    pub fn label(self) -> Cow<'static, str> {
        t!(format!("action.{}", self.id()))
    }

    pub fn category(self) -> Cow<'static, str> {
        match self {
            Action::TogglePlayback
            | Action::FastPlayback
            | Action::StepBackward
            | Action::StepForward
            | Action::MarkIn
            | Action::MarkOut
            | Action::FullscreenViewer
            | Action::ViewerZoomFit
            | Action::ViewerZoomActual => t!("action_category.playback"),
            Action::Undo
            | Action::Redo
            | Action::Copy
            | Action::Cut
            | Action::Paste
            | Action::PasteAttributes
            | Action::Delete
            | Action::RippleDelete
            | Action::Split
            | Action::ToggleDisabled
            | Action::RetimeControls
            | Action::SelectAll
            | Action::SelectFromPlayhead
            | Action::Rename => t!("action_category.edit"),
            Action::NewProject
            | Action::OpenProject
            | Action::SaveProject
            | Action::SaveProjectAs
            | Action::ImportMedia
            | Action::Export
            | Action::OpenSettings => t!("action_category.file"),
            Action::ZoomIn
            | Action::ZoomOut
            | Action::AddMarker
            | Action::SelectTool
            | Action::SlipTool => {
                t!("action_category.timeline")
            }
        }
    }

    fn default_shortcuts(self) -> Vec<Shortcut> {
        use egui::Key;
        let plain = Shortcut::plain;
        let ctrl = Shortcut::ctrl;
        let ctrl_shift = |key| Shortcut {
            shift: true,
            ..Shortcut::ctrl(key)
        };
        match self {
            Action::TogglePlayback => vec![plain(Key::Space)],
            Action::FastPlayback => vec![plain(Key::A)],
            Action::StepBackward => vec![plain(Key::ArrowLeft)],
            Action::StepForward => vec![plain(Key::ArrowRight)],
            Action::MarkIn => vec![plain(Key::I)],
            Action::MarkOut => vec![plain(Key::O)],
            Action::FullscreenViewer => vec![ctrl(Key::F)],
            Action::ViewerZoomFit => vec![plain(Key::Z)],
            Action::ViewerZoomActual => vec![Shortcut {
                alt: true,
                shift: true,
                ..plain(Key::Z)
            }],
            Action::Undo => vec![ctrl(Key::Z)],
            Action::Redo => vec![ctrl_shift(Key::Z)],
            Action::Copy => vec![ctrl(Key::C)],
            Action::Cut => vec![ctrl(Key::X)],
            Action::Paste => vec![ctrl(Key::V)],
            Action::PasteAttributes => vec![Shortcut {
                alt: true,
                ..plain(Key::V)
            }],
            Action::Delete => vec![plain(Key::Delete), plain(Key::Backspace)],
            // ISO key between left Shift and Z ("<" on Italian layouts).
            Action::RippleDelete => vec![plain(Key::IntlBackslash)],
            Action::Split => vec![plain(Key::T)],
            Action::ToggleDisabled => vec![plain(Key::D)],
            Action::RetimeControls => vec![ctrl(Key::R)],
            Action::SelectAll => vec![ctrl(Key::A)],
            Action::SelectFromPlayhead => vec![Shortcut {
                alt: true,
                ..plain(Key::Y)
            }],
            Action::Rename => vec![plain(Key::F2)],
            Action::NewProject => vec![ctrl(Key::N)],
            Action::OpenProject => vec![ctrl(Key::O)],
            Action::SaveProject => vec![ctrl(Key::S)],
            Action::SaveProjectAs => vec![ctrl_shift(Key::S)],
            Action::ImportMedia => vec![ctrl(Key::I)],
            Action::Export => vec![ctrl_shift(Key::E)],
            Action::OpenSettings => vec![ctrl(Key::Comma)],
            // "=" is the unshifted "+" of US layouts.
            Action::ZoomIn => vec![ctrl(Key::Plus), ctrl(Key::Equals)],
            Action::ZoomOut => vec![ctrl(Key::Minus)],
            Action::AddMarker => vec![plain(Key::M)],
            Action::SelectTool => vec![plain(Key::V)],
            Action::SlipTool => vec![plain(Key::B)],
        }
    }

    fn from_id(id: &str) -> Option<Action> {
        Action::ALL.into_iter().find(|a| a.id() == id)
    }
}

/// `ctrl` is Cmd on macOS (`Modifiers::command`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Shortcut {
    pub key: egui::Key,
    pub ctrl: bool,
    pub shift: bool,
    pub alt: bool,
}

impl Shortcut {
    pub fn plain(key: egui::Key) -> Self {
        Self {
            key,
            ctrl: false,
            shift: false,
            alt: false,
        }
    }

    pub fn ctrl(key: egui::Key) -> Self {
        Self {
            ctrl: true,
            ..Self::plain(key)
        }
    }

    pub fn from_event(key: egui::Key, modifiers: egui::Modifiers) -> Self {
        Self {
            key,
            ctrl: modifiers.command,
            shift: modifiers.shift,
            alt: modifiers.alt,
        }
    }

    fn modifiers(self) -> egui::Modifiers {
        egui::Modifiers {
            alt: self.alt,
            shift: self.shift,
            command: self.ctrl,
            ..Default::default()
        }
    }

    fn modifiers_match(self, current: egui::Modifiers) -> bool {
        // On symbols Shift may only be needed to produce the key ("+" on US
        // layouts): there it is ignored if the shortcut does not ask for it.
        if is_symbol(self.key) {
            current.matches_logically(self.modifiers()) && (self.alt || !current.alt)
        } else {
            current.matches_exact(self.modifiers())
        }
    }

    /// Ctrl+C/X/V arrive from eframe as `Copy`/`Cut`/`Paste` events,
    /// not as key presses.
    fn clipboard_event(self) -> Option<fn(&egui::Event) -> bool> {
        if !self.ctrl || self.shift || self.alt {
            return None;
        }
        match self.key {
            egui::Key::C => Some(|e| matches!(e, egui::Event::Copy)),
            egui::Key::X => Some(|e| matches!(e, egui::Event::Cut)),
            egui::Key::V => Some(|e| matches!(e, egui::Event::Paste(_))),
            _ => None,
        }
    }

    pub fn pressed(self, input: &egui::InputState) -> bool {
        if let Some(is_event) = self.clipboard_event()
            && input.events.iter().any(is_event)
        {
            return true;
        }
        input.key_pressed(self.key) && self.modifiers_match(input.modifiers)
    }

    pub fn down(self, input: &egui::InputState) -> bool {
        input.key_down(self.key) && self.modifiers_match(input.modifiers)
    }

    /// Settings file format, e.g. `Ctrl+Shift+S`.
    fn to_config(self) -> String {
        self.join(self.key.name())
    }

    fn from_config(text: &str) -> Option<Self> {
        let mut parts: Vec<&str> = text.split('+').collect();
        // "Ctrl++": the last "+" is the key, not a separator.
        let key_name = if text.ends_with("++") {
            parts.truncate(parts.len() - 2);
            "+"
        } else {
            parts.pop()?
        };
        let mut shortcut = Self::plain(egui::Key::from_name(key_name)?);
        for part in parts {
            match part {
                "Ctrl" => shortcut.ctrl = true,
                "Shift" => shortcut.shift = true,
                "Alt" => shortcut.alt = true,
                _ => return None,
            }
        }
        Some(shortcut)
    }

    fn join(self, key: &str) -> String {
        let mut text = String::new();
        for (on, name) in [
            (self.ctrl, "Ctrl+"),
            (self.shift, "Shift+"),
            (self.alt, "Alt+"),
        ] {
            if on {
                text.push_str(name);
            }
        }
        text.push_str(key);
        text
    }
}

impl std::fmt::Display for Shortcut {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let key: Cow<str> = match self.key {
            egui::Key::IntlBackslash => "<".into(),
            egui::Key::Minus => "-".into(),
            egui::Key::ArrowLeft => "⬅".into(),
            egui::Key::ArrowRight => "➡".into(),
            egui::Key::ArrowUp => "⬆".into(),
            egui::Key::ArrowDown => "⬇".into(),
            egui::Key::Space => t!("key.space"),
            egui::Key::Delete => t!("key.delete"),
            key => key.symbol_or_name().into(),
        };
        f.write_str(&self.join(&key))
    }
}

fn is_symbol(key: egui::Key) -> bool {
    use egui::Key::*;
    matches!(
        key,
        Plus | Minus
            | Equals
            | Colon
            | Semicolon
            | Comma
            | Period
            | Slash
            | Backslash
            | Pipe
            | Questionmark
            | Exclamationmark
            | Quote
            | Backtick
            | OpenBracket
            | CloseBracket
            | OpenCurlyBracket
            | CloseCurlyBracket
    )
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Keymap {
    bindings: BTreeMap<Action, Vec<Shortcut>>,
}

impl Default for Keymap {
    fn default() -> Self {
        Self {
            bindings: Action::ALL
                .into_iter()
                .map(|a| (a, a.default_shortcuts()))
                .collect(),
        }
    }
}

impl Keymap {
    pub fn shortcuts(&self, action: Action) -> &[Shortcut] {
        self.bindings.get(&action).map_or(&[], Vec::as_slice)
    }

    pub fn pressed(&self, action: Action, input: &egui::InputState) -> bool {
        self.shortcuts(action).iter().any(|s| s.pressed(input))
    }

    pub fn down(&self, action: Action, input: &egui::InputState) -> bool {
        self.shortcuts(action).iter().any(|s| s.down(input))
    }

    /// `"text (shortcut)"`, or just `"text"` if the action has none.
    pub fn menu_label(&self, text: &str, action: Action) -> String {
        match self.shortcuts(action).first() {
            Some(shortcut) => format!("{text} ({shortcut})"),
            None => text.to_owned(),
        }
    }

    /// Assigns `shortcut` to `action` (replacing `slot`, or in addition if
    /// `None`), removing it from any other action. Returns the actions it
    /// was taken away from.
    pub fn assign(
        &mut self,
        action: Action,
        slot: Option<usize>,
        shortcut: Shortcut,
    ) -> Vec<Action> {
        let mut stolen = Vec::new();
        for (&other, shortcuts) in &mut self.bindings {
            if other != action && shortcuts.contains(&shortcut) {
                shortcuts.retain(|s| *s != shortcut);
                stolen.push(other);
            }
        }
        let shortcuts = self.bindings.entry(action).or_default();
        match slot.filter(|&i| i < shortcuts.len()) {
            Some(i) => shortcuts[i] = shortcut,
            None => shortcuts.push(shortcut),
        }
        let mut seen = Vec::new();
        shortcuts.retain(|s| {
            let first = !seen.contains(s);
            seen.push(*s);
            first
        });
        stolen
    }

    pub fn remove(&mut self, action: Action, slot: usize) {
        if let Some(shortcuts) = self.bindings.get_mut(&action)
            && slot < shortcuts.len()
        {
            shortcuts.remove(slot);
        }
    }
}

const MAX_RECENT_PROJECTS: usize = 10;

/// State of the UI panels (size, open/closed): saved to find the interface
/// as it was left on reopening.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PanelLayout {
    pub media_pool_open: bool,
    pub effects_open: bool,
    pub inspector_open: bool,
    pub keyframe_editor_open: bool,
    pub mixer_open: bool,
    pub color_window_open: bool,
    /// What the Color window's two scope slots show.
    pub color_scopes: [vv_render::ScopeKind; 2],
    pub left_column_width: f32,
    /// Share of the left column's height given to the media pool when the
    /// Effects panel is open too.
    pub media_pool_fraction: f32,
    pub inspector_width: f32,
    pub timeline_height: f32,
}

impl Default for PanelLayout {
    fn default() -> Self {
        Self {
            media_pool_open: true,
            effects_open: false,
            inspector_open: true,
            keyframe_editor_open: false,
            mixer_open: false,
            color_window_open: false,
            color_scopes: [
                vv_render::ScopeKind::Waveform,
                vv_render::ScopeKind::Vectorscope,
            ],
            left_column_width: 260.0,
            media_pool_fraction: 0.5,
            inspector_width: 300.0,
            timeline_height: 240.0,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Settings {
    pub keymap: Keymap,
    pub language: Language,
    /// Kinetic scrolling (inertia after a touchpad swipe) on the timeline.
    pub kinetic_scroll: bool,
    pub kinetic_scroll_media_pool: bool,
    /// Preview from the all-intra proxy once ready: smooth scrubbing on
    /// long-GOP sources. Export always uses the originals.
    pub proxy_enabled: bool,
    pub proxy_quality: ProxyQuality,
    /// Seconds buffered ahead of/behind the playhead.
    pub lookahead_secs: f64,
    pub behind_secs: f64,
    /// Memory budget for the decoded frame cache of every `RenderAhead`.
    pub cache_budget_bytes: usize,
    pub hw_decode: crate::hw_decode::HwDecodeMode,
    /// `None`: `hw_decode::default_budget_bytes`, which follows the RAM.
    pub hw_decode_budget_bytes: Option<usize>,
    /// Read at startup only, see `hw_decode::enable_intel_experimental_decode`.
    pub intel_experimental_decode: bool,
    /// Recently opened projects, most recent first.
    pub recent_projects: Vec<PathBuf>,
    pub panels: PanelLayout,
    /// Saved by the user, in the order they were first saved.
    pub compressor_presets: Vec<Preset<vv_core::MultibandCompressor>>,
    pub eq_presets: Vec<Preset<vv_core::Equalizer>>,
    /// Microphone of the voiceover, by name; `None` is the system's default.
    pub input_device: Option<String>,
    /// Of the takes; `None`, or one this FFmpeg lacks, is
    /// `AudioFileFormat::resolve`'s choice.
    pub recording_format: Option<AudioFileFormat>,
    /// Where the takes go; `None` is `Recordings` next to the project file.
    pub recording_dir: Option<PathBuf>,
    /// Serve MCP on a local socket for `vv-app mcp --attach`.
    pub mcp_enabled: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Preset<P> {
    pub name: String,
    pub params: P,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            keymap: Keymap::default(),
            language: Language::default(),
            kinetic_scroll: true,
            kinetic_scroll_media_pool: true,
            proxy_enabled: false,
            proxy_quality: ProxyQuality::default(),
            lookahead_secs: crate::render_ahead::DEFAULT_LOOKAHEAD_SECS,
            behind_secs: crate::render_ahead::DEFAULT_BEHIND_SECS,
            cache_budget_bytes: crate::DEFAULT_CACHE_BUDGET_BYTES,
            hw_decode: Default::default(),
            hw_decode_budget_bytes: None,
            intel_experimental_decode: false,
            recent_projects: Vec::new(),
            panels: PanelLayout::default(),
            compressor_presets: Vec::new(),
            eq_presets: Vec::new(),
            input_device: None,
            recording_format: None,
            recording_dir: None,
            mcp_enabled: false,
        }
    }
}

impl Settings {
    /// Quality of the proxies to use, `None` if they are off.
    pub fn proxy(&self) -> Option<ProxyQuality> {
        self.proxy_enabled.then_some(self.proxy_quality)
    }

    pub fn hw_decode_budget_bytes(&self) -> usize {
        self.hw_decode_budget_bytes
            .unwrap_or_else(crate::hw_decode::default_budget_bytes)
    }

    pub fn add_recent_project(&mut self, path: PathBuf) {
        self.recent_projects.retain(|p| p != &path);
        self.recent_projects.insert(0, path);
        self.recent_projects.truncate(MAX_RECENT_PROJECTS);
    }
}

#[derive(Serialize, Deserialize, Default)]
struct SettingsFile {
    #[serde(default)]
    shortcuts: BTreeMap<String, Vec<String>>,
    #[serde(default)]
    language: Option<String>,
    #[serde(default)]
    kinetic_scroll: Option<bool>,
    #[serde(default)]
    kinetic_scroll_media_pool: Option<bool>,
    /// Not `proxy_enabled`: that key was always saved as `true` when
    /// proxies were on by default, it does not reflect a user choice.
    #[serde(default)]
    use_proxies: Option<bool>,
    #[serde(default)]
    proxy_quality: Option<String>,
    #[serde(default)]
    lookahead_secs: Option<f64>,
    #[serde(default)]
    behind_secs: Option<f64>,
    #[serde(default)]
    cache_budget_mb: Option<u32>,
    #[serde(default)]
    hw_decode: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    hw_decode_budget_mb: Option<u32>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    intel_experimental_decode: bool,
    #[serde(default)]
    recent_projects: Vec<PathBuf>,
    #[serde(default)]
    panels: PanelLayoutFile,
    /// One by one: a preset that no longer reads is dropped alone, not
    /// with the whole file.
    #[serde(default)]
    compressor_presets: Vec<serde_json::Value>,
    #[serde(default)]
    eq_presets: Vec<serde_json::Value>,
    #[serde(default)]
    input_device: Option<String>,
    #[serde(default)]
    recording_format: Option<String>,
    #[serde(default)]
    recording_dir: Option<PathBuf>,
    #[serde(default)]
    mcp_enabled: Option<bool>,
}

#[derive(Serialize, Deserialize, Default)]
struct PanelLayoutFile {
    #[serde(default)]
    media_pool_open: Option<bool>,
    #[serde(default)]
    effects_open: Option<bool>,
    #[serde(default)]
    inspector_open: Option<bool>,
    #[serde(default)]
    keyframe_editor_open: Option<bool>,
    #[serde(default)]
    mixer_open: Option<bool>,
    #[serde(default)]
    color_window_open: Option<bool>,
    #[serde(default)]
    color_scopes: Option<[vv_render::ScopeKind; 2]>,
    #[serde(default)]
    left_column_width: Option<f32>,
    #[serde(default)]
    media_pool_fraction: Option<f32>,
    #[serde(default)]
    inspector_width: Option<f32>,
    #[serde(default)]
    timeline_height: Option<f32>,
}

impl Settings {
    pub fn default_path() -> Option<PathBuf> {
        let config = std::env::var_os("XDG_CONFIG_HOME")
            .filter(|d| !d.is_empty())
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
        Some(config.join("venturi").join("settings.json"))
    }

    /// Missing or unreadable file = default settings; actions absent from
    /// the file keep the default shortcuts.
    pub fn load(path: &Path) -> Self {
        let Ok(text) = std::fs::read_to_string(path) else {
            return Self::default();
        };
        let file: SettingsFile = match serde_json::from_str(&text) {
            Ok(file) => file,
            Err(e) => {
                eprintln!("invalid settings in {}: {e}", path.display());
                return Self::default();
            }
        };
        let mut settings = Self::default();
        settings.language = file
            .language
            .as_deref()
            .and_then(Language::from_id)
            .unwrap_or_default();
        settings.kinetic_scroll = file.kinetic_scroll.unwrap_or(true);
        settings.kinetic_scroll_media_pool = file.kinetic_scroll_media_pool.unwrap_or(true);
        settings.proxy_enabled = file.use_proxies.unwrap_or(settings.proxy_enabled);
        settings.proxy_quality = file
            .proxy_quality
            .as_deref()
            .and_then(ProxyQuality::from_id)
            .unwrap_or_default();
        settings.lookahead_secs = file.lookahead_secs.unwrap_or(settings.lookahead_secs);
        settings.behind_secs = file.behind_secs.unwrap_or(settings.behind_secs);
        settings.cache_budget_bytes = file
            .cache_budget_mb
            .map_or(settings.cache_budget_bytes, |mb| mb as usize * 1_000_000);
        settings.hw_decode = file
            .hw_decode
            .as_deref()
            .and_then(crate::hw_decode::HwDecodeMode::from_id)
            .unwrap_or_default();
        settings.hw_decode_budget_bytes =
            file.hw_decode_budget_mb.map(|mb| mb as usize * 1_000_000);
        settings.intel_experimental_decode = file.intel_experimental_decode;
        settings.recent_projects = file.recent_projects;
        settings.input_device = file.input_device;
        settings.recording_format = file
            .recording_format
            .as_deref()
            .and_then(AudioFileFormat::from_id);
        settings.recording_dir = file.recording_dir;
        settings.compressor_presets = readable_presets(file.compressor_presets);
        settings.eq_presets = readable_presets(file.eq_presets);
        settings.mcp_enabled = file.mcp_enabled.unwrap_or(false);
        let defaults = PanelLayout::default();
        settings.panels = PanelLayout {
            media_pool_open: file
                .panels
                .media_pool_open
                .unwrap_or(defaults.media_pool_open),
            effects_open: file.panels.effects_open.unwrap_or(defaults.effects_open),
            inspector_open: file
                .panels
                .inspector_open
                .unwrap_or(defaults.inspector_open),
            keyframe_editor_open: file
                .panels
                .keyframe_editor_open
                .unwrap_or(defaults.keyframe_editor_open),
            mixer_open: file.panels.mixer_open.unwrap_or(defaults.mixer_open),
            color_window_open: file
                .panels
                .color_window_open
                .unwrap_or(defaults.color_window_open),
            color_scopes: file.panels.color_scopes.unwrap_or(defaults.color_scopes),
            left_column_width: file
                .panels
                .left_column_width
                .unwrap_or(defaults.left_column_width),
            media_pool_fraction: file
                .panels
                .media_pool_fraction
                .unwrap_or(defaults.media_pool_fraction),
            inspector_width: file
                .panels
                .inspector_width
                .unwrap_or(defaults.inspector_width),
            timeline_height: file
                .panels
                .timeline_height
                .unwrap_or(defaults.timeline_height),
        };
        for (id, shortcuts) in file.shortcuts {
            let Some(action) = Action::from_id(&id) else {
                continue;
            };
            let parsed = shortcuts
                .iter()
                .filter_map(|s| Shortcut::from_config(s))
                .collect();
            settings.keymap.bindings.insert(action, parsed);
        }
        settings
    }

    pub fn save(&self, path: &Path) -> Result<(), String> {
        let file = SettingsFile {
            shortcuts: self
                .keymap
                .bindings
                .iter()
                .map(|(action, shortcuts)| {
                    (
                        action.id().to_owned(),
                        shortcuts.iter().map(|s| s.to_config()).collect(),
                    )
                })
                .collect(),
            language: Some(self.language.id().to_owned()),
            kinetic_scroll: Some(self.kinetic_scroll),
            kinetic_scroll_media_pool: Some(self.kinetic_scroll_media_pool),
            use_proxies: Some(self.proxy_enabled),
            proxy_quality: Some(self.proxy_quality.id().to_owned()),
            lookahead_secs: Some(self.lookahead_secs),
            behind_secs: Some(self.behind_secs),
            cache_budget_mb: Some((self.cache_budget_bytes / 1_000_000) as u32),
            hw_decode: Some(self.hw_decode.id().to_owned()),
            hw_decode_budget_mb: self
                .hw_decode_budget_bytes
                .map(|bytes| (bytes / 1_000_000) as u32),
            intel_experimental_decode: self.intel_experimental_decode,
            recent_projects: self.recent_projects.clone(),
            panels: PanelLayoutFile {
                media_pool_open: Some(self.panels.media_pool_open),
                effects_open: Some(self.panels.effects_open),
                inspector_open: Some(self.panels.inspector_open),
                keyframe_editor_open: Some(self.panels.keyframe_editor_open),
                mixer_open: Some(self.panels.mixer_open),
                color_window_open: Some(self.panels.color_window_open),
                color_scopes: Some(self.panels.color_scopes),
                left_column_width: Some(self.panels.left_column_width),
                media_pool_fraction: Some(self.panels.media_pool_fraction),
                inspector_width: Some(self.panels.inspector_width),
                timeline_height: Some(self.panels.timeline_height),
            },
            compressor_presets: preset_values(&self.compressor_presets),
            eq_presets: preset_values(&self.eq_presets),
            input_device: self.input_device.clone(),
            recording_format: self.recording_format.map(|f| f.id().to_owned()),
            recording_dir: self.recording_dir.clone(),
            mcp_enabled: Some(self.mcp_enabled),
        };
        let text = serde_json::to_string_pretty(&file).map_err(|e| e.to_string())?;
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        }
        std::fs::write(path, text).map_err(|e| e.to_string())
    }
}

fn readable_presets<P: serde::de::DeserializeOwned>(
    values: Vec<serde_json::Value>,
) -> Vec<Preset<P>> {
    values
        .into_iter()
        .filter_map(|preset| serde_json::from_value(preset).ok())
        .collect()
}

fn preset_values<P: Serialize>(presets: &[Preset<P>]) -> Vec<serde_json::Value> {
    presets
        .iter()
        .filter_map(|preset| serde_json::to_value(preset).ok())
        .collect()
}

#[cfg(test)]
#[path = "tests/settings.rs"]
mod tests;
