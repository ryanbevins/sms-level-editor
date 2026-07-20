use crate::import::normalize_legacy_render_collision_winding;
use crate::{
    AuthoringError, AuthoringResult, Diagnostic, DiagnosticCode, ModelAssetDocument,
    MODEL_ASSET_FORMAT_VERSION,
};
use encoding_rs::SHIFT_JIS;

const MAX_NATIVE_ASSET_BYTES: usize = 512 * 1024 * 1024;

impl ModelAssetDocument {
    pub fn to_native_bytes(&self) -> AuthoringResult<Vec<u8>> {
        self.validate()?;
        let mut bytes = serde_json::to_vec_pretty(self)?;
        bytes.push(b'\n');
        Ok(bytes)
    }

    pub fn from_native_bytes(bytes: &[u8]) -> AuthoringResult<Self> {
        if bytes.len() > MAX_NATIVE_ASSET_BYTES {
            return Err(AuthoringError::invalid(format!(
                "native model asset is {} bytes; limit is {MAX_NATIVE_ASSET_BYTES}",
                bytes.len()
            )));
        }
        let value: serde_json::Value = serde_json::from_slice(bytes)?;
        let version = value
            .get("format_version")
            .and_then(serde_json::Value::as_u64)
            .ok_or_else(|| AuthoringError::invalid("missing integer format_version"))?;
        if version != u64::from(MODEL_ASSET_FORMAT_VERSION) {
            return Err(AuthoringError::invalid(format!(
                "unsupported model asset version {version}; expected {MODEL_ASSET_FORMAT_VERSION}"
            )));
        }
        let mut document: Self = serde_json::from_value(value)?;
        if document.migrate_legacy_reflected_z_coordinate_space() {
            document.diagnostics.push(Diagnostic::info(
                DiagnosticCode::CoordinateSpaceMigrated,
                "migrated legacy reflected-Z model coordinates to the canonical glTF-compatible basis",
                None,
            ));
        }
        document.validate()?;
        document.repair_legacy_conservative_materials();
        if normalize_legacy_render_collision_winding(&mut document)? {
            document.diagnostics.push(Diagnostic::info(
                DiagnosticCode::CollisionWindingNormalized,
                "repaired legacy render-derived collision whose walkable terrain faced downward",
                None,
            ));
        }
        document.validate()?;
        Ok(document)
    }

