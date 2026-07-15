use super::*;
use sms_formats::{J3dColorChannel, J3dIndirectMaterial, J3dTevOrder, J3dTevStage, J3dTexGen};

// TMapObjWave::load and TMapObjWave::draw in the SMS decompilation are the
// source of truth for this camera/player-centered close-range foam surface.
const WAVE_SIZE: f32 = 5_200.0;
const HALF_WAVE_SIZE: f32 = WAVE_SIZE * 0.5;
const GRID_SIZE: f32 = 200.0;
const GRID_COUNT: usize = 26;
const WAVE_COLOR: [u8; 4] = [0xc8, 0xc8, 0xff, 0];
const TEX0_SCALE: f32 = 0.0012;
const TEX1_SCALE: f32 = 0.0015;

#[derive(Default)]
pub(super) struct ProceduralWavePreview {
    pub(super) triangles: Vec<PreviewTriangle>,
    pub(super) source_vertices: usize,
    pub(super) triangle_count: usize,
}

pub(super) fn build_procedural_wave_preview(
    document: &StageDocument,
    model_index: usize,
    packet_index: usize,
    textures: &mut Vec<PreviewTexture>,
    materials: &mut Vec<J3dMaterial>,
) -> ProceduralWavePreview {
    let Some(asset) = document.assets.iter().find(|asset| {
        asset.kind == StageAssetKind::Texture
            && asset
                .path
                .to_string_lossy()
                .replace('\\', "/")
                .to_ascii_lowercase()
                .ends_with("/map/map/wave.bti")
    }) else {
        return ProceduralWavePreview::default();
    };
    let Ok(bytes) = read_stage_asset_bytes(&asset.path) else {
        return ProceduralWavePreview::default();
    };
    let Ok(mut texture) = decode_bti_texture(bytes) else {
        return ProceduralWavePreview::default();
    };
    texture.name = "wave.bti".to_owned();

    let texture_index = push_j3d_preview_textures(textures, std::slice::from_ref(&texture));
    let material_index = materials.len();
    materials.push(runtime_wave_material(material_index, texture_index));
    let triangles = wave_triangles(model_index, packet_index, material_index, texture_index);
    let triangle_count = triangles.len();

    ProceduralWavePreview {
        triangles,
        // Retail submits 26 strips with 52 vertices each. The editor stores
        // indexed-strip output as ordinary triangles, but reports the source
        // vertex workload here.
        source_vertices: GRID_COUNT * GRID_COUNT * 2,
        triangle_count,
    }
}

fn wave_triangles(
    model_index: usize,
    packet_index: usize,
    material_index: usize,
    texture_index: usize,
) -> Vec<PreviewTriangle> {
    let mut triangles = Vec::with_capacity(GRID_COUNT * (GRID_COUNT - 1) * 2);
    for z_index in 0..GRID_COUNT {
        let z0 = -HALF_WAVE_SIZE + z_index as f32 * GRID_SIZE;
        let z1 = z0 + GRID_SIZE;
        for x_index in 0..GRID_COUNT - 1 {
            let x0 = -HALF_WAVE_SIZE + x_index as f32 * GRID_SIZE;
            let x1 = x0 + GRID_SIZE;
            let vertices = [
                wave_vertex(x0, z0),
                wave_vertex(x0, z1),
                wave_vertex(x1, z0),
                wave_vertex(x1, z1),
            ];
            push_wave_triangle(
                &mut triangles,
                [vertices[0], vertices[1], vertices[2]],
                model_index,
                packet_index,
                material_index,
                texture_index,
            );
            push_wave_triangle(
                &mut triangles,
                [vertices[2], vertices[1], vertices[3]],
                model_index,
                packet_index,
                material_index,
                texture_index,
            );
        }
    }
    triangles
}

#[derive(Clone, Copy)]
struct WaveVertex {
    position: [f32; 3],
    color: [u8; 4],
    uv0: [f32; 2],
    uv1: [f32; 2],
}

