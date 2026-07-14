use super::*;

use std::{env, fs, path::PathBuf};

const BASE_ROOT_ENV: &str = "SMS_NOKI_TEST_BASE_ROOT";
const OUTPUT_ENV: &str = "SMS_NOKI_TEST_OUTPUT";

#[test]
#[ignore = "requires an extracted retail base root and is a manual performance probe"]
fn profiles_dolpic0_preview_and_animation_updates() {
    let base_root = env::var_os(BASE_ROOT_ENV)
        .map(PathBuf::from)
        .unwrap_or_else(|| panic!("set {BASE_ROOT_ENV} to the extracted game's data directory"));
    let decomp_root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../..");
    let registry = SchemaGenerator::new(decomp_root)
        .generate()
        .expect("generate decomp-derived object metadata");
    let document = StageDocument::open(&base_root, "dolpic0")
        .expect("open dolpic0")
        .with_registry(registry);

    let build_started = Instant::now();
    let preview = SmsEditorApp::build_model_preview(
        &document,
        PreviewVisibility {
            environment: true,
            goop: true,
            effects: false,
        },
    )
    .expect("build Dolpic preview");
    let build_elapsed = build_started.elapsed();
    let triangle_count = preview.triangles.len();
    let animated_model_count = preview.animated_models.len();
    let rotating_model_count = preview.rotating_models.len();
    let actor_particle_count = preview.actor_particles.len();

    let gpu_started = Instant::now();
    let gpu_viewport = gpu_viewport::GpuViewportScene::from_preview(
        &preview,
        eframe::wgpu::TextureFormat::Bgra8UnormSrgb,
    );
    let gpu_elapsed = gpu_started.elapsed();
    let mut app = SmsEditorApp {
        document: Some(document),
        model_preview: Some(preview),
        gpu_viewport: Some(gpu_viewport),
        ..SmsEditorApp::default()
    };

    eprintln!(
        "dolpic0 preview: build={build_elapsed:?}, gpu_prepare={gpu_elapsed:?}, triangles={triangle_count}, animated_models={animated_model_count}, rotating_models={rotating_model_count}, actor_particles={actor_particle_count}"
    );
    for seconds in [1_u64, 10, 60, 300] {
        app.animation_started_at = Instant::now() - std::time::Duration::from_secs(seconds);
        app.last_skeletal_animation_tick = u64::MAX;
        let started = Instant::now();
        app.update_skeletal_animations();
        eprintln!(
            "dolpic0 animation sample at {seconds}s: {:?}",
            started.elapsed()
        );
    }

    let base_preview = app
        .model_preview
        .take()
        .expect("Dolpic preview remains loaded");
    let measure_cpu = |label: &str, preview: ModelPreview| {
        let mut app = SmsEditorApp {
            model_preview: Some(preview),
            animation_started_at: Instant::now() - std::time::Duration::from_secs(60),
            last_skeletal_animation_tick: u64::MAX,
            ..SmsEditorApp::default()
        };
        let started = Instant::now();
        app.update_skeletal_animations();
        eprintln!("dolpic0 {label} CPU sample: {:?}", started.elapsed());
        std::hint::black_box(app.model_preview.take().expect("profile preview"));
    };

    measure_cpu("full", base_preview.clone());
    let mut skeletal = base_preview.clone();
    skeletal.rotating_models.clear();
    skeletal.actor_particles.clear();
    skeletal.texture_pattern_animations.clear();
    measure_cpu("skeletal-only", skeletal);
    let mut rotating = base_preview.clone();
    rotating.animated_models.clear();
    rotating.actor_particles.clear();
    rotating.texture_pattern_animations.clear();
    measure_cpu("rotating-only", rotating);
    let mut particles = base_preview.clone();
    particles.animated_models.clear();
    particles.rotating_models.clear();
    particles.texture_pattern_animations.clear();
    measure_cpu("particles-only", particles);
}

