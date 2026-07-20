use std::collections::BTreeSet;
use std::fs;
use std::io::Cursor;
use std::path::{Component, Path, PathBuf};

use base64::Engine;
use encoding_rs::SHIFT_JIS;
use gltf::buffer;
use gltf::image as gltf_image;
use gltf::mesh::Mode;

use crate::collision::{cleanup_diagnostics, collision_group};
use crate::math::{
    add, convert_local_transform, convert_normal, convert_position, convert_tangent, cross,
    identity, mul, normalize, sub, transform_point, transform_reverses_winding,
    validate_conversion, Matrix4,
};
use crate::{
    AuthoringError, AuthoringResult, CollisionDocument, CollisionImportOptions,
    CollisionImportResult, CollisionNodeSelection, CollisionSource, ColorSet, Diagnostic,
    DiagnosticCode, GxTextureBinding, ImportResult, ImportedAlphaMode, ModelAssetDocument,
    ModelImportOptions, ModelMaterial, ModelMesh, ModelNode, ModelPrimitive, ModelTexture,
    NodePurpose, SourcePbrMetadata, TexCoordSet,
};

const MAX_NODE_COUNT: usize = 1_000_000;
const MAX_VERTEX_COUNT: usize = 16_000_000;
const MAX_IMAGE_DIMENSION: u32 = 16_384;
const MAX_DECODED_IMAGE_BYTES: usize = 512 * 1024 * 1024;

pub fn import_model(
    path: impl AsRef<Path>,
    options: &ModelImportOptions,
) -> AuthoringResult<ImportResult> {
    validate_conversion(&options.coordinate_conversion)?;
    let path = path.as_ref();
    let loaded = load_gltf(
        path,
        options.max_source_bytes,
        options.max_total_buffer_bytes,
    )?;
    reject_unsupported_features(&loaded.gltf)?;
    let mut diagnostics = Vec::new();
    let materials = import_materials(&loaded.gltf, &mut diagnostics)?;
    let textures = import_textures(path, &loaded, options.max_source_bytes)?;
    let meshes = import_meshes(
        &loaded.gltf,
        &loaded.buffers,
        &options.coordinate_conversion,
        &mut diagnostics,
    )?;
    let (mut nodes, scene_roots) = import_nodes(
        &loaded.gltf,
        &options.coordinate_conversion,
        &mut diagnostics,
    )?;

    if let CollisionSource::EmbeddedNodes {
        prefix,
        selected_nodes,
        ..
    } = &options.collision
    {
        for node in &mut nodes {
            if node_matches(&node.name, prefix, selected_nodes) {
                node.purpose = NodePurpose::CollisionOnly;
            }
        }
    }

    let name = validate_name(
        path.file_stem()
            .and_then(|stem| stem.to_str())
            .unwrap_or("model"),
        "asset",
    )?;
    let mut asset = ModelAssetDocument {
        format_version: crate::MODEL_ASSET_FORMAT_VERSION,
        coordinate_space: crate::ModelCoordinateSpace::GltfCompatible,
        name,
        scene_roots,
        nodes,
        meshes,
        materials,
        textures,
        collision: None,
        diagnostics: Vec::new(),
        acknowledged_diagnostics: BTreeSet::new(),
    };

    let collision_result = match &options.collision {
        CollisionSource::None => None,
        CollisionSource::RenderGeometry { surface } => Some(collision_from_asset(
            &asset,
            CollisionSelection::Renderable,
            &std::collections::BTreeMap::new(),
            surface,
            true,
        )?),
        CollisionSource::EmbeddedNodes {
            prefix,
            selected_nodes,
            surfaces_by_node,
            default_surface,
        } => Some(collision_from_asset(
            &asset,
            CollisionSelection::Named {
                prefix,
                selected_nodes,
            },
            surfaces_by_node,
            default_surface,
            false,
        )?),
        CollisionSource::SeparateFile {
            path: collision_path,
            options,
        } => Some(import_collision(collision_path, options)?),
    };
    if let Some(mut result) = collision_result {
        if let Some(simplification) = &options.collision_simplification {
            let report = result.collision.simplify(simplification)?;
            result.diagnostics.push(Diagnostic::info(
                DiagnosticCode::CollisionSimplified,
                format!(
                    "simplified collision from {} to {} triangles",
                    report.input_triangles, report.output_triangles
                ),
                None,
            ));
            result.simplification = Some(report);
        }
        diagnostics.extend(result.diagnostics);
        asset.collision = Some(result.collision);
    }
    asset.diagnostics = diagnostics.clone();
    asset.validate()?;
    Ok(ImportResult { asset, diagnostics })
}

pub fn import_collision(
    path: impl AsRef<Path>,
    options: &CollisionImportOptions,
) -> AuthoringResult<CollisionImportResult> {
    let model_options = ModelImportOptions {
        coordinate_conversion: options.coordinate_conversion,
        collision: CollisionSource::None,
        collision_simplification: None,
        max_source_bytes: options.max_source_bytes,
        max_total_buffer_bytes: options.max_total_buffer_bytes,
    };
    let imported = import_model(path, &model_options)?;
    let selection = match &options.node_selection {
        CollisionNodeSelection::AllGeometry => CollisionSelection::All,
        CollisionNodeSelection::Named {
            prefix,
            selected_nodes,
        } => CollisionSelection::Named {
            prefix,
            selected_nodes,
        },
    };
    let mut result = collision_from_asset(
        &imported.asset,
        selection,
        &options.surfaces_by_node,
        &options.default_surface,
        false,
    )?;
    if let Some(simplification) = &options.simplification {
        let report = result.collision.simplify(simplification)?;
        result.diagnostics.push(Diagnostic::info(
            DiagnosticCode::CollisionSimplified,
            format!(
                "simplified collision from {} to {} triangles",
                report.input_triangles, report.output_triangles
            ),
            None,
        ));
        result.simplification = Some(report);
    }
    result.diagnostics.splice(0..0, imported.diagnostics);
    Ok(result)
}

