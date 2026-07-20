//! Typed, lossless editing of JDrama object parameters.

use std::collections::{BTreeMap, BTreeSet};

use sms_formats::{
    jdrama_key_code, JDramaField, JDramaFieldValue, JDramaRecord, JDramaRecordPayload,
};

use crate::{
    jdrama_record_at, PlacementBinding, Result, SceneError, SceneObject, SceneParameter,
    StageDocument, StageResourceDocument,
};

pub const OBJECT_PARAMETER_NAME: &str = "name";
pub const OBJECT_PARAMETER_CHARACTER_NAME: &str = "character_name";

/// The exact scalar/vector type stored in a strict JDrama record.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ObjectParameterKind {
    U32,
    I32,
    F32,
    Vec2F32,
    Vec3F32,
    ColorRgba8,
    String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObjectParameterChoice {
    pub raw_value: String,
    pub label: String,
    pub description: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObjectParameterIndexedChoice {
    pub label: String,
    pub index_label: String,
    pub description: String,
    pub default_index: i64,
    pub index_range: [i64; 2],
    pub retail_index_range: Option<[i64; 2]>,
    pub reserved_indices: Vec<i64>,
}

impl ObjectParameterIndexedChoice {
    pub fn accepts_index(&self, index: i64) -> bool {
        (self.index_range[0]..=self.index_range[1]).contains(&index)
            && !self.reserved_indices.contains(&index)
    }

    pub fn is_retail_index(&self, index: i64) -> bool {
        self.retail_index_range
            .is_none_or(|range| (range[0]..=range[1]).contains(&index))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObjectParameterInfo {
    /// A human-readable inspector label for a canonical serialized field.
    pub display_name: Option<String>,
    pub description: String,
    pub choices: Vec<ObjectParameterChoice>,
    /// One semantic choice whose serialized value is a user-authored integer index.
    pub indexed_choice: Option<ObjectParameterIndexedChoice>,
    pub integer_range: Option<[i64; 2]>,
}

/// One typed JDrama value in serialization order, including linked read-only fields.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EditableSceneParameter {
    pub key: String,
    pub kind: ObjectParameterKind,
    pub raw_value: String,
    /// Why this canonical field cannot be edited without rebuilding its
    /// authored runtime dependency closure.
    pub read_only_reason: Option<String>,
    /// Runtime behavior and constrained values derived from the owning class.
    pub info: Option<ObjectParameterInfo>,
}

/// Selects whether only in-memory edits or every stored canonical value is applied.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParameterApplyMode {
    DirtyOnly,
    AllCanonical,
}

/// Describes the editable, typed values in their exact JDrama stream order.
pub fn editable_object_parameters(record: &JDramaRecord) -> Result<Vec<EditableSceneParameter>> {
    let mut descriptors = Vec::new();
    let mut keys = BTreeSet::new();
    push_descriptor(
        &mut descriptors,
        &mut keys,
        &record.type_name,
        OBJECT_PARAMETER_NAME,
        ObjectParameterKind::String,
        record.name.clone(),
    )?;

    if let JDramaRecordPayload::Actor {
        character_name,
        fields,
        ..
    } = &record.payload
    {
        push_descriptor(
            &mut descriptors,
            &mut keys,
            &record.type_name,
            OBJECT_PARAMETER_CHARACTER_NAME,
            ObjectParameterKind::String,
            character_name.clone(),
        )?;
        append_field_descriptors(record, fields, &mut descriptors, &mut keys)?;
    } else if let Some(fields) = record_fields(record) {
        append_field_descriptors(record, fields, &mut descriptors, &mut keys)?;
    }

    Ok(descriptors)
}

/// Resolves an object's bound typed record and overlays any current canonical edits.
pub fn editable_parameters_for_object(
    document: &StageDocument,
    object: &SceneObject,
) -> Result<Vec<EditableSceneParameter>> {
    document.editable_parameters_for_object(object)
}

impl StageDocument {
    pub fn editable_parameters_for_object(
        &self,
        object: &SceneObject,
    ) -> Result<Vec<EditableSceneParameter>> {
        let placement = object.placement.as_ref().ok_or_else(|| {
            parameter_error(format!(
                "object '{}' has no typed placement binding",
                object.id
            ))
        })?;
        let record = match placement {
            PlacementBinding::Authored(authored) => &authored.prototype,
            PlacementBinding::Existing(address) | PlacementBinding::CloneOf(address) => {
                let archive = self.stage_archive.as_ref().ok_or_else(|| {
                    parameter_error(format!(
                        "object '{}' requires a detached stage archive",
                        object.id
                    ))
                })?;
                let resource = archive
                    .resource(&address.raw_resource_path)
                    .ok_or_else(|| {
                        parameter_error(format!(
                            "object '{}' references missing placement resource '{}'",
                            object.id,
                            String::from_utf8_lossy(&address.raw_resource_path)
                        ))
                    })?;
                let StageResourceDocument::Placement(placement_document) = resource else {
                    return Err(parameter_error(format!(
                        "object '{}' references a non-placement resource '{}'",
                        object.id,
                        String::from_utf8_lossy(&address.raw_resource_path)
                    )));
                };
                jdrama_record_at(&placement_document.root, &address.record_path).ok_or_else(
                    || {
                        parameter_error(format!(
                            "object '{}' has an invalid placement record path {}",
                            object.id,
                            display_record_path(&address.record_path)
                        ))
                    },
                )?
            }
        };

        let mut descriptors = editable_object_parameters(record)?;
        for descriptor in &mut descriptors {
            if let Some(parameter) = object.raw_params.get(&descriptor.key) {
                descriptor.raw_value = parameter.raw().to_string();
            }
            descriptor.read_only_reason =
                parameter_read_only_reason(Some(placement), &descriptor.key, &descriptor.raw_value);
        }
        Ok(descriptors)
    }
}

/// Rejects canonical parameter changes that would detach a record from the
/// runtime dependency closure owned by its placement binding.
///
/// This is an export boundary, not only a UI policy: project overlays and CLI
/// callers can construct clean `SceneParameter` values without going through
/// the inspector. Ordinary typed values remain editable, while linked values
/// must still equal the typed prototype unless the binding proves that the
/// corresponding dependency was updated with them.
pub(crate) fn validate_object_parameter_links(
    record: &JDramaRecord,
    object: &SceneObject,
    placement: &PlacementBinding,
) -> Result<()> {
    for descriptor in editable_object_parameters(record)? {
        let Some(parameter) = object.raw_params.get(&descriptor.key) else {
            continue;
        };
        if parameter.raw() == descriptor.raw_value {
            continue;
        }
        if let Some(reason) =
            parameter_read_only_reason(Some(placement), &descriptor.key, parameter.raw())
        {
            return Err(parameter_error(format!(
                "object '{}' cannot change linked parameter '{}': {reason}",
                object.id, descriptor.key
            )));
        }
    }
    Ok(())
}

/// Seeds canonical, non-dirty source parameters without overwriting current edits.
pub fn seed_scene_object_parameters(object: &mut SceneObject, record: &JDramaRecord) -> Result<()> {
    for descriptor in editable_object_parameters(record)? {
        let keep_edit = object
            .raw_params
            .get(&descriptor.key)
            .is_some_and(SceneParameter::is_dirty);
        if !keep_edit {
            object.insert_source_raw_param(descriptor.key, descriptor.raw_value);
        }
    }
    sync_scene_object_parameter_aliases(object);
    Ok(())
}

/// Refreshes legacy preview aliases from canonical typed parameters.
///
/// Aliases are always source values. Editing an alias directly is rejected by
/// [`apply_object_parameter_edits`]; callers should edit its canonical field.
pub fn sync_scene_object_parameter_aliases(object: &mut SceneObject) {
    sync_alias(object, OBJECT_PARAMETER_CHARACTER_NAME, "stream_string_0");

    let actor_tail = object
        .raw_param("resource_name")
        .or_else(|| object.raw_param("manager_name"))
        .map(str::to_string);
    if let Some(value) = actor_tail {
        object.insert_source_raw_param("actor_tail_string", value);
    }
    sync_alias(object, "item_selector", "nozzle_box_item");
    sync_alias(object, "body_color_index", "npc_body_color_index");
    sync_alias(object, "cloth_color_index", "npc_cloth_color_index");
    sync_alias(object, "pollution_amount", "npc_pollution_amount");
    sync_alias(object, "parts_color_index_0", "npc_parts_color_index_0");
    sync_alias(object, "parts_color_index_1", "npc_parts_color_index_1");
    sync_alias(object, "parts_color_index_2", "npc_parts_color_index_2");
    sync_alias(object, "parts_mask", "npc_parts_mask");
    sync_alias(object, "action_flags", "npc_action_flags");
    sync_alias(object, "blade_count", "grass_blade_count");
}

/// Applies canonical object parameter values with the requested persistence policy.
pub fn apply_object_parameter_edits(
    record: &mut JDramaRecord,
    object: &SceneObject,
    mode: ParameterApplyMode,
) -> Result<()> {
    let descriptors = editable_object_parameters(record)?;
    let schema = descriptors
        .iter()
        .map(|descriptor| (descriptor.key.as_str(), descriptor.kind))
        .collect::<BTreeMap<_, _>>();

    for (key, parameter) in &object.raw_params {
        if parameter.is_dirty() && !schema.contains_key(key.as_str()) {
            return Err(parameter_error(format!(
                "object '{}' has a dirty unknown or synthetic parameter '{key}'",
                object.id
            )));
        }
    }

    let mut parsed = BTreeMap::new();
    for descriptor in &descriptors {
        let Some(parameter) = object.raw_params.get(&descriptor.key) else {
            continue;
        };
        let should_apply = match mode {
            ParameterApplyMode::DirtyOnly => parameter.is_dirty(),
            ParameterApplyMode::AllCanonical => true,
        };
        if should_apply {
            parsed.insert(
                descriptor.key.clone(),
                parse_parameter_value(&descriptor.key, descriptor.kind, parameter.raw())?,
            );
        }
    }

    let mut updated = record.clone();
    if let Some(ParsedParameterValue::String(value)) = parsed.remove(OBJECT_PARAMETER_NAME) {
        updated.name = value;
    }
    if let Some(ParsedParameterValue::String(value)) =
        parsed.remove(OBJECT_PARAMETER_CHARACTER_NAME)
    {
        let JDramaRecordPayload::Actor { character_name, .. } = &mut updated.payload else {
            return Err(parameter_error(
                "character_name was supplied for a non-actor record",
            ));
        };
        *character_name = value;
    }

    if !parsed.is_empty() {
        let Some(fields) = record_fields_mut(&mut updated) else {
            return Err(parameter_error(format!(
                "record '{}' has no editable field stream",
                updated.type_name
            )));
        };
        for field in fields {
            if let Some(value) = parsed.remove(&field.name) {
                replace_field_value(field, value)?;
            }
        }
        if let Some((key, _)) = parsed.first_key_value() {
            return Err(parameter_error(format!(
                "record '{}' no longer contains canonical field '{key}'",
                updated.type_name
            )));
        }
    }

    *record = updated;
    Ok(())
}

/// Convenience wrapper for applying only values edited in this process.
pub fn apply_dirty_object_parameter_edits(
    record: &mut JDramaRecord,
    object: &SceneObject,
) -> Result<()> {
    apply_object_parameter_edits(record, object, ParameterApplyMode::DirtyOnly)
}

/// Applies every canonical value, including values deserialized as clean.
pub fn apply_all_object_parameters(record: &mut JDramaRecord, object: &SceneObject) -> Result<()> {
    apply_object_parameter_edits(record, object, ParameterApplyMode::AllCanonical)
}

#[derive(Debug, Clone, PartialEq)]
enum ParsedParameterValue {
    U32(u32),
    I32(i32),
    F32(f32),
    Vec2F32([f32; 2]),
    Vec3F32([f32; 3]),
    ColorRgba8([u8; 4]),
    String(String),
}

fn append_field_descriptors(
    record: &JDramaRecord,
    fields: &[JDramaField],
    descriptors: &mut Vec<EditableSceneParameter>,
    keys: &mut BTreeSet<String>,
) -> Result<()> {
    let transform_fields = transform_field_names(&record.type_name);
    for field in fields {
        if transform_fields.is_some_and(|names| names.contains(&field.name.as_str())) {
            continue;
        }
        let Some(kind) = field_kind(&field.value) else {
            continue;
        };
        push_descriptor(
            descriptors,
            keys,
            &record.type_name,
            &field.name,
            kind,
            canonical_field_value(&field.value),
        )?;
    }
    Ok(())
}

fn push_descriptor(
    descriptors: &mut Vec<EditableSceneParameter>,
    keys: &mut BTreeSet<String>,
    record_type: &str,
    key: &str,
    kind: ObjectParameterKind,
    raw_value: String,
) -> Result<()> {
    if !keys.insert(key.to_string()) {
        return Err(parameter_error(format!(
            "record has duplicate or ambiguous editable parameter name '{key}'"
        )));
    }
    descriptors.push(EditableSceneParameter {
        key: key.to_string(),
        kind,
        raw_value,
        read_only_reason: parameter_read_only_reason(None, key, ""),
        info: object_parameter_info(record_type, key),
    });
    Ok(())
}

fn object_parameter_info(record_type: &str, key: &str) -> Option<ObjectParameterInfo> {
    let record_type = record_type.rsplit("::").next().unwrap_or(record_type);
    let choice = |raw_value: &str, label: &str, description: &str| ObjectParameterChoice {
        raw_value: raw_value.to_string(),
        label: label.to_string(),
        description: description.to_string(),
    };
    if key == "coin_id" {
        let info = match record_type {
            "SamboFlower" => ObjectParameterInfo {
                display_name: Some("Flower coin".to_string()),
                description: "TSamboFlower checks only the sign of this value: any non-negative value registers its regular flower coin, while a negative value disables it. The flower_group_id field selects the linked flower group.".to_string(),
                choices: vec![
                    choice("-1", "Disabled", "Does not register a coin for this flower."),
                    choice("100", "Enabled", "Registers the flower's regular yellow coin. Sunshine's retail stages use 100 for enabled flowers."),
                ],
                indexed_choice: None,
                integer_range: None,
            },
            "MameGesso" => ObjectParameterInfo {
                display_name: Some("Unused coin value".to_string()),
                description: "TMameGesso::load calls TSpineEnemy::load directly instead of TSmallEnemy::load, so this serialized tail value is not assigned to the actor's coin selector by the retail runtime.".to_string(),
                choices: Vec::new(),
                indexed_choice: None,
                integer_range: None,
            },
            "BossManta" => ObjectParameterInfo {
                display_name: Some("Unused coin value".to_string()),
                description: "TBossManta does not use TSmallEnemy's coin loader, so the retail runtime does not interpret this serialized tail value as a coin-drop selector.".to_string(),
                choices: Vec::new(),
                indexed_choice: None,
                integer_range: None,
            },
            _ => ObjectParameterInfo {
                display_name: Some("Coin drop".to_string()),
                description: "Coin dropped by this TSmallEnemy-derived actor. Decomp behavior maps 0-49 to persistent blue-coin slots, 100 to an invisible TCoinEmpty placeholder, 101 to a fully disabled drop, 200 to a red coin, and -1 to the regular yellow-coin fallback. Blue slots must be unique within an area. Expanded slot values can be authored, but values outside 0-49 require a compatible expanded runtime.".to_string(),
                choices: vec![
                    choice("-1", "Yellow coin", "TItemManager rejects -1 as a special coin ID, so TSmallEnemy falls back to one regular yellow coin."),
                    choice("100", "No visible coin - empty actor", "Uses TCoinEmpty. The placeholder consumes the enemy's fallback drop but never appears."),
                    choice("101", "No coin drop - disabled", "TSmallEnemy treats 101 specially: it creates no coin actor and does not enable the regular-coin fallback."),
                    choice("200", "Red coin", "Registers and drops a red coin."),
                ],
                indexed_choice: Some(ObjectParameterIndexedChoice {
                    label: "Blue coin".to_string(),
                    index_label: "Slot".to_string(),
                    description: "Persistent per-area blue-coin save slot. Retail Sunshine implements slots 0-49. Other non-reserved values are retained for compatible expanded runtimes.".to_string(),
                    default_index: 0,
                    index_range: [0, i32::MAX as i64],
                    retail_index_range: Some([0, 49]),
                    reserved_indices: vec![100, 101, 200],
                }),
                integer_range: None,
            },
        };
        return Some(info);
    }
    if record_type != "Shine" {
        return None;
    }
    let info = match key {
        OBJECT_PARAMETER_NAME => ObjectParameterInfo {
            display_name: None,
            description: "Unique TNameRef key used by scripted Shine reveal calls. Editor-authored standalone Shines receive a generated unique name.".to_string(),
            choices: Vec::new(),
            indexed_choice: None,
            integer_range: None,
        },
        OBJECT_PARAMETER_CHARACTER_NAME => ObjectParameterInfo {
            display_name: None,
            description: "Character registration used by Sunshine to initialize the Shine actor and its resources.".to_string(),
            choices: Vec::new(),
            indexed_choice: None,
            integer_range: None,
        },
        "resource_name" => ObjectParameterInfo {
            display_name: None,
            description: "Stage-local resource stem. The normal Shine actor loads shine.bmd or shine_empty.bmd from this imported resource family.".to_string(),
            choices: Vec::new(),
            indexed_choice: None,
            integer_range: None,
        },
        "collection_type" => ObjectParameterInfo {
            display_name: None,
            description: "Initial runtime state read by TShine::loadBeforeInit. Normal is immediately collectible; Quickly starts the built-in delayed appearance sequence; Scripted/Demo stays dormant until another runtime object or event reveals it.".to_string(),
            choices: vec![
                choice("normal", "Normal - visible immediately", "Spawns active and collectible as soon as the stage loads."),
                choice("quickly", "Quick appearance", "Waits 240 frames, then starts Sunshine's built-in quick appearance camera sequence. The editor imports its required retail camera record automatically."),
                choice("demo", "Scripted / demo", "Starts dormant. It will not appear unless another actor or event calls a Shine appearance function."),
            ],
            indexed_choice: None,
            integer_range: None,
        },
        "shine_id" => ObjectParameterInfo {
            display_name: None,
            description: "Persistent global Shine save-flag ID. Independent slots are 0 through 119; reusing an ID shares collected state. -1 becomes 120 in TShine and is then folded to slot 0 by TFlagManager.".to_string(),
            choices: Vec::new(),
            indexed_choice: None,
            integer_range: Some([-1, 119]),
        },
        "in_stage" => ObjectParameterInfo {
            display_name: None,
            description: "Selects the collection cutscene camera. Sunshine stores -1 as outside and 0 as inside; other positive values are normalized to outside.".to_string(),
            choices: vec![
                choice("-1", "Outside collection camera", "Uses the outdoor Shine collection camera."),
                choice("0", "Inside collection camera", "Uses the indoor Shine collection camera."),
            ],
            indexed_choice: None,
            integer_range: Some([-1, 0]),
        },
        _ => return None,
    };
    Some(info)
}
fn parameter_read_only_reason(
    placement: Option<&PlacementBinding>,
    key: &str,
    raw_value: &str,
) -> Option<String> {
    let reason = match key {
        OBJECT_PARAMETER_NAME => {
            "Names participate in runtime TNameRef lookup and cannot be relinked safely yet."
        }
        "indexed_name_count" | "translucent_group_count" | "warp_pair_count" | "building_count" => {
            "Controls the serialized field layout and cannot be changed independently."
        }
        _ if key.starts_with("translucent_group_") && key.ends_with("_joint_count") => {
            "Controls the serialized field layout and cannot be changed independently."
        }
        "item_selector" => "Selects a runtime-managed resource; respawn the object to change.",
        "resource_name" => "Owns imported resources; respawn the object to change.",
        OBJECT_PARAMETER_CHARACTER_NAME => {
            "Owns a character registration; respawn the object to change."
        }
        "authoring_character_name" => {
            "Owns a character registration; respawn the object to change."
        }
        "graph_name" => "Owns an imported route graph; respawn the object to change.",
        "manager_name" => match placement {
            Some(PlacementBinding::Existing(_) | PlacementBinding::CloneOf(_)) => {
                "Retail and cloned manager links have no owned dependency."
            }
            Some(PlacementBinding::Authored(authored)) => {
                let matching_dependencies = authored
                    .dependencies
                    .iter()
                    .filter(|dependency| dependency.record.name == raw_value)
                    .count();
                if matching_dependencies == 1 {
                    return None;
                }
                return Some(format!(
                    "Requires one matching owned manager dependency (found {matching_dependencies})."
                ));
            }
            None => return None,
        },
        _ if key.ends_with("_name") => {
            "References runtime data that cannot be relinked safely yet."
        }
        _ => return None,
    };
    Some(reason.to_string())
}

fn record_fields(record: &JDramaRecord) -> Option<&[JDramaField]> {
    match &record.payload {
        JDramaRecordPayload::Fields { fields }
        | JDramaRecordPayload::Actor { fields, .. }
        | JDramaRecordPayload::Group { fields, .. } => Some(fields),
        JDramaRecordPayload::Empty => None,
    }
}

fn record_fields_mut(record: &mut JDramaRecord) -> Option<&mut [JDramaField]> {
    match &mut record.payload {
        JDramaRecordPayload::Fields { fields }
        | JDramaRecordPayload::Actor { fields, .. }
        | JDramaRecordPayload::Group { fields, .. } => Some(fields),
        JDramaRecordPayload::Empty => None,
    }
}

fn field_kind(value: &JDramaFieldValue) -> Option<ObjectParameterKind> {
    Some(match value {
        JDramaFieldValue::U32(_) => ObjectParameterKind::U32,
        JDramaFieldValue::I32(_) => ObjectParameterKind::I32,
        JDramaFieldValue::F32(_) => ObjectParameterKind::F32,
        JDramaFieldValue::Vec2F32(_) => ObjectParameterKind::Vec2F32,
        JDramaFieldValue::Vec3F32(_) => ObjectParameterKind::Vec3F32,
        JDramaFieldValue::ColorRgba8(_) => ObjectParameterKind::ColorRgba8,
        JDramaFieldValue::String(_) => ObjectParameterKind::String,
        JDramaFieldValue::LightMap(_) => return None,
    })
}

fn canonical_field_value(value: &JDramaFieldValue) -> String {
    match value {
        JDramaFieldValue::U32(value) => value.to_string(),
        JDramaFieldValue::I32(value) => value.to_string(),
        JDramaFieldValue::F32(value) => value.to_string(),
        JDramaFieldValue::Vec2F32(value) => format!("{},{}", value[0], value[1]),
        JDramaFieldValue::Vec3F32(value) => {
            format!("{},{},{}", value[0], value[1], value[2])
        }
        JDramaFieldValue::ColorRgba8(value) => {
            format!("{},{},{},{}", value[0], value[1], value[2], value[3])
        }
        JDramaFieldValue::String(value) => value.clone(),
        JDramaFieldValue::LightMap(_) => String::new(),
    }
}

fn parse_parameter_value(
    key: &str,
    kind: ObjectParameterKind,
    raw: &str,
) -> Result<ParsedParameterValue> {
    match kind {
        ObjectParameterKind::U32 => raw
            .trim()
            .parse::<u32>()
            .map(ParsedParameterValue::U32)
            .map_err(|error| invalid_value(key, raw, "u32", error)),
        ObjectParameterKind::I32 => raw
            .trim()
            .parse::<i32>()
            .map(ParsedParameterValue::I32)
            .map_err(|error| invalid_value(key, raw, "i32", error)),
        ObjectParameterKind::F32 => Ok(ParsedParameterValue::F32(parse_finite_f32(key, raw, raw)?)),
        ObjectParameterKind::Vec2F32 => {
            let values = parse_finite_vector::<2>(key, raw)?;
            Ok(ParsedParameterValue::Vec2F32(values))
        }
        ObjectParameterKind::Vec3F32 => {
            let values = parse_finite_vector::<3>(key, raw)?;
            Ok(ParsedParameterValue::Vec3F32(values))
        }
        ObjectParameterKind::ColorRgba8 => {
            let parts = split_exact::<4>(key, raw, "RGBA8")?;
            let mut values = [0_u8; 4];
            for (index, part) in parts.into_iter().enumerate() {
                values[index] = part
                    .trim()
                    .parse::<u8>()
                    .map_err(|error| invalid_value(key, raw, "RGBA8", error))?;
            }
            Ok(ParsedParameterValue::ColorRgba8(values))
        }
        ObjectParameterKind::String => {
            jdrama_key_code(raw).map_err(|error| {
                parameter_error(format!(
                    "parameter '{key}' is not valid Shift-JIS text: {error}"
                ))
            })?;
            Ok(ParsedParameterValue::String(raw.to_string()))
        }
    }
}

fn parse_finite_vector<const N: usize>(key: &str, raw: &str) -> Result<[f32; N]> {
    let parts = split_exact::<N>(key, raw, "finite float vector")?;
    let mut values = [0.0_f32; N];
    for (index, part) in parts.into_iter().enumerate() {
        values[index] = parse_finite_f32(key, raw, part)?;
    }
    Ok(values)
}

fn parse_finite_f32(key: &str, raw: &str, part: &str) -> Result<f32> {
    let value = part
        .trim()
        .parse::<f32>()
        .map_err(|error| invalid_value(key, raw, "finite f32", error))?;
    if !value.is_finite() {
        return Err(parameter_error(format!(
            "parameter '{key}' must contain only finite f32 values, got '{raw}'"
        )));
    }
    Ok(value)
}

fn split_exact<'a, const N: usize>(
    key: &str,
    raw: &'a str,
    expected: &str,
) -> Result<[&'a str; N]> {
    let parts = raw.split(',').collect::<Vec<_>>();
    parts.try_into().map_err(|parts: Vec<&str>| {
        parameter_error(format!(
            "parameter '{key}' expects {N} comma-separated {expected} components, got {} in '{raw}'",
            parts.len()
        ))
    })
}

