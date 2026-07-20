use super::*;

pub(super) fn framebuffer_size_for_rect(rect: egui::Rect) -> [usize; 2] {
    let max_side: f32 = 1280.0;
    let width = rect.width().max(1.0);
    let height = rect.height().max(1.0);
    let scale = (max_side / width.max(height)).min(1.0);
    [
        (width * scale).round().clamp(1.0, max_side) as usize,
        (height * scale).round().clamp(1.0, max_side) as usize,
    ]
}

pub(super) fn viewport_background_mesh(rect: egui::Rect) -> egui::Mesh {
    let mut mesh = egui::Mesh::default();
    let top = egui::Color32::from_rgb(30, 42, 48);
    let bottom = egui::Color32::from_rgb(18, 24, 26);
    mesh.colored_vertex(rect.left_top(), top);
    mesh.colored_vertex(rect.right_top(), top);
    mesh.colored_vertex(rect.right_bottom(), bottom);
    mesh.colored_vertex(rect.left_bottom(), bottom);
    mesh.add_triangle(0, 1, 2);
    mesh.add_triangle(0, 2, 3);
    mesh
}

pub(super) fn viewport_framebuffer_background(size: [usize; 2]) -> egui::ColorImage {
    let width = size[0].max(1);
    let height = size[1].max(1);
    let mut image = egui::ColorImage::filled(size, egui::Color32::from_rgb(18, 24, 26));
    let top = [35.0, 47.0, 55.0];
    let horizon = [23.0, 36.0, 40.0];
    let lower = [18.0, 24.0, 26.0];
    let denom = height.saturating_sub(1).max(1) as f32;

    for y in 0..height {
        let t = y as f32 / denom;
        let color = if t < 0.48 {
            lerp_rgb(top, horizon, t / 0.48)
        } else {
            lerp_rgb(horizon, lower, (t - 0.48) / 0.52)
        };
        for x in 0..width {
            image.pixels[y * width + x] = egui::Color32::from_rgb(color[0], color[1], color[2]);
        }
    }

    image
}

pub(super) fn lerp_rgb(a: [f32; 3], b: [f32; 3], t: f32) -> [u8; 3] {
    let t = t.clamp(0.0, 1.0);
    [
        (a[0] + (b[0] - a[0]) * t).clamp(0.0, 255.0) as u8,
        (a[1] + (b[1] - a[1]) * t).clamp(0.0, 255.0) as u8,
        (a[2] + (b[2] - a[2]) * t).clamp(0.0, 255.0) as u8,
    ]
}

pub(super) fn project_triangle_to_framebuffer(
    app: &SmsEditorApp,
    rect: egui::Rect,
    size: [usize; 2],
    vertices: [[f32; 3]; 3],
) -> Option<[ProjectedVertex; 3]> {
    Some([
        app.world_to_framebuffer(rect, size, vertices[0])?,
        app.world_to_framebuffer(rect, size, vertices[1])?,
        app.world_to_framebuffer(rect, size, vertices[2])?,
    ])
}

pub(super) fn projected_triangle_overlaps_frame(
    vertices: [ProjectedVertex; 3],
    size: [usize; 2],
) -> bool {
    if vertices
        .iter()
        .any(|vertex| !vertex.x.is_finite() || !vertex.y.is_finite() || !vertex.depth.is_finite())
    {
        return false;
    }

    let max_x = size[0] as f32;
    let max_y = size[1] as f32;
    let min_x = vertices
        .iter()
        .map(|vertex| vertex.x)
        .fold(f32::INFINITY, f32::min);
    let max_tri_x = vertices
        .iter()
        .map(|vertex| vertex.x)
        .fold(f32::NEG_INFINITY, f32::max);
    let min_y = vertices
        .iter()
        .map(|vertex| vertex.y)
        .fold(f32::INFINITY, f32::min);
    let max_tri_y = vertices
        .iter()
        .map(|vertex| vertex.y)
        .fold(f32::NEG_INFINITY, f32::max);

    min_x < max_x && max_tri_x >= 0.0 && min_y < max_y && max_tri_y >= 0.0
}

