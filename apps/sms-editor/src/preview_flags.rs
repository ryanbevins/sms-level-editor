use super::*;
use sms_formats::{J3dColorChannel, J3dIndirectMaterial, J3dTevOrder, J3dTevStage, J3dTexGen};

const MAX_FLAG_GRID_DIMENSION: usize = 512;

#[derive(Default)]
pub(super) struct ProceduralFlagPreview {
    pub(super) points: Vec<PreviewPoint>,
    pub(super) triangles: Vec<PreviewTriangle>,
    pub(super) object_model_indices: Vec<(String, usize)>,
    pub(super) animated_flags: Vec<AnimatedFlagPreview>,
    pub(super) flag_count: usize,
    pub(super) source_vertices: usize,
    pub(super) source_triangles: usize,
    pub(super) source_textures: usize,
    pub(super) packet_count: usize,
    pub(super) bounds_min: [f32; 3],
    pub(super) bounds_max: [f32; 3],
}

pub(super) fn build_procedural_flag_preview(
    document: &StageDocument,
    first_model_index: usize,
    first_packet_index: usize,
    textures: &mut Vec<PreviewTexture>,
    materials: &mut Vec<J3dMaterial>,
) -> ProceduralFlagPreview {
    let mut preview = ProceduralFlagPreview {
        bounds_min: [f32::INFINITY; 3],
        bounds_max: [f32::NEG_INFINITY; 3],
        ..Default::default()
    };
    let Some(registry) = document.registry.as_ref() else {
        return preview;
    };
    let mut texture_cache = BTreeMap::<String, (usize, usize, usize)>::new();

    for definition in &registry.map_obj_flags {
        let flutter_speed = runtime_flag_flutter_speed(document, definition);
        for object in document
            .objects
            .iter()
            .filter(|object| flag_definition_matches_object(object, definition))
        {
            let Some(resource_name) = flag_resource_name(object, definition) else {
                continue;
            };
            if !definition
                .registered_texture_names
                .iter()
                .any(|known| known.eq_ignore_ascii_case(resource_name))
            {
                continue;
            }

            let texture_cache_key = resource_name.to_ascii_lowercase();
            let (texture_index, material_index, packet_index) =
                if let Some(cached) = texture_cache.get(&texture_cache_key) {
                    *cached
                } else {
                    let Some(asset) = find_flag_texture_asset(document, definition, resource_name)
                    else {
                        continue;
                    };
                    let Ok(bytes) = read_stage_asset_bytes(&asset.path) else {
                        continue;
                    };
                    let Ok(mut texture) = decode_bti_texture(bytes) else {
                        continue;
                    };
                    texture.name = resource_name.to_string();
                    let texture_index =
                        push_j3d_preview_textures(textures, std::slice::from_ref(&texture));
                    let material_index = materials.len();
                    materials.push(runtime_flag_material(
                        material_index,
                        texture_index,
                        resource_name,
                    ));
                    let packet_index = first_packet_index + texture_cache.len();
                    texture_cache.insert(
                        texture_cache_key,
                        (texture_index, material_index, packet_index),
                    );
                    (texture_index, material_index, packet_index)
                };

            let model_index = first_model_index + preview.flag_count;
            let Some(mut animation) = build_flag_animation(object, definition, flutter_speed)
            else {
                continue;
            };
            let grid = build_flag_grid(&animation, 0.0);
            preview.flag_count += 1;
            preview.source_vertices += grid.vertices.len();
            preview.source_triangles += grid.triangles.len();
            preview
                .object_model_indices
                .push((object.id.clone(), model_index));
            let point_start = preview.points.len();
            for vertex in &grid.vertices {
                preview.points.push(PreviewPoint {
                    position: vertex.position,
                    model_index,
                });
                merge_bounds(
                    &mut preview.bounds_min,
                    &mut preview.bounds_max,
                    vertex.position,
                    vertex.position,
                );
            }
            let triangle_start = preview.triangles.len();
            preview
                .triangles
                .extend(grid.triangles.into_iter().map(|triangle| {
                    flag_triangle(
                        triangle,
                        model_index,
                        packet_index,
                        material_index,
                        texture_index,
                    )
                }));
            animation.point_range = point_start..preview.points.len();
            animation.triangle_range = triangle_start..preview.triangles.len();
            preview.animated_flags.push(animation);
        }
    }

    preview.source_textures = texture_cache.len();
    preview.packet_count = texture_cache.len();
    preview
}

