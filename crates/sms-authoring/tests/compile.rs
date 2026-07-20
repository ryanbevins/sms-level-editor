use sms_authoring::{
    decode_canonical_bmd3, GxMaterial, GxTextureBinding, ImportedAlphaMode, ModelAssetDocument,
    ModelMaterial, ModelMesh, ModelNode, ModelPrimitive, ModelTexture, NodePurpose,
    SourcePbrMetadata, TexCoordSet,
};
use sms_formats::{J3dMaterialTableKind, J3dRebuildSectionData, J3dScalarArray};

fn identity() -> [[f32; 4]; 4] {
    [
        [1.0, 0.0, 0.0, 0.0],
        [0.0, 1.0, 0.0, 0.0],
        [0.0, 0.0, 1.0, 0.0],
        [0.0, 0.0, 0.0, 1.0],
    ]
}

fn texture(index: u8) -> ModelTexture {
    let pixel = [index, index.wrapping_add(1), index.wrapping_add(2), 255];
    ModelTexture {
        name: format!("texture_{index}"),
        width: 16,
        height: 16,
        rgba8: pixel.repeat(16 * 16),
        encode_options: Default::default(),
    }
}

fn four_texture_asset() -> ModelAssetDocument {
    let mut gx = GxMaterial {
        name: "used_material".to_string(),
        tex_gen_count: 1,
        ..GxMaterial::default()
    };
    gx.texture_numbers[0] = Some(2);
    gx.tex_gens[0] = Some(sms_authoring::GxTexCoordGen {
        function: 1,
        source: 4,
        matrix: 60,
    });
    gx.tev_orders[0] = Some(sms_authoring::GxTevOrder {
        tex_coord: Some(0),
        tex_map: Some(0),
        color_channel: 0xff,
    });

    let mut asset = ModelAssetDocument::new("texture_pruning");
    asset.scene_roots = vec![0];
    asset.nodes = vec![ModelNode {
        name: "root".to_string(),
        parent: None,
        children: Vec::new(),
        mesh: Some(0),
        purpose: NodePurpose::Render,
        local_transform: identity(),
    }];
    asset.meshes = vec![ModelMesh {
        name: "triangle".to_string(),
        primitives: vec![ModelPrimitive {
            positions: vec![[0.0, 0.0, 0.0], [100.0, 0.0, 0.0], [0.0, 100.0, 0.0]],
            normals: vec![[0.0, 0.0, 1.0]; 3],
            tangents: Vec::new(),
            tex_coords: vec![TexCoordSet {
                set: 0,
                values: vec![[0.0, 0.0], [1.0, 0.0], [0.0, 1.0]],
            }],
            colors: Vec::new(),
            indices: vec![0, 1, 2],
            material: Some(0),
        }],
    }];
    asset.materials = vec![ModelMaterial {
        gx,
        source_base_color: [1.0; 4],
        base_color_texture: Some(GxTextureBinding {
            texture: 2,
            tex_coord: 0,
        }),
        vertex_color_set: None,
        source_double_sided: false,
        source_alpha_mode: ImportedAlphaMode::Opaque,
        source_pbr: SourcePbrMetadata {
            metallic_factor: 0.0,
            roughness_factor: 1.0,
            has_metallic_roughness_texture: false,
            has_normal_texture: false,
            has_occlusion_texture: false,
            emissive_factor: [0.0; 3],
            has_emissive_texture: false,
        },
    }];
    asset.textures = (0..4).map(texture).collect();
    asset
}

#[test]
fn bmd_compile_prunes_and_stably_remaps_unreferenced_textures() {
    let asset = four_texture_asset();
    let native = asset.to_native_bytes().unwrap();

    let first = asset.compile_bmd_with_report().unwrap();
    let second = asset.compile_bmd_with_report().unwrap();
    assert_eq!(first.bytes, second.bytes);
    assert_eq!(first.report, second.report);
    assert_eq!(first.report.total_byte_size, first.bytes.len());
    assert_eq!(first.report.sections.len(), 8);

    let decoded = decode_canonical_bmd3(&first.bytes).unwrap();
    let texture_section = decoded
        .sections
        .iter()
        .find_map(|section| match &section.data {
            J3dRebuildSectionData::Textures(textures) => Some(textures),
            _ => None,
        })
        .unwrap();
    assert_eq!(texture_section.texture_count, 1);
    assert_eq!(texture_section.textures.len(), 1);
    assert_eq!(texture_section.names.entries.len(), 1);
    assert_eq!(texture_section.names.entries[0].name, "texture_2");

    let material_section = decoded
        .sections
        .iter()
        .find_map(|section| match &section.data {
            J3dRebuildSectionData::Materials(materials) => Some(materials),
            _ => None,
        })
        .unwrap();
    let texture_numbers = material_section
        .tables
        .iter()
        .find_map(|table| {
            (table.kind == J3dMaterialTableKind::TextureNumber).then_some(&table.allocation)
        })
        .unwrap();
    let J3dScalarArray::Unsigned16(texture_numbers) = texture_numbers else {
        panic!("MAT3 texture-number bank was not u16");
    };
    let slot_bank_index = material_section.material_init_records[0].texture_number_indices[0];
    assert_eq!(texture_numbers[usize::from(slot_bank_index)], 0);

    assert_eq!(asset.textures.len(), 4);
    assert_eq!(asset.materials[0].gx.texture_numbers[0], Some(2));
    assert_eq!(asset.to_native_bytes().unwrap(), native);
    let reopened = ModelAssetDocument::from_native_bytes(&native).unwrap();
    assert_eq!(reopened.textures.len(), 4);
    assert_eq!(reopened.materials[0].gx.texture_numbers[0], Some(2));
}

#[test]
fn referenced_textures_keep_original_index_order_not_slot_discovery_order() {
    let mut asset = four_texture_asset();
    asset.materials[0].gx.texture_numbers[0] = Some(3);
    asset.materials[0].gx.texture_numbers[1] = Some(1);

    let decoded = asset.compile_bmd_document().unwrap();
    let texture_section = decoded
        .sections
        .iter()
        .find_map(|section| match &section.data {
            J3dRebuildSectionData::Textures(textures) => Some(textures),
            _ => None,
        })
        .unwrap();
    assert_eq!(
        texture_section
            .names
            .entries
            .iter()
            .map(|entry| entry.name.as_str())
            .collect::<Vec<_>>(),
        ["texture_1", "texture_3"]
    );

    let material_section = decoded
        .sections
        .iter()
        .find_map(|section| match &section.data {
            J3dRebuildSectionData::Materials(materials) => Some(materials),
            _ => None,
        })
        .unwrap();
    let J3dScalarArray::Unsigned16(texture_numbers) = &material_section
        .tables
        .iter()
        .find(|table| table.kind == J3dMaterialTableKind::TextureNumber)
        .unwrap()
        .allocation
    else {
        panic!("MAT3 texture-number bank was not u16");
    };
    let init = &material_section.material_init_records[0];
    assert_eq!(
        texture_numbers[usize::from(init.texture_number_indices[0])],
        1
    );
    assert_eq!(
        texture_numbers[usize::from(init.texture_number_indices[1])],
        0
    );
}

#[test]
fn out_of_range_gx_texture_reference_reports_material_and_slot() {
    let mut asset = four_texture_asset();
    asset.materials[0].gx.texture_numbers[0] = Some(4);
    let error = asset.compile_bmd().unwrap_err().to_string();
    assert!(
        error.contains(
            "material 0 (\"used_material\") texture slot 0 references texture 4, but the model contains 4 textures"
        ),
        "unexpected error: {error}"
    );
}