fn replace_field_value(field: &mut JDramaField, value: ParsedParameterValue) -> Result<()> {
    match (&mut field.value, value) {
        (JDramaFieldValue::U32(current), ParsedParameterValue::U32(value)) => *current = value,
        (JDramaFieldValue::I32(current), ParsedParameterValue::I32(value)) => *current = value,
        (JDramaFieldValue::F32(current), ParsedParameterValue::F32(value)) => *current = value,
        (JDramaFieldValue::Vec2F32(current), ParsedParameterValue::Vec2F32(value)) => {
            *current = value;
        }
        (JDramaFieldValue::Vec3F32(current), ParsedParameterValue::Vec3F32(value)) => {
            *current = value;
        }
        (JDramaFieldValue::ColorRgba8(current), ParsedParameterValue::ColorRgba8(value)) => {
            *current = value;
        }
        (JDramaFieldValue::String(current), ParsedParameterValue::String(value)) => {
            *current = value;
        }
        (current, _) => {
            return Err(parameter_error(format!(
                "canonical field '{}' changed type from {:?}",
                field.name, current
            )));
        }
    }
    Ok(())
}

fn transform_field_names(type_name: &str) -> Option<[&'static str; 3]> {
    match semantic_type_name(type_name) {
        "AreaCylinder" => Some(["center", "authoring_vector", "cylinder_parameters"]),
        "Generator" => Some(["position", "rotation", "authoring_vector"]),
        _ => None,
    }
}

