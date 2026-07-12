use super::*;

enum RetailLevelTransform {
    Sink { event_index: usize },
    ScaleGate,
    WallRock,
}

pub(super) fn level_transform_targets(
    document: &StageDocument,
    model_path: &str,
    file: &J3dFile,
) -> Vec<LevelTransformTarget> {
    let normalized = model_path.replace('\\', "/").to_ascii_lowercase();
    let is_map_model =
        normalized.ends_with("!/map/map/map.bmd") || normalized.ends_with("/map/map/map.bmd");
    let pollution_layer_index = pollution_layer_model_index(&normalized);
    if !is_map_model && pollution_layer_index.is_none() {
        return Vec::new();
    }

    let mut targets = BTreeMap::<usize, LevelTransformTarget>::new();
    for asset in document.assets.iter().filter(|asset| {
        asset.kind == StageAssetKind::Placement
            && asset
                .path
                .to_string_lossy()
                .replace('\\', "/")
                .to_ascii_lowercase()
                .ends_with("!/map/scene.bin")
    }) {
        let Ok(bytes) = read_stage_asset_bytes(&asset.path) else {
            continue;
        };
        let Ok(records) = parse_jdrama_object_records(&bytes) else {
            continue;
        };
        for record in records {
            let lower_type = record.type_name.to_ascii_lowercase();
            if let (Some(layer_index), Some(event)) =
                (pollution_layer_index, record.map_event_sink.as_ref())
            {
                for building in event
                    .buildings
                    .iter()
                    .filter(|building| usize::from(building.pollution_layer_index) == layer_index)
                {
                    let object_index = usize::from(building.pollution_object_index);
                    if let Ok(Some(joint_index)) = file.runtime_joint_child_index(0, object_index) {
                        targets.insert(
                            joint_index,
                            LevelTransformTarget {
                                joint_index,
                                translation_offset: [0.0; 3],
                                scale_multiplier: [1.0; 3],
                                behavior: LevelTransformBehavior::HideAfterStart,
                            },
                        );
                    }
                    if let Ok(Some(joint_index)) =
                        file.runtime_joint_child_index(0, object_index + 1)
                    {
                        targets.insert(
                            joint_index,
                            LevelTransformTarget {
                                joint_index,
                                translation_offset: [0.0; 3],
                                scale_multiplier: [1.0; 3],
                                behavior: LevelTransformBehavior::AlwaysHidden,
                            },
                        );
                    }
                }
                continue;
            }
            if !is_map_model {
                continue;
            }
            let mut event_targets = Vec::<(u16, RetailLevelTransform)>::new();
            if let Some(event) = record.map_event_sink {
                for (event_index, building) in event.buildings.into_iter().enumerate() {
                    event_targets.push((
                        building.building_index,
                        RetailLevelTransform::Sink { event_index },
                    ));
                }
            } else if lower_type == "dolpiceventriccogate" {
                event_targets.push((1, RetailLevelTransform::ScaleGate));
            } else if lower_type == "dolpiceventmammagate" {
                event_targets.push((2, RetailLevelTransform::ScaleGate));
            } else if lower_type == "mareeventwallrock" {
                for building_index in 1..=7 {
                    event_targets.push((building_index, RetailLevelTransform::WallRock));
                }
            }

            for (building_index, transform) in event_targets {
                let Ok(Some(joint_index)) =
                    file.map_building_joint_index(usize::from(building_index))
                else {
                    continue;
                };
                let (bounds_min, bounds_max) = file
                    .joint_bounds(joint_index)
                    .unwrap_or(([0.0; 3], [0.0; 3]));
                let (translation_offset, scale_multiplier) = match transform {
                    RetailLevelTransform::Sink { event_index } => {
                        let bounds_depth = (bounds_max[1] - bounds_min[1]).abs();
                        let sink_depth = match lower_type.as_str() {
                            "mapeventsinkbianco" if event_index == 0 => 1700.0,
                            "mapeventsinkbianco" => 1500.0,
                            "mapeventsirenasink" => 3500.0,
                            _ if bounds_depth.is_finite() && bounds_depth > 0.0 => bounds_depth,
                            _ => 1000.0,
                        };
                        ([0.0, -sink_depth, 0.0], [1.0; 3])
                    }
                    RetailLevelTransform::ScaleGate => ([0.0, 295.0, 0.0], [1.0, 0.008, 1.0]),
                    RetailLevelTransform::WallRock => {
                        let bounds_depth = (bounds_max[2] - bounds_min[2]).abs();
                        (
                            [
                                0.0,
                                0.0,
                                100.0
                                    + if bounds_depth.is_finite() {
                                        bounds_depth
                                    } else {
                                        0.0
                                    },
                            ],
                            [1.0; 3],
                        )
                    }
                };
                targets.insert(
                    joint_index,
                    LevelTransformTarget {
                        joint_index,
                        translation_offset,
                        scale_multiplier,
                        behavior: LevelTransformBehavior::Linear,
                    },
                );
            }
        }
    }

    targets.into_values().collect()
}

