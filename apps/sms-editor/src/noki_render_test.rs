use super::*;

use std::{
    env, fs,
    path::{Path, PathBuf},
};

const BASE_ROOT_ENV: &str = "SMS_BASE_ROOT";
const LEGACY_BASE_ROOT_ENV: &str = "SMS_NOKI_TEST_BASE_ROOT";
const OUTPUT_ENV: &str = "SMS_NOKI_TEST_OUTPUT";

fn retail_base_root() -> PathBuf {
    env::var_os(BASE_ROOT_ENV)
        .or_else(|| env::var_os(LEGACY_BASE_ROOT_ENV))
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            panic!(
                "set {BASE_ROOT_ENV} (or legacy {LEGACY_BASE_ROOT_ENV}) to the extracted game's data directory"
            )
        })
}

fn retail_placement_census(
    base_root: &Path,
    selectors: &[(&str, &str)],
) -> BTreeMap<(String, String, String), usize> {
    let selectors = selectors.iter().copied().collect::<BTreeSet<_>>();
    let mut census = BTreeMap::new();
    for archive in discover_scene_archives(base_root).expect("discover retail scene archives") {
        let document = StageDocument::open(base_root, &archive.stage_id)
            .unwrap_or_else(|error| panic!("open {}: {error}", archive.stage_id));
        for object in &document.objects {
            let Some(resource_name) = object.raw_param("actor_tail_string") else {
                continue;
            };
            if selectors.contains(&(object.factory_name.as_str(), resource_name)) {
                *census
                    .entry((
                        archive.stage_id.clone(),
                        object.factory_name.clone(),
                        resource_name.to_string(),
                    ))
                    .or_default() += 1;
            }
        }
    }
    census
}

#[test]
#[ignore = "requires an extracted retail base root"]
fn mamma0_ocean_water_survives_enemy_preview_catalog() {
    let base_root = retail_base_root();
    let decomp_root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../..");
    let registry = SchemaGenerator::new(decomp_root)
        .generate()
        .expect("generate decomp-derived object metadata");
    let document = StageDocument::open(&base_root, "mamma0")
        .expect("open mamma0")
        .with_registry(registry);

    let preview = SmsEditorApp::build_model_preview(
        &document,
        PreviewVisibility {
            environment: true,
            goop: true,
            effects: false,
        },
    )
    .expect("build mamma0 preview");
    let water_triangles = preview
        .triangles
        .iter()
        .filter(|triangle| triangle.render_layer == PreviewRenderLayer::Water)
        .count();

    assert_eq!(water_triangles, 1_595, "mamma0 retail sea geometry");

    for (factory_name, expected_rgb) in [
        ("PoiHana", [-191, 8, 303]),
        ("PoiHanaRed", [283, -53, -122]),
    ] {
        let object = document
            .objects
            .iter()
            .find(|object| object.factory_name == factory_name)
            .unwrap_or_else(|| panic!("mamma0 contains {factory_name}"));
        let model_index = preview.object_model_indices[&object.id];
        let body_material = preview
            .triangles
            .iter()
            .filter(|triangle| triangle.model_index == model_index)
            .filter_map(|triangle| triangle.material_index)
            .map(|index| &preview.materials[index])
            .find(|material| material.name.eq_ignore_ascii_case("_body"))
            .unwrap_or_else(|| panic!("{factory_name} _body material"));
        assert_eq!(
            body_material.tev_colors[0][..3],
            expected_rgb,
            "{factory_name} runtime TEV register 0 color"
        );
    }
}

#[test]
#[ignore = "requires an extracted retail base root"]
fn dolpic0_includes_animated_sea_indirect_screen_copy() {
    let base_root = retail_base_root();
    let decomp_root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../..");
    let registry = SchemaGenerator::new(decomp_root)
        .generate()
        .expect("generate decomp-derived object metadata");
    let document = StageDocument::open(&base_root, "dolpic0")
        .expect("open dolpic0")
        .with_registry(registry);
    let sea_indirect_asset = document
        .assets
        .iter()
        .find(|asset| {
            asset
                .path
                .to_string_lossy()
                .replace('\\', "/")
                .to_ascii_lowercase()
                .ends_with("/map/map/seaindirect.bmd")
        })
        .expect("Dolpic SeaIndirect model");
    let sea_indirect_bytes =
        read_stage_asset_bytes(&sea_indirect_asset.path).expect("read Dolpic SeaIndirect model");
    let sea_indirect_file =
        J3dFile::parse(&sea_indirect_bytes).expect("parse Dolpic SeaIndirect model");
    assert_eq!(
        sea_indirect_file
            .texture_previews()
            .expect("SeaIndirect textures")[1]
            .name
            .to_ascii_lowercase(),
        "indirectdummy"
    );
    let sea_asset = document
        .assets
        .iter()
        .find(|asset| {
            asset
                .path
                .to_string_lossy()
                .replace('\\', "/")
                .to_ascii_lowercase()
                .ends_with("/map/map/sea.bmd")
        })
        .expect("Dolpic sea model");
    let sea_bytes = read_stage_asset_bytes(&sea_asset.path).expect("read Dolpic sea model");
    let sea_textures = J3dFile::parse(&sea_bytes)
        .expect("parse Dolpic sea model")
        .texture_previews()
        .expect("Dolpic sea textures");
    assert!(sea_textures.iter().all(|texture| texture.mipmap_enabled));
    assert_eq!(
        sea_textures
            .iter()
            .map(|texture| texture.lod_bias)
            .collect::<Vec<_>>(),
        vec![2.0, 0.8, 0.0, -0.5, 0.0, 2.14, 0.4, 1.27]
    );
    let preview = SmsEditorApp::build_model_preview(
        &document,
        PreviewVisibility {
            environment: true,
            goop: true,
            effects: false,
        },
    )
    .expect("build Dolpic preview");

    let material_indices = preview
        .triangles
        .iter()
        .filter(|triangle| triangle.render_layer == PreviewRenderLayer::IndirectWater)
        .filter_map(|triangle| triangle.material_index)
        .collect::<BTreeSet<_>>();
    assert!(!material_indices.is_empty(), "SeaIndirect mesh is missing");

    for material_index in material_indices {
        let material = &preview.materials[material_index];
        assert!(material.indirect.enabled);
        assert_eq!(material.indirect.stage_count, 1);
        material.texture_indices[1]
            .and_then(|texture_index| preview.textures.get(texture_index))
            .expect("SeaIndirect screen-copy texture slot");
        assert!(preview.material_animation_bindings[material_index]
            .iter()
            .any(
                |binding| preview.texture_srt_animations[binding.animation_index]
                    .bindings
                    .get(binding.binding_index)
                    .is_some_and(|binding| binding.texture_matrix_index == 0)
            ));
    }

    let wave_triangles = preview
        .triangles
        .iter()
        .filter(|triangle| triangle.render_layer == PreviewRenderLayer::WaveFoam)
        .collect::<Vec<_>>();
    assert_eq!(
        wave_triangles.len(),
        1_300,
        "Dolpic TMapObjWave close-range grid"
    );
    let wave_material_index = wave_triangles[0]
        .material_index
        .expect("runtime wave material");
    let wave_material = &preview.materials[wave_material_index];
    assert_eq!(wave_material.name, "_runtime_wave");
    assert_eq!(wave_material.tev_stages.len(), 2);
    assert_eq!(wave_material.blend_mode.src_factor, 4);
    assert_eq!(wave_material.blend_mode.dst_factor, 2);
    let wave_texture = wave_material.texture_indices[0]
        .and_then(|texture_index| preview.textures.get(texture_index))
        .expect("decoded Dolpic wave.bti texture");
    assert!(wave_texture.mipmap_enabled);
    assert_eq!(wave_texture.min_lod, 0.0);
    assert_eq!(wave_texture.max_lod, 4.0);
    assert_eq!(wave_texture.lod_bias, 0.0);
    assert_eq!(wave_texture.mips.len(), 5);

    gpu_viewport::render_preview_offscreen(
        &preview,
        gpu_viewport::GpuViewportFrame {
            camera_position: [0.0, 10_000.0, -20_000.0],
            right: [1.0, 0.0, 0.0],
            up: [0.0, 1.0, 0.0],
            forward: [0.0, 0.0, 1.0],
            focal: 240.0,
            viewport_size: [320.0, 224.0],
            viewport_pan: [0.0; 2],
            near: 8.0,
            animation_seconds: 0.0,
            ..Default::default()
        },
        [320, 224],
    )
    .expect("render Dolpic WGPU framebuffer");
}

