use super::*;

impl SmsEditorApp {
    pub(super) fn viewport(&mut self, ui: &mut egui::Ui) {
        let available = ui.available_size();
        let size = egui::vec2(available.x.max(240.0), available.y.max(240.0));
        let (rect, response) = ui.allocate_exact_size(size, egui::Sense::click_and_drag());
        let painter = ui.painter_at(rect);

        self.handle_viewport_input(ui, rect, &response);
        self.paint_viewport(ui, &painter, rect);
    }

    pub(super) fn handle_viewport_input(
        &mut self,
        ui: &mut egui::Ui,
        rect: egui::Rect,
        response: &egui::Response,
    ) {
        if response.hovered() {
            let scroll = ui.input(|input| input.smooth_scroll_delta().y);
            if scroll.abs() > f32::EPSILON {
                let amount = self.renderer.camera().distance * scroll * 0.0015;
                self.dolly_camera(amount);
                self.mark_viewport_interaction(ui);
            }
        }

        let pointer_delta = ui.input(|input| input.pointer.delta());
        let modifiers = ui.input(|input| input.modifiers);
        let secondary_down = ui.input(|input| input.pointer.secondary_down());
        let moving_object =
            !modifiers.alt && self.tool == EditorTool::Move && self.selected_object_id.is_some();
        if moving_object && response.drag_started_by(egui::PointerButton::Primary) {
            self.begin_undo_transaction();
        }

        if response.hovered() && ui.input(|input| input.key_pressed(egui::Key::F)) {
            self.frame_selected();
            self.mark_viewport_interaction(ui);
        }

        if secondary_down
            && (response.hovered() || response.dragged_by(egui::PointerButton::Secondary))
            && self.handle_viewport_keyboard_fly(ui)
        {
            self.mark_viewport_interaction(ui);
        }

        if modifiers.alt && response.dragged_by(egui::PointerButton::Primary) {
            self.orbit_camera(pointer_delta);
            if pointer_delta != egui::Vec2::ZERO {
                self.mark_viewport_interaction(ui);
            }
        } else if modifiers.alt && response.dragged_by(egui::PointerButton::Secondary) {
            let amount =
                self.renderer.camera().distance * (pointer_delta.x - pointer_delta.y) * 0.006;
            self.dolly_camera(amount);
            if pointer_delta != egui::Vec2::ZERO {
                self.mark_viewport_interaction(ui);
            }
        } else if response.dragged_by(egui::PointerButton::Secondary) {
            self.rotate_camera_in_place(pointer_delta);
            if pointer_delta != egui::Vec2::ZERO {
                self.mark_viewport_interaction(ui);
            }
        } else if response.dragged_by(egui::PointerButton::Middle)
            || (modifiers.alt && response.dragged_by(egui::PointerButton::Middle))
        {
            self.pan_camera_pixels(rect, pointer_delta);
            if pointer_delta != egui::Vec2::ZERO {
                self.mark_viewport_interaction(ui);
            }
        } else if response.dragged_by(egui::PointerButton::Primary)
            && self.tool == EditorTool::Move
            && pointer_delta != egui::Vec2::ZERO
        {
            self.nudge_selected(self.viewport_drag_move_delta(rect, pointer_delta));
            self.mark_viewport_interaction(ui);
        }

        if moving_object && response.drag_stopped_by(egui::PointerButton::Primary) {
            self.commit_undo_transaction("Moved object");
        }

        if response.clicked() {
            if let Some(pos) = response.interact_pointer_pos() {
                if self.tool == EditorTool::Place {
                    if let Some(factory) = self.palette_factory.clone() {
                        let world = self.screen_to_world_floor(rect, pos);
                        self.spawn_object_at(factory, world);
                        return;
                    }
                }

                let nearest = self
                    .object_screen_positions(rect)
                    .into_iter()
                    .filter_map(|(id, object_pos, _)| {
                        let dist = object_pos.distance(pos);
                        (dist < 24.0).then_some((dist, id))
                    })
                    .min_by(|a, b| a.0.total_cmp(&b.0))
                    .map(|(_, id)| id);
                self.selected_object_id = nearest;
            }
        }
    }

