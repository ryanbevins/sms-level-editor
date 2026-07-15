use super::*;

#[test]
fn runtime_joint_children_reverse_inf1_sibling_order() {
    let parents = [None, Some(0), Some(1), Some(1), Some(1)];

    assert_eq!(runtime_child_joint_index(&parents, 1, 0), Some(4));
    assert_eq!(runtime_child_joint_index(&parents, 1, 1), Some(3));
    assert_eq!(runtime_child_joint_index(&parents, 1, 2), Some(2));
}

#[test]
fn map_buildings_use_the_first_runtime_root_child() {
    let parents = [None, Some(0), Some(0), Some(2), Some(2)];

    assert_eq!(map_building_joint_from_parents(&parents, 0), Some(4));
    assert_eq!(map_building_joint_from_parents(&parents, 1), Some(3));
}

#[test]
fn joint_subtree_membership_follows_parent_chain() {
    let parents = [None, Some(0), Some(1), Some(1), Some(3)];

    assert!(joint_is_in_subtree(4, 1, &parents));
    assert!(joint_is_in_subtree(3, 3, &parents));
    assert!(!joint_is_in_subtree(2, 3, &parents));
}

#[test]
fn reads_cull_modes_as_big_endian_gx_enums() {
    let table = [
        0, 0, 0, 0, // GX_CULL_NONE
        0, 0, 0, 2, // GX_CULL_BACK
        0, 0, 0, 1, // GX_CULL_FRONT
    ];

    assert_eq!(read_indexed_cull_mode(&table, Some(0), 0), Some(0));
    assert_eq!(read_indexed_cull_mode(&table, Some(0), 1), Some(2));
    assert_eq!(read_indexed_cull_mode(&table, Some(0), 2), Some(1));
    assert_eq!(read_indexed_cull_mode(&table, Some(0), 0xFF), None);
}

#[test]
fn parses_section_table_and_preserves_bytes() {
    let mut bytes = vec![0; 0x20];
    bytes[0..4].copy_from_slice(b"J3D2");
    bytes[4..8].copy_from_slice(b"bmd3");
    bytes[8..12].copy_from_slice(&(0x28u32.to_be_bytes()));
    bytes[12..16].copy_from_slice(&(1u32.to_be_bytes()));
    bytes.extend_from_slice(b"INF1");
    bytes.extend_from_slice(&(8u32.to_be_bytes()));

    let file = J3dFile::parse(&bytes).unwrap();
    assert_eq!(file.header().file_type, "bmd3");
    assert_eq!(file.sections()[0].tag, "INF1");
    assert_eq!(file.to_bytes(), bytes);
}

#[test]
fn accepts_retail_texture_only_bmt_with_overreported_block_count() {
    let mut bytes = vec![0u8; 0x28];
    bytes[0..4].copy_from_slice(b"J3D2");
    bytes[4..8].copy_from_slice(b"bmt3");
    bytes[8..12].copy_from_slice(&0x28u32.to_be_bytes());
    bytes[12..16].copy_from_slice(&2u32.to_be_bytes());
    bytes[0x20..0x24].copy_from_slice(b"TEX1");
    bytes[0x24..0x28].copy_from_slice(&8u32.to_be_bytes());

    let file = J3dFile::parse(&bytes).expect("retail texture-only BMT layout");
    assert_eq!(file.header().section_count, 2);
    assert_eq!(file.sections().len(), 1);
    assert_eq!(file.sections()[0].tag, "TEX1");
}

#[test]
fn still_rejects_overreported_bmd_block_count() {
    let mut bytes = vec![0u8; 0x28];
    bytes[0..4].copy_from_slice(b"J3D2");
    bytes[4..8].copy_from_slice(b"bmd3");
    bytes[8..12].copy_from_slice(&0x28u32.to_be_bytes());
    bytes[12..16].copy_from_slice(&2u32.to_be_bytes());
    bytes[0x20..0x24].copy_from_slice(b"TEX1");
    bytes[0x24..0x28].copy_from_slice(&8u32.to_be_bytes());

    assert!(J3dFile::parse(&bytes).is_err());
}

