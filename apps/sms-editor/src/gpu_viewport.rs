use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use bytemuck::{Pod, Zeroable};
use eframe::egui;
use eframe::egui_wgpu::{Callback, CallbackResources, CallbackTrait, ScreenDescriptor};
use eframe::wgpu::{self, util::DeviceExt};
use sms_formats::{
    J3dAlphaCompare, J3dBlendMode, J3dMaterial, J3dTevStage, J3dTexMatrix, J3dTextureSrtAnimation,
    J3dZMode,
};

use super::{
    preview_solid_triangle_colors, preview_triangle_normal, ModelPreview, PreviewRenderLayer,
    PreviewTexture, PreviewTriangle,
};

const DEPTH_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Depth24Plus;
const TEXTURE_SLOT_COUNT: usize = 8;
const TEV_STAGE_COUNT: usize = 16;
const TEX_MATRIX_ROW_COUNT: usize = TEXTURE_SLOT_COUNT * 3;
static NEXT_SCENE_GENERATION: AtomicU64 = AtomicU64::new(1);

const J3D_SHADER: &str = r#"
struct Camera {
    camera_position: vec4<f32>,
    right: vec4<f32>,
    up: vec4<f32>,
    forward: vec4<f32>,
    projection: vec4<f32>,
    clip: vec4<f32>,
};

struct Material {
    counts: vec4<u32>,
    alpha_compare: vec4<u32>,
    alpha_refs: vec4<f32>,
    material_colors: array<vec4<f32>, 2>,
    ambient_colors: array<vec4<f32>, 2>,
    tev_colors: array<vec4<f32>, 4>,
    tev_k_colors: array<vec4<f32>, 4>,
    color_channels: array<vec4<u32>, 4>,
    tex_gens: array<vec4<u32>, 8>,
    tex_matrix_rows: array<vec4<f32>, 24>,
    tex_effect_rows: array<vec4<f32>, 32>,
    tev_orders: array<vec4<u32>, 16>,
    tev_color_args: array<vec4<u32>, 16>,
    tev_color_ops: array<vec4<u32>, 16>,
    tev_alpha_args: array<vec4<u32>, 16>,
    tev_alpha_ops: array<vec4<u32>, 16>,
    tev_selectors: array<vec4<u32>, 16>,
    swap_tables: array<vec4<u32>, 4>,
    indirect_orders: array<vec4<u32>, 3>,
    indirect_matrix_rows: array<vec4<f32>, 6>,
    indirect_matrix_meta: array<vec4<u32>, 3>,
    indirect_stages0: array<vec4<u32>, 16>,
    indirect_stages1: array<vec4<u32>, 16>,
    indirect_stages2: array<vec4<u32>, 16>,
    fog_meta: vec4<u32>,
    fog_params: vec4<f32>,
    fog_color: vec4<f32>,
};

struct VertexIn {
    @location(0) position: vec3<f32>,
    @location(1) normal: vec3<f32>,
    @location(2) color0: vec4<f32>,
    @location(3) color1: vec4<f32>,
    @location(4) uv0: vec2<f32>,
    @location(5) uv1: vec2<f32>,
    @location(6) uv2: vec2<f32>,
    @location(7) uv3: vec2<f32>,
    @location(8) uv4: vec2<f32>,
    @location(9) uv5: vec2<f32>,
    @location(10) uv6: vec2<f32>,
    @location(11) uv7: vec2<f32>,
    @location(12) camera_relative: u32,
};

struct VertexOut {
    @builtin(position) position: vec4<f32>,
    @location(0) color0: vec4<f32>,
    @location(1) color1: vec4<f32>,
    @location(2) uv0: vec2<f32>,
    @location(3) uv1: vec2<f32>,
    @location(4) uv2: vec2<f32>,
    @location(5) uv3: vec2<f32>,
    @location(6) uv4: vec2<f32>,
    @location(7) uv5: vec2<f32>,
    @location(8) uv6: vec2<f32>,
    @location(9) uv7: vec2<f32>,
    @location(10) view_depth: f32,
};

@group(0) @binding(0)
var<uniform> camera: Camera;

@group(1) @binding(0)
var<uniform> material: Material;

@group(1) @binding(1) var texture0: texture_2d<f32>;
@group(1) @binding(2) var sampler0: sampler;
@group(1) @binding(3) var texture1: texture_2d<f32>;
@group(1) @binding(4) var sampler1: sampler;
@group(1) @binding(5) var texture2: texture_2d<f32>;
@group(1) @binding(6) var sampler2: sampler;
@group(1) @binding(7) var texture3: texture_2d<f32>;
@group(1) @binding(8) var sampler3: sampler;
@group(1) @binding(9) var texture4: texture_2d<f32>;
@group(1) @binding(10) var sampler4: sampler;
@group(1) @binding(11) var texture5: texture_2d<f32>;
@group(1) @binding(12) var sampler5: sampler;
@group(1) @binding(13) var texture6: texture_2d<f32>;
@group(1) @binding(14) var sampler6: sampler;
@group(1) @binding(15) var texture7: texture_2d<f32>;
@group(1) @binding(16) var sampler7: sampler;

fn raw_uv(input: VertexIn, index: u32) -> vec2<f32> {
    switch index {
        case 0u: { return input.uv0; }
        case 1u: { return input.uv1; }
        case 2u: { return input.uv2; }
        case 3u: { return input.uv3; }
        case 4u: { return input.uv4; }
        case 5u: { return input.uv5; }
        case 6u: { return input.uv6; }
        case 7u: { return input.uv7; }
        default: { return vec2<f32>(0.0); }
    }
}

fn generated_uv(coords: array<vec3<f32>, 8>, index: u32) -> vec2<f32> {
    switch index {
        case 0u: { return coords[0].xy; }
        case 1u: { return coords[1].xy; }
        case 2u: { return coords[2].xy; }
        case 3u: { return coords[3].xy; }
        case 4u: { return coords[4].xy; }
        case 5u: { return coords[5].xy; }
        case 6u: { return coords[6].xy; }
        case 7u: { return coords[7].xy; }
        default: { return vec2<f32>(0.0); }
    }
}

fn apply_effect_matrix(matrix_index: u32, value: vec4<f32>) -> vec4<f32> {
    let row = matrix_index * 4u;
    return vec4<f32>(
        dot(material.tex_effect_rows[row], value),
        dot(material.tex_effect_rows[row + 1u], value),
        dot(material.tex_effect_rows[row + 2u], value),
        dot(material.tex_effect_rows[row + 3u], value),
    );
}

fn channel_source(source: u32, vertex_color: vec4<f32>, material_color: vec4<f32>) -> vec4<f32> {
    if (source == 1u) {
        return vertex_color;
    }
    return material_color;
}

fn compute_color_channel(
    control_index: u32,
    material_index: u32,
    vertex_color: vec4<f32>,
    normal: vec3<f32>,
    position: vec3<f32>,
) -> vec4<f32> {
    let control = material.color_channels[control_index];
    let mat = channel_source(
        control.y,
        vertex_color,
        material.material_colors[material_index],
    );
    if (control.x == 0u) {
        return mat;
    }

    let ambient_source = control.z;
    let ambient = channel_source(
        ambient_source,
        vertex_color,
        material.ambient_colors[material_index],
    );
    let packed = control.w;
    let diffuse_function = packed & 0xffu;
    let attenuation_function = (packed >> 8u) & 0xffu;
    let light_mask = (packed >> 16u) & 0xffu;
    let light_position = vec3<f32>(200000.0, 500000.0, 200000.0);
    let light_direction = normalize(light_position - position);
    let normalized_normal = normalize(normal);
    if (attenuation_function == 0u) {
        let camera_light_direction = normalize(light_position - camera.camera_position.xyz);
        let view_direction = -camera.forward.xyz;
        let half_direction = normalize(camera_light_direction + view_direction);
        let half_cosine = max(dot(normalized_normal, half_direction), 0.0);
        let cosine_squared = half_cosine * half_cosine;
        let denominator = max(25.0 - 24.0 * cosine_squared, 0.000001);
        let facing_light = dot(normalized_normal, light_direction) >= 0.0;
        let specular = select(0.0, cosine_squared / denominator, facing_light);
        let enabled_specular = select(0.0, specular, (light_mask & 0x04u) != 0u);
        return clamp(
            mat * (ambient + vec4<f32>(vec3<f32>(enabled_specular), enabled_specular)),
            vec4<f32>(0.0),
            vec4<f32>(1.0),
        );
    }

    let n_dot_l = dot(normalized_normal, light_direction);
    var diffuse = 1.0;
    if (diffuse_function == 1u) {
        diffuse = n_dot_l;
    } else if (diffuse_function == 2u) {
        diffuse = max(n_dot_l, 0.0);
    }
    return clamp(mat * (ambient + vec4<f32>(vec3<f32>(diffuse), diffuse)), vec4<f32>(0.0), vec4<f32>(1.0));
}