    pub(super) fn handle_viewport_keyboard_fly(&mut self, ui: &egui::Ui) -> bool {
        let (dt, forward_key, back_key, left_key, right_key, up_key, down_key, shift, ctrl) = ui
            .input(|input| {
                (
                    input.stable_dt.clamp(1.0 / 240.0, 1.0 / 15.0),
                    input.key_down(egui::Key::W),
                    input.key_down(egui::Key::S),
                    input.key_down(egui::Key::A),
                    input.key_down(egui::Key::D),
                    input.key_down(egui::Key::E),
                    input.key_down(egui::Key::Q),
                    input.modifiers.shift,
                    input.modifiers.ctrl,
                )
            });
        let frame = self.camera_frame();
        let mut move_axis = [0.0, 0.0, 0.0];
        if forward_key {
            move_axis = vec3_add(move_axis, frame.forward);
        }
        if back_key {
            move_axis = vec3_sub(move_axis, frame.forward);
        }
        if right_key {
            move_axis = vec3_add(move_axis, frame.right);
        }
        if left_key {
            move_axis = vec3_sub(move_axis, frame.right);
        }
        if up_key {
            move_axis = vec3_add(move_axis, [0.0, 1.0, 0.0]);
        }
        if down_key {
            move_axis = vec3_sub(move_axis, [0.0, 1.0, 0.0]);
        }

        if vec3_dot(move_axis, move_axis) <= 0.0001 {
            return false;
        }

        let mut speed = self.viewport_fly_speed();
        if shift {
            speed *= 4.0;
        }
        if ctrl {
            speed *= 0.25;
        }
        self.translate_camera(vec3_scale(vec3_normalize(move_axis), speed * dt));
        true
    }

    pub(super) fn viewport_fly_speed(&self) -> f32 {
        (self.renderer.camera().distance * 0.8).clamp(300.0, 80_000.0) * self.camera_speed
    }

    pub(super) fn rotate_camera_in_place(&mut self, delta: egui::Vec2) {
        if delta == egui::Vec2::ZERO {
            return;
        }
        let old_position = self.camera_frame().position;
        {
            let camera = self.renderer.camera_mut();
            camera.yaw_degrees -= delta.x * 0.14;
            camera.pitch_degrees = (camera.pitch_degrees - delta.y * 0.12).clamp(-89.0, 89.0);
        }
        let frame = self.camera_frame();
        let distance = self.renderer.camera().distance;
        self.renderer.camera_mut().focus =
            vec3_add(old_position, vec3_scale(frame.forward, distance));
    }

    pub(super) fn orbit_camera(&mut self, delta: egui::Vec2) {
        if delta == egui::Vec2::ZERO {
            return;
        }
        let camera = self.renderer.camera_mut();
        camera.yaw_degrees -= delta.x * 0.18;
        camera.pitch_degrees = (camera.pitch_degrees - delta.y * 0.14).clamp(-89.0, 89.0);
    }

    pub(super) fn pan_camera_pixels(&mut self, rect: egui::Rect, delta: egui::Vec2) {
        if delta == egui::Vec2::ZERO {
            return;
        }
        let frame = self.camera_frame();
        let focal = perspective_focal_length(rect, self.viewport_zoom).max(1.0);
        let units_per_pixel = (self.renderer.camera().distance / focal).max(0.01);
        let world_delta = vec3_add(
            vec3_scale(frame.right, -delta.x * units_per_pixel),
            vec3_scale(frame.up, delta.y * units_per_pixel),
        );
        self.translate_camera(world_delta);
    }

    pub(super) fn viewport_drag_move_delta(&self, rect: egui::Rect, delta: egui::Vec2) -> [f32; 3] {
        if delta == egui::Vec2::ZERO {
            return [0.0, 0.0, 0.0];
        }
        let frame = self.camera_frame();
        let focal = perspective_focal_length(rect, self.viewport_zoom).max(1.0);
        let units_per_pixel = (self.renderer.camera().distance / focal).max(0.01);
        let right = vec3_normalize([frame.right[0], 0.0, frame.right[2]]);
        let forward = vec3_normalize([frame.forward[0], 0.0, frame.forward[2]]);
        let forward = if vec3_dot(forward, forward) <= 0.0001 {
            [0.0, 0.0, 1.0]
        } else {
            forward
        };
        vec3_add(
            vec3_scale(right, delta.x * units_per_pixel),
            vec3_scale(forward, -delta.y * units_per_pixel),
        )
    }

    pub(super) fn dolly_camera(&mut self, amount: f32) {
        if amount.abs() <= f32::EPSILON || !amount.is_finite() {
            return;
        }
        let frame = self.camera_frame();
        self.translate_camera(vec3_scale(frame.forward, amount));
    }

    pub(super) fn translate_camera(&mut self, delta: [f32; 3]) {
        if !delta.iter().all(|value| value.is_finite()) {
            return;
        }
        let camera = self.renderer.camera_mut();
        camera.focus = vec3_add(camera.focus, delta);
    }

