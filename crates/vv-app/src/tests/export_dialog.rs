use super::*;

#[test]
fn selecting_resolution_preserves_custom_edits_without_a_numeric_sentinel() {
    let mut dialog = ExportDialog::new(ExportSettings::new(PathBuf::from("out.mp4")), Vec::new());
    dialog.select_resolution(None, (8192, 4320));
    assert_eq!(dialog.settings.custom_size, Some((8192, 4320)));
    dialog.settings.custom_size = Some((10000, 2000));
    dialog.select_resolution(Some(75), (8192, 4320));
    assert_eq!(dialog.settings.custom_size, None);
    assert_eq!(dialog.settings.scale_percent, 75);
    dialog.select_resolution(None, (1920, 1080));
    assert_eq!(dialog.settings.custom_size, Some((10000, 2000)));
    dialog.select_resolution(Some(100), (16384, 16384));
    assert!(dialog.settings.valid_output_size((16384, 16384)));
}

fn video_frame(
    ctx: &egui::Context,
    dialog: &mut ExportDialog,
    info: &TimelineInfo,
    events: Vec<egui::Event>,
) -> egui::FullOutput {
    let raw = egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(
            egui::Pos2::ZERO,
            egui::vec2(640.0, 900.0),
        )),
        events,
        ..Default::default()
    };
    let mut output = ctx.run_ui(raw, |ui| dialog.video_section(ui, info));
    output.textures_delta.clear();
    output
}

fn label_center(output: &egui::FullOutput, label: &str) -> egui::Pos2 {
    fn find(shape: &egui::Shape, label: &str) -> Option<egui::Pos2> {
        match shape {
            egui::Shape::Text(text) if text.galley.text() == label => {
                Some(text.pos + text.galley.size() / 2.0)
            }
            egui::Shape::Vec(shapes) => shapes.iter().find_map(|shape| find(shape, label)),
            _ => None,
        }
    }
    output
        .shapes
        .iter()
        .find_map(|shape| find(&shape.shape, label))
        .unwrap_or_else(|| panic!("label {label:?} is not visible"))
}

fn video_click(
    ctx: &egui::Context,
    dialog: &mut ExportDialog,
    info: &TimelineInfo,
    pos: egui::Pos2,
) -> egui::FullOutput {
    video_frame(ctx, dialog, info, vec![egui::Event::PointerMoved(pos)]);
    video_frame(
        ctx,
        dialog,
        info,
        vec![egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed: true,
            modifiers: Default::default(),
        }],
    );
    video_frame(
        ctx,
        dialog,
        info,
        vec![egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed: false,
            modifiers: Default::default(),
        }],
    )
}

#[test]
fn cancelling_a_resolution_preview_preserves_the_custom_dimensions() {
    let info = TimelineInfo {
        resolution: (1920, 1080),
        fps: Rational::new(25, 1),
        total_frames: 25,
        marks: None,
        has_audio: false,
        waits_for_scan: false,
    };
    let mut settings = ExportSettings::new(std::env::temp_dir().join("custom.mp4"));
    settings.custom_size = Some((720, 1280));
    let mut dialog = ExportDialog::new(settings, Vec::new());
    let ctx = egui::Context::default();
    video_frame(&ctx, &mut dialog, &info, vec![]);
    let output = video_frame(&ctx, &mut dialog, &info, vec![]);
    let point = label_center(&output, &t!("export.custom_size"));
    video_click(&ctx, &mut dialog, &info, point);
    let output = video_frame(&ctx, &mut dialog, &info, vec![]);
    let preset = label_center(&output, "1920×1080 (100%)");
    video_frame(
        &ctx,
        &mut dialog,
        &info,
        vec![egui::Event::PointerMoved(preset)],
    );
    assert_eq!(dialog.settings.custom_size, None, "the preset is previewed");
    assert!(egui::Popup::is_any_open(&ctx));
    video_frame(
        &ctx,
        &mut dialog,
        &info,
        vec![egui::Event::Key {
            key: egui::Key::Escape,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: Default::default(),
        }],
    );
    video_frame(&ctx, &mut dialog, &info, vec![]);
    assert!(!egui::Popup::is_any_open(&ctx));
    assert_eq!(dialog.settings.custom_size, Some((720, 1280)));
}

