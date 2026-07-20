use std::collections::{BTreeMap, BTreeSet};

use super::{
    validate_project_relative_path, validate_stage_id, StageDocument, StageResourceDocument,
    ValidationIssue, SHINE_QUICK_CAMERA_NAME,
};

const SHINE_CAMERA_RESOURCE_PATHS: &[&[u8]] = &[b"map/scene.bin", b"map/tables.bin"];

fn has_named_record(record: &sms_formats::JDramaRecord, type_name: &str, name: &str) -> bool {
    if record.type_name.rsplit("::").next() == Some(type_name) && record.name == name {
        return true;
    }
    let sms_formats::JDramaRecordPayload::Group { children, .. } = &record.payload else {
        return false;
    };
    children
        .iter()
        .any(|child| has_named_record(child, type_name, name))
}

fn validate_quick_shine_camera(document: &StageDocument) -> Option<ValidationIssue> {
    let mut inspected_placement_resource = false;
    for raw_path in SHINE_CAMERA_RESOURCE_PATHS {
        match document.effective_resource_clone(raw_path) {
            Ok(Some(StageResourceDocument::Placement(resource))) => {
                inspected_placement_resource = true;
                if has_named_record(&resource.root, "CameraMapInfo", SHINE_QUICK_CAMERA_NAME) {
                    return None;
                }
            }
            Ok(Some(_)) => {
                return Some(ValidationIssue::error(
                    "invalid-shine-quick-camera-resource",
                    format!(
                        "Quick-appearance Shines require {} to be placement data",
                        String::from_utf8_lossy(raw_path)
                    ),
                ));
            }
            Ok(None) => {}
            Err(error) => {
                return Some(ValidationIssue::error(
                    "invalid-shine-quick-camera-resource",
                    format!(
                        "Could not inspect {} for the quick-appearance Shine camera: {error}",
                        String::from_utf8_lossy(raw_path)
                    ),
                ));
            }
        }
    }
    Some(ValidationIssue::error(
        "missing-shine-quick-camera",
        if inspected_placement_resource {
            format!(
                "Quick-appearance Shines require retail CameraMapInfo {:?}; reopen the stage so the object catalog can repair its runtime dependencies",
                SHINE_QUICK_CAMERA_NAME
            )
        } else {
            "Quick-appearance Shines require a camera table, but neither map/scene.bin nor map/tables.bin is available"
                .to_string()
        },
    ))
}

