use super::*;

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
    let Some(texture) = preview.textures.first_mut() else {
        return;
    };
    let Ok(bytes) = read_stage_asset_bytes(&asset.path) else {
        return;
    };
    let Some((width, height, rgba)) = decode_pollution_bitmap_mask(&bytes) else {
        return;
    };
    if texture.width != width || texture.height != height {
        return;
    }

    texture.rgba = rgba;
    texture.mips.clear();
    texture.mipmap_count = 1;
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
    let Some(animation_path) = model_texture_srt_animation_path(model_path) else {
        return;
    };
    let Some(asset) = document.assets.iter().find(|asset| {
        asset.kind == StageAssetKind::Animation
            && asset
                .path
                .to_string_lossy()
                .replace('\\', "/")
                .eq_ignore_ascii_case(&animation_path)
    }) else {
        return;
    };
    let Ok(bytes) = read_stage_asset_bytes(&asset.path) else {
        return;
    };
    let Ok(animation) = J3dTextureSrtAnimation::parse(&bytes) else {
        return;
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
    let candidates = material_table_candidates_for_model(model_path);
    let Some(table_path) = document.assets.iter().find_map(|asset| {
        if asset.kind != StageAssetKind::MaterialTable {
            return None;
        }
        let normalized = asset
            .path
            .to_string_lossy()
            .replace('\\', "/")
            .to_ascii_lowercase();
        candidates
            .iter()
            .any(|candidate| candidate == &normalized)
            .then_some(asset.path.clone())
    }) else {
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
    let required_materials = preview
        .triangles
        .iter()
        .filter_map(|triangle| triangle.material_index)
        .max()
        .map(|index| index + 1)
        .unwrap_or(0);
    if materials.len() < required_materials
        || materials
            .iter()
            .flat_map(|material| material.texture_indices)
            .flatten()
            .any(|index| index >= textures.len())
    {
        return;
    }

    preview.materials = materials;
    preview.textures = textures;
    for triangle in &mut preview.triangles {
        let Some(material) = triangle
            .material_index
            .and_then(|index| preview.materials.get(index))
        else {
            continue;
        };
        triangle.texture_index = material.texture_indices.into_iter().flatten().next();
        triangle.cull_mode = Some(material.cull_mode);
        triangle.alpha_compare = Some(material.alpha_compare);
        triangle.blend_mode = Some(material.blend_mode);
        triangle.z_mode = Some(material.z_mode);
        triangle.z_comp_loc = Some(material.z_comp_loc);
    }
}

pub(super) fn apply_npc_runtime_textures(
    document: &StageDocument,
    object: &SceneObject,
    preview: &mut J3dGeometryPreview,
) {
    if !object.factory_name.to_ascii_lowercase().starts_with("npc") {
        return;
    }
    let factory = object.factory_name.to_ascii_lowercase();
    let mut replacements = Vec::new();
    let monte_uses_pollution_texture = matches!(
        factory.as_str(),
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
pub(super) struct MonteAccessorySpec {
    pub(super) joint_name: &'static str,
    pub(super) asset_suffix: &'static str,
}

pub(super) fn monte_accessory_specs(object: &SceneObject) -> Vec<MonteAccessorySpec> {
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
            (0, "kubi", "/montemcommon/hata_model.bmd"),
            (1, "kubi", "/montemcommon/higea_model.bmd"),
            (2, "kubi", "/montemcommon/glassesa_model.bmd"),
            (3, "kubi", "/montemcommon/glassesb_model.bmd"),
            (4, "kubi", "/montemcommon/hatb_model.bmd"),
            (5, "kubi", "/montemcommon/hate_model.bmd"),
            (6, "kubi", "/montemcommon/hatd_model.bmd"),
            (7, "kubi", "/montemcommon/hatf_model.bmd"),
            (8, "kubi", "/montemcommon/hatg_model.bmd"),
            (9, "body_jnt", "/montemcommon/eria_model.bmd"),
            (10, "body_jnt", "/montemcommon/tieb_model.bmd"),
        ]);
        available.push(if factory == "npcmontemf" {
            (11, "body_jnt", "/tube_model.bmd")
        } else if factory == "npcmontemg" {
            (11, "handR_jnt", "/mop_model.bmd")
        } else if factory == "npcmontemh" {
            (11, "body_jnt", "/uklele_model.bmd")
        } else {
            (11, "body_jnt", "/montemcommon/nimotsu_model.bmd")
        });
    } else if factory.starts_with("npcmontew") {
        available.extend_from_slice(&[
            (0, "yashi_jnt", "/montewcommon/flower_model.bmd"),
            (1, "kubi", "/montewcommon/hwa_model.bmd"),
            (2, "kubi", "/montewcommon/gwb_model.bmd"),
            (3, "handR_jnt", "/montewcommon/arrowr_model.bmd"),
            (4, "handR_jnt", "/montewcommon/arrowl_model.bmd"),
        ]);
        if factory == "npcmontewc" {
            available.extend_from_slice(&[
                (5, "kubi", "/hwc_model.bmd"),
                (6, "handR_jnt", "/udewar_model.bmd"),
                (7, "handL_jnt", "/udewal_model.bmd"),
            ]);
        }
    }
    available
        .into_iter()
        .filter(|(bit, _, _)| mask & (1 << bit) != 0)
        .map(|(_, joint_name, asset_suffix)| MonteAccessorySpec {
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