struct LoadedGltf {
    gltf: gltf::Gltf,
    buffers: Vec<Vec<u8>>,
    root: PathBuf,
}

fn load_gltf(
    path: &Path,
    max_source_bytes: usize,
    max_total_bytes: usize,
) -> AuthoringResult<LoadedGltf> {
    if max_source_bytes == 0 || max_total_bytes == 0 {
        return Err(AuthoringError::invalid(
            "source and total buffer byte limits must be greater than zero",
        ));
    }
    let source = read_bounded(path, max_source_bytes)?;
    preflight_resource_uris(&source)?;
    let gltf =
        gltf::Gltf::from_slice(&source).map_err(|error| AuthoringError::Gltf(error.to_string()))?;
    let root = path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .to_path_buf();
    let mut buffers = Vec::with_capacity(gltf.document.buffers().len());
    let mut total_bytes = 0usize;
    for source_buffer in gltf.document.buffers() {
        let bytes = match source_buffer.source() {
            buffer::Source::Bin => gltf
                .blob
                .as_ref()
                .ok_or_else(|| {
                    AuthoringError::invalid("glTF buffer requests a missing GLB BIN chunk")
                })?
                .clone(),
            buffer::Source::Uri(uri) => resolve_uri(uri, &root, max_source_bytes)?,
        };
        if bytes.len() < source_buffer.length() {
            return Err(AuthoringError::invalid(format!(
                "buffer {} declares {} bytes but only {} were loaded",
                source_buffer.index(),
                source_buffer.length(),
                bytes.len()
            )));
        }
        total_bytes = total_bytes
            .checked_add(bytes.len())
            .ok_or_else(|| AuthoringError::invalid("total buffer byte count overflowed"))?;
        if total_bytes > max_total_bytes {
            return Err(AuthoringError::security(
                "buffer_budget_exceeded",
                format!("glTF buffers total {total_bytes} bytes; limit is {max_total_bytes}"),
            ));
        }
        buffers.push(bytes);
    }
    Ok(LoadedGltf {
        gltf,
        buffers,
        root,
    })
}

fn reject_unsupported_features(gltf: &gltf::Gltf) -> AuthoringResult<()> {
    if gltf.document.skins().next().is_some() {
        return Err(AuthoringError::unsupported(
            "skinning",
            "skins are not supported by the initial rigid-model importer",
        ));
    }
    if gltf.document.animations().next().is_some() {
        return Err(AuthoringError::unsupported(
            "animation",
            "animations are not supported by the initial model importer",
        ));
    }
    if gltf.document.nodes().len() > MAX_NODE_COUNT {
        return Err(AuthoringError::invalid(format!(
            "{} nodes exceed the safety limit of {MAX_NODE_COUNT}",
            gltf.document.nodes().len()
        )));
    }
    for mesh in gltf.document.meshes() {
        for primitive in mesh.primitives() {
            if primitive.morph_targets().next().is_some() {
                return Err(AuthoringError::unsupported(
                    "morph_targets",
                    format!("mesh {} contains morph targets", mesh.index()),
                ));
            }
            if primitive.attributes().any(|(semantic, _)| {
                matches!(
                    semantic,
                    gltf::Semantic::Joints(_) | gltf::Semantic::Weights(_)
                )
            }) {
                return Err(AuthoringError::unsupported(
                    "skinning_attributes",
                    format!(
                        "mesh {} contains JOINTS or WEIGHTS attributes",
                        mesh.index()
                    ),
                ));
            }
            for (semantic, _) in primitive.attributes() {
                match semantic {
                    gltf::Semantic::TexCoords(set) if set >= 8 => {
                        return Err(AuthoringError::unsupported(
                            "texture_coordinate_set",
                            format!(
                                "mesh {} uses TEXCOORD_{set}; GX supports sets 0 through 7",
                                mesh.index()
                            ),
                        ));
                    }
                    gltf::Semantic::Colors(set) if set >= 2 => {
                        return Err(AuthoringError::unsupported(
                            "color_set",
                            format!(
                                "mesh {} uses COLOR_{set}; GX supports sets 0 and 1",
                                mesh.index()
                            ),
                        ));
                    }
                    _ => {}
                }
            }
            if matches!(
                primitive.mode(),
                Mode::Points | Mode::Lines | Mode::LineLoop | Mode::LineStrip
            ) {
                return Err(AuthoringError::unsupported(
                    "non_triangle_primitive",
                    format!(
                        "mesh {} uses {:?} primitives",
                        mesh.index(),
                        primitive.mode()
                    ),
                ));
            }
        }
    }
    Ok(())
}

