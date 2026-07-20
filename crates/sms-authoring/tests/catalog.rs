use std::fs;
use std::path::{Path, PathBuf};

use sms_authoring::{
    import_model, AssetReference, CatalogError, CatalogFilter, CatalogIssueKind, CollisionDocument,
    CollisionGroup, CollisionSurface, ColorSet, ModelAssetCatalog, ModelAssetDocument,
    ModelCoordinateSpace, ModelImportOptions, ModelMesh, ModelNode, ModelPrimitive, ModelTexture,
    NodePurpose, TexCoordSet,
};

fn test_document(name: &str) -> ModelAssetDocument {
    let mut document = ModelAssetDocument::new(name);
    document.scene_roots = vec![0];
    document.nodes.push(ModelNode {
        name: "Root".to_string(),
        parent: None,
        children: Vec::new(),
        mesh: Some(0),
        purpose: NodePurpose::Render,
        local_transform: [
            [1.0, 0.0, 0.0, 0.0],
            [0.0, 1.0, 0.0, 0.0],
            [0.0, 0.0, 1.0, 0.0],
            [0.0, 0.0, 0.0, 1.0],
        ],
    });
    document.meshes.push(ModelMesh {
        name: "Triangle".to_string(),
        primitives: vec![ModelPrimitive {
            positions: vec![[0.0, 0.0, 0.0], [100.0, 0.0, 0.0], [0.0, 100.0, 0.0]],
            normals: vec![[0.0, 0.0, 1.0]; 3],
            tangents: vec![[1.0, 0.0, 0.0, 1.0]; 3],
            tex_coords: vec![TexCoordSet {
                set: 0,
                values: vec![[0.0, 0.0], [1.0, 0.0], [0.0, 1.0]],
            }],
            colors: vec![ColorSet {
                set: 0,
                values: vec![[1.0, 1.0, 1.0, 1.0]; 3],
            }],
            indices: vec![0, 1, 2],
            material: None,
        }],
    });
    document.textures.push(ModelTexture {
        name: "Pixel".to_string(),
        width: 1,
        height: 1,
        rgba8: vec![17, 34, 51, 255],
        encode_options: Default::default(),
    });
    document.collision = Some(CollisionDocument {
        vertices: vec![[0.0, 0.0, 0.0], [100.0, 0.0, 0.0], [0.0, 0.0, 100.0]],
        groups: vec![CollisionGroup {
            name: "ground".to_string(),
            surface: CollisionSurface::default(),
            triangles: vec![[0, 1, 2]],
        }],
    });
    document.validate().unwrap();
    document
}

#[test]
fn managed_manifest_round_trips_without_inline_geometry_or_textures() {
    let project = tempfile::tempdir().unwrap();
    let catalog = ModelAssetCatalog::open_project(project.path()).unwrap();
    let document = test_document("Triangle");

    let entry = catalog.create_asset("Models/Triangle", &document).unwrap();
    assert_eq!(entry.name, "Triangle");
    assert_eq!(entry.relative_path, Path::new("Models/Triangle.smsmodel"));
    assert_eq!(entry.mesh_count, 1);
    assert_eq!(entry.texture_count, 1);
    assert!(entry.has_collision);

    let manifest_path = project.path().join("Content/Models/Triangle.smsmodel");
    let before = fs::read(&manifest_path).unwrap();
    let text = String::from_utf8(before.clone()).unwrap();
    assert!(text.contains("\"manifest_version\": 1"));
    assert!(text.contains(&entry.id.to_string()));
    assert!(text.contains(".sms-assets/"));
    assert!(!text.contains("\"positions\""));
    assert!(!text.contains("ESIz/w=="));

    let loaded = catalog.load_asset(entry.id).unwrap();
    assert_eq!(loaded, document);
    catalog.save_asset(entry.id, &document).unwrap();
    let after = fs::read(&manifest_path).unwrap();
    assert_eq!(before, after, "saving unchanged data must be deterministic");

    let managed = catalog.managed_storage_root().join(entry.id.to_string());
    assert!(managed
        .join("geometry/mesh-0000-primitive-0000.smsgeom")
        .is_file());
    assert!(managed.join("geometry/collision.smscolgeom").is_file());
    assert!(managed.join("textures/texture-0000.rgba8").is_file());
}

