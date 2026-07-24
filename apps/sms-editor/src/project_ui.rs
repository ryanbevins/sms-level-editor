use super::*;

use std::path::{Path, PathBuf};

use rfd::FileDialog;

#[derive(Debug, Clone)]
pub(super) struct NewProjectDraft {
    name: String,
    base_game_root: String,
    descriptor_path: String,
}

impl NewProjectDraft {
    fn new() -> Self {
        Self {
            name: "New SMS Project".to_string(),
            base_game_root: default_base_root(),
            descriptor_path: String::new(),
        }
    }

    fn can_create(&self) -> bool {
        !self.name.trim().is_empty()
            && Path::new(self.base_game_root.trim()).is_dir()
            && !self.descriptor_path.trim().is_empty()
    }
}

impl SmsEditorApp {
    pub(super) fn project_hub(&mut self, ui: &mut egui::Ui) {
        let available = ui.available_rect_before_wrap();
        let card_width = available.width().min(1120.0);
        ui.vertical_centered(|ui| {
            ui.add_space((available.height() * 0.07).clamp(28.0, 72.0));
            ui.heading(
                egui::RichText::new("Graffito-Editor")
                    .size(34.0)
                    .color(egui::Color32::from_rgb(159, 218, 208)),
            );
            ui.label(
                egui::RichText::new("Choose a project to continue")
                    .size(16.0)
                    .color(egui::Color32::from_gray(185)),
            );
            ui.add_space(24.0);
        });

        let mut open_project = None;
        let mut remove_recent = None;
        let mut create_project = false;
        let mut browse_project = false;
        let mut import_legacy = false;
        let horizontal_margin = ((available.width() - card_width) * 0.5).max(0.0);
        ui.horizontal(|ui| {
            ui.add_space(horizontal_margin);
            ui.allocate_ui_with_layout(
                egui::vec2(card_width, ui.available_height()),
                egui::Layout::top_down(egui::Align::Center),
                |ui| {
                ui.horizontal(|ui| {
                    if ui
                        .add_sized([170.0, 38.0], egui::Button::new("New Project"))
                        .on_hover_text("Create a named .sms project and managed data folder")
                        .clicked()
                    {
                        create_project = true;
                    }
                    if ui
                        .add_sized([170.0, 38.0], egui::Button::new("Open Project..."))
                        .on_hover_text("Choose an existing .sms project in Explorer")
                        .clicked()
                    {
                        browse_project = true;
                    }
                    if ui
                        .add_sized(
                            [190.0, 38.0],
                            egui::Button::new("Import Legacy Folder..."),
                        )
                        .on_hover_text(
                            "Wrap an existing sms-project.toml folder in a new .sms descriptor",
                        )
                        .clicked()
                    {
                        import_legacy = true;
                    }
                });
                ui.add_space(10.0);
                ui.small(
                    "Projects are portable .sms descriptor files. Extracted game data remains read-only; editor changes live in a separate managed data folder.",
                );
                if let Some(error) = &self.project_hub_error {
                    ui.add_space(12.0);
                    ui.colored_label(egui::Color32::from_rgb(255, 132, 112), error);
                }
                ui.add_space(24.0);
                ui.separator();
                ui.add_space(12.0);
                ui.horizontal(|ui| {
                    ui.heading("Recent Projects");
                    ui.label(
                        egui::RichText::new(format!(
                            "{} project{}",
                            self.recent_projects.entries().len(),
                            if self.recent_projects.entries().len() == 1 {
                                ""
                            } else {
                                "s"
                            }
                        ))
                        .color(egui::Color32::from_gray(150)),
                    );
                });
                ui.add_space(6.0);

                if self.recent_projects.entries().is_empty() {
                    ui.group(|ui| {
                        ui.set_min_width((card_width - 24.0).max(320.0));
                        ui.add_space(10.0);
                        ui.label(egui::RichText::new("No recent projects yet").strong());
                        ui.small(
                            "Create a project or open an existing .sms file. It will appear here the next time the editor starts.",
                        );
                        ui.add_space(10.0);
                    });
                } else {
                    let entries = self.recent_projects.entries().to_vec();
                    egui::ScrollArea::vertical().show(ui, |ui| {
                        for entry in entries {
                            let descriptor_exists = entry.path.is_file();
                            let base_exists = entry.base_game_root.is_dir();
                            ui.group(|ui| {
                                ui.set_min_width((card_width - 24.0).max(320.0));
                                ui.horizontal(|ui| {
                                    ui.vertical(|ui| {
                                        let display_name = if entry.name.trim().is_empty() {
                                            entry
                                                .path
                                                .file_stem()
                                                .and_then(|stem| stem.to_str())
                                                .unwrap_or("Unnamed Project")
                                        } else {
                                            &entry.name
                                        };
                                        ui.label(
                                            egui::RichText::new(display_name).size(17.0).strong(),
                                        );
                                        ui.label(
                                            egui::RichText::new(entry.path.display().to_string())
                                                .monospace()
                                                .color(egui::Color32::from_gray(155)),
                                        );
                                        let status = if !descriptor_exists {
                                            "Project file is missing"
                                        } else if !base_exists {
                                            "Extracted game folder needs to be located"
                                        } else {
                                            "Ready"
                                        };
                                        let status_color = if descriptor_exists && base_exists {
                                            egui::Color32::from_rgb(111, 220, 168)
                                        } else {
                                            egui::Color32::from_rgb(245, 190, 90)
                                        };
                                        ui.horizontal(|ui| {
                                            ui.colored_label(status_color, status);
                                            ui.label(
                                                egui::RichText::new(recent_age_label(
                                                    entry.last_opened_unix,
                                                ))
                                                .color(egui::Color32::from_gray(145)),
                                            );
                                        });
                                    });
                                    ui.with_layout(
                                        egui::Layout::right_to_left(egui::Align::Center),
                                        |ui| {
                                            if ui.small_button("Remove").clicked() {
                                                remove_recent = Some(entry.path.clone());
                                            }
                                            if ui
                                                .add_enabled(
                                                    descriptor_exists,
                                                    egui::Button::new("Open"),
                                                )
                                                .clicked()
                                            {
                                                open_project = Some(entry.path.clone());
                                            }
                                        },
                                    );
                                });
                            });
                            ui.add_space(6.0);
                        }
                    });
                }
                },
            );
        });

        if create_project {
            self.project_hub_error = None;
            self.new_project_draft = Some(NewProjectDraft::new());
        }
        if browse_project {
            if let Some(path) = project_open_dialog().pick_file() {
                open_project = Some(path);
            }
        }
        if import_legacy {
            self.import_legacy_project_with_dialogs();
        }
        if let Some(path) = remove_recent {
            if let Err(error) = self.recent_projects.remove(&path) {
                self.project_hub_error = Some(error);
            }
        }
        if let Some(path) = open_project {
            self.open_project_descriptor(path);
        }
    }