fn semantic_type_name(type_name: &str) -> &str {
    type_name.rsplit("::").next().unwrap_or(type_name)
}

fn sync_alias(object: &mut SceneObject, canonical: &str, alias: &str) {
    if let Some(value) = object.raw_param(canonical).map(str::to_string) {
        object.insert_source_raw_param(alias, value);
    }
}

fn invalid_value(
    key: &str,
    raw: &str,
    expected: &str,
    error: impl std::fmt::Display,
) -> SceneError {
    parameter_error(format!(
        "parameter '{key}' expects {expected}, got '{raw}': {error}"
    ))
}

fn parameter_error(message: impl Into<String>) -> SceneError {
    SceneError::StageExport(format!("object parameter edit failed: {}", message.into()))
}

fn display_record_path(path: &[usize]) -> String {
    if path.is_empty() {
        "root".to_string()
    } else {
        path.iter()
            .map(usize::to_string)
            .collect::<Vec<_>>()
            .join("/")
    }
}

#[cfg(test)]
mod editability_tests {
    use super::{
        editable_object_parameters, parameter_read_only_reason, seed_scene_object_parameters,
        validate_object_parameter_links,
    };
    use crate::{
        AuthoredPlacement, AuthoredPlacementDependency, PlacementAddress, PlacementBinding,
        SceneObject,
    };
    use sms_formats::{JDramaField, JDramaFieldValue, JDramaRecord, JDramaRecordPayload};

