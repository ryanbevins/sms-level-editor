use super::*;

fn assert_vec3_close(actual: [f32; 3], expected: [f32; 3]) {
    for (actual, expected) in actual.into_iter().zip(expected) {
        assert!(
            (actual - expected).abs() < 0.001,
            "expected {expected}, got {actual}"
        );
    }
}

fn camera_app() -> SmsEditorApp {
    let mut app = SmsEditorApp::default();
    {
        let camera = app.renderer.camera_mut();
        camera.focus = [0.0, 0.0, 1000.0];
        camera.yaw_degrees = 0.0;
        camera.pitch_degrees = 0.0;
        camera.distance = 1000.0;
    }
    app
}

#[test]
fn nozzle_box_tev_color_matches_runtime_item_type() {
    let mut rocket = SceneObject::new("rocket-box", "NozzleBox");
    rocket.raw_params.insert(
        "stream_string_1".to_string(),
        "rocket_nozzle_item".to_string(),
    );
    let mut hover = SceneObject::new("hover-box", "NozzleBox");
    hover.raw_params.insert(
        "stream_string_1".to_string(),
        "back_nozzle_item".to_string(),
    );

    assert_eq!(nozzle_box_tev_reg1_color(&rocket), Some([255, 0, 0, 100]));
    assert_eq!(nozzle_box_tev_reg1_color(&hover), Some([90, 90, 120, 100]));
    assert_eq!(
        nozzle_box_tev_reg1_color(&SceneObject::new("coin", "Coin")),
        None
    );
}

#[test]
fn monte_material_colors_follow_retail_instance_indices() {
    let mut monte = SceneObject::new("monte", "NPCMonteMA");
    monte
        .raw_params
        .insert("npc_body_color_index".to_string(), "9".to_string());
    monte
        .raw_params
        .insert("npc_cloth_color_index".to_string(), "3".to_string());

    let colors = monte_material_colors(&monte).unwrap();
    assert_eq!(colors.body_reg0, Some([400, 250, 100, 255]));
    assert_eq!(
        colors.cloth_reg1_reg2,
        Some([[200, 200, 170, 255], [200, 200, 170, 255]])
    );
}

#[test]
fn npc_pollution_uses_white_k_color_with_amount_as_alpha() {
    let mut monte = SceneObject::new("monte", "NPCMonteMA");
    monte
        .raw_params
        .insert("npc_pollution_amount".to_string(), "37".to_string());
    monte
        .raw_params
        .insert("npc_parts_color_index_0".to_string(), "2".to_string());

    assert_eq!(npc_pollution_k_color(&monte), Some([255, 255, 255, 37]));
}

#[test]
fn material_table_candidates_include_base_actor_table() {
    assert_eq!(
        material_table_candidates_for_model("C:/game/dolpic.szs!/mapobj/kibako.bmd"),
        ["c:/game/dolpic.szs!/mapobj/kibako.bmt"]
    );
    assert_eq!(
        material_table_candidates_for_model("C:/game/dolpic.szs!/mapobj/kibako_crash.bmd"),
        [
            "c:/game/dolpic.szs!/mapobj/kibako_crash.bmt",
            "c:/game/dolpic.szs!/mapobj/kibako.bmt",
        ]
    );
    assert_eq!(
        material_table_candidates_for_model("C:/game/dolpic.szs!/mapobj/barrel_normal.bmd"),
        [
            "c:/game/dolpic.szs!/mapobj/barrel_normal.bmt",
            "c:/game/dolpic.szs!/mapobj/barrel.bmt",
        ]
    );
    assert_eq!(
        material_table_candidates_for_model("C:/game/dolpic.szs!/mapobj/barrel_offset.bmd"),
        [
            "c:/game/dolpic.szs!/mapobj/barrel_offset.bmt",
            "c:/game/dolpic.szs!/mapobj/barrel.bmt",
        ]
    );
    assert_eq!(
        material_table_candidates_for_model("C:/game/bianco0.szs!/mapobj/miniwindmilll.bmd"),
        [
            "c:/game/bianco0.szs!/mapobj/miniwindmilll.bmt",
            "c:/game/bianco0.szs!/mapobj/bianco.bmt",
        ]
    );
    assert_eq!(
        material_table_candidates_for_model("C:/game/mare0.szs!/marem/marem.bmd"),
        [
            "c:/game/mare0.szs!/marem/marem.bmt",
            "c:/game/mare0.szs!/marecommon/mare.bmt",
        ]
    );
}

#[test]
fn dummy_texture_names_resolve_shared_material_tables() {
    let textures = [sms_formats::J3dTexturePreview {
        name: "J_barrel_dammy".to_string(),
        width: 8,
        height: 8,
        format: 0,
        wrap_s: 0,
        wrap_t: 0,
        min_filter: 1,
        mag_filter: 1,
        mipmap_count: 1,
        rgba: vec![255; 8 * 8 * 4],
        mips: Vec::new(),
    }];

    assert_eq!(
        material_table_asset_score(
            "C:/game/dolpic.szs!/mapobj/barrel_normal.bmd",
            &textures,
            "C:/game/dolpic.szs!/mapobj/barrel.bmt",
        ),
        Some((3, 1))
    );
    assert_eq!(
        material_table_asset_score(
            "C:/game/dolpic.szs!/mapobj/barrel_variant.bmd",
            &textures,
            "C:/game/dolpic.szs!/mapobj/barrel.bmt",
        ),
        Some((2, "barrel".len()))
    );
    assert_eq!(
        material_table_asset_score(
            "C:/game/dolpic.szs!/mapobj/barrel_variant.bmd",
            &textures,
            "C:/game/bianco.szs!/mapobj/barrel.bmt",
        ),
        None
    );
    assert_eq!(
        material_table_asset_score(
            "C:/game/dolpic.szs!/actors/barrel_variant.bmd",
            &textures,
            "C:/game/dolpic.szs!/mapobj/barrel.bmt",
        ),
        Some((2, "barrel".len()))
    );
}