    pub(super) fn new_project_dialog(&mut self, ctx: &egui::Context) {
        if self.new_project_draft.is_none() {
            return;
        }
        let mut choose_base = false;
        let mut choose_descriptor = false;
        let mut create = false;
        let mut cancel = false;
        egui::Window::new("Create SMS Project")
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
            .show(ctx, |ui| {
                let draft = self
                    .new_project_draft
                    .as_mut()
                    .expect("new-project dialog requires a draft");
                ui.set_min_width(620.0);
                ui.label("A project has one .sms descriptor and one managed data folder.");
                ui.add_space(10.0);
                ui.label("Project Name");
                ui.text_edit_singleline(&mut draft.name);
                ui.add_space(8.0);
                if path_display_row(
                    ui,
                    "Extracted Game",
                    &draft.base_game_root,
                    "Browse...",
                    true,
                ) {
                    choose_base = true;
                }
                ui.small(
                    "The extracted base game is only read; project files are never written there.",
                );
                ui.add_space(8.0);
                if path_display_row(
                    ui,
                    "Project File",
                    &draft.descriptor_path,
                    "Choose...",
                    true,
                ) {
                    choose_descriptor = true;
                }
                ui.small("Choose the location and filename for the new .sms project.");
                ui.add_space(14.0);
                ui.horizontal(|ui| {
                    if ui
                        .add_enabled(draft.can_create(), egui::Button::new("Create Project"))
                        .clicked()
                    {
                        create = true;
                    }
                    if ui.button("Cancel").clicked() {
                        cancel = true;
                    }
                });
            });

        if choose_base {
            let current = self
                .new_project_draft
                .as_ref()
                .map(|draft| draft.base_game_root.as_str())
                .unwrap_or_default();
            if let Some(path) =
                folder_dialog("Choose Extracted Super Mario Sunshine Folder", current)
            {
                if let Some(draft) = &mut self.new_project_draft {
                    draft.base_game_root = path.to_string_lossy().into_owned();
                }
            }
        }
        if choose_descriptor {
            let draft = self
                .new_project_draft
                .as_ref()
                .expect("new-project dialog requires a draft");
            let suggested_name = format!("{}.sms", file_name_slug(&draft.name));
            let mut dialog = project_save_dialog().set_file_name(suggested_name);
            if let Some(parent) = existing_dialog_directory(&draft.descriptor_path) {
                dialog = dialog.set_directory(parent);
            }
            if let Some(path) = dialog.save_file() {
                if let Some(draft) = &mut self.new_project_draft {
                    draft.descriptor_path = with_sms_extension(path).to_string_lossy().into_owned();
                }
            }
        }
        if create {
            self.create_project_from_draft();
        } else if cancel {
            self.new_project_draft = None;
        }
    }

