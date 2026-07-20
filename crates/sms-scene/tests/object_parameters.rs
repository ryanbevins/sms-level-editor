use sms_formats::{
    JDramaDocument, JDramaField, JDramaFieldValue, JDramaLightMap, JDramaRecord,
    JDramaRecordPayload, JDramaTransform,
};
use sms_scene::{
    apply_object_parameter_edits, editable_object_parameters, seed_scene_object_parameters,
    sync_scene_object_parameter_aliases, ObjectParameterKind, ParameterApplyMode, SceneObject,
};

fn actor_record(fields: Vec<JDramaField>) -> JDramaRecord {
    JDramaRecord {
        type_name: "TestActor".to_string(),
        name: "source name".to_string(),
        payload: JDramaRecordPayload::Actor {
            transform: JDramaTransform {
                translation: [0.0; 3],
                rotation: [0.0; 3],
                scale: [1.0; 3],
            },
            character_name: "source character".to_string(),
            light_map: JDramaLightMap::default(),
            fields,
        },
    }
}

fn field(name: &str, value: JDramaFieldValue) -> JDramaField {
    JDramaField {
        name: name.to_string(),
        value,
    }
}

#[test]
fn descriptors_and_edits_cover_every_editable_value_kind_in_order() {
    let mut record = actor_record(vec![
        field("unsigned", JDramaFieldValue::U32(1)),
        field("signed", JDramaFieldValue::I32(-2)),
        field("scalar", JDramaFieldValue::F32(3.5)),
        field("pair", JDramaFieldValue::Vec2F32([4.0, 5.0])),
        field("triple", JDramaFieldValue::Vec3F32([6.0, 7.0, 8.0])),
        field("color", JDramaFieldValue::ColorRgba8([9, 10, 11, 12])),
        field("text", JDramaFieldValue::String("source text".to_string())),
        field(
            "light_map",
            JDramaFieldValue::LightMap(JDramaLightMap::default()),
        ),
    ]);
    let descriptors = editable_object_parameters(&record).unwrap();
    assert_eq!(
        descriptors
            .iter()
            .map(|descriptor| (descriptor.key.as_str(), descriptor.kind))
            .collect::<Vec<_>>(),
        vec![
            ("name", ObjectParameterKind::String),
            ("character_name", ObjectParameterKind::String),
            ("unsigned", ObjectParameterKind::U32),
            ("signed", ObjectParameterKind::I32),
            ("scalar", ObjectParameterKind::F32),
            ("pair", ObjectParameterKind::Vec2F32),
            ("triple", ObjectParameterKind::Vec3F32),
            ("color", ObjectParameterKind::ColorRgba8),
            ("text", ObjectParameterKind::String),
        ]
    );
    assert_eq!(descriptors[5].raw_value, "4,5");
    assert_eq!(descriptors[7].raw_value, "9,10,11,12");

    let mut object = SceneObject::new("edited", "TestActor");
    for (key, value) in [
        ("name", "edited name"),
        ("character_name", "edited character"),
        ("unsigned", "4294967295"),
        ("signed", "-2147483648"),
        ("scalar", "1.25"),
        ("pair", "2.5,-3.5"),
        ("triple", "4,5,6"),
        ("color", "255,128,64,32"),
        ("text", "edited text"),
    ] {
        object.set_raw_param(key, value);
    }
    apply_object_parameter_edits(&mut record, &object, ParameterApplyMode::DirtyOnly).unwrap();

    assert_eq!(record.name, "edited name");
    let JDramaRecordPayload::Actor {
        character_name,
        fields,
        ..
    } = record.payload
    else {
        panic!("expected actor");
    };
    assert_eq!(character_name, "edited character");
    assert_eq!(fields[0].value, JDramaFieldValue::U32(u32::MAX));
    assert_eq!(fields[1].value, JDramaFieldValue::I32(i32::MIN));
    assert_eq!(fields[2].value, JDramaFieldValue::F32(1.25));
    assert_eq!(fields[3].value, JDramaFieldValue::Vec2F32([2.5, -3.5]));
    assert_eq!(fields[4].value, JDramaFieldValue::Vec3F32([4.0, 5.0, 6.0]));
    assert_eq!(
        fields[5].value,
        JDramaFieldValue::ColorRgba8([255, 128, 64, 32])
    );
    assert_eq!(
        fields[6].value,
        JDramaFieldValue::String("edited text".to_string())
    );
    assert!(matches!(fields[7].value, JDramaFieldValue::LightMap(_)));
}

