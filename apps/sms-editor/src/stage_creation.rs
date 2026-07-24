use super::*;

#[derive(Debug, Clone)]
pub(super) struct NewStageDraft {
    pub(super) stage_id: String,
    pub(super) error: Option<String>,
}

impl NewStageDraft {
    fn for_app(app: &SmsEditorApp) -> Self {
        let stage_id = (0_u32..)
            .map(|index| format!("new_stage{index}"))
            .find(|candidate| !stage_id_exists(&app.scene_archives, candidate))
            .expect("the authored stage id sequence is effectively unbounded");
        Self {
            stage_id,
            error: None,
        }
    }

    fn normalized_stage_id(&self) -> &str {
        self.stage_id.trim()
    }

    fn validation_error(&self, app: &SmsEditorApp) -> Option<String> {
        let stage_id = self.normalized_stage_id();
        if stage_id.is_empty() {
            return Some("Enter a stage ID.".to_string());
        }
        if !stage_id
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || matches!(character, '_' | '-'))
        {
            return Some(
                "Stage IDs may contain only ASCII letters, digits, '_' and '-'.".to_string(),
            );
        }
        if stage_id_exists(&app.scene_archives, stage_id) {
            return Some(format!(
                "Stage ID '{stage_id}' already exists in the retail release or this project."
            ));
        }
        None
    }
}

impl SmsEditorApp {
    pub(super) fn request_new_stage(&mut self) {
        if self.current_project.is_none() {
            self.log
                .push("Create or open a .sms project before creating a stage.".to_string());
            return;
        }
        if self.background_receiver.is_some() {
            self.log
                .push("Wait for the current background operation to finish.".to_string());
            return;
        }
        self.new_stage_draft = Some(NewStageDraft::for_app(self));
    }

    pub(super) fn new_stage_dialog(&mut self, ctx: &egui::Context) {
        let Some(mut draft) = self.new_stage_draft.take() else {
            return;
        };
        let mut open = true;
        let mut create = false;
        let mut cancel = false;

        egui::Window::new("Create New Stage")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .default_width(520.0)
            .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
            .show(ctx, |ui| {
                ui.label(
                    "Create an empty, source-free stage with its own project runtime slot. No retail level or stage mapping is replaced.",
                );
                ui.add_space(8.0);
                egui::Grid::new("new-stage-fields")
                    .num_columns(2)
                    .spacing(egui::vec2(12.0, 8.0))
                    .show(ui, |ui| {
                        ui.label("Stage ID");
                        ui.add(
                            egui::TextEdit::singleline(&mut draft.stage_id)
                                .desired_width(300.0)
                                .hint_text("new_stage0"),
                        );
                        ui.end_row();
                    });

                ui.add_space(8.0);
                ui.small(
                    "The editor allocates the next safe reserved area/scenario entry in the project's stageArc.bin and creates a minimal runtime shell. After creation, drag in the world model and any cataloged typed object class; required manager and resource dependencies are added automatically. Assign a model as Stage skybox, then edit Stage Music and Stage Lighting in the inspector.",
                );
                ui.small("The extracted game remains read-only. The project mapping is installed only in managed builds and releases.");

                let validation_error = draft.validation_error(self);
                if let Some(error) = draft.error.as_ref().or(validation_error.as_ref()) {
                    ui.add_space(6.0);
                    ui.colored_label(egui::Color32::from_rgb(255, 150, 104), error);
                }

                ui.add_space(10.0);
                ui.horizontal(|ui| {
                    if ui
                        .add_enabled(
                            validation_error.is_none(),
                            egui::Button::new("Create Stage"),
                        )
                        .clicked()
                    {
                        create = true;
                    }
                    if ui.button("Cancel").clicked() {
                        cancel = true;
                    }
                });
            });

        if create {
            self.begin_new_stage_creation(draft);
        } else if open && !cancel {
            self.new_stage_draft = Some(draft);
        }
    }

