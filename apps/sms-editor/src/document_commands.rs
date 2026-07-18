use super::*;

impl ObjectUndoRecord {
    #[cfg(test)]
    fn between(before: &[SceneObject], after: &[SceneObject]) -> Self {
        let before_by_id = before
            .iter()
            .enumerate()
            .map(|(index, object)| (object.id.as_str(), (index, object)))
            .collect::<BTreeMap<_, _>>();
        let after_by_id = after
            .iter()
            .enumerate()
            .map(|(index, object)| (object.id.as_str(), (index, object)))
            .collect::<BTreeMap<_, _>>();
        let mut deltas = Vec::new();
        for (id, (index, object)) in &before_by_id {
            match after_by_id.get(id) {
                None => deltas.push(ObjectDelta::Remove {
                    index: *index,
                    object: (*object).clone(),
                }),
                Some((_, after)) if *object != *after => deltas.push(ObjectDelta::Update {
                    before: Box::new((*object).clone()),
                    after: Box::new((*after).clone()),
                }),
                Some(_) => {}
            }
        }
        for (id, (index, object)) in after_by_id {
            if !before_by_id.contains_key(id) {
                deltas.push(ObjectDelta::Insert {
                    index,
                    object: object.clone(),
                });
            }
        }
        Self { deltas }
    }

    fn apply_forward(&self, objects: &mut Vec<SceneObject>) {
        let mut removals = self
            .deltas
            .iter()
            .filter_map(|delta| match delta {
                ObjectDelta::Remove { index, object } => Some((*index, object)),
                _ => None,
            })
            .collect::<Vec<_>>();
        removals.sort_by_key(|(index, _)| std::cmp::Reverse(*index));
        for (index, object) in removals {
            remove_object_delta(objects, index, &object.id);
        }
        for delta in &self.deltas {
            if let ObjectDelta::Update { before, after } = delta {
                replace_object_delta(objects, &before.id, after.as_ref().clone());
            }
        }
        let mut inserts = self
            .deltas
            .iter()
            .filter_map(|delta| match delta {
                ObjectDelta::Insert { index, object } => Some((*index, object)),
                _ => None,
            })
            .collect::<Vec<_>>();
        inserts.sort_by_key(|(index, _)| *index);
        for (index, object) in inserts {
            objects.insert(index.min(objects.len()), object.clone());
        }
    }

    fn apply_reverse(&self, objects: &mut Vec<SceneObject>) {
        let mut inserted = self
            .deltas
            .iter()
            .filter_map(|delta| match delta {
                ObjectDelta::Insert { index, object } => Some((*index, object)),
                _ => None,
            })
            .collect::<Vec<_>>();
        inserted.sort_by_key(|(index, _)| std::cmp::Reverse(*index));
        for (index, object) in inserted {
            remove_object_delta(objects, index, &object.id);
        }
        for delta in &self.deltas {
            if let ObjectDelta::Update { before, after } = delta {
                replace_object_delta(objects, &after.id, before.as_ref().clone());
            }
        }
        let mut removed = self
            .deltas
            .iter()
            .filter_map(|delta| match delta {
                ObjectDelta::Remove { index, object } => Some((*index, object)),
                _ => None,
            })
            .collect::<Vec<_>>();
        removed.sort_by_key(|(index, _)| *index);
        for (index, object) in removed {
            objects.insert(index.min(objects.len()), object.clone());
        }
    }
}

fn remove_object_delta(objects: &mut Vec<SceneObject>, expected_index: usize, id: &str) {
    let index = objects
        .get(expected_index)
        .filter(|object| object.id == id)
        .map(|_| expected_index)
        .or_else(|| objects.iter().position(|object| object.id == id));
    if let Some(index) = index {
        objects.remove(index);
    }
}

fn replace_object_delta(objects: &mut [SceneObject], id: &str, replacement: SceneObject) {
    if let Some(object) = objects.iter_mut().find(|object| object.id == id) {
        *object = replacement;
    }
}

impl SmsEditorApp {
    pub(super) fn validate(&mut self) {
        if let Some(document) = &self.document {
            self.issues = validation_issues_for_preview(document, self.model_preview.as_ref());
            self.log.push(format!(
                "Validation produced {} issue(s).",
                self.issues.len()
            ));
            for issue in &self.issues {
                self.log.push(format!(
                    "{:?} [{}] {}",
                    issue.severity, issue.code, issue.message
                ));
            }
        } else {
            self.log.push("No stage open.".to_string());
        }
    }

