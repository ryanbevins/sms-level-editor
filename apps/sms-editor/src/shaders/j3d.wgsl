struct Camera {
    camera_position: vec4<f32>,
    right: vec4<f32>,
    up: vec4<f32>,
    forward: vec4<f32>,
    projection: vec4<f32>,
    clip: vec4<f32>,
    light_position: vec4<f32>,
    light_color: vec4<f32>,
    ambient_color: vec4<f32>,
    lighting_meta: vec4<f32>,
    render_target_size: vec4<f32>,
};

struct Material {
    counts: vec4<u32>,
    alpha_compare: vec4<u32>,
    alpha_refs: vec4<f32>,
    texture_sizes: array<vec4<f32>, 8>,
    texture_lod_parameters: array<vec4<f32>, 8>,
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
    runtime_parameters: vec4<f32>,
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
    // 0 = world, 1 = camera-relative world (sky), 2 = GX view space (shimmer),
    // 3 = camera-facing JPA billboard, 4 = billboarded JPA EFB distortion,
    // 5 = mirror surface with projective reflection coordinates,
    // 6 = world-space water sampling the screen copy,
    // 7 = camera-centered TMapObjWave foam;
    // normal.xy is half-size and normal.z rotation for both.
    @location(12) coordinate_space: u32,
    // xyz = joint center, w = 0 none / 1 full / 2 Y-axis billboard.
    @location(13) billboard_center_mode: vec4<f32>,
    @location(14) billboard_offset: vec3<f32>,
    @location(15) billboard_axis_y: vec3<f32>,
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
    @location(11) @interpolate(flat) coordinate_space: u32,
    @location(12) mirror_coord: vec3<f32>,
};

@group(0) @binding(0)
var<uniform> camera: Camera;

@group(0) @binding(1)
var<uniform> mirror_camera: Camera;

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

fn projected_world_coord(projection_camera: Camera, position: vec3<f32>) -> vec3<f32> {
    let relative = position - projection_camera.camera_position.xyz;
    let view_position = vec3<f32>(
        dot(relative, projection_camera.right.xyz),
        dot(relative, projection_camera.up.xyz),
        dot(relative, projection_camera.forward.xyz),
    );
    let depth = view_position.z;
    let clip_x = view_position.x * projection_camera.projection.x
        + projection_camera.projection.z * depth;
    let clip_y = view_position.y * projection_camera.projection.y
        + projection_camera.projection.w * depth;
    return vec3<f32>(
        0.5 * (clip_x + depth),
        0.5 * (depth - clip_y),
        depth,
    );
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
    let stored_ambient = select(
        material.ambient_colors[material_index],
        camera.ambient_color,
        material_index == 0u && camera.lighting_meta.x > 0.5,
    );
    let ambient = channel_source(
        ambient_source,
        vertex_color,
        stored_ambient,
    );
    let packed = control.w;
    let diffuse_function = packed & 0xffu;
    let attenuation_function = (packed >> 8u) & 0xffu;
    let light_mask = (packed >> 16u) & 0xffu;
    let light_position = camera.light_position.xyz;
    let light_color = camera.light_color;
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
            mat * (ambient + light_color * enabled_specular),
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
    let enabled_diffuse = select(0.0, diffuse, (light_mask & 0x01u) != 0u);
    return clamp(mat * (ambient + light_color * enabled_diffuse), vec4<f32>(0.0), vec4<f32>(1.0));
}

