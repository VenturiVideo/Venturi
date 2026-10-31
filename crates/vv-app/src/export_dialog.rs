//! Export settings window, shown before starting the export.

use std::borrow::Cow;
use std::path::{Path, PathBuf};

use crate::properties_panel::preview_combo;
use vv_core::{FrameIdx, Rational};
use vv_media::{AudioCodec, HwDevice, VideoCodec, VideoSettings};

use crate::export::ExportSettings;
use crate::format_duration;

/// Timeline data the window shows but does not modify.
pub struct TimelineInfo {
    pub resolution: (u32, u32),
    pub fps: Rational,
    pub total_frames: FrameIdx,
    /// In/out range, `None` if it covers the whole timeline.
    pub marks: Option<(FrameIdx, FrameIdx)>,
    pub has_audio: bool,
    /// The first plugin scan runs and the timeline's audio plays plugins.
    pub waits_for_scan: bool,
}

pub enum ExportDialogAction {
    None,
    Cancel,
    Export {
        settings: ExportSettings,
        range: std::ops::Range<FrameIdx>,
    },
}

pub struct ExportDialog {
    settings: ExportSettings,
    path_text: String,
    whole_timeline: bool,
    /// Kept aside while "Include audio" is off, so they can be restored.
    audio_settings: vv_media::AudioSettings,
    /// The GPU decoders of this machine, Settings > Playback's first.
    gpu_decoders: Vec<HwDevice>,
    /// `None` decodes on the CPU.
    decoder: Option<HwDevice>,
    /// Switch to NVENC once its check passes, unless an encoder was picked
    /// meanwhile (`ExportSettings::preferred` without blocking the UI).
    nvenc_pending: bool,
    /// File dialog opened on a separate thread (on the GNOME/Wayland event
    /// loop thread it marks the app as unresponsive).
    browsing: Option<std::sync::mpsc::Receiver<Option<PathBuf>>>,
}

impl ExportDialog {
    pub fn new(settings: ExportSettings, gpu_decoders: Vec<HwDevice>) -> Self {
        Self {
            path_text: settings.output_path.display().to_string(),
            audio_settings: settings.audio.clone().unwrap_or_default(),
            decoder: match settings.hw_decode.first() {
                Some(last) if gpu_decoders.contains(last) => Some(last.clone()),
                // The one used last time is gone: another GPU still beats the CPU.
                Some(_) => gpu_decoders.first().cloned(),
                None => None,
            },
            gpu_decoders,
            settings,
            nvenc_pending: false,
            whole_timeline: false,
            browsing: None,
        }
    }

    /// With the settings of `ExportSettings::preferred`.
    pub fn preferred(output_path: PathBuf, gpu_decoders: Vec<HwDevice>) -> Self {
        let settings = ExportSettings::with_preferred_audio(output_path);
        Self {
            nvenc_pending: true,
            ..Self::new(settings, gpu_decoders)
        }
    }

    /// Starts the encoders' checks; `true` while some is still running.
    fn poll_encoder_checks(&mut self) -> bool {
        if self.nvenc_pending
            && let Some(available) = VideoCodec::Nvenc.availability()
        {
            self.nvenc_pending = false;
            if available {
                self.settings.video = VideoSettings::for_codec(VideoCodec::Nvenc);
            }
        }
        // Not `any`: every check must start now, not one after the other.
        let pending = VideoCodec::choices()
            .iter()
            .filter(|codec| codec.availability().is_none())
            .count();
        pending > 0
    }

    pub fn show(&mut self, ctx: &egui::Context, info: &TimelineInfo) -> ExportDialogAction {
        if self.poll_encoder_checks() {
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        }
        let mut action = ExportDialogAction::None;
        let mut open = true;
        egui::Window::new(t!("export.title"))
            .id(egui::Id::new("export_dialog"))
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .default_width(460.0)
            .show(ctx, |ui| {
                self.destination_section(ui);
                ui.separator();
                self.range_section(ui, info);
                ui.separator();
                self.video_section(ui, info);
                ui.separator();
                self.audio_section(ui, info);
                ui.separator();
                action = self.buttons(ui, info);
            });
        if !open {
            action = ExportDialogAction::Cancel;
        }
        action
    }