    pub(super) fn save_project(&mut self) -> bool {
        let had_selected_model_asset = self.selected_model_asset.is_some();
        if let Some(document) = &self.document {
            if !document_uses_selected_base(document, self.base_root.trim()) {
                self.log.push(format!(
                    "Project save blocked: the open stage belongs to '{}', but Base Game Root is '{}'. Open a stage from the selected base before saving its project.",
                    document.base_root.display(),
                    self.base_root.trim()
                ));
                return false;
            }
        }
        if let Some(document) = &self.document {
            self.issues = document.validate();
            if self
                .issues
                .iter()
                .any(|issue| issue.severity == ValidationSeverity::Error)
            {
                self.log
                    .push("Project save blocked by validation errors.".to_string());
                return false;
            }
        }
        // Validate the stage and base ownership before committing any sibling
        // model/instance files. This keeps a known-invalid stage from causing a
        // partial project save that clears unrelated authoring dirty state.
        if !self.save_selected_model_asset() {
            return false;
        }
        if let Err(error) = self.save_model_instances() {
            self.log.push(format!("Project save failed: {error}"));
            return false;
        }

        let project_root = self.project_root.trim().to_string();
        if project_root.is_empty() {
            self.log.push("Project folder is required.".to_string());
            return false;
        }
        let result = match &mut self.document {
            Some(document) => match document.save_project_folder(PathBuf::from(project_root)) {
                Ok(outcome) => {
                    self.saved_objects = document.objects.clone();
                    self.document_dirty = false;
                    self.log.push(format!(
                        "Saved editor project with {} file(s).",
                        outcome.manifest.changed_files.len()
                    ));
                    for warning in outcome.warnings {
                        self.log.push(format!(
                            "Project save warning (recovery path {}): {}",
                            warning.recovery_path.display(),
                            warning.message
                        ));
                    }
                    true
                }
                Err(err) => {
                    self.log.push(format!("Project save failed: {err}"));
                    false
                }
            },
            None => {
                if had_selected_model_asset {
                    self.log
                        .push("Saved model authoring content (no stage is open).".to_string());
                    true
                } else {
                    self.log.push("No stage open.".to_string());
                    false
                }
            }
        };
        if result && self.current_project.is_some() {
            self.persist_project_settings(false)
        } else {
            result
        }
    }

    pub(super) fn build_game(&mut self) {
        self.start_managed_stage_build(false);
    }

    pub(super) fn build_and_launch(&mut self) {
        self.start_managed_stage_build(true);
    }