fn import_meshes(
    gltf: &gltf::Gltf,
    buffers: &[Vec<u8>],
    conversion: &crate::CoordinateConversion,
    diagnostics: &mut Vec<Diagnostic>,
) -> AuthoringResult<Vec<ModelMesh>> {
    gltf.document
        .meshes()
        .map(|mesh| {
            let mesh_name = validate_name(
                mesh.name().unwrap_or(&format!("mesh_{}", mesh.index())),
                "mesh",
            )?;
            let primitives = mesh
                .primitives()
                .enumerate()
                .map(|(primitive_index, primitive)| {
                    let context = format!("{mesh_name}/primitive_{primitive_index}");
                    let reader =
                        primitive.reader(|buffer| buffers.get(buffer.index()).map(Vec::as_slice));
                    let positions = reader
                        .read_positions()
                        .ok_or_else(|| {
                            AuthoringError::invalid(format!("{context} has no POSITION accessor"))
                        })?
                        .map(|position| convert_position(position, conversion))
                        .collect::<Vec<_>>();
                    if positions.len() > MAX_VERTEX_COUNT {
                        return Err(AuthoringError::invalid(format!(
                            "{context} has {} vertices; limit is {MAX_VERTEX_COUNT}",
                            positions.len()
                        )));
                    }
                    if positions.iter().flatten().any(|value| !value.is_finite()) {
                        return Err(AuthoringError::invalid(format!(
                            "{context} contains a non-finite position"
                        )));
                    }
                    let source_indices = reader
                        .read_indices()
                        .map(|indices| indices.into_u32().collect::<Vec<_>>())
                        .unwrap_or_else(|| (0..positions.len() as u32).collect());
                    if source_indices
                        .iter()
                        .any(|&index| index as usize >= positions.len())
                    {
                        return Err(AuthoringError::invalid(format!(
                            "{context} contains an out-of-range index"
                        )));
                    }
                    let indices = triangulate(
                        primitive.mode(),
                        &source_indices,
                        conversion.reverse_winding,
                        &context,
                    )?;

                    let normals = if let Some(normals) = reader.read_normals() {
                        let normals = normals
                            .map(|normal| convert_normal(normal, conversion))
                            .collect::<AuthoringResult<Vec<_>>>()?;
                        require_attribute_count(
                            &context,
                            "NORMAL",
                            positions.len(),
                            normals.len(),
                        )?;
                        normals
                    } else {
                        diagnostics.push(Diagnostic::warning(
                            DiagnosticCode::GeneratedNormals,
                            "generated smooth normals because the primitive has no NORMAL accessor",
                            Some(context.clone()),
                            false,
                        ));
                        generate_normals(&positions, &indices)
                    };
                    let tangents = reader
                        .read_tangents()
                        .map(|values| {
                            values
                                .map(|tangent| convert_tangent(tangent, conversion))
                                .collect::<Vec<_>>()
                        })
                        .unwrap_or_default();
                    if !tangents.is_empty() {
                        require_attribute_count(
                            &context,
                            "TANGENT",
                            positions.len(),
                            tangents.len(),
                        )?;
                    }
                    let mut tex_coords = Vec::new();
                    for set in 0..8 {
                        if let Some(values) = reader.read_tex_coords(set) {
                            let values = values.into_f32().collect::<Vec<_>>();
                            require_attribute_count(
                                &context,
                                &format!("TEXCOORD_{set}"),
                                positions.len(),
                                values.len(),
                            )?;
                            tex_coords.push(TexCoordSet {
                                set: set as u8,
                                values,
                            });
                        }
                    }
                    let mut colors = Vec::new();
                    for set in 0..2 {
                        if let Some(values) = reader.read_colors(set) {
                            let values = values.into_rgba_f32().collect::<Vec<_>>();
                            require_attribute_count(
                                &context,
                                &format!("COLOR_{set}"),
                                positions.len(),
                                values.len(),
                            )?;
                            colors.push(ColorSet {
                                set: set as u8,
                                values,
                            });
                        }
                    }
                    if indices.is_empty() {
                        diagnostics.push(Diagnostic::warning(
                            DiagnosticCode::EmptyPrimitive,
                            "primitive contains no complete triangles",
                            Some(context),
                            false,
                        ));
                    }
                    Ok(ModelPrimitive {
                        positions,
                        normals,
                        tangents,
                        tex_coords,
                        colors,
                        indices,
                        material: primitive.material().index().map(|index| index as u32),
                    })
                })
                .collect::<AuthoringResult<Vec<_>>>()?;
            Ok(ModelMesh {
                name: mesh_name,
                primitives,
            })
        })
        .collect()
}

fn import_nodes(
    gltf: &gltf::Gltf,
    conversion: &crate::CoordinateConversion,
    diagnostics: &mut Vec<Diagnostic>,
) -> AuthoringResult<(Vec<ModelNode>, Vec<u32>)> {
    let scene_count = gltf.document.scenes().len();
    if scene_count > 1 {
        diagnostics.push(Diagnostic::warning(
            DiagnosticCode::MultipleScenes,
            format!("glTF contains {scene_count} scenes; only the default scene is active"),
            None,
            false,
        ));
    }
    let active_scene = gltf
        .document
        .default_scene()
        .or_else(|| gltf.document.scenes().next());
    let scene_roots = active_scene
        .map(|scene| scene.nodes().map(|node| node.index() as u32).collect())
        .unwrap_or_default();
    let mut parents = vec![None; gltf.document.nodes().len()];
    for node in gltf.document.nodes() {
        for child in node.children() {
            if parents[child.index()]
                .replace(node.index() as u32)
                .is_some()
            {
                return Err(AuthoringError::invalid(format!(
                    "node {} has multiple parents",
                    child.index()
                )));
            }
        }
    }
    let nodes = gltf
        .document
        .nodes()
        .map(|node| {
            let name = validate_name(
                node.name().unwrap_or(&format!("node_{}", node.index())),
                "node",
            )?;
            let local_transform = convert_local_transform(node.transform().matrix(), conversion)?;
            Ok(ModelNode {
                name,
                parent: parents[node.index()],
                children: node.children().map(|child| child.index() as u32).collect(),
                mesh: node.mesh().map(|mesh| mesh.index() as u32),
                purpose: NodePurpose::Render,
                local_transform,
            })
        })
        .collect::<AuthoringResult<Vec<_>>>()?;
    validate_node_hierarchy(&nodes)?;
    Ok((nodes, scene_roots))
}

