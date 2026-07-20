use crate::model::{
    CollisionDocument, CollisionGroup, CollisionSurface, ImportedAlphaMode, ModelAssetDocument,
    ModelMaterial, ModelMesh, ModelNode, ModelPrimitive, NodePurpose, SourcePbrMetadata,
};

/// Builds the deterministic source-free proxy used by blank-stage bootstrap resources.
pub fn built_in_blank_stage_proxy(raw_path: &[u8]) -> ModelAssetDocument {
    let (name, color) = match raw_path {
        b"mapobj/coin.bmd" => ("coin_proxy", [255, 214, 54, 255]),
        b"mapobj/bottle_large.bmd" => ("bottle_proxy", [80, 176, 255, 255]),
        b"mapobj/juiceblock.bmd" => ("juice_block_proxy", [255, 145, 45, 255]),
        b"mapobj/normalblock.bmd" | b"mapobj/normalblock.col" => {
            ("normal_block_proxy", [188, 188, 196, 255])
        }
        _ => ("bootstrap_proxy", [220, 80, 220, 255]),
    };
    let positions = vec![
        [-50.0, -50.0, -50.0],
        [50.0, -50.0, -50.0],
        [50.0, -50.0, 50.0],
        [-50.0, -50.0, 50.0],
        [-50.0, 50.0, -50.0],
        [50.0, 50.0, -50.0],
        [50.0, 50.0, 50.0],
        [-50.0, 50.0, 50.0],
    ];
    let normal = 0.577_350_26;
    let normals = vec![
        [-normal, -normal, -normal],
        [normal, -normal, -normal],
        [normal, -normal, normal],
        [-normal, -normal, normal],
        [-normal, normal, -normal],
        [normal, normal, -normal],
        [normal, normal, normal],
        [-normal, normal, normal],
    ];
    let triangles = vec![
        [0, 1, 2],
        [0, 2, 3],
        [4, 6, 5],
        [4, 7, 6],
        [0, 5, 1],
        [0, 4, 5],
        [1, 6, 2],
        [1, 5, 6],
        [2, 7, 3],
        [2, 6, 7],
        [3, 4, 0],
        [3, 7, 4],
    ];
    let mut gx = sms_formats::GxMaterial {
        name: format!("{name}_material"),
        cull_mode: 2,
        color_channel_count: 1,
        material_colors: [Some(color), None],
        color_channels: [
            Some(sms_formats::GxColorChannel::default()),
            Some(sms_formats::GxColorChannel::default()),
            None,
            None,
        ],
        ..sms_formats::GxMaterial::default()
    };
    gx.tev_orders[0] = Some(sms_formats::GxTevOrder {
        tex_coord: None,
        tex_map: None,
        color_channel: 4,
    });
    if let Some(stage) = &mut gx.tev_stages[0] {
        stage.color_inputs = [10, 15, 15, 15];
        stage.alpha_inputs = [5, 7, 7, 7];
    }

    let mut asset = ModelAssetDocument::new(name);
    asset.scene_roots = vec![0];
    asset.nodes.push(ModelNode {
        name: "root".to_string(),
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
    asset.meshes.push(ModelMesh {
        name: "proxy_cube".to_string(),
        primitives: vec![ModelPrimitive {
            positions: positions.clone(),
            normals,
            tangents: Vec::new(),
            tex_coords: Vec::new(),
            colors: Vec::new(),
            indices: triangles.iter().flatten().copied().collect(),
            material: Some(0),
        }],
    });
    asset.materials.push(ModelMaterial {
        gx,
        source_base_color: color.map(|channel| f32::from(channel) / 255.0),
        base_color_texture: None,
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
    });
    asset.collision = Some(CollisionDocument {
        vertices: positions,
        groups: vec![CollisionGroup {
            name: "proxy_collision".to_string(),
            surface: CollisionSurface::default(),
            triangles,
        }],
    });
    asset
}