pub(super) fn projected_triangle_is_culled(
    vertices: [ProjectedVertex; 3],
    cull_mode: Option<u8>,
) -> bool {
    let Some(cull_mode) = cull_mode else {
        return false;
    };
    match cull_mode {
        0 => false,
        1 => edge_function(vertices[0], vertices[1], vertices[2]) < 0.0,
        2 => edge_function(vertices[0], vertices[1], vertices[2]) > 0.0,
        3 => true,
        _ => false,
    }
}

pub(super) fn rasterize_projected_preview_triangle(
    preview: &ModelPreview,
    image: &mut egui::ColorImage,
    depth: &mut [f32],
    projected: ProjectedPreviewTriangle<'_>,
    write_depth: bool,
) {
    let triangle = projected.triangle;
    let normal = preview_triangle_normal(triangle);
    let average_y = triangle
        .vertices
        .iter()
        .map(|vertex| vertex[1])
        .sum::<f32>()
        / 3.0;
    let alpha_test_fallback =
        triangle.alpha_compare.is_none() && preview_triangle_uses_alpha_test(preview, triangle);

    if let (Some(texture_index), Some(tex_coords)) = (triangle.texture_index, triangle.tex_coords) {
        if let Some(texture) = preview.textures.get(texture_index) {
            let mask_texture = triangle
                .mask_texture_index
                .and_then(|index| preview.textures.get(index))
                .zip(triangle.mask_tex_coords);
            rasterize_preview_triangle(
                image,
                depth,
                projected.screen,
                Some((texture, tex_coords)),
                mask_texture,
                preview_texture_tints(
                    triangle.color,
                    triangle.vertex_colors,
                    triangle.combine_mode,
                    triangle.render_layer,
                ),
                write_depth,
                triangle.alpha_compare,
                alpha_test_fallback,
            );
            return;
        }
    }

    rasterize_preview_triangle(
        image,
        depth,
        projected.screen,
        None,
        None,
        preview_solid_triangle_colors(triangle, normal, average_y),
        write_depth,
        triangle.alpha_compare,
        alpha_test_fallback,
    );
}

pub(super) fn preview_triangle_is_translucent(
    preview: &ModelPreview,
    triangle: &PreviewTriangle,
) -> bool {
    if matches!(
        triangle.render_layer,
        PreviewRenderLayer::Water | PreviewRenderLayer::MirrorSurface
    ) {
        return true;
    }
    if preview_triangle_uses_alpha_test(preview, triangle) {
        return false;
    }
    if matches!(
        triangle.render_layer,
        PreviewRenderLayer::Goop | PreviewRenderLayer::Shadow | PreviewRenderLayer::Heatwave
    ) {
        return true;
    }

    let has_alpha_source = preview_triangle_has_alpha_source(preview, triangle);
    if !has_alpha_source {
        return false;
    }

    triangle
        .blend_mode
        .map(|blend| blend.mode != 0)
        .unwrap_or(true)
}

pub(super) fn preview_triangle_uses_alpha_test(
    preview: &ModelPreview,
    triangle: &PreviewTriangle,
) -> bool {
    if matches!(
        triangle.render_layer,
        PreviewRenderLayer::Water | PreviewRenderLayer::MirrorSurface
    ) {
        return false;
    }

    if triangle
        .alpha_compare
        .is_some_and(alpha_compare_can_discard)
    {
        return true;
    }

    (triangle
        .texture_index
        .and_then(|index| preview.textures.get(index))
        .is_some_and(|texture| texture.has_alpha)
        && (triangle.blend_mode.is_none_or(|blend| blend.mode == 0)
            || triangle
                .texture_index
                .and_then(|index| preview.textures.get(index))
                .is_some_and(|texture| !texture.has_translucent_alpha)))
        || triangle.mask_texture_index.is_some()
}