@vertex
fn vs_main(input: VertexIn) -> VertexOut {
    // TSky::perform translates sky.bmd to the camera every frame. Keeping the
    // model in camera-local space here reproduces that behavior without
    // rebuilding its vertex buffer while the editor camera moves.
    let rel = select(
        input.position - camera.camera_position.xyz,
        input.position,
        input.camera_relative != 0u,
    );
    let depth = dot(rel, camera.forward.xyz);
    let clip_x = dot(rel, camera.right.xyz) * camera.projection.x + camera.projection.z * depth;
    let clip_y = dot(rel, camera.up.xyz) * camera.projection.y + camera.projection.w * depth;
    let depth_range = max(camera.clip.y - camera.clip.x, 1.0);
    let clip_z = (camera.clip.y / depth_range) * depth
        - (camera.clip.y * camera.clip.x / depth_range);

    let view_position = vec3<f32>(
        dot(rel, camera.right.xyz),
        dot(rel, camera.up.xyz),
        depth,
    );
    let view_normal = normalize(vec3<f32>(
        dot(input.normal, camera.right.xyz),
        dot(input.normal, camera.up.xyz),
        dot(input.normal, camera.forward.xyz),
    ));

    var coords: array<vec3<f32>, 8>;
    for (var i = 0u; i < 8u; i = i + 1u) {
        let config = material.tex_gens[i];
        let source = config.y;
        let mode = config.w & 0xffu;
        let view_space_mode = mode == 1u || mode == 3u || mode == 6u
            || mode == 7u || mode == 9u;
        var source_value = vec4<f32>(0.0, 0.0, 0.0, 1.0);
        if (source == 0u) {
            source_value = select(
                vec4<f32>(input.position, 1.0),
                vec4<f32>(view_position, 1.0),
                view_space_mode,
            );
        } else if (source == 1u || source == 2u || source == 3u) {
            source_value = select(
                vec4<f32>(input.normal, 0.0),
                vec4<f32>(view_normal, 0.0),
                view_space_mode,
            );
        } else if (source >= 4u && source <= 11u) {
            source_value = vec4<f32>(raw_uv(input, source - 4u), 0.0, 1.0);
        } else if (source >= 12u && source <= 18u) {
            let prior = coords[source - 12u];
            source_value = vec4<f32>(prior, 1.0);
        } else if (source == 19u) {
            source_value = vec4<f32>(input.color0.rg, 0.0, 1.0);
        } else if (source == 20u) {
            source_value = vec4<f32>(input.color1.rg, 0.0, 1.0);
        }

        let matrix_plus_one = config.z;
        if (matrix_plus_one != 0u) {
            let matrix_index = matrix_plus_one - 1u;
            let uses_effect = mode == 2u || mode == 3u || mode == 4u || mode == 5u
                || mode == 8u || mode == 9u || mode == 10u || mode == 11u;
            if (uses_effect) {
                source_value = apply_effect_matrix(matrix_index, source_value);
            }
            if (mode == 6u || mode == 10u) {
                source_value = vec4<f32>(
                    0.5 * source_value.x + 0.5 * source_value.w,
                    -0.5 * source_value.y + 0.5 * source_value.w,
                    source_value.z,
                    1.0,
                );
            } else if (mode == 7u || mode == 8u || mode == 9u || mode == 11u) {
                source_value = vec4<f32>(
                    0.5 * source_value.x + 0.5 * source_value.z,
                    -0.5 * source_value.y + 0.5 * source_value.z,
                    source_value.z,
                    1.0,
                );
            }
            let row = matrix_index * 3u;
            let generated = vec3<f32>(
                dot(material.tex_matrix_rows[row], source_value),
                dot(material.tex_matrix_rows[row + 1u], source_value),
                dot(material.tex_matrix_rows[row + 2u], source_value),
            );
            if (config.x == 0u && abs(generated.z) > 0.000001) {
                coords[i] = vec3<f32>(generated.xy / generated.z, generated.z);
            } else {
                coords[i] = generated;
            }
        } else {
            coords[i] = source_value.xyz;
        }
    }

    let rgb0 = compute_color_channel(0u, 0u, input.color0, input.normal, input.position);
    let alpha0 = compute_color_channel(1u, 0u, input.color0, input.normal, input.position);
    let rgb1 = compute_color_channel(2u, 1u, input.color1, input.normal, input.position);
    let alpha1 = compute_color_channel(3u, 1u, input.color1, input.normal, input.position);

    var out: VertexOut;
    out.position = vec4<f32>(clip_x, clip_y, clip_z, depth);
    out.color0 = vec4<f32>(rgb0.rgb, alpha0.a);
    out.color1 = vec4<f32>(rgb1.rgb, alpha1.a);
    out.uv0 = generated_uv(coords, 0u);
    out.uv1 = generated_uv(coords, 1u);
    out.uv2 = generated_uv(coords, 2u);
    out.uv3 = generated_uv(coords, 3u);
    out.uv4 = generated_uv(coords, 4u);
    out.uv5 = generated_uv(coords, 5u);
    out.uv6 = generated_uv(coords, 6u);
    out.uv7 = generated_uv(coords, 7u);
    out.view_depth = depth;
    return out;
}

fn tex_coord(input: VertexOut, index: u32) -> vec2<f32> {
    switch index {
        case 0u: { return input.uv0; }
        case 1u: { return input.uv1; }
        case 2u: { return input.uv2; }
        case 3u: { return input.uv3; }
        case 4u: { return input.uv4; }
        case 5u: { return input.uv5; }
        case 6u: { return input.uv6; }
        case 7u: { return input.uv7; }
        default: { return vec2<f32>(0.0); }
    }
}

fn sample_texture(slot: u32, uv: vec2<f32>) -> vec4<f32> {
    let uv_dx = dpdx(uv);
    let uv_dy = dpdy(uv);
    switch slot {
        case 0u: { return textureSampleGrad(texture0, sampler0, uv, uv_dx, uv_dy); }
        case 1u: { return textureSampleGrad(texture1, sampler1, uv, uv_dx, uv_dy); }
        case 2u: { return textureSampleGrad(texture2, sampler2, uv, uv_dx, uv_dy); }
        case 3u: { return textureSampleGrad(texture3, sampler3, uv, uv_dx, uv_dy); }
        case 4u: { return textureSampleGrad(texture4, sampler4, uv, uv_dx, uv_dy); }
        case 5u: { return textureSampleGrad(texture5, sampler5, uv, uv_dx, uv_dy); }
        case 6u: { return textureSampleGrad(texture6, sampler6, uv, uv_dx, uv_dy); }
        case 7u: { return textureSampleGrad(texture7, sampler7, uv, uv_dx, uv_dy); }
        default: { return vec4<f32>(1.0); }
    }
}

fn swap_color(value: vec4<f32>, table_index: u32) -> vec4<f32> {
    let table = material.swap_tables[min(table_index, 3u)];
    return vec4<f32>(
        value[min(table.x, 3u)],
        value[min(table.y, 3u)],
        value[min(table.z, 3u)],
        value[min(table.w, 3u)],
    );
}

fn raster_color(input: VertexOut, channel: u32) -> vec4<f32> {
    switch channel {
        case 0u: { return vec4<f32>(input.color0.rgb, 1.0); }
        case 1u: { return vec4<f32>(input.color1.rgb, 1.0); }
        case 2u: { return vec4<f32>(vec3<f32>(input.color0.a), input.color0.a); }
        case 3u: { return vec4<f32>(vec3<f32>(input.color1.a), input.color1.a); }
        case 4u: { return input.color0; }
        case 5u: { return input.color1; }
        case 6u: { return vec4<f32>(0.0); }
        default: { return vec4<f32>(0.0); }
    }
}

fn konst_color(selector: u32) -> vec3<f32> {
    switch selector {
        case 0u: { return vec3<f32>(1.0); }
        case 1u: { return vec3<f32>(0.875); }
        case 2u: { return vec3<f32>(0.75); }
        case 3u: { return vec3<f32>(0.625); }
        case 4u: { return vec3<f32>(0.5); }
        case 5u: { return vec3<f32>(0.375); }
        case 6u: { return vec3<f32>(0.25); }
        case 7u: { return vec3<f32>(0.125); }
        case 12u: { return material.tev_k_colors[0].rgb; }
        case 13u: { return material.tev_k_colors[1].rgb; }
        case 14u: { return material.tev_k_colors[2].rgb; }
        case 15u: { return material.tev_k_colors[3].rgb; }
        default: {
            if (selector >= 16u && selector <= 31u) {
                let color_index = (selector - 16u) & 3u;
                let channel = (selector - 16u) >> 2u;
                return vec3<f32>(material.tev_k_colors[color_index][channel]);
            }
            return vec3<f32>(1.0);
        }
    }
}

fn konst_alpha(selector: u32) -> f32 {
    switch selector {
        case 0u: { return 1.0; }
        case 1u: { return 0.875; }
        case 2u: { return 0.75; }
        case 3u: { return 0.625; }
        case 4u: { return 0.5; }
        case 5u: { return 0.375; }
        case 6u: { return 0.25; }
        case 7u: { return 0.125; }
        default: {
            if (selector >= 16u && selector <= 31u) {
                let color_index = (selector - 16u) & 3u;
                let channel = (selector - 16u) >> 2u;
                return material.tev_k_colors[color_index][channel];
            }
            return 1.0;
        }
    }
}

fn color_arg(
    selector: u32,
    previous: vec4<f32>,
    reg0: vec4<f32>,
    reg1: vec4<f32>,
    reg2: vec4<f32>,
    tex: vec4<f32>,
    ras: vec4<f32>,
    konst: vec3<f32>,
) -> vec3<f32> {
    switch selector {
        case 0u: { return previous.rgb; }
        case 1u: { return vec3<f32>(previous.a); }
        case 2u: { return reg0.rgb; }
        case 3u: { return vec3<f32>(reg0.a); }
        case 4u: { return reg1.rgb; }
        case 5u: { return vec3<f32>(reg1.a); }
        case 6u: { return reg2.rgb; }
        case 7u: { return vec3<f32>(reg2.a); }
        case 8u: { return tex.rgb; }
        case 9u: { return vec3<f32>(tex.a); }
        case 10u: { return ras.rgb; }
        case 11u: { return vec3<f32>(ras.a); }
        case 12u: { return vec3<f32>(1.0); }
        case 13u: { return vec3<f32>(0.5); }
        case 14u: { return konst; }
        case 16u: { return vec3<f32>(tex.r); }
        case 17u: { return vec3<f32>(tex.g); }
        case 18u: { return vec3<f32>(tex.b); }
        default: { return vec3<f32>(0.0); }
    }
}

fn alpha_arg(
    selector: u32,
    previous: vec4<f32>,
    reg0: vec4<f32>,
    reg1: vec4<f32>,
    reg2: vec4<f32>,
    tex: vec4<f32>,
    ras: vec4<f32>,
    konst: f32,
) -> f32 {
    switch selector {
        case 0u: { return previous.a; }
        case 1u: { return reg0.a; }
        case 2u: { return reg1.a; }
        case 3u: { return reg2.a; }
        case 4u: { return tex.a; }
        case 5u: { return ras.a; }
        case 6u: { return konst; }
        default: { return 0.0; }
    }
}

fn tev_scale(value: vec3<f32>, scale: u32) -> vec3<f32> {
    switch scale {
        case 1u: { return value * 2.0; }
        case 2u: { return value * 4.0; }
        case 3u: { return value * 0.5; }
        default: { return value; }
    }
}

fn tev_alpha_scale(value: f32, scale: u32) -> f32 {
    switch scale {
        case 1u: { return value * 2.0; }
        case 2u: { return value * 4.0; }
        case 3u: { return value * 0.5; }
        default: { return value; }
    }
}

fn tev_u8(value: f32) -> u32 {
    return u32(clamp(round(value * 255.0), 0.0, 255.0));
}

fn tev_pack_gr(value: vec3<f32>) -> u32 {
    return tev_u8(value.r) | (tev_u8(value.g) << 8u);
}

fn tev_pack_bgr(value: vec3<f32>) -> u32 {
    return tev_u8(value.r) | (tev_u8(value.g) << 8u) | (tev_u8(value.b) << 16u);
}