    fn begin_new_stage_creation(&mut self, mut draft: NewStageDraft) {
        if let Some(error) = draft.validation_error(self) {
            draft.error = Some(error);
            self.new_stage_draft = Some(draft);
            return;
        }
        if self.is_dirty() && !self.save_project() {
            draft.error = Some(
                "The current project could not be saved, so stage creation was stopped."
                    .to_string(),
            );
            self.new_stage_draft = Some(draft);
            return;
        }
        let Some(project) = self.current_project.clone() else {
            return;
        };

        let base_root = self.base_root.trim().to_string();
        let repo_root = self.repo_root.trim().to_string();
        let requested_project_root = self.project_root.trim().to_string();
        let stage_id = draft.normalized_stage_id().to_string();
        let background_stage_id = stage_id.clone();
        let mut archives = self.scene_archives.clone();
        let scene_labels = self.scene_labels.clone();
        let retail_skyboxes = self.retail_skyboxes.clone();
        let retail_music = self.retail_music.clone();
        let retail_sounds = self.retail_sounds.clone();
        let retail_dialogue_voices = self.retail_dialogue_voices.clone();
        let retail_stage_audio = self.retail_stage_audio.clone();
        let existing_object_authoring_catalog_cache = self
            .reusable_object_authoring_catalog_cache(Path::new(&base_root), self.registry.as_ref());
        let existing_registry = self.registry.clone();
        let visibility = self.preview_visibility();
        let (sender, receiver) = mpsc::channel();
        thread::spawn(move || {
            let result = (|| -> Result<Box<LoadedStage>, String> {
                let bootstrap_resources = sms_scene::BLANK_STAGE_BOOTSTRAP_REQUIREMENTS
                    .map(
                        |requirement| -> Result<sms_scene::BlankStageBootstrapResource, String> {
                            let proxy =
                                sms_authoring::built_in_blank_stage_proxy(requirement.raw_path);
                            let bytes = match requirement.kind {
                                sms_scene::BlankStageBootstrapKind::Model => {
                                    proxy.compile_bmd().map_err(|error| error.to_string())?
                                }
                                sms_scene::BlankStageBootstrapKind::Collision => proxy
                                    .collision
                                    .as_ref()
                                    .expect("built-in NormalBlock proxy always has collision")
                                    .to_col_bytes()
                                    .map_err(|error| error.to_string())?,
                            };
                            Ok(sms_scene::BlankStageBootstrapResource {
                                raw_path: requirement.raw_path.to_vec(),
                                bytes,
                            })
                        },
                    )
                    .into_iter()
                    .collect::<Result<Vec<_>, _>>()?;
                let bootstrap = sms_scene::BlankStageBootstrapManifest::from_authored_bytes(
                    bootstrap_resources,
                )
                .map_err(|error| error.to_string())?;
                let preset = sms_scene::BlankStagePreset {
                    target_slot: stage_id.clone(),
                    ..sms_scene::BlankStagePreset::default()
                };
                let archive = preset
                    .build(bootstrap)
                    .map_err(|error| format!("Could not build the empty stage shell: {error}"))?;

                let mut document = StageDocument::from_authored_archive(
                    PathBuf::from(&base_root),
                    stage_id.clone(),
                    archive,
                )
                .map_err(|error| error.to_string())?;
                let project_selection =
                    load_project_for_stage(&mut document, &requested_project_root)
                        .map_err(|error| error.to_string())?;

                let stage_table = managed_build::read_effective_runtime_stage_table(&project)?;
                let runtime_slot = sms_formats::append_jdrama_scenario_archive_slot(
                    &stage_table,
                    &format!("{stage_id}.arc"),
                )
                .map_err(|error| format!("Could not allocate a runtime stage slot: {error}"))?;
                document
                    .mark_changed_file("data/stageArc.bin", runtime_slot.bytes)
                    .map_err(|error| error.to_string())?;

                let (registry, schema_warning) = if let Some(registry) = existing_registry {
                    (Some(registry), None)
                } else {
                    match generate_editor_schema(Path::new(&repo_root)) {
                        Ok(registry) => (Some(registry), None),
                        Err(error) => (None, Some(error.to_string())),
                    }
                };
                if let Some(registry) = registry.clone() {
                    document = document.with_registry(registry);
                }
                let object_authoring_catalog_cache = resolve_object_authoring_catalog(
                    Path::new(&base_root),
                    &archives,
                    registry.as_ref(),
                    existing_object_authoring_catalog_cache,
                );
                let (
                    object_authoring_catalog_key,
                    object_authoring_catalog,
                    object_authoring_catalog_warnings,
                ) = split_object_authoring_catalog_cache(object_authoring_catalog_cache);
                document
                    .save_project_folder(&project_selection.project_root)
                    .map_err(|error| format!("Could not save the new stage: {error}"))?;

                insert_authored_scene_archive(&mut archives, &document.base_root, &stage_id);
                let scene = RenderScene::from_document(&document);
                let preview = SmsEditorApp::build_model_preview(&document, visibility);
                Ok(Box::new(LoadedStage {
                    base_root,
                    requested_project_root,
                    project_root: project_selection.project_root,
                    has_scene_index: true,
                    archives,
                    registry,
                    schema_warning,
                    object_authoring_catalog_key,
                    object_authoring_catalog,
                    object_authoring_catalog_warnings,
                    project_warning: project_selection.warning,
                    document,
                    scene,
                    preview,
                    scene_labels,
                    scene_label_warning: None,
                    retail_skyboxes,
                    skybox_warnings: Vec::new(),
                    retail_music,
                    retail_sounds,
                    retail_dialogue_voices,
                    retail_stage_audio,
                    music_warning: None,
                }))
            })();
            let _ = sender.send(BackgroundResult::CreateStage(result));
        });
        self.background_receiver = Some(receiver);
        self.background_label = Some(format!("Creating {background_stage_id}"));
        self.log.push(format!(
            "Creating empty authored stage '{background_stage_id}' and allocating a new project runtime slot. Typed object classes will be available with automatic dependencies..."
        ));
    }
}