    fn record(name: &str) -> JDramaRecord {
        JDramaRecord {
            type_name: "Fixture".to_string(),
            name: name.to_string(),
            payload: JDramaRecordPayload::Empty,
        }
    }

    fn authored_binding(dependency_names: &[&str]) -> PlacementBinding {
        PlacementBinding::Authored(AuthoredPlacement {
            raw_resource_path: b"map/scene.bin".to_vec(),
            target_group_index: 3,
            prototype: record("actor"),
            dependencies: dependency_names
                .iter()
                .map(|name| AuthoredPlacementDependency {
                    target: None,
                    target_group_index: 2,
                    record: record(name),
                })
                .collect(),
        })
    }

    fn actor_with_coin_id(record_type: &str, coin_id: i32) -> JDramaRecord {
        JDramaRecord {
            type_name: record_type.to_string(),
            name: format!("{record_type} fixture"),
            payload: JDramaRecordPayload::Actor {
                transform: sms_formats::JDramaTransform {
                    translation: [0.0; 3],
                    rotation: [0.0; 3],
                    scale: [1.0; 3],
                },
                character_name: "fixture character".to_string(),
                light_map: sms_formats::JDramaLightMap::default(),
                fields: vec![JDramaField {
                    name: "coin_id".to_string(),
                    value: JDramaFieldValue::I32(coin_id),
                }],
            },
        }
    }

