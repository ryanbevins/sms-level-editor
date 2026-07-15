use std::collections::BTreeSet;

use encoding_rs::SHIFT_JIS;
use serde::{Deserialize, Serialize};

use crate::binary::{be_f32, be_u16, be_u32};
use crate::{FormatError, Result};

const FORMAT: &str = "JDrama";
const MAX_SCAN_RECORDS: usize = 4096;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct JDramaObjectRecord {
    pub offset: usize,
    pub size: usize,
    pub type_name: String,
    pub object_name: Option<String>,
    pub transform: Option<JDramaTransform>,
    pub stream_strings: Vec<String>,
    #[serde(default)]
    pub obj_chara_folder: Option<String>,
    #[serde(default)]
    pub obj_manager_chara: Option<String>,
    #[serde(default)]
    pub live_actor_manager: Option<String>,
    #[serde(default)]
    pub actor_character: Option<String>,
    #[serde(default)]
    pub mario_modoki_telesa_imitation_index: Option<u32>,
    pub npc_params: Option<JDramaNpcParams>,
    #[serde(default)]
    pub map_obj_grass_blade_count: Option<u32>,
    #[serde(default)]
    pub map_wire_manager: Option<JDramaMapWireManagerParams>,
    pub map_event_sink: Option<JDramaMapEventSinkParams>,
    #[serde(default)]
    pub cube_general_info: Option<JDramaCubeGeneralInfo>,
    pub light: Option<JDramaLight>,
    pub ambient: Option<JDramaAmbient>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JDramaScenarioArchiveEntry {
    pub area_index: u32,
    pub scenario_index: u32,
    pub archive_name: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct JDramaCubeGeneralInfo {
    pub center: [f32; 3],
    pub rotation_degrees: [f32; 3],
    pub dimensions: [f32; 3],
    pub flags: u32,
    pub data_no: i32,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct JDramaMapWireManagerParams {
    pub wire_capacity: u32,
    pub actor_capacity: u32,
    pub draw_width: f32,
    pub draw_height: f32,
    pub upper_surface: [u8; 4],
    pub lower_surface: [u8; 4],
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct JDramaLight {
    pub name: Option<String>,
    pub position: [f32; 3],
    pub color: [u8; 4],
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct JDramaAmbient {
    pub name: Option<String>,
    pub color: [u8; 4],
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JDramaMapEventSinkParams {
    pub buildings: Vec<JDramaMapEventBuilding>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct JDramaMapEventBuilding {
    pub building_index: u16,
    pub pollution_layer_index: u16,
    pub pollution_object_index: u16,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct JDramaNpcParams {
    pub color_indices: [i32; 2],
    pub pollution_amount: i32,
    pub parts_color_indices: [i32; 3],
    pub parts_mask: i32,
    pub movement_type: i32,
    pub action_flags: i32,
    pub motion_min: i32,
    pub motion_max: i32,
    pub coin_flag: i32,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct JDramaTransform {
    pub translation: [f32; 3],
    pub rotation: [f32; 3],
    pub scale: [f32; 3],
}

pub fn parse_jdrama_object_records(bytes: &[u8]) -> Result<Vec<JDramaObjectRecord>> {
    let mut records = Vec::new();
    let mut visited = BTreeSet::new();
    parse_record_at(bytes, 0, bytes.len(), &mut visited, &mut records)?;
    Ok(records)
}

/// Parses the nested NameRef pointer/value arrays loaded by TApplication as
/// the runtime area/scenario archive table.
pub fn parse_jdrama_scenario_archive_entries(
    bytes: &[u8],
) -> Result<Vec<JDramaScenarioArchiveEntry>> {
    let (_, root_payload, root_end) = name_ref_record_layout(bytes, 0, bytes.len())?;
    let root_children = name_ref_array_children(bytes, root_payload, root_end)?;
    for outer_offset in root_children {
        let Ok((_, outer_payload, outer_end)) =
            name_ref_record_layout(bytes, outer_offset, root_end)
        else {
            continue;
        };
        let Ok(area_records) = name_ref_array_children(bytes, outer_payload, outer_end) else {
            continue;
        };
        let mut entries = Vec::new();
        let mut valid = true;
        for (area_index, area_offset) in area_records.into_iter().enumerate() {
            let Ok((_, area_payload, area_end)) =
                name_ref_record_layout(bytes, area_offset, outer_end)
            else {
                valid = false;
                break;
            };
            let Ok(scenario_records) = name_ref_array_children(bytes, area_payload, area_end)
            else {
                valid = false;
                break;
            };
            for (scenario_index, scenario_offset) in scenario_records.into_iter().enumerate() {
                let Ok((_, scenario_payload, scenario_end)) =
                    name_ref_record_layout(bytes, scenario_offset, area_end)
                else {
                    valid = false;
                    break;
                };
                let Ok((archive_name, archive_end)) =
                    read_len_string(bytes, scenario_payload, scenario_end)
                else {
                    valid = false;
                    break;
                };
                if archive_name.is_empty() || archive_end != scenario_end {
                    valid = false;
                    break;
                }
                entries.push(JDramaScenarioArchiveEntry {
                    area_index: area_index as u32,
                    scenario_index: scenario_index as u32,
                    archive_name,
                });
            }
            if !valid {
                break;
            }
        }
        if valid && !entries.is_empty() {
            return Ok(entries);
        }
    }
    Err(FormatError::Unsupported {
        format: FORMAT,
        message: "no nested scenario archive table was found".to_string(),
    })
}

fn name_ref_record_layout(
    bytes: &[u8],
    offset: usize,
    limit: usize,
) -> Result<(usize, usize, usize)> {
    let size =
        plausible_record_size(bytes, offset, limit).ok_or_else(|| invalid_offset(offset, limit))?;
    let end = offset + size;
    let (_, after_type) = read_len_string(bytes, offset + 6, end)?;
    let (_, payload) = read_name_ref(bytes, after_type, end)?;
    Ok((size, payload, end))
}

fn name_ref_array_children(bytes: &[u8], payload: usize, end: usize) -> Result<Vec<usize>> {
    let count = be_u32(bytes, payload, FORMAT)? as usize;
    if count > MAX_SCAN_RECORDS {
        return Err(FormatError::Unsupported {
            format: FORMAT,
            message: format!("NameRef array has implausible child count {count}"),
        });
    }
    let mut cursor = payload
        .checked_add(4)
        .ok_or_else(|| invalid_offset(payload, end))?;
    let mut children = Vec::with_capacity(count);
    for _ in 0..count {
        let size =
            plausible_record_size(bytes, cursor, end).ok_or_else(|| invalid_offset(cursor, end))?;
        children.push(cursor);
        cursor = cursor
            .checked_add(size)
            .ok_or_else(|| invalid_offset(cursor, end))?;
    }
    (cursor == end)
        .then_some(children)
        .ok_or_else(|| invalid_offset(cursor, end))
}

fn parse_record_at(
    bytes: &[u8],
    offset: usize,
    limit: usize,
    visited: &mut BTreeSet<usize>,
    records: &mut Vec<JDramaObjectRecord>,
) -> Result<usize> {
    if records.len() >= MAX_SCAN_RECORDS || !visited.insert(offset) {
        return Ok(plausible_record_size(bytes, offset, limit).unwrap_or(0));
    }

    let size =
        plausible_record_size(bytes, offset, limit).ok_or_else(|| FormatError::Unsupported {
            format: FORMAT,
            message: format!("not a JDrama object record at {offset:#x}"),
        })?;
    let end = offset + size;
    let mut cursor = offset + 4;
    cursor += 2; // type key code
    let (type_name, after_type) = read_len_string(bytes, cursor, end)?;
    let (object_name, after_name) = read_name_ref(bytes, after_type, end)
        .map(|(name, cursor)| (Some(name), cursor))
        .unwrap_or((None, after_type));
    let transform = read_actor_transform(bytes, after_name, end);
    let stream_strings = transform
        .map(|_| scan_ascii_stream_strings(bytes, after_name + 36, end))
        .unwrap_or_default();
    let obj_chara_folder = read_obj_chara_folder(bytes, after_name, end, &type_name);
    let obj_manager_chara = read_obj_manager_chara(bytes, after_name, end, &type_name);
    let live_actor_manager =
        transform.and_then(|_| read_live_actor_manager(bytes, after_name + 36, end));
    let actor_character = transform.and_then(|_| {
        read_len_string(bytes, after_name + 36, end)
            .ok()
            .map(|(value, _)| value)
            .filter(|value| !value.is_empty())
    });
    let mario_modoki_telesa_imitation_index = transform.and_then(|_| {
        read_mario_modoki_telesa_imitation_index(bytes, after_name + 36, end, &type_name)
    });
    let npc_params =
        transform.and_then(|_| read_npc_params(bytes, after_name + 36, end, &type_name));
    let map_obj_grass_blade_count = transform
        .and_then(|_| read_map_obj_grass_blade_count(bytes, after_name + 36, end, &type_name));
    let map_wire_manager = read_map_wire_manager_params(bytes, after_name, end, &type_name);
    let map_event_sink = read_map_event_sink_params(bytes, after_name, end, &type_name);
    let cube_general_info = read_cube_general_info(bytes, after_name, end, &type_name);
    let short_type = type_name.rsplit("::").next().unwrap_or(&type_name);
    let light = (short_type == "Light")
        .then(|| read_light(bytes, after_name, end, object_name.clone()))
        .flatten();
    let ambient = (short_type == "AmbColor")
        .then(|| read_ambient(bytes, after_name, end, object_name.clone()))
        .flatten();

    records.push(JDramaObjectRecord {
        offset,
        size,
        type_name,
        object_name,
        transform,
        stream_strings,
        obj_chara_folder,
        obj_manager_chara,
        live_actor_manager,
        actor_character,
        mario_modoki_telesa_imitation_index,
        npc_params,
        map_obj_grass_blade_count,
        map_wire_manager,
        map_event_sink,
        cube_general_info,
        light,
        ambient,
    });

    let mut scan = after_type;
    while scan + 8 <= end && records.len() < MAX_SCAN_RECORDS {
        if let Some(child_size) = plausible_record_size(bytes, scan, end) {
            parse_record_at(bytes, scan, end, visited, records)?;
            scan += child_size.max(1);
        } else {
            scan += 1;
        }
    }

    Ok(size)
}

fn read_mario_modoki_telesa_imitation_index(
    bytes: &[u8],
    start: usize,
    end: usize,
    type_name: &str,
) -> Option<u32> {
    // TSmallEnemy::load consumes the actor character/light map, manager,
    // graph, and coin ID. TMarioModokiTelesa::load then appends one u32.
    // Follow that stream layout instead of interpreting arbitrary tail bytes.
    if type_name.rsplit("::").next()? != "MarioModokiTelesa" {
        return None;
    }
    let (_, cursor) = read_live_actor_manager_with_cursor(bytes, start, end)?;
    let (_, cursor) = read_len_string(bytes, cursor, end).ok()?; // graph
    let selector = cursor.checked_add(4)?; // coin ID
    if selector.checked_add(4)? != end {
        return None;
    }
    // Values outside the decomp's explicit 1..=12 switch cases are valid and
    // intentionally retain the default imitation model (retail uses 120).
    be_u32(bytes, selector, FORMAT).ok()
}

fn read_obj_chara_folder(
    bytes: &[u8],
    start: usize,
    end: usize,
    type_name: &str,
) -> Option<String> {
    type_name
        .rsplit("::")
        .next()?
        .eq_ignore_ascii_case("ObjChara")
        .then(|| {
            read_len_string(bytes, start, end)
                .ok()
                .map(|(value, _)| value)
        })
        .flatten()
}

fn read_obj_manager_chara(
    bytes: &[u8],
    start: usize,
    end: usize,
    type_name: &str,
) -> Option<String> {
    type_name
        .rsplit("::")
        .next()?
        .contains("Manager")
        .then(|| {
            read_len_string(bytes, start, end)
                .ok()
                .map(|(value, _)| value)
        })
        .flatten()
}

fn read_live_actor_manager(bytes: &[u8], start: usize, end: usize) -> Option<String> {
    read_live_actor_manager_with_cursor(bytes, start, end).map(|(manager, _)| manager)
}

fn read_live_actor_manager_with_cursor(
    bytes: &[u8],
    start: usize,
    end: usize,
) -> Option<(String, usize)> {
    // JDrama::TActor::load reads a TCharacter name followed by a TLightMap.
    // TLiveActor and TSpineEnemy then read the TLiveManager name. Following
    // that exact stream layout avoids guessing a manager from an actor type.
    let (_, mut cursor) = read_len_string(bytes, start, end).ok()?;
    if cursor.checked_add(4)? > end {
        return None;
    }
    let light_count = be_u32(bytes, cursor, FORMAT).ok()? as usize;
    cursor = cursor.checked_add(4)?;
    // Every light-map entry contains a u32 index and at least the u16 length
    // of its name. Bound the loop by the containing record instead of
    // imposing a format limit that JDrama itself does not have.
    if light_count > end.checked_sub(cursor)? / 6 {
        return None;
    }
    for _ in 0..light_count {
        if cursor.checked_add(4)? > end {
            return None;
        }
        cursor = cursor.checked_add(4)?;
        let (_, next) = read_len_string(bytes, cursor, end).ok()?;
        cursor = next;
    }
    let (manager, cursor) = read_len_string(bytes, cursor, end).ok()?;
    (!manager.is_empty()).then_some((manager, cursor))
}

fn read_cube_general_info(
    bytes: &[u8],
    start: usize,
    end: usize,
    type_name: &str,
) -> Option<JDramaCubeGeneralInfo> {
    if !type_name
        .rsplit("::")
        .next()?
        .eq_ignore_ascii_case("CubeGeneralInfo")
    {
        return None;
    }
    if start.checked_add(48)? > end {
        return None;
    }

    let dimensions = read_vec3(bytes, start + 24)?.map(|value| value * 100.0);
    Some(JDramaCubeGeneralInfo {
        center: read_vec3(bytes, start)?,
        rotation_degrees: read_vec3(bytes, start + 12)?,
        dimensions,
        flags: be_u32(bytes, start + 36, FORMAT).ok()?,
        data_no: be_u32(bytes, start + 44, FORMAT).ok()? as i32,
    })
}

fn read_map_wire_manager_params(
    bytes: &[u8],
    start: usize,
    end: usize,
    type_name: &str,
) -> Option<JDramaMapWireManagerParams> {
    if !type_name
        .rsplit("::")
        .next()?
        .eq_ignore_ascii_case("MapWireManager")
    {
        return None;
    }

    // TMapWireManager::load reads a character name followed by capacities,
    // draw dimensions, and six u32 color channels. This is deliberately kept
    // here with the JDrama stream parser instead of inferred by the renderer.
    let (_, cursor) = read_len_string(bytes, start, end).ok()?;
    if cursor.checked_add(40)? > end {
        return None;
    }
    let color = |offset: usize| u8::try_from(be_u32(bytes, offset, FORMAT).ok()?).ok();
    Some(JDramaMapWireManagerParams {
        wire_capacity: be_u32(bytes, cursor, FORMAT).ok()?,
        actor_capacity: be_u32(bytes, cursor + 4, FORMAT).ok()?,
        draw_width: be_f32(bytes, cursor + 8, FORMAT).ok()?,
        draw_height: be_f32(bytes, cursor + 12, FORMAT).ok()?,
        upper_surface: [
            color(cursor + 16)?,
            color(cursor + 20)?,
            color(cursor + 24)?,
            0xff,
        ],
        lower_surface: [
            color(cursor + 28)?,
            color(cursor + 32)?,
            color(cursor + 36)?,
            0xff,
        ],
    })
}

fn read_map_obj_grass_blade_count(
    bytes: &[u8],
    start: usize,
    end: usize,
    type_name: &str,
) -> Option<u32> {
    if !type_name
        .rsplit("::")
        .next()?
        .eq_ignore_ascii_case("MapObjGrassGroup")
    {
        return None;
    }

    // TMapObjGrassGroup::load first delegates to JDrama::TActor::load. After
    // the transform that base loader consumes a character name followed by a
    // TLightMap (count, then index/name pairs). The grass blade count is the
    // next big-endian u32 in the placement stream.
    let (_, mut cursor) = read_len_string(bytes, start, end).ok()?;
    let light_count = be_u32(bytes, cursor, FORMAT).ok()? as usize;
    cursor = cursor.checked_add(4)?;
    if light_count > end.checked_sub(cursor)? / 6 {
        return None;
    }
    for _ in 0..light_count {
        cursor = cursor.checked_add(4)?;
        let (_, next) = read_len_string(bytes, cursor, end).ok()?;
        cursor = next;
    }

    let count = be_u32(bytes, cursor, FORMAT).ok()?;
    (count <= 200_000).then_some(count)
}

fn read_light(bytes: &[u8], start: usize, end: usize, name: Option<String>) -> Option<JDramaLight> {
    if start.checked_add(16)? > end {
        return None;
    }
    Some(JDramaLight {
        name,
        position: [
            be_f32(bytes, start, FORMAT).ok()?,
            be_f32(bytes, start + 4, FORMAT).ok()?,
            be_f32(bytes, start + 8, FORMAT).ok()?,
        ],
        color: bytes[start + 12..start + 16].try_into().ok()?,
    })
}

fn read_ambient(
    bytes: &[u8],
    start: usize,
    end: usize,
    name: Option<String>,
) -> Option<JDramaAmbient> {
    if start.checked_add(4)? > end {
        return None;
    }
    Some(JDramaAmbient {
        name,
        color: bytes[start..start + 4].try_into().ok()?,
    })
}

fn read_map_event_sink_params(
    bytes: &[u8],
    start: usize,
    end: usize,
    type_name: &str,
) -> Option<JDramaMapEventSinkParams> {
    let lower_type = type_name.to_ascii_lowercase();
    if !lower_type.contains("event") || !lower_type.contains("sink") {
        return None;
    }

    let building_count = be_u32(bytes, start, FORMAT).ok()? as usize;
    let first_building = be_u32(bytes, start.checked_add(4)?, FORMAT).ok()? as usize;
    if building_count == 0 || building_count > 64 || first_building > u16::MAX as usize {
        return None;
    }
    let entries_end = start.checked_add(8 + building_count.checked_mul(8)?)?;
    if entries_end > end {
        return None;
    }

    let buildings = (0..building_count)
        .map(|index| {
            let entry = start + 8 + index * 8;
            Some(JDramaMapEventBuilding {
                building_index: u16::try_from(first_building + index).ok()?,
                pollution_layer_index: u16::try_from(be_u32(bytes, entry, FORMAT).ok()?).ok()?,
                pollution_object_index: u16::try_from(be_u32(bytes, entry + 4, FORMAT).ok()?)
                    .ok()?,
            })
        })
        .collect::<Option<Vec<_>>>()?;
    Some(JDramaMapEventSinkParams { buildings })
}

fn read_npc_params(
    bytes: &[u8],
    start: usize,
    end: usize,
    type_name: &str,
) -> Option<JDramaNpcParams> {
    if !type_name.to_ascii_lowercase().starts_with("npc") {
        return None;
    }
    let (_, mut cursor) = read_len_string(bytes, start, end).ok()?;
    let light_count = be_u32(bytes, cursor, FORMAT).ok()? as usize;
    cursor = cursor.checked_add(4)?;
    if light_count > end.checked_sub(cursor)? / 6 {
        return None;
    }
    for _ in 0..light_count {
        cursor = cursor.checked_add(4)?;
        let (_, next) = read_len_string(bytes, cursor, end).ok()?;
        cursor = next;
    }
    let (_, cursor) = read_len_string(bytes, cursor, end).ok()?;
    let (_, cursor) = read_len_string(bytes, cursor, end).ok()?;
    if cursor.checked_add(48)? > end {
        return None;
    }
    let values: [i32; 12] = std::array::from_fn(|index| {
        be_u32(bytes, cursor + index * 4, FORMAT).unwrap_or_default() as i32
    });
    Some(JDramaNpcParams {
        color_indices: [values[0], values[1]],
        pollution_amount: values[2],
        parts_color_indices: [values[3], values[4], values[5]],
        parts_mask: values[6],
        movement_type: values[7],
        action_flags: values[8],
        motion_min: values[9],
        motion_max: values[10],
        coin_flag: values[11],
    })
}

fn scan_ascii_stream_strings(bytes: &[u8], start: usize, end: usize) -> Vec<String> {
    let mut strings = Vec::new();
    let mut cursor = start;
    while cursor + 2 <= end {
        let Ok(length) = be_u16(bytes, cursor, FORMAT) else {
            break;
        };
        let length = length as usize;
        let value_start = cursor + 2;
        let Some(value_end) = value_start.checked_add(length) else {
            break;
        };
        let valid = (3..=80).contains(&length)
            && value_end <= end
            && bytes[value_start..value_end]
                .iter()
                .all(|byte| byte.is_ascii_graphic() || *byte == b' ')
            && bytes[value_start..value_end]
                .iter()
                .any(|byte| byte.is_ascii_alphabetic());
        if valid {
            strings.push(String::from_utf8_lossy(&bytes[value_start..value_end]).into_owned());
            cursor = value_end;
        } else {
            cursor += 1;
        }
    }
    strings
}

fn plausible_record_size(bytes: &[u8], offset: usize, limit: usize) -> Option<usize> {
    if offset + 8 > bytes.len() || offset + 8 > limit {
        return None;
    }
    let size = be_u32(bytes, offset, FORMAT).ok()? as usize;
    if size < 10 || offset.checked_add(size)? > bytes.len() || offset + size > limit {
        return None;
    }

    let string_len = be_u16(bytes, offset + 6, FORMAT).ok()? as usize;
    if string_len == 0 || string_len > 80 || offset + 8 + string_len > offset + size {
        return None;
    }
    let type_bytes = bytes.get(offset + 8..offset + 8 + string_len)?;
    if !type_bytes
        .iter()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(*byte, b'_' | b':' | b'<' | b'>'))
    {
        return None;
    }

    Some(size)
}

fn read_name_ref(bytes: &[u8], offset: usize, limit: usize) -> Result<(String, usize)> {
    if offset + 4 > limit {
        return Err(invalid_offset(offset, limit));
    }
    let mut cursor = offset + 2; // key code
    read_len_string(bytes, cursor, limit).map(|(name, next)| {
        cursor = next;
        (name, cursor)
    })
}

fn read_len_string(bytes: &[u8], offset: usize, limit: usize) -> Result<(String, usize)> {
    if offset + 2 > limit {
        return Err(invalid_offset(offset, limit));
    }
    let len = be_u16(bytes, offset, FORMAT)? as usize;
    let start = offset + 2;
    let end = start
        .checked_add(len)
        .ok_or_else(|| invalid_offset(offset, limit))?;
    if end > limit || end > bytes.len() {
        return Err(invalid_offset(end, limit));
    }
    // Retail JDrama names use the GameCube-era Japanese code page. ASCII is a
    // subset of Shift-JIS, so this also preserves the common English names.
    let (value, _) = SHIFT_JIS.decode_without_bom_handling(&bytes[start..end]);
    let value = value.into_owned();
    Ok((value, end))
}

fn read_actor_transform(bytes: &[u8], offset: usize, limit: usize) -> Option<JDramaTransform> {
    if offset + 36 > limit {
        return None;
    }
    let translation = read_vec3(bytes, offset)?;
    let rotation = read_vec3(bytes, offset + 12)?;
    let scale = read_vec3(bytes, offset + 24)?;
    let transform = JDramaTransform {
        translation,
        rotation,
        scale,
    };
    is_plausible_actor_transform(transform).then_some(transform)
}

fn read_vec3(bytes: &[u8], offset: usize) -> Option<[f32; 3]> {
    Some([
        be_f32(bytes, offset, FORMAT).ok()?,
        be_f32(bytes, offset + 4, FORMAT).ok()?,
        be_f32(bytes, offset + 8, FORMAT).ok()?,
    ])
}

fn is_plausible_actor_transform(transform: JDramaTransform) -> bool {
    let values = transform
        .translation
        .into_iter()
        .chain(transform.rotation)
        .chain(transform.scale);
    if !values.clone().all(|value| value.is_finite()) {
        return false;
    }
    if !transform
        .translation
        .iter()
        .all(|value| value.abs() <= 1_000_000.0)
    {
        return false;
    }
    if !transform
        .rotation
        .iter()
        .all(|value| value.abs() <= 100_000.0)
    {
        return false;
    }
    transform.scale.iter().all(|value| value.abs() <= 1_000.0)
        && transform.scale.iter().any(|value| value.abs() > 0.0001)
}

fn invalid_offset(offset: usize, len: usize) -> FormatError {
    FormatError::InvalidOffset {
        format: FORMAT,
        offset,
        len,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn name_ref_record(type_name: &str, name: &str, payload: &[u8]) -> Vec<u8> {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&0u32.to_be_bytes());
        bytes.extend_from_slice(&0u16.to_be_bytes());
        put_len_string(&mut bytes, type_name.as_bytes());
        bytes.extend_from_slice(&0u16.to_be_bytes());
        put_len_string(&mut bytes, name.as_bytes());
        bytes.extend_from_slice(payload);
        let size = bytes.len() as u32;
        bytes[..4].copy_from_slice(&size.to_be_bytes());
        bytes
    }

    fn name_ref_array(type_name: &str, name: &str, children: &[Vec<u8>]) -> Vec<u8> {
        let mut payload = Vec::new();
        payload.extend_from_slice(&(children.len() as u32).to_be_bytes());
        for child in children {
            payload.extend_from_slice(child);
        }
        name_ref_record(type_name, name, &payload)
    }

    #[test]
    fn parses_nested_runtime_area_and_scenario_archive_indices() {
        let leaf = |name: &str, archive: &str| {
            let mut payload = Vec::new();
            put_len_string(&mut payload, archive.as_bytes());
            name_ref_record("Leaf", name, &payload)
        };
        let areas = vec![
            name_ref_array(
                "ValueArray",
                "area 0",
                &[
                    leaf("scenario 0", "first0.arc"),
                    leaf("scenario 1", "first1.arc"),
                ],
            ),
            name_ref_array("ValueArray", "area 1", &[leaf("scenario 0", "second0.arc")]),
        ];
        let outer = name_ref_array("PointerArray", "runtime stages", &areas);
        let root = name_ref_array("Root", "root", &[outer]);

        let entries = parse_jdrama_scenario_archive_entries(&root).expect("parse stage table");

        assert_eq!(entries.len(), 3);
        assert_eq!(entries[0].area_index, 0);
        assert_eq!(entries[1].scenario_index, 1);
        assert_eq!(entries[2].area_index, 1);
        assert_eq!(entries[2].archive_name, "second0.arc");
    }

    #[test]
    fn scans_ascii_stream_strings_after_binary_fields() {
        let bytes = [
            0xFF, 0x00, 0x00, 0x09, b'N', b'o', b'z', b'z', b'l', b'e', b'B', b'o', b'x', 0x00,
            0x12, b'r', b'o', b'c', b'k', b'e', b't', b'_', b'n', b'o', b'z', b'z', b'l', b'e',
            b'_', b'i', b't', b'e', b'm', 0x80,
        ];

        assert_eq!(
            scan_ascii_stream_strings(&bytes, 0, bytes.len()),
            ["NozzleBox", "rocket_nozzle_item"]
        );
    }

    #[test]
    fn reads_external_jdrama_light_and_ambient_state() {
        let mut light_bytes = Vec::new();
        for value in [200_000.0f32, 500_000.0, 200_000.0] {
            light_bytes.extend_from_slice(&value.to_be_bytes());
        }
        light_bytes.extend_from_slice(&[210, 150, 230, 255]);
        let light = read_light(
            &light_bytes,
            0,
            light_bytes.len(),
            Some("object sun".to_string()),
        )
        .unwrap();
        assert_eq!(light.position, [200_000.0, 500_000.0, 200_000.0]);
        assert_eq!(light.color, [210, 150, 230, 255]);

        let ambient = read_ambient(
            &[95, 80, 115, 255],
            0,
            4,
            Some("object ambient".to_string()),
        )
        .unwrap();
        assert_eq!(ambient.color, [95, 80, 115, 255]);
    }

    #[test]
    fn reads_npc_color_indices_and_parts_mask_after_actor_fields() {
        let mut bytes = Vec::new();
        put_len_string(&mut bytes, b"");
        bytes.extend_from_slice(&0_u32.to_be_bytes());
        put_len_string(&mut bytes, b"manager");
        put_len_string(&mut bytes, b"graph");
        for value in [9_i32, 3, 0, 1, 255, 2, 264, 0, 100, -1, 0, -1] {
            bytes.extend_from_slice(&value.to_be_bytes());
        }

        let params = read_npc_params(&bytes, 0, bytes.len(), "NPCMonteMA").unwrap();
        assert_eq!(params.color_indices, [9, 3]);
        assert_eq!(params.pollution_amount, 0);
        assert_eq!(params.parts_color_indices, [1, 255, 2]);
        assert_eq!(params.parts_mask, 264);
        assert_eq!(params.action_flags, 100);
    }

    #[test]
    fn reads_map_obj_grass_count_after_actor_light_map() {
        let mut bytes = Vec::new();
        put_len_string(&mut bytes, b"grass character");
        bytes.extend_from_slice(&1_u32.to_be_bytes());
        bytes.extend_from_slice(&3_u32.to_be_bytes());
        put_len_string(&mut bytes, b"object light");
        bytes.extend_from_slice(&21_000_u32.to_be_bytes());

        assert_eq!(
            read_map_obj_grass_blade_count(&bytes, 0, bytes.len(), "JDrama::MapObjGrassGroup"),
            Some(21_000)
        );
        assert_eq!(
            read_map_obj_grass_blade_count(&bytes, 0, bytes.len(), "MapObjGrassManager"),
            None
        );
    }

    #[test]
    fn reads_map_wire_manager_draw_settings() {
        let mut bytes = Vec::new();
        put_len_string(&mut bytes, b"wire character");
        bytes.extend_from_slice(&200_u32.to_be_bytes());
        bytes.extend_from_slice(&10_u32.to_be_bytes());
        bytes.extend_from_slice(&10.0_f32.to_be_bytes());
        bytes.extend_from_slice(&20.0_f32.to_be_bytes());
        for channel in [200_u32, 201, 202, 128, 129, 130] {
            bytes.extend_from_slice(&channel.to_be_bytes());
        }

        assert_eq!(
            read_map_wire_manager_params(&bytes, 0, bytes.len(), "MapWireManager"),
            Some(JDramaMapWireManagerParams {
                wire_capacity: 200,
                actor_capacity: 10,
                draw_width: 10.0,
                draw_height: 20.0,
                upper_surface: [200, 201, 202, 255],
                lower_surface: [128, 129, 130, 255],
            })
        );
        assert!(read_map_wire_manager_params(&bytes, 0, bytes.len(), "MapObjManager").is_none());
    }

    #[test]
    fn reads_obj_chara_folder_and_manager_reference_from_their_load_streams() {
        let mut chara_bytes = Vec::new();
        put_len_string(&mut chara_bytes, b"/scene/hamukuri");
        assert_eq!(
            read_obj_chara_folder(&chara_bytes, 0, chara_bytes.len(), "ObjChara").as_deref(),
            Some("/scene/hamukuri")
        );
        assert!(
            read_obj_chara_folder(&chara_bytes, 0, chara_bytes.len(), "HamuKuriManager").is_none()
        );

        let mut manager_bytes = Vec::new();
        put_len_string(&mut manager_bytes, b"HamuKuriChara");
        manager_bytes.extend_from_slice(&32_u32.to_be_bytes());
        assert_eq!(
            read_obj_manager_chara(&manager_bytes, 0, manager_bytes.len(), "HamuKuriManager")
                .as_deref(),
            Some("HamuKuriChara")
        );
        assert_eq!(
            read_obj_manager_chara(&manager_bytes, 0, manager_bytes.len(), "FruitsBoatManagerB")
                .as_deref(),
            Some("HamuKuriChara")
        );
        assert!(
            read_obj_manager_chara(&manager_bytes, 0, manager_bytes.len(), "HamuKuri").is_none()
        );
    }

    #[test]
    fn reads_live_actor_manager_after_character_and_light_map() {
        let mut bytes = Vec::new();
        put_len_string(&mut bytes, b"Enemy Character");
        bytes.extend_from_slice(&2_u32.to_be_bytes());
        bytes.extend_from_slice(&7_u32.to_be_bytes());
        put_len_string(&mut bytes, b"Object Light");
        bytes.extend_from_slice(&11_u32.to_be_bytes());
        put_len_string(&mut bytes, b"Object Shadow Light");
        put_len_string(&mut bytes, b"HamuKuri Manager");

        assert_eq!(
            read_live_actor_manager(&bytes, 0, bytes.len()).as_deref(),
            Some("HamuKuri Manager")
        );
    }

    #[test]
    fn reads_live_actor_manager_with_record_bounded_large_light_map() {
        let mut bytes = Vec::new();
        put_len_string(&mut bytes, b"Enemy Character");
        bytes.extend_from_slice(&65_u32.to_be_bytes());
        for index in 0..65_u32 {
            bytes.extend_from_slice(&index.to_be_bytes());
            put_len_string(&mut bytes, b"Object Light");
        }
        put_len_string(&mut bytes, b"HamuKuri Manager");

        assert_eq!(
            read_live_actor_manager(&bytes, 0, bytes.len()).as_deref(),
            Some("HamuKuri Manager")
        );
    }

    #[test]
    fn reads_mario_modoki_telesa_imitation_index_from_subclass_tail() {
        let mut bytes = Vec::new();
        put_len_string(&mut bytes, b"Telesa Character");
        bytes.extend_from_slice(&0_u32.to_be_bytes());
        put_len_string(&mut bytes, b"Telesa Manager");
        put_len_string(&mut bytes, b"(null)");
        bytes.extend_from_slice(&u32::MAX.to_be_bytes());
        bytes.extend_from_slice(&120_u32.to_be_bytes());

        assert_eq!(
            read_mario_modoki_telesa_imitation_index(&bytes, 0, bytes.len(), "MarioModokiTelesa"),
            Some(120)
        );
        assert_eq!(
            read_mario_modoki_telesa_imitation_index(&bytes, 0, bytes.len(), "Telesa"),
            None
        );
    }

    #[test]
    fn reads_cube_general_info_using_runtime_scale_and_model_slot() {
        let mut bytes = Vec::new();
        for value in [10.0_f32, 20.0, 30.0, 0.0, 45.0, 0.0, 2.0, 3.0, 4.0] {
            bytes.extend_from_slice(&value.to_be_bytes());
        }
        bytes.extend_from_slice(&0x80_u32.to_be_bytes());
        bytes.extend_from_slice(&2_u32.to_be_bytes());
        bytes.extend_from_slice(&3_u32.to_be_bytes());

        assert_eq!(
            read_cube_general_info(&bytes, 0, bytes.len(), "JDrama::CubeGeneralInfo"),
            Some(JDramaCubeGeneralInfo {
                center: [10.0, 20.0, 30.0],
                rotation_degrees: [0.0, 45.0, 0.0],
                dimensions: [200.0, 300.0, 400.0],
                flags: 0x80,
                data_no: 3,
            })
        );
        assert!(read_cube_general_info(&bytes, 0, bytes.len(), "MapObjBase").is_none());
    }

    #[test]
    fn reads_map_event_sink_building_range() {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&2_u32.to_be_bytes());
        bytes.extend_from_slice(&1_u32.to_be_bytes());
        bytes.extend_from_slice(&0_u32.to_be_bytes());
        bytes.extend_from_slice(&1_u32.to_be_bytes());
        bytes.extend_from_slice(&1_u32.to_be_bytes());
        bytes.extend_from_slice(&1_u32.to_be_bytes());

        let params =
            read_map_event_sink_params(&bytes, 0, bytes.len(), "MapEventSinkBianco").unwrap();
        assert_eq!(params.buildings.len(), 2);
        assert_eq!(params.buildings[0].building_index, 1);
        assert_eq!(params.buildings[0].pollution_layer_index, 0);
        assert_eq!(params.buildings[0].pollution_object_index, 1);
        assert_eq!(params.buildings[1].building_index, 2);
        assert_eq!(params.buildings[1].pollution_layer_index, 1);
        assert_eq!(params.buildings[1].pollution_object_index, 1);
        assert!(read_map_event_sink_params(&bytes, 0, bytes.len(), "MapEventSirenaSink").is_some());
        assert!(read_map_event_sink_params(&bytes, 0, bytes.len(), "MapObjBase").is_none());
    }

    fn put_len_string(bytes: &mut Vec<u8>, value: &[u8]) {
        bytes.extend_from_slice(&(value.len() as u16).to_be_bytes());
        bytes.extend_from_slice(value);
    }

    #[test]
    fn decodes_shift_jis_length_prefixed_string() {
        let bytes = [
            0x00, 0x10, 0x83, 0x6f, 0x83, 0x8b, 0x81, 0x5b, 0x83, 0x93, 0x83, 0x77, 0x83, 0x8b,
            0x83, 0x76, b'v', b'1',
        ];

        let (value, next) = read_len_string(&bytes, 0, bytes.len()).unwrap();

        assert_eq!(value, "バルーンヘルプv1");
        assert_eq!(next, bytes.len());
    }

    #[test]
    #[ignore = "requires an extracted retail base root"]
    fn parses_retail_obj_chara_resource_folders() {
        let base_root = std::env::var_os("SMS_BASE_ROOT")
            .map(std::path::PathBuf::from)
            .expect("set SMS_BASE_ROOT to the extracted game's root");
        let bytes = std::fs::read(base_root.join("files/data/scenecmn.bin"))
            .expect("read retail scenecmn.bin");
        let records = parse_jdrama_object_records(&bytes).expect("parse retail scenecmn.bin");
        let folders = records
            .iter()
            .filter_map(|record| record.obj_chara_folder.as_deref())
            .collect::<Vec<_>>();
        assert!(
            folders.len() > 40,
            "unexpected ObjChara coverage: {folders:?}"
        );
        assert!(folders.contains(&"/scene/hamukuri"));
        assert!(folders.contains(&"/scene/bgeso"));
        assert!(folders.contains(&"/scene/bosseel"));
    }
}
