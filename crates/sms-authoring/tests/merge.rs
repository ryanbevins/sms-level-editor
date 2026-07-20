use std::path::Path;

use sms_authoring::{
    import_model, merge_model_instances, AssetId, CollisionSurface, ModelImportOptions,
    ModelInstancePlacement, ResolvedModelInstance,
};

fn fixture_asset() -> sms_authoring::ModelAssetDocument {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/gltf/valid/material-textures/model.gltf");
    import_model(path, &ModelImportOptions::default())
        .unwrap()
        .asset
}

fn translated(x: f32) -> [[f32; 4]; 4] {
    [
        [1.0, 0.0, 0.0, 0.0],
        [0.0, 1.0, 0.0, 0.0],
        [0.0, 0.0, 1.0, 0.0],
        [x, 0.0, 0.0, 1.0],
    ]
}

#[test]
fn merge_remaps_content_and_bakes_enabled_collision_deterministically() {
    let asset_id = AssetId::new();
    let asset = fixture_asset();
    let mut first = ModelInstancePlacement::new(asset_id, "First");
    first.transform = translated(0.0);
    let mut second = ModelInstancePlacement::new(asset_id, "Second");
    second.transform = translated(500.0);
    second.collision_enabled = false;
    let instances = [
        ResolvedModelInstance {
            placement: first,
            asset: asset.clone(),
        },
        ResolvedModelInstance {
            placement: second,
            asset: asset.clone(),
        },
    ];
    let merged = merge_model_instances("World", &instances).unwrap();
    assert_eq!(merged.textures.len(), asset.textures.len() * 2);
    assert_eq!(merged.materials.len(), asset.materials.len() * 2);
    assert_eq!(merged.meshes.len(), asset.meshes.len() * 2);
    assert_eq!(
        merged.materials[asset.materials.len()]
            .base_color_texture
            .as_ref()
            .unwrap()
            .texture,
        asset.textures.len() as u32
    );
    assert_eq!(
        merged.meshes[asset.meshes.len()].primitives[0].material,
        Some(asset.materials.len() as u32)
    );
    assert_eq!(
        merged.converted_bounds().unwrap().unwrap().min,
        [-100.0, 0.0, -100.0]
    );
    assert_eq!(
        merged.converted_bounds().unwrap().unwrap().max,
        [600.0, 0.0, 100.0]
    );
    assert_eq!(
        merged.collision.as_ref().unwrap().groups.len(),
        asset.collision.as_ref().unwrap().groups.len()
    );

    let first_bmd = merged.compile_bmd().unwrap();
    let second_bmd = merged.compile_bmd().unwrap();
    assert_eq!(first_bmd, second_bmd);
    assert_eq!(
        sms_formats::J3dRebuildDocument::parse(&first_bmd)
            .unwrap()
            .to_bytes()
            .unwrap(),
        first_bmd
    );
    let first_col = merged.compile_col().unwrap();
    assert_eq!(first_col, merged.compile_col().unwrap());
    assert_eq!(
        sms_formats::ColFile::parse(&first_col)
            .unwrap()
            .to_bytes()
            .unwrap(),
        first_col
    );
}

#[test]
fn merge_applies_collision_surface_override_and_reflection_winding() {
    let asset_id = AssetId::new();
    let asset = fixture_asset();
    let first = ModelInstancePlacement::new(asset_id, "First");
    let mut reflected = ModelInstancePlacement::new(asset_id, "Reflected");
    reflected.transform = [
        [-1.0, 0.0, 0.0, 0.0],
        [0.0, 1.0, 0.0, 0.0],
        [0.0, 0.0, 1.0, 0.0],
        [500.0, 0.0, 0.0, 1.0],
    ];
    reflected.collision_surface_override = Some(CollisionSurface {
        surface_type: 0x4321,
        attribute_0: 0xaa,
        attribute_1: 0x55,
        data: Some(-7),
    });
    let merged = merge_model_instances(
        "World",
        &[
            ResolvedModelInstance {
                placement: first,
                asset: asset.clone(),
            },
            ResolvedModelInstance {
                placement: reflected,
                asset: asset.clone(),
            },
        ],
    )
    .unwrap();
    let groups = &merged.collision.as_ref().unwrap().groups;
    let first_group_count = asset.collision.as_ref().unwrap().groups.len();
    assert!(groups[..first_group_count]
        .iter()
        .all(|group| group.surface.surface_type == 0));
    assert!(groups[first_group_count..].iter().all(|group| {
        group.surface.surface_type == 0x4321
            && group.surface.attribute_0 == 0xaa
            && group.surface.attribute_1 == 0x55
            && group.surface.data == Some(-7)
    }));
}