fn import_materials(
    gltf: &gltf::Gltf,
    diagnostics: &mut Vec<Diagnostic>,
) -> AuthoringResult<Vec<ModelMaterial>> {
    gltf.document
        .materials()
        .enumerate()
        .map(|(index, material)| {
            let name = validate_name(
                material.name().unwrap_or(&format!("material_{index}")),
                "material",
            )?;
            let pbr = material.pbr_metallic_roughness();
            let source_pbr = SourcePbrMetadata {
                metallic_factor: pbr.metallic_factor(),
                roughness_factor: pbr.roughness_factor(),
                has_metallic_roughness_texture: pbr.metallic_roughness_texture().is_some(),
                has_normal_texture: material.normal_texture().is_some(),
                has_occlusion_texture: material.occlusion_texture().is_some(),
                emissive_factor: material.emissive_factor(),
                has_emissive_texture: material.emissive_texture().is_some(),
            };
            if source_pbr.metallic_factor != 1.0
                || source_pbr.roughness_factor != 1.0
                || source_pbr.has_metallic_roughness_texture
            {
                diagnostics.push(unmapped_material_diagnostic(
                    DiagnosticCode::UnmappedMetallicRoughness,
                    &name,
                    "metallic/roughness inputs are retained as metadata but are not mapped to GX",
                ));
            }
            if source_pbr.has_normal_texture {
                diagnostics.push(unmapped_material_diagnostic(
                    DiagnosticCode::UnmappedNormalTexture,
                    &name,
                    "normal textures are retained as metadata but are not mapped to GX",
                ));
            }
            if source_pbr.has_occlusion_texture {
                diagnostics.push(unmapped_material_diagnostic(
                    DiagnosticCode::UnmappedOcclusionTexture,
                    &name,
                    "occlusion textures are retained as metadata but are not mapped to GX",
                ));
            }
            if source_pbr.has_emissive_texture || source_pbr.emissive_factor != [0.0; 3] {
                diagnostics.push(unmapped_material_diagnostic(
                    DiagnosticCode::UnmappedEmissive,
                    &name,
                    "emissive inputs are retained as metadata but are not mapped to GX",
                ));
            }
            let base_color_texture = pbr
                .base_color_texture()
                .map(|info| {
                    let tex_coord = u8::try_from(info.tex_coord()).map_err(|_| {
                        AuthoringError::invalid(format!(
                            "material {name} texture coordinate index does not fit in u8"
                        ))
                    })?;
                    if tex_coord >= 8 {
                        return Err(AuthoringError::unsupported(
                            "texture_coordinate_set",
                            format!("material {name} uses TEXCOORD_{tex_coord}; GX supports sets 0 through 7"),
                        ));
                    }
                    Ok(GxTextureBinding {
                        texture: info.texture().index() as u32,
                        tex_coord,
                    })
                })
                .transpose()?;
            let source_alpha_mode = match material.alpha_mode() {
                gltf::material::AlphaMode::Opaque => ImportedAlphaMode::Opaque,
                gltf::material::AlphaMode::Mask => ImportedAlphaMode::Mask {
                    cutoff: material.alpha_cutoff().unwrap_or(0.5),
                },
                gltf::material::AlphaMode::Blend => ImportedAlphaMode::Blend,
            };
            let mut gx = sms_formats::GxMaterial {
                name: name.clone(),
                cull_mode: if material.double_sided() { 0 } else { 2 },
                color_channel_count: 1,
                material_colors: [Some(float_color_to_rgba8(pbr.base_color_factor())), None],
                color_channels: [
                    Some(sms_formats::GxColorChannel::default()),
                    Some(sms_formats::GxColorChannel::default()),
                    None,
                    None,
                ],
                ..sms_formats::GxMaterial::default()
            };
            if let Some(binding) = &base_color_texture {
                gx.texture_numbers[0] = Some(u16::try_from(binding.texture).map_err(|_| {
                    AuthoringError::invalid(format!(
                        "material {name} texture index does not fit in u16"
                    ))
                })?);
                gx.tex_gen_count = 1;
                gx.tex_gens[0] = Some(sms_formats::GxTexCoordGen {
                    function: 1,
                    source: 4 + binding.tex_coord,
                    matrix: 60,
                });
                gx.tev_orders[0] = Some(sms_formats::GxTevOrder {
                    tex_coord: Some(0),
                    tex_map: Some(0),
                    // GX_COLOR0A0 supplies the base-color factor through
                    // RASC/RASA. GX_COLOR_NULL selects raster zero and would
                    // make the conservative TEXC * RASC program invisible.
                    color_channel: 4,
                });
                if let Some(stage) = &mut gx.tev_stages[0] {
                    stage.color_inputs = [15, 8, 10, 15];
                    stage.alpha_inputs = [7, 4, 5, 7];
                }
            } else {
                gx.tev_orders[0] = Some(sms_formats::GxTevOrder {
                    tex_coord: None,
                    tex_map: None,
                    color_channel: 4,
                });
                if let Some(stage) = &mut gx.tev_stages[0] {
                    stage.color_inputs = [10, 15, 15, 15];
                    stage.alpha_inputs = [5, 7, 7, 7];
                }
            }
            match source_alpha_mode {
                ImportedAlphaMode::Opaque => {}
                ImportedAlphaMode::Mask { cutoff } => {
                    gx.alpha_compare = sms_formats::GxAlphaCompare {
                        comparison_0: 6,
                        reference_0: (cutoff.clamp(0.0, 1.0) * 255.0).round() as u8,
                        operation: 0,
                        comparison_1: 7,
                        reference_1: 0,
                    };
                }
                ImportedAlphaMode::Blend => {
                    gx.blend_mode = sms_formats::GxBlendMode {
                        mode: 1,
                        source_factor: 4,
                        destination_factor: 5,
                        logic_operation: 0xf,
                    };
                    gx.depth_mode.update_enabled = 0;
                    gx.z_compare_location = 0;
                }
            }
            Ok(ModelMaterial {
                gx,
                source_base_color: pbr.base_color_factor(),
                base_color_texture,
                vertex_color_set: None,
                source_double_sided: material.double_sided(),
                source_alpha_mode,
                source_pbr,
            })
        })
        .collect()
}