#[test]
fn shape_matrix_palette_inherits_ffff_slots_from_previous_packet() {
    let mut palette = Vec::new();
    assert_eq!(
        resolve_shape_matrix_palette(&[4, 7, 9], &mut palette),
        [4, 7, 9]
    );
    assert_eq!(
        resolve_shape_matrix_palette(&[0xFFFF, 12, 0xFFFF], &mut palette),
        [4, 12, 9]
    );
}

#[test]
fn billboard_metadata_preserves_joint_center_and_scaled_local_offsets() {
    let matrix = [
        [0.0, 0.0, 4.0, 10.0],
        [0.0, 3.0, 0.0, 20.0],
        [-2.0, 0.0, 0.0, 30.0],
    ];
    let vertices = [[10.0, 20.0, 30.0], [10.0, 23.0, 28.0], [14.0, 20.0, 30.0]];
    let billboard = billboard_for_triangle(vertices, None, matrix, 1).unwrap();

    assert_eq!(billboard.mode, J3dBillboardMode::Full);
    assert_eq!(billboard.center, [10.0, 20.0, 30.0]);
    assert_eq!(billboard.offsets[0], [0.0, 0.0, 0.0]);
    assert_eq!(billboard.offsets[1], [2.0, 3.0, 0.0]);
    assert_eq!(billboard.offsets[2], [0.0, 0.0, 4.0]);
}

#[test]
fn shape_type_two_is_y_axis_billboard() {
    let identity = [
        [1.0, 0.0, 0.0, 0.0],
        [0.0, 1.0, 0.0, 0.0],
        [0.0, 0.0, 1.0, 0.0],
    ];
    let billboard = billboard_for_triangle([[0.0; 3]; 3], None, identity, 2).unwrap();

    assert_eq!(billboard.mode, J3dBillboardMode::YAxis);
}

#[test]
fn extracts_f32_vertex_preview() {
    let mut bytes = vec![0; 0x20];
    bytes[0..4].copy_from_slice(b"J3D2");
    bytes[4..8].copy_from_slice(b"bmd3");
    bytes[12..16].copy_from_slice(&(2u32.to_be_bytes()));

    let inf_offset = bytes.len();
    bytes.extend_from_slice(b"INF1");
    bytes.extend_from_slice(&(0x18u32.to_be_bytes()));
    bytes.extend_from_slice(&[0; 8]);
    bytes.extend_from_slice(&(2u32.to_be_bytes()));
    bytes.extend_from_slice(&[0; 4]);

    let vtx_offset = bytes.len();
    bytes.extend_from_slice(b"VTX1");
    bytes.extend_from_slice(&(0x80u32.to_be_bytes()));
    bytes.extend_from_slice(&(0x40u32.to_be_bytes()));
    bytes.extend_from_slice(&(0x60u32.to_be_bytes()));
    bytes.resize(vtx_offset + 0x40, 0);
    bytes.extend_from_slice(&GX_VA_POS.to_be_bytes());
    bytes.extend_from_slice(&(1u32.to_be_bytes()));
    bytes.extend_from_slice(&GX_F32.to_be_bytes());
    bytes.extend_from_slice(&(0u32.to_be_bytes()));
    bytes.extend_from_slice(&GX_VA_NULL.to_be_bytes());
    bytes.extend_from_slice(&[0; 12]);
    bytes.resize(vtx_offset + 0x60, 0);
    for value in [1.0f32, 2.0, 3.0, -2.0, 4.0, 8.0] {
        bytes.extend_from_slice(&value.to_bits().to_be_bytes());
    }
    bytes.resize(vtx_offset + 0x80, 0);
    let file_size = bytes.len() as u32;
    bytes[8..12].copy_from_slice(&file_size.to_be_bytes());

    assert_eq!(inf_offset, 0x20);
    let file = J3dFile::parse(&bytes).unwrap();
    let preview = file.vertex_preview().unwrap();
    assert_eq!(preview.positions.len(), 2);
    assert_eq!(preview.bounds_min, [-2.0, 2.0, 3.0]);
    assert_eq!(preview.bounds_max, [1.0, 4.0, 8.0]);
}