#[test]
#[ignore = "requires an extracted retail base root"]
fn bianco_water_pollution_model_follows_map_static_placement() {
    let base_root = retail_base_root();
    let decomp_root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../..");
    let registry = SchemaGenerator::new(decomp_root)
        .generate()
        .expect("generate decomp-derived object metadata");

    for (stage_id, expected_active) in [("bianco5", false), ("bianco6", false), ("bianco7", true)] {
        let document = StageDocument::open(&base_root, stage_id)
            .unwrap_or_else(|error| panic!("open {stage_id}: {error}"))
            .with_registry(registry.clone());
        let asset = document
            .assets
            .iter()
            .find(|asset| {
                asset
                    .path
                    .to_string_lossy()
                    .replace('\\', "/")
                    .to_ascii_lowercase()
                    .ends_with("/map/map/biawaterpollution.bmd")
            })
            .unwrap_or_else(|| panic!("{stage_id} contains BiaWaterPollution.bmd"));

        assert_eq!(
            map_static_model_is_active(&document, &asset.path.to_string_lossy()),
            expected_active,
            "{stage_id} must follow its retail MapStaticObj placement"
        );
    }
}

#[test]
#[ignore = "requires an extracted retail base root"]
fn bianco_mirror_surface_follows_runtime_cube_volume() {
    let base_root = retail_base_root();
    for episode in 0..=7 {
        let stage_id = format!("bianco{episode}");
        let document = StageDocument::open(&base_root, &stage_id).expect("open Bianco episode");
        let cubes = mirror_cubes(&document);
        let expected = if episode == 7 {
            BTreeSet::new()
        } else {
            BTreeSet::from([0])
        };
        assert_eq!(
            active_mirror_model_slots(&document),
            expected,
            "{stage_id} must follow its retail CubeMirror table"
        );
        assert_eq!(cubes.len(), usize::from(episode != 7), "{stage_id}");
        if let Some(cube) = cubes.first() {
            let inside = [
                cube.center[0],
                cube.center[1] + cube.dimensions[1] * 0.5,
                cube.center[2],
            ];
            assert!(cube.contains(inside), "{stage_id} cube midpoint");
        }
    }
    let document = StageDocument::open(&base_root, "bianco7").expect("open bianco7");
    let slots = active_mirror_model_slots(&document);
    assert!(slots.is_empty(), "bianco7 has no active CubeMirror entries");
    let mirror = document
        .assets
        .iter()
        .find(|asset| {
            asset
                .path
                .to_string_lossy()
                .replace('\\', "/")
                .to_ascii_lowercase()
                .ends_with("/map/mirror/mirror00.bmd")
        })
        .expect("bianco7 contains the dormant authored mirror surface");
    assert!(!mirror_surface_model_is_active(
        &document.stage_id,
        &mirror.path.to_string_lossy(),
        &slots,
    ));

    let document = StageDocument::open(&base_root, "bianco6").expect("open bianco6");
    let cubes = mirror_cubes(&document);
    assert_eq!(
        cubes,
        vec![PreviewMirrorCube {
            center: [600.0, -4650.0, -2550.0],
            rotation_degrees: [0.0; 3],
            dimensions: [50_000.0, 30_000.0, 50_000.0],
            model_slot: 0,
        }],
        "bianco6 retail CubeMirror volume"
    );
    let preview = SmsEditorApp::build_model_preview(
        &document,
        PreviewVisibility {
            environment: true,
            goop: true,
            effects: false,
        },
    )
    .expect("build bianco6 preview");
    let mirror_triangles = preview
        .triangles
        .iter()
        .filter(|triangle| triangle.render_layer == PreviewRenderLayer::MirrorSurface)
        .collect::<Vec<_>>();
    assert_eq!(mirror_triangles.len(), 27, "bianco6 mirror00 geometry");
    let mirror_model_indices = mirror_triangles
        .iter()
        .map(|triangle| triangle.model_index)
        .collect::<BTreeSet<_>>();
    assert_eq!(mirror_model_indices.len(), 1);
    let mirror_model_index = *mirror_model_indices.first().expect("mirror00 model index");
    assert_eq!(
        preview.mirror_model_slots.get(&mirror_model_index),
        Some(&0)
    );
    let mirror_plane_y = mirror_triangles[0].vertices[0][1];
    let bridge = document
        .objects
        .iter()
        .find(|object| object.raw_param("actor_tail_string") == Some("BiaBridge"))
        .expect("bianco6 BiaBridge placement");
    let bridge_model_index = preview.object_model_indices[&bridge.id];
    let bridge_position = preview.mirror_actor_positions[&bridge_model_index];
    assert_eq!(bridge_position, bridge.transform.translation);
    assert!(
        bridge_position[1] + 50.0 < mirror_plane_y,
        "BiaBridge is submerged far enough that Sunshine excludes its model from the mirror draw"
    );

    let inside = [600.0, 20_000.0, -2550.0];
    let overview = [600.0, 26_000.0, -2550.0];
    assert_eq!(preview.active_mirror_slot(inside), Some(0));
    assert_eq!(preview.active_mirror_slot(overview), None);
    assert!(preview.mirror_surface_model_is_visible(mirror_model_index, inside));
    assert!(!preview.mirror_surface_model_is_visible(mirror_model_index, overview));

    let frame = gpu_viewport::GpuViewportFrame {
        camera_position: overview,
        right: [1.0, 0.0, 0.0],
        up: [0.0, 0.0, 1.0],
        forward: [0.0, -1.0, 0.0],
        focal: 180.0,
        viewport_size: [320.0, 224.0],
        viewport_pan: [0.0; 2],
        near: 8.0,
        animation_seconds: 0.0,
        ..Default::default()
    };
    let with_dormant_mirror = gpu_viewport::render_preview_offscreen(&preview, frame, [320, 224])
        .expect("render bianco6 from above its CubeMirror volume");
    let mut without_mirror = preview.clone();
    without_mirror
        .triangles
        .retain(|triangle| triangle.render_layer != PreviewRenderLayer::MirrorSurface);
    let without_mirror = gpu_viewport::render_preview_offscreen(&without_mirror, frame, [320, 224])
        .expect("render bianco6 without mirror00 geometry");
    assert_eq!(
        with_dormant_mirror.pixels, without_mirror.pixels,
        "an overview camera outside CubeMirror must not draw the giant reflection plane"
    );
}

