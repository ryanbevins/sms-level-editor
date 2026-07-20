use std::fs;
use std::path::Path;

use base64::Engine;
use sms_authoring::{
    import_model, AuthoringError, CollisionDocument, CollisionGroup,
    CollisionSimplificationOptions, CollisionSource, CollisionSurface, CoordinateConversion,
    DiagnosticCode, ModelAssetDocument, ModelImportOptions,
};

fn triangle_buffer() -> Vec<u8> {
    let mut bytes = Vec::new();
    for value in [0.0f32, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0, 0.0] {
        bytes.extend_from_slice(&value.to_le_bytes());
    }
    for index in [0u16, 1, 2] {
        bytes.extend_from_slice(&index.to_le_bytes());
    }
    bytes
}

#[test]
fn optional_qem_simplification_is_deterministic_and_respects_target() {
    let original = CollisionDocument {
        vertices: vec![
            [0.0, 0.0, 0.0],
            [1.0, 0.0, 0.0],
            [1.0, 0.0, 1.0],
            [0.0, 0.0, 1.0],
        ],
        groups: vec![CollisionGroup {
            name: "flat".to_string(),
            surface: CollisionSurface::default(),
            triangles: vec![[0, 2, 1], [0, 3, 2]],
        }],
    };
    let options = CollisionSimplificationOptions {
        target_ratio: 0.5,
        max_error: 0.0,
    };
    let mut first = original.clone();
    let mut second = original;
    let first_report = first.simplify(&options).unwrap();
    let second_report = second.simplify(&options).unwrap();
    assert_eq!(first, second);
    assert_eq!(first_report, second_report);
    assert_eq!(first_report.input_triangles, 2);
    assert_eq!(first_report.target_triangles, 1);
    assert_eq!(first_report.output_triangles, 1);
    assert_eq!(first_report.maximum_applied_error, 0.0);
}

fn triangle_json(buffer_uri: &str, node_name: &str, mode: u32) -> String {
    format!(
        r#"{{
  "asset": {{"version": "2.0"}},
  "buffers": [{{"uri": {buffer_uri:?}, "byteLength": 42}}],
  "bufferViews": [
    {{"buffer": 0, "byteOffset": 0, "byteLength": 36}},
    {{"buffer": 0, "byteOffset": 36, "byteLength": 6}}
  ],
  "accessors": [
    {{"bufferView": 0, "componentType": 5126, "count": 3, "type": "VEC3", "max": [1,1,0], "min": [0,0,0]}},
    {{"bufferView": 1, "componentType": 5123, "count": 3, "type": "SCALAR"}}
  ],
  "meshes": [{{"name":"mesh", "primitives": [{{"attributes": {{"POSITION": 0}}, "indices": 1, "mode": {mode}}}]}}],
  "nodes": [{{"name":{node_name:?}, "mesh":0, "translation":[1,2,3]}}],
  "scenes": [{{"nodes":[0]}}],
  "scene": 0
}}"#
    )
}

fn write_triangle(directory: &Path, node_name: &str) -> std::path::PathBuf {
    let model_path = directory.join("model.gltf");
    fs::write(directory.join("mesh.bin"), triangle_buffer()).unwrap();
    fs::write(&model_path, triangle_json("mesh.bin", node_name, 4)).unwrap();
    model_path
}

#[test]
fn default_conversion_preserves_blender_gltf_axes_and_winding() {
    let conversion = CoordinateConversion::default();
    assert_eq!(
        conversion.basis,
        [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]]
    );
    assert!(!conversion.reverse_winding);
}