    pub(super) fn request_project_hub(&mut self) {
        if self.background_receiver.is_some() {
            self.log.push(
                "Wait for the current background operation before closing the project.".to_string(),
            );
            return;
        }
        if self.is_dirty() {
            self.pending_project_hub = true;
        } else {
            self.enter_project_hub();
        }
    }

    pub(super) fn choose_schema_source_root(&mut self) {
        if let Some(path) = folder_dialog("Choose SMS Decompilation Repository", &self.repo_root) {
            self.repo_root = path.to_string_lossy().into_owned();
            self.persist_project_settings(false);
        }
    }

    pub(super) fn choose_base_game_root(&mut self) {
        if self.document.is_some() || self.background_receiver.is_some() {
            self.log.push(
                "Close the current project stage before changing its extracted game folder."
                    .to_string(),
            );
            return;
        }
        if let Some(path) = folder_dialog(
            "Choose Extracted Super Mario Sunshine Folder",
            &self.base_root,
        ) {
            self.base_root = path.to_string_lossy().into_owned();
            self.last_scanned_base_root.clear();
            self.last_auto_refresh_attempt_root.clear();
            self.pending_auto_refresh_root = Some(self.base_root.clone());
            self.persist_project_settings(false);
        }
    }

    pub(super) fn choose_dolphin_executable(&mut self) {
        let mut dialog = FileDialog::new()
            .set_title("Choose Dolphin Emulator")
            .add_filter("Windows application", &["exe"]);
        if let Some(parent) = existing_dialog_directory(&self.dolphin_path) {
            dialog = dialog.set_directory(parent);
        }
        if let Some(path) = dialog.pick_file() {
            self.dolphin_path = path.to_string_lossy().into_owned();
            self.persist_project_settings(false);
        }
    }

    pub(super) fn choose_game_image(&mut self) {
        let mut dialog = FileDialog::new()
            .set_title("Choose Super Mario Sunshine Game")
            .add_filter("Game image", &["iso", "rvz", "gcm", "wbfs", "dol"]);
        if let Some(parent) = existing_dialog_directory(&self.game_path) {
            dialog = dialog.set_directory(parent);
        }
        if let Some(path) = dialog.pick_file() {
            self.game_path = path.to_string_lossy().into_owned();
            self.persist_project_settings(false);
        }
    }

    pub(super) fn choose_dolphin_user_directory(&mut self) {
        if let Some(path) = folder_dialog("Choose Dolphin User Directory", &self.dolphin_user_dir) {
            self.dolphin_user_dir = path.to_string_lossy().into_owned();
            self.persist_project_settings(false);
        }
    }

