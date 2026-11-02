use super::*;

#[test]
fn uri_list_yields_decoded_local_paths() {
    let list = b"# comment\r\nfile:///home/me/a%20b.mp4\r\nfile://localhost/tmp/%C3%A8.wav\r\nhttps://x/y\r\n";
    assert_eq!(
        parse_uri_list(list),
        vec![
            PathBuf::from("/home/me/a b.mp4"),
            PathBuf::from("/tmp/è.wav")
        ]
    );
}

fn surface(surface: u32, point: egui::Pos2) -> SurfacePoint {
    SurfacePoint { surface, point }
}

fn raw(viewport: egui::ViewportId, events: Vec<egui::Event>) -> egui::RawInput {
    let mut input = egui::RawInput {
        viewport_id: viewport,
        screen_rect: Some(egui::Rect::from_min_size(
            egui::Pos2::ZERO,
            egui::vec2(300.0, 300.0),
        )),
        events,
        ..Default::default()
    };
    for id in [
        egui::ViewportId::ROOT,
        egui::ViewportId::from_hash_of("source"),
        egui::ViewportId::from_hash_of("target"),
    ] {
        input.viewports.insert(
            id,
            egui::ViewportInfo {
                parent: (id != egui::ViewportId::ROOT).then_some(egui::ViewportId::ROOT),
                ..Default::default()
            },
        );
    }
    input
}

fn button(point: egui::Pos2, pressed: bool) -> egui::Event {
    egui::Event::PointerButton {
        pos: point,
        button: egui::PointerButton::Primary,
        pressed,
        modifiers: Default::default(),
    }
}

fn frame(ctx: &egui::Context, raw: egui::RawInput, mut show: impl FnMut(&mut egui::Ui)) {
    let mut output = ctx.run_ui(raw, |ui| show(ui));
    output.textures_delta.clear();
}

fn drop(ui: &mut egui::Ui) -> Option<Arc<crate::timeline_ui::Generator>> {
    ui.interact(ui.max_rect(), ui.id().with("drop"), egui::Sense::hover())
        .dnd_release_payload()
}

fn gesture(
    source: egui::ViewportId,
    target: egui::ViewportId,
    zoom: f32,
) -> (egui::Context, Arc<Mutex<PointerState>>) {
    let ctx = egui::Context::default();
    ctx.set_zoom_factor(zoom);
    let state = Arc::new(Mutex::new(PointerState::default()));
    state
        .lock()
        .unwrap()
        .press(7, surface(10, egui::pos2(10.0, 10.0)));
    let bind_state = state.clone();
    let pointer = PointerBridge {
        state: state.clone(),
        bind: Arc::new(move |token| bind_state.lock().unwrap().bind(token)),
        token: Arc::new(AtomicU64::new(1)),
    };
    ctx.add_plugin(crate::panel_drag::NativeDrag::with_wayland(pointer));
    frame(&ctx, raw(target, vec![]), |ui| {
        drop(ui);
    });
    let point = egui::pos2(10.0, 10.0);
    frame(
        &ctx,
        raw(
            source,
            vec![egui::Event::PointerMoved(point), button(point, true)],
        ),
        |ui| {
            egui::DragAndDrop::set_payload(ui.ctx(), crate::timeline_ui::Generator::SolidColor);
        },
    );
    (ctx, state)
}

fn release(state: &Mutex<PointerState>, target: Option<u32>) {
    let mut state = state.lock().unwrap();
    let generation = state.release(7).unwrap();
    state.leave(7, 10);
    if let Some(target) = target {
        state.enter(7, surface(target, egui::pos2(80.0, 120.0)));
    }
    state.finish_release(generation);
}

#[test]
fn wayland_drop_delivers_once_without_desktop_coordinates_in_each_viewport_direction() {
    let source = egui::ViewportId::from_hash_of("source");
    let target = egui::ViewportId::from_hash_of("target");
    for (source, target) in [
        (source, egui::ViewportId::ROOT),
        (egui::ViewportId::ROOT, target),
        (source, target),
    ] {
        for zoom in [1.0, 2.0] {
            let (ctx, state) = gesture(source, target, zoom);
            release(&state, Some(20));
            frame(
                &ctx,
                raw(
                    source,
                    vec![
                        button(egui::pos2(900.0, 100.0), false),
                        egui::Event::PointerGone,
                    ],
                ),
                |_| {},
            );
            let point = egui::pos2(80.0, 120.0) / zoom;
            frame(&ctx, raw(target, vec![]), |ui| {
                drop(ui);
            });
            frame(
                &ctx,
                raw(target, vec![egui::Event::PointerMoved(point)]),
                |ui| {
                    assert_eq!(ui.input(|i| i.pointer.interact_pos()), Some(point));
                    assert_eq!(
                        drop(ui).as_deref(),
                        Some(&crate::timeline_ui::Generator::SolidColor),
                        "source={source:?}, target={target:?}, zoom={zoom}, released={}, payload={}",
                        ui.input(|i| i.pointer.any_released()),
                        egui::DragAndDrop::has_any_payload(ui.ctx())
                    );
                },
            );
            frame(&ctx, raw(target, vec![]), |ui| {
                assert!(!ui.input(|i| i.pointer.any_released()));
                assert!(drop(ui).is_none());
            });
        }
    }
}