    pub(super) fn mark_viewport_interaction(&mut self, ui: &egui::Ui) {
        ui.ctx().request_repaint();
    }

    pub(super) fn paint_viewport(
        &mut self,
        ui: &egui::Ui,
        painter: &egui::Painter,
        rect: egui::Rect,
    ) {
        painter.add(egui::Shape::mesh(viewport_background_mesh(rect)));

        if self.model_preview.is_some() {
            self.paint_model_preview(ui.ctx(), painter, rect);
        } else {
            self.paint_stage_silhouette(painter, rect);
        }
        // The grid is an editor aid rather than part of Sunshine's EFB. Keep it
        // above the source-accurate offscreen game render.
        if self.renderer.config().show_grid {
            self.paint_grid(painter, rect);
        }

        let object_positions = self.object_screen_positions(rect);
        for (id, pos, label) in object_positions {
            let selected = self.selected_object_id.as_deref() == Some(&id);
            if self.view_mode != ViewMode::Objects && !selected {
                continue;
            }
            let color = if selected {
                egui::Color32::from_rgb(255, 214, 102)
            } else {
                egui::Color32::from_rgb(93, 205, 184)
            };
            painter.circle_filled(
                pos + egui::vec2(3.0, 4.0),
                9.0,
                egui::Color32::from_black_alpha(80),
            );
            painter.circle_filled(pos, 8.0, color);
            painter.circle_stroke(
                pos,
                11.0,
                egui::Stroke::new(1.5, egui::Color32::from_rgb(240, 244, 246)),
            );
            painter.text(
                pos + egui::vec2(14.0, -8.0),
                egui::Align2::LEFT_TOP,
                label,
                egui::FontId::proportional(12.0),
                egui::Color32::from_rgb(232, 236, 238),
            );

            if selected {
                self.paint_gizmo(painter, rect, pos);
            }
        }

        self.paint_viewport_overlays(ui, painter, rect);
    }

    pub(super) fn paint_grid(&self, painter: &egui::Painter, rect: egui::Rect) {
        let minor = egui::Stroke::new(
            1.0,
            egui::Color32::from_rgba_unmultiplied(178, 186, 178, 32),
        );
        let major = egui::Stroke::new(
            1.5,
            egui::Color32::from_rgba_unmultiplied(213, 200, 160, 58),
        );

        for i in -10..=10 {
            let v = i as f32 * 500.0;
            let stroke = if i % 5 == 0 { major } else { minor };
            self.paint_world_segment(painter, rect, [v, 0.0, -5000.0], [v, 0.0, 5000.0], stroke);
            self.paint_world_segment(painter, rect, [-5000.0, 0.0, v], [5000.0, 0.0, v], stroke);
        }

        self.paint_world_segment(
            painter,
            rect,
            [-5200.0, 0.0, 0.0],
            [5200.0, 0.0, 0.0],
            egui::Stroke::new(2.0, egui::Color32::from_rgb(206, 82, 82)),
        );
        self.paint_world_segment(
            painter,
            rect,
            [0.0, 0.0, -5200.0],
            [0.0, 0.0, 5200.0],
            egui::Stroke::new(2.0, egui::Color32::from_rgb(82, 168, 110)),
        );
    }

    pub(super) fn paint_world_segment(
        &self,
        painter: &egui::Painter,
        rect: egui::Rect,
        start: [f32; 3],
        end: [f32; 3],
        stroke: egui::Stroke,
    ) {
        if let Some(segment) = self.project_world_segment_to_screen(rect, start, end) {
            painter.line_segment(segment, stroke);
        }
    }

    pub(super) fn paint_stage_silhouette(&self, painter: &egui::Painter, rect: egui::Rect) {
        let island = vec![
            self.world_to_screen(rect, [-3200.0, -80.0, -2400.0]),
            self.world_to_screen(rect, [1200.0, -80.0, -3000.0]),
            self.world_to_screen(rect, [3400.0, -80.0, -200.0]),
            self.world_to_screen(rect, [2200.0, -80.0, 2600.0]),
            self.world_to_screen(rect, [-2600.0, -80.0, 2800.0]),
            self.world_to_screen(rect, [-3900.0, -80.0, 600.0]),
        ];
        painter.add(egui::Shape::convex_polygon(
            island,
            egui::Color32::from_rgb(76, 84, 66),
            egui::Stroke::new(1.0, egui::Color32::from_rgb(148, 162, 123)),
        ));

        let plaza = vec![
            self.world_to_screen(rect, [-1200.0, 0.0, -900.0]),
            self.world_to_screen(rect, [900.0, 0.0, -900.0]),
            self.world_to_screen(rect, [1200.0, 0.0, 900.0]),
            self.world_to_screen(rect, [-1000.0, 0.0, 1100.0]),
        ];
        painter.add(egui::Shape::convex_polygon(
            plaza,
            egui::Color32::from_rgb(126, 111, 82),
            egui::Stroke::new(1.5, egui::Color32::from_rgb(198, 170, 112)),
        ));
    }

