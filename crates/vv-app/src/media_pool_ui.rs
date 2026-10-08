//! Media pool and effects panels.

use super::*;

/// Progress ring; `None` = queued (background ring only).
pub(crate) fn proxy_progress_ring(ui: &mut egui::Ui, fraction: Option<f32>) -> egui::Response {
    const SIZE: f32 = 34.0;
    const STROKE: f32 = 3.0;
    let (rect, response) = ui.allocate_exact_size(egui::vec2(SIZE, SIZE), egui::Sense::hover());
    let painter = ui.painter();
    let center = rect.center();
    let radius = (SIZE - STROKE) / 2.0;
    let track_color = ui.visuals().widgets.inactive.bg_fill;
    painter.circle_stroke(center, radius, egui::Stroke::new(STROKE, track_color));
    if let Some(fraction) = fraction {
        let fraction = fraction.clamp(0.0, 1.0);
        let segments = ((fraction * 48.0).ceil() as usize).max(1);
        let start = -std::f32::consts::FRAC_PI_2;
        let points: Vec<egui::Pos2> = (0..=segments)
            .map(|i| {
                let angle = start + std::f32::consts::TAU * fraction * i as f32 / segments as f32;
                center + radius * egui::vec2(angle.cos(), angle.sin())
            })
            .collect();
        painter.add(egui::Shape::line(
            points,
            egui::Stroke::new(STROKE, ui.visuals().selection.bg_fill),
        ));
    }
    response
}

/// Label following the cursor while dragging a media.
pub(crate) fn show_drag_ghost(ui: &egui::Ui, id: egui::Id, label: &str) {
    let Some(pos) = ui.input(|i| i.pointer.hover_pos()) else {
        return;
    };
    egui::Area::new(id.with("drag_ghost"))
        .order(egui::Order::Tooltip)
        .fixed_pos(pos + egui::vec2(12.0, 12.0))
        .interactable(false)
        .show(ui.ctx(), |ui| {
            egui::Frame::popup(ui.style()).show(ui, |ui| {
                ui.label(label);
            });
        });
}

pub(crate) fn effects_section_header(ui: &mut egui::Ui, title: &str) {
    let width = ui.available_width();
    let (rect, _) = ui.allocate_exact_size(egui::vec2(width, HEADER_HEIGHT), egui::Sense::hover());
    ui.painter()
        .rect_filled(rect, 0.0, ui.visuals().widgets.inactive.weak_bg_fill);
    ui.painter().text(
        rect.left_center() + egui::vec2(6.0, 0.0),
        egui::Align2::LEFT_CENTER,
        title,
        egui::FontId::proportional(13.0),
        ui.visuals().strong_text_color(),
    );
}

/// Effects panel entry: thumbnail on the left and name, draggable onto
/// the timeline.
pub(crate) fn effect_item(ui: &mut egui::Ui, generator: timeline_ui::Generator) {
    let (rect, _) =
        ui.allocate_exact_size(egui::vec2(ui.available_width(), 30.0), egui::Sense::hover());
    let id = ui
        .id()
        .with(("effect_item", timeline_ui::generator_label(generator)));
    let resp = ui
        .interact(rect, id, egui::Sense::click_and_drag())
        .on_hover_text(t!("pool.drag_to_timeline"));
    let visuals = ui.visuals();
    let (bg, stroke) = if resp.hovered() || resp.dragged() {
        (
            visuals.widgets.hovered.weak_bg_fill,
            visuals.widgets.hovered.fg_stroke.color,
        )
    } else {
        (
            visuals.widgets.inactive.weak_bg_fill,
            visuals.widgets.noninteractive.bg_stroke.color,
        )
    };
    let painter = ui.painter();
    painter.rect_filled(rect, 3.0, bg);
    let thumb = egui::Rect::from_min_size(rect.min, egui::vec2(54.0, rect.height())).shrink(1.0);
    match generator {
        timeline_ui::Generator::SolidColor => {
            painter.rect_filled(thumb, 2.0, egui::Color32::from_rgb(106, 176, 204));
        }
        timeline_ui::Generator::Text => {
            painter.rect_filled(thumb, 2.0, egui::Color32::BLACK);
            painter.text(
                thumb.center(),
                egui::Align2::CENTER_CENTER,
                "Aa",
                egui::FontId::proportional(13.0),
                egui::Color32::WHITE,
            );
        }
        timeline_ui::Generator::Adjustment => {
            painter.rect_filled(thumb, 2.0, egui::Color32::from_rgb(150, 150, 165));
        }
    }
    painter.rect_stroke(
        rect,
        3.0,
        egui::Stroke::new(1.0, stroke),
        egui::StrokeKind::Inside,
    );
    painter.text(
        egui::pos2(thumb.right() + 14.0, rect.center().y),
        egui::Align2::LEFT_CENTER,
        timeline_ui::generator_label(generator),
        egui::FontId::proportional(13.0),
        visuals.text_color(),
    );
    resp.dnd_set_drag_payload(generator);
    if resp.dragged() {
        show_drag_ghost(ui, id, &timeline_ui::generator_label(generator));
    }
}

/// Filter entry in the Effects panel: dragged onto an existing video clip
/// (never onto empty space). Its kind (`vv_core::FilterKind`) is the same one
/// saved in `EffectStack::filters`: no double representation between editor
/// and model.
pub(crate) fn filter_item(ui: &mut egui::Ui, filter: timeline_ui::FilterEntry) {
    let label = filter.label();
    let (rect, _) =
        ui.allocate_exact_size(egui::vec2(ui.available_width(), 30.0), egui::Sense::hover());
    let id = ui.id().with(("filter_item", &label));
    let resp = ui
        .interact(rect, id, egui::Sense::click_and_drag())
        .on_hover_text(t!("pool.drag_to_clip"));
    let visuals = ui.visuals();
    let (bg, stroke) = if resp.hovered() || resp.dragged() {
        (
            visuals.widgets.hovered.weak_bg_fill,
            visuals.widgets.hovered.fg_stroke.color,
        )
    } else {
        (
            visuals.widgets.inactive.weak_bg_fill,
            visuals.widgets.noninteractive.bg_stroke.color,
        )
    };
    let painter = ui.painter();
    painter.rect_filled(rect, 3.0, bg);
    let thumb = egui::Rect::from_min_size(rect.min, egui::vec2(54.0, rect.height())).shrink(1.0);
    painter.rect_filled(thumb, 2.0, egui::Color32::from_gray(40));
    timeline_ui::paint_gear_icon(painter, thumb.center(), 11.0, egui::Color32::from_gray(220));
    painter.rect_stroke(
        rect,
        3.0,
        egui::Stroke::new(1.0, stroke),
        egui::StrokeKind::Inside,
    );
    painter.text(
        egui::pos2(thumb.right() + 14.0, rect.center().y),
        egui::Align2::LEFT_CENTER,
        label.as_ref(),
        egui::FontId::proportional(13.0),
        visuals.text_color(),
    );
    resp.dnd_set_drag_payload(filter);
    if resp.dragged() {
        show_drag_ghost(ui, id, &label);
    }
}