#[test]
fn normalized_dummy_names_resolve_differently_separated_model_names() {
    let textures = [sms_formats::J3dTexturePreview {
        name: "nozzleItem_dummy".to_string(),
        width: 1,
        height: 1,
        format: 0,
        wrap_s: 0,
        wrap_t: 0,
        min_filter: 1,
        mag_filter: 1,
        mipmap_count: 1,
        rgba: vec![255; 4],
        mips: Vec::new(),
    }];

    assert_eq!(
        material_table_asset_score(
            "stage.szs!/mapobj/normal_nozzle_item.bmd",
            &textures,
            "stage.szs!/mapobj/nozzleItem.bmt",
        ),
        Some((2, "nozzleitem".len()))
    );
}

#[test]
fn npc_starting_animation_uses_family_wait_resource() {
    let monte = SceneObject::new("monte", "NPCMonteMA");
    assert_eq!(
        starting_joint_animation_candidates(&monte, "C:/game/dolpic0.szs!/montema/moma_model.bmd"),
        [
            "C:/game/dolpic0.szs!/montemcommon/mom_wait.bck",
            "C:/game/dolpic0.szs!/montem/mom_wait.bck",
        ]
    );

    let mare = SceneObject::new("mare", "NPCMareMB");
    assert_eq!(
        starting_joint_animation_candidates(&mare, "C:/game/mare0.szs!/marem/marem.bmd"),
        ["C:/game/mare0.szs!/marem/marem_wait.bck"]
    );
}

#[test]
fn monte_starting_eye_pattern_uses_retail_variant_resource() {
    let monte = SceneObject::new("monte", "NPCMonteMA");
    assert_eq!(
        starting_texture_pattern_candidates(&monte, "C:/game/dolpic10.szs!/montema/moma_model.bmd"),
        ["C:/game/dolpic10.szs!/montemcommon/moma_wink.btp"]
    );
}

#[test]
fn npc_eye_material_names_are_treated_as_two_sided_decals() {
    assert!(is_npc_eye_material_name("_eye_mat"));
    assert!(is_npc_eye_material_name("1_eye_mat"));
    assert!(!is_npc_eye_material_name("_hand_mat"));
}

#[test]
fn monte_parts_mask_selects_retail_joint_attachments() {
    let mut monte = SceneObject::new("monte", "NPCMonteMA");
    monte
        .raw_params
        .insert("npc_parts_mask".to_string(), "7".to_string());
    let parts = npc_accessory_specs(&monte);

    assert_eq!(parts.len(), 3);
    assert_eq!(parts[0].joint_name, Some("kubi"));
    assert_eq!(parts[0].asset_suffix, "/montemcommon/hata_model.bmd");
    assert_eq!(parts[2].asset_suffix, "/montemcommon/glassesa_model.bmd");
}

#[test]
fn peach_parts_mask_attaches_default_visible_ponytail() {
    let mut peach = SceneObject::new("peach", "NPCPeach");
    peach
        .raw_params
        .insert("npc_parts_mask".to_string(), "24".to_string());
    let parts = npc_accessory_specs(&peach);

    assert_eq!(parts.len(), 1);
    assert_eq!(parts[0].joint_name, Some("kubi"));
    assert_eq!(parts[0].asset_suffix, "/peach/peach_hair_ponytail.bmd");
}

#[test]
fn peach_hair_parts_use_their_retail_wait_animations() {
    assert_eq!(
        accessory_joint_animation_suffix("/peach/peach_hair_normal.bmd"),
        Some("/peach/peach_hair_normal_wait.bck")
    );
    assert_eq!(
        accessory_joint_animation_suffix("/peach/peach_hair_ponytail.bmd"),
        Some("/peach/peach_hair_ponytail_wait.bck")
    );
    assert_eq!(
        accessory_joint_animation_suffix("/montemcommon/hata_model.bmd"),
        None
    );
}

#[test]
fn toadsworth_parts_mask_attaches_cane_to_retail_finger_joint() {
    let mut kinojii = SceneObject::new("kinojii", "NPCKinojii");
    kinojii
        .raw_params
        .insert("npc_parts_mask".to_string(), "1".to_string());
    let parts = npc_accessory_specs(&kinojii);

    assert_eq!(parts.len(), 1);
    assert_eq!(parts[0].joint_name, Some("jnt_rfinger_1"));
    assert_eq!(parts[0].asset_suffix, "/kinoji_stick.bmd");
}

