use std::collections::BTreeSet;

use super::{validate_project_relative_path, validate_stage_id, StageDocument, ValidationIssue};

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
    }

    issues
}
