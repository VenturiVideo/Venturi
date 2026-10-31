//! Media pool and effects panels.

use super::*;
use media_pool::RenameTarget;
use pool_view::{
    HEADER_HEIGHT, MenuChoice, PoolEntry, PoolEvent, PoolFolder, PoolMenus, PoolView, Thumb,
};

/// "New folder", or "New folder 2", ... if `taken` has it.
pub(crate) fn new_folder_name(taken: &[&str]) -> String {
    let base = t!("pool.new_folder_name").into_owned();
    std::iter::once(base.clone())
        .chain((2..).map(|n| format!("{base} {n}")))
        .find(|name| !taken.contains(&name.as_str()))
        .expect("infinite candidates")
}

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
    timeline_ui::paint_fx_icon(painter, thumb.center(), 15.0, egui::Color32::from_gray(220));
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
#[derive(Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum EffectsTab {
    #[default]
    Effects,
    Presets,
}

#[derive(Default)]
pub(crate) struct EffectsPanelState {
    pub tab: EffectsTab,
    pub search: String,
}

/// Magic wand with sparkles near its tip.
fn paint_wand_icon(painter: &egui::Painter, center: egui::Pos2, color: egui::Color32) {
    painter.line_segment(
        [
            center + egui::vec2(-7.0, 7.0),
            center + egui::vec2(3.0, -3.0),
        ],
        egui::Stroke::new(2.0, color),
    );
    let stroke = egui::Stroke::new(1.2, color);
    for (offset, arm) in [((5.0, -5.0), 3.0), ((-2.0, -6.0), 1.8), ((6.0, 2.0), 1.8)] {
        let at = center + egui::vec2(offset.0, offset.1);
        painter.line_segment(
            [at - egui::vec2(arm, 0.0), at + egui::vec2(arm, 0.0)],
            stroke,
        );
        painter.line_segment(
            [at - egui::vec2(0.0, arm), at + egui::vec2(0.0, arm)],
            stroke,
        );
    }
}

pub(crate) fn file_label(path: &std::path::Path) -> String {
    path.file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("?")
        .to_string()
}

/// Context menu entries of the project's pool.
struct ProjectMenus;

enum ProjectAction {
    Delete,
    DuplicateTimeline(MediaId),
    Relink,
    AddToLibrary,
    RelinkFolder(FolderId),
    ImportOtio,
    NewTimeline,
}

impl PoolMenus<MediaId, FolderId> for ProjectMenus {
    type Action = ProjectAction;

    fn item_menu(
        &self,
        ui: &mut egui::Ui,
        entry: &PoolEntry<MediaId, FolderId>,
        selected: usize,
        out: &mut Vec<MenuChoice<MediaId, ProjectAction>>,
    ) {
        let mut action = |ui: &mut egui::Ui, label, action| {
            if ui.button(label).clicked() {
                out.push(MenuChoice::Action(action));
                ui.close();
            }
        };
        // Only timelines can be renamed: a media keeps its file name.
        if entry.renamable {
            action(
                ui,
                t!("pool.duplicate_timeline"),
                ProjectAction::DuplicateTimeline(entry.id),
            );
            pool_view::rename_button(ui, entry, out);
            ui.separator();
        }
        let relink = if selected > 1 {
            t!("pool.relink_many", count = selected)
        } else {
            t!("pool.relink_one")
        };
        if ui.button(relink).clicked() {
            out.push(MenuChoice::Action(ProjectAction::Relink));
            ui.close();
        }
        ui.separator();
        if ui
            .button(t!("pool.add_to_library"))
            .on_hover_text(t!("pool.add_to_library_hint"))
            .clicked()
        {
            out.push(MenuChoice::Action(ProjectAction::AddToLibrary));
            ui.close();
        }
        ui.separator();
        if ui.button(t!("menu.delete")).clicked() {
            out.push(MenuChoice::Action(ProjectAction::Delete));
            ui.close();
        }
    }

    fn folder_menu(&self, ui: &mut egui::Ui, folder: &FolderId, out: &mut Vec<ProjectAction>) {
        if ui.button(t!("pool.relink_folder")).clicked() {
            out.push(ProjectAction::RelinkFolder(*folder));
            ui.close();
        }
    }

    fn background_menu(&self, ui: &mut egui::Ui, out: &mut Vec<ProjectAction>) {
        ui.menu_button(t!("pool.timelines"), |ui| {
            if ui.button(t!("menu.import_otio")).clicked() {
                out.push(ProjectAction::ImportOtio);
                ui.close();
            }
            if ui.button(t!("pool.new_timeline")).clicked() {
                out.push(ProjectAction::NewTimeline);
                ui.close();
            }
        });
    }
}