#[test]
#[ignore = "requires an extracted retail base root"]
fn retail_reflect_sky_helpers_never_enter_the_main_viewport() {
    let base_root = retail_base_root();
    let expected_stage_ids = [
        "bianco0",
        "bianco1",
        "bianco2",
        "bianco3",
        "bianco4",
        "bianco5",
        "bianco6",
        "bianco7",
        "mamma0",
        "mamma1",
        "mamma2",
        "mamma3",
        "mamma4",
        "mamma5",
        "mamma6",
        "mamma7",
        "pinnaBoss0",
        "pinnaBoss1",
        "pinnaParco0",
        "pinnaParco1",
        "pinnaParco2",
        "pinnaParco3",
        "pinnaParco4",
        "pinnaParco5",
        "pinnaParco6",
        "pinnaParco7",
    ];
    let expected_census = expected_stage_ids
        .iter()
        .map(|stage_id| {
            (
                (
                    (*stage_id).to_string(),
                    "MapStaticObj".to_string(),
                    "ReflectSky".to_string(),
                ),
                1,
            )
        })
        .collect::<BTreeMap<_, _>>();
    assert_eq!(
        retail_placement_census(&base_root, &[("MapStaticObj", "ReflectSky")]),
        expected_census,
        "complete Japanese retail ReflectSky placement census"
    );

    for stage_id in expected_stage_ids {
        let document = StageDocument::open(&base_root, stage_id)
            .unwrap_or_else(|error| panic!("open {stage_id}: {error}"));
        let helper = document
            .objects
            .iter()
            .find(|object| {
                object.factory_name == "MapStaticObj"
                    && object.raw_param("actor_tail_string") == Some("ReflectSky")
            })
            .unwrap_or_else(|| panic!("{stage_id} ReflectSky placement"));
        let helper_asset = document
            .assets
            .iter()
            .find(|asset| {
                asset
                    .path
                    .to_string_lossy()
                    .replace('\\', "/")
                    .to_ascii_lowercase()
                    .ends_with("/map/map/reflectsky.bmd")
            })
            .unwrap_or_else(|| panic!("{stage_id} reflectsky.bmd"));
        assert!(path_is_mirror_sky_helper_model_path(
            &helper_asset.path.to_string_lossy()
        ));
        assert!(!is_default_preview_model_path(
            &helper_asset.path.to_string_lossy(),
            true,
            true,
            true,
        ));

        let preview = SmsEditorApp::build_model_preview(
            &document,
            PreviewVisibility {
                environment: true,
                goop: true,
                effects: true,
            },
        )
        .unwrap_or_else(|| panic!("build {stage_id} preview"));
        assert!(
            !preview.object_model_indices.contains_key(&helper.id),
            "{stage_id} ReflectSky must stay exclusive to the mirror-sky pass"
        );
    }
}

#[test]
#[ignore = "requires SMS_BASE_ROOT with extracted retail assets"]
fn retail_stage_preview_matrix_keeps_environment_and_instance_models() {
    let base_root = env::var_os("SMS_BASE_ROOT")
        .map(PathBuf::from)
        .expect("set SMS_BASE_ROOT to the extracted game's data directory");
    let decomp_root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../..");
    let registry = SchemaGenerator::new(decomp_root)
        .generate()
        .expect("generate decomp-derived object metadata");
    let visibility = PreviewVisibility {
        environment: true,
        goop: true,
        effects: false,
    };

    let bianco = StageDocument::open(&base_root, "bianco7")
        .expect("open bianco7")
        .with_registry(registry.clone());
    let dirty_lake = bianco
        .objects
        .iter()
        .find(|object| {
            object.factory_name == "MapStaticObj"
                && object.raw_param("actor_tail_string") == Some("BiaWaterPollution")
        })
        .expect("bianco7 BiaWaterPollution placement");
    assert!(
        dirty_lake.asset_hints.iter().any(|hint| {
            hint.role == AssetRole::InferredPreviewModel
                && hint
                    .path
                    .replace('\\', "/")
                    .to_ascii_lowercase()
                    .ends_with("/map/map/biawaterpollution.bmd")
        }),
        "bianco7 dirty-lake placement lost BiaWaterPollution.bmd: {:?}",
        dirty_lake.asset_hints
    );
    let bianco_preview =
        SmsEditorApp::build_model_preview(&bianco, visibility).expect("build bianco7 preview");
    assert_eq!(
        bianco_preview
            .triangles
            .iter()
            .filter(|triangle| triangle.render_layer == PreviewRenderLayer::Goop)
            .count(),
        400,
        "bianco7 retail BiaWaterPollution geometry"
    );

    let assert_instances = |document: &StageDocument,
                            preview: &ModelPreview,
                            factory_name: &str,
                            resource_name: &str,
                            expected_count: usize,
                            expected_model_suffix: &str,
                            expected_triangles: usize| {
        let objects = document
            .objects
            .iter()
            .filter(|object| {
                object.factory_name == factory_name
                    && object.raw_param("actor_tail_string") == Some(resource_name)
            })
            .collect::<Vec<_>>();
        assert_eq!(
            objects.len(),
            expected_count,
            "{} retail {factory_name}/{resource_name} placement count",
            document.stage_id
        );

        for object in objects {
            assert!(
                object.asset_hints.iter().any(|hint| {
                    hint.role == AssetRole::InferredPreviewModel
                        && hint
                            .path
                            .replace('\\', "/")
                            .to_ascii_lowercase()
                            .ends_with(expected_model_suffix)
                }),
                "{} {factory_name}/{resource_name} selected the wrong model: {:?}",
                document.stage_id,
                object.asset_hints
            );
            let model_index = preview
                .object_model_indices
                .get(&object.id)
                .unwrap_or_else(|| {
                    panic!(
                        "{} {factory_name}/{resource_name} has no rendered model index",
                        document.stage_id
                    )
                });
            let triangle_count = preview
                .triangles
                .iter()
                .filter(|triangle| triangle.model_index == *model_index)
                .count();
            assert_eq!(
                triangle_count, expected_triangles,
                "{} {factory_name}/{resource_name} rendered the wrong model",
                document.stage_id
            );
        }
    };

    let mamma = StageDocument::open(&base_root, "mamma0")
        .expect("open mamma0")
        .with_registry(registry.clone());
    let mamma_preview =
        SmsEditorApp::build_model_preview(&mamma, visibility).expect("build mamma0 preview");
    for (resource, triangles) in [
        ("SandBombBaseMushroom", 394),
        ("SandBombBasePyramid", 104),
        ("SandBombBaseShit", 326),
        ("SandBombBaseStar", 328),
        ("SandBombBaseTurtle", 780),
        ("SandBombBaseFoot", 649),
        ("SandBombBaseStairs", 202),
    ] {
        let suffix = format!("/mapobj/{}.bmd", resource.to_ascii_lowercase());
        assert_instances(
            &mamma,
            &mamma_preview,
            "SandBombBase",
            resource,
            1,
            &suffix,
            triangles,
        );
    }
    assert_instances(
        &mamma,
        &mamma_preview,
        "BananaTree",
        "BananaTree",
        19,
        "/mapobj/bananatree.bmd",
        448,
    );
    assert_instances(
        &mamma,
        &mamma_preview,
        "MapObjTreeScale",
        "BananaTree",
        2,
        "/mapobj/bananatree.bmd",
        448,
    );
    for (resource, count, triangles) in [("palmNormal", 3, 638), ("palmLeaf", 3, 516)] {
        let suffix = format!("/mapobj/{}.bmd", resource.to_ascii_lowercase());
        assert_instances(
            &mamma,
            &mamma_preview,
            "Palm",
            resource,
            count,
            &suffix,
            triangles,
        );
    }

    let dolpic = StageDocument::open(&base_root, "dolpic10")
        .expect("open dolpic10")
        .with_registry(registry);
    let dolpic_preview =
        SmsEditorApp::build_model_preview(&dolpic, visibility).expect("build dolpic10 preview");
    for (resource, count, triangles) in [("palmNormal", 18, 638), ("palmLeaf", 5, 516)] {
        let suffix = format!("/mapobj/{}.bmd", resource.to_ascii_lowercase());
        assert_instances(
            &dolpic,
            &dolpic_preview,
            "Palm",
            resource,
            count,
            &suffix,
            triangles,
        );
    }
    for (resource, count, triangles) in [
        ("FruitCoconut", 9, 234),
        ("FruitPapaya", 3, 280),
        ("FruitPine", 3, 362),
        ("FruitDurian", 3, 360),
        ("FruitBanana", 6, 248),
        ("RedPepper", 4, 98),
    ] {
        let suffix = format!("/mapobj/{}.bmd", resource.to_ascii_lowercase());
        assert_instances(
            &dolpic,
            &dolpic_preview,
            "ResetFruit",
            resource,
            count,
            &suffix,
            triangles,
        );
    }
}

