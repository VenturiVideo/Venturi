//! Menu bar and keyboard shortcuts.

use super::*;

impl VenturiApp {
    pub(crate) fn handle_shortcuts(&mut self, ui: &mut egui::Ui) {
        // Copy/paste are handled outside `ui.input`: `ctx.copy_text` takes
        // the same lock and would deadlock inside it.
        let mut clipboard_events: Vec<egui::Event> = Vec::new();
        let mut arrow_input = (None, 0.0);
        let mut set_fullscreen = None;
        // Keys typed into a text field (e.g. the title) are not shortcuts:
        // "T" would cut the clips, Backspace would delete them.
        let typing = ui.ctx().egui_wants_keyboard_input();
        if let Some(timeline_id) = self.timeline_id {
            self.timeline_state
                .drop_locked(&self.session.project.timelines[timeline_id]);
        }
        let capturing_shortcut = self
            .settings_dialog
            .as_ref()
            .is_some_and(|d| d.is_capturing());
        let keymap = self.settings.keymap.clone();
        ui.input(|i| {
            arrow_input.1 = i.time;
            if typing || capturing_shortcut {
                return;
            }
            let pressed = |action| keymap.pressed(action, i);
            arrow_input.0 = match (
                keymap.down(Action::StepBackward, i),
                keymap.down(Action::StepForward, i),
            ) {
                (true, false) => Some(-1),
                (false, true) => Some(1),
                _ => None,
            };
            if pressed(Action::Delete) {
                // The panel that got the last click decides who deletes:
                // keyframe editor, media pool or timeline.
                if self.keyframe_editor.owns_delete() {
                    self.delete_selected_keyframes();
                } else if self.media_pool_state.focused {
                    self.delete_selected_media();
                } else {
                    self.delete_selected();
                }
            }
            if pressed(Action::RippleDelete) {
                self.ripple_delete_selected();
            }
            if pressed(Action::Split) {
                self.split_at_playhead();
            }
            if pressed(Action::SelectTool) {
                self.timeline_state.tool = timeline_ui::TimelineTool::Select;
            }
            if pressed(Action::SlipTool) {
                self.timeline_state.tool = timeline_ui::TimelineTool::Slip;
            }
            if pressed(Action::ToggleDisabled) {
                self.toggle_disabled_selected();
            }
            if pressed(Action::RetimeControls) {
                self.toggle_retime_controls();
            }
            if pressed(Action::Undo) {
                self.undo();
            }
            if pressed(Action::Redo) {
                self.redo();
            }
            if pressed(Action::TogglePlayback) {
                self.toggle_playback();
            }
            if pressed(Action::MarkIn) {
                self.mark_at_playhead(true);
            }
            if pressed(Action::MarkOut) {
                self.mark_at_playhead(false);
            }
            if pressed(Action::FastPlayback) {
                self.handle_fast_playback_key();
            }
            if pressed(Action::SelectAll) {
                // Same rule as Del: the panel with the last click
                // decides what Ctrl+A selects.
                if self.media_pool_state.focused {
                    self.select_all_media();
                } else {
                    self.select_all_clips();
                }
            }
            if pressed(Action::Rename) && self.media_pool_state.focused {
                self.rename_selected_pool_item();
            }
            if pressed(Action::SelectFromPlayhead) {
                self.select_clips_from_playhead();
            }
            if pressed(Action::ImportMedia) {
                self.import_media_dialog();
            }
            if pressed(Action::SaveProject) {
                self.save_project();
            }
            if pressed(Action::SaveProjectAs) {
                self.save_project_as();
            }
            if pressed(Action::NewProject) {
                self.request_project_switch(ProjectSwitch::New);
            }
            if pressed(Action::OpenProject) {
                self.request_project_switch(ProjectSwitch::Open);
            }
            if pressed(Action::Export) {
                self.start_export();
            }
            // Only collected: handled outside here, see above.
            for (action, event) in [
                (Action::Copy, egui::Event::Copy),
                (Action::Cut, egui::Event::Cut),
                (Action::Paste, egui::Event::Paste(String::new())),
            ] {
                if pressed(action) {
                    clipboard_events.push(event);
                }
            }
            if pressed(Action::PasteAttributes) {
                self.open_paste_attributes_dialog();
            }
            if pressed(Action::ZoomIn) {
                self.timeline_state.zoom_in();
            }
            if pressed(Action::ZoomOut) {
                self.timeline_state.zoom_out();
            }
            if pressed(Action::AddMarker) {
                self.add_marker_at_playhead();
            }
            if pressed(Action::ViewerZoomFit) {
                self.viewer_zoom.fit();
            }
            if pressed(Action::ViewerZoomActual)
                && let Some((area, frame_px)) = self.viewer_geometry
            {
                self.viewer_zoom
                    .set_scale(1.0, area, frame_px, i.pixels_per_point);
            }
            if pressed(Action::FullscreenViewer) {
                set_fullscreen = Some(!self.viewer_fullscreen);
            }
            if self.viewer_fullscreen && i.key_pressed(egui::Key::Escape) {
                set_fullscreen = Some(false);
            }
        });
        // Outside `ui.input`: `send_viewport_cmd` takes the same lock again.
        if let Some(on) = set_fullscreen.take() {
            self.viewer_fullscreen = on;
            ui.ctx()
                .send_viewport_cmd(egui::ViewportCommand::Fullscreen(on));
        }
        self.handle_clipboard_events(ui, &clipboard_events);
        if self.step_playhead_with_arrows(arrow_input.0, arrow_input.1) {
            ui.ctx().request_repaint();
        }
    }