pub(super) fn preview_triangle_has_alpha_source(
    preview: &ModelPreview,
    triangle: &PreviewTriangle,
) -> bool {
    let material_alpha = triangle.color.is_some_and(|color| {
        triangle.texture_index.is_none()
            && color[3] < 245
            && matches!(
                triangle.combine_mode,
                J3dPreviewCombineMode::TextureModulateMaterial
                    | J3dPreviewCombineMode::MaterialOnly
            )
    });
    let vertex_alpha = triangle.vertex_colors.is_some_and(|colors| {
        colors.iter().any(|color| color[3] < 245)
            && matches!(
                triangle.combine_mode,
                J3dPreviewCombineMode::TextureModulateVertex | J3dPreviewCombineMode::VertexOnly
            )
    });
    let texture_alpha = triangle
        .texture_index
        .and_then(|index| preview.textures.get(index))
        .is_some_and(|texture| texture.has_translucent_alpha);
    let mask_alpha = triangle.mask_texture_index.is_some();

    material_alpha || vertex_alpha || texture_alpha || mask_alpha
}

pub(super) fn alpha_compare_can_discard(compare: J3dAlphaCompare) -> bool {
    (0..=255).any(|alpha| !alpha_compare_passes(compare, alpha))
}

pub(super) fn alpha_compare_passes(compare: J3dAlphaCompare, alpha: u8) -> bool {
    let a = alpha as i16;
    let pass0 = alpha_compare_op_passes(compare.comp0, a, compare.ref0 as i16);
    let pass1 = alpha_compare_op_passes(compare.comp1, a, compare.ref1 as i16);
    match compare.op {
        0 => pass0 && pass1,
        1 => pass0 || pass1,
        2 => pass0 ^ pass1,
        3 => pass0 == pass1,
        _ => pass0 && pass1,
    }
}