#[test]
fn npc_parts_tables_cover_every_retail_family_with_parts() {
    let cases = [
        ("NPCMareM", 7),
        ("NPCMareMB", 9),
        ("NPCMareMC", 10),
        ("NPCMareMD", 8),
        ("NPCMareW", 6),
        ("NPCMareWB", 7),
        ("NPCKinopio", 1),
        ("NPCKinojii", 1),
        ("NPCPeach", 4),
        ("NPCRaccoonDog", 1),
    ];
    for (factory, expected_count) in cases {
        let mut object = SceneObject::new(factory, factory);
        object
            .raw_params
            .insert("npc_parts_mask".to_string(), "-1".to_string());
        assert_eq!(
            npc_accessory_specs(&object).len(),
            expected_count,
            "{factory} parts table"
        );
    }
}

#[test]
fn root_attached_noki_parts_do_not_require_a_body_joint() {
    let mut fisherman = SceneObject::new("fisherman", "NPCMareMB");
    fisherman
        .raw_params
        .insert("npc_parts_mask".to_string(), (1 << 9).to_string());
    let parts = npc_accessory_specs(&fisherman);
    assert_eq!(parts.len(), 1);
    assert_eq!(parts[0].joint_name, None);
    assert_eq!(parts[0].asset_suffix, "/marembturizao.bmd");
}

#[test]
fn npc_circle_shadow_uses_retail_default_radius() {
    let mut triangles = Vec::new();
    push_npc_circle_shadow(
        &mut triangles,
        Transform {
            translation: [10.0, 20.0, 30.0],
            ..Transform::default()
        },
        4,
        8,
    );

    assert_eq!(triangles.len(), 20);
    assert_eq!(triangles[0].render_layer, PreviewRenderLayer::Shadow);
    assert_eq!(triangles[0].vertices[0], [10.0, 21.5, 30.0]);
    assert!((triangles[0].vertices[1][0] - 70.0).abs() < 0.001);
    assert_eq!(triangles[0].blend_mode.unwrap().mode, 1);
}

#[test]
fn monte_model_loader_flags_follow_manager_entries() {
    assert_eq!(
        npc_model_loader_flags(&SceneObject::new("ma", "NPCMonteMA")),
        Some(0x1030_0000)
    );
    assert_eq!(
        npc_model_loader_flags(&SceneObject::new("md", "NPCMonteMD")),
        Some(0x1021_0000)
    );
}

#[test]
fn npc_archive_models_are_supported_object_previews() {
    assert!(is_supported_object_preview_model_path(
        "stage.szs!/montema/moma_model.bmd"
    ));
    assert!(is_supported_object_preview_model_path(
        "/scene/kinopio/kinopio_body.bmd"
    ));
    assert!(is_supported_object_preview_model_path(
        "stage.szs!/sambohead/sambohead.bmd"
    ));
}

#[test]
fn world_model_path_normalization_deduplicates_scene_instances() {
    let world_models = BTreeSet::from([normalized_preview_asset_path(
        r"C:\game\dolpic0.szs!/map/map/sky.bmd",
    )]);

    assert!(!should_instance_object_preview_model(
        "C:/GAME/dolpic0.szs!/map/map/sky.bmd",
        &world_models
    ));
    assert!(should_instance_object_preview_model(
        "C:/game/dolpic0.szs!/montema/moma_model.bmd",
        &world_models
    ));
    assert!(should_instance_object_preview_model(
        "C:/game/dolpic0.szs!/sambohead/sambohead.bmd",
        &world_models
    ));
}

#[test]
fn palm_leaf_placement_is_kept_as_an_object_preview() {
    let mut palm_leaf = SceneObject::new("PalmLeaf 2", "Palm");
    palm_leaf
        .raw_params
        .insert("name".to_string(), "PalmLeaf 2".to_string());
    palm_leaf.asset_hints.push(AssetRef {
        path: "stage.szs!/mapobj/palmleaf.bmd".to_string(),
        role: AssetRole::PreviewModel,
    });

    assert_eq!(
        object_preview_model_path(&palm_leaf, &BTreeSet::new()).as_deref(),
        Some("stage.szs!/mapobj/palmleaf.bmd")
    );
}

fn preview_for_texture_alpha(has_alpha: bool, has_translucent_alpha: bool) -> ModelPreview {
    let image = egui::ColorImage::filled([1, 1], egui::Color32::WHITE);
    ModelPreview {
        points: Vec::new(),
        triangles: Vec::new(),
        textures: vec![PreviewTexture {
            image: image.clone(),
            mips: vec![image],
            format: 6,
            wrap_s: 1,
            wrap_t: 1,
            min_filter: 1,
            mag_filter: 1,
            mipmap_count: 1,
            has_alpha,
            has_translucent_alpha,
        }],
        materials: Vec::new(),
        texture_srt_animations: Vec::new(),
        texture_pattern_animations: Vec::new(),
        material_animation_bindings: Vec::new(),
        bounds_min: [0.0, 0.0, 0.0],
        bounds_max: [1.0, 1.0, 1.0],
        camera_bounds_min: [0.0, 0.0, 0.0],
        camera_bounds_max: [1.0, 1.0, 1.0],
        sky_radius: 0.0,
        loaded_models: 1,
        failed_models: 0,
        source_vertices: 0,
        source_triangles: 0,
        source_textures: 1,
        object_model_indices: BTreeMap::new(),
        animated_models: Vec::new(),
    }
}

fn preview_for_alpha_texture(has_translucent_alpha: bool) -> ModelPreview {
    preview_for_texture_alpha(true, has_translucent_alpha)
}

