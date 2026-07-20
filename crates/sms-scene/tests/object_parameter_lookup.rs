use std::collections::BTreeMap;
use std::path::PathBuf;

use sms_formats::{
    JDramaDocument, JDramaField, JDramaFieldValue, JDramaRecord, JDramaRecordPayload,
};
use sms_scene::{
    AuthoredPlacement, PlacementAddress, PlacementBinding, SceneObject, SourceFreeStageArchive,
    StageArchiveEdits, StageDocument, StageLighting, StageResourceDocument,
};

fn record(name: &str, value: u32) -> JDramaRecord {
    JDramaRecord {
        type_name: "TestFields".to_string(),
        name: name.to_string(),
        payload: JDramaRecordPayload::Fields {
            fields: vec![JDramaField {
                name: "value".to_string(),
                value: JDramaFieldValue::U32(value),
            }],
        },
    }
}

fn document_with_archive(archive: SourceFreeStageArchive) -> StageDocument {
    StageDocument {
        stage_id: "lookup".to_string(),
        base_root: PathBuf::from("."),
        assets: Vec::new(),
        objects: Vec::new(),
        changed_files: BTreeMap::new(),
        stage_archive: Some(archive),
        stage_archive_source_path: None,
        archive_edits: StageArchiveEdits::default(),
        registry: None,
        route_authoring: None,
        load_issues: Vec::new(),
        lighting: StageLighting::default(),
        actor_previews: BTreeMap::new(),
        loaded_project: None,
    }
}

#[test]
fn stage_document_resolves_existing_and_authored_parameters() {
    let existing_record = record("existing", 10);
    let root = JDramaRecord {
        type_name: "Strategy".to_string(),
        name: "root".to_string(),
        payload: JDramaRecordPayload::Group {
            fields: Vec::new(),
            children: vec![existing_record],
        },
    };
    let mut archive = SourceFreeStageArchive::new().unwrap();
    archive
        .insert_resource(
            b"scene.bin".to_vec(),
            StageResourceDocument::Placement(JDramaDocument { root }),
        )
        .unwrap();
    let document = document_with_archive(archive);

    let mut existing = SceneObject::new("existing-object", "TestFields");
    existing.placement = Some(PlacementBinding::Existing(PlacementAddress {
        raw_resource_path: b"scene.bin".to_vec(),
        record_path: vec![0],
    }));
    existing.set_raw_param("value", "11");
    let descriptors = document.editable_parameters_for_object(&existing).unwrap();
    assert_eq!(descriptors[0].key, "name");
    assert_eq!(descriptors[0].raw_value, "existing");
    assert_eq!(descriptors[1].key, "value");
    assert_eq!(descriptors[1].raw_value, "11");

    let mut authored = SceneObject::new("authored-object", "TestFields");
    authored.placement = Some(PlacementBinding::Authored(AuthoredPlacement {
        raw_resource_path: b"scene.bin".to_vec(),
        target_group_index: 3,
        prototype: record("authored", 20),
        dependencies: Vec::new(),
    }));
    authored.insert_source_raw_param("value", "21");
    let descriptors = document.editable_parameters_for_object(&authored).unwrap();
    assert_eq!(descriptors[0].raw_value, "authored");
    assert_eq!(descriptors[1].raw_value, "21");
}

#[test]
fn stage_document_rejects_missing_or_invalid_typed_bindings() {
    let document = document_with_archive(SourceFreeStageArchive::new().unwrap());
    let unbound = SceneObject::new("unbound", "TestFields");
    assert!(document
        .editable_parameters_for_object(&unbound)
        .unwrap_err()
        .to_string()
        .contains("no typed placement binding"));

    let mut missing = SceneObject::new("missing", "TestFields");
    missing.placement = Some(PlacementBinding::Existing(PlacementAddress {
        raw_resource_path: b"missing.bin".to_vec(),
        record_path: Vec::new(),
    }));
    assert!(document
        .editable_parameters_for_object(&missing)
        .unwrap_err()
        .to_string()
        .contains("missing placement resource"));
}