    fn destination_section(&mut self, ui: &mut egui::Ui) {
        ui.strong(t!("export.destination"));
        ui.horizontal(|ui| {
            ui.add(egui::TextEdit::singleline(&mut self.path_text).desired_width(340.0));
            if ui
                .add_enabled(
                    self.browsing.is_none(),
                    egui::Button::new(t!("export.browse")),
                )
                .clicked()
            {
                let current = PathBuf::from(&self.path_text);
                let (tx, rx) = std::sync::mpsc::channel();
                std::thread::spawn(move || {
                    let mut dialog = rfd::FileDialog::new().add_filter("mp4", &["mp4"]);
                    if let Some(dir) = current.parent().filter(|d| d.is_dir()) {
                        dialog = dialog.set_directory(dir);
                    }
                    if let Some(name) = current.file_name() {
                        dialog = dialog.set_file_name(name.to_string_lossy());
                    }
                    let _ = tx.send(dialog.save_file());
                });
                self.browsing = Some(rx);
            }
            if let Some(rx) = &self.browsing {
                match rx.try_recv() {
                    Ok(chosen) => {
                        if let Some(path) = chosen {
                            self.path_text = path.display().to_string();
                        }
                        self.browsing = None;
                    }
                    Err(std::sync::mpsc::TryRecvError::Empty) => {
                        ui.ctx()
                            .request_repaint_after(std::time::Duration::from_millis(100));
                    }
                    Err(std::sync::mpsc::TryRecvError::Disconnected) => self.browsing = None,
                }
            }
        });
        match validate_path(&self.path_text) {
            Ok(path) if path.exists() => {
                ui.colored_label(egui::Color32::YELLOW, t!("export.overwrite_warning"));
            }
            Ok(_) => {}
            Err(e) => {
                ui.colored_label(egui::Color32::RED, e);
            }
        }
    }

    fn range_section(&mut self, ui: &mut egui::Ui, info: &TimelineInfo) {
        ui.strong(t!("export.range"));
        let fps = info.fps.as_f64();
        match info.marks {
            Some((mark_in, mark_out)) => {
                ui.radio_value(
                    &mut self.whole_timeline,
                    false,
                    t!(
                        "export.range_in_out",
                        start = format_duration(mark_in, fps),
                        end = format_duration(mark_out, fps),
                        duration = format_duration(mark_out - mark_in, fps)
                    ),
                );
                ui.radio_value(
                    &mut self.whole_timeline,
                    true,
                    t!(
                        "export.range_whole",
                        duration = format_duration(info.total_frames, fps)
                    ),
                );
            }
            None => {
                ui.label(t!(
                    "export.range_whole",
                    duration = format_duration(info.total_frames, fps)
                ));
                ui.label(t!("export.range_mark_hint"));
            }
        }
    }

