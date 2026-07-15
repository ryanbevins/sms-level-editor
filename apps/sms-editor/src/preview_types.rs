use super::*;

#[derive(Clone)]
pub(super) struct ModelPreview {
    pub(super) points: Vec<PreviewPoint>,
    pub(super) triangles: Vec<PreviewTriangle>,
    pub(super) textures: Vec<PreviewTexture>,
    pub(super) materials: Vec<J3dMaterial>,
    pub(super) texture_srt_animations: Vec<J3dTextureSrtAnimation>,
    pub(super) texture_pattern_animations: Vec<PreviewTexturePatternAnimation>,
    pub(super) material_animation_bindings: Vec<Vec<PreviewMaterialAnimationBinding>>,
    pub(super) bounds_min: [f32; 3],
    pub(super) bounds_max: [f32; 3],
    pub(super) camera_bounds_min: [f32; 3],
    pub(super) camera_bounds_max: [f32; 3],
    pub(super) loaded_models: usize,
    pub(super) failed_models: usize,
    pub(super) source_vertices: usize,
    pub(super) source_triangles: usize,
    pub(super) source_textures: usize,
    pub(super) object_model_indices: BTreeMap<String, usize>,
    pub(super) animated_models: Vec<AnimatedModelPreview>,
    pub(super) rotating_models: Vec<RuntimeRotatingModelPreview>,
    pub(super) level_transform_models: Vec<LevelTransformModelPreview>,
    pub(super) level_transform_particles: Vec<LevelTransformParticlePreview>,
    pub(super) actor_particles: Vec<LevelTransformParticlePreview>,
    pub(super) level_transform_duration_frames: f32,
    pub(super) level_transform_particle_end_frames: f32,
}

#[derive(Debug, Clone, Copy)]
pub(super) struct PreviewMaterialAnimationBinding {
    pub(super) animation_index: usize,
    pub(super) binding_index: usize,
}

#[derive(Clone)]
pub(super) struct PreviewTexturePatternAnimation {
    pub(super) animation: J3dTexturePatternAnimation,
    pub(super) phase_seconds: f32,
    pub(super) bindings: Vec<PreviewTexturePatternBinding>,
}

#[derive(Clone)]
pub(super) struct PreviewTexturePatternBinding {
    pub(super) material_index: usize,
    pub(super) texture_slot: usize,
    pub(super) texture_base: usize,
    pub(super) animation_binding_index: usize,
    pub(super) current_texture_index: Option<usize>,
}

impl ModelPreview {
    pub(super) fn has_level_transformation(&self) -> bool {
        !self.level_transform_models.is_empty() || !self.level_transform_particles.is_empty()
    }

    pub(super) fn center(&self) -> [f32; 3] {
        [
            (self.camera_bounds_min[0] + self.camera_bounds_max[0]) * 0.5,
            (self.camera_bounds_min[1] + self.camera_bounds_max[1]) * 0.5,
            (self.camera_bounds_min[2] + self.camera_bounds_max[2]) * 0.5,
        ]
    }

    pub(super) fn radius(&self) -> f32 {
        let dx = self.camera_bounds_max[0] - self.camera_bounds_min[0];
        let dy = self.camera_bounds_max[1] - self.camera_bounds_min[1];
        let dz = self.camera_bounds_max[2] - self.camera_bounds_min[2];
        ((dx * dx + dy * dy + dz * dz).sqrt() * 0.5).max(1000.0)
    }
}

