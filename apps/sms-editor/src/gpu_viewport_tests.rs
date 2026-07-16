use super::*;

#[test]
fn j3d_vertex_layout_fits_webgpu_minimum_attribute_limit() {
    assert!(GpuVertex::ATTRIBUTES.len() <= 16);
}
use crate::{PreviewPoint, PreviewRenderLayer};
use sms_formats::{
    J3dBillboard, J3dBillboardMode, J3dColorChannel, J3dIndirectMaterial, J3dPreviewCombineMode,
    J3dTevOrder, J3dTexGen, SMS_MAP_MODEL_LOAD_FLAGS,
};

const TEST_RENDER_TARGET_SIZE: [u32; 2] = [640, 448];

#[test]
fn material_uniform_is_uniform_buffer_aligned() {
    assert_eq!(std::mem::size_of::<GpuMaterialUniform>() % 16, 0);
    assert_eq!(std::mem::align_of::<GpuMaterialUniform>(), 4);
}

#[test]
fn j3d_shader_parses_and_validates() {
    let module = wgpu::naga::front::wgsl::parse_str(J3D_SHADER).expect("J3D WGSL parses");
    wgpu::naga::valid::Validator::new(
        wgpu::naga::valid::ValidationFlags::all(),
        wgpu::naga::valid::Capabilities::empty(),
    )
    .validate(&module)
    .expect("J3D WGSL validates");
}

#[test]
fn camera_uniform_is_visible_to_fragment_lod_sampling() {
    assert!(camera_binding_visibility(0).contains(wgpu::ShaderStages::VERTEX));
    assert!(camera_binding_visibility(0).contains(wgpu::ShaderStages::FRAGMENT));
    assert_eq!(camera_binding_visibility(1), wgpu::ShaderStages::VERTEX);
}

#[test]
fn viewport_projection_has_no_far_clip_plane() {
    assert!(J3D_SHADER.contains("let clip_z = depth - camera.clip.x;"));
    assert!(!J3D_SHADER.contains("camera.clip.y - camera.clip.x"));
}

#[test]
fn j3d_vertex_shader_stops_after_the_material_texgen_count() {
    assert!(J3D_SHADER.contains("i >= material.counts.y"));
}

#[test]
fn sky_pass_cannot_occlude_distant_level_geometry() {
    let key = GpuMaterialState::from_j3d(&test_material(1)).pipeline_key(PreviewRenderLayer::Sky);

    assert_eq!(key.pass, GpuBatchPass::Sky);
    assert_eq!(
        key.depth,
        GpuDepthState {
            write: false,
            compare: GpuDepthCompare::Always,
        }
    );
}

#[test]
fn offscreen_cache_reuses_an_unchanged_static_frame() {
    let state =
        GpuOffscreenFrameState::new(GpuViewportFrame::default(), None, TEST_RENDER_TARGET_SIZE);

    assert!(offscreen_render_required(
        None,
        state,
        GpuOffscreenInvalidation::default()
    ));
    assert!(!offscreen_render_required(
        Some(state),
        state,
        GpuOffscreenInvalidation::default()
    ));
}

#[test]
fn offscreen_cache_tracks_camera_lighting_and_mirror_projection_state() {
    let frame = GpuViewportFrame::default();
    let state = GpuOffscreenFrameState::new(frame, None, TEST_RENDER_TARGET_SIZE);

    let mut moved_camera = frame;
    moved_camera.camera_position[0] = 25.0;
    assert!(offscreen_render_required(
        Some(state),
        GpuOffscreenFrameState::new(moved_camera, None, TEST_RENDER_TARGET_SIZE),
        GpuOffscreenInvalidation::default()
    ));

    let mut changed_light = frame;
    changed_light.light_color[1] = 0.25;
    assert!(offscreen_render_required(
        Some(state),
        GpuOffscreenFrameState::new(changed_light, None, TEST_RENDER_TARGET_SIZE),
        GpuOffscreenInvalidation::default()
    ));

    assert!(offscreen_render_required(
        Some(state),
        GpuOffscreenFrameState::new(frame, Some(48.0), TEST_RENDER_TARGET_SIZE),
        GpuOffscreenInvalidation::default()
    ));
}

#[test]
fn offscreen_cache_invalidates_for_scene_resource_and_animation_changes() {
    let state =
        GpuOffscreenFrameState::new(GpuViewportFrame::default(), None, TEST_RENDER_TARGET_SIZE);
    let invalidations = [
        GpuOffscreenInvalidation {
            target: true,
            ..Default::default()
        },
        GpuOffscreenInvalidation {
            scene: true,
            ..Default::default()
        },
        GpuOffscreenInvalidation {
            geometry: true,
            ..Default::default()
        },
        GpuOffscreenInvalidation {
            materials: true,
            ..Default::default()
        },
        GpuOffscreenInvalidation {
            time_animation: true,
            ..Default::default()
        },
    ];

    for invalidation in invalidations {
        assert!(offscreen_render_required(Some(state), state, invalidation));
    }
}

#[test]
fn offscreen_cache_ignores_time_for_static_materials() {
    let frame = GpuViewportFrame::default();
    let state = GpuOffscreenFrameState::new(frame, None, TEST_RENDER_TARGET_SIZE);
    let mut later = frame;
    later.animation_seconds = 120.0;
    let later_state = GpuOffscreenFrameState::new(later, None, TEST_RENDER_TARGET_SIZE);

    assert_eq!(state, later_state);
    assert!(!offscreen_render_required(
        Some(state),
        later_state,
        GpuOffscreenInvalidation::default()
    ));
    assert!(offscreen_render_required(
        Some(state),
        later_state,
        GpuOffscreenInvalidation {
            time_animation: true,
            ..Default::default()
        }
    ));
}

#[test]
fn color_texgen_uses_post_lighting_gx_channel_values() {
    assert!(J3D_SHADER.contains("source_value = vec4<f32>(channel0.rg, 0.0, 1.0)"));
    assert!(J3D_SHADER.contains("source_value = vec4<f32>(channel1.rg, 0.0, 1.0)"));
}

#[test]
fn specular_color_channel_uses_gx_light_two_mask() {
    assert!(J3D_SHADER.contains("(light_mask & 0x04u) != 0u"));
}

#[test]
fn unclamped_tev_outputs_wrap_to_u8_when_reused_by_coin_material() {
    fn regular(a: i32, b: i32, c: i32, d: i32, bias: i32, scale: i32) -> i32 {
        let a = a & 255;
        let b = b & 255;
        let c = c & 255;
        let lerp = (((a * 256 + (b - a) * (c + (c >> 7))) * scale) + 128) >> 8;
        ((d + bias) * scale + lerp).clamp(-1024, 1023)
    }

    // The first coin stage writes 0.5 * 4 to an unclamped signed register.
    // GX feeds only its low byte into C on the next stage, selecting the dark
    // base register. Treating 512 as a floating-point 2.0 instead saturates the
    // final coin color and is the visible brightness bug this guards against.
    let stage0 = regular(0, 0, 255, 0, 128, 4);
    assert_eq!(stage0, 512);
    assert_eq!(stage0 & 255, 0);
    assert_eq!(regular(173, 255, stage0, 0, 0, 1), 173);
    assert!(J3D_SHADER.contains("return tev_s10(value) & 255;"));
    assert!(J3D_SHADER.contains("f32(tev_input_u8(previous.r)) / 255.0"));
}