    pub(super) fn paint_model_preview(
        &mut self,
        ctx: &egui::Context,
        painter: &egui::Painter,
        rect: egui::Rect,
    ) {
        self.update_level_transformation(ctx);
        self.update_skeletal_animations();
        let has_triangles = self
            .model_preview
            .as_ref()
            .is_some_and(|preview| !preview.triangles.is_empty());

        if has_triangles {
            if self.model_preview.as_ref().is_some_and(|preview| {
                !preview.texture_srt_animations.is_empty() || !preview.animated_models.is_empty()
            }) {
                ctx.request_repaint_after(std::time::Duration::from_millis(16));
            }
            if let (Some(gpu_viewport), Some(frame)) =
                (self.gpu_viewport.as_ref(), self.gpu_viewport_frame(rect))
            {
                gpu_viewport.set_frame(frame);
                painter.add(gpu_viewport.paint_callback(rect));
            } else {
                let key = self.model_framebuffer_key(rect);
                let needs_render =
                    self.model_framebuffer.is_none() || self.model_framebuffer_key != key;
                if needs_render {
                    if let Some(image) = self.render_model_framebuffer(rect) {
                        if let Some(handle) = &mut self.model_framebuffer {
                            handle.set(image, egui::TextureOptions::LINEAR);
                        } else {
                            self.model_framebuffer = Some(ctx.load_texture(
                                "sms-model-framebuffer",
                                image,
                                egui::TextureOptions::LINEAR,
                            ));
                        }
                        self.model_framebuffer_key = key;
                    }
                }

                if let Some(handle) = &self.model_framebuffer {
                    painter.image(
                        handle.id(),
                        rect,
                        egui::Rect::from_min_max(egui::Pos2::ZERO, egui::pos2(1.0, 1.0)),
                        egui::Color32::WHITE,
                    );
                }
            }
        } else if let Some(preview) = &self.model_preview {
            self.paint_point_model_preview(painter, rect, preview);
        }

        if self.renderer.config().show_object_bounds {
            if let Some(preview) = &self.model_preview {
                self.paint_preview_bounds(painter, rect, preview);
            }
        }
    }

    pub(super) fn update_level_transformation(&mut self, ctx: &egui::Context) {
        let (duration_frames, particle_end_frames) = self
            .model_preview
            .as_ref()
            .map(|preview| {
                (
                    preview.level_transform_duration_frames,
                    preview.level_transform_particle_end_frames,
                )
            })
            .unwrap_or((600.0, 600.0));
        let mut particle_frame = if self.level_transform_progress >= 1.0 {
            particle_end_frames
        } else {
            self.level_transform_progress * duration_frames
        };
        if self.level_transform_playing {
            let start_frame = self.level_transform_playback_origin * duration_frames;
            particle_frame = start_frame
                + self.level_transform_started_at.elapsed().as_secs_f32()
                    * SMS_ANIMATION_FRAMES_PER_SECOND;
            self.level_transform_progress = (particle_frame / duration_frames).clamp(0.0, 1.0);
            if particle_frame >= particle_end_frames {
                particle_frame = particle_end_frames;
                self.level_transform_playing = false;
            } else {
                ctx.request_repaint_after(std::time::Duration::from_millis(16));
            }
        }

        let geometry_progress = level_transform_sample_progress(
            self.level_transform_progress,
            duration_frames,
            self.level_transform_playing,
        );
        let progress_bits = particle_frame.floor().to_bits();
        if progress_bits == self.last_level_transform_progress_bits {
            return;
        }
        let Some(preview) = self.model_preview.as_mut() else {
            return;
        };
        if !preview.has_level_transformation() {
            self.last_level_transform_progress_bits = progress_bits;
            return;
        }
        self.last_level_transform_progress_bits = progress_bits;

        let ModelPreview {
            level_transform_models,
            level_transform_particles,
            points,
            triangles,
            ..
        } = preview;
        for source in level_transform_models.iter() {
            let overrides = level_transform_overrides(&source.targets, geometry_progress);
            let Ok(mut posed_triangles) = source
                .file
                .triangles_with_joint_overrides(source.loader_flags, &overrides)
            else {
                continue;
            };
            apply_level_transform_visibility(
                &source.file,
                &source.targets,
                geometry_progress,
                &mut posed_triangles,
            );
            for (point, position) in points[source.point_range.clone()].iter_mut().zip(
                posed_triangles
                    .iter()
                    .flat_map(|triangle| triangle.vertices)
                    .step_by(source.point_stride),
            ) {
                point.position = position;
            }
            for (triangle, posed) in triangles[source.triangle_range.clone()].iter_mut().zip(
                posed_triangles
                    .iter()
                    .filter(|triangle| triangle_vertices_are_finite(triangle.vertices)),
            ) {
                triangle.vertices = posed.vertices;
                triangle.normals = posed.normals;
            }
        }
        apply_level_transform_particles(level_transform_particles, particle_frame, triangles);

        if let Some(gpu_viewport) = &self.gpu_viewport {
            let mut triangle_ranges = preview
                .level_transform_models
                .iter()
                .map(|model| model.triangle_range.clone())
                .collect::<Vec<_>>();
            triangle_ranges.extend(
                preview
                    .level_transform_particles
                    .iter()
                    .map(|particles| particles.triangle_range.clone()),
            );
            gpu_viewport.update_geometry(preview, &triangle_ranges);
        }
        self.clear_viewport_preview_cache();
    }