#[test]
#[ignore = "requires SMS_BASE_ROOT with extracted Japanese retail assets"]
fn retail_surf_geso_overrides_render_all_variants_with_runtime_colors() {
    let base_root = retail_base_root();
    let decomp_root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../..");
    let registry = SchemaGenerator::new(decomp_root)
        .generate()
        .expect("generate decomp-derived SurfGeso metadata");
    let archives = discover_scene_archives(&base_root).expect("discover retail scene archives");
    let expected_colors = BTreeMap::from([
        ("SurfGesoRed", [255, 180, 255, 255]),
        ("SurfGesoYellow", [255, 255, 125, 255]),
        ("SurfGesoGreen", [180, 255, 180, 255]),
    ]);
    let mut rendered = BTreeMap::<String, usize>::new();

    for archive in archives {
        let document = StageDocument::open(&base_root, &archive.stage_id)
            .unwrap_or_else(|error| panic!("open {}: {error}", archive.stage_id))
            .with_registry(registry.clone());
        let objects = document
            .objects
            .iter()
            .filter(|object| expected_colors.contains_key(object.factory_name.as_str()))
            .collect::<Vec<_>>();
        if objects.is_empty() {
            continue;
        }
        let preview = SmsEditorApp::build_model_preview(
            &document,
            PreviewVisibility {
                environment: false,
                goop: false,
                effects: false,
            },
        )
        .unwrap_or_else(|| panic!("build {} preview", archive.stage_id));

        for object in objects {
            let expected_color = expected_colors[object.factory_name.as_str()];
            assert_eq!(
                object.raw_param("actor_tail_string"),
                Some(object.factory_name.as_str())
            );
            assert!(object.asset_hints.iter().any(|hint| {
                hint.role == AssetRole::InferredPreviewModel
                    && hint
                        .path
                        .replace('\\', "/")
                        .to_ascii_lowercase()
                        .ends_with("/mapobj/surfgeso.bmd")
            }));
            let model_index = preview
                .object_model_indices
                .get(&object.id)
                .unwrap_or_else(|| {
                    panic!(
                        "{} {} has no rendered SurfGeso model",
                        archive.stage_id, object.factory_name
                    )
                });
            let triangles = preview
                .triangles
                .iter()
                .filter(|triangle| triangle.model_index == *model_index)
                .collect::<Vec<_>>();
            assert!(
                !triangles.is_empty(),
                "{} {} has no geometry",
                archive.stage_id,
                object.factory_name
            );
            let material_indices = triangles
                .iter()
                .filter_map(|triangle| triangle.material_index)
                .collect::<BTreeSet<_>>();
            assert!(!material_indices.is_empty());
            for material_index in material_indices {
                assert_eq!(
                    preview.materials[material_index].tev_colors[1], expected_color,
                    "{} {} TEVREG1",
                    archive.stage_id, object.factory_name
                );
            }
            *rendered.entry(object.factory_name.clone()).or_default() += 1;
        }
    }

    assert_eq!(
        rendered,
        BTreeMap::from([
            ("SurfGesoGreen".to_string(), 4),
            ("SurfGesoRed".to_string(), 4),
            ("SurfGesoYellow".to_string(), 4),
        ]),
        "all 12 Japanese retail SurfGeso placements"
    );
}

