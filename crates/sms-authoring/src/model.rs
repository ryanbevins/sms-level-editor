use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::Diagnostic;

pub const MODEL_ASSET_FORMAT_VERSION: u32 = 1;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelCoordinateSpace {
    /// Assets imported before the canonical glTF/Sunshine basis correction.
    #[default]
    LegacyReflectedZ,
    /// Canonical right-handed Y-up coordinates shared by glTF and Sunshine.
    GltfCompatible,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelAssetDocument {
    pub format_version: u32,
    #[serde(default)]
    pub coordinate_space: ModelCoordinateSpace,
    pub name: String,
    pub scene_roots: Vec<u32>,
    pub nodes: Vec<ModelNode>,
    pub meshes: Vec<ModelMesh>,
    pub materials: Vec<ModelMaterial>,
    pub textures: Vec<ModelTexture>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub collision: Option<CollisionDocument>,
    #[serde(default)]
    pub diagnostics: Vec<Diagnostic>,
    /// Import warning categories the author explicitly accepted after review.
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub acknowledged_diagnostics: BTreeSet<crate::DiagnosticCode>,
}

impl ModelAssetDocument {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            format_version: MODEL_ASSET_FORMAT_VERSION,
            coordinate_space: ModelCoordinateSpace::GltfCompatible,
            name: name.into(),
            scene_roots: Vec::new(),
            nodes: Vec::new(),
            meshes: Vec::new(),
            materials: Vec::new(),
            textures: Vec::new(),
            collision: None,
            diagnostics: Vec::new(),
            acknowledged_diagnostics: BTreeSet::new(),
        }
    }

    pub fn unacknowledged_required_diagnostics(&self) -> Vec<&Diagnostic> {
        self.diagnostics
            .iter()
            .filter(|diagnostic| {
                diagnostic.acknowledgement_required
                    && !self.acknowledged_diagnostics.contains(&diagnostic.code)
            })
            .collect()
    }

    /// Migrates geometry imported with the original reflected-Z basis to the
    /// canonical identity basis now shared with glTF and Blender.
    pub(crate) fn migrate_legacy_reflected_z_coordinate_space(&mut self) -> bool {
        if self.coordinate_space != ModelCoordinateSpace::LegacyReflectedZ {
            return false;
        }

        for node in &mut self.nodes {
            for column in 0..4 {
                for row in 0..4 {
                    if (column == 2) ^ (row == 2) {
                        node.local_transform[column][row] = -node.local_transform[column][row];
                    }
                }
            }
        }
        for primitive in self.meshes.iter_mut().flat_map(|mesh| &mut mesh.primitives) {
            for position in &mut primitive.positions {
                position[2] = -position[2];
            }
            for normal in &mut primitive.normals {
                normal[2] = -normal[2];
            }
            for tangent in &mut primitive.tangents {
                tangent[2] = -tangent[2];
                tangent[3] = -tangent[3];
            }
            for triangle in primitive.indices.chunks_exact_mut(3) {
                triangle.swap(1, 2);
            }
        }
        if let Some(collision) = &mut self.collision {
            for vertex in &mut collision.vertices {
                vertex[2] = -vertex[2];
            }
            for group in &mut collision.groups {
                for triangle in &mut group.triangles {
                    triangle.swap(1, 2);
                }
            }
        }

        self.coordinate_space = ModelCoordinateSpace::GltfCompatible;
        true
    }

    /// Repairs the conservative material program emitted by the initial v1
    /// importer. That program multiplied TEXC/TEXA by RASC/RASA while its TEV
    /// order selected GX_COLOR_NULL, whose raster value is zero on GX.
    ///
    /// The match is deliberately limited to the exact generated program so a
    /// hand-authored advanced MAT3 setup that uses GX_COLOR_NULL is preserved.
    pub(crate) fn repair_legacy_conservative_materials(&mut self) -> bool {
        let mut changed = false;
        for material in &mut self.materials {
            let gx = &mut material.gx;
            if gx.tev_stage_count != 1 {
                continue;
            }
            let Some(order) = gx.tev_orders[0] else {
                continue;
            };
            let Some(stage) = gx.tev_stages[0] else {
                continue;
            };
            if order.color_channel != 0xff {
                continue;
            }

            let legacy_textured_stage = sms_formats::GxTevStage {
                color_inputs: [15, 8, 10, 15],
                alpha_inputs: [7, 4, 5, 7],
                ..sms_formats::GxTevStage::default()
            };
            let generated_textured = material.base_color_texture.as_ref().is_some_and(|binding| {
                order.tex_coord == Some(0)
                    && order.tex_map == Some(0)
                    && stage == legacy_textured_stage
                    && gx.tex_gen_count == 1
                    && gx.tex_gens[0]
                        == Some(sms_formats::GxTexCoordGen {
                            function: 1,
                            source: 4 + binding.tex_coord,
                            matrix: 60,
                        })
                    && u16::try_from(binding.texture)
                        .ok()
                        .is_some_and(|texture| gx.texture_numbers[0] == Some(texture))
            });
            let generated_untextured = material.base_color_texture.is_none()
                && order.tex_coord.is_none()
                && order.tex_map.is_none()
                && stage == sms_formats::GxTevStage::default()
                && gx.tex_gen_count == 0
                && gx.texture_numbers.iter().all(Option::is_none);
            if !generated_textured && !generated_untextured {
                continue;
            }

            gx.color_channel_count = gx.color_channel_count.max(1);
            gx.color_channels[0].get_or_insert_default();
            gx.color_channels[1].get_or_insert_default();
            gx.tev_orders[0]
                .as_mut()
                .expect("the generated TEV order was checked above")
                .color_channel = 4;
            if generated_untextured {
                let stage = gx.tev_stages[0]
                    .as_mut()
                    .expect("the generated TEV stage was checked above");
                stage.color_inputs = [10, 15, 15, 15];
                stage.alpha_inputs = [5, 7, 7, 7];
            }
            changed = true;
        }
        changed
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModelNode {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<u32>,
    pub children: Vec<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mesh: Option<u32>,
    #[serde(default)]
    pub purpose: NodePurpose,
    /// Column-major local transform in converted Sunshine coordinates.
    pub local_transform: [[f32; 4]; 4],
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NodePurpose {
    #[default]
    Render,
    CollisionOnly,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModelMesh {
    pub name: String,
    pub primitives: Vec<ModelPrimitive>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModelPrimitive {
    pub positions: Vec<[f32; 3]>,
    pub normals: Vec<[f32; 3]>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tangents: Vec<[f32; 4]>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tex_coords: Vec<TexCoordSet>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub colors: Vec<ColorSet>,
    /// Canonical triangle-list indices after mode conversion and winding fix.
    pub indices: Vec<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub material: Option<u32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct ModelBounds {
    pub min: [f32; 3],
    pub max: [f32; 3],
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TexCoordSet {
    pub set: u8,
    pub values: Vec<[f32; 2]>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ColorSet {
    pub set: u8,
    pub values: Vec<[f32; 4]>,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum ImportedAlphaMode {
    Opaque,
    Mask { cutoff: f32 },
    Blend,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GxTextureBinding {
    pub texture: u32,
    pub tex_coord: u8,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SourcePbrMetadata {
    pub metallic_factor: f32,
    pub roughness_factor: f32,
    pub has_metallic_roughness_texture: bool,
    pub has_normal_texture: bool,
    pub has_occlusion_texture: bool,
    pub emissive_factor: [f32; 3],
    pub has_emissive_texture: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModelMaterial {
    /// Complete GX/MAT3 state edited and compiled without a lossy projection.
    pub gx: sms_formats::GxMaterial,
    /// Original conservative glTF mapping inputs retained for inspection.
    pub source_base_color: [f32; 4],
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_color_texture: Option<GxTextureBinding>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vertex_color_set: Option<u8>,
    pub source_double_sided: bool,
    pub source_alpha_mode: ImportedAlphaMode,
    pub source_pbr: SourcePbrMetadata,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModelTexture {
    pub name: String,
    pub width: u32,
    pub height: u32,
    #[serde(with = "rgba_bytes")]
    pub rgba8: Vec<u8>,
    pub encode_options: sms_formats::GxTextureEncodeOptions,
}

mod rgba_bytes {
    use base64::Engine;
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S>(bytes: &[u8], serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&base64::engine::general_purpose::STANDARD.encode(bytes))
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<Vec<u8>, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        base64::engine::general_purpose::STANDARD
            .decode(value)
            .map_err(serde::de::Error::custom)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct CoordinateConversion {
    pub units_per_meter: f32,
    /// Row-major 3x3 basis applied before unit scaling.
    pub basis: [[f32; 3]; 3],
    pub reverse_winding: bool,
}

impl Default for CoordinateConversion {
    fn default() -> Self {
        Self {
            units_per_meter: 100.0,
            basis: [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]],
            reverse_winding: false,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CollisionSurface {
    pub surface_type: u16,
    pub attribute_0: u8,
    pub attribute_1: u8,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<i16>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CollisionDocument {
    pub vertices: Vec<[f32; 3]>,
    pub groups: Vec<CollisionGroup>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CollisionGroup {
    pub name: String,
    pub surface: CollisionSurface,
    pub triangles: Vec<[u32; 3]>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "source", rename_all = "snake_case")]
pub enum CollisionSource {
    None,
    RenderGeometry {
        surface: CollisionSurface,
    },
    EmbeddedNodes {
        #[serde(default = "default_collision_prefix")]
        prefix: String,
        #[serde(default)]
        selected_nodes: BTreeSet<String>,
        #[serde(default)]
        surfaces_by_node: BTreeMap<String, CollisionSurface>,
        #[serde(default)]
        default_surface: CollisionSurface,
    },
    SeparateFile {
        path: PathBuf,
        #[serde(default)]
        options: CollisionImportOptions,
    },
}

fn default_collision_prefix() -> String {
    "COL_".to_string()
}

impl Default for CollisionSource {
    fn default() -> Self {
        Self::RenderGeometry {
            surface: CollisionSurface::default(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModelImportOptions {
    #[serde(default)]
    pub coordinate_conversion: CoordinateConversion,
    #[serde(default)]
    pub collision: CollisionSource,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub collision_simplification: Option<CollisionSimplificationOptions>,
    #[serde(default = "default_max_source_bytes")]
    pub max_source_bytes: usize,
    #[serde(default = "default_max_total_buffer_bytes")]
    pub max_total_buffer_bytes: usize,
}

impl Default for ModelImportOptions {
    fn default() -> Self {
        Self {
            coordinate_conversion: CoordinateConversion::default(),
            collision: CollisionSource::default(),
            collision_simplification: None,
            max_source_bytes: default_max_source_bytes(),
            max_total_buffer_bytes: default_max_total_buffer_bytes(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CollisionImportOptions {
    #[serde(default)]
    pub coordinate_conversion: CoordinateConversion,
    #[serde(default)]
    pub node_selection: CollisionNodeSelection,
    #[serde(default)]
    pub surfaces_by_node: BTreeMap<String, CollisionSurface>,
    #[serde(default)]
    pub default_surface: CollisionSurface,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub simplification: Option<CollisionSimplificationOptions>,
    #[serde(default = "default_max_source_bytes")]
    pub max_source_bytes: usize,
    #[serde(default = "default_max_total_buffer_bytes")]
    pub max_total_buffer_bytes: usize,
}

impl Default for CollisionImportOptions {
    fn default() -> Self {
        Self {
            coordinate_conversion: CoordinateConversion::default(),
            node_selection: CollisionNodeSelection::AllGeometry,
            surfaces_by_node: BTreeMap::new(),
            default_surface: CollisionSurface::default(),
            simplification: None,
            max_source_bytes: default_max_source_bytes(),
            max_total_buffer_bytes: default_max_total_buffer_bytes(),
        }
    }
}

/// Optional deterministic collision reduction. `max_error` is the maximum
/// quadric error in converted Sunshine coordinate units.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct CollisionSimplificationOptions {
    pub target_ratio: f32,
    pub max_error: f32,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "selection", rename_all = "snake_case")]
pub enum CollisionNodeSelection {
    #[default]
    AllGeometry,
    Named {
        #[serde(default = "default_collision_prefix")]
        prefix: String,
        #[serde(default)]
        selected_nodes: BTreeSet<String>,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ImportResult {
    pub asset: ModelAssetDocument,
    pub diagnostics: Vec<Diagnostic>,
}

fn default_max_source_bytes() -> usize {
    64 * 1024 * 1024
}

fn default_max_total_buffer_bytes() -> usize {
    256 * 1024 * 1024
}
