use std::path::{Path, PathBuf};

use sms_authoring::{
    decode_canonical_bmd3, import_model, AuthoringError, CollisionImportOptions, CollisionSource,
    DiagnosticCode, ModelImportOptions,
};

fn fixture_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("gltf")
}

fn model_in(directory: &Path) -> PathBuf {
    for filename in ["model.gltf", "model.glb"] {
        let candidate = directory.join(filename);
        if candidate.is_file() {
            return candidate;
        }
    }
    panic!(
        "fixture directory {} has no model file",
        directory.display()
    );
}

#[test]
fn every_repository_authored_valid_fixture_imports_deterministically() {
    let valid = fixture_root().join("valid");
    let mut directories = std::fs::read_dir(&valid)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.is_dir())
        .collect::<Vec<_>>();
    directories.sort();
    assert!(!directories.is_empty());
    for directory in directories {
        let model = model_in(&directory);
        let first = import_model(&model, &ModelImportOptions::default())
            .unwrap_or_else(|error| panic!("{} failed: {error}", model.display()));
        let second = import_model(&model, &ModelImportOptions::default())
            .unwrap_or_else(|error| panic!("{} failed on repeat: {error}", model.display()));
        assert_eq!(
            first.asset,
            second.asset,
            "{} was unstable",
            model.display()
        );
        assert_eq!(
            first.asset.to_native_bytes().unwrap(),
            second.asset.to_native_bytes().unwrap(),
            "{} native bytes were unstable",
            model.display()
        );
        let native = first.asset.to_native_bytes().unwrap();
        let reloaded = sms_authoring::ModelAssetDocument::from_native_bytes(&native)
            .unwrap_or_else(|error| panic!("{} native reload failed: {error}", model.display()));
        assert_eq!(
            reloaded,
            first.asset,
            "{} native reload drifted",
            model.display()
        );

        let first_bmd = first
            .asset
            .compile_bmd()
            .unwrap_or_else(|error| panic!("{} BMD compile failed: {error}", model.display()));
        let second_bmd = first.asset.compile_bmd().unwrap();
        assert_eq!(
            first_bmd,
            second_bmd,
            "{} BMD bytes were unstable",
            model.display()
        );
        let decoded = decode_canonical_bmd3(&first_bmd)
            .unwrap_or_else(|error| panic!("{} BMD decode failed: {error}", model.display()));
        assert_eq!(decoded.to_bytes().unwrap(), first_bmd);
        if let Some(collision) = &first.asset.collision {
            let first_col = collision.to_col_bytes().unwrap();
            let second_col = collision.to_col_bytes().unwrap();
            assert_eq!(
                first_col,
                second_col,
                "{} COL bytes were unstable",
                model.display()
            );
            assert_eq!(
                sms_formats::ColFile::parse(&first_col)
                    .unwrap()
                    .to_bytes()
                    .unwrap(),
                first_col,
                "{} COL semantic reload drifted",
                model.display()
            );
        }
    }
}

#[test]
fn material_fixture_maps_safe_fields_and_reports_unmapped_pbr() {
    let model = fixture_root()
        .join("valid")
        .join("material-textures")
        .join("model.gltf");
    let imported = import_model(model, &ModelImportOptions::default()).unwrap();
    assert_eq!(imported.asset.textures.len(), 2);
    assert_eq!(
        imported.asset.textures[0].rgba8.len(),
        (imported.asset.textures[0].width * imported.asset.textures[0].height * 4) as usize
    );
    assert_eq!(imported.asset.materials.len(), 3);
    assert!(matches!(
        imported.asset.materials[1].source_alpha_mode,
        sms_authoring::ImportedAlphaMode::Mask { cutoff } if cutoff == 0.375
    ));
    assert_eq!(imported.asset.materials[1].gx.cull_mode, 0);
    assert_eq!(imported.asset.materials[1].gx.alpha_compare.reference_0, 96);
    for material in &imported.asset.materials {
        assert_eq!(material.gx.color_channel_count, 1);
        assert!(material.gx.color_channels[0].is_some());
        assert!(material.gx.color_channels[1].is_some());
        assert_eq!(
            material.gx.tev_orders[0]
                .expect("conservative TEV order")
                .color_channel,
            4,
            "{} must use GX_COLOR0A0 when multiplying by RASC/RASA",
            material.gx.name
        );
    }
    let bmd = imported.asset.compile_bmd().unwrap();
    let preview = sms_formats::J3dFile::parse(bmd)
        .unwrap()
        .geometry_preview()
        .unwrap();
    assert!(preview.materials.iter().all(|material| {
        material
            .tev_stages
            .first()
            .is_some_and(|stage| stage.order.color_channel == 4)
    }));
    assert!(imported.diagnostics.iter().any(|diagnostic| {
        diagnostic.code == DiagnosticCode::UnmappedMetallicRoughness
            && diagnostic.acknowledgement_required
    }));
}