fn tev_color_result(
    a: vec3<f32>,
    b: vec3<f32>,
    c: vec3<f32>,
    d: vec3<f32>,
    operation: vec4<u32>,
) -> vec3<f32> {
    let op = operation.x;
    var result = vec3<f32>(0.0);
    if (op == 0u || op == 1u) {
        let mixed = mix(a, b, c);
        result = select(d + mixed, d - mixed, op == 1u);
        if (operation.y == 1u) {
            result = result + vec3<f32>(0.5);
        } else if (operation.y == 2u) {
            result = result - vec3<f32>(0.5);
        }
        result = tev_scale(result, operation.z);
    } else {
        var comparison = vec3<f32>(0.0);
        if (op == 8u) {
            comparison = vec3<f32>(select(0.0, 1.0, tev_u8(a.r) > tev_u8(b.r)));
        } else if (op == 9u) {
            comparison = vec3<f32>(select(0.0, 1.0, tev_u8(a.r) == tev_u8(b.r)));
        } else if (op == 10u) {
            comparison = vec3<f32>(select(0.0, 1.0, tev_pack_gr(a) > tev_pack_gr(b)));
        } else if (op == 11u) {
            comparison = vec3<f32>(select(0.0, 1.0, tev_pack_gr(a) == tev_pack_gr(b)));
        } else if (op == 12u) {
            comparison = vec3<f32>(select(0.0, 1.0, tev_pack_bgr(a) > tev_pack_bgr(b)));
        } else if (op == 13u) {
            comparison = vec3<f32>(select(0.0, 1.0, tev_pack_bgr(a) == tev_pack_bgr(b)));
        } else if (op == 14u) {
            comparison = vec3<f32>(
                select(0.0, 1.0, tev_u8(a.r) > tev_u8(b.r)),
                select(0.0, 1.0, tev_u8(a.g) > tev_u8(b.g)),
                select(0.0, 1.0, tev_u8(a.b) > tev_u8(b.b)),
            );
        } else {
            comparison = vec3<f32>(
                select(0.0, 1.0, tev_u8(a.r) == tev_u8(b.r)),
                select(0.0, 1.0, tev_u8(a.g) == tev_u8(b.g)),
                select(0.0, 1.0, tev_u8(a.b) == tev_u8(b.b)),
            );
        }
        result = d + comparison * c;
    }
    let packed = operation.w;
    if ((packed & 0xffu) != 0u) {
        return clamp(result, vec3<f32>(0.0), vec3<f32>(1.0));
    }
    return clamp(result, vec3<f32>(-4.0), vec3<f32>(4.0));
}

fn tev_alpha_result(a: f32, b: f32, c: f32, d: f32, operation: vec4<u32>) -> f32 {
    let op = operation.x;
    var result = 0.0;
    if (op == 0u || op == 1u) {
        let mixed = mix(a, b, c);
        result = select(d + mixed, d - mixed, op == 1u);
        if (operation.y == 1u) {
            result = result + 0.5;
        } else if (operation.y == 2u) {
            result = result - 0.5;
        }
        result = tev_alpha_scale(result, operation.z);
    } else if (op == 14u) {
        result = d + select(0.0, c, tev_u8(a) > tev_u8(b));
    } else {
        result = d + select(0.0, c, tev_u8(a) == tev_u8(b));
    }
    let packed = operation.w;
    if ((packed & 0xffu) != 0u) {
        return clamp(result, 0.0, 1.0);
    }
    return clamp(result, -4.0, 4.0);
}

fn gx_compare(value: f32, compare: u32, reference: f32) -> bool {
    let quantized_value = tev_u8(value);
    let quantized_reference = tev_u8(reference);
    switch compare {
        case 0u: { return false; }
        case 1u: { return quantized_value < quantized_reference; }
        case 2u: { return quantized_value == quantized_reference; }
        case 3u: { return quantized_value <= quantized_reference; }
        case 4u: { return quantized_value > quantized_reference; }
        case 5u: { return quantized_value != quantized_reference; }
        case 6u: { return quantized_value >= quantized_reference; }
        default: { return true; }
    }
}

fn alpha_compare_passes(alpha: f32) -> bool {
    let pass0 = gx_compare(alpha, material.alpha_compare.x, material.alpha_refs.x);
    let pass1 = gx_compare(alpha, material.alpha_compare.z, material.alpha_refs.y);
    switch material.alpha_compare.y {
        case 0u: { return pass0 && pass1; }
        case 1u: { return pass0 || pass1; }
        case 2u: { return pass0 != pass1; }
        case 3u: { return pass0 == pass1; }
        default: { return true; }
    }
}

fn indirect_offset(stage_index: u32, input: VertexOut) -> vec2<f32> {
    if (material.counts.w == 0u) {
        return vec2<f32>(0.0);
    }
    let stage0 = material.indirect_stages0[stage_index];
    let indirect_stage = stage0.x;
    if (indirect_stage >= material.counts.w || stage0.w == 0u) {
        return vec2<f32>(0.0);
    }
    let order = material.indirect_orders[indirect_stage];
    if (order.x == 0u || order.y == 0u) {
        return vec2<f32>(0.0);
    }
    var uv = tex_coord(input, order.x - 1u);
    uv = uv * vec2<f32>(exp2(-f32(order.z)), exp2(-f32(order.w)));
    let sample_value = sample_texture(order.y - 1u, uv).rgb * 2.0 - vec3<f32>(1.0);
    let matrix_id = stage0.w;
    if (matrix_id >= 1u && matrix_id <= 3u) {
        let matrix_index = matrix_id - 1u;
        let row = matrix_index * 2u;
        let source = vec3<f32>(sample_value.rg, 1.0);
        return vec2<f32>(
            dot(material.indirect_matrix_rows[row].xyz, source),
            dot(material.indirect_matrix_rows[row + 1u].xyz, source),
        );
    }
    if (matrix_id >= 5u && matrix_id <= 7u) {
        return vec2<f32>(sample_value.r, 0.0);
    }
    if (matrix_id >= 9u && matrix_id <= 11u) {
        return vec2<f32>(0.0, sample_value.g);
    }
    return vec2<f32>(0.0);
}

fn apply_fog(color: vec4<f32>, depth: f32) -> vec4<f32> {
    let fog_type = material.fog_meta.x;
    if (fog_type == 0u) {
        return color;
    }
    let start_z = material.fog_params.x;
    let end_z = material.fog_params.y;
    let denominator = max(abs(end_z - start_z), 0.0001);
    let normalized_depth = max((depth - start_z) / denominator, 0.0);
    var fog_amount = clamp(normalized_depth, 0.0, 1.0);
    if (fog_type == 4u) {
        fog_amount = 1.0 - exp2(-8.0 * normalized_depth);
    } else if (fog_type == 5u) {
        fog_amount = 1.0 - exp2(-8.0 * normalized_depth * normalized_depth);
    } else if (fog_type == 6u) {
        fog_amount = exp2(-8.0 * max(1.0 - normalized_depth, 0.0));
    } else if (fog_type == 7u) {
        let reverse_depth = max(1.0 - normalized_depth, 0.0);
        fog_amount = exp2(-8.0 * reverse_depth * reverse_depth);
    }
    return vec4<f32>(mix(color.rgb, material.fog_color.rgb, clamp(fog_amount, 0.0, 1.0)), color.a);
}

@fragment
fn fs_main(input: VertexOut) -> @location(0) vec4<f32> {
    var previous = vec4<f32>(0.0);
    var reg0 = material.tev_colors[0];
    var reg1 = material.tev_colors[1];
    var reg2 = material.tev_colors[2];

    for (var stage_index = 0u; stage_index < 16u; stage_index = stage_index + 1u) {
        if (stage_index >= material.counts.x) {
            break;
        }
        let order = material.tev_orders[stage_index];
        let selectors = material.tev_selectors[stage_index];
        var tex = vec4<f32>(1.0);
        if (order.x != 0u && order.y != 0u) {
            var uv = tex_coord(input, order.x - 1u);
            uv = uv + indirect_offset(stage_index, input);
            tex = sample_texture(order.y - 1u, uv);
        }
        tex = swap_color(tex, selectors.w);
        let ras = swap_color(raster_color(input, order.z), selectors.z);
        let color_konst = konst_color(selectors.x);
        let alpha_konst = konst_alpha(selectors.y);
        let color_args = material.tev_color_args[stage_index];
        let alpha_args = material.tev_alpha_args[stage_index];

        let color_a = color_arg(color_args.x, previous, reg0, reg1, reg2, tex, ras, color_konst);
        let color_b = color_arg(color_args.y, previous, reg0, reg1, reg2, tex, ras, color_konst);
        let color_c = color_arg(color_args.z, previous, reg0, reg1, reg2, tex, ras, color_konst);
        let color_d = color_arg(color_args.w, previous, reg0, reg1, reg2, tex, ras, color_konst);
        let alpha_a = alpha_arg(alpha_args.x, previous, reg0, reg1, reg2, tex, ras, alpha_konst);
        let alpha_b = alpha_arg(alpha_args.y, previous, reg0, reg1, reg2, tex, ras, alpha_konst);
        let alpha_c = alpha_arg(alpha_args.z, previous, reg0, reg1, reg2, tex, ras, alpha_konst);
        let alpha_d = alpha_arg(alpha_args.w, previous, reg0, reg1, reg2, tex, ras, alpha_konst);

        let color_result = tev_color_result(
            color_a,
            color_b,
            color_c,
            color_d,
            material.tev_color_ops[stage_index],
        );
        let alpha_result = tev_alpha_result(
            alpha_a,
            alpha_b,
            alpha_c,
            alpha_d,
            material.tev_alpha_ops[stage_index],
        );
        let color_register = material.tev_color_ops[stage_index].w >> 8u;
        let alpha_register = material.tev_alpha_ops[stage_index].w >> 8u;
        if (color_register == 0u) {
            previous = vec4<f32>(color_result, previous.a);
        } else if (color_register == 1u) {
            reg0 = vec4<f32>(color_result, reg0.a);
        } else if (color_register == 2u) {
            reg1 = vec4<f32>(color_result, reg1.a);
        } else {
            reg2 = vec4<f32>(color_result, reg2.a);
        }
        if (alpha_register == 0u) {
            previous.a = alpha_result;
        } else if (alpha_register == 1u) {
            reg0.a = alpha_result;
        } else if (alpha_register == 2u) {
            reg1.a = alpha_result;
        } else {
            reg2.a = alpha_result;
        }
    }

    if (!alpha_compare_passes(previous.a)) {
        discard;
    }
    return apply_fog(previous, input.view_depth);
}
"#;

const DEPTH_CLEAR_SHADER: &str = r#"
@vertex
fn vs_main(@builtin(vertex_index) vertex_index: u32) -> @builtin(position) vec4<f32> {
    let positions = array<vec2<f32>, 3>(
        vec2<f32>(-1.0, -3.0),
        vec2<f32>(3.0, 1.0),
        vec2<f32>(-1.0, 1.0)
    );
    return vec4<f32>(positions[vertex_index], 1.0, 1.0);
}

