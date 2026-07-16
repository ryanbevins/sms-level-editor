use super::*;

fn assert_vec3_close(actual: [f32; 3], expected: [f32; 3]) {
    for (actual, expected) in actual.into_iter().zip(expected) {
        assert!(
            (actual - expected).abs() < 0.001,
            "expected {expected}, got {actual}"
        );
    }
}

#[test]
fn coin_variants_use_the_item_managers_retail_rotation_speed() {
    for (factory, class) in [
        ("coin", "TCoin"),
        ("CoinRed", ""),
        ("CoinBlue", ""),
        ("coin_red", "TCoinRed"),
        ("coin_blue", "TCoinBlue"),
        ("FlowerCoin", "TFlowerCoin"),
        ("joint_coin", "TCoin"),
    ] {
        let mut object = SceneObject::new("coin-instance", factory);
        object.class_name = Some(class.to_string());
        assert_eq!(runtime_yaw_degrees_per_frame(&object), 2.0, "{factory}");
    }

    assert_eq!(
        runtime_yaw_degrees_per_frame(&SceneObject::new("tree-instance", "PalmTree")),
        0.0
    );
}

#[test]
fn runtime_rotation_uses_sunshines_clock_and_wraps_yaw() {
    let transform = Transform {
        rotation_degrees: [10.0, 350.0, 20.0],
        ..Transform::default()
    };
    let animated = runtime_rotated_transform(transform, 1.0, 2.0);

    assert_vec3_close(animated.rotation_degrees, [10.0, 110.0, 20.0]);
}

#[test]
fn full_billboard_local_positive_z_moves_toward_the_camera() {
    let billboard = J3dBillboard {
        mode: sms_formats::J3dBillboardMode::Full,
        center: [0.0, 0.0, 100.0],
        axes: [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]],
        offsets: [[0.0, 0.0, 10.0]; 3],
        normals: None,
    };
    let vertices = j3d_billboard_world_vertices(
        billboard,
        CameraFrame {
            position: [0.0; 3],
            right: [1.0, 0.0, 0.0],
            up: [0.0, 1.0, 0.0],
            forward: [0.0, 0.0, 1.0],
        },
    );
    assert_eq!(vertices[0], [0.0, 0.0, 90.0]);
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
fn automatic_scene_refresh_is_queued_once_per_base_root() {
    let mut app = SmsEditorApp {
        base_root: ".".to_string(),
        ..SmsEditorApp::default()
    };
    let (_sender, receiver) = mpsc::channel();
    app.background_receiver = Some(receiver);

    app.refresh_scene_browser_if_needed();
    assert_eq!(app.pending_auto_refresh_root.as_deref(), Some("."));
    assert!(app.last_auto_refresh_attempt_root.is_empty());

    app.pending_auto_refresh_root = None;
    app.last_auto_refresh_attempt_root = ".".to_string();
    app.refresh_scene_browser_if_needed();
    assert!(app.pending_auto_refresh_root.is_none());
}

#[test]
fn fly_camera_velocity_interpolates_in_and_out() {
    let accelerated =
        viewport_ui::interpolate_camera_velocity([0.0; 3], [1000.0, 0.0, 0.0], 1.0 / 60.0, 8.0);
    assert!(accelerated[0] > 0.0);
    assert!(accelerated[0] < 1000.0);

    let accelerated_again =
        viewport_ui::interpolate_camera_velocity(accelerated, [1000.0, 0.0, 0.0], 1.0 / 60.0, 8.0);
    assert!(accelerated_again[0] > accelerated[0]);
    assert!(accelerated_again[0] < 1000.0);

    let decelerated =
        viewport_ui::interpolate_camera_velocity(accelerated_again, [0.0; 3], 1.0 / 60.0, 12.0);
    assert!(decelerated[0] > 0.0);
    assert!(decelerated[0] < accelerated_again[0]);
}

#[test]
fn fly_camera_scroll_adjusts_and_clamps_speed() {
    assert!(viewport_ui::camera_speed_after_scroll(1.0, 120.0) > 1.0);
    assert!(viewport_ui::camera_speed_after_scroll(1.0, -120.0) < 1.0);
    assert_eq!(viewport_ui::camera_speed_after_scroll(8.0, 10_000.0), 8.0);
    assert_eq!(
        viewport_ui::camera_speed_after_scroll(0.01, -10_000.0),
        0.01
    );
}

#[test]
fn viewport_markers_show_only_selection_outside_objects_mode() {
    let app_objects = vec![
        SceneObject::new("obj-a", "Coin"),
        SceneObject::new("obj-b", "Shine"),
    ];
    let mut app = SmsEditorApp {
        document: Some(test_document(app_objects)),
        selected_object_id: Some("obj-b".to_string()),
        view_mode: ViewMode::Lit,
        ..SmsEditorApp::default()
    };

    let marker_ids = |app: &SmsEditorApp| {
        app.viewport_marker_objects()
            .map(|object| object.id.clone())
            .collect::<Vec<_>>()
    };
    assert_eq!(marker_ids(&app), ["obj-b".to_string()]);

    app.view_mode = ViewMode::Collision;
    assert_eq!(marker_ids(&app), ["obj-b".to_string()]);

    app.view_mode = ViewMode::Objects;
    assert_eq!(marker_ids(&app), ["obj-a".to_string(), "obj-b".to_string()]);

    app.view_mode = ViewMode::Lit;
    app.selected_object_id = None;
    assert!(marker_ids(&app).is_empty());
}

#[test]
fn viewport_mesh_picking_selects_the_object_away_from_its_origin_marker() {
    let mut object = SceneObject::new("obj-mesh", "Coin");
    object.transform.translation = [600.0, 0.0, 1000.0];
    let mut preview = preview_for_texture_alpha(false, false);
    preview.object_model_indices.insert(object.id.clone(), 7);
    let mut triangle = textured_blended_triangle();
    triangle.vertices = [
        [-200.0, -200.0, 1000.0],
        [200.0, -200.0, 1000.0],
        [0.0, 200.0, 1000.0],
    ];
    triangle.model_index = 7;
    triangle.texture_index = None;
    triangle.tex_coords = None;
    preview.triangles.push(triangle);

    let app = SmsEditorApp {
        document: Some(test_document(vec![object])),
        model_preview: Some(preview),
        ..camera_app()
    };
    let rect = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(200.0, 200.0));

    assert_eq!(
        app.object_mesh_at_screen_position(rect, rect.center())
            .as_deref(),
        Some("obj-mesh")
    );
}

#[test]
fn viewport_mesh_picking_prefers_the_nearest_overlapping_object() {
    let mut preview = preview_for_texture_alpha(false, false);
    preview
        .object_model_indices
        .insert("far-object".to_string(), 1);
    preview
        .object_model_indices
        .insert("near-object".to_string(), 2);
    for (model_index, depth, extent) in [(1, 1000.0, 200.0), (2, 500.0, 100.0)] {
        let mut triangle = textured_blended_triangle();
        triangle.vertices = [
            [-extent, -extent, depth],
            [extent, -extent, depth],
            [0.0, extent, depth],
        ];
        triangle.model_index = model_index;
        triangle.texture_index = None;
        triangle.tex_coords = None;
        preview.triangles.push(triangle);
    }

    let app = SmsEditorApp {
        model_preview: Some(preview),
        ..camera_app()
    };
    let rect = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(200.0, 200.0));

    assert_eq!(
        app.object_mesh_at_screen_position(rect, rect.center())
            .as_deref(),
        Some("near-object")
    );
}