#[derive(Clone)]
pub(super) struct LevelTransformParticlePreview {
    pub(super) effect: JpaEffect,
    pub(super) kind: JpaParticleKind,
    pub(super) shared_simulation_id: Option<u16>,
    pub(super) origin_offset: [f32; 3],
    pub(super) triangle_range: std::ops::Range<usize>,
    pub(super) particle_capacity: usize,
    pub(super) model_index: Option<usize>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(super) enum JpaParticleKind {
    Parent,
    Child,
}

#[derive(Debug, Clone, Copy)]
pub(super) struct PreviewPoint {
    pub(super) position: [f32; 3],
    pub(super) model_index: usize,
}

#[derive(Debug, Clone, Copy)]
pub(super) struct PreviewTriangle {
    pub(super) vertices: [[f32; 3]; 3],
    pub(super) normals: Option<[[f32; 3]; 3]>,
    pub(super) color_channels: [Option<[[u8; 4]; 3]>; 2],
    pub(super) tex_coord_sets: [Option<[[f32; 2]; 3]>; 8],
    pub(super) material_index: Option<usize>,
    pub(super) packet_index: usize,
    pub(super) model_index: usize,
    pub(super) render_layer: PreviewRenderLayer,
    pub(super) color: Option<[u8; 4]>,
    pub(super) vertex_colors: Option<[[u8; 4]; 3]>,
    pub(super) combine_mode: J3dPreviewCombineMode,
    pub(super) tex_coords: Option<[[f32; 2]; 3]>,
    pub(super) texture_index: Option<usize>,
    pub(super) mask_tex_coords: Option<[[f32; 2]; 3]>,
    pub(super) mask_texture_index: Option<usize>,
    pub(super) cull_mode: Option<u8>,
    pub(super) alpha_compare: Option<J3dAlphaCompare>,
    pub(super) blend_mode: Option<J3dBlendMode>,
    pub(super) z_mode: Option<J3dZMode>,
    pub(super) billboard: Option<J3dBillboard>,
    pub(super) particle_type: Option<u8>,
    pub(super) particle_pivot: Option<[f32; 2]>,
    pub(super) particle_direction: Option<[f32; 3]>,
    pub(super) particle_color_mode: Option<u8>,
    pub(super) particle_environment_color: Option<[u8; 4]>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum PreviewRenderLayer {
    Sky,
    Main,
    Water,
    WaveFoam,
    IndirectWater,
    MirrorSurface,
    MirrorScene,
    Goop,
    Shadow,
    Heatwave,
    Particle,
    ParticleDistortion,
}

#[derive(Debug, Clone, Copy)]
pub(super) struct ProjectedVertex {
    pub(super) x: f32,
    pub(super) y: f32,
    pub(super) depth: f32,
    pub(super) inv_depth: f32,
}

#[derive(Debug, Clone, Copy)]
pub(super) struct CameraFrame {
    pub(super) position: [f32; 3],
    pub(super) right: [f32; 3],
    pub(super) up: [f32; 3],
    pub(super) forward: [f32; 3],
}

#[derive(Clone, Copy)]
pub(super) struct ProjectedPreviewTriangle<'a> {
    pub(super) triangle: &'a PreviewTriangle,
    pub(super) screen: [ProjectedVertex; 3],
    pub(super) average_depth: f32,
}

#[derive(Clone)]
pub(super) struct PreviewTexture {
    pub(super) image: egui::ColorImage,
    pub(super) mips: Vec<egui::ColorImage>,
    pub(super) format: u8,
    pub(super) wrap_s: u8,
    pub(super) wrap_t: u8,
    pub(super) min_filter: u8,
    pub(super) mag_filter: u8,
    pub(super) mipmap_enabled: bool,
    pub(super) do_edge_lod: bool,
    pub(super) bias_clamp: bool,
    pub(super) max_anisotropy: u8,
    pub(super) min_lod: f32,
    pub(super) max_lod: f32,
    pub(super) lod_bias: f32,
    pub(super) mipmap_count: u8,
    pub(super) has_alpha: bool,
    pub(super) has_translucent_alpha: bool,
}

#[derive(Clone)]
pub(super) struct CachedObjectModelPreview {
    pub(super) file: J3dFile,
    pub(super) joint_animation: Option<J3dJointAnimation>,
    pub(super) prepared_triangles: Option<Arc<J3dPreparedAnimatedTriangles>>,
    pub(super) loader_flags: u32,
    pub(super) preview: J3dGeometryPreview,
    pub(super) texture_base: usize,
    pub(super) material_base: usize,
    pub(super) joint_names: Vec<String>,
    pub(super) instances: Vec<AnimatedModelInstance>,
}

#[derive(Clone)]
pub(super) struct CachedAccessoryModelPreview {
    pub(super) file: Arc<J3dFile>,
    pub(super) joint_animation: Option<Arc<J3dJointAnimation>>,
    pub(super) prepared_triangles: Option<Arc<J3dPreparedAnimatedTriangles>>,
    pub(super) loader_flags: u32,
    pub(super) preview: J3dGeometryPreview,
    pub(super) local_triangles: Arc<Vec<J3dTriangle>>,
    pub(super) texture_base: usize,
    pub(super) material_base: usize,
}

#[derive(Clone)]
pub(super) struct AnimatedModelPreview {
    pub(super) file: J3dFile,
    pub(super) animation: J3dJointAnimation,
    pub(super) prepared_triangles: Option<Arc<J3dPreparedAnimatedTriangles>>,
    pub(super) loader_flags: u32,
    pub(super) instances: Vec<AnimatedModelInstance>,
}

#[derive(Clone)]
pub(super) struct RuntimeRotatingModelPreview {
    pub(super) positions: Arc<Vec<[f32; 3]>>,
    pub(super) triangles: Arc<Vec<J3dTriangle>>,
    pub(super) instances: Vec<AnimatedModelInstance>,
}

#[derive(Clone)]
pub(super) struct LevelTransformModelPreview {
    pub(super) file: J3dFile,
    pub(super) loader_flags: u32,
    pub(super) targets: Vec<LevelTransformTarget>,
    pub(super) point_range: std::ops::Range<usize>,
    pub(super) point_stride: usize,
    pub(super) triangle_range: std::ops::Range<usize>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct LevelTransformTarget {
    pub(super) joint_index: usize,
    pub(super) translation_offset: [f32; 3],
    pub(super) scale_multiplier: [f32; 3],
    pub(super) behavior: LevelTransformBehavior,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum LevelTransformBehavior {
    Linear,
    AlwaysHidden,
    HideAfterStart,
}

#[derive(Debug, Clone)]
pub(super) struct AnimatedModelInstance {
    pub(super) transform: Transform,
    pub(super) model_index: usize,
    pub(super) point_range: std::ops::Range<usize>,
    pub(super) point_stride: usize,
    pub(super) triangle_range: std::ops::Range<usize>,
    pub(super) accessories: Vec<AnimatedAccessoryInstance>,
    pub(super) runtime_yaw_degrees_per_frame: f32,
}

#[derive(Debug, Clone)]
pub(super) struct AnimatedAccessoryInstance {
    pub(super) joint_index: Option<usize>,
    pub(super) file: Arc<J3dFile>,
    pub(super) joint_animation: Option<Arc<J3dJointAnimation>>,
    pub(super) prepared_triangles: Option<Arc<J3dPreparedAnimatedTriangles>>,
    pub(super) loader_flags: u32,
    pub(super) local_triangles: Arc<Vec<J3dTriangle>>,
    pub(super) triangle_range: std::ops::Range<usize>,
}

#[derive(Clone, PartialEq)]
pub(super) struct ModelFramebufferKey {
    pub(super) stage_id: String,
    pub(super) size: [usize; 2],
    pub(super) camera_focus: [u32; 3],
    pub(super) camera_yaw: u32,
    pub(super) camera_pitch: u32,
    pub(super) camera_distance: u32,
    pub(super) viewport_pan: [u32; 2],
    pub(super) viewport_zoom: u32,
    pub(super) triangle_count: usize,
    pub(super) texture_count: usize,
    pub(super) source_triangles: usize,
}