@fragment
fn fs_main() -> @location(0) vec4<f32> {
    return vec4<f32>(0.0);
}
"#;

#[derive(Clone)]
pub struct GpuViewportScene {
    shared: Arc<Mutex<GpuViewportShared>>,
}

impl GpuViewportScene {
    pub fn from_preview(preview: &ModelPreview, target_format: wgpu::TextureFormat) -> Self {
        Self {
            shared: Arc::new(Mutex::new(GpuViewportShared {
                scene: GpuSceneData::from_preview(preview),
                frame: GpuViewportFrame::default(),
                generation: NEXT_SCENE_GENERATION.fetch_add(1, Ordering::Relaxed),
                target_format,
            })),
        }
    }

    pub fn set_frame(&self, frame: GpuViewportFrame) {
        if let Ok(mut shared) = self.shared.lock() {
            shared.frame = frame;
        }
    }

    pub fn paint_callback(&self, rect: egui::Rect) -> egui::PaintCallback {
        Callback::new_paint_callback(
            rect,
            GpuViewportCallback {
                shared: self.shared.clone(),
            },
        )
    }
}

#[derive(Clone, Copy, Debug)]
pub struct GpuViewportFrame {
    pub camera_position: [f32; 3],
    pub right: [f32; 3],
    pub up: [f32; 3],
    pub forward: [f32; 3],
    pub focal: f32,
    pub viewport_size: [f32; 2],
    pub viewport_pan: [f32; 2],
    pub near: f32,
    pub far: f32,
    pub animation_seconds: f32,
}

impl Default for GpuViewportFrame {
    fn default() -> Self {
        Self {
            camera_position: [0.0; 3],
            right: [1.0, 0.0, 0.0],
            up: [0.0, 1.0, 0.0],
            forward: [0.0, 0.0, 1.0],
            focal: 1.0,
            viewport_size: [1.0, 1.0],
            viewport_pan: [0.0; 2],
            near: 8.0,
            far: 100_000.0,
            animation_seconds: 0.0,
        }
    }
}

struct GpuViewportShared {
    scene: GpuSceneData,
    frame: GpuViewportFrame,
    generation: u64,
    target_format: wgpu::TextureFormat,
}

struct GpuViewportCallback {
    shared: Arc<Mutex<GpuViewportShared>>,
}

impl CallbackTrait for GpuViewportCallback {
    fn prepare(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        _screen_descriptor: &ScreenDescriptor,
        _egui_encoder: &mut wgpu::CommandEncoder,
        callback_resources: &mut CallbackResources,
    ) -> Vec<wgpu::CommandBuffer> {
        let Ok(shared) = self.shared.lock() else {
            return Vec::new();
        };
        let resources = callback_resources
            .entry::<GpuViewportResources>()
            .or_insert_with(|| GpuViewportResources::new(device, shared.target_format));
        resources.ensure_scene(device, queue, &shared.scene, shared.generation);
        resources.write_frame(queue, shared.frame, &shared.scene);
        Vec::new()
    }

    fn paint(
        &self,
        _info: egui::PaintCallbackInfo,
        render_pass: &mut wgpu::RenderPass<'static>,
        callback_resources: &CallbackResources,
    ) {
        if let Some(resources) = callback_resources.get::<GpuViewportResources>() {
            resources.paint(render_pass);
        }
    }
}

#[derive(Clone)]
struct GpuSceneData {
    textures: Vec<GpuTextureData>,
    materials: Vec<GpuMaterialData>,
    batches: Vec<GpuBatchData>,
}

impl GpuSceneData {
    fn from_preview(preview: &ModelPreview) -> Self {
        let mut textures = vec![GpuTextureData::white()];
        textures.extend(
            preview
                .textures
                .iter()
                .map(GpuTextureData::from_preview_texture),
        );
        let mut materials = preview
            .materials
            .iter()
            .map(|material| GpuMaterialData::from_j3d(material, preview))
            .collect::<Vec<_>>();
        let mut fallback_materials = BTreeMap::<(usize, usize, u8), usize>::new();
        let mut batch_map = BTreeMap::<(usize, usize, GpuPipelineKey), usize>::new();
        let mut batches = Vec::<GpuBatchData>::new();

        for triangle in &preview.triangles {
            let material_index = triangle
                .material_index
                .filter(|index| *index < materials.len())
                .unwrap_or_else(|| {
                    let texture = triangle.texture_index.unwrap_or(usize::MAX);
                    let mask = triangle.mask_texture_index.unwrap_or(usize::MAX);
                    let layer = render_layer_id(triangle.render_layer);
                    *fallback_materials
                        .entry((texture, mask, layer))
                        .or_insert_with(|| {
                            let index = materials.len();
                            materials.push(GpuMaterialData::fallback(triangle, preview));
                            index
                        })
                });
            let material = &materials[material_index];
            if material.state.cull == GpuCullMode::All {
                continue;
            }
            let pipeline_key = material.state.pipeline_key(triangle.render_layer);
            let batch_index = *batch_map
                .entry((triangle.packet_index, material_index, pipeline_key))
                .or_insert_with(|| {
                    let index = batches.len();
                    batches.push(GpuBatchData {
                        pipeline_key,
                        material_index,
                        packet_index: triangle.packet_index,
                        vertices: Vec::new(),
                        indices: Vec::new(),
                    });
                    index
                });
            let batch = &mut batches[batch_index];
            let base = batch.vertices.len() as u32;
            let face_normal = preview_triangle_normal(triangle);
            let legacy_colors = legacy_vertex_colors(triangle, face_normal);
            for vertex_index in 0..3 {
                let normal = triangle
                    .normals
                    .map(|normals| normals[vertex_index])
                    .unwrap_or(face_normal);
                let color0 = triangle.color_channels[0]
                    .map(|colors| color_u8_to_f32(colors[vertex_index]))
                    .or_else(|| {
                        triangle
                            .vertex_colors
                            .map(|colors| color_u8_to_f32(colors[vertex_index]))
                    })
                    .unwrap_or(legacy_colors[vertex_index]);
                let color1 = triangle.color_channels[1]
                    .map(|colors| color_u8_to_f32(colors[vertex_index]))
                    .unwrap_or([1.0; 4]);
                let tex_coords: [[f32; 2]; TEXTURE_SLOT_COUNT] = std::array::from_fn(|slot| {
                    triangle.tex_coord_sets[slot]
                        .map(|coords| coords[vertex_index])
                        .or_else(|| {
                            (slot == 0)
                                .then_some(triangle.tex_coords)
                                .flatten()
                                .map(|coords| coords[vertex_index])
                        })
                        .unwrap_or([0.0; 2])
                });
                batch.vertices.push(GpuVertex {
                    position: triangle.vertices[vertex_index],
                    normal,
                    color0,
                    color1,
                    uv0: tex_coords[0],
                    uv1: tex_coords[1],
                    uv2: tex_coords[2],
                    uv3: tex_coords[3],
                    uv4: tex_coords[4],
                    uv5: tex_coords[5],
                    uv6: tex_coords[6],
                    uv7: tex_coords[7],
                    camera_relative: camera_relative_for_render_layer(triangle.render_layer),
                });
            }
            batch.indices.extend_from_slice(&[base, base + 1, base + 2]);
        }

        Self {
            textures,
            materials,
            batches,
        }
    }
}

fn legacy_vertex_colors(triangle: &PreviewTriangle, normal: [f32; 3]) -> [[f32; 4]; 3] {
    if let Some(color) = triangle.color {
        return [color_u8_to_f32(color); 3];
    }
    let average_y = triangle
        .vertices
        .iter()
        .map(|vertex| vertex[1])
        .sum::<f32>()
        / 3.0;
    preview_solid_triangle_colors(triangle, normal, average_y).map(color32_to_f32)
}

fn color32_to_f32(color: egui::Color32) -> [f32; 4] {
    let [r, g, b, a] = color.to_srgba_unmultiplied();
    color_u8_to_f32([r, g, b, a])
}

fn color_u8_to_f32(color: [u8; 4]) -> [f32; 4] {
    color.map(|value| value as f32 / 255.0)
}

fn render_layer_id(layer: PreviewRenderLayer) -> u8 {
    match layer {
        PreviewRenderLayer::Sky => 0,
        PreviewRenderLayer::Main => 1,
        PreviewRenderLayer::Water => 2,
        PreviewRenderLayer::Goop => 3,
    }
}

fn camera_relative_for_render_layer(layer: PreviewRenderLayer) -> u32 {
    u32::from(layer == PreviewRenderLayer::Sky)
}

#[derive(Clone)]
struct GpuTextureData {
    mips: Vec<GpuTextureMip>,
    format: wgpu::TextureFormat,
    address_mode_u: wgpu::AddressMode,
    address_mode_v: wgpu::AddressMode,
    mag_filter: wgpu::FilterMode,
    min_filter: wgpu::FilterMode,
    mipmap_filter: wgpu::MipmapFilterMode,
}

#[derive(Clone)]
struct GpuTextureMip {
    size: [u32; 2],
    rgba: Vec<u8>,
}

impl GpuTextureData {
    fn white() -> Self {
        Self {
            mips: vec![GpuTextureMip {
                size: [1, 1],
                rgba: vec![255; 4],
            }],
            format: wgpu::TextureFormat::Rgba8UnormSrgb,
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Nearest,
            min_filter: wgpu::FilterMode::Nearest,
            mipmap_filter: wgpu::MipmapFilterMode::Nearest,
        }
    }

    fn from_preview_texture(texture: &PreviewTexture) -> Self {
        let max_levels = texture.mipmap_count.max(1) as usize;
        let mut mips = texture
            .mips
            .iter()
            .take(max_levels)
            .map(gpu_mip_from_color_image)
            .collect::<Vec<_>>();
        if mips.is_empty() {
            mips.push(gpu_mip_from_color_image(&texture.image));
        }
        Self {
            mips,
            format: gpu_texture_format_for_j3d(texture.format),
            address_mode_u: sampler_address_mode(texture.wrap_s),
            address_mode_v: sampler_address_mode(texture.wrap_t),
            mag_filter: sampler_mag_filter(texture.mag_filter),
            min_filter: sampler_min_filter(texture.min_filter),
            mipmap_filter: sampler_mipmap_filter(texture.min_filter, texture.mipmap_count),
        }
    }
}