#[test]
fn geometry_updates_touch_only_requested_triangle_batches() {
    let mut preview = geometry_update_preview();
    let mut scene = GpuSceneData::from_preview(&preview);
    let static_location = scene.triangle_vertices[0].unwrap();
    let animated_location = scene.triangle_vertices[1].unwrap();
    let static_position =
        scene.batches[static_location.batch_index].vertices[static_location.vertex_offset].position;
    let mut dirty_vertex_ranges = vec![None; scene.batches.len()];

    preview.triangles[1].vertices[0] = [50.0, 60.0, 70.0];
    let changed = scene.update_geometry(&preview, &[0..0, 1..2], &mut dirty_vertex_ranges);

    assert!(changed);
    assert_eq!(
        dirty_vertex_ranges[animated_location.batch_index],
        Some(animated_location.vertex_offset..animated_location.vertex_offset + 3)
    );
    assert_eq!(
        scene.batches[animated_location.batch_index].vertices[animated_location.vertex_offset]
            .position,
        [50.0, 60.0, 70.0]
    );
    assert_eq!(
        scene.batches[static_location.batch_index].vertices[static_location.vertex_offset].position,
        static_position
    );
}

#[test]
fn geometry_updates_keep_dynamic_particle_shape_metadata() {
    let mut preview = geometry_update_preview();
    preview.triangles[1].particle_type = Some(3);
    preview.triangles[1].particle_pivot = Some([1.0, 2.0]);
    preview.triangles[1].particle_direction = Some([4.0, 5.0, 6.0]);
    preview.triangles[1].tex_coords = Some([[0.0, 0.1], [1.0, 0.1], [1.0, 0.2]]);
    let mut scene = GpuSceneData::from_preview(&preview);

    preview.triangles[1].particle_direction = Some([7.0, 8.0, 9.0]);
    preview.triangles[1].tex_coords = Some([[0.0, 0.7], [1.0, 0.7], [1.0, 0.8]]);
    let particle_range = 1..2;
    let mut dirty_vertex_ranges = vec![None; scene.batches.len()];
    scene.update_geometry(
        &preview,
        std::slice::from_ref(&particle_range),
        &mut dirty_vertex_ranges,
    );

    let location = scene.triangle_vertices[1].unwrap();
    let vertex = scene.batches[location.batch_index].vertices[location.vertex_offset];
    assert_eq!(vertex.billboard_center_mode[3], 3.0);
    assert_eq!(&vertex.billboard_center_mode[..2], &[1.0, 2.0]);
    assert_eq!(vertex.billboard_axis_y, [7.0, 8.0, 9.0]);
    assert_eq!(vertex.uv0, [0.0, 0.7]);
}

#[test]
fn geometry_only_updates_match_a_full_gpu_repack_byte_for_byte() {
    let mut preview = geometry_update_preview();
    let triangle = &mut preview.triangles[1];
    triangle.color = None;
    triangle.tex_coord_sets[0] = Some([[0.0, 0.0], [1.0, 0.0], [0.0, 1.0]]);
    triangle.tex_coord_sets[4] = Some([[0.2, 0.3], [0.4, 0.5], [0.6, 0.7]]);
    triangle.billboard = Some(J3dBillboard {
        mode: J3dBillboardMode::Full,
        center: [10.0, 20.0, 30.0],
        axes: [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]],
        offsets: [[-1.0, -2.0, 0.0], [1.0, -2.0, 0.0], [-1.0, 2.0, 0.0]],
        normals: Some([[0.0, 0.0, 1.0]; 3]),
    });
    let mut incrementally_updated = GpuSceneData::from_preview(&preview);

    let triangle = &mut preview.triangles[1];
    triangle.vertices = [[50.0, 60.0, 70.0], [54.0, 60.0, 70.0], [50.0, 66.0, 70.0]];
    triangle.billboard = Some(J3dBillboard {
        mode: J3dBillboardMode::YAxis,
        center: [51.0, 62.0, 73.0],
        axes: [[0.0, 0.0, -1.0], [0.0, 1.0, 0.0], [1.0, 0.0, 0.0]],
        offsets: [[-2.0, -3.0, 0.0], [2.0, -3.0, 0.0], [-2.0, 3.0, 0.0]],
        normals: Some([[1.0, 0.0, 0.0]; 3]),
    });
    let triangle_range = 1..2;
    let mut dirty_vertex_ranges = vec![None; incrementally_updated.batches.len()];
    incrementally_updated.update_geometry(
        &preview,
        std::slice::from_ref(&triangle_range),
        &mut dirty_vertex_ranges,
    );
    let fully_repacked = GpuSceneData::from_preview(&preview);

    assert_eq!(
        gpu_triangle_vertex_bytes(&incrementally_updated, 1),
        gpu_triangle_vertex_bytes(&fully_repacked, 1)
    );
}

#[test]
fn plain_mesh_updates_match_a_full_gpu_repack_byte_for_byte() {
    let mut preview = geometry_update_preview();
    let triangle = &mut preview.triangles[1];
    triangle.color_channels[0] = Some([[10, 20, 30, 40]; 3]);
    triangle.color_channels[1] = Some([[50, 60, 70, 80]; 3]);
    triangle.tex_coord_sets[0] = Some([[0.0, 0.0], [1.0, 0.0], [0.0, 1.0]]);
    let mut incrementally_updated = GpuSceneData::from_preview(&preview);

    let triangle = &mut preview.triangles[1];
    triangle.vertices = [[50.0, 60.0, 70.0], [54.0, 60.0, 70.0], [50.0, 66.0, 70.0]];
    triangle.normals = Some([[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]]);
    let triangle_range = 1..2;
    let mut dirty_vertex_ranges = vec![None; incrementally_updated.batches.len()];
    incrementally_updated.update_geometry(
        &preview,
        std::slice::from_ref(&triangle_range),
        &mut dirty_vertex_ranges,
    );
    let fully_repacked = GpuSceneData::from_preview(&preview);

    assert_eq!(
        gpu_triangle_vertex_bytes(&incrementally_updated, 1),
        gpu_triangle_vertex_bytes(&fully_repacked, 1)
    );
}

