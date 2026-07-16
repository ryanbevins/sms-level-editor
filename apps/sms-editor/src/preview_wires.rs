use super::*;

const WIRE_TABLE_NAME: &str = "ワイヤーキューブテーブル";
const DEFAULT_DRAW_WIDTH: f32 = 5.0;
const DEFAULT_DRAW_HEIGHT: f32 = 6.0;
const DEFAULT_UPPER_COLOR: [u8; 4] = [0x78, 0x78, 0x78, 0xff];
const DEFAULT_LOWER_COLOR: [u8; 4] = [0x32, 0x32, 0x32, 0xff];
const WIRE_FITTING_LOADER_FLAGS: u32 = 0x1021_0000;
const MAX_WIRE_POINTS: usize = 4_096;

pub(super) struct ProceduralWirePreview {
    pub(super) points: Vec<PreviewPoint>,
    pub(super) triangles: Vec<PreviewTriangle>,
    pub(super) wire_count: usize,
    pub(super) source_vertices: usize,
    pub(super) source_triangles: usize,
    pub(super) source_textures: usize,
    pub(super) packet_count: usize,
    pub(super) bounds_min: [f32; 3],
    pub(super) bounds_max: [f32; 3],
}

#[derive(Debug, Clone, Copy)]
struct WireDefinition {
    center: [f32; 3],
    rotation_degrees: [f32; 3],
    dimensions: [f32; 3],
}

#[derive(Debug, Clone, Copy)]
struct WireStyle {
    draw_width: f32,
    draw_height: f32,
    upper_surface: [u8; 4],
    lower_surface: [u8; 4],
}

impl Default for WireStyle {
    fn default() -> Self {
        Self {
            draw_width: DEFAULT_DRAW_WIDTH,
            draw_height: DEFAULT_DRAW_HEIGHT,
            upper_surface: DEFAULT_UPPER_COLOR,
            lower_surface: DEFAULT_LOWER_COLOR,
        }
    }
}

pub(super) fn build_procedural_wire_preview(
    document: &StageDocument,
    first_model_index: usize,
    first_packet_index: usize,
    textures: &mut Vec<PreviewTexture>,
    materials: &mut Vec<J3dMaterial>,
) -> ProceduralWirePreview {
    let definitions = load_wire_definitions(document);
    let style = load_wire_style(document);
    let fitting = load_wire_fitting_preview(document);
    let fitting_bases = fitting.as_ref().map(|preview| {
        let texture_base = push_preview_textures(textures, preview);
        let material_base = push_preview_materials(materials, preview, texture_base);
        (texture_base, material_base)
    });
    let mut result = ProceduralWirePreview {
        points: Vec::new(),
        triangles: Vec::new(),
        wire_count: definitions.len(),
        source_vertices: 0,
        source_triangles: 0,
        source_textures: fitting.as_ref().map_or(0, |preview| preview.textures.len()),
        packet_count: 0,
        bounds_min: [f32::INFINITY; 3],
        bounds_max: [f32::NEG_INFINITY; 3],
    };

    for (wire_index, definition) in definitions.into_iter().enumerate() {
        let model_index = first_model_index + wire_index;
        let path = wire_path(definition);
        if path.len() < 2 {
            continue;
        }
        for position in &path {
            result.points.push(PreviewPoint {
                position: *position,
                model_index,
            });
            merge_bounds(
                &mut result.bounds_min,
                &mut result.bounds_max,
                *position,
                *position,
            );
        }

        let packet_index = first_packet_index + result.packet_count;
        result.packet_count += 1;
        let before = result.triangles.len();
        push_wire_prism(
            &mut result.triangles,
            &path,
            style,
            model_index,
            packet_index,
        );
        result.source_vertices += path.len() * 3;
        result.source_triangles += result.triangles.len() - before;

        let Some(preview) = fitting.as_ref() else {
            continue;
        };
        let Some((texture_base, material_base)) = fitting_bases else {
            continue;
        };
        let [start, end] = [path[0], *path.last().unwrap_or(&path[0])];
        for (position, yaw_offset) in [(start, 0.0), (end, 180.0)] {
            let transform = Transform {
                translation: position,
                rotation_degrees: [
                    definition.rotation_degrees[0],
                    definition.rotation_degrees[1] + yaw_offset,
                    definition.rotation_degrees[2],
                ],
                scale: [1.0; 3],
            };
            let packet_base = first_packet_index + result.packet_count;
            result.packet_count += preview
                .triangles
                .iter()
                .map(|triangle| triangle.packet_index)
                .max()
                .map(|index| index + 1)
                .unwrap_or(1);
            append_fitting_instance(
                &mut result,
                preview,
                transform,
                model_index,
                packet_base,
                texture_base,
                material_base,
                textures.len(),
                materials.len(),
            );
        }
    }

    result
}