    pub(super) fn update_skeletal_animations(&mut self) {
        let elapsed_seconds = self.animation_started_at.elapsed().as_secs_f32();
        let tick = (elapsed_seconds.max(0.0) * 30.0) as u64;
        if tick == self.last_skeletal_animation_tick {
            return;
        }
        let Some(preview) = self.model_preview.as_mut() else {
            return;
        };
        if preview.animated_models.is_empty() && preview.texture_pattern_animations.is_empty() {
            return;
        }
        self.last_skeletal_animation_tick = tick;

        let mut dirty_materials = Vec::new();
        {
            let ModelPreview {
                animated_models,
                texture_pattern_animations,
                materials,
                points,
                triangles,
                ..
            } = preview;
            for source in animated_models {
                let Ok(posed_triangles) = source.file.animated_triangles_with_joint_animation(
                    source.loader_flags,
                    &source.animation,
                    elapsed_seconds,
                ) else {
                    continue;
                };
                let joint_matrices = source
                    .file
                    .joint_matrices_with_joint_animation(
                        source.loader_flags,
                        &source.animation,
                        elapsed_seconds,
                    )
                    .unwrap_or_default();
                for instance in &source.instances {
                    for (point, position) in points[instance.point_range.clone()].iter_mut().zip(
                        posed_triangles
                            .iter()
                            .flat_map(|triangle| triangle.vertices)
                            .step_by(instance.point_stride),
                    ) {
                        point.position = transform_preview_point(position, instance.transform);
                    }
                    for (triangle, posed) in
                        triangles[instance.triangle_range.clone()].iter_mut().zip(
                            posed_triangles
                                .iter()
                                .filter(|triangle| triangle_vertices_are_finite(triangle.vertices)),
                        )
                    {
                        triangle.vertices =
                            transform_preview_vertices(posed.vertices, instance.transform);
                        triangle.normals = posed
                            .normals
                            .map(|normals| transform_preview_normals(normals, instance.transform));
                    }
                    for accessory in &instance.accessories {
                        let joint_matrix = match accessory.joint_index {
                            Some(index) => {
                                let Some(matrix) = joint_matrices.get(index).copied() else {
                                    continue;
                                };
                                matrix
                            }
                            None => j3d_identity_matrix(),
                        };
                        let posed_triangles =
                            accessory.joint_animation.as_ref().and_then(|animation| {
                                accessory
                                    .file
                                    .animated_triangles_with_joint_animation(
                                        accessory.loader_flags,
                                        animation.as_ref(),
                                        elapsed_seconds,
                                    )
                                    .ok()
                            });
                        let local_triangles = posed_triangles
                            .as_deref()
                            .unwrap_or(accessory.local_triangles.as_slice());
                        for (triangle, local) in triangles[accessory.triangle_range.clone()]
                            .iter_mut()
                            .zip(local_triangles.iter())
                        {
                            triangle.vertices = transform_preview_vertices(
                                local
                                    .vertices
                                    .map(|vertex| transform_j3d_matrix_point(joint_matrix, vertex)),
                                instance.transform,
                            );
                            triangle.normals = local.normals.map(|normals| {
                                transform_preview_normals(
                                    normals.map(|normal| {
                                        transform_j3d_matrix_normal(joint_matrix, normal)
                                    }),
                                    instance.transform,
                                )
                            });
                        }
                    }
                }
            }
            for pattern in texture_pattern_animations {
                let frame = pattern
                    .animation
                    .playback_frame(elapsed_seconds + pattern.phase_seconds);
                for binding in &mut pattern.bindings {
                    let next = pattern.animation.bindings[binding.animation_binding_index]
                        .texture_index(frame)
                        .map(|index| binding.texture_base + index);
                    if next == binding.current_texture_index {
                        continue;
                    }
                    let Some(material) = materials.get_mut(binding.material_index) else {
                        continue;
                    };
                    material.texture_indices[binding.texture_slot] = next;
                    binding.current_texture_index = next;
                    dirty_materials.push(binding.material_index);
                }
            }
        }
        if let Some(gpu_viewport) = &self.gpu_viewport {
            let triangle_ranges = preview
                .animated_models
                .iter()
                .flat_map(|model| {
                    model.instances.iter().flat_map(|instance| {
                        std::iter::once(instance.triangle_range.clone()).chain(
                            instance
                                .accessories
                                .iter()
                                .map(|accessory| accessory.triangle_range.clone()),
                        )
                    })
                })
                .collect::<Vec<_>>();
            gpu_viewport.update_geometry(preview, &triangle_ranges);
            if !dirty_materials.is_empty() {
                dirty_materials.sort_unstable();
                dirty_materials.dedup();
                gpu_viewport.update_materials(preview, &dirty_materials);
            }
        }
        self.clear_viewport_preview_cache();
    }

