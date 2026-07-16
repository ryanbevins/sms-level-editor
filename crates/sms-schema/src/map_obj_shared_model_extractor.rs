use std::collections::{BTreeMap, BTreeSet};

use regex::Regex;

use super::{braced_body, parse_cpp_u32, MapObjModelOverrideDefinition, MapObjTevColorDefinition};

#[derive(Debug, Clone)]
pub(super) struct SharedModelLoader {
    pub model_path: String,
    pub load_flags: u32,
}

pub(super) fn extract_shared_model_loaders(text: &str) -> BTreeMap<String, SharedModelLoader> {
    let loader_re = Regex::new(
        r#"(?s)([A-Za-z_][A-Za-z0-9_]*)\s*=\s*SMS_MakeSDLModelData\s*\(\s*"([^"]+\.bmd)"\s*,\s*([^,)]+)\s*\)"#,
    )
    .expect("valid shared SDL model loader regex");
    loader_re
        .captures_iter(text)
        .filter_map(|captures| {
            Some((
                captures[1].to_string(),
                SharedModelLoader {
                    model_path: captures[2].to_string(),
                    load_flags: parse_cpp_u32(&captures[3])?,
                },
            ))
        })
        .collect()
}

pub(super) fn extract_map_obj_shared_models(
    text: &str,
    binding_source_file: &str,
    model_source_file: &str,
    loaders: &BTreeMap<String, SharedModelLoader>,
) -> Result<Vec<MapObjModelOverrideDefinition>, String> {
    let method_re = Regex::new(r"void\s+([A-Za-z_][A-Za-z0-9_]*)::initMapObj\s*\([^)]*\)\s*\{")
        .expect("valid initMapObj method regex");
    let manager_assignment_re = Regex::new(
        r"(?:SDLModelData\s*\*\s*)?([A-Za-z_][A-Za-z0-9_]*)\s*=\s*gpMapObjManager->([A-Za-z_][A-Za-z0-9_]*)\s*;",
    )
    .expect("valid manager model-data assignment regex");
    let packet_color_re = Regex::new(
        r"(?s)initPacketMatColor\s*\([^;]*?GX_TEVREG([0-3])\s*,\s*&([A-Za-z_][A-Za-z0-9_]*)\s*\)",
    )
    .expect("valid packet color regex");
    let branch_re = Regex::new(
        r#"(?:if|else\s+if)\s*\(\s*strcmp\s*\(\s*unkF4\s*,\s*"([^"]+)"\s*\)\s*==\s*0\s*\)\s*\{"#,
    )
    .expect("valid resource branch regex");
    let mut definitions = Vec::new();

    for captures in method_re.captures_iter(text) {
        let whole = captures
            .get(0)
            .ok_or_else(|| "missing initMapObj method match".to_string())?;
        let body = braced_body(text, whole.end() - 1)
            .ok_or_else(|| format!("unterminated {}::initMapObj", &captures[1]))?;
        let mut loader_fields = BTreeSet::new();
        for assignment in manager_assignment_re.captures_iter(body) {
            let local = &assignment[1];
            let field = &assignment[2];
            if !loaders.contains_key(field) {
                continue;
            }
            let construction_re = Regex::new(&format!(
                r"mMActor\s*=\s*SMS_MakeMActorFromSDLModelData\s*\(\s*{}\b",
                regex::escape(local)
            ))
            .expect("valid shared model construction dataflow regex");
            if construction_re.is_match(body) {
                loader_fields.insert(field.to_string());
            }
        }
        if loader_fields.is_empty() {
            continue;
        }
        if loader_fields.len() != 1 {
            return Err(format!(
                "{}::initMapObj references multiple shared model loaders: {loader_fields:?}",
                &captures[1]
            ));
        }
        let loader_field = loader_fields
            .iter()
            .next()
            .expect("one loader field after length check");
        let loader = &loaders[loader_field];
        let packet_color = packet_color_re.captures(body).ok_or_else(|| {
            format!(
                "{}::initMapObj uses {loader_field} without a packet color binding",
                &captures[1]
            )
        })?;
        let tev_register = packet_color[1]
            .parse::<u8>()
            .map_err(|_| format!("invalid TEV register in {}::initMapObj", &captures[1]))?;
        let color_variable = &packet_color[2];
        let assignment_re = Regex::new(&format!(
            r"\b{}\.(r|g|b|a)\s*=\s*(0[xX][0-9A-Fa-f]+|[0-9]+)",
            regex::escape(color_variable)
        ))
        .expect("valid shared-model color assignment regex");

        for branch in branch_re.captures_iter(body) {
            let branch_match = branch
                .get(0)
                .ok_or_else(|| "missing resource branch match".to_string())?;
            let branch_body = braced_body(body, branch_match.end() - 1)
                .ok_or_else(|| format!("unterminated resource branch {}", &branch[1]))?;
            let mut color = [None; 4];
            for assignment in assignment_re.captures_iter(branch_body) {
                let channel = match &assignment[1] {
                    "r" => 0,
                    "g" => 1,
                    "b" => 2,
                    "a" => 3,
                    _ => unreachable!("color assignment regex restricts channels"),
                };
                color[channel] =
                    parse_cpp_u32(&assignment[2]).and_then(|value| i16::try_from(value).ok());
            }
            let [Some(r), Some(g), Some(b), Some(a)] = color else {
                return Err(format!(
                    "resource {} in {}::initMapObj has an incomplete {} color",
                    &branch[1], &captures[1], color_variable
                ));
            };
            definitions.push(MapObjModelOverrideDefinition {
                resource_name: branch[1].to_string(),
                class_name: captures[1].to_string(),
                model_path: loader.model_path.clone(),
                load_flags: loader.load_flags,
                tev_color: Some(MapObjTevColorDefinition {
                    register: tev_register,
                    color: [r, g, b, a],
                }),
                binding_source_file: binding_source_file.to_string(),
                model_source_file: model_source_file.to_string(),
            });
        }
    }
    Ok(definitions)
}

