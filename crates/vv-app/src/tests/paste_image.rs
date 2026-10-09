use super::*;

fn temp_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("vv-paste-image-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn v_key(pressed: bool) -> egui::Event {
    egui::Event::Key {
        key: egui::Key::V,
        physical_key: None,
        pressed,
        repeat: false,
        modifiers: egui::Modifiers::COMMAND,
    }
}

fn feed(watch: &mut PasteKeyWatch, events: Vec<egui::Event>) -> Vec<egui::Event> {
    let mut raw = egui::RawInput {
        events,
        ..Default::default()
    };
    watch.feed(&mut raw);
    raw.events
}

fn pastes(events: &[egui::Event]) -> Vec<&str> {
    events
        .iter()
        .filter_map(|e| match e {
            egui::Event::Paste(text) => Some(text.as_str()),
            _ => None,
        })
        .collect()
}

#[test]
fn a_v_release_without_its_press_becomes_an_empty_paste() {
    let mut watch = PasteKeyWatch::default();
    let events = feed(&mut watch, vec![v_key(false)]);
    assert_eq!(pastes(&events), [""]);
}

#[test]
fn a_text_paste_is_not_doubled_on_release() {
    let mut watch = PasteKeyWatch::default();
    feed(&mut watch, vec![egui::Event::Paste("text".into())]);
    let events = feed(&mut watch, vec![v_key(false)]);
    assert!(pastes(&events).is_empty());
    // The next swallowed Ctrl+V is recognized again.
    let events = feed(&mut watch, vec![v_key(false)]);
    assert_eq!(pastes(&events), [""]);
}

#[test]
fn a_plain_v_is_not_a_paste() {
    let mut watch = PasteKeyWatch::default();
    feed(&mut watch, vec![v_key(true)]);
    let events = feed(&mut watch, vec![v_key(false)]);
    assert!(pastes(&events).is_empty());
}

#[test]
fn the_proposed_name_skips_the_taken_ones_in_any_format() {
    let dir = temp_dir("next-name");
    assert_eq!(next_image_name(&dir), "Image 001");
    std::fs::write(dir.join("Image 001.png"), b"").unwrap();
    std::fs::write(dir.join("Image 002.jpg"), b"").unwrap();
    assert_eq!(next_image_name(&dir), "Image 003");
    // A folder not created yet.
    assert_eq!(next_image_name(&dir.join("missing")), "Image 001");
}

#[test]
fn name_problem_rejects_empty_separators_and_existing_files() {
    let dir = temp_dir("problem");
    std::fs::write(dir.join("taken.png"), b"").unwrap();
    assert!(name_problem(&dir, "", "png").is_some());
    assert!(name_problem(&dir, "a/b", "png").is_some());
    assert!(name_problem(&dir, "..", "png").is_some());
    assert!(name_problem(&dir, "taken", "png").is_some());
    assert!(name_problem(&dir, "taken", "jpg").is_none());
    assert!(name_problem(&dir, "fresh", "png").is_none());
}

#[test]
fn save_image_creates_the_folder_and_never_overwrites() {
    let dir = temp_dir("save").join(PASTED_IMAGES_DIR);
    let image = ClipboardImage {
        bytes: vec![1, 2, 3],
        extension: "png",
    };
    let path = save_image(&dir, "Shot", &image).unwrap();
    assert_eq!(path, dir.join("Shot.png"));
    assert_eq!(std::fs::read(&path).unwrap(), [1, 2, 3]);
    assert!(save_image(&dir, "Shot", &image).is_err());
}

#[test]
fn an_image_without_a_saved_project_asks_to_save_first() {
    let mut app = VenturiApp::default();
    app.open_paste_image_dialog(ClipboardImage {
        bytes: vec![1],
        extension: "png",
    });
    assert!(app.paste_image_needs_save);
    assert!(app.paste_image_dialog.is_none());
}