    pub(super) fn persist_project_settings(&mut self, announce: bool) -> bool {
        let saved_camera = self.current_project_camera_state();
        let Some(project) = &mut self.current_project else {
            return false;
        };
        if let Some((stage_id, camera)) = saved_camera {
            project.descriptor.stage_cameras.insert(stage_id, camera);
        }
        project.descriptor.name = self.project_name_draft.trim().to_string();
        project.descriptor.base_game_root = PathBuf::from(self.base_root.trim());
        project.descriptor.schema_source_root = optional_path(&self.repo_root);
        project.descriptor.last_stage = optional_string(&self.stage_id);
        project.descriptor.launch = ProjectLaunchConfiguration {
            dolphin_executable: optional_path(&self.dolphin_path),
            game_image: optional_path(&self.game_path),
            dolphin_user_directory: optional_path(&self.dolphin_user_dir),
        };
        match project.save() {
            Ok(()) => {
                self.camera_state_save_pending = false;
                if let Err(error) = self.recent_projects.touch(project) {
                    self.log.push(error);
                }
                if announce {
                    self.log.push(format!(
                        "Saved project descriptor '{}'.",
                        project.descriptor_path.display()
                    ));
                }
                true
            }
            Err(error) => {
                self.log.push(error);
                false
            }
        }
    }

    fn current_project_camera_state(&self) -> Option<(String, ProjectCameraState)> {
        let stage_id = self.stage_id.trim();
        let document = self.document.as_ref()?;
        if stage_id.is_empty() || document.stage_id != stage_id {
            return None;
        }
        let camera = self.renderer.camera();
        let state = ProjectCameraState {
            focus: camera.focus,
            distance: camera.distance,
            yaw_degrees: camera.yaw_degrees,
            pitch_degrees: camera.pitch_degrees,
            viewport_pan: [self.viewport_pan.x, self.viewport_pan.y],
            viewport_zoom: self.viewport_zoom,
            camera_speed: self.camera_speed,
        };
        state.is_valid().then(|| (stage_id.to_string(), state))
    }

    pub(super) fn restore_project_camera_state(&mut self) -> bool {
        let stage_id = self.stage_id.trim();
        let Some(state) = self
            .current_project
            .as_ref()
            .and_then(|project| project.descriptor.stage_cameras.get(stage_id))
            .cloned()
        else {
            return false;
        };
        if !state.is_valid() {
            self.log.push(format!(
                "Ignored invalid saved camera for stage '{stage_id}'."
            ));
            return false;
        }
        let camera = self.renderer.camera_mut();
        camera.focus = state.focus;
        camera.distance = state.distance.max(50.0);
        camera.yaw_degrees = state.yaw_degrees;
        camera.pitch_degrees = state.pitch_degrees.clamp(-89.0, 89.0);
        self.viewport_pan = egui::vec2(state.viewport_pan[0], state.viewport_pan[1]);
        self.viewport_zoom = state.viewport_zoom.clamp(0.05, 20.0);
        self.camera_speed = state.camera_speed.clamp(0.01, 8.0);
        self.camera_state_save_pending = false;
        self.log
            .push(format!("Restored camera for stage '{stage_id}'."));
        true
    }

    pub(super) fn queue_camera_state_save(&mut self) {
        if self.current_project.is_some() && self.document.is_some() {
            self.camera_state_save_pending = true;
            self.camera_state_changed_at = Instant::now();
        }
    }

    pub(super) fn persist_camera_state_if_due(&mut self) {
        if self.camera_state_save_pending
            && self.camera_state_changed_at.elapsed() >= Duration::from_millis(750)
            && !self.persist_project_settings(false)
        {
            self.camera_state_changed_at = Instant::now();
        }
    }

    pub(super) fn flush_camera_state(&mut self) {
        if self.camera_state_save_pending {
            self.persist_project_settings(false);
        }
    }

    pub(super) fn adopt_resolved_project_data_root(&mut self) {
        let Some(project) = &mut self.current_project else {
            return;
        };
        let resolved = PathBuf::from(self.project_root.trim());
        if !paths_refer_to_same_location(&project.data_root(), &resolved) {
            project.descriptor.project_data_root = resolved;
        }
        self.persist_project_settings(false);
    }

