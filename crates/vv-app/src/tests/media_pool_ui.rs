use super::*;
use crate::pool_view::kinetic_pool_scroll;

fn timeline_item(app: &VenturiApp, timeline_id: vv_core::TimelineId) -> MediaId {
    app.session
        .project
        .media_pool
        .iter()
        .find(|(_, item)| item.compound == Some(timeline_id))
        .map(|(id, _)| id)
        .expect("every timeline has a pool item")
}

#[test]
fn context_menu_deletes_the_selection_and_undo_restores_it() {
    use media_pool::PoolLayout;

    for layout in [PoolLayout::Details, PoolLayout::List, PoolLayout::Grid] {
        for timeline_target in [false, true] {
            for selection in [0, 1, 2] {
                let mut app = VenturiApp::default();
                let timeline = app.ensure_timeline();
                let timeline_media = timeline_item(&app, timeline);
                let mut item = app.session.project.media_pool[timeline_media].clone();
                item.path = "footage.mp4".into();
                item.compound = None;
                let media = app.session.project.media_pool.insert(item);
                let target = if timeline_target {
                    timeline_media
                } else {
                    media
                };
                let other = if timeline_target {
                    media
                } else {
                    timeline_media
                };
                let name = file_label(&app.session.project.media_pool[target].path);
                let selected = match selection {
                    0 => vec![other],
                    1 => vec![target],
                    _ => vec![target, other],
                };
                let removed = if selection == 2 {
                    vec![target, other]
                } else {
                    vec![target]
                };
                app.media_pool_state.select_only(selected);
                app.settings.panels.media_pool_layout = layout;
                app.session.mark_saved();

                let ctx = egui::Context::default();
                let mut time = 0.0;
                let mut frame = |app: &mut VenturiApp, events| {
                    let mut input = column_input(events);
                    input.time = Some(time);
                    time += 1.0 / 60.0;
                    let mut output = ctx.run_ui(input, |ui| {
                        app.show_project_pool(ui, &mut None);
                    });
                    output.textures_delta.clear();
                    output
                };
                let text_position = |output: &egui::FullOutput, label: &str| {
                    output
                        .shapes
                        .iter()
                        .find_map(|shape| match &shape.shape {
                            egui::Shape::Text(text) if text.galley.text() == label => {
                                Some(text.pos + text.galley.size() / 2.0)
                            }
                            _ => None,
                        })
                        .unwrap_or_else(|| panic!("{label:?} is visible in {layout:?}"))
                };
                let button = |pos, button, pressed| egui::Event::PointerButton {
                    pos,
                    button,
                    pressed,
                    modifiers: egui::Modifiers::NONE,
                };
                let output = frame(&mut app, vec![]);
                let position = text_position(&output, &name);
                frame(&mut app, vec![egui::Event::PointerMoved(position)]);
                frame(
                    &mut app,
                    vec![button(position, egui::PointerButton::Secondary, true)],
                );
                frame(
                    &mut app,
                    vec![button(position, egui::PointerButton::Secondary, false)],
                );
                let output = frame(&mut app, vec![]);
                let position = text_position(&output, &t!("menu.delete"));
                assert_eq!(
                    app.media_pool_state.selected,
                    removed.iter().copied().collect()
                );
                frame(&mut app, vec![egui::Event::PointerMoved(position)]);
                frame(
                    &mut app,
                    vec![button(position, egui::PointerButton::Primary, true)],
                );
                frame(
                    &mut app,
                    vec![button(position, egui::PointerButton::Primary, false)],
                );

                for &id in &removed {
                    assert!(!app.session.project.media_pool.contains_key(id));
                }
                assert_eq!(app.session.project.media_pool.len(), 2 - removed.len());
                assert!(app.media_pool_state.selected.is_empty());
                assert!(app.session.has_unsaved_changes());
                assert_eq!(
                    app.session.project.timelines.contains_key(timeline),
                    !removed.contains(&timeline_media)
                );
                assert!(!egui::Popup::is_any_open(&ctx));

                app.undo();
                assert!(app.session.project.media_pool.contains_key(media));
                assert!(app.session.project.media_pool.contains_key(timeline_media));
                assert!(app.session.project.timelines.contains_key(timeline));
            }
        }
    }
}

#[test]
fn duplicating_a_timeline_twice_numbers_the_copies() {
    let mut app = VenturiApp::default();
    let timeline_id = app.ensure_timeline();
    let source = timeline_item(&app, timeline_id);
    let base = format!(
        "{} {}",
        file_label(&app.session.project.media_pool[source].path),
        t!("pool.copy_suffix")
    );

    app.duplicate_timeline(source);
    app.duplicate_timeline(source);

    let mut names: Vec<String> = app
        .session
        .project
        .media_pool
        .values()
        .filter(|item| item.compound.is_some())
        .map(|item| file_label(&item.path))
        .collect();
    names.sort();
    assert_eq!(names[1..], [base.clone(), format!("{base} 2")]);
    assert_eq!(app.session.project.timelines.len(), 3);
    assert!(app.has_unsaved_changes());
}

#[test]
fn renaming_to_the_same_name_changes_nothing() {
    let mut app = VenturiApp::default();
    let timeline_id = app.ensure_timeline();
    let item = timeline_item(&app, timeline_id);
    app.session.mark_saved();
    let name = file_label(&app.session.project.media_pool[item].path);
    app.rename_timeline(item, name);
    assert!(!app.session.has_unsaved_changes());
    app.rename_timeline(item, "Edit".into());
    assert_eq!(app.session.project.timelines[timeline_id].name, "Edit");
    assert!(app.session.has_unsaved_changes());
}

fn column_input(events: Vec<egui::Event>) -> egui::RawInput {
    egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(
            egui::Pos2::ZERO,
            egui::vec2(300.0, 600.0),
        )),
        events,
        ..Default::default()
    }
}

