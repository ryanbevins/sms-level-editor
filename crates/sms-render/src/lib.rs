//! Renderer-facing scene, camera, and viewport configuration boundaries.
//!
//! The egui/wgpu callback backend remains in the desktop application because it
//! depends on eframe integration. This crate owns the editor-independent scene
//! and camera contract shared by render frontends.

use serde::{Deserialize, Serialize};
use sms_scene::{AssetRole, StageDocument};

/// Optional intermediate targets required by the scene's GX material passes.
///
/// Frontends can use this contract to avoid allocating full-size render targets
/// for effects that are not present in the active scene.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ViewportTargetFeatures {
    pub screen_copy: bool,
    pub wave_mask: bool,
    pub mirror: bool,
}

/// How faithfully a GX blend mode can be represented by wgpu's blend state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GxBlendCompatibility {
    Native,
    /// This logic operation depends only on the incoming fragment (or a
    /// constant), so a frontend can reproduce it without reading the current
    /// framebuffer value.
    SourceIndependentLogicOperation {
        logic_operation: u8,
    },
    /// GX logic operations act on the existing framebuffer value. WebGPU does
    /// not expose fixed-function framebuffer logic operations.
    UnsupportedLogicOperation {
        logic_operation: u8,
    },
    UnsupportedMode {
        mode: u8,
    },
}

pub fn gx_blend_compatibility(mode: u8, logic_operation: u8) -> GxBlendCompatibility {
    match mode {
        0 | 1 | 3 => GxBlendCompatibility::Native,
        2 if matches!(logic_operation, 0 | 3 | 5 | 12 | 15) => {
            GxBlendCompatibility::SourceIndependentLogicOperation { logic_operation }
        }
        2 => GxBlendCompatibility::UnsupportedLogicOperation { logic_operation },
        mode => GxBlendCompatibility::UnsupportedMode { mode },
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RendererConfig {
    pub show_collision: bool,
    pub show_object_bounds: bool,
    pub show_grid: bool,
    pub clear_color: [f32; 4],
}

impl Default for RendererConfig {
    fn default() -> Self {
        Self {
            show_collision: true,
            show_object_bounds: false,
            show_grid: true,
            clear_color: [0.04, 0.045, 0.05, 1.0],
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RenderScene {
    pub stage_id: String,
    pub model_paths: Vec<String>,
    pub collision_paths: Vec<String>,
    pub object_count: usize,
}

impl RenderScene {
    pub fn from_document(document: &StageDocument) -> Self {
        let mut model_paths = Vec::new();
        let mut collision_paths = Vec::new();

        for asset in &document.assets {
            let path = asset.path.to_string_lossy().to_string();
            match asset
                .path
                .extension()
                .and_then(|ext| ext.to_str())
                .map(|ext| ext.to_ascii_lowercase())
                .as_deref()
            {
                Some("bmd" | "bdl") => model_paths.push(path),
                Some("col") => collision_paths.push(path),
                _ => {}
            }
        }

        for object in &document.objects {
            for hint in &object.asset_hints {
                match hint.role {
                    AssetRole::PreviewModel | AssetRole::InferredPreviewModel => {
                        model_paths.push(hint.path.clone())
                    }
                    AssetRole::Collision => collision_paths.push(hint.path.clone()),
                    _ => {}
                }
            }
        }

        model_paths.sort();
        model_paths.dedup();
        collision_paths.sort();
        collision_paths.dedup();

        Self {
            stage_id: document.stage_id.clone(),
            model_paths,
            collision_paths,
            object_count: document.objects.len(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ViewportCamera {
    pub focus: [f32; 3],
    pub yaw_degrees: f32,
    pub pitch_degrees: f32,
    pub distance: f32,
}

impl Default for ViewportCamera {
    fn default() -> Self {
        Self {
            focus: [0.0, 0.0, 0.0],
            yaw_degrees: 45.0,
            pitch_degrees: -30.0,
            distance: 5000.0,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ViewportDrawList {
    pub labels: Vec<String>,
    pub grid_enabled: bool,
    pub collision_enabled: bool,
    pub bounds_enabled: bool,
}

pub struct ViewportRenderer {
    config: RendererConfig,
    camera: ViewportCamera,
}

impl ViewportRenderer {
    pub fn new(config: RendererConfig) -> Self {
        Self {
            config,
            camera: ViewportCamera::default(),
        }
    }

    pub fn config(&self) -> &RendererConfig {
        &self.config
    }

    pub fn config_mut(&mut self) -> &mut RendererConfig {
        &mut self.config
    }

    pub fn camera(&self) -> &ViewportCamera {
        &self.camera
    }

    pub fn camera_mut(&mut self) -> &mut ViewportCamera {
        &mut self.camera
    }

    pub fn build_draw_list(&self, scene: &RenderScene) -> ViewportDrawList {
        let mut labels = vec![format!("Stage {}", scene.stage_id)];
        labels.push(format!("{} model asset(s)", scene.model_paths.len()));
        labels.push(format!(
            "{} collision asset(s)",
            scene.collision_paths.len()
        ));
        labels.push(format!("{} object(s)", scene.object_count));

        ViewportDrawList {
            labels,
            grid_enabled: self.config.show_grid,
            collision_enabled: self.config.show_collision,
            bounds_enabled: self.config.show_object_bounds,
        }
    }

    pub fn preferred_backends() -> wgpu::Backends {
        wgpu::Backends::all()
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::path::PathBuf;

    use sms_scene::StageDocument;

    use super::*;

    #[test]
    fn creates_draw_list_from_empty_document() {
        let document = StageDocument {
            stage_id: "dolpic".to_string(),
            base_root: PathBuf::from("."),
            assets: vec![],
            objects: vec![],
            changed_files: BTreeMap::new(),
            registry: None,
            load_issues: Vec::new(),
            lighting: Default::default(),
            actor_previews: BTreeMap::new(),
            loaded_project: None,
        };
        let scene = RenderScene::from_document(&document);
        let renderer = ViewportRenderer::new(RendererConfig::default());
        let draw_list = renderer.build_draw_list(&scene);
        assert!(draw_list
            .labels
            .iter()
            .any(|label| label.contains("dolpic")));
    }

    #[test]
    fn classifies_source_independent_and_framebuffer_dependent_gx_logic_operations() {
        for logic_operation in [0, 3, 5, 12, 15] {
            assert_eq!(
                gx_blend_compatibility(2, logic_operation),
                GxBlendCompatibility::SourceIndependentLogicOperation { logic_operation }
            );
        }
        assert_eq!(
            gx_blend_compatibility(2, 6),
            GxBlendCompatibility::UnsupportedLogicOperation { logic_operation: 6 }
        );
        assert_eq!(gx_blend_compatibility(1, 6), GxBlendCompatibility::Native);
        assert_eq!(
            gx_blend_compatibility(9, 0),
            GxBlendCompatibility::UnsupportedMode { mode: 9 }
        );
    }
}