#[test]
#[ignore = "requires SMS_BASE_ROOT with extracted Japanese retail assets"]
fn retail_map_obj_indirect_flags_render_all_dokan_gate_and_ice_block_placements() {
    let base_root = retail_base_root();
    let decomp_root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../..");
    let registry = SchemaGenerator::new(decomp_root)
        .generate()
        .expect("generate decomp-derived MapObj loader flags");
    let expected = BTreeMap::from([
        ("dolpic0", vec![("MapObjBase", "DokanGate", 2)]),
        ("dolpic1", vec![("MapObjBase", "DokanGate", 2)]),
        ("dolpic5", vec![("MapObjBase", "DokanGate", 2)]),
        ("dolpic6", vec![("MapObjBase", "DokanGate", 2)]),
        ("dolpic7", vec![("MapObjBase", "DokanGate", 2)]),
        ("dolpic8", vec![("MapObjBase", "DokanGate", 2)]),
        ("dolpic9", vec![("MapObjBase", "DokanGate", 2)]),
        ("dolpic10", vec![("MapObjBase", "DokanGate", 2)]),
        ("coro_ex5", vec![("IceBlock", "IceBlock", 4)]),
        ("dolpic_ex0", vec![("IceBlock", "IceBlock", 1)]),
        ("mare0", vec![("IceBlock", "IceBlock", 1)]),
        ("mare1", vec![("IceBlock", "IceBlock", 1)]),
        ("mare3", vec![("IceBlock", "IceBlock", 1)]),
        ("mare4", vec![("IceBlock", "IceBlock", 1)]),
        ("mare6", vec![("IceBlock", "IceBlock", 1)]),
        ("mare7", vec![("IceBlock", "IceBlock", 1)]),
        ("sirena_ex1", vec![("IceBlock", "IceBlock", 1)]),
    ]);
    let expected_census = expected
        .iter()
        .flat_map(|(stage_id, resources)| {
            resources
                .iter()
                .map(|(factory_name, resource_name, count)| {
                    (
                        (
                            (*stage_id).to_string(),
                            (*factory_name).to_string(),
                            (*resource_name).to_string(),
                        ),
                        *count,
                    )
                })
        })
        .collect::<BTreeMap<_, _>>();
    assert_eq!(
        retail_placement_census(
            &base_root,
            &[("MapObjBase", "DokanGate"), ("IceBlock", "IceBlock")]
        ),
        expected_census,
        "complete Japanese retail DokanGate and IceBlock placement census"
    );
    let mut totals = BTreeMap::<String, usize>::new();

    for (stage_id, resources) in expected {
        let document = StageDocument::open(&base_root, stage_id)
            .unwrap_or_else(|error| panic!("open {stage_id}: {error}"))
            .with_registry(registry.clone());
        let preview = SmsEditorApp::build_model_preview(
            &document,
            PreviewVisibility {
                environment: false,
                goop: false,
                effects: false,
            },
        )
        .unwrap_or_else(|| panic!("build {stage_id} preview"));

        for (factory_name, resource_name, expected_count) in resources {
            let objects = document
                .objects
                .iter()
                .filter(|object| {
                    object.factory_name == factory_name
                        && object.raw_param("actor_tail_string") == Some(resource_name)
                })
                .collect::<Vec<_>>();
            assert_eq!(objects.len(), expected_count, "{stage_id} {resource_name}");
            for object in objects {
                assert_eq!(
                    document.object_preview_load_flags(object),
                    Some(0x1122_0000),
                    "{stage_id} {resource_name} schema flags"
                );
                let model_index = preview
                    .object_model_indices
                    .get(&object.id)
                    .unwrap_or_else(|| panic!("{stage_id} {resource_name} has no rendered model"));
                let material_indices = preview
                    .triangles
                    .iter()
                    .filter(|triangle| triangle.model_index == *model_index)
                    .filter_map(|triangle| triangle.material_index)
                    .collect::<BTreeSet<_>>();
                assert!(!material_indices.is_empty(), "{stage_id} {resource_name}");
                assert!(material_indices
                    .iter()
                    .all(|index| preview.materials[*index].loader_flags == 0x1122_0000));
                assert!(material_indices
                    .iter()
                    .any(|index| preview.materials[*index].indirect.enabled));
                *totals.entry(resource_name.to_string()).or_default() += 1;
            }
        }
    }
    assert_eq!(
        totals,
        BTreeMap::from([("DokanGate".to_string(), 16), ("IceBlock".to_string(), 12)])
    );
}

#[test]
#[ignore = "requires SMS_BASE_ROOT with extracted Japanese retail assets"]
fn retail_mare_pollution_uses_map_static_flags_and_keeps_collision_only_rows_model_less() {
    let base_root = retail_base_root();
    let decomp_root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../..");
    let registry = SchemaGenerator::new(decomp_root)
        .generate()
        .expect("generate decomp-derived map-static flags");
    assert_eq!(
        retail_placement_census(
            &base_root,
            &[
                ("MapStaticObj", "mareSeaPollutionS0"),
                ("MapStaticObj", "mareSeaPollutionS12"),
                ("MapStaticObj", "mareSeaPollutionS34567"),
            ]
        ),
        BTreeMap::from([
            (
                (
                    "mare0".to_string(),
                    "MapStaticObj".to_string(),
                    "mareSeaPollutionS0".to_string(),
                ),
                1,
            ),
            (
                (
                    "mare1".to_string(),
                    "MapStaticObj".to_string(),
                    "mareSeaPollutionS12".to_string(),
                ),
                1,
            ),
            (
                (
                    "mare2".to_string(),
                    "MapStaticObj".to_string(),
                    "mareSeaPollutionS12".to_string(),
                ),
                1,
            ),
            (
                (
                    "mare3".to_string(),
                    "MapStaticObj".to_string(),
                    "mareSeaPollutionS12".to_string(),
                ),
                1,
            ),
            (
                (
                    "mare4".to_string(),
                    "MapStaticObj".to_string(),
                    "mareSeaPollutionS34567".to_string(),
                ),
                1,
            ),
            (
                (
                    "mare5".to_string(),
                    "MapStaticObj".to_string(),
                    "mareSeaPollutionS34567".to_string(),
                ),
                1,
            ),
            (
                (
                    "mare6".to_string(),
                    "MapStaticObj".to_string(),
                    "mareSeaPollutionS34567".to_string(),
                ),
                1,
            ),
            (
                (
                    "mare7".to_string(),
                    "MapStaticObj".to_string(),
                    "mareSeaPollutionS34567".to_string(),
                ),
                1,
            ),
        ]),
        "complete Japanese retail Mare pollution placement census"
    );

    for (stage_id, resource_name, material_name, expected_triangles) in [
        ("mare0", "mareSeaPollutionS0", "_pollutionSea_s0o_1", 786),
        ("mare1", "mareSeaPollutionS12", "_pollutionSea_s123o_3", 761),
        ("mare2", "mareSeaPollutionS12", "_pollutionSea_s123o_3", 761),
        ("mare3", "mareSeaPollutionS12", "_pollutionSea_s123o_3", 761),
    ] {
        let document = StageDocument::open(&base_root, stage_id)
            .unwrap_or_else(|error| panic!("open {stage_id}: {error}"))
            .with_registry(registry.clone());
        let object = document
            .objects
            .iter()
            .find(|object| {
                object.factory_name == "MapStaticObj"
                    && object.raw_param("actor_tail_string") == Some(resource_name)
            })
            .unwrap_or_else(|| panic!("{stage_id} {resource_name} placement"));
        assert_eq!(
            document.object_preview_load_flags(object),
            Some(0x1021_0000)
        );
        let model_asset = document
            .assets
            .iter()
            .find(|asset| {
                asset
                    .path
                    .to_string_lossy()
                    .replace('\\', "/")
                    .to_ascii_lowercase()
                    .ends_with(&format!(
                        "/map/map/{}.bmd",
                        resource_name.to_ascii_lowercase()
                    ))
            })
            .unwrap_or_else(|| panic!("{stage_id} {resource_name}.bmd"));
        assert_eq!(
            map_static_model_loader_flags(&document, &model_asset.path.to_string_lossy()),
            Some(0x1021_0000)
        );
        let preview = SmsEditorApp::build_model_preview(
            &document,
            PreviewVisibility {
                environment: true,
                goop: true,
                effects: false,
            },
        )
        .unwrap_or_else(|| panic!("build {stage_id} preview"));
        let target_materials = preview
            .materials
            .iter()
            .enumerate()
            .filter(|(_, material)| material.name == material_name)
            .map(|(index, _)| index)
            .collect::<BTreeSet<_>>();
        assert!(!target_materials.is_empty(), "{stage_id} {material_name}");
        let target_triangles = preview
            .triangles
            .iter()
            .filter(|triangle| {
                triangle
                    .material_index
                    .is_some_and(|index| target_materials.contains(&index))
            })
            .count();
        assert_eq!(
            target_triangles, expected_triangles,
            "{stage_id} {material_name}"
        );
        assert!(target_materials.iter().all(|index| {
            let material = &preview.materials[*index];
            material.loader_flags == 0x1021_0000 && !material.indirect.enabled
        }));
    }

    for stage_id in ["mare4", "mare5", "mare6", "mare7"] {
        let document = StageDocument::open(&base_root, stage_id)
            .unwrap_or_else(|error| panic!("open {stage_id}: {error}"))
            .with_registry(registry.clone());
        let object = document
            .objects
            .iter()
            .find(|object| {
                object.factory_name == "MapStaticObj"
                    && object.raw_param("actor_tail_string") == Some("mareSeaPollutionS34567")
            })
            .unwrap_or_else(|| panic!("{stage_id} collision-only pollution placement"));
        assert_eq!(
            document.object_preview_load_flags(object),
            Some(0x1021_0000)
        );
        assert!(object
            .asset_hints
            .iter()
            .all(|hint| hint.role != AssetRole::InferredPreviewModel));
        assert!(document.assets.iter().all(|asset| {
            !asset
                .path
                .to_string_lossy()
                .replace('\\', "/")
                .to_ascii_lowercase()
                .ends_with("/map/map/mareseapollutions34567.bmd")
        }));
    }
}