fn gpu_texture_format_for_j3d(format: u8) -> wgpu::TextureFormat {
    // GX intensity formats carry numeric intensity/mask values, not sRGB
    // colors. Sampling them through an sRGB texture view darkens their RGB
    // before TEV math while leaving alpha untouched, which breaks effects
    // such as sky cloud masks that use the same intensity in both paths.
    if matches!(format, 0..=3) {
        wgpu::TextureFormat::Rgba8Unorm
    } else {
        wgpu::TextureFormat::Rgba8UnormSrgb
    }
}

fn gpu_mip_from_color_image(image: &egui::ColorImage) -> GpuTextureMip {
    let mut rgba = Vec::with_capacity(image.pixels.len() * 4);
    for pixel in &image.pixels {
        rgba.extend_from_slice(&pixel.to_srgba_unmultiplied());
    }
    GpuTextureMip {
        size: [image.size[0].max(1) as u32, image.size[1].max(1) as u32],
        rgba,
    }
}

fn sampler_address_mode(wrap: u8) -> wgpu::AddressMode {
    match wrap {
        0 => wgpu::AddressMode::ClampToEdge,
        2 => wgpu::AddressMode::MirrorRepeat,
        _ => wgpu::AddressMode::Repeat,
    }
}

fn sampler_mag_filter(filter: u8) -> wgpu::FilterMode {
    if filter == 1 {
        wgpu::FilterMode::Linear
    } else {
        wgpu::FilterMode::Nearest
    }
}

fn sampler_min_filter(filter: u8) -> wgpu::FilterMode {
    match filter {
        1 | 3 | 5 => wgpu::FilterMode::Linear,
        _ => wgpu::FilterMode::Nearest,
    }
}

fn sampler_mipmap_filter(filter: u8, mipmap_count: u8) -> wgpu::MipmapFilterMode {
    if mipmap_count <= 1 {
        return wgpu::MipmapFilterMode::Nearest;
    }
    match filter {
        2..=5 => wgpu::MipmapFilterMode::Linear,
        _ => wgpu::MipmapFilterMode::Nearest,
    }
}

#[derive(Clone)]
struct GpuMaterialData {
    uniform: GpuMaterialUniform,
    texture_indices: [usize; TEXTURE_SLOT_COUNT],
    state: GpuMaterialState,
    tex_matrices: [Option<J3dTexMatrix>; TEXTURE_SLOT_COUNT],
    animations: Vec<GpuMaterialAnimation>,
}

#[derive(Clone)]
struct GpuMaterialAnimation {
    animation: J3dTextureSrtAnimation,
    binding_index: usize,
}

impl GpuMaterialData {
    fn from_j3d(material: &J3dMaterial, preview: &ModelPreview) -> Self {
        let texture_indices = std::array::from_fn(|slot| {
            material.texture_indices[slot]
                .filter(|index| *index < preview.textures.len())
                .map(|index| index + 1)
                .unwrap_or(0)
        });
        let animations = preview
            .material_animation_bindings
            .get(material.material_index)
            .into_iter()
            .flatten()
            .filter_map(|binding| {
                preview
                    .texture_srt_animations
                    .get(binding.animation_index)
                    .cloned()
                    .map(|animation| GpuMaterialAnimation {
                        animation,
                        binding_index: binding.binding_index,
                    })
            })
            .collect();
        Self {
            uniform: GpuMaterialUniform::from_j3d(material),
            texture_indices,
            state: GpuMaterialState::from_j3d(material),
            tex_matrices: material.tex_matrices,
            animations,
        }
    }

    fn fallback(triangle: &PreviewTriangle, preview: &ModelPreview) -> Self {
        let mut uniform = GpuMaterialUniform::fallback(triangle.texture_index.is_some());
        if let Some(compare) = triangle.alpha_compare {
            uniform.set_alpha_compare(compare);
        }
        let mut texture_indices = [0; TEXTURE_SLOT_COUNT];
        texture_indices[0] = triangle
            .texture_index
            .filter(|index| *index < preview.textures.len())
            .map(|index| index + 1)
            .unwrap_or(0);
        texture_indices[1] = triangle
            .mask_texture_index
            .filter(|index| *index < preview.textures.len())
            .map(|index| index + 1)
            .unwrap_or(0);
        Self {
            uniform,
            texture_indices,
            state: GpuMaterialState {
                cull: gpu_cull_mode(triangle.cull_mode.unwrap_or(0)),
                alpha_compare: triangle.alpha_compare.unwrap_or(always_alpha_compare()),
                blend: triangle.blend_mode.unwrap_or(J3dBlendMode {
                    mode: 0,
                    src_factor: 1,
                    dst_factor: 0,
                    logic_op: 3,
                }),
                depth: triangle.z_mode.unwrap_or(default_z_mode()),
            },
            tex_matrices: [None; TEXTURE_SLOT_COUNT],
            animations: Vec::new(),
        }
    }

    fn uniform_at_time(&self, elapsed_seconds: f32) -> GpuMaterialUniform {
        let mut uniform = self.uniform;
        for animated in &self.animations {
            let Some(binding) = animated.animation.bindings.get(animated.binding_index) else {
                continue;
            };
            let slot = binding.texture_matrix_index as usize;
            let Some(mut matrix) = self.tex_matrices.get(slot).copied().flatten() else {
                continue;
            };
            let frame = animated.animation.playback_frame(elapsed_seconds);
            let srt = binding.sample(frame);
            matrix.center = binding.center;
            matrix.scale = srt.scale;
            matrix.rotation = srt.rotation;
            matrix.translation = srt.translation;
            let rows = texture_srt_rows(matrix);
            uniform.tex_matrix_rows[slot * 3..slot * 3 + 3].copy_from_slice(&rows);
        }
        uniform
    }
}

#[derive(Clone, Copy)]
struct GpuMaterialState {
    cull: GpuCullMode,
    alpha_compare: J3dAlphaCompare,
    blend: J3dBlendMode,
    depth: J3dZMode,
}

impl GpuMaterialState {
    fn from_j3d(material: &J3dMaterial) -> Self {
        Self {
            cull: gpu_cull_mode(material.cull_mode),
            alpha_compare: material.alpha_compare,
            blend: material.blend_mode,
            depth: material.z_mode,
        }
    }

    fn pipeline_key(self, render_layer: PreviewRenderLayer) -> GpuPipelineKey {
        let pass = if render_layer == PreviewRenderLayer::Sky {
            GpuBatchPass::Sky
        } else if self.blend.mode == 1 || self.blend.mode == 3 {
            GpuBatchPass::Translucent
        } else if !alpha_compare_is_always(self.alpha_compare) {
            GpuBatchPass::AlphaTest
        } else {
            GpuBatchPass::Opaque
        };
        GpuPipelineKey {
            pass,
            depth: GpuDepthState {
                write: self.depth.update_enable != 0,
                compare: if self.depth.compare_enable == 0 {
                    GpuDepthCompare::Always
                } else {
                    gx_compare_to_gpu(self.depth.func)
                },
            },
            cull: self.cull,
            blend: GpuBlendKey {
                mode: self.blend.mode,
                src_factor: self.blend.src_factor,
                dst_factor: self.blend.dst_factor,
                logic_op: self.blend.logic_op,
            },
        }
    }
}

fn always_alpha_compare() -> J3dAlphaCompare {
    J3dAlphaCompare {
        comp0: 7,
        ref0: 0,
        op: 0,
        comp1: 7,
        ref1: 0,
    }
}

fn default_z_mode() -> J3dZMode {
    J3dZMode {
        compare_enable: 1,
        func: 3,
        update_enable: 1,
    }
}