fn flag_resource_name<'a>(
    object: &'a SceneObject,
    definition: &sms_schema::MapObjFlagDefinition,
) -> Option<&'a str> {
    let resource_key = format!("stream_string_{}", definition.resource_name_stream_index);
    if definition.resource_name_stream_index == 0 {
        // TMapObjFlag::load reads its texture resource after TMapObjBase's
        // common TActor stream, so the first subclass string is the authored
        // selector. Keep stream_string_0 only for older overlays and synthetic
        // objects created before actor_tail_string was exposed.
        object
            .raw_param("actor_tail_string")
            .or_else(|| object.raw_param(&resource_key))
    } else {
        object.raw_param(&resource_key)
    }
}

fn flag_definition_matches_object(
    object: &SceneObject,
    definition: &sms_schema::MapObjFlagDefinition,
) -> bool {
    object.factory_name == definition.factory_name
}

fn find_flag_texture_asset<'a>(
    document: &'a StageDocument,
    definition: &sms_schema::MapObjFlagDefinition,
    resource_name: &str,
) -> Option<&'a StageAsset> {
    let runtime_path = definition
        .texture_path_pattern
        .replacen("%s", resource_name, 1)
        .replace('\\', "/");
    let archive_path = runtime_path
        .strip_prefix("/scene")
        .unwrap_or(&runtime_path)
        .to_ascii_lowercase();
    document.assets.iter().find(|asset| {
        asset.kind == StageAssetKind::Texture
            && asset
                .path
                .to_string_lossy()
                .replace('\\', "/")
                .to_ascii_lowercase()
                .ends_with(&archive_path)
    })
}

#[derive(Clone, Copy)]
struct FlagVertex {
    position: [f32; 3],
    uv: [f32; 2],
}

struct FlagGrid {
    vertices: Vec<FlagVertex>,
    triangles: Vec<[FlagVertex; 3]>,
}

fn build_flag_animation(
    object: &SceneObject,
    definition: &sms_schema::MapObjFlagDefinition,
    flutter_speed_degrees_per_frame: f32,
) -> Option<AnimatedFlagPreview> {
    let scale_y = object.transform.scale[1];
    let scale_z = object.transform.scale[2];
    if !scale_y.is_finite()
        || !scale_z.is_finite()
        || scale_y.abs() <= f32::EPSILON
        || scale_z.abs() <= f32::EPSILON
    {
        return None;
    }

    let scaled_height = 100.0 * scale_z;
    let scaled_width = 100.0 * scale_y;
    let rows = retail_flag_grid_dimension(scaled_height / 50.0)?;
    let cols = retail_flag_grid_dimension(scaled_width / 100.0)?;
    let step_height = scaled_height / (rows - 1) as f32;
    let step_width = scaled_width / (cols - 1) as f32;
    let flag_height = definition.default_height as f32 / scale_z;
    let flag_width = definition.default_width as f32 / scale_y;
    let segment_size = definition.default_segment_size as f32 * scale_z;
    if definition.phase_wrap_degrees == 0 {
        return None;
    }
    let mut transform = object.transform;
    // Retail bakes the authored Y/Z scale into the generated grid and submits
    // it through a rotation/translation-only local matrix.
    transform.scale = [1.0; 3];

    Some(AnimatedFlagPreview {
        transform,
        rows,
        cols,
        step_height,
        step_width,
        flag_height,
        flag_width,
        segment_size,
        initial_phase_degrees: stable_flag_phase(&object.id),
        flutter_speed_degrees_per_frame,
        phase_wrap_degrees: definition.phase_wrap_degrees as f32,
        point_range: 0..0,
        triangle_range: 0..0,
    })
}

