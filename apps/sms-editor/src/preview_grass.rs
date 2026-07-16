use super::*;

const GRASS_WIDTH: f32 = 12.0;
const GRASS_SWING_WIDTH: f32 = 50.0;
const GRASS_SWING_PHASE_STEP: f32 = 0.2;
const GRASS_RAND_SCALE: f32 = 1.0 / 32_768.0;
const MAX_GRASS_BLADES_PER_STAGE: usize = 250_000;
const POINTS_PER_GRASS_GROUP: usize = 500;

const GRASS_UPPER_COLOR: [u8; 4] = [60, 200, 60, 255];
const GRASS_LOWER_COLOR: [u8; 4] = [40, 100, 40, 255];

pub(super) struct ProceduralGrassPreview {
    pub(super) points: Vec<PreviewPoint>,
    pub(super) triangles: Vec<PreviewTriangle>,
    pub(super) object_model_indices: Vec<(String, usize)>,
    pub(super) group_count: usize,
    pub(super) blade_count: usize,
    pub(super) bounds_min: [f32; 3],
    pub(super) bounds_max: [f32; 3],
}

pub(super) fn build_procedural_grass_preview(
    document: &StageDocument,
    first_model_index: usize,
    packet_index: usize,
) -> ProceduralGrassPreview {
    let mut preview = ProceduralGrassPreview {
        points: Vec::new(),
        triangles: Vec::new(),
        object_model_indices: Vec::new(),
        group_count: 0,
        blade_count: 0,
        bounds_min: [f32::INFINITY; 3],
        bounds_max: [f32::NEG_INFINITY; 3],
    };
    let mut random = SmsRand::new(1);

    for object in document
        .objects
        .iter()
        .filter(|object| is_map_obj_grass_group(object))
    {
        let Some(authored_count) = object
            .raw_params
            .get("grass_blade_count")
            .and_then(|value| value.parse::<usize>().ok())
        else {
            continue;
        };
        let remaining = MAX_GRASS_BLADES_PER_STAGE.saturating_sub(preview.blade_count);
        let blade_count = authored_count.min(remaining);
        if blade_count == 0 {
            continue;
        }

        let model_index = first_model_index + preview.group_count;
        preview
            .object_model_indices
            .push((object.id.clone(), model_index));
        preview.group_count += 1;
        preview.blade_count += blade_count;

        let scale_x = object.transform.scale[0] * 100.0;
        let scale_y = object.transform.scale[1] * 200.0;
        let scale_z = object.transform.scale[2] * 100.0;
        let base_y = object.transform.translation[1];
        let point_stride = (blade_count / POINTS_PER_GRASS_GROUP).max(1);

        for blade_index in 0..blade_count {
            // TMapObjGrassGroup::load consumes the MSL rand sequence in X, Z,
            // Y order. A stable editor seed keeps the authored random field
            // reproducible while preserving the retail distribution.
            let x = scale_x * random.next_unit() * 2.0 + object.transform.translation[0] - scale_x;
            let z = scale_z * random.next_unit() * 2.0 + object.transform.translation[2] - scale_z;
            let top_y = scale_y * random.next_unit() + base_y + 100.0;
            let swing =
                GRASS_SWING_WIDTH * ((blade_index % 10) as f32 * GRASS_SWING_PHASE_STEP).sin();
            let center = [x, base_y, z];
            let offsets = [
                [-GRASS_WIDTH, 0.0, 0.0],
                [swing, top_y - base_y, 0.0],
                [GRASS_WIDTH, 0.0, 0.0],
            ];
            let vertices = offsets.map(|offset| {
                [
                    center[0] + offset[0],
                    center[1] + offset[1],
                    center[2] + offset[2],
                ]
            });

            if blade_index % point_stride == 0 {
                preview.points.push(PreviewPoint {
                    position: vertices[1],
                    model_index,
                });
            }
            merge_bounds(
                &mut preview.bounds_min,
                &mut preview.bounds_max,
                [
                    x - GRASS_WIDTH - GRASS_SWING_WIDTH,
                    base_y.min(top_y),
                    z - GRASS_WIDTH,
                ],
                [
                    x + GRASS_WIDTH + GRASS_SWING_WIDTH,
                    base_y.max(top_y),
                    z + GRASS_WIDTH,
                ],
            );

            preview.triangles.push(PreviewTriangle {
                vertices,
                normals: Some([[0.0, 0.0, 1.0]; 3]),
                color_channels: [
                    Some([GRASS_LOWER_COLOR, GRASS_UPPER_COLOR, GRASS_LOWER_COLOR]),
                    None,
                ],
                tex_coord_sets: [None; 8],
                material_index: None,
                packet_index,
                model_index,
                render_layer: PreviewRenderLayer::Main,
                color: None,
                vertex_colors: Some([GRASS_LOWER_COLOR, GRASS_UPPER_COLOR, GRASS_LOWER_COLOR]),
                combine_mode: J3dPreviewCombineMode::VertexOnly,
                tex_coords: None,
                texture_index: None,
                mask_tex_coords: None,
                mask_texture_index: None,
                cull_mode: Some(0),
                alpha_compare: None,
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
                billboard: Some(J3dBillboard {
                    mode: J3dBillboardMode::YAxis,
                    center,
                    axes: [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]],
                    offsets,
                    normals: Some([[0.0, 0.0, 1.0]; 3]),
                }),
                particle_type: None,
                particle_pivot: None,
                particle_direction: None,
                particle_color_mode: None,
                particle_environment_color: None,
            });
        }

        if preview.blade_count == MAX_GRASS_BLADES_PER_STAGE {
            break;
        }
    }

    preview
}