#[test]
fn recognizes_texture_konst_modulate_tev_args() {
    assert!(tev_args_are_texture_konst_modulate(TevColorArgs {
        a: GX_CC_ZERO,
        b: GX_CC_TEXC,
        c: GX_CC_KONST,
        d: GX_CC_ZERO,
    }));
    assert!(tev_args_are_texture_konst_modulate(TevColorArgs {
        a: GX_CC_ZERO,
        b: GX_CC_KONST,
        c: GX_CC_TEXC,
        d: GX_CC_ZERO,
    }));
    assert!(!tev_args_are_texture_konst_modulate(TevColorArgs {
        a: GX_CC_ZERO,
        b: GX_CC_TEXC,
        c: GX_CC_RASC,
        d: GX_CC_ZERO,
    }));
    assert!(!tev_args_are_texture_konst_modulate(TevColorArgs {
        a: GX_CC_TEXC,
        b: GX_CC_RASC,
        c: GX_CC_KONST,
        d: GX_CC_C0,
    }));
}

#[test]
fn recognizes_texture_raster_konst_blend_tev_args() {
    assert!(tev_args_are_texture_raster_konst_blend(TevColorArgs {
        a: GX_CC_TEXC,
        b: GX_CC_RASC,
        c: GX_CC_KONST,
        d: GX_CC_C0,
    }));
    assert!(tev_args_are_texture_raster_konst_blend(TevColorArgs {
        a: GX_CC_TEXRRR,
        b: GX_CC_RASC,
        c: GX_CC_KONST,
        d: GX_CC_C0,
    }));
    assert!(!tev_args_are_texture_raster_konst_blend(TevColorArgs {
        a: GX_CC_ZERO,
        b: GX_CC_TEXC,
        c: GX_CC_KONST,
        d: GX_CC_ZERO,
    }));
    assert!(!tev_args_are_texture_raster_konst_blend(TevColorArgs {
        a: GX_CC_TEXC,
        b: GX_CC_RASC,
        c: GX_CC_KONST,
        d: GX_CC_ZERO,
    }));
}

#[test]
fn recognizes_previous_texture_modulate_tev_args() {
    assert!(tev_args_are_previous_texture_modulate(TevColorArgs {
        a: GX_CC_ZERO,
        b: GX_CC_CPREV,
        c: GX_CC_TEXC,
        d: GX_CC_ZERO,
    }));
    assert!(tev_args_are_previous_texture_modulate(TevColorArgs {
        a: GX_CC_ZERO,
        b: GX_CC_TEXC,
        c: GX_CC_CPREV,
        d: GX_CC_ZERO,
    }));
    assert!(!tev_args_are_previous_texture_modulate(TevColorArgs {
        a: GX_CC_ZERO,
        b: GX_CC_TEXC,
        c: GX_CC_KONST,
        d: GX_CC_ZERO,
    }));
}

#[test]
fn recognizes_texture_konst_modulate_alpha_args() {
    const GX_CA_RASA: u8 = 5;

    assert!(tev_alpha_args_are_texture_konst_modulate(TevAlphaArgs {
        a: GX_CA_ZERO,
        b: GX_CA_TEXA,
        c: GX_CA_KONST,
        d: GX_CA_ZERO,
    }));
    assert!(!tev_alpha_args_are_texture_konst_modulate(TevAlphaArgs {
        a: GX_CA_ZERO,
        b: GX_CA_TEXA,
        c: GX_CA_RASA,
        d: GX_CA_ZERO,
    }));
}

#[test]
fn texture_raster_konst_preview_tint_blends_toward_material() {
    assert_eq!(texture_raster_konst_blend_preview_channel(128, 160), 175);
    assert_eq!(texture_raster_konst_blend_preview_channel(255, 160), 255);
    assert_eq!(texture_raster_konst_blend_preview_channel(0, 255), 0);
}

#[test]
fn generated_position_texcoords_keep_textured_triangles_visible() {
    let mut triangles = Vec::new();
    let a = primitive_vertex([10.0, 2.0, 30.0]);
    let b = primitive_vertex([20.0, 2.0, 30.0]);
    let c = primitive_vertex([10.0, 2.0, 50.0]);

    push_triangle(
        &mut triangles,
        a,
        b,
        c,
        None,
        J3dMaterialRenderState::default(),
        Some(MaterialPreviewBinding {
            texture_index: Some(3),
            tex_coord_source: TexCoordPreviewSource::Position,
            tex_mtx: None,
            mask_texture_index: None,
            mask_tex_coord_source: TexCoordPreviewSource::Vertex(0),
            mask_tex_mtx: None,
            combine_mode: J3dPreviewCombineMode::TextureOnly,
            tint_color: None,
        }),
    );

    assert_eq!(triangles[0].texture_index, Some(3));
    assert_eq!(
        triangles[0].tex_coords,
        Some([[10.0, 30.0], [20.0, 30.0], [10.0, 50.0]])
    );
}