#[test]
#[ignore = "requires an extracted retail base root"]
fn renders_maremb_body_and_accessories_to_screenshot() {
    let base_root = env::var_os(BASE_ROOT_ENV)
        .map(PathBuf::from)
        .unwrap_or_else(|| panic!("set {BASE_ROOT_ENV} to the extracted game's data directory"));
    let output = env::var_os(OUTPUT_ENV)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("target/noki-render/maremb-accessories.bmp"));

    let document = open_mare0_with_schema(&base_root);
    let object = document
        .objects
        .iter()
        .find(|object| {
            object.factory_name.eq_ignore_ascii_case("NPCMareMB")
                && object
                    .raw_params
                    .get("npc_parts_mask")
                    .and_then(|mask| mask.parse::<u32>().ok())
                    .is_some_and(|mask| mask & (1 << 9) != 0)
        })
        .cloned()
        .expect("mare0 should contain the MareMB fishing NPC");
    let preview = SmsEditorApp::build_model_preview(
        &document,
        PreviewVisibility {
            environment: false,
            goop: false,
            effects: false,
        },
    )
    .expect("build Noki preview");
    let model_index = *preview
        .object_model_indices
        .get(&object.id)
        .expect("Noki model index");
    let animated_instance = preview
        .animated_models
        .iter()
        .flat_map(|model| &model.instances)
        .find(|instance| instance.model_index == model_index)
        .expect("animated MareMB instance");
    for accessory in &animated_instance.accessories {
        let triangles = &preview.triangles[accessory.triangle_range.clone()];
        let textured = triangles
            .iter()
            .filter(|triangle| triangle.texture_index.is_some())
            .count();
        assert_eq!(
            textured,
            triangles.len(),
            "Noki accessory contains untextured triangles"
        );
    }
    assert_root_accessory_meets_hand_grip(&preview, model_index);
    let body_material = preview
        .triangles
        .iter()
        .filter(|triangle| triangle.model_index == model_index)
        .filter_map(|triangle| triangle.material_index)
        .map(|index| &preview.materials[index])
        .find(|material| material.name.eq_ignore_ascii_case("_body"))
        .expect("Noki body material");
    assert_eq!(
        body_material.tev_k_colors[0][3], 0,
        "runtime pollution initialization must hide the dirty-layer default"
    );
    render_isolated_noki(document, object, preview, model_index, output);
}

#[test]
#[ignore = "requires an extracted retail base root"]
fn renders_marem_instance_palette_to_screenshot() {
    let base_root = env::var_os(BASE_ROOT_ENV)
        .map(PathBuf::from)
        .unwrap_or_else(|| panic!("set {BASE_ROOT_ENV} to the extracted game's data directory"));
    let output = env::var_os("SMS_MAREM_TEST_OUTPUT")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("target/noki-render/marem-instance-palette.bmp"));

    let document = open_mare0_with_schema(&base_root);
    let object = document
        .objects
        .iter()
        .find(|object| {
            object.factory_name.eq_ignore_ascii_case("NPCMareM")
                && object
                    .raw_params
                    .get("npc_parts_mask")
                    .is_some_and(|mask| mask == "97")
        })
        .cloned()
        .expect("mare0 should contain the palette-3 MareM NPC");
    let preview = SmsEditorApp::build_model_preview(
        &document,
        PreviewVisibility {
            environment: false,
            goop: false,
            effects: false,
        },
    )
    .expect("build MareM preview");
    let model_index = *preview
        .object_model_indices
        .get(&object.id)
        .expect("MareM model index");
    let instance = preview
        .animated_models
        .iter()
        .flat_map(|model| &model.instances)
        .find(|instance| instance.model_index == model_index)
        .expect("animated MareM instance");
    let hat = instance.accessories.first().expect("MareM Hat A accessory");
    let hat_material = preview.triangles[hat.triangle_range.clone()]
        .iter()
        .filter_map(|triangle| triangle.material_index)
        .map(|index| &preview.materials[index])
        .find(|material| material.name.eq_ignore_ascii_case("_mat1"))
        .expect("MareM Hat A material");
    assert_eq!(hat_material.tev_colors[1], [10, 10, 10, 255]);
    assert_eq!(hat_material.tev_colors[2], [150, -30, 40, 255]);

    render_isolated_noki(document, object, preview, model_index, output);
}

fn render_isolated_noki(
    document: StageDocument,
    object: SceneObject,
    mut preview: ModelPreview,
    model_index: usize,
    output: PathBuf,
) {
    preview
        .triangles
        .retain(|triangle| triangle.model_index == model_index);
    assert!(
        !preview.triangles.is_empty(),
        "Noki preview has no triangles"
    );
    assert!(
        preview
            .triangles
            .iter()
            .any(|triangle| triangle.texture_index.is_some()),
        "Noki preview has no textured triangles"
    );

    let mut app = SmsEditorApp {
        document: Some(document),
        model_preview: Some(preview),
        ..SmsEditorApp::default()
    };
    let camera = app.renderer.camera_mut();
    camera.focus = [
        object.transform.translation[0],
        object.transform.translation[1] + 70.0,
        object.transform.translation[2],
    ];
    camera.yaw_degrees = object.transform.rotation_degrees[1] + 180.0;
    camera.pitch_degrees = -8.0;
    camera.distance = 360.0;

    let frame = app.camera_frame();
    let lighting = app
        .document
        .as_ref()
        .and_then(|document| document.lighting.object_lighting())
        .expect("mare0 object lighting");
    let size = [640, 640];
    let image = gpu_viewport::render_preview_offscreen(
        app.model_preview.as_ref().expect("Noki preview"),
        gpu_viewport::GpuViewportFrame {
            camera_position: frame.position,
            right: frame.right,
            up: frame.up,
            forward: frame.forward,
            focal: perspective_focal_length(
                egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(640.0, 640.0)),
                1.0,
            ),
            viewport_size: [640.0, 640.0],
            viewport_pan: [0.0; 2],
            near: 8.0,
            far: 20_000.0,
            animation_seconds: 0.0,
            light_position: lighting.position,
            light_color: gpu_viewport::color_u8_to_f32(lighting.color),
            ambient_color: Some(gpu_viewport::color_u8_to_f32(lighting.ambient)),
        },
        size,
    )
    .expect("render Noki WGPU framebuffer");
    let chromatic_pixels = image
        .pixels
        .iter()
        .filter(|pixel| {
            let [red, green, blue, _] = pixel.to_srgba_unmultiplied();
            let min = red.min(green).min(blue);
            let max = red.max(green).max(blue);
            max > 40 && max - min > 24
        })
        .count();
    assert!(
        chromatic_pixels > 2_000,
        "Noki render regressed to monochrome ({chromatic_pixels} chromatic pixels)"
    );
    if let Some(parent) = output.parent() {
        fs::create_dir_all(parent).expect("create screenshot directory");
    }
    write_bmp(&output, &image).expect("write Noki screenshot");
    eprintln!("Noki rendering screenshot: {}", output.display());
}

