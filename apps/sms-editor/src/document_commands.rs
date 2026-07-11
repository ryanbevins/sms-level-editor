use super::*;

impl SmsEditorApp {
    pub(super) fn validate(&mut self) {
        if let Some(document) = &self.document {
            self.issues = document.validate();
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
        if let Some(document) = &mut self.document {
            if let Err(err) = document.queue_editor_overlay_change() {
                self.log.push(format!("Scene overlay export failed: {err}"));
                return false;
            }
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

        let result = match &self.document {
            Some(document) => match document.save_project_folder(PathBuf::from(&self.project_root))
            {
                Ok(manifest) => {
                    self.saved_objects = document.objects.clone();
                    self.log.push(format!(
                        "Saved editor project with {} file(s).",
                        manifest.changed_files.len()
                    ));
                    true
                }
                Err(err) => {
                    self.log.push(format!("Project save failed: {err}"));
                    false
                }
            },
            None => {
                self.log.push("No stage open.".to_string());
                false
            }
        };
        result
    }

    pub(super) fn launch_dolphin(&mut self) {
        if self.dolphin_path.trim().is_empty() || self.game_path.trim().is_empty() {
            self.log
                .push("Dolphin executable and game path are required.".to_string());
            return;
        }

        let mut command = Command::new(&self.dolphin_path);
        if !self.dolphin_user_dir.trim().is_empty() {
            command.arg("-u").arg(&self.dolphin_user_dir);
        }
        command.arg("-b").arg("-e").arg(&self.game_path);

        match command.spawn() {
            Ok(_) => self.log.push("Launched Dolphin.".to_string()),
            Err(err) => self.log.push(format!("Failed to launch Dolphin: {err}")),
        }
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

        self.mutate_document("Added object", |document| document.add_object(object));
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
        clone.transform.translation[0] += self.snap_translation.max(25.0);
        clone.transform.translation[2] += self.snap_translation.max(25.0);
        self.mutate_document("Duplicated object", |document| document.add_object(clone));
        self.selected_object_id = Some(id);
        self.rebuild_model_preview_from_document();
    }

    pub(super) fn delete_selected(&mut self) {
        let Some(selected_id) = self.selected_object_id.clone() else {
            return;
        };
        self.mutate_document("Deleted object", |document| {
            document.objects.retain(|object| object.id != selected_id);
        });
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
        self.mutate_document("Updated transform", |document| {
            if let Some(object) = document
                .objects
                .iter_mut()
                .find(|object| object.id == selected_id)
            {
                object.transform = transform;
            }
        });
        if self.update_object_preview_transform(&selected_id, old_transform, transform) {
            self.rebuild_gpu_viewport_scene();
            self.clear_viewport_preview_cache();
        } else {
            self.rebuild_model_preview_from_document();
        }
    }

    pub(super) fn nudge_selected(&mut self, delta: [f32; 3]) {
        let Some(mut object) = self.selected_object().cloned() else {
            return;
        };
        for (value, add) in object.transform.translation.iter_mut().zip(delta) {
            *value += add;
        }
        if self.snap_enabled {
            snap_transform(
                &mut object.transform,
                self.snap_translation,
                self.snap_rotation,
                self.snap_scale,
            );
        }
        self.update_selected_transform(object.transform);
    }

    pub(super) fn mutate_document(&mut self, label: &str, mutate: impl FnOnce(&mut StageDocument)) {
        let in_transaction = self.undo_transaction.is_some();
        if !in_transaction {
            self.push_undo_snapshot();
        }
        if let Some(document) = &mut self.document {
            mutate(document);
            if let Err(err) = document.queue_editor_overlay_change() {
                self.log.push(format!("Scene overlay update failed: {err}"));
            }
            self.issues = document.validate();
            if !in_transaction {
                self.log.push(format!("{label}."));
            }
        }
    }

    pub(super) fn begin_undo_transaction(&mut self) {
        if self.undo_transaction.is_none() {
            self.undo_transaction = self
                .document
                .as_ref()
                .map(|document| document.objects.clone());
        }
    }

    pub(super) fn commit_undo_transaction(&mut self, label: &str) {
        let Some(before) = self.undo_transaction.take() else {
            return;
        };
        let changed = self
            .document
            .as_ref()
            .is_some_and(|document| document.objects != before);
        if !changed {
            return;
        }
        self.undo_stack.push(before);
        if self.undo_stack.len() > 80 {
            self.undo_stack.remove(0);
        }
        self.redo_stack.clear();
        self.log.push(format!("{label}."));
    }

    pub(super) fn push_undo_snapshot(&mut self) {
        if let Some(document) = &self.document {
            self.undo_stack.push(document.objects.clone());
            if self.undo_stack.len() > 80 {
                self.undo_stack.remove(0);
            }
            self.redo_stack.clear();
        }
    }

    pub(super) fn undo(&mut self) {
        let Some(document) = &mut self.document else {
            return;
        };
        let Some(previous) = self.undo_stack.pop() else {
            return;
        };
        self.redo_stack.push(document.objects.clone());
        document.objects = previous;
        if let Err(err) = document.queue_editor_overlay_change() {
            self.log.push(format!("Scene overlay update failed: {err}"));
        }
        self.issues = document.validate();
        self.ensure_selection_exists();
        self.rebuild_model_preview_from_document();
        self.log.push("Undo.".to_string());
    }

    pub(super) fn redo(&mut self) {
        let Some(document) = &mut self.document else {
            return;
        };
        let Some(next) = self.redo_stack.pop() else {
            return;
        };
        self.undo_stack.push(document.objects.clone());
        document.objects = next;
        if let Err(err) = document.queue_editor_overlay_change() {
            self.log.push(format!("Scene overlay update failed: {err}"));
        }
        self.issues = document.validate();
        self.ensure_selection_exists();
        self.rebuild_model_preview_from_document();
        self.log.push("Redo.".to_string());
    }

    pub(super) fn can_undo(&self) -> bool {
        !self.undo_stack.is_empty()
    }

    pub(super) fn can_redo(&self) -> bool {
        !self.redo_stack.is_empty()
    }

    pub(super) fn is_dirty(&self) -> bool {
        self.document
            .as_ref()
            .is_some_and(|document| document.objects != self.saved_objects)
    }

    pub(super) fn unsaved_changes_dialog(&mut self, ctx: &egui::Context) {
        if self.pending_stage_open.is_none() && !self.close_confirmation_requested {
            return;
        }

        let mut action = None;
        egui::Window::new("Unsaved changes")
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
            .show(ctx, |ui| {
                ui.label("The current stage has changes that have not been saved.");
                ui.label("Save the editor project before continuing?");
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
            }
            _ => {}
        }
    }

    pub(super) fn finish_pending_navigation(&mut self, ctx: &egui::Context) {
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
        self.model_preview = self.document.as_ref().and_then(|document| {
            SmsEditorApp::build_model_preview(document, self.preview_visibility())
        });
        self.rebuild_gpu_viewport_scene();
        self.clear_viewport_preview_cache();
    }

    pub(super) fn update_object_preview_transform(
        &mut self,
        object_id: &str,
        old_transform: Transform,
        new_transform: Transform,
    ) -> bool {
        if !transform_has_invertible_scale(old_transform) {
            return false;
        }
        let Some(preview) = self.model_preview.as_mut() else {
            return false;
        };
        let Some(model_index) = preview.object_model_indices.get(object_id).copied() else {
            return false;
        };

        let (old_preview_transform, new_preview_transform) = self
            .document
            .as_ref()
            .and_then(|document| {
                document
                    .objects
                    .iter()
                    .find(|object| object.id == object_id)
            })
            .map(|object| {
                (
                    reset_fruit_preview_transform(object, old_transform),
                    reset_fruit_preview_transform(object, new_transform),
                )
            })
            .unwrap_or((old_transform, new_transform));

        for model in &mut preview.animated_models {
            if let Some(instance) = model
                .instances
                .iter_mut()
                .find(|instance| instance.model_index == model_index)
            {
                instance.transform = new_preview_transform;
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
        for triangle in &mut preview.triangles {
            if triangle.model_index == model_index {
                triangle.vertices = triangle.vertices.map(|vertex| {
                    retransform_preview_point(vertex, old_preview_transform, new_preview_transform)
                });
                triangle.normals = triangle.normals.map(|normals| {
                    normals.map(|normal| {
                        retransform_preview_normal(
                            normal,
                            old_preview_transform,
                            new_preview_transform,
                        )
                    })
                });
                changed = true;
            }
        }
        if changed {
            recompute_model_preview_bounds(preview);
        }
        changed
    }

    pub(super) fn rebuild_gpu_viewport_scene(&mut self) {
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
        if let Some(object) = self.selected_object() {
            self.renderer.camera_mut().focus = object.transform.translation;
            self.viewport_pan = egui::Vec2::ZERO;
        }
    }

    pub(super) fn reset_camera(&mut self) {
        self.viewport_pan = egui::Vec2::ZERO;
        self.viewport_zoom = 1.0;
        if let Some(preview) = &self.model_preview {
            let camera = self.renderer.camera_mut();
            camera.focus = preview.center();
            camera.yaw_degrees = self.startup_camera_yaw.unwrap_or(222.0);
            camera.pitch_degrees = self.startup_camera_pitch.unwrap_or(-30.0);
            camera.distance = (preview.radius() * 4.2).clamp(2500.0, 600_000.0);
            return;
        }

        let camera = self.renderer.camera_mut();
        camera.focus = [0.0, 0.0, 0.0];
        camera.yaw_degrees = self.startup_camera_yaw.unwrap_or(222.0);
        camera.pitch_degrees = self.startup_camera_pitch.unwrap_or(-30.0);
        camera.distance = 7000.0;
    }

    pub(super) fn apply_startup_camera_focus(&mut self) {
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