fn load_wire_definitions(document: &StageDocument) -> Vec<WireDefinition> {
    let mut definitions = Vec::new();
    for asset in document.assets.iter().filter(|asset| {
        asset.kind == StageAssetKind::Placement
            && asset
                .path
                .to_string_lossy()
                .replace('\\', "/")
                .to_ascii_lowercase()
                .ends_with("/map/tables.bin")
    }) {
        let Ok(bytes) = read_stage_asset_bytes(&asset.path) else {
            continue;
        };
        let Ok(records) = parse_jdrama_object_records(&bytes) else {
            continue;
        };
        let wire_tables = records
            .iter()
            .filter(|record| {
                record.type_name.rsplit("::").next() == Some("CubeGeneralInfoTable")
                    && record.object_name.as_deref() == Some(WIRE_TABLE_NAME)
            })
            .map(|record| record.offset..record.offset + record.size)
            .collect::<Vec<_>>();
        for record in records.iter().filter(|record| {
            record.type_name.rsplit("::").next() == Some("CubeGeneralInfo")
                && wire_tables.iter().any(|table| {
                    record.offset > table.start && record.offset + record.size <= table.end
                })
        }) {
            let Some(definition) = record.cube_general_info.and_then(wire_definition_from_cube)
            else {
                continue;
            };
            definitions.push(definition);
        }
    }
    definitions
}

fn wire_definition_from_cube(info: sms_formats::JDramaCubeGeneralInfo) -> Option<WireDefinition> {
    if !info.center.iter().all(|value| value.is_finite())
        || !info.rotation_degrees.iter().all(|value| value.is_finite())
        || !info.dimensions.iter().all(|value| value.is_finite())
        || info.dimensions[2] <= 50.0
        || info.dimensions[2] > 1_000_000.0
    {
        return None;
    }
    Some(WireDefinition {
        center: info.center,
        rotation_degrees: info.rotation_degrees,
        dimensions: info.dimensions,
    })
}

fn load_wire_style(document: &StageDocument) -> WireStyle {
    for asset in document.assets.iter().filter(|asset| {
        asset.kind == StageAssetKind::Placement
            && asset
                .path
                .to_string_lossy()
                .replace('\\', "/")
                .to_ascii_lowercase()
                .ends_with("/map/scene.bin")
    }) {
        let Ok(bytes) = read_stage_asset_bytes(&asset.path) else {
            continue;
        };
        let Ok(records) = parse_jdrama_object_records(&bytes) else {
            continue;
        };
        if let Some(params) = records.iter().find_map(|record| record.map_wire_manager) {
            if params.draw_width.is_finite()
                && params.draw_width > 0.0
                && params.draw_height.is_finite()
                && params.draw_height > 0.0
            {
                return WireStyle {
                    draw_width: params.draw_width,
                    draw_height: params.draw_height,
                    upper_surface: params.upper_surface,
                    lower_surface: params.lower_surface,
                };
            }
        }
    }
    WireStyle::default()
}