fn is_map_obj_grass_group(object: &SceneObject) -> bool {
    object.factory_name == "MapObjGrassGroup"
        || object
            .class_name
            .as_deref()
            .is_some_and(|name| name == "MapObjGrassGroup")
}

struct SmsRand {
    next: u32,
}

impl SmsRand {
    fn new(seed: u32) -> Self {
        Self { next: seed }
    }

    fn next_unit(&mut self) -> f32 {
        self.next = 0x41c6_4e6d_u32.wrapping_mul(self.next).wrapping_add(12_345);
        ((self.next >> 16) & 0x7fff) as f32 * GRASS_RAND_SCALE
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_camera_facing_retail_grass_triangles() {
        let mut document = StageDocument {
            stage_id: "dolpic_ex3".to_string(),
            base_root: PathBuf::new(),
            assets: Vec::new(),
            objects: Vec::new(),
            changed_files: BTreeMap::new(),
            registry: None,
            load_issues: Vec::new(),
            lighting: Default::default(),
            actor_previews: BTreeMap::new(),
            loaded_project: None,
        };
        let mut grass = SceneObject::new("grass-0", "MapObjGrassGroup");
        grass.transform.translation = [100.0, 20.0, -50.0];
        grass.transform.scale = [2.0, 1.5, 3.0];
        grass
            .raw_params
            .insert("grass_blade_count".to_string(), "3".to_string().into());
        document.objects.push(grass);

        let preview = build_procedural_grass_preview(&document, 7, 11);

        assert_eq!(preview.group_count, 1);
        assert_eq!(preview.blade_count, 3);
        assert_eq!(preview.triangles.len(), 3);
        assert_eq!(preview.object_model_indices, [("grass-0".to_string(), 7)]);
        let triangle = preview.triangles[0];
        assert_eq!(triangle.packet_index, 11);
        assert_eq!(triangle.model_index, 7);
        assert_eq!(
            triangle.color_channels[0],
            Some([GRASS_LOWER_COLOR, GRASS_UPPER_COLOR, GRASS_LOWER_COLOR])
        );
        assert!(matches!(
            triangle.billboard.map(|billboard| billboard.mode),
            Some(J3dBillboardMode::YAxis)
        ));
        assert!(bounds_are_finite(preview.bounds_min, preview.bounds_max));
    }

    #[test]
    fn sms_rand_matches_msl_lcg() {
        let mut random = SmsRand::new(1);
        assert_eq!(random.next_unit(), 16_838.0 * GRASS_RAND_SCALE);
        assert_eq!(random.next_unit(), 5_758.0 * GRASS_RAND_SCALE);
    }

    #[test]
    fn grass_preview_identity_is_case_sensitive() {
        assert!(is_map_obj_grass_group(&SceneObject::new(
            "exact",
            "MapObjGrassGroup"
        )));
        assert!(!is_map_obj_grass_group(&SceneObject::new(
            "wrong-case",
            "mapobjgrassgroup"
        )));
    }

    #[test]
    #[ignore = "requires SMS_GRASS_TEST_BASE_ROOT with extracted retail assets"]
    fn dolpic_ex3_builds_all_retail_grass_when_assets_are_available() {
        let base_root = std::env::var("SMS_GRASS_TEST_BASE_ROOT")
            .expect("set SMS_GRASS_TEST_BASE_ROOT to an extracted retail base root");
        let document = StageDocument::open(base_root, "dolpic_ex3").unwrap();

        let preview = SmsEditorApp::build_model_preview(
            &document,
            PreviewVisibility {
                environment: true,
                goop: true,
                effects: true,
            },
        )
        .unwrap();
        let grass_triangles = preview
            .triangles
            .iter()
            .filter(|triangle| {
                triangle.color_channels[0]
                    == Some([GRASS_LOWER_COLOR, GRASS_UPPER_COLOR, GRASS_LOWER_COLOR])
                    && matches!(
                        triangle.billboard.map(|billboard| billboard.mode),
                        Some(J3dBillboardMode::YAxis)
                    )
            })
            .count();

        assert_eq!(grass_triangles, 84_000);
        assert_eq!(
            document
                .objects
                .iter()
                .filter(|object| is_map_obj_grass_group(object))
                .count(),
            4
        );
    }
}