#[test]
fn native_load_repairs_the_exact_legacy_invisible_material_program() {
    let model = fixture_root()
        .join("valid")
        .join("material-textures")
        .join("model.gltf");
    let mut legacy = import_model(model, &ModelImportOptions::default())
        .unwrap()
        .asset;
    let gx = &mut legacy.materials[0].gx;
    gx.color_channel_count = 0;
    gx.color_channels = [None; 4];
    gx.tev_orders[0].as_mut().unwrap().color_channel = 0xff;

    let repaired =
        sms_authoring::ModelAssetDocument::from_native_bytes(&legacy.to_native_bytes().unwrap())
            .unwrap();
    let gx = &repaired.materials[0].gx;
    assert_eq!(gx.color_channel_count, 1);
    assert!(gx.color_channels[0].is_some());
    assert!(gx.color_channels[1].is_some());
    assert_eq!(gx.tev_orders[0].unwrap().color_channel, 4);
}

#[test]
fn acknowledgement_required_pbr_diagnostics_persist_in_native_assets() {
    let model = fixture_root()
        .join("valid")
        .join("pbr-diagnostics")
        .join("model.glb");
    let mut asset = import_model(model, &ModelImportOptions::default())
        .unwrap()
        .asset;
    let codes = asset
        .unacknowledged_required_diagnostics()
        .into_iter()
        .map(|diagnostic| diagnostic.code)
        .collect::<std::collections::BTreeSet<_>>();
    assert!(!codes.is_empty());
    asset.acknowledged_diagnostics = codes;
    let reopened =
        sms_authoring::ModelAssetDocument::from_native_bytes(&asset.to_native_bytes().unwrap())
            .unwrap();
    assert!(reopened.unacknowledged_required_diagnostics().is_empty());
}

#[test]
fn repository_security_and_rejection_fixtures_fail_with_stable_categories() {
    for (directory, expected_code) in [
        ("network-uri", "external_uri"),
        ("path-traversal", "path_traversal"),
    ] {
        let model = fixture_root()
            .join("security")
            .join(directory)
            .join("model.gltf");
        let error = import_model(model, &ModelImportOptions::default()).unwrap_err();
        assert!(
            matches!(error, AuthoringError::Security { ref code, .. } if code == expected_code),
            "unexpected error for {directory}: {error}"
        );
    }

    for (directory, expected_code) in [
        ("lines", "non_triangle_primitive"),
        ("points", "non_triangle_primitive"),
        ("morph-target", "morph_targets"),
        ("skin-animation", "skinning"),
    ] {
        let model = fixture_root()
            .join("unsupported")
            .join(directory)
            .join("model.gltf");
        let error = import_model(model, &ModelImportOptions::default()).unwrap_err();
        assert!(
            matches!(error, AuthoringError::Unsupported { ref code, .. } if code == expected_code),
            "unexpected error for {directory}: {error}"
        );
    }

    let model = fixture_root()
        .join("rejected")
        .join("unencodable-name")
        .join("model.gltf");
    let error = import_model(model, &ModelImportOptions::default()).unwrap_err();
    assert!(
        matches!(error, AuthoringError::Unsupported { code, .. } if code == "unencodable_name")
    );
}

#[test]
fn minimal_fixture_compiles_to_deterministic_reparseable_bmd_and_col() {
    let model = fixture_root()
        .join("valid")
        .join("minimal-external")
        .join("model.gltf");
    let imported = import_model(model, &ModelImportOptions::default()).unwrap();
    let first_bmd = imported.asset.compile_bmd().unwrap();
    let second_bmd = imported.asset.compile_bmd().unwrap();
    assert_eq!(first_bmd, second_bmd);
    let reopened = decode_canonical_bmd3(&first_bmd).unwrap();
    assert_eq!(reopened.to_bytes().unwrap(), first_bmd);
    let preview = sms_formats::J3dFile::parse(first_bmd)
        .unwrap()
        .geometry_preview()
        .unwrap();
    assert_eq!(preview.triangles.len(), 1);

    let first_col = imported.asset.compile_col().unwrap();
    let second_col = imported.asset.compile_col().unwrap();
    assert_eq!(first_col, second_col);
    assert_eq!(
        sms_formats::ColFile::parse(&first_col)
            .unwrap()
            .to_bytes()
            .unwrap(),
        first_col
    );
}

#[test]
fn malformed_fixtures_never_panic_or_import() {
    for directory in [
        "accessor-out-of-bounds",
        "malformed-glb",
        "missing-external",
    ] {
        let model = model_in(&fixture_root().join("invalid").join(directory));
        assert!(
            import_model(&model, &ModelImportOptions::default()).is_err(),
            "{} unexpectedly imported",
            model.display()
        );
    }
}

#[test]
fn separate_and_disabled_collision_modes_are_explicit_and_deterministic() {
    let render = fixture_root()
        .join("valid")
        .join("minimal-external")
        .join("model.gltf");
    let separate = fixture_root()
        .join("valid")
        .join("collision-separate")
        .join("model.glb");
    let options = ModelImportOptions {
        collision: CollisionSource::SeparateFile {
            path: separate,
            options: CollisionImportOptions::default(),
        },
        ..ModelImportOptions::default()
    };
    let first = import_model(&render, &options).unwrap().asset;
    let second = import_model(&render, &options).unwrap().asset;
    assert_eq!(first.collision, second.collision);
    assert!(!first.collision.as_ref().unwrap().groups.is_empty());

    let without_collision = import_model(
        render,
        &ModelImportOptions {
            collision: CollisionSource::None,
            ..ModelImportOptions::default()
        },
    )
    .unwrap()
    .asset;
    assert!(without_collision.collision.is_none());
    assert!(without_collision.compile_col().is_err());
}