fn alpha_compare_is_always(compare: J3dAlphaCompare) -> bool {
    match compare.op {
        0 => compare.comp0 == 7 && compare.comp1 == 7,
        1 => compare.comp0 == 7 || compare.comp1 == 7,
        2 => false,
        3 => compare.comp0 == compare.comp1 && compare.ref0 == compare.ref1,
        _ => false,
    }
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct GpuMaterialUniform {
    counts: [u32; 4],
    alpha_compare: [u32; 4],
    alpha_refs: [f32; 4],
    material_colors: [[f32; 4]; 2],
    ambient_colors: [[f32; 4]; 2],
    tev_colors: [[f32; 4]; 4],
    tev_k_colors: [[f32; 4]; 4],
    color_channels: [[u32; 4]; 4],
    tex_gens: [[u32; 4]; TEXTURE_SLOT_COUNT],
    tex_matrix_rows: [[f32; 4]; TEX_MATRIX_ROW_COUNT],
    tex_effect_rows: [[f32; 4]; TEXTURE_SLOT_COUNT * 4],
    tev_orders: [[u32; 4]; TEV_STAGE_COUNT],
    tev_color_args: [[u32; 4]; TEV_STAGE_COUNT],
    tev_color_ops: [[u32; 4]; TEV_STAGE_COUNT],
    tev_alpha_args: [[u32; 4]; TEV_STAGE_COUNT],
    tev_alpha_ops: [[u32; 4]; TEV_STAGE_COUNT],
    tev_selectors: [[u32; 4]; TEV_STAGE_COUNT],
    swap_tables: [[u32; 4]; 4],
    indirect_orders: [[u32; 4]; 3],
    indirect_matrix_rows: [[f32; 4]; 6],
    indirect_matrix_meta: [[u32; 4]; 3],
    indirect_stages0: [[u32; 4]; TEV_STAGE_COUNT],
    indirect_stages1: [[u32; 4]; TEV_STAGE_COUNT],
    indirect_stages2: [[u32; 4]; TEV_STAGE_COUNT],
    fog_meta: [u32; 4],
    fog_params: [f32; 4],
    fog_color: [f32; 4],
}

impl GpuMaterialUniform {
    fn from_j3d(material: &J3dMaterial) -> Self {
        let mut uniform = Self::zeroed();
        uniform.counts = [
            material.tev_stages.len().min(TEV_STAGE_COUNT) as u32,
            material.tex_gen_count.min(TEXTURE_SLOT_COUNT as u8) as u32,
            material.color_channel_count.min(4) as u32,
            material.indirect.stage_count.min(3) as u32,
        ];
        uniform.set_alpha_compare(material.alpha_compare);
        uniform.material_colors = material.material_colors.map(color_u8_to_f32);
        uniform.ambient_colors = material.ambient_colors.map(color_u8_to_f32);
        uniform.tev_colors = material
            .tev_colors
            .map(|color| color.map(|value| value as f32 / 255.0));
        uniform.tev_k_colors = material.tev_k_colors.map(color_u8_to_f32);
        uniform.color_channels = material.color_channels.map(|channel| {
            [
                channel.enable as u32,
                channel.mat_src as u32,
                channel.amb_src as u32,
                channel.diffuse_fn as u32
                    | ((channel.attenuation_fn as u32) << 8)
                    | ((channel.light_mask as u32) << 16),
            ]
        });
        for slot in 0..TEXTURE_SLOT_COUNT {
            let tex_gen = material.tex_gens[slot];
            let matrix_slot = tex_matrix_slot(tex_gen.matrix)
                .filter(|slot| *slot < TEXTURE_SLOT_COUNT)
                .map(|slot| slot as u32 + 1)
                .unwrap_or(0);
            let matrix = tex_matrix_slot(tex_gen.matrix)
                .and_then(|slot| material.tex_matrices.get(slot).copied().flatten());
            uniform.tex_gens[slot] = [
                tex_gen.gen_type as u32,
                tex_gen.source as u32,
                matrix_slot,
                matrix
                    .map(|matrix| {
                        matrix.mode as u32
                            | ((matrix.projection as u32) << 8)
                            | ((u32::from(matrix.maya)) << 16)
                    })
                    .unwrap_or(0),
            ];
            if let Some(matrix) = material.tex_matrices[slot] {
                let rows = texture_srt_rows(matrix);
                uniform.tex_matrix_rows[slot * 3..slot * 3 + 3].copy_from_slice(&rows);
                uniform.tex_effect_rows[slot * 4..slot * 4 + 4]
                    .copy_from_slice(&matrix.effect_matrix);
            } else {
                uniform.tex_matrix_rows[slot * 3] = [1.0, 0.0, 0.0, 0.0];
                uniform.tex_matrix_rows[slot * 3 + 1] = [0.0, 1.0, 0.0, 0.0];
                uniform.tex_matrix_rows[slot * 3 + 2] = [0.0, 0.0, 1.0, 0.0];
                uniform.tex_effect_rows[slot * 4] = [1.0, 0.0, 0.0, 0.0];
                uniform.tex_effect_rows[slot * 4 + 1] = [0.0, 1.0, 0.0, 0.0];
                uniform.tex_effect_rows[slot * 4 + 2] = [0.0, 0.0, 1.0, 0.0];
                uniform.tex_effect_rows[slot * 4 + 3] = [0.0, 0.0, 0.0, 1.0];
            }
        }
        for (stage_index, stage) in material.tev_stages.iter().take(TEV_STAGE_COUNT).enumerate() {
            uniform.write_tev_stage(stage_index, *stage);
        }
        uniform.swap_tables = material.swap_tables.map(|table| table.map(u32::from));
        for slot in 0..3 {
            if let Some(order) = material.indirect.orders[slot] {
                let scale = material.indirect.scales[slot];
                uniform.indirect_orders[slot] = [
                    order.tex_coord.map(|value| value as u32 + 1).unwrap_or(0),
                    order.tex_map.map(|value| value as u32 + 1).unwrap_or(0),
                    scale.map(|value| value.scale_s as u32).unwrap_or(0),
                    scale.map(|value| value.scale_t as u32).unwrap_or(0),
                ];
            }
            if let Some(matrix) = material.indirect.matrices[slot] {
                let scale = 2.0f32.powi(matrix.scale_exponent as i32);
                uniform.indirect_matrix_rows[slot * 2] = [
                    matrix.rows[0][0] * scale,
                    matrix.rows[0][1] * scale,
                    matrix.rows[0][2] * scale,
                    0.0,
                ];
                uniform.indirect_matrix_rows[slot * 2 + 1] = [
                    matrix.rows[1][0] * scale,
                    matrix.rows[1][1] * scale,
                    matrix.rows[1][2] * scale,
                    0.0,
                ];
                uniform.indirect_matrix_meta[slot][0] = matrix.scale_exponent as i32 as u32;
            }
        }
        if let Some(fog) = material.fog {
            uniform.fog_meta = [
                fog.fog_type as u32,
                fog.adjustment_enabled as u32,
                fog.center as u32,
                0,
            ];
            uniform.fog_params = [fog.start_z, fog.end_z, fog.near_z, fog.far_z];
            uniform.fog_color = color_u8_to_f32(fog.color);
        }
        uniform
    }

    fn fallback(textured: bool) -> Self {
        let mut uniform = Self::zeroed();
        uniform.counts = [1, 1, 0, 0];
        uniform.set_alpha_compare(always_alpha_compare());
        uniform.material_colors = [[1.0; 4]; 2];
        uniform.ambient_colors = [[1.0; 4]; 2];
        uniform.tev_k_colors = [[1.0; 4]; 4];
        uniform.color_channels = [[0, 1, 1, 0], [0, 1, 1, 0], [0, 1, 1, 0], [0, 1, 1, 0]];
        uniform.tex_gens[0] = [1, 4, 0, 0];
        uniform.tex_matrix_rows[0] = [1.0, 0.0, 0.0, 0.0];
        uniform.tex_matrix_rows[1] = [0.0, 1.0, 0.0, 0.0];
        uniform.tex_matrix_rows[2] = [0.0, 0.0, 1.0, 0.0];
        uniform.tev_orders[0] = if textured { [1, 1, 4, 0] } else { [0, 0, 4, 0] };
        uniform.tev_color_args[0] = if textured {
            [15, 8, 10, 15]
        } else {
            [15, 15, 15, 10]
        };
        uniform.tev_alpha_args[0] = if textured { [7, 4, 5, 7] } else { [7, 7, 7, 5] };
        uniform.tev_color_ops[0] = [0, 0, 0, 1];
        uniform.tev_alpha_ops[0] = [0, 0, 0, 1];
        uniform.swap_tables = [[0, 1, 2, 3], [0, 0, 0, 0], [1, 1, 1, 1], [2, 2, 2, 2]];
        uniform
    }

    fn set_alpha_compare(&mut self, compare: J3dAlphaCompare) {
        self.alpha_compare = [
            compare.comp0 as u32,
            compare.op as u32,
            compare.comp1 as u32,
            1,
        ];
        self.alpha_refs = [
            compare.ref0 as f32 / 255.0,
            compare.ref1 as f32 / 255.0,
            0.0,
            0.0,
        ];
    }

    fn write_tev_stage(&mut self, index: usize, stage: J3dTevStage) {
        self.tev_orders[index] = [
            stage
                .order
                .tex_coord
                .map(|value| value as u32 + 1)
                .unwrap_or(0),
            stage
                .order
                .tex_map
                .map(|value| value as u32 + 1)
                .unwrap_or(0),
            stage.order.color_channel as u32,
            0,
        ];
        self.tev_color_args[index] = stage.color_args.map(u32::from);
        self.tev_color_ops[index] = [
            stage.color_op as u32,
            stage.color_bias as u32,
            stage.color_scale as u32,
            stage.color_clamp as u32 | ((stage.color_register as u32) << 8),
        ];
        self.tev_alpha_args[index] = stage.alpha_args.map(u32::from);
        self.tev_alpha_ops[index] = [
            stage.alpha_op as u32,
            stage.alpha_bias as u32,
            stage.alpha_scale as u32,
            stage.alpha_clamp as u32 | ((stage.alpha_register as u32) << 8),
        ];
        self.tev_selectors[index] = [
            stage.konst_color as u32,
            stage.konst_alpha as u32,
            stage.raster_swap as u32,
            stage.texture_swap as u32,
        ];
        self.indirect_stages0[index] = [
            stage.indirect.stage as u32,
            stage.indirect.format as u32,
            stage.indirect.bias as u32,
            stage.indirect.matrix as u32,
        ];
        self.indirect_stages1[index] = [
            stage.indirect.wrap_s as u32,
            stage.indirect.wrap_t as u32,
            stage.indirect.add_previous as u32,
            stage.indirect.use_original_lod as u32,
        ];
        self.indirect_stages2[index][0] = stage.indirect.alpha as u32;
    }
}

fn tex_matrix_slot(matrix: u8) -> Option<usize> {
    if matrix < 30 || matrix == 60 {
        return None;
    }
    let offset = matrix - 30;
    offset.is_multiple_of(3).then_some((offset / 3) as usize)
}

fn texture_srt_rows(matrix: J3dTexMatrix) -> [[f32; 4]; 3] {
    let radians = matrix.rotation as f32 * std::f32::consts::TAU / 65536.0;
    let (sin, cos) = radians.sin_cos();
    let mut rows = if matrix.maya {
        [
            [
                matrix.scale[0] * cos,
                matrix.scale[1] * sin,
                0.0,
                (matrix.translation[0] - 0.5) * cos
                    - sin * ((matrix.translation[1] - 0.5) + matrix.scale[1])
                    + 0.5,
            ],
            [
                -matrix.scale[0] * sin,
                matrix.scale[1] * cos,
                0.0,
                -(matrix.translation[0] - 0.5) * sin
                    - cos * ((matrix.translation[1] - 0.5) + matrix.scale[1])
                    + 0.5,
            ],
            [0.0, 0.0, 1.0, 0.0],
        ]
    } else {
        [
            [
                matrix.scale[0] * cos,
                -matrix.scale[0] * sin,
                0.0,
                -matrix.scale[0] * cos * matrix.center[0]
                    + matrix.scale[0] * sin * matrix.center[1]
                    + matrix.center[0]
                    + matrix.translation[0],
            ],
            [
                matrix.scale[1] * sin,
                matrix.scale[1] * cos,
                0.0,
                -matrix.scale[1] * sin * matrix.center[0]
                    - matrix.scale[1] * cos * matrix.center[1]
                    + matrix.center[1]
                    + matrix.translation[1],
            ],
            [0.0, 0.0, 1.0, 0.0],
        ]
    };
    if matches!(matrix.mode, 7 | 8 | 9 | 11) {
        for row in rows.iter_mut().take(2) {
            row[2] = row[3];
            row[3] = 0.0;
        }
    }
    rows
}

#[derive(Clone)]
struct GpuBatchData {
    pipeline_key: GpuPipelineKey,
    material_index: usize,
    packet_index: usize,
    vertices: Vec<GpuVertex>,
    indices: Vec<u32>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct GpuPipelineKey {
    pass: GpuBatchPass,
    depth: GpuDepthState,
    cull: GpuCullMode,
    blend: GpuBlendKey,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum GpuBatchPass {
    Sky,
    Opaque,
    AlphaTest,
    Translucent,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct GpuDepthState {
    write: bool,
    compare: GpuDepthCompare,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum GpuDepthCompare {
    Never,
    Less,
    Equal,
    LessEqual,
    Greater,
    NotEqual,
    GreaterEqual,
    Always,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum GpuCullMode {
    None,
    Front,
    Back,
    All,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct GpuBlendKey {
    mode: u8,
    src_factor: u8,
    dst_factor: u8,
    logic_op: u8,
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct GpuVertex {
    position: [f32; 3],
    normal: [f32; 3],
    color0: [f32; 4],
    color1: [f32; 4],
    uv0: [f32; 2],
    uv1: [f32; 2],
    uv2: [f32; 2],
    uv3: [f32; 2],
    uv4: [f32; 2],
    uv5: [f32; 2],
    uv6: [f32; 2],
    uv7: [f32; 2],
    camera_relative: u32,
}

impl GpuVertex {
    const ATTRIBUTES: [wgpu::VertexAttribute; 13] = wgpu::vertex_attr_array![
        0 => Float32x3,
        1 => Float32x3,
        2 => Float32x4,
        3 => Float32x4,
        4 => Float32x2,
        5 => Float32x2,
        6 => Float32x2,
        7 => Float32x2,
        8 => Float32x2,
        9 => Float32x2,
        10 => Float32x2,
        11 => Float32x2,
        12 => Uint32
    ];

    fn layout() -> wgpu::VertexBufferLayout<'static> {
        wgpu::VertexBufferLayout {
            array_stride: std::mem::size_of::<Self>() as wgpu::BufferAddress,
            step_mode: wgpu::VertexStepMode::Vertex,
            attributes: &Self::ATTRIBUTES,
        }
    }
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct GpuCameraUniform {
    camera_position: [f32; 4],
    right: [f32; 4],
    up: [f32; 4],
    forward: [f32; 4],
    projection: [f32; 4],
    clip: [f32; 4],
}

impl From<GpuViewportFrame> for GpuCameraUniform {
    fn from(frame: GpuViewportFrame) -> Self {
        let half_width = (frame.viewport_size[0] * 0.5).max(1.0);
        let half_height = (frame.viewport_size[1] * 0.5).max(1.0);
        Self {
            camera_position: vec4(frame.camera_position, 0.0),
            right: vec4(frame.right, 0.0),
            up: vec4(frame.up, 0.0),
            forward: vec4(frame.forward, 0.0),
            projection: [
                frame.focal / half_width,
                frame.focal / half_height,
                frame.viewport_pan[0] / half_width,
                -frame.viewport_pan[1] / half_height,
            ],
            clip: [frame.near, frame.far, 0.0, 0.0],
        }
    }
}

struct GpuViewportResources {
    pipeline_layout: wgpu::PipelineLayout,
    material_layout: wgpu::BindGroupLayout,
    depth_clear_pipeline: wgpu::RenderPipeline,
    target_format: wgpu::TextureFormat,
    pipelines: BTreeMap<GpuPipelineKey, wgpu::RenderPipeline>,
    camera_buffer: wgpu::Buffer,
    camera_bind_group: wgpu::BindGroup,
    textures: Vec<GpuTextureResource>,
    material_buffers: Vec<wgpu::Buffer>,
    material_bind_groups: Vec<wgpu::BindGroup>,
    batches: Vec<GpuBatchResources>,
    draw_order: Vec<GpuDrawCommand>,
    generation: u64,
}

impl GpuViewportResources {
    fn new(device: &wgpu::Device, target_format: wgpu::TextureFormat) -> Self {
        let camera_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("sms viewport camera layout"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            }],
        });
        let mut material_entries = vec![wgpu::BindGroupLayoutEntry {
            binding: 0,
            visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
            ty: wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Uniform,
                has_dynamic_offset: false,
                min_binding_size: None,
            },
            count: None,
        }];
        for slot in 0..TEXTURE_SLOT_COUNT {
            material_entries.push(wgpu::BindGroupLayoutEntry {
                binding: 1 + slot as u32 * 2,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Float { filterable: true },
                    view_dimension: wgpu::TextureViewDimension::D2,
                    multisampled: false,
                },
                count: None,
            });
            material_entries.push(wgpu::BindGroupLayoutEntry {
                binding: 2 + slot as u32 * 2,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                count: None,
            });
        }
        let material_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("sms viewport J3D material layout"),
            entries: &material_entries,
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("sms viewport J3D pipeline layout"),
            bind_group_layouts: &[Some(&camera_layout), Some(&material_layout)],
            immediate_size: 0,
        });
        let camera_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("sms viewport camera buffer"),
            size: std::mem::size_of::<GpuCameraUniform>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let camera_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("sms viewport camera bind group"),
            layout: &camera_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: camera_buffer.as_entire_binding(),
            }],
        });
        Self {
            pipeline_layout,
            material_layout,
            depth_clear_pipeline: create_depth_clear_pipeline(device, target_format),
            target_format,
            pipelines: BTreeMap::new(),
            camera_buffer,
            camera_bind_group,
            textures: Vec::new(),
            material_buffers: Vec::new(),
            material_bind_groups: Vec::new(),
            batches: Vec::new(),
            draw_order: Vec::new(),
            generation: 0,
        }
    }

    fn ensure_scene(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        scene: &GpuSceneData,
        generation: u64,
    ) {
        if self.generation == generation {
            return;
        }
        for batch in &scene.batches {
            self.ensure_pipeline(device, batch.pipeline_key);
        }
        self.textures = scene
            .textures
            .iter()
            .map(|texture| GpuTextureResource::new(device, queue, texture))
            .collect();
        self.material_buffers.clear();
        self.material_bind_groups.clear();
        for (index, material) in scene.materials.iter().enumerate() {
            let buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("sms viewport J3D material uniform"),
                contents: bytemuck::bytes_of(&material.uniform),
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            });
            let mut entries = vec![wgpu::BindGroupEntry {
                binding: 0,
                resource: buffer.as_entire_binding(),
            }];
            for slot in 0..TEXTURE_SLOT_COUNT {
                let texture_index = material.texture_indices[slot].min(self.textures.len() - 1);
                let texture = &self.textures[texture_index];
                entries.push(wgpu::BindGroupEntry {
                    binding: 1 + slot as u32 * 2,
                    resource: wgpu::BindingResource::TextureView(&texture.view),
                });
                entries.push(wgpu::BindGroupEntry {
                    binding: 2 + slot as u32 * 2,
                    resource: wgpu::BindingResource::Sampler(&texture.sampler),
                });
            }
            let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some(&format!("sms viewport J3D material {index}")),
                layout: &self.material_layout,
                entries: &entries,
            });
            self.material_buffers.push(buffer);
            self.material_bind_groups.push(bind_group);
        }
        self.batches = scene
            .batches
            .iter()
            .filter(|batch| !batch.vertices.is_empty() && !batch.indices.is_empty())
            .map(|batch| GpuBatchResources::new(device, batch))
            .collect();
        self.draw_order = sorted_gpu_draw_order(&self.batches);
        self.generation = generation;
    }

    fn ensure_pipeline(&mut self, device: &wgpu::Device, key: GpuPipelineKey) {
        if !self.pipelines.contains_key(&key) {
            let pipeline = create_pipeline(device, &self.pipeline_layout, self.target_format, key);
            self.pipelines.insert(key, pipeline);
        }
    }

    fn write_frame(&mut self, queue: &wgpu::Queue, frame: GpuViewportFrame, scene: &GpuSceneData) {
        queue.write_buffer(
            &self.camera_buffer,
            0,
            bytemuck::bytes_of(&GpuCameraUniform::from(frame)),
        );
        for (material, buffer) in scene.materials.iter().zip(&self.material_buffers) {
            if material.animations.is_empty() {
                continue;
            }
            let uniform = material.uniform_at_time(frame.animation_seconds);
            queue.write_buffer(buffer, 0, bytemuck::bytes_of(&uniform));
        }
    }

    fn paint(&self, render_pass: &mut wgpu::RenderPass<'static>) {
        render_pass.set_pipeline(&self.depth_clear_pipeline);
        render_pass.draw(0..3, 0..1);
        render_pass.set_bind_group(0, &self.camera_bind_group, &[]);
        for command in &self.draw_order {
            let Some(batch) = self.batches.get(command.batch_index) else {
                continue;
            };
            let Some(pipeline) = self.pipelines.get(&batch.pipeline_key) else {
                continue;
            };
            let Some(material) = self.material_bind_groups.get(batch.material_index) else {
                continue;
            };
            render_pass.set_pipeline(pipeline);
            render_pass.set_bind_group(1, material, &[]);
            render_pass.set_vertex_buffer(0, batch.vertex_buffer.slice(..));
            render_pass.set_index_buffer(batch.index_buffer.slice(..), wgpu::IndexFormat::Uint32);
            render_pass.draw_indexed(0..batch.index_count, 0, 0..1);
        }
    }
}

