use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::math::{transform_point, transform_reverses_winding};
use crate::{
    AssetId, AuthoringError, AuthoringResult, CollisionDocument, CollisionSurface,
    ModelAssetDocument, ModelNode, NodePurpose,
};

/// Selects how an authored model instance participates in stage export.
///
/// A detached runtime object is the safe default because it does not replace
/// the stage terrain model. Stock map-object replacement remains a separate,
/// explicitly selected workflow so arbitrary resource keys are never passed to
/// Sunshine's compiled `MapObjBase` registry.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelInstanceExportMode {
    #[default]
    SeparateRuntimeObject,
    StockMapObjBase,
    MapTerrain,
    /// Rebuilds the stage-global `map/map/sky.bmd` consumed by `TSky`.
    ///
    /// This remains an explicit scene role instead of a filename convention:
    /// the editor also authors the matching typed `Sky` placement when the
    /// open stage does not already contain one.
    Skybox,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModelInstancePlacement {
    pub instance_id: Uuid,
    pub asset_id: AssetId,
    pub name: String,
    /// Column-major transform in Sunshine coordinates.
    pub transform: [[f32; 4]; 4],
    #[serde(default = "default_true")]
    pub collision_enabled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub collision_surface_override: Option<CollisionSurface>,
    #[serde(default)]
    pub export_mode: ModelInstanceExportMode,
    /// Decomp-verified stock slot selected for `StockMapObjBase` export.
    /// Empty until the user explicitly chooses a compatible slot.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub stock_map_obj_resource: String,
}

