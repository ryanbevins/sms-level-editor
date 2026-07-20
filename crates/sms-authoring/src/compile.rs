use sms_formats::{
    compile_static_bmd3, validate_canonical_bmd3, GxEncodedTexture, GxMaterial, J3dRebuildDocument,
    RgbaImage, StaticModel, StaticModelMesh, StaticModelVertex,
};

use crate::import::{active_node_mask, global_transforms};
use crate::math::{
    transform_normal, transform_point, transform_reverses_winding, transform_tangent_frame,
};
use crate::{AuthoringError, AuthoringResult, ModelAssetDocument, ModelBounds, NodePurpose};

/// Exact allocation size of one section in a compiled canonical BMD3.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BmdSectionSize {
    pub tag: [u8; 4],
    pub byte_size: u32,
}

/// Size information produced alongside a canonical BMD3 compile.
///
/// `total_byte_size` is the complete file size, including the J3D header, and
/// each section size includes that section's eight-byte tag/size header and
/// canonical alignment padding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BmdCompileReport {
    pub total_byte_size: usize,
    pub sections: Vec<BmdSectionSize>,
}

/// Canonical BMD3 bytes and their allocation report from the same compile.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompiledBmd {
    pub bytes: Vec<u8>,
    pub report: BmdCompileReport,
}

/// Decodes a source-free BMD3 into the complete typed J3D section document.
///
/// No input bytes are retained: unknown or unsupported section tags are rejected
/// instead of being copied opaquely into a native model asset.
pub fn decode_bmd3(bytes: &[u8]) -> AuthoringResult<J3dRebuildDocument> {
    let document = J3dRebuildDocument::parse(bytes)
        .map_err(|error| AuthoringError::Compile(format!("decode BMD3: {error}")))?;
    if document.file_type != *b"bmd3" {
        return Err(AuthoringError::Compile(format!(
            "expected J3D2 bmd3, found {}",
            String::from_utf8_lossy(&document.file_type)
        )));
    }
    Ok(document)
}

/// Decodes and verifies the canonical rigid eight-section BMD3 authoring profile.
pub fn decode_canonical_bmd3(bytes: &[u8]) -> AuthoringResult<J3dRebuildDocument> {
    let document = decode_bmd3(bytes)?;
    validate_canonical_bmd3(&document)
        .map_err(|error| AuthoringError::Compile(format!("validate canonical BMD3: {error}")))?;
    Ok(document)
}

impl ModelAssetDocument {
    /// Bounds of active render nodes after hierarchy, axis, and unit conversion.
    pub fn converted_bounds(&self) -> AuthoringResult<Option<ModelBounds>> {
        self.validate()?;
        let transforms = global_transforms(&self.nodes)?;
        let active = active_node_mask(self)?;
        let mut bounds: Option<ModelBounds> = None;
        for (node_index, node) in self.nodes.iter().enumerate() {
            if !active[node_index] || node.purpose != NodePurpose::Render {
                continue;
            }
            let Some(mesh_index) = node.mesh else {
                continue;
            };
            for position in self.meshes[mesh_index as usize]
                .primitives
                .iter()
                .flat_map(|primitive| primitive.positions.iter().copied())
            {
                let position = transform_point(transforms[node_index], position);
                match &mut bounds {
                    Some(bounds) => {
                        for (axis, value) in position.into_iter().enumerate() {
                            bounds.min[axis] = bounds.min[axis].min(value);
                            bounds.max[axis] = bounds.max[axis].max(value);
                        }
                    }
                    None => {
                        bounds = Some(ModelBounds {
                            min: position,
                            max: position,
                        });
                    }
                }
            }
        }
        Ok(bounds)
    }