struct GpuTextureResource {
    _texture: wgpu::Texture,
    view: wgpu::TextureView,
    sampler: wgpu::Sampler,
}

impl GpuTextureResource {
    fn new(device: &wgpu::Device, queue: &wgpu::Queue, data: &GpuTextureData) -> Self {
        let base_mip = data.mips.first().expect("viewport texture has a base mip");
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("sms viewport J3D texture"),
            size: wgpu::Extent3d {
                width: base_mip.size[0],
                height: base_mip.size[1],
                depth_or_array_layers: 1,
            },
            mip_level_count: data.mips.len() as u32,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: data.format,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        for (level, mip) in data.mips.iter().enumerate() {
            queue.write_texture(
                wgpu::TexelCopyTextureInfo {
                    texture: &texture,
                    mip_level: level as u32,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                &mip.rgba,
                wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(mip.size[0] * 4),
                    rows_per_image: Some(mip.size[1]),
                },
                wgpu::Extent3d {
                    width: mip.size[0],
                    height: mip.size[1],
                    depth_or_array_layers: 1,
                },
            );
        }
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("sms viewport J3D sampler"),
            address_mode_u: data.address_mode_u,
            address_mode_v: data.address_mode_v,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: data.mag_filter,
            min_filter: data.min_filter,
            mipmap_filter: data.mipmap_filter,
            lod_min_clamp: 0.0,
            lod_max_clamp: data.mips.len().saturating_sub(1) as f32,
            ..Default::default()
        });
        Self {
            _texture: texture,
            view,
            sampler,
        }
    }
}

struct GpuBatchResources {
    pipeline_key: GpuPipelineKey,
    material_index: usize,
    packet_index: usize,
    vertex_buffer: wgpu::Buffer,
    index_buffer: wgpu::Buffer,
    index_count: u32,
}

impl GpuBatchResources {
    fn new(device: &wgpu::Device, batch: &GpuBatchData) -> Self {
        Self {
            pipeline_key: batch.pipeline_key,
            material_index: batch.material_index,
            packet_index: batch.packet_index,
            vertex_buffer: device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("sms viewport J3D vertex buffer"),
                contents: bytemuck::cast_slice(&batch.vertices),
                usage: wgpu::BufferUsages::VERTEX,
            }),
            index_buffer: device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("sms viewport J3D index buffer"),
                contents: bytemuck::cast_slice(&batch.indices),
                usage: wgpu::BufferUsages::INDEX,
            }),
            index_count: batch.indices.len() as u32,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct GpuDrawCommand {
    batch_index: usize,
}