@vertex
fn vs_main(input: VertexIn) -> VertexOut {
    // TSky::perform translates sky.bmd to the camera every frame. Keeping the
    // model in camera-local space here reproduces that behavior without
    // rebuilding its vertex buffer while the editor camera moves.
    var world_position = input.position;
    if (input.coordinate_space == 7u) {
        // TMapObjWave centers its 5200-unit grid on Mario and evaluates both
        // sine components in world space. The free editor camera substitutes
        // for Mario so the close-range foam remains under the viewpoint.
        world_position.x += camera.camera_position.x;
        world_position.z += camera.camera_position.z;
        let seconds = material.runtime_parameters.x;
        world_position.y = 30.0 * sin(0.02 * (world_position.x / 6.28318530718) + 0.6 * seconds)
            + 25.0 * sin(0.03 * (world_position.z / 6.28318530718) + 0.9 * seconds);
    }
    let rel = select(
        world_position - camera.camera_position.xyz,
        world_position,
        input.coordinate_space == 1u || input.coordinate_space == 2u,
    );
    var view_position = vec3<f32>(
        dot(rel, camera.right.xyz),
        dot(rel, camera.up.xyz),
        dot(rel, camera.forward.xyz),
    );
    var view_normal = normalize(vec3<f32>(
        dot(input.normal, camera.right.xyz),
        dot(input.normal, camera.up.xyz),
        dot(input.normal, camera.forward.xyz),
    ));
    let billboard_mode = u32(round(input.billboard_center_mode.w));
    if (billboard_mode != 0u && input.coordinate_space != 3u && input.coordinate_space != 4u) {
        // J3DModel::calcBBoard runs after the model matrix is concatenated with
        // the view matrix. A full billboard replaces that view-space 3x3 with
        // its axis scales; a Y billboard retains the model's Y axis and builds
        // the other axes from camera-right. The offsets were extracted from
        // the original rigid joint matrix on the CPU.
        let center_rel = input.billboard_center_mode.xyz - camera.camera_position.xyz;
        let center_view = vec3<f32>(
            dot(center_rel, camera.right.xyz),
            dot(center_rel, camera.up.xyz),
            dot(center_rel, camera.forward.xyz),
        );
        if (billboard_mode == 1u) {
            // GX view space looks down -Z while the editor's depth convention
            // is +Z forward. Billboard-local depth must cross that boundary
            // too; otherwise front-facing glow shells sit behind their model.
            view_position = center_view + vec3<f32>(
                input.billboard_offset.xy,
                -input.billboard_offset.z,
            );
            view_normal = normalize(vec3<f32>(input.normal.xy, -input.normal.z));
        } else {
            let axis_y_view = normalize(vec3<f32>(
                dot(input.billboard_axis_y, camera.right.xyz),
                dot(input.billboard_axis_y, camera.up.xyz),
                dot(input.billboard_axis_y, camera.forward.xyz),
            ));
            let axis_x_view = vec3<f32>(1.0, 0.0, 0.0);
            let axis_z_view = -normalize(cross(axis_x_view, axis_y_view));
            view_position = center_view
                + axis_x_view * input.billboard_offset.x
                + axis_y_view * input.billboard_offset.y
                + axis_z_view * input.billboard_offset.z;
            view_normal = normalize(
                axis_x_view * input.normal.x
                    + axis_y_view * input.normal.y
                    + axis_z_view * input.normal.z,
            );
        }
    } else if (input.coordinate_space == 2u) {
        // TShimmer cancels the camera view matrix before drawing its -Z-facing
        // quad, so its model coordinates arrive in GX view space.
        view_position = vec3<f32>(input.position.xy, -input.position.z);
        view_normal = normalize(vec3<f32>(input.normal.xy, -input.normal.z));
    } else if (input.coordinate_space == 3u || input.coordinate_space == 4u) {
        // JPADraw assigns V=0 to positive local Y. ESP1 pivots use 1 as the
        // center, with 0 and 2 anchoring the corresponding sprite edge.
        let pivot = input.billboard_center_mode.xy;
        let corner = vec2<f32>(
            input.uv1.x * 2.0 - 1.0 + (1.0 - pivot.x),
            1.0 - input.uv1.y * 2.0 + (pivot.y - 1.0),
        );
        let angle = input.normal.z * 3.14159265359;
        let particle_type = billboard_mode;
        if ((particle_type == 3u || particle_type == 4u)
            && dot(input.billboard_axis_y, input.billboard_axis_y) > 0.000001) {
            // JPA directional shapes are real 3D quads. Their Y axis follows
            // the selected particle vector; rotation type Y spins only the
            // narrow axis around that vector. Treating the angle as a 2D
            // billboard rotation turns radial rays into vertical columns.
            let direction = normalize(input.billboard_axis_y);
            var side = cross(vec3<f32>(0.0, 1.0, 0.0), direction);
            if (dot(side, side) <= 0.000001) {
                side = cross(vec3<f32>(1.0, 0.0, 0.0), direction);
            }
            side = normalize(side);
            let across = normalize(cross(direction, side));
            let rotated_across = across * cos(angle) + side * sin(angle);
            let world_offset = rotated_across * (corner.x * input.normal.x)
                + direction * (corner.y * input.normal.y);
            view_position += vec3<f32>(
                dot(world_offset, camera.right.xyz),
                dot(world_offset, camera.up.xyz),
                dot(world_offset, camera.forward.xyz),
            );
        } else if (particle_type != 255u) {
            let rotated = vec2<f32>(
                corner.x * cos(angle) - corner.y * sin(angle),
                corner.x * sin(angle) + corner.y * cos(angle),
            );
            let billboard_offset = rotated * input.normal.xy;
            view_position.x = view_position.x + billboard_offset.x;
            view_position.y = view_position.y + billboard_offset.y;
        }
        view_normal = vec3<f32>(0.0, 0.0, -1.0);
    }
    let depth = view_position.z;
    let clip_x = view_position.x * camera.projection.x + camera.projection.z * depth;
    let clip_y = view_position.y * camera.projection.y + camera.projection.w * depth;
    // Infinite-far perspective projection. This retains the near clip while
    // allowing an isolated level to remain visible at any camera distance.
    let clip_z = depth - camera.clip.x;

    let rgb0 = compute_color_channel(0u, 0u, input.color0, input.normal, world_position);
    let alpha0 = compute_color_channel(1u, 0u, input.color0, input.normal, world_position);
    let rgb1 = compute_color_channel(2u, 1u, input.color1, input.normal, world_position);
    let alpha1 = compute_color_channel(3u, 1u, input.color1, input.normal, world_position);
    let channel0 = vec4<f32>(rgb0.rgb, alpha0.a);
    let channel1 = vec4<f32>(rgb1.rgb, alpha1.a);

    var coords: array<vec3<f32>, 8>;
    for (var i = 0u; i < 8u; i = i + 1u) {
        if (i >= material.counts.y) {
            break;
        }
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
            if (input.coordinate_space == 7u && (source == 4u || source == 5u)) {
                let seconds = material.runtime_parameters.x;
                if (source == 4u) {
                    source_value.x += camera.camera_position.x * 0.0012 + seconds * 0.045;
                    source_value.y += camera.camera_position.z * 0.0012;
                } else {
                    source_value.x += camera.camera_position.x * 0.0012;
                    source_value.y += camera.camera_position.z * 0.0015 + seconds * 0.045;
                }
            }
        } else if (source >= 12u && source <= 18u) {
            let prior = coords[source - 12u];
            source_value = vec4<f32>(prior, 1.0);
        } else if (source == 19u) {
            source_value = vec4<f32>(channel0.rg, 0.0, 1.0);
        } else if (source == 20u) {
            source_value = vec4<f32>(channel1.rg, 0.0, 1.0);
        }

        let matrix_plus_one = config.z;
        if (input.coordinate_space == 5u && i == 0u) {
            // TMirrorModelManager projects the mirror-camera texture
            // through texgen 0 after reflecting the live camera across the
            // authored mirror plane.
            // Keep S/T/Q homogeneous here. Dividing at each vertex makes the
            // large mirror00 polygons warp affinely as the camera rotates.
            coords[i] = projected_world_coord(mirror_camera, input.position);
            continue;
        }
        if (matrix_plus_one != 0u) {
            let matrix_index = matrix_plus_one - 1u;
            let uses_effect = mode == 2u || mode == 3u || mode == 4u || mode == 5u
                || mode == 8u || mode == 9u || mode == 10u || mode == 11u;
            if (uses_effect) {
                source_value = apply_effect_matrix(matrix_index, source_value);
            }
            if (mode == 6u || mode == 10u) {
                source_value = vec4<f32>(
                    0.5 * source_value.x + 0.5,
                    -0.5 * source_value.y + 0.5,
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

    var out: VertexOut;
    out.position = vec4<f32>(clip_x, clip_y, clip_z, depth);
    out.color0 = channel0;
    out.color1 = channel1;
    out.uv0 = generated_uv(coords, 0u);
    out.uv1 = generated_uv(coords, 1u);
    out.uv2 = generated_uv(coords, 2u);
    out.uv3 = generated_uv(coords, 3u);
    out.uv4 = generated_uv(coords, 4u);
    out.uv5 = generated_uv(coords, 5u);
    out.uv6 = generated_uv(coords, 6u);
    out.uv7 = generated_uv(coords, 7u);
    out.view_depth = depth;
    out.coordinate_space = input.coordinate_space;
    out.mirror_coord = projected_world_coord(mirror_camera, world_position);
    return out;
}

fn tex_coord(input: VertexOut, index: u32) -> vec2<f32> {
    if ((input.coordinate_space == 2u || input.coordinate_space == 6u) && index == 1u) {
        // SMS_GetLightPerspectiveForEffectMtx projects both the view-space
        // shimmer quad and SeaIndirect's world-space mesh onto the screen
        // texture. Derive the coordinate per fragment so SeaIndirect's large
        // polygons cannot introduce affine projection error.
        return input.position.xy / vec2<f32>(textureDimensions(texture1));
    }
    if (input.coordinate_space == 5u && index == 0u) {
        let q = select(input.mirror_coord.z, 0.000001, abs(input.mirror_coord.z) < 0.000001);
        return input.mirror_coord.xy / q;
    }
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

fn screen_copy_sample(coordinate_space: u32, slot: u32) -> bool {
    return ((coordinate_space == 2u
            || coordinate_space == 4u
            || coordinate_space == 6u) && slot == 1u)
        || (coordinate_space == 5u && slot == 0u);
}

fn sample_texture_level_zero(slot: u32, uv: vec2<f32>) -> vec4<f32> {
    switch slot {
        case 0u: { return textureSampleLevel(texture0, sampler0, uv, 0.0); }
        case 1u: { return textureSampleLevel(texture1, sampler1, uv, 0.0); }
        case 2u: { return textureSampleLevel(texture2, sampler2, uv, 0.0); }
        case 3u: { return textureSampleLevel(texture3, sampler3, uv, 0.0); }
        case 4u: { return textureSampleLevel(texture4, sampler4, uv, 0.0); }
        case 5u: { return textureSampleLevel(texture5, sampler5, uv, 0.0); }
        case 6u: { return textureSampleLevel(texture6, sampler6, uv, 0.0); }
        case 7u: { return textureSampleLevel(texture7, sampler7, uv, 0.0); }
        default: { return vec4<f32>(1.0); }
    }
}

fn sample_texture(
    coordinate_space: u32,
    slot: u32,
    uv: vec2<f32>,
    lod_uv: vec2<f32>,
) -> vec4<f32> {
    if (slot >= 8u) {
        return vec4<f32>(1.0);
    }
    if (screen_copy_sample(coordinate_space, slot)) {
        // EFB and mirror copies have one level regardless of the sampler state
        // carried by the placeholder J3D texture bound to this slot.
        return sample_texture_level_zero(slot, uv);
    }
    // Sunshine chooses mips while rendering its fixed 640x448 EFB. Scale the
    // derivatives from the editor's physical render target back to that pixel
    // density before applying GXInitTexObjLOD's authored bias.
    let bias_scale = exp2(material.texture_lod_parameters[slot].x);
    let uv_dx = dpdx(lod_uv) * (camera.render_target_size.x / 640.0) * bias_scale;
    let uv_dy = dpdy(lod_uv) * (camera.render_target_size.y / 448.0) * bias_scale;
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

fn rgb5a3_copy_value(value: vec4<f32>) -> vec4<f32> {
    // TMirrorCamera allocates its EFB copy as GX_TF_RGB5A3. Preserve that
    // conversion even though the editor renders the copy at viewport
    // resolution: opaque texels use RGB555, while translucent texels use
    // A3RGB4. TEV therefore receives the same color and alpha precision as SMS.
    let rgba8 = clamp(floor(value * 255.0 + 0.5), vec4<f32>(0.0), vec4<f32>(255.0));
    if (rgba8.a >= 224.0) {
        let rgb5 = floor(rgba8.rgb / 8.0);
        return vec4<f32>(rgb5 / 31.0, 1.0);
    }
    let rgb4 = floor(rgba8.rgb / 16.0);
    let alpha3 = floor(rgba8.a / 32.0);
    return vec4<f32>(rgb4 / 15.0, alpha3 / 7.0);
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

fn tev_s10(value: f32) -> i32 {
    return clamp(i32(round(value * 255.0)), -1024, 1023);
}

fn tev_input_u8(value: f32) -> i32 {
    // A, B, and C read only the low eight bits of a TEV register. This is
    // observable when an earlier unclamped stage writes a signed 10-bit value.
    return tev_s10(value) & 255;
}

fn tev_regular_channel(
    a: f32,
    b: f32,
    c: f32,
    d: f32,
    operation: vec4<u32>,
) -> f32 {
    let input_a = tev_input_u8(a);
    let input_b = tev_input_u8(b);
    let input_c = tev_input_u8(c);
    let input_d = tev_s10(d);
    var bias = 0;
    if (operation.y == 1u) {
        bias = 128;
    } else if (operation.y == 2u) {
        bias = -128;
    }

    // GX expands C from 0..255 to 0..256, shifts scale into both terms, and
    // applies a one-unit rounding difference between add and subtract.
    let lerp_numerator = (input_a * 256)
        + (input_b - input_a) * (input_c + (input_c >> 7));
    var result = 0;
    if (operation.z == 3u) {
        let lerp = lerp_numerator >> 8;
        result = select(input_d + bias + lerp, input_d + bias - lerp, operation.x == 1u) >> 1;
    } else {
        var scale = 1;
        if (operation.z == 1u) {
            scale = 2;
        } else if (operation.z == 2u) {
            scale = 4;
        }
        let rounding = select(128, 127, operation.x == 1u);
        let lerp = (lerp_numerator * scale + rounding) >> 8;
        result = select((input_d + bias) * scale + lerp, (input_d + bias) * scale - lerp, operation.x == 1u);
    }

    let clamped = select(
        clamp(result, -1024, 1023),
        clamp(result, 0, 255),
        (operation.w & 0xffu) != 0u,
    );
    return f32(clamped) / 255.0;
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
    if (op == 0u || op == 1u) {
        return vec3<f32>(
            tev_regular_channel(a.r, b.r, c.r, d.r, operation),
            tev_regular_channel(a.g, b.g, c.g, d.g, operation),
            tev_regular_channel(a.b, b.b, c.b, d.b, operation),
        );
    }

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
    let result = d + comparison * c;
    return select(
        clamp(result, vec3<f32>(-1024.0 / 255.0), vec3<f32>(1023.0 / 255.0)),
        clamp(result, vec3<f32>(0.0), vec3<f32>(1.0)),
        (operation.w & 0xffu) != 0u,
    );
}

fn tev_alpha_result(a: f32, b: f32, c: f32, d: f32, operation: vec4<u32>) -> f32 {
    let op = operation.x;
    if (op == 0u || op == 1u) {
        return tev_regular_channel(a, b, c, d, operation);
    }
    let result = select(
        d + select(0.0, c, tev_u8(a) == tev_u8(b)),
        d + select(0.0, c, tev_u8(a) > tev_u8(b)),
        op == 14u,
    );
    return select(
        clamp(result, -1024.0 / 255.0, 1023.0 / 255.0),
        clamp(result, 0.0, 1.0),
        (operation.w & 0xffu) != 0u,
    );
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
    // GX performs indirect operations on integer texture samples. ITF_8 keeps
    // all eight bits and STU bias converts 0..255 to signed -128..127 values.
    // Lower formats discard the low bits and use a +1 bias instead.
    var sample_value = floor(sample_texture(
        input.coordinate_space,
        order.y - 1u,
        uv,
        uv,
    ).rgb * 255.0 + 0.5);
    var format_shift = 0.0;
    if (stage0.y == 1u) {
        format_shift = 3.0;
    } else if (stage0.y == 2u) {
        format_shift = 4.0;
    } else if (stage0.y == 3u) {
        format_shift = 5.0;
    }
    sample_value = floor(sample_value / exp2(format_shift));
    let bias_value = select(-128.0, 1.0, stage0.y != 0u);
    if ((stage0.z & 1u) != 0u) {
        sample_value.x = sample_value.x + bias_value;
    }
    if ((stage0.z & 2u) != 0u) {
        sample_value.y = sample_value.y + bias_value;
    }
    if ((stage0.z & 4u) != 0u) {
        sample_value.z = sample_value.z + bias_value;
    }
    let matrix_id = stage0.w;
    if (matrix_id >= 1u && matrix_id <= 3u) {
        let matrix_index = matrix_id - 1u;
        let row = matrix_index * 2u;
        return vec2<f32>(
            dot(material.indirect_matrix_rows[row].xyz, sample_value),
            dot(material.indirect_matrix_rows[row + 1u].xyz, sample_value),
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
    if (input.coordinate_space == 7u) {
        // TMapObjWave relies on the rendered sea and terrain depth to confine
        // its moving grid to visible ocean. The editor's camera-centered proxy
        // can otherwise lift individual crests through shallow beach geometry,
        // so sample the visible-water coverage pass before evaluating TEV.
        let mask_size = vec2<f32>(textureDimensions(texture1));
        let mask_uv = input.position.xy / mask_size;
        if (textureSampleLevel(texture1, sampler1, mask_uv, 0.0).r < 0.5) {
            discard;
        }
    }
    if (input.coordinate_space == 4u) {
        let mask = sample_texture(input.coordinate_space, 0u, input.uv0, input.uv0);
        let target_size = vec2<f32>(textureDimensions(texture1));
        let screen_uv = input.position.xy / target_size;
        let displacement = (mask.rg - vec2<f32>(0.5)) * 0.035 * input.color0.a;
        let scene = sample_texture(
            input.coordinate_space,
            1u,
            screen_uv + displacement,
            screen_uv,
        );
        return vec4<f32>(scene.rgb, mask.a * input.color0.a);
    }
    var previous = vec4<f32>(0.0);
    var reg0 = material.tev_colors[0];
    var reg1 = material.tev_colors[1];
    if (material.fog_meta.w == 1u) {
        reg1 = input.color1;
    }
    var reg2 = material.tev_colors[2];

    for (var stage_index = 0u; stage_index < 16u; stage_index = stage_index + 1u) {
        if (stage_index >= material.counts.x) {
            break;
        }
        let order = material.tev_orders[stage_index];
        let selectors = material.tev_selectors[stage_index];
        var tex = vec4<f32>(1.0);
        if (order.x != 0u && order.y != 0u) {
            let original_uv = tex_coord(input, order.x - 1u);
            var uv = original_uv;
            // GX regular coordinates are measured in destination texels. The
            // matrix result is therefore a texel displacement, not normalized
            // UV. Sunshine's shimmer destination is its 320x224 screen copy.
            let texture_size = material.texture_sizes[order.y - 1u].xy;
            uv = uv + indirect_offset(stage_index, input) / texture_size;
            let use_original_lod = material.indirect_stages1[stage_index].w != 0u;
            let lod_uv = select(uv, original_uv, use_original_lod);
            tex = sample_texture(input.coordinate_space, order.y - 1u, uv, lod_uv);
            if (input.coordinate_space == 5u && order.y == 1u) {
                tex = rgb5a3_copy_value(tex);
            }
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

    // The pixel engine consumes the low eight bits of the final signed TEV
    // registers. This is a wrap, not a saturating clamp: BiancoRiver's last
    // water stage deliberately scales an unclamped result by 4x.
    let output = vec4<f32>(
        f32(tev_input_u8(previous.r)) / 255.0,
        f32(tev_input_u8(previous.g)) / 255.0,
        f32(tev_input_u8(previous.b)) / 255.0,
        f32(tev_input_u8(previous.a)) / 255.0,
    );
    if (!alpha_compare_passes(output.a)) {
        discard;
    }
    return clamp(apply_fog(output, input.view_depth), vec4<f32>(0.0), vec4<f32>(1.0));
}

@fragment
fn fs_wave_mask(_input: VertexOut) -> @location(0) vec4<f32> {
    return vec4<f32>(1.0);
}