fn textured_blended_triangle() -> PreviewTriangle {
    PreviewTriangle {
        vertices: [[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]],
        normals: None,
        color_channels: [None; 2],
        tex_coord_sets: [None; 8],
        material_index: None,
        packet_index: 0,
        model_index: 1,
        render_layer: PreviewRenderLayer::Main,
        color: None,
        vertex_colors: None,
        combine_mode: J3dPreviewCombineMode::TextureOnly,
        tex_coords: Some([[0.0, 0.0], [1.0, 0.0], [0.0, 1.0]]),
        texture_index: Some(0),
        mask_tex_coords: None,
        mask_texture_index: None,
        cull_mode: None,
        alpha_compare: None,
        blend_mode: Some(J3dBlendMode {
            mode: 1,
            src_factor: 4,
            dst_factor: 5,
            logic_op: 0,
        }),
        z_mode: None,
    }
}

#[test]
fn rmb_free_look_keeps_camera_position_fixed() {
    let mut app = camera_app();
    let old_position = app.camera_frame().position;

    app.rotate_camera_in_place(egui::vec2(80.0, -30.0));

    assert_vec3_close(app.camera_frame().position, old_position);
}

#[test]
fn rmb_horizontal_drag_uses_unreal_style_yaw_sign() {
    let mut app = camera_app();

    app.rotate_camera_in_place(egui::vec2(80.0, 0.0));

    assert!(app.renderer.camera().yaw_degrees < 0.0);
}

#[test]
fn alt_orbit_uses_same_horizontal_yaw_sign() {
    let mut app = camera_app();

    app.orbit_camera(egui::vec2(80.0, 0.0));

    assert!(app.renderer.camera().yaw_degrees < 0.0);
}

#[test]
fn move_drag_uses_camera_relative_ground_plane() {
    let app = camera_app();
    let rect = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(800.0, 600.0));

    let right_drag = app.viewport_drag_move_delta(rect, egui::vec2(10.0, 0.0));
    let up_drag = app.viewport_drag_move_delta(rect, egui::vec2(0.0, -10.0));

    assert!(right_drag[0] < 0.0);
    assert!(right_drag[2].abs() < 0.001);
    assert!(up_drag[2] > 0.0);
    assert!(up_drag[0].abs() < 0.001);
}

#[test]
fn move_drag_rotates_with_camera_yaw() {
    let mut app = camera_app();
    app.renderer.camera_mut().yaw_degrees = 90.0;
    let rect = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(800.0, 600.0));

    let right_drag = app.viewport_drag_move_delta(rect, egui::vec2(10.0, 0.0));
    let up_drag = app.viewport_drag_move_delta(rect, egui::vec2(0.0, -10.0));

    assert!(right_drag[2] > 0.0);
    assert!(right_drag[0].abs() < 0.001);
    assert!(up_drag[0] > 0.0);
    assert!(up_drag[2].abs() < 0.001);
}

#[test]
fn reflection_helper_meshes_are_effect_meshes() {
    let path = "stage.szs!/map/map/reflectsky.bmd";

    assert!(!is_default_preview_model_path(path, true, true, false));
    assert!(is_default_preview_model_path(path, true, true, true));
    assert!(!is_camera_bounds_model_path(path));
}

#[test]
fn skybox_model_is_loaded_as_camera_relative_environment() {
    let path = "stage.szs!/map/map/sky.bmd";

    assert!(is_default_preview_model_path(path, false, false, false));
    assert!(!is_camera_bounds_model_path(path));
    assert_eq!(
        preview_render_layer_for_model_path(path),
        PreviewRenderLayer::Sky
    );
    assert_eq!(
        model_loader_flags_for_path(path),
        SMS_DEFAULT_OBJECT_MODEL_LOAD_FLAGS
    );
    assert!(!path_is_sky_model_path("stage.szs!/map/map/reflectsky.bmd"));
}

#[test]
fn shimmer_models_use_the_heatwave_layer_and_indirect_loader_flags() {
    for path in [
        "stage.szs!/mapobj/shimmerlow.bmd",
        "stage.szs!/mapobj/shimmerlowfar.bmd",
        "stage.szs!/mapobj/shimmerhi.bmd",
        "stage.szs!/mapobj/shimmerhifar.bmd",
    ] {
        assert!(path_is_shimmer_model_path(path));
        assert_eq!(
            preview_render_layer_for_model_path(path),
            PreviewRenderLayer::Heatwave
        );
        assert_eq!(model_loader_flags_for_path(path), 0x1101_0000);
    }

    assert!(!path_is_shimmer_model_path(
        "stage.szs!/mapobj/shimmerunrelated.bmd"
    ));
}

#[test]
fn shimmer_draw_transform_keeps_scale_but_cancels_placement_pose() {
    let transform = Transform {
        translation: [10.0, 20.0, 30.0],
        rotation_degrees: [40.0, 50.0, 60.0],
        scale: [1.12, 1.12, 1.0],
    };

    assert_eq!(
        shimmer_preview_transform(transform),
        Transform {
            translation: [0.0; 3],
            rotation_degrees: [0.0; 3],
            scale: transform.scale,
        }
    );
}

#[test]
fn reset_fruit_draw_transform_matches_runtime_body_radius_offsets() {
    let transform = Transform {
        translation: [10.0, 300.0, 30.0],
        rotation_degrees: [0.0; 3],
        scale: [1.0; 3],
    };

    for (resource_name, expected_y) in [
        ("FruitCoconut", 340.0),
        ("FruitPapaya", 340.0),
        ("FruitDurian", 345.0),
        ("FruitPine", 350.0),
        ("RedPepper", 300.0),
        ("FruitBanana", 300.0),
    ] {
        let mut object = SceneObject::new(resource_name, "ResetFruit");
        object
            .raw_params
            .insert("stream_string_0".to_string(), resource_name.to_string());

        assert_eq!(
            reset_fruit_preview_transform(&object, transform).translation[1],
            expected_y
        );
    }
}