    fn create_project_from_draft(&mut self) {
        let Some(draft) = self.new_project_draft.clone() else {
            return;
        };
        if !draft.can_create() {
            self.project_hub_error = Some(
                "Choose a project name, an existing extracted game folder, and a .sms destination."
                    .to_string(),
            );
            return;
        }
        let descriptor_path = with_sms_extension(PathBuf::from(draft.descriptor_path.trim()));
        let base_game_root = match std::fs::canonicalize(draft.base_game_root.trim()) {
            Ok(path) => path,
            Err(error) => {
                self.project_hub_error = Some(format!(
                    "Could not open extracted game folder '{}': {error}",
                    draft.base_game_root
                ));
                return;
            }
        };
        let descriptor = SmsProjectFile::new(
            draft.name.trim(),
            base_game_root,
            default_project_data_root(&descriptor_path),
            optional_path(&self.repo_root),
        );
        if let Err(error) = descriptor.save(&descriptor_path) {
            self.project_hub_error = Some(error);
            return;
        }
        self.new_project_draft = None;
        self.open_project_descriptor(descriptor_path);
    }

    fn open_project_descriptor(&mut self, path: PathBuf) {
        match OpenProject::load(path) {
            Ok(project) => self.activate_project(project),
            Err(error) => self.project_hub_error = Some(error),
        }
    }

    fn activate_project(&mut self, project: OpenProject) {
        self.base_root = project
            .descriptor
            .base_game_root
            .to_string_lossy()
            .into_owned();
        self.project_root = project.data_root().to_string_lossy().into_owned();
        self.repo_root = project
            .descriptor
            .schema_source_root
            .as_ref()
            .map(|path| path.to_string_lossy().into_owned())
            .unwrap_or_else(default_repo_root);
        self.stage_id = project.descriptor.last_stage.clone().unwrap_or_default();
        self.dolphin_path = display_optional_path(&project.descriptor.launch.dolphin_executable);
        self.game_path = display_optional_path(&project.descriptor.launch.game_image);
        self.dolphin_user_dir =
            display_optional_path(&project.descriptor.launch.dolphin_user_directory);
        self.project_name_draft = project.descriptor.name.clone();
        self.current_project = Some(project);
        if let Some(project) = &self.current_project {
            if let Err(error) = self.recent_projects.touch(project) {
                self.log.push(error);
            }
        }
        self.project_hub_error = None;
        self.show_project_hub = false;
        self.reset_open_stage_state();
        self.force_refresh_model_catalog();
        self.pending_auto_refresh_root = Some(self.base_root.clone());
        self.log
            .push(format!("Opened project '{}'.", self.project_name_draft));
    }

    fn import_legacy_project_with_dialogs(&mut self) {
        let Some(legacy_root) = FileDialog::new()
            .set_title("Choose Legacy Graffito-Editor Project Folder")
            .pick_folder()
        else {
            return;
        };
        if !legacy_root.join("sms-project.toml").is_file() {
            self.project_hub_error = Some(format!(
                "The selected folder has no sms-project.toml: {}",
                legacy_root.display()
            ));
            return;
        }
        let suggested = legacy_root
            .file_name()
            .and_then(|name| name.to_str())
            .map(file_name_slug)
            .unwrap_or_else(|| "Imported Project".to_string());
        let mut dialog = project_save_dialog().set_file_name(format!("{suggested}.sms"));
        if let Some(parent) = legacy_root.parent() {
            dialog = dialog.set_directory(parent);
        }
        let Some(descriptor_path) = dialog.save_file() else {
            return;
        };
        match import_legacy_project(
            &legacy_root,
            &with_sms_extension(descriptor_path),
            optional_path(&self.repo_root),
        ) {
            Ok(project) => self.activate_project(project),
            Err(error) => self.project_hub_error = Some(error),
        }
    }

    pub(super) fn enter_project_hub(&mut self) {
        self.persist_project_settings(false);
        self.current_project = None;
        self.show_project_hub = true;
        self.project_hub_error = None;
        self.new_project_draft = None;
        self.pending_project_hub = false;
        self.reset_open_stage_state();
    }