#[test]
fn particle_updates_match_a_full_gpu_repack_byte_for_byte() {
    let mut preview = geometry_update_preview();
    let triangle = &mut preview.triangles[1];
    triangle.render_layer = PreviewRenderLayer::Particle;
    triangle.particle_type = Some(3);
    triangle.particle_pivot = Some([1.0, 2.0]);
    triangle.particle_direction = Some([0.0, 1.0, 0.0]);
    triangle.color_channels[0] = Some([[10, 20, 30, 40]; 3]);
    triangle.color_channels[1] = Some([[50, 60, 70, 80]; 3]);
    triangle.tex_coord_sets[0] = Some([[0.0, 0.0], [1.0, 0.0], [0.0, 1.0]]);
    triangle.tex_coord_sets[3] = Some([[0.1, 0.2], [0.3, 0.4], [0.5, 0.6]]);
    let mut incrementally_updated = GpuSceneData::from_preview(&preview);

    let triangle = &mut preview.triangles[1];
    triangle.vertices = [
        [100.0, 200.0, 300.0],
        [110.0, 200.0, 300.0],
        [100.0, 210.0, 300.0],
    ];
    triangle.normals = Some([[4.0, 5.0, 6.0]; 3]);
    triangle.particle_type = Some(6);
    triangle.particle_pivot = Some([2.0, 0.0]);
    triangle.particle_direction = Some([7.0, 8.0, 9.0]);
    triangle.color_channels[0] = Some([[11, 21, 31, 41], [12, 22, 32, 42], [13, 23, 33, 43]]);
    triangle.color_channels[1] = Some([[51, 61, 71, 81], [52, 62, 72, 82], [53, 63, 73, 83]]);
    triangle.tex_coord_sets[0] = Some([[0.0, 0.7], [1.0, 0.7], [0.0, 0.8]]);
    triangle.tex_coord_sets[3] = Some([[0.2, 0.3], [0.4, 0.5], [0.6, 0.7]]);
    let triangle_range = 1..2;
    let mut dirty_vertex_ranges = vec![None; incrementally_updated.batches.len()];
    incrementally_updated.update_geometry(
        &preview,
        std::slice::from_ref(&triangle_range),
        &mut dirty_vertex_ranges,
    );
    let fully_repacked = GpuSceneData::from_preview(&preview);

    assert_eq!(
        gpu_triangle_vertex_bytes(&incrementally_updated, 1),
        gpu_triangle_vertex_bytes(&fully_repacked, 1)
    );
}

#[test]
fn geometry_updates_limit_uploads_to_the_dirty_vertex_span() {
    let mut preview = geometry_update_preview();
    preview.triangles[1].packet_index = preview.triangles[0].packet_index;
    let mut scene = GpuSceneData::from_preview(&preview);
    let first = scene.triangle_vertices[0].unwrap();
    let second = scene.triangle_vertices[1].unwrap();
    assert_eq!(first.batch_index, second.batch_index);
    let mut dirty_vertex_ranges = vec![None; scene.batches.len()];

    preview.triangles[1].vertices[0] = [50.0, 60.0, 70.0];
    let second_range = 1..2;
    scene.update_geometry(
        &preview,
        std::slice::from_ref(&second_range),
        &mut dirty_vertex_ranges,
    );

    let dirty = dirty_vertex_ranges[first.batch_index].clone().unwrap();
    assert_eq!(dirty, second.vertex_offset..second.vertex_offset + 3);
    assert_eq!(
        dirty.len() * std::mem::size_of::<GpuVertex>(),
        3 * std::mem::size_of::<GpuVertex>()
    );
    assert!(dirty.len() < scene.batches[first.batch_index].vertices.len());

    let first_range = 0..1;
    scene.update_geometry(
        &preview,
        std::slice::from_ref(&first_range),
        &mut dirty_vertex_ranges,
    );
    assert_eq!(
        dirty_vertex_ranges[first.batch_index],
        Some(first.vertex_offset..second.vertex_offset + 3)
    );
}

#[test]
fn geometry_updates_refresh_the_mirror_plane_height() {
    let mut preview = geometry_update_preview();
    preview.triangles[0].render_layer = PreviewRenderLayer::MirrorSurface;
    preview.mirror_model_slots.insert(1, 0);
    for vertex in &mut preview.triangles[0].vertices {
        vertex[1] = 12.0;
    }
    let mut scene = GpuSceneData::from_preview(&preview);
    assert_eq!(scene.mirror_plane_y_by_slot.get(&0), Some(&12.0));

    for vertex in &mut preview.triangles[0].vertices {
        vertex[1] = 48.0;
    }
    let mirror_range = 0..1;
    let mut dirty_vertex_ranges = vec![None; scene.batches.len()];
    scene.update_geometry(
        &preview,
        std::slice::from_ref(&mirror_range),
        &mut dirty_vertex_ranges,
    );

    assert_eq!(scene.mirror_plane_y_by_slot.get(&0), Some(&48.0));
}

#[test]
fn viewport_targets_are_requested_only_for_present_effect_passes() {
    let preview = geometry_update_preview();
    assert_eq!(
        GpuSceneData::from_preview(&preview).target_features(),
        ViewportTargetFeatures::default()
    );

    let mut preview = geometry_update_preview();
    preview.triangles[0].render_layer = PreviewRenderLayer::Heatwave;
    assert!(
        GpuSceneData::from_preview(&preview)
            .target_features()
            .screen_copy
    );

    preview.triangles[0].render_layer = PreviewRenderLayer::MirrorSurface;
    assert!(
        GpuSceneData::from_preview(&preview)
            .target_features()
            .mirror
    );

    preview.triangles[0].render_layer = PreviewRenderLayer::WaveFoam;
    let wave_without_water = GpuSceneData::from_preview(&preview).target_features();
    assert!(!wave_without_water.wave_mask);
    preview.triangles[1].render_layer = PreviewRenderLayer::Water;
    assert!(
        GpuSceneData::from_preview(&preview)
            .target_features()
            .wave_mask
    );
}

#[test]
fn static_vertex_indexing_preserves_the_exact_expanded_triangle_stream() {
    let static_preview = shared_static_quad_preview();
    let static_scene = GpuSceneData::from_preview(&static_preview);
    assert_eq!(static_scene.batches.len(), 1);
    let indexed_batch = &static_scene.batches[0];
    let indices = indexed_batch
        .indices
        .as_ref()
        .expect("a static quad should share its exact-bit corner vertices");
    assert!(!indexed_batch.updateable);
    assert_eq!(indexed_batch.vertices.len(), 4);
    assert_eq!(indices.len(), 6);
    assert!(static_scene.triangle_vertices.iter().all(Option::is_none));

    let mut updateable_preview = static_preview;
    updateable_preview
        .object_model_indices
        .insert("updateable-object".to_owned(), 1);
    let direct_scene = GpuSceneData::from_preview(&updateable_preview);
    assert_eq!(direct_scene.batches.len(), 1);
    let direct_batch = &direct_scene.batches[0];
    assert!(direct_batch.updateable);
    assert!(direct_batch.indices.is_none());
    assert_eq!(direct_batch.vertices.len(), 6);
    assert!(direct_scene.triangle_vertices.iter().all(Option::is_some));

    let expanded = indices
        .iter()
        .map(|index| indexed_batch.vertices[*index as usize])
        .collect::<Vec<_>>();
    assert_eq!(
        bytemuck::cast_slice::<GpuVertex, u8>(&expanded),
        bytemuck::cast_slice::<GpuVertex, u8>(&direct_batch.vertices)
    );
}