fn wire_path(definition: WireDefinition) -> Vec<[f32; 3]> {
    let half_span = transform_preview_vector(
        [0.0, 0.0, definition.dimensions[2] * 0.5],
        Transform {
            translation: [0.0; 3],
            rotation_degrees: definition.rotation_degrees,
            scale: [1.0; 3],
        },
    );
    let vertical_offset = definition.dimensions[1];
    let start = [
        definition.center[0] - half_span[0],
        definition.center[1] - half_span[1] + vertical_offset,
        definition.center[2] - half_span[2],
    ];
    let end = [
        definition.center[0] + half_span[0],
        definition.center[1] + half_span[1] + vertical_offset,
        definition.center[2] + half_span[2],
    ];
    let span = vec3_sub(end, start);
    let interior_count = ((definition.dimensions[2] / 50.0 + 1.0) - 2.0)
        .trunc()
        .max(0.0) as usize;
    let interior_count = interior_count.min(MAX_WIRE_POINTS);
    let sag = vertical_offset * 0.5;
    let mut path = Vec::with_capacity(interior_count + 2);
    path.push(start);
    if interior_count != 0 {
        for index in 0..interior_count {
            // Retail stores (index + 1) / mNumMapWirePoints, including the
            // duplicated endpoint that closes the GX triangle strips.
            let position = (index + 1) as f32 / interior_count as f32;
            path.push([
                start[0] + span[0] * position,
                start[1] + span[1] * position - sag * (position * std::f32::consts::PI).sin(),
                start[2] + span[2] * position,
            ]);
        }
    }
    if path
        .last()
        .is_none_or(|point| wire_vec3_length(vec3_sub(end, *point)) > 0.001)
    {
        path.push(end);
    }
    path
}

fn push_wire_prism(
    triangles: &mut Vec<PreviewTriangle>,
    path: &[[f32; 3]],
    style: WireStyle,
    model_index: usize,
    packet_index: usize,
) {
    let span = vec3_sub(*path.last().unwrap_or(&path[0]), path[0]);
    let horizontal_length = (span[0] * span[0] + span[2] * span[2]).sqrt();
    let lateral = if horizontal_length > 0.0001 {
        [
            -span[2] / horizontal_length * style.draw_width,
            0.0,
            span[0] / horizontal_length * style.draw_width,
        ]
    } else {
        [style.draw_width, 0.0, 0.0]
    };

    for segment in path.windows(2) {
        if wire_vec3_length(vec3_sub(segment[1], segment[0])) <= 0.001 {
            continue;
        }
        let cross_section = |point: [f32; 3]| {
            [
                vec3_sub(point, lateral),
                vec3_add(point, lateral),
                [point[0], point[1] - style.draw_height, point[2]],
            ]
        };
        let [left0, right0, bottom0] = cross_section(segment[0]);
        let [left1, right1, bottom1] = cross_section(segment[1]);
        push_wire_quad(
            triangles,
            [left0, left1, right1, right0],
            [0.0, 1.0, 0.0],
            style.upper_surface,
            model_index,
            packet_index,
        );
        push_wire_quad(
            triangles,
            [left0, bottom0, bottom1, left1],
            vec3_normalize(vec3_add(
                lateral.map(|value| -value),
                [0.0, -style.draw_height, 0.0],
            )),
            style.lower_surface,
            model_index,
            packet_index,
        );
        push_wire_quad(
            triangles,
            [right0, right1, bottom1, bottom0],
            vec3_normalize(vec3_add(lateral, [0.0, -style.draw_height, 0.0])),
            style.lower_surface,
            model_index,
            packet_index,
        );
    }
}

fn push_wire_quad(
    triangles: &mut Vec<PreviewTriangle>,
    mut quad: [[f32; 3]; 4],
    outward: [f32; 3],
    color: [u8; 4],
    model_index: usize,
    packet_index: usize,
) {
    let normal = vec3_normalize(vec3_cross(
        vec3_sub(quad[1], quad[0]),
        vec3_sub(quad[2], quad[0]),
    ));
    if vec3_dot(normal, outward) < 0.0 {
        quad.swap(1, 3);
    }
    let normal = vec3_normalize(vec3_cross(
        vec3_sub(quad[1], quad[0]),
        vec3_sub(quad[2], quad[0]),
    ));
    for vertices in [[quad[0], quad[1], quad[2]], [quad[0], quad[2], quad[3]]] {
        triangles.push(wire_triangle(
            vertices,
            normal,
            color,
            model_index,
            packet_index,
        ));
    }
}