pub(super) fn level_transform_overrides(
    targets: &[LevelTransformTarget],
    progress: f32,
) -> Vec<J3dJointTransformOverride> {
    let progress = progress.clamp(0.0, 1.0);
    let remaining = 1.0 - progress;
    targets
        .iter()
        .map(|target| {
            let (translation_offset, scale_multiplier) = match target.behavior {
                LevelTransformBehavior::Linear => (
                    target.translation_offset.map(|value| value * remaining),
                    target
                        .scale_multiplier
                        .map(|value| value + (1.0 - value) * progress),
                ),
                LevelTransformBehavior::AlwaysHidden | LevelTransformBehavior::HideAfterStart => {
                    ([0.0; 3], [1.0; 3])
                }
            };
            J3dJointTransformOverride {
                joint_index: target.joint_index,
                translation_offset,
                scale_multiplier,
            }
        })
        .collect()
}

pub(super) fn apply_level_transform_visibility(
    file: &J3dFile,
    targets: &[LevelTransformTarget],
    progress: f32,
    triangles: &mut [J3dTriangle],
) {
    let mut hidden_shapes = BTreeSet::new();
    for target in targets
        .iter()
        .filter(|target| level_transform_target_is_hidden(target, progress))
    {
        if let Ok(shapes) = file.joint_subtree_shape_indices(target.joint_index) {
            hidden_shapes.extend(shapes);
        }
    }
    for triangle in triangles
        .iter_mut()
        .filter(|triangle| hidden_shapes.contains(&triangle.shape_index))
    {
        triangle.vertices = [triangle.vertices[0]; 3];
    }
}

pub(super) fn level_transform_target_is_hidden(
    target: &LevelTransformTarget,
    progress: f32,
) -> bool {
    match target.behavior {
        LevelTransformBehavior::Linear => false,
        LevelTransformBehavior::AlwaysHidden => true,
        LevelTransformBehavior::HideAfterStart => progress > f32::EPSILON,
    }
}

pub(super) fn apply_pollution_bitmap_mask(
    document: &StageDocument,
    model_path: &str,
    preview: &mut J3dGeometryPreview,
) {
    if pollution_layer_model_index(model_path).is_none() {
        return;
    }
    let Some(bitmap_path) = model_path
        .rsplit_once('.')
        .map(|(base, _)| format!("{base}.bmp"))
    else {
        return;
    };
    let Some(asset) = document.assets.iter().find(|asset| {
        asset.kind == StageAssetKind::Texture
            && asset
                .path
                .to_string_lossy()
                .replace('\\', "/")
                .eq_ignore_ascii_case(&bitmap_path)
    }) else {
        return;
    };
    let Ok(bytes) = read_stage_asset_bytes(&asset.path) else {
        return;
    };
    let Some((width, height, rgba)) = decode_pollution_bitmap_mask(&bytes) else {
        return;
    };

    replace_pollution_mask_texture_aliases(&mut preview.textures, width, height, &rgba);
}

pub(super) fn replace_pollution_mask_texture_aliases(
    textures: &mut [sms_formats::J3dTexturePreview],
    width: u16,
    height: u16,
    rgba: &[u8],
) {
    let Some(dynamic_texture) = textures.first() else {
        return;
    };
    if dynamic_texture.width != width || dynamic_texture.height != height {
        return;
    }

    // TPollutionLayer writes the live mask through the model's first ResTIMG.
    // Some retail pollution models repeat that same named texture resource for
    // multiple materials. J3D shares the underlying image data, but the preview
    // decoder owns one RGBA buffer per TEX1 entry, so update every alias here.
    let dynamic_texture_name = dynamic_texture.name.clone();
    for texture in textures.iter_mut().filter(|texture| {
        texture.name == dynamic_texture_name && texture.width == width && texture.height == height
    }) {
        texture.rgba.clear();
        texture.rgba.extend_from_slice(rgba);
        texture.mips.clear();
        texture.mipmap_count = 1;
    }
}