#[test]
#[ignore = "requires SMS_BASE_ROOT with extracted Japanese retail assets"]
fn retail_nozzle_colors_and_red_pepper_offsets_reach_rendered_materials_and_geometry() {
    let base_root = retail_base_root();
    let decomp_root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../..");
    let registry = SchemaGenerator::new(decomp_root)
        .generate()
        .expect("generate decomp-derived typed preview programs");
    let archives = discover_scene_archives(&base_root).expect("discover retail scene archives");
    let expected_nozzle_colors = BTreeMap::from([
        ("normal_nozzle_item", [0, 0, 255, 100]),
        ("rocket_nozzle_item", [255, 0, 0, 100]),
        ("back_nozzle_item", [90, 90, 120, 100]),
    ]);
    let mut nozzle_counts = BTreeMap::<String, usize>::new();
    let mut red_pepper_by_stage = BTreeMap::<String, usize>::new();
    for archive in archives {
        let document = StageDocument::open(&base_root, &archive.stage_id)
            .unwrap_or_else(|error| panic!("open {}: {error}", archive.stage_id))
            .with_registry(registry.clone());
        for object in &document.objects {
            if object.factory_name == "NozzleBox" {
                assert_eq!(object.raw_param("actor_tail_string"), Some("NozzleBox"));
                let selector = object
                    .raw_param("nozzle_box_item")
                    .unwrap_or_else(|| panic!("{} NozzleBox selector", archive.stage_id));
                let expected = expected_nozzle_colors
                    .get(selector)
                    .unwrap_or_else(|| panic!("unknown retail NozzleBox selector {selector}"));
                assert_eq!(
                    map_obj_string_tev_color(object, document.registry.as_ref())
                        .map(|definition| definition.color),
                    Some(*expected)
                );
                *nozzle_counts.entry(selector.to_string()).or_default() += 1;
            }
            if object.factory_name == "ResetFruit"
                && object.raw_param("actor_tail_string") == Some("RedPepper")
            {
                *red_pepper_by_stage
                    .entry(archive.stage_id.clone())
                    .or_default() += 1;
            }
        }
    }
    assert_eq!(
        nozzle_counts,
        BTreeMap::from([
            ("back_nozzle_item".to_string(), 47),
            ("normal_nozzle_item".to_string(), 47),
            ("rocket_nozzle_item".to_string(), 61),
        ])
    );
    assert_eq!(
        red_pepper_by_stage,
        BTreeMap::from([
            ("delfinoBoss".to_string(), 10),
            ("dolpic0".to_string(), 4),
            ("dolpic1".to_string(), 4),
            ("dolpic5".to_string(), 4),
            ("dolpic6".to_string(), 4),
            ("dolpic7".to_string(), 4),
            ("dolpic8".to_string(), 4),
            ("dolpic9".to_string(), 3),
            ("dolpic10".to_string(), 4),
        ])
    );

    let mamma = StageDocument::open(&base_root, "mamma0")
        .expect("open mamma0")
        .with_registry(registry.clone());
    let mamma_preview = SmsEditorApp::build_model_preview(
        &mamma,
        PreviewVisibility {
            environment: false,
            goop: false,
            effects: false,
        },
    )
    .expect("build mamma0 preview");
    let mut nozzle_materials = BTreeMap::<String, BTreeSet<usize>>::new();
    for object in mamma
        .objects
        .iter()
        .filter(|object| object.factory_name == "NozzleBox")
    {
        let selector = object.raw_param("nozzle_box_item").expect("typed selector");
        let model_index = mamma_preview
            .object_model_indices
            .get(&object.id)
            .unwrap_or_else(|| panic!("mamma0 {selector} rendered model"));
        let indices = mamma_preview
            .triangles
            .iter()
            .filter(|triangle| triangle.model_index == *model_index)
            .filter_map(|triangle| triangle.material_index)
            .collect::<BTreeSet<_>>();
        assert!(!indices.is_empty(), "mamma0 {selector} materials");
        assert!(indices.iter().all(|index| {
            mamma_preview.materials[*index].tev_colors[1] == expected_nozzle_colors[selector]
        }));
        nozzle_materials.insert(selector.to_string(), indices);
    }
    assert_eq!(nozzle_materials.len(), 3);
    let sets = nozzle_materials.values().collect::<Vec<_>>();
    for left in 0..sets.len() {
        for right in left + 1..sets.len() {
            assert!(sets[left].is_disjoint(sets[right]));
        }
    }

    let dolpic = StageDocument::open(&base_root, "dolpic0")
        .expect("open dolpic0")
        .with_registry(registry);
    let dolpic_preview = SmsEditorApp::build_model_preview(
        &dolpic,
        PreviewVisibility {
            environment: false,
            goop: false,
            effects: false,
        },
    )
    .expect("build dolpic0 preview");
    let red_peppers = dolpic
        .objects
        .iter()
        .filter(|object| {
            object.factory_name == "ResetFruit"
                && object.raw_param("actor_tail_string") == Some("RedPepper")
        })
        .collect::<Vec<_>>();
    assert_eq!(red_peppers.len(), 4);
    for object in red_peppers {
        assert_eq!(object.transform.translation[1], 300.0);
        let model_index = dolpic_preview
            .object_model_indices
            .get(&object.id)
            .expect("rendered RedPepper model");
        let min_y = dolpic_preview
            .triangles
            .iter()
            .filter(|triangle| triangle.model_index == *model_index)
            .flat_map(|triangle| triangle.vertices.iter().map(|vertex| vertex[1]))
            .fold(f32::INFINITY, f32::min);
        assert!(
            (min_y - 349.945_3).abs() < 0.01,
            "dolpic0 RedPepper {} min Y {min_y}",
            object.id
        );
    }
}