#[test]
fn imports_external_triangle_and_round_trips_native_asset() {
    let directory = tempfile::tempdir().unwrap();
    let path = write_triangle(directory.path(), "root");
    let imported = import_model(&path, &ModelImportOptions::default()).unwrap();
    let primitive = &imported.asset.meshes[0].primitives[0];
    assert_eq!(primitive.positions[1], [100.0, 0.0, 0.0]);
    assert_eq!(primitive.indices, [0, 1, 2]);
    assert_eq!(
        imported.asset.nodes[0].local_transform[3],
        [100.0, 200.0, 300.0, 1.0]
    );
    assert_eq!(
        imported.asset.converted_bounds().unwrap().unwrap(),
        sms_authoring::ModelBounds {
            min: [100.0, 200.0, 300.0],
            max: [200.0, 300.0, 300.0],
        }
    );
    assert!(imported
        .diagnostics
        .iter()
        .any(|diagnostic| diagnostic.code == DiagnosticCode::GeneratedNormals));

    let collision = imported.asset.collision.as_ref().unwrap();
    assert_eq!(collision.vertices[0], [100.0, 200.0, 300.0]);
    let encoded_col = collision.to_col_bytes().unwrap();
    let parsed_col = sms_formats::ColFile::parse(&encoded_col).unwrap();
    assert_eq!(parsed_col.groups()[0].triangles.len(), 1);

    let native = imported.asset.to_native_bytes().unwrap();
    let restored = ModelAssetDocument::from_native_bytes(&native).unwrap();
    assert_eq!(restored, imported.asset);
    assert_eq!(native, restored.to_native_bytes().unwrap());

    let preview = sms_formats::J3dFile::parse(imported.asset.compile_bmd().unwrap())
        .unwrap()
        .geometry_preview()
        .unwrap();
    let stage = &preview.materials[0].tev_stages[0];
    assert_eq!(stage.order.color_channel, 4);
    assert_eq!(stage.color_args, [10, 15, 15, 15]);
    assert_eq!(stage.alpha_args, [5, 7, 7, 7]);
}

#[test]
fn render_collision_normalizes_a_predominantly_inverted_terrain_shell() {
    let directory = tempfile::tempdir().unwrap();
    let mut bytes = Vec::new();
    for value in [0.0f32, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0] {
        bytes.extend_from_slice(&value.to_le_bytes());
    }
    for index in [0u16, 1, 2] {
        bytes.extend_from_slice(&index.to_le_bytes());
    }
    fs::write(directory.path().join("terrain.bin"), bytes).unwrap();
    let json = r#"{
  "asset": {"version": "2.0"},
  "buffers": [{"uri": "terrain.bin", "byteLength": 42}],
  "bufferViews": [
    {"buffer": 0, "byteOffset": 0, "byteLength": 36},
    {"buffer": 0, "byteOffset": 36, "byteLength": 6}
  ],
  "accessors": [
    {"bufferView": 0, "componentType": 5126, "count": 3, "type": "VEC3", "max": [1,0,1], "min": [0,0,0]},
    {"bufferView": 1, "componentType": 5123, "count": 3, "type": "SCALAR"}
  ],
  "meshes": [{"name":"terrain", "primitives": [{"attributes": {"POSITION": 0}, "indices": 1, "mode": 4}]}],
  "nodes": [{"name":"ground", "mesh":0}],
  "scenes": [{"nodes":[0]}],
  "scene": 0
}"#;
    let path = directory.path().join("terrain.gltf");
    fs::write(&path, json).unwrap();

    let imported = import_model(path, &ModelImportOptions::default()).unwrap();
    assert!(imported
        .diagnostics
        .iter()
        .any(|diagnostic| { diagnostic.code == DiagnosticCode::CollisionWindingNormalized }));
    let collision = imported.asset.collision.unwrap();
    let triangle = collision.groups[0].triangles[0];
    let [a, b, c] = triangle.map(|index| collision.vertices[index as usize]);
    let ab = [b[0] - a[0], b[1] - a[1], b[2] - a[2]];
    let ac = [c[0] - a[0], c[1] - a[1], c[2] - a[2]];
    let normal_y = ab[2] * ac[0] - ab[0] * ac[2];
    assert!(normal_y > 0.0, "normalized COL terrain must face upward");
}