/// Transition entry in the Effects panel: dragged near an edge (left or
/// right) of an existing video clip, never at its center nor onto empty
/// space. The kind (`vv_core::TransitionKind`) is the same one saved in
/// `EffectStack::transition_in`/`transition_out`.
pub(crate) fn transition_item(ui: &mut egui::Ui, kind: vv_core::TransitionKind) {
    let label = timeline_ui::transition_kind_label(kind);
    let (rect, _) =
        ui.allocate_exact_size(egui::vec2(ui.available_width(), 30.0), egui::Sense::hover());
    let id = ui.id().with(("transition_item", &label));
    let resp = ui
        .interact(rect, id, egui::Sense::click_and_drag())
        .on_hover_text(t!("pool.drag_to_clip_edge"));
    let visuals = ui.visuals();
    let (bg, stroke) = if resp.hovered() || resp.dragged() {
        (
            visuals.widgets.hovered.weak_bg_fill,
            visuals.widgets.hovered.fg_stroke.color,
        )
    } else {
        (
            visuals.widgets.inactive.weak_bg_fill,
            visuals.widgets.noninteractive.bg_stroke.color,
        )
    };
    let painter = ui.painter();
    painter.rect_filled(rect, 3.0, bg);
    let thumb = egui::Rect::from_min_size(rect.min, egui::vec2(54.0, rect.height())).shrink(1.0);
    painter.rect_filled(thumb, 2.0, egui::Color32::from_gray(40));
    timeline_ui::paint_bracket_icon(
        painter,
        thumb.center(),
        thumb.height() * 0.6,
        vv_core::FadeEdge::Out,
        egui::Color32::from_gray(220),
    );
    painter.rect_stroke(
        rect,
        3.0,
        egui::Stroke::new(1.0, stroke),
        egui::StrokeKind::Inside,
    );
    painter.text(
        egui::pos2(thumb.right() + 14.0, rect.center().y),
        egui::Align2::LEFT_CENTER,
        label.as_ref(),
        egui::FontId::proportional(13.0),
        visuals.text_color(),
    );
    resp.dnd_set_drag_payload(kind);
    if resp.dragged() {
        show_drag_ghost(ui, id, &label);
    }
}

/// Width of the "Duration" column: the same in the header and in the rows,
/// so they stay aligned.
pub(crate) const DURATION_COL_W: f32 = 64.0;

/// Free background on the sides and below the items: always somewhere to
/// right click, even with the pool full.
pub(crate) const SIDE_PAD: i8 = 6;
pub(crate) const BOTTOM_PAD: f32 = 28.0;

/// Height of the media pool header bar.
pub(crate) const HEADER_HEIGHT: f32 = 22.0;

/// Column header of the media pool: every cell is clickable in full
/// (not just the text), as in a file manager list.
pub(crate) fn media_pool_header(ui: &mut egui::Ui, state: &mut media_pool::MediaPoolState) {
    use media_pool::SortKey;
    let width = ui.available_width();
    let (rect, _) = ui.allocate_exact_size(egui::vec2(width, HEADER_HEIGHT), egui::Sense::hover());
    let duration_w = DURATION_COL_W.min(width);
    let (name_rect, duration_rect) = (
        egui::Rect::from_min_max(
            rect.left_top(),
            egui::pos2(rect.right() - duration_w, rect.bottom()),
        ),
        egui::Rect::from_min_max(
            egui::pos2(rect.right() - duration_w, rect.top()),
            rect.right_bottom(),
        ),
    );
    let sort = state.sort;
    for (key, label, cell) in [
        (SortKey::Name, t!("pool.name"), name_rect),
        (SortKey::Duration, t!("pool.duration"), duration_rect),
    ] {
        let resp = ui.interact(
            cell,
            ui.id().with(("media_pool_header", key as u8)),
            egui::Sense::click(),
        );
        let active = sort.key == key;
        let bg = if resp.hovered() {
            ui.visuals().widgets.hovered.weak_bg_fill
        } else if active {
            ui.visuals().widgets.active.weak_bg_fill
        } else {
            ui.visuals().widgets.inactive.weak_bg_fill
        };
        ui.painter().rect_filled(cell, 0.0, bg);
        let text_color = ui.visuals().strong_text_color();
        ui.painter().text(
            cell.left_center() + egui::vec2(6.0, 0.0),
            egui::Align2::LEFT_CENTER,
            label,
            egui::FontId::proportional(13.0),
            text_color,
        );
        if active {
            // Small triangle drawn by hand instead of a character: the ones
            // in system fonts are tall and pointy, this one is squashed.
            let c = egui::pos2(cell.right() - 10.0, cell.center().y);
            let (w, h) = (4.5, 2.5);
            let points = if sort.ascending {
                vec![
                    egui::pos2(c.x - w, c.y + h),
                    egui::pos2(c.x + w, c.y + h),
                    egui::pos2(c.x, c.y - h),
                ]
            } else {
                vec![
                    egui::pos2(c.x - w, c.y - h),
                    egui::pos2(c.x + w, c.y - h),
                    egui::pos2(c.x, c.y + h),
                ]
            };
            ui.painter().add(egui::Shape::convex_polygon(
                points,
                text_color,
                egui::Stroke::NONE,
            ));
        }
        if resp.clicked() {
            state.toggle_sort(key);
        }
    }
}