    pub(crate) fn show_menu_bar(&mut self, ui: &mut egui::Ui) {
        let keymap = self.settings.keymap.clone();
        let mut set_fullscreen = None;
        egui::Panel::top("menu_bar").show(ui, |ui| {
            egui::MenuBar::new().ui(ui, |ui| {
                let mut bar_menus = Vec::new();
                let export_disabled = self.timeline_id.is_none()
                    || self.export.is_some()
                    || self.export_dialog.is_some();

                bar_menus.push(
                    ui.menu_button(t!("menu.file"), |ui| {
                        if ui
                            .button(keymap.menu_label(&t!("menu.new_project"), Action::NewProject))
                            .clicked()
                        {
                            self.request_project_switch(ProjectSwitch::New);
                            ui.close();
                        }
                        if ui
                            .button(
                                keymap.menu_label(&t!("menu.open_project"), Action::OpenProject),
                            )
                            .clicked()
                        {
                            self.request_project_switch(ProjectSwitch::Open);
                            ui.close();
                        }
                        self.recent_projects_menu(ui);
                        if ui
                            .button(keymap.menu_label(&t!("menu.save"), Action::SaveProject))
                            .clicked()
                        {
                            self.save_project();
                            ui.close();
                        }
                        if ui
                            .button(keymap.menu_label(&t!("menu.save_as"), Action::SaveProjectAs))
                            .clicked()
                        {
                            self.save_project_as();
                            ui.close();
                        }
                        ui.separator();
                        if ui
                            .button(
                                keymap.menu_label(&t!("menu.import_media"), Action::ImportMedia),
                            )
                            .clicked()
                        {
                            self.import_media_dialog();
                            ui.close();
                        }
                        if ui
                            .button(t!("menu.import_otio"))
                            .on_hover_text(t!("menu.import_otio_hint"))
                            .clicked()
                        {
                            self.import_otio_dialog();
                            ui.close();
                        }
                        ui.separator();
                        if ui
                            .add_enabled(
                                !export_disabled,
                                egui::Button::new(
                                    keymap.menu_label(&t!("menu.export"), Action::Export),
                                ),
                            )
                            .on_hover_text(t!("menu.export_hint"))
                            .clicked()
                        {
                            self.start_export();
                            ui.close();
                        }
                        if ui
                            .add_enabled(
                                self.timeline_id.is_some(),
                                egui::Button::new(t!("menu.export_otio")),
                            )
                            .on_hover_text(t!("menu.export_otio_hint"))
                            .clicked()
                        {
                            self.export_otio_dialog();
                            ui.close();
                        }
                        ui.separator();
                        if ui.button(t!("menu.settings")).clicked() {
                            self.open_settings(settings_dialog::Section::General);
                            ui.close();
                        }
                    })
                    .response,
                );

                bar_menus.push(
                    ui.menu_button(t!("menu.edit"), |ui| {
                        if ui
                            .button(keymap.menu_label(&t!("menu.undo"), Action::Undo))
                            .clicked()
                        {
                            self.undo();
                            ui.close();
                        }
                        if ui
                            .button(keymap.menu_label(&t!("menu.redo"), Action::Redo))
                            .clicked()
                        {
                            self.redo();
                            ui.close();
                        }
                        self.undo_history_menu(ui);
                        ui.separator();
                        if ui
                            .add_enabled(
                                !self.timeline_state.selected.is_empty(),
                                egui::Button::new(
                                    keymap.menu_label(&t!("menu.copy"), Action::Copy),
                                ),
                            )
                            .clicked()
                        {
                            // Also writes the placeholder to the system clipboard, see
                            // `handle_clipboard_events`.
                            self.handle_clipboard_events(ui, &[egui::Event::Copy]);
                            ui.close();
                        }
                        if ui
                            .add_enabled(
                                !self.timeline_state.selected.is_empty(),
                                egui::Button::new(keymap.menu_label(&t!("menu.cut"), Action::Cut)),
                            )
                            .clicked()
                        {
                            self.handle_clipboard_events(ui, &[egui::Event::Cut]);
                            ui.close();
                        }
                        if ui
                            .add_enabled(
                                !self.timeline_state.clipboard.is_empty(),
                                egui::Button::new(
                                    keymap.menu_label(&t!("menu.paste"), Action::Paste),
                                ),
                            )
                            .on_hover_text(t!("menu.paste_hint"))
                            .clicked()
                        {
                            self.paste_clipboard_at_playhead();
                            ui.close();
                        }
                        if ui
                            .add_enabled(
                                !self.timeline_state.clipboard.is_empty()
                                    && !self.timeline_state.selected.is_empty(),
                                egui::Button::new(keymap.menu_label(
                                    &t!("menu.paste_attributes"),
                                    Action::PasteAttributes,
                                )),
                            )
                            .on_hover_text(t!("menu.paste_attributes_hint"))
                            .clicked()
                        {
                            self.open_paste_attributes_dialog();
                            ui.close();
                        }
                        ui.separator();
                        if ui
                            .button(keymap.menu_label(&t!("menu.delete"), Action::Delete))
                            .clicked()
                        {
                            self.delete_selected();
                            ui.close();
                        }
                        if ui
                            .button(
                                keymap.menu_label(&t!("menu.ripple_delete"), Action::RippleDelete),
                            )
                            .on_hover_text(t!("menu.ripple_delete_hint"))
                            .clicked()
                        {
                            self.ripple_delete_selected();
                            ui.close();
                        }
                        if ui
                            .button(keymap.menu_label(&t!("menu.split"), Action::Split))
                            .on_hover_text(t!("menu.split_hint"))
                            .clicked()
                        {
                            self.split_at_playhead();
                            ui.close();
                        }
                    })
                    .response,
                );

                bar_menus.push(
                    ui.menu_button(t!("menu.timeline"), |ui| {
                        // Checkboxes: they stay open on click, unlike the
                        // action buttons elsewhere in the menus.
                        ui.checkbox(
                            &mut self.selection_follows_playhead,
                            t!("menu.selection_follows_playhead"),
                        )
                        .on_hover_text(t!("menu.selection_follows_playhead_hint"));
                        ui.checkbox(&mut self.scrub_audio, t!("menu.scrub_audio"))
                            .on_hover_text(t!("menu.scrub_audio_hint"));
                        ui.separator();
                        if ui
                            .button(keymap.menu_label(&t!("menu.zoom_in"), Action::ZoomIn))
                            .clicked()
                        {
                            self.timeline_state.zoom_in();
                            ui.close();
                        }
                        if ui
                            .button(keymap.menu_label(&t!("menu.zoom_out"), Action::ZoomOut))
                            .clicked()
                        {
                            self.timeline_state.zoom_out();
                            ui.close();
                        }
                        ui.label(t!("menu.zoom_hint"));
                        ui.separator();
                        if ui
                            .button(keymap.menu_label(
                                &t!("timeline.retime_controls"),
                                Action::RetimeControls,
                            ))
                            .clicked()
                        {
                            self.toggle_retime_controls();
                            ui.close();
                        }
                        if ui
                            .add_enabled(
                                !self.timeline_state.selected.is_empty(),
                                egui::Button::new(t!("timeline.remove_silences")),
                            )
                            .clicked()
                        {
                            self.timeline_state.silence_dialog_requested =
                                Some(self.timeline_state.selected.iter().copied().collect());
                            ui.close();
                        }
                        if ui
                            .add_enabled(
                                self.timeline_id.is_some(),
                                egui::Button::new(
                                    keymap
                                        .menu_label(&t!("timeline.add_marker"), Action::AddMarker),
                                ),
                            )
                            .clicked()
                        {
                            self.add_marker_at_playhead();
                            ui.close();
                        }
                    })
                    .response,
                );

                bar_menus.push(
                    ui.menu_button(t!("menu.playback"), |ui| {
                        if ui
                            .checkbox(&mut self.settings.proxy_enabled, t!("menu.use_proxy"))
                            .on_hover_text(t!("menu.use_proxy_hint"))
                            .changed()
                        {
                            self.apply_proxy_settings();
                            self.persist_settings();
                        }
                        if ui.button(t!("menu.playback_settings")).clicked() {
                            self.open_settings(settings_dialog::Section::Playback);
                            ui.close();
                        }
                    })
                    .response,
                );

                bar_menus.push(
                    ui.menu_button(t!("menu.view"), |ui| {
                        ui.checkbox(
                            &mut self.settings.panels.inspector_open,
                            t!("menu.inspector"),
                        );
                        ui.checkbox(&mut self.audiometer_enabled, t!("menu.audiometer"))
                            .on_hover_text(t!("menu.audiometer_hint"));
                        ui.separator();
                        if ui
                            .button(keymap.menu_label(
                                &t!("menu.fullscreen_player"),
                                Action::FullscreenViewer,
                            ))
                            .clicked()
                        {
                            set_fullscreen = Some(true);
                            ui.close();
                        }
                    })
                    .response,
                );

                bar_menus.push(
                    ui.menu_button(t!("menu.help"), |ui| {
                        if ui.button(t!("menu.about")).clicked() {
                            self.about_open = true;
                            ui.close();
                        }
                    })
                    .response,
                );
                switch_bar_menu_on_hover(ui.ctx(), &bar_menus);
            });
        });

        if let Some(true) = set_fullscreen.take() {
            self.viewer_fullscreen = true;
            ui.ctx()
                .send_viewport_cmd(egui::ViewportCommand::Fullscreen(true));
        }
    }