#[test]
fn custom_dimensions_are_preserved_and_invalid_dimensions_disable_export() {
    let info = TimelineInfo {
        resolution: (1920, 1080),
        fps: Rational::new(25, 1),
        total_frames: 25,
        marks: None,
        has_audio: false,
        waits_for_scan: false,
    };
    for width in [721, 720] {
        let mut settings = ExportSettings::new(std::env::temp_dir().join("custom.mp4"));
        settings.custom_size = Some((width, 1280));
        settings.aspect = vv_session::export::AspectMode::Fill;
        let mut dialog = ExportDialog::new(settings, Vec::new());
        let ctx = egui::Context::default();
        let mut point = egui::Pos2::ZERO;
        let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
            assert!(matches!(
                dialog.buttons(ui, &info),
                ExportDialogAction::None
            ));
            point = egui::pos2(ui.min_rect().left() + 12.0, ui.min_rect().bottom() - 8.0);
        });
        output.textures_delta.clear();
        for pressed in [true, false] {
            let raw = egui::RawInput {
                events: vec![
                    egui::Event::PointerMoved(point),
                    egui::Event::PointerButton {
                        pos: point,
                        button: egui::PointerButton::Primary,
                        pressed,
                        modifiers: Default::default(),
                    },
                ],
                ..Default::default()
            };
            let mut output = ctx.run_ui(raw, |ui| {
                let action = dialog.buttons(ui, &info);
                if !pressed && width == 720 {
                    let ExportDialogAction::Export { settings, range } = action else {
                        panic!("a valid custom size must start the export on click");
                    };
                    assert_eq!(settings.custom_size, Some((720, 1280)));
                    assert_eq!(settings.aspect, vv_session::export::AspectMode::Fill);
                    assert_eq!(range, 0..25);
                } else {
                    assert!(matches!(action, ExportDialogAction::None));
                }
            });
            output.textures_delta.clear();
        }
    }
}

#[test]
fn validate_path_appends_mp4_and_rejects_missing_dirs() {
    let dir = std::env::temp_dir();
    let expected = dir.join("video.mp4");
    assert_eq!(
        validate_path(dir.join("video").to_str().unwrap()),
        Ok(expected.clone())
    );
    assert_eq!(validate_path(expected.to_str().unwrap()), Ok(expected));
    assert!(validate_path("").is_err());
    assert!(validate_path("/does/not/really/exist/video.mp4").is_err());
}

#[test]
fn export_uses_in_out_marks_unless_whole_timeline_is_chosen() {
    let info = TimelineInfo {
        resolution: (1920, 1080),
        fps: Rational::new(25, 1),
        total_frames: 100,
        marks: Some((20, 60)),
        has_audio: true,
        waits_for_scan: false,
    };
    let path = std::env::temp_dir().join("out.mp4");
    let mut dialog = ExportDialog::new(ExportSettings::new(path), Vec::new());
    assert_eq!(dialog.export_range(&info), 20..60);
    dialog.whole_timeline = true;
    assert_eq!(dialog.export_range(&info), 0..100);
}

#[test]
fn output_size_is_even_and_exact_at_full_scale() {
    let mut settings = ExportSettings::new(PathBuf::new());
    assert_eq!(settings.output_size((1920, 1080)), (1920, 1080));
    settings.scale_percent = 75;
    assert_eq!(settings.output_size((1920, 1080)), (1440, 810));
    settings.scale_percent = 25;
    assert_eq!(settings.output_size((1366, 768)), (340, 192));
}