#[test]
fn viewport_picking_does_not_let_a_hidden_origin_behind_the_mesh_win() {
    let mut front = SceneObject::new("front-object", "Coin");
    front.transform.translation = [600.0, 0.0, 500.0];
    let mut behind = SceneObject::new("behind-object", "Coin");
    behind.transform.translation = [0.0, 0.0, 1000.0];
    let mut preview = preview_for_texture_alpha(false, false);
    preview.object_model_indices.insert(front.id.clone(), 1);
    let mut triangle = textured_blended_triangle();
    triangle.vertices = [
        [-100.0, -100.0, 500.0],
        [100.0, -100.0, 500.0],
        [0.0, 100.0, 500.0],
    ];
    triangle.model_index = 1;
    triangle.texture_index = None;
    triangle.tex_coords = None;
    preview.triangles.push(triangle);

    let app = SmsEditorApp {
        document: Some(test_document(vec![front, behind])),
        model_preview: Some(preview),
        ..camera_app()
    };
    let rect = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(200.0, 200.0));

    assert_eq!(
        app.object_at_screen_position(rect, rect.center())
            .as_deref(),
        Some("front-object")
    );
}

#[test]
fn viewport_picking_rejects_an_object_hidden_behind_stage_geometry() {
    let mut object = SceneObject::new("hidden-object", "Coin");
    object.transform.translation = [0.0, 0.0, 1000.0];
    let mut preview = preview_for_texture_alpha(false, false);
    preview.object_model_indices.insert(object.id.clone(), 2);
    for (model_index, depth, extent) in [(1, 500.0, 150.0), (2, 1000.0, 200.0)] {
        let mut triangle = textured_blended_triangle();
        triangle.vertices = [
            [-extent, -extent, depth],
            [extent, -extent, depth],
            [0.0, extent, depth],
        ];
        triangle.model_index = model_index;
        triangle.texture_index = None;
        triangle.tex_coords = None;
        preview.triangles.push(triangle);
    }

    let app = SmsEditorApp {
        document: Some(test_document(vec![object])),
        model_preview: Some(preview),
        ..camera_app()
    };
    let rect = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(200.0, 200.0));

    assert_eq!(app.object_at_screen_position(rect, rect.center()), None);
}

#[test]
fn viewport_picking_keeps_an_object_in_front_of_stage_geometry_selectable() {
    let mut object = SceneObject::new("visible-object", "Coin");
    object.transform.translation = [0.0, 0.0, 500.0];
    let mut preview = preview_for_texture_alpha(false, false);
    preview.object_model_indices.insert(object.id.clone(), 2);
    for (model_index, depth, extent) in [(1, 1000.0, 200.0), (2, 500.0, 150.0)] {
        let mut triangle = textured_blended_triangle();
        triangle.vertices = [
            [-extent, -extent, depth],
            [extent, -extent, depth],
            [0.0, extent, depth],
        ];
        triangle.model_index = model_index;
        triangle.texture_index = None;
        triangle.tex_coords = None;
        preview.triangles.push(triangle);
    }

    let app = SmsEditorApp {
        document: Some(test_document(vec![object])),
        model_preview: Some(preview),
        ..camera_app()
    };
    let rect = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(200.0, 200.0));

    assert_eq!(
        app.object_at_screen_position(rect, rect.center())
            .as_deref(),
        Some("visible-object")
    );
}

#[test]
fn viewport_picking_ignores_translucent_stage_geometry() {
    let mut object = SceneObject::new("object-under-water", "Coin");
    object.transform.translation = [0.0, 0.0, 1000.0];
    let mut preview = preview_for_texture_alpha(false, false);
    preview.object_model_indices.insert(object.id.clone(), 2);
    for (model_index, depth, extent, render_layer) in [
        (1, 500.0, 150.0, PreviewRenderLayer::Water),
        (2, 1000.0, 200.0, PreviewRenderLayer::Main),
    ] {
        let mut triangle = textured_blended_triangle();
        triangle.vertices = [
            [-extent, -extent, depth],
            [extent, -extent, depth],
            [0.0, extent, depth],
        ];
        triangle.model_index = model_index;
        triangle.render_layer = render_layer;
        triangle.texture_index = None;
        triangle.tex_coords = None;
        preview.triangles.push(triangle);
    }

    let app = SmsEditorApp {
        document: Some(test_document(vec![object])),
        model_preview: Some(preview),
        ..camera_app()
    };
    let rect = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(200.0, 200.0));

    assert_eq!(
        app.object_at_screen_position(rect, rect.center())
            .as_deref(),
        Some("object-under-water")
    );
}

#[test]
fn selected_object_outline_keeps_the_silhouette_and_removes_internal_edges() {
    let mut preview = preview_for_texture_alpha(false, false);
    preview
        .object_model_indices
        .insert("selected-object".to_string(), 9);
    for vertices in [
        [
            [-100.0, -100.0, 1000.0],
            [100.0, -100.0, 1000.0],
            [100.0, 100.0, 1000.0],
        ],
        [
            [-100.0, -100.0, 1000.0],
            [100.0, 100.0, 1000.0],
            [-100.0, 100.0, 1000.0],
        ],
    ] {
        let mut triangle = textured_blended_triangle();
        triangle.vertices = vertices;
        triangle.model_index = 9;
        triangle.texture_index = None;
        triangle.tex_coords = None;
        preview.triangles.push(triangle);
    }

    let app = SmsEditorApp {
        model_preview: Some(preview),
        selected_object_id: Some("selected-object".to_string()),
        ..camera_app()
    };
    let rect = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(200.0, 200.0));

    let segments = app.selected_object_outline_segments(rect);
    let paths = viewport_ui::outline_paths_from_segments(&segments);
    assert_eq!(paths.len(), 1);
    assert_eq!(paths[0].first(), paths[0].last());
}

#[test]
fn selected_object_outline_merges_overlapping_polygon_coverage() {
    let size = [8, 6];
    let mut coverage = vec![false; size[0] * size[1]];
    for y in 1..=4 {
        for x in 1..=4 {
            coverage[y * size[0] + x] = true;
        }
    }
    for y in 2..=3 {
        for x in 3..=6 {
            coverage[y * size[0] + x] = true;
        }
    }
    let rect = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(8.0, 6.0));
    let segments = viewport_ui::outline_segments_from_coverage(&coverage, size, [1, 6, 1, 4], rect);

    assert!(!segments.iter().any(|segment| {
        segment[0].x == 5.0 && segment[1].x == 5.0 && segment[0].y <= 3.0 && segment[1].y >= 3.0
    }));
}

#[test]
fn bounded_outline_coverage_matches_full_frame_coverage() {
    let size = [9, 7];
    let bounds = [3, 6, 2, 4];
    let rect = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(180.0, 140.0));
    let mut full = vec![false; size[0] * size[1]];
    for y in bounds[2]..=bounds[3] {
        for x in bounds[0]..=bounds[1] {
            full[y * size[0] + x] = !(x == 4 && y == 3);
        }
    }
    let bounded_size = [bounds[1] - bounds[0] + 1, bounds[3] - bounds[2] + 1];
    let mut bounded = vec![false; bounded_size[0] * bounded_size[1]];
    for y in bounds[2]..=bounds[3] {
        for x in bounds[0]..=bounds[1] {
            bounded[(y - bounds[2]) * bounded_size[0] + x - bounds[0]] = full[y * size[0] + x];
        }
    }

    assert_eq!(
        viewport_ui::outline_segments_from_coverage(&full, size, bounds, rect),
        viewport_ui::outline_segments_from_bounded_coverage(
            &bounded,
            bounded_size,
            [bounds[0], bounds[2]],
            size,
            bounds,
            rect,
        )
    );
}