pub(super) fn decode_pollution_bitmap_mask(bytes: &[u8]) -> Option<(u16, u16, Vec<u8>)> {
    if bytes.get(..2)? != b"BM" {
        return None;
    }
    let pixel_offset = u32::from_le_bytes(bytes.get(10..14)?.try_into().ok()?) as usize;
    let width = i32::from_le_bytes(bytes.get(18..22)?.try_into().ok()?);
    let height = i32::from_le_bytes(bytes.get(22..26)?.try_into().ok()?);
    let planes = u16::from_le_bytes(bytes.get(26..28)?.try_into().ok()?);
    let bits_per_pixel = u16::from_le_bytes(bytes.get(28..30)?.try_into().ok()?);
    let compression = u32::from_le_bytes(bytes.get(30..34)?.try_into().ok()?);
    if width <= 0 || height == 0 || planes != 1 || bits_per_pixel != 8 || compression != 0 {
        return None;
    }

    let width = usize::try_from(width).ok()?;
    let height_abs = height.unsigned_abs() as usize;
    let row_stride = width.checked_add(3)? & !3;
    let pixel_len = row_stride.checked_mul(height_abs)?;
    let pixels = bytes.get(pixel_offset..pixel_offset.checked_add(pixel_len)?)?;
    let mut rgba = Vec::with_capacity(width.checked_mul(height_abs)?.checked_mul(4)?);
    for y in 0..height_abs {
        let source_y = if height > 0 { height_abs - 1 - y } else { y };
        let row = pixels
            .get(source_y.checked_mul(row_stride)?..)?
            .get(..width)?;
        for &value in row {
            rgba.extend_from_slice(&[value, value, value, value]);
        }
    }

    Some((
        u16::try_from(width).ok()?,
        u16::try_from(height_abs).ok()?,
        rgba,
    ))
}

pub(super) fn push_preview_textures(
    textures: &mut Vec<PreviewTexture>,
    preview: &J3dGeometryPreview,
) -> usize {
    let texture_base = textures.len();
    for texture in &preview.textures {
        let expected_len = texture.width as usize * texture.height as usize * 4;
        if texture.rgba.len() == expected_len && expected_len > 0 {
            let has_alpha = texture.rgba.chunks_exact(4).any(|pixel| pixel[3] < 245);
            let has_translucent_alpha = texture
                .rgba
                .chunks_exact(4)
                .any(|pixel| pixel[3] > 12 && pixel[3] < 245);
            let mut mips = texture
                .mips
                .iter()
                .filter_map(|mip| {
                    let expected_len = mip.width as usize * mip.height as usize * 4;
                    (mip.rgba.len() == expected_len && expected_len > 0).then(|| {
                        egui::ColorImage::from_rgba_unmultiplied(
                            [mip.width as usize, mip.height as usize],
                            &mip.rgba,
                        )
                    })
                })
                .collect::<Vec<_>>();
            if mips.is_empty() {
                mips.push(egui::ColorImage::from_rgba_unmultiplied(
                    [texture.width as usize, texture.height as usize],
                    &texture.rgba,
                ));
            }
            textures.push(PreviewTexture {
                image: mips[0].clone(),
                mips,
                format: texture.format,
                wrap_s: texture.wrap_s,
                wrap_t: texture.wrap_t,
                min_filter: texture.min_filter,
                mag_filter: texture.mag_filter,
                mipmap_count: texture.mipmap_count,
                has_alpha,
                has_translucent_alpha,
            });
        } else {
            let image = egui::ColorImage::filled([1, 1], egui::Color32::WHITE);
            textures.push(PreviewTexture {
                image: image.clone(),
                mips: vec![image],
                format: texture.format,
                wrap_s: texture.wrap_s,
                wrap_t: texture.wrap_t,
                min_filter: texture.min_filter,
                mag_filter: texture.mag_filter,
                mipmap_count: texture.mipmap_count,
                has_alpha: false,
                has_translucent_alpha: false,
            });
        }
    }
    texture_base
}

pub(super) fn push_preview_materials(
    materials: &mut Vec<J3dMaterial>,
    preview: &J3dGeometryPreview,
    texture_base: usize,
) -> usize {
    let material_base = materials.len();
    for material in &preview.materials {
        let mut material = material.clone();
        material.material_index = materials.len();
        for index in material.texture_indices.iter_mut().flatten() {
            *index += texture_base;
        }
        materials.push(material);
    }
    material_base
}