#[test]
fn generated_normal_texcoords_use_face_normal_preview() {
    let mut triangles = Vec::new();

    push_triangle(
        &mut triangles,
        primitive_vertex([0.0, 0.0, 0.0]),
        primitive_vertex([1.0, 0.0, 0.0]),
        primitive_vertex([0.0, 1.0, 0.0]),
        None,
        J3dMaterialRenderState::default(),
        Some(MaterialPreviewBinding {
            texture_index: Some(4),
            tex_coord_source: TexCoordPreviewSource::Normal,
            tex_mtx: None,
            mask_texture_index: None,
            mask_tex_coord_source: TexCoordPreviewSource::Vertex(0),
            mask_tex_mtx: None,
            combine_mode: J3dPreviewCombineMode::TextureOnly,
            tint_color: None,
        }),
    );

    assert_eq!(triangles[0].texture_index, Some(4));
    assert_eq!(
        triangles[0].tex_coords,
        Some([[0.5, 0.5], [0.5, 0.5], [0.5, 0.5]])
    );
}

#[test]
fn generated_normal_texcoords_prefer_vertex_normals() {
    let mut triangles = Vec::new();
    let mut a = primitive_vertex([0.0, 0.0, 0.0]);
    let mut b = primitive_vertex([1.0, 0.0, 0.0]);
    let mut c = primitive_vertex([0.0, 1.0, 0.0]);
    a.normal = Some([1.0, 0.0, 0.0]);
    b.normal = Some([0.0, 1.0, 0.0]);
    c.normal = Some([0.0, 0.0, 1.0]);

    push_triangle(
        &mut triangles,
        a,
        b,
        c,
        None,
        J3dMaterialRenderState::default(),
        Some(MaterialPreviewBinding {
            texture_index: Some(4),
            tex_coord_source: TexCoordPreviewSource::Normal,
            tex_mtx: None,
            mask_texture_index: None,
            mask_tex_coord_source: TexCoordPreviewSource::Vertex(0),
            mask_tex_mtx: None,
            combine_mode: J3dPreviewCombineMode::TextureOnly,
            tint_color: None,
        }),
    );

    assert_eq!(
        triangles[0].tex_coords,
        Some([[1.0, 0.5], [0.5, 1.0], [0.5, 0.5]])
    );
}

fn primitive_vertex(position: [f32; 3]) -> PrimitiveVertex {
    PrimitiveVertex {
        position,
        normal: None,
        colors: [None; 2],
        tex_coords: [None; TEX_COORD_COUNT],
    }
}

#[test]
fn reads_material_tev_color_registers() {
    let mut bytes = vec![0; 0x120];
    bytes[0xDC..0xDE].copy_from_slice(&0u16.to_be_bytes());
    bytes[0xDE..0xE0].copy_from_slice(&1u16.to_be_bytes());
    bytes[0xE0..0xE2].copy_from_slice(&0xFFFFu16.to_be_bytes());
    bytes[0xE2..0xE4].copy_from_slice(&0xFFFFu16.to_be_bytes());
    bytes[0x100..0x102].copy_from_slice(&(-12i16).to_be_bytes());
    bytes[0x102..0x104].copy_from_slice(&34i16.to_be_bytes());
    bytes[0x104..0x106].copy_from_slice(&255i16.to_be_bytes());
    bytes[0x106..0x108].copy_from_slice(&300i16.to_be_bytes());
    bytes[0x108..0x10A].copy_from_slice(&10i16.to_be_bytes());
    bytes[0x10A..0x10C].copy_from_slice(&20i16.to_be_bytes());
    bytes[0x10C..0x10E].copy_from_slice(&30i16.to_be_bytes());
    bytes[0x10E..0x110].copy_from_slice(&40i16.to_be_bytes());

    let colors = material_tev_colors(&bytes, 0, Some(0x100));

    assert_eq!(colors[0], [-12, 34, 255, 300]);
    assert_eq!(colors[1], [10, 20, 30, 40]);
    assert_eq!(colors[2], [0, 0, 0, 0]);
}