impl VenturiApp {
    /// Only folders and timelines can be renamed: a media keeps its file
    /// name.
    pub(crate) fn rename_selected_pool_item(&mut self) {
        let state = &mut self.media_pool_state;
        if let Some(folder) = state.selected_folder {
            if let Some(f) = self.session.project.folders.get(folder) {
                state.start_rename(RenameTarget::Folder(folder), f.name.clone());
            }
            return;
        }
        if let Some(&id) = state.single_selected()
            && let Some(item) = self.session.project.media_pool.get(id)
            && item.compound.is_some()
        {
            state.start_rename(RenameTarget::Item(id), file_label(&item.path));
        }
    }

    /// Enter in the pool: the same as double clicking the only selected item.
    pub(crate) fn open_selected_pool_compound(&mut self) {
        if let Some(&id) = self.media_pool_state.single_selected() {
            self.media_pool_state.rename_pending = None;
            self.open_pool_compound(id);
        }
    }

    /// Opens `id` if it is a compound clip or timeline; false otherwise.
    fn open_pool_compound(&mut self, id: MediaId) -> bool {
        let compound = self
            .session
            .project
            .media_pool
            .get(id)
            .and_then(|m| m.compound);
        if let Some(timeline_id) = compound {
            self.open_timeline(timeline_id);
        }
        compound.is_some()
    }

    /// Selects `media` and makes it visible: opens the pool and the folders
    /// containing it, drops a search hiding it, scrolls to it.
    pub(crate) fn reveal_in_media_pool(&mut self, media: MediaId) {
        let Some(item) = self.session.project.media_pool.get(media) else {
            return;
        };
        self.settings.panels.media_pool_open = true;
        self.library_ui.tab = library_ui::PoolTab::Project;
        let folders = &self.session.project.folders;
        self.media_pool_state
            .reveal(media, &file_label(&item.path), item.folder, |f| {
                folders.get(*f).map(|f| f.parent)
            });
    }

    /// Created expanded (with its parent), with its name ready to edit.
    fn new_folder(&mut self, parent: Option<FolderId>) {
        let taken: Vec<&str> = self
            .session
            .project
            .folders
            .values()
            .filter(|f| f.parent == parent)
            .map(|f| f.name.as_str())
            .collect();
        let name = new_folder_name(&taken);
        let mut add = vv_core::AddEntities::new(vv_core::CommandLabel::NewFolder);
        let folder = add.folder(
            &mut self.session.project,
            vv_core::MediaFolder {
                name: name.clone(),
                parent,
            },
        );
        self.session.apply(Box::new(add));
        self.media_pool_state.expanded.extend(parent);
        self.media_pool_state
            .start_rename(RenameTarget::Folder(folder), name);
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
        preview_action: &mut Option<Browsed>,
    ) {
        let area = ui.available_rect_before_wrap();
        self.library_ui.area = Some(area);
        let content = self.show_pool_tabs(ui, area);
        pool_view::beside_tabs(ui, content, |ui| {
            self.show_library_copy_progress(ui);
            match self.library_ui.tab {
                library_ui::PoolTab::Project => self.show_project_pool(ui, preview_action),
                library_ui::PoolTab::Library => self.show_library(ui, preview_action),
            }
        });
        ui.allocate_rect(area, egui::Sense::hover());
    }

