use std::collections::{BTreeMap, BTreeSet};

use regex::Regex;

use super::{
    braced_body, cpp_identifier, parse_cpp_string, parse_cpp_u32, split_cpp_initializer_fields,
    MapObjAnimationResourceDefinition, MapObjCollisionResourceDefinition, MapObjResourceDefinition,
};

#[derive(Debug)]
struct ParsedMapObjData {
    resource_name: Option<String>,
    actor_type: u32,
    object_flags: u32,
    required_manager_name: Option<String>,
    has_hold_dependency: bool,
    has_move_dependency: bool,
    uses_resource_name_model_fallback: bool,
    primary_model: Option<String>,
    animation_resources: Vec<MapObjAnimationResourceDefinition>,
    hold_model_path: Option<String>,
    move_bck_path: Option<String>,
    load_flags: u32,
    collision_resources: Vec<MapObjCollisionResourceDefinition>,
    is_terminal: bool,
}

#[derive(Debug, Clone)]
struct ParsedAnimationInfo {
    actor_count: u32,
    resources: Vec<MapObjAnimationResourceDefinition>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct MapObjModelLoaderPolicy {
    indirect_flag: u32,
    indirect_load_flags: u32,
    default_load_flags: u32,
}

impl MapObjModelLoaderPolicy {
    fn effective_load_flags(self, map_obj_flags: u32) -> u32 {
        if map_obj_flags & self.indirect_flag != 0 {
            self.indirect_load_flags
        } else {
            self.default_load_flags
        }
    }
}

fn extract_model_loader_policy(text: &str) -> Result<MapObjModelLoaderPolicy, String> {
    let init_actor_data_re = Regex::new(r"void\s+TMapObjBase::initActorData\s*\([^)]*\)\s*\{")
        .expect("valid initActorData regex");
    let init_actor_data = init_actor_data_re
        .find(text)
        .ok_or_else(|| "missing TMapObjBase::initActorData".to_string())?;
    let init_actor_data_body = braced_body(text, init_actor_data.end() - 1)
        .ok_or_else(|| "unterminated TMapObjBase::initActorData".to_string())?;
    let copy_re = Regex::new(r"unkF8\s*=\s*mMapObjData->unk34\s*;")
        .expect("valid map-object flag copy regex");
    if !copy_re.is_match(init_actor_data_body) {
        return Err(
            "TMapObjBase::initActorData no longer copies TMapObjData::unk34 to unkF8".to_string(),
        );
    }

    let make_actors_re = Regex::new(r"void\s+TMapObjBase::makeMActors\s*\([^)]*\)\s*\{")
        .expect("valid makeMActors regex");
    let make_actors = make_actors_re
        .find(text)
        .ok_or_else(|| "missing TMapObjBase::makeMActors".to_string())?;
    let make_actors_body = braced_body(text, make_actors.end() - 1)
        .ok_or_else(|| "unterminated TMapObjBase::makeMActors".to_string())?;
    let policy_re = Regex::new(
        r"(?s)if\s*\(\s*unkF8\s*&\s*([^)]+?)\s*\)\s*\{?\s*mMActorKeeper->mModelLoaderFlags\s*=\s*([^;]+?)\s*;\s*\}?\s*else\s*\{?\s*mMActorKeeper->mModelLoaderFlags\s*=\s*([^;]+?)\s*;",
    )
    .expect("valid map-object loader policy regex");
    let policies = policy_re
        .captures_iter(make_actors_body)
        .collect::<Vec<_>>();
    if policies.len() != 1 {
        return Err(format!(
            "TMapObjBase::makeMActors has {} recognizable unkF8 loader policies; expected exactly one",
            policies.len()
        ));
    }
    let policy = &policies[0];
    let parse = |capture: usize, label: &str| {
        parse_cpp_u32(&policy[capture])
            .ok_or_else(|| format!("TMapObjBase::makeMActors has non-numeric {label}"))
    };
    let policy = MapObjModelLoaderPolicy {
        indirect_flag: parse(1, "indirect MapObj flag")?,
        indirect_load_flags: parse(2, "indirect model-loader flags")?,
        default_load_flags: parse(3, "default model-loader flags")?,
    };
    if policy.indirect_flag == 0
        || policy.indirect_load_flags == 0
        || policy.default_load_flags == 0
    {
        return Err("TMapObjBase::makeMActors loader policy contains zero flags".to_string());
    }
    Ok(policy)
}

pub(super) fn has_null_animation_model_fallback(text: &str) -> bool {
    let method_re = Regex::new(r"void\s+TMapObjBase::makeMActors\s*\([^)]*\)\s*\{")
        .expect("valid TMapObjBase::makeMActors regex");
    let Some(method) = method_re.find(text) else {
        return false;
    };
    let Some(body) = braced_body(text, method.end() - 1) else {
        return false;
    };
    let animated_branch_re =
        Regex::new(r"if\s*\(\s*mMapObjData->mAnim\s*\)").expect("valid animation branch regex");
    if !animated_branch_re.is_match(body) {
        return false;
    }
    let else_re = Regex::new(r"\belse\s*\{").expect("valid else branch regex");
    let snprintf_re = Regex::new(
        r#"snprintf\s*\(\s*([A-Za-z_][A-Za-z0-9_]*)\s*,\s*64\s*,\s*"%s\.bmd"\s*,\s*mMapObjData->unk0\s*\)"#,
    )
    .expect("valid basename fallback snprintf regex");
    for else_branch in else_re.find_iter(body) {
        let Some(else_body) = braced_body(body, else_branch.end() - 1) else {
            continue;
        };
        let Some(format_call) = snprintf_re.captures(else_body) else {
            continue;
        };
        let init_re = Regex::new(&format!(
            r"mMActor\s*=\s*initMActor\s*\(\s*{}\b",
            regex::escape(&format_call[1])
        ))
        .expect("valid basename fallback initMActor regex");
        if init_re.is_match(else_body) {
            return true;
        }
    }
    false
}

/// Extracts only the `TMapObjData` records reachable through `sObjDataTable`,
/// preserving that runtime lookup order. Unreferenced declarations are not resources.
pub(super) fn extract_map_obj_resources(
    text: &str,
    source_file: &str,
) -> Result<Vec<MapObjResourceDefinition>, String> {
    let loader_policy = extract_model_loader_policy(text)?;
    let table = extract_obj_data_table(text)?;
    let reachable_data = table.iter().cloned().collect::<BTreeSet<_>>();
    let reachable_infos = referenced_anim_infos(text, &reachable_data)?;
    let reachable_arrays = referenced_anim_arrays(text, &reachable_infos)?;
    let anim_arrays = extract_anim_arrays(text, &reachable_arrays)?;
    let anim_infos = extract_anim_infos(text, &anim_arrays, &reachable_infos)?;
    let reachable_collision_infos = referenced_collision_infos(text, &reachable_data)?;
    let reachable_collision_arrays = referenced_collision_arrays(text, &reachable_collision_infos)?;
    let collision_arrays = extract_collision_arrays(text, &reachable_collision_arrays)?;
    let collision_infos =
        extract_collision_infos(text, &collision_arrays, &reachable_collision_infos)?;
    let reachable_holds = referenced_map_obj_data(text, &reachable_data, 10, "hold")?;
    let hold_models = extract_hold_models(text, &reachable_holds)?;
    let reachable_moves = referenced_map_obj_data(text, &reachable_data, 11, "move")?;
    let move_bcks = extract_move_bcks(text, &reachable_moves)?;
    let data = extract_map_obj_data(
        text,
        &anim_infos,
        &collision_infos,
        &hold_models,
        &move_bcks,
        &reachable_data,
        loader_policy,
    )?;

    let mut definitions = Vec::new();
    let mut seen_variables = BTreeSet::new();
    let mut saw_terminal = false;
    for (index, variable) in table.iter().enumerate() {
        if !seen_variables.insert(variable.as_str()) {
            return Err(format!(
                "sObjDataTable contains duplicate entry {variable} at index {index}"
            ));
        }
        let record = data.get(variable).ok_or_else(|| {
            format!("sObjDataTable entry {variable} has no TMapObjData declaration")
        })?;
        if record.is_terminal {
            if index + 1 != table.len() {
                return Err(format!(
                    "terminal sObjDataTable entry {variable} occurs before the end"
                ));
            }
            if record.resource_name.is_some() {
                return Err(format!(
                    "terminal sObjDataTable entry {variable} unexpectedly has a resource name"
                ));
            }
            saw_terminal = true;
            continue;
        }
        let resource_name = record.resource_name.clone().ok_or_else(|| {
            format!("non-terminal sObjDataTable entry {variable} has no resource name")
        })?;
        definitions.push(MapObjResourceDefinition {
            resource_name,
            actor_type: record.actor_type,
            object_flags: record.object_flags,
            required_manager_name: record.required_manager_name.clone().ok_or_else(|| {
                format!(
                    "non-terminal sObjDataTable entry {variable} has no required TLiveManager name"
                )
            })?,
            has_hold_dependency: record.has_hold_dependency,
            has_move_dependency: record.has_move_dependency,
            uses_resource_name_model_fallback: record.uses_resource_name_model_fallback,
            primary_model: record.primary_model.clone(),
            animation_resources: record.animation_resources.clone(),
            hold_model_path: record.hold_model_path.clone(),
            move_bck_path: record.move_bck_path.clone(),
            load_flags: record.load_flags,
            collision_resources: record.collision_resources.clone(),
            source_file: source_file.to_string(),
        });
    }
    if !saw_terminal {
        return Err("sObjDataTable has no final zero-type terminator".to_string());
    }
    Ok(definitions)
}

fn extract_anim_arrays(
    text: &str,
    reachable: &BTreeSet<String>,
) -> Result<BTreeMap<String, Vec<MapObjAnimationResourceDefinition>>, String> {
    let declaration = Regex::new(
        r"static\s+const\s+TMapObjAnimData\s+([A-Za-z_][A-Za-z0-9_]*)\s*\[\s*\]\s*=\s*\{",
    )
    .expect("valid TMapObjAnimData regex");
    let entry_re = Regex::new(r"\{([^{}]*)\}").expect("valid animation-data entry regex");
    let mut arrays = BTreeMap::new();
    for captures in declaration.captures_iter(text) {
        if !reachable.contains(&captures[1]) {
            continue;
        }
        let whole = captures
            .get(0)
            .ok_or_else(|| "missing animation-array declaration match".to_string())?;
        let body = braced_body(text, whole.end() - 1)
            .ok_or_else(|| format!("unterminated animation array {}", &captures[1]))?;
        let mut resources = Vec::new();
        for entry in entry_re.captures_iter(body) {
            let fields = split_cpp_initializer_fields(&entry[1]);
            if fields.len() != 5 {
                return Err(format!(
                    "animation array {} has an entry with {} fields; expected 5",
                    &captures[1],
                    fields.len()
                ));
            }
            let channel = parse_cpp_u32(fields[2]).ok_or_else(|| {
                format!(
                    "animation array {} has a non-numeric animation channel",
                    &captures[1]
                )
            })?;
            let animation_channel = u8::try_from(channel).map_err(|_| {
                format!(
                    "animation array {} has an animation channel exceeding u8",
                    &captures[1]
                )
            })?;
            resources.push(MapObjAnimationResourceDefinition {
                model_name: parse_cpp_string(fields[0]),
                animation_name: parse_cpp_string(fields[1]),
                animation_channel,
                extra_name: parse_cpp_string(fields[3]),
                bas_path: parse_cpp_string(fields[4]),
            });
        }
        if resources.is_empty() {
            return Err(format!("animation array {} has no entries", &captures[1]));
        }
        if arrays.insert(captures[1].to_string(), resources).is_some() {
            return Err(format!("duplicate animation array {}", &captures[1]));
        }
    }
    Ok(arrays)
}

fn extract_anim_infos(
    text: &str,
    arrays: &BTreeMap<String, Vec<MapObjAnimationResourceDefinition>>,
    reachable: &BTreeSet<String>,
) -> Result<BTreeMap<String, ParsedAnimationInfo>, String> {
    let declaration =
        Regex::new(r"static\s+const\s+TMapObjAnimDataInfo\s+([A-Za-z_][A-Za-z0-9_]*)\s*=\s*\{")
            .expect("valid TMapObjAnimDataInfo regex");
    let mut infos = BTreeMap::new();
    for captures in declaration.captures_iter(text) {
        if !reachable.contains(&captures[1]) {
            continue;
        }
        let whole = captures
            .get(0)
            .ok_or_else(|| "missing animation-info declaration match".to_string())?;
        let body = braced_body(text, whole.end() - 1)
            .ok_or_else(|| format!("unterminated animation info {}", &captures[1]))?;
        let fields = split_cpp_initializer_fields(body);
        if fields.len() < 3 {
            return Err(format!(
                "animation info {} has {} fields; expected at least 3",
                &captures[1],
                fields.len()
            ));
        }
        let entry_count = parse_cpp_u32(fields[0]).ok_or_else(|| {
            format!(
                "animation info {} has a non-numeric entry count",
                &captures[1]
            )
        })? as usize;
        let actor_count = parse_cpp_u32(fields[1]).ok_or_else(|| {
            format!(
                "animation info {} has a non-numeric actor count",
                &captures[1]
            )
        })?;
        let resources = if entry_count == 0 {
            Vec::new()
        } else {
            let array_name = cpp_identifier(fields[2]).ok_or_else(|| {
                format!(
                    "animation info {} has entries but no animation array",
                    &captures[1]
                )
            })?;
            let array = arrays.get(array_name).ok_or_else(|| {
                format!(
                    "animation info {} references unknown array {array_name}",
                    &captures[1]
                )
            })?;
            if entry_count > array.len() {
                return Err(format!(
                    "animation info {} requests {entry_count} entries from {array_name}, which has {}",
                    &captures[1],
                    array.len()
                ));
            }
            array[..entry_count].to_vec()
        };
        if actor_count > 0
            && resources
                .first()
                .and_then(|resource| resource.model_name.as_deref())
                .is_none()
        {
            return Err(format!(
                "animation info {} has actors but its first model is null",
                &captures[1]
            ));
        }
        if infos
            .insert(
                captures[1].to_string(),
                ParsedAnimationInfo {
                    actor_count,
                    resources,
                },
            )
            .is_some()
        {
            return Err(format!("duplicate animation info {}", &captures[1]));
        }
    }
    Ok(infos)
}

fn referenced_map_obj_data(
    text: &str,
    reachable_data: &BTreeSet<String>,
    field_index: usize,
    label: &str,
) -> Result<BTreeSet<String>, String> {
    let declaration = Regex::new(r"static\s+TMapObjData\s+([A-Za-z_][A-Za-z0-9_]*)\s*=\s*\{")
        .expect("valid TMapObjData regex");
    let identifier = Regex::new(r"^[A-Za-z_][A-Za-z0-9_]*$").expect("valid C++ identifier regex");
    let mut referenced = BTreeSet::new();
    for captures in declaration.captures_iter(text) {
        if !reachable_data.contains(&captures[1]) {
            continue;
        }
        let whole = captures
            .get(0)
            .ok_or_else(|| "missing reachable map-object declaration match".to_string())?;
        let body = braced_body(text, whole.end() - 1)
            .ok_or_else(|| format!("unterminated map-object data {}", &captures[1]))?;
        let fields = split_cpp_initializer_fields(body);
        if fields.len() < 15 {
            return Err(format!(
                "map-object data {} has {} fields; expected at least 15",
                &captures[1],
                fields.len()
            ));
        }
        if let Some(variable) = cpp_identifier(fields[field_index]) {
            if !identifier.is_match(variable) {
                return Err(format!(
                    "map-object data {} has a non-identifier {label} dependency {variable:?}",
                    &captures[1]
                ));
            }
            referenced.insert(variable.to_string());
        }
    }
    Ok(referenced)
}

fn extract_hold_models(
    text: &str,
    reachable: &BTreeSet<String>,
) -> Result<BTreeMap<String, String>, String> {
    let declaration = Regex::new(r"static\s+TMapObjHoldData\s+([A-Za-z_][A-Za-z0-9_]*)\s*=\s*\{")
        .expect("valid TMapObjHoldData regex");
    let mut models = BTreeMap::new();
    for captures in declaration.captures_iter(text) {
        if !reachable.contains(&captures[1]) {
            continue;
        }
        let whole = captures
            .get(0)
            .ok_or_else(|| "missing hold-data declaration match".to_string())?;
        let body = braced_body(text, whole.end() - 1)
            .ok_or_else(|| format!("unterminated hold data {}", &captures[1]))?;
        let fields = split_cpp_initializer_fields(body);
        if fields.len() < 2 {
            return Err(format!(
                "hold data {} has {} fields; expected at least 2",
                &captures[1],
                fields.len()
            ));
        }
        let model_path = parse_cpp_string(fields[0])
            .ok_or_else(|| format!("hold data {} has no model path", &captures[1]))?;
        if models.insert(captures[1].to_string(), model_path).is_some() {
            return Err(format!("duplicate hold data {}", &captures[1]));
        }
    }
    Ok(models)
}

fn extract_move_bcks(
    text: &str,
    reachable: &BTreeSet<String>,
) -> Result<BTreeMap<String, String>, String> {
    let declaration = Regex::new(r"static\s+TMapObjMoveData\s+([A-Za-z_][A-Za-z0-9_]*)\s*=\s*\{")
        .expect("valid TMapObjMoveData regex");
    let mut bcks = BTreeMap::new();
    for captures in declaration.captures_iter(text) {
        if !reachable.contains(&captures[1]) {
            continue;
        }
        let whole = captures
            .get(0)
            .ok_or_else(|| "missing move-data declaration match".to_string())?;
        let body = braced_body(text, whole.end() - 1)
            .ok_or_else(|| format!("unterminated move data {}", &captures[1]))?;
        let fields = split_cpp_initializer_fields(body);
        let bck_path = fields
            .first()
            .and_then(|field| parse_cpp_string(field))
            .ok_or_else(|| format!("move data {} has no BCK path", &captures[1]))?;
        if bcks.insert(captures[1].to_string(), bck_path).is_some() {
            return Err(format!("duplicate move data {}", &captures[1]));
        }
    }
    Ok(bcks)
}

fn referenced_collision_infos(
    text: &str,
    reachable_data: &BTreeSet<String>,
) -> Result<BTreeSet<String>, String> {
    let declaration = Regex::new(r"static\s+TMapObjData\s+([A-Za-z_][A-Za-z0-9_]*)\s*=\s*\{")
        .expect("valid TMapObjData regex");
    let mut infos = BTreeSet::new();
    for captures in declaration.captures_iter(text) {
        if !reachable_data.contains(&captures[1]) {
            continue;
        }
        let whole = captures
            .get(0)
            .ok_or_else(|| "missing reachable map-object declaration match".to_string())?;
        let body = braced_body(text, whole.end() - 1)
            .ok_or_else(|| format!("unterminated map-object data {}", &captures[1]))?;
        let fields = split_cpp_initializer_fields(body);
        if fields.len() < 15 {
            return Err(format!(
                "map-object data {} has {} fields; expected at least 15",
                &captures[1],
                fields.len()
            ));
        }
        if let Some(info) = cpp_identifier(fields[6]) {
            infos.insert(info.to_string());
        }
    }
    Ok(infos)
}

fn referenced_collision_arrays(
    text: &str,
    reachable_infos: &BTreeSet<String>,
) -> Result<BTreeSet<String>, String> {
    let declaration =
        Regex::new(r"static\s+const\s+TMapObjCollisionInfo\s+([A-Za-z_][A-Za-z0-9_]*)\s*=\s*\{")
            .expect("valid TMapObjCollisionInfo regex");
    let mut arrays = BTreeSet::new();
    for captures in declaration.captures_iter(text) {
        if !reachable_infos.contains(&captures[1]) {
            continue;
        }
        let whole = captures
            .get(0)
            .ok_or_else(|| "missing collision-info declaration match".to_string())?;
        let body = braced_body(text, whole.end() - 1)
            .ok_or_else(|| format!("unterminated collision info {}", &captures[1]))?;
        let fields = split_cpp_initializer_fields(body);
        if fields.len() < 3 {
            return Err(format!(
                "collision info {} has {} fields; expected 3",
                &captures[1],
                fields.len()
            ));
        }
        let array = cpp_identifier(fields[2]).ok_or_else(|| {
            format!(
                "collision info {} has no collision-data array",
                &captures[1]
            )
        })?;
        arrays.insert(array.to_string());
    }
    Ok(arrays)
}

fn extract_collision_arrays(
    text: &str,
    reachable_arrays: &BTreeSet<String>,
) -> Result<BTreeMap<String, Vec<MapObjCollisionResourceDefinition>>, String> {
    let declaration = Regex::new(
        r"static\s+const\s+TMapObjCollisionData\s+([A-Za-z_][A-Za-z0-9_]*)\s*\[\s*\]\s*=\s*\{",
    )
    .expect("valid TMapObjCollisionData regex");
    let entry_re = Regex::new(r"\{([^{}]*)\}").expect("valid collision-data entry regex");
    let mut arrays = BTreeMap::new();
    for captures in declaration.captures_iter(text) {
        if !reachable_arrays.contains(&captures[1]) {
            continue;
        }
        let whole = captures
            .get(0)
            .ok_or_else(|| "missing collision-array declaration match".to_string())?;
        let body = braced_body(text, whole.end() - 1)
            .ok_or_else(|| format!("unterminated collision array {}", &captures[1]))?;
        let mut resources = Vec::new();
        for entry in entry_re.captures_iter(body) {
            let fields = split_cpp_initializer_fields(&entry[1]);
            if fields.len() < 2 {
                return Err(format!(
                    "collision array {} has an entry with fewer than 2 fields",
                    &captures[1]
                ));
            }
            let resource_name = parse_cpp_string(fields[0]).unwrap_or_default();
            let flags = parse_cpp_u32(fields[1])
                .ok_or_else(|| format!("collision array {} has non-numeric flags", &captures[1]))?;
            let flags = u16::try_from(flags)
                .map_err(|_| format!("collision array {} flags exceed u16", &captures[1]))?;
            resources.push(MapObjCollisionResourceDefinition {
                resource_name,
                flags,
                collision_kind: (flags & 3) as u8,
                max_vertices: None,
            });
        }
        if arrays.insert(captures[1].to_string(), resources).is_some() {
            return Err(format!("duplicate collision array {}", &captures[1]));
        }
    }
    Ok(arrays)
}

fn extract_collision_infos(
    text: &str,
    arrays: &BTreeMap<String, Vec<MapObjCollisionResourceDefinition>>,
    reachable_infos: &BTreeSet<String>,
) -> Result<BTreeMap<String, Vec<MapObjCollisionResourceDefinition>>, String> {
    let declaration =
        Regex::new(r"static\s+const\s+TMapObjCollisionInfo\s+([A-Za-z_][A-Za-z0-9_]*)\s*=\s*\{")
            .expect("valid TMapObjCollisionInfo regex");
    let mut infos = BTreeMap::new();
    for captures in declaration.captures_iter(text) {
        if !reachable_infos.contains(&captures[1]) {
            continue;
        }
        let whole = captures
            .get(0)
            .ok_or_else(|| "missing collision-info declaration match".to_string())?;
        let body = braced_body(text, whole.end() - 1)
            .ok_or_else(|| format!("unterminated collision info {}", &captures[1]))?;
        let fields = split_cpp_initializer_fields(body);
        if fields.len() < 3 {
            return Err(format!(
                "collision info {} has {} fields; expected 3",
                &captures[1],
                fields.len()
            ));
        }
        let count = parse_cpp_u32(fields[0])
            .ok_or_else(|| format!("collision info {} has non-numeric count", &captures[1]))?
            as usize;
        let array_name = cpp_identifier(fields[2]).ok_or_else(|| {
            format!(
                "collision info {} has no collision-data array",
                &captures[1]
            )
        })?;
        let array = arrays.get(array_name).ok_or_else(|| {
            format!(
                "collision info {} references unknown array {array_name}",
                &captures[1]
            )
        })?;
        if count > array.len() {
            return Err(format!(
                "collision info {} requests {count} entries from {array_name}, which has {}",
                &captures[1],
                array.len()
            ));
        }
        infos.insert(
            captures[1].to_string(),
            array[..count]
                .iter()
                .filter(|resource| !resource.resource_name.is_empty())
                .cloned()
                .collect(),
        );
    }
    Ok(infos)
}

fn extract_map_obj_data(
    text: &str,
    infos: &BTreeMap<String, ParsedAnimationInfo>,
    collision_infos: &BTreeMap<String, Vec<MapObjCollisionResourceDefinition>>,
    hold_models: &BTreeMap<String, String>,
    move_bcks: &BTreeMap<String, String>,
    reachable: &BTreeSet<String>,
    loader_policy: MapObjModelLoaderPolicy,
) -> Result<BTreeMap<String, ParsedMapObjData>, String> {
    let declaration = Regex::new(r"static\s+TMapObjData\s+([A-Za-z_][A-Za-z0-9_]*)\s*=\s*\{")
        .expect("valid TMapObjData regex");
    let mut data = BTreeMap::new();
    for captures in declaration.captures_iter(text) {
        if !reachable.contains(&captures[1]) {
            continue;
        }
        let whole = captures
            .get(0)
            .ok_or_else(|| "missing map-object declaration match".to_string())?;
        let body = braced_body(text, whole.end() - 1)
            .ok_or_else(|| format!("unterminated map-object data {}", &captures[1]))?;
        let fields = split_cpp_initializer_fields(body);
        if fields.len() < 15 {
            return Err(format!(
                "map-object data {} has {} fields; expected at least 15",
                &captures[1],
                fields.len()
            ));
        }
        let resource_name = parse_cpp_string(fields[0]);
        let actor_type = parse_cpp_u32(fields[1]).ok_or_else(|| {
            format!(
                "map-object data {} has a non-numeric actor type",
                &captures[1]
            )
        })?;
        let map_obj_flags = parse_cpp_u32(fields[13]).ok_or_else(|| {
            format!(
                "map-object data {} has non-numeric unk34 flags",
                &captures[1]
            )
        })?;
        let required_manager_name = parse_cpp_string(fields[2]);
        let animation_info = cpp_identifier(fields[4]);
        let (primary_model, animation_resources) = match animation_info {
            Some(info_name) => {
                let info = infos.get(info_name).ok_or_else(|| {
                    format!(
                        "map-object data {} references unknown animation info {info_name}",
                        &captures[1]
                    )
                })?;
                let primary_model = (info.actor_count > 0)
                    .then(|| {
                        info.resources
                            .first()
                            .and_then(|resource| resource.model_name.clone())
                    })
                    .flatten();
                (primary_model, info.resources.clone())
            }
            None => (
                resource_name.as_ref().map(|name| format!("{name}.bmd")),
                Vec::new(),
            ),
        };
        let collision_resources = match cpp_identifier(fields[6]) {
            Some(info_name) => collision_infos.get(info_name).cloned().ok_or_else(|| {
                format!(
                    "map-object data {} references unknown collision info {info_name}",
                    &captures[1]
                )
            })?,
            None => Vec::new(),
        };
        let hold_model_path = match cpp_identifier(fields[10]) {
            Some(hold_name) => Some(hold_models.get(hold_name).cloned().ok_or_else(|| {
                format!(
                    "map-object data {} references unknown hold data {hold_name}",
                    &captures[1]
                )
            })?),
            None => None,
        };
        let move_bck_path = match cpp_identifier(fields[11]) {
            Some(move_name) => Some(move_bcks.get(move_name).cloned().ok_or_else(|| {
                format!(
                    "map-object data {} references unknown move data {move_name}",
                    &captures[1]
                )
            })?),
            None => None,
        };
        let record = ParsedMapObjData {
            resource_name,
            actor_type,
            object_flags: map_obj_flags,
            required_manager_name,
            has_hold_dependency: hold_model_path.is_some(),
            has_move_dependency: move_bck_path.is_some(),
            uses_resource_name_model_fallback: animation_info.is_none(),
            primary_model,
            animation_resources,
            hold_model_path,
            move_bck_path,
            load_flags: loader_policy.effective_load_flags(map_obj_flags),
            collision_resources,
            is_terminal: actor_type == 0,
        };
        if data.insert(captures[1].to_string(), record).is_some() {
            return Err(format!("duplicate map-object data {}", &captures[1]));
        }
    }
    Ok(data)
}

fn referenced_anim_infos(
    text: &str,
    reachable_data: &BTreeSet<String>,
) -> Result<BTreeSet<String>, String> {
    let declaration = Regex::new(r"static\s+TMapObjData\s+([A-Za-z_][A-Za-z0-9_]*)\s*=\s*\{")
        .expect("valid TMapObjData regex");
    let mut infos = BTreeSet::new();
    for captures in declaration.captures_iter(text) {
        if !reachable_data.contains(&captures[1]) {
            continue;
        }
        let whole = captures
            .get(0)
            .ok_or_else(|| "missing reachable map-object declaration match".to_string())?;
        let body = braced_body(text, whole.end() - 1)
            .ok_or_else(|| format!("unterminated map-object data {}", &captures[1]))?;
        let fields = split_cpp_initializer_fields(body);
        if fields.len() < 15 {
            return Err(format!(
                "map-object data {} has {} fields; expected at least 15",
                &captures[1],
                fields.len()
            ));
        }
        if let Some(info) = cpp_identifier(fields[4]) {
            infos.insert(info.to_string());
        }
    }
    Ok(infos)
}

fn referenced_anim_arrays(
    text: &str,
    reachable_infos: &BTreeSet<String>,
) -> Result<BTreeSet<String>, String> {
    let declaration =
        Regex::new(r"static\s+const\s+TMapObjAnimDataInfo\s+([A-Za-z_][A-Za-z0-9_]*)\s*=\s*\{")
            .expect("valid TMapObjAnimDataInfo regex");
    let mut arrays = BTreeSet::new();
    for captures in declaration.captures_iter(text) {
        if !reachable_infos.contains(&captures[1]) {
            continue;
        }
        let whole = captures
            .get(0)
            .ok_or_else(|| "missing reachable animation-info declaration match".to_string())?;
        let body = braced_body(text, whole.end() - 1)
            .ok_or_else(|| format!("unterminated animation info {}", &captures[1]))?;
        let fields = split_cpp_initializer_fields(body);
        if fields.len() < 3 {
            return Err(format!(
                "animation info {} has {} fields; expected at least 3",
                &captures[1],
                fields.len()
            ));
        }
        let entry_count = parse_cpp_u32(fields[0]).ok_or_else(|| {
            format!(
                "animation info {} has a non-numeric entry count",
                &captures[1]
            )
        })?;
        if entry_count != 0 {
            let array = cpp_identifier(fields[2]).ok_or_else(|| {
                format!(
                    "animation info {} has entries but no animation array",
                    &captures[1]
                )
            })?;
            arrays.insert(array.to_string());
        }
    }
    Ok(arrays)
}

fn extract_obj_data_table(text: &str) -> Result<Vec<String>, String> {
    let declaration = Regex::new(r"static\s+TMapObjData\s*\*\s*sObjDataTable\s*\[\s*\]\s*=\s*\{")
        .expect("valid sObjDataTable regex");
    let whole = declaration
        .find(text)
        .ok_or_else(|| "missing sObjDataTable".to_string())?;
    let body = braced_body(text, whole.end() - 1)
        .ok_or_else(|| "unterminated sObjDataTable".to_string())?;
    let mut entries = Vec::new();
    for field in split_cpp_initializer_fields(body) {
        let variable = cpp_identifier(field)
            .ok_or_else(|| format!("invalid sObjDataTable entry {field:?}"))?;
        entries.push(variable.to_string());
    }
    if entries.is_empty() {
        return Err("sObjDataTable is empty".to_string());
    }
    Ok(entries)
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &str = r#"
        void TMapObjBase::initActorData() {
            unkF8 = mMapObjData->unk34;
        }
        void TMapObjBase::makeMActors() {
            mMActorKeeper = new TMActorKeeper(mManager, 1);
            if (unkF8 & 0x8000) {
                mMActorKeeper->mModelLoaderFlags = 0x11220000;
            } else {
                mMActorKeeper->mModelLoaderFlags = 0x10220000;
            }
            if (mMapObjData->mAnim) {
                mMActor = initMActor(mMapObjData->mAnim->unk4[0].unk0, nullptr, 0);
            } else {
                char buffer[64];
                snprintf(buffer, 64, "%s.bmd", mMapObjData->unk0);
                mMActor = initMActor(buffer, nullptr, 0);
            }
        }
        static const TMapObjAnimData no_data_anim_data[] = {
            { nullptr, nullptr, 0, nullptr, nullptr },
        };
        static const TMapObjAnimDataInfo no_data_anim_info = {
            0, 0, nullptr,
        };
        static const TMapObjAnimData wood_anim_data[] = {
            { "kibako.bmd", "wood_wait", 0, "wood_extra", "/scene/mapObj/wood_wait.bas" },
            { "kibako_break.bmd", "wood_break", 5, nullptr, nullptr },
            { "ignored_tail.bmd", "ignored_tail", 0, nullptr, nullptr },
        };
        static const TMapObjAnimDataInfo wood_anim_info = {
            2, 2, wood_anim_data,
        };
        static TMapObjHoldData fruit_hold_data = {
            "/scene/mapObj/fruit_offset.bmd", "fruit_center",
        };
        static TMapObjMoveData fruit_move_data = {
            "/scene/mapObj/fruit_move.bck",
        };
        static TMapObjData stray_data = {
            "Stray", 0x40000001, nullptr, nullptr, nullptr, nullptr, nullptr, nullptr,
            nullptr, nullptr, nullptr, nullptr, 0.0f, 0x00000000, 0,
        };
        static TMapObjData fruit_data = {
            "FruitPapaya", 0x40000002, "fruit manager", nullptr, nullptr, nullptr, nullptr, nullptr,
            nullptr, nullptr, &fruit_hold_data, &fruit_move_data, 0.0f, 0x00008000, 0,
        };
        static TMapObjData wood_data = {
            "WoodBox", 0x40000003, "wood manager", nullptr, &wood_anim_info, nullptr, nullptr, nullptr,
            nullptr, nullptr, nullptr, nullptr, 0.0f, 0x00000000, 0,
        };
        static TMapObjData controller_data = {
            "Controller", 0x40000004, "controller manager", nullptr, &no_data_anim_info, nullptr, nullptr,
            nullptr, nullptr, nullptr, nullptr, nullptr, 0.0f, 0x00000000, 0,
        };
        static TMapObjData end_data = {
            nullptr, 0, nullptr, nullptr, &no_data_anim_info, nullptr, nullptr, nullptr,
            nullptr, nullptr, nullptr, nullptr, 0.0f, 0x00000000, 0,
        };
        static TMapObjData* sObjDataTable[] = {
            &wood_data, &fruit_data, &controller_data, &end_data,
        };
    "#;

    #[test]
    fn follows_runtime_table_order_and_model_selection_semantics() {
        let resources = extract_map_obj_resources(FIXTURE, "src/MoveBG/MapObjInit.cpp").unwrap();
        assert_eq!(
            resources
                .iter()
                .map(|resource| resource.resource_name.as_str())
                .collect::<Vec<_>>(),
            ["WoodBox", "FruitPapaya", "Controller"]
        );
        assert_eq!(resources[0].primary_model.as_deref(), Some("kibako.bmd"));
        assert_eq!(resources[0].actor_type, 0x4000_0003);
        assert_eq!(resources[0].object_flags, 0);
        assert_eq!(resources[0].required_manager_name, "wood manager");
        assert!(!resources[0].uses_resource_name_model_fallback);
        assert_eq!(resources[0].load_flags, 0x1022_0000);
        assert_eq!(
            resources[0].animation_resources,
            vec![
                MapObjAnimationResourceDefinition {
                    model_name: Some("kibako.bmd".to_string()),
                    animation_name: Some("wood_wait".to_string()),
                    animation_channel: 0,
                    extra_name: Some("wood_extra".to_string()),
                    bas_path: Some("/scene/mapObj/wood_wait.bas".to_string()),
                },
                MapObjAnimationResourceDefinition {
                    model_name: Some("kibako_break.bmd".to_string()),
                    animation_name: Some("wood_break".to_string()),
                    animation_channel: 5,
                    extra_name: None,
                    bas_path: None,
                },
            ]
        );
        assert_eq!(resources[0].hold_model_path, None);
        assert_eq!(resources[0].move_bck_path, None);
        assert_eq!(
            resources[1].primary_model.as_deref(),
            Some("FruitPapaya.bmd")
        );
        assert_eq!(resources[1].object_flags, 0x0000_8000);
        assert_eq!(resources[1].required_manager_name, "fruit manager");
        assert!(resources[1].has_hold_dependency);
        assert!(resources[1].has_move_dependency);
        assert!(resources[1].uses_resource_name_model_fallback);
        assert_eq!(resources[1].load_flags, 0x1122_0000);
        assert!(resources[1].animation_resources.is_empty());
        assert_eq!(
            resources[1].hold_model_path.as_deref(),
            Some("/scene/mapObj/fruit_offset.bmd")
        );
        assert_eq!(
            resources[1].move_bck_path.as_deref(),
            Some("/scene/mapObj/fruit_move.bck")
        );
        assert_eq!(resources[2].primary_model, None);
        assert_eq!(resources[2].required_manager_name, "controller manager");
        assert!(!resources[2].has_hold_dependency);
        assert!(!resources[2].has_move_dependency);
        assert!(resources[2].animation_resources.is_empty());
        assert!(!resources
            .iter()
            .any(|resource| resource.resource_name == "Stray"));
    }

    #[test]
    fn null_animation_fallback_does_not_hide_hold_or_move_dependencies() {
        let resources = extract_map_obj_resources(FIXTURE, "src/MoveBG/MapObjInit.cpp").unwrap();
        let fruit = resources
            .iter()
            .find(|resource| resource.resource_name == "FruitPapaya")
            .unwrap();
        assert!(fruit.uses_resource_name_model_fallback);
        assert_eq!(fruit.primary_model.as_deref(), Some("FruitPapaya.bmd"));
        assert!(fruit.has_hold_dependency);
        assert!(fruit.has_move_dependency);
        assert_eq!(
            fruit.hold_model_path.as_deref(),
            Some("/scene/mapObj/fruit_offset.bmd")
        );
        assert_eq!(
            fruit.move_bck_path.as_deref(),
            Some("/scene/mapObj/fruit_move.bck")
        );
    }

    #[test]
    fn rejects_table_entries_after_the_runtime_terminator() {
        let invalid = FIXTURE.replace(
            "&wood_data, &fruit_data, &controller_data, &end_data,",
            "&wood_data, &end_data, &fruit_data, &controller_data,",
        );
        let error = extract_map_obj_resources(&invalid, "fixture.cpp").unwrap_err();
        assert!(error.contains("before the end"), "{error}");
    }

    #[test]
    fn ignores_malformed_unregistered_map_obj_declarations() {
        let text = FIXTURE.replace(
            r#"static TMapObjData stray_data = {
            "Stray", 0x40000001, nullptr, nullptr, nullptr, nullptr, nullptr, nullptr,
            nullptr, nullptr, nullptr, nullptr, 0.0f, 0x00000000, 0,
        };"#,
            "static TMapObjData stray_data = { BROKEN_WIP };",
        );
        assert_ne!(text, FIXTURE, "malformed declaration fixture must change");
        let resources = extract_map_obj_resources(&text, "fixture.cpp").unwrap();
        assert_eq!(resources.len(), 3);
    }