#[test]
fn textureless_tev_stage_blends_registers_like_blue_coin() {
    let mut bytes = vec![0; 0x120];
    bytes[0xE4..0xE6].copy_from_slice(&0u16.to_be_bytes());
    let stage = 0x100;
    bytes[stage + 1] = GX_CC_C1;
    bytes[stage + 2] = GX_CC_C0;
    bytes[stage + 3] = GX_CC_CPREV;
    bytes[stage + 4] = GX_CC_RASC;
    bytes[stage + 0x0A] = GX_CA_ZERO;
    bytes[stage + 0x0B] = GX_CA_ZERO;
    bytes[stage + 0x0C] = GX_CA_ZERO;
    bytes[stage + 0x0D] = GX_CA_APREV;

    let tev_colors = [
        [16, 89, 255, 255],
        [20, 20, 141, 255],
        [105, 93, 178, 255],
        [0, 0, 0, 0],
    ];
    let color = material_tev_stage_preview_color(
        &bytes,
        0,
        Some(stage),
        0,
        tev_colors,
        [[255, 255, 255, 255]; 4],
        Some([255, 255, 255, 50]),
        Some([128, 128, 128, 50]),
        RasterColorSource::Material,
    )
    .unwrap();

    assert_eq!(color, [144, 217, 255, 50]);
}

#[test]
fn texture_tint_usefulness_keeps_alpha_only_tints() {
    assert!(!preview_tint_color_is_useful([255, 255, 255, 255]));
    assert!(preview_tint_color_is_useful([255, 255, 255, 128]));
    assert!(preview_tint_color_is_useful([0, 0, 0, 255]));
}

#[test]
fn konst_alpha_selector_reads_k_color_channels() {
    let colors = [
        [10, 20, 30, 40],
        [50, 60, 70, 80],
        [90, 100, 110, 120],
        [130, 140, 150, 160],
    ];
    assert_eq!(tev_konst_alpha_for_selector(0x00, colors), Some(255));
    assert_eq!(tev_konst_alpha_for_selector(0x10, colors), Some(10));
    assert_eq!(tev_konst_alpha_for_selector(0x15, colors), Some(60));
    assert_eq!(tev_konst_alpha_for_selector(0x1A, colors), Some(110));
    assert_eq!(tev_konst_alpha_for_selector(0x1F, colors), Some(160));
}

#[test]
fn intensity_textures_replicate_intensity_into_alpha() {
    let mut i4 = vec![0; 8 * 8 * 4];
    i4[0] = 0xA3;
    let mut i4_rgba = vec![0; 8 * 8 * 4];
    decode_i4(&i4, 0, 8, 8, &mut i4_rgba).unwrap();
    assert_eq!(&i4_rgba[0..4], &[170, 170, 170, 170]);
    assert_eq!(&i4_rgba[4..8], &[51, 51, 51, 51]);

    let mut i8 = vec![0; 8 * 4];
    i8[0] = 73;
    let mut i8_rgba = vec![0; 8 * 4 * 4];
    decode_i8(&i8, 0, 8, 4, &mut i8_rgba).unwrap();
    assert_eq!(&i8_rgba[0..4], &[73, 73, 73, 73]);
}

#[test]
fn intensity_alpha_textures_store_alpha_before_intensity() {
    let mut ia4_rgba = vec![0; 8 * 4 * 4];
    let mut ia4 = vec![0; 8 * 4];
    ia4[0] = 0xA3;
    decode_ia4(&ia4, 0, 8, 4, &mut ia4_rgba).unwrap();
    assert_eq!(&ia4_rgba[0..4], &[51, 51, 51, 170]);

    let mut ia8_rgba = vec![0; 4 * 4 * 4];
    let mut ia8 = vec![0; 4 * 4 * 2];
    ia8[0] = 201;
    ia8[1] = 42;
    decode_ia8(&ia8, 0, 4, 4, &mut ia8_rgba).unwrap();
    assert_eq!(&ia8_rgba[0..4], &[42, 42, 42, 201]);
}

