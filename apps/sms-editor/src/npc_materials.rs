use super::*;

pub(super) fn push_object_preview_materials(
    materials: &mut Vec<J3dMaterial>,
    cached: &CachedObjectModelPreview,
    object: &SceneObject,
    registry: Option<&ObjectRegistry>,
) -> usize {
    let string_tev_color = map_obj_string_tev_color(object, registry);
    let has_npc_colors = registry.is_some_and(|registry| {
        registry
            .npc_material_colors_for(&object.factory_name)
            .any(|definition| definition.model_index == 0)
    });
    let pollution_k_color = npc_initial_pollution_k_color(object, registry);
    let has_enemy_colors = registry.is_some_and(|registry| {
        registry
            .enemy_material_colors
            .iter()
            .any(|definition| definition.factory_name == object.factory_name)
    });
    let map_obj_tev_color = map_obj_model_override_tev_color(object, registry);
    if string_tev_color.is_none()
        && !has_npc_colors
        && pollution_k_color.is_none()
        && !has_enemy_colors
        && map_obj_tev_color.is_none()
    {
        return cached.material_base;
    }
    let source_end = cached.material_base + cached.preview.materials.len();
    let source_materials = materials[cached.material_base..source_end].to_vec();
    let material_base = materials.len();
    for mut material in source_materials {
        material.material_index = materials.len();
        if let Some(color) = string_tev_color {
            if let Some(target) = material.tev_colors.get_mut(usize::from(color.register)) {
                *target = color.color;
            }
        }
        if let Some(color) = map_obj_tev_color {
            if let Some(target) = material.tev_colors.get_mut(usize::from(color.register)) {
                *target = color.color;
            }
        }
        if !material.name.eq_ignore_ascii_case("_eye_mat") {
            if let Some(color) = pollution_k_color {
                material.tev_k_colors[0] = color;
            }
        }
        apply_npc_root_material_colors(&mut material, object, registry);
        apply_enemy_material_colors(&mut material, object, registry);
        materials.push(material);
    }
    material_base
}

pub(super) fn map_obj_model_override_tev_color(
    object: &SceneObject,
    registry: Option<&ObjectRegistry>,
) -> Option<sms_schema::MapObjTevColorDefinition> {
    let registry = registry?;
    let resource_name = object.raw_param("actor_tail_string")?;
    registry
        .find_map_obj_model_override(&object.factory_name, resource_name)?
        .tev_color
}

fn apply_npc_root_material_colors(
    material: &mut J3dMaterial,
    object: &SceneObject,
    registry: Option<&ObjectRegistry>,
) {
    let Some(registry) = registry else {
        return;
    };
    let material_name = material.name.clone();
    for definition in registry
        .npc_material_colors_for(&object.factory_name)
        .filter(|definition| definition.model_index == 0)
        .filter(|definition| material_name.eq_ignore_ascii_case(&definition.change.material_name))
    {
        let Some(color_index) =
            npc_root_color_index(object, usize::from(definition.color_index_channel))
        else {
            continue;
        };
        apply_npc_color_change(material, &definition.change, color_index);
    }
}

pub(super) fn apply_npc_color_change(
    material: &mut J3dMaterial,
    change: &sms_schema::NpcColorChangeDefinition,
    color_index: usize,
) {
    match change.mode {
        0 => {
            if let Some(color) = change.colors0.get(color_index) {
                material.material_colors[0] = color.map(|value| value as u8);
            }
        }
        1 => {
            if let Some(color) = change.colors0.get(color_index) {
                material.tev_colors[0] = *color;
            }
        }
        2 => {
            if let Some(color) = change.colors0.get(color_index) {
                material.tev_colors[1] = *color;
            }
            if let Some(color) = change.colors1.get(color_index) {
                material.tev_colors[2] = *color;
            }
        }
        _ => {}
    }
}

pub(super) fn npc_root_color_index(object: &SceneObject, channel: usize) -> Option<usize> {
    let parameter = match channel {
        0 => "npc_body_color_index",
        1 => "npc_cloth_color_index",
        _ => return None,
    };
    object
        .raw_params
        .get(parameter)?
        .parse::<i32>()
        .ok()?
        .try_into()
        .ok()
}

fn npc_initial_pollution_k_color(
    object: &SceneObject,
    registry: Option<&ObjectRegistry>,
) -> Option<[u8; 4]> {
    registry?
        .npc_material_colors_for(&object.factory_name)
        .next()?;
    npc_pollution_k_color(object)
}

pub(super) fn npc_pollution_k_color(object: &SceneObject) -> Option<[u8; 4]> {
    let amount = object
        .raw_params
        .get("npc_pollution_amount")?
        .parse::<i32>()
        .ok()?
        .clamp(0, 255) as u8;
    Some([255, 255, 255, amount])
}

pub(super) fn apply_enemy_material_colors(
    material: &mut J3dMaterial,
    object: &SceneObject,
    registry: Option<&ObjectRegistry>,
) {
    apply_enemy_tev_overrides(
        &mut material.tev_colors,
        &material.name,
        &object.factory_name,
        registry,
    );
}

pub(super) fn apply_enemy_tev_overrides(
    tev_colors: &mut [[i16; 4]; 4],
    material_name: &str,
    factory_name: &str,
    registry: Option<&ObjectRegistry>,
) {
    let Some(registry) = registry else {
        return;
    };
    for definition in registry.enemy_material_colors.iter().filter(|definition| {
        definition.factory_name == factory_name
            && definition.material_name.eq_ignore_ascii_case(material_name)
    }) {
        let Some(target) = tev_colors.get_mut(usize::from(definition.tev_register)) else {
            continue;
        };
        for (target, source) in target.iter_mut().zip(definition.color) {
            if let Some(source) = source {
                *target = source;
            }
        }
    }
}

pub(super) fn map_obj_string_tev_color(
    object: &SceneObject,
    registry: Option<&ObjectRegistry>,
) -> Option<sms_schema::MapObjTevColorDefinition> {
    let registry = registry?;
    let resource_name = object.raw_param("actor_tail_string")?;
    let program = registry.find_map_obj_string_tev_program(&object.factory_name, resource_name)?;
    // Compatibility for overlays authored before the typed selector existed.
    let selector = object
        .raw_param("nozzle_box_item")
        .or_else(|| object.raw_param("stream_string_1"));
    let color = selector
        .and_then(|selector| {
            program
                .variants
                .iter()
                .find(|variant| variant.selector_value == selector)
                .map(|variant| variant.color)
        })
        .unwrap_or(program.default_color);
    Some(sms_schema::MapObjTevColorDefinition {
        register: program.tev_register,
        color,
    })
}