/// Returns whether the query changed.
fn media_pool_search_field(ui: &mut egui::Ui, search: &mut String) -> bool {
    let before = search.clone();
    ui.horizontal(|ui| {
        let clear_w = if search.is_empty() { 0.0 } else { 24.0 };
        let resp = ui.add(
            egui::TextEdit::singleline(search)
                .hint_text(t!("pool.search"))
                .desired_width(ui.available_width() - clear_w),
        );
        if resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Escape)) {
            search.clear();
        }
        if !search.is_empty()
            && ui
                .small_button("×")
                .on_hover_text(t!("pool.search_clear"))
                .clicked()
        {
            search.clear();
        }
    });
    *search != before
}

pub(crate) fn file_label(path: &std::path::Path) -> String {
    path.file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("?")
        .to_string()
}

/// Name text field of the item being renamed: `Some(Some(name))` to
/// confirm, `Some(None)` to cancel, `None` while still editing.
fn rename_field(ui: &mut egui::Ui, rename: &mut media_pool::Rename) -> Option<Option<String>> {
    let edit_id = ui.id().with("media_pool_rename");
    let mut output = egui::TextEdit::singleline(&mut rename.text)
        .id(edit_id)
        .desired_width(ui.available_width() - DURATION_COL_W)
        .show(ui);
    if rename.just_started {
        rename.just_started = false;
        output.response.request_focus();
        let end = egui::text::CCursor::new(rename.text.chars().count());
        output
            .state
            .cursor
            .set_char_range(Some(egui::text::CCursorRange::two(
                egui::text::CCursor::new(0),
                end,
            )));
        output.state.store(ui.ctx(), edit_id);
    }
    if !output.response.lost_focus() {
        return None;
    }
    let name = rename.text.trim();
    if ui.input(|i| i.key_pressed(egui::Key::Escape)) || name.is_empty() {
        Some(None)
    } else {
        Some(Some(name.to_string()))
    }
}

/// Horizontal step per folder level.
const FOLDER_INDENT: f32 = 14.0;

fn indented<R>(ui: &mut egui::Ui, depth: usize, add: impl FnOnce(&mut egui::Ui) -> R) -> R {
    if depth == 0 {
        return add(ui);
    }
    ui.horizontal(|ui| {
        ui.add_space(depth as f32 * FOLDER_INDENT);
        ui.vertical(add).inner
    })
    .inner
}

/// Thumbnail of a timeline in the pool: a film strip.
fn paint_film_icon(
    painter: &egui::Painter,
    rect: egui::Rect,
    color: egui::Color32,
    hole_color: egui::Color32,
) {
    let film = egui::Rect::from_center_size(rect.center(), egui::vec2(44.0, 28.0));
    painter.rect_filled(film, 2.0, color);
    const HOLES: usize = 6;
    let step = film.width() / HOLES as f32;
    for i in 0..HOLES {
        let x = film.left() + step * (i as f32 + 0.5);
        for y in [film.top() + 3.5, film.bottom() - 3.5] {
            painter.rect_filled(
                egui::Rect::from_center_size(egui::pos2(x, y), egui::vec2(4.0, 3.0)),
                0.5,
                hole_color,
            );
        }
    }
    let frames = film.shrink2(egui::vec2(3.0, 8.0));
    let gap = 3.0;
    let frame_w = (frames.width() - gap) / 2.0;
    for i in 0..2 {
        let left = frames.left() + i as f32 * (frame_w + gap);
        painter.rect_filled(
            egui::Rect::from_min_size(
                egui::pos2(left, frames.top()),
                egui::vec2(frame_w, frames.height()),
            ),
            1.0,
            hole_color,
        );
    }
}

/// Thumbnail of a media whose file is missing.
fn paint_offline_thumbnail(painter: &egui::Painter, rect: egui::Rect) {
    painter.rect_filled(rect, 2.0, egui::Color32::BLACK);
    let c = rect.center();
    let (half_w, h) = (12.0, 21.0);
    let (top, bottom) = (c.y - h / 2.0, c.y + h / 2.0);
    painter.add(egui::Shape::convex_polygon(
        vec![
            egui::pos2(c.x, top),
            egui::pos2(c.x + half_w, bottom),
            egui::pos2(c.x - half_w, bottom),
        ],
        crate::theme::ERROR,
        egui::Stroke::NONE,
    ));
    painter.rect_filled(
        egui::Rect::from_min_max(
            egui::pos2(c.x - 1.25, top + 7.0),
            egui::pos2(c.x + 1.25, bottom - 6.0),
        ),
        1.0,
        egui::Color32::BLACK,
    );
    painter.circle_filled(egui::pos2(c.x, bottom - 3.0), 1.4, egui::Color32::BLACK);
}

impl VenturiApp {
    fn start_rename(&mut self, media_id: MediaId) {
        let Some(item) = self.session.project.media_pool.get(media_id) else {
            return;
        };
        self.media_pool_state.renaming = Some(media_pool::Rename {
            target: media_pool::RenameTarget::Media(media_id),
            text: file_label(&item.path),
            just_started: true,
        });
    }

    /// Only folders and timelines can be renamed: a media keeps its file
    /// name.
    pub(crate) fn rename_selected_pool_item(&mut self) {
        if let Some(folder) = self.media_pool_state.selected_folder {
            self.start_folder_rename(folder);
            return;
        }
        let mut selected = self.media_pool_state.selected.iter();
        if let (Some(&id), None) = (selected.next(), selected.next())
            && self
                .session
                .project
                .media_pool
                .get(id)
                .is_some_and(|item| item.compound.is_some())
        {
            self.media_pool_state.rename_pending = None;
            self.start_rename(id);
        }
    }

    /// Selects `media` and makes it visible: opens the pool and the folders
    /// containing it, drops a search hiding it, scrolls to it.
    pub(crate) fn reveal_in_media_pool(&mut self, media: MediaId) {
        let Some(item) = self.session.project.media_pool.get(media) else {
            return;
        };
        self.settings.panels.media_pool_open = true;
        let state = &mut self.media_pool_state;
        if !media_pool::matches_search(&file_label(&item.path), &state.search) {
            state.search.clear();
        }
        let mut visited = std::collections::HashSet::new();
        let mut folder = item.folder;
        while let Some(f) = folder
            && let Some(parent) = self.session.project.folders.get(f).map(|f| f.parent)
            && visited.insert(f)
        {
            state.expanded.insert(f);
            folder = parent;
        }
        state.select_only([media]);
        state.rename_pending = None;
        state.scroll_vel = 0.0;
        state.reveal = Some(media);
    }

