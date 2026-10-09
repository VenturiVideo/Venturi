//! Pasting an image from the system clipboard: saved under
//! `PASTED_IMAGES_DIR` next to the project file and added to the pool.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::mpsc;

use crate::{VenturiApp, theme};

/// Not translated: the project's paths must not depend on the UI language.
pub(crate) const PASTED_IMAGES_DIR: &str = "Pasted Images";

/// The clipboard formats we save as they are, best first.
pub(crate) const IMAGE_MIME_TYPES: [(&str, &str); 6] = [
    ("image/png", "png"),
    ("image/jpeg", "jpg"),
    ("image/webp", "webp"),
    ("image/gif", "gif"),
    ("image/bmp", "bmp"),
    ("image/tiff", "tif"),
];

pub(crate) struct ClipboardImage {
    pub bytes: Vec<u8>,
    pub extension: &'static str,
}

pub(crate) struct PasteImageDialog {
    pub name: String,
    image: ClipboardImage,
    dir: PathBuf,
    /// Until the name field has been shown once: it then takes focus with
    /// the whole name selected.
    just_opened: bool,
}

/// egui-winit swallows a Ctrl+V whose clipboard holds no text (an image):
/// neither the key press nor a `Paste` reach egui, only the V release.
/// That release is turned back into an empty `Paste` here.
#[derive(Default)]
pub(crate) struct PasteKeyWatch {
    v_down: bool,
    pasted: bool,
}

impl PasteKeyWatch {
    pub(crate) fn feed(&mut self, raw_input: &mut egui::RawInput) {
        let mut swallowed = false;
        for event in &raw_input.events {
            match event {
                egui::Event::Paste(_) => self.pasted = true,
                egui::Event::Key {
                    key: egui::Key::V,
                    pressed,
                    ..
                } => {
                    if *pressed {
                        self.v_down = true;
                    } else {
                        swallowed |= !self.v_down && !self.pasted;
                        *self = Self::default();
                    }
                }
                _ => {}
            }
        }
        if swallowed {
            raw_input.events.push(egui::Event::Paste(String::new()));
        }
    }
}

/// The first `Image NNN` not in `dir` yet, in any format.
pub(crate) fn next_image_name(dir: &Path) -> String {
    (1..)
        .map(|n| format!("Image {n:03}"))
        .find(|name| {
            IMAGE_MIME_TYPES
                .iter()
                .all(|(_, ext)| !dir.join(format!("{name}.{ext}")).exists())
        })
        .expect("an unbounded range")
}

/// Why `name` cannot become `<name>.<extension>` in `dir`, if it cannot.
pub(crate) fn name_problem(dir: &Path, name: &str, extension: &str) -> Option<String> {
    if name.is_empty() {
        return Some(t!("paste_image.name_empty").into_owned());
    }
    if name.contains(['/', '\\', '\0']) || name == "." || name == ".." {
        return Some(t!("paste_image.name_invalid").into_owned());
    }
    if dir.join(format!("{name}.{extension}")).exists() {
        return Some(t!("paste_image.name_taken").into_owned());
    }
    None
}

fn save_image(dir: &Path, name: &str, image: &ClipboardImage) -> std::io::Result<PathBuf> {
    std::fs::create_dir_all(dir)?;
    let path = dir.join(format!("{name}.{}", image.extension));
    // `create_new`: a file appeared after the check is not overwritten.
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)?
        .write_all(&image.bytes)?;
    Ok(path)
}

fn read_pipe(mut pipe: impl Read, extension: &'static str) -> Option<ClipboardImage> {
    let mut bytes = Vec::new();
    pipe.read_to_end(&mut bytes).ok()?;
    (!bytes.is_empty()).then_some(ClipboardImage { bytes, extension })
}

/// X11 and the rest: arboard hands out decoded RGBA, saved as PNG.
fn arboard_image() -> Option<ClipboardImage> {
    let image = arboard::Clipboard::new().ok()?.get_image().ok()?;
    let mut bytes = Vec::new();
    let mut encoder = png::Encoder::new(&mut bytes, image.width as u32, image.height as u32);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    encoder
        .write_header()
        .and_then(|mut writer| writer.write_image_data(&image.bytes))
        .ok()?;
    Some(ClipboardImage {
        bytes,
        extension: "png",
    })
}

impl VenturiApp {
    fn pasted_images_dir(&self) -> Option<PathBuf> {
        Some(self.session.path()?.parent()?.join(PASTED_IMAGES_DIR))
    }