#[test]
fn left_column_split_follows_the_drag_and_keeps_both_panes_visible() {
    let ctx = egui::Context::default();
    let mut fraction = 0.5;
    let split = |events, fraction: &mut f32| {
        let mut rects = None;
        ctx.run_ui(column_input(events), |ui| {
            rects = Some(split_left_column(ui, fraction));
        })
        .textures_delta
        .clear();
        rects.unwrap()
    };
    let (pool, effects) = split(vec![], &mut fraction);
    assert!((pool.height() - effects.height()).abs() < 1.0);

    let grab = egui::pos2(150.0, pool.bottom() + COLUMN_SPLITTER_HEIGHT / 2.0);
    let button = |pressed| egui::Event::PointerButton {
        pos: grab,
        button: egui::PointerButton::Primary,
        pressed,
        modifiers: egui::Modifiers::NONE,
    };
    split(vec![egui::Event::PointerMoved(grab)], &mut fraction);
    split(vec![button(true)], &mut fraction);
    split(
        vec![egui::Event::PointerMoved(grab + egui::vec2(0.0, 100.0))],
        &mut fraction,
    );
    let (dragged_pool, _) = split(vec![], &mut fraction);
    assert!(
        (dragged_pool.height() - pool.height() - 100.0).abs() < 1.0,
        "{} -> {}",
        pool.height(),
        dragged_pool.height()
    );

    let mut fraction = 1.0;
    let (_, effects) = split(vec![], &mut fraction);
    assert!(effects.height() >= MIN_COLUMN_PANE_HEIGHT - 0.5);
}

#[test]
fn pool_swipe_keeps_scrolling_after_release_only_when_enabled() {
    for enabled in [true, false] {
        let ctx = egui::Context::default();
        let mut state = media_pool::MediaPoolState::default();
        let mut offset = 0.0;
        let mut frame = |events| {
            ctx.run_ui(column_input(events), |ui| {
                kinetic_pool_scroll(ui.ctx(), &mut state, enabled);
                let output = egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .show(ui, |ui| ui.allocate_space(egui::vec2(100.0, 5000.0)));
                offset = output.state.offset.y;
                state.scroll_area = Some(media_pool::PoolScrollArea {
                    id: output.id,
                    viewport: output.inner_rect,
                    max_offset: output.content_size.y - output.inner_rect.height(),
                });
            })
            .textures_delta
            .clear();
            offset
        };
        let wheel = |phase| egui::Event::MouseWheel {
            unit: egui::MouseWheelUnit::Point,
            delta: egui::vec2(0.0, -20.0),
            phase,
            modifiers: egui::Modifiers::NONE,
        };
        frame(vec![egui::Event::PointerMoved(egui::pos2(100.0, 100.0))]);
        frame(vec![wheel(egui::TouchPhase::Start)]);
        for _ in 0..3 {
            frame(vec![wheel(egui::TouchPhase::Move)]);
        }
        let released = frame(vec![wheel(egui::TouchPhase::End)]);
        assert!(released > 0.0, "the swipe scrolls down");
        let coasted = frame(vec![]) - released;
        if enabled {
            assert!(coasted > 0.0, "inertia keeps scrolling");
        } else {
            assert_eq!(coasted, 0.0, "no inertia when disabled");
        }
    }
}

#[test]
fn f2_renames_a_single_selected_timeline_or_folder() {
    let mut app = VenturiApp::default();
    let timeline_id = app.ensure_timeline();
    let item = timeline_item(&app, timeline_id);

    app.media_pool_state.select_only([item]);
    app.rename_selected_pool_item();
    assert_eq!(
        app.media_pool_state
            .renaming
            .as_ref()
            .map(|r| r.target.clone()),
        Some(media_pool::RenameTarget::Item(item))
    );

    app.media_pool_state.renaming = None;
    app.duplicate_timeline(item);
    let copy = *app.media_pool_state.selected.first().unwrap();
    app.media_pool_state.select_only([item, copy]);
    app.rename_selected_pool_item();
    assert!(app.media_pool_state.renaming.is_none());

    let folder = app.session.project.folders.insert(vv_core::MediaFolder {
        name: "footage".into(),
        parent: None,
    });
    app.media_pool_state.select_folder(folder);
    assert!(app.media_pool_state.selected.is_empty());
    app.rename_selected_pool_item();
    assert_eq!(
        app.media_pool_state
            .renaming
            .as_ref()
            .map(|r| r.target.clone()),
        Some(media_pool::RenameTarget::Folder(folder))
    );
}

#[test]
fn revealing_a_media_opens_its_folders_and_drops_a_hiding_search() {
    let mut app = VenturiApp::default();
    let timeline_id = app.ensure_timeline();
    let item = timeline_item(&app, timeline_id);
    let outer = app.session.project.folders.insert(vv_core::MediaFolder {
        name: "outer".into(),
        parent: None,
    });
    let inner = app.session.project.folders.insert(vv_core::MediaFolder {
        name: "inner".into(),
        parent: Some(outer),
    });
    app.session.project.media_pool[item].folder = Some(inner);
    app.media_pool_state.expanded.insert(inner);
    app.media_pool_state.search = "no such name".into();
    app.settings.panels.media_pool_open = false;

    app.reveal_in_media_pool(item);

    assert!(app.settings.panels.media_pool_open);
    assert!(app.media_pool_state.search.is_empty());
    assert!(app.media_pool_state.expanded.contains(&outer));
    assert!(app.media_pool_state.expanded.contains(&inner));
    assert_eq!(
        app.media_pool_state.selected,
        std::collections::BTreeSet::from([item])
    );
    assert_eq!(app.media_pool_state.reveal, Some(item));
}
