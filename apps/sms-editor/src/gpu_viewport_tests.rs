use super::*;

#[test]
fn j3d_vertex_layout_fits_webgpu_minimum_attribute_limit() {
    assert!(GpuVertex::ATTRIBUTES.len() <= 16);
}
use crate::{PreviewPoint, PreviewRenderLayer};
use sms_formats::{
    J3dColorChannel, J3dIndirectMaterial, J3dPreviewCombineMode, J3dTevOrder, J3dTexGen,
    SMS_MAP_MODEL_LOAD_FLAGS,
};

#[test]
fn material_uniform_is_uniform_buffer_aligned() {
    assert_eq!(std::mem::size_of::<GpuMaterialUniform>() % 16, 0);
    assert_eq!(std::mem::align_of::<GpuMaterialUniform>(), 4);
}

#[test]
fn j3d_shader_parses_and_validates() {
    let module = naga::front::wgsl::parse_str(J3D_SHADER).expect("J3D WGSL parses");
    naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::all(),
    )
    .validate(&module)
    .expect("J3D WGSL validates");
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
}

#[test]
fn geometry_updates_touch_only_requested_triangle_batches() {
    let mut preview = geometry_update_preview();
    let mut scene = GpuSceneData::from_preview(&preview);
    let static_location = scene.triangle_vertices[0].unwrap();
    let animated_location = scene.triangle_vertices[1].unwrap();
    let static_position =
        scene.batches[static_location.batch_index].vertices[static_location.vertex_offset].position;

    preview.triangles[1].vertices[0] = [50.0, 60.0, 70.0];
    let dirty = scene.update_geometry(&preview, &[0..0, 1..2]);

    assert_eq!(dirty, BTreeSet::from([animated_location.batch_index]));
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
    scene.update_geometry(&preview, std::slice::from_ref(&particle_range));

    let location = scene.triangle_vertices[1].unwrap();
    let vertex = scene.batches[location.batch_index].vertices[location.vertex_offset];
    assert_eq!(vertex.billboard_center_mode[3], 3.0);
    assert_eq!(&vertex.billboard_center_mode[..2], &[1.0, 2.0]);
    assert_eq!(vertex.billboard_axis_y, [7.0, 8.0, 9.0]);
    assert_eq!(vertex.uv0, [0.0, 0.7]);
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
        sky_radius: 0.0,
        loaded_models: 2,
        failed_models: 0,
        source_vertices: 6,
        source_triangles: 2,
        source_textures: 0,
        object_model_indices: BTreeMap::new(),
        animated_models: Vec::new(),
        rotating_models: Vec::new(),
        level_transform_models: Vec::new(),
        level_transform_particles: Vec::new(),
        actor_particles: Vec::new(),
        level_transform_duration_frames: 600.0,
        level_transform_particle_end_frames: 600.0,
    }
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
        coordinate_space_for_render_layer(PreviewRenderLayer::Particle),
        3
    );
    assert_eq!(
        coordinate_space_for_render_layer(PreviewRenderLayer::ParticleDistortion),
        4
    );
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
fn gx_source_color_blend_factors_do_not_change_with_blend_slot() {
    assert_eq!(gx_blend_factor(2), wgpu::BlendFactor::Src);
    assert_eq!(gx_blend_factor(3), wgpu::BlendFactor::OneMinusSrc);
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
    ];
    let order = sorted_gpu_draw_order_from_info(batches);
    assert_eq!(
        order,
        vec![
            GpuDrawCommand { batch_index: 1 },
            GpuDrawCommand { batch_index: 0 },
            GpuDrawCommand { batch_index: 3 },
            GpuDrawCommand { batch_index: 2 },
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