fn stage_id_exists(archives: &[SceneArchiveInfo], stage_id: &str) -> bool {
    archives
        .iter()
        .any(|archive| archive.stage_id.eq_ignore_ascii_case(stage_id))
}

pub(super) fn insert_authored_scene_archive(
    archives: &mut Vec<SceneArchiveInfo>,
    base_root: &Path,
    stage_id: &str,
) {
    if stage_id_exists(archives, stage_id) {
        return;
    }
    let relative_path = PathBuf::from("files")
        .join("data")
        .join("scene")
        .join(format!("{stage_id}.szs"));
    let group = stage_id
        .chars()
        .take_while(|character| !character.is_ascii_digit())
        .collect::<String>()
        .trim_end_matches('_')
        .to_string();
    archives.push(SceneArchiveInfo {
        stage_id: stage_id.to_string(),
        group,
        path: base_root.join(&relative_path),
        relative_path,
        size_bytes: 0,
    });
    archives.sort_by(|left, right| {
        left.stage_id
            .to_ascii_lowercase()
            .cmp(&right.stage_id.to_ascii_lowercase())
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_stage_validation_requires_a_unique_portable_runtime_stem() {
        let mut app = SmsEditorApp::default();
        app.scene_archives.push(SceneArchiveInfo {
            stage_id: "retail0".to_string(),
            group: "retail".to_string(),
            relative_path: PathBuf::from("files/data/scene/retail0.szs"),
            path: PathBuf::from("base/files/data/scene/retail0.szs"),
            size_bytes: 1,
        });

        let mut draft = NewStageDraft::for_app(&app);
        draft.stage_id = "Retail0".to_string();
        assert!(draft
            .validation_error(&app)
            .is_some_and(|error| error.contains("already exists")));
        draft.stage_id = "stage.with.dot".to_string();
        assert!(draft
            .validation_error(&app)
            .is_some_and(|error| error.contains("ASCII")));
        draft.stage_id = "authored_stage-1".to_string();
        assert_eq!(draft.validation_error(&app), None);
    }

    #[test]
    fn authored_scene_archive_uses_a_virtual_release_path() {
        let mut archives = Vec::new();
        insert_authored_scene_archive(&mut archives, Path::new("base"), "custom0");
        insert_authored_scene_archive(&mut archives, Path::new("base"), "CUSTOM0");
        assert_eq!(archives.len(), 1);
        assert_eq!(
            archives[0].relative_path,
            PathBuf::from("files/data/scene/custom0.szs")
        );
        assert_eq!(archives[0].size_bytes, 0);
    }
}