#[test]
fn intrinsically_dynamic_batches_remain_direct_triangle_lists() {
    let billboard = J3dBillboard {
        mode: J3dBillboardMode::Full,
        center: [0.5, 0.5, 0.0],
        axes: [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]],
        offsets: [[-0.5, -0.5, 0.0], [0.5, -0.5, 0.0], [0.5, 0.5, 0.0]],
        normals: Some([[0.0, 0.0, 1.0]; 3]),
    };
    let mut billboard_preview = shared_static_quad_preview();
    for triangle in &mut billboard_preview.triangles {
        triangle.billboard = Some(billboard);
    }

    let mut particle_preview = shared_static_quad_preview();
    for triangle in &mut particle_preview.triangles {
        triangle.render_layer = PreviewRenderLayer::Particle;
        triangle.particle_type = Some(0);
    }

    let mut wave_preview = shared_static_quad_preview();
    for triangle in &mut wave_preview.triangles {
        triangle.render_layer = PreviewRenderLayer::WaveFoam;
    }

    for preview in [billboard_preview, particle_preview, wave_preview] {
        let scene = GpuSceneData::from_preview(&preview);
        assert!(scene.batches.iter().all(|batch| batch.updateable));
        assert!(scene.batches.iter().all(|batch| batch.indices.is_none()));
        assert!(scene.triangle_vertices.iter().all(Option::is_some));
    }
}

#[test]
fn ranged_flag_and_rotating_batches_remain_direct_triangle_lists() {
    let mut flag_preview = shared_static_quad_preview();
    flag_preview
        .animated_flags
        .push(crate::AnimatedFlagPreview {
            transform: crate::Transform::default(),
            rows: 2,
            cols: 2,
            step_height: 1.0,
            step_width: 1.0,
            flag_height: 1.0,
            flag_width: 1.0,
            segment_size: 1.0,
            initial_phase_degrees: 0.0,
            flutter_speed_degrees_per_frame: 1.0,
            phase_wrap_degrees: 360.0,
            point_range: 0..0,
            triangle_range: 0..2,
        });

    let mut rotating_preview = shared_static_quad_preview();
    rotating_preview
        .rotating_models
        .push(crate::RuntimeRotatingModelPreview {
            positions: Arc::new(Vec::new()),
            triangles: Arc::new(Vec::new()),
            instances: vec![crate::AnimatedModelInstance {
                transform: crate::Transform::default(),
                model_index: 1,
                point_range: 0..0,
                point_stride: 1,
                triangle_range: 0..2,
                accessories: Vec::new(),
                runtime_yaw_degrees_per_frame: 1.0,
            }],
        });

    for preview in [flag_preview, rotating_preview] {
        let scene = GpuSceneData::from_preview(&preview);
        assert!(scene.batches.iter().all(|batch| batch.updateable));
        assert!(scene.batches.iter().all(|batch| batch.indices.is_none()));
        assert!(scene.triangle_vertices.iter().all(Option::is_some));
    }
}

#[test]
fn bounded_cache_eviction_keeps_separately_active_values_alive() {
    let active = Arc::new(String::from("active"));
    let active_values = [active.clone()];
    let mut cache = BTreeMap::new();
    insert_bounded_cache(&mut cache, 2, 0, active.clone());
    insert_bounded_cache(&mut cache, 2, 1, Arc::new(String::from("cached")));

    insert_bounded_cache(&mut cache, 2, 1, Arc::new(String::from("replacement")));
    assert_eq!(cache.len(), 2, "replacing a key must not flush the cache");
    insert_bounded_cache(&mut cache, 2, 2, Arc::new(String::from("new")));

    assert_eq!(cache.keys().copied().collect::<Vec<_>>(), vec![2]);
    assert!(Arc::ptr_eq(&active_values[0], &active));
    assert_eq!(active_values[0].as_str(), "active");
}

#[test]
fn mirror_scene_includes_sky_and_actor_models_but_not_level_water() {
    let preview = geometry_update_preview();
    let level = preview.triangles[0];
    let actor = preview.triangles[1];

    assert!(!triangle_is_mirror_visible(&level, None));
    assert!(triangle_is_mirror_visible(&actor, Some([0.0; 3])));

    let mut sky = level;
    sky.render_layer = PreviewRenderLayer::Sky;
    assert!(triangle_is_mirror_visible(&sky, None));

    let mut water = actor;
    water.render_layer = PreviewRenderLayer::Water;
    assert!(!triangle_is_mirror_visible(&water, Some([0.0; 3])));
}

#[test]
fn mirror_actor_submission_matches_runtime_cube_and_plane_checks() {
    let position = [100.0, 950.0, 200.0];

    assert!(mirror_batch_is_visible(
        PreviewRenderLayer::Main,
        Some(position),
        Some(0),
        Some(0),
        Some(1000.0),
    ));
    assert!(!mirror_batch_is_visible(
        PreviewRenderLayer::Main,
        Some([100.0, 949.0, 200.0]),
        Some(0),
        Some(0),
        Some(1000.0),
    ));
    assert!(!mirror_batch_is_visible(
        PreviewRenderLayer::Main,
        Some(position),
        Some(1),
        Some(0),
        Some(1000.0),
    ));
    assert!(!mirror_batch_is_visible(
        PreviewRenderLayer::Main,
        Some(position),
        None,
        Some(0),
        Some(1000.0),
    ));

    assert!(mirror_batch_is_visible(
        PreviewRenderLayer::MirrorScene,
        None,
        None,
        Some(0),
        Some(1000.0),
    ));
    assert!(mirror_batch_is_visible(
        PreviewRenderLayer::Sky,
        None,
        None,
        Some(0),
        Some(1000.0),
    ));
}

#[test]
fn gpu_scene_tracks_actor_root_for_mirror_submission() {
    let mut preview = geometry_update_preview();
    preview.triangles[0].render_layer = PreviewRenderLayer::MirrorSurface;
    preview.mirror_model_slots.insert(1, 0);
    preview.mirror_cubes.push(crate::PreviewMirrorCube {
        center: [0.0, -500.0, 0.0],
        rotation_degrees: [0.0; 3],
        dimensions: [2000.0, 2000.0, 2000.0],
        model_slot: 0,
    });
    let actor_model_index = preview.triangles[1].model_index;
    preview
        .mirror_actor_positions
        .insert(actor_model_index, [0.0, -100.0, 0.0]);

    let scene = GpuSceneData::from_preview(&preview);
    let actor_batch = scene
        .batches
        .iter()
        .find(|batch| batch.mirror_actor_model_index == Some(actor_model_index))
        .expect("actor mirror batch");
    assert_eq!(actor_batch.mirror_actor_slot, Some(0));
    assert!(!mirror_batch_is_visible(
        actor_batch.render_layer,
        actor_batch.mirror_actor_position,
        actor_batch.mirror_actor_slot,
        Some(0),
        Some(0.0),
    ));
}

