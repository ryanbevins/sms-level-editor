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
    pub npc_params: Option<JDramaNpcParams>,
    pub map_event_sink: Option<JDramaMapEventSinkParams>,
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
    let npc_params =
        transform.and_then(|_| read_npc_params(bytes, after_name + 36, end, &type_name));
    let map_event_sink = read_map_event_sink_params(bytes, after_name, end, &type_name);

    records.push(JDramaObjectRecord {
        offset,
        size,
        type_name,
        object_name,
        transform,
        stream_strings,
        npc_params,
        map_event_sink,
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
}