pub(super) fn alpha_compare_op_passes(compare: u8, alpha: i16, reference: i16) -> bool {
    match compare {
        0 => false,
        1 => alpha < reference,
        2 => alpha == reference,
        3 => alpha <= reference,
        4 => alpha > reference,
        5 => alpha != reference,
        6 => alpha >= reference,
        7 => true,
        _ => true,
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) fn rasterize_preview_triangle(
    image: &mut egui::ColorImage,
    depth: &mut [f32],
    vertices: [ProjectedVertex; 3],
    texture: Option<(&PreviewTexture, [[f32; 2]; 3])>,
    mask_texture: Option<(&PreviewTexture, [[f32; 2]; 3])>,
    tints: [egui::Color32; 3],
    write_depth: bool,
    alpha_compare: Option<J3dAlphaCompare>,
    alpha_test_fallback: bool,
) {
    let area = edge_function(vertices[0], vertices[1], vertices[2]);
    if !area.is_finite() || area.abs() < 0.5 {
        return;
    }

    let width = image.size[0];
    let height = image.size[1];
    let min_x = vertices
        .iter()
        .map(|vertex| vertex.x)
        .fold(f32::INFINITY, f32::min)
        .floor()
        .max(0.0) as usize;
    let max_x = vertices
        .iter()
        .map(|vertex| vertex.x)
        .fold(f32::NEG_INFINITY, f32::max)
        .ceil()
        .min((width.saturating_sub(1)) as f32) as usize;
    let min_y = vertices
        .iter()
        .map(|vertex| vertex.y)
        .fold(f32::INFINITY, f32::min)
        .floor()
        .max(0.0) as usize;
    let max_y = vertices
        .iter()
        .map(|vertex| vertex.y)
        .fold(f32::NEG_INFINITY, f32::max)
        .ceil()
        .min((height.saturating_sub(1)) as f32) as usize;

    if min_x > max_x || min_y > max_y {
        return;
    }

    for y in min_y..=max_y {
        for x in min_x..=max_x {
            let px = x as f32 + 0.5;
            let py = y as f32 + 0.5;
            let weights = [
                edge_function_point(vertices[1], vertices[2], px, py) / area,
                edge_function_point(vertices[2], vertices[0], px, py) / area,
                edge_function_point(vertices[0], vertices[1], px, py) / area,
            ];
            if weights.iter().any(|weight| *weight < -0.0001) {
                continue;
            }

            let Some(corrected_weights) = perspective_correct_weights(vertices, weights) else {
                continue;
            };
            let pixel_depth = perspective_correct_depth(vertices, weights);
            let index = y * width + x;
            if pixel_depth >= depth[index] {
                continue;
            }

            let color = if let Some((texture, tex_coords)) = texture {
                let uv = [
                    corrected_weights[0] * tex_coords[0][0]
                        + corrected_weights[1] * tex_coords[1][0]
                        + corrected_weights[2] * tex_coords[2][0],
                    corrected_weights[0] * tex_coords[0][1]
                        + corrected_weights[1] * tex_coords[1][1]
                        + corrected_weights[2] * tex_coords[2][1],
                ];
                let texture_color = sample_preview_texture(texture, uv);
                let tint = interpolate_color(tints, corrected_weights);
                combine_texture_and_tint(texture, texture_color, tint)
            } else {
                interpolate_color(tints, corrected_weights)
            };
            let color = if let Some((mask_texture, mask_tex_coords)) = mask_texture {
                let uv = [
                    corrected_weights[0] * mask_tex_coords[0][0]
                        + corrected_weights[1] * mask_tex_coords[1][0]
                        + corrected_weights[2] * mask_tex_coords[2][0],
                    corrected_weights[0] * mask_tex_coords[0][1]
                        + corrected_weights[1] * mask_tex_coords[1][1]
                        + corrected_weights[2] * mask_tex_coords[2][1],
                ];
                let mask_color = sample_preview_texture(mask_texture, uv);
                let mask_alpha = (mask_color[0] + mask_color[1] + mask_color[2]) / 3.0;
                [color[0], color[1], color[2], color[3] * mask_alpha]
            } else {
                color
            };

            if let Some(compare) = alpha_compare {
                let alpha = (color[3].clamp(0.0, 1.0) * 255.0) as u8;
                if !alpha_compare_passes(compare, alpha) {
                    continue;
                }
            } else if color[3] < (if alpha_test_fallback { 0.28 } else { 0.12 }) {
                continue;
            }
            let color = software_output_color_for_pass(color, write_depth);
            blend_depth_pixel(image, depth, index, pixel_depth, color, write_depth);
        }
    }
}

pub(super) fn rasterize_depth_tested_segment(
    image: &mut egui::ColorImage,
    depth: &[f32],
    start: ProjectedVertex,
    end: ProjectedVertex,
    color: egui::Color32,
) {
    let dx = end.x - start.x;
    let dy = end.y - start.y;
    let steps = dx.abs().max(dy.abs()).ceil().max(1.0) as usize;
    let width = image.size[0];
    let height = image.size[1];

    for step in 0..=steps {
        let t = step as f32 / steps as f32;
        let x = (start.x + dx * t).round() as isize;
        let y = (start.y + dy * t).round() as isize;
        if x < 0 || y < 0 || x >= width as isize || y >= height as isize {
            continue;
        }
        let inv_depth = start.inv_depth + (end.inv_depth - start.inv_depth) * t;
        if !inv_depth.is_finite() || inv_depth <= 0.0 {
            continue;
        }
        let segment_depth = 1.0 / inv_depth;
        let index = y as usize * width + x as usize;
        let stored_depth = depth[index];
        let tolerance = (stored_depth.abs() * 0.001).max(0.5);
        if segment_depth <= stored_depth + tolerance {
            image.pixels[index] = color;
        }
    }
}

pub(super) fn software_output_color_for_pass(color: [f32; 4], write_depth: bool) -> [f32; 4] {
    if write_depth {
        [color[0], color[1], color[2], 1.0]
    } else {
        color
    }
}

pub(super) fn edge_function(a: ProjectedVertex, b: ProjectedVertex, c: ProjectedVertex) -> f32 {
    edge_function_point(a, b, c.x, c.y)
}

pub(super) fn edge_function_point(a: ProjectedVertex, b: ProjectedVertex, x: f32, y: f32) -> f32 {
    (x - a.x) * (b.y - a.y) - (y - a.y) * (b.x - a.x)
}

pub(super) fn projected_triangle_depth_at_point(
    vertices: [ProjectedVertex; 3],
    x: f32,
    y: f32,
) -> Option<f32> {
    let area = edge_function(vertices[0], vertices[1], vertices[2]);
    if !area.is_finite() || area.abs() <= f32::EPSILON {
        return None;
    }

    let weights = [
        edge_function_point(vertices[1], vertices[2], x, y) / area,
        edge_function_point(vertices[2], vertices[0], x, y) / area,
        edge_function_point(vertices[0], vertices[1], x, y) / area,
    ];
    if weights.iter().any(|weight| *weight < -0.0001) {
        return None;
    }

    let depth = perspective_correct_depth(vertices, weights);
    depth.is_finite().then_some(depth)
}

pub(super) fn perspective_correct_weights(
    vertices: [ProjectedVertex; 3],
    weights: [f32; 3],
) -> Option<[f32; 3]> {
    let weighted_inv_depth = [
        weights[0] * vertices[0].inv_depth,
        weights[1] * vertices[1].inv_depth,
        weights[2] * vertices[2].inv_depth,
    ];
    let sum = weighted_inv_depth[0] + weighted_inv_depth[1] + weighted_inv_depth[2];
    if !sum.is_finite() || sum.abs() <= f32::EPSILON {
        return None;
    }
    Some([
        weighted_inv_depth[0] / sum,
        weighted_inv_depth[1] / sum,
        weighted_inv_depth[2] / sum,
    ])
}

pub(super) fn perspective_correct_depth(vertices: [ProjectedVertex; 3], weights: [f32; 3]) -> f32 {
    let inv_depth = weights[0] * vertices[0].inv_depth
        + weights[1] * vertices[1].inv_depth
        + weights[2] * vertices[2].inv_depth;
    if inv_depth > 0.0 && inv_depth.is_finite() {
        1.0 / inv_depth
    } else {
        f32::INFINITY
    }
}

pub(super) fn sample_preview_texture(texture: &PreviewTexture, uv: [f32; 2]) -> [f32; 4] {
    let width = texture.image.size[0].max(1);
    let height = texture.image.size[1].max(1);
    let u = wrap_texture_coord(uv[0], texture.wrap_s);
    let v = wrap_texture_coord(uv[1], texture.wrap_t);
    let x = u * (width.saturating_sub(1)) as f32;
    let y = v * (height.saturating_sub(1)) as f32;
    let x0 = x.floor() as usize;
    let y0 = y.floor() as usize;
    let x1 = (x0 + 1).min(width - 1);
    let y1 = (y0 + 1).min(height - 1);
    let tx = x - x0 as f32;
    let ty = y - y0 as f32;

    let c00 = color32_to_rgba(texture.image.pixels[y0 * width + x0]);
    let c10 = color32_to_rgba(texture.image.pixels[y0 * width + x1]);
    let c01 = color32_to_rgba(texture.image.pixels[y1 * width + x0]);
    let c11 = color32_to_rgba(texture.image.pixels[y1 * width + x1]);
    let top = lerp_rgba(c00, c10, tx);
    let bottom = lerp_rgba(c01, c11, tx);
    lerp_rgba(top, bottom, ty)
}

pub(super) fn interpolate_color(colors: [egui::Color32; 3], weights: [f32; 3]) -> [f32; 4] {
    let colors = colors.map(color32_to_rgba);
    [
        weights[0] * colors[0][0] + weights[1] * colors[1][0] + weights[2] * colors[2][0],
        weights[0] * colors[0][1] + weights[1] * colors[1][1] + weights[2] * colors[2][1],
        weights[0] * colors[0][2] + weights[1] * colors[1][2] + weights[2] * colors[2][2],
        weights[0] * colors[0][3] + weights[1] * colors[1][3] + weights[2] * colors[2][3],
    ]
}

pub(super) fn multiply_rgba(a: [f32; 4], b: [f32; 4]) -> [f32; 4] {
    [a[0] * b[0], a[1] * b[1], a[2] * b[2], a[3] * b[3]]
}

pub(super) fn combine_texture_and_tint(
    texture: &PreviewTexture,
    texture_color: [f32; 4],
    tint: [f32; 4],
) -> [f32; 4] {
    let color = multiply_rgba(texture_color, tint);
    let tint_is_dark = tint[0].max(tint[1]).max(tint[2]) < 0.35;
    let intensity_mask = matches!(texture.format, 0 | 1) && !texture.has_alpha;
    if intensity_mask && tint_is_dark && tint[3] < 0.98 {
        let intensity = (texture_color[0] + texture_color[1] + texture_color[2]) / 3.0;
        return [tint[0], tint[1], tint[2], intensity * tint[3]];
    }

    color
}

pub(super) fn lerp_rgba(a: [f32; 4], b: [f32; 4], t: f32) -> [f32; 4] {
    [
        a[0] + (b[0] - a[0]) * t,
        a[1] + (b[1] - a[1]) * t,
        a[2] + (b[2] - a[2]) * t,
        a[3] + (b[3] - a[3]) * t,
    ]
}

pub(super) fn color32_to_rgba(color: egui::Color32) -> [f32; 4] {
    let [red, green, blue, alpha] = color.to_srgba_unmultiplied();
    [
        red as f32 / 255.0,
        green as f32 / 255.0,
        blue as f32 / 255.0,
        alpha as f32 / 255.0,
    ]
}

pub(super) fn rgba_to_color32(color: [f32; 4]) -> egui::Color32 {
    egui::Color32::from_rgba_unmultiplied(
        (color[0].clamp(0.0, 1.0) * 255.0) as u8,
        (color[1].clamp(0.0, 1.0) * 255.0) as u8,
        (color[2].clamp(0.0, 1.0) * 255.0) as u8,
        (color[3].clamp(0.0, 1.0) * 255.0) as u8,
    )
}

pub(super) fn blend_depth_pixel(
    image: &mut egui::ColorImage,
    depth: &mut [f32],
    index: usize,
    pixel_depth: f32,
    src: [f32; 4],
    write_depth: bool,
) {
    let alpha = src[3].clamp(0.0, 1.0);
    if alpha >= 0.98 {
        image.pixels[index] = rgba_to_color32(src);
        if write_depth {
            depth[index] = pixel_depth;
        }
        return;
    }

    let dst = color32_to_rgba(image.pixels[index]);
    let out_alpha = alpha + dst[3] * (1.0 - alpha);
    if out_alpha <= 0.001 {
        return;
    }
    let out = [
        (src[0] * alpha + dst[0] * dst[3] * (1.0 - alpha)) / out_alpha,
        (src[1] * alpha + dst[1] * dst[3] * (1.0 - alpha)) / out_alpha,
        (src[2] * alpha + dst[2] * dst[3] * (1.0 - alpha)) / out_alpha,
        out_alpha,
    ];
    image.pixels[index] = rgba_to_color32(out);
    if write_depth && alpha >= 0.65 {
        depth[index] = pixel_depth;
    }
}

pub(super) fn preview_triangle_normal(triangle: &PreviewTriangle) -> [f32; 3] {
    if let Some(normals) = triangle.normals {
        return vec3_normalize([
            (normals[0][0] + normals[1][0] + normals[2][0]) / 3.0,
            (normals[0][1] + normals[1][1] + normals[2][1]) / 3.0,
            (normals[0][2] + normals[1][2] + normals[2][2]) / 3.0,
        ]);
    }
    triangle_normal(triangle.vertices)
}

pub(super) fn triangle_normal(vertices: [[f32; 3]; 3]) -> [f32; 3] {
    let ab = [
        vertices[1][0] - vertices[0][0],
        vertices[1][1] - vertices[0][1],
        vertices[1][2] - vertices[0][2],
    ];
    let ac = [
        vertices[2][0] - vertices[0][0],
        vertices[2][1] - vertices[0][1],
        vertices[2][2] - vertices[0][2],
    ];
    let normal = [
        ab[1] * ac[2] - ab[2] * ac[1],
        ab[2] * ac[0] - ab[0] * ac[2],
        ab[0] * ac[1] - ab[1] * ac[0],
    ];
    let length = (normal[0] * normal[0] + normal[1] * normal[1] + normal[2] * normal[2])
        .sqrt()
        .max(0.0001);
    [normal[0] / length, normal[1] / length, normal[2] / length]
}

pub(super) fn wrap_texture_coord(value: f32, wrap: u8) -> f32 {
    match wrap {
        0 => value.clamp(0.0, 1.0),
        2 => {
            let value = value.rem_euclid(2.0);
            if value > 1.0 {
                2.0 - value
            } else {
                value
            }
        }
        _ => value.rem_euclid(1.0),
    }
}