    pub(super) fn gpu_viewport_frame(
        &self,
        rect: egui::Rect,
    ) -> Option<gpu_viewport::GpuViewportFrame> {
        let preview = self.model_preview.as_ref()?;
        let frame = self.camera_frame();
        let focal = perspective_focal_length(rect, self.viewport_zoom);
        let far = preview.far_clip(self.renderer.camera().distance);
        Some(gpu_viewport::GpuViewportFrame {
            camera_position: frame.position,
            right: frame.right,
            up: frame.up,
            forward: frame.forward,
            focal,
            viewport_size: [rect.width().max(1.0), rect.height().max(1.0)],
            viewport_pan: [self.viewport_pan.x, self.viewport_pan.y],
            near: VIEWPORT_NEAR_CLIP,
            far,
            animation_seconds: self.animation_started_at.elapsed().as_secs_f32(),
        })
    }

    pub(super) fn paint_point_model_preview(
        &self,
        painter: &egui::Painter,
        rect: egui::Rect,
        preview: &ModelPreview,
    ) {
        let stroke =
            egui::Stroke::new(1.0, egui::Color32::from_rgba_unmultiplied(91, 190, 173, 95));
        for pair in preview.points.windows(2) {
            if pair[0].model_index != pair[1].model_index {
                continue;
            }

            if let Some([a, b]) =
                self.project_world_segment_to_screen(rect, pair[0].position, pair[1].position)
            {
                if rect.expand(80.0).contains(a)
                    && rect.expand(80.0).contains(b)
                    && a.distance(b) < 90.0
                {
                    painter.line_segment([a, b], stroke);
                }
            }
        }

        for point in &preview.points {
            if let Some((screen, _)) = self.project_world_to_screen(rect, point.position) {
                if rect.expand(4.0).contains(screen) {
                    painter.circle_filled(
                        screen,
                        1.4,
                        egui::Color32::from_rgba_unmultiplied(224, 243, 229, 155),
                    );
                }
            }
        }
    }

    pub(super) fn render_model_framebuffer(&self, rect: egui::Rect) -> Option<egui::ColorImage> {
        let preview = self.model_preview.as_ref()?;
        if preview.triangles.is_empty() || rect.width() < 2.0 || rect.height() < 2.0 {
            return None;
        }

        let size = framebuffer_size_for_rect(rect);
        let mut image = viewport_framebuffer_background(size);
        let mut depth = vec![f32::INFINITY; size[0] * size[1]];

        for triangle in preview
            .triangles
            .iter()
            .filter(|triangle| triangle.render_layer == PreviewRenderLayer::Sky)
        {
            if let Some(projected) = self.project_preview_triangle(rect, size, triangle) {
                rasterize_projected_preview_triangle(
                    preview, &mut image, &mut depth, projected, false,
                );
            }
        }

        for triangle in preview.triangles.iter().filter(|triangle| {
            triangle.render_layer != PreviewRenderLayer::Sky
                && !preview_triangle_is_translucent(preview, triangle)
        }) {
            if let Some(projected) = self.project_preview_triangle(rect, size, triangle) {
                rasterize_projected_preview_triangle(
                    preview, &mut image, &mut depth, projected, true,
                );
            }
        }

        let mut translucent: Vec<_> = preview
            .triangles
            .iter()
            .filter(|triangle| {
                triangle.render_layer != PreviewRenderLayer::Sky
                    && preview_triangle_is_translucent(preview, triangle)
            })
            .filter_map(|triangle| self.project_preview_triangle(rect, size, triangle))
            .collect();
        translucent.sort_by(|a, b| b.average_depth.total_cmp(&a.average_depth));
        for projected in translucent {
            rasterize_projected_preview_triangle(preview, &mut image, &mut depth, projected, false);
        }

        Some(image)
    }