fn import_textures(
    source_path: &Path,
    loaded: &LoadedGltf,
    max_source_bytes: usize,
) -> AuthoringResult<Vec<ModelTexture>> {
    let mut decoded_images = Vec::with_capacity(loaded.gltf.document.images().len());
    for source_image in loaded.gltf.document.images() {
        let encoded = match source_image.source() {
            gltf_image::Source::Uri { uri, .. } => {
                resolve_uri(uri, &loaded.root, max_source_bytes)?
            }
            gltf_image::Source::View { view, .. } => {
                let buffer = loaded.buffers.get(view.buffer().index()).ok_or_else(|| {
                    AuthoringError::invalid(format!(
                        "image {} references missing buffer {}",
                        source_image.index(),
                        view.buffer().index()
                    ))
                })?;
                let end = view
                    .offset()
                    .checked_add(view.length())
                    .ok_or_else(|| AuthoringError::invalid("image buffer view range overflowed"))?;
                buffer
                    .get(view.offset()..end)
                    .ok_or_else(|| {
                        AuthoringError::invalid(format!(
                            "image {} buffer view is out of bounds",
                            source_image.index()
                        ))
                    })?
                    .to_vec()
            }
        };
        let reader = ::image::ImageReader::new(Cursor::new(&encoded))
            .with_guessed_format()
            .map_err(|error| {
                AuthoringError::invalid(format!("could not identify image: {error}"))
            })?;
        let decoded = reader.decode().map_err(|error| {
            AuthoringError::invalid(format!(
                "could not decode image {} from {}: {error}",
                source_image.index(),
                source_path.display()
            ))
        })?;
        if decoded.width() > MAX_IMAGE_DIMENSION || decoded.height() > MAX_IMAGE_DIMENSION {
            return Err(AuthoringError::invalid(format!(
                "image {} is {}x{}; maximum dimension is {MAX_IMAGE_DIMENSION}",
                source_image.index(),
                decoded.width(),
                decoded.height()
            )));
        }
        let rgba = decoded.to_rgba8();
        if rgba.len() > MAX_DECODED_IMAGE_BYTES {
            return Err(AuthoringError::invalid(format!(
                "image {} decodes to {} bytes; maximum is {MAX_DECODED_IMAGE_BYTES}",
                source_image.index(),
                rgba.len()
            )));
        }
        decoded_images.push((rgba.width(), rgba.height(), rgba.into_raw()));
    }

    loaded
        .gltf
        .document
        .textures()
        .map(|texture| {
            let source = texture.source();
            let (width, height, rgba8) = decoded_images
                .get(source.index())
                .ok_or_else(|| {
                    AuthoringError::invalid(format!(
                        "texture {} references a missing image",
                        texture.index()
                    ))
                })?
                .clone();
            let sampler = texture.sampler();
            Ok(ModelTexture {
                name: validate_name(
                    texture
                        .name()
                        .unwrap_or(&format!("texture_{}", texture.index())),
                    "texture",
                )?,
                width,
                height,
                rgba8,
                encode_options: texture_encode_options(&sampler),
            })
        })
        .collect()
}

enum CollisionSelection<'a> {
    All,
    Renderable,
    Named {
        prefix: &'a str,
        selected_nodes: &'a BTreeSet<String>,
    },
}

fn collision_from_asset(
    asset: &ModelAssetDocument,
    selection: CollisionSelection<'_>,
    surfaces_by_node: &std::collections::BTreeMap<String, crate::CollisionSurface>,
    default_surface: &crate::CollisionSurface,
    normalize_render_terrain_winding: bool,
) -> AuthoringResult<CollisionImportResult> {
    let global_transforms = global_transforms(&asset.nodes)?;
    let active_nodes = active_node_mask(asset)?;
    let mut collision = CollisionDocument {
        vertices: Vec::new(),
        groups: Vec::new(),
    };
    for (node_index, node) in asset.nodes.iter().enumerate() {
        let selected = active_nodes[node_index]
            && match &selection {
                CollisionSelection::All => true,
                CollisionSelection::Renderable => node.purpose == NodePurpose::Render,
                CollisionSelection::Named {
                    prefix,
                    selected_nodes,
                } => node_matches(&node.name, prefix, selected_nodes),
            };
        if !selected {
            continue;
        }
        let Some(mesh_index) = node.mesh else {
            continue;
        };
        let mesh = asset.meshes.get(mesh_index as usize).ok_or_else(|| {
            AuthoringError::invalid(format!("node {} references a missing mesh", node.name))
        })?;
        let surface = surfaces_by_node
            .get(&node.name)
            .unwrap_or(default_surface)
            .clone();
        for (primitive_index, primitive) in mesh.primitives.iter().enumerate() {
            let first_vertex = u32::try_from(collision.vertices.len()).map_err(|_| {
                AuthoringError::Collision("collision vertex count exceeds u32".to_string())
            })?;
            collision.vertices.extend(
                primitive
                    .positions
                    .iter()
                    .map(|&position| transform_point(global_transforms[node_index], position)),
            );
            let reverse_winding = transform_reverses_winding(global_transforms[node_index]);
            let triangles = primitive
                .indices
                .chunks_exact(3)
                .map(|triangle| {
                    if reverse_winding {
                        [
                            first_vertex + triangle[0],
                            first_vertex + triangle[2],
                            first_vertex + triangle[1],
                        ]
                    } else {
                        [
                            first_vertex + triangle[0],
                            first_vertex + triangle[1],
                            first_vertex + triangle[2],
                        ]
                    }
                })
                .collect();
            collision.groups.push(collision_group(
                format!("{}/primitive_{primitive_index}", node.name),
                surface.clone(),
                triangles,
            ));
        }
    }
    let winding_normalized =
        normalize_render_terrain_winding && normalize_inverted_terrain_shell(&mut collision);
    let cleanup = collision.cleanup_exact()?;
    let mut diagnostics = cleanup_diagnostics(cleanup);
    if winding_normalized {
        diagnostics.push(Diagnostic::info(
            DiagnosticCode::CollisionWindingNormalized,
            "reversed a predominantly downward-facing render-derived collision shell so walkable terrain faces upward",
            None,
        ));
    }
    Ok(CollisionImportResult {
        collision,
        diagnostics,
        cleanup,
        simplification: None,
    })
}