fn wire_vec3_length(vector: [f32; 3]) -> f32 {
    vec3_dot(vector, vector).sqrt()
}

fn wire_triangle(
    vertices: [[f32; 3]; 3],
    normal: [f32; 3],
    color: [u8; 4],
    model_index: usize,
    packet_index: usize,
) -> PreviewTriangle {
    PreviewTriangle {
        vertices,
        normals: Some([normal; 3]),
        color_channels: [Some([color; 3]), None],
        tex_coord_sets: [None; 8],
        material_index: None,
        packet_index,
        model_index,
        render_layer: PreviewRenderLayer::Main,
        color: Some(color),
        vertex_colors: Some([color; 3]),
        combine_mode: J3dPreviewCombineMode::VertexOnly,
        tex_coords: None,
        texture_index: None,
        mask_tex_coords: None,
        mask_texture_index: None,
        cull_mode: Some(2),
        alpha_compare: None,
        blend_mode: Some(J3dBlendMode {
            mode: 1,
            src_factor: 4,
            dst_factor: 5,
            logic_op: 15,
        }),
        z_mode: Some(J3dZMode {
            compare_enable: 1,
            func: 3,
            update_enable: 1,
        }),
        billboard: None,
        particle_type: None,
        particle_pivot: None,
        particle_direction: None,
        particle_color_mode: None,
        particle_environment_color: None,
    }
}

fn load_wire_fitting_preview(document: &StageDocument) -> Option<J3dGeometryPreview> {
    let roots = [
        document.base_root.join("files/data"),
        document.base_root.join("data"),
        document.base_root.clone(),
    ];
    for root in roots {
        let candidates = [
            root.join("common/map/wirefitting.bmd"),
            PathBuf::from(format!(
                "{}!/map/wirefitting.bmd",
                root.join("common.szs").display()
            )),
        ];
        for candidate in candidates {
            let Ok(bytes) = read_stage_asset_bytes(&candidate) else {
                continue;
            };
            let Ok(file) = J3dFile::parse(&bytes) else {
                continue;
            };
            if let Ok(preview) = file.geometry_preview_with_loader_flags(WIRE_FITTING_LOADER_FLAGS)
            {
                return Some(preview);
            }
        }
    }
    None
}