#[test]
fn mirror_surface_batches_require_the_matching_active_cube_slot() {
    assert!(!render_layer_is_visible_for_active_mirror(
        PreviewRenderLayer::MirrorSurface,
        Some(0),
        None,
    ));
    assert!(!render_layer_is_visible_for_active_mirror(
        PreviewRenderLayer::MirrorSurface,
        Some(0),
        Some(1),
    ));
    assert!(render_layer_is_visible_for_active_mirror(
        PreviewRenderLayer::MirrorSurface,
        Some(0),
        Some(0),
    ));
    assert!(!render_layer_is_visible_for_active_mirror(
        PreviewRenderLayer::MirrorSurface,
        None,
        None,
    ));
    assert!(render_layer_is_visible_for_active_mirror(
        PreviewRenderLayer::Main,
        None,
        None,
    ));
}

fn geometry_update_preview() -> ModelPreview {
    let triangle = |packet_index, x| PreviewTriangle {
        vertices: [[x, 0.0, 0.0], [x + 1.0, 0.0, 0.0], [x, 1.0, 0.0]],
        normals: Some([[0.0, 0.0, 1.0]; 3]),
        color_channels: [None; 2],
        tex_coord_sets: [None; 8],
        material_index: None,
        packet_index,
        model_index: packet_index + 1,
        render_layer: PreviewRenderLayer::Main,
        color: Some([255; 4]),
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
    };
    ModelPreview {
        points: Vec::<PreviewPoint>::new(),
        triangles: vec![triangle(0, 0.0), triangle(1, 10.0)],
        textures: Vec::new(),
        materials: Vec::new(),
        texture_srt_animations: Vec::new(),
        texture_pattern_animations: Vec::new(),
        material_animation_bindings: Vec::new(),
        bounds_min: [0.0; 3],
        bounds_max: [11.0, 1.0, 0.0],
        camera_bounds_min: [0.0; 3],
        camera_bounds_max: [11.0, 1.0, 0.0],
        loaded_models: 2,
        failed_models: 0,
        model_failures: Vec::new(),
        source_vertices: 6,
        source_triangles: 2,
        source_textures: 0,
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

fn shared_static_quad_preview() -> ModelPreview {
    let mut preview = geometry_update_preview();
    let triangles = &mut preview.triangles;
    triangles[0].packet_index = 0;
    triangles[0].model_index = 1;
    triangles[0].vertices = [[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [1.0, 1.0, 0.0]];
    triangles[1].packet_index = 0;
    triangles[1].model_index = 1;
    triangles[1].vertices = [[0.0, 0.0, 0.0], [1.0, 1.0, 0.0], [0.0, 1.0, 0.0]];
    preview
}

fn gpu_triangle_vertex_bytes(scene: &GpuSceneData, triangle_index: usize) -> &[u8] {
    let location = scene.triangle_vertices[triangle_index].unwrap();
    bytemuck::cast_slice(
        &scene.batches[location.batch_index].vertices
            [location.vertex_offset..location.vertex_offset + 3],
    )
}

#[test]
fn j3d_new_texture_matrix_modes_put_translation_in_q_column() {
    let mut matrix = test_tex_matrix(6);
    let old_rows = texture_srt_rows(matrix);
    matrix.mode = 7;
    let new_rows = texture_srt_rows(matrix);

    assert_eq!(old_rows[0][2], 0.0);
    assert_ne!(old_rows[0][3], 0.0);
    assert_eq!(new_rows[0][2], old_rows[0][3]);
    assert_eq!(new_rows[0][3], 0.0);
}

#[test]
fn specialized_material_modes_resolve_before_gpu_state() {
    let opaque = test_material(1);
    let alpha_edge = test_material(2);
    let translucent = test_material(4);
    assert_eq!(
        GpuMaterialState::from_j3d(&opaque)
            .pipeline_key(PreviewRenderLayer::Main)
            .pass,
        GpuBatchPass::Opaque
    );
    assert_eq!(
        GpuMaterialState::from_j3d(&alpha_edge)
            .pipeline_key(PreviewRenderLayer::Main)
            .pass,
        GpuBatchPass::AlphaTest
    );
    assert_eq!(
        GpuMaterialState::from_j3d(&translucent)
            .pipeline_key(PreviewRenderLayer::Main)
            .pass,
        GpuBatchPass::Translucent
    );
}

#[test]
fn render_layers_select_their_runtime_coordinate_space() {
    assert_eq!(
        coordinate_space_for_render_layer(PreviewRenderLayer::Sky),
        1
    );
    assert_eq!(
        coordinate_space_for_render_layer(PreviewRenderLayer::Heatwave),
        2
    );
    assert_eq!(
        coordinate_space_for_render_layer(PreviewRenderLayer::Main),
        0
    );
    assert_eq!(
        coordinate_space_for_render_layer(PreviewRenderLayer::Water),
        0
    );
    assert_eq!(
        coordinate_space_for_render_layer(PreviewRenderLayer::WaveFoam),
        7
    );
    assert_eq!(
        coordinate_space_for_render_layer(PreviewRenderLayer::IndirectWater),
        6
    );
    assert_eq!(
        coordinate_space_for_render_layer(PreviewRenderLayer::MirrorSurface),
        5
    );
    assert_eq!(
        coordinate_space_for_render_layer(PreviewRenderLayer::Particle),
        3
    );
    assert_eq!(
        coordinate_space_for_render_layer(PreviewRenderLayer::ParticleDistortion),
        4
    );
}

#[test]
fn mirror_camera_reflects_across_surface_and_widens_fov() {
    let frame = GpuViewportFrame {
        camera_position: [10.0, 20.0, 30.0],
        right: [1.0, 0.0, 0.0],
        up: [0.0, 1.0, 0.0],
        forward: [0.0, 0.0, 1.0],
        focal: 500.0,
        viewport_size: [800.0, 600.0],
        ..Default::default()
    };

    let mirrored = mirror_viewport_frame(frame, Some(100.0));

    assert_eq!(mirrored.camera_position, [10.0, 180.0, 30.0]);
    assert_eq!(mirrored.right, [-1.0, 0.0, -0.0]);
    assert_eq!(mirrored.up, [0.0, -1.0, 0.0]);
    assert_eq!(mirrored.forward, [0.0, -0.0, 1.0]);
    assert!(mirrored.focal < frame.focal);
}

#[test]
fn mirror_projection_preserves_q_until_fragment_sampling() {
    assert!(J3D_SHADER.contains("@location(12) mirror_coord: vec3<f32>"));
    assert!(J3D_SHADER.contains("return input.mirror_coord.xy / q;"));
    assert!(!J3D_SHADER.contains("projected_world_uv"));
}

#[test]
fn mirror_sample_uses_sunshines_rgb5a3_copy_precision() {
    assert!(J3D_SHADER.contains("fn rgb5a3_copy_value"));
    assert!(J3D_SHADER.contains("tex = rgb5a3_copy_value(tex);"));
    assert!(J3D_SHADER.contains("if (rgba8.a >= 224.0)"));
}

#[test]
fn heatwave_materials_render_after_the_efb_snapshot_boundary() {
    assert_eq!(
        GpuMaterialState::from_j3d(&test_material(4))
            .pipeline_key(PreviewRenderLayer::Heatwave)
            .pass,
        GpuBatchPass::Heatwave
    );
}

#[test]
fn sea_indirect_materials_render_after_the_efb_snapshot_boundary() {
    assert_eq!(
        GpuMaterialState::from_j3d(&test_material(4))
            .pipeline_key(PreviewRenderLayer::IndirectWater)
            .pass,
        GpuBatchPass::Heatwave
    );
}

#[test]
fn wave_and_particles_follow_the_indirect_water_phase() {
    let material = GpuMaterialState::from_j3d(&test_material(4));

    assert_eq!(
        material.pipeline_key(PreviewRenderLayer::WaveFoam).pass,
        GpuBatchPass::WaveFoam
    );
    assert_eq!(
        material.pipeline_key(PreviewRenderLayer::Particle).pass,
        GpuBatchPass::Particle
    );
    assert_eq!(
        material
            .pipeline_key(PreviewRenderLayer::ParticleDistortion)
            .pass,
        GpuBatchPass::Particle
    );
}

#[test]
fn only_authored_screen_effect_layers_use_the_efb_copy() {
    assert!(render_layer_uses_efb_copy(PreviewRenderLayer::Heatwave));
    assert!(render_layer_uses_efb_copy(
        PreviewRenderLayer::IndirectWater
    ));
    assert!(render_layer_uses_efb_copy(
        PreviewRenderLayer::ParticleDistortion
    ));
    assert!(!render_layer_uses_efb_copy(PreviewRenderLayer::WaveFoam));
    assert!(!render_layer_uses_efb_copy(PreviewRenderLayer::Particle));
}

#[test]
fn heatwave_offsets_use_sunshines_half_resolution_screen_texture() {
    let mut preview = geometry_update_preview();
    preview.triangles[0].render_layer = PreviewRenderLayer::Heatwave;
    let scene = GpuSceneData::from_preview(&preview);
    let batch = scene
        .batches
        .iter()
        .find(|batch| batch.pipeline_key.pass == GpuBatchPass::Heatwave)
        .expect("heatwave batch");

    assert_eq!(
        scene.materials[batch.material_index].uniform.texture_sizes[1],
        [320.0, 224.0, 1.0 / 320.0, 1.0 / 224.0]
    );
}

#[test]
fn sea_indirect_offsets_use_sunshines_half_resolution_screen_texture() {
    let mut preview = geometry_update_preview();
    preview.triangles[0].render_layer = PreviewRenderLayer::IndirectWater;
    let scene = GpuSceneData::from_preview(&preview);
    let batch = scene
        .batches
        .iter()
        .find(|batch| batch.render_layer == PreviewRenderLayer::IndirectWater)
        .expect("SeaIndirect batch");

    assert_eq!(batch.pipeline_key.pass, GpuBatchPass::Heatwave);
    assert_eq!(
        scene.materials[batch.material_index].uniform.texture_sizes[1],
        [320.0, 224.0, 1.0 / 320.0, 1.0 / 224.0]
    );
}

#[test]
fn sea_indirect_uses_the_live_screen_projection() {
    assert!(J3D_SHADER
        .contains("(input.coordinate_space == 2u || input.coordinate_space == 6u) && index == 1u"));
    assert!(
        J3D_SHADER.contains("return input.position.xy / vec2<f32>(textureDimensions(texture1));")
    );
}

#[test]
fn runtime_wave_uses_camera_centered_displacement_and_retail_scroll_rates() {
    assert!(J3D_SHADER.contains("input.coordinate_space == 7u"));
    assert!(J3D_SHADER.contains("world_position.x += camera.camera_position.x"));
    assert!(J3D_SHADER.contains("world_position.z += camera.camera_position.z"));
    assert!(J3D_SHADER.contains("0.6 * seconds"));
    assert!(J3D_SHADER.contains("0.9 * seconds"));
    assert!(J3D_SHADER.contains("seconds * 0.045"));
    assert!(J3D_SHADER.contains("textureSampleLevel(texture1, sampler1, mask_uv, 0.0)"));
    assert!(J3D_SHADER.contains("fn fs_wave_mask"));
}

#[test]
fn runtime_wave_mask_uses_authored_water_geometry() {
    assert!(wave_mask_source_layer(PreviewRenderLayer::Water));
    assert!(!wave_mask_source_layer(PreviewRenderLayer::Main));
    assert!(!wave_mask_source_layer(PreviewRenderLayer::IndirectWater));
    assert!(!wave_mask_source_layer(PreviewRenderLayer::WaveFoam));
}

#[test]
fn runtime_wave_batch_updates_its_animation_clock() {
    assert_eq!(
        GpuMaterialState::from_j3d(&test_material(4))
            .pipeline_key(PreviewRenderLayer::WaveFoam)
            .pass,
        GpuBatchPass::WaveFoam
    );

    let mut preview = geometry_update_preview();
    preview.triangles[0].render_layer = PreviewRenderLayer::WaveFoam;
    let scene = GpuSceneData::from_preview(&preview);
    let batch = scene
        .batches
        .iter()
        .find(|batch| batch.render_layer == PreviewRenderLayer::WaveFoam)
        .expect("runtime wave batch");
    let material = &scene.materials[batch.material_index];

    assert!(material.runtime_wave);
    assert_eq!(material.uniform_at_time(2.5).runtime_parameters[0], 2.5);
}

#[test]
fn material_refresh_reapplies_batch_derived_runtime_state() {
    let mut preview = geometry_update_preview();
    preview.materials = vec![test_material(4)];
    preview.triangles[0].material_index = Some(0);
    preview.triangles[0].render_layer = PreviewRenderLayer::WaveFoam;
    preview.triangles[1].material_index = Some(0);
    preview.triangles[1].render_layer = PreviewRenderLayer::Heatwave;
    let mut scene = GpuSceneData::from_preview(&preview);

    assert!(scene.materials[0].runtime_wave);
    assert_eq!(
        scene.materials[0].uniform.texture_sizes[1][0..2],
        [320.0, 224.0]
    );

    scene.replace_material_from_preview(0, &preview.materials[0], &preview);

    assert!(scene.materials[0].runtime_wave);
    assert_eq!(
        scene.materials[0].uniform.texture_sizes[1][0..2],
        [320.0, 224.0]
    );
}

fn lod_preview_texture() -> PreviewTexture {
    let mip = |size| egui::ColorImage::filled([size, size], egui::Color32::WHITE);
    PreviewTexture {
        image: mip(8),
        mips: vec![mip(8), mip(4), mip(2)],
        format: 0,
        wrap_s: 1,
        wrap_t: 1,
        min_filter: 5,
        mag_filter: 1,
        mipmap_enabled: true,
        do_edge_lod: false,
        bias_clamp: false,
        max_anisotropy: 0,
        min_lod: 1.0,
        max_lod: 4.0,
        lod_bias: 2.0,
        mipmap_count: 3,
        has_alpha: true,
        has_translucent_alpha: true,
    }
}

#[test]
fn gpu_sampler_preserves_authored_lod_range_and_gx_mip_filter() {
    let texture = lod_preview_texture();
    let data = GpuTextureData::from_preview_texture(&texture);

    assert_eq!(data.mips.len(), 3);
    assert_eq!(data.lod_min_clamp, 1.0);
    assert_eq!(data.lod_max_clamp, 2.0);
    assert_eq!(data.mipmap_filter, wgpu::MipmapFilterMode::Linear);
    assert_eq!(
        sampler_mipmap_filter(3, true),
        wgpu::MipmapFilterMode::Nearest
    );
    assert_eq!(
        sampler_mipmap_filter(4, true),
        wgpu::MipmapFilterMode::Linear
    );
}

#[test]
fn gpu_sampler_preserves_gx_anisotropy_when_webgpu_filters_are_compatible() {
    let mut texture = lod_preview_texture();
    texture.max_anisotropy = 2;
    let data = GpuTextureData::from_preview_texture(&texture);

    assert_eq!(data.anisotropy_clamp, 4);
    assert!(authored_sampler_anisotropy_is_supported(&texture));

    texture.min_filter = 3;
    let data = GpuTextureData::from_preview_texture(&texture);
    assert_eq!(data.anisotropy_clamp, 1);
    assert!(!authored_sampler_anisotropy_is_supported(&texture));
}

#[test]
fn disabled_mipmaps_upload_and_sample_only_the_base_level() {
    let mut texture = lod_preview_texture();
    texture.mipmap_enabled = false;
    let data = GpuTextureData::from_preview_texture(&texture);

    assert_eq!(data.mips.len(), 1);
    assert_eq!(data.lod_min_clamp, 0.0);
    assert_eq!(data.lod_max_clamp, 0.0);
}

#[test]
fn material_uniform_carries_texture_lod_bias_and_header_flags() {
    let mut texture = lod_preview_texture();
    texture.do_edge_lod = true;
    texture.bias_clamp = true;
    texture.max_anisotropy = 2;

    assert_eq!(texture_lod_uniform(&texture), [2.0, 1.0, 4.0, 519.0]);
    assert_eq!(gx_lod_bias(0.8), 0.78125);
    assert_eq!(gx_lod_bias(2.14), 2.125);
    assert_eq!(gx_lod_bias(-0.5), -0.5);
}

#[test]
fn gx_lod_derivatives_use_the_physical_target_and_authored_bias() {
    let uniform = GpuCameraUniform::from_frame(GpuViewportFrame::default(), [1280, 896]);

    assert_eq!(uniform.render_target_size[0..2], [1280.0, 896.0]);
    assert!(J3D_SHADER.contains("camera.render_target_size.x / 640.0"));
    assert!(J3D_SHADER.contains("camera.render_target_size.y / 448.0"));
    assert!(J3D_SHADER.contains("exp2(material.texture_lod_parameters[slot].x)"));
    assert!(J3D_SHADER.contains("sample_texture_level_zero"));
    assert!(J3D_SHADER.contains("let lod_uv = select(uv, original_uv, use_original_lod);"));
}

#[test]
fn gx_texture_channels_are_sampled_as_numeric_values() {
    for format in [0, 1, 2, 3, 4, 5, 6, 8, 9, 10, 14] {
        assert_eq!(
            gpu_texture_format_for_j3d(format),
            wgpu::TextureFormat::Rgba8Unorm
        );
    }
}

#[test]
fn gx_composite_only_decodes_for_an_srgb_surface() {
    assert_eq!(
        composite_fragment_entry(wgpu::TextureFormat::Bgra8UnormSrgb),
        "fs_srgb_target"
    );
    assert_eq!(
        composite_fragment_entry(wgpu::TextureFormat::Bgra8Unorm),
        "fs_unorm_target"
    );
}

#[test]
fn gx_composite_matches_eframe_depth_attachment_without_writing_it() {
    let depth = composite_depth_stencil_state();
    assert_eq!(depth.format, wgpu::TextureFormat::Depth24Plus);
    assert_eq!(depth.depth_write_enabled, Some(false));
    assert_eq!(depth.depth_compare, Some(wgpu::CompareFunction::Always));
}

#[test]
fn gx_color_blend_aliases_follow_their_source_and_destination_slots() {
    assert_eq!(gx_source_blend_factor(2), wgpu::BlendFactor::Dst);
    assert_eq!(gx_source_blend_factor(3), wgpu::BlendFactor::OneMinusDst);
    assert_eq!(gx_destination_blend_factor(2), wgpu::BlendFactor::Src);
    assert_eq!(
        gx_destination_blend_factor(3),
        wgpu::BlendFactor::OneMinusSrc
    );
}

#[test]
fn gx_subtract_blend_subtracts_source_from_framebuffer() {
    let blend = gpu_blend_state(GpuBlendKey {
        mode: 3,
        src_factor: 4,
        dst_factor: 5,
        logic_op: 3,
    })
    .expect("GX subtract enables blending");

    assert_eq!(blend.color.src_factor, wgpu::BlendFactor::One);
    assert_eq!(blend.color.dst_factor, wgpu::BlendFactor::One);
    assert_eq!(blend.color.operation, wgpu::BlendOperation::ReverseSubtract);
    assert_eq!(blend.alpha.operation, wgpu::BlendOperation::ReverseSubtract);
}

#[test]
fn gx_logic_operations_use_an_explicit_overwrite_fallback() {
    assert!(gpu_blend_state(GpuBlendKey {
        mode: 2,
        src_factor: 4,
        dst_factor: 5,
        logic_op: 6,
    })
    .is_none());
}

#[test]
fn gx_noop_logic_operation_preserves_framebuffer_color() {
    let blend = gpu_blend_state(GpuBlendKey {
        mode: 2,
        src_factor: 0,
        dst_factor: 0,
        logic_op: 5,
    })
    .expect("GX_LO_NOOP uses a zero-source/one-destination blend");

    assert_eq!(blend.color.src_factor, wgpu::BlendFactor::Zero);
    assert_eq!(blend.color.dst_factor, wgpu::BlendFactor::One);
    assert_eq!(blend.alpha.src_factor, wgpu::BlendFactor::Zero);
    assert_eq!(blend.alpha.dst_factor, wgpu::BlendFactor::One);
    assert!(J3D_SHADER.contains("case 0u: { return vec4<f32>(0.0); }"));
    assert!(J3D_SHADER.contains("case 12u: { return vec4<f32>(1.0) - color; }"));
    assert!(J3D_SHADER.contains("case 15u: { return vec4<f32>(1.0); }"));
}

#[test]
fn material_uniform_carries_logic_mode_for_source_transforms() {
    let mut material = test_material(1);
    material.blend_mode.mode = 2;
    material.blend_mode.logic_op = 12;
    let uniform = GpuMaterialUniform::from_j3d(&material);

    assert_eq!(uniform.runtime_parameters[1], 2.0);
    assert_eq!(uniform.runtime_parameters[2], 12.0);
}

#[test]
fn pipeline_blend_keys_drop_fields_unused_by_the_active_mode() {
    assert_eq!(
        GpuBlendKey {
            mode: 3,
            src_factor: 4,
            dst_factor: 5,
            logic_op: 6,
        }
        .canonical(),
        GpuBlendKey {
            mode: 3,
            src_factor: 0,
            dst_factor: 0,
            logic_op: 0,
        }
    );
    assert_eq!(
        GpuBlendKey {
            mode: 2,
            src_factor: 4,
            dst_factor: 5,
            logic_op: 6,
        }
        .canonical(),
        GpuBlendKey {
            mode: 2,
            src_factor: 0,
            dst_factor: 0,
            logic_op: 6,
        }
    );
}

#[test]
fn disabled_depth_compare_maps_to_always() {
    let mut material = test_material(1);
    material.z_mode.compare_enable = 0;
    let key = GpuMaterialState::from_j3d(&material).pipeline_key(PreviewRenderLayer::Main);
    assert_eq!(key.depth.compare, GpuDepthCompare::Always);
}

#[test]
fn j3d_draw_mode_controls_opaque_and_translucent_buffers() {
    let mut opaque_with_blending = test_material(1);
    opaque_with_blending.blend_mode.mode = 1;
    assert_eq!(
        GpuMaterialState::from_j3d(&opaque_with_blending)
            .pipeline_key(PreviewRenderLayer::Main)
            .pass,
        GpuBatchPass::Opaque
    );

    let translucent = test_material(4);
    assert_eq!(
        GpuMaterialState::from_j3d(&translucent)
            .pipeline_key(PreviewRenderLayer::Main)
            .pass,
        GpuBatchPass::Translucent
    );
}

#[test]
fn packet_sort_matches_j3d_material_buffers_without_camera_resorting() {
    let solid_key = test_pipeline_key(GpuBatchPass::Opaque);
    let translucent_key = test_pipeline_key(GpuBatchPass::Translucent);
    let batches = vec![
        GpuDrawBatchInfo {
            batch_index: 0,
            pass: solid_key.pass,
            material_index: 2,
            packet_index: 2,
        },
        GpuDrawBatchInfo {
            batch_index: 1,
            pass: solid_key.pass,
            material_index: 1,
            packet_index: 1,
        },
        GpuDrawBatchInfo {
            batch_index: 2,
            pass: translucent_key.pass,
            material_index: 4,
            packet_index: 4,
        },
        GpuDrawBatchInfo {
            batch_index: 3,
            pass: translucent_key.pass,
            material_index: 3,
            packet_index: 3,
        },
        GpuDrawBatchInfo {
            batch_index: 6,
            pass: GpuBatchPass::Particle,
            material_index: 1,
            packet_index: 1,
        },
        GpuDrawBatchInfo {
            batch_index: 5,
            pass: GpuBatchPass::WaveFoam,
            material_index: 1,
            packet_index: 1,
        },
        GpuDrawBatchInfo {
            batch_index: 4,
            pass: GpuBatchPass::Heatwave,
            material_index: 1,
            packet_index: 1,
        },
    ];
    let order = sorted_gpu_draw_order_from_info(batches);
    assert_eq!(
        order,
        vec![
            GpuDrawCommand { batch_index: 1 },
            GpuDrawCommand { batch_index: 0 },
            GpuDrawCommand { batch_index: 3 },
            GpuDrawCommand { batch_index: 2 },
            GpuDrawCommand { batch_index: 4 },
            GpuDrawCommand { batch_index: 5 },
            GpuDrawCommand { batch_index: 6 },
        ]
    );
}

fn test_material(mode: u8) -> J3dMaterial {
    let (alpha_compare, blend_mode, z_mode) = match mode {
        2 => (
            J3dAlphaCompare {
                comp0: 6,
                ref0: 128,
                op: 0,
                comp1: 3,
                ref1: 255,
            },
            J3dBlendMode {
                mode: 0,
                src_factor: 1,
                dst_factor: 0,
                logic_op: 3,
            },
            default_z_mode(),
        ),
        4 => (
            always_alpha_compare(),
            J3dBlendMode {
                mode: 1,
                src_factor: 4,
                dst_factor: 5,
                logic_op: 3,
            },
            J3dZMode {
                compare_enable: 1,
                func: 3,
                update_enable: 0,
            },
        ),
        _ => (
            always_alpha_compare(),
            J3dBlendMode {
                mode: 0,
                src_factor: 1,
                dst_factor: 0,
                logic_op: 3,
            },
            default_z_mode(),
        ),
    };
    J3dMaterial {
        name: String::new(),
        material_index: 0,
        material_id: 0,
        loader_flags: SMS_MAP_MODEL_LOAD_FLAGS,
        lighting_enabled: false,
        mode,
        cull_mode: 2,
        color_channel_count: 1,
        material_colors: [[255; 4]; 2],
        ambient_colors: [[50; 4]; 2],
        color_channels: [J3dColorChannel::default(); 4],
        tex_gen_count: 1,
        tex_gens: std::array::from_fn(|slot| J3dTexGen {
            gen_type: 1,
            source: 4 + slot as u8,
            matrix: 60,
        }),
        tex_matrices: [None; 8],
        texture_indices: [None; 8],
        tev_colors: [[0; 4]; 4],
        tev_k_colors: [[255; 4]; 4],
        tev_stages: vec![J3dTevStage {
            order: J3dTevOrder {
                tex_coord: None,
                tex_map: None,
                color_channel: 4,
            },
            color_args: [15, 15, 15, 10],
            color_op: 0,
            color_bias: 0,
            color_scale: 0,
            color_clamp: 1,
            color_register: 0,
            alpha_args: [7, 7, 7, 5],
            alpha_op: 0,
            alpha_bias: 0,
            alpha_scale: 0,
            alpha_clamp: 1,
            alpha_register: 0,
            konst_color: 12,
            konst_alpha: 28,
            raster_swap: 0,
            texture_swap: 0,
            indirect: Default::default(),
        }],
        swap_tables: [[0, 1, 2, 3]; 4],
        indirect: J3dIndirectMaterial::default(),
        fog: None,
        alpha_compare,
        blend_mode,
        z_mode,
        z_comp_loc: 1,
        dither: 0,
    }
}

fn test_tex_matrix(mode: u8) -> J3dTexMatrix {
    J3dTexMatrix {
        projection: 1,
        mode,
        maya: false,
        center: [0.5, 0.5, 0.0],
        scale: [1.0, 1.0],
        rotation: 0,
        translation: [0.25, 0.5],
        effect_matrix: [
            [1.0, 0.0, 0.0, 0.0],
            [0.0, 1.0, 0.0, 0.0],
            [0.0, 0.0, 1.0, 0.0],
            [0.0, 0.0, 0.0, 1.0],
        ],
    }
}

fn test_pipeline_key(pass: GpuBatchPass) -> GpuPipelineKey {
    GpuPipelineKey {
        pass,
        depth: GpuDepthState {
            write: pass != GpuBatchPass::Translucent,
            compare: GpuDepthCompare::LessEqual,
        },
        cull: GpuCullMode::Back,
        blend: GpuBlendKey {
            mode: u8::from(pass == GpuBatchPass::Translucent),
            src_factor: 4,
            dst_factor: 5,
            logic_op: 3,
        },
    }
}