#[test]
fn nozzle_box_tev_color_matches_runtime_item_type() {
    let registry = ObjectRegistry {
        objects: vec![sms_schema::ObjectDefinition {
            factory_name: "NozzleBox".to_string(),
            class_name: "TNozzleBox".to_string(),
            category: "MapObj".to_string(),
            source: sms_schema::SchemaSource::MarNameRefGen,
            display_name: None,
            preview_model: None,
            hidden: false,
            unsafe_to_edit: false,
        }],
        map_obj_string_tev_programs: vec![sms_schema::MapObjStringTevProgramDefinition {
            resource_name: "NozzleBox".to_string(),
            class_name: "TNozzleBox".to_string(),
            tev_register: 1,
            default_color: [255, 255, 255, 100],
            variants: vec![
                sms_schema::MapObjStringTevVariantDefinition {
                    selector_value: "normal_nozzle_item".to_string(),
                    color: [0, 0, 255, 100],
                },
                sms_schema::MapObjStringTevVariantDefinition {
                    selector_value: "rocket_nozzle_item".to_string(),
                    color: [255, 0, 0, 100],
                },
                sms_schema::MapObjStringTevVariantDefinition {
                    selector_value: "back_nozzle_item".to_string(),
                    color: [90, 90, 120, 100],
                },
            ],
            source_file: "src/MoveBG/Item.cpp".to_string(),
        }],
        ..ObjectRegistry::default()
    };
    let mut rocket = SceneObject::new("rocket-box", "NozzleBox");
    rocket.set_raw_param("actor_tail_string", "NozzleBox");
    rocket.set_raw_param("nozzle_box_item", "rocket_nozzle_item");
    let mut hover = SceneObject::new("hover-box", "NozzleBox");
    hover.set_raw_param("actor_tail_string", "NozzleBox");
    hover.set_raw_param("nozzle_box_item", "back_nozzle_item");
    let mut legacy = SceneObject::new("legacy-box", "NozzleBox");
    legacy.set_raw_param("actor_tail_string", "NozzleBox");
    legacy.set_raw_param("stream_string_1", "normal_nozzle_item");
    let color = |object: &SceneObject| {
        map_obj_string_tev_color(object, Some(&registry)).map(|definition| definition.color)
    };

    assert_eq!(color(&rocket), Some([255, 0, 0, 100]));
    assert_eq!(color(&hover), Some([90, 90, 120, 100]));
    assert_eq!(color(&legacy), Some([0, 0, 255, 100]));
    rocket.set_raw_param("nozzle_box_item", "Rocket_Nozzle_Item");
    assert_eq!(color(&rocket), Some([255, 255, 255, 100]));
    rocket.set_raw_param("actor_tail_string", "nozzlebox");
    assert_eq!(color(&rocket), None);

    let mut wrong_factory = SceneObject::new("wrong", "NozzleBoxAlias");
    wrong_factory.set_raw_param("actor_tail_string", "NozzleBox");
    wrong_factory.set_raw_param("nozzle_box_item", "normal_nozzle_item");
    assert_eq!(
        map_obj_string_tev_color(&wrong_factory, Some(&registry)),
        None
    );
}

#[test]
fn placement_stream_rgb_reaches_the_decomp_selected_tev_register() {
    let registry = ObjectRegistry {
        objects: vec![sms_schema::ObjectDefinition {
            factory_name: "FixturePaint".to_string(),
            class_name: "TFixturePaint".to_string(),
            category: "MapObj".to_string(),
            source: sms_schema::SchemaSource::MarNameRefGen,
            display_name: None,
            preview_model: None,
            hidden: false,
            unsafe_to_edit: false,
        }],
        map_obj_stream_tev_colors: vec![sms_schema::MapObjStreamTevColorDefinition {
            class_name: "TFixturePaint".to_string(),
            tev_register: 2,
            trailing_rgb_u32_count: 3,
            alpha: 255,
            source_file: "src/MoveBG/Fixture.cpp".to_string(),
        }],
        ..ObjectRegistry::default()
    };
    let mut object = SceneObject::new("paint", "FixturePaint");
    object.source_record_bytes = Some(
        [0xAA, 0xBB]
            .into_iter()
            .chain(0x0000_01FFu32.to_be_bytes())
            .chain(0x0000_0078u32.to_be_bytes())
            .chain(0x1234_5609u32.to_be_bytes())
            .collect(),
    );

    assert_eq!(
        map_obj_stream_tev_color(&object, Some(&registry)),
        Some(sms_schema::MapObjTevColorDefinition {
            register: 2,
            color: [255, 120, 9, 255],
        })
    );
    object.factory_name = "UnrelatedPaint".to_string();
    assert_eq!(map_obj_stream_tev_color(&object, Some(&registry)), None);
}

#[test]
#[ignore = "requires the extracted retail game"]
fn retail_nozzle_boxes_keep_typed_items_and_tev_colors() {
    let base_root = std::env::var_os("SMS_BASE_ROOT")
        .map(PathBuf::from)
        .expect("set SMS_BASE_ROOT to the extracted game's root");
    let expected = [
        (
            "mamma0",
            vec![
                ("normal_nozzle_item", [0, 0, 255, 100]),
                ("rocket_nozzle_item", [255, 0, 0, 100]),
                ("back_nozzle_item", [90, 90, 120, 100]),
            ],
        ),
        ("dolpic0", vec![("rocket_nozzle_item", [255, 0, 0, 100])]),
        (
            "dolpic10",
            vec![
                ("normal_nozzle_item", [0, 0, 255, 100]),
                ("rocket_nozzle_item", [255, 0, 0, 100]),
                ("rocket_nozzle_item", [255, 0, 0, 100]),
                ("back_nozzle_item", [90, 90, 120, 100]),
            ],
        ),
    ];

    let decomp_root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../..");
    let registry = SchemaGenerator::new(decomp_root)
        .generate()
        .expect("generate decomp-derived NozzleBox colors");
    for (stage, mut expected) in expected {
        let document = StageDocument::open(&base_root, stage)
            .unwrap_or_else(|error| panic!("open retail {stage}: {error}"))
            .with_registry(registry.clone());
        let mut actual: Vec<_> = document
            .objects
            .iter()
            .filter(|object| object.factory_name == "NozzleBox")
            .map(|object| {
                (
                    object.raw_param("nozzle_box_item").unwrap_or_else(|| {
                        panic!("{stage} NozzleBox lost its typed item selector")
                    }),
                    map_obj_string_tev_color(object, document.registry.as_ref())
                        .map(|definition| definition.color)
                        .unwrap_or_else(|| panic!("{stage} NozzleBox lost its TEV color")),
                )
            })
            .collect();
        actual.sort_unstable();
        expected.sort_unstable();
        assert_eq!(actual, expected, "unexpected retail {stage} NozzleBoxes");
    }
}

#[test]
fn npc_root_material_colors_follow_schema_channels() {
    let mut monte = SceneObject::new("monte", "NPCMonteMA");
    monte
        .raw_params
        .insert("npc_body_color_index".to_string(), "9".to_string().into());
    monte
        .raw_params
        .insert("npc_cloth_color_index".to_string(), "3".to_string().into());
    let registry = ObjectRegistry {
        npc_actors: vec![sms_schema::NpcActorDefinition {
            actor_key: "MonteMA".to_string(),
            source_file: "src/NPC/NpcInitData.cpp".to_string(),
            parts: Vec::new(),
        }],
        npc_material_colors: vec![sms_schema::NpcMaterialColorDefinition {
            actor_key: "MonteMA".to_string(),
            model_index: 0,
            color_index_channel: 1,
            change: sms_schema::NpcColorChangeDefinition {
                mode: 2,
                material_name: "_fuku_mat".to_string(),
                colors0: vec![[1, 2, 3, 255]],
                colors1: vec![[4, 5, 6, 255]],
            },
            source_file: "src/NPC/NpcInitData.cpp".to_string(),
        }],
        ..ObjectRegistry::default()
    };

    assert_eq!(npc_root_color_index(&monte, 0), Some(9));
    assert_eq!(npc_root_color_index(&monte, 1), Some(3));
    assert_eq!(
        registry
            .npc_material_colors_for(&monte.factory_name)
            .next()
            .map(|definition| definition.change.material_name.as_str()),
        Some("_fuku_mat")
    );
}

