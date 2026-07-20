use std::collections::BTreeSet;

use encoding_rs::SHIFT_JIS;
use serde::{Deserialize, Serialize};

use crate::binary::{be_f32, be_u16, be_u32};
use crate::{FormatError, Result};

const FORMAT: &str = "JDrama";
const MAX_SCAN_RECORDS: usize = 4096;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct JDramaObjectRecord {
    /// Semantic child-index path from the root record. Unlike `offset`, this
    /// remains stable when the stream is rebuilt from typed records.
    #[serde(default)]
    pub record_path: Vec<usize>,
    pub offset: usize,
    pub size: usize,
    pub type_name: String,
    pub object_name: Option<String>,
    pub transform: Option<JDramaTransform>,
    pub stream_strings: Vec<String>,
    #[serde(default)]
    pub obj_chara_folder: Option<String>,
    #[serde(default)]
    pub smpl_chara_archive_path: Option<String>,
    #[serde(default)]
    pub obj_manager_chara: Option<String>,
    #[serde(default)]
    pub live_actor_manager: Option<String>,
    /// First length-prefixed string after the common TActor character and
    /// light-map fields. Its meaning belongs to the decomp-derived class
    /// schema; the format parser intentionally does not guess a subclass.
    #[serde(default)]
    pub actor_tail_string: Option<String>,
    /// Item selector appended by `TNozzleBox::load` after the common
    /// `TMapObjBase` resource name (for example `rocket_nozzle_item`).
    #[serde(default)]
    pub nozzle_box_item: Option<String>,
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
    /// Exact bytes after the record's TNameRef fields. Unknown class data is
    /// intentionally kept opaque instead of being byte-scanned for metadata.
    #[serde(default)]
    pub raw_payload: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JDramaScenarioArchiveEntry {
    pub area_index: u32,
    pub scenario_index: u32,
    pub archive_name: String,
}

/// One outer runtime area from `stageArc.bin`, including reserved empty
/// carriers whose scenario zero points at `none.arc`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JDramaScenarioArchiveArea {
    pub area_index: u32,
    pub name: String,
    pub archive_names: Vec<String>,
}

/// Result of authoring one runtime stage slot into `stageArc.bin`.
///
/// Sunshine stores its runtime stage registry as a nested JDrama tree. The
/// executable has several fixed 61-element area tables, so authored slots use
/// scenarios in decomp-verified reserved carrier areas and never grow the
/// outer area array.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JDramaScenarioArchiveWriteOutcome {
    pub bytes: Vec<u8>,
    pub entry: JDramaScenarioArchiveEntry,
    /// `false` only when [`ensure_jdrama_scenario_archive_slot`] found the
    /// exact, unique mapping already present and returned the input unchanged.
    pub inserted: bool,
}

/// Runtime areas reserved by retail as a single `none.arc` scenario.
///
/// `shineStageTable` in the neighboring decomp covers only areas 0 through 60;
/// these four carriers are within that bound, map to a valid shine stage, are
/// outside the extra-stage range, and have no area-specific setup behavior.
pub const SMS_AUTHORED_RUNTIME_CARRIER_AREAS: [u8; 4] = [17, 18, 53, 54];

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