#[test]
#[ignore = "requires an extracted retail base root and is a manual performance probe"]
fn profiles_dolpic0_preview_and_animation_updates() {
    let base_root = retail_base_root();
    let decomp_root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../..");
    let registry = SchemaGenerator::new(decomp_root)
        .generate()
        .expect("generate decomp-derived object metadata");
    let document = StageDocument::open(&base_root, "dolpic0")
        .expect("open dolpic0")
        .with_registry(registry);

    let build_started = Instant::now();
    let preview = SmsEditorApp::build_model_preview(
        &document,
        PreviewVisibility {
            environment: true,
            goop: true,
            effects: false,
        },
    )
    .expect("build Dolpic preview");
    let build_elapsed = build_started.elapsed();
    let triangle_count = preview.triangles.len();
    let animated_model_count = preview.animated_models.len();
    let rotating_model_count = preview.rotating_models.len();
    let actor_particle_count = preview.actor_particles.len();

    let gpu_started = Instant::now();
    let gpu_viewport = gpu_viewport::GpuViewportScene::from_preview(
        &preview,
        eframe::wgpu::TextureFormat::Bgra8UnormSrgb,
    );
    let gpu_elapsed = gpu_started.elapsed();
    let mut app = SmsEditorApp {
        document: Some(document),
        model_preview: Some(preview),
        gpu_viewport: Some(gpu_viewport),
        ..SmsEditorApp::default()
    };

    eprintln!(
        "dolpic0 preview: build={build_elapsed:?}, gpu_prepare={gpu_elapsed:?}, triangles={triangle_count}, animated_models={animated_model_count}, rotating_models={rotating_model_count}, actor_particles={actor_particle_count}"
    );
    for seconds in [1_u64, 10, 60, 300] {
        app.animation_started_at = Instant::now() - std::time::Duration::from_secs(seconds);
        app.last_skeletal_animation_tick = u64::MAX;
        let started = Instant::now();
        app.update_skeletal_animations();
        eprintln!(
            "dolpic0 animation sample at {seconds}s: {:?}",
            started.elapsed()
        );
    }

    let base_preview = app
        .model_preview
        .take()
        .expect("Dolpic preview remains loaded");
    let measure_cpu = |label: &str, preview: ModelPreview| {
        let mut app = SmsEditorApp {
            model_preview: Some(preview),
            animation_started_at: Instant::now() - std::time::Duration::from_secs(60),
            last_skeletal_animation_tick: u64::MAX,
            ..SmsEditorApp::default()
        };
        let started = Instant::now();
        app.update_skeletal_animations();
        eprintln!("dolpic0 {label} CPU sample: {:?}", started.elapsed());
        std::hint::black_box(app.model_preview.take().expect("profile preview"));
    };

    measure_cpu("full", base_preview.clone());
    let mut skeletal = base_preview.clone();
    skeletal.rotating_models.clear();
    skeletal.actor_particles.clear();
    skeletal.texture_pattern_animations.clear();
    measure_cpu("skeletal-only", skeletal);
    let mut rotating = base_preview.clone();
    rotating.animated_models.clear();
    rotating.actor_particles.clear();
    rotating.texture_pattern_animations.clear();
    measure_cpu("rotating-only", rotating);
    let mut particles = base_preview.clone();
    particles.animated_models.clear();
    particles.rotating_models.clear();
    particles.texture_pattern_animations.clear();
    measure_cpu("particles-only", particles);
}

#[test]
#[ignore = "requires an extracted retail base root"]
fn renders_maremb_body_and_accessories_to_screenshot() {
    let base_root = retail_base_root();
    let output = env::var_os(OUTPUT_ENV)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("target/noki-render/maremb-accessories.bmp"));

    let document = open_mare0_with_schema(&base_root);
    let object = document
        .objects
        .iter()
        .find(|object| {
            object.factory_name == "NPCMareMB"
                && object
                    .raw_params
                    .get("npc_parts_mask")
                    .and_then(|mask| mask.parse::<u32>().ok())
                    .is_some_and(|mask| mask & (1 << 9) != 0)
        })
        .cloned()
        .expect("mare0 should contain the MareMB fishing NPC");
    let preview = SmsEditorApp::build_model_preview(
        &document,
        PreviewVisibility {
            environment: false,
            goop: false,
            effects: false,
        },
    )
    .expect("build Noki preview");
    let model_index = *preview
        .object_model_indices
        .get(&object.id)
        .expect("Noki model index");
    let animated_instance = preview
        .animated_models
        .iter()
        .flat_map(|model| &model.instances)
        .find(|instance| instance.model_index == model_index)
        .expect("animated MareMB instance");
    for accessory in &animated_instance.accessories {
        let triangles = &preview.triangles[accessory.triangle_range.clone()];
        let textured = triangles
            .iter()
            .filter(|triangle| triangle.texture_index.is_some())
            .count();
        assert_eq!(
            textured,
            triangles.len(),
            "Noki accessory contains untextured triangles"
        );
    }
    assert_root_accessory_meets_hand_grip(&preview, model_index);
    let body_material = preview
        .triangles
        .iter()
        .filter(|triangle| triangle.model_index == model_index)
        .filter_map(|triangle| triangle.material_index)
        .map(|index| &preview.materials[index])
        .find(|material| material.name.eq_ignore_ascii_case("_body"))
        .expect("Noki body material");
    assert_eq!(
        body_material.tev_k_colors[0][3], 0,
        "runtime pollution initialization must hide the dirty-layer default"
    );
    render_isolated_noki(document, object, preview, model_index, output);
}

#[test]
#[ignore = "requires an extracted retail base root"]
fn renders_marem_instance_palette_to_screenshot() {
    let base_root = retail_base_root();
    let output = env::var_os("SMS_MAREM_TEST_OUTPUT")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("target/noki-render/marem-instance-palette.bmp"));

    let document = open_mare0_with_schema(&base_root);
    let object = document
        .objects
        .iter()
        .find(|object| {
            object.factory_name == "NPCMareM"
                && object
                    .raw_params
                    .get("npc_parts_mask")
                    .is_some_and(|mask| mask == "97")
        })
        .cloned()
        .expect("mare0 should contain the palette-3 MareM NPC");
    let preview = SmsEditorApp::build_model_preview(
        &document,
        PreviewVisibility {
            environment: false,
            goop: false,
            effects: false,
        },
    )
    .expect("build MareM preview");
    let model_index = *preview
        .object_model_indices
        .get(&object.id)
        .expect("MareM model index");
    let instance = preview
        .animated_models
        .iter()
        .flat_map(|model| &model.instances)
        .find(|instance| instance.model_index == model_index)
        .expect("animated MareM instance");
    let hat = instance.accessories.first().expect("MareM Hat A accessory");
    let hat_material = preview.triangles[hat.triangle_range.clone()]
        .iter()
        .filter_map(|triangle| triangle.material_index)
        .map(|index| &preview.materials[index])
        .find(|material| material.name.eq_ignore_ascii_case("_mat1"))
        .expect("MareM Hat A material");
    assert_eq!(hat_material.tev_colors[1], [10, 10, 10, 255]);
    assert_eq!(hat_material.tev_colors[2], [150, -30, 40, 255]);

    render_isolated_noki(document, object, preview, model_index, output);
}