fn sorted_gpu_draw_order(batches: &[GpuBatchResources]) -> Vec<GpuDrawCommand> {
    sorted_gpu_draw_order_from_info(batches.iter().enumerate().map(|(batch_index, batch)| {
        GpuDrawBatchInfo {
            batch_index,
            pass: batch.pipeline_key.pass,
            material_index: batch.material_index,
            packet_index: batch.packet_index,
        }
    }))
}

#[derive(Clone, Copy)]
struct GpuDrawBatchInfo {
    batch_index: usize,
    pass: GpuBatchPass,
    material_index: usize,
    packet_index: usize,
}

fn sorted_gpu_draw_order_from_info(
    batches: impl IntoIterator<Item = GpuDrawBatchInfo>,
) -> Vec<GpuDrawCommand> {
    let mut solid = Vec::<(usize, usize, usize)>::new();
    let mut translucent = Vec::<(usize, usize, usize)>::new();
    let mut sky = Vec::<(usize, usize, usize)>::new();
    for batch in batches {
        let entry = (batch.material_index, batch.packet_index, batch.batch_index);
        match batch.pass {
            GpuBatchPass::Sky => sky.push(entry),
            GpuBatchPass::Translucent => translucent.push(entry),
            GpuBatchPass::Opaque | GpuBatchPass::AlphaTest => solid.push(entry),
        }
    }
    sky.sort_unstable();
    solid.sort_unstable();
    translucent.sort_unstable();
    sky.into_iter()
        .chain(solid)
        .chain(translucent)
        .map(|(_, _, batch_index)| GpuDrawCommand { batch_index })
        .collect()
}

fn create_depth_clear_pipeline(
    device: &wgpu::Device,
    target_format: wgpu::TextureFormat,
) -> wgpu::RenderPipeline {
    let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("sms viewport depth clear layout"),
        bind_group_layouts: &[],
        immediate_size: 0,
    });
    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("sms viewport depth clear shader"),
        source: wgpu::ShaderSource::Wgsl(DEPTH_CLEAR_SHADER.into()),
    });
    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("sms viewport depth clear pipeline"),
        layout: Some(&layout),
        vertex: wgpu::VertexState {
            module: &shader,
            entry_point: Some("vs_main"),
            compilation_options: Default::default(),
            buffers: &[],
        },
        fragment: Some(wgpu::FragmentState {
            module: &shader,
            entry_point: Some("fs_main"),
            compilation_options: Default::default(),
            targets: &[Some(wgpu::ColorTargetState {
                format: target_format,
                blend: None,
                write_mask: wgpu::ColorWrites::empty(),
            })],
        }),
        primitive: wgpu::PrimitiveState::default(),
        depth_stencil: Some(wgpu::DepthStencilState {
            format: DEPTH_FORMAT,
            depth_write_enabled: Some(true),
            depth_compare: Some(wgpu::CompareFunction::Always),
            stencil: Default::default(),
            bias: Default::default(),
        }),
        multisample: wgpu::MultisampleState::default(),
        multiview_mask: None,
        cache: None,
    })
}

fn create_pipeline(
    device: &wgpu::Device,
    layout: &wgpu::PipelineLayout,
    target_format: wgpu::TextureFormat,
    key: GpuPipelineKey,
) -> wgpu::RenderPipeline {
    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("sms viewport J3D TEV shader"),
        source: wgpu::ShaderSource::Wgsl(J3D_SHADER.into()),
    });
    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("sms viewport J3D pipeline"),
        layout: Some(layout),
        vertex: wgpu::VertexState {
            module: &shader,
            entry_point: Some("vs_main"),
            compilation_options: Default::default(),
            buffers: &[GpuVertex::layout()],
        },
        fragment: Some(wgpu::FragmentState {
            module: &shader,
            entry_point: Some("fs_main"),
            compilation_options: Default::default(),
            targets: &[Some(wgpu::ColorTargetState {
                format: target_format,
                blend: gpu_blend_state(key.blend),
                write_mask: wgpu::ColorWrites::ALL,
            })],
        }),
        primitive: wgpu::PrimitiveState {
            topology: wgpu::PrimitiveTopology::TriangleList,
            strip_index_format: None,
            // The editor's positive-depth camera basis mirrors GX clip-space winding.
            // Preserve J3D's GX_CULL_FRONT/BACK semantics by declaring GX faces CW.
            front_face: wgpu::FrontFace::Cw,
            cull_mode: wgpu_cull_mode(key.cull),
            polygon_mode: wgpu::PolygonMode::Fill,
            unclipped_depth: false,
            conservative: false,
        },
        depth_stencil: Some(wgpu::DepthStencilState {
            format: DEPTH_FORMAT,
            depth_write_enabled: Some(key.depth.write),
            depth_compare: Some(wgpu_depth_compare(key.depth.compare)),
            stencil: Default::default(),
            bias: Default::default(),
        }),
        multisample: wgpu::MultisampleState::default(),
        multiview_mask: None,
        cache: None,
    })
}

fn gpu_blend_state(key: GpuBlendKey) -> Option<wgpu::BlendState> {
    match key.mode {
        1 => Some(wgpu::BlendState {
            color: wgpu::BlendComponent {
                src_factor: gx_blend_factor(key.src_factor),
                dst_factor: gx_blend_factor(key.dst_factor),
                operation: wgpu::BlendOperation::Add,
            },
            alpha: wgpu::BlendComponent {
                src_factor: gx_blend_factor(key.src_factor),
                dst_factor: gx_blend_factor(key.dst_factor),
                operation: wgpu::BlendOperation::Add,
            },
        }),
        3 => Some(wgpu::BlendState {
            color: wgpu::BlendComponent {
                src_factor: wgpu::BlendFactor::One,
                dst_factor: wgpu::BlendFactor::One,
                operation: wgpu::BlendOperation::Subtract,
            },
            alpha: wgpu::BlendComponent {
                src_factor: wgpu::BlendFactor::One,
                dst_factor: wgpu::BlendFactor::One,
                operation: wgpu::BlendOperation::Subtract,
            },
        }),
        _ => None,
    }
}

fn gx_blend_factor(factor: u8) -> wgpu::BlendFactor {
    match factor {
        0 => wgpu::BlendFactor::Zero,
        1 => wgpu::BlendFactor::One,
        2 => wgpu::BlendFactor::Src,
        3 => wgpu::BlendFactor::OneMinusSrc,
        4 => wgpu::BlendFactor::SrcAlpha,
        5 => wgpu::BlendFactor::OneMinusSrcAlpha,
        6 => wgpu::BlendFactor::DstAlpha,
        7 => wgpu::BlendFactor::OneMinusDstAlpha,
        _ => wgpu::BlendFactor::One,
    }
}

fn gpu_cull_mode(cull_mode: u8) -> GpuCullMode {
    match cull_mode {
        1 => GpuCullMode::Front,
        2 => GpuCullMode::Back,
        3 => GpuCullMode::All,
        _ => GpuCullMode::None,
    }
}

fn gx_compare_to_gpu(compare: u8) -> GpuDepthCompare {
    match compare {
        0 => GpuDepthCompare::Never,
        1 => GpuDepthCompare::Less,
        2 => GpuDepthCompare::Equal,
        3 => GpuDepthCompare::LessEqual,
        4 => GpuDepthCompare::Greater,
        5 => GpuDepthCompare::NotEqual,
        6 => GpuDepthCompare::GreaterEqual,
        7 => GpuDepthCompare::Always,
        _ => GpuDepthCompare::LessEqual,
    }
}

fn wgpu_depth_compare(compare: GpuDepthCompare) -> wgpu::CompareFunction {
    match compare {
        GpuDepthCompare::Never => wgpu::CompareFunction::Never,
        GpuDepthCompare::Less => wgpu::CompareFunction::Less,
        GpuDepthCompare::Equal => wgpu::CompareFunction::Equal,
        GpuDepthCompare::LessEqual => wgpu::CompareFunction::LessEqual,
        GpuDepthCompare::Greater => wgpu::CompareFunction::Greater,
        GpuDepthCompare::NotEqual => wgpu::CompareFunction::NotEqual,
        GpuDepthCompare::GreaterEqual => wgpu::CompareFunction::GreaterEqual,
        GpuDepthCompare::Always => wgpu::CompareFunction::Always,
    }
}

fn wgpu_cull_mode(cull_mode: GpuCullMode) -> Option<wgpu::Face> {
    match cull_mode {
        GpuCullMode::None | GpuCullMode::All => None,
        GpuCullMode::Front => Some(wgpu::Face::Front),
        GpuCullMode::Back => Some(wgpu::Face::Back),
    }
}

fn vec4(value: [f32; 3], w: f32) -> [f32; 4] {
    [value[0], value[1], value[2], w]
}

#[cfg(test)]
mod tests {
    use super::*;
    use sms_formats::{
        J3dColorChannel, J3dIndirectMaterial, J3dTevOrder, J3dTexGen, SMS_MAP_MODEL_LOAD_FLAGS,
    };

    #[test]
    fn material_uniform_is_uniform_buffer_aligned() {
        assert_eq!(std::mem::size_of::<GpuMaterialUniform>() % 16, 0);
        assert_eq!(std::mem::align_of::<GpuMaterialUniform>(), 4);
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
    fn only_sky_vertices_are_camera_relative() {
        assert_eq!(camera_relative_for_render_layer(PreviewRenderLayer::Sky), 1);
        assert_eq!(
            camera_relative_for_render_layer(PreviewRenderLayer::Main),
            0
        );
        assert_eq!(
            camera_relative_for_render_layer(PreviewRenderLayer::Water),
            0
        );
    }

    #[test]
    fn gx_intensity_textures_are_sampled_in_linear_space() {
        for format in 0..=3 {
            assert_eq!(
                gpu_texture_format_for_j3d(format),
                wgpu::TextureFormat::Rgba8Unorm
            );
        }
        assert_eq!(
            gpu_texture_format_for_j3d(6),
            wgpu::TextureFormat::Rgba8UnormSrgb
        );
    }

    #[test]
    fn gx_source_color_blend_factors_do_not_change_with_blend_slot() {
        assert_eq!(gx_blend_factor(2), wgpu::BlendFactor::Src);
        assert_eq!(gx_blend_factor(3), wgpu::BlendFactor::OneMinusSrc);
    }

    #[test]
    fn disabled_depth_compare_maps_to_always() {
        let mut material = test_material(1);
        material.z_mode.compare_enable = 0;
        let key = GpuMaterialState::from_j3d(&material).pipeline_key(PreviewRenderLayer::Main);
        assert_eq!(key.depth.compare, GpuDepthCompare::Always);
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
}