#[test]
fn reset_fruit_draw_transform_scales_the_runtime_body_radius() {
    let mut object = SceneObject::new("pine", "ResetFruit");
    object
        .raw_params
        .insert("stream_string_0".to_string(), "FruitPine".to_string());
    let transform = Transform {
        translation: [0.0, 100.0, 0.0],
        rotation_degrees: [0.0; 3],
        scale: [2.0; 3],
    };

    assert_eq!(
        reset_fruit_preview_transform(&object, transform).translation[1],
        210.0
    );
}

#[test]
fn skybox_vertices_track_camera_translation() {
    let vertices = [[1.0, 2.0, 3.0], [-4.0, 5.0, 6.0], [7.0, 8.0, -9.0]];
    let camera = [100.0, 200.0, 300.0];

    assert_eq!(
        preview_triangle_world_vertices(vertices, PreviewRenderLayer::Sky, camera),
        [
            [101.0, 202.0, 303.0],
            [96.0, 205.0, 306.0],
            [107.0, 208.0, 291.0],
        ]
    );
    assert_eq!(
        preview_triangle_world_vertices(vertices, PreviewRenderLayer::Main, camera),
        vertices
    );
}

#[test]
fn skybox_radius_expands_the_far_clip() {
    let mut preview = preview_for_texture_alpha(false, false);
    preview.sky_radius = 150_000.0;

    let far = preview.far_clip(1_000.0);
    assert!((157_000.0..158_000.0).contains(&far));
}

#[test]
fn viewport_background_is_one_continuous_gradient() {
    let rect = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(800.0, 600.0));
    let mesh = viewport_background_mesh(rect);

    assert_eq!(mesh.vertices.len(), 4);
    assert_eq!(mesh.indices, [0, 1, 2, 0, 2, 3]);
    assert_eq!(mesh.vertices[0].color, mesh.vertices[1].color);
    assert_eq!(mesh.vertices[2].color, mesh.vertices[3].color);
    assert_ne!(mesh.vertices[0].color, mesh.vertices[2].color);
}

#[test]
fn viewport_lines_clip_at_the_near_plane_instead_of_the_crosshair() {
    let camera = CameraFrame {
        position: [0.0, 0.0, 0.0],
        right: [1.0, 0.0, 0.0],
        up: [0.0, 1.0, 0.0],
        forward: [0.0, 0.0, 1.0],
    };

    let clipped =
        clip_world_segment_to_near_plane(camera, [0.0, 0.0, -10.0], [10.0, 0.0, 10.0], 1.0)
            .unwrap();

    assert_vec3_close(clipped[0], [5.5, 0.0, 1.0]);
    assert_vec3_close(clipped[1], [10.0, 0.0, 10.0]);
}

#[test]
fn viewport_lines_fully_behind_the_camera_are_hidden() {
    let camera = CameraFrame {
        position: [0.0, 0.0, 0.0],
        right: [1.0, 0.0, 0.0],
        up: [0.0, 1.0, 0.0],
        forward: [0.0, 0.0, 1.0],
    };

    assert!(
        clip_world_segment_to_near_plane(camera, [0.0, 0.0, -10.0], [10.0, 0.0, -1.0], 1.0,)
            .is_none()
    );
}

#[test]
fn pollution_meshes_are_goop_not_generic_effects() {
    let path = "stage.szs!/map/pollution/pollution00.bmd";

    assert!(is_default_preview_model_path(path, true, true, false));
    assert!(!is_default_preview_model_path(path, true, false, true));
    assert_eq!(
        preview_render_layer_for_model_path(path),
        PreviewRenderLayer::Goop
    );
    assert!(!is_camera_bounds_model_path(path));
}

#[test]
fn archived_pollution_models_require_an_active_ymap_layer() {
    let first = "stage.szs!/map/pollution/pollution00.bmd";
    let second = "stage.szs!/map/pollution/pollution01.bdl";

    assert!(!pollution_layer_model_is_active(first, 0));
    assert!(pollution_layer_model_is_active(first, 1));
    assert!(!pollution_layer_model_is_active(second, 1));
    assert!(pollution_layer_model_is_active(second, 2));
}

#[test]
fn ymap_layer_count_is_read_as_big_endian() {
    assert_eq!(
        pollution_layer_count_from_bytes(&[0, 0, 0, 0, 0, 0, 0, 8]),
        Some(0)
    );
    assert_eq!(pollution_layer_count_from_bytes(&[0, 0, 0, 3]), Some(3));
    assert_eq!(pollution_layer_count_from_bytes(&[0, 0, 0]), None);
}

#[test]
fn mare_lettered_pollution_layers_follow_the_runtime_name_table() {
    assert_eq!(
        pollution_layer_model_index("stage.szs!/map/pollution/pollutionA.bmd"),
        Some(7)
    );
    assert_eq!(
        pollution_layer_model_index("stage.szs!/map/pollution/pollutionB.bmd"),
        Some(8)
    );
}

