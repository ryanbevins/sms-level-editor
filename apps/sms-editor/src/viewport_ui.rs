use super::*;

const CAMERA_SPEED_MIN: f32 = 0.01;
const CAMERA_SPEED_MAX: f32 = 8.0;
const CAMERA_SPEED_SCROLL_SCALE: f32 = 240.0;
const CAMERA_FLY_ACCELERATION_INTERP_SPEED: f32 = 8.0;
const CAMERA_FLY_DECELERATION_INTERP_SPEED: f32 = 12.0;
const CAMERA_FLY_STOP_SPEED: f32 = 0.5;
const PICK_DEPTH_EPSILON: f32 = 0.01;
const GIZMO_AXIS_LENGTH_PIXELS: f32 = 78.0;
const GIZMO_RING_RADIUS_PIXELS: f32 = 64.0;
const GIZMO_HIT_RADIUS_PIXELS: f32 = 10.0;

pub(super) fn collision_surface_color(surface_type: u16) -> egui::Color32 {
    const PALETTE: [[u8; 3]; 12] = [
        [76, 184, 168],
        [104, 164, 230],
        [232, 176, 72],
        [206, 112, 154],
        [128, 194, 92],
        [164, 125, 220],
        [230, 112, 84],
        [72, 172, 220],
        [218, 204, 88],
        [92, 196, 126],
        [214, 132, 222],
        [226, 142, 66],
    ];
    let mut hash = u32::from(surface_type);
    hash ^= hash >> 7;
    hash = hash.wrapping_mul(0x9e37_79b1);
    let [r, g, b] = PALETTE[hash as usize % PALETTE.len()];
    egui::Color32::from_rgb(r, g, b)
}

fn shaded_collision_color(triangle: &CollisionPreviewTriangle) -> egui::Color32 {
    let base = collision_surface_color(triangle.surface_type);
    let edge_a = vec3_sub(triangle.vertices[1], triangle.vertices[0]);
    let edge_b = vec3_sub(triangle.vertices[2], triangle.vertices[0]);
    let normal = vec3_normalize(vec3_cross(edge_a, edge_b));
    let light = vec3_normalize([0.35, 0.85, 0.4]);
    let shade = (0.68 + 0.32 * vec3_dot(normal, light).abs()).clamp(0.0, 1.0);
    egui::Color32::from_rgb(
        (f32::from(base.r()) * shade) as u8,
        (f32::from(base.g()) * shade) as u8,
        (f32::from(base.b()) * shade) as u8,
    )
}

#[derive(Debug, Clone, Copy)]
struct GizmoScreenAxis {
    start: egui::Pos2,
    end: egui::Pos2,
    direction: egui::Vec2,
}

#[derive(Debug, Clone)]
struct GizmoGeometry {
    origin: egui::Pos2,
    axes: [GizmoScreenAxis; 3],
    rotation_rings: [Vec<egui::Pos2>; 3],
    world_units_per_pixel: f32,
}

fn preview_triangle_writes_opaque_depth(
    preview: &ModelPreview,
    triangle: &PreviewTriangle,
) -> bool {
    !matches!(
        triangle.render_layer,
        PreviewRenderLayer::Sky
            | PreviewRenderLayer::MirrorScene
            | PreviewRenderLayer::WaveFoam
            | PreviewRenderLayer::IndirectWater
    ) && !preview_triangle_is_translucent(preview, triangle)
}

#[cfg(test)]
pub(super) fn outline_segments_from_coverage(
    coverage: &[bool],
    size: [usize; 2],
    bounds: [usize; 4],
    rect: egui::Rect,
) -> Vec<[egui::Pos2; 2]> {
    outline_segments_from_bounded_coverage(coverage, size, [0, 0], size, bounds, rect)
}

pub(super) fn outline_segments_from_bounded_coverage(
    coverage: &[bool],
    coverage_size: [usize; 2],
    coverage_origin: [usize; 2],
    framebuffer_size: [usize; 2],
    bounds: [usize; 4],
    rect: egui::Rect,
) -> Vec<[egui::Pos2; 2]> {
    let [coverage_width, coverage_height] = coverage_size;
    let [origin_x, origin_y] = coverage_origin;
    let [width, height] = framebuffer_size;
    let [min_x, max_x, min_y, max_y] = bounds;
    let screen_pos = |x: f32, y: f32| {
        egui::pos2(
            rect.left() + x * rect.width() / width.max(1) as f32,
            rect.top() + y * rect.height() / height.max(1) as f32,
        )
    };
    let covered = |x: isize, y: isize| {
        let local_x = x - origin_x as isize;
        let local_y = y - origin_y as isize;
        local_x >= 0
            && local_y >= 0
            && (local_x as usize) < coverage_width
            && (local_y as usize) < coverage_height
            && coverage[local_y as usize * coverage_width + local_x as usize]
    };
    let mut segments = Vec::new();

    for y in min_y as isize - 1..=max_y as isize {
        for x in min_x as isize - 1..=max_x as isize {
            let case = u8::from(covered(x, y))
                | (u8::from(covered(x + 1, y)) << 1)
                | (u8::from(covered(x + 1, y + 1)) << 2)
                | (u8::from(covered(x, y + 1)) << 3);
            if matches!(case, 0 | 15) {
                continue;
            }

            let x = x as f32;
            let y = y as f32;
            let top = screen_pos(x + 1.0, y + 0.5);
            let right = screen_pos(x + 1.5, y + 1.0);
            let bottom = screen_pos(x + 1.0, y + 1.5);
            let left = screen_pos(x + 0.5, y + 1.0);
            let cell_segments: &[(egui::Pos2, egui::Pos2)] = match case {
                1 | 14 => &[(left, top)],
                2 | 13 => &[(top, right)],
                3 | 12 => &[(left, right)],
                4 | 11 => &[(right, bottom)],
                5 => &[(left, top), (right, bottom)],
                6 | 9 => &[(top, bottom)],
                7 | 8 => &[(left, bottom)],
                10 => &[(top, right), (bottom, left)],
                _ => &[],
            };
            segments.extend(cell_segments.iter().map(|(start, end)| [*start, *end]));
        }
    }

    segments
}

fn outline_point_key(point: egui::Pos2) -> [u32; 2] {
    [point.x.to_bits(), point.y.to_bits()]
}

fn smooth_closed_outline_path(path: Vec<egui::Pos2>) -> Vec<egui::Pos2> {
    if path.len() < 4 || outline_point_key(path[0]) != outline_point_key(*path.last().unwrap()) {
        return path;
    }

    let points = &path[..path.len() - 1];
    let mut smoothed = Vec::with_capacity(points.len() * 2 + 1);
    for index in 0..points.len() {
        let current = points[index];
        let next = points[(index + 1) % points.len()];
        let delta = next - current;
        smoothed.push(current + delta * 0.25);
        smoothed.push(current + delta * 0.75);
    }
    smoothed.push(smoothed[0]);
    smoothed
}