/// Migrates catalog assets created before render-derived collision winding was
/// normalized. Exact group-name parity proves the stored COL was generated
/// from the asset's active render primitives; embedded and separate collision
/// therefore retain their explicitly authored orientation.
pub(crate) fn normalize_legacy_render_collision_winding(
    asset: &mut ModelAssetDocument,
) -> AuthoringResult<bool> {
    let active = active_node_mask(asset)?;
    let mut expected_groups = BTreeSet::new();
    for (node_index, node) in asset.nodes.iter().enumerate() {
        if !active[node_index] || node.purpose != NodePurpose::Render {
            continue;
        }
        let Some(mesh_index) = node.mesh else {
            continue;
        };
        let mesh = asset.meshes.get(mesh_index as usize).ok_or_else(|| {
            AuthoringError::invalid(format!(
                "node {} references missing mesh {mesh_index}",
                node.name
            ))
        })?;
        for primitive_index in 0..mesh.primitives.len() {
            expected_groups.insert(format!("{}/primitive_{primitive_index}", node.name));
        }
    }
    let Some(collision) = asset.collision.as_mut() else {
        return Ok(false);
    };
    if collision.groups.len() != expected_groups.len()
        || collision
            .groups
            .iter()
            .any(|group| !expected_groups.contains(&group.name))
    {
        return Ok(false);
    }
    Ok(normalize_inverted_terrain_shell(collision))
}

/// Repairs render-derived terrain exported by tools that preserve an inverted
/// whole-model winding. Sunshine derives floor, roof, and wall classification
/// directly from COL triangle order, so an inverted shell makes every visible
/// floor behave as a roof.
///
/// The decision is global rather than per triangle or material: this preserves
/// the relative orientation of floors, walls, and ceilings. Only faces that
/// Sunshine itself would classify as floor/roof contribute to the decision.
pub(crate) fn normalize_inverted_terrain_shell(collision: &mut CollisionDocument) -> bool {
    let mut upward_projected_area = 0.0_f64;
    let mut downward_projected_area = 0.0_f64;
    for group in &collision.groups {
        for triangle in &group.triangles {
            let [Some(a), Some(b), Some(c)] =
                triangle.map(|index| collision.vertices.get(index as usize).copied())
            else {
                continue;
            };
            let face = cross(sub(b, a), sub(c, a));
            let length_squared = face[0] * face[0] + face[1] * face[1] + face[2] * face[2];
            if length_squared == 0.0 || face[1] * face[1] <= 0.04 * length_squared {
                continue;
            }
            if face[1] > 0.0 {
                upward_projected_area += f64::from(face[1]);
            } else {
                downward_projected_area += f64::from(-face[1]);
            }
        }
    }

    // Require a meaningful majority so closed or intentionally two-sided
    // shells with balanced floors and ceilings retain their authored winding.
    if downward_projected_area <= upward_projected_area * 1.05
        || downward_projected_area <= f64::EPSILON
    {
        return false;
    }
    for group in &mut collision.groups {
        for triangle in &mut group.triangles {
            triangle.swap(1, 2);
        }
    }
    true
}

pub(crate) fn active_node_mask(asset: &ModelAssetDocument) -> AuthoringResult<Vec<bool>> {
    if asset.scene_roots.is_empty() {
        return Ok(vec![true; asset.nodes.len()]);
    }
    let mut active = vec![false; asset.nodes.len()];
    let mut pending = asset.scene_roots.clone();
    while let Some(index) = pending.pop() {
        let node = asset.nodes.get(index as usize).ok_or_else(|| {
            AuthoringError::invalid(format!("scene root or child {index} is out of range"))
        })?;
        if std::mem::replace(&mut active[index as usize], true) {
            continue;
        }
        pending.extend(node.children.iter().copied());
    }
    Ok(active)
}

pub(crate) fn global_transforms(nodes: &[ModelNode]) -> AuthoringResult<Vec<Matrix4>> {
    fn resolve(
        index: usize,
        nodes: &[ModelNode],
        states: &mut [u8],
        result: &mut [Matrix4],
    ) -> AuthoringResult<Matrix4> {
        match states[index] {
            2 => return Ok(result[index]),
            1 => return Err(AuthoringError::invalid("node hierarchy contains a cycle")),
            _ => {}
        }
        states[index] = 1;
        let matrix = if let Some(parent) = nodes[index].parent {
            mul(
                resolve(parent as usize, nodes, states, result)?,
                nodes[index].local_transform,
            )
        } else {
            nodes[index].local_transform
        };
        result[index] = matrix;
        states[index] = 2;
        Ok(matrix)
    }
    let mut states = vec![0; nodes.len()];
    let mut result = vec![identity(); nodes.len()];
    for index in 0..nodes.len() {
        resolve(index, nodes, &mut states, &mut result)?;
    }
    Ok(result)
}