    fn start_managed_stage_build(&mut self, launch_after_build: bool) {
        if self.background_receiver.is_some() {
            self.log
                .push("Another background operation is already running.".to_string());
            return;
        }
        if launch_after_build && self.dolphin_path.trim().is_empty() {
            self.log
                .push("Launch in Dolphin requires a Dolphin executable.".to_string());
            return;
        }
        if self.document.is_none() {
            self.log.push("No stage open.".to_string());
            return;
        }
        if self.current_project.is_none() {
            self.log.push(
                "Stage build requires a saved .sms project so the managed game build has a safe owned location."
                    .to_string(),
            );
            return;
        }
        if !self.save_project() {
            self.log
                .push("Stage build stopped because the project could not be saved.".to_string());
            return;
        }
        let Some(document) = self.document.clone() else {
            self.log
                .push("No stage open after project save.".to_string());
            return;
        };
        let Some(project) = self.current_project.clone() else {
            self.log
                .push("Project closed while preparing the game build.".to_string());
            return;
        };
        let model_instances = self
            .model_instances
            .iter()
            .filter(|instance| instance.stage_id.eq_ignore_ascii_case(&document.stage_id))
            .cloned()
            .collect::<Vec<_>>();
        let model_instance_count = model_instances.len();
        let Some(content_root) = self.model_content_root() else {
            self.log
                .push("Stage build blocked: project Content root is unavailable.".to_string());
            return;
        };
        let model_assets = match Self::load_model_asset_snapshot(&content_root, &model_instances) {
            Ok(model_assets) => model_assets,
            Err(error) => {
                self.log.push(format!(
                    "Stage build blocked while snapshotting model assets: {error}"
                ));
                return;
            }
        };

        let (sender, receiver) = mpsc::channel();
        let build_cancel = Arc::new(AtomicBool::new(false));
        let task_cancel = Arc::clone(&build_cancel);
        thread::spawn(move || {
            let result = managed_build::check_cancelled(&task_cancel)
                .and_then(|()| {
                    SmsEditorApp::stage_edits_with_model_instances_from_snapshot_cancellable(
                        &model_assets,
                        &model_instances,
                        &document.archive_edits,
                        document.stage_archive.as_ref(),
                        document.registry.as_ref(),
                        &task_cancel,
                    )
                })
                .and_then(|edits| {
                    managed_build::check_cancelled(&task_cancel)?;
                    document
                        .build_stage_archive_with_edits(&edits)
                        .map_err(|error| error.to_string())
                });
            let result = result.and_then(|archive_bytes| {
                managed_build::check_cancelled(&task_cancel)?;
                managed_build::build_managed_game(&project, &document, &archive_bytes, &task_cancel)
            });
            if launch_after_build {
                let result = result.and_then(|build| {
                    managed_build::prepare_managed_game_launch(build, &task_cancel)
                });
                let _ = sender.send(BackgroundResult::BuildAndRun(result));
            } else {
                let _ = sender.send(BackgroundResult::Build(result));
            }
        });
        self.background_receiver = Some(receiver);
        self.active_build_cancel = Some(build_cancel);
        self.background_label = Some(if launch_after_build {
            "Preparing and launching current scene".to_string()
        } else {
            "Building managed game".to_string()
        });
        self.log.push(format!(
            "Building stage from semantic documents and {} placed model instance(s) into the project's managed game directory{}...",
            model_instance_count,
            if launch_after_build {
                ", then preparing direct scene boot in Dolphin"
            } else {
                ""
            }
        ));
    }

    pub(super) fn cancel_active_build(&mut self) {
        let Some(cancel) = &self.active_build_cancel else {
            return;
        };
        if !cancel.swap(true, Ordering::AcqRel) {
            self.log.push(
                "Cancelling managed game build; the current file operation will finish or stop at its next checked chunk."
                    .to_string(),
            );
        }
    }

    pub(super) fn launch_managed_dolphin(
        &mut self,
        outcome: &managed_build::ManagedGameLaunchOutcome,
    ) {
        if self.dolphin_path.trim().is_empty() {
            self.log.push(
                "Managed game build completed, but Dolphin executable is not configured."
                    .to_string(),
            );
            return;
        }
        if !managed_dolphin_exec_is_directory_main(
            &outcome.run.run_root,
            &outcome.direct_boot.launch_dol,
        ) {
            self.log.push(format!(
                "Refusing managed Dolphin launch because its executable must be the managed directory mount point '{}': got '{}'.",
                outcome.run.run_root.join("sys").join("main.dol").display(),
                outcome.direct_boot.launch_dol.display()
            ));
            return;
        }

        let mut command = Command::new(&self.dolphin_path);
        let configured_user_dir =
            Self::configure_dolphin_user_directory(&mut command, &self.dolphin_user_dir);
        command
            .current_dir(&outcome.run.run_root)
            .arg("-b")
            .arg("-e")
            .arg(&outcome.direct_boot.launch_dol);

        match command.spawn() {
            Ok(_) => self.log.push(format!(
                "Launched Dolphin directly into '{}' (runtime area {}, scenario {}) with managed game '{}' using {}.",
                outcome.direct_boot.target.archive_name,
                outcome.direct_boot.target.area_index,
                outcome.direct_boot.target.scenario_index,
                outcome.direct_boot.launch_dol.display(),
                configured_user_dir
                    .as_ref()
                    .map(|path| format!("configured Dolphin user directory '{}'", path.display()))
                    .unwrap_or_else(|| "Dolphin's normal user profile".to_string())
            )),
            Err(err) => self
                .log
                .push(format!("Failed to launch managed Dolphin build: {err}")),
        }
    }

    pub(super) fn launch_dolphin(&mut self) {
        if self.dolphin_path.trim().is_empty() || self.game_path.trim().is_empty() {
            self.log
                .push("Dolphin executable and game path are required.".to_string());
            return;
        }

        let mut command = Command::new(&self.dolphin_path);
        Self::configure_dolphin_user_directory(&mut command, &self.dolphin_user_dir);
        command.arg("-b").arg("-e").arg(&self.game_path);

        match command.spawn() {
            Ok(_) => self.log.push("Launched Dolphin.".to_string()),
            Err(err) => self.log.push(format!("Failed to launch Dolphin: {err}")),
        }
    }