impl ModelInstancePlacement {
    pub fn new(asset_id: AssetId, name: impl Into<String>) -> Self {
        Self {
            instance_id: Uuid::new_v4(),
            asset_id,
            name: name.into(),
            transform: identity(),
            collision_enabled: true,
            collision_surface_override: None,
            export_mode: ModelInstanceExportMode::default(),
            stock_map_obj_resource: String::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedModelInstance {
    pub placement: ModelInstancePlacement,
    pub asset: ModelAssetDocument,
}

/// Combines resolved project instances into one source-free world asset.
///
/// Instance transforms remain as hierarchy parents for BMD compilation, while
/// collision positions are baked immediately because Sunshine COL has no node
/// hierarchy. Texture, material, mesh, and triangle references are remapped
/// without coalescing user-authored state.
pub fn merge_model_instances(
    name: impl Into<String>,
    instances: &[ResolvedModelInstance],
) -> AuthoringResult<ModelAssetDocument> {
    if instances.is_empty() {
        return Err(AuthoringError::Invalid(
            "at least one resolved model instance is required".to_string(),
        ));
    }
    let mut merged = ModelAssetDocument::new(name);
    let mut collision = CollisionDocument {
        vertices: Vec::new(),
        groups: Vec::new(),
    };
    let mut has_collision = false;

    for instance in instances {
        instance.asset.validate()?;
        if instance
            .placement
            .transform
            .iter()
            .flatten()
            .any(|value| !value.is_finite())
        {
            return Err(AuthoringError::Invalid(format!(
                "instance {:?} contains a non-finite transform",
                instance.placement.name
            )));
        }

        let texture_offset = merged.textures.len();
        let material_offset = merged.materials.len();
        let mesh_offset = merged.meshes.len();
        let parent_index = merged.nodes.len();
        let node_offset = parent_index
            .checked_add(1)
            .ok_or_else(|| AuthoringError::Invalid("node offset overflow".to_string()))?;

        merged.textures.extend(instance.asset.textures.clone());
        for mut material in instance.asset.materials.clone() {
            for texture_number in material.gx.texture_numbers.iter_mut().flatten() {
                let remapped = usize::from(*texture_number)
                    .checked_add(texture_offset)
                    .ok_or_else(|| AuthoringError::Invalid("texture index overflow".to_string()))?;
                *texture_number = u16::try_from(remapped).map_err(|_| {
                    AuthoringError::Invalid("merged texture index exceeds u16".to_string())
                })?;
            }
            if let Some(binding) = &mut material.base_color_texture {
                binding.texture = binding
                    .texture
                    .checked_add(u32::try_from(texture_offset).map_err(|_| {
                        AuthoringError::Invalid("merged texture offset exceeds u32".to_string())
                    })?)
                    .ok_or_else(|| AuthoringError::Invalid("texture index overflow".to_string()))?;
            }
            merged.materials.push(material);
        }
        for mut mesh in instance.asset.meshes.clone() {
            for primitive in &mut mesh.primitives {
                if let Some(material) = &mut primitive.material {
                    *material = material
                        .checked_add(u32::try_from(material_offset).map_err(|_| {
                            AuthoringError::Invalid(
                                "merged material offset exceeds u32".to_string(),
                            )
                        })?)
                        .ok_or_else(|| {
                            AuthoringError::Invalid("material index overflow".to_string())
                        })?;
                }
            }
            merged.meshes.push(mesh);
        }

        let source_roots = if instance.asset.scene_roots.is_empty() {
            instance
                .asset
                .nodes
                .iter()
                .enumerate()
                .filter_map(|(index, node)| node.parent.is_none().then_some(index as u32))
                .collect::<Vec<_>>()
        } else {
            instance.asset.scene_roots.clone()
        };
        if source_roots.is_empty() {
            return Err(AuthoringError::Invalid(format!(
                "instance {:?} has no hierarchy roots",
                instance.placement.name
            )));
        }
        let root_children = source_roots
            .iter()
            .map(|root| remap_u32(*root, node_offset, "node"))
            .collect::<AuthoringResult<Vec<_>>>()?;
        merged.nodes.push(ModelNode {
            name: instance.placement.name.clone(),
            parent: None,
            children: root_children,
            mesh: None,
            purpose: NodePurpose::Render,
            local_transform: instance.placement.transform,
        });
        merged
            .scene_roots
            .push(u32::try_from(parent_index).map_err(|_| {
                AuthoringError::Invalid("merged parent node index exceeds u32".to_string())
            })?);
        for (source_index, mut node) in instance.asset.nodes.clone().into_iter().enumerate() {
            let source_index = u32::try_from(source_index).map_err(|_| {
                AuthoringError::Invalid("source node index exceeds u32".to_string())
            })?;
            node.children = node
                .children
                .into_iter()
                .map(|child| remap_u32(child, node_offset, "node"))
                .collect::<AuthoringResult<Vec<_>>>()?;
            node.parent = if source_roots.contains(&source_index) {
                Some(u32::try_from(parent_index).map_err(|_| {
                    AuthoringError::Invalid("merged parent node index exceeds u32".to_string())
                })?)
            } else {
                node.parent
                    .map(|parent| remap_u32(parent, node_offset, "node"))
                    .transpose()?
            };
            node.mesh = node
                .mesh
                .map(|mesh| remap_u32(mesh, mesh_offset, "mesh"))
                .transpose()?;
            merged.nodes.push(node);
        }

        for diagnostic in &instance.asset.diagnostics {
            let mut diagnostic = diagnostic.clone();
            diagnostic.context = Some(match diagnostic.context {
                Some(context) => format!("{}: {context}", instance.placement.name),
                None => instance.placement.name.clone(),
            });
            merged.diagnostics.push(diagnostic);
        }

        if instance.placement.collision_enabled {
            if let Some(source_collision) = &instance.asset.collision {
                has_collision = true;
                let vertex_offset = u32::try_from(collision.vertices.len()).map_err(|_| {
                    AuthoringError::Collision(
                        "merged collision vertex count exceeds u32".to_string(),
                    )
                })?;
                collision.vertices.extend(
                    source_collision
                        .vertices
                        .iter()
                        .map(|position| transform_point(instance.placement.transform, *position)),
                );
                let reverse_winding = transform_reverses_winding(instance.placement.transform);
                for source_group in &source_collision.groups {
                    let mut group = source_group.clone();
                    group.name = format!("{}/{}", instance.placement.name, group.name);
                    if let Some(surface) = &instance.placement.collision_surface_override {
                        group.surface = surface.clone();
                    }
                    for triangle in &mut group.triangles {
                        for index in triangle.iter_mut() {
                            *index = index.checked_add(vertex_offset).ok_or_else(|| {
                                AuthoringError::Collision(
                                    "merged collision index exceeds u32".to_string(),
                                )
                            })?;
                        }
                        if reverse_winding {
                            triangle.swap(1, 2);
                        }
                    }
                    collision.groups.push(group);
                }
            }
        }
    }

    if has_collision {
        collision.cleanup_exact()?;
        merged.collision = Some(collision);
    }
    merged.validate()?;
    Ok(merged)
}

fn remap_u32(index: u32, offset: usize, kind: &str) -> AuthoringResult<u32> {
    index
        .checked_add(
            u32::try_from(offset).map_err(|_| {
                AuthoringError::Invalid(format!("merged {kind} offset exceeds u32"))
            })?,
        )
        .ok_or_else(|| AuthoringError::Invalid(format!("merged {kind} index overflow")))
}

fn default_true() -> bool {
    true
}

fn identity() -> [[f32; 4]; 4] {
    [
        [1.0, 0.0, 0.0, 0.0],
        [0.0, 1.0, 0.0, 0.0],
        [0.0, 0.0, 1.0, 0.0],
        [0.0, 0.0, 0.0, 1.0],
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_placement_defaults_to_separate_runtime_object_export() {
        let asset_id = AssetId::new();
        let instance_id = Uuid::new_v4();
        let value = serde_json::json!({
            "instance_id": instance_id,
            "asset_id": asset_id,
            "name": "legacy instance",
            "transform": identity(),
            "collision_enabled": true
        });

        let placement: ModelInstancePlacement = serde_json::from_value(value).unwrap();

        assert_eq!(
            placement.export_mode,
            ModelInstanceExportMode::SeparateRuntimeObject
        );
        assert!(placement.stock_map_obj_resource.is_empty());
    }
}
