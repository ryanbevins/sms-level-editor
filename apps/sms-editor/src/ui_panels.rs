use sms_scene::{EditableSceneParameter, ObjectParameterKind};

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
                if ui
                    .add_enabled(
                        self.current_project.is_some()
                            && self.background_receiver.is_none(),
                        egui::Button::new("New Stage..."),
                    )
                    .on_hover_text(
                        "Create an empty source-free stage with a new project-owned runtime slot",
                    )
                    .clicked()
                {
                    ui.close();
                    self.request_new_stage();
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
                let build_enabled = self.document.is_some()
                    && self.current_project.is_some()
                    && self.background_receiver.is_none();
                if ui
                    .add_enabled(build_enabled, egui::Button::new("Build Game"))
                    .on_hover_text(
                        "Build a complete runnable game directory with the level and its separate authored BMD resources installed",
                    )
                    .clicked()
                {
                    ui.close();
                    self.build_game();
                }
                if ui
                    .add_enabled(
                        build_enabled && !self.dolphin_path.trim().is_empty(),
                        egui::Button::new("Launch in Dolphin"),
                    )
                    .on_hover_text(
                        "Save current changes, update the isolated runnable game mirror, and boot directly into the open scene in Dolphin",
                    )
                    .clicked()
                {
                    ui.close();
                    self.build_and_launch();
                }
                ui.separator();
                if ui
                    .button("Launch Configured Game (legacy)")
                    .on_hover_text("Launch the separately configured ISO, RVZ, GCM, WBFS, or DOL without deploying this project")
                    .clicked()
                {
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
                if let Some(label) = self.background_label.clone() {
                    ui.label(label);
                    if let Some(cancel_requested) = self
                        .active_build_cancel
                        .as_ref()
                        .map(|cancel| cancel.load(Ordering::Acquire))
                    {
                        if ui
                            .add_enabled(
                                !cancel_requested,
                                egui::Button::new("Cancel").small(),
                            )
                            .on_hover_text(if cancel_requested {
                                "Cancellation requested"
                            } else {
                                "Cancel this managed game build"
                            })
                            .clicked()
                        {
                            self.cancel_active_build();
                        }
                    }
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
                    if mode == ViewMode::Collision {
                        self.renderer.config_mut().show_collision = true;
                    }
                    self.clear_viewport_preview_cache();
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
        let managed_build_root = self
            .current_project
            .as_ref()
            .map(|project| project.managed_build_root().to_string_lossy().into_owned())
            .unwrap_or_else(|| "Create or open a .sms project".to_string());
        path_display_row(ui, "Managed Build", &managed_build_root, "Managed", false);
        ui.small(
            "Build Game creates a complete independent runnable game directory and writes the rebuilt level at its exact game-relative path. Authored models remain separate BMD resources inside the stage archive; the extracted base game is never changed.",
        );
        if let Some(document) = &self.document {
            ui.horizontal(|ui| {
                ui.label("Stage");
                ui.monospace(&document.stage_id);
            });
        } else {
            labeled_text(ui, "Stage", &mut self.stage_id);
        }

        ui.separator();
        ui.heading("Viewport");
        let collision_visibility_changed = {
            let config = self.renderer.config_mut();
            ui.checkbox(&mut config.show_grid, "Grid");
            let changed = ui
                .checkbox(&mut config.show_collision, "Collision")
                .on_hover_text("Show collision geometry in Collision view")
                .changed();
            ui.checkbox(&mut config.show_object_bounds, "Object bounds");
            changed
        };
        if collision_visibility_changed {
            self.clear_viewport_preview_cache();
        }
        let environment_changed = ui
            .checkbox(&mut self.show_environment_meshes, "Water")
            .changed();
        let goop_changed = ui.checkbox(&mut self.show_goop_meshes, "Goop").changed();
        let effects_changed = ui
            .checkbox(&mut self.show_effects, "Effects")
            .on_hover_text("Show particle systems and effect-only scene models")
            .changed();
        if environment_changed || goop_changed || effects_changed {
            self.rebuild_model_preview_from_document();
            if effects_changed {
                self.log.push(format!(
                    "Effect previews {}.",
                    if self.show_effects {
                        "enabled"
                    } else {
                        "hidden"
                    }
                ));
            }
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
        let choose_user_dir =
            path_display_row(ui, "User Dir", &self.dolphin_user_dir, "Browse...", true);
        ui.small(
            "Launch in Dolphin refreshes the managed runnable mirror and boots the open scene directly. Leave User Dir blank to use your normal Dolphin profile and controller configuration.",
        );
        ui.label("Legacy external game launch (optional)");
        let choose_game = path_display_row(ui, "Game", &self.game_path, "Browse...", true);
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
            ui.selectable_value(
                &mut self.content_browser_kind,
                ContentBrowserKind::Stages,
                "Stages",
            );
            ui.selectable_value(
                &mut self.content_browser_kind,
                ContentBrowserKind::Objects,
                "Objects",
            );
            ui.selectable_value(
                &mut self.content_browser_kind,
                ContentBrowserKind::Models,
                "Model Assets",
            );
            ui.selectable_value(
                &mut self.content_browser_kind,
                ContentBrowserKind::GameSkyboxes,
                "Game Skyboxes",
            );
        });
        ui.separator();
        match self.content_browser_kind {
            ContentBrowserKind::Stages => self.stage_content_browser_panel(ui),
            ContentBrowserKind::Objects => self.palette_panel(ui),
            ContentBrowserKind::Models => self.model_content_browser_panel(ui),
            ContentBrowserKind::GameSkyboxes => self.game_skybox_content_browser_panel(ui),
        }
    }

    fn game_skybox_content_browser_panel(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.heading("Game Skyboxes");
            ui.label("Search");
            ui.add(
                egui::TextEdit::singleline(&mut self.skybox_filter)
                    .desired_width(220.0)
                    .hint_text("Stage or localized name"),
            );
        });
        ui.small(
            "Indexed from the extracted game's stage archives. Applying one copies the complete typed sky.* bundle into the open scene and creates Sky when the stage does not have it yet.",
        );
        ui.separator();

        let filter = self.skybox_filter.to_ascii_lowercase();
        let entries = self
            .retail_skyboxes
            .iter()
            .filter(|entry| {
                let localized = self.scene_labels.get(&entry.stage_id.to_ascii_lowercase());
                filter.is_empty()
                    || entry.stage_id.to_ascii_lowercase().contains(&filter)
                    || localized.is_some_and(|label| {
                        label
                            .stage_name
                            .as_ref()
                            .is_some_and(|name| name.to_ascii_lowercase().contains(&filter))
                            || label
                                .scenario_names
                                .iter()
                                .any(|name| name.to_ascii_lowercase().contains(&filter))
                    })
            })
            .cloned()
            .collect::<Vec<_>>();
        let mut apply = None;
        egui::ScrollArea::vertical().show(ui, |ui| {
            let layout = content_browser_layout(ui.available_width(), entries.len());
            egui::Grid::new("game-skybox-content-browser-grid")
                .num_columns(layout.columns)
                .min_col_width(layout.card_width)
                .max_col_width(layout.card_width)
                .spacing(egui::vec2(8.0, 8.0))
                .show(ui, |ui| {
                    for (index, entry) in entries.iter().enumerate() {
                        let localized = self
                            .scene_labels
                            .get(&entry.stage_id.to_ascii_lowercase());
                        let mut lines = vec![entry.stage_id.clone()];
                        if let Some(name) = localized.and_then(|label| label.stage_name.as_deref()) {
                            lines.push(name.to_string());
                        }
                        lines.push(format!("{} sky.* resources", entry.resource_count));
                        let response = content_browser_card_button(
                            ui,
                            egui::vec2(layout.card_width, 88.0),
                            false,
                            &lines.join("\n"),
                        )
                        .on_hover_text(format!(
                            "Source: {}\nApply this complete retail skybox bundle to the open stage.",
                            entry.archive_path.display()
                        ));
                        if response.clicked() {
                            apply = Some(entry.clone());
                        }
                        if (index + 1) % layout.columns == 0 {
                            ui.end_row();
                        }
                    }
                });
            if entries.is_empty() {
                ui.label("No indexed retail skyboxes match this search.");
            }
        });
        if let Some(entry) = apply {
            self.request_retail_skybox(entry);
        }
    }

    fn stage_content_browser_panel(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.heading("Content Browser");
            ui.add_space(8.0);
            if ui
                .add_enabled(
                    self.current_project.is_some() && self.background_receiver.is_none(),
                    egui::Button::new("+ New Stage").small(),
                )
                .on_hover_text("Create an empty authored stage with a new runtime slot")
                .clicked()
            {
                self.request_new_stage();
            }
            ui.label("Search");
            ui.add(
                egui::TextEdit::singleline(&mut self.scene_filter)
                    .desired_width(240.0)
                    .hint_text("Stage or archive path"),
            );
        });
        ui.small(format!(
            "{} stage{} from this project's extracted game  |  {} localized label{}  |  current: {}",
            self.scene_archives.len(),
            if self.scene_archives.len() == 1 {
                ""
            } else {
                "s"
            },
            self.scene_labels.len(),
            if self.scene_labels.len() == 1 { "" } else { "s" },
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
                let localized = self
                    .scene_labels
                    .get(&archive.stage_id.to_ascii_lowercase());
                filter.is_empty()
                    || archive.stage_id.to_ascii_lowercase().contains(&filter)
                    || archive.group.to_ascii_lowercase().contains(&filter)
                    || localized.is_some_and(|label| {
                        label
                            .stage_name
                            .as_ref()
                            .is_some_and(|name| name.to_ascii_lowercase().contains(&filter))
                            || label
                                .scenario_names
                                .iter()
                                .any(|name| name.to_ascii_lowercase().contains(&filter))
                    })
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
            let layout = content_browser_layout(ui.available_width(), archives.len());
            egui::Grid::new("content-browser-grid")
                .num_columns(layout.columns)
                .min_col_width(layout.card_width)
                .max_col_width(layout.card_width)
                .spacing(egui::vec2(8.0, 8.0))
                .show(ui, |ui| {
                    for (index, archive) in archives.iter().enumerate() {
                        let selected = self.stage_id.eq_ignore_ascii_case(&archive.stage_id);
                        let localized = self
                            .scene_labels
                            .get(&archive.stage_id.to_ascii_lowercase());
                        let label = content_browser_card_text(archive, localized);
                        let response = content_browser_card_button(
                            ui,
                            egui::vec2(layout.card_width, 88.0),
                            selected,
                            &label,
                        )
                        .on_hover_text(content_browser_hover_text(archive, localized));
                        if response.clicked() {
                            open_archive = Some(archive.clone());
                        }
                        if (index + 1) % layout.columns == 0 {
                            ui.end_row();
                        }
                    }
                });

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
        ui.small("Drag a class into the viewport, or use Add at the camera focus.");
        ui.add_space(4.0);

        let mut chosen: Option<String> = None;
        let mut spawn_now: Option<String> = None;
        let filter = self.object_filter.to_ascii_lowercase();
        let matching_objects: Vec<ObjectDefinition> = self
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
                    .cloned()
                    .collect()
            })
            .unwrap_or_default();
        let entries = matching_objects
            .into_iter()
            .map(|object| {
                let placeable = self.can_spawn_factory(&object.factory_name);
                (object, placeable)
            })
            .collect::<Vec<_>>();
        let placeable_count = entries.iter().filter(|(_, placeable)| *placeable).count();
        ui.small(format!(
            "{placeable_count} placeable / {} matching",
            entries.len()
        ));

        egui::ScrollArea::vertical().show(ui, |ui| {
            for (object, placeable) in entries {
                ui.horizontal(|ui| {
                    let selected = self.palette_factory.as_deref() == Some(&object.factory_name);
                    let current_stage_clone = self.document.as_ref().is_some_and(|document| {
                        document.objects.iter().any(|candidate| {
                            candidate.factory_name == object.factory_name
                                && candidate.placement.is_some()
                        })
                    });
                    let catalog_default =
                        self.object_authoring_catalog.find(&object.factory_name);
                    let route_help = if object.factory_name == "Mario" {
                        if current_stage_clone {
                            "Special Mario placement: this stage already has its singleton player record; edit or move the existing Mario."
                        } else if placeable {
                            "Special Mario placement: creates the blank-stage player record at the drop location."
                        } else {
                            "Special Mario placement is available only when an authored blank stage does not already contain Mario."
                        }
                    } else if object.factory_name == "Sky" {
                        if current_stage_clone {
                            "Special Sky placement: this stage already has its singleton TSky record; edit or move the existing Sky."
                        } else if placeable {
                            "Special Sky placement: creates TSky. Then assign a .smsmodel with the Stage skybox export role."
                        } else {
                            "Special Sky placement is unavailable because this stage cannot accept another TSky record."
                        }
                    } else if current_stage_clone {
                        "Placement source: current-stage clone of an existing typed record."
                    } else if catalog_default.is_some() {
                        "Placement source: retail-backed authored default."
                    } else if placeable {
                        "Placement source: typed project constructor."
                    } else if is_palette_service_record(&object) {
                        "Service record: installed automatically when you place an actor that depends on it; it is not directly placeable in the viewport."
                    } else if object.unsafe_to_edit {
                        "Unavailable class: the schema marks this record unsafe to edit, so it cannot be placed directly in the viewport."
                    } else {
                        "Unavailable class: no safe retail-backed default or current-stage typed instance is available to clone, so direct placement would be unsafe."
                    };
                    let mut placement_help =
                        format!("{} / {}\n{route_help}", object.category, object.class_name);
                    if let Some(template) = catalog_default {
                        placement_help.push_str(&format!(
                            "\nDefault source stage: {}\nAutomatic support: {} dependenc{} and {} required resource{}.",
                            template.source_stage,
                            template.dependencies.len(),
                            if template.dependencies.len() == 1 {
                                "y"
                            } else {
                                "ies"
                            },
                            template.resources.len(),
                            if template.resources.len() == 1 {
                                ""
                            } else {
                                "s"
                            },
                        ));
                    }
                    let response = ui
                        .selectable_label(selected, &object.factory_name)
                        .on_hover_text(placement_help.as_str());
                    if placeable {
                        response.dnd_set_drag_payload(ObjectPaletteDragPayload {
                            factory_name: object.factory_name.clone(),
                        });
                    }
                    if placeable && response.clicked() {
                        chosen = Some(object.factory_name.clone());
                    }
                    if ui
                        .add_enabled(placeable, egui::Button::new("Add"))
                        .on_hover_text(placement_help.as_str())
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
        ui.horizontal(|ui| {
            ui.heading("Hierarchy");
            let object_count = self
                .document
                .as_ref()
                .map_or(0, |document| document.objects.len());
            let model_instance_count = self
                .model_instances
                .iter()
                .filter(|instance| instance.stage_id.eq_ignore_ascii_case(&self.stage_id))
                .count();
            ui.label(
                egui::RichText::new(format!(
                    "{object_count} objects / {} model instances",
                    model_instance_count
                ))
                .small()
                .color(egui::Color32::GRAY),
            );
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui
                    .add_enabled(
                        self.selected_object_id.is_some(),
                        egui::Button::new("Delete").small(),
                    )
                    .on_hover_text("Delete the selected object")
                    .clicked()
                {
                    self.delete_selected();
                }
                if ui
                    .add_enabled(
                        self.selected_object_id.is_some(),
                        egui::Button::new("Duplicate").small(),
                    )
                    .on_hover_text("Duplicate the selected object")
                    .clicked()
                {
                    self.duplicate_selected();
                }
            });
        });
        ui.add(
            egui::TextEdit::singleline(&mut self.outliner_filter)
                .hint_text("Search hierarchy...")
                .desired_width(f32::INFINITY),
        );

        let selected_id = self.selected_object_id.as_deref();
        let tree = self
            .document
            .as_ref()
            .map(|document| build_outliner_tree(document, &self.outliner_filter));
        if let Some(tree) = tree.as_ref() {
            if !self.outliner_filter.trim().is_empty() {
                ui.label(
                    egui::RichText::new(format!(
                        "Showing {} of {} objects",
                        tree.visible_objects, tree.total_objects
                    ))
                    .small()
                    .color(egui::Color32::GRAY),
                );
            }
        }
        ui.separator();

        let mut clicked_id = None;
        let mut clicked_model_instance = None;
        let model_instances = self
            .model_instances
            .iter()
            .filter(|instance| instance.stage_id.eq_ignore_ascii_case(&self.stage_id))
            .cloned()
            .collect::<Vec<_>>();
        egui::ScrollArea::vertical().show(ui, |ui| {
            if !model_instances.is_empty() {
                egui::CollapsingHeader::new("Authored Model Instances")
                    .default_open(true)
                    .show(ui, |ui| {
                        for instance in &model_instances {
                            if ui
                                .selectable_label(
                                    self.selected_model_instance_id
                                        == Some(instance.placement.instance_id),
                                    format!(
                                        "{}  ({})",
                                        instance.placement.name, instance.placement.instance_id
                                    ),
                                )
                                .clicked()
                            {
                                clicked_model_instance = Some(instance.placement.instance_id);
                            }
                        }
                    });
                ui.separator();
            }
            let Some(tree) = tree.as_ref() else {
                ui.add_space(16.0);
                ui.vertical_centered(|ui| {
                    ui.label(egui::RichText::new("No level open").color(egui::Color32::GRAY));
                    ui.small("Open a level to browse its scene hierarchy.");
                });
                return;
            };
            clicked_id = show_outliner_tree(
                ui,
                tree,
                selected_id,
                !self.outliner_filter.trim().is_empty(),
            );
            if tree.visible_objects == 0 {
                ui.add_space(16.0);
                ui.vertical_centered(|ui| {
                    ui.label(egui::RichText::new("No matching objects").strong());
                    ui.small("Try a factory, class, object name, or identifier.");
                });
            }
        });
        if let Some(id) = clicked_id {
            if self.asset_dirty && !self.save_selected_model_asset() {
                return;
            }
            self.selected_object_id = Some(id);
            self.selected_model_instance_id = None;
            self.selected_model_asset = None;
            self.selected_model_document = None;
            self.saved_model_document = None;
        }
        if let Some(id) = clicked_model_instance {
            if self.asset_dirty && !self.save_selected_model_asset() {
                return;
            }
            self.selected_model_instance_id = Some(id);
            self.selected_object_id = None;
            self.selected_model_asset = None;
            self.selected_model_document = None;
            self.saved_model_document = None;
        }
    }

    pub(super) fn inspector_panel(&mut self, ui: &mut egui::Ui) {
        if self.selected_model_instance_id.is_some() {
            self.model_instance_inspector_panel(ui);
            self.stage_lighting_panel(ui);
            return;
        }
        if self.selected_model_document.is_some() {
            self.model_asset_inspector_panel(ui);
            self.stage_lighting_panel(ui);
            return;
        }
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

            if !object.runtime_references.is_empty() {
                ui.separator();
                ui.heading("Runtime Links");
                ui.small(
                    "Choose the placed actors used by fixed runtime name lookups. Export binds their runtime identities without changing the editor labels.",
                );
                for (index, reference) in object.runtime_references.iter().enumerate() {
                    let candidates = self
                        .document
                        .as_ref()
                        .into_iter()
                        .flat_map(|document| document.objects.iter())
                        .filter(|candidate| {
                            candidate.id != object.id
                                && candidate.factory_name == reference.required_factory_name
                        })
                        .map(|candidate| {
                            let label = candidate
                                .raw_param("name")
                                .filter(|name| !name.is_empty())
                                .map_or_else(
                                    || candidate.id.clone(),
                                    |name| format!("{name} ({})", candidate.id),
                                );
                            (candidate.id.clone(), label)
                        })
                        .collect::<Vec<_>>();
                    let mut selected = reference.target_object_id.clone();
                    ui.label(format!(
                        "{}Triggered {}",
                        if reference.required { "" } else { "Optional " },
                        reference.required_factory_name
                    ));
                    let selected_label = selected
                        .as_ref()
                        .and_then(|selected_id| {
                            candidates
                                .iter()
                                .find(|(id, _)| id == selected_id)
                                .map(|(_, label)| label.as_str())
                        })
                        .unwrap_or("Select an actor...");
                    let response = egui::ComboBox::from_id_salt((
                        "runtime-reference",
                        object.id.as_str(),
                        index,
                    ))
                    .selected_text(selected_label)
                    .show_ui(ui, |ui| {
                        ui.selectable_value(&mut selected, None, "Unassigned");
                        for (candidate_id, label) in &candidates {
                            ui.selectable_value(&mut selected, Some(candidate_id.clone()), label);
                        }
                    });
                    response.response.on_hover_text(format!(
                        "{} runtime lookup {:?}; only compatible {} actors are listed.",
                        if reference.required {
                            "Required"
                        } else {
                            "Optional"
                        },
                        reference.runtime_name,
                        reference.required_factory_name
                    ));
                    if selected != reference.target_object_id {
                        self.update_selected_runtime_reference(index, selected);
                    }
                    if candidates.is_empty() && reference.required {
                        ui.colored_label(
                            egui::Color32::from_rgb(255, 180, 90),
                            format!(
                                "Place a {} actor in this stage, then select it here.",
                                reference.required_factory_name
                            ),
                        );
                    }
                }
            }

            ui.separator();
            ui.heading("Params");
            let editable_parameters = self
                .document
                .as_ref()
                .map(|document| document.editable_parameters_for_object(&object));
            let mut canonical_keys = std::collections::BTreeSet::new();
            match editable_parameters {
                Some(Ok(parameters)) => {
                    egui::Grid::new("typed-object-parameters")
                        .num_columns(3)
                        .spacing(egui::vec2(8.0, 5.0))
                        .show(ui, |ui| {
                            for parameter in parameters {
                                canonical_keys.insert(parameter.key.clone());
                                let editable = parameter.read_only_reason.is_none();
                                let parameter_hover = parameter.info.as_ref().map_or_else(
                                    || format!("{:?}", parameter.kind),
                                    |info| format!("{:?}\n\n{}", parameter.kind, info.description),
                                );
                                let display_name = parameter
                                    .info
                                    .as_ref()
                                    .and_then(|info| info.display_name.as_deref())
                                    .unwrap_or(parameter.key.as_str());
                                ui.add_enabled(editable, egui::Label::new(display_name))
                                    .on_hover_text(parameter_hover);
                                let ObjectParameterControlResponse {
                                    edit,
                                    raw_value,
                                    error,
                                } = ui
                                    .add_enabled_ui(editable, |ui| {
                                        object_parameter_control(ui, &parameter)
                                    })
                                    .inner;
                                let mut status_help = Vec::new();
                                let (status, status_color) =
                                    if let Some(reason) = parameter.read_only_reason.as_deref() {
                                        status_help.push(format!("Read-only: {reason}"));
                                        ("Read-only \u{24d8}".to_string(), None)
                                    } else if let Some(error) = error {
                                        status_help.push(error);
                                        (
                                            "Invalid value \u{24d8}".to_string(),
                                            Some(egui::Color32::from_rgb(255, 116, 104)),
                                        )
                                    } else {
                                        (format!("{:?} \u{24d8}", parameter.kind), None)
                                    };
                                if let Some(info) = parameter.info.as_ref() {
                                    status_help.push(info.description.clone());
                                }
                                let status = status_color.map_or_else(
                                    || egui::RichText::new(&status).small().weak(),
                                    |color| egui::RichText::new(&status).small().color(color),
                                );
                                let response = ui.label(status);
                                if !status_help.is_empty() {
                                    response.on_hover_text(status_help.join("\n\n"));
                                }
                                ui.end_row();

                                if editable {
                                    if edit.started {
                                        self.begin_parameter_undo_transaction();
                                    }
                                    if let Some(raw_value) = raw_value {
                                        self.update_selected_parameter(
                                            parameter.key.clone(),
                                            raw_value,
                                        );
                                    }
                                    if edit.stopped {
                                        self.commit_undo_transaction("Updated object parameter");
                                        self.rebuild_model_preview_from_document();
                                    }
                                }
                            }
                        });
                }
                Some(Err(error)) => {
                    ui.colored_label(
                        egui::Color32::from_rgb(255, 116, 104),
                        format!("Could not resolve typed parameters: {error}"),
                    );
                }
                None => {
                    ui.colored_label(
                        egui::Color32::from_rgb(255, 116, 104),
                        "The selected object is not attached to an open stage document.",
                    );
                }
            }

            let diagnostics = object
                .raw_params
                .iter()
                .filter(|(key, _)| !canonical_keys.contains(*key))
                .collect::<Vec<_>>();
            if !diagnostics.is_empty() {
                egui::CollapsingHeader::new("Diagnostics")
                    .default_open(false)
                    .show(ui, |ui| {
                        ui.small(
                            "Derived preview aliases and legacy raw values are read-only. Edit the canonical typed parameter above.",
                        );
                        for (key, parameter) in diagnostics {
                            if let Some(decoded) = parameter.decoded() {
                                ui.monospace(format!(
                                    "{key}: {} ({decoded:?})",
                                    parameter.raw()
                                ));
                            } else {
                                ui.monospace(format!("{key}: {}", parameter.raw()));
                            }
                        }
                    });
            }
        } else {
            ui.heading("Inspector");
            if self.document.is_some() {
                ui.label("No object selected.");
            } else {
                ui.label("No stage open.");
            }
        }
        self.stage_lighting_panel(ui);
    }

    fn stage_lighting_panel(&mut self, ui: &mut egui::Ui) {
        let Some(mut lighting) = self
            .document
            .as_ref()
            .map(|document| document.lighting.clone())
        else {
            return;
        };
        if lighting.lights.is_empty() && lighting.ambients.is_empty() {
            return;
        }

        ui.separator();
        let mut changed = false;
        egui::CollapsingHeader::new("Stage Lighting")
            .default_open(false)
            .show(ui, |ui| {
                ui.small(
                    "These are the ordered typed LightAry and AmbAry records used by Sunshine. Changes are stored with this scene and work on authored or retail-derived stages.",
                );
                for (index, light) in lighting.lights.iter_mut().enumerate() {
                    let name = light.name.as_deref().unwrap_or("Unnamed light");
                    egui::CollapsingHeader::new(format!("Light {} - {name}", index + 1))
                        .id_salt(("stage-light", index))
                        .show(ui, |ui| {
                            ui.label("Position / direction source");
                            changed |= vector_drag(ui, &mut light.position, 100.0).changed;
                            ui.label("Color");
                            changed |= rgba8_drag(ui, &mut light.color);
                        });
                }
                for (index, ambient) in lighting.ambients.iter_mut().enumerate() {
                    let name = ambient.name.as_deref().unwrap_or("Unnamed ambient");
                    egui::CollapsingHeader::new(format!("Ambient {} - {name}", index + 1))
                        .id_salt(("stage-ambient", index))
                        .show(ui, |ui| {
                            changed |= rgba8_drag(ui, &mut ambient.color);
                        });
                }
            });
        if changed {
            self.update_stage_lighting(lighting);
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

struct ObjectParameterControlResponse {
    edit: VectorDragResponse,
    raw_value: Option<String>,
    error: Option<String>,
}

fn object_parameter_control(
    ui: &mut egui::Ui,
    parameter: &EditableSceneParameter,
) -> ObjectParameterControlResponse {
    if let Some(info) = parameter
        .info
        .as_ref()
        .filter(|info| !info.choices.is_empty() || info.indexed_choice.is_some())
    {
        const INDEXED_CHOICE_TOKEN: &str = "\0indexed-choice";

        let parsed_index = parameter.raw_value.trim().parse::<i64>().ok();
        let indexed_selected = info
            .indexed_choice
            .as_ref()
            .is_some_and(|indexed| parsed_index.is_some_and(|value| indexed.accepts_index(value)));
        let mut selected = if indexed_selected {
            INDEXED_CHOICE_TOKEN.to_string()
        } else {
            parameter.raw_value.clone()
        };
        let selected_text = if indexed_selected {
            info.indexed_choice
                .as_ref()
                .map(|indexed| indexed.label.as_str())
                .unwrap_or(parameter.raw_value.as_str())
        } else {
            info.choices
                .iter()
                .find(|choice| choice.raw_value == selected)
                .map(|choice| choice.label.as_str())
                .unwrap_or(parameter.raw_value.as_str())
        };
        let mut edit = VectorDragResponse::default();
        let mut raw_value = None;
        let mut error = None;

        ui.horizontal(|ui| {
            let mut selection_changed = false;
            egui::ComboBox::from_id_salt(("object-parameter-choice", parameter.key.as_str()))
                .selected_text(selected_text)
                .width(150.0)
                .show_ui(ui, |ui| {
                    for choice in &info.choices {
                        let response = ui
                            .selectable_value(
                                &mut selected,
                                choice.raw_value.clone(),
                                &choice.label,
                            )
                            .on_hover_text(&choice.description);
                        selection_changed |= response.changed();
                    }
                    if let Some(indexed) = info.indexed_choice.as_ref() {
                        let response = ui
                            .selectable_value(
                                &mut selected,
                                INDEXED_CHOICE_TOKEN.to_string(),
                                &indexed.label,
                            )
                            .on_hover_text(&indexed.description);
                        selection_changed |= response.changed();
                    }
                });

            if selection_changed {
                edit.changed = true;
                edit.started = true;
                edit.stopped = true;
                raw_value = Some(if selected == INDEXED_CHOICE_TOKEN {
                    info.indexed_choice
                        .as_ref()
                        .map(|indexed| indexed.default_index.to_string())
                        .unwrap_or_default()
                } else {
                    selected.clone()
                });
            }

            if selected == INDEXED_CHOICE_TOKEN {
                let indexed = info
                    .indexed_choice
                    .as_ref()
                    .expect("indexed selection requires indexed metadata");
                ui.label(format!("{}:", indexed.index_label));
                let mut index = if indexed_selected {
                    parsed_index.unwrap_or(indexed.default_index)
                } else {
                    indexed.default_index
                };
                let response = ui
                    .add(
                        egui::DragValue::new(&mut index)
                            .speed(1.0)
                            .range(indexed.index_range[0]..=indexed.index_range[1]),
                    )
                    .on_hover_text(&indexed.description);
                let index_edit = object_parameter_widget_response(&response);
                edit.merge(index_edit);
                if index_edit.changed {
                    if indexed.reserved_indices.contains(&index) {
                        let reserved_for = info
                            .choices
                            .iter()
                            .find(|choice| choice.raw_value == index.to_string())
                            .map(|choice| choice.label.as_str())
                            .unwrap_or("another coin type");
                        error = Some(format!(
                            "Blue coin slot {index} is reserved for {reserved_for}. Choose another slot."
                        ));
                        raw_value = None;
                    } else {
                        raw_value = Some(index.to_string());
                    }
                }
                if !indexed.is_retail_index(index) {
                    ui.label(
                        egui::RichText::new("Expanded")
                            .small()
                            .color(egui::Color32::from_rgb(235, 190, 92)),
                    )
                    .on_hover_text(
                        "This slot is retained by the editor but requires a compatible expanded runtime; retail Sunshine only implements blue-coin slots 0-49.",
                    );
                }
            }
        });

        return ObjectParameterControlResponse {
            edit,
            raw_value,
            error,
        };
    }
    match parameter.kind {
        ObjectParameterKind::U32 => {
            let parsed = parameter.raw_value.trim().parse::<u32>();
            let mut error = parsed
                .as_ref()
                .err()
                .map(|error| format!("Expected u32: {error}"));
            let mut value = parsed.unwrap_or_default();
            let response = ui.add(egui::DragValue::new(&mut value).speed(1.0));
            let edit = object_parameter_widget_response(&response);
            let raw_value = edit.changed.then(|| value.to_string());
            if raw_value.is_some() {
                error = None;
            }
            ObjectParameterControlResponse {
                edit,
                raw_value,
                error,
            }
        }
        ObjectParameterKind::I32 => {
            let parsed = parameter.raw_value.trim().parse::<i32>();
            let mut error = parsed
                .as_ref()
                .err()
                .map(|error| format!("Expected i32: {error}"));
            let mut value = parsed.unwrap_or_default();
            let mut drag = egui::DragValue::new(&mut value).speed(1.0);
            if let Some([minimum, maximum]) =
                parameter.info.as_ref().and_then(|info| info.integer_range)
            {
                drag = drag.range(minimum..=maximum);
            }
            let response = ui.add(drag);
            let edit = object_parameter_widget_response(&response);
            let raw_value = edit.changed.then(|| value.to_string());
            if raw_value.is_some() {
                error = None;
            }
            ObjectParameterControlResponse {
                edit,
                raw_value,
                error,
            }
        }
        ObjectParameterKind::F32 => {
            let parsed = parse_finite_parameter_f32(&parameter.raw_value);
            let mut error = parsed.as_ref().err().cloned();
            let mut value = parsed.unwrap_or_default();
            let response = ui.add(egui::DragValue::new(&mut value).speed(0.1));
            let edit = object_parameter_widget_response(&response);
            let raw_value = edit.changed.then(|| value.to_string());
            if raw_value.is_some() {
                error = None;
            }
            ObjectParameterControlResponse {
                edit,
                raw_value,
                error,
            }
        }
        ObjectParameterKind::Vec2F32 => {
            let parsed = parse_finite_parameter_vector::<2>(&parameter.raw_value);
            let mut error = parsed.as_ref().err().cloned();
            let mut values = parsed.unwrap_or([0.0; 2]);
            let mut edit = VectorDragResponse::default();
            ui.horizontal(|ui| {
                for (label, value) in ["X", "Y"].into_iter().zip(values.iter_mut()) {
                    let response = ui.add(
                        egui::DragValue::new(value)
                            .speed(0.1)
                            .prefix(format!("{label} ")),
                    );
                    edit.merge(object_parameter_widget_response(&response));
                }
            });
            let raw_value = edit.changed.then(|| format!("{},{}", values[0], values[1]));
            if raw_value.is_some() {
                error = None;
            }
            ObjectParameterControlResponse {
                edit,
                raw_value,
                error,
            }
        }
        ObjectParameterKind::Vec3F32 => {
            let parsed = parse_finite_parameter_vector::<3>(&parameter.raw_value);
            let mut error = parsed.as_ref().err().cloned();
            let mut values = parsed.unwrap_or([0.0; 3]);
            let mut edit = VectorDragResponse::default();
            ui.horizontal(|ui| {
                for (label, value) in ["X", "Y", "Z"].into_iter().zip(values.iter_mut()) {
                    let response = ui.add(
                        egui::DragValue::new(value)
                            .speed(0.1)
                            .prefix(format!("{label} ")),
                    );
                    edit.merge(object_parameter_widget_response(&response));
                }
            });
            let raw_value = edit
                .changed
                .then(|| format!("{},{},{}", values[0], values[1], values[2]));
            if raw_value.is_some() {
                error = None;
            }
            ObjectParameterControlResponse {
                edit,
                raw_value,
                error,
            }
        }
        ObjectParameterKind::ColorRgba8 => {
            let parsed = parse_parameter_rgba8(&parameter.raw_value);
            let mut error = parsed.as_ref().err().cloned();
            let mut values = parsed.unwrap_or([0; 4]);
            let mut edit = VectorDragResponse::default();
            ui.horizontal(|ui| {
                for (label, value) in ["R", "G", "B", "A"].into_iter().zip(values.iter_mut()) {
                    let response = ui.add(
                        egui::DragValue::new(value)
                            .range(0..=255)
                            .speed(1.0)
                            .prefix(format!("{label} ")),
                    );
                    edit.merge(object_parameter_widget_response(&response));
                }
            });
            let raw_value = edit
                .changed
                .then(|| format!("{},{},{},{}", values[0], values[1], values[2], values[3]));
            if raw_value.is_some() {
                error = None;
            }
            ObjectParameterControlResponse {
                edit,
                raw_value,
                error,
            }
        }
        ObjectParameterKind::String => {
            let mut value = parameter.raw_value.clone();
            let response = ui.add(
                egui::TextEdit::singleline(&mut value)
                    .desired_width(220.0)
                    .clip_text(false),
            );
            let edit = object_parameter_widget_response(&response);
            let error = sms_formats::jdrama_key_code(&value)
                .err()
                .map(|error| format!("Invalid Shift-JIS text: {error}"));
            let raw_value = (edit.changed && error.is_none()).then_some(value);
            ObjectParameterControlResponse {
                edit,
                raw_value,
                error,
            }
        }
    }
}

fn object_parameter_widget_response(response: &egui::Response) -> VectorDragResponse {
    VectorDragResponse {
        changed: response.changed(),
        started: response.gained_focus() || response.drag_started(),
        stopped: response.lost_focus() || response.drag_stopped(),
    }
}

fn parse_finite_parameter_f32(raw: &str) -> Result<f32, String> {
    let value = raw
        .trim()
        .parse::<f32>()
        .map_err(|error| format!("Expected finite f32: {error}"))?;
    if value.is_finite() {
        Ok(value)
    } else {
        Err(format!("Expected finite f32, got '{raw}'"))
    }
}

fn parse_finite_parameter_vector<const N: usize>(raw: &str) -> Result<[f32; N], String> {
    let parts = raw.split(',').collect::<Vec<_>>();
    if parts.len() != N {
        return Err(format!(
            "Expected {N} comma-separated finite f32 components, got {}",
            parts.len()
        ));
    }
    let mut values = [0.0; N];
    for (index, part) in parts.into_iter().enumerate() {
        values[index] = parse_finite_parameter_f32(part)
            .map_err(|error| format!("Component {}: {error}", index + 1))?;
    }
    Ok(values)
}

fn parse_parameter_rgba8(raw: &str) -> Result<[u8; 4], String> {
    let parts = raw.split(',').collect::<Vec<_>>();
    if parts.len() != 4 {
        return Err(format!(
            "Expected 4 comma-separated RGBA8 components, got {}",
            parts.len()
        ));
    }
    let mut values = [0; 4];
    for (index, part) in parts.into_iter().enumerate() {
        values[index] = part
            .trim()
            .parse::<u8>()
            .map_err(|error| format!("Channel {}: {error}", index + 1))?;
    }
    Ok(values)
}

fn is_palette_service_record(object: &ObjectDefinition) -> bool {
    is_palette_service_type(&object.factory_name) || is_palette_service_type(&object.class_name)
}

fn is_palette_service_type(type_name: &str) -> bool {
    let leaf = type_name.rsplit("::").next().unwrap_or(type_name).trim();
    let semantic = leaf.strip_prefix('T').unwrap_or(leaf);
    semantic.ends_with("Manager") || semantic.ends_with("Director")
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

fn rgba8_drag(ui: &mut egui::Ui, color: &mut [u8; 4]) -> bool {
    let mut changed = false;
    ui.horizontal(|ui| {
        for (channel, label) in color.iter_mut().zip(["R", "G", "B", "A"]) {
            changed |= ui
                .add(
                    egui::DragValue::new(channel)
                        .range(0..=255)
                        .speed(1.0)
                        .prefix(format!("{label} ")),
                )
                .changed();
        }
    });
    changed
}

#[cfg(test)]
mod parameter_control_tests {
    use super::{
        is_palette_service_type, parse_finite_parameter_f32, parse_finite_parameter_vector,
        parse_parameter_rgba8,
    };

    #[test]
    fn parameter_number_parsers_accept_canonical_values() {
        assert_eq!(parse_finite_parameter_f32(" 1.25 "), Ok(1.25));
        assert_eq!(
            parse_finite_parameter_vector::<3>("1,-2.5,3"),
            Ok([1.0, -2.5, 3.0])
        );
        assert_eq!(parse_parameter_rgba8("0,128,255,7"), Ok([0, 128, 255, 7]));
    }

    #[test]
    fn parameter_number_parsers_reject_non_finite_or_malformed_values() {
        for raw in ["NaN", "inf", "-inf"] {
            assert!(parse_finite_parameter_f32(raw).is_err(), "{raw}");
        }
        assert!(parse_finite_parameter_vector::<2>("1").is_err());
        assert!(parse_finite_parameter_vector::<2>("1,NaN").is_err());
        assert!(parse_parameter_rgba8("0,1,2").is_err());
        assert!(parse_parameter_rgba8("0,1,2,256").is_err());
    }

    #[test]
    fn palette_service_types_cover_managers_and_directors_only() {
        assert!(is_palette_service_type("TBEelTearsManager"));
        assert!(is_palette_service_type("JDrama::TMarDirector"));
        assert!(is_palette_service_type("MapObjManager"));
        assert!(!is_palette_service_type("TBEelTears"));
        assert!(!is_palette_service_type("DirectorSwitch"));
    }
}