    #[test]
    fn small_enemy_coin_ids_use_decomp_derived_drop_choices_for_every_actor() {
        for record_type in [
            "MarioModokiTelesa",
            "LoopTelesa",
            "BoxTelesa",
            "SeeTelesa",
            "HamuKuri",
            "HaneHamuKuri",
            "HaneHamuKuri2",
            "Gesso",
            "HanaSambo",
            "SamboHead",
            "DebuTelesa",
            "Yumbo",
            "TabePuku",
            "LandGesso",
            "PoiHana",
            "PoiHanaRed",
            "SleepPoiHana",
            "FireWanwan",
            "AmiNoko",
            "Kumokun",
            "FireHamuKuri",
            "DoroHaneKuri",
            "TamaNoko",
            "BossDangoHamuKuri",
            "Rocket",
            "ElecNokonoko",
            "MoePukuLaunchPad",
            "TobiPukuLaunchPad",
            "StayPakkun",
        ] {
            let parameters = editable_object_parameters(&actor_with_coin_id(record_type, 101))
                .expect("coin-bearing actor should expose typed parameters");
            let coin = parameters
                .iter()
                .find(|parameter| parameter.key == "coin_id")
                .expect("coin_id descriptor");
            let info = coin.info.as_ref().expect("coin_id metadata");
            assert_eq!(
                info.display_name.as_deref(),
                Some("Coin drop"),
                "{record_type}"
            );
            assert_eq!(info.choices.len(), 4, "{record_type}");
            for expected in [-1, 100, 101, 200] {
                assert!(
                    info.choices
                        .iter()
                        .any(|choice| choice.raw_value == expected.to_string()),
                    "{record_type} should offer {expected}"
                );
            }
            let blue = info
                .indexed_choice
                .as_ref()
                .expect("blue coin should be one indexed choice");
            assert_eq!(blue.label, "Blue coin", "{record_type}");
            assert_eq!(blue.index_label, "Slot", "{record_type}");
            assert_eq!(blue.index_range, [0, i32::MAX as i64], "{record_type}");
            assert_eq!(blue.retail_index_range, Some([0, 49]), "{record_type}");
            for supported in [0, 49, 50, 99, 102, i32::MAX as i64] {
                assert!(
                    blue.accepts_index(supported),
                    "{record_type} should retain blue slot {supported}"
                );
            }
            for reserved in [-1, 100, 101, 200] {
                assert!(
                    !blue.accepts_index(reserved),
                    "{record_type} should reserve {reserved}"
                );
            }
            assert!(blue.is_retail_index(49), "{record_type}");
            assert!(!blue.is_retail_index(50), "{record_type}");
            assert_eq!(
                info.choices
                    .iter()
                    .find(|choice| choice.raw_value == "101")
                    .map(|choice| choice.label.as_str()),
                Some("No coin drop - disabled"),
                "{record_type}"
            );
            assert!(
                info.description.contains("TItemManager") || info.description.contains("Decomp")
            );
            assert!(info.description.contains("unique"));
        }
    }