#[test]
fn wayland_drop_waits_for_the_native_release_barrier_after_destination_input() {
    let source = egui::ViewportId::from_hash_of("source");
    let target = egui::ViewportId::ROOT;
    let (ctx, state) = gesture(source, target, 1.0);
    let generation = {
        let mut state = state.lock().unwrap();
        let generation = state.release(7).unwrap();
        state.leave(7, 10);
        state.enter(7, surface(20, egui::pos2(80.0, 120.0)));
        generation
    };
    frame(
        &ctx,
        raw(
            target,
            vec![egui::Event::PointerMoved(egui::pos2(80.0, 120.0))],
        ),
        |ui| {
            assert!(!ui.input(|i| i.pointer.any_released()));
            drop(ui);
        },
    );
    frame(
        &ctx,
        raw(source, vec![button(egui::pos2(900.0, 100.0), false)]),
        |_| {},
    );
    state.lock().unwrap().finish_release(generation);
    frame(&ctx, raw(target, vec![]), |ui| {
        assert_eq!(
            drop(ui).as_deref(),
            Some(&crate::timeline_ui::Generator::SolidColor)
        );
    });
}

#[test]
fn wayland_parent_drop_survives_an_immediate_child_processing_the_source_release() {
    let source = egui::ViewportId::from_hash_of("source");
    let target = egui::ViewportId::ROOT;
    let (ctx, state) = gesture(source, target, 1.0);
    release(&state, Some(20));
    frame(
        &ctx,
        raw(
            target,
            vec![egui::Event::PointerMoved(egui::pos2(80.0, 120.0))],
        ),
        |ui| {
            frame(
                &ctx,
                raw(source, vec![button(egui::pos2(900.0, 100.0), false)]),
                |_| {},
            );
            assert_eq!(
                drop(ui).as_deref(),
                Some(&crate::timeline_ui::Generator::SolidColor)
            );
        },
    );
}

#[test]
fn wayland_outside_release_and_leaving_the_drop_surface_cannot_deliver_later() {
    for outside in [true, false] {
        let source = egui::ViewportId::from_hash_of("source");
        let target = egui::ViewportId::ROOT;
        let (ctx, state) = gesture(source, target, 1.0);
        release(&state, (!outside).then_some(20));
        if !outside {
            state.lock().unwrap().leave(7, 20);
        }
        frame(
            &ctx,
            raw(source, vec![button(egui::pos2(900.0, 100.0), false)]),
            |_| {},
        );
        state
            .lock()
            .unwrap()
            .enter(7, surface(20, egui::pos2(80.0, 120.0)));
        frame(
            &ctx,
            raw(
                target,
                vec![egui::Event::PointerMoved(egui::pos2(80.0, 120.0))],
            ),
            |ui| {
                assert!(!ui.input(|i| i.pointer.any_released()));
                assert!(drop(ui).is_none());
            },
        );
    }
}

#[test]
fn wayland_same_surface_release_keeps_the_normal_egui_drop() {
    let source = egui::ViewportId::from_hash_of("source");
    let (ctx, state) = gesture(source, egui::ViewportId::ROOT, 1.0);
    frame(&ctx, raw(source, vec![]), |ui| {
        drop(ui);
    });
    let mut pointer = state.lock().unwrap();
    let generation = pointer.release(7).unwrap();
    pointer.finish_release(generation);
    std::mem::drop(pointer);
    let point = egui::pos2(10.0, 10.0);
    frame(&ctx, raw(source, vec![button(point, false)]), |ui| {
        assert_eq!(
            drop(ui).as_deref(),
            Some(&crate::timeline_ui::Generator::SolidColor)
        );
    });
}

#[test]
fn wayland_stale_pointer_input_and_a_closed_source_do_not_choose_a_destination() {
    let source = egui::ViewportId::from_hash_of("source");
    let target = egui::ViewportId::ROOT;
    for closed in [true, false] {
        let (ctx, state) = gesture(source, target, 1.0);
        release(&state, Some(20));
        frame(
            &ctx,
            raw(source, vec![button(egui::pos2(900.0, 100.0), false)]),
            |_| {},
        );
        let mut input = raw(
            target,
            vec![
                egui::Event::PointerMoved(egui::pos2(80.0, 120.0)),
                egui::Event::PointerGone,
            ],
        );
        if closed {
            input.viewports.remove(&source);
        }
        frame(&ctx, input, |ui| {
            assert!(!ui.input(|i| i.pointer.any_released()));
            assert!(drop(ui).is_none());
        });
    }
}

#[test]
fn wayland_pointer_binding_handles_late_binding_new_gestures_and_other_seats() {
    let mut state = PointerState::default();
    state.press(7, surface(10, egui::pos2(1.0, 2.0)));
    assert!(state.release(8).is_none());
    state.enter(8, surface(99, egui::Pos2::ZERO));
    assert_eq!(state.focus.unwrap().surface, 10);
    let first = state.release(7).unwrap();
    state.leave(7, 10);
    let point = egui::pos2(80.0, 120.0);
    state.enter(7, surface(20, point));
    state.finish_release(first);
    state.bind(1);
    assert_eq!(state.drop_position(1), PointerDrop::Dropped(point));
    state.press(7, surface(10, egui::Pos2::ZERO));
    assert_eq!(state.drop_position(1), PointerDrop::Cancelled);
    state.bind(2);
    state.finish_release(first);
    assert_eq!(state.drop_position(2), PointerDrop::Pending);
    state.cancel();
    assert_eq!(state.drop_position(2), PointerDrop::Cancelled);
}