fn render_isolated_noki(
    document: StageDocument,
    object: SceneObject,
    mut preview: ModelPreview,
    model_index: usize,
    output: PathBuf,
) {
    preview
        .triangles
        .retain(|triangle| triangle.model_index == model_index);
    assert!(
        !preview.triangles.is_empty(),
        "Noki preview has no triangles"
    );
    assert!(
        preview
            .triangles
            .iter()
            .any(|triangle| triangle.texture_index.is_some()),
        "Noki preview has no textured triangles"
    );

    let mut app = SmsEditorApp {
        document: Some(document),
        model_preview: Some(preview),
        ..SmsEditorApp::default()
    };
    let camera = app.renderer.camera_mut();
    camera.focus = [
        object.transform.translation[0],
        object.transform.translation[1] + 70.0,
        object.transform.translation[2],
    ];
    camera.yaw_degrees = object.transform.rotation_degrees[1] + 180.0;
    camera.pitch_degrees = -8.0;
    camera.distance = 360.0;

    let frame = app.camera_frame();
    let lighting = app
        .document
        .as_ref()
        .and_then(|document| document.lighting.object_lighting())
        .expect("mare0 object lighting");
    let size = [640, 640];
    let image = gpu_viewport::render_preview_offscreen(
        app.model_preview.as_ref().expect("Noki preview"),
        gpu_viewport::GpuViewportFrame {
            camera_position: frame.position,
            right: frame.right,
            up: frame.up,
            forward: frame.forward,
            focal: perspective_focal_length(
                egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(640.0, 640.0)),
                1.0,
            ),
            viewport_size: [640.0, 640.0],
            viewport_pan: [0.0; 2],
            near: 8.0,
            animation_seconds: 0.0,
            light_position: lighting.position,
            light_color: gpu_viewport::color_u8_to_f32(lighting.color),
            ambient_color: Some(gpu_viewport::color_u8_to_f32(lighting.ambient)),
        },
        size,
    )
    .expect("render Noki WGPU framebuffer");
    let chromatic_pixels = image
        .pixels
        .iter()
        .filter(|pixel| {
            let [red, green, blue, _] = pixel.to_srgba_unmultiplied();
            let min = red.min(green).min(blue);
            let max = red.max(green).max(blue);
            max > 40 && max - min > 24
        })
        .count();
    assert!(
        chromatic_pixels > 2_000,
        "Noki render regressed to monochrome ({chromatic_pixels} chromatic pixels)"
    );
    if let Some(parent) = output.parent() {
        fs::create_dir_all(parent).expect("create screenshot directory");
    }
    write_bmp(&output, &image).expect("write Noki screenshot");
    eprintln!("Noki rendering screenshot: {}", output.display());
}

fn open_mare0_with_schema(base_root: &std::path::Path) -> StageDocument {
    let decomp_root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../..");
    let registry = SchemaGenerator::new(decomp_root)
        .generate()
        .expect("generate decomp-derived NPC metadata");
    StageDocument::open(base_root, "mare0")
        .expect("open mare0")
        .with_registry(registry)
}

fn assert_root_accessory_meets_hand_grip(preview: &ModelPreview, model_index: usize) {
    let source = preview
        .animated_models
        .iter()
        .find(|model| {
            model
                .instances
                .iter()
                .any(|instance| instance.model_index == model_index)
        })
        .expect("animated Noki source");
    let instance = source
        .instances
        .iter()
        .find(|instance| instance.model_index == model_index)
        .expect("animated Noki instance");
    let root_accessory = instance
        .accessories
        .iter()
        .find(|accessory| accessory.joint_index.is_none())
        .expect("root-attached fishing rod");
    let joint_names = source.file.joint_names().expect("Noki joint names");
    let hand_index = joint_names
        .iter()
        .position(|name| name.eq_ignore_ascii_case("migite"))
        .expect("Noki right hand joint");
    let matrices = source
        .file
        .joint_matrices_with_joint_animation(source.loader_flags, &source.animation, 0.0)
        .expect("Noki wait-pose matrices");
    let hand = [
        matrices[hand_index][0][3],
        matrices[hand_index][1][3],
        matrices[hand_index][2][3],
    ];
    let nearest_vertex_distance = root_accessory
        .local_triangles
        .iter()
        .flat_map(|triangle| triangle.vertices)
        .map(|vertex| {
            vertex
                .into_iter()
                .zip(hand)
                .map(|(vertex, hand)| (vertex - hand).powi(2))
                .sum::<f32>()
                .sqrt()
        })
        .fold(f32::INFINITY, f32::min);
    // The authored palm and rod surfaces do not share vertices. A nearby rod
    // vertex is a substantially tighter regression check than overlapping the
    // hand with the accessory's full (rod-length) bounding box.
    assert!(
        nearest_vertex_distance < 15.0,
        "root accessory no longer meets the animated hand grip: distance={nearest_vertex_distance}, hand={hand:?}"
    );
}

fn write_bmp(path: &std::path::Path, image: &egui::ColorImage) -> std::io::Result<()> {
    let width = image.size[0];
    let height = image.size[1];
    let row_size = (width * 3 + 3) & !3;
    let pixel_size = row_size * height;
    let file_size = 14 + 40 + pixel_size;
    let mut bytes = Vec::with_capacity(file_size);
    bytes.extend_from_slice(b"BM");
    bytes.extend_from_slice(&(file_size as u32).to_le_bytes());
    bytes.extend_from_slice(&[0; 4]);
    bytes.extend_from_slice(&(54u32).to_le_bytes());
    bytes.extend_from_slice(&(40u32).to_le_bytes());
    bytes.extend_from_slice(&(width as i32).to_le_bytes());
    bytes.extend_from_slice(&(height as i32).to_le_bytes());
    bytes.extend_from_slice(&(1u16).to_le_bytes());
    bytes.extend_from_slice(&(24u16).to_le_bytes());
    bytes.extend_from_slice(&[0; 4]);
    bytes.extend_from_slice(&(pixel_size as u32).to_le_bytes());
    bytes.extend_from_slice(&[0; 16]);

    for y in (0..height).rev() {
        for color in &image.pixels[y * width..(y + 1) * width] {
            let [red, green, blue, _] = color.to_srgba_unmultiplied();
            bytes.extend_from_slice(&[blue, green, red]);
        }
        bytes.resize(bytes.len() + row_size - width * 3, 0);
    }
    fs::write(path, bytes)
}