    #[test]
    fn coin_id_metadata_preserves_class_specific_runtime_semantics() {
        let sambo = editable_object_parameters(&actor_with_coin_id("SamboFlower", 100)).unwrap();
        let sambo_info = sambo
            .iter()
            .find(|parameter| parameter.key == "coin_id")
            .and_then(|parameter| parameter.info.as_ref())
            .unwrap();
        assert_eq!(sambo_info.display_name.as_deref(), Some("Flower coin"));
        assert_eq!(
            sambo_info
                .choices
                .iter()
                .map(|choice| choice.raw_value.as_str())
                .collect::<Vec<_>>(),
            vec!["-1", "100"]
        );
        assert!(sambo_info.description.contains("sign"));
        assert!(sambo_info.indexed_choice.is_none());

        for record_type in ["MameGesso", "BossManta"] {
            let parameters =
                editable_object_parameters(&actor_with_coin_id(record_type, 100)).unwrap();
            let info = parameters
                .iter()
                .find(|parameter| parameter.key == "coin_id")
                .and_then(|parameter| parameter.info.as_ref())
                .unwrap();
            assert_eq!(
                info.display_name.as_deref(),
                Some("Unused coin value"),
                "{record_type}"
            );
            assert!(info.choices.is_empty(), "{record_type}");
            assert!(info.indexed_choice.is_none(), "{record_type}");
            assert!(info.description.contains("not"), "{record_type}");
        }
    }