#[test]
fn npc_pollution_uses_white_k_color_with_amount_as_alpha() {
    let mut monte = SceneObject::new("monte", "NPCMonteMA");
    monte
        .raw_params
        .insert("npc_pollution_amount".to_string(), "37".to_string().into());
    monte.raw_params.insert(
        "npc_parts_color_index_0".to_string(),
        "2".to_string().into(),
    );

    assert_eq!(npc_pollution_k_color(&monte), Some([255, 255, 255, 37]));
    let mut maremb = SceneObject::new("fisher", "NPCMareMB");
    maremb
        .raw_params
        .insert("npc_pollution_amount".to_string(), "0".to_string().into());
    assert_eq!(npc_pollution_k_color(&maremb), Some([255, 255, 255, 0]));
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
        ["c:/game/mare0.szs!/marem/marem.bmt"]
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
        mipmap_enabled: false,
        do_edge_lod: false,
        bias_clamp: false,
        max_anisotropy: 0,
        min_lod: 0.0,
        max_lod: 0.0,
        lod_bias: 0.0,
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
fn accessory_dummy_texture_resolves_archive_shared_material_table() {
    let textures = [sms_formats::J3dTexturePreview {
        name: "J_mare_dammy".to_string(),
        width: 8,
        height: 8,
        format: 0,
        wrap_s: 0,
        wrap_t: 0,
        min_filter: 1,
        mag_filter: 1,
        mipmap_enabled: false,
        do_edge_lod: false,
        bias_clamp: false,
        max_anisotropy: 0,
        min_lod: 0.0,
        max_lod: 0.0,
        lod_bias: 0.0,
        mipmap_count: 1,
        rgba: vec![255; 8 * 8 * 4],
        mips: Vec::new(),
    }];

    assert_eq!(
        material_table_asset_score(
            "stage.szs!/maremb/maremb_set.bmd",
            &textures,
            "stage.szs!/marecommon/mare.bmt",
        ),
        Some((2, "mare".len()))
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
        mipmap_enabled: false,
        do_edge_lod: false,
        bias_clamp: false,
        max_anisotropy: 0,
        min_lod: 0.0,
        max_lod: 0.0,
        lod_bias: 0.0,
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
            "C:/game/dolpic0.szs!/montema/montema_wait.bck",
            "C:/game/dolpic0.szs!/montemcommon/mom_wait.bck",
            "C:/game/dolpic0.szs!/montem/mom_wait.bck",
        ]
    );

    let mare = SceneObject::new("mare", "NPCMareMB");
    assert_eq!(
        starting_joint_animation_candidates(&mare, "C:/game/mare0.szs!/marem/marem.bmd"),
        [
            "C:/game/mare0.szs!/maremb/maremb_wait.bck",
            "C:/game/mare0.szs!/marem/marem_wait.bck",
        ]
    );
}

#[test]
fn level_transformation_overrides_scrub_from_retail_start_to_bind_pose() {
    let target = LevelTransformTarget {
        joint_index: 7,
        translation_offset: [0.0, -1500.0, 0.0],
        scale_multiplier: [1.0, 0.008, 1.0],
        behavior: LevelTransformBehavior::Linear,
    };

    let start = level_transform_overrides(&[target], 0.0)[0];
    assert_eq!(start.translation_offset, [0.0, -1500.0, 0.0]);
    assert_eq!(start.scale_multiplier, [1.0, 0.008, 1.0]);

    let middle = level_transform_overrides(&[target], 0.5)[0];
    assert_eq!(middle.translation_offset, [0.0, -750.0, 0.0]);
    assert!((middle.scale_multiplier[1] - 0.504).abs() < 0.0001);

    let end = level_transform_overrides(&[target], 1.0)[0];
    assert_eq!(end.translation_offset, [0.0; 3]);
    assert_eq!(end.scale_multiplier, [1.0; 3]);
}

#[test]
fn linked_pollution_meshes_follow_retail_visibility_swap() {
    let hidden = LevelTransformTarget {
        joint_index: 3,
        translation_offset: [0.0; 3],
        scale_multiplier: [1.0; 3],
        behavior: LevelTransformBehavior::AlwaysHidden,
    };
    let cleaned = LevelTransformTarget {
        joint_index: 4,
        translation_offset: [0.0; 3],
        scale_multiplier: [1.0; 3],
        behavior: LevelTransformBehavior::HideAfterStart,
    };

    assert!(level_transform_target_is_hidden(&hidden, 0.0));
    assert!(!level_transform_target_is_hidden(&cleaned, 0.0));
    assert!(level_transform_target_is_hidden(&cleaned, 0.1));
    assert_eq!(
        level_transform_overrides(&[hidden], 0.0)[0].scale_multiplier,
        [1.0; 3]
    );
}

#[test]
fn gatekeeper_uses_retail_sleep_and_texture_animations() {
    let gatekeeper = SceneObject::new("boss", "GateKeeper");
    let model = "C:/game/dolpic0.szs!/gatekeeper/gene_pakkun_model1.bmd";

    assert_eq!(
        starting_joint_animation_candidates(&gatekeeper, model),
        ["C:/game/dolpic0.szs!/gatekeeper/gene_pakkun_wait1.bck"]
    );
    assert_eq!(
        model_texture_srt_animation_paths(model),
        [
            "C:/game/dolpic0.szs!/gatekeeper/gene_pakkun_tex0.btk",
            "C:/game/dolpic0.szs!/gatekeeper/gene_pakkun_tex1.btk",
        ]
    );
}