    fn video_section(&mut self, ui: &mut egui::Ui, info: &TimelineInfo) {
        ui.strong("Video");
        let video = &mut self.settings.video;
        egui::Grid::new("export_video_grid")
            .num_columns(2)
            .spacing([12.0, 6.0])
            .show(ui, |ui| {
                ui.label(t!("export.decoder"));
                let mut items = vec![(None, "CPU".to_owned(), true)];
                items.extend(self.gpu_decoders.iter().map(|device| {
                    (
                        Some(device.clone()),
                        format!("GPU ({})", crate::hw_decode::device_label(device)),
                        !vv_media::hw::known_unavailable(device),
                    )
                }));
                if self.gpu_decoders.is_empty() {
                    items.push((Some(HwDevice::Cuda), "GPU".to_owned(), false));
                }
                preview_combo(
                    ui,
                    "export_decoder",
                    &mut self.decoder,
                    &items,
                    None,
                    None,
                    Some(&t!("export.unavailable_on_system")),
                );
                ui.end_row();

                ui.label(t!("export.encoder"));
                let before = video.codec;
                let items: Vec<_> = VideoCodec::choices()
                    .iter()
                    .map(|codec| match codec.availability() {
                        Some(available) => {
                            (*codec, video_codec_label(*codec).to_string(), available)
                        }
                        None => (
                            *codec,
                            format!("{} – {}", video_codec_label(*codec), t!("export.checking")),
                            false,
                        ),
                    })
                    .collect();
                preview_combo(
                    ui,
                    "export_video_codec",
                    &mut video.codec,
                    &items,
                    None,
                    None,
                    Some(&t!("export.unavailable_on_system")),
                );
                if video.codec != before {
                    *video = VideoSettings::for_codec(video.codec);
                    self.nvenc_pending = false;
                }
                ui.end_row();

                if !video.codec.presets().is_empty() {
                    ui.label(t!("export.preset"))
                        .on_hover_text(t!("export.preset_hint"));
                    let items: Vec<_> = video
                        .codec
                        .presets()
                        .iter()
                        .map(|preset| (preset.to_string(), preset.to_string(), true))
                        .collect();
                    preview_combo(
                        ui,
                        "export_video_preset",
                        &mut video.preset,
                        &items,
                        None,
                        None,
                        None,
                    );
                    ui.end_row();
                }

                ui.label(match video.codec {
                    VideoCodec::X264 => t!("export.quality_crf"),
                    VideoCodec::Nvenc => t!("export.quality_cq"),
                    VideoCodec::Vulkan => t!("export.quality_qp"),
                    VideoCodec::VideoToolbox => t!("export.quality"),
                });
                let hint = if video.codec.quality_rises() {
                    t!("export.quality_rising_hint")
                } else {
                    t!("export.quality_hint")
                };
                ui.add(egui::Slider::new(
                    &mut video.quality,
                    video.codec.quality_range(),
                ))
                .on_hover_text(hint);
                ui.end_row();

                ui.label(t!("export.resolution"));
                let scale = &mut self.settings.scale_percent;
                let size_label = |percent: u32| {
                    let mut probe = ExportSettings::new(PathBuf::new());
                    probe.scale_percent = percent;
                    let (w, h) = probe.output_size(info.resolution);
                    format!("{w}×{h} ({percent}%)")
                };
                let items: Vec<_> = ExportSettings::SCALE_CHOICES
                    .iter()
                    .map(|percent| (*percent, size_label(*percent), true))
                    .collect();
                preview_combo(ui, "export_scale", scale, &items, None, None, None);
                ui.end_row();

                ui.label(t!("export.frame_rate"));
                ui.label(t!(
                    "export.fps_of_timeline",
                    fps = format!("{:.3}", info.fps.as_f64())
                ));
                ui.end_row();
            });
    }

    fn audio_section(&mut self, ui: &mut egui::Ui, info: &TimelineInfo) {
        ui.strong("Audio");
        if !info.has_audio {
            ui.label(t!("export.no_audio"));
            return;
        }
        let mut include = self.settings.audio.is_some();
        ui.checkbox(&mut include, t!("export.include_audio"));
        if !include {
            if let Some(audio) = self.settings.audio.take() {
                self.audio_settings = audio;
            }
            return;
        }
        let audio = self
            .settings
            .audio
            .get_or_insert_with(|| self.audio_settings.clone());
        egui::Grid::new("export_audio_grid")
            .num_columns(2)
            .spacing([12.0, 6.0])
            .show(ui, |ui| {
                ui.label(t!("export.encoder"));
                let items: Vec<_> = AudioCodec::ALL
                    .iter()
                    .map(|codec| {
                        (
                            *codec,
                            audio_codec_label(*codec).to_string(),
                            codec.is_available(),
                        )
                    })
                    .collect();
                preview_combo(
                    ui,
                    "export_audio_codec",
                    &mut audio.codec,
                    &items,
                    None,
                    None,
                    Some(&t!("export.unavailable_in_ffmpeg")),
                );
                ui.end_row();

                if audio.codec == AudioCodec::Aac {
                    ui.label(t!("export.preset"));
                    let items: Vec<_> = [false, true]
                        .into_iter()
                        .map(|fast| (fast, aac_coder_label(fast).to_string(), true))
                        .collect();
                    preview_combo(
                        ui,
                        "export_audio_coder",
                        &mut audio.fast_coder,
                        &items,
                        None,
                        None,
                        None,
                    );
                    ui.end_row();
                }

                ui.label("Bitrate");
                let items: Vec<_> = [96, 128, 160, 192, 256, 320]
                    .into_iter()
                    .map(|kbps| (kbps, format!("{kbps} kbps"), true))
                    .collect();
                preview_combo(
                    ui,
                    "export_audio_bitrate",
                    &mut audio.bitrate_kbps,
                    &items,
                    None,
                    None,
                    None,
                );
                ui.end_row();
            });
    }