#[test]
fn catalog_load_migrates_missing_legacy_coordinate_space_without_source_gltf() {
    let mut legacy = test_document("Legacy reflected Z");
    legacy.coordinate_space = ModelCoordinateSpace::LegacyReflectedZ;
    legacy.nodes[0].local_transform[3][2] = 30.0;
    legacy.meshes[0].primitives[0].positions[2][2] = 20.0;
    legacy.collision.as_mut().unwrap().vertices[2][2] = 100.0;

    let project = tempfile::tempdir().unwrap();
    let catalog = ModelAssetCatalog::open_project(project.path()).unwrap();
    let entry = catalog.create_asset("LegacyReflectedZ", &legacy).unwrap();
    let manifest_path = project.path().join("Content/LegacyReflectedZ.smsmodel");
    let manifest = fs::read_to_string(&manifest_path).unwrap();
    let legacy_manifest = manifest.replace(
        concat!(
            "  \"coordinate_space\": \"legacy_reflected_z\",",
            "
"
        ),
        "",
    );
    assert_ne!(
        manifest, legacy_manifest,
        "fixture must remove the new marker"
    );
    fs::write(&manifest_path, legacy_manifest).unwrap();

    let migrated = catalog.load_asset(entry.id).unwrap();
    assert_eq!(
        migrated.coordinate_space,
        ModelCoordinateSpace::GltfCompatible
    );
    assert_eq!(migrated.nodes[0].local_transform[3][2], -30.0);
    assert_eq!(migrated.meshes[0].primitives[0].positions[2][2], -20.0);
    assert_eq!(migrated.meshes[0].primitives[0].indices, [0, 2, 1]);
    assert_eq!(migrated.collision.as_ref().unwrap().vertices[2][2], -100.0);
    assert_eq!(
        migrated.collision.as_ref().unwrap().groups[0].triangles[0],
        [0, 2, 1]
    );
    assert!(migrated.diagnostics.iter().any(|diagnostic| {
        diagnostic.code == sms_authoring::DiagnosticCode::CoordinateSpaceMigrated
    }));

    catalog.save_asset(entry.id, &migrated).unwrap();
    let reopened = catalog.load_asset(entry.id).unwrap();
    assert_eq!(
        reopened, migrated,
        "coordinate migration must apply exactly once"
    );
}

#[test]
fn catalog_load_repairs_the_exact_legacy_invisible_material_program() {
    let source = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/gltf/valid/material-textures/model.gltf");
    let mut legacy = import_model(source, &ModelImportOptions::default())
        .unwrap()
        .asset;
    let gx = &mut legacy.materials[0].gx;
    gx.color_channel_count = 0;
    gx.color_channels = [None; 4];
    gx.tev_orders[0].as_mut().unwrap().color_channel = 0xff;

    let project = tempfile::tempdir().unwrap();
    let catalog = ModelAssetCatalog::open_project(project.path()).unwrap();
    let entry = catalog.create_asset("Legacy", &legacy).unwrap();
    let repaired = catalog.load_asset(entry.id).unwrap();
    let gx = &repaired.materials[0].gx;
    assert_eq!(gx.color_channel_count, 1);
    assert!(gx.color_channels[0].is_some());
    assert!(gx.color_channels[1].is_some());
    assert_eq!(gx.tev_orders[0].unwrap().color_channel, 4);
}

#[test]
fn catalog_load_repairs_legacy_render_derived_inverted_collision() {
    let mut legacy = test_document("Legacy inverted terrain");
    let primitive = &mut legacy.meshes[0].primitives[0];
    primitive.positions = vec![[0.0, 0.0, 0.0], [100.0, 0.0, 0.0], [0.0, 0.0, 100.0]];
    primitive.normals = vec![[0.0, -1.0, 0.0]; 3];
    primitive.indices = vec![0, 1, 2];
    let collision = legacy.collision.as_mut().unwrap();
    collision.vertices = primitive.positions.clone();
    collision.groups[0].name = "Root/primitive_0".to_string();
    collision.groups[0].triangles = vec![[0, 1, 2]];
    legacy.validate().unwrap();

    let project = tempfile::tempdir().unwrap();
    let catalog = ModelAssetCatalog::open_project(project.path()).unwrap();
    let entry = catalog
        .create_asset("LegacyInvertedTerrain", &legacy)
        .unwrap();
    let repaired = catalog.load_asset(entry.id).unwrap();

    assert_eq!(
        repaired.collision.unwrap().groups[0].triangles[0],
        [0, 2, 1]
    );
    assert!(repaired.diagnostics.iter().any(|diagnostic| {
        diagnostic.code == sms_authoring::DiagnosticCode::CollisionWindingNormalized
    }));
}