#[test]
fn typed_transform_fields_are_not_exposed_as_parameters() {
    for (type_name, names) in [
        (
            "AreaCylinder",
            ["center", "authoring_vector", "cylinder_parameters"],
        ),
        (
            "JDrama::Generator",
            ["position", "rotation", "authoring_vector"],
        ),
    ] {
        let record = JDramaRecord {
            type_name: type_name.to_string(),
            name: "typed transform".to_string(),
            payload: JDramaRecordPayload::Fields {
                fields: vec![
                    field(names[0], JDramaFieldValue::Vec3F32([1.0; 3])),
                    field(names[1], JDramaFieldValue::Vec3F32([2.0; 3])),
                    field(names[2], JDramaFieldValue::Vec3F32([3.0; 3])),
                    field("editable", JDramaFieldValue::U32(7)),
                ],
            },
        };
        let descriptors = editable_object_parameters(&record).unwrap();
        assert_eq!(
            descriptors
                .iter()
                .map(|descriptor| descriptor.key.as_str())
                .collect::<Vec<_>>(),
            vec!["name", "editable"]
        );
    }
}

#[test]
fn seeding_and_syncing_refresh_clean_preview_aliases() {
    let fields = [
        ("resource_name", "MapResource"),
        ("item_selector", "Rocket"),
        ("body_color_index", "1"),
        ("cloth_color_index", "2"),
        ("pollution_amount", "3"),
        ("parts_color_index_0", "4"),
        ("parts_color_index_1", "5"),
        ("parts_color_index_2", "6"),
        ("parts_mask", "7"),
        ("action_flags", "8"),
        ("blade_count", "9"),
    ]
    .into_iter()
    .map(|(name, value)| {
        let value = if name == "resource_name" || name == "item_selector" {
            JDramaFieldValue::String(value.to_string())
        } else {
            JDramaFieldValue::I32(value.parse().unwrap())
        };
        field(name, value)
    })
    .collect();
    let record = actor_record(fields);
    let mut object = SceneObject::new("aliases", "TestActor");
    seed_scene_object_parameters(&mut object, &record).unwrap();

    for (alias, expected) in [
        ("stream_string_0", "source character"),
        ("actor_tail_string", "MapResource"),
        ("nozzle_box_item", "Rocket"),
        ("npc_body_color_index", "1"),
        ("npc_cloth_color_index", "2"),
        ("npc_pollution_amount", "3"),
        ("npc_parts_color_index_0", "4"),
        ("npc_parts_color_index_1", "5"),
        ("npc_parts_color_index_2", "6"),
        ("npc_parts_mask", "7"),
        ("npc_action_flags", "8"),
        ("grass_blade_count", "9"),
    ] {
        let parameter = object.raw_params.get(alias).unwrap();
        assert_eq!(parameter.raw(), expected);
        assert!(!parameter.is_dirty(), "alias {alias} must stay clean");
    }

    object.set_raw_param("resource_name", "ChangedResource");
    object.set_raw_param("body_color_index", "42");
    sync_scene_object_parameter_aliases(&mut object);
    assert_eq!(
        object.raw_param("actor_tail_string"),
        Some("ChangedResource")
    );
    assert_eq!(object.raw_param("npc_body_color_index"), Some("42"));
    assert!(!object.raw_params["actor_tail_string"].is_dirty());
    assert!(!object.raw_params["npc_body_color_index"].is_dirty());
}