    fn start_folder_rename(&mut self, folder: FolderId) {
        let Some(f) = self.session.project.folders.get(folder) else {
            return;
        };
        self.media_pool_state.renaming = Some(media_pool::Rename {
            target: media_pool::RenameTarget::Folder(folder),
            text: f.name.clone(),
            just_started: true,
        });
    }

    /// Created expanded (with its parent), with its name ready to edit.
    fn new_folder(&mut self, parent: Option<FolderId>) {
        let base = t!("pool.new_folder_name").into_owned();
        let taken: std::collections::HashSet<&str> = self
            .session
            .project
            .folders
            .values()
            .filter(|f| f.parent == parent)
            .map(|f| f.name.as_str())
            .collect();
        let name = std::iter::once(base.clone())
            .chain((2..).map(|n| format!("{base} {n}")))
            .find(|name| !taken.contains(name.as_str()))
            .expect("infinite candidates");
        let mut add = vv_core::AddEntities::new(vv_core::CommandLabel::NewFolder);
        let folder = add.folder(
            &mut self.session.project,
            vv_core::MediaFolder { name, parent },
        );
        self.session.apply(Box::new(add));
        self.media_pool_state.expanded.extend(parent);
        self.start_folder_rename(folder);
    }

    fn move_media_to_folder(&mut self, media: &[MediaId], folder: Option<FolderId>) {
        let media: Vec<MediaId> = media
            .iter()
            .copied()
            .filter(|&id| {
                self.session
                    .project
                    .media_pool
                    .get(id)
                    .is_some_and(|item| item.folder != folder)
            })
            .collect();
        if !media.is_empty() {
            self.session
                .apply(Box::new(vv_core::SetMediaFolder::new(media, folder)));
        }
    }

    /// Drops onto the pool: media or a folder dragged into `folder`.
    fn accept_pool_drop(&mut self, resp: &egui::Response, folder: Option<FolderId>) {
        if let Some(set) = resp.dnd_release_payload::<timeline_ui::MediaDragSet>() {
            let media: Vec<MediaId> = set.items.iter().map(|d| d.media_id).collect();
            self.move_media_to_folder(&media, folder);
        } else if let Some(dragged) = resp.dnd_release_payload::<media_pool::FolderDrag>()
            && self.session.project.can_move_folder(dragged.0, folder)
            && self.session.project.folders[dragged.0].parent != folder
        {
            self.session
                .apply(Box::new(vv_core::MoveFolder::new(dragged.0, folder)));
            self.media_pool_state.expanded.extend(folder);
        }
    }

    fn folder_row(&mut self, ui: &mut egui::Ui, folder: FolderId) {
        let Some(name) = self
            .session
            .project
            .folders
            .get(folder)
            .map(|f| f.name.clone())
        else {
            return;
        };
        let expanded = self.media_pool_state.expanded.contains(&folder);
        let height = 24.0;
        let rect = ui
            .allocate_exact_size(
                egui::vec2(ui.available_width(), height),
                egui::Sense::hover(),
            )
            .0;
        let renaming = self
            .media_pool_state
            .renaming
            .as_mut()
            .filter(|r| r.target == media_pool::RenameTarget::Folder(folder));
        let is_renaming = renaming.is_some();
        let sense = if is_renaming {
            egui::Sense::hover()
        } else {
            egui::Sense::click_and_drag()
        };
        let resp = ui.interact(rect, ui.id().with(("media_pool_folder", folder)), sense);
        let drop_hover = resp
            .dnd_hover_payload::<timeline_ui::MediaDragSet>()
            .is_some()
            || resp
                .dnd_hover_payload::<media_pool::FolderDrag>()
                .is_some_and(|d| d.0 != folder);
        let visuals = ui.visuals();
        if drop_hover {
            ui.painter()
                .rect_filled(rect, 3.0, crate::theme::ACCENT_TRANSLUCENT);
        } else if resp.hovered() {
            ui.painter()
                .rect_filled(rect, 3.0, visuals.widgets.hovered.weak_bg_fill);
        }
        let color = visuals.text_color();
        let arrow = egui::Rect::from_center_size(
            egui::pos2(rect.left() + 8.0, rect.center().y),
            egui::vec2(8.0, 8.0),
        );
        let points = if expanded {
            vec![arrow.left_top(), arrow.right_top(), arrow.center_bottom()]
        } else {
            vec![arrow.left_top(), arrow.right_center(), arrow.left_bottom()]
        };
        ui.painter().add(egui::Shape::convex_polygon(
            points,
            color,
            egui::Stroke::NONE,
        ));
        let icon = egui::Rect::from_min_size(
            egui::pos2(rect.left() + 18.0, rect.center().y - 5.0),
            egui::vec2(15.0, 11.0),
        );
        let icon_color = visuals.weak_text_color();
        ui.painter().rect_filled(
            egui::Rect::from_min_size(icon.left_top() - egui::vec2(0.0, 2.0), egui::vec2(6.0, 3.0)),
            1.0,
            icon_color,
        );
        ui.painter().rect_filled(icon, 1.5, icon_color);
        let text_rect = rect.with_min_x(rect.left() + 38.0);
        match renaming {
            Some(rename) => {
                let done = ui
                    .scope_builder(egui::UiBuilder::new().max_rect(text_rect), |ui| {
                        rename_field(ui, rename)
                    })
                    .inner;
                if let Some(new_name) = done {
                    self.media_pool_state.renaming = None;
                    if let Some(name) = new_name
                        && self
                            .session
                            .project
                            .folders
                            .get(folder)
                            .is_some_and(|f| f.name != name)
                    {
                        self.session
                            .apply(Box::new(vv_core::RenameFolder::new(folder, name)));
                    }
                }
            }
            None => {
                ui.painter().text(
                    text_rect.left_center(),
                    egui::Align2::LEFT_CENTER,
                    &name,
                    egui::FontId::proportional(14.0),
                    color,
                );
            }
        }
        if self.media_pool_state.selected_folder == Some(folder) {
            ui.painter().rect_stroke(
                rect,
                3.0,
                egui::Stroke::new(2.0, egui::Color32::WHITE),
                egui::StrokeKind::Inside,
            );
            ui.painter()
                .rect_filled(rect, 3.0, egui::Color32::from_white_alpha(18));
        }
        if resp.clicked() {
            self.media_pool_state.select_folder(folder);
            self.media_pool_state.rename_pending = None;
            if expanded {
                self.media_pool_state.expanded.remove(&folder);
            } else {
                self.media_pool_state.expanded.insert(folder);
            }
        }
        resp.dnd_set_drag_payload(media_pool::FolderDrag(folder));
        if resp.dragged() {
            show_drag_ghost(ui, resp.id, &name);
        }
        self.accept_pool_drop(&resp, Some(folder));
        resp.context_menu(|ui| {
            if ui.button(t!("pool.new_folder")).clicked() {
                self.new_folder(Some(folder));
                ui.close();
            }
            if ui.button(t!("pool.rename")).clicked() {
                self.start_folder_rename(folder);
                ui.close();
            }
            if ui.button(t!("pool.relink_folder")).clicked() {
                self.relink_folder_dialog(folder);
                ui.close();
            }
            ui.separator();
            if ui
                .button(t!("pool.delete_folder"))
                .on_hover_text(t!("pool.delete_folder_hint"))
                .clicked()
            {
                self.session
                    .apply(Box::new(vv_core::DeleteFolder::new(folder)));
                ui.close();
            }
        });
    }