    /// Builds the canonical eight-section BMD3 document for all active,
    /// renderable rigid nodes. Node transforms are baked into shape vertices.
    pub fn compile_bmd_document(&self) -> AuthoringResult<J3dRebuildDocument> {
        self.validate()?;
        let mut materials = self
            .materials
            .iter()
            .map(|material| material.gx.clone())
            .collect::<Vec<_>>();
        let referenced_texture_indices =
            compact_material_texture_references(&mut materials, self.textures.len())?;
        let textures = referenced_texture_indices
            .into_iter()
            .map(|texture_index| {
                let texture = &self.textures[texture_index];
                let width = u16::try_from(texture.width).map_err(|_| {
                    AuthoringError::Compile(format!(
                        "texture {} width {} exceeds GX u16 dimensions",
                        texture.name, texture.width
                    ))
                })?;
                let height = u16::try_from(texture.height).map_err(|_| {
                    AuthoringError::Compile(format!(
                        "texture {} height {} exceeds GX u16 dimensions",
                        texture.name, texture.height
                    ))
                })?;
                let image = RgbaImage::new(width, height, texture.rgba8.clone())
                    .map_err(|error| AuthoringError::Compile(error.to_string()))?;
                GxEncodedTexture::encode_rgba(texture.name.clone(), &image, texture.encode_options)
                    .map_err(|error| AuthoringError::Compile(error.to_string()))
            })
            .collect::<AuthoringResult<Vec<_>>>()?;
        let needs_default_material = self.meshes.iter().any(|mesh| {
            mesh.primitives
                .iter()
                .any(|primitive| primitive.material.is_none())
        });
        let default_material_index = if needs_default_material {
            let index = u16::try_from(materials.len())
                .map_err(|_| AuthoringError::Compile("material count exceeds u16".to_string()))?;
            materials.push(default_import_material("default_material"));
            Some(index)
        } else {
            None
        };

        let transforms = global_transforms(&self.nodes)?;
        let active = active_node_mask(self)?;
        let mut meshes = Vec::new();
        for (node_index, node) in self.nodes.iter().enumerate() {
            if !active[node_index] || node.purpose != NodePurpose::Render {
                continue;
            }
            let Some(mesh_index) = node.mesh else {
                continue;
            };
            let mesh = &self.meshes[mesh_index as usize];
            for (primitive_index, primitive) in mesh.primitives.iter().enumerate() {
                if primitive.indices.is_empty() {
                    continue;
                }
                let material_index = if let Some(material) = primitive.material {
                    u16::try_from(material).map_err(|_| {
                        AuthoringError::Compile(format!("material index {material} exceeds u16"))
                    })?
                } else {
                    default_material_index.expect("a default material was allocated")
                };
                let mut vertices = Vec::with_capacity(primitive.positions.len());
                for vertex_index in 0..primitive.positions.len() {
                    let position =
                        transform_point(transforms[node_index], primitive.positions[vertex_index]);
                    let normal =
                        transform_normal(transforms[node_index], primitive.normals[vertex_index])?;
                    let mut vertex = StaticModelVertex::new(position, normal);
                    if let Some(tangent) = primitive.tangents.get(vertex_index) {
                        vertex.normal_binormal_tangent = Some(transform_tangent_frame(
                            transforms[node_index],
                            primitive.normals[vertex_index],
                            *tangent,
                        )?);
                    }
                    for color in &primitive.colors {
                        vertex.colors[color.set as usize] =
                            Some(color.values[vertex_index].map(float_to_unorm8));
                    }
                    for tex_coord in &primitive.tex_coords {
                        vertex.tex_coords[tex_coord.set as usize] =
                            Some(tex_coord.values[vertex_index]);
                    }
                    vertices.push(vertex);
                }
                let reverse_winding = transform_reverses_winding(transforms[node_index]);
                meshes.push(StaticModelMesh {
                    name: format!("{}_{primitive_index}", node.name),
                    material_index,
                    vertices,
                    triangles: primitive
                        .indices
                        .chunks_exact(3)
                        .map(|triangle| {
                            if reverse_winding {
                                [triangle[0], triangle[2], triangle[1]]
                            } else {
                                [triangle[0], triangle[1], triangle[2]]
                            }
                        })
                        .collect(),
                });
            }
        }
        if meshes.is_empty() {
            return Err(AuthoringError::Compile(
                "model contains no active render triangles".to_string(),
            ));
        }
        compile_static_bmd3(&StaticModel {
            root_joint_name: self.name.clone(),
            meshes,
            materials,
            textures,
        })
        .map_err(|error| AuthoringError::Compile(error.to_string()))
    }