    pub(super) fn configure_dolphin_user_directory(
        command: &mut Command,
        configured: &str,
    ) -> Option<PathBuf> {
        let configured = configured.trim();
        if configured.is_empty() {
            return None;
        }

        let path = PathBuf::from(configured);
        command.arg("-u").arg(&path);
        Some(path)
    }

    pub(super) fn spawn_object_at(&mut self, factory_name: String, translation: [f32; 3]) {
        let id = format!(
            "{}-obj-{:04}",
            sanitize_id(&self.stage_id),
            self.next_object_serial
        );
        self.next_object_serial += 1;

        let mut object = SceneObject::new(id.clone(), factory_name.clone());
        object.transform.translation = translation;
        if let Some(schema) = self
            .registry
            .as_ref()
            .and_then(|registry| registry.find_object(&factory_name))
        {
            object.class_name = Some(schema.class_name.clone());
            if let Some(model) = &schema.preview_model {
                object.asset_hints.push(AssetRef {
                    path: model.clone(),
                    role: AssetRole::PreviewModel,
                });
            }
        }

        let index = self
            .document
            .as_ref()
            .map_or(0, |document| document.objects.len());
        self.apply_object_edit(
            "Added object",
            ObjectUndoRecord {
                deltas: vec![ObjectDelta::Insert { index, object }],
            },
        );
        self.selected_object_id = Some(id);
        self.rebuild_model_preview_from_document();
    }

    pub(super) fn duplicate_selected(&mut self) {
        let Some(source) = self.selected_object().cloned() else {
            return;
        };
        let id = format!(
            "{}-obj-{:04}",
            sanitize_id(&self.stage_id),
            self.next_object_serial
        );
        self.next_object_serial += 1;

        let mut clone = source;
        clone.id = id.clone();
        clone.placement = clone
            .placement
            .as_ref()
            .map(|binding| sms_scene::PlacementBinding::CloneOf(binding.address().clone()));
        clone.transform.translation[0] += self.snap_translation.max(25.0);
        clone.transform.translation[2] += self.snap_translation.max(25.0);
        let index = self
            .document
            .as_ref()
            .map_or(0, |document| document.objects.len());
        self.apply_object_edit(
            "Duplicated object",
            ObjectUndoRecord {
                deltas: vec![ObjectDelta::Insert {
                    index,
                    object: clone,
                }],
            },
        );
        self.selected_object_id = Some(id);
        self.rebuild_model_preview_from_document();
    }

    pub(super) fn delete_selected(&mut self) {
        if self.delete_selected_model_instance() {
            return;
        }
        let Some(selected_id) = self.selected_object_id.clone() else {
            return;
        };
        let Some((index, object)) = self.document.as_ref().and_then(|document| {
            document
                .objects
                .iter()
                .enumerate()
                .find(|(_, object)| object.id == selected_id)
                .map(|(index, object)| (index, object.clone()))
        }) else {
            return;
        };
        self.apply_object_edit(
            "Deleted object",
            ObjectUndoRecord {
                deltas: vec![ObjectDelta::Remove { index, object }],
            },
        );
        self.selected_object_id = None;
        self.rebuild_model_preview_from_document();
    }

    pub(super) fn update_selected_transform(&mut self, transform: Transform) {
        let Some(selected_id) = self.selected_object_id.clone() else {
            return;
        };
        let Some(old_transform) = self.selected_object().map(|object| object.transform) else {
            return;
        };
        if old_transform == transform {
            return;
        }
        let Some(before) = self.selected_object().cloned() else {
            return;
        };
        let mut after = before.clone();
        after.transform = transform;
        self.apply_object_edit(
            "Updated transform",
            ObjectUndoRecord {
                deltas: vec![ObjectDelta::Update {
                    before: Box::new(before),
                    after: Box::new(after),
                }],
            },
        );
        let has_rendered_model = self
            .model_preview
            .as_ref()
            .is_some_and(|preview| preview.object_model_indices.contains_key(&selected_id));
        if !has_rendered_model {
            return;
        }
        if let Some(triangle_ranges) =
            self.update_object_preview_transform(&selected_id, old_transform, transform)
        {
            if let (Some(gpu_viewport), Some(preview)) =
                (self.gpu_viewport.as_ref(), self.model_preview.as_ref())
            {
                gpu_viewport.update_geometry(preview, &triangle_ranges);
            }
            self.clear_viewport_preview_cache();
        } else {
            self.rebuild_model_preview_from_document();
        }
    }