    pub(crate) fn show_about_dialog(&mut self, ctx: &egui::Context) {
        let icon = self.about_icon(ctx);
        egui::Window::new(t!("about.title"))
            .open(&mut self.about_open)
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
            .show(ctx, |ui| {
                ui.vertical_centered(|ui| {
                    if let Some(texture) = &icon {
                        ui.add(egui::Image::new(texture).fit_to_exact_size(egui::vec2(96.0, 96.0)));
                    }
                    ui.heading("Venturi video editor");
                    ui.label(t!("about.version", version = env!("VV_VERSION")));
                    ui.add_space(8.0);
                    ui.label(t!(
                        "about.author",
                        author = "Moreno Razzoli a.k.a. Morrolinux"
                    ));
                });
            });
    }

    /// App icon texture, loaded the first time the about box is opened.
    fn about_icon(&mut self, ctx: &egui::Context) -> Option<egui::TextureHandle> {
        if self.about_icon.is_none() {
            let icon = app_icon()?;
            let image = egui::ColorImage::from_rgba_unmultiplied(
                [icon.width as usize, icon.height as usize],
                &icon.rgba,
            );
            self.about_icon =
                Some(ctx.load_texture("app_icon", image, egui::TextureOptions::LINEAR));
        }
        self.about_icon.clone()
    }