    #[test]
    fn shine_parameters_explain_runtime_modes_and_safe_ranges() {
        let record = JDramaRecord {
            type_name: "Shine".to_string(),
            name: "retail shine".to_string(),
            payload: JDramaRecordPayload::Actor {
                transform: sms_formats::JDramaTransform {
                    translation: [0.0; 3],
                    rotation: [0.0; 3],
                    scale: [1.0; 3],
                },
                character_name: "??????".to_string(),
                light_map: sms_formats::JDramaLightMap::default(),
                fields: vec![
                    JDramaField {
                        name: "collection_type".to_string(),
                        value: JDramaFieldValue::String("demo".to_string()),
                    },
                    JDramaField {
                        name: "shine_id".to_string(),
                        value: JDramaFieldValue::I32(104),
                    },
                    JDramaField {
                        name: "in_stage".to_string(),
                        value: JDramaFieldValue::I32(1),
                    },
                ],
            },
        };

        let parameters = editable_object_parameters(&record).unwrap();
        let collection = parameters
            .iter()
            .find(|parameter| parameter.key == "collection_type")
            .unwrap();
        let collection_info = collection.info.as_ref().unwrap();
        assert_eq!(
            collection_info
                .choices
                .iter()
                .map(|choice| choice.raw_value.as_str())
                .collect::<Vec<_>>(),
            vec!["normal", "quickly", "demo"]
        );
        assert!(collection_info.description.contains("dormant"));

        let shine_id = parameters
            .iter()
            .find(|parameter| parameter.key == "shine_id")
            .unwrap();
        assert_eq!(
            shine_id.info.as_ref().unwrap().integer_range,
            Some([-1, 119])
        );

        let in_stage = parameters
            .iter()
            .find(|parameter| parameter.key == "in_stage")
            .unwrap();
        assert_eq!(in_stage.info.as_ref().unwrap().integer_range, Some([-1, 0]));
        assert_eq!(
            in_stage
                .info
                .as_ref()
                .unwrap()
                .choices
                .iter()
                .map(|choice| choice.raw_value.as_str())
                .collect::<Vec<_>>(),
            vec!["-1", "0"]
        );
    }