fn wave_vertex(x: f32, z: f32) -> WaveVertex {
    let alpha = (255.0 * (1.0 - x.abs().max(z.abs()) / HALF_WAVE_SIZE)).clamp(0.0, 255.0) as u8;
    WaveVertex {
        // X/Z stay camera-local. The GPU reproduces TMapObjWave's world-space
        // sine displacement and scrolling UVs from the live camera position.
        position: [x, 0.0, z],
        color: [WAVE_COLOR[0], WAVE_COLOR[1], WAVE_COLOR[2], alpha],
        uv0: [x * TEX0_SCALE, z * TEX0_SCALE],
        uv1: [x * TEX1_SCALE * 0.8, z * TEX1_SCALE],
    }
}

fn push_wave_triangle(
    triangles: &mut Vec<PreviewTriangle>,
    vertices: [WaveVertex; 3],
    model_index: usize,
    packet_index: usize,
    material_index: usize,
    texture_index: usize,
) {
    let positions = vertices.map(|vertex| vertex.position);
    let colors = vertices.map(|vertex| vertex.color);
    let uv0 = vertices.map(|vertex| vertex.uv0);
    let uv1 = vertices.map(|vertex| vertex.uv1);
    let mut tex_coord_sets = [None; 8];
    tex_coord_sets[0] = Some(uv0);
    tex_coord_sets[1] = Some(uv1);
    triangles.push(PreviewTriangle {
        vertices: positions,
        normals: Some([[0.0, 1.0, 0.0]; 3]),
        color_channels: [Some(colors), None],
        tex_coord_sets,
        material_index: Some(material_index),
        packet_index,
        model_index,
        render_layer: PreviewRenderLayer::WaveFoam,
        color: None,
        vertex_colors: Some(colors),
        combine_mode: J3dPreviewCombineMode::TextureModulateVertex,
        tex_coords: Some(uv0),
        texture_index: Some(texture_index),
        mask_tex_coords: None,
        mask_texture_index: None,
        cull_mode: Some(0),
        alpha_compare: Some(runtime_wave_alpha_compare()),
        blend_mode: Some(runtime_wave_blend_mode()),
        z_mode: Some(runtime_wave_z_mode()),
        billboard: None,
        particle_type: None,
        particle_pivot: None,
        particle_direction: None,
        particle_color_mode: None,
        particle_environment_color: None,
    });
}

fn runtime_wave_material(material_index: usize, texture_index: usize) -> J3dMaterial {
    let mut texture_indices = [None; 8];
    texture_indices[0] = Some(texture_index);
    // GX_COLOR0A0 configures RGB and alpha independently. TMapObjWave selects
    // vertex input for both, so the max-axis fade stored in vertex alpha must
    // be present in both J3D channel-control slots.
    let vertex_color_channel = J3dColorChannel {
        enable: 0,
        mat_src: 1,
        light_mask: 0,
        diffuse_fn: 2,
        attenuation_fn: 2,
        amb_src: 1,
    };
    J3dMaterial {
        name: "_runtime_wave".to_owned(),
        material_index,
        material_id: material_index,
        loader_flags: SMS_MAP_MODEL_LOAD_FLAGS,
        lighting_enabled: false,
        mode: 4,
        cull_mode: 0,
        color_channel_count: 1,
        material_colors: [[255; 4]; 2],
        ambient_colors: [[255; 4]; 2],
        color_channels: [
            vertex_color_channel,
            vertex_color_channel,
            J3dColorChannel::default(),
            J3dColorChannel::default(),
        ],
        tex_gen_count: 2,
        tex_gens: std::array::from_fn(|slot| J3dTexGen {
            gen_type: 1,
            source: 4 + slot as u8,
            matrix: 60,
        }),
        tex_matrices: [None; 8],
        texture_indices,
        tev_colors: [
            [0xc2, 0xf2, 0xbe, 0],
            [0, 0, 0, 0x48],
            [0, 0, 0, 0x90],
            [0; 4],
        ],
        tev_k_colors: [[255; 4]; 4],
        tev_stages: vec![
            J3dTevStage {
                order: J3dTevOrder {
                    tex_coord: Some(0),
                    tex_map: Some(0),
                    color_channel: 4,
                },
                color_args: [15, 15, 15, 15],
                color_op: 0,
                color_bias: 0,
                color_scale: 0,
                color_clamp: 1,
                color_register: 0,
                alpha_args: [7, 4, 5, 7],
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
            },
            J3dTevStage {
                order: J3dTevOrder {
                    tex_coord: Some(1),
                    tex_map: Some(0),
                    color_channel: 4,
                },
                color_args: [10, 15, 15, 15],
                color_op: 0,
                color_bias: 0,
                color_scale: 1,
                color_clamp: 1,
                color_register: 0,
                alpha_args: [7, 4, 0, 7],
                alpha_op: 0,
                alpha_bias: 0,
                alpha_scale: 1,
                alpha_clamp: 1,
                alpha_register: 0,
                konst_color: 12,
                konst_alpha: 28,
                raster_swap: 0,
                texture_swap: 0,
                indirect: Default::default(),
            },
        ],
        swap_tables: [[0, 1, 2, 3]; 4],
        indirect: J3dIndirectMaterial::default(),
        fog: None,
        alpha_compare: runtime_wave_alpha_compare(),
        blend_mode: runtime_wave_blend_mode(),
        z_mode: runtime_wave_z_mode(),
        z_comp_loc: 1,
        dither: 0,
    }
}