#[test]
fn imports_base64_data_buffer_and_collision_only_node() {
    let directory = tempfile::tempdir().unwrap();
    let payload = base64::engine::general_purpose::STANDARD.encode(triangle_buffer());
    let uri = format!("data:application/octet-stream;base64,{payload}");
    let path = directory.path().join("model.gltf");
    fs::write(&path, triangle_json(&uri, "COL_ground", 4)).unwrap();
    let options = ModelImportOptions {
        collision: CollisionSource::EmbeddedNodes {
            prefix: "COL_".to_string(),
            selected_nodes: Default::default(),
            surfaces_by_node: Default::default(),
            default_surface: CollisionSurface {
                surface_type: 7,
                attribute_0: 4,
                attribute_1: 9,
                data: Some(-2),
            },
        },
        ..ModelImportOptions::default()
    };
    let imported = import_model(path, &options).unwrap();
    assert_eq!(
        imported.asset.nodes[0].purpose,
        sms_authoring::NodePurpose::CollisionOnly
    );
    let collision = imported.asset.collision.unwrap();
    assert_eq!(collision.groups[0].surface.surface_type, 7);
    let col = collision.to_col_file().unwrap();
    assert!(col.groups()[0].has_per_triangle_data);
    assert_eq!(col.groups()[0].triangles[0].data, Some(-2));
}

#[test]
fn rejects_traversal_network_and_unencodable_names() {
    let directory = tempfile::tempdir().unwrap();
    for (uri, expected_code) in [
        ("../outside.bin", "path_traversal"),
        ("https://example.invalid/model.bin", "external_uri"),
    ] {
        let path = directory.path().join(format!("{expected_code}.gltf"));
        fs::write(&path, triangle_json(uri, "root", 4)).unwrap();
        let error = import_model(path, &ModelImportOptions::default()).unwrap_err();
        assert!(matches!(error, AuthoringError::Security { code, .. } if code == expected_code));
    }

    let path = write_triangle(directory.path(), "emoji_😀");
    let error = import_model(path, &ModelImportOptions::default()).unwrap_err();
    assert!(
        matches!(error, AuthoringError::Unsupported { code, .. } if code == "unencodable_name")
    );
}

#[test]
fn rejects_non_triangle_primitives_explicitly() {
    let directory = tempfile::tempdir().unwrap();
    fs::write(directory.path().join("mesh.bin"), triangle_buffer()).unwrap();
    let path = directory.path().join("lines.gltf");
    fs::write(&path, triangle_json("mesh.bin", "root", 1)).unwrap();
    let error = import_model(path, &ModelImportOptions::default()).unwrap_err();
    assert!(
        matches!(error, AuthoringError::Unsupported { code, .. } if code == "non_triangle_primitive")
    );
}

#[test]
fn generated_oversized_inputs_respect_source_and_total_buffer_limits() {
    let directory = tempfile::tempdir().unwrap();
    let path = write_triangle(directory.path(), "root");

    let source_limited = ModelImportOptions {
        max_source_bytes: 64,
        ..ModelImportOptions::default()
    };
    let source_error = import_model(&path, &source_limited).unwrap_err();
    assert!(source_error.to_string().contains("limit"));

    let total_limited = ModelImportOptions {
        max_source_bytes: 16 * 1024,
        max_total_buffer_bytes: triangle_buffer().len() - 1,
        ..ModelImportOptions::default()
    };
    let total_error = import_model(&path, &total_limited).unwrap_err();
    assert!(total_error.to_string().contains("buffer"));
    assert!(total_error.to_string().contains("limit"));
}

#[test]
fn collision_cleanup_is_exact_deterministic_and_preserves_raw_surface_values() {
    let original = CollisionDocument {
        vertices: vec![
            [0.0, 0.0, 0.0],
            [1.0, 0.0, 0.0],
            [0.0, 1.0, 0.0],
            [-0.0, 0.0, 0.0],
        ],
        groups: vec![CollisionGroup {
            name: "ground".to_string(),
            surface: CollisionSurface {
                surface_type: 0x4321,
                attribute_0: 0xaa,
                attribute_1: 0x55,
                data: None,
            },
            triangles: vec![[0, 1, 2], [3, 1, 2], [0, 0, 1]],
        }],
    };
    let mut first = original.clone();
    let mut second = original;
    let report = first.cleanup_exact().unwrap();
    assert_eq!(report.welded_vertices, 1);
    assert_eq!(report.removed_duplicate_triangles, 1);
    assert_eq!(report.removed_degenerate_triangles, 1);
    second.cleanup_exact().unwrap();
    assert_eq!(first, second);
    let col = first.to_col_file().unwrap();
    assert_eq!(col.groups()[0].surface_type, 0x4321);
    assert_eq!(col.groups()[0].triangles[0].attribute_0, 0xaa);
    assert_eq!(col.groups()[0].triangles[0].attribute_1, 0x55);
}