    fn rename_timeline(&mut self, media_id: MediaId, name: String) {
        let unchanged = self
            .session
            .project
            .media_pool
            .get(media_id)
            .is_some_and(|item| file_label(&item.path) == name);
        if !unchanged {
            self.session
                .apply(Box::new(vv_core::RenameTimeline::new(media_id, name)));
        }
    }

    /// The copy is called "<name> copy", "<name> copy 2", ... and ends up
    /// selected.
    pub(crate) fn duplicate_timeline(&mut self, media_id: MediaId) {
        let Some(item) = self.session.project.media_pool.get(media_id) else {
            return;
        };
        let base = format!("{} {}", file_label(&item.path), t!("pool.copy_suffix"));
        let taken: std::collections::HashSet<String> = self
            .session
            .project
            .media_pool
            .values()
            .filter(|m| m.compound.is_some())
            .map(|m| file_label(&m.path))
            .collect();
        let name = std::iter::once(base.clone())
            .chain((2..).map(|n| format!("{base} {n}")))
            .find(|name| !taken.contains(name))
            .expect("infinite candidates");
        if let Some((copy, add)) =
            vv_core::pool::duplicate_timeline(&mut self.session.project, media_id, name)
        {
            self.session.apply(Box::new(add));
            self.media_pool_state.select_only([copy]);
        }
    }