    /// Compiles a canonical BMD3 and reports its total and per-section sizes.
    pub fn compile_bmd_with_report(&self) -> AuthoringResult<CompiledBmd> {
        let document = self.compile_bmd_document()?;
        let sections = document
            .sections
            .iter()
            .map(|section| BmdSectionSize {
                tag: section.tag(),
                byte_size: section.declared_size,
            })
            .collect();
        let bytes = document
            .to_bytes()
            .map_err(|error| AuthoringError::Compile(error.to_string()))?;
        Ok(CompiledBmd {
            report: BmdCompileReport {
                total_byte_size: bytes.len(),
                sections,
            },
            bytes,
        })
    }

    pub fn compile_bmd(&self) -> AuthoringResult<Vec<u8>> {
        Ok(self.compile_bmd_with_report()?.bytes)
    }

    pub fn compile_col(&self) -> AuthoringResult<Vec<u8>> {
        self.collision
            .as_ref()
            .ok_or_else(|| AuthoringError::Collision("model asset has no collision".to_string()))?
            .to_col_bytes()
    }
}

/// Clones are passed here by the BMD compiler, so remapping never mutates the
/// editable native asset. Referenced textures are compacted in original TEX1
/// index order rather than material/slot discovery order.
fn compact_material_texture_references(
    materials: &mut [GxMaterial],
    texture_count: usize,
) -> AuthoringResult<Vec<usize>> {
    let mut referenced = vec![false; texture_count];
    for (material_index, material) in materials.iter().enumerate() {
        for (slot, texture) in material.texture_numbers.iter().enumerate() {
            let Some(texture) = texture else {
                continue;
            };
            let original_index = usize::from(*texture);
            let Some(is_referenced) = referenced.get_mut(original_index) else {
                return Err(AuthoringError::Compile(format!(
                    "material {material_index} ({:?}) texture slot {slot} references texture {texture}, but the model contains {texture_count} textures",
                    material.name
                )));
            };
            *is_referenced = true;
        }
    }

    let mut compact_indices = vec![None; texture_count];
    let mut referenced_texture_indices = Vec::new();
    for (original_index, is_referenced) in referenced.into_iter().enumerate() {
        if !is_referenced {
            continue;
        }
        let compact_index = u16::try_from(referenced_texture_indices.len()).map_err(|_| {
            AuthoringError::Compile("referenced texture count exceeds u16".to_string())
        })?;
        compact_indices[original_index] = Some(compact_index);
        referenced_texture_indices.push(original_index);
    }

    for material in materials {
        for texture in material.texture_numbers.iter_mut().flatten() {
            *texture = compact_indices[usize::from(*texture)]
                .expect("each material texture reference was collected above");
        }
    }
    Ok(referenced_texture_indices)
}

fn default_import_material(name: &str) -> GxMaterial {
    let mut material = GxMaterial {
        name: name.to_string(),
        color_channel_count: 1,
        color_channels: [
            Some(sms_formats::GxColorChannel::default()),
            Some(sms_formats::GxColorChannel::default()),
            None,
            None,
        ],
        ..GxMaterial::default()
    };
    material.tev_orders[0] = Some(sms_formats::GxTevOrder {
        tex_coord: None,
        tex_map: None,
        color_channel: 4,
    });
    if let Some(stage) = &mut material.tev_stages[0] {
        stage.color_inputs = [10, 15, 15, 15];
        stage.alpha_inputs = [5, 7, 7, 7];
    }
    material
}

fn float_to_unorm8(value: f32) -> u8 {
    (value.clamp(0.0, 1.0) * 255.0).round() as u8
}