pub(super) fn attach_model_texture_srt_animation(
    document: &StageDocument,
    model_path: &str,
    material_base: usize,
    model_materials: &[J3dMaterial],
    animations: &mut Vec<J3dTextureSrtAnimation>,
    material_bindings: &mut [Vec<PreviewMaterialAnimationBinding>],
) {
    for animation_path in model_texture_srt_animation_paths(model_path) {
        let Some(asset) = document.assets.iter().find(|asset| {
            asset.kind == StageAssetKind::Animation
                && asset
                    .path
                    .to_string_lossy()
                    .replace('\\', "/")
                    .eq_ignore_ascii_case(&animation_path)
        }) else {
            continue;
        };
        let Ok(bytes) = read_stage_asset_bytes(&asset.path) else {
            continue;
        };
        let Ok(animation) = J3dTextureSrtAnimation::parse(&bytes) else {
            continue;
        };

        let animation_index = animations.len();
        let mut matched = false;
        for (binding_index, binding) in animation.bindings.iter().enumerate() {
            let Some(material_index) = model_materials
                .iter()
                .position(|material| material.name == binding.material_name)
            else {
                continue;
            };
            let global_material_index = material_base + material_index;
            let Some(bindings) = material_bindings.get_mut(global_material_index) else {
                continue;
            };
            bindings.push(PreviewMaterialAnimationBinding {
                animation_index,
                binding_index,
            });
            matched = true;
        }
        if matched {
            animations.push(animation);
        }
    }
}

pub(super) fn model_texture_srt_animation_paths(model_path: &str) -> Vec<String> {
    let normalized = model_path.replace('\\', "/");
    let Some((directory, file_name)) = normalized.rsplit_once('/') else {
        return model_texture_srt_animation_path(&normalized)
            .into_iter()
            .collect();
    };
    if file_name.eq_ignore_ascii_case("gene_pakkun_model1.bmd")
        || file_name.eq_ignore_ascii_case("gene_pakkun_model1.bdl")
    {
        // TBiancoGateKeeper::init constructs a TMultiBtk with both manager
        // animations, rather than using the model's basename.
        return ["gene_pakkun_tex0.btk", "gene_pakkun_tex1.btk"]
            .into_iter()
            .map(|name| format!("{directory}/{name}"))
            .collect();
    }
    model_texture_srt_animation_path(&normalized)
        .into_iter()
        .collect()
}

pub(super) fn model_texture_srt_animation_path(model_path: &str) -> Option<String> {
    let extension_offset = model_path.rfind('.')?;
    Some(format!("{}.btk", &model_path[..extension_offset]))
}

pub(super) fn starting_joint_animation(
    document: &StageDocument,
    object: &SceneObject,
    model_path: &str,
) -> Option<J3dJointAnimation> {
    let candidates = starting_joint_animation_candidates(object, model_path);
    let asset = document.assets.iter().find(|asset| {
        asset.kind == StageAssetKind::Animation
            && candidates.iter().any(|candidate| {
                asset
                    .path
                    .to_string_lossy()
                    .replace('\\', "/")
                    .eq_ignore_ascii_case(candidate)
            })
    })?;
    let bytes = read_stage_asset_bytes(&asset.path).ok()?;
    J3dJointAnimation::parse(bytes).ok()
}

pub(super) fn starting_joint_animation_candidates(
    object: &SceneObject,
    model_path: &str,
) -> Vec<String> {
    let normalized = model_path.replace('\\', "/");
    let archive = normalized.split_once("!/").map(|(archive, _)| archive);
    let factory = object.factory_name.to_ascii_lowercase();
    let relative_candidates: &[&str] = if factory.starts_with("npcmontem") {
        &["montemcommon/mom_wait.bck", "montem/mom_wait.bck"]
    } else if factory.starts_with("npcmontew") {
        &["montewcommon/mow_wait.bck", "montew/mow_wait.bck"]
    } else if factory.starts_with("npcmarem") {
        &["marem/marem_wait.bck"]
    } else if factory.starts_with("npcmarew") {
        &["marew/marew_wait.bck"]
    } else if factory == "npckinopio" {
        &["kinopio/kinopio_wait.bck"]
    } else if factory == "npckinojii" {
        &["kinojii/kinoji_wait.bck"]
    } else if factory == "npcpeach" {
        &["peach/peach_wait.bck"]
    } else if factory == "npcraccoondog" {
        &["raccoondog/tanuki_wait_a.bck"]
    } else if factory == "gatekeeper" {
        // TNerveBGKSleep starts BCK index 10, which is wait1 in the manager's
        // alphabetically indexed GateKeeper animation resources.
        &["gatekeeper/gene_pakkun_wait1.bck"]
    } else {
        &[]
    };

    relative_candidates
        .iter()
        .map(|relative| {
            archive.map_or_else(
                || relative.to_string(),
                |archive| format!("{archive}!/{relative}"),
            )
        })
        .collect()
}