#[test]
fn named_static_pollution_models_are_not_ymap_layers() {
    let path = "stage.szs!/map/map/mareSeaPollutionS0.bmd";

    assert_eq!(pollution_layer_model_index(path), None);
    assert!(pollution_layer_model_is_active(path, 0));
}

#[test]
fn pollution_bitmap_replaces_the_embedded_authoring_mask_top_down() {
    let mut bmp = vec![0u8; 70];
    bmp[0..2].copy_from_slice(b"BM");
    bmp[10..14].copy_from_slice(&54u32.to_le_bytes());
    bmp[14..18].copy_from_slice(&40u32.to_le_bytes());
    bmp[18..22].copy_from_slice(&2i32.to_le_bytes());
    bmp[22..26].copy_from_slice(&2i32.to_le_bytes());
    bmp[26..28].copy_from_slice(&1u16.to_le_bytes());
    bmp[28..30].copy_from_slice(&8u16.to_le_bytes());
    // BMP rows are bottom-up and padded to four bytes.
    bmp[54..62].copy_from_slice(&[30, 40, 0, 0, 10, 20, 0, 0]);

    let (width, height, rgba) = decode_pollution_bitmap_mask(&bmp).unwrap();

    assert_eq!((width, height), (2, 2));
    assert_eq!(
        rgba,
        vec![10, 10, 10, 10, 20, 20, 20, 20, 30, 30, 30, 30, 40, 40, 40, 40]
    );
}

#[test]
fn pollution_bitmap_rejects_non_i8_or_truncated_inputs() {
    assert_eq!(decode_pollution_bitmap_mask(b"not a bitmap"), None);

    let mut bmp = vec![0u8; 54];
    bmp[0..2].copy_from_slice(b"BM");
    bmp[10..14].copy_from_slice(&54u32.to_le_bytes());
    bmp[18..22].copy_from_slice(&1i32.to_le_bytes());
    bmp[22..26].copy_from_slice(&1i32.to_le_bytes());
    bmp[26..28].copy_from_slice(&1u16.to_le_bytes());
    bmp[28..30].copy_from_slice(&24u16.to_le_bytes());
    assert_eq!(decode_pollution_bitmap_mask(&bmp), None);
}

#[test]
fn every_model_layer_uses_its_same_basename_btk() {
    for (model, animation) in [
        (
            "stage.szs!/map/pollution/pollution00.bmd",
            "stage.szs!/map/pollution/pollution00.btk",
        ),
        ("stage.szs!/map/map/sea.bmd", "stage.szs!/map/map/sea.btk"),
        ("stage.szs!/map/map/sky.bmd", "stage.szs!/map/map/sky.btk"),
        (
            "stage.szs!/mapobj/animated.bdl",
            "stage.szs!/mapobj/animated.btk",
        ),
    ] {
        assert_eq!(
            model_texture_srt_animation_path(model).as_deref(),
            Some(animation)
        );
    }
}

#[test]
fn named_pollution_map_meshes_are_goop() {
    let path = "stage.szs!/map/map/mareseapollutions0.bmd";

    assert!(is_default_preview_model_path(path, true, true, false));
    assert!(!is_default_preview_model_path(path, true, false, true));
    assert_eq!(
        preview_render_layer_for_model_path(path),
        PreviewRenderLayer::Goop
    );
    assert!(!is_camera_bounds_model_path(path));
}

#[test]
fn sea_meshes_are_level_water_layer() {
    let path = "stage.szs!/map/map/sea.bmd";

    assert!(is_default_preview_model_path(path, true, true, false));
    assert!(!is_default_preview_model_path(path, false, true, true));
    assert_eq!(
        preview_render_layer_for_model_path(path),
        PreviewRenderLayer::Water
    );
    assert!(!is_camera_bounds_model_path(path));
}

#[test]
fn source_named_river_models_are_level_water_layers() {
    for path in [
        "stage.szs!/map/map/BiancoRiver.bmd",
        "stage.szs!/map/map/MonteRiver.bmd",
    ] {
        assert_eq!(
            preview_render_layer_for_model_path(path),
            PreviewRenderLayer::Water
        );
    }
}

#[test]
fn map_puddles_are_level_water_layer() {
    let path = "stage.szs!/map/mirror/puddle00.bmd";

    assert!(is_default_preview_model_path(path, true, true, false));
    assert!(!is_default_preview_model_path(path, false, true, true));
    assert_eq!(
        preview_render_layer_for_model_path(path),
        PreviewRenderLayer::Water
    );
    assert!(!is_camera_bounds_model_path(path));
}

#[test]
fn indirect_water_helpers_stay_hidden_by_default() {
    let sea_path = "stage.szs!/map/map/seaindirect.bmd";
    let puddle_path = "stage.szs!/map/mirror/puddle_ind00.bmd";

    for path in [sea_path, puddle_path] {
        assert!(!is_default_preview_model_path(path, true, true, true));
        assert_ne!(
            preview_render_layer_for_model_path(path),
            PreviewRenderLayer::Water
        );
        assert!(!is_camera_bounds_model_path(path));
    }
}

#[test]
fn water_layer_renders_translucent_without_texture_alpha() {
    let preview = preview_for_alpha_texture(false);
    let mut triangle = textured_blended_triangle();
    triangle.render_layer = PreviewRenderLayer::Water;
    triangle.texture_index = None;
    triangle.tex_coords = None;
    triangle.blend_mode = None;

    assert!(!preview_triangle_uses_alpha_test(&preview, &triangle));
    assert!(preview_triangle_is_translucent(&preview, &triangle));
}