#[test]
fn gatekeeper_replaces_its_dummy_with_the_stage_pollution_texture() {
    assert_eq!(
        actor_runtime_texture_replacements("GateKeeper"),
        [("Q_kepper_dummy_128IA4", "/map/pollution/h_ma_rak.bti")]
    );
    assert!(actor_runtime_texture_replacements("gatekeeper").is_empty());
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
fn enemy_material_colors_override_only_decomp_assigned_channels() {
    let registry = ObjectRegistry {
        enemy_material_colors: vec![sms_schema::EnemyMaterialTevColorDefinition {
            factory_name: "PoiHanaRed".to_string(),
            material_name: "_body".to_string(),
            tev_register: 0,
            color: [Some(283), Some(-53), Some(-122), None],
            source_file: "src/Enemy/poihana.cpp".to_string(),
        }],
        ..ObjectRegistry::default()
    };
    let mut tev_colors = [[0; 4]; 4];
    tev_colors[0] = [1, 2, 3, 77];

    apply_enemy_tev_overrides(&mut tev_colors, "_body", "PoiHanaRed", Some(&registry));

    assert_eq!(tev_colors[0], [283, -53, -122, 77]);

    let mut wrong_case = [[0; 4]; 4];
    apply_enemy_tev_overrides(&mut wrong_case, "_body", "poihanared", Some(&registry));
    assert_eq!(wrong_case, [[0; 4]; 4]);
}

#[test]
fn surf_geso_shared_model_colors_follow_exact_decomp_resource_variants() {
    let variants = [
        ("SurfGesoRed", [255, 180, 255, 255]),
        ("SurfGesoYellow", [255, 255, 125, 255]),
        ("SurfGesoGreen", [180, 255, 180, 255]),
    ];
    let registry = ObjectRegistry {
        objects: variants
            .iter()
            .map(|(factory_name, _)| ObjectDefinition {
                factory_name: (*factory_name).to_string(),
                class_name: "TSurfGesoObj".to_string(),
                category: "MapObj".to_string(),
                source: sms_schema::SchemaSource::MarNameRefGen,
                display_name: None,
                preview_model: None,
                hidden: false,
                unsafe_to_edit: false,
            })
            .collect(),
        map_obj_model_overrides: variants
            .iter()
            .map(
                |(resource_name, color)| sms_schema::MapObjModelOverrideDefinition {
                    resource_name: (*resource_name).to_string(),
                    class_name: "TSurfGesoObj".to_string(),
                    model_path: "/scene/mapObj/surfgeso.bmd".to_string(),
                    load_flags: 0x1022_0000,
                    tev_color: Some(sms_schema::MapObjTevColorDefinition {
                        register: 1,
                        color: *color,
                    }),
                    binding_source_file: "src/MoveBG/MapObjRicco.cpp".to_string(),
                    model_source_file: "src/MoveBG/MapObjManager.cpp".to_string(),
                },
            )
            .collect(),
        ..ObjectRegistry::default()
    };

    for (factory_name, expected) in variants {
        let mut object = SceneObject::new(factory_name, factory_name);
        object.set_raw_param("actor_tail_string", factory_name);
        assert_eq!(
            map_obj_model_override_tev_color(&object, Some(&registry)),
            Some(sms_schema::MapObjTevColorDefinition {
                register: 1,
                color: expected
            })
        );
    }

    let mut wrong_class = SceneObject::new("wrong", "Shine");
    wrong_class.set_raw_param("actor_tail_string", "SurfGesoRed");
    assert_eq!(
        map_obj_model_override_tev_color(&wrong_class, Some(&registry)),
        None
    );
}

#[test]
fn npc_parts_mask_uses_decomp_schema_metadata() {
    let mut document = test_document(Vec::new());
    document.registry = Some(ObjectRegistry {
        npc_actors: vec![sms_schema::NpcActorDefinition {
            actor_key: "MareM".to_string(),
            source_file: "src/NPC/NpcInitData.cpp".to_string(),
            parts: vec![sms_schema::NpcPartDefinition {
                bit_index: 0,
                color_index_channel: 0,
                models: vec![sms_schema::NpcPartModelDefinition {
                    joint_name: Some("kubi".to_string()),
                    model_name: "custom_hat.bmd".to_string(),
                }],
                color_changes: vec![sms_schema::NpcColorChangeDefinition {
                    mode: 2,
                    material_name: "_hat".to_string(),
                    colors0: vec![[10, 20, 30, 255]],
                    colors1: vec![[40, 50, 60, 255]],
                }],
                uses_pollution: true,
                uses_shared_materials: true,
            }],
        }],
        ..ObjectRegistry::default()
    });
    let mut mare = SceneObject::new("mare", "NPCMareMA");
    mare.raw_params
        .insert("npc_parts_mask".to_string(), "1".to_string().into());
    let parts = npc_accessory_specs(&document, &mare);

    assert_eq!(parts.len(), 1);
    assert_eq!(parts[0].joint_name.as_deref(), Some("kubi"));
    assert_eq!(parts[0].asset_suffix, "/custom_hat.bmd");
    assert_eq!(parts[0].color_index_channel, 0);
    assert_eq!(parts[0].color_changes[0].material_name, "_hat");
    assert_eq!(parts[0].color_changes[0].colors1[0], [40, 50, 60, 255]);
    assert!(parts[0].uses_pollution);
}

#[test]
fn peach_hair_parts_use_their_retail_wait_animations() {
    assert_eq!(
        accessory_joint_animation_path("stage.szs!/peach/peach_hair_normal.bmd").as_deref(),
        Some("stage.szs!/peach/peach_hair_normal_wait.bck")
    );
    assert_eq!(
        accessory_joint_animation_path("stage.szs!/peach/peach_hair_ponytail.bmd").as_deref(),
        Some("stage.szs!/peach/peach_hair_ponytail_wait.bck")
    );
    assert_eq!(
        accessory_joint_animation_path("stage.szs!/custom/lantern.bdl").as_deref(),
        Some("stage.szs!/custom/lantern_wait.bck")
    );
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
fn coin_circle_shadow_uses_retail_radius_on_the_world_surface() {
    let mut world = textured_blended_triangle();
    world.vertices = [
        [-100.0, 10.0, -100.0],
        [100.0, 10.0, -100.0],
        [0.0, 10.0, 100.0],
    ];
    let mut water = world;
    water.vertices = water.vertices.map(|mut vertex| {
        vertex[1] = 20.0;
        vertex
    });
    water.render_layer = PreviewRenderLayer::Water;

    let transform = Transform {
        translation: [0.0, 50.0, 0.0],
        scale: [0.7, 0.7, 0.7],
        ..Transform::default()
    };
    let ground_y = shadow_ground_height(transform.translation, &[world, water]).unwrap();
    let mut shadows = Vec::new();
    push_coin_circle_shadow(&mut shadows, transform, ground_y, 4, 8);

    assert_eq!(ground_y, 10.0);
    assert_eq!(shadows.len(), 20);
    assert_eq!(shadows[0].vertices[0], [0.0, 11.5, 0.0]);
    assert!((shadows[0].vertices[1][0] - 35.0).abs() < 0.001);
    assert_eq!(shadows[0].render_layer, PreviewRenderLayer::Shadow);
}

#[test]
fn invisible_coin_proxy_does_not_get_a_preview_shadow() {
    let mut object = SceneObject::new("coin-proxy", "Coin");
    object.set_raw_param("stream_string_0", "コイン キャラ");
    object.set_raw_param("actor_tail_string", "invisible_coin");

    assert!(!is_coin_object(&object));
}

#[test]
fn monte_model_loader_flags_follow_manager_entries() {
    assert_eq!(
        actor_model_loader_flags(&SceneObject::new("ma", "NPCMonteMA")),
        Some(0x1030_0000)
    );
    assert_eq!(
        actor_model_loader_flags(&SceneObject::new("md", "NPCMonteMD")),
        Some(0x1021_0000)
    );
    assert_eq!(
        actor_model_loader_flags(&SceneObject::new("boss", "GateKeeper")),
        None,
        "enemy loader flags come from the decomp-derived preview catalog"
    );
    assert_eq!(
        actor_model_loader_flags(&SceneObject::new("mare-m", "NPCMareMD")),
        Some(0x1030_0000)
    );
    assert_eq!(
        actor_model_loader_flags(&SceneObject::new("mare-w", "NPCMareWB")),
        Some(0x1030_0000)
    );
    assert_eq!(
        actor_model_loader_flags(&SceneObject::new("wrong-case", "npcMonteMA")),
        None
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
        .insert("name".to_string(), "PalmLeaf 2".to_string().into());
    palm_leaf.asset_hints.push(AssetRef {
        path: "stage.szs!/mapobj/palmleaf.bmd".to_string(),
        role: AssetRole::PreviewModel,
    });

    assert_eq!(
        object_preview_model_path(&palm_leaf, &BTreeSet::new()).as_deref(),
        Some("stage.szs!/mapobj/palmleaf.bmd")
    );
}

#[test]
fn explicit_preview_hints_stay_distinct_from_inferred_fallbacks() {
    let mut object = SceneObject::new("boss", "BossTelesa");
    object.asset_hints.push(AssetRef {
        path: "stage.szs!/btelesa/guessed.bmd".to_string(),
        role: AssetRole::InferredPreviewModel,
    });

    assert!(object_preview_model_path(&object, &BTreeSet::new()).is_none());
    assert_eq!(
        object_inferred_preview_model_path(&object, &BTreeSet::new()).as_deref(),
        Some("stage.szs!/btelesa/guessed.bmd")
    );

    object.asset_hints.push(AssetRef {
        path: "stage.szs!/btelesa/explicit.bmd".to_string(),
        role: AssetRole::PreviewModel,
    });
    assert_eq!(
        object_preview_model_path(&object, &BTreeSet::new()).as_deref(),
        Some("stage.szs!/btelesa/explicit.bmd")
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
            mipmap_enabled: false,
            do_edge_lod: false,
            bias_clamp: false,
            max_anisotropy: 0,
            min_lod: 0.0,
            max_lod: 0.0,
            lod_bias: 0.0,
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
        loaded_models: 1,
        failed_models: 0,
        model_failures: Vec::new(),
        source_vertices: 0,
        source_triangles: 0,
        source_textures: 1,
        object_model_indices: BTreeMap::new(),
        mirror_actor_positions: BTreeMap::new(),
        mirror_cubes: Vec::new(),
        mirror_model_slots: BTreeMap::new(),
        animated_models: Vec::new(),
        animated_flags: Vec::new(),
        rotating_models: Vec::new(),
        level_transform_models: Vec::new(),
        level_transform_particles: Vec::new(),
        actor_particles: Vec::new(),
        level_transform_duration_frames: 600.0,
        level_transform_particle_end_frames: 600.0,
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
        billboard: None,
        particle_type: None,
        particle_pivot: None,
        particle_direction: None,
        particle_color_mode: None,
        particle_environment_color: None,
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
fn authored_water_reflections_follow_environment_visibility() {
    let path = "stage.szs!/map/map/reflectparts.bmd";

    assert!(path_is_water_reflection_model_path(path));
    assert!(is_default_preview_model_path(path, true, true, false));
    assert!(!is_default_preview_model_path(path, false, true, true));
    assert_eq!(
        preview_render_layer_for_model_path(path),
        PreviewRenderLayer::MirrorScene
    );
    assert!(!is_camera_bounds_model_path(path));

    // Sunshine copies the main sky's material table onto ReflectSky before
    // drawing it. The editor mirrors its already-loaded sky instead of showing
    // the unpatched helper geometry.
    let reflect_sky = "stage.szs!/map/map/reflectsky.bmd";
    assert!(path_is_mirror_sky_helper_model_path(reflect_sky));
    assert!(!path_is_water_reflection_model_path(reflect_sky));
    assert!(!is_default_preview_model_path(
        reflect_sky,
        true,
        true,
        false
    ));
    assert!(!is_default_preview_model_path(
        reflect_sky,
        true,
        true,
        true
    ));
    assert_eq!(
        preview_render_layer_for_model_path(reflect_sky),
        PreviewRenderLayer::MirrorScene
    );
}

#[test]
fn authored_mirror_surface_follows_environment_visibility() {
    let path = "stage.szs!/map/mirror/mirror00.bmd";

    assert!(path_is_mirror_surface_model_path(path));
    assert!(is_default_preview_model_path(path, true, true, false));
    assert!(!is_default_preview_model_path(path, false, true, true));
    assert_eq!(
        preview_render_layer_for_model_path(path),
        PreviewRenderLayer::MirrorSurface
    );
    assert!(!is_camera_bounds_model_path(path));
}

#[test]
fn mirror_surface_slots_follow_the_runtime_filename_mapping() {
    assert_eq!(
        mirror_surface_model_slot("bianco7", "stage.szs!/map/mirror/mirror00.bmd"),
        Some(0)
    );
    assert_eq!(
        mirror_surface_model_slot("pinna0", "stage.szs!/map/mirror/mirror205.bmd"),
        Some(0)
    );
    assert_eq!(
        mirror_surface_model_slot("bianco7", "stage.szs!/map/map/map.bmd"),
        None
    );

    let active = BTreeSet::from([0]);
    assert!(mirror_surface_model_is_active(
        "bianco7",
        "stage.szs!/map/mirror/mirror00.bmd",
        &active,
    ));
    assert!(!mirror_surface_model_is_active(
        "bianco7",
        "stage.szs!/map/mirror/mirror01.bmd",
        &active,
    ));
}

#[test]
fn mirror_cube_membership_matches_sunshines_bottom_anchored_rotated_volume() {
    let axis_aligned = PreviewMirrorCube {
        center: [10.0, 20.0, 30.0],
        rotation_degrees: [0.0; 3],
        dimensions: [100.0, 200.0, 300.0],
        model_slot: 0,
    };
    assert!(axis_aligned.contains([10.0, 20.1, 30.0]));
    assert!(axis_aligned.contains([59.9, 219.9, 179.9]));
    assert!(!axis_aligned.contains([10.0, 20.0, 30.0]));
    assert!(!axis_aligned.contains([10.0, 220.0, 30.0]));
    assert!(!axis_aligned.contains([60.0, 100.0, 30.0]));

    let yawed = PreviewMirrorCube {
        center: [0.0; 3],
        rotation_degrees: [0.0, 90.0, 0.0],
        dimensions: [100.0, 100.0, 20.0],
        model_slot: 1,
    };
    assert!(yawed.contains([0.0, 50.0, 40.0]));
    assert!(!yawed.contains([40.0, 50.0, 0.0]));
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
    let registry = reset_fruit_registry();
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
        ("RedPepper", 350.0),
        ("FruitBanana", 300.0),
    ] {
        let mut object = SceneObject::new(resource_name, "ResetFruit");
        object.raw_params.insert(
            "stream_string_0".to_string(),
            resource_name.to_string().into(),
        );

        assert_eq!(
            reset_fruit_preview_transform(&object, transform, Some(&registry)).translation[1],
            expected_y
        );
    }
}

#[test]
fn reset_fruit_draw_transform_scales_the_runtime_body_radius() {
    let registry = reset_fruit_registry();
    let mut object = SceneObject::new("pine", "ResetFruit");
    object.raw_params.insert(
        "stream_string_0".to_string(),
        "FruitPine".to_string().into(),
    );
    let transform = Transform {
        translation: [0.0, 100.0, 0.0],
        rotation_degrees: [0.0; 3],
        scale: [2.0; 3],
    };

    assert_eq!(
        reset_fruit_preview_transform(&object, transform, Some(&registry)).translation[1],
        210.0
    );

    object.factory_name = "resetFruit".to_string();
    assert_eq!(
        reset_fruit_preview_transform(&object, transform, Some(&registry)),
        transform
    );
}

#[test]
fn reset_fruit_matrix_correction_includes_xyz_rotation() {
    let registry = reset_fruit_registry();
    let mut banana = SceneObject::new("banana", "ResetFruit");
    banana.set_raw_param("actor_tail_string", "FruitBanana");
    let transform = Transform {
        translation: [0.0, 100.0, 0.0],
        rotation_degrees: [30.0, 40.0, 50.0],
        scale: [1.0, 1.5, 1.0],
    };

    let transformed = reset_fruit_preview_transform(&banana, transform, Some(&registry));
    assert!((transformed.translation[1] - 114.784_58).abs() < 0.000_1);
}

fn reset_fruit_registry() -> ObjectRegistry {
    let entries = [
        ("FruitCoconut", 0x4000_0390, 40, None, None),
        ("FruitPapaya", 0x4000_0391, 40, None, None),
        ("FruitPine", 0x4000_0392, 50, None, Some(10)),
        ("FruitDurian", 0x4000_0393, 45, None, None),
        ("FruitBanana", 0x4000_0394, 50, Some(50), None),
        ("RedPepper", 0x4000_0395, 50, None, None),
    ];
    ObjectRegistry {
        map_obj_resources: entries
            .iter()
            .map(
                |(resource_name, actor_type, _, _, _)| sms_schema::MapObjResourceDefinition {
                    resource_name: (*resource_name).to_string(),
                    actor_type: *actor_type,
                    primary_model: Some(format!("{resource_name}.bmd")),
                    load_flags: 0x1022_0000,
                    source_file: "src/MoveBG/MapObjInit.cpp".to_string(),
                },
            )
            .collect(),
        map_obj_ball_transforms: entries
            .iter()
            .map(|(_, actor_type, body_radius, positive, one_minus)| {
                sms_schema::MapObjBallTransformDefinition {
                    actor_type: *actor_type,
                    body_radius: *body_radius,
                    positive_y_axis_subtract: *positive,
                    one_minus_y_axis_subtract: *one_minus,
                    source_file: "src/MoveBG/MapObjBall.cpp".to_string(),
                }
            })
            .collect(),
        ..ObjectRegistry::default()
    }
}

#[test]
fn case_distinct_factories_do_not_inherit_coin_or_npc_behavior() {
    assert!(!is_coin_object(&SceneObject::new("wrong-case", "coinRed")));
    assert_eq!(
        runtime_yaw_degrees_per_frame(&SceneObject::new("wrong-case", "shine")),
        0.0
    );

    let wrong_case_npc = SceneObject::new("wrong-case", "npcMonteMA");
    assert!(starting_joint_animation_candidates(
        &wrong_case_npc,
        "C:/game/dolpic0.szs!/montema/moma_model.bmd"
    )
    .is_empty());
    assert!(starting_texture_pattern_candidates(
        &wrong_case_npc,
        "C:/game/dolpic0.szs!/montema/moma_model.bmd"
    )
    .is_empty());
    assert!(actor_runtime_texture_replacements(&wrong_case_npc.factory_name).is_empty());
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
fn pollution_bitmap_replaces_every_material_alias_of_the_dynamic_texture() {
    fn texture(name: &str, width: u16, height: u16, value: u8) -> sms_formats::J3dTexturePreview {
        sms_formats::J3dTexturePreview {
            name: name.to_string(),
            width,
            height,
            format: 1,
            wrap_s: 0,
            wrap_t: 0,
            min_filter: 0,
            mag_filter: 0,
            mipmap_enabled: true,
            do_edge_lod: false,
            bias_clamp: false,
            max_anisotropy: 0,
            min_lod: 0.0,
            max_lod: 1.0,
            lod_bias: 0.0,
            mipmap_count: 2,
            rgba: vec![value; width as usize * height as usize * 4],
            mips: vec![],
        }
    }

    let mut textures = vec![
        texture("DummyPollution256x256_I8", 2, 2, 1),
        texture("TestChoco2", 2, 2, 2),
        texture("DummyPollution256x256_I8", 2, 2, 3),
        texture("DummyPollution256x256_I8", 1, 1, 4),
    ];
    let runtime_mask = vec![9; 2 * 2 * 4];

    replace_pollution_mask_texture_aliases(&mut textures, 2, 2, &runtime_mask);

    assert_eq!(textures[0].rgba, runtime_mask);
    assert_eq!(textures[2].rgba, runtime_mask);
    assert_eq!(textures[0].mipmap_count, 1);
    assert_eq!(textures[2].mipmap_count, 1);
    assert!(!textures[0].mipmap_enabled);
    assert_eq!(textures[0].min_lod, 0.0);
    assert_eq!(textures[0].max_lod, 0.0);
    assert_eq!(textures[0].lod_bias, 0.0);
    assert!(textures[0].mips.is_empty());
    assert!(textures[2].mips.is_empty());
    assert_eq!(textures[1].rgba, vec![2; 2 * 2 * 4]);
    assert_eq!(textures[3].rgba, vec![4; 4]);
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
fn decomp_owned_map_static_models_require_a_matching_placement() {
    let mut document = test_document(Vec::new());
    document.registry = Some(ObjectRegistry {
        map_static_models: vec![
            sms_schema::MapStaticModelDefinition {
                actor_name: "BiancoRiver".to_string(),
                model_path: Some("/scene/map/map/BiancoRiver.bmd".to_string()),
                load_flags: 0x1021_0000,
                source_file: "src/Map/MapStaticObject.cpp".to_string(),
                stage_bootstrap_created: false,
            },
            sms_schema::MapStaticModelDefinition {
                actor_name: "BiaWaterPollution".to_string(),
                model_path: Some("/scene/map/map/BiaWaterPollution.bmd".to_string()),
                load_flags: 0x1122_0000,
                source_file: "src/Map/MapStaticObject.cpp".to_string(),
                stage_bootstrap_created: false,
            },
            sms_schema::MapStaticModelDefinition {
                actor_name: "sea".to_string(),
                model_path: Some("/scene/map/map/sea.bmd".to_string()),
                load_flags: 0x1022_0000,
                source_file: "src/Map/MapStaticObject.cpp".to_string(),
                stage_bootstrap_created: true,
            },
            sms_schema::MapStaticModelDefinition {
                actor_name: "mareSeaPollutionS34567".to_string(),
                model_path: None,
                load_flags: 0x1021_0000,
                source_file: "src/Map/MapStaticObject.cpp".to_string(),
                stage_bootstrap_created: false,
            },
        ],
        ..ObjectRegistry::default()
    });
    let mut river = SceneObject::new("river", "MapStaticObj");
    river.raw_params.insert(
        "stream_string_0".to_string(),
        "BiancoRiver".to_string().into(),
    );
    document.objects.push(river);

    assert!(map_static_model_is_active(
        &document,
        "stage.szs!/map/map/BiancoRiver.bmd"
    ));
    assert!(!map_static_model_is_active(
        &document,
        "stage.szs!/map/map/BiaWaterPollution.bmd"
    ));
    assert!(map_static_model_is_active(
        &document,
        "stage.szs!/map/map/sea.bmd"
    ));
    assert!(map_static_model_is_active(
        &document,
        "stage.szs!/map/map/map.bmd"
    ));
    assert_eq!(
        map_static_model_loader_flags(&document, "stage.szs!/map/map/sea.bmd"),
        Some(0x1022_0000)
    );

    let mut pollution = SceneObject::new("dirty lake", "MapStaticObj");
    pollution.raw_params.insert(
        "stream_string_0".to_string(),
        "BiaWaterPollution".to_string().into(),
    );
    document.objects.push(pollution);
    assert!(map_static_model_is_active(
        &document,
        "stage.szs!/map/map/BiaWaterPollution.bmd"
    ));
    assert_eq!(
        map_static_model_loader_flags(&document, "stage.szs!/map/map/BiaWaterPollution.bmd"),
        Some(0x1122_0000)
    );

    assert!(map_static_model_is_active(
        &document,
        "stage.szs!/map/map/mareSeaPollutionS34567.bmd"
    ));
    assert_eq!(
        map_static_model_loader_flags(&document, "stage.szs!/map/map/mareSeaPollutionS34567.bmd"),
        None
    );
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
fn sea_indirect_is_the_default_screen_copy_water_effect() {
    let sea_path = "stage.szs!/map/map/seaindirect.bmd";

    assert!(is_default_preview_model_path(sea_path, true, true, false));
    assert!(!is_default_preview_model_path(sea_path, false, true, true));
    assert_eq!(
        preview_render_layer_for_model_path(sea_path),
        PreviewRenderLayer::IndirectWater
    );
    assert!(!is_camera_bounds_model_path(sea_path));
}

#[test]
fn dormant_puddle_indirect_helpers_stay_hidden_by_default() {
    let path = "stage.szs!/map/mirror/puddle_ind00.bmd";

    assert!(!is_default_preview_model_path(path, true, true, true));
    assert_ne!(
        preview_render_layer_for_model_path(path),
        PreviewRenderLayer::Water
    );
    assert!(!is_camera_bounds_model_path(path));
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
fn billboard_transform_tracks_instance_center_rotation_and_scale() {
    let billboard = J3dBillboard {
        mode: sms_formats::J3dBillboardMode::Full,
        center: [1.0, 2.0, 3.0],
        axes: [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]],
        offsets: [[2.0, 3.0, 4.0]; 3],
        normals: None,
    };
    let transform = Transform {
        translation: [10.0, 20.0, 30.0],
        rotation_degrees: [0.0, 90.0, 0.0],
        scale: [2.0, 3.0, 4.0],
    };
    let transformed = transform_j3d_billboard(billboard, transform, None).unwrap();

    assert_vec3_close(transformed.center, [22.0, 26.0, 28.0]);
    assert_vec3_close(transformed.offsets[0], [4.0, 9.0, 16.0]);
    assert_vec3_close(transformed.axes[0], [0.0, 0.0, -1.0]);
    assert_vec3_close(transformed.axes[1], [0.0, 1.0, 0.0]);
    assert_vec3_close(transformed.axes[2], [1.0, 0.0, 0.0]);
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
                billboard: None,
                particle_type: None,
                particle_pivot: None,
                particle_direction: None,
                particle_color_mode: None,
                particle_environment_color: None,
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
            loaded_models: 1,
            failed_models: 0,
            model_failures: Vec::new(),
            source_vertices: 3,
            source_triangles: 1,
            source_textures: 0,
            object_model_indices,
            mirror_actor_positions: BTreeMap::from([(7, old_transform.translation)]),
            mirror_cubes: Vec::new(),
            mirror_model_slots: BTreeMap::new(),
            animated_models: Vec::new(),
            animated_flags: Vec::new(),
            rotating_models: Vec::new(),
            level_transform_models: Vec::new(),
            level_transform_particles: Vec::new(),
            actor_particles: Vec::new(),
            level_transform_duration_frames: 600.0,
            level_transform_particle_end_frames: 600.0,
        }),
        ..SmsEditorApp::default()
    };

    assert!(app
        .update_object_preview_transform("obj-1", old_transform, new_transform)
        .is_some());
    assert_eq!(
        app.model_preview
            .as_ref()
            .and_then(|preview| preview.mirror_actor_positions.get(&7)),
        Some(&new_transform.translation)
    );
    let preview = app.model_preview.as_ref().unwrap();
    let ranges = document_commands::preview_triangle_ranges_for_model(preview, "obj-1");
    assert_eq!(ranges.len(), 1);
    assert_eq!(ranges[0], 0..1);
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

    app.mutate_document("Moved object", |document| {
        document.objects[0].transform.translation[0] = 25.0;
    });
    assert!(app.is_dirty());

    app.mutate_document("Restored object", |document| {
        document.objects[0].transform.translation[0] = 0.0;
    });
    assert!(!app.is_dirty());
}

#[test]
fn project_save_uses_the_same_trimmed_project_path_as_project_load() {
    let root = std::env::temp_dir().join(format!(
        "sms-editor-app-save-path-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let mut app = SmsEditorApp {
        project_root: format!("  {}  ", root.display()),
        document: Some(test_document(vec![SceneObject::new("obj-1", "Coin")])),
        ..SmsEditorApp::default()
    };

    assert!(app.save_project());
    assert!(root.join("sms-project.toml").is_file());
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn completed_stage_load_is_discarded_when_the_project_path_changed() {
    let document = test_document(Vec::new());
    let scene = RenderScene::from_document(&document);
    let mut app = SmsEditorApp {
        base_root: "base-root".to_string(),
        project_root: "project-b".to_string(),
        stage_id: "dolpic0".to_string(),
        ..SmsEditorApp::default()
    };
    let loaded = LoadedStage {
        base_root: "base-root".to_string(),
        project_root: "project-a".to_string(),
        archives: Vec::new(),
        registry: None,
        schema_warning: None,
        document,
        scene,
        preview: None,
    };

    app.apply_loaded_stage(loaded);

    assert!(app.document.is_none());
    assert!(app
        .log
        .iter()
        .any(|message| message.contains("superseded project root")));
}

#[test]
fn completed_stage_load_is_discarded_when_the_selected_stage_changed() {
    let document = test_document(Vec::new());
    let scene = RenderScene::from_document(&document);
    let mut app = SmsEditorApp {
        base_root: "base-root".to_string(),
        project_root: "project".to_string(),
        stage_id: "bianco0".to_string(),
        ..SmsEditorApp::default()
    };
    let loaded = LoadedStage {
        base_root: "base-root".to_string(),
        project_root: "project".to_string(),
        archives: Vec::new(),
        registry: None,
        schema_warning: None,
        document,
        scene,
        preview: None,
    };

    app.apply_loaded_stage(loaded);

    assert!(app.document.is_none());
    assert!(app
        .log
        .iter()
        .any(|message| message.contains("superseded stage")));
}

#[test]
fn schema_refresh_updates_derived_preview_metadata_without_marking_the_document_dirty() {
    let object = SceneObject::new("obj-1", "Fixture");
    let mut document = test_document(vec![object.clone()]);
    document.assets.push(StageAsset {
        path: PathBuf::from("stage.szs!/map/fixture.bmd"),
        kind: StageAssetKind::Model,
    });
    let mut app = SmsEditorApp {
        document: Some(document),
        saved_objects: vec![object],
        ..SmsEditorApp::default()
    };
    let registry = ObjectRegistry {
        object_resources: vec![sms_schema::ObjectResourceBinding {
            factory_name: "Fixture".to_string(),
            model_index: 0,
            role: sms_schema::ObjectResourceRole::Primary,
            model_name: "fixture.bmd".to_string(),
            resource_base: None,
            load_flags: 0,
            source_file: "src/fixture.cpp".to_string(),
        }],
        ..ObjectRegistry::default()
    };
    let (sender, receiver) = std::sync::mpsc::channel();
    sender
        .send(BackgroundResult::Schema(Box::new(Ok(registry))))
        .unwrap();
    app.background_receiver = Some(receiver);

    app.poll_background_task(&egui::Context::default());

    assert!(!app.is_dirty());
    assert_eq!(app.document.as_ref().unwrap().objects, app.saved_objects);
    assert_eq!(app.saved_objects[0].asset_hints.len(), 1);
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
    assert!(
        app.document.as_ref().unwrap().changed_files.is_empty(),
        "transaction deltas must not serialize the full editor overlay"
    );
    app.commit_undo_transaction("Moved object");

    assert_eq!(app.undo_stack.len(), 1);
    assert!(matches!(
        app.undo_stack.back().unwrap().deltas.as_slice(),
        [ObjectDelta::Update { before, after }]
            if before.transform.translation[0] == 0.0
                && after.transform.translation[0] == 20.0
    ));
    assert_eq!(app.document.as_ref().unwrap().changed_files.len(), 1);
    app.undo();
    assert_eq!(app.selected_object().unwrap().transform.translation[0], 0.0);
    app.redo();
    assert_eq!(
        app.selected_object().unwrap().transform.translation[0],
        20.0
    );
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
        lighting: Default::default(),
        actor_previews: BTreeMap::new(),
        loaded_project: None,
    }
}

#[test]
fn model_preview_failures_are_deduplicated_and_detail_bounded() {
    let mut failed_assets = BTreeSet::new();
    let mut failures = Vec::new();
    for index in 0..(MAX_MODEL_FAILURE_DETAILS + 3) {
        record_model_preview_failure(
            &mut failed_assets,
            &mut failures,
            &format!("stage.szs!/map/model-{index}.bmd"),
            format!("parse error {index}"),
        );
    }
    record_model_preview_failure(
        &mut failed_assets,
        &mut failures,
        "STAGE.SZS!/MAP/MODEL-0.BMD",
        "duplicate error".to_string(),
    );

    assert_eq!(failed_assets.len(), MAX_MODEL_FAILURE_DETAILS + 3);
    assert_eq!(failures.len(), MAX_MODEL_FAILURE_DETAILS);
    assert_eq!(failures[0].error, "parse error 0");
}

#[test]
fn a_failure_only_preview_retains_actionable_asset_details() {
    let mut document = test_document(Vec::new());
    document.assets.push(StageAsset {
        path: PathBuf::from("definitely-missing-preview-model.bmd"),
        kind: StageAssetKind::Model,
    });

    let preview = SmsEditorApp::build_model_preview(
        &document,
        PreviewVisibility {
            environment: true,
            goop: true,
            effects: true,
        },
    )
    .expect("failure details survive without decoded geometry");

    assert_eq!(preview.failed_models, 1);
    assert_eq!(preview.model_failures.len(), 1);
    assert!(preview.model_failures[0]
        .asset_path
        .contains("definitely-missing-preview-model.bmd"));
    assert!(preview.model_failures[0].error.contains("read asset"));
}

#[test]
fn renderer_validation_names_only_framebuffer_dependent_logic_materials() {
    let mut issues = Vec::new();
    append_gpu_blend_validation_issue(&mut issues, 7, "logic-xor", 2, 6);
    append_gpu_blend_validation_issue(&mut issues, 8, "logic-copy", 2, 3);

    assert_eq!(issues.len(), 1);
    assert_eq!(issues[0].code, "renderer-gx-logic-op-unsupported-7");
    assert!(issues[0].message.contains("logic-xor"));
    assert!(issues[0].message.contains("operation 6"));
}

#[test]
fn renderer_validation_reports_unsupported_texture_lod_flags() {
    let mut preview = preview_for_texture_alpha(true, true);
    preview.textures[0].do_edge_lod = true;
    preview.textures[0].bias_clamp = true;
    let issues = validation_issues_for_preview(&test_document(Vec::new()), Some(&preview));

    let issue = issues
        .iter()
        .find(|issue| issue.code == "renderer-gx-texture-lod-unsupported-0")
        .expect("LOD fidelity warning");
    assert!(issue.message.contains("edge LOD"));
    assert!(issue.message.contains("LOD bias clamp"));
}