fn runtime_wave_alpha_compare() -> J3dAlphaCompare {
    J3dAlphaCompare {
        comp0: 6,
        ref0: 0x55,
        op: 1,
        comp1: 3,
        ref1: 0x23,
    }
}

fn runtime_wave_blend_mode() -> J3dBlendMode {
    J3dBlendMode {
        mode: 1,
        src_factor: 4,
        dst_factor: 2,
        logic_op: 5,
    }
}

fn runtime_wave_z_mode() -> J3dZMode {
    J3dZMode {
        compare_enable: 1,
        func: 3,
        update_enable: 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retail_wave_grid_and_radial_alpha_are_preserved() {
        let triangles = wave_triangles(3, 7, 2, 1);

        assert_eq!(triangles.len(), 1_300);
        assert!(triangles.iter().all(|triangle| {
            triangle.render_layer == PreviewRenderLayer::WaveFoam
                && triangle.model_index == 3
                && triangle.packet_index == 7
                && triangle.material_index == Some(2)
                && triangle.texture_index == Some(1)
        }));
        assert_eq!(wave_vertex(0.0, 0.0).color[3], 255);
        assert_eq!(wave_vertex(-2_600.0, 0.0).color[3], 0);
        let vertex = wave_vertex(200.0, -400.0);
        assert!((vertex.uv0[0] - 0.24).abs() < 0.000_001);
        assert!((vertex.uv0[1] + 0.48).abs() < 0.000_001);
        assert!((vertex.uv1[0] - 0.24).abs() < 0.000_001);
        assert!((vertex.uv1[1] + 0.6).abs() < 0.000_001);
    }

    #[test]
    fn retail_wave_tev_and_blend_state_are_preserved() {
        let material = runtime_wave_material(4, 9);

        assert_eq!(material.material_index, 4);
        assert_eq!(material.texture_indices[0], Some(9));
        assert_eq!(material.color_channels[0].mat_src, 1);
        assert_eq!(material.color_channels[1].mat_src, 1);
        assert_eq!(material.tex_gen_count, 2);
        assert_eq!(material.tev_stages.len(), 2);
        assert_eq!(material.tev_stages[0].alpha_args, [7, 4, 5, 7]);
        assert_eq!(material.tev_stages[1].color_args, [10, 15, 15, 15]);
        assert_eq!(material.tev_stages[1].alpha_args, [7, 4, 0, 7]);
        assert_eq!(material.tev_stages[1].color_scale, 1);
        assert_eq!(material.tev_stages[1].alpha_scale, 1);
        assert_eq!(material.alpha_compare, runtime_wave_alpha_compare());
        assert_eq!(material.blend_mode, runtime_wave_blend_mode());
        assert_eq!(material.z_mode, runtime_wave_z_mode());
    }
}
