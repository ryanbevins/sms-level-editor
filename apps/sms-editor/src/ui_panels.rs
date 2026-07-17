use super::*;

impl SmsEditorApp {
    pub(super) fn toolbar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing = egui::vec2(8.0, 6.0);

            ui.menu_button("File", |ui| {
                if ui
                    .button("Open a SMSProject")
                    .on_hover_text("Return to the recent-project hub")
                    .clicked()
                {
                    ui.close();
                    self.request_project_hub();
                }
                ui.separator();
                if ui
                    .add_enabled(
                        self.background_receiver.is_none(),
                        egui::Button::new("Schema"),
                    )
                    .clicked()
                {
                    ui.close();
                    self.generate_schema();
                }
                if ui
                    .add_enabled(self.document.is_some(), egui::Button::new("Validate"))
                    .clicked()
                {
                    ui.close();
                    self.validate();
                }
                if ui
                    .add_enabled(self.document.is_some(), egui::Button::new("Save Project"))
                    .clicked()
                {
                    ui.close();
                    self.save_project();
                }
                let export_enabled = self.document.is_some()
                    && !self.stage_export_path.trim().is_empty()
                    && self.background_receiver.is_none();
                if ui
                    .add_enabled(export_enabled, egui::Button::new("Export Stage"))
                    .on_hover_text(
                        "Create a new rebuilt stage archive at the explicit external path",
                    )
                    .clicked()
                {
                    ui.close();
                    self.export_stage_archive();
                }
                ui.separator();
                if ui.button("Launch").clicked() {
                    ui.close();
                    self.launch_dolphin();
                }
            });
            ui.menu_button("Edit", |ui| {
                if ui.button("Project Settings...").clicked() {
                    ui.close();
                    self.show_project_settings = true;
                }
                ui.separator();
                ui.checkbox(&mut self.show_stats, "Show Stats");
                if ui
                    .checkbox(&mut self.show_console, "Show Console")
                    .changed()
                {
                    if self.show_console {
                        self.bottom_tab = BottomTab::Console;
                    } else if self.bottom_tab == BottomTab::Console {
                        self.bottom_tab = BottomTab::Content;
                    }
                }
            });

            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let (warnings, errors) = self.issue_counts();
                let color = if errors > 0 {
                    egui::Color32::from_rgb(255, 116, 104)
                } else if warnings > 0 {
                    egui::Color32::from_rgb(235, 190, 92)
                } else {
                    egui::Color32::from_rgb(111, 220, 168)
                };
                let issue_button = egui::Button::new(
                    egui::RichText::new(format!("{warnings} warnings  {errors} errors"))
                        .color(color)
                        .strong(),
                )
                .fill(egui::Color32::from_rgb(37, 42, 43))
                .stroke(egui::Stroke::new(1.4, color));
                if ui
                    .add(issue_button)
                    .on_hover_text("Open validation issues")
                    .clicked()
                {
                    self.show_issues = true;
                }
                if self.is_dirty() {
                    ui.colored_label(egui::Color32::from_rgb(245, 190, 90), "Unsaved changes");
                }
                if let Some(label) = &self.background_label {
                    ui.label(label);
                    ui.spinner();
                }
            });
        });
    }

    pub(super) fn viewport_toolbar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing = egui::vec2(5.0, 4.0);
            for tool in [
                EditorTool::Select,
                EditorTool::Move,
                EditorTool::Rotate,
                EditorTool::Scale,
            ] {
                if ui
                    .selectable_label(self.tool == tool, tool.label())
                    .on_hover_text(format!("{} tool ({})", tool.label(), tool_shortcut(tool)))
                    .clicked()
                {
                    self.tool = tool;
                }
            }

            ui.separator();
            if ui
                .selectable_label(
                    self.snap_enabled,
                    if self.snap_enabled {
                        "Snapping On"
                    } else {
                        "Snapping Off"
                    },
                )
                .on_hover_text("Enable or disable transform snapping")
                .clicked()
            {
                self.snap_enabled = !self.snap_enabled;
            }
            ui.add_enabled_ui(self.snap_enabled, |ui| {
                ui.add(
                    egui::DragValue::new(&mut self.snap_translation)
                        .range(0.01..=100_000.0)
                        .speed(5.0)
                        .prefix("Move "),
                )
                .on_hover_text("Translation snap interval");
                ui.add(
                    egui::DragValue::new(&mut self.snap_rotation)
                        .range(0.01..=360.0)
                        .speed(1.0)
                        .prefix("Rotate ")
                        .suffix("°"),
                )
                .on_hover_text("Rotation snap interval");
                ui.add(
                    egui::DragValue::new(&mut self.snap_scale)
                        .range(0.001..=100.0)
                        .speed(0.01)
                        .prefix("Scale "),
                )
                .on_hover_text("Scale snap interval");
            });

            ui.separator();
            for mode in [ViewMode::Lit, ViewMode::Collision, ViewMode::Objects] {
                if ui
                    .selectable_label(self.view_mode == mode, mode.label())
                    .clicked()
                {
                    self.view_mode = mode;
                }
            }

            if self
                .model_preview
                .as_ref()
                .is_some_and(ModelPreview::has_level_transformation)
            {
                ui.separator();
                let label = if self.level_transform_playing {
                    "Pause Level Change"
                } else {
                    "Play Level Change"
                };
                if ui
                    .button(label)
                    .on_hover_text(
                        "Preview the asset-driven level change at Sunshine's animation rate",
                    )
                    .clicked()
                {
                    if self.level_transform_playing {
                        self.level_transform_playing = false;
                    } else {
                        if self.level_transform_progress >= 1.0 {
                            self.level_transform_progress = 0.0;
                        }
                        self.level_transform_playback_origin = self.level_transform_progress;
                        self.level_transform_started_at = Instant::now();
                        self.level_transform_playing = true;
                    }
                }
                if ui
                    .button("Reset Level Change")
                    .on_hover_text("Return the map and linked pollution to the retail start state")
                    .clicked()
                {
                    self.level_transform_playing = false;
                    self.level_transform_progress = 0.0;
                }
            }
        });
    }

    pub(super) fn right_dock(&mut self, ui: &mut egui::Ui) {
        let available_height = ui.available_height();
        let outliner_height = (available_height * 0.44)
            .max(150.0)
            .min((available_height - 170.0).max(150.0));
        ui.allocate_ui_with_layout(
            egui::vec2(ui.available_width(), outliner_height),
            egui::Layout::top_down(egui::Align::Min),
            |ui| self.outliner_panel(ui),
        );
        ui.separator();
        egui::ScrollArea::vertical()
            .id_salt("inspector-scroll")
            .show(ui, |ui| self.inspector_panel(ui));
    }

    pub(super) fn bottom_dock(&mut self, ui: &mut egui::Ui) {
        if !self.show_console && self.bottom_tab == BottomTab::Console {
            self.bottom_tab = BottomTab::Content;
        }
        ui.horizontal(|ui| {
            ui.selectable_value(&mut self.bottom_tab, BottomTab::Content, "Content Browser");
            ui.selectable_value(&mut self.bottom_tab, BottomTab::Palette, "Object Palette");
            if self.show_console {
                ui.selectable_value(&mut self.bottom_tab, BottomTab::Console, "Console");
            }
        });
        ui.separator();

        match self.bottom_tab {
            BottomTab::Content => self.content_browser_panel(ui),
            BottomTab::Palette => self.palette_panel(ui),
            BottomTab::Console => self.console(ui),
        }
    }

    pub(super) fn project_settings_window(&mut self, ctx: &egui::Context) {
        if !self.show_project_settings {
            return;
        }
        let mut open = self.show_project_settings;
        egui::Window::new("Project Settings")
            .open(&mut open)
            .default_width(580.0)
            .default_height(680.0)
            .resizable(true)
            .show(ctx, |ui| {
                egui::ScrollArea::vertical().show(ui, |ui| self.project_panel(ui));
            });
        self.show_project_settings = open;
    }

    pub(super) fn issues_window(&mut self, ctx: &egui::Context) {
        if !self.show_issues {
            return;
        }
        let mut open = self.show_issues;
        egui::Window::new("Validation Issues")
            .open(&mut open)
            .default_width(680.0)
            .default_height(440.0)
            .resizable(true)
            .show(ctx, |ui| self.issues_panel(ui));
        self.show_issues = open;
    }

    pub(super) fn project_panel(&mut self, ui: &mut egui::Ui) {
        ui.heading("Project Settings");
        ui.add_space(4.0);
        let mut save_name = false;
        if let Some(project) = &self.current_project {
            ui.horizontal(|ui| {
                ui.label("Name");
                ui.text_edit_singleline(&mut self.project_name_draft);
                if ui
                    .add_enabled(
                        !self.project_name_draft.trim().is_empty()
                            && self.project_name_draft != project.descriptor.name,
                        egui::Button::new("Save Name"),
                    )
                    .clicked()
                {
                    save_name = true;
                }
            });
            ui.small(format!(
                "Project file: {}",
                project.descriptor_path.display()
            ));
        } else {
            ui.colored_label(
                egui::Color32::from_rgb(245, 190, 90),
                "Temporary session — create a .sms project to make it appear on the launch hub.",
            );
        }
        if save_name {
            self.persist_project_settings(true);
        }

        ui.add_space(8.0);
        let choose_repo = path_display_row(ui, "Schema Source", &self.repo_root, "Browse...", true);
        let choose_base = path_display_row(
            ui,
            "Extracted Game",
            &self.base_root,
            "Browse...",
            self.document.is_none() && self.background_receiver.is_none(),
        );
        path_display_row(ui, "Project Data", &self.project_root, "Managed", false);
        ui.small(
            "The .sms file defines this project. Managed edits stay outside the extracted game directory.",
        );
        if choose_repo {
            self.choose_schema_source_root();
        }
        if choose_base {
            self.choose_base_game_root();
        }
        if let Some(document) = &self.document {
            if !document_uses_selected_base(document, self.base_root.trim()) {
                ui.colored_label(
                    egui::Color32::from_rgb(255, 150, 104),
                    format!(
                        "The open stage is still from '{}'. Open a stage from the selected Base Game Root before saving.",
                        document.base_root.display()
                    ),
                );
            }
        }
        let choose_export = path_display_row(
            ui,
            "Stage Export",
            &self.stage_export_path,
            "Choose...",
            self.document.is_some(),
        );
        if choose_export {
            self.choose_stage_export_path();
        }
        if !self.stage_export_path.is_empty() && ui.small_button("Clear export path").clicked() {
            self.stage_export_path.clear();
        }
        labeled_text(ui, "Stage", &mut self.stage_id);

        ui.separator();
        ui.heading("Viewport");
        {
            let config = self.renderer.config_mut();
            ui.checkbox(&mut config.show_grid, "Grid");
            ui.checkbox(&mut config.show_collision, "Collision");
            ui.checkbox(&mut config.show_object_bounds, "Object bounds");
        }
        let environment_changed = ui
            .checkbox(&mut self.show_environment_meshes, "Water")
            .changed();
        let goop_changed = ui.checkbox(&mut self.show_goop_meshes, "Goop").changed();
        let effects_changed = ui
            .checkbox(&mut self.show_effect_meshes, "Effects")
            .changed();
        if environment_changed || goop_changed || effects_changed {
            let model_preview = self.document.as_ref().and_then(|document| {
                SmsEditorApp::build_model_preview(document, self.preview_visibility())
            });
            self.model_preview = model_preview;
            self.last_level_transform_progress_bits = u32::MAX;
            self.rebuild_gpu_viewport_scene();
            self.clear_viewport_preview_cache();
            self.reset_camera();
        }
        ui.add(egui::Slider::new(&mut self.viewport_zoom, 0.35..=2.5).text("Zoom"));
        ui.add(egui::Slider::new(&mut self.camera_speed, 0.01..=8.0).text("Speed"))
            .on_hover_text("Hold right mouse and use the mouse wheel to adjust fly speed");
        if self
            .model_preview
            .as_ref()
            .is_some_and(ModelPreview::has_level_transformation)
        {
            ui.separator();
            ui.label("Level transformation");
            if let Some(duration_frames) = self
                .model_preview
                .as_ref()
                .map(|preview| preview.level_transform_duration_frames)
            {
                ui.small(format!(
                    "Asset timeline: {:.1}s at {:.0} FPS",
                    level_transform_duration_seconds(duration_frames),
                    SMS_ANIMATION_FRAMES_PER_SECOND
                ));
            }
            ui.horizontal(|ui| {
                let play_label = if self.level_transform_playing {
                    "Pause"
                } else {
                    "Play"
                };
                if ui
                    .button(play_label)
                    .on_hover_text("Preview the retail map-joint transformation")
                    .clicked()
                {
                    if self.level_transform_playing {
                        self.level_transform_playing = false;
                    } else {
                        if self.level_transform_progress >= 1.0 {
                            self.level_transform_progress = 0.0;
                        }
                        self.level_transform_playback_origin = self.level_transform_progress;
                        self.level_transform_started_at = Instant::now();
                        self.level_transform_playing = true;
                    }
                }
                if ui.button("Reset").clicked() {
                    self.level_transform_playing = false;
                    self.level_transform_progress = 0.0;
                }
                if ui.button("End").clicked() {
                    self.level_transform_playing = false;
                    self.level_transform_progress = 1.0;
                }
            });
            if ui
                .add(
                    egui::Slider::new(&mut self.level_transform_progress, 0.0..=1.0)
                        .show_value(false),
                )
                .on_hover_text("Scrub from the retail starting state to the recovered state")
                .changed()
            {
                self.level_transform_playing = false;
            }
        }
        if ui.button("Frame Selection").clicked() {
            self.frame_selected();
        }
        if ui.button("Reset Camera").clicked() {
            self.reset_camera();
        }

        ui.separator();
        ui.heading("Dolphin");
        let choose_dolphin =
            path_display_row(ui, "Executable", &self.dolphin_path, "Browse...", true);
        let choose_game = path_display_row(ui, "Game", &self.game_path, "Browse...", true);
        let choose_user_dir =
            path_display_row(ui, "User Dir", &self.dolphin_user_dir, "Browse...", true);
        if choose_dolphin {
            self.choose_dolphin_executable();
        }
        if choose_game {
            self.choose_game_image();
        }
        if choose_user_dir {
            self.choose_dolphin_user_directory();
        }
        let mut launch_settings_changed = false;
        ui.horizontal_wrapped(|ui| {
            if !self.dolphin_path.is_empty() && ui.small_button("Clear executable").clicked() {
                self.dolphin_path.clear();
                launch_settings_changed = true;
            }
            if !self.game_path.is_empty() && ui.small_button("Clear game").clicked() {
                self.game_path.clear();
                launch_settings_changed = true;
            }
            if !self.dolphin_user_dir.is_empty() && ui.small_button("Clear user dir").clicked() {
                self.dolphin_user_dir.clear();
                launch_settings_changed = true;
            }
        });
        if launch_settings_changed {
            self.persist_project_settings(false);
        }

        ui.separator();
        if let Some(registry) = &self.registry {
            ui.label(format!("{} object schema entries", registry.objects.len()));
            ui.label(format!("{} parameter hints", registry.params.len()));
            ui.label(format!("{} asset hints", registry.asset_hints.len()));
            ui.label(format!(
                "{} NPC initialization definitions",
                registry.npc_actors.len()
            ));
        } else {
            ui.label("Schema not generated.");
        }
    }

    pub(super) fn content_browser_panel(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.heading("Content Browser");
            ui.add_space(8.0);
            ui.label("Search");
            ui.add(
                egui::TextEdit::singleline(&mut self.scene_filter)
                    .desired_width(240.0)
                    .hint_text("Stage or archive path"),
            );
        });
        ui.small(format!(
            "{} stage{} from this project's extracted game  |  current: {}",
            self.scene_archives.len(),
            if self.scene_archives.len() == 1 {
                ""
            } else {
                "s"
            },
            if self.stage_id.trim().is_empty() {
                "none"
            } else {
                self.stage_id.as_str()
            }
        ));
        ui.separator();

        let filter = self.scene_filter.to_ascii_lowercase();
        let archives: Vec<SceneArchiveInfo> = self
            .scene_archives
            .iter()
            .filter(|archive| {
                filter.is_empty()
                    || archive.stage_id.to_ascii_lowercase().contains(&filter)
                    || archive.group.to_ascii_lowercase().contains(&filter)
                    || archive
                        .relative_path
                        .to_string_lossy()
                        .to_ascii_lowercase()
                        .contains(&filter)
            })
            .cloned()
            .collect();

        let mut open_archive = None;
        egui::ScrollArea::vertical().show(ui, |ui| {
            let mut start = 0;
            while start < archives.len() {
                let group = archives[start].group.clone();
                let end = archives[start..]
                    .iter()
                    .position(|archive| archive.group != group)
                    .map_or(archives.len(), |offset| start + offset);
                let layout = content_browser_layout(ui.available_width(), end - start);
                ui.add_space(5.0);
                ui.label(
                    egui::RichText::new(if group.is_empty() {
                        "Ungrouped"
                    } else {
                        group.as_str()
                    })
                    .strong()
                    .color(egui::Color32::from_rgb(159, 208, 201)),
                );
                ui.add_space(3.0);
                egui::Grid::new(("content-browser-grid", group.as_str()))
                    .num_columns(layout.columns)
                    .min_col_width(layout.card_width)
                    .max_col_width(layout.card_width)
                    .spacing(egui::vec2(8.0, 8.0))
                    .show(ui, |ui| {
                        for (index, archive) in archives[start..end].iter().enumerate() {
                            let selected = self.stage_id.eq_ignore_ascii_case(&archive.stage_id);
                            let label = format!(
                                "{}\n{}",
                                archive.stage_id,
                                format_bytes_short(archive.size_bytes)
                            );
                            let response = ui
                                .add_sized(
                                    [layout.card_width, 52.0],
                                    egui::Button::selectable(selected, label),
                                )
                                .on_hover_text(format!(
                                    "{}\n{}",
                                    archive.relative_path.display(),
                                    archive.path.display()
                                ));
                            if response.clicked() {
                                open_archive = Some(archive.clone());
                            }
                            if (index + 1) % layout.columns == 0 {
                                ui.end_row();
                            }
                        }
                    });
                start = end;
            }

            if archives.is_empty() {
                if self.background_receiver.is_some() {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label("Discovering stages from the project game root...");
                    });
                } else {
                    ui.label("No stages match the current search.");
                }
            }
        });

        if let Some(archive) = open_archive {
            self.open_scene_archive(archive);
        }
    }

    pub(super) fn palette_panel(&mut self, ui: &mut egui::Ui) {
        ui.heading("Object Palette");
        ui.text_edit_singleline(&mut self.object_filter);
        ui.add_space(4.0);

        let mut chosen: Option<String> = None;
        let mut spawn_now: Option<String> = None;
        let filter = self.object_filter.to_ascii_lowercase();
        let entries: Vec<ObjectDefinition> = self
            .registry
            .as_ref()
            .map(|registry| {
                registry
                    .objects
                    .iter()
                    .filter(|object| !object.hidden)
                    .filter(|object| {
                        filter.is_empty()
                            || object.factory_name.to_ascii_lowercase().contains(&filter)
                            || object.class_name.to_ascii_lowercase().contains(&filter)
                            || object.category.to_ascii_lowercase().contains(&filter)
                    })
                    .take(160)
                    .cloned()
                    .collect()
            })
            .unwrap_or_default();

        egui::ScrollArea::vertical().show(ui, |ui| {
            for object in entries {
                ui.horizontal(|ui| {
                    let selected = self.palette_factory.as_deref() == Some(&object.factory_name);
                    if ui
                        .selectable_label(selected, &object.factory_name)
                        .on_hover_text(format!("{} / {}", object.category, object.class_name))
                        .clicked()
                    {
                        chosen = Some(object.factory_name.clone());
                    }
                    if ui
                        .add_enabled(self.document.is_some(), egui::Button::new("Add"))
                        .clicked()
                    {
                        spawn_now = Some(object.factory_name.clone());
                    }
                });
                ui.small(format!("{}  {}", object.category, object.class_name));
                ui.separator();
            }
        });

        if let Some(factory) = chosen {
            self.palette_factory = Some(factory);
            self.tool = EditorTool::Place;
        }
        if let Some(factory) = spawn_now {
            self.spawn_object_at(factory, self.default_spawn_position());
        }
    }

    pub(super) fn outliner_panel(&mut self, ui: &mut egui::Ui) {
        ui.heading("Hierarchy");
        ui.horizontal(|ui| {
            if ui
                .add_enabled(
                    self.selected_object_id.is_some(),
                    egui::Button::new("Duplicate"),
                )
                .clicked()
            {
                self.duplicate_selected();
            }
            if ui
                .add_enabled(
                    self.selected_object_id.is_some(),
                    egui::Button::new("Delete"),
                )
                .clicked()
            {
                self.delete_selected();
            }
        });
        ui.separator();

        let selected_id = self.selected_object_id.as_deref();
        let mut clicked_id = None;
        egui::ScrollArea::vertical().show(ui, |ui| {
            let Some(document) = &self.document else {
                return;
            };
            for object in &document.objects {
                let selected = selected_id == Some(object.id.as_str());
                if ui
                    .selectable_label(selected, &object.factory_name)
                    .clicked()
                {
                    clicked_id = Some(object.id.clone());
                }
                ui.small(format!(
                    "{}  {}",
                    object.id,
                    object.class_name.as_deref().unwrap_or("Unknown")
                ));
                ui.separator();
            }
        });
        if let Some(id) = clicked_id {
            self.selected_object_id = Some(id);
        }
    }

    pub(super) fn inspector_panel(&mut self, ui: &mut egui::Ui) {
        let selected = self.selected_object().cloned();
        if let Some(object) = selected {
            ui.heading(&object.factory_name);
            ui.label(format!("Id: {}", object.id));
            ui.label(format!(
                "Class: {}",
                object.class_name.as_deref().unwrap_or("Unknown")
            ));
            ui.separator();

            let mut transform = object.transform;
            let mut edit = VectorDragResponse::default();

            ui.label("Translation");
            edit.merge(vector_drag(ui, &mut transform.translation, 1.0));
            ui.label("Rotation");
            edit.merge(vector_drag(ui, &mut transform.rotation_degrees, 0.5));
            ui.label("Scale");
            edit.merge(vector_drag(ui, &mut transform.scale, 0.01));

            if edit.started {
                self.begin_undo_transaction();
            }
            if edit.changed {
                self.update_selected_transform(transform);
            }
            if edit.stopped {
                self.commit_undo_transaction("Updated transform");
            }

            ui.separator();
            ui.horizontal(|ui| {
                if ui.button("Frame").clicked() {
                    self.frame_selected();
                }
                if ui.button("Duplicate").clicked() {
                    self.duplicate_selected();
                }
                if ui.button("Delete").clicked() {
                    self.delete_selected();
                }
            });

            ui.separator();
            ui.heading("Params");
            if object.raw_params.is_empty() {
                ui.label("No decoded params yet.");
            } else {
                for (key, parameter) in object.raw_params {
                    if let Some(decoded) = parameter.decoded() {
                        ui.label(format!("{key}: {parameter} ({decoded:?})"));
                    } else {
                        ui.label(format!("{key}: {parameter}"));
                    }
                }
            }
        } else {
            ui.heading("Inspector");
            if self.document.is_some() {
                ui.label("No object selected.");
            } else {
                ui.label("No stage open.");
            }
        }
    }

    pub(super) fn issues_panel(&mut self, ui: &mut egui::Ui) {
        ui.heading("Validation");
        if self.issues.is_empty() {
            ui.colored_label(egui::Color32::from_rgb(111, 220, 168), "Clean");
            return;
        }

        egui::ScrollArea::vertical().show(ui, |ui| {
            for issue in &self.issues {
                let color = match issue.severity {
                    ValidationSeverity::Info => egui::Color32::from_rgb(150, 180, 220),
                    ValidationSeverity::Warning => egui::Color32::from_rgb(235, 190, 92),
                    ValidationSeverity::Error => egui::Color32::from_rgb(255, 116, 104),
                };
                ui.colored_label(color, format!("{:?} [{}]", issue.severity, issue.code));
                ui.label(&issue.message);
                ui.separator();
            }
        });
    }

    pub(super) fn console(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.heading("Console");
            if ui.button("Clear").clicked() {
                self.log.clear();
            }
        });
        egui::ScrollArea::vertical()
            .stick_to_bottom(true)
            .show(ui, |ui| {
                for line in &self.log {
                    ui.label(line);
                }
            });
    }
}

fn tool_shortcut(tool: EditorTool) -> &'static str {
    match tool {
        EditorTool::Select => "Q",
        EditorTool::Move => "W",
        EditorTool::Rotate => "E",
        EditorTool::Scale => "R",
        EditorTool::Place => "Object Palette",
    }
}