    fn recent_projects_menu(&mut self, ui: &mut egui::Ui) {
        let recent = self.settings.recent_projects.clone();
        let mut chosen = None;
        ui.add_enabled_ui(!recent.is_empty(), |ui| {
            ui.menu_button(t!("menu.recent_projects"), |ui| {
                for path in &recent {
                    let label = path
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_else(|| path.display().to_string());
                    if ui
                        .button(label)
                        .on_hover_text(path.display().to_string())
                        .clicked()
                    {
                        chosen = Some(path.clone());
                    }
                }
            });
        });
        if let Some(path) = chosen {
            self.request_project_switch(ProjectSwitch::OpenRecent(path));
            ui.close();
        }
    }

    /// As in Blender: newest first, the dot on the current state and one
    /// click to jump to any point, forwards or backwards.
    fn undo_history_menu(&mut self, ui: &mut egui::Ui) {
        let labels: Vec<vv_core::CommandLabel> = self.session.history.labels().collect();
        let current = self.session.history.position();
        let mut jump = None;
        ui.add_enabled_ui(!labels.is_empty(), |ui| {
            ui.menu_button(t!("menu.undo_history"), |ui| {
                egui::ScrollArea::vertical()
                    .max_height(400.0)
                    .show(ui, |ui| {
                        for position in (0..=labels.len()).rev() {
                            let text = match position {
                                0 => t!("history.original"),
                                _ => command_label(labels[position - 1]),
                            };
                            if ui.radio(position == current, text).clicked() {
                                jump = Some(position);
                            }
                        }
                    });
            });
        });
        if let Some(position) = jump {
            self.go_to_history(position);
            ui.close();
        }
    }
}