/// Applies the signed NPC parts-mask normalization used by
/// `TBaseNPC::setIndividualDifference_` before it constructs `TNpcParts`.
/// Retail placements use negative values as a no-parts sentinel.
pub fn effective_npc_parts_mask(parts_mask: i32) -> u32 {
    parts_mask.max(0) as u32
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct JDramaTransform {
    pub translation: [f32; 3],
    pub rotation: [f32; 3],
    pub scale: [f32; 3],
}

#[derive(Debug, Clone, PartialEq)]
struct ActorStreamLayout {
    transform: JDramaTransform,
    character_name: String,
    light_names: Vec<String>,
    tail_offset: usize,
}

/// A source-free, recursively editable JDrama stream.
///
/// Unlike [`JDramaObjectRecord`], this representation never retains the input
/// record or an opaque payload. A record is admitted only when its load stream
/// can be represented by the typed variants below. Unsupported subclass tails
/// therefore fail at import instead of silently becoming byte passthroughs.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct JDramaDocument {
    pub root: JDramaRecord,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct JDramaRecord {
    pub type_name: String,
    pub name: String,
    pub payload: JDramaRecordPayload,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum JDramaRecordPayload {
    Empty,
    Fields {
        fields: Vec<JDramaField>,
    },
    Actor {
        transform: JDramaTransform,
        character_name: String,
        light_map: JDramaLightMap,
        fields: Vec<JDramaField>,
    },
    Group {
        /// Base-class fields before the child count, such as `IdxGroup`'s
        /// group index or `MarScene`'s light map.
        fields: Vec<JDramaField>,
        children: Vec<JDramaRecord>,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct JDramaField {
    /// Decomp-facing field name. Names are descriptive metadata; field order
    /// is the on-disc stream order.
    pub name: String,
    pub value: JDramaFieldValue,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", content = "value", rename_all = "snake_case")]
pub enum JDramaFieldValue {
    U32(u32),
    I32(i32),
    F32(f32),
    Vec2F32([f32; 2]),
    Vec3F32([f32; 3]),
    ColorRgba8([u8; 4]),
    String(String),
    LightMap(JDramaLightMap),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct JDramaLightMap {
    pub entries: Vec<JDramaLightMapEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JDramaLightMapEntry {
    pub channel: u32,
    pub light_name: String,
}

impl JDramaDocument {
    pub fn parse(bytes: &[u8]) -> Result<Self> {
        parse_jdrama_document(bytes)
    }

    /// Rebuilds the complete stream from semantic fields. Record sizes and
    /// string lengths are regenerated; no source buffer is consulted.
    pub fn to_bytes(&self) -> Result<Vec<u8>> {
        encode_jdrama_document(self)
    }
}

impl JDramaRecord {
    pub fn new(
        type_name: impl Into<String>,
        name: impl Into<String>,
        payload: JDramaRecordPayload,
    ) -> Result<Self> {
        let type_name = type_name.into();
        let name = name.into();
        jdrama_key_code(&type_name)?;
        jdrama_key_code(&name)?;
        Ok(Self {
            type_name,
            name,
            payload,
        })
    }
}

/// Implements `JDrama::TNameRef::calcKeyCode` over the Shift-JIS stream bytes.
pub fn jdrama_key_code(value: &str) -> Result<u16> {
    let encoded = encode_shift_jis(value)?;
    let key = encoded.iter().fold(0_u32, |key, byte| {
        key.wrapping_mul(3).wrapping_add(*byte as u32)
    });
    Ok(key as u16)
}

/// Parses a JDrama record tree without retaining source bytes. Unknown
/// payloads are rejected with their type and offset.
pub fn parse_jdrama_document(bytes: &[u8]) -> Result<JDramaDocument> {
    if bytes.is_empty() {
        return Err(FormatError::TooSmall {
            format: FORMAT,
            expected: 10,
            actual: 0,
        });
    }
    let mut record_count = 0;
    let (root, size) = parse_strict_record(bytes, 0, bytes.len(), &mut record_count)?;
    if size != bytes.len() {
        return Err(FormatError::Unsupported {
            format: FORMAT,
            message: format!(
                "root record covers {size:#x} bytes but the stream contains {:#x}",
                bytes.len()
            ),
        });
    }
    Ok(JDramaDocument { root })
}

pub fn encode_jdrama_document(document: &JDramaDocument) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    write_strict_record(&mut bytes, &document.root)?;
    Ok(bytes)
}

fn field(name: &str, value: JDramaFieldValue) -> JDramaField {
    JDramaField {
        name: name.to_string(),
        value,
    }
}

struct StrictCursor<'a> {
    bytes: &'a [u8],
    offset: usize,
    end: usize,
}

impl<'a> StrictCursor<'a> {
    fn new(bytes: &'a [u8], offset: usize, end: usize) -> Self {
        Self { bytes, offset, end }
    }

    fn remaining(&self) -> usize {
        self.end.saturating_sub(self.offset)
    }

    fn is_done(&self) -> bool {
        self.offset == self.end
    }

    fn require(&self, count: usize) -> Result<()> {
        if count <= self.remaining() {
            Ok(())
        } else {
            Err(invalid_offset(self.offset.saturating_add(count), self.end))
        }
    }

    fn u32(&mut self) -> Result<u32> {
        self.require(4)?;
        let value = be_u32(self.bytes, self.offset, FORMAT)?;
        self.offset += 4;
        Ok(value)
    }

    fn i32(&mut self) -> Result<i32> {
        Ok(self.u32()? as i32)
    }

    fn f32(&mut self) -> Result<f32> {
        self.require(4)?;
        let value = be_f32(self.bytes, self.offset, FORMAT)?;
        self.offset += 4;
        Ok(value)
    }

    fn vec2_f32(&mut self) -> Result<[f32; 2]> {
        Ok([self.f32()?, self.f32()?])
    }

    fn vec3_f32(&mut self) -> Result<[f32; 3]> {
        Ok([self.f32()?, self.f32()?, self.f32()?])
    }

    fn color_rgba8(&mut self) -> Result<[u8; 4]> {
        self.require(4)?;
        let value = self.bytes[self.offset..self.offset + 4]
            .try_into()
            .expect("four bytes were bounds checked");
        self.offset += 4;
        Ok(value)
    }

    fn string(&mut self) -> Result<String> {
        let (value, next) = read_len_string_strict(self.bytes, self.offset, self.end)?;
        self.offset = next;
        Ok(value)
    }

    fn light_map(&mut self) -> Result<JDramaLightMap> {
        let count = self.u32()? as usize;
        if count > MAX_SCAN_RECORDS || count > self.remaining() / 6 {
            return Err(FormatError::ResourceLimit {
                format: FORMAT,
                resource: "light-map entries",
                requested: count,
                limit: (self.remaining() / 6).min(MAX_SCAN_RECORDS),
            });
        }
        let mut entries = Vec::with_capacity(count);
        for _ in 0..count {
            entries.push(JDramaLightMapEntry {
                channel: self.u32()?,
                light_name: self.string()?,
            });
        }
        Ok(JDramaLightMap { entries })
    }
}

fn parse_strict_record(
    bytes: &[u8],
    offset: usize,
    limit: usize,
    record_count: &mut usize,
) -> Result<(JDramaRecord, usize)> {
    *record_count = record_count.saturating_add(1);
    if *record_count > MAX_SCAN_RECORDS {
        return Err(FormatError::ResourceLimit {
            format: FORMAT,
            resource: "record count",
            requested: *record_count,
            limit: MAX_SCAN_RECORDS,
        });
    }

    let size =
        plausible_record_size(bytes, offset, limit).ok_or_else(|| FormatError::Unsupported {
            format: FORMAT,
            message: format!("not a JDrama object record at {offset:#x}"),
        })?;
    let end = offset + size;
    let stored_type_key_code = be_u16(bytes, offset + 4, FORMAT)?;
    let (type_name, after_type) = read_len_string_strict(bytes, offset + 6, end)?;
    let expected_type_key_code = jdrama_key_code(&type_name)?;
    if stored_type_key_code != expected_type_key_code {
        return Err(FormatError::Unsupported {
            format: FORMAT,
            message: format!(
                "type key code {stored_type_key_code:#06x} for {type_name} at {offset:#x} does not match derived key {expected_type_key_code:#06x}"
            ),
        });
    }
    if after_type + 2 > end {
        return Err(invalid_offset(after_type + 2, end));
    }
    let stored_name_key_code = be_u16(bytes, after_type, FORMAT)?;
    let (name, payload_offset) = read_len_string_strict(bytes, after_type + 2, end)?;
    let expected_name_key_code = jdrama_key_code(&name)?;
    if stored_name_key_code != expected_name_key_code {
        return Err(FormatError::Unsupported {
            format: FORMAT,
            message: format!(
                "name key code {stored_name_key_code:#06x} for {name} at {offset:#x} does not match derived key {expected_name_key_code:#06x}"
            ),
        });
    }
    let payload =
        parse_strict_payload(bytes, payload_offset, end, offset, &type_name, record_count)
            .map_err(|error| FormatError::Unsupported {
                format: FORMAT,
                message: format!("while parsing {type_name} at {offset:#x}: {error}"),
            })?;
    Ok((
        JDramaRecord {
            type_name,
            name,
            payload,
        },
        size,
    ))
}

fn parse_strict_payload(
    bytes: &[u8],
    payload_offset: usize,
    end: usize,
    record_offset: usize,
    type_name: &str,
    record_count: &mut usize,
) -> Result<JDramaRecordPayload> {
    let short_type = semantic_type_name(type_name);
    if is_plain_group_type(short_type) {
        return parse_strict_group(bytes, payload_offset, end, Vec::new(), record_count);
    }
    if short_type == "IdxGroup" {
        let mut cursor = StrictCursor::new(bytes, payload_offset, end);
        let fields = vec![field("group_index", JDramaFieldValue::U32(cursor.u32()?))];
        return parse_strict_group(bytes, cursor.offset, end, fields, record_count);
    }
    if matches!(short_type, "SmJ3DScn" | "MarScene") {
        let mut cursor = StrictCursor::new(bytes, payload_offset, end);
        let light_map = cursor.light_map()?;
        let fields = vec![field("light_map", JDramaFieldValue::LightMap(light_map))];
        return parse_strict_group(bytes, cursor.offset, end, fields, record_count);
    }

    let mut cursor = StrictCursor::new(bytes, payload_offset, end);
    let fixed_fields = match short_type {
        "Light" | "IdxLight" => Some(parse_light_fields(&mut cursor)?),
        "AmbColor" => Some(vec![field(
            "color",
            JDramaFieldValue::ColorRgba8(cursor.color_rgba8()?),
        )]),
        "ObjChara" => Some(vec![field(
            "resource_folder",
            JDramaFieldValue::String(cursor.string()?),
        )]),
        "SmplChara" => Some(vec![field(
            "archive_path",
            JDramaFieldValue::String(cursor.string()?),
        )]),
        "ScenarioArchiveName" => Some(vec![field(
            "archive_name",
            JDramaFieldValue::String(cursor.string()?),
        )]),
        "CubeGeneralInfo" => {
            let mut fields = parse_cube_general_fields(&mut cursor)?;
            if !cursor.is_done() {
                fields.push(field(
                    "manager_group_name",
                    JDramaFieldValue::String(cursor.string()?),
                ));
            }
            Some(fields)
        }
        "CameraCubeInfo" => {
            let mut fields = parse_cube_general_fields(&mut cursor)?;
            fields.push(field(
                "camera_map_tool_name",
                JDramaFieldValue::String(cursor.string()?),
            ));
            Some(fields)
        }
        "CubeStreamInfo" => {
            let mut fields = parse_cube_general_fields(&mut cursor)?;
            fields.push(field("stream_value", JDramaFieldValue::I32(cursor.i32()?)));
            fields.push(field("stream_rate", JDramaFieldValue::F32(cursor.f32()?)));
            Some(fields)
        }
        "CameraMapTool" | "CameraMapInfo" => Some(vec![
            field("position", JDramaFieldValue::Vec3F32(cursor.vec3_f32()?)),
            field("pitch_yaw", JDramaFieldValue::Vec2F32(cursor.vec2_f32()?)),
            field("flags", JDramaFieldValue::I32(cursor.i32()?)),
            field("camera_mode", JDramaFieldValue::I32(cursor.i32()?)),
            field("camera_parameter", JDramaFieldValue::I32(cursor.i32()?)),
            field("demo_length_frames", JDramaFieldValue::I32(cursor.i32()?)),
        ]),
        "StagePositionInfo" => Some(vec![
            field("position", JDramaFieldValue::Vec3F32(cursor.vec3_f32()?)),
            field("rotation", JDramaFieldValue::Vec3F32(cursor.vec3_f32()?)),
            field("scale", JDramaFieldValue::Vec3F32(cursor.vec3_f32()?)),
        ]),
        "StageEventInfo" => {
            let mut fields = vec![field("event_id", JDramaFieldValue::U32(cursor.u32()?))];
            for name in [
                "event_name",
                "event_script",
                "event_object",
                "event_camera",
                "event_bgm",
                "event_message",
            ] {
                fields.push(field(name, JDramaFieldValue::String(cursor.string()?)));
            }
            if cursor.remaining() == 4 {
                fields.push(field(
                    "authoring_parameter",
                    JDramaFieldValue::U32(cursor.u32()?),
                ));
            }
            Some(fields)
        }
        "StageEnemyInfo" => Some(vec![
            field("manager_name", JDramaFieldValue::String(cursor.string()?)),
            field("flags", JDramaFieldValue::I32(cursor.i32()?)),
            field("weight", JDramaFieldValue::U32(cursor.u32()?)),
        ]),
        "DrawBufObj" => Some(vec![
            field("draw_buffer_flags", JDramaFieldValue::U32(cursor.u32()?)),
            field("draw_buffer_size", JDramaFieldValue::U32(cursor.u32()?)),
        ]),
        "Viewport" => Some(vec![
            field("left", JDramaFieldValue::I32(cursor.i32()?)),
            field("top", JDramaFieldValue::I32(cursor.i32()?)),
            field("right", JDramaFieldValue::I32(cursor.i32()?)),
            field("bottom", JDramaFieldValue::I32(cursor.i32()?)),
        ]),
        "MapWireManager" => Some(parse_map_wire_manager_fields(&mut cursor)?),
        "SunMgr" => Some(vec![
            field("sun_color_r", JDramaFieldValue::U32(cursor.u32()?)),
            field("sun_color_g", JDramaFieldValue::U32(cursor.u32()?)),
            field("sun_color_b", JDramaFieldValue::U32(cursor.u32()?)),
            field("sun_color_a", JDramaFieldValue::U32(cursor.u32()?)),
            field("sun_size", JDramaFieldValue::F32(cursor.f32()?)),
        ]),
        "MapObjSoundGroup" => Some(vec![field(
            "graph_name",
            JDramaFieldValue::String(cursor.string()?),
        )]),
        "MapObjWave" => Some(vec![field(
            "authoring_character_name",
            JDramaFieldValue::String(cursor.string()?),
        )]),
        "AreaCylinder" => Some(parse_area_cylinder_fields(&mut cursor)?),
        "Generator" => Some(parse_generator_fields(&mut cursor)?),
        "Map" => Some(parse_map_fields(&mut cursor)?),
        value if is_map_event_sink_type(value) => {
            Some(parse_map_event_sink_fields(&mut cursor, value)?)
        }
        "DolpicEventRiccoGate" | "DolpicEventMammaGate" => {
            Some(parse_dolpic_gate_event_fields(&mut cursor)?)
        }
        "MirrorModelManager" => Some(vec![
            field("opaque_model_count", JDramaFieldValue::I32(cursor.i32()?)),
            field(
                "translucent_model_count",
                JDramaFieldValue::I32(cursor.i32()?),
            ),
            field("paired_model_count", JDramaFieldValue::I32(cursor.i32()?)),
        ]),
        "MarioPositionObj" => Some(parse_mario_position_fields(&mut cursor)?),
        _ => None,
    };
    if let Some(fields) = fixed_fields {
        ensure_strict_payload_end(&cursor, type_name, record_offset)?;
        return Ok(JDramaRecordPayload::Fields { fields });
    }

    if cursor.is_done() {
        return Ok(JDramaRecordPayload::Empty);
    }

    // Most `*Manager` factories inherit TObjManager. Test that exact base
    // stream before the actor shape because arbitrary manager values can look
    // like finite transforms by coincidence.
    if short_type.contains("Manager") || short_type == "MareJellyFish" {
        let mut manager_cursor = StrictCursor::new(bytes, payload_offset, end);
        if let Ok(fields) = parse_obj_manager_fields(&mut manager_cursor, short_type) {
            if manager_cursor.is_done() {
                return Ok(JDramaRecordPayload::Fields { fields });
            }
        }
    }

    if let Some((transform, character_name, light_map, mut actor_cursor)) =
        parse_actor_prefix(bytes, payload_offset, end)
    {
        let fields = parse_actor_tail(&mut actor_cursor, short_type)?;
        ensure_strict_payload_end(&actor_cursor, type_name, record_offset)?;
        return Ok(JDramaRecordPayload::Actor {
            transform,
            character_name,
            light_map,
            fields,
        });
    }

    Err(unsupported_strict_payload(
        type_name,
        record_offset,
        cursor.remaining(),
    ))
}

fn is_plain_group_type(short_type: &str) -> bool {
    matches!(
        short_type,
        "GroupObj"
            | "NameRefGrp"
            | "Strategy"
            | "LightAry"
            | "AmbAry"
            | "EventTable"
            | "CameraMapToolTable"
            | "CubeGeneralInfoTable"
            | "StreamGeneralInfoTable"
            | "ScenarioArchiveNameTable"
            | "ScenarioArchiveNamesInStage"
            | "PositionHolder"
            | "StageEnemyInfoHeader"
    )
}

fn parse_strict_group(
    bytes: &[u8],
    count_offset: usize,
    end: usize,
    fields: Vec<JDramaField>,
    record_count: &mut usize,
) -> Result<JDramaRecordPayload> {
    let mut cursor = StrictCursor::new(bytes, count_offset, end);
    let count = cursor.u32()? as usize;
    if count > MAX_SCAN_RECORDS {
        return Err(FormatError::ResourceLimit {
            format: FORMAT,
            resource: "group children",
            requested: count,
            limit: MAX_SCAN_RECORDS,
        });
    }
    let mut children = Vec::with_capacity(count);
    for _ in 0..count {
        let (child, size) = parse_strict_record(bytes, cursor.offset, end, record_count)?;
        cursor.offset = cursor
            .offset
            .checked_add(size)
            .ok_or_else(|| invalid_offset(cursor.offset, end))?;
        children.push(child);
    }
    if !cursor.is_done() {
        return Err(invalid_offset(cursor.offset, end));
    }
    Ok(JDramaRecordPayload::Group { fields, children })
}

fn parse_light_fields(cursor: &mut StrictCursor<'_>) -> Result<Vec<JDramaField>> {
    let mut fields = vec![
        field("position", JDramaFieldValue::Vec3F32(cursor.vec3_f32()?)),
        field("color", JDramaFieldValue::ColorRgba8(cursor.color_rgba8()?)),
    ];
    // Retail LightAry entries append the serialized light range even though
    // the current decomp's TLight::load body only names the position/color
    // portion. Keeping it typed avoids treating the extra word as padding.
    if cursor.remaining() == 4 {
        fields.push(field("range", JDramaFieldValue::F32(cursor.f32()?)));
    }
    Ok(fields)
}

fn parse_mario_position_fields(cursor: &mut StrictCursor<'_>) -> Result<Vec<JDramaField>> {
    let mut fields = Vec::new();
    let mut index = 0_usize;
    while !cursor.is_done() {
        if index == 8 {
            return Err(FormatError::ResourceLimit {
                format: FORMAT,
                resource: "Mario start positions",
                requested: index + 1,
                limit: 8,
            });
        }
        fields.push(field(
            &format!("position_{index}_label"),
            JDramaFieldValue::String(cursor.string()?),
        ));
        fields.push(field(
            &format!("position_{index}_translation"),
            JDramaFieldValue::Vec3F32(cursor.vec3_f32()?),
        ));
        fields.push(field(
            &format!("position_{index}_rotation"),
            JDramaFieldValue::Vec3F32(cursor.vec3_f32()?),
        ));
        fields.push(field(
            &format!("position_{index}_authoring_vector"),
            JDramaFieldValue::Vec3F32(cursor.vec3_f32()?),
        ));
        index += 1;
    }
    Ok(fields)
}

fn parse_cube_general_fields(cursor: &mut StrictCursor<'_>) -> Result<Vec<JDramaField>> {
    Ok(vec![
        field("center", JDramaFieldValue::Vec3F32(cursor.vec3_f32()?)),
        field(
            "rotation_degrees",
            JDramaFieldValue::Vec3F32(cursor.vec3_f32()?),
        ),
        field(
            "dimensions_scale",
            JDramaFieldValue::Vec3F32(cursor.vec3_f32()?),
        ),
        field("flags", JDramaFieldValue::U32(cursor.u32()?)),
        field("reserved", JDramaFieldValue::U32(cursor.u32()?)),
        field("data_no", JDramaFieldValue::I32(cursor.i32()?)),
    ])
}

fn parse_map_wire_manager_fields(cursor: &mut StrictCursor<'_>) -> Result<Vec<JDramaField>> {
    let mut fields = vec![
        field("character_name", JDramaFieldValue::String(cursor.string()?)),
        field("wire_capacity", JDramaFieldValue::U32(cursor.u32()?)),
        field("actor_capacity", JDramaFieldValue::U32(cursor.u32()?)),
    ];

    // The option scene uses the base manager stream only. Stage scenes append
    // the wire renderer's dimensions and as many authoring color components as
    // that retail record contains.
    if cursor.remaining() >= 8 {
        fields.push(field("draw_width", JDramaFieldValue::F32(cursor.f32()?)));
        fields.push(field("draw_height", JDramaFieldValue::F32(cursor.f32()?)));
    }
    for name in [
        "upper_red",
        "upper_green",
        "upper_blue",
        "lower_red",
        "lower_green",
        "lower_blue",
    ] {
        if cursor.is_done() {
            break;
        }
        fields.push(field(name, JDramaFieldValue::U32(cursor.u32()?)));
    }
    Ok(fields)
}

fn parse_area_cylinder_fields(cursor: &mut StrictCursor<'_>) -> Result<Vec<JDramaField>> {
    let mut fields = vec![
        field("center", JDramaFieldValue::Vec3F32(cursor.vec3_f32()?)),
        field(
            "authoring_vector",
            JDramaFieldValue::Vec3F32(cursor.vec3_f32()?),
        ),
        field(
            "cylinder_parameters",
            JDramaFieldValue::Vec3F32(cursor.vec3_f32()?),
        ),
        field(
            "authoring_character_name",
            JDramaFieldValue::String(cursor.string()?),
        ),
    ];
    parse_indexed_name_entries(cursor, &mut fields)?;
    fields.push(field(
        "manager_group_name",
        JDramaFieldValue::String(cursor.string()?),
    ));
    fields.push(field(
        "raw_angle_hundredths",
        JDramaFieldValue::I32(cursor.i32()?),
    ));
    Ok(fields)
}

fn parse_generator_fields(cursor: &mut StrictCursor<'_>) -> Result<Vec<JDramaField>> {
    let mut fields = vec![
        field("position", JDramaFieldValue::Vec3F32(cursor.vec3_f32()?)),
        field("rotation", JDramaFieldValue::Vec3F32(cursor.vec3_f32()?)),
        field(
            "authoring_vector",
            JDramaFieldValue::Vec3F32(cursor.vec3_f32()?),
        ),
        field(
            "authoring_character_name",
            JDramaFieldValue::String(cursor.string()?),
        ),
    ];
    parse_indexed_name_entries(cursor, &mut fields)?;
    fields.push(field(
        "graph_name",
        JDramaFieldValue::String(cursor.string()?),
    ));
    fields.push(field(
        "manager_name",
        JDramaFieldValue::String(cursor.string()?),
    ));
    fields.push(field("timer_max", JDramaFieldValue::I32(cursor.i32()?)));
    Ok(fields)
}

fn parse_indexed_name_entries(
    cursor: &mut StrictCursor<'_>,
    fields: &mut Vec<JDramaField>,
) -> Result<()> {
    let count = cursor.u32()? as usize;
    if count > MAX_SCAN_RECORDS || count > cursor.remaining() / 6 {
        return Err(FormatError::ResourceLimit {
            format: FORMAT,
            resource: "indexed name entries",
            requested: count,
            limit: (cursor.remaining() / 6).min(MAX_SCAN_RECORDS),
        });
    }
    fields.push(field(
        "indexed_name_count",
        JDramaFieldValue::U32(count as u32),
    ));
    for index in 0..count {
        fields.push(field(
            &format!("indexed_name_{index}_value"),
            JDramaFieldValue::I32(cursor.i32()?),
        ));
        fields.push(field(
            &format!("indexed_name_{index}_name"),
            JDramaFieldValue::String(cursor.string()?),
        ));
    }
    Ok(())
}

fn parse_map_fields(cursor: &mut StrictCursor<'_>) -> Result<Vec<JDramaField>> {
    let xlu_count = cursor.u32()? as usize;
    if xlu_count > MAX_SCAN_RECORDS {
        return Err(FormatError::ResourceLimit {
            format: FORMAT,
            resource: "map translucent groups",
            requested: xlu_count,
            limit: MAX_SCAN_RECORDS,
        });
    }
    let mut fields = vec![field(
        "translucent_group_count",
        JDramaFieldValue::U32(xlu_count as u32),
    )];
    for group_index in 0..xlu_count {
        let joint_count = cursor.u32()? as usize;
        if joint_count > MAX_SCAN_RECORDS || joint_count > cursor.remaining() / 8 {
            return Err(FormatError::ResourceLimit {
                format: FORMAT,
                resource: "map translucent joints",
                requested: joint_count,
                limit: (cursor.remaining() / 8).min(MAX_SCAN_RECORDS),
            });
        }
        fields.push(field(
            &format!("translucent_group_{group_index}_joint_count"),
            JDramaFieldValue::U32(joint_count as u32),
        ));
        for joint_index in 0..joint_count {
            fields.push(field(
                &format!("translucent_group_{group_index}_joint_{joint_index}_parent"),
                JDramaFieldValue::U32(cursor.u32()?),
            ));
            fields.push(field(
                &format!("translucent_group_{group_index}_joint_{joint_index}_child"),
                JDramaFieldValue::U32(cursor.u32()?),
            ));
        }
    }
    for name in [
        "collision_grid_width",
        "collision_grid_height",
        "collision_triangle_capacity",
        "collision_list_capacity",
        "collision_warp_capacity",
    ] {
        fields.push(field(name, JDramaFieldValue::I32(cursor.i32()?)));
    }

    let warp_count = cursor.u32()? as usize;
    if warp_count > 20 || warp_count > MAX_SCAN_RECORDS {
        return Err(FormatError::ResourceLimit {
            format: FORMAT,
            resource: "map warp pairs",
            requested: warp_count,
            limit: 20,
        });
    }
    fields.push(field(
        "warp_pair_count",
        JDramaFieldValue::U32(warp_count as u32),
    ));
    if warp_count == 0 {
        if cursor.remaining() == 4 {
            fields.push(field(
                "authoring_parameter",
                JDramaFieldValue::U32(cursor.u32()?),
            ));
        }
        return Ok(fields);
    }
    fields.push(field("warp_flags", JDramaFieldValue::U32(cursor.u32()?)));
    for index in 0..warp_count {
        fields.push(field(
            &format!("warp_pair_{index}_source_id"),
            JDramaFieldValue::U32(cursor.u32()?),
        ));
        fields.push(field(
            &format!("warp_pair_{index}_destination_id"),
            JDramaFieldValue::U32(cursor.u32()?),
        ));
    }
    for index in 0..warp_count * 2 {
        fields.push(field(
            &format!("warp_point_{index}_name"),
            JDramaFieldValue::String(cursor.string()?),
        ));
        fields.push(field(
            &format!("warp_point_{index}_position"),
            JDramaFieldValue::Vec3F32(cursor.vec3_f32()?),
        ));
        fields.push(field(
            &format!("warp_point_{index}_rotation"),
            JDramaFieldValue::Vec3F32(cursor.vec3_f32()?),
        ));
        fields.push(field(
            &format!("warp_point_{index}_scale"),
            JDramaFieldValue::Vec3F32(cursor.vec3_f32()?),
        ));
    }
    Ok(fields)
}

fn parse_map_event_sink_fields(
    cursor: &mut StrictCursor<'_>,
    short_type: &str,
) -> Result<Vec<JDramaField>> {
    let building_count = cursor.u32()? as usize;
    if building_count > 64 || building_count > cursor.remaining() / 8 {
        return Err(FormatError::ResourceLimit {
            format: FORMAT,
            resource: "map-event buildings",
            requested: building_count,
            limit: (cursor.remaining() / 8).min(64),
        });
    }
    let mut fields = vec![
        field(
            "building_count",
            JDramaFieldValue::U32(building_count as u32),
        ),
        field("first_building_index", JDramaFieldValue::U32(cursor.u32()?)),
    ];
    for index in 0..building_count {
        fields.push(field(
            &format!("building_{index}_pollution_layer"),
            JDramaFieldValue::U32(cursor.u32()?),
        ));
        fields.push(field(
            &format!("building_{index}_pollution_object"),
            JDramaFieldValue::U32(cursor.u32()?),
        ));
        if short_type == "MapEventSinkShadowMario" {
            fields.push(field(
                &format!("building_{index}_actor_name"),
                JDramaFieldValue::String(cursor.string()?),
            ));
        }
    }
    if matches!(short_type, "MapEventSinkBianco" | "MapEventSirenaSink") {
        fields.push(field(
            "warp_name",
            JDramaFieldValue::String(cursor.string()?),
        ));
        fields.push(field(
            "warp_position",
            JDramaFieldValue::Vec3F32(cursor.vec3_f32()?),
        ));
        fields.push(field("reserved", JDramaFieldValue::I32(cursor.i32()?)));
        fields.push(field("warp_y", JDramaFieldValue::F32(cursor.f32()?)));
        for index in 0..4 {
            fields.push(field(
                &format!("authoring_parameter_{index}"),
                JDramaFieldValue::F32(cursor.f32()?),
            ));
        }
    }
    Ok(fields)
}

fn parse_dolpic_gate_event_fields(cursor: &mut StrictCursor<'_>) -> Result<Vec<JDramaField>> {
    let mut fields = vec![
        field("warp_name", JDramaFieldValue::String(cursor.string()?)),
        field(
            "warp_position",
            JDramaFieldValue::Vec3F32(cursor.vec3_f32()?),
        ),
        field("reserved", JDramaFieldValue::I32(cursor.i32()?)),
        field("warp_y", JDramaFieldValue::F32(cursor.f32()?)),
    ];
    for index in 0..4 {
        fields.push(field(
            &format!("authoring_parameter_{index}"),
            JDramaFieldValue::F32(cursor.f32()?),
        ));
    }
    Ok(fields)
}

fn parse_actor_prefix<'a>(
    bytes: &'a [u8],
    offset: usize,
    end: usize,
) -> Option<(JDramaTransform, String, JDramaLightMap, StrictCursor<'a>)> {
    let mut cursor = StrictCursor::new(bytes, offset, end);
    let transform = JDramaTransform {
        translation: cursor.vec3_f32().ok()?,
        rotation: cursor.vec3_f32().ok()?,
        scale: cursor.vec3_f32().ok()?,
    };
    if !transform
        .translation
        .into_iter()
        .chain(transform.rotation)
        .chain(transform.scale)
        .all(f32::is_finite)
    {
        return None;
    }
    let character_name = cursor.string().ok()?;
    let light_map = cursor.light_map().ok()?;
    Some((transform, character_name, light_map, cursor))
}

fn parse_actor_tail(cursor: &mut StrictCursor<'_>, short_type: &str) -> Result<Vec<JDramaField>> {
    if cursor.is_done() {
        return Ok(Vec::new());
    }

    let fields = match short_type {
        "LiveActor" => vec![field(
            "manager_name",
            JDramaFieldValue::String(cursor.string()?),
        )],
        "OneShotGenerator" => vec![
            field("graph_name", JDramaFieldValue::String(cursor.string()?)),
            field("manager_name", JDramaFieldValue::String(cursor.string()?)),
        ],
        "MapObjGrassGroup" => vec![field("blade_count", JDramaFieldValue::U32(cursor.u32()?))],
        "SwitchHelp" => vec![
            field("help_flags", JDramaFieldValue::U32(cursor.u32()?)),
            field("help_message_index", JDramaFieldValue::U32(cursor.u32()?)),
            field(
                "target_actor_name",
                JDramaFieldValue::String(cursor.string()?),
            ),
        ],
        "BalloonHelp" => vec![
            field("help_flags", JDramaFieldValue::U32(cursor.u32()?)),
            field("help_message_index", JDramaFieldValue::U32(cursor.u32()?)),
            field(
                "target_actor_name",
                JDramaFieldValue::String(cursor.string()?),
            ),
        ],
        "MarioModokiTelesa" => vec![
            field("manager_name", JDramaFieldValue::String(cursor.string()?)),
            field("graph_name", JDramaFieldValue::String(cursor.string()?)),
            field("coin_id", JDramaFieldValue::I32(cursor.i32()?)),
            field("imitation_index", JDramaFieldValue::U32(cursor.u32()?)),
        ],
        "NozzleBox" => vec![
            field("resource_name", JDramaFieldValue::String(cursor.string()?)),
            field("item_selector", JDramaFieldValue::String(cursor.string()?)),
            field("validity_name", JDramaFieldValue::String(cursor.string()?)),
            field("break_height", JDramaFieldValue::F32(cursor.f32()?)),
            field("respawn_height", JDramaFieldValue::F32(cursor.f32()?)),
        ],
        "MapObjChangeStage" => vec![
            field("resource_name", JDramaFieldValue::String(cursor.string()?)),
            field("stage_id", JDramaFieldValue::U32(cursor.u32()?)),
        ],
        "MapObjStartDemo" => vec![
            field("resource_name", JDramaFieldValue::String(cursor.string()?)),
            field("demo_id", JDramaFieldValue::U32(cursor.u32()?)),
        ],
        "Mario" => vec![
            field("starting_water", JDramaFieldValue::U32(cursor.u32()?)),
            field("equipment_flags", JDramaFieldValue::U32(cursor.u32()?)),
        ],
        "RedCoinSwitch" => vec![
            field("resource_name", JDramaFieldValue::String(cursor.string()?)),
            field("timer_tenths", JDramaFieldValue::I32(cursor.i32()?)),
            field("tev_red", JDramaFieldValue::U32(cursor.u32()?)),
            field("tev_green", JDramaFieldValue::U32(cursor.u32()?)),
            field("tev_blue", JDramaFieldValue::U32(cursor.u32()?)),
        ],
        "WaterHitPictureHideObj" | "HideObjPictureTwin" => vec![
            field("resource_name", JDramaFieldValue::String(cursor.string()?)),
            field("manhole_flag", JDramaFieldValue::I32(cursor.i32()?)),
            field("appear_rate", JDramaFieldValue::F32(cursor.f32()?)),
            field("appear_height", JDramaFieldValue::F32(cursor.f32()?)),
            field("object_timer", JDramaFieldValue::I32(cursor.i32()?)),
            field("tev_red", JDramaFieldValue::U32(cursor.u32()?)),
            field("tev_green", JDramaFieldValue::U32(cursor.u32()?)),
            field("tev_blue", JDramaFieldValue::U32(cursor.u32()?)),
        ],
        "WoodBox" | "MapObjNail" | "WaterHitHideObj" | "HipDropHideObj" | "MiniWindmill"
        | "Billboard" | "FruitBasketEvent" | "FruitHitHideObj" | "PosterTeresa"
        | "DolWeathercock" | "WatermelonBlock" | "BellWatermill" | "BrickBlock"
        | "PictureTeresa" | "SuperHipDropBlock" | "BreakableBlock" => {
            vec![
                field("resource_name", JDramaFieldValue::String(cursor.string()?)),
                field("event_id", JDramaFieldValue::I32(cursor.i32()?)),
                field("appear_rate", JDramaFieldValue::F32(cursor.f32()?)),
                field("appear_height", JDramaFieldValue::F32(cursor.f32()?)),
                field("object_timer", JDramaFieldValue::I32(cursor.i32()?)),
            ]
        }
        "HideObj" => vec![
            field("resource_name", JDramaFieldValue::String(cursor.string()?)),
            field("event_id", JDramaFieldValue::I32(cursor.i32()?)),
            field("appear_rate", JDramaFieldValue::F32(cursor.f32()?)),
            field("appear_height", JDramaFieldValue::F32(cursor.f32()?)),
        ],
        "MapObjSmoke" => vec![
            field("resource_name", JDramaFieldValue::String(cursor.string()?)),
            field("event_id", JDramaFieldValue::I32(cursor.i32()?)),
            field("appear_rate", JDramaFieldValue::F32(cursor.f32()?)),
            field("appear_height", JDramaFieldValue::F32(cursor.f32()?)),
        ],
        "Door" => vec![
            field("resource_name", JDramaFieldValue::String(cursor.string()?)),
            field("door_type", JDramaFieldValue::I32(cursor.i32()?)),
        ],
        "FlowerCoin" => vec![
            field("resource_name", JDramaFieldValue::String(cursor.string()?)),
            field("flower_id", JDramaFieldValue::I32(cursor.i32()?)),
        ],
        "LeafBoatRotten" => vec![
            field("resource_name", JDramaFieldValue::String(cursor.string()?)),
            field("lifetime_tenths", JDramaFieldValue::I32(cursor.i32()?)),
        ],
        "WarpArea" => vec![
            field("source_area", JDramaFieldValue::I32(cursor.i32()?)),
            field("destination_area", JDramaFieldValue::I32(cursor.i32()?)),
        ],
        "BiaWatermillVertical" => vec![
            field("resource_name", JDramaFieldValue::String(cursor.string()?)),
            field(
                "rotation_speed_thousandths",
                JDramaFieldValue::I32(cursor.i32()?),
            ),
        ],
        "MapObjSwitch" => vec![
            field("resource_name", JDramaFieldValue::String(cursor.string()?)),
            field("timer_tenths", JDramaFieldValue::I32(cursor.i32()?)),
            field("tev_red", JDramaFieldValue::U32(cursor.u32()?)),
            field("tev_green", JDramaFieldValue::U32(cursor.u32()?)),
            field("tev_blue", JDramaFieldValue::U32(cursor.u32()?)),
        ],
        "Puncher" => vec![
            field("resource_name", JDramaFieldValue::String(cursor.string()?)),
            field("launch_power", JDramaFieldValue::I32(cursor.i32()?)),
        ],
        "MareEventBumpyWall" => vec![
            field("resource_name", JDramaFieldValue::String(cursor.string()?)),
            field("building_index", JDramaFieldValue::I32(cursor.i32()?)),
        ],
        "HideObjInfo" => vec![
            field("event_id", JDramaFieldValue::I32(cursor.i32()?)),
            field("appear_rate", JDramaFieldValue::F32(cursor.f32()?)),
            field("appear_height", JDramaFieldValue::F32(cursor.f32()?)),
            field("object_timer", JDramaFieldValue::I32(cursor.i32()?)),
        ],
        "LeanMirror" => {
            let mut fields = vec![
                field("resource_name", JDramaFieldValue::String(cursor.string()?)),
                field("radius_percent", JDramaFieldValue::F32(cursor.f32()?)),
            ];
            if !cursor.is_done() {
                fields.push(field(
                    "demo_target_name",
                    JDramaFieldValue::String(cursor.string()?),
                ));
                fields.push(field(
                    "demo_target_position",
                    JDramaFieldValue::Vec3F32(cursor.vec3_f32()?),
                ));
            }
            if !cursor.is_done() {
                fields.push(field(
                    "authoring_rotation",
                    JDramaFieldValue::Vec3F32(cursor.vec3_f32()?),
                ));
                fields.push(field(
                    "authoring_scale",
                    JDramaFieldValue::Vec3F32(cursor.vec3_f32()?),
                ));
            }
            fields
        }
        "RideCloud" => {
            let mut fields = vec![
                field("resource_name", JDramaFieldValue::String(cursor.string()?)),
                field("graph_name", JDramaFieldValue::String(cursor.string()?)),
            ];
            for name in [
                "upper_red",
                "upper_green",
                "upper_blue",
                "lower_red",
                "lower_green",
                "lower_blue",
            ] {
                fields.push(field(name, JDramaFieldValue::U32(cursor.u32()?)));
            }
            fields
        }
        "MapObjWaterSpray" => vec![
            field("resource_name", JDramaFieldValue::String(cursor.string()?)),
            field("spray_radius", JDramaFieldValue::F32(cursor.f32()?)),
            field("spray_height", JDramaFieldValue::F32(cursor.f32()?)),
            field("color_red", JDramaFieldValue::U32(cursor.u32()?)),
            field("color_green", JDramaFieldValue::U32(cursor.u32()?)),
            field("color_blue", JDramaFieldValue::U32(cursor.u32()?)),
            field("color_alpha", JDramaFieldValue::U32(cursor.u32()?)),
        ],
        "NormalLift" | "EXKickBoard" | "Kamaboko" | "Uirou" | "Castella" | "Hikidashi" => {
            let mut fields = vec![
                field("resource_name", JDramaFieldValue::String(cursor.string()?)),
                field("graph_name", JDramaFieldValue::String(cursor.string()?)),
            ];
            if !cursor.is_done() {
                fields.push(field(
                    "initial_rail_position",
                    JDramaFieldValue::F32(cursor.f32()?),
                ));
            }
            fields
        }
        "RailBlock" | "RailBlockR" | "RailBlockY" | "RailBlockB" | "EXRollCube" => vec![
            field("resource_name", JDramaFieldValue::String(cursor.string()?)),
            field("graph_name", JDramaFieldValue::String(cursor.string()?)),
        ],
        "WoodBlock" | "YoshiBlock" => vec![
            field("resource_name", JDramaFieldValue::String(cursor.string()?)),
            field("graph_name", JDramaFieldValue::String(cursor.string()?)),
            field("collision_value", JDramaFieldValue::F32(cursor.f32()?)),
            field("tev_red", JDramaFieldValue::U32(cursor.u32()?)),
            field("tev_green", JDramaFieldValue::U32(cursor.u32()?)),
            field("tev_blue", JDramaFieldValue::U32(cursor.u32()?)),
        ],
        "Umaibou" | "GetaGreen" | "GetaOrange" | "RollBlock" | "RollBlockR" | "RollBlockY"
        | "RollBlockB" => vec![
            field("resource_name", JDramaFieldValue::String(cursor.string()?)),
            field(
                "rotation_speed_hundredths",
                JDramaFieldValue::I32(cursor.i32()?),
            ),
        ],
        "CoinBlue" => vec![
            field("resource_name", JDramaFieldValue::String(cursor.string()?)),
            field("blue_coin_id", JDramaFieldValue::I32(cursor.i32()?)),
        ],
        "EggYoshi" => vec![
            field("resource_name", JDramaFieldValue::String(cursor.string()?)),
            field("egg_id", JDramaFieldValue::I32(cursor.i32()?)),
        ],
        "Shine" => vec![
            field("resource_name", JDramaFieldValue::String(cursor.string()?)),
            field(
                "collection_type",
                JDramaFieldValue::String(cursor.string()?),
            ),
            field("shine_id", JDramaFieldValue::I32(cursor.i32()?)),
            field("in_stage", JDramaFieldValue::I32(cursor.i32()?)),
        ],
        "JumpMushroom" => vec![
            field("resource_name", JDramaFieldValue::String(cursor.string()?)),
            field("collision_type", JDramaFieldValue::I32(cursor.i32()?)),
        ],
        "CraneRotY" => vec![
            field("resource_name", JDramaFieldValue::String(cursor.string()?)),
            field(
                "rotation_target_degrees",
                JDramaFieldValue::F32(cursor.f32()?),
            ),
        ],
        "BossManta" | "MameGesso" => vec![
            field("manager_name", JDramaFieldValue::String(cursor.string()?)),
            field("graph_name", JDramaFieldValue::String(cursor.string()?)),
            field("coin_id", JDramaFieldValue::I32(cursor.i32()?)),
        ],
        "NPCBoard" | "GateKeeper" | "OrangeSeal" | "FruitsBoat" | "FruitsBoatB" | "FruitsBoatC"
        | "FruitsBoatD" | "BossPakkun" | "Koopa" | "KoopaJr" | "LimitKoopaJr" | "BathtubPeach"
        | "HamukuriLauncher" | "BossTelesa" | "BossGesso" | "BossEel" | "KBossPakkun" | "Kukku"
        | "Cannon" | "BossHanachan" | "SleepBossHanachan" | "BossWanwan" | "TinKoopa"
        | "RiccoHook" | "HinoKuri2" => {
            vec![
                field("manager_name", JDramaFieldValue::String(cursor.string()?)),
                field("graph_name", JDramaFieldValue::String(cursor.string()?)),
            ]
        }
        "LoopTelesa" | "BoxTelesa" | "SeeTelesa" | "HamuKuri" | "HaneHamuKuri"
        | "HaneHamuKuri2" | "Gesso" | "HanaSambo" | "SamboHead" | "DebuTelesa" | "Yumbo"
        | "TabePuku" | "LandGesso" | "PoiHana" | "PoiHanaRed" | "SleepPoiHana" | "FireWanwan"
        | "AmiNoko" | "Kumokun" | "FireHamuKuri" | "DoroHaneKuri" | "TamaNoko"
        | "BossDangoHamuKuri" | "Rocket" | "ElecNokonoko" => vec![
            field("manager_name", JDramaFieldValue::String(cursor.string()?)),
            field("graph_name", JDramaFieldValue::String(cursor.string()?)),
            field("coin_id", JDramaFieldValue::I32(cursor.i32()?)),
        ],
        "Amenbo" | "Kazekun" => vec![field(
            "manager_name",
            JDramaFieldValue::String(cursor.string()?),
        )],
        "MoePukuLaunchPad" | "TobiPukuLaunchPad" => vec![
            field("manager_name", JDramaFieldValue::String(cursor.string()?)),
            field("graph_name", JDramaFieldValue::String(cursor.string()?)),
            field("coin_id", JDramaFieldValue::I32(cursor.i32()?)),
            field("launch_speed", JDramaFieldValue::I32(cursor.i32()?)),
        ],
        "EMario" => vec![
            field("manager_name", JDramaFieldValue::String(cursor.string()?)),
            field("graph_name", JDramaFieldValue::String(cursor.string()?)),
            field("costume_red", JDramaFieldValue::U32(cursor.u32()?)),
            field("costume_green", JDramaFieldValue::U32(cursor.u32()?)),
            field("costume_blue", JDramaFieldValue::U32(cursor.u32()?)),
            field("costume_alpha", JDramaFieldValue::U32(cursor.u32()?)),
            field("authoring_value_0", JDramaFieldValue::U32(cursor.u32()?)),
            field("authoring_value_1", JDramaFieldValue::U32(cursor.u32()?)),
        ],
        "SamboFlower" => vec![
            field("manager_name", JDramaFieldValue::String(cursor.string()?)),
            field("graph_name", JDramaFieldValue::String(cursor.string()?)),
            field("coin_id", JDramaFieldValue::I32(cursor.i32()?)),
            field("flower_group_id", JDramaFieldValue::I32(cursor.i32()?)),
        ],
        "WireTrap" => vec![
            field("manager_name", JDramaFieldValue::String(cursor.string()?)),
            field("graph_name", JDramaFieldValue::String(cursor.string()?)),
            field("shake_base", JDramaFieldValue::I32(cursor.i32()?)),
            field("color_type", JDramaFieldValue::I32(cursor.i32()?)),
            field("wait_time", JDramaFieldValue::I32(cursor.i32()?)),
            field("mode", JDramaFieldValue::I32(cursor.i32()?)),
        ],
        "CommonLauncher" => vec![
            field("manager_name", JDramaFieldValue::String(cursor.string()?)),
            field("graph_name", JDramaFieldValue::String(cursor.string()?)),
            field(
                "launched_enemy_name",
                JDramaFieldValue::String(cursor.string()?),
            ),
            field("launch_period", JDramaFieldValue::I32(cursor.i32()?)),
        ],
        "FluffManager" => vec![
            field("resource_name", JDramaFieldValue::String(cursor.string()?)),
            field("wind_depth", JDramaFieldValue::F32(cursor.f32()?)),
            field("wind_scale_percent", JDramaFieldValue::F32(cursor.f32()?)),
            field("fluff_count", JDramaFieldValue::I32(cursor.i32()?)),
        ],
        "SwingBoard" => vec![
            field("resource_name", JDramaFieldValue::String(cursor.string()?)),
            field("board_height", JDramaFieldValue::F32(cursor.f32()?)),
            field("swing_parameter", JDramaFieldValue::F32(cursor.f32()?)),
        ],
        "RailFence" => vec![
            field("resource_name", JDramaFieldValue::String(cursor.string()?)),
            field("graph_name", JDramaFieldValue::String(cursor.string()?)),
        ],
        "BeeHive" => vec![
            field("manager_name", JDramaFieldValue::String(cursor.string()?)),
            field("graph_name", JDramaFieldValue::String(cursor.string()?)),
            field("bee_count", JDramaFieldValue::U32(cursor.u32()?)),
            field("first_event_id", JDramaFieldValue::U32(cursor.u32()?)),
            field("last_event_id", JDramaFieldValue::U32(cursor.u32()?)),
        ],
        "StayPakkun" => vec![
            field("manager_name", JDramaFieldValue::String(cursor.string()?)),
            field("graph_name", JDramaFieldValue::String(cursor.string()?)),
            field("coin_id", JDramaFieldValue::I32(cursor.i32()?)),
        ],
        "AnimalBird" => vec![
            field("manager_name", JDramaFieldValue::String(cursor.string()?)),
            field("graph_name", JDramaFieldValue::String(cursor.string()?)),
            field("bird_id", JDramaFieldValue::I32(cursor.i32()?)),
        ],
        "AnimalMew" => vec![
            field("manager_name", JDramaFieldValue::String(cursor.string()?)),
            field("graph_name", JDramaFieldValue::String(cursor.string()?)),
            field("mew_count", JDramaFieldValue::U32(cursor.u32()?)),
        ],
        "FishoidA" | "FishoidB" | "FishoidC" | "FishoidD" | "Butterfly" | "ButterflyB"
        | "ButterflyC" => vec![
            field("manager_name", JDramaFieldValue::String(cursor.string()?)),
            field("graph_name", JDramaFieldValue::String(cursor.string()?)),
            field("fish_count", JDramaFieldValue::U32(cursor.u32()?)),
            field("group_id", JDramaFieldValue::I32(cursor.i32()?)),
        ],
        value if is_npc_actor_type(value) => {
            let mut fields = vec![
                field("manager_name", JDramaFieldValue::String(cursor.string()?)),
                field("graph_name", JDramaFieldValue::String(cursor.string()?)),
            ];
            let mut names = vec![
                "body_color_index",
                "cloth_color_index",
                "pollution_amount",
                "parts_color_index_0",
                "parts_color_index_1",
                "parts_color_index_2",
                "parts_mask",
                "movement_type",
                "action_flags",
                "motion_min",
                "motion_max",
                "coin_flag",
            ];
            // NPCDummy retail records end one word before the common NPC tail;
            // the record-bounded stream leaves the final source read at its
            // default value.
            if value == "NPCDummy" {
                names.clear();
            }
            for name in names {
                fields.push(field(name, JDramaFieldValue::I32(cursor.i32()?)));
            }
            fields
        }
        value if actor_single_string_tail(value) => {
            let mut fields = vec![field(
                "resource_name",
                JDramaFieldValue::String(cursor.string()?),
            )];
            if cursor.remaining() == 4 {
                fields.push(field("damage_height", JDramaFieldValue::F32(cursor.f32()?)));
            }
            fields
        }
        _ => {
            return Err(FormatError::Unsupported {
                format: FORMAT,
                message: format!(
                    "strict JDrama schema has no typed actor tail for {short_type} ({} bytes remain)",
                    cursor.remaining()
                ),
            });
        }
    };
    Ok(fields)
}

fn actor_single_string_tail(short_type: &str) -> bool {
    matches!(
        short_type,
        "MapObjFlag"
            | "Shimmer"
            | "MapStaticObj"
            | "Manhole"
            | "Palm"
            | "MapObjBase"
            | "MonumentShine"
            | "BellDolpicTV"
            | "BellDolpicPolice"
            | "DptMonteFence"
            | "TurboNozzleDoor"
            | "Coin"
            | "WaterRecoverObj"
            | "Mushroom1upX"
            | "WoodBarrel"
            | "ResetFruit"
            | "CoverFruit"
            | "NormalBlock"
            | "WatermelonStatic"
            | "WaterMoveBlock"
            | "Fence"
            | "IceCar"
            | "Football"
            | "CasinoRoulette"
            | "PalmOugi"
            | "SlotDrum"
            | "CasinoPanelGate"
            | "Bathtub"
            | "ItemSlotDrum"
            | "Donchou"
            | "BananaTree"
            | "CoinRed"
            | "SandBlock"
            | "SakuCasino"
            | "MapObjTreeScale"
            | "RiccoLog"
            | "SirenaCasinoRoof"
            | "IceBlock"
            | "Item"
            | "MapObjRootPakkun"
            | "BigWindmill"
            | "MapObjSteam"
            | "GlassBreak"
            | "LampTrapSpike"
            | "Fruit"
            | "Closet"
            | "WindmillRoof"
            | "LampTrapIron"
            | "ChestRevolve"
            | "Roulette"
            | "MapObjGeneral"
            | "MuddyBoat"
            | "TelesaSlot"
            | "BiaBell"
            | "SirenabossWall"
            | "DemoCannon"
            | "BiaWatermill"
            | "MareGate"
            | "PanelBreak"
            | "BiaTurnBridge"
            | "PanelRevolve"
            | "ItemNozzle"
            | "MapObjFloatOnSea"
            | "PalmSago"
            | "SandBird"
            | "LeafBoat"
            | "ShiningStone"
            | "SandBombBase"
            | "SandLeafBase"
            | "JumpBase"
            | "Cogwheel"
            | "SandCastle"
            | "SandEgg"
            | "MareFall"
            | "MammaYacht"
            | "MareCork"
            | "GoalFlag"
            | "SandBomb"
            | "HangingBridge"
            | "CoinFish"
            | "FenceRevolve"
            | "FenceInner"
            | "WaterMelon"
            | "BasketReverse"
            | "FruitTree"
            | "FileLoadBlockA"
            | "FileLoadBlockB"
            | "FileLoadBlockC"
            | "PalmNatume"
            | "FerrisWheel"
            | "RandomFruit"
            | "Viking"
            | "PinnaCoaster"
            | "Merrygoround"
            | "FenceWaterH"
            | "RiccoSwitch"
            | "RiccoSwitchShine"
            | "PinnaDoor"
            | "PinnaDoorOpen"
            | "FenceWaterV"
            | "BalloonKoopaJr"
            | "ShellCup"
            | "craneUpDown"
            | "GateShell"
            | "AmiKing"
            | "riccoWatermill"
            | "SurfGesoRed"
            | "SurfGesoYellow"
            | "SurfGesoGreen"
    )
}

fn is_map_event_sink_type(short_type: &str) -> bool {
    matches!(
        short_type,
        "MapEventSinkInPollution"
            | "MapEventSinkInPollutionReset"
            | "MapEventSinkShadowMario"
            | "MapEventSirenaSink"
            | "MapEventSinkBianco"
            | "AirportEventSink"
    )
}

fn parse_obj_manager_fields(
    cursor: &mut StrictCursor<'_>,
    short_type: &str,
) -> Result<Vec<JDramaField>> {
    let mut fields = vec![
        field("character_name", JDramaFieldValue::String(cursor.string()?)),
        field("capacity", JDramaFieldValue::U32(cursor.u32()?)),
    ];
    if matches!(
        short_type,
        "ItemManager" | "MapObjManager" | "MapObjBaseManager" | "PoolManager"
    ) {
        fields.push(field("clip_distance", JDramaFieldValue::F32(cursor.f32()?)));
        fields.push(field("clip_radius", JDramaFieldValue::F32(cursor.f32()?)));
    } else if cursor.remaining() == 4 {
        fields.push(field(
            "manager_load_value",
            JDramaFieldValue::U32(cursor.u32()?),
        ));
    }
    Ok(fields)
}

fn ensure_strict_payload_end(
    cursor: &StrictCursor<'_>,
    type_name: &str,
    record_offset: usize,
) -> Result<()> {
    if cursor.is_done() {
        Ok(())
    } else {
        Err(unsupported_strict_payload(
            type_name,
            record_offset,
            cursor.remaining(),
        ))
    }
}

fn unsupported_strict_payload(
    type_name: &str,
    record_offset: usize,
    remaining: usize,
) -> FormatError {
    FormatError::Unsupported {
        format: FORMAT,
        message: format!(
            "strict JDrama schema has no typed payload for {type_name} at {record_offset:#x} ({remaining} bytes remain)"
        ),
    }
}

fn write_strict_record(bytes: &mut Vec<u8>, record: &JDramaRecord) -> Result<()> {
    let start = bytes.len();
    bytes.extend_from_slice(&0_u32.to_be_bytes());
    bytes.extend_from_slice(&jdrama_key_code(&record.type_name)?.to_be_bytes());
    write_len_string(bytes, &record.type_name)?;
    bytes.extend_from_slice(&jdrama_key_code(&record.name)?.to_be_bytes());
    write_len_string(bytes, &record.name)?;

    match &record.payload {
        JDramaRecordPayload::Empty => {}
        JDramaRecordPayload::Fields { fields } => write_fields(bytes, fields)?,
        JDramaRecordPayload::Actor {
            transform,
            character_name,
            light_map,
            fields,
        } => {
            write_vec3_f32(bytes, transform.translation);
            write_vec3_f32(bytes, transform.rotation);
            write_vec3_f32(bytes, transform.scale);
            write_len_string(bytes, character_name)?;
            write_light_map(bytes, light_map)?;
            write_fields(bytes, fields)?;
        }
        JDramaRecordPayload::Group { fields, children } => {
            write_fields(bytes, fields)?;
            let child_count =
                u32::try_from(children.len()).map_err(|_| FormatError::ResourceLimit {
                    format: FORMAT,
                    resource: "group children",
                    requested: children.len(),
                    limit: u32::MAX as usize,
                })?;
            bytes.extend_from_slice(&child_count.to_be_bytes());
            for child in children {
                write_strict_record(bytes, child)?;
            }
        }
    }

    let size = bytes.len() - start;
    let size = u32::try_from(size).map_err(|_| FormatError::ResourceLimit {
        format: FORMAT,
        resource: "record bytes",
        requested: size,
        limit: u32::MAX as usize,
    })?;
    bytes[start..start + 4].copy_from_slice(&size.to_be_bytes());
    Ok(())
}

fn write_fields(bytes: &mut Vec<u8>, fields: &[JDramaField]) -> Result<()> {
    for field in fields {
        match &field.value {
            JDramaFieldValue::U32(value) => bytes.extend_from_slice(&value.to_be_bytes()),
            JDramaFieldValue::I32(value) => bytes.extend_from_slice(&value.to_be_bytes()),
            JDramaFieldValue::F32(value) => bytes.extend_from_slice(&value.to_bits().to_be_bytes()),
            JDramaFieldValue::Vec2F32(value) => {
                for component in value {
                    bytes.extend_from_slice(&component.to_bits().to_be_bytes());
                }
            }
            JDramaFieldValue::Vec3F32(value) => write_vec3_f32(bytes, *value),
            JDramaFieldValue::ColorRgba8(value) => bytes.extend_from_slice(value),
            JDramaFieldValue::String(value) => write_len_string(bytes, value)?,
            JDramaFieldValue::LightMap(value) => write_light_map(bytes, value)?,
        }
    }
    Ok(())
}

fn write_vec3_f32(bytes: &mut Vec<u8>, value: [f32; 3]) {
    for component in value {
        bytes.extend_from_slice(&component.to_bits().to_be_bytes());
    }
}

fn write_light_map(bytes: &mut Vec<u8>, light_map: &JDramaLightMap) -> Result<()> {
    let count = u32::try_from(light_map.entries.len()).map_err(|_| FormatError::ResourceLimit {
        format: FORMAT,
        resource: "light-map entries",
        requested: light_map.entries.len(),
        limit: u32::MAX as usize,
    })?;
    bytes.extend_from_slice(&count.to_be_bytes());
    for entry in &light_map.entries {
        bytes.extend_from_slice(&entry.channel.to_be_bytes());
        write_len_string(bytes, &entry.light_name)?;
    }
    Ok(())
}

fn write_len_string(bytes: &mut Vec<u8>, value: &str) -> Result<()> {
    let encoded = encode_shift_jis(value)?;
    let len = u16::try_from(encoded.len()).map_err(|_| FormatError::ResourceLimit {
        format: FORMAT,
        resource: "Shift-JIS string bytes",
        requested: encoded.len(),
        limit: u16::MAX as usize,
    })?;
    bytes.extend_from_slice(&len.to_be_bytes());
    bytes.extend_from_slice(&encoded);
    Ok(())
}

fn encode_shift_jis(value: &str) -> Result<Vec<u8>> {
    let (encoded, _, had_errors) = SHIFT_JIS.encode(value);
    if had_errors {
        return Err(FormatError::Unsupported {
            format: FORMAT,
            message: format!("string cannot be encoded as Shift-JIS: {value:?}"),
        });
    }
    Ok(encoded.into_owned())
}

fn read_len_string_strict(bytes: &[u8], offset: usize, limit: usize) -> Result<(String, usize)> {
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
    let (value, had_errors) = SHIFT_JIS.decode_without_bom_handling(&bytes[start..end]);
    if had_errors {
        return Err(FormatError::Unsupported {
            format: FORMAT,
            message: format!("invalid Shift-JIS string at {offset:#x}"),
        });
    }
    Ok((value.into_owned(), end))
}

pub fn parse_jdrama_object_records(bytes: &[u8]) -> Result<Vec<JDramaObjectRecord>> {
    let mut records = Vec::new();
    let mut visited = BTreeSet::new();
    parse_record_at(
        bytes,
        0,
        bytes.len(),
        &mut Vec::new(),
        &mut visited,
        &mut records,
    )?;
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

/// Parses the complete outer runtime table, retaining empty/reserved areas
/// which the flat entry parser necessarily omits.
pub fn parse_jdrama_scenario_archive_areas(bytes: &[u8]) -> Result<Vec<JDramaScenarioArchiveArea>> {
    let document = JDramaDocument::parse(bytes)?;
    let mut table_paths = Vec::new();
    collect_scenario_archive_table_paths(&document.root, &mut Vec::new(), &mut table_paths);
    if table_paths.len() != 1 {
        return Err(FormatError::Unsupported {
            format: FORMAT,
            message: format!(
                "expected exactly one ScenarioArchiveNamesInStage table, found {}",
                table_paths.len()
            ),
        });
    }
    let table = jdrama_record_at_path(&document.root, &table_paths[0]).ok_or_else(|| {
        FormatError::Unsupported {
            format: FORMAT,
            message: "runtime scenario archive table path became invalid".to_string(),
        }
    })?;
    let JDramaRecordPayload::Group {
        children: area_records,
        ..
    } = &table.payload
    else {
        return Err(FormatError::Unsupported {
            format: FORMAT,
            message: "ScenarioArchiveNamesInStage is not a JDrama group".to_string(),
        });
    };
    area_records
        .iter()
        .enumerate()
        .map(|(area_index, area)| {
            if semantic_type_name(&area.type_name) != "ScenarioArchiveNameTable" {
                return Err(FormatError::Unsupported {
                    format: FORMAT,
                    message: format!(
                        "runtime area {area_index} has unexpected type {:?}",
                        area.type_name
                    ),
                });
            }
            let JDramaRecordPayload::Group { children, .. } = &area.payload else {
                return Err(FormatError::Unsupported {
                    format: FORMAT,
                    message: format!("runtime area {area_index} is not a JDrama group"),
                });
            };
            let archive_names = children
                .iter()
                .enumerate()
                .map(|(scenario_index, scenario)| {
                    scenario_archive_name(scenario)
                        .map(str::to_owned)
                        .ok_or_else(|| FormatError::Unsupported {
                            format: FORMAT,
                            message: format!(
                                "runtime area {area_index} scenario {scenario_index} is not a ScenarioArchiveName record"
                            ),
                        })
                })
                .collect::<Result<Vec<_>>>()?;
            Ok(JDramaScenarioArchiveArea {
                area_index: area_index as u32,
                name: area.name.clone(),
                archive_names,
            })
        })
        .collect()
}

/// Appends a genuinely new runtime scenario to a reserved carrier area in
/// `stageArc.bin`.
///
/// Existing areas and scenarios are rebuilt byte-for-byte through the typed
/// JDrama document. The outer area array is never enlarged: the retail
/// executable indexes several fixed 61-element tables by area without bounds
/// checks. Each carrier retains scenario zero (`none.arc`) and supplies
/// scenarios 1 through 255 for authored stages.
pub fn append_jdrama_scenario_archive_slot(
    bytes: &[u8],
    archive_name: &str,
) -> Result<JDramaScenarioArchiveWriteOutcome> {
    author_jdrama_scenario_archive_slot(bytes, archive_name, false)
}

/// Ensures that a unique authored runtime slot exists for `archive_name`.
///
/// This is the idempotent form used when reconciling managed build output. If
/// exactly one matching archive is already present in a reserved carrier, the
/// original bytes are returned unchanged. Retail or ambiguous duplicate
/// mappings are rejected.
pub fn ensure_jdrama_scenario_archive_slot(
    bytes: &[u8],
    archive_name: &str,
) -> Result<JDramaScenarioArchiveWriteOutcome> {
    author_jdrama_scenario_archive_slot(bytes, archive_name, true)
}

fn author_jdrama_scenario_archive_slot(
    bytes: &[u8],
    archive_name: &str,
    allow_existing: bool,
) -> Result<JDramaScenarioArchiveWriteOutcome> {
    let requested_stem = validate_authored_runtime_archive_name(archive_name)?;
    let existing = parse_jdrama_scenario_archive_entries(bytes)?;
    let matching = existing
        .iter()
        .filter(|entry| {
            runtime_archive_stem(&entry.archive_name)
                .is_some_and(|stem| stem.eq_ignore_ascii_case(requested_stem))
        })
        .collect::<Vec<_>>();
    let mut document = JDramaDocument::parse(bytes)?;
    let mut table_paths = Vec::new();
    collect_scenario_archive_table_paths(&document.root, &mut Vec::new(), &mut table_paths);
    if table_paths.len() != 1 {
        return Err(FormatError::Unsupported {
            format: FORMAT,
            message: format!(
                "expected exactly one ScenarioArchiveNamesInStage table, found {}",
                table_paths.len()
            ),
        });
    }
    let table =
        jdrama_record_at_path_mut(&mut document.root, &table_paths[0]).ok_or_else(|| {
            FormatError::Unsupported {
                format: FORMAT,
                message: "runtime scenario archive table path became invalid".to_string(),
            }
        })?;
    let JDramaRecordPayload::Group {
        children: areas, ..
    } = &mut table.payload
    else {
        return Err(FormatError::Unsupported {
            format: FORMAT,
            message: "ScenarioArchiveNamesInStage is not a JDrama group".to_string(),
        });
    };
    let minimum_areas = usize::from(
        *SMS_AUTHORED_RUNTIME_CARRIER_AREAS
            .iter()
            .max()
            .expect("carrier list is non-empty"),
    ) + 1;
    if areas.len() < minimum_areas || areas.len() > 61 {
        return Err(FormatError::Unsupported {
            format: FORMAT,
            message: format!(
                "runtime area table has {} areas; reserved carriers require {minimum_areas}..=61",
                areas.len()
            ),
        });
    }
    for &area_index in &SMS_AUTHORED_RUNTIME_CARRIER_AREAS {
        validate_authored_runtime_carrier(areas, area_index)?;
    }
    if !matching.is_empty() {
        if allow_existing
            && matching.len() == 1
            && u8::try_from(matching[0].area_index)
                .ok()
                .is_some_and(|area| SMS_AUTHORED_RUNTIME_CARRIER_AREAS.contains(&area))
            && (1..=u32::from(u8::MAX)).contains(&matching[0].scenario_index)
        {
            return Ok(JDramaScenarioArchiveWriteOutcome {
                bytes: bytes.to_vec(),
                entry: matching[0].clone(),
                inserted: false,
            });
        }
        return Err(FormatError::Unsupported {
            format: FORMAT,
            message: format!(
                "runtime archive stem {requested_stem:?} is already mapped by {} stageArc.bin entr{}",
                matching.len(),
                if matching.len() == 1 { "y" } else { "ies" }
            ),
        });
    }
    let area_index = SMS_AUTHORED_RUNTIME_CARRIER_AREAS
        .iter()
        .copied()
        .find(|area_index| {
            let Some(area) = areas.get(usize::from(*area_index)) else {
                return false;
            };
            let JDramaRecordPayload::Group { children, .. } = &area.payload else {
                return false;
            };
            children.len() <= usize::from(u8::MAX)
        })
        .ok_or_else(|| FormatError::ResourceLimit {
            format: FORMAT,
            resource: "authored runtime slots",
            requested: SMS_AUTHORED_RUNTIME_CARRIER_AREAS.len() * usize::from(u8::MAX) + 1,
            limit: SMS_AUTHORED_RUNTIME_CARRIER_AREAS.len() * usize::from(u8::MAX),
        })?;
    let JDramaRecordPayload::Group {
        children: scenarios,
        ..
    } = &mut areas[usize::from(area_index)].payload
    else {
        unreachable!("carrier validation proved a group payload")
    };
    let scenario_index = u8::try_from(scenarios.len()).map_err(|_| FormatError::ResourceLimit {
        format: FORMAT,
        resource: "runtime scenario index",
        requested: scenarios.len(),
        limit: u8::MAX as usize,
    })?;
    let scenario = JDramaRecord::new(
        "ScenarioArchiveName",
        format!("Graffito-Editor {requested_stem}"),
        JDramaRecordPayload::Fields {
            fields: vec![field(
                "archive_name",
                JDramaFieldValue::String(archive_name.to_string()),
            )],
        },
    )?;
    scenarios.push(scenario);

    let output = document.to_bytes()?;
    let entry = JDramaScenarioArchiveEntry {
        area_index: u32::from(area_index),
        scenario_index: u32::from(scenario_index),
        archive_name: archive_name.to_string(),
    };
    let rebuilt_entries = parse_jdrama_scenario_archive_entries(&output)?;
    if !rebuilt_entries.contains(&entry) {
        return Err(FormatError::Unsupported {
            format: FORMAT,
            message: "authored runtime slot did not survive semantic rebuild".to_string(),
        });
    }
    Ok(JDramaScenarioArchiveWriteOutcome {
        bytes: output,
        entry,
        inserted: true,
    })
}

fn validate_authored_runtime_carrier(areas: &[JDramaRecord], area_index: u8) -> Result<()> {
    let area = areas
        .get(usize::from(area_index))
        .ok_or_else(|| FormatError::Unsupported {
            format: FORMAT,
            message: format!("runtime carrier area {area_index} is missing"),
        })?;
    if semantic_type_name(&area.type_name) != "ScenarioArchiveNameTable" {
        return Err(FormatError::Unsupported {
            format: FORMAT,
            message: format!(
                "runtime carrier area {area_index} has unexpected type {:?}",
                area.type_name
            ),
        });
    }
    let JDramaRecordPayload::Group { children, .. } = &area.payload else {
        return Err(FormatError::Unsupported {
            format: FORMAT,
            message: format!("runtime carrier area {area_index} is not a JDrama group"),
        });
    };
    let Some(scenario_zero) = children.first() else {
        return Err(FormatError::Unsupported {
            format: FORMAT,
            message: format!("runtime carrier area {area_index} has no reserved scenario zero"),
        });
    };
    if scenario_archive_name(scenario_zero)
        .is_none_or(|name| !name.eq_ignore_ascii_case("none.arc"))
    {
        return Err(FormatError::Unsupported {
            format: FORMAT,
            message: format!(
                "runtime carrier area {area_index} scenario zero must remain none.arc"
            ),
        });
    }
    Ok(())
}

fn scenario_archive_name(record: &JDramaRecord) -> Option<&str> {
    if semantic_type_name(&record.type_name) != "ScenarioArchiveName" {
        return None;
    }
    let JDramaRecordPayload::Fields { fields } = &record.payload else {
        return None;
    };
    match fields.as_slice() {
        [JDramaField {
            value: JDramaFieldValue::String(archive_name),
            ..
        }] => Some(archive_name),
        _ => None,
    }
}

fn validate_authored_runtime_archive_name(archive_name: &str) -> Result<&str> {
    if archive_name.contains(['/', '\\', '\0']) {
        return Err(FormatError::Unsupported {
            format: FORMAT,
            message: format!("runtime archive must be a plain filename: {archive_name:?}"),
        });
    }
    let path = std::path::Path::new(archive_name);
    let extension = path.extension().and_then(|value| value.to_str());
    if !extension.is_some_and(|value| value.eq_ignore_ascii_case("arc")) {
        return Err(FormatError::Unsupported {
            format: FORMAT,
            message: format!("runtime archive must use the .arc extension: {archive_name:?}"),
        });
    }
    let stem = path
        .file_stem()
        .and_then(|value| value.to_str())
        .filter(|value| !value.is_empty())
        .ok_or_else(|| FormatError::Unsupported {
            format: FORMAT,
            message: format!("runtime archive has no filename stem: {archive_name:?}"),
        })?;
    if !stem
        .chars()
        .all(|character| character.is_ascii_alphanumeric() || matches!(character, '_' | '-'))
    {
        return Err(FormatError::Unsupported {
            format: FORMAT,
            message: format!(
                "runtime archive stem must contain only ASCII letters, digits, '_' or '-': {stem:?}"
            ),
        });
    }
    // Prove every emitted JDrama string can be represented before mutating
    // the semantic tree.
    encode_shift_jis(archive_name)?;
    Ok(stem)
}

fn runtime_archive_stem(archive_name: &str) -> Option<&str> {
    let file_name = archive_name
        .rsplit(['/', '\\'])
        .next()
        .filter(|name| !name.is_empty())?;
    let stem = file_name
        .rsplit_once('.')
        .map_or(file_name, |(stem, extension)| {
            if extension.eq_ignore_ascii_case("arc") || extension.eq_ignore_ascii_case("szs") {
                stem
            } else {
                file_name
            }
        });
    (!stem.is_empty()).then_some(stem)
}

fn collect_scenario_archive_table_paths(
    record: &JDramaRecord,
    path: &mut Vec<usize>,
    matches: &mut Vec<Vec<usize>>,
) {
    if semantic_type_name(&record.type_name) == "ScenarioArchiveNamesInStage" {
        matches.push(path.clone());
    }
    let JDramaRecordPayload::Group { children, .. } = &record.payload else {
        return;
    };
    for (index, child) in children.iter().enumerate() {
        path.push(index);
        collect_scenario_archive_table_paths(child, path, matches);
        path.pop();
    }
}

fn jdrama_record_at_path_mut<'a>(
    mut record: &'a mut JDramaRecord,
    path: &[usize],
) -> Option<&'a mut JDramaRecord> {
    for index in path {
        let JDramaRecordPayload::Group { children, .. } = &mut record.payload else {
            return None;
        };
        record = children.get_mut(*index)?;
    }
    Some(record)
}

fn jdrama_record_at_path<'a>(
    mut record: &'a JDramaRecord,
    path: &[usize],
) -> Option<&'a JDramaRecord> {
    for index in path {
        let JDramaRecordPayload::Group { children, .. } = &record.payload else {
            return None;
        };
        record = children.get(*index)?;
    }
    Some(record)
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
    name_ref_array_children_from_count(bytes, payload, end)
}

fn name_ref_array_children_from_count(
    bytes: &[u8],
    count_offset: usize,
    end: usize,
) -> Result<Vec<usize>> {
    let count = be_u32(bytes, count_offset, FORMAT)? as usize;
    if count > MAX_SCAN_RECORDS {
        return Err(FormatError::Unsupported {
            format: FORMAT,
            message: format!("NameRef array has implausible child count {count}"),
        });
    }
    let mut cursor = count_offset
        .checked_add(4)
        .ok_or_else(|| invalid_offset(count_offset, end))?;
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

fn record_child_offsets(
    bytes: &[u8],
    payload: usize,
    end: usize,
    type_name: &str,
) -> Result<Option<Vec<usize>>> {
    let short_type = type_name.rsplit("::").next().unwrap_or(type_name);
    let count_offset = match short_type {
        // Exact list/array layouts instantiated by JDrama::TNameRefGen and
        // TMarNameRefGen in the neighboring decomp.
        "GroupObj"
        | "NameRefGrp"
        | "Strategy"
        | "LightAry"
        | "AmbAry"
        | "EventTable"
        | "CameraMapToolTable"
        | "CubeGeneralInfoTable"
        | "StreamGeneralInfoTable"
        | "ScenarioArchiveNameTable"
        | "ScenarioArchiveNamesInStage"
        | "PositionHolder" => payload,
        // TIdxGroupObj::loadSuper appends its u32 group index before the
        // inherited TViewObjPtrListT child count.
        "IdxGroup" => payload
            .checked_add(4)
            .ok_or_else(|| invalid_offset(payload, end))?,
        // TSmJ3DScn::loadSuper appends a TLightMap before its inherited child
        // count. MarScene is the SMS factory alias for that same class.
        "SmJ3DScn" | "MarScene" => light_map_end(bytes, payload, end)?,
        _ => return Ok(None),
    };
    name_ref_array_children_from_count(bytes, count_offset, end).map(Some)
}

fn light_map_end(bytes: &[u8], start: usize, end: usize) -> Result<usize> {
    let count = be_u32(bytes, start, FORMAT)? as usize;
    let mut cursor = start
        .checked_add(4)
        .ok_or_else(|| invalid_offset(start, end))?;
    if count > end.saturating_sub(cursor) / 6 {
        return Err(invalid_offset(cursor, end));
    }
    for _ in 0..count {
        cursor = cursor
            .checked_add(4)
            .ok_or_else(|| invalid_offset(cursor, end))?;
        let (_, next) = read_len_string(bytes, cursor, end)?;
        cursor = next;
    }
    Ok(cursor)
}

fn parse_record_at(
    bytes: &[u8],
    offset: usize,
    limit: usize,
    record_path: &mut Vec<usize>,
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
    let (object_name, after_name) =
        read_name_ref(bytes, after_type, end).map(|(name, cursor)| (Some(name), cursor))?;
    let child_offsets = record_child_offsets(bytes, after_name, end, &type_name)?;
    let actor_layout = child_offsets
        .is_none()
        .then(|| read_actor_stream_layout(bytes, after_name, end))
        .flatten();
    let transform = actor_layout.as_ref().map(|layout| layout.transform);
    let stream_strings = actor_layout
        .as_ref()
        .map(|layout| {
            std::iter::once(layout.character_name.clone())
                .chain(layout.light_names.iter().cloned())
                .filter(|value| !value.is_empty())
                .collect()
        })
        .unwrap_or_default();
    let obj_chara_folder = read_obj_chara_folder(bytes, after_name, end, &type_name);
    let smpl_chara_archive_path = read_smpl_chara_archive_path(bytes, after_name, end, &type_name);
    let obj_manager_chara = read_obj_manager_chara(bytes, after_name, end, &type_name);
    let actor_tail_string = actor_layout
        .as_ref()
        .and_then(|layout| read_len_string(bytes, layout.tail_offset, end).ok())
        .map(|(value, _)| value)
        .filter(|value| !value.is_empty());
    let nozzle_box_item = actor_layout.as_ref().and_then(|layout| {
        read_nozzle_box_item_selector(bytes, layout.tail_offset, end, &type_name)
    });
    let live_actor_manager = explicit_live_actor_layout(&type_name)
        .then(|| actor_tail_string.clone())
        .flatten();
    let actor_character = actor_layout
        .as_ref()
        .map(|layout| layout.character_name.clone())
        .filter(|value| !value.is_empty());
    let mario_modoki_telesa_imitation_index = actor_layout.as_ref().and_then(|_| {
        read_mario_modoki_telesa_imitation_index(bytes, after_name + 36, end, &type_name)
    });
    let npc_params = actor_layout
        .as_ref()
        .and_then(|_| read_npc_params(bytes, after_name + 36, end, &type_name));
    let map_obj_grass_blade_count = actor_layout
        .as_ref()
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
    let raw_payload = bytes[after_name..end].to_vec();

    records.push(JDramaObjectRecord {
        record_path: record_path.clone(),
        offset,
        size,
        type_name,
        object_name,
        transform,
        stream_strings,
        obj_chara_folder,
        smpl_chara_archive_path,
        obj_manager_chara,
        live_actor_manager,
        actor_tail_string,
        nozzle_box_item,
        actor_character,
        mario_modoki_telesa_imitation_index,
        npc_params,
        map_obj_grass_blade_count,
        map_wire_manager,
        map_event_sink,
        cube_general_info,
        light,
        ambient,
        raw_payload,
    });

    if let Some(child_offsets) = child_offsets {
        for (child_index, child_offset) in child_offsets.into_iter().enumerate() {
            if records.len() >= MAX_SCAN_RECORDS {
                return Err(FormatError::Unsupported {
                    format: FORMAT,
                    message: format!("record tree exceeds the {MAX_SCAN_RECORDS}-record limit"),
                });
            }
            record_path.push(child_index);
            parse_record_at(bytes, child_offset, end, record_path, visited, records)?;
            record_path.pop();
        }
    }

    Ok(size)
}

fn read_nozzle_box_item_selector(
    bytes: &[u8],
    start: usize,
    end: usize,
    type_name: &str,
) -> Option<String> {
    // TNozzleBox::load calls TMapObjBase::load first. The retail NozzleBox
    // resource has no damage-height field, so its exact subclass tail is the
    // resource name, item selector, validity string, and two f32 values.
    if type_name.rsplit("::").next()? != "NozzleBox" {
        return None;
    }
    let (resource_name, cursor) = read_len_string(bytes, start, end).ok()?;
    if resource_name != "NozzleBox" {
        return None;
    }
    let (item, cursor) = read_len_string(bytes, cursor, end).ok()?;
    let (_, cursor) = read_len_string(bytes, cursor, end).ok()?;
    if cursor.checked_add(8)? != end {
        return None;
    }
    let _box_break_time = be_f32(bytes, cursor, FORMAT).ok()?;
    let _respawn_time = be_f32(bytes, cursor + 4, FORMAT).ok()?;
    (!item.is_empty()).then_some(item)
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

fn explicit_live_actor_layout(type_name: &str) -> bool {
    matches!(
        semantic_type_name(type_name),
        "LiveActor" | "MarioModokiTelesa"
    ) || is_npc_actor_type(type_name)
}

fn semantic_type_name(type_name: &str) -> &str {
    type_name.rsplit("::").next().unwrap_or(type_name)
}

fn is_npc_actor_type(type_name: &str) -> bool {
    // TMarNameRefGen's NPC factories use an exact, case-sensitive `NPC`
    // prefix and all construct TBaseNPC with the same placement stream.
    semantic_type_name(type_name).starts_with("NPC")
}

fn read_obj_chara_folder(
    bytes: &[u8],
    start: usize,
    end: usize,
    type_name: &str,
) -> Option<String> {
    (semantic_type_name(type_name) == "ObjChara")
        .then(|| {
            read_len_string(bytes, start, end)
                .ok()
                .map(|(value, _)| value)
        })
        .flatten()
}

fn read_smpl_chara_archive_path(
    bytes: &[u8],
    start: usize,
    end: usize,
    type_name: &str,
) -> Option<String> {
    (semantic_type_name(type_name) == "SmplChara")
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
    if semantic_type_name(type_name) != "CubeGeneralInfo" {
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
    if semantic_type_name(type_name) != "MapWireManager" {
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
    if semantic_type_name(type_name) != "MapObjGrassGroup" {
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
    #[derive(Clone, Copy)]
    enum BuildingLayout {
        Standard,
        ShadowMario,
    }

    // These are the exact sink factories registered by TMarNameRefGen and
    // getNameRef_MapObj in the neighboring decomp. TMapEventSinkShadowMario
    // appends an actor-name string to every building entry; the other
    // registered subclasses retain TMapEventSink's two-u32 entry layout.
    let layout = match semantic_type_name(type_name) {
        "MapEventSinkInPollution"
        | "MapEventSinkInPollutionReset"
        | "MapEventSirenaSink"
        | "MapEventSinkBianco"
        | "AirportEventSink" => BuildingLayout::Standard,
        "MapEventSinkShadowMario" => BuildingLayout::ShadowMario,
        _ => return None,
    };

    if start.checked_add(8)? > end {
        return None;
    }
    let building_count = be_u32(bytes, start, FORMAT).ok()? as usize;
    let first_building = be_u32(bytes, start.checked_add(4)?, FORMAT).ok()? as usize;
    if building_count == 0 || building_count > 64 || first_building > u16::MAX as usize {
        return None;
    }
    let mut cursor = start.checked_add(8)?;
    let mut buildings = Vec::with_capacity(building_count);
    for index in 0..building_count {
        let next = cursor.checked_add(8)?;
        if next > end {
            return None;
        }
        buildings.push(JDramaMapEventBuilding {
            building_index: u16::try_from(first_building.checked_add(index)?).ok()?,
            pollution_layer_index: u16::try_from(be_u32(bytes, cursor, FORMAT).ok()?).ok()?,
            pollution_object_index: u16::try_from(be_u32(bytes, cursor + 4, FORMAT).ok()?).ok()?,
        });
        cursor = next;
        if matches!(layout, BuildingLayout::ShadowMario) {
            let (_, next) = read_len_string(bytes, cursor, end).ok()?;
            cursor = next;
        }
    }
    Some(JDramaMapEventSinkParams { buildings })
}

fn read_npc_params(
    bytes: &[u8],
    start: usize,
    end: usize,
    type_name: &str,
) -> Option<JDramaNpcParams> {
    if !is_npc_actor_type(type_name) {
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

fn read_actor_stream_layout(
    bytes: &[u8],
    offset: usize,
    limit: usize,
) -> Option<ActorStreamLayout> {
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
    let finite = transform
        .translation
        .into_iter()
        .chain(transform.rotation)
        .chain(transform.scale)
        .all(f32::is_finite);
    if !finite {
        return None;
    }

    // JDrama::TActor::load always follows its nine floats with a TCharacter
    // name and an exact TLightMap stream. Requiring both base-class fields
    // prevents arbitrary plausible float payloads from becoming transforms.
    let (character_name, mut cursor) = read_len_string(bytes, offset + 36, limit).ok()?;
    let light_count = be_u32(bytes, cursor, FORMAT).ok()? as usize;
    cursor = cursor.checked_add(4)?;
    if light_count > limit.checked_sub(cursor)? / 6 {
        return None;
    }
    let mut light_names = Vec::with_capacity(light_count);
    for _ in 0..light_count {
        cursor = cursor.checked_add(4)?;
        let (name, next) = read_len_string(bytes, cursor, limit).ok()?;
        light_names.push(name);
        cursor = next;
    }
    Some(ActorStreamLayout {
        transform,
        character_name,
        light_names,
        tail_offset: cursor,
    })
}

fn read_vec3(bytes: &[u8], offset: usize) -> Option<[f32; 3]> {
    Some([
        be_f32(bytes, offset, FORMAT).ok()?,
        be_f32(bytes, offset + 4, FORMAT).ok()?,
        be_f32(bytes, offset + 8, FORMAT).ok()?,
    ])
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
        bytes.extend_from_slice(&jdrama_key_code(type_name).unwrap().to_be_bytes());
        put_len_string(&mut bytes, &encode_shift_jis(type_name).unwrap());
        bytes.extend_from_slice(&jdrama_key_code(name).unwrap().to_be_bytes());
        put_len_string(&mut bytes, &encode_shift_jis(name).unwrap());
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
    fn strict_document_rebuilds_nested_actor_tree_without_source_bytes() {
        let mut actor_payload = Vec::new();
        for value in [1.0_f32, 2.0, 3.0, 0.0, 45.0, 0.0, 1.0, 2.0, 3.0] {
            actor_payload.extend_from_slice(&value.to_be_bytes());
        }
        put_len_string(&mut actor_payload, &encode_shift_jis("旗 キャラ").unwrap());
        actor_payload.extend_from_slice(&1_u32.to_be_bytes());
        actor_payload.extend_from_slice(&3_u32.to_be_bytes());
        put_len_string(&mut actor_payload, b"object light");
        put_len_string(&mut actor_payload, b"flagWhite");
        let actor = name_ref_record("MapObjFlag", "旗 0", &actor_payload);

        let mut light_payload = Vec::new();
        for value in [200_000.0_f32, 500_000.0, 200_000.0] {
            light_payload.extend_from_slice(&value.to_be_bytes());
        }
        light_payload.extend_from_slice(&[210, 150, 230, 255]);
        light_payload.extend_from_slice(&50.0_f32.to_be_bytes());
        let light = name_ref_record("Light", "object sun", &light_payload);
        let bytes = name_ref_array("GroupObj", "全体シーン", &[actor, light]);

        let document = parse_jdrama_document(&bytes).expect("strict semantic parse");
        assert_eq!(document.to_bytes().expect("semantic rebuild"), bytes);

        let JDramaRecordPayload::Group { children, .. } = &document.root.payload else {
            panic!("root should preserve hierarchy");
        };
        assert_eq!(children.len(), 2);
        let JDramaRecordPayload::Actor {
            transform,
            character_name,
            light_map,
            fields,
        } = &children[0].payload
        else {
            panic!("first child should be a typed actor");
        };
        assert_eq!(transform.translation, [1.0, 2.0, 3.0]);
        assert_eq!(character_name, "旗 キャラ");
        assert_eq!(light_map.entries[0].channel, 3);
        assert_eq!(fields[0].name, "resource_name");
        assert_eq!(
            fields[0].value,
            JDramaFieldValue::String("flagWhite".to_string())
        );
        let legacy_records = parse_jdrama_object_records(&bytes).unwrap();
        assert_eq!(legacy_records[0].record_path, Vec::<usize>::new());
        assert_eq!(legacy_records[1].record_path, [0]);
        assert_eq!(legacy_records[2].record_path, [1]);
    }

    #[test]
    fn strict_writer_derives_key_codes_and_recomputes_sizes() {
        let bytes = name_ref_record("AmbColor", "ambient", &[1, 2, 3, 4]);
        let mut document = parse_jdrama_document(&bytes).expect("parse ambient");
        document.root.name.push_str(" extended");

        let rebuilt = document.to_bytes().expect("rebuild ambient");
        assert_eq!(be_u32(&rebuilt, 0, FORMAT).unwrap() as usize, rebuilt.len());
        assert_eq!(
            be_u16(&rebuilt, 4, FORMAT).unwrap(),
            jdrama_key_code("AmbColor").unwrap()
        );
        let (_, after_type) = read_len_string_strict(&rebuilt, 6, rebuilt.len()).unwrap();
        assert_eq!(
            be_u16(&rebuilt, after_type, FORMAT).unwrap(),
            jdrama_key_code("ambient extended").unwrap()
        );
    }

    #[test]
    fn strict_parser_rejects_nonsemantic_key_codes() {
        let mut bytes = name_ref_record("AmbColor", "ambient", &[1, 2, 3, 4]);
        bytes[4..6].copy_from_slice(&0x1234_u16.to_be_bytes());
        let error = parse_jdrama_document(&bytes).unwrap_err().to_string();
        assert!(error.contains("does not match derived key"), "{error}");
    }

    #[test]
    fn strict_parser_rejects_unmodeled_payload_instead_of_caching_it() {
        let bytes = name_ref_record("Opaque", "object", &[1, 2, 3, 4]);
        let error = parse_jdrama_document(&bytes).unwrap_err().to_string();
        assert!(error.contains("no typed payload for Opaque"), "{error}");
    }

    #[test]
    fn key_code_uses_encoded_shift_jis_bytes() {
        let encoded = encode_shift_jis("全体シーン").unwrap();
        let expected = encoded.iter().fold(0_u32, |key, byte| {
            key.wrapping_mul(3).wrapping_add(*byte as u32)
        }) as u16;
        assert_eq!(jdrama_key_code("全体シーン").unwrap(), expected);
    }

    #[test]
    #[ignore = "requires SMS_BASE_ROOT and complete strict class coverage"]
    fn strict_retail_scene_bin_census_is_byte_identical() {
        let base_root = std::env::var_os("SMS_BASE_ROOT")
            .map(std::path::PathBuf::from)
            .expect("set SMS_BASE_ROOT to the extracted game's root");
        let archives = crate::discover_scene_archives(&base_root).expect("discover stage archives");
        let mut checked = 0_usize;
        let mut rebuilt = 0_usize;
        let mut rebuilt_stages = Vec::new();
        let mut failures = Vec::new();
        for archive in archives {
            let Ok(bytes) = crate::extract_archive_file(&archive.path, "map/scene.bin") else {
                continue;
            };
            checked += 1;
            match parse_jdrama_document(&bytes).and_then(|document| document.to_bytes()) {
                Ok(output) if output == bytes => {
                    rebuilt += 1;
                    rebuilt_stages.push(archive.stage_id.clone());
                }
                Ok(output) => failures.push(format!(
                    "{}: rebuilt {} bytes instead of {}",
                    archive.stage_id,
                    output.len(),
                    bytes.len()
                )),
                Err(error) => failures.push(format!("{}: {error}", archive.stage_id)),
            }
        }
        assert_eq!(checked, 107, "retail scene.bin count drifted");
        assert_eq!(
            rebuilt,
            checked,
            "strict scene.bin census rebuilt {rebuilt}/{checked} ({rebuilt_stages:?}); first failures: {:#?}",
            &failures[..failures.len().min(20)]
        );
    }

    #[test]
    #[ignore = "requires SMS_BASE_ROOT and complete strict class coverage"]
    fn strict_retail_all_jdrama_bin_census_is_byte_identical() {
        let base_root = std::env::var_os("SMS_BASE_ROOT")
            .map(std::path::PathBuf::from)
            .expect("set SMS_BASE_ROOT to the extracted game's root");
        let archives = crate::discover_scene_archives(&base_root).expect("discover stage archives");
        let mut checked = 0_usize;
        let mut rebuilt = 0_usize;
        let mut failures = Vec::new();
        for archive_info in archives {
            let source = std::fs::read(&archive_info.path)
                .unwrap_or_else(|error| panic!("read {}: {error}", archive_info.path.display()));
            let decoded = if source.starts_with(b"Yaz0") {
                crate::decode_yaz0(&source).unwrap_or_else(|error| {
                    panic!("decode {}: {error}", archive_info.path.display())
                })
            } else {
                source
            };
            let archive = crate::RarcArchive::parse(decoded)
                .unwrap_or_else(|error| panic!("parse {}: {error}", archive_info.path.display()));
            for entry in archive.file_entries() {
                let lower_path = entry.path.to_ascii_lowercase();
                if !(lower_path.ends_with("/scene.bin") || lower_path.ends_with("/tables.bin")) {
                    continue;
                }
                checked += 1;
                let bytes = archive
                    .file_bytes_raw(&entry.raw_path)
                    .unwrap_or_else(|error| {
                        panic!(
                            "read {} in {}: {error}",
                            entry.path,
                            archive_info.path.display()
                        )
                    });
                match parse_jdrama_document(&bytes).and_then(|document| document.to_bytes()) {
                    Ok(output) if output == bytes => rebuilt += 1,
                    Ok(output) => failures.push(format!(
                        "{}!/{}: rebuilt {} bytes instead of {}",
                        archive_info.stage_id,
                        entry.path,
                        output.len(),
                        bytes.len()
                    )),
                    Err(error) => failures.push(format!(
                        "{}!/{}: {error}",
                        archive_info.stage_id, entry.path
                    )),
                }
            }
        }
        assert_eq!(checked, 214, "retail JDrama scene/tables count drifted");
        assert_eq!(
            rebuilt,
            checked,
            "strict JDrama-bin census rebuilt {rebuilt}/{checked}; first failures: {:#?}",
            &failures[..failures.len().min(30)]
        );
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

    fn authored_runtime_table(first_carrier_custom_scenarios: usize) -> Vec<u8> {
        let leaf = |name: String, archive: String| {
            let mut payload = Vec::new();
            put_len_string(&mut payload, archive.as_bytes());
            name_ref_record("ScenarioArchiveName", &name, &payload)
        };
        let areas = (0..55)
            .map(|area_index| {
                let is_carrier =
                    SMS_AUTHORED_RUNTIME_CARRIER_AREAS.contains(&u8::try_from(area_index).unwrap());
                let mut scenarios = vec![leaf(
                    format!("scenario {area_index} 0"),
                    if is_carrier {
                        "none.arc".to_string()
                    } else {
                        format!("retail{area_index}.arc")
                    },
                )];
                if area_index == usize::from(SMS_AUTHORED_RUNTIME_CARRIER_AREAS[0]) {
                    scenarios.extend((1..=first_carrier_custom_scenarios).map(|scenario_index| {
                        leaf(
                            format!("authored {scenario_index}"),
                            format!("authored{scenario_index}.arc"),
                        )
                    }));
                }
                name_ref_array(
                    "ScenarioArchiveNameTable",
                    &format!("area {area_index}"),
                    &scenarios,
                )
            })
            .collect::<Vec<_>>();
        let outer = name_ref_array("ScenarioArchiveNamesInStage", "runtime stages", &areas);
        name_ref_array("NameRefGrp", "root", &[outer])
    }

    #[test]
    fn authors_a_new_reserved_carrier_scenario_without_replacing_retail_slots() {
        let source = authored_runtime_table(0);
        let before = parse_jdrama_scenario_archive_areas(&source).unwrap();

        let authored = append_jdrama_scenario_archive_slot(&source, "myStage.arc").unwrap();
        assert!(authored.inserted);
        assert_eq!(authored.entry.area_index, 17);
        assert_eq!(authored.entry.scenario_index, 1);
        assert_eq!(authored.entry.archive_name, "myStage.arc");
        let after = parse_jdrama_scenario_archive_areas(&authored.bytes).unwrap();
        assert_eq!(after.len(), before.len(), "outer area count must not grow");
        for area_index in 0..after.len() {
            if area_index == 17 {
                assert_eq!(after[area_index].archive_names[0], "none.arc");
                assert_eq!(after[area_index].archive_names[1], "myStage.arc");
            } else {
                assert_eq!(after[area_index], before[area_index]);
            }
        }
    }

    #[test]
    fn ensuring_an_authored_runtime_slot_is_byte_idempotent() {
        let source = authored_runtime_table(0);
        let authored = append_jdrama_scenario_archive_slot(&source, "existing.arc").unwrap();

        let ensured = ensure_jdrama_scenario_archive_slot(&authored.bytes, "EXISTING.ARC").unwrap();
        assert!(!ensured.inserted);
        assert_eq!(ensured.bytes, authored.bytes);
        assert_eq!(ensured.entry.area_index, 17);
        assert_eq!(ensured.entry.scenario_index, 1);
    }

    #[test]
    fn appending_rejects_duplicate_runtime_archive_stems() {
        let source = authored_runtime_table(0);

        let error = append_jdrama_scenario_archive_slot(&source, "Retail0.arc")
            .unwrap_err()
            .to_string();
        assert!(error.contains("already mapped"), "{error}");
        let ensure_error = ensure_jdrama_scenario_archive_slot(&source, "Retail0.arc")
            .unwrap_err()
            .to_string();
        assert!(ensure_error.contains("already mapped"), "{ensure_error}");
    }

    #[test]
    fn authored_slot_allocation_rejects_an_incompatible_carrier_baseline() {
        let source = authored_runtime_table(0);
        let mut document = JDramaDocument::parse(&source).unwrap();
        let mut paths = Vec::new();
        collect_scenario_archive_table_paths(&document.root, &mut Vec::new(), &mut paths);
        let table = jdrama_record_at_path_mut(&mut document.root, &paths[0]).unwrap();
        let JDramaRecordPayload::Group {
            children: areas, ..
        } = &mut table.payload
        else {
            unreachable!()
        };
        let JDramaRecordPayload::Group {
            children: scenarios,
            ..
        } = &mut areas[17].payload
        else {
            unreachable!()
        };
        let JDramaRecordPayload::Fields { fields } = &mut scenarios[0].payload else {
            unreachable!()
        };
        fields[0].value = JDramaFieldValue::String("occupied.arc".to_string());
        let incompatible = document.to_bytes().unwrap();

        let error = append_jdrama_scenario_archive_slot(&incompatible, "newStage.arc")
            .unwrap_err()
            .to_string();

        assert!(
            error.contains("scenario zero must remain none.arc"),
            "{error}"
        );
    }

    #[test]
    fn full_reserved_carrier_spills_into_the_next_safe_area() {
        let source = authored_runtime_table(255);

        let authored = append_jdrama_scenario_archive_slot(&source, "spill.arc").unwrap();

        assert_eq!(authored.entry.area_index, 18);
        assert_eq!(authored.entry.scenario_index, 1);
        assert_eq!(
            parse_jdrama_scenario_archive_areas(&authored.bytes).unwrap()[18].archive_names,
            ["none.arc", "spill.arc"]
        );
    }

    #[test]
    #[ignore = "requires SMS_BASE_ROOT with extracted retail data/stageArc.bin"]
    fn retail_stage_arc_authors_a_source_free_reserved_scenario() {
        let base_root = std::env::var_os("SMS_BASE_ROOT")
            .map(std::path::PathBuf::from)
            .expect("set SMS_BASE_ROOT to the extracted game's root");
        let path = base_root.join("files/data/stageArc.bin");
        let source = std::fs::read(&path).expect("read retail stageArc.bin");
        let document = JDramaDocument::parse(&source).expect("typed stageArc.bin parse");
        assert_eq!(
            document.to_bytes().expect("typed stageArc.bin rebuild"),
            source,
            "retail stageArc.bin must be source-free byte exact before editing"
        );
        let before = parse_jdrama_scenario_archive_entries(&source).unwrap();
        let areas = parse_jdrama_scenario_archive_areas(&source).unwrap();
        assert_eq!(areas.len(), 61);
        for area_index in SMS_AUTHORED_RUNTIME_CARRIER_AREAS {
            assert_eq!(areas[usize::from(area_index)].archive_names, ["none.arc"]);
        }

        let authored =
            append_jdrama_scenario_archive_slot(&source, "smsEditorRuntimeTest.arc").unwrap();
        let after = parse_jdrama_scenario_archive_entries(&authored.bytes).unwrap();
        assert_eq!(after.len(), before.len() + 1);
        for entry in before {
            assert!(after.contains(&entry), "retail mapping changed: {entry:?}");
        }
        assert!(after.contains(&authored.entry));
        assert_eq!(authored.entry.area_index, 17);
        assert_eq!(authored.entry.scenario_index, 1);

        let ensured =
            ensure_jdrama_scenario_archive_slot(&authored.bytes, "smsEditorRuntimeTest.arc")
                .unwrap();
        assert!(!ensured.inserted);
        assert_eq!(ensured.bytes, authored.bytes);
    }

    #[test]
    fn actor_transform_requires_the_complete_tactor_base_stream() {
        let mut payload = Vec::new();
        for value in [1.0_f32, 2.0, 3.0, 0.0, 45.0, 0.0, 1.0, 1.0, 1.0] {
            payload.extend_from_slice(&value.to_be_bytes());
        }
        put_len_string(&mut payload, b"character");
        payload.extend_from_slice(&0u32.to_be_bytes());
        let bytes = name_ref_record("SmJ3DAct", "actor", &payload);

        let records = parse_jdrama_object_records(&bytes).unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].transform.unwrap().translation, [1.0, 2.0, 3.0]);
        assert_eq!(records[0].stream_strings, ["character"]);
        assert_eq!(records[0].raw_payload, payload);
    }

    #[test]
    fn plausible_floats_without_tactor_fields_do_not_become_a_transform() {
        let mut payload = Vec::new();
        for value in [1.0_f32, 2.0, 3.0, 0.0, 45.0, 0.0, 1.0, 1.0, 1.0] {
            payload.extend_from_slice(&value.to_be_bytes());
        }
        let bytes = name_ref_record("Opaque", "opaque", &payload);

        let records = parse_jdrama_object_records(&bytes).unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].transform, None);
        assert_eq!(records[0].raw_payload, payload);
    }

    #[test]
    fn unknown_payload_is_not_scanned_for_record_shaped_children() {
        let embedded = name_ref_record("Leaf", "embedded", &[]);
        let bytes = name_ref_record("Opaque", "root", &embedded);

        let records = parse_jdrama_object_records(&bytes).unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].raw_payload, embedded);
    }

    #[test]
    fn known_group_layout_parses_only_its_counted_children() {
        let child = name_ref_record("Leaf", "child", &[]);
        let bytes = name_ref_array("GroupObj", "root", &[child]);

        let records = parse_jdrama_object_records(&bytes).unwrap();
        assert_eq!(records.len(), 2);
        assert_eq!(records[1].type_name, "Leaf");
        assert_eq!(records[1].object_name.as_deref(), Some("child"));
    }

    #[test]
    fn malformed_known_group_layout_is_rejected() {
        let bytes = name_ref_record("GroupObj", "root", &1u32.to_be_bytes());
        assert!(parse_jdrama_object_records(&bytes).is_err());
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
        assert!(read_npc_params(&bytes, 0, bytes.len(), "npcMonteMA").is_none());
    }

    #[test]
    fn npc_parts_mask_clamps_negative_retail_sentinel_before_bit_tests() {
        assert_eq!(effective_npc_parts_mask(-1), 0);
        assert_eq!(effective_npc_parts_mask(i32::MIN), 0);
        assert_eq!(effective_npc_parts_mask(0), 0);
        assert_eq!(effective_npc_parts_mask(1), 1);
        assert_eq!(effective_npc_parts_mask(i32::MAX), i32::MAX as u32);
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
        assert_eq!(
            read_map_obj_grass_blade_count(&bytes, 0, bytes.len(), "mapObjGrassGroup"),
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
        assert!(read_map_wire_manager_params(&bytes, 0, bytes.len(), "mapWireManager").is_none());
    }

    #[test]
    fn reads_character_resources_and_manager_reference_from_their_load_streams() {
        let mut chara_bytes = Vec::new();
        put_len_string(&mut chara_bytes, b"/scene/hamukuri");
        assert_eq!(
            read_obj_chara_folder(&chara_bytes, 0, chara_bytes.len(), "ObjChara").as_deref(),
            Some("/scene/hamukuri")
        );
        assert!(
            read_obj_chara_folder(&chara_bytes, 0, chara_bytes.len(), "HamuKuriManager").is_none()
        );
        assert!(read_obj_chara_folder(&chara_bytes, 0, chara_bytes.len(), "objChara").is_none());

        let mut simple_chara_bytes = Vec::new();
        put_len_string(&mut simple_chara_bytes, b"/scene/map/map.arc");
        assert_eq!(
            read_smpl_chara_archive_path(
                &simple_chara_bytes,
                0,
                simple_chara_bytes.len(),
                "JDrama::SmplChara",
            )
            .as_deref(),
            Some("/scene/map/map.arc")
        );
        assert!(read_smpl_chara_archive_path(
            &simple_chara_bytes,
            0,
            simple_chara_bytes.len(),
            "ObjChara"
        )
        .is_none());

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
            read_live_actor_manager_with_cursor(&bytes, 0, bytes.len())
                .map(|(manager, _)| manager)
                .as_deref(),
            Some("HamuKuri Manager")
        );
    }

    #[test]
    fn live_actor_manager_tail_requires_exact_semantic_type_case() {
        let actor_payload = |type_name: &str| {
            let mut payload = Vec::new();
            for value in [0.0_f32, 0.0, 0.0, 0.0, 0.0, 0.0, 1.0, 1.0, 1.0] {
                payload.extend_from_slice(&value.to_be_bytes());
            }
            put_len_string(&mut payload, b"Enemy Character");
            payload.extend_from_slice(&0_u32.to_be_bytes());
            put_len_string(&mut payload, b"Enemy Manager");
            name_ref_record(type_name, "actor", &payload)
        };

        let exact = parse_jdrama_object_records(&actor_payload("LiveActor")).unwrap();
        assert_eq!(
            exact[0].live_actor_manager.as_deref(),
            Some("Enemy Manager")
        );
        assert_eq!(exact[0].actor_tail_string.as_deref(), Some("Enemy Manager"));

        let wrong_case = parse_jdrama_object_records(&actor_payload("liveActor")).unwrap();
        assert_eq!(wrong_case[0].live_actor_manager, None);
        assert_eq!(
            wrong_case[0].actor_tail_string.as_deref(),
            Some("Enemy Manager")
        );
        assert!(wrong_case[0].transform.is_some());
    }

    #[test]
    fn reads_nozzle_box_item_from_its_exact_subclass_tail() {
        let nozzle_payload = |type_name: &str, trailing_byte: bool| {
            let mut payload = Vec::new();
            for value in [0.0_f32, 0.0, 0.0, 0.0, 0.0, 0.0, 1.0, 1.0, 1.0] {
                payload.extend_from_slice(&value.to_be_bytes());
            }
            put_len_string(&mut payload, b"NozzleBox Character");
            payload.extend_from_slice(&0_u32.to_be_bytes());
            put_len_string(&mut payload, b"NozzleBox");
            put_len_string(&mut payload, b"rocket_nozzle_item");
            put_len_string(&mut payload, b"valid");
            payload.extend_from_slice(&50.0_f32.to_be_bytes());
            payload.extend_from_slice(&20.0_f32.to_be_bytes());
            if trailing_byte {
                payload.push(0);
            }
            (
                payload.clone(),
                name_ref_record(type_name, "nozzle box", &payload),
            )
        };

        let (payload, bytes) = nozzle_payload("NozzleBox", false);
        let exact = parse_jdrama_object_records(&bytes).unwrap();
        assert_eq!(exact[0].actor_tail_string.as_deref(), Some("NozzleBox"));
        assert_eq!(
            exact[0].nozzle_box_item.as_deref(),
            Some("rocket_nozzle_item")
        );
        assert_eq!(exact[0].raw_payload, payload);

        let (_, wrong_case) = nozzle_payload("nozzleBox", false);
        assert_eq!(
            parse_jdrama_object_records(&wrong_case).unwrap()[0].nozzle_box_item,
            None
        );

        let (_, malformed) = nozzle_payload("NozzleBox", true);
        let malformed = parse_jdrama_object_records(&malformed).unwrap();
        assert_eq!(malformed[0].nozzle_box_item, None);
        assert_eq!(malformed[0].actor_tail_string.as_deref(), Some("NozzleBox"));
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
            read_live_actor_manager_with_cursor(&bytes, 0, bytes.len())
                .map(|(manager, _)| manager)
                .as_deref(),
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
        assert!(read_cube_general_info(&bytes, 0, bytes.len(), "cubeGeneralInfo").is_none());
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
        for type_name in [
            "MapEventSinkInPollution",
            "MapEventSinkInPollutionReset",
            "MapEventSirenaSink",
            "MapEventSinkBianco",
            "AirportEventSink",
        ] {
            assert!(
                read_map_event_sink_params(&bytes, 0, bytes.len(), type_name).is_some(),
                "registered standard sink {type_name}"
            );
        }
        assert!(read_map_event_sink_params(&bytes, 0, bytes.len(), "MapObjBase").is_none());
        assert!(read_map_event_sink_params(&bytes, 0, bytes.len(), "mapEventSinkBianco").is_none());
        assert!(read_map_event_sink_params(&bytes, 0, bytes.len(), "UnrelatedEventSink").is_none());
    }

    #[test]
    fn reads_shadow_mario_event_sink_variable_building_entries() {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&2_u32.to_be_bytes());
        bytes.extend_from_slice(&4_u32.to_be_bytes());
        bytes.extend_from_slice(&7_u32.to_be_bytes());
        bytes.extend_from_slice(&8_u32.to_be_bytes());
        put_len_string(&mut bytes, b"shadow actor 1");
        bytes.extend_from_slice(&9_u32.to_be_bytes());
        bytes.extend_from_slice(&10_u32.to_be_bytes());
        put_len_string(&mut bytes, b"shadow actor 2");

        let params =
            read_map_event_sink_params(&bytes, 0, bytes.len(), "MapEventSinkShadowMario").unwrap();
        assert_eq!(
            params.buildings,
            [
                JDramaMapEventBuilding {
                    building_index: 4,
                    pollution_layer_index: 7,
                    pollution_object_index: 8,
                },
                JDramaMapEventBuilding {
                    building_index: 5,
                    pollution_layer_index: 9,
                    pollution_object_index: 10,
                },
            ]
        );
    }

    fn put_len_string(bytes: &mut Vec<u8>, value: &[u8]) {
        bytes.extend_from_slice(&(value.len() as u16).to_be_bytes());
        bytes.extend_from_slice(value);
    }

    #[test]
    fn parses_us_only_actor_tails() {
        for (actor_type, resource_name) in [
            ("WatermelonStatic", "WatermelonStatic"),
            ("WaterMoveBlock", "water_roll_block"),
            ("IceCar", "ice_car"),
            ("Football", "football"),
        ] {
            let mut bytes = Vec::new();
            put_len_string(&mut bytes, resource_name.as_bytes());
            let len = bytes.len();
            let mut cursor = StrictCursor::new(&bytes, 0, len);
            let fields = parse_actor_tail(&mut cursor, actor_type).unwrap();
            assert!(cursor.is_done(), "{actor_type}");
            assert_eq!(
                fields,
                [field(
                    "resource_name",
                    JDramaFieldValue::String(resource_name.to_string())
                )],
                "{actor_type}"
            );
        }

        let mut breakable = Vec::new();
        put_len_string(&mut breakable, b"breakable_block");
        breakable.extend_from_slice(&(-1_i32).to_be_bytes());
        breakable.extend_from_slice(&100.0_f32.to_be_bytes());
        breakable.extend_from_slice(&(-1.0_f32).to_be_bytes());
        breakable.extend_from_slice(&120_i32.to_be_bytes());
        let breakable_len = breakable.len();
        let mut cursor = StrictCursor::new(&breakable, 0, breakable_len);
        let fields = parse_actor_tail(&mut cursor, "BreakableBlock").unwrap();
        assert!(cursor.is_done());
        assert_eq!(fields[0].name, "resource_name");
        assert_eq!(fields[1].value, JDramaFieldValue::I32(-1));
        assert_eq!(fields[2].value, JDramaFieldValue::F32(100.0));
        assert_eq!(fields[3].value, JDramaFieldValue::F32(-1.0));
        assert_eq!(fields[4].value, JDramaFieldValue::I32(120));

        let mut hinokuri = Vec::new();
        put_len_string(&mut hinokuri, b"hinokuri_manager");
        put_len_string(&mut hinokuri, b"hinokuri");
        let hinokuri_len = hinokuri.len();
        let mut cursor = StrictCursor::new(&hinokuri, 0, hinokuri_len);
        let fields = parse_actor_tail(&mut cursor, "HinoKuri2").unwrap();
        assert!(cursor.is_done());
        assert_eq!(fields[0].name, "manager_name");
        assert_eq!(fields[1].name, "graph_name");
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