    fn export_range(&self, info: &TimelineInfo) -> std::ops::Range<FrameIdx> {
        match info.marks {
            Some((mark_in, mark_out)) if !self.whole_timeline => mark_in..mark_out,
            _ => 0..info.total_frames,
        }
    }

    /// The chosen decoder, then the other GPUs as fallbacks for the files it
    /// refuses.
    fn selected_decoders(&self) -> Vec<HwDevice> {
        let Some(chosen) = &self.decoder else {
            return Vec::new();
        };
        std::iter::once(chosen.clone())
            .chain(self.gpu_decoders.iter().filter(|d| *d != chosen).cloned())
            .collect()
    }

    fn buttons(&mut self, ui: &mut egui::Ui, info: &TimelineInfo) -> ExportDialogAction {
        let path = validate_path(&self.path_text);
        let mut action = ExportDialogAction::None;
        let waits = info.waits_for_scan && self.settings.audio.is_some();
        ui.horizontal(|ui| {
            if ui
                .add_enabled(
                    path.is_ok() && !waits,
                    egui::Button::new(t!("export.start")),
                )
                .clicked()
                && let Ok(path) = path
            {
                let mut settings = self.settings.clone();
                settings.output_path = path;
                settings.hw_decode = self.selected_decoders();
                action = ExportDialogAction::Export {
                    settings,
                    range: self.export_range(info),
                };
            }
            if ui.button(t!("common.cancel")).clicked() {
                action = ExportDialogAction::Cancel;
            }
            if waits {
                ui.weak(t!("mixer.vst3_scanning"));
            }
        });
        action
    }
}

/// Valid output path, with the `.mp4` extension added if missing.
fn validate_path(text: &str) -> Result<PathBuf, Cow<'static, str>> {
    let text = text.trim();
    if text.is_empty() {
        return Err(t!("export.error_no_path"));
    }
    let mut path = PathBuf::from(text);
    if path.file_name().is_none() {
        return Err(t!("export.error_no_file_name"));
    }
    if !path
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("mp4"))
    {
        path.as_mut_os_string().push(".mp4");
    }
    let parent = path.parent().filter(|p| !p.as_os_str().is_empty());
    if parent.is_some_and(|p| !p.is_dir()) {
        return Err(t!("export.error_missing_folder"));
    }
    Ok(path)
}

/// Path proposed on the first export: "<project>-<timeline>.mp4", next to
/// the project if it was saved.
pub fn default_output_path(
    project_path: Option<&Path>,
    project_name: &str,
    timeline_name: &str,
) -> PathBuf {
    let dir = match project_path.and_then(Path::parent) {
        Some(dir) => dir.to_path_buf(),
        None => std::env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_default(),
    };
    let file_name: String = format!("{project_name}-{timeline_name}.mp4")
        .chars()
        .map(|c| if std::path::is_separator(c) { '_' } else { c })
        .collect();
    dir.join(file_name)
}

fn video_codec_label(codec: VideoCodec) -> &'static str {
    match codec {
        VideoCodec::X264 => "H.264 – x264 (CPU)",
        VideoCodec::Nvenc => "H.264 – NVENC (GPU NVIDIA)",
        VideoCodec::VideoToolbox => "H.264 – VideoToolbox (GPU)",
        VideoCodec::Vulkan => "H.264 – Vulkan (GPU)",
    }
}

fn audio_codec_label(codec: AudioCodec) -> Cow<'static, str> {
    match codec {
        AudioCodec::Aac => "AAC – ffmpeg".into(),
        AudioCodec::FdkAac => t!("export.codec_fdk"),
    }
}

fn aac_coder_label(fast: bool) -> Cow<'static, str> {
    if fast {
        t!("export.coder_fast")
    } else {
        t!("export.coder_quality")
    }
}

#[cfg(test)]
#[path = "tests/export_dialog.rs"]
mod tests;