#[test]
fn dialog_renders_without_starting_an_export_on_its_own() {
    let info = TimelineInfo {
        resolution: (1920, 1080),
        fps: Rational::new(60, 1),
        total_frames: 28_800,
        marks: Some((600, 1200)),
        has_audio: true,
        waits_for_scan: false,
    };
    let mut dialog = ExportDialog::new(
        ExportSettings::new(std::env::temp_dir().join("x.mp4")),
        Vec::new(),
    );
    let ctx = egui::Context::default();
    for _ in 0..3 {
        let mut action = None;
        let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
            action = Some(dialog.show(ui.ctx(), &info));
        });
        output.textures_delta.clear();
        assert!(matches!(action, Some(ExportDialogAction::None)));
    }
}

#[test]
fn gpu_decoding_is_kept_only_while_a_gpu_decoder_is_there() {
    let mut settings = ExportSettings::new(PathBuf::from("out.mp4"));
    settings.hw_decode = vec![HwDevice::Cuda];

    let dialog = ExportDialog::new(settings.clone(), vec![HwDevice::Vulkan(None)]);
    assert_eq!(dialog.selected_decoders(), [HwDevice::Vulkan(None)]);

    let dialog = ExportDialog::new(settings, Vec::new());
    assert!(dialog.selected_decoders().is_empty());

    let mut dialog = ExportDialog::new(
        ExportSettings::new(PathBuf::from("out.mp4")),
        vec![HwDevice::Cuda],
    );
    assert!(dialog.selected_decoders().is_empty());
    dialog.decoder = Some(HwDevice::Cuda);
    assert_eq!(dialog.selected_decoders(), [HwDevice::Cuda]);
}

#[test]
fn the_chosen_gpu_decoder_goes_first_and_is_remembered() {
    let gpus = vec![HwDevice::Cuda, HwDevice::Vulkan(None)];
    let mut dialog = ExportDialog::new(ExportSettings::new(PathBuf::from("out.mp4")), gpus.clone());
    dialog.decoder = Some(HwDevice::Vulkan(None));
    assert_eq!(
        dialog.selected_decoders(),
        [HwDevice::Vulkan(None), HwDevice::Cuda]
    );

    let mut settings = ExportSettings::new(PathBuf::from("out.mp4"));
    settings.hw_decode = dialog.selected_decoders();
    let dialog = ExportDialog::new(settings, gpus);
    assert_eq!(dialog.decoder, Some(HwDevice::Vulkan(None)));
}

fn wait_for_encoder_checks(dialog: &mut ExportDialog) {
    while dialog.poll_encoder_checks() {
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}

#[test]
fn preferred_dialog_switches_to_nvenc_only_once_it_passed_its_check() {
    let mut dialog = ExportDialog::preferred(PathBuf::from("out.mp4"), Vec::new());
    assert_eq!(dialog.settings.video.codec, VideoCodec::X264);
    wait_for_encoder_checks(&mut dialog);
    let nvenc = VideoCodec::Nvenc.is_available();
    assert_eq!(dialog.settings.video.codec == VideoCodec::Nvenc, nvenc);

    let mut dialog = ExportDialog::preferred(PathBuf::from("out.mp4"), Vec::new());
    dialog.nvenc_pending = false;
    wait_for_encoder_checks(&mut dialog);
    assert_eq!(dialog.settings.video.codec, VideoCodec::X264);
}

#[test]
fn default_output_path_names_project_and_timeline_next_to_the_project() {
    assert_eq!(
        default_output_path(Some(Path::new("/videos/trip.vvproj")), "trip", "Cut 2"),
        PathBuf::from("/videos/trip-Cut 2.mp4")
    );
    assert_eq!(
        default_output_path(Some(Path::new("/videos/trip.vvproj")), "trip", "a/b"),
        PathBuf::from("/videos/trip-a_b.mp4")
    );
}