#[allow(clippy::too_many_arguments)]
pub(super) fn attach_npc_texture_pattern_animation(
    document: &StageDocument,
    object: &SceneObject,
    model_path: &str,
    material_base: usize,
    texture_base: usize,
    model_materials: &[J3dMaterial],
    materials: &mut [J3dMaterial],
    animations: &mut Vec<PreviewTexturePatternAnimation>,
) {
    let candidates = starting_texture_pattern_candidates(object, model_path);
    let Some(asset) = document.assets.iter().find(|asset| {
        asset.kind == StageAssetKind::Animation
            && candidates.iter().any(|candidate| {
                asset
                    .path
                    .to_string_lossy()
                    .replace('\\', "/")
                    .eq_ignore_ascii_case(candidate)
            })
    }) else {
        return;
    };
    let Ok(bytes) = read_stage_asset_bytes(&asset.path) else {
        return;
    };
    let Ok(animation) = J3dTexturePatternAnimation::parse(bytes) else {
        return;
    };
    let phase_frames = if animation.max_frame == 0 {
        0
    } else {
        stable_string_hash(&object.id) % animation.max_frame as u64
    };
    let phase_seconds = phase_frames as f32 / 60.0;
    let frame = animation.playback_frame(phase_seconds);
    let mut bindings = Vec::new();
    for (animation_binding_index, binding) in animation.bindings.iter().enumerate() {
        let Some(local_material_index) = model_materials
            .iter()
            .position(|material| material.name.eq_ignore_ascii_case(&binding.material_name))
        else {
            continue;
        };
        let texture_slot = binding.texture_slot as usize;
        if texture_slot >= 8 {
            continue;
        }
        let material_index = material_base + local_material_index;
        let current_texture_index = binding
            .texture_index(frame)
            .map(|index| texture_base + index);
        let Some(material) = materials.get_mut(material_index) else {
            continue;
        };
        material.texture_indices[texture_slot] = current_texture_index;
        bindings.push(PreviewTexturePatternBinding {
            material_index,
            texture_slot,
            texture_base,
            animation_binding_index,
            current_texture_index,
        });
    }
    if !bindings.is_empty() {
        animations.push(PreviewTexturePatternAnimation {
            animation,
            phase_seconds,
            bindings,
        });
    }
}

pub(super) fn starting_texture_pattern_candidates(
    object: &SceneObject,
    model_path: &str,
) -> Vec<String> {
    let normalized = model_path.replace('\\', "/");
    let archive = normalized.split_once("!/").map(|(archive, _)| archive);
    let factory = object.factory_name.to_ascii_lowercase();
    let Some(relative) = (match factory.as_str() {
        "npcmontema" | "npcmontemh" => Some("montemcommon/moma_wink.btp"),
        "npcmontemb" => Some("montemcommon/momb_wink.btp"),
        "npcmontemc" | "npcmontemg" => Some("montemcommon/momc_wink.btp"),
        "npcmontemd" => Some("montemcommon/momd_wink.btp"),
        "npcmontem" | "npcmontemf" => Some("montemcommon/mom_wink.btp"),
        "npcmontewa" => Some("montewcommon/mowa_wink.btp"),
        "npcmontewb" => Some("montewcommon/mowb_wink.btp"),
        "npcmontew" | "npcmontewc" => Some("montewcommon/mow_wink.btp"),
        _ => None,
    }) else {
        return Vec::new();
    };
    vec![archive.map_or_else(
        || relative.to_string(),
        |archive| format!("{archive}!/{relative}"),
    )]
}

pub(super) fn stable_string_hash(value: &str) -> u64 {
    value.bytes().fold(0xcbf2_9ce4_8422_2325, |hash, byte| {
        (hash ^ byte as u64).wrapping_mul(0x1000_0000_01b3)
    })
}

pub(super) fn apply_model_material_table(
    document: &StageDocument,
    model_path: &str,
    loader_flags: u32,
    preview: &mut J3dGeometryPreview,
) {
    let Some(table_path) = document
        .assets
        .iter()
        .filter(|asset| asset.kind == StageAssetKind::MaterialTable)
        .filter_map(|asset| {
            let path = asset.path.to_string_lossy();
            material_table_asset_score(model_path, &preview.textures, &path)
                .map(|score| (score, asset.path.clone()))
        })
        .max_by_key(|(score, _)| *score)
        .map(|(_, path)| path)
    else {
        return;
    };
    let Ok(bytes) = read_stage_asset_bytes(&table_path) else {
        return;
    };
    let Ok(table) = J3dFile::parse(&bytes) else {
        return;
    };
    let Ok(materials) = table.material_programs_with_loader_flags(loader_flags) else {
        return;
    };
    let Ok(textures) = table.texture_previews() else {
        return;
    };
    if materials
        .iter()
        .flat_map(|material| material.texture_indices)
        .flatten()
        .any(|index| index >= textures.len())
    {
        return;
    }

    apply_material_table_to_preview(preview, materials, textures);
}