    /// Reads the clipboard off the UI thread (the source app may take its
    /// time): `poll_clipboard_image` gets the outcome.
    pub(crate) fn request_clipboard_image(&mut self, ctx: &egui::Context) {
        let (tx, rx) = mpsc::channel();
        self.clipboard_image_rx = Some(rx);
        let ctx = ctx.clone();
        #[cfg(target_os = "linux")]
        if let Some(dnd) = &self.wayland_dnd {
            let source = dnd.clipboard_image();
            std::thread::spawn(move || {
                let _ = tx.send(source.and_then(|(pipe, ext)| read_pipe(pipe, ext)));
                ctx.request_repaint();
            });
            return;
        }
        std::thread::spawn(move || {
            let _ = tx.send(arboard_image());
            ctx.request_repaint();
        });
    }

    /// No image in the clipboard: the paste is the clips' one.
    pub(crate) fn poll_clipboard_image(&mut self) {
        let Some(rx) = &self.clipboard_image_rx else {
            return;
        };
        let image = match rx.try_recv() {
            Ok(image) => image,
            Err(mpsc::TryRecvError::Empty) => return,
            Err(mpsc::TryRecvError::Disconnected) => None,
        };
        self.clipboard_image_rx = None;
        match image {
            Some(image) => self.open_paste_image_dialog(image),
            None => self.paste_clipboard_at_playhead(),
        }
    }

    fn open_paste_image_dialog(&mut self, image: ClipboardImage) {
        let Some(dir) = self.pasted_images_dir() else {
            self.paste_image_needs_save = true;
            return;
        };
        self.paste_image_dialog = Some(PasteImageDialog {
            name: next_image_name(&dir),
            image,
            dir,
            just_opened: true,
        });
    }

    pub(crate) fn show_paste_image_dialog(&mut self, ctx: &egui::Context) {
        let Some(dialog) = &mut self.paste_image_dialog else {
            return;
        };
        let problem = name_problem(&dialog.dir, dialog.name.trim(), dialog.image.extension);
        let (mut add, mut cancel) = (false, false);
        let mut open = true;
        egui::Window::new(t!("paste_image.title"))
            .id(egui::Id::new("paste_image_dialog"))
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .default_width(360.0)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                ui.label(t!("paste_image.name"));
                let edit_id = egui::Id::new("paste_image_name");
                let mut output = egui::TextEdit::singleline(&mut dialog.name)
                    .id(edit_id)
                    .desired_width(f32::INFINITY)
                    .show(ui);
                if dialog.just_opened {
                    dialog.just_opened = false;
                    output.response.request_focus();
                    let end = egui::text::CCursor::new(dialog.name.chars().count());
                    output
                        .state
                        .cursor
                        .set_char_range(Some(egui::text::CCursorRange::two(
                            egui::text::CCursor::new(0),
                            end,
                        )));
                    output.state.store(ui.ctx(), edit_id);
                }
                if output.response.lost_focus() {
                    if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                        cancel = true;
                    } else if ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                        add = true;
                        if problem.is_some() {
                            output.response.request_focus();
                        }
                    }
                }
                match &problem {
                    Some(problem) => ui.colored_label(theme::ERROR, problem),
                    None => ui.weak(t!("paste_image.saved_in", dir = PASTED_IMAGES_DIR)),
                };
                ui.separator();
                ui.horizontal(|ui| {
                    add |= ui
                        .add_enabled(problem.is_none(), egui::Button::new(t!("paste_image.add")))
                        .clicked();
                    cancel |= ui.button(t!("common.cancel")).clicked();
                });
            });
        if !open || cancel {
            self.paste_image_dialog = None;
            return;
        }
        if !add || problem.is_some() {
            return;
        }
        let dialog = self.paste_image_dialog.take().expect("just checked");
        match save_image(&dialog.dir, dialog.name.trim(), &dialog.image) {
            Ok(path) => self.import_media(path),
            Err(e) => {
                self.project_error = Some(t!("paste_image.save_failed", error = e).into_owned())
            }
        }
    }

    /// Why the image cannot be pasted yet, with a way out.
    pub(crate) fn show_paste_image_needs_save(&mut self, ctx: &egui::Context) {
        if !self.paste_image_needs_save {
            return;
        }
        let (mut save, mut close) = (false, false);
        egui::Window::new(t!("paste_image.needs_save_title"))
            .id(egui::Id::new("paste_image_needs_save"))
            .collapsible(false)
            .resizable(false)
            .default_width(380.0)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                ui.label(t!("paste_image.needs_save", dir = PASTED_IMAGES_DIR));
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
            self.paste_image_needs_save = false;
        }
    }
}

#[cfg(test)]
#[path = "tests/paste_image.rs"]
mod tests;