#[allow(clippy::too_many_arguments)]
fn append_fitting_instance(
    result: &mut ProceduralWirePreview,
    preview: &J3dGeometryPreview,
    transform: Transform,
    model_index: usize,
    packet_base: usize,
    texture_base: usize,
    material_base: usize,
    texture_count: usize,
    material_count: usize,
) {
    result.source_vertices += preview.positions.len();
    result.source_triangles += preview.triangles.len();
    for position in &preview.positions {
        let position = transform_preview_point(*position, transform);
        merge_bounds(
            &mut result.bounds_min,
            &mut result.bounds_max,
            position,
            position,
        );
    }
    for triangle in &preview.triangles {
        let vertices = transform_preview_vertices(triangle.vertices, transform);
        if !triangle_vertices_are_finite(vertices) {
            continue;
        }
        result.triangles.push(PreviewTriangle {
            vertices,
            normals: triangle
                .normals
                .map(|normals| transform_preview_normals(normals, transform)),
            color_channels: triangle.color_channels,
            tex_coord_sets: triangle.tex_coord_sets,
            material_index: triangle.material_index.and_then(|index| {
                let index = material_base + index;
                (index < material_count).then_some(index)
            }),
            packet_index: packet_base + triangle.packet_index,
            model_index,
            render_layer: PreviewRenderLayer::Main,
            color: triangle.color,
            vertex_colors: triangle.vertex_colors,
            combine_mode: triangle.combine_mode,
            tex_coords: triangle.tex_coords,
            texture_index: triangle.texture_index.and_then(|index| {
                let index = texture_base + index;
                (index < texture_count).then_some(index)
            }),
            mask_tex_coords: triangle.mask_tex_coords,
            mask_texture_index: triangle.mask_texture_index.and_then(|index| {
                let index = texture_base + index;
                (index < texture_count).then_some(index)
            }),
            cull_mode: triangle.cull_mode,
            alpha_compare: triangle.alpha_compare,
            blend_mode: triangle.blend_mode,
            z_mode: triangle.z_mode,
            billboard: triangle.billboard.and_then(|billboard| {
                transform_j3d_billboard(
                    billboard,
                    transform,
                    triangle
                        .normals
                        .map(|normals| transform_preview_normals(normals, transform)),
                )
            }),
            particle_type: None,
            particle_pivot: None,
            particle_direction: None,
            particle_color_mode: None,
            particle_environment_color: None,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_retail_sagged_triangular_prism() {
        let definition = WireDefinition {
            center: [100.0, 200.0, 300.0],
            rotation_degrees: [0.0, 90.0, 0.0],
            dimensions: [100.0, 200.0, 500.0],
        };
        let path = wire_path(definition);
        assert_eq!(path.first().copied(), Some([-150.0, 400.0, 300.0]));
        assert_eq!(path.last().copied(), Some([350.0, 400.0, 300.0]));
        assert!(path.iter().any(|point| point[1] < 310.0));

        let mut triangles = Vec::new();
        push_wire_prism(&mut triangles, &path, WireStyle::default(), 7, 11);
        assert!(!triangles.is_empty());
        assert!(triangles.iter().all(|triangle| triangle.model_index == 7));
        assert!(triangles.iter().all(|triangle| triangle.packet_index == 11));
        assert!(triangles
            .iter()
            .any(|triangle| { triangle.color_channels[0] == Some([DEFAULT_UPPER_COLOR; 3]) }));
        assert!(triangles
            .iter()
            .any(|triangle| { triangle.color_channels[0] == Some([DEFAULT_LOWER_COLOR; 3]) }));
    }

    #[test]
    fn wire_definition_uses_explicit_cube_layout_instead_of_actor_transform() {
        let definition = wire_definition_from_cube(sms_formats::JDramaCubeGeneralInfo {
            center: [100.0, 200.0, 300.0],
            rotation_degrees: [0.0, 90.0, 0.0],
            dimensions: [400.0, 500.0, 600.0],
            flags: 0,
            data_no: 0,
        })
        .expect("valid wire cube");

        assert_eq!(definition.center, [100.0, 200.0, 300.0]);
        assert_eq!(definition.rotation_degrees, [0.0, 90.0, 0.0]);
        assert_eq!(definition.dimensions, [400.0, 500.0, 600.0]);
    }

    #[test]
    #[ignore = "requires SMS_BASE_ROOT with extracted retail assets"]
    fn representative_stages_load_their_wire_tables_when_assets_are_available() {
        let base_root = std::env::var("SMS_BASE_ROOT")
            .or_else(|_| std::env::var("SMS_WIRE_TEST_BASE_ROOT"))
            .expect("set SMS_BASE_ROOT to an extracted retail base root");
        for (stage, expected) in [("bianco0", 13), ("mamma0", 16), ("mare0", 1)] {
            let document = StageDocument::open(&base_root, stage).unwrap();
            assert_eq!(load_wire_definitions(&document).len(), expected, "{stage}");
            let mut textures = Vec::new();
            let mut materials = Vec::new();
            let preview =
                build_procedural_wire_preview(&document, 1, 0, &mut textures, &mut materials);
            assert_eq!(preview.wire_count, expected, "{stage}");
            assert!(!preview.triangles.is_empty(), "{stage}");
            assert!(!materials.is_empty(), "{stage} wire fittings");
            assert!(bounds_are_finite(preview.bounds_min, preview.bounds_max));
        }

        let document = StageDocument::open(&base_root, "bianco0").unwrap();
        let style = load_wire_style(&document);
        assert_eq!(style.draw_width, 10.0);
        assert_eq!(style.draw_height, 20.0);
        assert_eq!(style.upper_surface, [200, 200, 200, 255]);
        assert_eq!(style.lower_surface, [128, 128, 128, 255]);
    }
}