#[test]
fn timg_preview_preserves_authored_gx_lod_state() {
    let mut bytes = vec![0u8; 0x20 + 3 * 32];
    bytes[0] = GX_TF_I4;
    bytes[2..4].copy_from_slice(&8u16.to_be_bytes());
    bytes[4..6].copy_from_slice(&8u16.to_be_bytes());
    bytes[0x10] = 1;
    bytes[0x11] = 1;
    bytes[0x12] = 1;
    bytes[0x13] = 2;
    bytes[0x14] = 5;
    bytes[0x15] = 1;
    bytes[0x16] = (-8i8) as u8;
    bytes[0x17] = 16;
    bytes[0x18] = 3;
    bytes[0x1A..0x1C].copy_from_slice(&(-50i16).to_be_bytes());
    bytes[0x1C..0x20].copy_from_slice(&0x20u32.to_be_bytes());

    let texture = decode_bti_texture(bytes).expect("synthetic TIMG");

    assert!(texture.mipmap_enabled);
    assert!(texture.do_edge_lod);
    assert!(texture.bias_clamp);
    assert_eq!(texture.max_anisotropy, 2);
    assert_eq!(texture.min_lod, -1.0);
    assert_eq!(texture.max_lod, 2.0);
    assert_eq!(texture.lod_bias, -0.5);
    assert_eq!(texture.mipmap_count, 3);
    assert_eq!(texture.mips.len(), 3);
}

#[test]
fn softimage_matrix_path_keeps_scale_out_of_child_rotation() {
    let transforms = [
        JointPreviewTransform {
            scale_compensate: false,
            scale: [2.0, 1.0, 1.0],
            rotation: [0, 0, 0],
            translation: [0.0; 3],
        },
        JointPreviewTransform {
            scale_compensate: false,
            scale: [1.0; 3],
            rotation: [0, 0, 0x4000],
            translation: [0.0; 3],
        },
    ];
    let parents = [None, Some(0)];

    let basic = basic_joint_matrices(&transforms, &parents);
    let softimage = softimage_joint_matrices(&transforms, &parents);
    let point = [1.0, 0.0, 0.0];

    assert_vec3_close(transform_mtx34_point(basic[1], point), [0.0, 1.0, 0.0]);
    assert_vec3_close(transform_mtx34_point(softimage[1], point), [0.0, 2.0, 0.0]);
}

#[test]
fn maya_matrix_path_applies_parent_scale_compensation() {
    let transforms = [
        JointPreviewTransform {
            scale_compensate: false,
            scale: [2.0; 3],
            rotation: [0; 3],
            translation: [0.0; 3],
        },
        JointPreviewTransform {
            scale_compensate: true,
            scale: [1.0; 3],
            rotation: [0; 3],
            translation: [2.0, 0.0, 0.0],
        },
    ];
    let matrices = maya_joint_matrices(&transforms, &[None, Some(0)]);

    assert_vec3_close(
        transform_mtx34_point(matrices[1], [1.0, 0.0, 0.0]),
        [5.0, 0.0, 0.0],
    );
}

#[test]
fn normal_matrices_use_inverse_transpose_for_nonuniform_scale() {
    let matrix = [
        [2.0, 0.0, 0.0, 0.0],
        [0.0, 1.0, 0.0, 0.0],
        [0.0, 0.0, 1.0, 0.0],
    ];
    let normal = transform_mtx34_normal(matrix, [1.0, 1.0, 0.0]).unwrap();
    let inverse_length = 1.0 / 1.25f32.sqrt();
    assert_vec3_close(normal, [0.5 * inverse_length, inverse_length, 0.0]);
}

#[test]
fn envelope_matrices_accumulate_weighted_joint_transforms() {
    let mut weighted = [[0.0; 4]; 3];
    let mut translated = identity_mtx34();
    translated[0][3] = 8.0;
    add_weighted_mtx34(&mut weighted, identity_mtx34(), 0.25);
    add_weighted_mtx34(&mut weighted, translated, 0.75);

    assert_vec3_close(
        transform_mtx34_point(weighted, [2.0, 0.0, 0.0]),
        [8.0, 0.0, 0.0],
    );
}

