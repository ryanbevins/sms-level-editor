use std::collections::{BTreeMap, BTreeSet};

use regex::Regex;

use super::{
    braced_body, cpp_identifier, parse_cpp_string, parse_cpp_u32, split_cpp_initializer_fields,
    MapObjResourceDefinition,
};

#[derive(Debug)]
struct ParsedMapObjData {
    resource_name: Option<String>,
    actor_type: u32,
    primary_model: Option<String>,
    load_flags: u32,
    is_terminal: bool,
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
    let anim_arrays = extract_anim_array_primary_models(text, &reachable_arrays)?;
    let anim_infos = extract_anim_info_primary_models(text, &anim_arrays, &reachable_infos)?;
    let data = extract_map_obj_data(text, &anim_infos, &reachable_data, loader_policy)?;

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
            primary_model: record.primary_model.clone(),
            load_flags: record.load_flags,
            source_file: source_file.to_string(),
        });
    }
    if !saw_terminal {
        return Err("sObjDataTable has no final zero-type terminator".to_string());
    }
    Ok(definitions)
}

fn extract_anim_array_primary_models(
    text: &str,
    reachable: &BTreeSet<String>,
) -> Result<BTreeMap<String, Option<String>>, String> {
    let declaration = Regex::new(
        r"static\s+const\s+TMapObjAnimData\s+([A-Za-z_][A-Za-z0-9_]*)\s*\[\s*\]\s*=\s*\{",
    )
    .expect("valid TMapObjAnimData regex");
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
        let first_entry_open = body
            .find('{')
            .ok_or_else(|| format!("animation array {} has no entries", &captures[1]))?;
        let first_entry = braced_body(body, first_entry_open).ok_or_else(|| {
            format!(
                "unterminated first entry in animation array {}",
                &captures[1]
            )
        })?;
        let fields = split_cpp_initializer_fields(first_entry);
        let primary_model = fields.first().and_then(|field| parse_cpp_string(field));
        if arrays
            .insert(captures[1].to_string(), primary_model)
            .is_some()
        {
            return Err(format!("duplicate animation array {}", &captures[1]));
        }
    }
    Ok(arrays)
}

fn extract_anim_info_primary_models(
    text: &str,
    arrays: &BTreeMap<String, Option<String>>,
    reachable: &BTreeSet<String>,
) -> Result<BTreeMap<String, Option<String>>, String> {
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
        let actor_count = parse_cpp_u32(fields[1]).ok_or_else(|| {
            format!(
                "animation info {} has a non-numeric actor count",
                &captures[1]
            )
        })?;
        let primary_model = if actor_count == 0 {
            None
        } else {
            let array_name = cpp_identifier(fields[2]).ok_or_else(|| {
                format!(
                    "animation info {} has actors but no animation array",
                    &captures[1]
                )
            })?;
            arrays
                .get(array_name)
                .ok_or_else(|| {
                    format!(
                        "animation info {} references unknown array {array_name}",
                        &captures[1]
                    )
                })?
                .clone()
                .ok_or_else(|| {
                    format!(
                        "animation info {} has actors but its first model is null",
                        &captures[1]
                    )
                })
                .map(Some)?
        };
        if infos
            .insert(captures[1].to_string(), primary_model)
            .is_some()
        {
            return Err(format!("duplicate animation info {}", &captures[1]));
        }
    }
    Ok(infos)
}

fn extract_map_obj_data(
    text: &str,
    infos: &BTreeMap<String, Option<String>>,
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
        let primary_model = match cpp_identifier(fields[4]) {
            Some(info_name) => infos.get(info_name).cloned().ok_or_else(|| {
                format!(
                    "map-object data {} references unknown animation info {info_name}",
                    &captures[1]
                )
            })?,
            None => resource_name.as_ref().map(|name| format!("{name}.bmd")),
        };
        let record = ParsedMapObjData {
            resource_name,
            actor_type,
            primary_model,
            load_flags: loader_policy.effective_load_flags(map_obj_flags),
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
        let actor_count = parse_cpp_u32(fields[1]).ok_or_else(|| {
            format!(
                "animation info {} has a non-numeric actor count",
                &captures[1]
            )
        })?;
        if actor_count != 0 {
            let array = cpp_identifier(fields[2]).ok_or_else(|| {
                format!(
                    "animation info {} has actors but no animation array",
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
            { "kibako.bmd", nullptr, 0, nullptr, nullptr },
        };
        static const TMapObjAnimDataInfo wood_anim_info = {
            1, 1, wood_anim_data,
        };
        static TMapObjData stray_data = {
            "Stray", 0x40000001, nullptr, nullptr, nullptr, nullptr, nullptr, nullptr,
            nullptr, nullptr, nullptr, nullptr, 0.0f, 0x00000000, 0,
        };
        static TMapObjData fruit_data = {
            "FruitPapaya", 0x40000002, nullptr, nullptr, nullptr, nullptr, nullptr, nullptr,
            nullptr, nullptr, nullptr, nullptr, 0.0f, 0x00008000, 0,
        };
        static TMapObjData wood_data = {
            "WoodBox", 0x40000003, nullptr, nullptr, &wood_anim_info, nullptr, nullptr, nullptr,
            nullptr, nullptr, nullptr, nullptr, 0.0f, 0x00000000, 0,
        };
        static TMapObjData controller_data = {
            "Controller", 0x40000004, nullptr, nullptr, &no_data_anim_info, nullptr, nullptr,
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
        assert_eq!(resources[0].load_flags, 0x1022_0000);
        assert_eq!(
            resources[1].primary_model.as_deref(),
            Some("FruitPapaya.bmd")
        );
        assert_eq!(resources[1].load_flags, 0x1122_0000);
        assert_eq!(resources[2].primary_model, None);
        assert!(!resources
            .iter()
            .any(|resource| resource.resource_name == "Stray"));
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
            "FruitPapaya", 0x40000002, nullptr, nullptr, nullptr, nullptr, nullptr, nullptr,
            nullptr, nullptr, nullptr, nullptr, 0.0f, 0x00008000, 0,
        };"#,
            r#"static TMapObjData fruit_data = { "FruitPapaya" };"#,
        );
        let error = extract_map_obj_resources(&text, "fixture.cpp").unwrap_err();
        assert!(error.contains("fruit_data has 1 fields"), "{error}");
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