fn command_label(label: vv_core::CommandLabel) -> std::borrow::Cow<'static, str> {
    use vv_core::CommandLabel as L;
    match label {
        L::AddTrack => t!("history.add_track"),
        L::RemoveTrack => t!("history.remove_track"),
        L::MuteTrack => t!("history.mute_track"),
        L::SoloTrack => t!("history.solo_track"),
        L::LockTrack => t!("history.lock_track"),
        L::ToggleClipsDisabled => t!("history.toggle_clips_disabled"),
        L::InsertClips => t!("history.insert_clips"),
        L::PasteClips => t!("history.paste_clips"),
        L::DuplicateClips => t!("history.duplicate_clips"),
        L::DeleteClips => t!("history.delete_clips"),
        L::RippleDelete => t!("history.ripple_delete"),
        L::MoveClips => t!("history.move_clips"),
        L::TrimClips => t!("history.trim_clips"),
        L::SlipClips => t!("history.slip_clips"),
        L::UnlinkClips => t!("history.unlink_clips"),
        L::LinkClips => t!("history.link_clips"),
        L::SplitClips => t!("history.split_clips"),
        L::Fade => t!("history.fade"),
        L::Transform => t!("history.transform"),
        L::Flip => t!("history.flip"),
        L::Gain => t!("history.gain"),
        L::ResetGain => t!("history.reset_gain"),
        L::ArmTrack => t!("history.arm_track"),
        L::RecordVoiceover => t!("history.record_voiceover"),
        L::MixerGain => t!("history.mixer_gain"),
        L::MixerPan => t!("history.mixer_pan"),
        L::AddAudioEffect => t!("history.add_audio_effect"),
        L::RemoveAudioEffect => t!("history.remove_audio_effect"),
        L::MoveAudioEffect => t!("history.move_audio_effect"),
        L::EditAudioEffect => t!("history.edit_audio_effect"),
        L::ToggleAudioEffect => t!("history.toggle_audio_effect"),
        L::Title => t!("history.title"),
        L::ResetTransform => t!("history.reset_transform"),
        L::ClipColor => t!("history.clip_color"),
        L::ClipDisplayColor => t!("history.clip_display_color"),
        L::SetKeyframe => t!("history.set_keyframe"),
        L::RemoveKeyframe => t!("history.remove_keyframe"),
        L::RemoveMedia => t!("history.remove_media"),
        L::RelinkMedia => t!("history.relink_media"),
        L::Filters => t!("history.filters"),
        L::Masks => t!("history.masks"),
        L::BlendMode => t!("history.blend_mode"),
        L::Transition => t!("history.transition"),
        L::MakeCompoundClip => t!("history.make_compound_clip"),
        L::MoveKeyframes => t!("history.move_keyframes"),
        L::SetInterpolation => t!("history.set_interpolation"),
        L::PasteAttributes => t!("history.paste_attributes"),
        L::ClipSpeed => t!("history.clip_speed"),
        L::RemoveSilences => t!("history.remove_silences"),
        L::AddMarker => t!("history.add_marker"),
        L::EditMarker => t!("history.edit_marker"),
        L::MoveMarker => t!("history.move_marker"),
        L::DeleteMarker => t!("history.delete_marker"),
        L::ImportMedia => t!("history.import_media"),
        L::ImportOtio => t!("history.import_otio"),
        L::NewTimeline => t!("history.new_timeline"),
        L::DuplicateTimeline => t!("history.duplicate_timeline"),
        L::RenameTimeline => t!("history.rename_timeline"),
        L::NewFolder => t!("history.new_folder"),
        L::RenameFolder => t!("history.rename_folder"),
        L::MoveToFolder => t!("history.move_to_folder"),
        L::DeleteFolder => t!("history.delete_folder"),
    }
}

/// egui's `MenuBar` only opens menus on click; once one is open, hovering
/// another bar entry should switch to it like in native menu bars.
fn switch_bar_menu_on_hover(ctx: &egui::Context, bar_menus: &[egui::Response]) {
    let popup_id = egui::Popup::default_response_id;
    if !bar_menus
        .iter()
        .any(|r| egui::Popup::is_id_open(ctx, popup_id(r)))
    {
        return;
    }
    if let Some(hovered) = bar_menus
        .iter()
        .find(|r| r.hovered() && !egui::Popup::is_id_open(ctx, popup_id(r)))
    {
        egui::Popup::open_id(ctx, popup_id(hovered));
        ctx.request_repaint();
    }
}