#[test]
fn precomputed_joint_matrices_preserve_direct_draw_matrix_selection() {
    let mut bytes = vec![0; 0x28];
    bytes[0x08..0x0A].copy_from_slice(&1u16.to_be_bytes());
    bytes[0x0C..0x10].copy_from_slice(&0x20u32.to_be_bytes());
    bytes[0x10..0x14].copy_from_slice(&0x22u32.to_be_bytes());
    bytes[0x20] = 0;
    bytes[0x22..0x24].copy_from_slice(&1u16.to_be_bytes());
    let file = J3dFile {
        header: J3dHeader {
            file_type: "bmd3".to_string(),
            file_size: bytes.len() as u32,
            section_count: 1,
        },
        sections: vec![J3dSection {
            tag: "DRW1".to_string(),
            offset: 0,
            size: bytes.len() as u32,
        }],
        bytes: bytes.into(),
    };
    let identity = identity_mtx34();
    let mut translated = identity;
    translated[0][3] = 42.0;

    let draw_matrices = file
        .preview_draw_matrices_from_joint_matrices(&[identity, translated])
        .unwrap();

    assert_eq!(draw_matrices, vec![Some(translated)]);
}

#[test]
fn draw_matrix_refactor_preserves_joint_only_models() {
    let file = J3dFile {
        header: J3dHeader {
            file_type: "bmd3".to_string(),
            file_size: 0,
            section_count: 0,
        },
        sections: Vec::new(),
        bytes: Arc::from([]),
    };
    let joint_matrices = file.preview_joint_matrices(0, None, &[]).unwrap();

    assert_eq!(
        file.preview_draw_matrices(0, None, &[]).unwrap(),
        file.preview_draw_matrices_from_joint_matrices(&joint_matrices)
            .unwrap()
    );
}

#[test]
fn prepared_source_guard_survives_j3d_file_clone() {
    let mut bytes = vec![0; 0x20];
    bytes[0..4].copy_from_slice(b"J3D2");
    bytes[4..8].copy_from_slice(b"bmd3");
    bytes[8..12].copy_from_slice(&0x28u32.to_be_bytes());
    bytes[12..16].copy_from_slice(&1u32.to_be_bytes());
    bytes.extend_from_slice(b"INF1");
    bytes.extend_from_slice(&8u32.to_be_bytes());
    let file = J3dFile::parse(&bytes).unwrap();
    let prepared = J3dPreparedAnimatedTriangles {
        source: PreparedModelSource(Arc::clone(&file.bytes)),
        packets: Vec::new(),
        source_triangle_count: 0,
        max_packet_vertex_count: 0,
    };

    let cloned = file.clone();
    assert!(Arc::ptr_eq(&file.bytes, &cloned.bytes));
    assert!(cloned.prepared_animation_source_matches(&prepared));

    let independently_parsed = J3dFile::parse(&bytes).unwrap();
    assert!(!independently_parsed.prepared_animation_source_matches(&prepared));
}