fn apply_material_table_to_preview(
    preview: &mut J3dGeometryPreview,
    materials: Vec<J3dMaterial>,
    textures: Vec<sms_formats::J3dTexturePreview>,
) {
    // J3DModelData::setMaterialTable copies material state by JUT material name;
    // it does not assume that the BMD and BMT material arrays share an order.
    for mut replacement in materials {
        let Some((index, current)) = preview
            .materials
            .iter()
            .enumerate()
            .find(|(_, current)| current.name == replacement.name)
        else {
            continue;
        };
        replacement.name.clone_from(&current.name);
        replacement.material_index = current.material_index;
        replacement.material_id = current.material_id;
        preview.materials[index] = replacement;
    }
    if !textures.is_empty() {
        preview.textures = textures;
    }

    for triangle in &mut preview.triangles {
        let Some(material) = triangle
            .material_index
            .and_then(|index| preview.materials.get(index))
        else {
            continue;
        };
        triangle.texture_index = material
            .texture_indices
            .into_iter()
            .flatten()
            .find(|index| *index < preview.textures.len());
        triangle.cull_mode = Some(material.cull_mode);
        triangle.alpha_compare = Some(material.alpha_compare);
        triangle.blend_mode = Some(material.blend_mode);
        triangle.z_mode = Some(material.z_mode);
        triangle.z_comp_loc = Some(material.z_comp_loc);
    }
}

pub(super) fn apply_actor_runtime_textures(
    document: &StageDocument,
    object: &SceneObject,
    preview: &mut J3dGeometryPreview,
) {
    let factory = object.factory_name.to_ascii_lowercase();
    let replacements = actor_runtime_texture_replacements(&factory);
    if replacements.is_empty() {
        return;
    }

    for (dummy_name, asset_suffix) in replacements {
        let texture_indices = preview
            .textures
            .iter()
            .enumerate()
            .filter_map(|(index, texture)| {
                texture
                    .name
                    .eq_ignore_ascii_case(dummy_name)
                    .then_some(index)
            })
            .collect::<Vec<_>>();
        if texture_indices.is_empty() {
            continue;
        }
        let Some(asset) = document.assets.iter().find(|asset| {
            asset.kind == StageAssetKind::Texture
                && asset
                    .path
                    .to_string_lossy()
                    .replace('\\', "/")
                    .to_ascii_lowercase()
                    .ends_with(asset_suffix)
        }) else {
            continue;
        };
        let Ok(bytes) = read_stage_asset_bytes(&asset.path) else {
            continue;
        };
        let Ok(mut texture) = decode_bti_texture(bytes) else {
            continue;
        };
        texture.name = dummy_name.to_string();
        for texture_index in texture_indices {
            preview.textures[texture_index] = texture.clone();
        }
    }
}

pub(super) fn actor_runtime_texture_replacements(
    factory: &str,
) -> Vec<(&'static str, &'static str)> {
    if factory == "gatekeeper" {
        // TBiancoGateKeeper::init replaces this authored dummy texture with
        // the current stage's pollution texture.
        return vec![("Q_kepper_dummy_128IA4", "/map/pollution/h_ma_rak.bti")];
    }
    if !factory.starts_with("npc") {
        return Vec::new();
    }

    let mut replacements = Vec::new();
    let monte_uses_pollution_texture = matches!(
        factory,
        "npcmontem" | "npcmontema" | "npcmontemc" | "npcmontew" | "npcmontewa"
    );
    if !factory.starts_with("npcmonte") || monte_uses_pollution_texture {
        replacements.push(("H_ma_rak_dummy", "/map/pollution/h_ma_rak.bti"));
    }
    if factory.starts_with("npcmontem") && factory != "npcmonteme" {
        replacements.push(("I_mom_mino_dummyI4", "/montemcommon/i_mom_mino_rgba.bti"));
    } else if factory.starts_with("npcmontew") {
        replacements.push(("I_mow_mino_dummyI4", "/montewcommon/i_mow_mino_rgba.bti"));
    }

    replacements
}

pub(super) fn apply_npc_eye_decal_culling(object: &SceneObject, preview: &mut J3dGeometryPreview) {
    if !object.factory_name.to_ascii_lowercase().starts_with("npc") {
        return;
    }
    let eye_materials = preview
        .materials
        .iter()
        .map(|material| is_npc_eye_material_name(&material.name))
        .collect::<Vec<_>>();
    for (material, is_eye) in preview.materials.iter_mut().zip(&eye_materials) {
        if *is_eye {
            material.cull_mode = 0;
        }
    }
    for triangle in &mut preview.triangles {
        if triangle
            .material_index
            .and_then(|index| eye_materials.get(index))
            .copied()
            .unwrap_or(false)
        {
            triangle.cull_mode = Some(0);
        }
    }
}

