use super::*;

#[test]
fn runtime_joint_children_reverse_inf1_sibling_order() {
    let parents = [None, Some(0), Some(1), Some(1), Some(1)];

    assert_eq!(runtime_child_joint_index(&parents, 1, 0), Some(4));
    assert_eq!(runtime_child_joint_index(&parents, 1, 1), Some(3));
    assert_eq!(runtime_child_joint_index(&parents, 1, 2), Some(2));
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
