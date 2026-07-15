use super::*;

#[derive(Debug, Clone, Copy)]
pub(super) struct CameraProjection {
    frame: CameraFrame,
    screen_center: egui::Pos2,
    focal: f32,
}

impl CameraProjection {
    pub(super) fn project_world_to_screen(&self, point: [f32; 3]) -> Option<(egui::Pos2, f32)> {
        let rel = vec3_sub(point, self.frame.position);
        let depth = vec3_dot(rel, self.frame.forward);
        if depth < VIEWPORT_NEAR_CLIP || !depth.is_finite() {
            return None;
        }

        let x = vec3_dot(rel, self.frame.right) / depth * self.focal;
        let y = vec3_dot(rel, self.frame.up) / depth * self.focal;
        if !x.is_finite() || !y.is_finite() {
            return None;
        }

        Some((self.screen_center + egui::vec2(x, -y), depth))
    }

    pub(super) fn project_world_segment_to_screen(
        &self,
        start: [f32; 3],
        end: [f32; 3],
    ) -> Option<[egui::Pos2; 2]> {
        let [start, end] =
            clip_world_segment_to_near_plane(self.frame, start, end, VIEWPORT_NEAR_CLIP)?;
        Some([
            self.project_world_to_screen(start)?.0,
            self.project_world_to_screen(end)?.0,
        ])
    }
}

impl SmsEditorApp {
    pub(super) fn issue_counts(&self) -> (usize, usize) {
        let warnings = self
            .issues
            .iter()
            .filter(|issue| issue.severity == ValidationSeverity::Warning)
            .count();
        let errors = self
            .issues
            .iter()
            .filter(|issue| issue.severity == ValidationSeverity::Error)
            .count();
        (warnings, errors)
    }

    pub(super) fn world_to_screen(&self, rect: egui::Rect, point: [f32; 3]) -> egui::Pos2 {
        self.project_world_to_screen(rect, point)
            .map(|(screen, _)| screen)
            .unwrap_or(rect.center() + self.viewport_pan)
    }

    pub(super) fn project_world_to_screen(
        &self,
        rect: egui::Rect,
        point: [f32; 3],
    ) -> Option<(egui::Pos2, f32)> {
        self.camera_projection(rect).project_world_to_screen(point)
    }

    pub(super) fn camera_projection(&self, rect: egui::Rect) -> CameraProjection {
        CameraProjection {
            frame: self.camera_frame(),
            screen_center: rect.center() + self.viewport_pan,
            focal: perspective_focal_length(rect, self.viewport_zoom),
        }
    }

    pub(super) fn camera_frame(&self) -> CameraFrame {
        let camera = self.renderer.camera();
        let yaw = camera.yaw_degrees.to_radians();
        let pitch = camera.pitch_degrees.to_radians();
        let forward = vec3_normalize([
            yaw.sin() * pitch.cos(),
            pitch.sin(),
            yaw.cos() * pitch.cos(),
        ]);
        let right = vec3_normalize([-yaw.cos(), 0.0, yaw.sin()]);
        let up = vec3_normalize(vec3_cross(right, forward));
        let position = vec3_sub(camera.focus, vec3_scale(forward, camera.distance));
        CameraFrame {
            position,
            right,
            up,
            forward,
        }
    }

    pub(super) fn screen_to_world_floor(&self, rect: egui::Rect, pos: egui::Pos2) -> [f32; 3] {
        let frame = self.camera_frame();
        let floor_y = self.renderer.camera().focus[1];
        let focal = perspective_focal_length(rect, self.viewport_zoom);
        let local = pos - rect.center() - self.viewport_pan;
        let ray = vec3_normalize(vec3_add(
            frame.forward,
            vec3_add(
                vec3_scale(frame.right, local.x / focal),
                vec3_scale(frame.up, -local.y / focal),
            ),
        ));
        if ray[1].abs() < 0.0001 {
            return self.renderer.camera().focus;
        }
        let t = (floor_y - frame.position[1]) / ray[1];
        if !t.is_finite() || t <= 0.0 {
            return self.renderer.camera().focus;
        }

        vec3_add(frame.position, vec3_scale(ray, t))
    }
}