pub(super) fn is_npc_eye_material_name(name: &str) -> bool {
    name.to_ascii_lowercase().ends_with("_eye_mat")
}

#[derive(Clone, Copy)]
pub(super) struct NpcAccessorySpec {
    pub(super) joint_name: Option<&'static str>,
    pub(super) asset_suffix: &'static str,
}

pub(super) fn npc_accessory_specs(object: &SceneObject) -> Vec<NpcAccessorySpec> {
    let Some(mask) = object
        .raw_params
        .get("npc_parts_mask")
        .and_then(|value| value.parse::<i32>().ok())
        .map(|value| value as u32)
    else {
        return Vec::new();
    };
    let factory = object.factory_name.to_ascii_lowercase();
    let mut available = Vec::new();
    if factory.starts_with("npcmontem") {
        available.extend_from_slice(&[
            (0, Some("kubi"), "/montemcommon/hata_model.bmd"),
            (1, Some("kubi"), "/montemcommon/higea_model.bmd"),
            (2, Some("kubi"), "/montemcommon/glassesa_model.bmd"),
            (3, Some("kubi"), "/montemcommon/glassesb_model.bmd"),
            (4, Some("kubi"), "/montemcommon/hatb_model.bmd"),
            (5, Some("kubi"), "/montemcommon/hate_model.bmd"),
            (6, Some("kubi"), "/montemcommon/hatd_model.bmd"),
            (7, Some("kubi"), "/montemcommon/hatf_model.bmd"),
            (8, Some("kubi"), "/montemcommon/hatg_model.bmd"),
            (9, Some("body_jnt"), "/montemcommon/eria_model.bmd"),
            (10, Some("body_jnt"), "/montemcommon/tieb_model.bmd"),
        ]);
        available.push(if factory == "npcmontemf" {
            (11, Some("body_jnt"), "/tube_model.bmd")
        } else if factory == "npcmontemg" {
            (11, Some("handR_jnt"), "/mop_model.bmd")
        } else if factory == "npcmontemh" {
            (11, Some("body_jnt"), "/uklele_model.bmd")
        } else {
            (11, Some("body_jnt"), "/montemcommon/nimotsu_model.bmd")
        });
    } else if factory.starts_with("npcmontew") {
        available.extend_from_slice(&[
            (0, Some("yashi_jnt"), "/montewcommon/flower_model.bmd"),
            (1, Some("kubi"), "/montewcommon/hwa_model.bmd"),
            (2, Some("kubi"), "/montewcommon/gwb_model.bmd"),
            (3, Some("handR_jnt"), "/montewcommon/arrowr_model.bmd"),
            (4, Some("handR_jnt"), "/montewcommon/arrowl_model.bmd"),
        ]);
        if factory == "npcmontewc" {
            available.extend_from_slice(&[
                (5, Some("kubi"), "/hwc_model.bmd"),
                (6, Some("handR_jnt"), "/udewar_model.bmd"),
                (7, Some("handL_jnt"), "/udewal_model.bmd"),
            ]);
        }
    } else if factory.starts_with("npcmarem") {
        available.extend_from_slice(&[
            (0, Some("kubi"), "/maremhat_a.bmd"),
            (1, Some("kubi"), "/maremhat_b.bmd"),
            (3, Some("kubi"), "/maremhat_e.bmd"),
            (4, Some("koshi"), "/maremmakigai_a.bmd"),
            (5, Some("koshi"), "/maremmakigai_b.bmd"),
            (6, Some("kubi"), "/marem_grass.bmd"),
            (7, Some("koshi"), "/marembivalve_b.bmd"),
        ]);
        if factory == "npcmaremb" {
            available.extend_from_slice(&[
                (8, Some("kubi"), "/maremb_set.bmd"),
                (9, None, "/marembturizao.bmd"),
            ]);
        } else if factory == "npcmaremc" {
            available.extend_from_slice(&[
                (8, Some("kubi"), "/maremchat_f.bmd"),
                (9, Some("kubi"), "/maremcagohige.bmd"),
                (10, Some("kubi"), "/maremckuchihige.bmd"),
            ]);
        } else if factory == "npcmaremd" {
            available.push((8, Some("migite"), "/maremdhoragai_a.bmd"));
        }
    } else if factory.starts_with("npcmarew") {
        available.extend_from_slice(&[
            (0, Some("kubi"), "/marewpearl_a.bmd"),
            (1, Some("kubi"), "/marewhatw_a.bmd"),
            (4, Some("koshi"), "/marewkai_a.bmd"),
            (5, Some("koshi"), "/marewkai_b.bmd"),
            (8, Some("koshi"), "/marewbivalvew_a.bmd"),
            (9, Some("kubi"), "/marewhatw_d.bmd"),
        ]);
        if factory == "npcmarewb" {
            available.push((10, None, "/marewbbaby.bmd"));
        }
    } else if factory == "npckinopio" {
        available.push((0, Some("kubi"), "/kinopio_sunmegane.bmd"));
    } else if factory == "npckinojii" {
        available.push((0, Some("jnt_rfinger_1"), "/kinoji_stick.bmd"));
    } else if factory == "npcpeach" {
        // TNpcParts::partsPerform hides the parasol and alternate hands in
        // Peach's default state, while her normal or ponytail hair remains
        // attached to the neck joint. Keep the resource-mask bit positions in
        // lockstep with sPeach_InitData in NpcInitData.cpp.
        available.extend_from_slice(&[
            (0, Some("kubi"), "/peach/peach_hair_normal.bmd"),
            (1, Some("jnt_hand_L"), "/peach/peach_hand1_l.bmd"),
            (2, Some("jnt_hand_R"), "/peach/peach_hand1_r.bmd"),
            (3, Some("kubi"), "/peach/peach_hair_ponytail.bmd"),
        ]);
    } else if factory == "npcraccoondog" {
        available.push((0, Some("ukiwa_null"), "/ukiwa.bmd"));
    }
    available
        .into_iter()
        .filter(|(bit, _, _)| mask & (1 << bit) != 0)
        .map(|(_, joint_name, asset_suffix)| NpcAccessorySpec {
            joint_name,
            asset_suffix,
        })
        .collect()
}

