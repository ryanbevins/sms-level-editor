use super::*;

impl SmsEditorApp {
    pub(super) fn toolbar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal_wrapped(|ui| {
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
            if let Some(project) = &self.current_project {
                ui.label(
                    egui::RichText::new(&project.descriptor.name)
                        .strong()
                        .color(egui::Color32::from_rgb(159, 208, 201)),
                );
            }
            ui.separator();
            for tool in [
                EditorTool::Select,
                EditorTool::Move,
                EditorTool::Rotate,
                EditorTool::Scale,
                EditorTool::Place,
            ] {
                if ui
                    .selectable_label(self.tool == tool, tool.label())
                    .on_hover_text(format!("{} tool", tool.label()))
                    .clicked()
                {
                    self.tool = tool;
                }
            }

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

            ui.separator();
            if ui
                .add_enabled(self.can_undo(), egui::Button::new("Undo"))
                .clicked()
            {
                self.undo();
            }
            if ui
                .add_enabled(self.can_redo(), egui::Button::new("Redo"))
                .clicked()
            {
                self.redo();
            }

            ui.separator();
            if let Some(label) = &self.background_label {
                ui.spinner();
                ui.label(label);
                ui.separator();
            }
            if self.is_dirty() {
                ui.colored_label(egui::Color32::from_rgb(245, 190, 90), "Unsaved changes");
            }
            ui.separator();
            let (warnings, errors) = self.issue_counts();
            ui.colored_label(
                if errors > 0 {
                    egui::Color32::from_rgb(255, 116, 104)
                } else if warnings > 0 {
                    egui::Color32::from_rgb(235, 190, 92)
                } else {
                    egui::Color32::from_rgb(111, 220, 168)
                },
                format!("{} warnings  {} errors", warnings, errors),
            );
        });
    }

    pub(super) fn left_dock(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.selectable_value(&mut self.left_tab, LeftTab::Project, "Project");
            ui.selectable_value(&mut self.left_tab, LeftTab::Content, "Content");
            ui.selectable_value(&mut self.left_tab, LeftTab::Palette, "Palette");
            ui.selectable_value(&mut self.left_tab, LeftTab::Outliner, "Outliner");
        });
        ui.separator();

        match self.left_tab {
            LeftTab::Project => self.project_panel(ui),
            LeftTab::Content => self.content_browser_panel(ui),
            LeftTab::Palette => self.palette_panel(ui),
            LeftTab::Outliner => self.outliner_panel(ui),
        }
    }

    pub(super) fn right_dock(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.selectable_value(&mut self.right_tab, RightTab::Inspector, "Inspector");
            ui.selectable_value(&mut self.right_tab, RightTab::Assets, "Assets");
            ui.selectable_value(&mut self.right_tab, RightTab::Issues, "Issues");
        });
        ui.separator();

        match self.right_tab {
            RightTab::Inspector => self.inspector_panel(ui),
            RightTab::Assets => self.assets_panel(ui),
            RightTab::Issues => self.issues_panel(ui),
        }
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
        ui.heading("Snap");
        ui.checkbox(&mut self.snap_enabled, "Enabled");
        ui.add(
            egui::DragValue::new(&mut self.snap_translation)
                .speed(5.0)
                .prefix("Move "),
        );
        ui.add(
            egui::DragValue::new(&mut self.snap_rotation)
                .speed(1.0)
                .prefix("Rotate "),
        );
        ui.add(
            egui::DragValue::new(&mut self.snap_scale)
                .speed(0.01)
                .prefix("Scale "),
        );

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
        ui.heading("Content Browser");
        ui.add_space(4.0);
        if path_display_row(
            ui,
            "Extracted Game",
            &self.base_root,
            "Browse...",
            self.document.is_none() && self.background_receiver.is_none(),
        ) {
            self.choose_base_game_root();
        }

        ui.horizontal(|ui| {
            let can_scan = PathBuf::from(self.base_root.trim()).exists();
            if command_button(ui, "Scan", can_scan).clicked() {
                self.scan_scenes();
            }
            if command_button(ui, "Open", !self.stage_id.trim().is_empty()).clicked() {
                self.request_open_stage(self.stage_id.clone());
            }
        });

        ui.separator();
        ui.horizontal(|ui| {
            ui.label("Scenes");
            ui.text_edit_singleline(&mut self.scene_filter);
        });
        ui.small(format!(
            "{} scene archive(s)  current: {}",
            self.scene_archives.len(),
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
        let mut current_group = String::new();
        egui::ScrollArea::vertical().show(ui, |ui| {
            for archive in archives {
                if archive.group != current_group {
                    current_group = archive.group.clone();
                    ui.add_space(4.0);
                    ui.label(
                        egui::RichText::new(if current_group.is_empty() {
                            "Ungrouped"
                        } else {
                            current_group.as_str()
                        })
                        .strong()
                        .color(egui::Color32::from_rgb(159, 208, 201)),
                    );
                }

                let selected = self.stage_id.eq_ignore_ascii_case(&archive.stage_id);
                let label = format!(
                    "{}    {}",
                    archive.stage_id,
                    format_bytes_short(archive.size_bytes)
                );
                let response = ui
                    .selectable_label(selected, label)
                    .on_hover_text(archive.path.display().to_string());
                ui.small(archive.relative_path.display().to_string());
                ui.separator();

                if response.clicked() {
                    open_archive = Some(archive);
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
            self.right_tab = RightTab::Inspector;
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
                if self.snap_enabled {
                    snap_transform(
                        &mut transform,
                        self.snap_translation,
                        self.snap_rotation,
                        self.snap_scale,
                    );
                }
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

    pub(super) fn assets_panel(&mut self, ui: &mut egui::Ui) {
        ui.heading("Assets");
        if let Some(document) = &self.document {
            let scene = self.render_scene.as_ref();
            ui.label(format!(
                "{} scanned assets  {} models  {} collision",
                document.assets.len(),
                scene.map_or(0, |scene| scene.model_paths.len()),
                scene.map_or(0, |scene| scene.collision_paths.len())
            ));
            if let Some(preview) = &self.model_preview {
                ui.label(format!(
                    "Preview: {} model(s), {} shown point(s), {} source vertex/vertices, {} failed",
                    preview.loaded_models,
                    preview.points.len(),
                    preview.source_vertices,
                    preview.failed_models
                ));
            }
            ui.separator();
            egui::ScrollArea::vertical().show(ui, |ui| {
                for asset in document.assets.iter().take(400) {
                    ui.horizontal(|ui| {
                        ui.monospace(format!("{:?}", asset.kind));
                        ui.label(asset.path.display().to_string());
                    });
                }
            });
        } else {
            ui.label("No stage open.");
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