#[test]
fn all_canonical_mode_applies_values_deserialized_as_clean() {
    let mut object = SceneObject::new("persisted", "TestActor");
    object.set_raw_param("unsigned", "41");
    let serialized = serde_json::to_vec(&object).unwrap();
    let object: SceneObject = serde_json::from_slice(&serialized).unwrap();
    assert!(!object.raw_params["unsigned"].is_dirty());

    let prototype = actor_record(vec![field("unsigned", JDramaFieldValue::U32(1))]);
    let mut dirty_only = prototype.clone();
    apply_object_parameter_edits(&mut dirty_only, &object, ParameterApplyMode::DirtyOnly).unwrap();
    assert_eq!(dirty_only, prototype);

    let mut all = prototype;
    apply_object_parameter_edits(&mut all, &object, ParameterApplyMode::AllCanonical).unwrap();
    let JDramaRecordPayload::Actor { fields, .. } = all.payload else {
        panic!("expected actor");
    };
    assert_eq!(fields[0].value, JDramaFieldValue::U32(41));
}

#[test]
fn invalid_values_unknown_keys_and_duplicates_are_rejected_atomically() {
    let prototype = actor_record(vec![
        field("unsigned", JDramaFieldValue::U32(1)),
        field("signed", JDramaFieldValue::I32(2)),
        field("scalar", JDramaFieldValue::F32(3.0)),
        field("pair", JDramaFieldValue::Vec2F32([4.0, 5.0])),
        field("triple", JDramaFieldValue::Vec3F32([6.0, 7.0, 8.0])),
        field("color", JDramaFieldValue::ColorRgba8([9, 10, 11, 12])),
        field("text", JDramaFieldValue::String("valid".to_string())),
    ]);
    for (key, value) in [
        ("unsigned", "-1"),
        ("signed", "2147483648"),
        ("scalar", "NaN"),
        ("pair", "1"),
        ("triple", "1,2,inf"),
        ("color", "0,1,2,256"),
        ("text", "not Shift-JIS \u{1f600}"),
        ("actor_tail_string", "synthetic edit"),
    ] {
        let mut record = prototype.clone();
        let mut object = SceneObject::new("invalid", "TestActor");
        object.set_raw_param(key, value);
        let error =
            apply_object_parameter_edits(&mut record, &object, ParameterApplyMode::DirtyOnly)
                .unwrap_err();
        assert!(error.to_string().contains("object parameter edit failed"));
        assert_eq!(record, prototype, "invalid {key} edit mutated the record");
    }

    let duplicate = actor_record(vec![
        field("same", JDramaFieldValue::U32(1)),
        field("same", JDramaFieldValue::U32(2)),
    ]);
    let error = editable_object_parameters(&duplicate).unwrap_err();
    assert!(error.to_string().contains("duplicate or ambiguous"));
}

#[test]
fn edited_record_round_trips_through_strict_jdrama_encoding() {
    let mut record = JDramaRecord {
        type_name: "Light".to_string(),
        name: "source light".to_string(),
        payload: JDramaRecordPayload::Fields {
            fields: vec![
                field("position", JDramaFieldValue::Vec3F32([0.0; 3])),
                field("color", JDramaFieldValue::ColorRgba8([255; 4])),
                field("range", JDramaFieldValue::F32(100.0)),
            ],
        },
    };
    let mut object = SceneObject::new("light", "Light");
    object.set_raw_param("name", "edited light");
    object.set_raw_param("position", "1.5,2.5,3.5");
    object.set_raw_param("color", "1,2,3,4");
    object.set_raw_param("range", "250.25");
    apply_object_parameter_edits(&mut record, &object, ParameterApplyMode::DirtyOnly).unwrap();

    let document = JDramaDocument { root: record };
    let bytes = document.to_bytes().unwrap();
    let reopened = JDramaDocument::parse(&bytes).unwrap();
    assert_eq!(reopened.to_bytes().unwrap(), bytes);
    assert_eq!(reopened.root, document.root);
}