#[test]
fn rename_move_duplicate_search_and_folders_preserve_reference_identity() {
    let project = tempfile::tempdir().unwrap();
    let catalog = ModelAssetCatalog::open_project(project.path()).unwrap();
    let original = catalog
        .create_asset("Models/Original", &test_document("Original"))
        .unwrap();

    let renamed = catalog.rename_asset(original.id, "Renamed").unwrap();
    assert_eq!(renamed.id, original.id);
    assert_eq!(renamed.name, "Renamed");
    assert_eq!(renamed.relative_path, Path::new("Models/Renamed.smsmodel"));
    assert_eq!(catalog.load_asset(original.id).unwrap().name, "Renamed");

    let moved = catalog
        .move_asset(original.id, "Environment/Props")
        .unwrap();
    assert_eq!(moved.id, original.id);
    assert_eq!(
        moved.relative_path,
        Path::new("Environment/Props/Renamed.smsmodel")
    );

    let filtered = catalog
        .search(&CatalogFilter {
            text: Some("renamed environment".to_string()),
            folder: Some(PathBuf::from("Environment")),
            has_collision: Some(true),
        })
        .unwrap();
    assert_eq!(filtered.assets, vec![moved.clone()]);

    let duplicate = catalog
        .duplicate_asset(original.id, "Environment/Props/Copy")
        .unwrap();
    assert_ne!(duplicate.id, original.id);
    assert_eq!(duplicate.name, "Copy");
    let mut expected_copy = catalog.load_asset(original.id).unwrap();
    expected_copy.name = "Copy".to_string();
    assert_eq!(catalog.load_asset(duplicate.id).unwrap(), expected_copy);

    catalog.create_folder("Characters/NPCs").unwrap();
    let folders = catalog.folders().unwrap();
    assert!(folders.contains(&PathBuf::from("Environment")));
    assert!(folders.contains(&PathBuf::from("Environment/Props")));
    assert!(folders.contains(&PathBuf::from("Characters/NPCs")));
}

#[test]
fn delete_is_blocked_until_the_reference_set_is_empty() {
    let project = tempfile::tempdir().unwrap();
    let catalog = ModelAssetCatalog::open_project(project.path()).unwrap();
    let entry = catalog
        .create_asset("Models/Referenced", &test_document("Referenced"))
        .unwrap();
    let reference = AssetReference {
        owner: "test11 placed instance 7".to_string(),
        project_path: Some(PathBuf::from("Stages/test11.smsstage")),
    };

    let error = catalog
        .delete_asset(entry.id, std::slice::from_ref(&reference))
        .unwrap_err();
    match error {
        CatalogError::Referenced {
            asset_id,
            references,
        } => {
            assert_eq!(asset_id, entry.id);
            assert_eq!(references, vec![reference]);
        }
        other => panic!("unexpected error: {other}"),
    }
    assert!(catalog.load_asset(entry.id).is_ok());

    let report = catalog.delete_asset(entry.id, &[]).unwrap();
    assert_eq!(report.id, entry.id);
    assert!(report.managed_blob_directory_removed);
    assert!(matches!(
        catalog.load_asset(entry.id),
        Err(CatalogError::NotFound(id)) if id == entry.id
    ));
    assert!(!catalog
        .managed_storage_root()
        .join(entry.id.to_string())
        .exists());
}