#[derive(Debug, Clone)]
pub(super) struct DirectClassModelOverride {
    pub class_name: String,
    pub model_path: String,
    pub load_flags: u32,
    pub source_file: String,
}

pub(super) fn extract_direct_make_mactors_overrides(
    text: &str,
    source_file: &str,
) -> Result<Vec<DirectClassModelOverride>, String> {
    let method_re = Regex::new(r"void\s+([A-Za-z_][A-Za-z0-9_]*)::makeMActors\s*\([^)]*\)\s*\{")
        .expect("valid makeMActors method regex");
    let flags_re = Regex::new(r"mModelLoaderFlags\s*=\s*([^;]+)")
        .expect("valid direct model loader flags regex");
    let model_re = Regex::new(r#"mMActor\s*=\s*initMActor\s*\(\s*"([^"]+\.bmd)""#)
        .expect("valid direct initMActor model regex");
    let else_re = Regex::new(r"\belse\s*\{").expect("valid else-branch regex");
    let mut overrides = Vec::new();
    for captures in method_re.captures_iter(text) {
        let whole = captures
            .get(0)
            .ok_or_else(|| "missing makeMActors method match".to_string())?;
        let body = braced_body(text, whole.end() - 1)
            .ok_or_else(|| format!("unterminated {}::makeMActors", &captures[1]))?;
        let Some(flags) = flags_re.captures(body) else {
            continue;
        };
        let Some(load_flags) = parse_cpp_u32(&flags[1]) else {
            return Err(format!(
                "{}::makeMActors has non-numeric model loader flags",
                &captures[1]
            ));
        };
        let mut normal_models = Vec::new();
        for else_branch in else_re.find_iter(body) {
            let else_body = braced_body(body, else_branch.end() - 1).ok_or_else(|| {
                format!("unterminated else branch in {}::makeMActors", &captures[1])
            })?;
            normal_models.extend(
                model_re
                    .captures_iter(else_body)
                    .map(|model| model[1].to_string()),
            );
        }
        if normal_models.is_empty() {
            continue;
        }
        if normal_models.len() != 1 {
            return Err(format!(
                "{}::makeMActors has {} literal models in normal else branches; expected one",
                &captures[1],
                normal_models.len()
            ));
        }
        overrides.push(DirectClassModelOverride {
            class_name: captures[1].to_string(),
            model_path: normal_models.pop().expect("one normal model after check"),
            load_flags,
            source_file: source_file.to_string(),
        });
    }
    Ok(overrides)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn joins_resource_branches_to_manager_shared_model_and_colors() {
        let manager = r#"
            mShared = SMS_MakeSDLModelData("/scene/mapObj/shared.bmd", 0x10220000);
        "#;
        let actor = r#"
            void TSharedActor::initMapObj() {
                if (strcmp(unkF4, "SharedRed") == 0) {
                    tint.r = 0xFF; tint.g = 0x40; tint.b = 0x20; tint.a = 0xFF;
                }
                SDLModelData* modelData = gpMapObjManager->mShared;
                mMActor = SMS_MakeMActorFromSDLModelData(modelData, animationData, 3);
                initPacketMatColor(getModel(), GX_TEVREG1, &tint);
            }
        "#;
        let loaders = extract_shared_model_loaders(manager);
        let definitions = extract_map_obj_shared_models(
            actor,
            "src/MoveBG/Actor.cpp",
            "src/MoveBG/MapObjManager.cpp",
            &loaders,
        )
        .unwrap();
        assert_eq!(definitions.len(), 1);
        assert_eq!(definitions[0].resource_name, "SharedRed");
        assert_eq!(definitions[0].class_name, "TSharedActor");
        assert_eq!(definitions[0].model_path, "/scene/mapObj/shared.bmd");
        assert_eq!(definitions[0].load_flags, 0x1022_0000);
        assert_eq!(
            definitions[0].tev_color,
            Some(MapObjTevColorDefinition {
                register: 1,
                color: [255, 64, 32, 255]
            })
        );
    }

    #[test]
    fn extracts_normal_model_from_direct_make_mactors_override() {
        let text = r#"
            void TSpecial::makeMActors() {
                mMActorKeeper->mModelLoaderFlags = 0x10220000;
                if (special) {
                    mMActor = initMActor("special_empty.bmd", nullptr, 0);
                } else {
                    mMActor = initMActor("special.bmd", nullptr, 0);
                }
                initMActor("later_but_not_normal.bmd", nullptr, 0);
            }
        "#;
        let overrides = extract_direct_make_mactors_overrides(text, "src/MoveBG/Special.cpp")
            .expect("extract direct override");
        assert_eq!(overrides.len(), 1);
        assert_eq!(overrides[0].class_name, "TSpecial");
        assert_eq!(overrides[0].model_path, "special.bmd");
        assert_eq!(overrides[0].load_flags, 0x1022_0000);
    }

    #[test]
    fn ignores_shared_manager_field_reads_without_actor_construction() {
        let manager = r#"
            mShared = SMS_MakeSDLModelData("/scene/mapObj/shared.bmd", 0x10220000);
        "#;
        let actor = r#"
            void TUnrelated::initMapObj() {
                if (strcmp(unkF4, "Unrelated") == 0) {
                    tint.r = 1; tint.g = 2; tint.b = 3; tint.a = 4;
                }
                SDLModelData* modelData = gpMapObjManager->mShared;
                inspect(modelData);
                initPacketMatColor(getModel(), GX_TEVREG1, &tint);
            }
        "#;
        let definitions = extract_map_obj_shared_models(
            actor,
            "src/MoveBG/Actor.cpp",
            "src/MoveBG/MapObjManager.cpp",
            &extract_shared_model_loaders(manager),
        )
        .expect("unrelated reads are not extraction failures");
        assert!(definitions.is_empty());
    }

    #[test]
    fn rejects_ambiguous_normal_make_mactors_branch() {
        let text = r#"
            void TAmbiguous::makeMActors() {
                mMActorKeeper->mModelLoaderFlags = 0x10220000;
                if (special) {
                    mMActor = initMActor("special.bmd", nullptr, 0);
                } else {
                    mMActor = initMActor("normal_a.bmd", nullptr, 0);
                    mMActor = initMActor("normal_b.bmd", nullptr, 0);
                }
            }
        "#;
        let error = extract_direct_make_mactors_overrides(text, "fixture.cpp").unwrap_err();
        assert!(error.contains("expected one"), "{error}");
    }
}