    fn show_project_pool(&mut self, ui: &mut egui::Ui, preview_action: &mut Option<Browsed>) {
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
        let entries = self
            .session
            .project
            .media_pool
            .iter()
            .map(|(id, item)| {
                let thumb = if item.compound.is_some() {
                    Thumb::Timeline
                } else if self.media_pool_state.is_offline(&id) {
                    Thumb::Offline
                } else {
                    match self.thumbnails.get(&item.content_hash).cloned().flatten() {
                        Some(texture) => Thumb::Texture(texture),
                        None => Thumb::None,
                    }
                };
                PoolEntry {
                    id,
                    name: file_label(&item.path),
                    folder: item.folder,
                    meta: item.meta.clone(),
                    thumb,
                    proxy: self
                        .proxy_worker
                        .as_ref()
                        .and_then(|w| w.state(item.content_hash)),
                    renamable: item.compound.is_some(),
                }
            })
            .collect();
        let folders = self
            .session
            .project
            .folders
            .iter()
            .map(|(id, f)| PoolFolder {
                id,
                name: f.name.clone(),
                parent: f.parent,
            })
            .collect();
        let events = pool_view::show_pool(
            ui,
            &mut self.media_pool_state,
            PoolView {
                folders,
                entries,
                menus: &ProjectMenus,
                kinetic_scroll: self.settings.kinetic_scroll_media_pool,
                layout: &mut self.settings.panels.media_pool_layout,
                item_hint: t!("pool.item_hint"),
                empty_hint: None,
            },
        );
        for event in events {
            match event {
                PoolEvent::Rename(id, name) => self.rename_timeline(id, name),
                PoolEvent::RenameFolder(folder, name) => self
                    .session
                    .apply(Box::new(vv_core::RenameFolder::new(folder, name))),
                PoolEvent::NewFolder(parent) => self.new_folder(parent),
                PoolEvent::DeleteFolder(folder) => self
                    .session
                    .apply(Box::new(vv_core::DeleteFolder::new(folder))),
                PoolEvent::MoveItems(media, folder) => self.move_media_to_folder(&media, folder),
                PoolEvent::MoveFolder(folder, parent) => {
                    if self.session.project.can_move_folder(folder, parent) {
                        self.session
                            .apply(Box::new(vv_core::MoveFolder::new(folder, parent)));
                    }
                }
                // A compound clip (or a project timeline, see
                // `MediaItem::compound`) opens as a top level timeline: from
                // the pool there is no parent to stack in the breadcrumb.
                // "Preview" makes no sense for it.
                PoolEvent::Open(id) => {
                    if !self.open_pool_compound(id) {
                        *preview_action = Some(Browsed::Media(id));
                    }
                }
                PoolEvent::DragStarted(media) => {
                    let set = timeline_ui::MediaDragSet {
                        items: media
                            .iter()
                            .map(|&id| {
                                timeline_ui::MediaDrag::whole(
                                    id,
                                    &self.session.project.media_pool[id].meta,
                                )
                            })
                            .collect(),
                    };
                    egui::DragAndDrop::set_payload(ui.ctx(), set);
                }
                PoolEvent::Action(ProjectAction::DuplicateTimeline(id)) => {
                    self.duplicate_timeline(id)
                }
                PoolEvent::Action(ProjectAction::Delete) => self.delete_selected_media(),
                PoolEvent::Action(ProjectAction::Relink) => self.relink_media_dialog(),
                PoolEvent::Action(ProjectAction::AddToLibrary) => {
                    self.add_selected_media_to_library()
                }
                PoolEvent::Action(ProjectAction::RelinkFolder(folder)) => {
                    self.relink_folder_dialog(folder)
                }
                PoolEvent::Action(ProjectAction::ImportOtio) => self.import_otio_dialog(),
                PoolEvent::Action(ProjectAction::NewTimeline) => self.open_new_timeline_dialog(),
            }
        }
    }

    /// Effects panel: generators, filters and transitions to drag onto the
    /// timeline in one tab, the presets in the other.
    pub(crate) fn show_effects_list(&mut self, ui: &mut egui::Ui) {
        let area = ui.available_rect_before_wrap();
        let tabs = [
            pool_view::SideTab {
                tab: EffectsTab::Effects,
                hint: t!("effects.tab_effects"),
                icon: paint_wand_icon,
            },
            pool_view::SideTab {
                tab: EffectsTab::Presets,
                hint: t!("effects.presets"),
                icon: |p, c, color| timeline_ui::paint_preset_icon(p, c, 8.0, color),
            },
        ];
        let (content, _) = pool_view::side_tabs(ui, area, &mut self.effects_panel.tab, &tabs);
        pool_view::beside_tabs(ui, content, |ui| {
            pool_view::search_field(ui, &mut self.effects_panel.search, "effects");
            let query = self.effects_panel.search.trim().to_owned();
            let matches = |label: &str| media_pool::matches_search(label, &query);
            egui::ScrollArea::vertical()
                .id_salt("effects_scroll")
                .auto_shrink([false, false])
                .show(ui, |ui| match self.effects_panel.tab {
                    EffectsTab::Effects => {
                        let generators: Vec<_> = timeline_ui::Generator::ALL
                            .into_iter()
                            .filter(|&g| matches(&timeline_ui::generator_label(g)))
                            .collect();
                        let filters: Vec<_> = timeline_ui::FILTER_ENTRIES
                            .into_iter()
                            .filter(|f| matches(&f.label()))
                            .collect();
                        let transitions: Vec<_> = timeline_ui::ALL_TRANSITION_KINDS
                            .into_iter()
                            .filter(|&k| matches(&timeline_ui::transition_kind_label(k)))
                            .collect();
                        if generators.is_empty() && filters.is_empty() && transitions.is_empty() {
                            ui.weak(t!("pool.search_no_results"));
                        }
                        if !generators.is_empty() {
                            effects_section_header(ui, &t!("effects.generators"));
                            for generator in generators {
                                effect_item(ui, generator);
                            }
                            ui.add_space(8.0);
                        }
                        if !filters.is_empty() {
                            effects_section_header(ui, &t!("effects.filters"));
                            for filter in filters {
                                filter_item(ui, filter);
                            }
                            ui.add_space(8.0);
                        }
                        if !transitions.is_empty() {
                            effects_section_header(ui, &t!("effects.transitions"));
                            for transition in transitions {
                                transition_item(ui, transition);
                            }
                        }
                    }
                    EffectsTab::Presets => self.show_preset_list(ui, &query),
                });
        });
        ui.allocate_rect(area, egui::Sense::hover());
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

#[cfg(test)]
#[path = "tests/media_pool_ui.rs"]
mod tests;