#[test]
fn unmasked_goop_layer_renders_as_translucent_overlay() {
    let preview = preview_for_texture_alpha(false, false);
    let mut triangle = textured_blended_triangle();
    triangle.render_layer = PreviewRenderLayer::Goop;
    triangle.blend_mode = None;

    assert!(!preview_triangle_uses_alpha_test(&preview, &triangle));
    assert!(preview_triangle_is_translucent(&preview, &triangle));
}

#[test]
fn masked_goop_layer_uses_alpha_test() {
    let preview = preview_for_texture_alpha(false, false);
    let mut triangle = textured_blended_triangle();
    triangle.render_layer = PreviewRenderLayer::Goop;
    triangle.blend_mode = None;
    triangle.mask_texture_index = Some(0);
    triangle.mask_tex_coords = triangle.tex_coords;

    assert!(preview_triangle_uses_alpha_test(&preview, &triangle));
    assert!(!preview_triangle_is_translucent(&preview, &triangle));
}

#[test]
fn blended_cutout_texture_uses_alpha_test_not_translucency() {
    let preview = preview_for_alpha_texture(false);
    let triangle = textured_blended_triangle();

    assert!(preview_triangle_uses_alpha_test(&preview, &triangle));
    assert!(!preview_triangle_is_translucent(&preview, &triangle));
}

#[test]
fn blended_fractional_alpha_texture_stays_translucent() {
    let preview = preview_for_alpha_texture(true);
    let triangle = textured_blended_triangle();

    assert!(!preview_triangle_uses_alpha_test(&preview, &triangle));
    assert!(preview_triangle_is_translucent(&preview, &triangle));
}

#[test]
fn masked_texture_triangle_uses_alpha_test() {
    let preview = preview_for_texture_alpha(false, false);
    let mut triangle = textured_blended_triangle();
    triangle.blend_mode = None;
    triangle.mask_texture_index = Some(0);
    triangle.mask_tex_coords = triangle.tex_coords;

    assert!(preview_triangle_uses_alpha_test(&preview, &triangle));
    assert!(!preview_triangle_is_translucent(&preview, &triangle));
}

#[test]
fn parses_comma_separated_camera_focus_arg() {
    assert_eq!(
        parse_vec3_arg("-995.2,8353,6493"),
        Some([-995.2, 8353.0, 6493.0])
    );
    assert_eq!(parse_vec3_arg("1,2"), None);
}

#[test]
fn textured_material_tint_does_not_inherit_material_alpha() {
    let tints = preview_texture_tints(
        Some([128, 128, 128, 50]),
        None,
        J3dPreviewCombineMode::TextureModulateMaterial,
        PreviewRenderLayer::Main,
    );

    assert_eq!(
        tints[0],
        egui::Color32::from_rgba_unmultiplied(128, 128, 128, 255)
    );
}

#[test]
fn color32_to_rgba_unpremultiplies_transparent_editor_tints() {
    let rgba = color32_to_rgba(egui::Color32::from_rgba_unmultiplied(144, 217, 255, 50));

    assert!((rgba[0] - 144.0 / 255.0).abs() < 0.01);
    assert!((rgba[1] - 217.0 / 255.0).abs() < 0.01);
    assert!((rgba[2] - 1.0).abs() < 0.01);
    assert!((rgba[3] - 50.0 / 255.0).abs() < 0.01);
}

#[test]
fn software_opaque_pass_outputs_solid_pixels_after_alpha_keep() {
    let mut image = egui::ColorImage::filled([1, 1], egui::Color32::from_rgb(10, 20, 30));
    let mut depth = vec![f32::INFINITY];
    let src = software_output_color_for_pass([0.8, 0.4, 0.2, 0.25], true);

    blend_depth_pixel(&mut image, &mut depth, 0, 42.0, src, true);

    let rgba = color32_to_rgba(image.pixels[0]);
    assert!((rgba[0] - 0.8).abs() < 0.01);
    assert!((rgba[1] - 0.4).abs() < 0.01);
    assert!((rgba[2] - 0.2).abs() < 0.01);
    assert!((rgba[3] - 1.0).abs() < 0.01);
    assert_eq!(depth[0], 42.0);
}

#[test]
fn software_translucent_pass_keeps_fractional_alpha_blending() {
    let mut image = egui::ColorImage::filled([1, 1], egui::Color32::from_rgb(10, 20, 30));
    let mut depth = vec![f32::INFINITY];
    let src = software_output_color_for_pass([1.0, 0.0, 0.0, 0.25], false);

    blend_depth_pixel(&mut image, &mut depth, 0, 42.0, src, false);

    let rgba = color32_to_rgba(image.pixels[0]);
    assert!(rgba[0] > 0.25);
    assert!(rgba[1] < 20.0 / 255.0);
    assert!(rgba[2] < 30.0 / 255.0);
    assert_eq!(depth[0], f32::INFINITY);
}

#[test]
fn textured_material_alpha_does_not_make_opaque_texture_translucent() {
    let preview = preview_for_texture_alpha(false, false);
    let mut triangle = textured_blended_triangle();
    triangle.color = Some([128, 128, 128, 50]);
    triangle.combine_mode = J3dPreviewCombineMode::TextureModulateMaterial;

    assert!(!preview_triangle_uses_alpha_test(&preview, &triangle));
    assert!(!preview_triangle_is_translucent(&preview, &triangle));
}