    #[test]
    fn closure_driving_fields_are_read_only_but_ordinary_fields_are_editable() {
        let record = JDramaRecord {
            type_name: "Fixture".to_string(),
            name: "fixture".to_string(),
            payload: JDramaRecordPayload::Fields {
                fields: [
                    "resource_name",
                    "graph_name",
                    "target_actor_name",
                    "item_selector",
                    "ordinary",
                ]
                .into_iter()
                .map(|name| JDramaField {
                    name: name.to_string(),
                    value: JDramaFieldValue::String("value".to_string()),
                })
                .collect(),
            },
        };
        let descriptors = editable_object_parameters(&record).unwrap();
        for key in [
            "name",
            "resource_name",
            "graph_name",
            "target_actor_name",
            "item_selector",
        ] {
            let descriptor = descriptors
                .iter()
                .find(|descriptor| descriptor.key == key)
                .unwrap();
            assert!(
                descriptor
                    .read_only_reason
                    .as_deref()
                    .is_some_and(|reason| reason.contains("runtime") || reason.contains("respawn")),
                "{key} should be linked read-only"
            );
        }
        assert!(descriptors
            .iter()
            .find(|descriptor| descriptor.key == "ordinary")
            .unwrap()
            .read_only_reason
            .is_none());

        let character_reason = parameter_read_only_reason(None, "character_name", "value")
            .expect("character_name should be linked read-only");
        assert!(character_reason.contains("respawn"));
        let authoring_character_reason =
            parameter_read_only_reason(None, "authoring_character_name", "value")
                .expect("authoring_character_name should be linked read-only");
        assert!(authoring_character_reason.contains("respawn"));

        for key in [
            "indexed_name_count",
            "translucent_group_count",
            "translucent_group_2_joint_count",
            "warp_pair_count",
            "building_count",
        ] {
            let reason = parameter_read_only_reason(None, key, "1").unwrap();
            assert!(
                reason.contains("serialized field layout"),
                "{key}: {reason}"
            );
        }
    }

    #[test]
    fn manager_name_requires_exactly_one_matching_owned_dependency() {
        let address = PlacementAddress {
            raw_resource_path: b"map/scene.bin".to_vec(),
            record_path: vec![3, 0],
        };
        for binding in [
            PlacementBinding::Existing(address.clone()),
            PlacementBinding::CloneOf(address),
        ] {
            assert!(
                parameter_read_only_reason(Some(&binding), "manager_name", "manager").is_some()
            );
        }

        let missing = authored_binding(&[]);
        assert!(
            parameter_read_only_reason(Some(&missing), "manager_name", "manager")
                .unwrap()
                .contains("found 0")
        );

        let exact = authored_binding(&["manager"]);
        assert_eq!(
            parameter_read_only_reason(Some(&exact), "manager_name", "manager"),
            None
        );

        let ambiguous = authored_binding(&["manager", "manager"]);
        assert!(
            parameter_read_only_reason(Some(&ambiguous), "manager_name", "manager")
                .unwrap()
                .contains("found 2")
        );
    }

    #[test]
    fn export_boundary_rejects_clean_link_changes_but_allows_owned_manager_rename() {
        let prototype = JDramaRecord {
            type_name: "Fixture".to_string(),
            name: "fixture".to_string(),
            payload: JDramaRecordPayload::Fields {
                fields: [
                    ("resource_name", "slot_a"),
                    ("character_name", "character_a"),
                    ("graph_name", "route_a"),
                    ("manager_name", "manager_a"),
                    ("item_selector", "normal_nozzle_item"),
                    ("ordinary", "7"),
                ]
                .into_iter()
                .map(|(name, value)| JDramaField {
                    name: name.to_string(),
                    value: JDramaFieldValue::String(value.to_string()),
                })
                .collect(),
            },
        };
        let mut object = SceneObject::new("fixture", "Fixture");
        seed_scene_object_parameters(&mut object, &prototype).unwrap();
        let mut binding = PlacementBinding::Authored(AuthoredPlacement {
            raw_resource_path: b"map/scene.bin".to_vec(),
            target_group_index: 3,
            prototype: prototype.clone(),
            dependencies: vec![AuthoredPlacementDependency {
                target: None,
                target_group_index: 2,
                record: record("manager_a"),
            }],
        });

        for key in [
            "name",
            "resource_name",
            "character_name",
            "graph_name",
            "item_selector",
        ] {
            let original = object.raw_param(key).unwrap().to_string();
            object.insert_source_raw_param(key, format!("{original}_changed"));
            let error = validate_object_parameter_links(&prototype, &object, &binding)
                .expect_err("a clean linked overlay change must be rejected");
            assert!(error.to_string().contains(key), "{error}");
            object.insert_source_raw_param(key, original);
        }

        object.insert_source_raw_param("manager_name", "manager_b");
        let error = validate_object_parameter_links(&prototype, &object, &binding)
            .expect_err("an unowned manager rename must be rejected");
        assert!(error.to_string().contains("manager_name"), "{error}");

        let PlacementBinding::Authored(authored) = &mut binding else {
            unreachable!();
        };
        authored.dependencies[0].record.name = "manager_b".to_string();
        validate_object_parameter_links(&prototype, &object, &binding)
            .expect("an exactly owned manager may be renamed with its dependency");

        object.insert_source_raw_param("manager_name", "manager_a");
        object.insert_source_raw_param("ordinary", "8");
        validate_object_parameter_links(&prototype, &object, &binding)
            .expect("ordinary clean overlay values remain editable");
    }
}