pub(super) fn validate_document(document: &StageDocument) -> Vec<ValidationIssue> {
    let mut issues = document.load_issues.clone();

    if !document.base_root.exists() {
        issues.push(ValidationIssue::error(
            "missing-base-root",
            format!("Base root does not exist: {}", document.base_root.display()),
        ));
    }

    if document.assets.is_empty() {
        issues.push(ValidationIssue::warning(
            "no-stage-assets",
            format!("No assets found for stage '{}'", document.stage_id),
        ));
    }

    if document.lighting.object_lighting_uses_ordinal_fallback() {
        issues.push(ValidationIssue::warning(
            "ordinal-object-lighting-fallback",
            "Object lighting was selected by retail table position because semantic runtime names were unavailable",
        ));
    }

    if validate_stage_id(&document.stage_id).is_err() {
        issues.push(ValidationIssue::error(
            "invalid-stage-id",
            format!(
                "Stage id '{}' is not safe for project output",
                document.stage_id
            ),
        ));
    }

    for path in document.changed_files.keys() {
        if validate_project_relative_path(path).is_err() {
            issues.push(ValidationIssue::error(
                "unsafe-project-path",
                format!("Changed file path is unsafe: {}", path.display()),
            ));
        }
    }

    let mut object_ids = BTreeSet::new();
    let mut authored_shines_by_flag = BTreeMap::<i32, Vec<String>>::new();
    let mut has_quick_authored_shine = false;
    for object in &document.objects {
        if object.id.trim().is_empty() {
            issues.push(ValidationIssue::error(
                "empty-object-id",
                "Scene objects must have a non-empty id",
            ));
        }
        if !object_ids.insert(object.id.as_str()) {
            issues.push(ValidationIssue::error(
                "duplicate-object-id",
                format!("Object id '{}' is duplicated", object.id),
            ));
        }
        if object.factory_name.trim().is_empty() {
            issues.push(ValidationIssue::error(
                "empty-factory-name",
                format!("Object {} has no factory name", object.id),
            ));
        }

        if !object.transform.is_finite() {
            issues.push(ValidationIssue::error(
                "invalid-transform",
                format!("Object {} has a non-finite transform", object.id),
            ));
        }
        if object
            .transform
            .scale
            .iter()
            .any(|value| value.abs() <= f32::EPSILON)
        {
            issues.push(ValidationIssue::warning(
                "zero-scale",
                format!("Object {} has a non-invertible scale", object.id),
            ));
        }

        if let Some(registry) = &document.registry {
            if registry.find_object(&object.factory_name).is_none() && object.source.is_none() {
                issues.push(ValidationIssue::warning(
                    "unknown-factory",
                    format!(
                        "Object '{}' is not in the generated registry",
                        object.factory_name
                    ),
                ));
            }
        }

        let is_authored_shine = matches!(
            &object.placement,
            Some(super::PlacementBinding::Authored(authored))
                if authored.prototype.type_name.rsplit("::").next() == Some("Shine")
        );
        if !is_authored_shine {
            continue;
        }

        match object.raw_param("collection_type") {
            Some("normal") => {}
            Some("quickly") => has_quick_authored_shine = true,
            Some(mode) => issues.push(ValidationIssue::warning(
                "shine-requires-external-trigger",
                format!(
                    "Authored Shine '{}' uses collection_type '{mode}', so Sunshine creates it dormant until an external event triggers it; use 'normal' for an immediately visible standalone Shine",
                    object.id
                ),
            )),
            None => issues.push(ValidationIssue::warning(
                "missing-shine-collection-type",
                format!(
                    "Authored Shine '{}' has no collection_type; use 'normal' for an immediately visible standalone Shine",
                    object.id
                ),
            )),
        }

        match object
            .raw_param("shine_id")
            .and_then(|value| value.parse::<i32>().ok())
        {
            Some(shine_id @ -1..=119) => {
                let effective_flag = if shine_id == -1 { 0 } else { shine_id };
                authored_shines_by_flag
                    .entry(effective_flag)
                    .or_default()
                    .push(object.id.clone());
            }
            Some(shine_id) => issues.push(ValidationIssue::warning(
                "invalid-shine-id",
                format!(
                    "Authored Shine '{}' has shine_id {shine_id}; use -1 or 0 through 119 (the runtime folds -1/120+ onto flag 0)",
                    object.id
                ),
            )),
            None => issues.push(ValidationIssue::warning(
                "invalid-shine-id",
                format!(
                    "Authored Shine '{}' has no valid integer shine_id; use -1 or 0 through 119",
                    object.id
                ),
            )),
        }

        match object
            .raw_param("in_stage")
            .and_then(|value| value.parse::<i32>().ok())
        {
            Some(-1 | 0) => {}
            Some(in_stage) => issues.push(ValidationIssue::warning(
                "invalid-shine-camera-mode",
                format!(
                    "Authored Shine '{}' has in_stage {in_stage}; use -1 for the outside collection camera or 0 for the inside camera",
                    object.id
                ),
            )),
            None => issues.push(ValidationIssue::warning(
                "invalid-shine-camera-mode",
                format!(
                    "Authored Shine '{}' has no valid integer in_stage; use -1 for outside or 0 for inside",
                    object.id
                ),
            )),
        }
    }

    if has_quick_authored_shine {
        if let Some(issue) = validate_quick_shine_camera(document) {
            issues.push(issue);
        }
    }

    for (shine_flag, object_ids) in authored_shines_by_flag {
        if object_ids.len() > 1 {
            issues.push(ValidationIssue::warning(
                "duplicate-authored-shine-id",
                format!(
                    "Authored Shines {} share persistent Shine flag {shine_flag}; collecting one will mark all of them collected",
                    object_ids.join(", ")
                ),
            ));
        }
    }

    issues
}
