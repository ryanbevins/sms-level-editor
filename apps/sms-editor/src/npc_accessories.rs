use super::*;

pub(super) fn apply_npc_accessory_material_color(
    material: &mut J3dMaterial,
    object: &SceneObject,
    spec: &NpcAccessorySpec,
) {
    let color_index = npc_parts_color_index(object, usize::from(spec.color_index_channel));
    let material_name = material.name.clone();
    for change in spec
        .color_changes
        .iter()
        .filter(|change| material_name.eq_ignore_ascii_case(&change.material_name))
    {
        let Some(index) = color_index else {
            continue;
        };
        apply_npc_color_change(material, change, index);
    }

    if spec.uses_pollution {
        if let Some(color) = npc_pollution_k_color(object) {
            material.tev_k_colors[0] = color;
        }
    }
}