    pub fn validate(&self) -> AuthoringResult<()> {
        if self.format_version != MODEL_ASSET_FORMAT_VERSION {
            return Err(AuthoringError::invalid(format!(
                "unsupported model asset version {}; expected {MODEL_ASSET_FORMAT_VERSION}",
                self.format_version
            )));
        }
        validate_name(&self.name, "asset")?;
        for &root in &self.scene_roots {
            if root as usize >= self.nodes.len() {
                return Err(AuthoringError::invalid(format!(
                    "scene root {root} is outside {} nodes",
                    self.nodes.len()
                )));
            }
        }
        for (node_index, node) in self.nodes.iter().enumerate() {
            validate_name(&node.name, &format!("node {node_index}"))?;
            if node
                .parent
                .is_some_and(|parent| parent as usize >= self.nodes.len())
            {
                return Err(AuthoringError::invalid(format!(
                    "node {node_index} has an out-of-range parent"
                )));
            }
            if node
                .children
                .iter()
                .any(|&child| child as usize >= self.nodes.len())
            {
                return Err(AuthoringError::invalid(format!(
                    "node {node_index} has an out-of-range child"
                )));
            }
            if node
                .mesh
                .is_some_and(|mesh| mesh as usize >= self.meshes.len())
            {
                return Err(AuthoringError::invalid(format!(
                    "node {node_index} has an out-of-range mesh"
                )));
            }
            if node
                .local_transform
                .iter()
                .flatten()
                .any(|value| !value.is_finite())
            {
                return Err(AuthoringError::invalid(format!(
                    "node {node_index} has a non-finite transform"
                )));
            }
        }
        validate_hierarchy(&self.nodes)?;
        for (mesh_index, mesh) in self.meshes.iter().enumerate() {
            validate_name(&mesh.name, &format!("mesh {mesh_index}"))?;
            for (primitive_index, primitive) in mesh.primitives.iter().enumerate() {
                let vertex_count = primitive.positions.len();
                if primitive.normals.len() != vertex_count {
                    return Err(AuthoringError::invalid(format!(
                        "mesh {mesh_index} primitive {primitive_index} has mismatched normal count"
                    )));
                }
                if !primitive.tangents.is_empty() && primitive.tangents.len() != vertex_count {
                    return Err(AuthoringError::invalid(format!(
                        "mesh {mesh_index} primitive {primitive_index} has mismatched tangent count"
                    )));
                }
                if primitive
                    .tex_coords
                    .iter()
                    .any(|set| set.set >= 8 || set.values.len() != vertex_count)
                {
                    return Err(AuthoringError::invalid(format!(
                        "mesh {mesh_index} primitive {primitive_index} has an invalid texture-coordinate set"
                    )));
                }
                if primitive
                    .colors
                    .iter()
                    .any(|set| set.set >= 2 || set.values.len() != vertex_count)
                {
                    return Err(AuthoringError::invalid(format!(
                        "mesh {mesh_index} primitive {primitive_index} has an invalid color set"
                    )));
                }
                if primitive.indices.len() % 3 != 0 {
                    return Err(AuthoringError::invalid(format!(
                        "mesh {mesh_index} primitive {primitive_index} is not a triangle list"
                    )));
                }
                if primitive
                    .indices
                    .iter()
                    .any(|&index| index as usize >= vertex_count)
                {
                    return Err(AuthoringError::invalid(format!(
                        "mesh {mesh_index} primitive {primitive_index} has an out-of-range index"
                    )));
                }
                if primitive
                    .material
                    .is_some_and(|material| material as usize >= self.materials.len())
                {
                    return Err(AuthoringError::invalid(format!(
                        "mesh {mesh_index} primitive {primitive_index} has an out-of-range material"
                    )));
                }
                if primitive
                    .positions
                    .iter()
                    .flatten()
                    .any(|value| !value.is_finite())
                    || primitive
                        .normals
                        .iter()
                        .flatten()
                        .any(|value| !value.is_finite())
                {
                    return Err(AuthoringError::invalid(format!(
                        "mesh {mesh_index} primitive {primitive_index} contains non-finite geometry"
                    )));
                }
            }
        }
        for (texture_index, texture) in self.textures.iter().enumerate() {
            validate_name(&texture.name, &format!("texture {texture_index}"))?;
            let expected = usize::try_from(texture.width)
                .ok()
                .and_then(|width| {
                    usize::try_from(texture.height)
                        .ok()
                        .and_then(|height| width.checked_mul(height))
                })
                .and_then(|pixels| pixels.checked_mul(4))
                .ok_or_else(|| {
                    AuthoringError::invalid(format!("texture {texture_index} dimensions overflow"))
                })?;
            if texture.rgba8.len() != expected {
                return Err(AuthoringError::invalid(format!(
                    "texture {texture_index} has {} RGBA bytes; expected {expected}",
                    texture.rgba8.len()
                )));
            }
        }
        for (material_index, material) in self.materials.iter().enumerate() {
            validate_name(&material.gx.name, &format!("material {material_index}"))?;
            for (slot, texture) in material.gx.texture_numbers.iter().enumerate() {
                if let Some(texture) = texture {
                    if usize::from(*texture) >= self.textures.len() {
                        return Err(AuthoringError::invalid(format!(
                            "material {material_index} ({:?}) texture slot {slot} references texture {texture}, but the model contains {} textures",
                            material.gx.name,
                            self.textures.len()
                        )));
                    }
                }
            }
            if material.base_color_texture.as_ref().is_some_and(|binding| {
                binding.texture as usize >= self.textures.len() || binding.tex_coord >= 8
            }) {
                return Err(AuthoringError::invalid(format!(
                    "material {material_index} has an invalid base-color texture binding"
                )));
            }
        }
        if let Some(collision) = &self.collision {
            collision.validate()?;
        }
        Ok(())
    }
}

fn validate_name(name: &str, context: &str) -> AuthoringResult<()> {
    let (_, _, had_errors) = SHIFT_JIS.encode(name);
    if had_errors {
        return Err(AuthoringError::invalid(format!(
            "{context} name {name:?} cannot be encoded as Shift-JIS"
        )));
    }
    Ok(())
}

fn validate_hierarchy(nodes: &[crate::ModelNode]) -> AuthoringResult<()> {
    for (parent_index, parent) in nodes.iter().enumerate() {
        for &child in &parent.children {
            if nodes[child as usize].parent != Some(parent_index as u32) {
                return Err(AuthoringError::invalid(format!(
                    "node {parent_index} and child {child} have inconsistent hierarchy metadata"
                )));
            }
        }
    }
    fn visit(index: usize, nodes: &[crate::ModelNode], states: &mut [u8]) -> AuthoringResult<()> {
        match states[index] {
            2 => return Ok(()),
            1 => return Err(AuthoringError::invalid("node hierarchy contains a cycle")),
            _ => {}
        }
        states[index] = 1;
        if let Some(parent) = nodes[index].parent {
            visit(parent as usize, nodes, states)?;
        }
        states[index] = 2;
        Ok(())
    }
    let mut states = vec![0; nodes.len()];
    for index in 0..nodes.len() {
        visit(index, nodes, &mut states)?;
    }
    Ok(())
}