    /// Contents of the Media pool section in the left column.
    pub(crate) fn show_media_pool(
        &mut self,
        ui: &mut egui::Ui,
        preview_action: &mut Option<MediaId>,
    ) {
        if let Some(worker) = &self.proxy_worker {
            let progress = worker.progress();
            let paused = worker.is_paused();
            if progress.finished < progress.total {
                ui.horizontal(|ui| {
                    let label = if paused {
                        t!("pool.resume")
                    } else {
                        t!("pool.pause")
                    };
                    if ui
                        .small_button(label)
                        .on_hover_text(t!("pool.proxy_generation"))
                        .clicked()
                    {
                        worker.set_paused(!paused);
                    }
                    ui.add(
                        egui::ProgressBar::new(progress.fraction)
                            .text(format!("Proxy {}/{}", progress.finished, progress.total)),
                    );
                });
                if !paused {
                    ui.ctx()
                        .request_repaint_after(std::time::Duration::from_millis(100));
                }
            }
        }
        let search_changed = media_pool_search_field(ui, &mut self.media_pool_state.search);
        media_pool_header(ui, &mut self.media_pool_state);
        kinetic_pool_scroll(
            ui.ctx(),
            &mut self.media_pool_state,
            self.settings.kinetic_scroll_media_pool,
        );
        // `auto_shrink` off: the panel must fill the assigned width,
        // otherwise its resize springs back.
        let output = egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                let mut items: Vec<(MediaId, String, vv_core::MediaMeta, u64, bool)> = self
                    .session
                    .project
                    .media_pool
                    .iter()
                    .map(|(id, item)| {
                        (
                            id,
                            file_label(&item.path),
                            item.meta.clone(),
                            item.content_hash,
                            item.compound.is_some(),
                        )
                    })
                    .collect();
                let query = self.media_pool_state.search.trim().to_string();
                if !query.is_empty() {
                    items.retain(|(_, label, ..)| media_pool::matches_search(label, &query));
                }
                // Hidden items must not stay selected: Del would delete them.
                if search_changed {
                    self.media_pool_state
                        .selected
                        .retain(|id| items.iter().any(|item| item.0 == *id));
                }
                media_pool::sort_items(
                    &mut items,
                    self.media_pool_state.sort,
                    |(_, label, ..)| label.as_str(),
                    |(_, _, meta, ..)| meta.duration_frames as f64 / meta.fps.as_f64().max(1e-9),
                );
                let folders: Vec<(FolderId, &str, Option<FolderId>)> = self
                    .session
                    .project
                    .folders
                    .iter()
                    .map(|(id, f)| (id, f.name.as_str(), f.parent))
                    .collect();
                let items = items
                    .into_iter()
                    .map(|item| {
                        let folder = self.session.project.media_pool[item.0].folder;
                        (item, folder)
                    })
                    .collect();
                let rows = if query.is_empty() {
                    media_pool::tree_rows(&folders, items, &self.media_pool_state.expanded)
                } else {
                    media_pool::search_rows(&folders, items)
                };
                if rows.is_empty() && !query.is_empty() {
                    ui.weak(t!("pool.search_no_results"));
                }
                let order: Vec<MediaId> = rows
                    .iter()
                    .filter_map(|row| match row {
                        media_pool::PoolRow::Item { item, .. } => Some(item.0),
                        media_pool::PoolRow::Folder { .. } => None,
                    })
                    .collect();
                let drags: Vec<timeline_ui::MediaDrag> = order
                    .iter()
                    .map(|id| {
                        timeline_ui::MediaDrag::whole(
                            *id,
                            &self.session.project.media_pool[*id].meta,
                        )
                    })
                    .collect();
                // Interacted with before the items: in egui the last one wins, so a click
                // on an item does not start the selection rectangle. The clip rect
                // is the viewport: `available_rect_before_wrap` is viewport-sized
                // but scrolls away with the content.
                let bg = ui.interact(
                    ui.clip_rect(),
                    ui.id().with("media_pool_bg"),
                    egui::Sense::click_and_drag(),
                );
                bg.context_menu(|ui| {
                    if ui.button(t!("pool.new_folder")).clicked() {
                        self.new_folder(None);
                        ui.close();
                    }
                    ui.menu_button(t!("pool.timelines"), |ui| {
                        if ui.button(t!("menu.import_otio")).clicked() {
                            self.import_otio_dialog();
                            ui.close();
                        }
                        if ui.button(t!("pool.new_timeline")).clicked() {
                            self.open_new_timeline_dialog();
                            ui.close();
                        }
                    });
                });
                let mut item_rects: Vec<(MediaId, egui::Rect)> = Vec::new();
                // The items leave a strip of background on the sides (and
                // `BOTTOM_PAD` below): with a full pool there would be no
                // free spot left to right click on.
                egui::Frame::NONE
                    .inner_margin(egui::Margin::symmetric(SIDE_PAD, 0))
                    .show(ui, |ui| {
                        for row in rows {
                            let ((id, label, meta, content_hash, is_timeline), depth, folder) =
                                match row {
                                    media_pool::PoolRow::Folder { id, depth } => {
                                        indented(ui, depth, |ui| self.folder_row(ui, id));
                                        continue;
                                    }
                                    media_pool::PoolRow::Item { item, depth } => {
                                        let folder = self.session.project.media_pool[item.0].folder;
                                        (item, depth, folder)
                                    }
                                };
                            let proxy_state = self
                                .proxy_worker
                                .as_ref()
                                .and_then(|w| w.state(content_hash));
                            let offline = self.media_pool_state.is_offline(id);
                            let thumbnail = if is_timeline || offline {
                                None
                            } else {
                                self.thumbnails.get(&content_hash).cloned().flatten()
                            };
                            let renaming = self
                                .media_pool_state
                                .renaming
                                .as_mut()
                                .filter(|r| r.target == media_pool::RenameTarget::Media(id));
                            let is_renaming = renaming.is_some();
                            let mut rename_done = None;
                            let mut label_rect = egui::Rect::NOTHING;
                            let group_resp = indented(ui, depth, |ui| {
                                ui.group(|ui| {
                                    ui.set_min_width(ui.available_width());
                                    ui.horizontal(|ui| {
                                        let thumb_size = egui::vec2(64.0, 36.0);
                                        match &thumbnail {
                                            Some(texture) => {
                                                let tex_size = texture.size_vec2();
                                                let scale = (thumb_size.x / tex_size.x)
                                                    .min(thumb_size.y / tex_size.y);
                                                let (rect, _) = ui.allocate_exact_size(
                                                    thumb_size,
                                                    egui::Sense::hover(),
                                                );
                                                ui.painter().rect_filled(
                                                    rect,
                                                    2.0,
                                                    egui::Color32::BLACK,
                                                );
                                                egui::Image::new(texture)
                                                    .fit_to_exact_size(tex_size * scale)
                                                    .paint_at(
                                                        ui,
                                                        egui::Rect::from_center_size(
                                                            rect.center(),
                                                            tex_size * scale,
                                                        ),
                                                    );
                                            }
                                            None => {
                                                let (rect, _) = ui.allocate_exact_size(
                                                    thumb_size,
                                                    egui::Sense::hover(),
                                                );
                                                ui.painter().rect_filled(
                                                    rect,
                                                    2.0,
                                                    ui.visuals().extreme_bg_color,
                                                );
                                                if offline {
                                                    paint_offline_thumbnail(ui.painter(), rect);
                                                } else if is_timeline {
                                                    paint_film_icon(
                                                        ui.painter(),
                                                        rect,
                                                        ui.visuals().weak_text_color(),
                                                        ui.visuals().extreme_bg_color,
                                                    );
                                                } else if !meta.has_video {
                                                    ui.painter().text(
                                                        rect.center(),
                                                        egui::Align2::CENTER_CENTER,
                                                        "🔊",
                                                        egui::FontId::proportional(18.0),
                                                        ui.visuals().weak_text_color(),
                                                    );
                                                }
                                            }
                                        }
                                        ui.vertical(|ui| {
                                            match renaming {
                                                Some(rename) => {
                                                    rename_done = rename_field(ui, rename);
                                                }
                                                None => label_rect = ui.label(&label).rect,
                                            }
                                            ui.small(if meta.is_image() {
                                                t!(
                                                    "pool.meta_image",
                                                    width = meta.width,
                                                    height = meta.height
                                                )
                                            } else if meta.has_video {
                                                format!(
                                                    "{}x{} · {:.2}fps · {}",
                                                    meta.width,
                                                    meta.height,
                                                    meta.fps.as_f64(),
                                                    if meta.has_audio {
                                                        t!("pool.audio")
                                                    } else {
                                                        t!("pool.muted")
                                                    }
                                                )
                                                .into()
                                            } else {
                                                t!(
                                                    "pool.meta_audio",
                                                    rate = meta.sample_rate,
                                                    channels = meta.channels
                                                )
                                            });
                                        });
                                        ui.with_layout(
                                            egui::Layout::right_to_left(egui::Align::Center),
                                            |ui| {
                                                // An image has no real duration.
                                                let duration_label = if meta.is_image() {
                                                    "—".to_string()
                                                } else {
                                                    format_duration(
                                                        meta.duration_frames,
                                                        meta.fps.as_f64(),
                                                    )
                                                };
                                                ui.add_sized(
                                                    egui::vec2(
                                                        DURATION_COL_W,
                                                        ui.available_height(),
                                                    ),
                                                    egui::Label::new(
                                                        egui::RichText::new(duration_label)
                                                            .monospace(),
                                                    ),
                                                );
                                                match proxy_state {
                                                    Some(proxy_worker::ProxyState::Generating(
                                                        f,
                                                    )) => {
                                                        proxy_progress_ring(ui, Some(f))
                                                            .on_hover_text(t!(
                                                                "pool.proxy_progress",
                                                                percent =
                                                                    format!("{:.0}", f * 100.0)
                                                            ));
                                                    }
                                                    Some(proxy_worker::ProxyState::Queued) => {
                                                        proxy_progress_ring(ui, None)
                                                            .on_hover_text(t!("pool.proxy_queued"));
                                                    }
                                                    Some(proxy_worker::ProxyState::Failed) => {
                                                        ui.colored_label(egui::Color32::RED, "!")
                                                            .on_hover_text(t!("pool.proxy_failed"));
                                                    }
                                                    _ => {}
                                                }
                                            },
                                        );
                                    });
                                })
                                .response
                            });
                            if proxy_state == Some(proxy_worker::ProxyState::Ready) {
                                let rect = group_resp.rect.shrink(1.0);
                                ui.painter().rect_filled(
                                    egui::Rect::from_min_size(
                                        rect.left_top(),
                                        egui::vec2(2.0, rect.height()),
                                    ),
                                    1.0,
                                    timeline_ui::PROXY_COLOR,
                                );
                            }
                            // Double click: preview. Drag: adds the media onto the timeline.
                            let interact_id = ui.id().with("media_pool_item").with(id);
                            // While renaming, the item must not steal the clicks of
                            // the text field.
                            let sense = if is_renaming {
                                egui::Sense::hover()
                            } else {
                                egui::Sense::click_and_drag()
                            };
                            let resp = ui
                                .interact(group_resp.rect, interact_id, sense)
                                .on_hover_text(t!("pool.item_hint"));
                            if let Some(new_name) = rename_done {
                                self.media_pool_state.renaming = None;
                                if let Some(name) = new_name {
                                    self.rename_timeline(id, name);
                                }
                            }
                            item_rects.push((id, group_resp.rect));
                            if self.media_pool_state.reveal == Some(id) {
                                self.media_pool_state.reveal = None;
                                ui.scroll_to_rect(group_resp.rect, Some(egui::Align::Center));
                            }
                            if self.media_pool_state.selected.contains(&id) {
                                ui.painter().rect_stroke(
                                    group_resp.rect,
                                    4.0,
                                    egui::Stroke::new(2.0, egui::Color32::WHITE),
                                    egui::StrokeKind::Inside,
                                );
                                ui.painter().rect_filled(
                                    group_resp.rect,
                                    4.0,
                                    egui::Color32::from_white_alpha(18),
                                );
                            }
                            if resp.clicked() {
                                let modifiers = ui.input(|i| i.modifiers);
                                let on_name = is_timeline
                                    && modifiers.is_none()
                                    && self.media_pool_state.selected.len() == 1
                                    && self.media_pool_state.selected.contains(&id)
                                    && resp
                                        .interact_pointer_pos()
                                        .is_some_and(|pos| label_rect.contains(pos));
                                self.media_pool_state.click(id, modifiers, &order);
                                self.media_pool_state.rename_pending =
                                    on_name.then(|| (id, ui.input(|i| i.time)));
                            }
                            // Right click outside the selection replaces it, like a drag.
                            if resp.secondary_clicked()
                                && !self.media_pool_state.selected.contains(&id)
                            {
                                self.media_pool_state
                                    .click(id, egui::Modifiers::NONE, &order);
                            }
                            resp.context_menu(|ui| {
                                if is_timeline {
                                    if ui.button(t!("pool.duplicate_timeline")).clicked() {
                                        self.duplicate_timeline(id);
                                        ui.close();
                                    }
                                    if ui.button(t!("pool.rename")).clicked() {
                                        self.start_rename(id);
                                        ui.close();
                                    }
                                    ui.separator();
                                }
                                let count = self.media_pool_state.selected.len().max(1);
                                let label = if count > 1 {
                                    t!("pool.relink_many", count = count)
                                } else {
                                    t!("pool.relink_one")
                                };
                                if ui.button(label).clicked() {
                                    self.relink_media_dialog();
                                    ui.close();
                                }
                            });
                            // Dragging an item outside the selection replaces it
                            // with that item (as on the timeline, see
                            // `timeline_ui::drag_group_for`).
                            if resp.drag_started() {
                                self.media_pool_state.rename_pending = None;
                            }
                            if resp.drag_started() && !self.media_pool_state.selected.contains(&id)
                            {
                                self.media_pool_state
                                    .click(id, egui::Modifiers::NONE, &order);
                            }
                            // Dragging an item of the selection drags the whole
                            // selection, in panel order: the timeline appends them
                            // one after the other.
                            let payload = if self.media_pool_state.selected.len() > 1
                                && self.media_pool_state.selected.contains(&id)
                            {
                                timeline_ui::MediaDragSet {
                                    items: drags
                                        .iter()
                                        .filter(|d| {
                                            self.media_pool_state.selected.contains(&d.media_id)
                                        })
                                        .copied()
                                        .collect(),
                                }
                            } else {
                                timeline_ui::MediaDragSet::one(timeline_ui::MediaDrag::whole(
                                    id, &meta,
                                ))
                            };
                            let dragged_count = payload.items.len();
                            resp.dnd_set_drag_payload(payload);
                            self.accept_pool_drop(&resp, folder);
                            if resp.double_clicked() {
                                self.media_pool_state.rename_pending = None;
                                // A compound clip (or a project timeline, see
                                // `MediaItem::compound`) opens as a top level timeline:
                                // from the pool there is no parent to stack in the
                                // breadcrumb. "Preview" makes no sense for it.
                                match self
                                    .session
                                    .project
                                    .media_pool
                                    .get(id)
                                    .and_then(|m| m.compound)
                                {
                                    Some(timeline_id) => self.open_timeline(timeline_id),
                                    None => *preview_action = Some(id),
                                }
                            }
                            if resp.dragged() {
                                let ghost = if dragged_count > 1 {
                                    t!("pool.items", count = dragged_count).into_owned()
                                } else {
                                    label.clone()
                                };
                                show_drag_ghost(ui, interact_id, &ghost);
                            }
                        }
                    });
                ui.add_space(BOTTOM_PAD);
                self.accept_pool_drop(&bg, None);
                if let Some((id, clicked_at)) = self.media_pool_state.rename_pending {
                    let wait = ui.ctx().options(|o| o.input_options.max_double_click_delay);
                    let elapsed = ui.input(|i| i.time) - clicked_at;
                    if elapsed > wait {
                        self.media_pool_state.rename_pending = None;
                        self.start_rename(id);
                    } else {
                        ui.ctx()
                            .request_repaint_after(std::time::Duration::from_secs_f64(
                                wait - elapsed,
                            ));
                    }
                }

                if bg.drag_started() {
                    if let Some(pos) = bg.interact_pointer_pos() {
                        self.media_pool_state.marquee = Some((pos, pos));
                    }
                } else if bg.dragged() {
                    if let (Some((_, end)), Some(pos)) = (
                        &mut self.media_pool_state.marquee,
                        bg.interact_pointer_pos(),
                    ) {
                        *end = pos;
                    }
                } else if bg.drag_stopped() {
                    if let Some((start, end)) = self.media_pool_state.marquee.take() {
                        let rect = egui::Rect::from_two_pos(start, end);
                        let hits = item_rects
                            .iter()
                            .filter(|(_, r)| r.intersects(rect))
                            .map(|(id, _)| *id);
                        self.media_pool_state.select_only(hits);
                    }
                } else if bg.clicked() {
                    self.media_pool_state.rename_pending = None;
                    self.media_pool_state.clear();
                }
                if let Some((start, end)) = self.media_pool_state.marquee {
                    let rect = egui::Rect::from_two_pos(start, end);
                    ui.painter()
                        .rect_filled(rect, 0.0, crate::theme::ACCENT_TRANSLUCENT);
                    ui.painter().rect_stroke(
                        rect,
                        0.0,
                        egui::Stroke::new(1.0, crate::theme::ACCENT),
                        egui::StrokeKind::Inside,
                    );
                }
            });
        self.media_pool_state.scroll_area = Some(media_pool::PoolScrollArea {
            id: output.id,
            viewport: output.inner_rect,
            max_offset: (output.content_size.y - output.inner_rect.height()).max(0.0),
        });
    }

    /// Effects section: generators and effects to drag onto the timeline.
    pub(crate) fn show_effects_list(ui: &mut egui::Ui) {
        effects_section_header(ui, &t!("effects.generators"));
        egui::ScrollArea::vertical()
            .id_salt("effects_scroll")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                for generator in timeline_ui::Generator::ALL {
                    effect_item(ui, generator);
                }
                ui.add_space(8.0);
                effects_section_header(ui, &t!("effects.filters"));
                for filter in timeline_ui::FILTER_ENTRIES {
                    filter_item(ui, filter);
                }
                ui.add_space(8.0);
                effects_section_header(ui, &t!("effects.transitions"));
                for transition in timeline_ui::ALL_TRANSITION_KINDS {
                    transition_item(ui, transition);
                }
            });
    }
}