    #[test]
    fn rejects_malformed_reachable_map_obj_declarations() {
        let text = FIXTURE.replace(
            r#"static TMapObjData fruit_data = {
            "FruitPapaya", 0x40000002, "fruit manager", nullptr, nullptr, nullptr, nullptr, nullptr,
            nullptr, nullptr, &fruit_hold_data, &fruit_move_data, 0.0f, 0x00008000, 0,
        };"#,
            r#"static TMapObjData fruit_data = { "FruitPapaya" };"#,
        );
        let error = extract_map_obj_resources(&text, "fixture.cpp").unwrap_err();
        assert!(error.contains("fruit_data has 1 fields"), "{error}");
    }

    #[test]
    fn rejects_a_reachable_resource_without_its_runtime_manager_name() {
        let text = FIXTURE.replacen("\"wood manager\"", "nullptr", 1);
        let error = extract_map_obj_resources(&text, "fixture.cpp").unwrap_err();
        assert!(
            error.contains("wood_data has no required TLiveManager name"),
            "{error}"
        );
    }

    #[test]
    fn requires_the_runtime_unk34_loader_policy() {
        let missing_copy = FIXTURE.replace("unkF8 = mMapObjData->unk34;", "unkF8 = 0;");
        let error = extract_map_obj_resources(&missing_copy, "fixture.cpp").unwrap_err();
        assert!(error.contains("copies TMapObjData::unk34"), "{error}");

        let missing_branch = FIXTURE.replace("if (unkF8 & 0x8000)", "if (otherFlags & 0x8000)");
        let error = extract_map_obj_resources(&missing_branch, "fixture.cpp").unwrap_err();
        assert!(
            error.contains("recognizable unkF8 loader policies"),
            "{error}"
        );
    }

    #[test]
    fn recognizes_semantic_null_animation_model_fallback_across_formatting() {
        let text = r#"
            void TMapObjBase::makeMActors() {
                if (mMapObjData->mAnim) {
                    mMActor = initMActor(mMapObjData->mAnim->unk4[0].unk0, nullptr, 0);
                }
                else {
                    char generated[64];
                    snprintf(
                        generated,
                        64,
                        "%s.bmd",
                        mMapObjData->unk0
                    );
                    mMActor = initMActor(generated, nullptr, 0);
                }
            }
        "#;
        assert!(has_null_animation_model_fallback(text));
        assert!(!has_null_animation_model_fallback(
            &text.replace("mMapObjData->unk0", "otherName")
        ));
    }
}