pub(super) fn outline_paths_from_segments(segments: &[[egui::Pos2; 2]]) -> Vec<Vec<egui::Pos2>> {
    let mut adjacency = BTreeMap::<[u32; 2], Vec<usize>>::new();
    for (index, segment) in segments.iter().enumerate() {
        adjacency
            .entry(outline_point_key(segment[0]))
            .or_default()
            .push(index);
        adjacency
            .entry(outline_point_key(segment[1]))
            .or_default()
            .push(index);
    }

    let mut used = vec![false; segments.len()];
    let mut paths = Vec::new();
    for first_segment in 0..segments.len() {
        if used[first_segment] {
            continue;
        }
        let segment = segments[first_segment];
        let start = if adjacency
            .get(&outline_point_key(segment[1]))
            .is_some_and(|neighbors| neighbors.len() == 1)
        {
            segment[1]
        } else {
            segment[0]
        };
        let start_key = outline_point_key(start);
        let mut current = start;
        let mut path = vec![start];

        loop {
            let current_key = outline_point_key(current);
            let Some(next_segment) = adjacency
                .get(&current_key)
                .and_then(|neighbors| neighbors.iter().copied().find(|index| !used[*index]))
            else {
                break;
            };
            used[next_segment] = true;
            let next = if outline_point_key(segments[next_segment][0]) == current_key {
                segments[next_segment][1]
            } else {
                segments[next_segment][0]
            };
            path.push(next);
            current = next;
            if outline_point_key(current) == start_key || path.len() > segments.len() + 1 {
                break;
            }
        }
        if path.len() >= 2 {
            paths.push(smooth_closed_outline_path(path));
        }
    }
    paths
}

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
        if let Some(payload) = response.dnd_release_payload::<ObjectPaletteDragPayload>() {
            if let Some(pointer) = ui.input(|input| input.pointer.latest_pos()) {
                let world = self.screen_to_world_floor(rect, pointer);
                self.clear_audio_helper_selection();
                self.spawn_object_at(payload.factory_name.clone(), world);
                self.content_browser.inspector_active = false;
            }
            return;
        }
        if let Some(payload) = response.dnd_release_payload::<ModelAssetDragPayload>() {
            if let Some(pointer) = ui.input(|input| input.pointer.latest_pos()) {
                let world = self.screen_to_world_floor(rect, pointer);
                self.clear_audio_helper_selection();
                self.spawn_model_instance_at(payload.asset_id, world);
                self.content_browser.inspector_active = false;
            }
            return;
        }
        let secondary_down = ui.input(|input| input.pointer.secondary_down());
        let fly_navigation_active = secondary_down
            && (response.hovered() || response.dragged_by(egui::PointerButton::Secondary));

        if response.hovered() {
            let scroll = ui.input(|input| input.smooth_scroll_delta().y);
            if scroll.abs() > f32::EPSILON {
                if fly_navigation_active {
                    self.camera_speed = camera_speed_after_scroll(self.camera_speed, scroll);
                    self.queue_camera_state_save();
                } else {
                    let amount = self.renderer.camera().distance * scroll * 0.0015;
                    self.dolly_camera(amount);
                }
                self.mark_viewport_interaction(ui);
            }
        }

        let pointer_delta = ui.input(|input| input.pointer.delta());
        let modifiers = ui.input(|input| input.modifiers);
        let route_handle_using_pointer = self.handle_route_handle_drag(ui, rect, response);
        let gizmo_using_pointer =
            route_handle_using_pointer || self.handle_transform_gizmo_input(ui, rect, response);

        if response.hovered() && ui.input(|input| input.key_pressed(egui::Key::F)) {
            self.frame_selected();
            self.mark_viewport_interaction(ui);
        }

        if self.handle_viewport_keyboard_fly(ui, fly_navigation_active) {
            self.mark_viewport_interaction(ui);
        }

        if gizmo_using_pointer {
            self.mark_viewport_interaction(ui);
        } else if modifiers.alt && response.dragged_by(egui::PointerButton::Primary) {
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
        }

        if response.clicked() && self.hovered_gizmo_axis.is_none() && !gizmo_using_pointer {
            self.content_browser.inspector_active = false;
            if let Some(pos) = response.interact_pointer_pos() {
                let model_instance_id = self.model_instance_at_screen_position(rect, pos);
                let object_id = model_instance_id
                    .is_none()
                    .then(|| self.object_at_screen_position(rect, pos))
                    .flatten();
                let leaving_helper_for_world_selection = self.selected_audio_helper_id.is_some()
                    && (model_instance_id.is_some() || object_id.is_some());
                if !leaving_helper_for_world_selection
                    && self.handle_route_viewport_click(rect, response, modifiers)
                {
                    return;
                }
                if self.tool == EditorTool::Place {
                    if let Some(placement) = self.active_placement.clone() {
                        let world = self.screen_to_world_floor(rect, pos);
                        self.clear_audio_helper_selection();
                        match placement {
                            ActivePlacement::Model { asset_id } => {
                                self.spawn_model_instance_at(asset_id, world);
                            }
                            ActivePlacement::Object { factory_name } => {
                                self.spawn_object_at(factory_name, world);
                            }
                        }
                        return;
                    }
                }

                if self.asset_dirty && !self.save_selected_model_asset() {
                    return;
                }
                self.clear_audio_helper_selection();
                if let Some(instance_id) = model_instance_id {
                    self.selected_model_instance_id = Some(instance_id);
                    self.selected_object_id = None;
                    self.selected_model_asset = None;
                    self.selected_model_document = None;
                    self.saved_model_document = None;
                } else {
                    self.selected_model_instance_id = None;
                    self.selected_object_id = object_id;
                    if self.selected_object_id.is_some() {
                        self.selected_model_asset = None;
                        self.selected_model_document = None;
                        self.saved_model_document = None;
                    }
                }
            }
        }
    }

    fn handle_transform_gizmo_input(
        &mut self,
        ui: &egui::Ui,
        rect: egui::Rect,
        response: &egui::Response,
    ) -> bool {
        let route_origin = self.selected_route_control_position();
        let supports_gizmo = if route_origin.is_some() {
            self.tool == EditorTool::Move
        } else {
            matches!(
                self.tool,
                EditorTool::Move | EditorTool::Rotate | EditorTool::Scale
            )
        };
        let pointer = ui.input(|input| input.pointer.interact_pos());
        let world_origin = route_origin.or_else(|| {
            self.selected_object()
                .map(|object| object.transform.translation)
        });
        let geometry = supports_gizmo
            .then(|| world_origin.and_then(|origin| self.gizmo_geometry(rect, origin)))
            .flatten();
        let hovered = if response.hovered() {
            pointer
                .zip(geometry.as_ref())
                .and_then(|(pointer, geometry)| gizmo_axis_at_pointer(self.tool, geometry, pointer))
        } else {
            None
        };
        self.hovered_gizmo_axis = self.gizmo_drag.map(|drag| drag.axis).or(hovered);

        if self.hovered_gizmo_axis.is_some() {
            ui.ctx().set_cursor_icon(if self.gizmo_drag.is_some() {
                egui::CursorIcon::Grabbing
            } else {
                egui::CursorIcon::Grab
            });
        }

        let primary_pressed = ui.input(|input| input.pointer.primary_pressed());
        if self.gizmo_drag.is_none() && primary_pressed {
            if let (Some(axis), Some(pointer), Some(geometry), Some(origin)) =
                (hovered, pointer, geometry.as_ref(), world_origin)
            {
                let start_transform = if route_origin.is_some() {
                    self.begin_route_undo_transaction();
                    Transform {
                        translation: origin,
                        ..Transform::default()
                    }
                } else {
                    self.begin_undo_transaction();
                    self.selected_object()
                        .map_or_else(Transform::default, |object| object.transform)
                };
                self.gizmo_drag = Some(GizmoDrag {
                    axis,
                    tool: self.tool,
                    start_pointer: pointer,
                    screen_origin: geometry.origin,
                    screen_direction: geometry.axes[axis.index()].direction,
                    world_units_per_pixel: geometry.world_units_per_pixel,
                    start_transform,
                });
            }
        }

        let was_dragging = self.gizmo_drag.is_some();
        let primary_down = ui.input(|input| input.pointer.primary_down());
        if primary_down {
            if let (Some(drag), Some(pointer)) = (self.gizmo_drag, pointer) {
                let transform = transform_from_gizmo_drag(
                    drag,
                    pointer,
                    self.snap_enabled,
                    self.snap_translation,
                    self.snap_rotation,
                    self.snap_scale,
                );
                if self.route_undo_transaction.is_some() {
                    self.update_selected_route_control_position(transform.translation);
                } else {
                    self.update_selected_transform(transform);
                }
            }
        }

        let primary_released = ui.input(|input| input.pointer.primary_released());
        if primary_released {
            if let Some(drag) = self.gizmo_drag.take() {
                if self.route_undo_transaction.is_some() {
                    self.commit_route_undo_transaction("Moved route control point");
                    return true;
                }
                self.commit_undo_transaction(match drag.tool {
                    EditorTool::Move => "Moved object",
                    EditorTool::Rotate => "Rotated object",
                    EditorTool::Scale => "Scaled object",
                    _ => "Transformed object",
                });
            }
        }

        was_dragging || self.gizmo_drag.is_some() || (primary_pressed && hovered.is_some())
    }

    pub(super) fn object_at_screen_position(
        &self,
        rect: egui::Rect,
        pos: egui::Pos2,
    ) -> Option<String> {
        let projection = self.camera_projection(rect);
        let origin_hit = self.document.as_ref().and_then(|document| {
            document
                .objects
                .iter()
                .filter_map(|object| {
                    let (screen, depth) =
                        projection.project_world_to_screen(object.transform.translation)?;
                    (screen.distance(pos) < 24.0).then_some((depth, object.id.clone()))
                })
                .min_by(|a, b| a.0.total_cmp(&b.0))
        });
        let object_hit = [
            self.object_mesh_hit_at_screen_position(rect, pos),
            origin_hit,
        ]
        .into_iter()
        .flatten()
        .min_by(|a, b| a.0.total_cmp(&b.0));
        let stage_depth = self.stage_surface_depth_at_screen_position(rect, pos);

        object_hit
            .filter(|(object_depth, _)| {
                stage_depth
                    .is_none_or(|stage_depth| *object_depth <= stage_depth + PICK_DEPTH_EPSILON)
            })
            .map(|(_, object_id)| object_id)
    }

    #[cfg(test)]
    pub(super) fn object_mesh_at_screen_position(
        &self,
        rect: egui::Rect,
        pos: egui::Pos2,
    ) -> Option<String> {
        self.object_mesh_hit_at_screen_position(rect, pos)
            .map(|(_, object_id)| object_id)
    }

    fn object_mesh_hit_at_screen_position(
        &self,
        rect: egui::Rect,
        pos: egui::Pos2,
    ) -> Option<(f32, String)> {
        if !rect.contains(pos) {
            return None;
        }

        let preview = self.model_preview.as_ref()?;
        if preview.object_model_indices.is_empty() {
            return None;
        }

        let size = framebuffer_size_for_rect(rect);
        let framebuffer_pos = [
            (pos.x - rect.left()) * size[0] as f32 / rect.width().max(1.0),
            (pos.y - rect.top()) * size[1] as f32 / rect.height().max(1.0),
        ];
        let object_ids_by_model = preview
            .object_model_indices
            .iter()
            .map(|(object_id, model_index)| (*model_index, object_id.as_str()))
            .collect::<BTreeMap<_, _>>();

        preview
            .triangles
            .iter()
            .filter_map(|triangle| {
                let object_id = object_ids_by_model.get(&triangle.model_index)?;
                let projected = self.project_preview_triangle(rect, size, triangle)?;
                let depth = projected_triangle_depth_at_point(
                    projected.screen,
                    framebuffer_pos[0],
                    framebuffer_pos[1],
                )?;
                Some((depth, *object_id))
            })
            .min_by(|a, b| a.0.total_cmp(&b.0))
            .map(|(depth, object_id)| (depth, object_id.to_string()))
    }

    fn stage_surface_depth_at_screen_position(
        &self,
        rect: egui::Rect,
        pos: egui::Pos2,
    ) -> Option<f32> {
        if !rect.contains(pos) {
            return None;
        }

        let preview = self.model_preview.as_ref()?;
        let size = framebuffer_size_for_rect(rect);
        let framebuffer_pos = [
            (pos.x - rect.left()) * size[0] as f32 / rect.width().max(1.0),
            (pos.y - rect.top()) * size[1] as f32 / rect.height().max(1.0),
        ];
        let object_model_indices = preview
            .object_model_indices
            .values()
            .copied()
            .collect::<BTreeSet<_>>();

        preview
            .triangles
            .iter()
            .filter(|triangle| {
                !object_model_indices.contains(&triangle.model_index)
                    && preview_triangle_writes_opaque_depth(preview, triangle)
            })
            .filter_map(|triangle| {
                let projected = self.project_preview_triangle(rect, size, triangle)?;
                projected_triangle_depth_at_point(
                    projected.screen,
                    framebuffer_pos[0],
                    framebuffer_pos[1],
                )
            })
            .min_by(f32::total_cmp)
    }

    pub(super) fn handle_viewport_keyboard_fly(
        &mut self,
        ui: &egui::Ui,
        navigation_active: bool,
    ) -> bool {
        let (dt, forward_key, back_key, left_key, right_key, up_key, down_key, shift) =
            ui.input(|input| {
                (
                    input.stable_dt.clamp(1.0 / 240.0, 1.0 / 15.0),
                    input.key_down(egui::Key::W),
                    input.key_down(egui::Key::S),
                    input.key_down(egui::Key::A),
                    input.key_down(egui::Key::D),
                    input.key_down(egui::Key::E),
                    input.key_down(egui::Key::Q),
                    input.modifiers.shift,
                )
            });
        let frame = self.camera_frame();
        let mut move_axis = [0.0, 0.0, 0.0];
        if navigation_active && forward_key {
            move_axis = vec3_add(move_axis, frame.forward);
        }
        if navigation_active && back_key {
            move_axis = vec3_sub(move_axis, frame.forward);
        }
        if navigation_active && right_key {
            move_axis = vec3_add(move_axis, frame.right);
        }
        if navigation_active && left_key {
            move_axis = vec3_sub(move_axis, frame.right);
        }
        if navigation_active && up_key {
            move_axis = vec3_add(move_axis, [0.0, 1.0, 0.0]);
        }
        if navigation_active && down_key {
            move_axis = vec3_sub(move_axis, [0.0, 1.0, 0.0]);
        }

        let mut speed = self.viewport_fly_speed();
        if shift {
            speed *= 4.0;
        }
        let has_input = vec3_dot(move_axis, move_axis) > 0.0001;
        let target_velocity = if has_input {
            vec3_scale(vec3_normalize(move_axis), speed)
        } else {
            [0.0; 3]
        };
        let interp_speed = if has_input {
            CAMERA_FLY_ACCELERATION_INTERP_SPEED
        } else {
            CAMERA_FLY_DECELERATION_INTERP_SPEED
        };
        self.camera_fly_velocity = interpolate_camera_velocity(
            self.camera_fly_velocity,
            target_velocity,
            dt,
            interp_speed,
        );
        if !has_input
            && vec3_dot(self.camera_fly_velocity, self.camera_fly_velocity)
                <= CAMERA_FLY_STOP_SPEED * CAMERA_FLY_STOP_SPEED
        {
            self.camera_fly_velocity = [0.0; 3];
            return false;
        }

        self.translate_camera(vec3_scale(self.camera_fly_velocity, dt));
        true
    }

    pub(super) fn viewport_fly_speed(&self) -> f32 {
        (self.renderer.camera().distance * 0.8).clamp(300.0, 80_000.0) * self.camera_speed
    }

    pub(super) fn stop_camera_fly(&mut self) {
        self.camera_fly_velocity = [0.0; 3];
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
        self.queue_camera_state_save();
    }

    pub(super) fn orbit_camera(&mut self, delta: egui::Vec2) {
        if delta == egui::Vec2::ZERO {
            return;
        }
        {
            let camera = self.renderer.camera_mut();
            camera.yaw_degrees -= delta.x * 0.18;
            camera.pitch_degrees = (camera.pitch_degrees - delta.y * 0.14).clamp(-89.0, 89.0);
        }
        self.queue_camera_state_save();
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
        self.queue_camera_state_save();
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

        if self.model_preview.is_some() || self.view_mode == ViewMode::Collision {
            self.paint_model_preview(ui.ctx(), painter, rect);
        } else {
            self.paint_stage_silhouette(painter, rect);
        }
        // The grid is an editor aid rather than part of Sunshine's EFB. Keep it
        // above the source-accurate offscreen game render.
        if self.renderer.config().show_grid {
            self.paint_grid(painter, rect);
        }
        self.paint_model_instances(painter, rect);
        self.paint_routes(painter, rect);
        self.paint_audio_helpers(painter, rect);
        self.paint_selected_object_outline(painter, rect);

        let projection = self.camera_projection(rect);
        for object in self.viewport_marker_objects() {
            let Some((pos, _)) = projection.project_world_to_screen(object.transform.translation)
            else {
                continue;
            };
            let selected = self.selected_object_id.as_deref() == Some(object.id.as_str());
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
                &object.factory_name,
                egui::FontId::proportional(12.0),
                egui::Color32::from_rgb(232, 236, 238),
            );

            if selected {
                self.paint_gizmo(painter, rect, object.transform.translation);
            }
        }

        self.paint_viewport_overlays(ui, painter, rect);
    }

    pub(super) fn viewport_marker_objects(&self) -> impl Iterator<Item = &SceneObject> {
        let show_all = self.view_mode == ViewMode::Objects;
        let selected_id = self.selected_object_id.as_deref();
        self.document
            .iter()
            .flat_map(|document| document.objects.iter())
            .filter(move |object| show_all || selected_id == Some(object.id.as_str()))
    }

    pub(super) fn selected_object_outline_segments(
        &self,
        rect: egui::Rect,
    ) -> Vec<[egui::Pos2; 2]> {
        let preview = match &self.model_preview {
            Some(preview) => preview,
            None => return Vec::new(),
        };
        let model_index = match self
            .selected_object_id
            .as_ref()
            .and_then(|id| preview.object_model_indices.get(id))
            .copied()
        {
            Some(model_index) => model_index,
            None => return Vec::new(),
        };
        let size = framebuffer_size_for_rect(rect);
        let width = size[0];
        let height = size[1];
        let projected_triangles = preview
            .triangles
            .iter()
            .filter(|triangle| triangle.model_index == model_index)
            .filter(|triangle| {
                !matches!(
                    triangle.render_layer,
                    PreviewRenderLayer::Shadow
                        | PreviewRenderLayer::Particle
                        | PreviewRenderLayer::ParticleDistortion
                )
            })
            .filter_map(|triangle| {
                let projected = self.project_preview_triangle(rect, size, triangle)?;
                let min_x = projected
                    .screen
                    .iter()
                    .map(|vertex| vertex.x)
                    .fold(f32::INFINITY, f32::min)
                    .floor()
                    .max(0.0) as usize;
                let max_x = projected
                    .screen
                    .iter()
                    .map(|vertex| vertex.x)
                    .fold(f32::NEG_INFINITY, f32::max)
                    .ceil()
                    .min(width.saturating_sub(1) as f32) as usize;
                let min_y = projected
                    .screen
                    .iter()
                    .map(|vertex| vertex.y)
                    .fold(f32::INFINITY, f32::min)
                    .floor()
                    .max(0.0) as usize;
                let max_y = projected
                    .screen
                    .iter()
                    .map(|vertex| vertex.y)
                    .fold(f32::NEG_INFINITY, f32::max)
                    .ceil()
                    .min(height.saturating_sub(1) as f32) as usize;
                if min_x > max_x || min_y > max_y {
                    return None;
                }
                Some((projected, [min_x, max_x, min_y, max_y]))
            })
            .collect::<Vec<_>>();
        let coverage_bounds = projected_triangles
            .iter()
            .map(|(_, bounds)| *bounds)
            .reduce(
                |[old_min_x, old_max_x, old_min_y, old_max_y], [min_x, max_x, min_y, max_y]| {
                    [
                        old_min_x.min(min_x),
                        old_max_x.max(max_x),
                        old_min_y.min(min_y),
                        old_max_y.max(max_y),
                    ]
                },
            );
        let Some([min_x, max_x, min_y, max_y]) = coverage_bounds else {
            return Vec::new();
        };
        let coverage_size = [max_x - min_x + 1, max_y - min_y + 1];
        let mut coverage = vec![false; coverage_size[0] * coverage_size[1]];

        for (projected, [triangle_min_x, triangle_max_x, triangle_min_y, triangle_max_y]) in
            projected_triangles
        {
            for y in triangle_min_y..=triangle_max_y {
                for x in triangle_min_x..=triangle_max_x {
                    if projected_triangle_depth_at_point(
                        projected.screen,
                        x as f32 + 0.5,
                        y as f32 + 0.5,
                    )
                    .is_some()
                    {
                        coverage[(y - min_y) * coverage_size[0] + (x - min_x)] = true;
                    }
                }
            }
        }

        outline_segments_from_bounded_coverage(
            &coverage,
            coverage_size,
            [min_x, min_y],
            size,
            [min_x, max_x, min_y, max_y],
            rect,
        )
    }

    pub(super) fn paint_selected_object_outline(&self, painter: &egui::Painter, rect: egui::Rect) {
        let segments = self.selected_object_outline_segments(rect);
        let paths = outline_paths_from_segments(&segments);
        let backing =
            egui::Stroke::new(4.25, egui::Color32::from_rgba_unmultiplied(105, 68, 0, 185));
        let highlight = egui::Stroke::new(2.5, egui::Color32::from_rgb(255, 198, 0));
        for path in &paths {
            painter.add(egui::Shape::line(path.clone(), backing));
        }
        for path in paths {
            painter.add(egui::Shape::line(path, highlight));
        }
    }

    pub(super) fn paint_grid(&self, painter: &egui::Painter, rect: egui::Rect) {
        let projection = self.camera_projection(rect);
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
            Self::paint_world_segment(
                painter,
                &projection,
                [v, 0.0, -5000.0],
                [v, 0.0, 5000.0],
                stroke,
            );
            Self::paint_world_segment(
                painter,
                &projection,
                [-5000.0, 0.0, v],
                [5000.0, 0.0, v],
                stroke,
            );
        }

        Self::paint_world_segment(
            painter,
            &projection,
            [-5200.0, 0.0, 0.0],
            [5200.0, 0.0, 0.0],
            egui::Stroke::new(2.0, egui::Color32::from_rgb(206, 82, 82)),
        );
        Self::paint_world_segment(
            painter,
            &projection,
            [0.0, 0.0, -5200.0],
            [0.0, 0.0, 5200.0],
            egui::Stroke::new(2.0, egui::Color32::from_rgb(82, 168, 110)),
        );
    }

    fn paint_world_segment(
        painter: &egui::Painter,
        projection: &super::camera::CameraProjection,
        start: [f32; 3],
        end: [f32; 3],
        stroke: egui::Stroke,
    ) {
        if let Some(segment) = projection.project_world_segment_to_screen(start, end) {
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
        if self.view_mode == ViewMode::Collision {
            if self.renderer.config().show_collision {
                self.paint_collision_preview(ctx, painter, rect);
            } else {
                painter.text(
                    rect.center(),
                    egui::Align2::CENTER_CENTER,
                    "Collision display is disabled in Viewport settings.",
                    egui::FontId::proportional(15.0),
                    egui::Color32::from_rgb(224, 213, 178),
                );
            }
            return;
        }

        self.update_level_transformation(ctx);
        self.update_skeletal_animations();
        let has_triangles = self
            .model_preview
            .as_ref()
            .is_some_and(|preview| !preview.triangles.is_empty());

        if has_triangles {
            if self.model_preview.as_ref().is_some_and(|preview| {
                !preview.texture_srt_animations.is_empty()
                    || !preview.texture_pattern_animations.is_empty()
                    || !preview.animated_models.is_empty()
                    || !preview.animated_flags.is_empty()
                    || !preview.rotating_models.is_empty()
                    || !preview.actor_particles.is_empty()
            }) {
                ctx.request_repaint();
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

    fn paint_collision_preview(
        &mut self,
        ctx: &egui::Context,
        painter: &egui::Painter,
        rect: egui::Rect,
    ) {
        let Some(preview) = self.model_preview.as_ref() else {
            painter.text(
                rect.center(),
                egui::Align2::CENTER_CENTER,
                "No collision geometry was found for this stage.",
                egui::FontId::proportional(15.0),
                egui::Color32::from_rgb(224, 213, 178),
            );
            return;
        };
        if preview.collision_triangles.is_empty() {
            painter.text(
                rect.center(),
                egui::Align2::CENTER_CENTER,
                "No collision geometry was found for this stage.",
                egui::FontId::proportional(15.0),
                egui::Color32::from_rgb(224, 213, 178),
            );
            return;
        }

        let key = self.model_framebuffer_key(rect);
        let needs_render = self.model_framebuffer.is_none() || self.model_framebuffer_key != key;
        if needs_render {
            if let Some(image) = self.render_collision_framebuffer(rect) {
                if let Some(handle) = &mut self.model_framebuffer {
                    handle.set(image, egui::TextureOptions::LINEAR);
                } else {
                    self.model_framebuffer = Some(ctx.load_texture(
                        "sms-collision-framebuffer",
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

        let label = format!(
            "Collision | {} triangle(s) | {} surface type(s)",
            preview.collision_triangles.len(),
            preview.collision_surface_count
        );
        let label_pos = rect.left_top() + egui::vec2(10.0, 10.0);
        let galley = painter.layout_no_wrap(
            label,
            egui::FontId::monospace(12.0),
            egui::Color32::from_rgb(226, 236, 232),
        );
        let label_rect = egui::Rect::from_min_size(
            label_pos - egui::vec2(6.0, 4.0),
            galley.size() + egui::vec2(12.0, 8.0),
        );
        painter.rect_filled(
            label_rect,
            4.0,
            egui::Color32::from_rgba_unmultiplied(12, 17, 19, 210),
        );
        painter.galley(label_pos, galley, egui::Color32::WHITE);
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
                ctx.request_repaint();
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
        if preview.animated_models.is_empty()
            && preview.animated_flags.is_empty()
            && preview.rotating_models.is_empty()
            && preview.actor_particles.is_empty()
            && preview.texture_pattern_animations.is_empty()
        {
            return;
        }
        self.last_skeletal_animation_tick = tick;

        let mut dirty_materials = Vec::new();
        {
            let ModelPreview {
                animated_models,
                animated_flags,
                rotating_models,
                actor_particles,
                texture_pattern_animations,
                materials,
                points,
                triangles,
                ..
            } = preview;
            for source in animated_models {
                let pose_result = source.prepared_triangles.as_ref().map_or_else(
                    || {
                        source
                            .file
                            .animated_triangles_and_joint_matrices_with_joint_animation(
                                source.loader_flags,
                                &source.animation,
                                elapsed_seconds,
                            )
                    },
                    |prepared| {
                        source.file.animate_prepared_triangles_with_joint_animation(
                            prepared,
                            source.loader_flags,
                            &source.animation,
                            elapsed_seconds,
                        )
                    },
                );
                let Ok((posed_triangles, joint_matrices)) = pose_result else {
                    continue;
                };
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
                        triangle.billboard = posed.billboard.and_then(|billboard| {
                            transform_j3d_billboard(
                                billboard,
                                instance.transform,
                                posed.normals.map(|normals| {
                                    transform_preview_normals(normals, instance.transform)
                                }),
                            )
                        });
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
                                    .prepared_triangles
                                    .as_ref()
                                    .map_or_else(
                                        || {
                                            accessory.file.animated_triangles_with_joint_animation(
                                                accessory.loader_flags,
                                                animation.as_ref(),
                                                elapsed_seconds,
                                            )
                                        },
                                        |prepared| {
                                            accessory
                                                .file
                                                .animate_prepared_triangles_with_joint_animation(
                                                    prepared,
                                                    accessory.loader_flags,
                                                    animation.as_ref(),
                                                    elapsed_seconds,
                                                )
                                                .map(|(triangles, _)| triangles)
                                        },
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
                            triangle.billboard = local.billboard.and_then(|billboard| {
                                let joint_normals = local.normals.map(|normals| {
                                    normals.map(|normal| {
                                        transform_j3d_matrix_normal(joint_matrix, normal)
                                    })
                                });
                                let billboard = transform_j3d_billboard_matrix(
                                    billboard,
                                    joint_matrix,
                                    joint_normals,
                                )?;
                                transform_j3d_billboard(
                                    billboard,
                                    instance.transform,
                                    joint_normals.map(|normals| {
                                        transform_preview_normals(normals, instance.transform)
                                    }),
                                )
                            });
                        }
                    }
                }
            }
            for source in rotating_models {
                for instance in &source.instances {
                    let transform = runtime_rotated_transform(
                        instance.transform,
                        elapsed_seconds,
                        instance.runtime_yaw_degrees_per_frame,
                    );
                    for (point, position) in points[instance.point_range.clone()]
                        .iter_mut()
                        .zip(source.positions.iter().step_by(instance.point_stride))
                    {
                        point.position = transform_preview_point(*position, transform);
                    }
                    for (triangle, local) in
                        triangles[instance.triangle_range.clone()].iter_mut().zip(
                            source
                                .triangles
                                .iter()
                                .filter(|triangle| triangle_vertices_are_finite(triangle.vertices)),
                        )
                    {
                        triangle.vertices = transform_preview_vertices(local.vertices, transform);
                        triangle.normals = local
                            .normals
                            .map(|normals| transform_preview_normals(normals, transform));
                        triangle.billboard = local.billboard.and_then(|billboard| {
                            transform_j3d_billboard(
                                billboard,
                                transform,
                                local
                                    .normals
                                    .map(|normals| transform_preview_normals(normals, transform)),
                            )
                        });
                    }
                }
            }
            for source in animated_flags {
                animate_flag_preview(source, tick as f32, points, triangles);
            }
            apply_actor_particles(
                actor_particles,
                elapsed_seconds * SMS_ANIMATION_FRAMES_PER_SECOND,
                triangles,
            );
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
                .chain(preview.rotating_models.iter().flat_map(|model| {
                    model
                        .instances
                        .iter()
                        .map(|instance| instance.triangle_range.clone())
                }))
                .chain(
                    preview
                        .animated_flags
                        .iter()
                        .map(|flag| flag.triangle_range.clone()),
                )
                .chain(
                    preview
                        .actor_particles
                        .iter()
                        .map(|particles| particles.triangle_range.clone()),
                )
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
        self.model_preview.as_ref()?;
        let frame = self.camera_frame();
        let focal = perspective_focal_length(rect, self.viewport_zoom);
        let lighting = self
            .document
            .as_ref()
            .and_then(|document| document.lighting.object_lighting());
        Some(gpu_viewport::GpuViewportFrame {
            camera_position: frame.position,
            right: frame.right,
            up: frame.up,
            forward: frame.forward,
            focal,
            viewport_size: [rect.width().max(1.0), rect.height().max(1.0)],
            viewport_pan: [self.viewport_pan.x, self.viewport_pan.y],
            near: VIEWPORT_NEAR_CLIP,
            animation_seconds: self.animation_started_at.elapsed().as_secs_f32(),
            light_position: lighting
                .map(|lighting| lighting.position)
                .unwrap_or([200_000.0, 500_000.0, 200_000.0]),
            light_color: lighting
                .map(|lighting| gpu_viewport::color_u8_to_f32(lighting.color))
                .unwrap_or([1.0; 4]),
            ambient_color: lighting.map(|lighting| gpu_viewport::color_u8_to_f32(lighting.ambient)),
        })
    }

    pub(super) fn paint_point_model_preview(
        &self,
        painter: &egui::Painter,
        rect: egui::Rect,
        preview: &ModelPreview,
    ) {
        let projection = self.camera_projection(rect);
        let stroke =
            egui::Stroke::new(1.0, egui::Color32::from_rgba_unmultiplied(91, 190, 173, 95));
        for pair in preview.points.windows(2) {
            if pair[0].model_index != pair[1].model_index {
                continue;
            }

            if let Some([a, b]) =
                projection.project_world_segment_to_screen(pair[0].position, pair[1].position)
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
            if let Some((screen, _)) = projection.project_world_to_screen(point.position) {
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
        let camera_position = self.camera_frame().position;
        let triangle_is_visible = |triangle: &PreviewTriangle| {
            triangle.render_layer != PreviewRenderLayer::MirrorSurface
                || preview.mirror_surface_model_is_visible(triangle.model_index, camera_position)
        };

        for triangle in preview
            .triangles
            .iter()
            .filter(|triangle| triangle_is_visible(triangle))
            .filter(|triangle| triangle.render_layer == PreviewRenderLayer::Sky)
        {
            if let Some(projected) = self.project_preview_triangle(rect, size, triangle) {
                rasterize_projected_preview_triangle(
                    preview, &mut image, &mut depth, projected, false,
                );
            }
        }

        for triangle in preview
            .triangles
            .iter()
            .filter(|triangle| triangle_is_visible(triangle))
            .filter(|triangle| preview_triangle_writes_opaque_depth(preview, triangle))
        {
            if let Some(projected) = self.project_preview_triangle(rect, size, triangle) {
                rasterize_projected_preview_triangle(
                    preview, &mut image, &mut depth, projected, true,
                );
            }
        }

        let mut translucent: Vec<_> = preview
            .triangles
            .iter()
            .filter(|triangle| triangle_is_visible(triangle))
            .filter(|triangle| {
                !matches!(
                    triangle.render_layer,
                    PreviewRenderLayer::Sky
                        | PreviewRenderLayer::MirrorScene
                        | PreviewRenderLayer::WaveFoam
                        | PreviewRenderLayer::IndirectWater
                ) && preview_triangle_is_translucent(preview, triangle)
            })
            .filter_map(|triangle| self.project_preview_triangle(rect, size, triangle))
            .collect();
        translucent.sort_by(|a, b| b.average_depth.total_cmp(&a.average_depth));
        for projected in translucent {
            rasterize_projected_preview_triangle(preview, &mut image, &mut depth, projected, false);
        }

        Some(image)
    }

    pub(super) fn render_collision_framebuffer(
        &self,
        rect: egui::Rect,
    ) -> Option<egui::ColorImage> {
        let preview = self.model_preview.as_ref()?;
        if preview.collision_triangles.is_empty() || rect.width() < 2.0 || rect.height() < 2.0 {
            return None;
        }

        let size = framebuffer_size_for_rect(rect);
        let mut image = viewport_framebuffer_background(size);
        let mut depth = vec![f32::INFINITY; size[0] * size[1]];
        let mut projected_triangles = Vec::new();

        for triangle in &preview.collision_triangles {
            let Some(screen) = project_triangle_to_framebuffer(self, rect, size, triangle.vertices)
            else {
                continue;
            };
            if !projected_triangle_overlaps_frame(screen, size) {
                continue;
            }
            let color = shaded_collision_color(triangle);
            rasterize_preview_triangle(
                &mut image, &mut depth, screen, None, None, [color; 3], true, None, false,
            );
            projected_triangles.push(screen);
        }

        let edge_color = egui::Color32::from_rgb(24, 31, 33);
        for screen in projected_triangles {
            for [start, end] in [
                [screen[0], screen[1]],
                [screen[1], screen[2]],
                [screen[2], screen[0]],
            ] {
                rasterize_depth_tested_segment(&mut image, &depth, start, end, edge_color);
            }
        }

        Some(image)
    }

    pub(super) fn model_framebuffer_key(&self, rect: egui::Rect) -> Option<ModelFramebufferKey> {
        let preview = self.model_preview.as_ref()?;
        let camera = self.renderer.camera();
        Some(ModelFramebufferKey {
            stage_id: self.stage_id.clone(),
            view_mode: self.view_mode,
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
            collision_triangle_count: preview.collision_triangles.len(),
        })
    }

    pub(super) fn project_preview_triangle<'a>(
        &self,
        rect: egui::Rect,
        size: [usize; 2],
        triangle: &'a PreviewTriangle,
    ) -> Option<ProjectedPreviewTriangle<'a>> {
        let camera = self.camera_frame();
        let vertices = triangle.billboard.map_or_else(
            || {
                preview_triangle_world_vertices(
                    triangle.vertices,
                    triangle.render_layer,
                    camera.position,
                )
            },
            |billboard| j3d_billboard_world_vertices(billboard, camera),
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
        let projection = self.camera_projection(rect);
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
            Self::paint_world_segment(painter, &projection, corners[a], corners[b], stroke);
        }
    }

    fn gizmo_geometry(&self, rect: egui::Rect, world_origin: [f32; 3]) -> Option<GizmoGeometry> {
        let projection = self.camera_projection(rect);
        let (origin, depth) = projection.project_world_to_screen(world_origin)?;
        let focal = perspective_focal_length(rect, self.viewport_zoom).max(1.0);
        let world_units_per_pixel = (depth / focal).max(0.001);
        let world_axis_length = GIZMO_AXIS_LENGTH_PIXELS * world_units_per_pixel;
        let axes = std::array::from_fn(|index| {
            let axis = [GizmoAxis::X, GizmoAxis::Y, GizmoAxis::Z][index];
            let end_world = vec3_add(
                world_origin,
                vec3_scale(axis.world_direction(), world_axis_length),
            );
            let end = projection
                .project_world_to_screen(end_world)
                .map_or(origin, |(screen, _)| screen);
            let delta = end - origin;
            let direction = if delta.length_sq() > 0.0001 {
                delta.normalized()
            } else {
                egui::Vec2::ZERO
            };
            GizmoScreenAxis {
                start: origin + direction * 8.0,
                end,
                direction,
            }
        });
        let ring_radius = GIZMO_RING_RADIUS_PIXELS * world_units_per_pixel;
        let rotation_rings = std::array::from_fn(|axis_index| {
            let axis = [GizmoAxis::X, GizmoAxis::Y, GizmoAxis::Z][axis_index];
            (0..=64)
                .filter_map(|step| {
                    let angle = step as f32 / 64.0 * std::f32::consts::TAU;
                    let (sin, cos) = angle.sin_cos();
                    let offset = match axis {
                        GizmoAxis::X => [0.0, cos * ring_radius, sin * ring_radius],
                        GizmoAxis::Y => [cos * ring_radius, 0.0, sin * ring_radius],
                        GizmoAxis::Z => [cos * ring_radius, sin * ring_radius, 0.0],
                    };
                    projection
                        .project_world_to_screen(vec3_add(world_origin, offset))
                        .map(|(screen, _)| screen)
                })
                .collect()
        });
        Some(GizmoGeometry {
            origin,
            axes,
            rotation_rings,
            world_units_per_pixel,
        })
    }

    pub(super) fn paint_gizmo(
        &self,
        painter: &egui::Painter,
        rect: egui::Rect,
        world_origin: [f32; 3],
    ) {
        if !matches!(
            self.tool,
            EditorTool::Move | EditorTool::Rotate | EditorTool::Scale
        ) {
            return;
        }
        let Some(geometry) = self.gizmo_geometry(rect, world_origin) else {
            return;
        };
        let active_axis = self
            .gizmo_drag
            .map(|drag| drag.axis)
            .or(self.hovered_gizmo_axis);
        let axes = [GizmoAxis::X, GizmoAxis::Y, GizmoAxis::Z];

        if self.tool == EditorTool::Rotate {
            for axis in axes {
                let points = &geometry.rotation_rings[axis.index()];
                if points.len() < 2 {
                    continue;
                }
                painter.add(egui::Shape::line(
                    points.clone(),
                    egui::Stroke::new(5.0, egui::Color32::from_black_alpha(150)),
                ));
                let color = gizmo_axis_color(axis, active_axis == Some(axis));
                painter.add(egui::Shape::line(
                    points.clone(),
                    egui::Stroke::new(if active_axis == Some(axis) { 4.0 } else { 2.5 }, color),
                ));
                if let Some(label_position) = points.first() {
                    painter.text(
                        *label_position,
                        egui::Align2::CENTER_CENTER,
                        axis.label(),
                        egui::FontId::proportional(12.0),
                        color,
                    );
                }
            }
        } else {
            for axis in axes {
                let screen_axis = geometry.axes[axis.index()];
                let highlighted = active_axis == Some(axis);
                let color = gizmo_axis_color(axis, highlighted);
                painter.line_segment(
                    [screen_axis.start, screen_axis.end],
                    egui::Stroke::new(6.0, egui::Color32::from_black_alpha(155)),
                );
                if self.tool == EditorTool::Move {
                    painter.arrow(
                        screen_axis.start,
                        screen_axis.end - screen_axis.start,
                        egui::Stroke::new(if highlighted { 4.5 } else { 3.0 }, color),
                    );
                } else {
                    painter.line_segment(
                        [screen_axis.start, screen_axis.end],
                        egui::Stroke::new(if highlighted { 4.5 } else { 3.0 }, color),
                    );
                    let handle = egui::Rect::from_center_size(
                        screen_axis.end,
                        egui::vec2(
                            if highlighted { 13.0 } else { 10.0 },
                            if highlighted { 13.0 } else { 10.0 },
                        ),
                    );
                    painter.rect_filled(handle, 1.0, color);
                    painter.rect_stroke(
                        handle,
                        1.0,
                        egui::Stroke::new(1.0, egui::Color32::from_black_alpha(180)),
                        egui::StrokeKind::Inside,
                    );
                }
                painter.text(
                    screen_axis.end + screen_axis.direction * 10.0,
                    egui::Align2::CENTER_CENTER,
                    axis.label(),
                    egui::FontId::proportional(12.0),
                    color,
                );
            }
        }
        painter.circle_filled(
            geometry.origin,
            if active_axis.is_some() { 6.0 } else { 4.5 },
            egui::Color32::from_rgb(235, 238, 240),
        );
        painter.circle_stroke(
            geometry.origin,
            7.0,
            egui::Stroke::new(1.5, egui::Color32::from_black_alpha(180)),
        );
    }

    pub(super) fn paint_viewport_overlays(
        &self,
        ui: &egui::Ui,
        painter: &egui::Painter,
        rect: egui::Rect,
    ) {
        if self.show_fps {
            ui.ctx().request_repaint();
            let stable_dt = ui.input(|input| input.stable_dt).max(0.000_001);
            let fps = 1.0 / stable_dt;
            let text = format!("{fps:.0} FPS");
            let anchor = rect.right_top() + egui::vec2(-12.0, 12.0);
            painter.text(
                anchor + egui::vec2(1.0, 1.0),
                egui::Align2::RIGHT_TOP,
                &text,
                egui::FontId::monospace(14.0),
                egui::Color32::from_black_alpha(210),
            );
            painter.text(
                anchor,
                egui::Align2::RIGHT_TOP,
                text,
                egui::FontId::monospace(14.0),
                egui::Color32::from_rgb(235, 241, 242),
            );
        }
        if self.show_stats {
            let overlay = egui::Rect::from_min_size(
                rect.min + egui::vec2(16.0, 16.0),
                egui::vec2(280.0, 118.0),
            );
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
                .render_scene
                .as_ref()
                .map(|scene| {
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
            painter.text(
                overlay.min + egui::vec2(14.0, 90.0),
                egui::Align2::LEFT_TOP,
                format!("RMB + WASD/QE  wheel speed {:.2}x", self.camera_speed),
                egui::FontId::monospace(11.0),
                egui::Color32::from_rgb(160, 176, 179),
            );
        }

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

        if let Some(placement) = self
            .active_placement
            .as_ref()
            .filter(|_| self.tool == EditorTool::Place)
        {
            let text = match placement {
                ActivePlacement::Object { factory_name } => factory_name.clone(),
                ActivePlacement::Model { asset_id } => self
                    .model_catalog_entries
                    .iter()
                    .find(|entry| entry.id == *asset_id)
                    .map(|entry| entry.name.clone())
                    .unwrap_or_else(|| "model asset".to_string()),
            };
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
                format!("Placing {text} — click to confirm"),
                egui::FontId::proportional(13.0),
                egui::Color32::from_rgb(215, 238, 231),
            );
        }
    }
}

fn gizmo_axis_color(axis: GizmoAxis, highlighted: bool) -> egui::Color32 {
    if highlighted {
        return egui::Color32::from_rgb(255, 224, 92);
    }
    match axis {
        GizmoAxis::X => egui::Color32::from_rgb(238, 72, 72),
        GizmoAxis::Y => egui::Color32::from_rgb(76, 202, 105),
        GizmoAxis::Z => egui::Color32::from_rgb(72, 139, 242),
    }
}

fn gizmo_axis_at_pointer(
    tool: EditorTool,
    geometry: &GizmoGeometry,
    pointer: egui::Pos2,
) -> Option<GizmoAxis> {
    let mut best: Option<(f32, GizmoAxis)> = None;
    for axis in [GizmoAxis::X, GizmoAxis::Y, GizmoAxis::Z] {
        let distance = if tool == EditorTool::Rotate {
            geometry.rotation_rings[axis.index()]
                .windows(2)
                .map(|segment| point_segment_distance(pointer, segment[0], segment[1]))
                .min_by(f32::total_cmp)
                .unwrap_or(f32::INFINITY)
        } else if matches!(tool, EditorTool::Move | EditorTool::Scale) {
            let screen_axis = geometry.axes[axis.index()];
            point_segment_distance(pointer, screen_axis.start, screen_axis.end)
        } else {
            f32::INFINITY
        };
        if distance <= GIZMO_HIT_RADIUS_PIXELS
            && best.is_none_or(|(best_distance, _)| distance < best_distance)
        {
            best = Some((distance, axis));
        }
    }
    best.map(|(_, axis)| axis)
}

fn point_segment_distance(point: egui::Pos2, start: egui::Pos2, end: egui::Pos2) -> f32 {
    let segment = end - start;
    let length_squared = segment.length_sq();
    if length_squared <= f32::EPSILON {
        return point.distance(start);
    }
    let offset = point - start;
    let projection =
        ((offset.x * segment.x + offset.y * segment.y) / length_squared).clamp(0.0, 1.0);
    point.distance(start + segment * projection)
}

pub(super) fn transform_from_gizmo_drag(
    drag: GizmoDrag,
    pointer: egui::Pos2,
    snap_enabled: bool,
    translation_snap: f32,
    rotation_snap: f32,
    scale_snap: f32,
) -> Transform {
    let mut transform = drag.start_transform;
    let pointer_delta = pointer - drag.start_pointer;
    let projected_pixels =
        pointer_delta.x * drag.screen_direction.x + pointer_delta.y * drag.screen_direction.y;
    let axis = drag.axis.index();
    match drag.tool {
        EditorTool::Move => {
            let mut value = drag.start_transform.translation[axis]
                + projected_pixels * drag.world_units_per_pixel;
            if snap_enabled && translation_snap > f32::EPSILON {
                value = snap_value(value, translation_snap);
            }
            transform.translation[axis] = value;
        }
        EditorTool::Rotate => {
            let start = drag.start_pointer - drag.screen_origin;
            let current = pointer - drag.screen_origin;
            let mut delta_degrees = if start.length_sq() > 16.0 && current.length_sq() > 16.0 {
                let cross = start.x * current.y - start.y * current.x;
                let dot = start.x * current.x + start.y * current.y;
                cross.atan2(dot).to_degrees()
            } else {
                (pointer_delta.x - pointer_delta.y) * 0.5
            };
            let mut value = drag.start_transform.rotation_degrees[axis] + delta_degrees;
            if snap_enabled && rotation_snap > f32::EPSILON {
                value = snap_value(value, rotation_snap);
                delta_degrees = value - drag.start_transform.rotation_degrees[axis];
            }
            transform.rotation_degrees[axis] =
                (drag.start_transform.rotation_degrees[axis] + delta_degrees).rem_euclid(360.0);
        }
        EditorTool::Scale => {
            let response_scale = drag.start_transform.scale[axis].abs().max(0.25);
            let mut value = drag.start_transform.scale[axis]
                + projected_pixels / GIZMO_AXIS_LENGTH_PIXELS * response_scale;
            if snap_enabled && scale_snap > f32::EPSILON {
                value = snap_value(value, scale_snap);
            }
            transform.scale[axis] = value.max(0.001);
        }
        EditorTool::Select | EditorTool::Place => {}
    }
    transform
}

pub(super) fn camera_speed_after_scroll(current: f32, scroll_delta: f32) -> f32 {
    if !current.is_finite() || !scroll_delta.is_finite() {
        return current.clamp(CAMERA_SPEED_MIN, CAMERA_SPEED_MAX);
    }
    (current * 2.0_f32.powf(scroll_delta / CAMERA_SPEED_SCROLL_SCALE))
        .clamp(CAMERA_SPEED_MIN, CAMERA_SPEED_MAX)
}

pub(super) fn interpolate_camera_velocity(
    current: [f32; 3],
    target: [f32; 3],
    dt: f32,
    interp_speed: f32,
) -> [f32; 3] {
    if !dt.is_finite() || !interp_speed.is_finite() || dt <= 0.0 || interp_speed <= 0.0 {
        return current;
    }
    let alpha = 1.0 - (-interp_speed * dt).exp();
    [
        current[0] + (target[0] - current[0]) * alpha,
        current[1] + (target[1] - current[1]) * alpha,
        current[2] + (target[2] - current[2]) * alpha,
    ]
}