    #[cfg(test)]
    pub(super) fn mutate_document(&mut self, label: &str, mutate: impl FnOnce(&mut StageDocument)) {
        let in_transaction = self.undo_transaction.is_some();
        let before = if in_transaction {
            None
        } else {
            self.document
                .as_ref()
                .map(|document| document.objects.clone())
        };
        if let Some(document) = &mut self.document {
            mutate(document);
        }
        let undo_record = if in_transaction {
            None
        } else {
            before
                .as_ref()
                .zip(self.document.as_ref())
                .map(|(before, document)| ObjectUndoRecord::between(before, &document.objects))
        };
        if let Some(record) = undo_record {
            self.push_undo_record(record);
        }
        self.document_dirty = if in_transaction {
            true
        } else {
            self.document
                .as_ref()
                .is_some_and(|document| document.objects != self.saved_objects)
        };
        if !in_transaction {
            self.flush_document_change();
            self.log.push(format!("{label}."));
        }
    }

    fn apply_object_edit(&mut self, label: &str, record: ObjectUndoRecord) {
        if record.deltas.is_empty() {
            return;
        }
        let in_transaction = self.undo_transaction.is_some();
        let Some(document) = &mut self.document else {
            return;
        };
        record.apply_forward(&mut document.objects);
        self.document_dirty = if in_transaction {
            true
        } else {
            document.objects != self.saved_objects
        };
        if !in_transaction {
            self.push_undo_record(record);
            self.flush_document_change();
            self.log.push(format!("{label}."));
        }
    }

    fn flush_document_change(&mut self) {
        let Some(document) = &mut self.document else {
            return;
        };
        if let Err(err) = document.queue_editor_overlay_change() {
            self.log.push(format!("Scene overlay update failed: {err}"));
        }
        self.issues = validation_issues_for_preview(document, self.model_preview.as_ref());
    }

    pub(super) fn begin_undo_transaction(&mut self) {
        if self.undo_transaction.is_none() {
            self.undo_transaction = self.selected_object_id.as_ref().and_then(|selected_id| {
                self.document.as_ref().and_then(|document| {
                    document
                        .objects
                        .iter()
                        .enumerate()
                        .find(|(_, object)| &object.id == selected_id)
                        .map(|(index, object)| ObjectUndoTransaction {
                            index,
                            before: object.clone(),
                        })
                })
            });
        }
    }

    pub(super) fn commit_undo_transaction(&mut self, label: &str) {
        let Some(transaction) = self.undo_transaction.take() else {
            return;
        };
        if let Some(preview) = &mut self.model_preview {
            recompute_model_preview_bounds(preview);
        }
        let record = self.document.as_ref().map(|document| {
            if let Some(after) = document
                .objects
                .iter()
                .find(|object| object.id == transaction.before.id)
            {
                ObjectUndoRecord {
                    deltas: (after != &transaction.before)
                        .then(|| ObjectDelta::Update {
                            before: Box::new(transaction.before.clone()),
                            after: Box::new(after.clone()),
                        })
                        .into_iter()
                        .collect(),
                }
            } else {
                ObjectUndoRecord {
                    deltas: vec![ObjectDelta::Remove {
                        index: transaction.index,
                        object: transaction.before.clone(),
                    }],
                }
            }
        });
        self.document_dirty = self
            .document
            .as_ref()
            .is_some_and(|document| document.objects != self.saved_objects);
        let Some(record) = record.filter(|record| !record.deltas.is_empty()) else {
            return;
        };
        self.push_undo_record(record);
        self.flush_document_change();
        self.log.push(format!("{label}."));
    }

    fn push_undo_record(&mut self, record: ObjectUndoRecord) {
        if record.deltas.is_empty() {
            return;
        }
        self.undo_stack.push_back(record);
        if self.undo_stack.len() > 80 {
            self.undo_stack.pop_front();
        }
        self.redo_stack.clear();
    }