#[test]
fn prepared_display_lists_match_legacy_pose_and_topology_exactly() {
    let descs = [
        VertexDesc {
            attr: GX_VA_PNMTXIDX,
            attr_type: GX_DIRECT,
        },
        VertexDesc {
            attr: GX_VA_POS,
            attr_type: GX_DIRECT,
        },
        VertexDesc {
            attr: GX_VA_NRM,
            attr_type: GX_DIRECT,
        },
    ];
    let attr_formats = [
        AttributeFormat {
            attr: GX_VA_POS,
            cnt: GX_POS_XYZ,
            component_type: GX_F32,
            frac: 0,
        },
        AttributeFormat {
            attr: GX_VA_NRM,
            cnt: GX_NRM_XYZ,
            component_type: GX_F32,
            frac: 0,
        },
    ];
    let position_format = PositionFormat {
        component_type: GX_F32,
        frac: 0,
    };
    let vertex_arrays = VertexArrays {
        normal_offset: None,
        normal_format: Some(NormalFormat {
            component_type: GX_F32,
            frac: 0,
            components: 3,
        }),
        color_offsets: [None; 2],
        color_formats: [None; 2],
        tex_offsets: [None; TEX_COORD_COUNT],
        tex_formats: [None; TEX_COORD_COUNT],
    };
    let group_matrices = [0, 1];
    let mut display_list = Vec::new();
    display_list.push(GX_DRAW_TRIANGLE_STRIP);
    display_list.extend_from_slice(&4u16.to_be_bytes());
    push_direct_test_vertex(&mut display_list, 0, [0.0, 0.0, 0.0], [1.0, 1.0, 0.0]);
    push_direct_test_vertex(&mut display_list, 0, [1.0, 0.0, 0.0], [1.0, 1.0, 0.0]);
    push_direct_test_vertex(&mut display_list, 3, [0.0, 1.0, 0.0], [0.0, 1.0, 1.0]);
    push_direct_test_vertex(&mut display_list, 3, [1.0, 1.0, 0.0], [0.0, 1.0, 1.0]);
    display_list.push(GX_DRAW_QUADS);
    display_list.extend_from_slice(&4u16.to_be_bytes());
    for (matrix_slot, position) in [
        (0, [2.0, 0.0, 0.0]),
        (3, [3.0, 0.0, 0.0]),
        (3, [3.0, 1.0, 0.0]),
        (0, [2.0, 1.0, 0.0]),
    ] {
        push_direct_test_vertex(&mut display_list, matrix_slot, position, [0.0, 0.0, 1.0]);
    }
    display_list.push(0);

    let prepared_display_list = decode_prepared_display_list(
        &display_list,
        &[],
        &[],
        &descs,
        &attr_formats,
        position_format,
        vertex_arrays,
        &group_matrices,
    )
    .unwrap();
    let source_triangle_count = prepared_display_list.triangle_indices.len();
    let max_packet_vertex_count = prepared_display_list.vertices.len();
    let prepared = J3dPreparedAnimatedTriangles {
        source: PreparedModelSource(Arc::from([])),
        packets: vec![PreparedAnimatedPacket {
            vertices: prepared_display_list.vertices,
            triangle_indices: prepared_display_list.triangle_indices,
            shape_index: 0,
            packet_index: 0,
            billboard: None,
        }],
        source_triangle_count,
        max_packet_vertex_count,
    };
    assert_eq!(prepared.source_triangle_count(), 4);

    let mut translated = identity_mtx34();
    translated[0][3] = 4.0;
    translated[1][3] = -2.0;
    let nonuniform = [
        [2.0, 0.0, 0.0, 1.0],
        [0.0, 0.5, 0.0, 2.0],
        [0.0, 0.0, 3.0, 3.0],
    ];
    let collapsed = [[0.0; 4]; 3];
    for draw_matrices in [
        vec![Some(identity_mtx34()), Some(identity_mtx34())],
        vec![Some(identity_mtx34()), Some(translated)],
        vec![Some(nonuniform), Some(translated)],
        vec![None, Some(nonuniform)],
        vec![Some(collapsed), Some(collapsed)],
    ] {
        let legacy = decode_display_list(
            &display_list,
            &[],
            &[],
            &descs,
            &attr_formats,
            position_format,
            vertex_arrays,
            &group_matrices,
            &draw_matrices,
            None,
            J3dMaterialRenderState::default(),
            None,
        )
        .unwrap();

        assert_eq!(prepared.pose(&draw_matrices), legacy);
    }
}

fn push_direct_test_vertex(
    bytes: &mut Vec<u8>,
    matrix_slot: u8,
    position: [f32; 3],
    normal: [f32; 3],
) {
    bytes.push(matrix_slot);
    for component in position.into_iter().chain(normal) {
        bytes.extend_from_slice(&component.to_bits().to_be_bytes());
    }
}

#[test]
fn compact_pe_blocks_use_material_mode_presets() {
    let explicit = (
        J3dAlphaCompare {
            comp0: 0,
            ref0: 7,
            op: 0,
            comp1: 0,
            ref1: 9,
        },
        J3dBlendMode {
            mode: 0,
            src_factor: 1,
            dst_factor: 0,
            logic_op: 3,
        },
        J3dZMode {
            compare_enable: 0,
            func: 7,
            update_enable: 1,
        },
        0,
    );
    let compact = resolve_pe_state(
        4,
        false,
        Some(explicit.0),
        Some(explicit.1),
        Some(explicit.2),
        Some(explicit.3),
    );
    let full = resolve_pe_state(
        4,
        true,
        Some(explicit.0),
        Some(explicit.1),
        Some(explicit.2),
        Some(explicit.3),
    );

    assert_eq!(compact.1.mode, 1);
    assert_eq!(compact.2.update_enable, 0);
    assert_eq!(full, explicit);
}

fn assert_vec3_close(actual: [f32; 3], expected: [f32; 3]) {
    for (actual, expected) in actual.into_iter().zip(expected) {
        assert!((actual - expected).abs() < 0.0001, "{actual} != {expected}");
    }
}