const COLUMN_SPLITTER_HEIGHT: f32 = 8.0;
const MIN_COLUMN_PANE_HEIGHT: f32 = 60.0;

/// Draggable separator between the media pool (above) and the Effects panel
/// (below). Returns the rects of the two panes. The split is kept as a
/// fraction so the panes scale with the window.
pub(crate) fn split_left_column(ui: &egui::Ui, fraction: &mut f32) -> (egui::Rect, egui::Rect) {
    let column = ui.available_rect_before_wrap();
    let panes_height = (column.height() - COLUMN_SPLITTER_HEIGHT).max(0.0);
    let max_pool = (panes_height - MIN_COLUMN_PANE_HEIGHT).max(0.0);
    let clamp_pool = |h: f32| h.min(max_pool).max(MIN_COLUMN_PANE_HEIGHT.min(max_pool));
    let mut pool_height = clamp_pool(*fraction * panes_height);
    let splitter_rect = |pool_height: f32| {
        egui::Rect::from_min_size(
            column.min + egui::vec2(0.0, pool_height),
            egui::vec2(column.width(), COLUMN_SPLITTER_HEIGHT),
        )
    };
    let resp = ui.interact(
        splitter_rect(pool_height),
        ui.id().with("left_column_splitter"),
        egui::Sense::drag(),
    );
    let active = resp.hovered() || resp.dragged();
    if active {
        ui.ctx()
            .output_mut(|o| o.cursor_icon = egui::CursorIcon::ResizeVertical);
    }
    if resp.dragged() && panes_height > 0.0 {
        pool_height = clamp_pool(pool_height + resp.drag_delta().y);
        *fraction = pool_height / panes_height;
    }
    let splitter = splitter_rect(pool_height);
    ui.painter().hline(
        splitter.x_range(),
        splitter.center().y,
        egui::Stroke::new(1.0, egui::Color32::from_gray(if active { 160 } else { 80 })),
    );
    let pool = egui::Rect::from_min_max(column.min, egui::pos2(column.max.x, splitter.top()));
    let effects = egui::Rect::from_min_max(egui::pos2(column.min.x, splitter.bottom()), column.max);
    (pool, effects)
}