    pub(super) fn undo(&mut self) {
        if (self.selected_model_instance_id.is_some()
            || (self.selected_object_id.is_none()
                && self.selected_model_document.is_none()
                && !self.model_instance_undo_stack.is_empty()))
            && self.undo_model_instance()
        {
            return;
        }
        if self.selected_model_document.is_some() && self.undo_model_asset() {
            return;
        }
        if self.document.is_none() {
            return;
        }
        let Some(record) = self.undo_stack.pop_back() else {
            return;
        };
        if let Some(document) = &mut self.document {
            record.apply_reverse(&mut document.objects);
            self.document_dirty = document.objects != self.saved_objects;
        }
        self.redo_stack.push_back(record);
        self.flush_document_change();
        self.ensure_selection_exists();
        self.rebuild_model_preview_from_document();
        self.log.push("Undo.".to_string());
    }

    pub(super) fn redo(&mut self) {
        if (self.selected_model_instance_id.is_some()
            || (self.selected_object_id.is_none()
                && self.selected_model_document.is_none()
                && !self.model_instance_redo_stack.is_empty()))
            && self.redo_model_instance()
        {
            return;
        }
        if self.selected_model_document.is_some() && self.redo_model_asset() {
            return;
        }
        if self.document.is_none() {
            return;
        }
        let Some(record) = self.redo_stack.pop_back() else {
            return;
        };
        if let Some(document) = &mut self.document {
            record.apply_forward(&mut document.objects);
            self.document_dirty = document.objects != self.saved_objects;
        }
        self.undo_stack.push_back(record);
        self.flush_document_change();
        self.ensure_selection_exists();
        self.rebuild_model_preview_from_document();
        self.log.push("Redo.".to_string());
    }

    pub(super) fn is_dirty(&self) -> bool {
        self.document_dirty || self.asset_dirty || self.model_instances_dirty
    }

