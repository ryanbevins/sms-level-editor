//! Typed, lossless editing of JDrama object parameters.

use std::collections::{BTreeMap, BTreeSet};

use sms_formats::{
    jdrama_key_code, JDramaField, JDramaFieldValue, JDramaRecord, JDramaRecordPayload,
};
use sms_schema::ObjectRegistry;

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
pub struct ObjectParameterBitFlag {
    pub bit: u8,
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
    /// Independently toggleable bits whose unknown bits must be preserved.
    pub bit_flags: Vec<ObjectParameterBitFlag>,
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
        enrich_monte_parameter_info(&mut descriptors, object, self.registry.as_ref());
        enrich_named_object_model_parameter_info(&mut descriptors, object, self.registry.as_ref());
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
#[cfg(test)]
pub(crate) fn validate_object_parameter_links(
    record: &JDramaRecord,
    object: &SceneObject,
    placement: &PlacementBinding,
) -> Result<()> {
    validate_object_parameter_links_with_owned_name(record, object, placement, false)
}

pub(crate) fn validate_object_parameter_links_with_owned_name(
    record: &JDramaRecord,
    object: &SceneObject,
    placement: &PlacementBinding,
    allow_editor_owned_name: bool,
) -> Result<()> {
    for descriptor in editable_object_parameters(record)? {
        let Some(parameter) = object.raw_params.get(&descriptor.key) else {
            continue;
        };
        if parameter.raw() == descriptor.raw_value {
            continue;
        }
        if allow_editor_owned_name && descriptor.key == OBJECT_PARAMETER_NAME {
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
    let bit_flag = |bit: u8, label: &str, description: &str| ObjectParameterBitFlag {
        bit,
        label: label.to_string(),
        description: description.to_string(),
    };
    let plain = |display_name: &str, description: &str| ObjectParameterInfo {
        display_name: Some(display_name.to_string()),
        bit_flags: Vec::new(),
        description: description.to_string(),
        choices: Vec::new(),
        indexed_choice: None,
        integer_range: None,
    };

    if record_type.starts_with("NPCMonte") {
        let info = match key {
            OBJECT_PARAMETER_NAME => plain(
                "Runtime name",
                "TNameRef identity used by scripts and runtime lookups. Respawn the actor to change it safely.",
            ),
            OBJECT_PARAMETER_CHARACTER_NAME => plain(
                "Character registration",
                "Stage character-data registration that supplies this Pianta's model and animations.",
            ),
            "body_color_index" => plain(
                "Skin palette",
                "Palette index applied to the root-model body materials. Available indices come from the decomp-extracted TNpcInitInfo color tables for this exact Pianta variant.",
            ),
            "cloth_color_index" => plain(
                "Clothing palette",
                "Palette index applied to the root-model clothing materials. Available indices come from the decomp-extracted TNpcInitInfo color tables for this exact Pianta variant.",
            ),
            "pollution_amount" => ObjectParameterInfo {
                integer_range: Some([0, 255]),
                ..plain(
                    "Pollution amount",
                    "Initial pollution tint/intensity. Retail converts this byte-scale value to a 0-1 ratio for pollution-capable NPCs.",
                )
            },
            "parts_color_index_0" => plain(
                "Accessory palette 1",
                "Palette index used by accessories assigned to color channel 1 in the decomp-derived part definitions.",
            ),
            "parts_color_index_1" => plain(
                "Accessory palette 2",
                "Palette index used by accessories assigned to color channel 2 in the decomp-derived part definitions.",
            ),
            "parts_color_index_2" => plain(
                "Accessory palette 3",
                "Palette index used by accessories assigned to color channel 3 in the decomp-derived part definitions.",
            ),
            "parts_mask" => plain(
                "Accessories",
                "Bitmask of optional Pianta parts. The inspector resolves each bit to the exact models and attachment joints extracted from TNpcInitInfo.",
            ),
            "movement_type" => plain(
                "Behavior preset",
                "Retail Monte behavior and waiting-animation preset. Graph-linked Piantas move between nodes; stationary Piantas use the named waiting pose. Out-of-range values fall back to preset 0.",
            ),
            "action_flags" => ObjectParameterInfo {
                bit_flags: vec![bit_flag(
                    0,
                    "Can throw Mario",
                    "Retail creates TNpcThrow only when bit 0 is set. Other serialized bits are preserved even though setIndividualDifference_ does not interpret them.",
                )],
                ..plain(
                    "Throw options",
                    "Only bit 0 enables the Pianta throw controller in the retail loader. Unknown bits remain intact.",
                )
            },
            "motion_min" => plain(
                "Throw speed",
                "Launch speed passed to SMS_ThrowMario when this Pianta's throw controller is enabled.",
            ),
            "motion_max" => plain(
                "Throw angle",
                "Launch angle in degrees. Retail treats 0 or less as forward, 90 or more as straight up, and interpolates between them.",
            ),
            "coin_flag" => ObjectParameterInfo {
                choices: vec![
                    choice(
                        "100",
                        "No reward",
                        "Retail does not construct TNpcCoin for event ID 100.",
                    ),
                    choice(
                        "200",
                        "Red coin",
                        "Creates a red coin reward through TItemManager.",
                    ),
                    choice(
                        "2000",
                        "1-Up mushroom",
                        "Creates mushroom1up through TMapObjBaseManager.",
                    ),
                ],
                indexed_choice: Some(ObjectParameterIndexedChoice {
                    label: "Blue coin".to_string(),
                    index_label: "Slot".to_string(),
                    description: "Persistent per-area blue-coin slot. Retail accepts exactly 0 through 49, and each slot should be unique within the area.".to_string(),
                    default_index: 0,
                    index_range: [0, 49],
                    retail_index_range: Some([0, 49]),
                    reserved_indices: Vec::new(),
                }),
                ..plain(
                    "Reward",
                    "Reward produced by the NPC coin controller: blue-coin slots 0-49, red coin 200, 1-Up 2000, or no reward for other values such as the retail default 100.",
                )
            },
            _ => return None,
        };
        return Some(info);
    }

    if record_type == "NozzleBox" {
        let info = match key {
            OBJECT_PARAMETER_NAME => plain(
                "Runtime name",
                "TNameRef identity used by scripts and runtime lookups.",
            ),
            OBJECT_PARAMETER_CHARACTER_NAME => plain(
                "Character registration",
                "Character-data registration used to initialize the Nozzle Box actor and its shared resources.",
            ),
            "resource_name" => plain(
                "Resource family",
                "Must remain NozzleBox. TNozzleBox::load uses this TMapObjBase resource family for the intact, broken, and translucent models.",
            ),
            "item_selector" => ObjectParameterInfo {
                choices: vec![
                    choice(
                        "normal_nozzle_item",
                        "Hover Nozzle",
                        "Spawns the blue Hover Nozzle pickup (runtime nozzle type 4).",
                    ),
                    choice(
                        "rocket_nozzle_item",
                        "Rocket Nozzle",
                        "Spawns the red Rocket Nozzle pickup (runtime nozzle type 1).",
                    ),
                    choice(
                        "back_nozzle_item",
                        "Turbo Nozzle",
                        "Spawns the gray-blue Turbo Nozzle pickup (runtime nozzle type 5).",
                    ),
                ],
                ..plain(
                    "Nozzle type",
                    "Pickup created inside the box. Changing this also changes the retail TEV color applied to every box model state.",
                )
            },
            "validity_name" => ObjectParameterInfo {
                choices: vec![
                    choice(
                        "invalid",
                        "Locked until acquired",
                        "Starts translucent and unbreakable for Rocket and Turbo boxes, then activates when Mario obtains that nozzle type elsewhere. Retail uses the literal string invalid.",
                    ),
                    choice(
                        "valid",
                        "Available immediately",
                        "Starts solid and breakable. Retail recognizes only the exact string valid as enabled.",
                    ),
                ],
                ..plain(
                    "Initial availability",
                    "Initial box state read by TNozzleBox::load. Hover boxes are always made available during loadAfter; this setting affects Rocket and Turbo boxes.",
                )
            },
            "break_height" => plain(
                "Forward ejection strength",
                "Serialized forward launch setting for the released pickup. Retail multiplies this value by 0.02 before applying it as horizontal velocity; the retail value 100 becomes speed 2.",
            ),
            "respawn_height" => plain(
                "Upward ejection speed",
                "Vertical velocity added when the pickup is released. Any negative value selects the retail default of 20; zero or positive values are used directly.",
            ),
            _ => return None,
        };
        return Some(info);
    }

    if key == "coin_id" {
        let info = match record_type {
            "SamboFlower" => ObjectParameterInfo {
                display_name: Some("Flower coin".to_string()),
                bit_flags: Vec::new(),
                description: "TSamboFlower checks only the sign of this value: any non-negative value registers its regular flower coin, while a negative value disables it. The flower_group_id field selects the linked flower group.".to_string(),
                choices: vec![
                    choice("-1", "Disabled", "Does not register a coin for this flower."),
                    choice("100", "Enabled", "Registers the flower's regular yellow coin. Sunshine's retail stages use 100 for enabled flowers."),
                ],
                indexed_choice: None,
                integer_range: None,
            },
            "MameGesso" => ObjectParameterInfo {
                bit_flags: Vec::new(),
                ..plain(
                    "Unused coin value",
                    "TMameGesso::load calls TSpineEnemy::load directly instead of TSmallEnemy::load, so this serialized tail value is not assigned to the actor's coin selector by the retail runtime.",
                )
            },
            "BossManta" => ObjectParameterInfo {
                bit_flags: Vec::new(),
                ..plain(
                    "Unused coin value",
                    "TBossManta does not use TSmallEnemy's coin loader, so the retail runtime does not interpret this serialized tail value as a coin-drop selector.",
                )
            },
            _ => ObjectParameterInfo {
                display_name: Some("Coin drop".to_string()),
                bit_flags: Vec::new(),
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
            bit_flags: Vec::new(),
            description: "Unique TNameRef key used by scripted Shine reveal calls. Editor-authored standalone Shines receive a generated unique name.".to_string(),
            choices: Vec::new(),
            indexed_choice: None,
            integer_range: None,
        },
        OBJECT_PARAMETER_CHARACTER_NAME => ObjectParameterInfo {
            display_name: None,
            bit_flags: Vec::new(),
            description: "Character registration used by Sunshine to initialize the Shine actor and its resources.".to_string(),
            choices: Vec::new(),
            indexed_choice: None,
            integer_range: None,
        },
        "resource_name" => ObjectParameterInfo {
            display_name: None,
            bit_flags: Vec::new(),
            description: "Stage-local resource stem. The normal Shine actor loads shine.bmd or shine_empty.bmd from this imported resource family.".to_string(),
            choices: Vec::new(),
            indexed_choice: None,
            integer_range: None,
        },
        "collection_type" => ObjectParameterInfo {
            display_name: None,
            bit_flags: Vec::new(),
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
            bit_flags: Vec::new(),
            description: "Persistent global Shine save-flag ID. Independent slots are 0 through 119; reusing an ID shares collected state. -1 becomes 120 in TShine and is then folded to slot 0 by TFlagManager.".to_string(),
            choices: Vec::new(),
            indexed_choice: None,
            integer_range: Some([-1, 119]),
        },
        "in_stage" => ObjectParameterInfo {
            display_name: None,
            bit_flags: Vec::new(),
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

fn enrich_monte_parameter_info(
    parameters: &mut [EditableSceneParameter],
    object: &SceneObject,
    registry: Option<&ObjectRegistry>,
) {
    if !object.factory_name.starts_with("NPCMonte") {
        return;
    }
    let Some(registry) = registry else {
        return;
    };

    for parameter in parameters {
        let Some(info) = parameter.info.as_mut() else {
            continue;
        };
        match parameter.key.as_str() {
            "movement_type" => {
                if let Some(presets) = registry.npc_action_presets_for(&object.factory_name) {
                    info.choices = presets
                        .action_flags
                        .iter()
                        .enumerate()
                        .map(|(index, flags)| ObjectParameterChoice {
                            raw_value: index.to_string(),
                            label: format!(
                                "{index}: {}",
                                monte_behavior_preset_name(index, *flags)
                            ),
                            description: monte_behavior_preset_description(index, *flags),
                        })
                        .collect();
                }
            }
            "parts_mask" => {
                if let Some(actor) = registry.find_npc_actor(&object.factory_name) {
                    info.bit_flags = actor
                        .parts
                        .iter()
                        .map(|part| ObjectParameterBitFlag {
                            bit: part.bit_index,
                            label: npc_part_label(part),
                            description: npc_part_description(part),
                        })
                        .collect();
                }
            }
            "body_color_index" => {
                let count = registry
                    .npc_material_colors_for(&object.factory_name)
                    .filter(|definition| definition.color_index_channel == 0)
                    .map(|definition| npc_color_count(&definition.change))
                    .max()
                    .unwrap_or(0);
                install_npc_palette_choice(info, count);
            }
            "cloth_color_index" => {
                let count = registry
                    .npc_material_colors_for(&object.factory_name)
                    .filter(|definition| definition.color_index_channel == 1)
                    .map(|definition| npc_color_count(&definition.change))
                    .max()
                    .unwrap_or(0);
                install_npc_palette_choice(info, count);
            }
            key if key.starts_with("parts_color_index_") => {
                let channel = key
                    .strip_prefix("parts_color_index_")
                    .and_then(|value| value.parse::<u8>().ok());
                let count = channel
                    .and_then(|channel| {
                        registry.find_npc_actor(&object.factory_name).map(|actor| {
                            actor
                                .parts
                                .iter()
                                .filter(|part| part.color_index_channel == channel)
                                .flat_map(|part| part.color_changes.iter())
                                .map(npc_color_count)
                                .max()
                                .unwrap_or(0)
                        })
                    })
                    .unwrap_or(0);
                install_npc_palette_choice(info, count);
            }
            _ => {}
        }
    }
}

fn enrich_named_object_model_parameter_info(
    parameters: &mut [EditableSceneParameter],
    object: &SceneObject,
    registry: Option<&ObjectRegistry>,
) {
    let Some(registry) = registry else {
        return;
    };
    let mut variants = registry
        .named_object_models
        .iter()
        .filter(|definition| definition.factory_name == object.factory_name)
        .collect::<Vec<_>>();
    if variants.is_empty() {
        return;
    }
    variants.sort_by(|left, right| {
        left.model_path
            .cmp(&right.model_path)
            .then_with(|| left.object_name.cmp(&right.object_name))
    });

    for parameter in parameters {
        parameter.info = match parameter.key.as_str() {
            OBJECT_PARAMETER_NAME => Some(ObjectParameterInfo {
                display_name: Some("Gate variant".to_string()),
                bit_flags: Vec::new(),
                description: "TModelGate::loadAfter compares this exact JDrama name against the decomp gateNames table. The matching table index selects the gate model, BTK/BRK/BPK animation family, warp destination, and persistent visibility flag; an unknown name falls back to the first gate variant.".to_string(),
                choices: variants
                    .iter()
                    .map(|definition| ObjectParameterChoice {
                        raw_value: definition.object_name.clone(),
                        label: named_object_variant_label(
                            &definition.object_name,
                            &definition.model_path,
                        ),
                        description: format!(
                            "Exact decomp selector {}. Loads {} with model-loader flags 0x{:08X}.",
                            definition.object_name,
                            definition.model_path,
                            definition.load_flags
                        ),
                    })
                    .collect(),
                indexed_choice: None,
                integer_range: None,
            }),
            OBJECT_PARAMETER_CHARACTER_NAME => Some(ObjectParameterInfo {
                display_name: Some("Character registration".to_string()),
                bit_flags: Vec::new(),
                description: "JDrama::TActor::load resolves this serialized TCharacter registration. TModelGate has no additional placement-stream parameters: its gate variant comes from the exact runtime name above, while interaction radii, blur settings, animation timing, and opening behavior are constants initialized by TModelGate::loadAfter.".to_string(),
                choices: Vec::new(),
                indexed_choice: None,
                integer_range: None,
            }),
            _ => parameter.info.take(),
        };
    }
}

fn named_object_variant_label(object_name: &str, model_path: &str) -> String {
    let model_name = model_path
        .rsplit('/')
        .next()
        .unwrap_or(model_path)
        .strip_suffix(".bmd")
        .unwrap_or(model_path);
    format!("{object_name} ({model_name})")
}

fn install_npc_palette_choice(info: &mut ObjectParameterInfo, count: usize) {
    let Ok(last_index) = i64::try_from(count.saturating_sub(1)) else {
        return;
    };
    if count == 0 {
        return;
    }
    info.choices.push(ObjectParameterChoice {
        raw_value: "255".to_string(),
        label: "Unused (255)".to_string(),
        description: "Retail placements use 255 for an unused color channel. Select a real palette before enabling an accessory that consumes this channel.".to_string(),
    });
    info.indexed_choice = Some(ObjectParameterIndexedChoice {
        label: "Palette".to_string(),
        index_label: "Index".to_string(),
        description: format!(
            "Decomp-derived palette index for this exact Pianta variant (0-{last_index})."
        ),
        default_index: 0,
        index_range: [0, last_index],
        retail_index_range: Some([0, last_index]),
        reserved_indices: Vec::new(),
    });
}

fn npc_color_count(change: &sms_schema::NpcColorChangeDefinition) -> usize {
    change.colors0.len().max(change.colors1.len())
}

fn monte_behavior_preset_name(index: usize, flags: u32) -> String {
    let base = match flags {
        0x0000 => "Default (walk + normal wait)",
        0x0001 => "Sitting",
        0x0002 => "Wait A",
        0x0004 => "Dancing",
        0x0008 => "Run + normal wait",
        0x000A => "Run + Wait A",
        0x0010 => "Wait B",
        0x0018 => "Run + Wait B",
        0x0020 => "Talking / gesturing",
        0x0021 => "Sitting + talking",
        0x0028 => "Run + talking / gesturing",
        0x0040 => "Angry loop",
        0x0080 => "Continuous walk",
        0x0088 => "Continuous run",
        0x0400 => "Hold arrow sign",
        0x1080 => "Continuous walk (retail special)",
        0x4088 => "Burning + continuous run",
        _ => return format!("Action flags 0x{flags:04X}"),
    };

    let first_matching_index = match flags {
        0x0000 => 0,
        0x0002 => 2,
        0x0010 => 3,
        0x0020 => 4,
        0x0080 => 6,
        _ => index,
    };
    if index == first_matching_index {
        base.to_string()
    } else {
        format!("{base} (retail variant)")
    }
}

fn monte_behavior_preset_description(index: usize, flags: u32) -> String {
    let behavior = match flags {
        0x0000 => "Uses the normal wait animation. With a graph, walks between nodes and pauses at them.",
        0x0001 => "Uses seated wait, talk, wet, and mad animations and does not turn to face Mario.",
        0x0002 => "Uses mom_wait_a while stationary or paused at a graph node.",
        0x0004 => "Uses the looping dance animation while stationary or paused.",
        0x0008 => "Uses run speed and animation between graph nodes, then pauses with the normal wait.",
        0x000A => "Runs between graph nodes and uses mom_wait_a while paused.",
        0x0010 => "Uses mom_wait_b while stationary or paused at a graph node.",
        0x0018 => "Runs between graph nodes and uses mom_wait_b while paused.",
        0x0020 => "Uses the talk/gesture animation while stationary or paused at a graph node.",
        0x0021 => "Uses the seated talk animation and does not turn to face Mario.",
        0x0028 => "Runs between graph nodes and uses the talk/gesture animation while paused.",
        0x0040 => "Uses the looping mad animation while stationary or paused at a graph node.",
        0x0080 => "Walks continuously: at a graph node, immediately chooses the next node instead of entering a timed wait.",
        0x0088 => "Runs continuously and immediately chooses the next graph node instead of entering a timed wait.",
        0x0400 => "Installs the retail hold-arrow animation override and prevents normal turning behavior.",
        0x1080 => "Walks continuously. Retail also sets action bit 0x1000; the US decomp has no Monte-specific behavior attached to that bit.",
        0x4088 => "Runs continuously with smoke/fire effects and boosted speed. The Pianta cannot talk and reacts only to water until extinguished.",
        _ => "No verified friendly behavior name is available for this action-flag combination.",
    };
    format!("{behavior} Retail table entry {index}; action flags 0x{flags:04X}.")
}

fn npc_part_label(part: &sms_schema::NpcPartDefinition) -> String {
    let model = part
        .models
        .first()
        .map(|model| model.model_name.as_str())
        .unwrap_or("part");
    let stem = model.strip_suffix(".bmd").unwrap_or(model);
    let stem = stem.strip_suffix("_model").unwrap_or(stem);
    match stem.to_ascii_lowercase().as_str() {
        "hata" => "Hat A".to_string(),
        "hatb" => "Hat B".to_string(),
        "hatd" => "Hat D".to_string(),
        "hate" => "Hat E".to_string(),
        "hatf" => "Hat F".to_string(),
        "hatg" => "Hat G".to_string(),
        "higea" => "Mustache".to_string(),
        "glassesa" => "Glasses A".to_string(),
        "glassesb" => "Glasses B".to_string(),
        "eria" => "Collar".to_string(),
        "tieb" => "Tie".to_string(),
        "nimotsu" => "Baggage".to_string(),
        "uklele" => "Ukulele".to_string(),
        "udewar" => "Right bracelet".to_string(),
        "udewal" => "Left bracelet".to_string(),
        "arrowr" => "Right arrow".to_string(),
        "arrowl" => "Left arrow".to_string(),
        _ => humanize_npc_resource_name(stem),
    }
}

fn npc_part_description(part: &sms_schema::NpcPartDefinition) -> String {
    let models = part
        .models
        .iter()
        .map(|model| match model.joint_name.as_deref() {
            Some(joint) => format!("{} on {joint}", model.model_name),
            None => format!("{} on the root joint", model.model_name),
        })
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "TNpcInitInfo part bit {}. Decomp-derived model attachment: {models}.",
        part.bit_index
    )
}

fn humanize_npc_resource_name(name: &str) -> String {
    let mut output = String::new();
    for character in name.chars() {
        if character.is_ascii_uppercase()
            && output
                .chars()
                .last()
                .is_some_and(|previous| previous.is_ascii_lowercase())
        {
            output.push(' ');
        }
        output.push(character);
    }
    let mut output = output.replace('_', " ");
    if let Some(first) = output.get_mut(0..1) {
        first.make_ascii_uppercase();
    }
    output
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
        "item_selector" | "validity_name" => return None,
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
        apply_dirty_object_parameter_edits, editable_object_parameters,
        enrich_monte_parameter_info, enrich_named_object_model_parameter_info,
        monte_behavior_preset_name, parameter_read_only_reason, seed_scene_object_parameters,
        validate_object_parameter_links,
    };
    use crate::{
        AuthoredPlacement, AuthoredPlacementDependency, PlacementAddress, PlacementBinding,
        SceneObject,
    };
    use sms_formats::{JDramaField, JDramaFieldValue, JDramaRecord, JDramaRecordPayload};
    use sms_schema::{
        NpcActionPresetDefinition, NpcActorDefinition, NpcColorChangeDefinition,
        NpcMaterialColorDefinition, NpcPartDefinition, NpcPartModelDefinition, ObjectRegistry,
    };

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
    fn jelly_gate_parameters_follow_the_decomp_name_selected_model_table() {
        let mut parameters = editable_object_parameters(&JDramaRecord {
            type_name: "JellyGate".to_string(),
            name: "GateToRicco".to_string(),
            payload: JDramaRecordPayload::Actor {
                transform: sms_formats::JDramaTransform {
                    translation: [0.0; 3],
                    rotation: [0.0; 3],
                    scale: [1.0; 3],
                },
                character_name: "GateToRicco character".to_string(),
                light_map: sms_formats::JDramaLightMap::default(),
                fields: Vec::new(),
            },
        })
        .unwrap();
        let registry = ObjectRegistry {
            named_object_models: vec![
                sms_schema::NamedObjectModelDefinition {
                    factory_name: "JellyGate".to_string(),
                    object_name: "GateToRicco".to_string(),
                    model_path: "/scene/map/map/gate/05_gate02rico.bmd".to_string(),
                    load_flags: 0x1110_0000,
                    source_file: "src/MoveBG/ModelGate.cpp".to_string(),
                },
                sms_schema::NamedObjectModelDefinition {
                    factory_name: "JellyGate".to_string(),
                    object_name: "Gate".to_string(),
                    model_path: "/scene/map/map/gate/05_gate01.bmd".to_string(),
                    load_flags: 0x1110_0000,
                    source_file: "src/MoveBG/ModelGate.cpp".to_string(),
                },
            ],
            ..ObjectRegistry::default()
        };
        let object = SceneObject::new("gate", "JellyGate");

        enrich_named_object_model_parameter_info(&mut parameters, &object, Some(&registry));

        assert_eq!(parameters.len(), 2, "TModelGate has no custom stream tail");
        let name = &parameters[0];
        assert_eq!(name.key, "name");
        assert_eq!(
            name.info.as_ref().unwrap().display_name.as_deref(),
            Some("Gate variant")
        );
        assert_eq!(
            name.info
                .as_ref()
                .unwrap()
                .choices
                .iter()
                .map(|choice| choice.raw_value.as_str())
                .collect::<Vec<_>>(),
            ["Gate", "GateToRicco"]
        );
        assert!(name.info.as_ref().unwrap().choices[1]
            .description
            .contains("05_gate02rico.bmd"));
        assert_eq!(
            parameters[1].info.as_ref().unwrap().display_name.as_deref(),
            Some("Character registration")
        );
    }

    #[test]
    fn monte_parameters_use_readable_schema_backed_controls() {
        let mut parameters = editable_object_parameters(&JDramaRecord {
            type_name: "NPCMonteMA".to_string(),
            name: "monte".to_string(),
            payload: JDramaRecordPayload::Actor {
                transform: sms_formats::JDramaTransform {
                    translation: [0.0; 3],
                    rotation: [0.0; 3],
                    scale: [1.0; 3],
                },
                character_name: "monte".to_string(),
                light_map: sms_formats::JDramaLightMap::default(),
                fields: vec![
                    JDramaField {
                        name: "body_color_index".to_string(),
                        value: JDramaFieldValue::I32(0),
                    },
                    JDramaField {
                        name: "parts_mask".to_string(),
                        value: JDramaFieldValue::I32(0),
                    },
                    JDramaField {
                        name: "movement_type".to_string(),
                        value: JDramaFieldValue::I32(0),
                    },
                    JDramaField {
                        name: "action_flags".to_string(),
                        value: JDramaFieldValue::I32(0),
                    },
                    JDramaField {
                        name: "coin_flag".to_string(),
                        value: JDramaFieldValue::I32(100),
                    },
                ],
            },
        })
        .unwrap();
        let change = NpcColorChangeDefinition {
            mode: 1,
            material_name: "skin".to_string(),
            colors0: vec![[0, 0, 0, 255], [255, 255, 255, 255]],
            colors1: Vec::new(),
        };
        let registry = ObjectRegistry {
            npc_action_presets: vec![NpcActionPresetDefinition {
                actor_family: "Monte".to_string(),
                action_flags: vec![0, 8, 0x4088],
                source_file: "src/NPC/NpcInitActionData.cpp".to_string(),
            }],
            npc_actors: vec![NpcActorDefinition {
                actor_key: "MonteMA".to_string(),
                source_file: "src/NPC/NpcInitData.cpp".to_string(),
                parts: vec![NpcPartDefinition {
                    bit_index: 2,
                    color_index_channel: 0,
                    models: vec![NpcPartModelDefinition {
                        joint_name: Some("head".to_string()),
                        model_name: "glassesA_model.bmd".to_string(),
                    }],
                    color_changes: vec![change.clone()],
                    uses_pollution: false,
                    uses_shared_materials: false,
                }],
            }],
            npc_material_colors: vec![NpcMaterialColorDefinition {
                actor_key: "MonteMA".to_string(),
                model_index: 0,
                color_index_channel: 0,
                change,
                source_file: "src/NPC/NpcInitData.cpp".to_string(),
            }],
            ..ObjectRegistry::default()
        };
        let object = SceneObject::new("monte", "NPCMonteMA");
        enrich_monte_parameter_info(&mut parameters, &object, Some(&registry));

        let info = |key: &str| {
            parameters
                .iter()
                .find(|parameter| parameter.key == key)
                .unwrap()
                .info
                .as_ref()
                .unwrap()
        };
        assert_eq!(
            info("body_color_index").display_name.as_deref(),
            Some("Skin palette")
        );
        assert_eq!(
            info("body_color_index")
                .indexed_choice
                .as_ref()
                .unwrap()
                .index_range,
            [0, 1]
        );
        assert_eq!(info("parts_mask").bit_flags[0].label, "Glasses A");
        assert!(info("parts_mask").bit_flags[0]
            .description
            .contains("glassesA_model.bmd"));
        assert_eq!(
            info("movement_type").choices[0].label,
            "0: Default (walk + normal wait)"
        );
        assert_eq!(
            info("movement_type").choices[1].label,
            "1: Run + normal wait"
        );
        assert_eq!(
            info("movement_type").choices[2].label,
            "2: Burning + continuous run"
        );
        assert_eq!(info("action_flags").bit_flags[0].label, "Can throw Mario");
        assert_eq!(
            info("coin_flag")
                .choices
                .iter()
                .map(|choice| choice.raw_value.as_str())
                .collect::<Vec<_>>(),
            ["100", "200", "2000"]
        );
    }

    #[test]
    fn us_retail_monte_behavior_table_has_source_grounded_names() {
        let flags = [
            0x0000, 0x0001, 0x0002, 0x0010, 0x0020, 0x0021, 0x0080, 0x0088, 0x0040, 0x0000, 0x0000,
            0x0004, 0x1080, 0x0000, 0x0002, 0x0010, 0x0020, 0x0008, 0x000A, 0x0018, 0x0028, 0x0080,
            0x0000, 0x0400, 0x4088,
        ];
        let expected = [
            "Default (walk + normal wait)",
            "Sitting",
            "Wait A",
            "Wait B",
            "Talking / gesturing",
            "Sitting + talking",
            "Continuous walk",
            "Continuous run",
            "Angry loop",
            "Default (walk + normal wait) (retail variant)",
            "Default (walk + normal wait) (retail variant)",
            "Dancing",
            "Continuous walk (retail special)",
            "Default (walk + normal wait) (retail variant)",
            "Wait A (retail variant)",
            "Wait B (retail variant)",
            "Talking / gesturing (retail variant)",
            "Run + normal wait",
            "Run + Wait A",
            "Run + Wait B",
            "Run + talking / gesturing",
            "Continuous walk (retail variant)",
            "Default (walk + normal wait) (retail variant)",
            "Hold arrow sign",
            "Burning + continuous run",
        ];

        let names = flags
            .into_iter()
            .enumerate()
            .map(|(index, flags)| monte_behavior_preset_name(index, flags))
            .collect::<Vec<_>>();
        assert_eq!(names, expected);
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
    fn nozzle_box_parameters_use_retail_names_choices_and_editable_tail_values() {
        let mut record = JDramaRecord {
            type_name: "NozzleBox".to_string(),
            name: "nozzle box".to_string(),
            payload: JDramaRecordPayload::Actor {
                transform: sms_formats::JDramaTransform {
                    translation: [0.0; 3],
                    rotation: [0.0; 3],
                    scale: [1.0; 3],
                },
                character_name: "NozzleBox Character".to_string(),
                light_map: sms_formats::JDramaLightMap::default(),
                fields: vec![
                    JDramaField {
                        name: "resource_name".to_string(),
                        value: JDramaFieldValue::String("NozzleBox".to_string()),
                    },
                    JDramaField {
                        name: "item_selector".to_string(),
                        value: JDramaFieldValue::String("rocket_nozzle_item".to_string()),
                    },
                    JDramaField {
                        name: "validity_name".to_string(),
                        value: JDramaFieldValue::String("invalid".to_string()),
                    },
                    JDramaField {
                        name: "break_height".to_string(),
                        value: JDramaFieldValue::F32(100.0),
                    },
                    JDramaField {
                        name: "respawn_height".to_string(),
                        value: JDramaFieldValue::F32(-1.0),
                    },
                ],
            },
        };
        let parameters = editable_object_parameters(&record).unwrap();
        let parameter = |key: &str| {
            parameters
                .iter()
                .find(|parameter| parameter.key == key)
                .unwrap()
        };

        assert_eq!(
            parameter("item_selector")
                .info
                .as_ref()
                .unwrap()
                .display_name
                .as_deref(),
            Some("Nozzle type")
        );
        assert_eq!(
            parameter("item_selector")
                .info
                .as_ref()
                .unwrap()
                .choices
                .iter()
                .map(|choice| choice.label.as_str())
                .collect::<Vec<_>>(),
            ["Hover Nozzle", "Rocket Nozzle", "Turbo Nozzle"]
        );
        assert_eq!(
            parameter("validity_name")
                .info
                .as_ref()
                .unwrap()
                .display_name
                .as_deref(),
            Some("Initial availability")
        );
        assert_eq!(
            parameter("break_height")
                .info
                .as_ref()
                .unwrap()
                .display_name
                .as_deref(),
            Some("Forward ejection strength")
        );
        assert_eq!(
            parameter("respawn_height")
                .info
                .as_ref()
                .unwrap()
                .display_name
                .as_deref(),
            Some("Upward ejection speed")
        );
        assert!(parameter("item_selector").read_only_reason.is_none());
        assert!(parameter("validity_name").read_only_reason.is_none());
        assert!(parameter("resource_name").read_only_reason.is_some());

        let mut object = SceneObject::new("nozzle-box", "NozzleBox");
        seed_scene_object_parameters(&mut object, &record).unwrap();
        object.set_raw_param("item_selector", "back_nozzle_item");
        object.set_raw_param("validity_name", "valid");
        object.set_raw_param("break_height", "150");
        object.set_raw_param("respawn_height", "30");
        apply_dirty_object_parameter_edits(&mut record, &object).unwrap();

        let JDramaRecordPayload::Actor { fields, .. } = &record.payload else {
            unreachable!();
        };
        assert_eq!(
            fields[0].value,
            JDramaFieldValue::String("NozzleBox".to_string())
        );
        assert_eq!(
            fields[1].value,
            JDramaFieldValue::String("back_nozzle_item".to_string())
        );
        assert_eq!(
            fields[2].value,
            JDramaFieldValue::String("valid".to_string())
        );
        assert_eq!(fields[3].value, JDramaFieldValue::F32(150.0));
        assert_eq!(fields[4].value, JDramaFieldValue::F32(30.0));
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
        for key in ["name", "resource_name", "graph_name", "target_actor_name"] {
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
        for key in ["item_selector", "ordinary"] {
            assert!(descriptors
                .iter()
                .find(|descriptor| descriptor.key == key)
                .unwrap()
                .read_only_reason
                .is_none());
        }

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

        for key in ["name", "resource_name", "character_name", "graph_name"] {
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