/// Touchpad swipe on the pool, applied before its ScrollArea's `show` like
/// the timeline's (`timeline_ui::sync_timeline_scroll`): the swipe's speed
/// becomes an inertia that keeps scrolling after release.
fn kinetic_pool_scroll(ctx: &egui::Context, state: &mut media_pool::MediaPoolState, enabled: bool) {
    if !enabled {
        state.scroll_vel = 0.0;
        return;
    }
    let Some(area) = state.scroll_area else {
        return;
    };
    let Some(mut scroll_state) = egui::containers::scroll_area::State::load(ctx, area.id) else {
        return;
    };
    let dt = ctx.input(|i| i.stable_dt).min(0.1);
    let wheel = if timeline_ui::pointer_over(ctx, area.viewport) {
        ctx.input(|i| i.smooth_scroll_delta.y)
    } else {
        0.0
    };
    if wheel != 0.0 {
        scroll_state.offset.y = (scroll_state.offset.y - wheel).clamp(0.0, area.max_offset);
        state.scroll_vel = if dt > 0.0 {
            -timeline_ui::KINETIC_VELOCITY_GAIN * wheel / dt
        } else {
            0.0
        };
        ctx.input_mut(|i| i.smooth_scroll_delta.y = 0.0);
        scroll_state.store(ctx, area.id);
    } else if timeline_ui::apply_kinetic_scroll(
        &mut scroll_state.offset.y,
        &mut state.scroll_vel,
        area.max_offset,
        dt,
    ) {
        scroll_state.store(ctx, area.id);
        ctx.request_repaint();
    }
}

#[cfg(test)]
#[path = "tests/media_pool_ui.rs"]
mod tests;