fn build_flag_grid(source: &AnimatedFlagPreview, frame: f32) -> FlagGrid {
    let phase = flag_phase_at_frame(source, frame);
    let mut vertices = Vec::with_capacity(source.rows * source.cols);
    for col in 0..source.cols {
        for row in 0..source.rows {
            vertices.push(FlagVertex {
                position: flag_vertex_position(source, col, row, phase),
                uv: [
                    row as f32 / (source.rows - 1) as f32,
                    (source.cols - 1 - col) as f32 / (source.cols - 1) as f32,
                ],
            });
        }
    }

    let mut triangles = Vec::with_capacity((source.cols - 1) * (source.rows - 1) * 2);
    for col in 0..source.cols - 1 {
        for row in 0..source.rows - 1 {
            let a = vertices[col * source.rows + row];
            let b = vertices[(col + 1) * source.rows + row];
            let c = vertices[col * source.rows + row + 1];
            let d = vertices[(col + 1) * source.rows + row + 1];
            triangles.push([a, b, c]);
            triangles.push([c, b, d]);
        }
    }
    FlagGrid {
        vertices,
        triangles,
    }
}

fn flag_phase_at_frame(source: &AnimatedFlagPreview, frame: f32) -> f32 {
    (source.initial_phase_degrees + frame * source.flutter_speed_degrees_per_frame)
        .rem_euclid(source.phase_wrap_degrees)
}

fn flag_vertex_position(
    source: &AnimatedFlagPreview,
    col: usize,
    row: usize,
    phase: f32,
) -> [f32; 3] {
    let row_ratio = row as f32 / source.rows as f32;
    let wave_phase = phase - row as f32 * source.flag_height + col as f32 * source.flag_width;
    transform_preview_point(
        [
            source.segment_size * row_ratio * wave_phase.to_radians().sin(),
            col as f32 * source.step_width,
            row as f32 * source.step_height,
        ],
        source.transform,
    )
}

pub(super) fn animate_flag_preview(
    source: &AnimatedFlagPreview,
    frame: f32,
    points: &mut [PreviewPoint],
    triangles: &mut [PreviewTriangle],
) {
    let expected_points = source.rows * source.cols;
    let expected_triangles = (source.rows - 1) * (source.cols - 1) * 2;
    if source.point_range.len() != expected_points
        || source.triangle_range.len() != expected_triangles
        || source.point_range.end > points.len()
        || source.triangle_range.end > triangles.len()
    {
        return;
    }
    let phase = flag_phase_at_frame(source, frame);
    for col in 0..source.cols {
        for row in 0..source.rows {
            points[source.point_range.start + col * source.rows + row].position =
                flag_vertex_position(source, col, row, phase);
        }
    }
    let mut triangle_index = source.triangle_range.start;
    for col in 0..source.cols - 1 {
        for row in 0..source.rows - 1 {
            let point = |col: usize, row: usize| {
                points[source.point_range.start + col * source.rows + row].position
            };
            let a = point(col, row);
            let b = point(col + 1, row);
            let c = point(col, row + 1);
            let d = point(col + 1, row + 1);
            triangles[triangle_index].vertices = [a, b, c];
            triangles[triangle_index + 1].vertices = [c, b, d];
            triangle_index += 2;
        }
    }
}

fn runtime_flag_flutter_speed(
    document: &StageDocument,
    definition: &sms_schema::MapObjFlagDefinition,
) -> f32 {
    let Some(area_index) = runtime_stage_area_index(document, definition) else {
        return definition.default_flutter_speed_degrees_per_frame as f32;
    };
    definition
        .area_flutter_speeds
        .iter()
        .find(|speed| speed.area_index == area_index)
        .map_or(
            definition.default_flutter_speed_degrees_per_frame,
            |speed| speed.degrees_per_frame,
        ) as f32
}

fn runtime_stage_area_index(
    document: &StageDocument,
    definition: &sms_schema::MapObjFlagDefinition,
) -> Option<u32> {
    let relative = definition
        .stage_archive_table_path
        .trim_start_matches(['/', '\\']);
    let candidates = [
        document.base_root.join(relative),
        document.base_root.join("files").join(relative),
    ];
    let bytes = candidates
        .iter()
        .find_map(|path| std::fs::read(path).ok())?;
    sms_formats::parse_jdrama_scenario_archive_entries(&bytes)
        .ok()?
        .into_iter()
        .find(|entry| {
            std::path::Path::new(&entry.archive_name)
                .file_stem()
                .and_then(|stem| stem.to_str())
                .is_some_and(|stem| stem.eq_ignore_ascii_case(&document.stage_id))
        })
        .map(|entry| entry.area_index)
}