pub(super) fn find_stage_asset_by_suffix<'a>(
    document: &'a StageDocument,
    kind: StageAssetKind,
    suffix: &str,
) -> Option<&'a StageAsset> {
    let suffix = suffix.to_ascii_lowercase();
    document.assets.iter().find(|asset| {
        asset.kind == kind
            && asset
                .path
                .to_string_lossy()
                .replace('\\', "/")
                .to_ascii_lowercase()
                .ends_with(&suffix)
    })
}

pub(super) fn accessory_joint_animation(
    document: &StageDocument,
    asset_suffix: &str,
) -> Option<J3dJointAnimation> {
    let animation_suffix = accessory_joint_animation_suffix(asset_suffix)?;
    let asset = find_stage_asset_by_suffix(document, StageAssetKind::Animation, animation_suffix)?;
    let bytes = read_stage_asset_bytes(&asset.path).ok()?;
    J3dJointAnimation::parse(bytes).ok()
}

pub(super) fn accessory_joint_animation_suffix(asset_suffix: &str) -> Option<&'static str> {
    let normalized = asset_suffix.to_ascii_lowercase();
    if normalized.ends_with("/peach/peach_hair_normal.bmd") {
        Some("/peach/peach_hair_normal_wait.bck")
    } else if normalized.ends_with("/peach/peach_hair_ponytail.bmd") {
        Some("/peach/peach_hair_ponytail_wait.bck")
    } else {
        None
    }
}

pub(super) fn push_accessory_instance_materials(
    materials: &mut Vec<J3dMaterial>,
    cached: &CachedAccessoryModelPreview,
    object: &SceneObject,
    asset_suffix: &str,
) -> usize {
    if !monte_accessory_has_instance_colors(asset_suffix) {
        return cached.material_base;
    }
    let source_end = cached.material_base + cached.preview.materials.len();
    let source = materials[cached.material_base..source_end].to_vec();
    let material_base = materials.len();
    for mut material in source {
        material.material_index = materials.len();
        apply_monte_accessory_material_color(&mut material, object, asset_suffix);
        materials.push(material);
    }
    material_base
}

pub(super) fn monte_accessory_has_instance_colors(asset_suffix: &str) -> bool {
    [
        "hata_model.bmd",
        "hatb_model.bmd",
        "hatd_model.bmd",
        "hate_model.bmd",
        "hatf_model.bmd",
        "hatg_model.bmd",
        "higea_model.bmd",
        "glassesb_model.bmd",
        "eria_model.bmd",
        "tieb_model.bmd",
        "flower_model.bmd",
        "hwa_model.bmd",
        "gwb_model.bmd",
    ]
    .iter()
    .any(|name| asset_suffix.ends_with(name))
}

pub(super) fn npc_parts_color_index(object: &SceneObject, channel: usize) -> Option<usize> {
    object
        .raw_params
        .get(&format!("npc_parts_color_index_{channel}"))?
        .parse::<i32>()
        .ok()?
        .try_into()
        .ok()
}