    fn reset_open_stage_state(&mut self) {
        self.cancel_model_import();
        self.new_stage_draft = None;
        self.document = None;
        self.render_scene = None;
        self.scene_archives.clear();
        self.scene_labels.clear();
        self.retail_skyboxes.clear();
        self.retail_goop_templates.clear();
        self.goop_templates_indexed = false;
        self.retail_music.clear();
        self.retail_sounds.clear();
        self.retail_dialogue_voices.clear();
        self.retail_stage_audio.clear();
        self.install_object_authoring_catalog_cache(None);
        self.model_preview = None;
        self.authored_model_preview_base = None;
        self.gpu_viewport = None;
        self.model_framebuffer = None;
        self.model_framebuffer_key = None;
        self.issues.clear();
        self.selected_object_id = None;
        self.model_catalog_root = None;
        self.model_catalog_entries.clear();
        self.model_catalog_issues.clear();
        self.model_asset_preview_cache.clear();
        self.selected_model_asset = None;
        self.selected_model_document = None;
        self.saved_model_document = None;
        self.selected_model_instance_id = None;
        self.model_instances.clear();
        self.model_instances_dirty = false;
        self.model_instance_undo_stack.clear();
        self.model_instance_redo_stack.clear();
        self.active_placement = None;
        self.asset_dirty = false;
        self.asset_undo_stack.clear();
        self.asset_redo_stack.clear();
        self.saved_objects.clear();
        self.saved_lighting = StageLighting::default();
        self.document_dirty = false;
        self.undo_stack.clear();
        self.redo_stack.clear();
        self.undo_transaction = None;
        self.pending_stage_open = None;
        self.camera_state_save_pending = false;
        self.hovered_gizmo_axis = None;
        self.gizmo_drag = None;
        self.last_scanned_base_root.clear();
        self.last_auto_refresh_attempt_root.clear();
    }
}

pub(super) fn path_display_row(
    ui: &mut egui::Ui,
    label: &str,
    value: &str,
    button_label: &str,
    enabled: bool,
) -> bool {
    let mut clicked = false;
    ui.horizontal(|ui| {
        ui.label(label);
        let display = if value.trim().is_empty() {
            "Not selected"
        } else {
            value.trim()
        };
        ui.add(
            egui::TextEdit::singleline(&mut display.to_string())
                .desired_width(ui.available_width().max(180.0) - 92.0)
                .interactive(false),
        )
        .on_hover_text(display);
        if ui
            .add_enabled(enabled, egui::Button::new(button_label))
            .clicked()
        {
            clicked = true;
        }
    });
    clicked
}

fn project_open_dialog() -> FileDialog {
    FileDialog::new()
        .set_title("Open Graffito-Editor Project")
        .add_filter("Graffito-Editor project", &[SMS_PROJECT_EXTENSION])
}

fn project_save_dialog() -> FileDialog {
    FileDialog::new()
        .set_title("Choose Graffito-Editor Project File")
        .add_filter("Graffito-Editor project", &[SMS_PROJECT_EXTENSION])
}

fn folder_dialog(title: &str, current: &str) -> Option<PathBuf> {
    let mut dialog = FileDialog::new().set_title(title);
    if let Some(directory) = existing_dialog_directory(current) {
        dialog = dialog.set_directory(directory);
    }
    dialog.pick_folder()
}

fn existing_dialog_directory(path: &str) -> Option<PathBuf> {
    let path = PathBuf::from(path.trim());
    if path.is_dir() {
        Some(path)
    } else {
        path.parent()
            .filter(|parent| parent.is_dir())
            .map(Path::to_path_buf)
    }
}

fn optional_path(value: &str) -> Option<PathBuf> {
    (!value.trim().is_empty()).then(|| PathBuf::from(value.trim()))
}

fn optional_string(value: &str) -> Option<String> {
    (!value.trim().is_empty()).then(|| value.trim().to_string())
}

fn display_optional_path(path: &Option<PathBuf>) -> String {
    path.as_ref()
        .map(|path| path.to_string_lossy().into_owned())
        .unwrap_or_default()
}

fn file_name_slug(name: &str) -> String {
    let slug = name
        .trim()
        .chars()
        .map(|character| {
            if matches!(
                character,
                '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*'
            ) {
                '-'
            } else {
                character
            }
        })
        .collect::<String>();
    let slug = slug.trim_matches([' ', '.']);
    if slug.is_empty() {
        "New SMS Project".to_string()
    } else {
        slug.to_string()
    }
}

fn paths_refer_to_same_location(left: &Path, right: &Path) -> bool {
    let left = std::fs::canonicalize(left).unwrap_or_else(|_| left.to_path_buf());
    let right = std::fs::canonicalize(right).unwrap_or_else(|_| right.to_path_buf());
    #[cfg(windows)]
    {
        left.to_string_lossy()
            .eq_ignore_ascii_case(&right.to_string_lossy())
    }
    #[cfg(not(windows))]
    {
        left == right
    }
}