#[test]
fn traversal_reserved_storage_and_tampered_blob_paths_are_rejected() {
    let project = tempfile::tempdir().unwrap();
    let catalog = ModelAssetCatalog::open_project(project.path()).unwrap();
    let document = test_document("Safe");

    assert!(matches!(
        catalog.create_asset("../Outside", &document),
        Err(CatalogError::UnsafePath { .. })
    ));
    assert!(matches!(
        catalog.create_asset(".sms-assets/Imposter", &document),
        Err(CatalogError::UnsafePath { .. })
    ));
    assert!(matches!(
        catalog.create_asset("Models/CON.smsmodel", &document),
        Err(CatalogError::UnsafePath { .. })
    ));
    assert!(matches!(
        catalog.load_asset_path("../Outside.smsmodel"),
        Err(CatalogError::UnsafePath { .. })
    ));

    let entry = catalog.create_asset("Models/Safe", &document).unwrap();
    let manifest_path = project.path().join("Content/Models/Safe.smsmodel");
    let original_path = format!(
        ".sms-assets/{}/geometry/mesh-0000-primitive-0000.smsgeom",
        entry.id
    );
    let tampered = fs::read_to_string(&manifest_path)
        .unwrap()
        .replace(&original_path, "../outside.smsgeom");
    fs::write(&manifest_path, tampered).unwrap();

    assert!(matches!(
        catalog.load_asset_path("Models/Safe.smsmodel"),
        Err(CatalogError::InvalidManifest { .. })
    ));
    let scan = catalog.scan().unwrap();
    assert!(scan.assets.is_empty());
    assert_eq!(scan.issues.len(), 1);
    assert_eq!(scan.issues[0].kind, CatalogIssueKind::InvalidManifest);
    assert!(!project.path().join("outside.smsgeom").exists());
}

#[test]
fn source_gltf_and_reimport_recipe_are_not_retained() {
    let project = tempfile::tempdir().unwrap();
    let catalog = ModelAssetCatalog::open_project(project.path()).unwrap();
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/gltf/valid/minimal-external/model.gltf");

    let imported = catalog
        .import_gltf("Imported/Fixture", &fixture, &ModelImportOptions::default())
        .unwrap();
    let loaded = catalog.load_asset(imported.entry.id).unwrap();
    assert_eq!(loaded.name, "Fixture");
    assert!(!loaded.meshes.is_empty());

    let mut files = Vec::new();
    collect_files(&project.path().join("Content"), &mut files);
    for path in files {
        let bytes = fs::read(&path).unwrap();
        let text = String::from_utf8_lossy(&bytes);
        assert!(
            !text.contains("model.gltf"),
            "retained source in {}",
            path.display()
        );
        assert!(
            !text.contains("mesh.bin"),
            "retained source in {}",
            path.display()
        );
        assert!(
            !text.contains("coordinate_conversion"),
            "retained import recipe in {}",
            path.display()
        );
        assert!(
            !text.contains(&fixture.display().to_string()),
            "retained absolute source path in {}",
            path.display()
        );
    }
}

#[test]
fn invalid_manifests_and_duplicate_ids_are_reported_without_aborting_scan() {
    let project = tempfile::tempdir().unwrap();
    let catalog = ModelAssetCatalog::open_project(project.path()).unwrap();
    let content = project.path().join("Content");
    fs::write(content.join("Broken.smsmodel"), b"{not json").unwrap();
    let valid = catalog
        .create_asset("Models/Valid", &test_document("Valid"))
        .unwrap();
    fs::copy(
        content.join("Models/Valid.smsmodel"),
        content.join("Models/Duplicate.smsmodel"),
    )
    .unwrap();

    let scan = catalog.scan().unwrap();
    assert_eq!(scan.assets.len(), 2);
    assert!(scan
        .issues
        .iter()
        .any(|issue| issue.relative_path == Path::new("Broken.smsmodel")));
    assert_eq!(
        scan.issues
            .iter()
            .filter(|issue| issue.kind == CatalogIssueKind::DuplicateAssetId)
            .count(),
        2
    );
    assert!(matches!(
        catalog.load_asset(valid.id),
        Err(CatalogError::DuplicateAssetId(id)) if id == valid.id
    ));
}

fn collect_files(directory: &Path, output: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(directory).unwrap() {
        let entry = entry.unwrap();
        if entry.file_type().unwrap().is_dir() {
            collect_files(&entry.path(), output);
        } else {
            output.push(entry.path());
        }
    }
}