    pub(super) fn unsaved_changes_dialog(&mut self, ctx: &egui::Context) {
        if self.pending_stage_open.is_none()
            && !self.close_confirmation_requested
            && !self.pending_project_hub
        {
            return;
        }

        let mut action = None;
        egui::Window::new("Unsaved changes")
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
            .show(ctx, |ui| {
                ui.label("The current stage or model asset has changes that have not been saved.");
                ui.label(if self.pending_project_hub {
                    "Save the project before returning to the project hub?"
                } else {
                    "Save the editor project before continuing?"
                });
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if ui.button("Save and Continue").clicked() {
                        action = Some(0);
                    }
                    if ui.button("Discard").clicked() {
                        action = Some(1);
                    }
                    if ui.button("Cancel").clicked() {
                        action = Some(2);
                    }
                });
            });

        match action {
            Some(0) if self.save_project() => self.finish_pending_navigation(ctx),
            Some(1) => self.finish_pending_navigation(ctx),
            Some(2) => {
                self.pending_stage_open = None;
                self.close_confirmation_requested = false;
                self.pending_project_hub = false;
            }
            _ => {}
        }
    }

    pub(super) fn finish_pending_navigation(&mut self, ctx: &egui::Context) {
        if self.pending_project_hub {
            self.enter_project_hub();
            return;
        }
        if self.close_confirmation_requested {
            self.close_confirmation_requested = false;
            self.close_authorized = true;
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            return;
        }
        if let Some(stage_id) = self.pending_stage_open.take() {
            self.stage_id = stage_id;
            self.open_stage();
        }
    }

    pub(super) fn selected_object(&self) -> Option<&SceneObject> {
        let selected_id = self.selected_object_id.as_ref()?;
        self.document
            .as_ref()?
            .objects
            .iter()
            .find(|object| &object.id == selected_id)
    }

    pub(super) fn clear_viewport_preview_cache(&mut self) {
        self.model_framebuffer = None;
        self.model_framebuffer_key = None;
    }

    pub(super) fn rebuild_model_preview_from_document(&mut self) {
        let visibility = self.preview_visibility();
        let (render_scene, model_preview) =
            self.document.as_ref().map_or((None, None), |document| {
                (
                    Some(RenderScene::from_document(document)),
                    SmsEditorApp::build_model_preview(document, visibility),
                )
            });
        self.render_scene = render_scene;
        self.reset_authored_model_preview_base();
        self.model_preview = model_preview;
        if let Some(document) = &self.document {
            self.issues = validation_issues_for_preview(document, self.model_preview.as_ref());
        }
        self.last_level_transform_progress_bits = u32::MAX;
        self.rebuild_gpu_viewport_scene();
        self.clear_viewport_preview_cache();
    }

    pub(super) fn update_object_preview_transform(
        &mut self,
        object_id: &str,
        old_transform: Transform,
        new_transform: Transform,
    ) -> Option<Vec<std::ops::Range<usize>>> {
        let defer_full_bounds_recompute = self.undo_transaction.is_some();
        if !transform_has_invertible_scale(old_transform) {
            return None;
        }
        let preview = self.model_preview.as_mut()?;
        let model_index = preview.object_model_indices.get(object_id).copied()?;
        let triangle_ranges = preview_triangle_ranges_for_model_index(preview, model_index);

        let (old_preview_transform, new_preview_transform) = self
            .document
            .as_ref()
            .and_then(|document| {
                let object = document
                    .objects
                    .iter()
                    .find(|object| object.id == object_id)?;
                Some((
                    object,
                    document.registry.as_ref(),
                    document.actor_preview(object),
                ))
            })
            .map(|(object, registry, actor_preview)| {
                (
                    actor_runtime_preview_transform(
                        reset_fruit_preview_transform(object, old_transform, registry),
                        actor_preview,
                    ),
                    actor_runtime_preview_transform(
                        reset_fruit_preview_transform(object, new_transform, registry),
                        actor_preview,
                    ),
                )
            })
            .unwrap_or((old_transform, new_transform));
        preview
            .mirror_actor_positions
            .insert(model_index, new_preview_transform.translation);

        for model in &mut preview.animated_models {
            if let Some(instance) = model
                .instances
                .iter_mut()
                .find(|instance| instance.model_index == model_index)
            {
                instance.transform = new_preview_transform;
            }
        }
        for model in &mut preview.rotating_models {
            if let Some(instance) = model
                .instances
                .iter_mut()
                .find(|instance| instance.model_index == model_index)
            {
                instance.transform = new_preview_transform;
            }
        }
        for particles in &mut preview.actor_particles {
            if particles.model_index == Some(model_index) {
                particles.origin_offset = retransform_preview_point(
                    particles.origin_offset,
                    old_preview_transform,
                    new_preview_transform,
                );
            }
        }

        let mut changed = false;
        for point in &mut preview.points {
            if point.model_index == model_index {
                point.position = retransform_preview_point(
                    point.position,
                    old_preview_transform,
                    new_preview_transform,
                );
                changed = true;
            }
        }
        for range in &triangle_ranges {
            for triangle in &mut preview.triangles[range.clone()] {
                triangle.vertices = triangle.vertices.map(|vertex| {
                    retransform_preview_point(vertex, old_preview_transform, new_preview_transform)
                });
                let normals = if matches!(
                    triangle.render_layer,
                    PreviewRenderLayer::Particle | PreviewRenderLayer::ParticleDistortion
                ) {
                    triangle.normals
                } else {
                    triangle.normals.map(|normals| {
                        normals.map(|normal| {
                            retransform_preview_normal(
                                normal,
                                old_preview_transform,
                                new_preview_transform,
                            )
                        })
                    })
                };
                triangle.billboard = triangle.billboard.and_then(|billboard| {
                    retransform_j3d_billboard(
                        billboard,
                        old_preview_transform,
                        new_preview_transform,
                        normals,
                    )
                });
                triangle.normals = normals;
                changed = true;
            }
        }
        if changed {
            if defer_full_bounds_recompute {
                expand_model_preview_bounds(preview, model_index, &triangle_ranges);
            } else {
                recompute_model_preview_bounds(preview);
            }
        }
        changed.then_some(triangle_ranges)
    }

    pub(super) fn rebuild_gpu_viewport_scene(&mut self) {
        self.sync_authored_model_instance_preview();
        let Some(target_format) = self.gpu_target_format else {
            self.gpu_viewport = None;
            return;
        };
        let Some(preview) = self.model_preview.as_ref() else {
            self.gpu_viewport = None;
            return;
        };
        self.gpu_viewport = Some(gpu_viewport::GpuViewportScene::from_preview(
            preview,
            target_format,
        ));
    }

    pub(super) fn ensure_selection_exists(&mut self) {
        let exists = self.selected_object_id.as_ref().is_some_and(|id| {
            self.document
                .as_ref()
                .is_some_and(|document| document.objects.iter().any(|object| &object.id == id))
        });
        if !exists {
            self.selected_object_id = None;
        }
    }

    pub(super) fn default_spawn_position(&self) -> [f32; 3] {
        self.renderer.camera().focus
    }

    pub(super) fn frame_selected(&mut self) {
        self.stop_camera_fly();
        if let Some(object) = self.selected_object() {
            self.renderer.camera_mut().focus = object.transform.translation;
            self.viewport_pan = egui::Vec2::ZERO;
            self.queue_camera_state_save();
        }
    }

    pub(super) fn reset_camera(&mut self) {
        self.stop_camera_fly();
        self.viewport_pan = egui::Vec2::ZERO;
        self.viewport_zoom = 1.0;
        if let Some(preview) = &self.model_preview {
            let camera = self.renderer.camera_mut();
            camera.focus = preview.center();
            camera.yaw_degrees = self.startup_camera_yaw.unwrap_or(222.0);
            camera.pitch_degrees = self.startup_camera_pitch.unwrap_or(-30.0);
            camera.distance = (preview.radius() * 4.2).clamp(2500.0, 600_000.0);
            self.queue_camera_state_save();
            return;
        }

        let camera = self.renderer.camera_mut();
        camera.focus = [0.0, 0.0, 0.0];
        camera.yaw_degrees = self.startup_camera_yaw.unwrap_or(222.0);
        camera.pitch_degrees = self.startup_camera_pitch.unwrap_or(-30.0);
        camera.distance = 7000.0;
        self.queue_camera_state_save();
    }

    pub(super) fn apply_startup_camera_focus(&mut self) {
        self.stop_camera_fly();
        if let Some(focus) = self.startup_camera_focus {
            let camera = self.renderer.camera_mut();
            camera.focus = focus;
            if let Some(distance) = self.startup_camera_distance {
                camera.distance = distance.max(50.0);
            }
            self.viewport_pan = egui::Vec2::ZERO;
            self.viewport_zoom = 1.0;
            self.log.push(format!(
                "Focused startup camera on {:.1}, {:.1}, {:.1}.",
                focus[0], focus[1], focus[2]
            ));
            return;
        }
        let Some(needle) = self.startup_focus_object.as_deref() else {
            return;
        };
        let Some(document) = &self.document else {
            return;
        };
        let needle = needle.trim().to_ascii_lowercase();
        if needle.is_empty() {
            return;
        }
        if let Some(object) = document
            .objects
            .iter()
            .find(|object| object_matches_focus(object, &needle))
        {
            let camera = self.renderer.camera_mut();
            camera.focus = object.transform.translation;
            camera.distance = self.startup_camera_distance.unwrap_or(2200.0).max(50.0);
            self.viewport_pan = egui::Vec2::ZERO;
            self.viewport_zoom = 1.0;
            self.selected_object_id = Some(object.id.clone());
            self.log.push(format!(
                "Focused startup camera on '{}'.",
                object_display_name(object)
            ));
        }
    }
}

pub(super) fn managed_dolphin_exec_is_directory_main(
    run_root: &std::path::Path,
    launch_dol: &std::path::Path,
) -> bool {
    launch_dol == run_root.join("sys").join("main.dol")
}

#[cfg(test)]
pub(super) fn preview_triangle_ranges_for_model(
    preview: &ModelPreview,
    object_id: &str,
) -> Vec<std::ops::Range<usize>> {
    let Some(model_index) = preview.object_model_indices.get(object_id).copied() else {
        return Vec::new();
    };
    preview_triangle_ranges_for_model_index(preview, model_index)
}

fn preview_triangle_ranges_for_model_index(
    preview: &ModelPreview,
    model_index: usize,
) -> Vec<std::ops::Range<usize>> {
    let mut ranges = Vec::new();
    let mut start = None;
    for (index, triangle) in preview.triangles.iter().enumerate() {
        if triangle.model_index == model_index {
            start.get_or_insert(index);
        } else if let Some(start) = start.take() {
            ranges.push(start..index);
        }
    }
    if let Some(start) = start {
        ranges.push(start..preview.triangles.len());
    }
    ranges
}