#[test]
fn retransform_preview_point_preserves_object_local_space() {
    let old_transform = Transform {
        translation: [100.0, 20.0, -40.0],
        rotation_degrees: [0.0, 90.0, 0.0],
        scale: [2.0, 1.0, 1.0],
    };
    let new_transform = Transform {
        translation: [-30.0, 10.0, 80.0],
        rotation_degrees: [0.0, -45.0, 0.0],
        scale: [1.0, 2.0, 1.0],
    };
    let local = [8.0, 4.0, -12.0];
    let old_world = transform_preview_point(local, old_transform);
    let new_world = transform_preview_point(local, new_transform);

    assert_vec3_close(
        retransform_preview_point(old_world, old_transform, new_transform),
        new_world,
    );
}

#[test]
fn transform_preview_normal_ignores_translation_and_normalizes() {
    let transform = Transform {
        translation: [500.0, 0.0, -1000.0],
        rotation_degrees: [0.0, 90.0, 0.0],
        scale: [2.0, 1.0, 1.0],
    };

    assert_vec3_close(
        transform_preview_normal([1.0, 0.0, 0.0], transform),
        [0.0, 0.0, -1.0],
    );
}

#[test]
fn updating_object_transform_moves_cached_preview_mesh() {
    let old_transform = Transform::default();
    let new_transform = Transform {
        translation: [50.0, 0.0, -25.0],
        ..Transform::default()
    };
    let mut object_model_indices = BTreeMap::new();
    object_model_indices.insert("obj-1".to_string(), 7);
    let mut app = SmsEditorApp {
        model_preview: Some(ModelPreview {
            points: vec![PreviewPoint {
                position: [1.0, 2.0, 3.0],
                model_index: 7,
            }],
            triangles: vec![PreviewTriangle {
                vertices: [[1.0, 0.0, 0.0], [0.0, 2.0, 0.0], [0.0, 0.0, 3.0]],
                normals: Some([[0.0, 1.0, 0.0]; 3]),
                color_channels: [None; 2],
                tex_coord_sets: [None; 8],
                material_index: None,
                packet_index: 0,
                model_index: 7,
                render_layer: PreviewRenderLayer::Main,
                color: None,
                vertex_colors: None,
                combine_mode: J3dPreviewCombineMode::VertexOnly,
                tex_coords: None,
                texture_index: None,
                mask_tex_coords: None,
                mask_texture_index: None,
                cull_mode: None,
                alpha_compare: None,
                blend_mode: None,
                z_mode: None,
            }],
            textures: Vec::new(),
            materials: Vec::new(),
            texture_srt_animations: Vec::new(),
            texture_pattern_animations: Vec::new(),
            material_animation_bindings: Vec::new(),
            bounds_min: [0.0, 0.0, 0.0],
            bounds_max: [1.0, 2.0, 3.0],
            camera_bounds_min: [0.0, 0.0, 0.0],
            camera_bounds_max: [1.0, 2.0, 3.0],
            sky_radius: 0.0,
            loaded_models: 1,
            failed_models: 0,
            source_vertices: 3,
            source_triangles: 1,
            source_textures: 0,
            object_model_indices,
            animated_models: Vec::new(),
        }),
        ..SmsEditorApp::default()
    };

    assert!(app.update_object_preview_transform("obj-1", old_transform, new_transform));
    let preview = app.model_preview.as_ref().unwrap();
    assert_vec3_close(preview.points[0].position, [51.0, 2.0, -22.0]);
    assert_vec3_close(preview.triangles[0].vertices[0], [51.0, 0.0, -25.0]);
    assert_vec3_close(preview.triangles[0].vertices[1], [50.0, 2.0, -25.0]);
    assert_vec3_close(preview.triangles[0].vertices[2], [50.0, 0.0, -22.0]);
}

#[test]
fn dirty_state_tracks_saved_object_content() {
    let object = SceneObject::new("obj-1", "coin");
    let mut app = SmsEditorApp {
        document: Some(test_document(vec![object.clone()])),
        saved_objects: vec![object],
        ..SmsEditorApp::default()
    };
    assert!(!app.is_dirty());

    app.document.as_mut().unwrap().objects[0]
        .transform
        .translation[0] = 25.0;
    assert!(app.is_dirty());
}

#[test]
fn transform_transaction_creates_one_undo_entry() {
    let object = SceneObject::new("obj-1", "coin");
    let mut app = SmsEditorApp {
        document: Some(test_document(vec![object.clone()])),
        saved_objects: vec![object],
        selected_object_id: Some("obj-1".to_string()),
        ..SmsEditorApp::default()
    };

    app.begin_undo_transaction();
    let mut transform = app.selected_object().unwrap().transform;
    transform.translation[0] = 10.0;
    app.update_selected_transform(transform);
    transform.translation[0] = 20.0;
    app.update_selected_transform(transform);
    app.commit_undo_transaction("Moved object");

    assert_eq!(app.undo_stack.len(), 1);
    app.undo();
    assert_eq!(app.selected_object().unwrap().transform.translation[0], 0.0);
}

fn test_document(objects: Vec<SceneObject>) -> StageDocument {
    StageDocument {
        stage_id: "dolpic0".to_string(),
        base_root: PathBuf::from("."),
        assets: Vec::new(),
        objects,
        changed_files: BTreeMap::new(),
        registry: None,
        load_issues: Vec::new(),
    }
}