fn open_mare0_with_schema(base_root: &std::path::Path) -> StageDocument {
    let decomp_root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../..");
    let registry = SchemaGenerator::new(decomp_root)
        .generate()
        .expect("generate decomp-derived NPC metadata");
    StageDocument::open(base_root, "mare0")
        .expect("open mare0")
        .with_registry(registry)
}

fn assert_root_accessory_meets_hand_grip(preview: &ModelPreview, model_index: usize) {
    let source = preview
        .animated_models
        .iter()
        .find(|model| {
            model
                .instances
                .iter()
                .any(|instance| instance.model_index == model_index)
        })
        .expect("animated Noki source");
    let instance = source
        .instances
        .iter()
        .find(|instance| instance.model_index == model_index)
        .expect("animated Noki instance");
    let root_accessory = instance
        .accessories
        .iter()
        .find(|accessory| accessory.joint_index.is_none())
        .expect("root-attached fishing rod");
    let joint_names = source.file.joint_names().expect("Noki joint names");
    let hand_index = joint_names
        .iter()
        .position(|name| name.eq_ignore_ascii_case("migite"))
        .expect("Noki right hand joint");
    let matrices = source
        .file
        .joint_matrices_with_joint_animation(source.loader_flags, &source.animation, 0.0)
        .expect("Noki wait-pose matrices");
    let hand = [
        matrices[hand_index][0][3],
        matrices[hand_index][1][3],
        matrices[hand_index][2][3],
    ];
    let nearest_vertex_distance = root_accessory
        .local_triangles
        .iter()
        .flat_map(|triangle| triangle.vertices)
        .map(|vertex| {
            vertex
                .into_iter()
                .zip(hand)
                .map(|(vertex, hand)| (vertex - hand).powi(2))
                .sum::<f32>()
                .sqrt()
        })
        .fold(f32::INFINITY, f32::min);
    // The authored palm and rod surfaces do not share vertices. A nearby rod
    // vertex is a substantially tighter regression check than overlapping the
    // hand with the accessory's full (rod-length) bounding box.
    assert!(
        nearest_vertex_distance < 15.0,
        "root accessory no longer meets the animated hand grip: distance={nearest_vertex_distance}, hand={hand:?}"
    );
}

fn write_bmp(path: &std::path::Path, image: &egui::ColorImage) -> std::io::Result<()> {
    let width = image.size[0];
    let height = image.size[1];
    let row_size = (width * 3 + 3) & !3;
    let pixel_size = row_size * height;
    let file_size = 14 + 40 + pixel_size;
    let mut bytes = Vec::with_capacity(file_size);
    bytes.extend_from_slice(b"BM");
    bytes.extend_from_slice(&(file_size as u32).to_le_bytes());
    bytes.extend_from_slice(&[0; 4]);
    bytes.extend_from_slice(&(54u32).to_le_bytes());
    bytes.extend_from_slice(&(40u32).to_le_bytes());
    bytes.extend_from_slice(&(width as i32).to_le_bytes());
    bytes.extend_from_slice(&(height as i32).to_le_bytes());
    bytes.extend_from_slice(&(1u16).to_le_bytes());
    bytes.extend_from_slice(&(24u16).to_le_bytes());
    bytes.extend_from_slice(&[0; 4]);
    bytes.extend_from_slice(&(pixel_size as u32).to_le_bytes());
    bytes.extend_from_slice(&[0; 16]);

    for y in (0..height).rev() {
        for color in &image.pixels[y * width..(y + 1) * width] {
            let [red, green, blue, _] = color.to_srgba_unmultiplied();
            bytes.extend_from_slice(&[blue, green, red]);
        }
        bytes.resize(bytes.len() + row_size - width * 3, 0);
    }
    fs::write(path, bytes)
}