fn triangulate(
    mode: Mode,
    source: &[u32],
    reverse_winding: bool,
    context: &str,
) -> AuthoringResult<Vec<u32>> {
    let mut triangles = Vec::new();
    match mode {
        Mode::Triangles => {
            if !source.len().is_multiple_of(3) {
                return Err(AuthoringError::invalid(format!(
                    "{context} triangle index count {} is not divisible by three",
                    source.len()
                )));
            }
            triangles.extend_from_slice(source);
        }
        Mode::TriangleStrip => {
            for index in 0..source.len().saturating_sub(2) {
                if index % 2 == 0 {
                    triangles.extend_from_slice(&[
                        source[index],
                        source[index + 1],
                        source[index + 2],
                    ]);
                } else {
                    triangles.extend_from_slice(&[
                        source[index + 1],
                        source[index],
                        source[index + 2],
                    ]);
                }
            }
        }
        Mode::TriangleFan => {
            for index in 1..source.len().saturating_sub(1) {
                triangles.extend_from_slice(&[source[0], source[index], source[index + 1]]);
            }
        }
        _ => {
            return Err(AuthoringError::unsupported(
                "non_triangle_primitive",
                format!("{context} uses {mode:?}"),
            ));
        }
    }
    if reverse_winding {
        for triangle in triangles.chunks_exact_mut(3) {
            triangle.swap(1, 2);
        }
    }
    Ok(triangles)
}

fn generate_normals(positions: &[[f32; 3]], indices: &[u32]) -> Vec<[f32; 3]> {
    let mut normals = vec![[0.0; 3]; positions.len()];
    for triangle in indices.chunks_exact(3) {
        let a = positions[triangle[0] as usize];
        let b = positions[triangle[1] as usize];
        let c = positions[triangle[2] as usize];
        let face = cross(sub(b, a), sub(c, a));
        for &index in triangle {
            normals[index as usize] = add(normals[index as usize], face);
        }
    }
    normals.into_iter().map(normalize).collect()
}

fn require_attribute_count(
    context: &str,
    semantic: &str,
    expected: usize,
    actual: usize,
) -> AuthoringResult<()> {
    if actual != expected {
        return Err(AuthoringError::invalid(format!(
            "{context} {semantic} has {actual} values; expected {expected}"
        )));
    }
    Ok(())
}

fn validate_node_hierarchy(nodes: &[ModelNode]) -> AuthoringResult<()> {
    let _ = global_transforms(nodes)?;
    for (node_index, node) in nodes.iter().enumerate() {
        for &child in &node.children {
            if nodes[child as usize].parent != Some(node_index as u32) {
                return Err(AuthoringError::invalid(format!(
                    "node {node_index} child {child} has inconsistent parent metadata"
                )));
            }
        }
    }
    Ok(())
}

fn validate_name(name: &str, kind: &str) -> AuthoringResult<String> {
    let (_, _, had_errors) = SHIFT_JIS.encode(name);
    if had_errors {
        return Err(AuthoringError::unsupported(
            "unencodable_name",
            format!("{kind} name {name:?} cannot be encoded as Shift-JIS"),
        ));
    }
    Ok(name.to_string())
}

fn node_matches(name: &str, prefix: &str, selected_nodes: &BTreeSet<String>) -> bool {
    name.starts_with(prefix) || selected_nodes.contains(name)
}

fn unmapped_material_diagnostic(code: DiagnosticCode, material: &str, message: &str) -> Diagnostic {
    Diagnostic::warning(code, message, Some(material.to_string()), true)
}

fn read_bounded(path: &Path, max_bytes: usize) -> AuthoringResult<Vec<u8>> {
    let metadata = fs::metadata(path).map_err(|source| AuthoringError::io(path, source))?;
    if metadata.len() > max_bytes as u64 {
        return Err(AuthoringError::security(
            "resource_too_large",
            format!(
                "{} is {} bytes; limit is {max_bytes}",
                path.display(),
                metadata.len()
            ),
        ));
    }
    let bytes = fs::read(path).map_err(|source| AuthoringError::io(path, source))?;
    if bytes.len() > max_bytes {
        return Err(AuthoringError::security(
            "resource_too_large",
            format!("{} grew beyond the {max_bytes}-byte limit", path.display()),
        ));
    }
    Ok(bytes)
}

fn resolve_uri(uri: &str, root: &Path, max_bytes: usize) -> AuthoringResult<Vec<u8>> {
    if uri
        .get(..5)
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case("data:"))
    {
        return decode_data_uri(uri, max_bytes);
    }
    let decoded = validate_external_uri_syntax(uri)?;
    let relative = Path::new(&decoded);
    let canonical_root =
        fs::canonicalize(root).map_err(|source| AuthoringError::io(root, source))?;
    let candidate = root.join(relative);
    let canonical_candidate =
        fs::canonicalize(&candidate).map_err(|source| AuthoringError::io(&candidate, source))?;
    if !canonical_candidate.starts_with(&canonical_root) {
        return Err(AuthoringError::security(
            "path_traversal",
            format!("resource path {uri:?} resolves outside the model directory"),
        ));
    }
    read_bounded(&canonical_candidate, max_bytes)
}