    pub(super) fn model_framebuffer_key(&self, rect: egui::Rect) -> Option<ModelFramebufferKey> {
        let preview = self.model_preview.as_ref()?;
        let camera = self.renderer.camera();
        Some(ModelFramebufferKey {
            stage_id: self.stage_id.clone(),
            size: framebuffer_size_for_rect(rect),
            camera_focus: camera.focus.map(f32::to_bits),
            camera_yaw: camera.yaw_degrees.to_bits(),
            camera_pitch: camera.pitch_degrees.to_bits(),
            camera_distance: camera.distance.to_bits(),
            viewport_pan: [self.viewport_pan.x.to_bits(), self.viewport_pan.y.to_bits()],
            viewport_zoom: self.viewport_zoom.to_bits(),
            triangle_count: preview.triangles.len(),
            texture_count: preview.textures.len(),
            source_triangles: preview.source_triangles,
        })
    }

    pub(super) fn project_preview_triangle<'a>(
        &self,
        rect: egui::Rect,
        size: [usize; 2],
        triangle: &'a PreviewTriangle,
    ) -> Option<ProjectedPreviewTriangle<'a>> {
        let vertices = preview_triangle_world_vertices(
            triangle.vertices,
            triangle.render_layer,
            self.camera_frame().position,
        );
        let screen = project_triangle_to_framebuffer(self, rect, size, vertices)?;
        if !projected_triangle_overlaps_frame(screen, size) {
            return None;
        }
        if projected_triangle_is_culled(screen, triangle.cull_mode) {
            return None;
        }
        Some(ProjectedPreviewTriangle {
            triangle,
            screen,
            average_depth: (screen[0].depth + screen[1].depth + screen[2].depth) / 3.0,
        })
    }

    pub(super) fn world_to_framebuffer(
        &self,
        rect: egui::Rect,
        size: [usize; 2],
        point: [f32; 3],
    ) -> Option<ProjectedVertex> {
        let (screen, depth) = self.project_world_to_screen(rect, point)?;
        let x = (screen.x - rect.left()) * size[0] as f32 / rect.width().max(1.0);
        let y = (screen.y - rect.top()) * size[1] as f32 / rect.height().max(1.0);
        Some(ProjectedVertex {
            x,
            y,
            depth,
            inv_depth: 1.0 / depth,
        })
    }

    pub(super) fn paint_preview_bounds(
        &self,
        painter: &egui::Painter,
        rect: egui::Rect,
        preview: &ModelPreview,
    ) {
        let min = preview.bounds_min;
        let max = preview.bounds_max;
        let corners = [
            [min[0], min[1], min[2]],
            [max[0], min[1], min[2]],
            [max[0], min[1], max[2]],
            [min[0], min[1], max[2]],
            [min[0], max[1], min[2]],
            [max[0], max[1], min[2]],
            [max[0], max[1], max[2]],
            [min[0], max[1], max[2]],
        ];
        let edges = [
            (0, 1),
            (1, 2),
            (2, 3),
            (3, 0),
            (4, 5),
            (5, 6),
            (6, 7),
            (7, 4),
            (0, 4),
            (1, 5),
            (2, 6),
            (3, 7),
        ];
        let stroke = egui::Stroke::new(
            1.4,
            egui::Color32::from_rgba_unmultiplied(255, 214, 102, 115),
        );
        for (a, b) in edges {
            self.paint_world_segment(painter, rect, corners[a], corners[b], stroke);
        }
    }

    pub(super) fn paint_gizmo(
        &self,
        painter: &egui::Painter,
        _rect: egui::Rect,
        origin: egui::Pos2,
    ) {
        painter.arrow(
            origin,
            egui::vec2(54.0, 0.0),
            egui::Stroke::new(3.0, egui::Color32::from_rgb(230, 82, 82)),
        );
        painter.arrow(
            origin,
            egui::vec2(-32.0, -34.0),
            egui::Stroke::new(3.0, egui::Color32::from_rgb(82, 176, 116)),
        );
        painter.arrow(
            origin,
            egui::vec2(24.0, -46.0),
            egui::Stroke::new(3.0, egui::Color32::from_rgb(93, 158, 236)),
        );
    }

    pub(super) fn paint_viewport_overlays(
        &self,
        _ui: &egui::Ui,
        painter: &egui::Painter,
        rect: egui::Rect,
    ) {
        let overlay =
            egui::Rect::from_min_size(rect.min + egui::vec2(16.0, 16.0), egui::vec2(280.0, 118.0));
        painter.rect_filled(
            overlay,
            6.0,
            egui::Color32::from_rgba_unmultiplied(12, 15, 16, 210),
        );
        painter.rect_stroke(
            overlay,
            6.0,
            egui::Stroke::new(
                1.0,
                egui::Color32::from_rgba_unmultiplied(255, 255, 255, 30),
            ),
            egui::StrokeKind::Inside,
        );

        let document_summary = self
            .document
            .as_ref()
            .map(|document| {
                let scene = RenderScene::from_document(document);
                format!(
                    "{}  models:{}  col:{}  obj:{}",
                    scene.stage_id,
                    scene.model_paths.len(),
                    scene.collision_paths.len(),
                    scene.object_count
                )
            })
            .unwrap_or_else(|| "No stage loaded".to_string());

        painter.text(
            overlay.min + egui::vec2(14.0, 12.0),
            egui::Align2::LEFT_TOP,
            document_summary,
            egui::FontId::proportional(14.0),
            egui::Color32::from_rgb(238, 241, 242),
        );
        painter.text(
            overlay.min + egui::vec2(14.0, 40.0),
            egui::Align2::LEFT_TOP,
            format!(
                "{} / {} / snap {}",
                self.tool.label(),
                self.view_mode.label(),
                if self.snap_enabled { "on" } else { "off" }
            ),
            egui::FontId::monospace(12.0),
            egui::Color32::from_rgb(190, 202, 204),
        );
        let camera = self.renderer.camera();
        painter.text(
            overlay.min + egui::vec2(14.0, 66.0),
            egui::Align2::LEFT_TOP,
            if let Some(preview) = &self.model_preview {
                format!(
                    "verts {}  tris {}  tex {}  pts {}  dist {:.0}",
                    preview.source_vertices,
                    preview.source_triangles,
                    preview.source_textures,
                    preview.points.len(),
                    camera.distance
                )
            } else {
                format!(
                    "yaw {:.0}  pitch {:.0}  dist {:.0}",
                    camera.yaw_degrees, camera.pitch_degrees, camera.distance
                )
            },
            egui::FontId::monospace(12.0),
            egui::Color32::from_rgb(190, 202, 204),
        );

        let compass_center = egui::pos2(rect.right() - 70.0, rect.top() + 74.0);
        painter.circle_filled(
            compass_center,
            42.0,
            egui::Color32::from_rgba_unmultiplied(10, 13, 14, 190),
        );
        painter.circle_stroke(
            compass_center,
            42.0,
            egui::Stroke::new(
                1.0,
                egui::Color32::from_rgba_unmultiplied(255, 255, 255, 40),
            ),
        );
        painter.text(
            compass_center + egui::vec2(0.0, -31.0),
            egui::Align2::CENTER_CENTER,
            "N",
            egui::FontId::monospace(12.0),
            egui::Color32::from_rgb(235, 220, 160),
        );
        painter.arrow(
            compass_center,
            egui::vec2(0.0, -24.0),
            egui::Stroke::new(2.0, egui::Color32::from_rgb(235, 220, 160)),
        );
        painter.arrow(
            compass_center,
            egui::vec2(23.0, 10.0),
            egui::Stroke::new(2.0, egui::Color32::from_rgb(93, 158, 236)),
        );

        if self.palette_factory.is_some() && self.tool == EditorTool::Place {
            let text = self.palette_factory.as_deref().unwrap_or_default();
            let rect = egui::Rect::from_min_size(
                rect.left_bottom() + egui::vec2(16.0, -48.0),
                egui::vec2(260.0, 34.0),
            );
            painter.rect_filled(
                rect,
                5.0,
                egui::Color32::from_rgba_unmultiplied(24, 36, 34, 220),
            );
            painter.text(
                rect.center(),
                egui::Align2::CENTER_CENTER,
                format!("Placing {text}"),
                egui::FontId::proportional(13.0),
                egui::Color32::from_rgb(215, 238, 231),
            );
        }
    }
}