fn retail_flag_grid_dimension(raw: f32) -> Option<usize> {
    if !raw.is_finite() {
        return None;
    }
    let dimension = if raw < 2.0 { 3 } else { raw as usize };
    (dimension <= MAX_FLAG_GRID_DIMENSION).then_some(dimension)
}

fn stable_flag_phase(id: &str) -> f32 {
    let hash = id.bytes().fold(2_166_136_261_u32, |hash, byte| {
        (hash ^ u32::from(byte)).wrapping_mul(16_777_619)
    });
    (hash % 360) as f32
}

fn flag_triangle(
    vertices: [FlagVertex; 3],
    model_index: usize,
    packet_index: usize,
    material_index: usize,
    texture_index: usize,
) -> PreviewTriangle {
    let tex_coords = vertices.map(|vertex| vertex.uv);
    PreviewTriangle {
        vertices: vertices.map(|vertex| vertex.position),
        normals: None,
        color_channels: [None; 2],
        tex_coord_sets: [None; 8],
        material_index: Some(material_index),
        packet_index,
        model_index,
        render_layer: PreviewRenderLayer::Main,
        color: None,
        vertex_colors: None,
        combine_mode: J3dPreviewCombineMode::TextureOnly,
        tex_coords: Some(tex_coords),
        texture_index: Some(texture_index),
        mask_tex_coords: None,
        mask_texture_index: None,
        cull_mode: Some(0),
        alpha_compare: Some(J3dAlphaCompare {
            comp0: 4,
            ref0: 0,
            op: 0,
            comp1: 4,
            ref1: 0,
        }),
        blend_mode: Some(J3dBlendMode {
            mode: 1,
            src_factor: 1,
            dst_factor: 0,
            logic_op: 5,
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

fn runtime_flag_material(
    material_index: usize,
    texture_index: usize,
    resource_name: &str,
) -> J3dMaterial {
    let mut texture_indices = [None; 8];
    texture_indices[0] = Some(texture_index);
    J3dMaterial {
        name: format!("_runtime_flag_{resource_name}"),
        material_index,
        material_id: material_index,
        loader_flags: SMS_MAP_MODEL_LOAD_FLAGS,
        lighting_enabled: false,
        mode: 1,
        cull_mode: 0,
        color_channel_count: 0,
        material_colors: [[255; 4]; 2],
        ambient_colors: [[255; 4]; 2],
        color_channels: [J3dColorChannel::default(); 4],
        tex_gen_count: 1,
        tex_gens: std::array::from_fn(|slot| J3dTexGen {
            gen_type: 1,
            source: 4 + slot as u8,
            matrix: 60,
        }),
        tex_matrices: [None; 8],
        texture_indices,
        tev_colors: [[0; 4]; 4],
        tev_k_colors: [[255; 4]; 4],
        tev_stages: vec![J3dTevStage {
            order: J3dTevOrder {
                tex_coord: Some(0),
                tex_map: Some(0),
                color_channel: 4,
            },
            // TMapObjFlagManager::initDraw passes TEXC/TEXA as A with both
            // blend inputs zero, producing the texture without raster color.
            color_args: [8, 15, 15, 15],
            color_op: 0,
            color_bias: 0,
            color_scale: 0,
            color_clamp: 1,
            color_register: 0,
            alpha_args: [4, 7, 7, 7],
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
        alpha_compare: J3dAlphaCompare {
            comp0: 4,
            ref0: 0,
            op: 0,
            comp1: 4,
            ref1: 0,
        },
        blend_mode: J3dBlendMode {
            mode: 1,
            src_factor: 1,
            dst_factor: 0,
            logic_op: 5,
        },
        z_mode: J3dZMode {
            compare_enable: 1,
            func: 3,
            update_enable: 1,
        },
        z_comp_loc: 0,
        dither: 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn definition() -> sms_schema::MapObjFlagDefinition {
        sms_schema::MapObjFlagDefinition {
            factory_name: "DerivedFlagFactory".to_string(),
            class_name: "TDerivedFlag".to_string(),
            texture_path_pattern: "/scene/mapObj/%s.bti".to_string(),
            registered_texture_names: vec!["derivedTexture".to_string()],
            resource_name_stream_index: 0,
            default_height: 125,
            default_width: 130,
            default_segment_size: 20,
            default_flutter_speed_degrees_per_frame: 8,
            area_flutter_speeds: vec![sms_schema::MapObjFlagAreaSpeed {
                area_index: 0,
                degrees_per_frame: 16,
            }],
            phase_wrap_degrees: 360,
            stage_archive_table_path: "/data/stageArc.bin".to_string(),
            source_file: "src/MoveBG/MapObjFlag.cpp".to_string(),
        }
    }

    #[test]
    fn retail_flag_grid_uses_authored_yz_scale_and_two_sided_state() {
        let mut object = SceneObject::new("flag-0", "DerivedFlagFactory");
        object.transform.translation = [10.0, 20.0, 30.0];
        object.transform.scale = [9.0, 2.0, 1.5];
        let animation =
            build_flag_animation(&object, &definition(), 16.0).expect("build flag animation");
        let grid = build_flag_grid(&animation, 0.0);

        assert_eq!(grid.vertices.len(), 6);
        assert_eq!(grid.triangles.len(), 4);
        assert_eq!(grid.vertices[0].position, [10.0, 20.0, 30.0]);
        assert_eq!(grid.vertices[0].uv, [0.0, 1.0]);
        assert_eq!(grid.vertices[5].uv, [1.0, 0.0]);
        let triangle = flag_triangle(grid.triangles[0], 4, 5, 6, 7);
        assert_eq!(triangle.combine_mode, J3dPreviewCombineMode::TextureOnly);
        assert_eq!(triangle.cull_mode, Some(0));
        assert_eq!(triangle.material_index, Some(6));
        assert_eq!(triangle.texture_index, Some(7));
        assert_eq!(triangle.alpha_compare.expect("alpha compare").comp0, 4);
        assert_eq!(triangle.z_mode.expect("z mode").update_enable, 1);
    }

    #[test]
    fn flag_animation_updates_only_its_generated_geometry() {
        let mut object = SceneObject::new("flag-0", "DerivedFlagFactory");
        object.transform.scale = [1.0, 2.0, 1.5];
        let mut animation =
            build_flag_animation(&object, &definition(), 16.0).expect("build flag animation");
        let grid = build_flag_grid(&animation, 0.0);
        let mut points = grid
            .vertices
            .iter()
            .map(|vertex| PreviewPoint {
                position: vertex.position,
                model_index: 0,
            })
            .collect::<Vec<_>>();
        let mut triangles = grid
            .triangles
            .into_iter()
            .map(|triangle| flag_triangle(triangle, 0, 0, 0, 0))
            .collect::<Vec<_>>();
        animation.point_range = 0..points.len();
        animation.triangle_range = 0..triangles.len();
        let initial_points = points.clone();

        animate_flag_preview(&animation, 1.0, &mut points, &mut triangles);

        assert!(points
            .iter()
            .zip(initial_points)
            .any(|(animated, initial)| animated.position[0] != initial.position[0]));
        assert_eq!(triangles[0].vertices[0], points[0].position);
        assert_eq!(triangles[0].vertices[1], points[animation.rows].position);
    }

    #[test]
    fn renderer_identity_and_texture_names_come_from_schema_definition() {
        let mut registry = ObjectRegistry::default();
        registry.map_obj_flags.push(definition());
        let mut document = StageDocument {
            stage_id: "test".to_string(),
            base_root: PathBuf::new(),
            assets: Vec::new(),
            objects: vec![SceneObject::new("flag", "MapObjFlag")],
            changed_files: BTreeMap::new(),
            registry: Some(registry),
            load_issues: Vec::new(),
            lighting: Default::default(),
            actor_previews: BTreeMap::new(),
            loaded_project: None,
        };
        document.objects[0].raw_params.insert(
            "stream_string_0".to_string(),
            "derivedTexture".to_string().into(),
        );

        let preview =
            build_procedural_flag_preview(&document, 1, 2, &mut Vec::new(), &mut Vec::new());
        assert_eq!(
            preview.flag_count, 0,
            "the non-schema factory must not match"
        );
        assert!(flag_definition_matches_object(
            &SceneObject::new("exact", "DerivedFlagFactory"),
            &definition()
        ));
        assert!(!flag_definition_matches_object(
            &SceneObject::new("wrong-case", "derivedflagfactory"),
            &definition()
        ));
    }

    #[test]
    fn flag_texture_selector_prefers_the_subclass_tail_over_actor_character() {
        let definition = definition();
        let mut object = SceneObject::new("flag", "DerivedFlagFactory");
        object.set_raw_param("stream_string_0", "shared flag character");
        object.set_raw_param("actor_tail_string", "derivedTexture");

        assert_eq!(
            flag_resource_name(&object, &definition),
            Some("derivedTexture")
        );
    }

    #[test]
    fn runtime_flag_material_outputs_the_texture_without_raster_shading() {
        let material = runtime_flag_material(3, 5, "derivedTexture");

        assert!(!material.lighting_enabled);
        assert_eq!(material.color_channel_count, 0);
        assert_eq!(material.texture_indices[0], Some(5));
        assert_eq!(material.tev_stages.len(), 1);
        assert_eq!(material.tev_stages[0].color_args, [8, 15, 15, 15]);
        assert_eq!(material.tev_stages[0].alpha_args, [4, 7, 7, 7]);
        assert_eq!(material.tev_stages[0].order.color_channel, 4);
    }

    #[test]
    #[ignore = "requires SMS_BASE_ROOT with extracted retail assets"]
    fn dolpic_flags_use_retail_bti_assets_when_available() {
        let base_root = std::env::var("SMS_BASE_ROOT")
            .or_else(|_| std::env::var("SMS_FLAG_TEST_BASE_ROOT"))
            .expect("set SMS_BASE_ROOT to an extracted retail base root");
        let decomp_root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../..");
        let registry = SchemaGenerator::new(decomp_root)
            .generate()
            .expect("generate schema");
        let document = StageDocument::open(base_root, "dolpic0")
            .expect("open dolpic0")
            .with_registry(registry);
        let preview = SmsEditorApp::build_model_preview(
            &document,
            PreviewVisibility {
                environment: true,
                goop: true,
                effects: true,
            },
        )
        .expect("build preview");
        let placed_flags = document
            .objects
            .iter()
            .filter(|object| object.factory_name == "MapObjFlag")
            .count();
        let rendered_flags = document
            .objects
            .iter()
            .filter(|object| object.factory_name == "MapObjFlag")
            .filter(|object| preview.object_model_indices.contains_key(&object.id))
            .count();
        let flag_materials = preview
            .materials
            .iter()
            .filter(|material| material.name.starts_with("_runtime_flag_"))
            .collect::<Vec<_>>();

        assert_eq!(placed_flags, 9);
        assert_eq!(rendered_flags, placed_flags);
        assert_eq!(preview.animated_flags.len(), placed_flags);
        let definition = document
            .registry
            .as_ref()
            .and_then(|registry| registry.map_obj_flags.first())
            .expect("generated flag definition");
        let area_index = runtime_stage_area_index(&document, definition)
            .expect("resolve runtime area from the extracted stage archive table");
        let expected_speed = definition
            .area_flutter_speeds
            .iter()
            .find(|speed| speed.area_index == area_index)
            .map_or(
                definition.default_flutter_speed_degrees_per_frame,
                |speed| speed.degrees_per_frame,
            ) as f32;
        assert!(preview
            .animated_flags
            .iter()
            .all(|flag| flag.flutter_speed_degrees_per_frame == expected_speed));
        assert_eq!(flag_materials.len(), 3);
        assert!(flag_materials.iter().all(|material| {
            !material.lighting_enabled
                && material.color_channel_count == 0
                && material.tev_stages[0].color_args == [8, 15, 15, 15]
        }));
    }
}