fn validate_external_uri_syntax(uri: &str) -> AuthoringResult<String> {
    if uri.contains("//") && uri.find("//").is_some_and(|offset| offset <= 8)
        || uri.contains(':')
        || uri.contains('\\')
        || uri.contains('?')
        || uri.contains('#')
    {
        return Err(AuthoringError::security(
            "external_uri",
            format!("network, absolute, and non-path URI {uri:?} is not allowed"),
        ));
    }
    let decoded = decode_percent(uri)?;
    if decoded.contains('\0') || decoded.contains(':') || decoded.contains('\\') {
        return Err(AuthoringError::security(
            "invalid_path",
            "resource path contains a NUL byte",
        ));
    }
    let relative = Path::new(&decoded);
    if relative.is_absolute()
        || relative.components().any(|component| {
            matches!(
                component,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        })
    {
        return Err(AuthoringError::security(
            "path_traversal",
            format!("resource path {uri:?} escapes the model directory"),
        ));
    }
    Ok(decoded)
}

fn preflight_resource_uris(source: &[u8]) -> AuthoringResult<()> {
    let json_bytes = if source.starts_with(b"glTF") {
        if source.len() < 20 {
            return Ok(());
        }
        let chunk_length = u32::from_le_bytes(source[12..16].try_into().expect("four bytes"));
        let chunk_type = u32::from_le_bytes(source[16..20].try_into().expect("four bytes"));
        if chunk_type != 0x4e4f_534a {
            return Ok(());
        }
        let Some(end) = 20usize.checked_add(chunk_length as usize) else {
            return Ok(());
        };
        let Some(json) = source.get(20..end) else {
            return Ok(());
        };
        json
    } else {
        source
    };
    let Ok(value) = serde_json::from_slice::<serde_json::Value>(json_bytes) else {
        return Ok(());
    };
    fn visit(value: &serde_json::Value) -> AuthoringResult<()> {
        match value {
            serde_json::Value::Object(object) => {
                for (key, value) in object {
                    if key == "uri" {
                        if let Some(uri) = value.as_str() {
                            if !uri
                                .get(..5)
                                .is_some_and(|prefix| prefix.eq_ignore_ascii_case("data:"))
                            {
                                validate_external_uri_syntax(uri)?;
                            }
                        }
                    } else {
                        visit(value)?;
                    }
                }
            }
            serde_json::Value::Array(values) => {
                for value in values {
                    visit(value)?;
                }
            }
            _ => {}
        }
        Ok(())
    }
    visit(&value)
}

fn decode_data_uri(uri: &str, max_bytes: usize) -> AuthoringResult<Vec<u8>> {
    let (metadata, payload) = uri[5..]
        .split_once(',')
        .ok_or_else(|| AuthoringError::invalid("data URI is missing its comma separator"))?;
    let bytes = if metadata
        .split(';')
        .any(|component| component.eq_ignore_ascii_case("base64"))
    {
        base64::engine::general_purpose::STANDARD
            .decode(payload)
            .map_err(|error| AuthoringError::invalid(format!("invalid base64 data URI: {error}")))?
    } else {
        decode_percent_bytes(payload)?
    };
    if bytes.len() > max_bytes {
        return Err(AuthoringError::security(
            "resource_too_large",
            format!(
                "data URI decodes to {} bytes; limit is {max_bytes}",
                bytes.len()
            ),
        ));
    }
    Ok(bytes)
}

fn decode_percent(value: &str) -> AuthoringResult<String> {
    String::from_utf8(decode_percent_bytes(value)?)
        .map_err(|_| AuthoringError::security("invalid_path", "resource URI is not valid UTF-8"))
}

fn decode_percent_bytes(value: &str) -> AuthoringResult<Vec<u8>> {
    let bytes = value.as_bytes();
    let mut output = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            if index + 2 >= bytes.len() {
                return Err(AuthoringError::invalid("truncated percent escape in URI"));
            }
            let high = hex_digit(bytes[index + 1])?;
            let low = hex_digit(bytes[index + 2])?;
            output.push((high << 4) | low);
            index += 3;
        } else {
            output.push(bytes[index]);
            index += 1;
        }
    }
    Ok(output)
}

fn hex_digit(value: u8) -> AuthoringResult<u8> {
    match value {
        b'0'..=b'9' => Ok(value - b'0'),
        b'a'..=b'f' => Ok(value - b'a' + 10),
        b'A'..=b'F' => Ok(value - b'A' + 10),
        _ => Err(AuthoringError::invalid("invalid percent escape in URI")),
    }
}

fn map_wrap(value: gltf::texture::WrappingMode) -> u8 {
    match value {
        gltf::texture::WrappingMode::ClampToEdge => 0,
        gltf::texture::WrappingMode::MirroredRepeat => 2,
        gltf::texture::WrappingMode::Repeat => 1,
    }
}

fn map_mag_filter(value: gltf::texture::MagFilter) -> u8 {
    match value {
        gltf::texture::MagFilter::Nearest => 0,
        gltf::texture::MagFilter::Linear => 1,
    }
}

fn map_min_filter(value: gltf::texture::MinFilter) -> u8 {
    match value {
        gltf::texture::MinFilter::Nearest => 0,
        gltf::texture::MinFilter::Linear => 1,
        gltf::texture::MinFilter::NearestMipmapNearest => 2,
        gltf::texture::MinFilter::LinearMipmapNearest => 3,
        gltf::texture::MinFilter::NearestMipmapLinear => 4,
        gltf::texture::MinFilter::LinearMipmapLinear => 5,
    }
}

fn texture_encode_options(
    sampler: &gltf::texture::Sampler<'_>,
) -> sms_formats::GxTextureEncodeOptions {
    let min_filter = sampler.min_filter().map(map_min_filter).unwrap_or(5);
    sms_formats::GxTextureEncodeOptions {
        encoding: sms_formats::GxTextureEncoding::AutoLossless,
        palette_format: sms_formats::GxPaletteFormat::Rgb5A3,
        mip_count: if min_filter >= 2 { 0 } else { 1 },
        sampler: sms_formats::GxSampler {
            wrap_s: map_wrap(sampler.wrap_s()),
            wrap_t: map_wrap(sampler.wrap_t()),
            min_filter,
            mag_filter: sampler.mag_filter().map(map_mag_filter).unwrap_or(1),
            ..sms_formats::GxSampler::default()
        },
    }
}

fn float_color_to_rgba8(color: [f32; 4]) -> [u8; 4] {
    color.map(|component| (component.clamp(0.0, 1.0) * 255.0).round() as u8)
}
