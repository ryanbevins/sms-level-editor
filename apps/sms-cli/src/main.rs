use std::path::PathBuf;
use std::process::Command;

use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};
use sms_formats::{
    discover_scene_archives, read_stage_asset_bytes, J3dFile, J3dTriangle, StageAssetKind,
};
use sms_scene::StageDocument;
use sms_schema::SchemaGenerator;

#[derive(Debug, Parser)]
#[command(name = "sms-cli")]
#[command(about = "Super Mario Sunshine editor automation CLI")]
struct Args {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Debug, Subcommand)]
enum Commands {
    /// Extract a disc image with nodtool.
    Extract {
        #[arg(long)]
        image: PathBuf,
        #[arg(long)]
        out: PathBuf,
        #[arg(long, default_value = "nodtool")]
        nodtool: PathBuf,
    },
    /// Generate and print decomp-derived object schema.
    Schema {
        #[arg(long, default_value = "..")]
        repo_root: PathBuf,
    },
    /// List scene archives discovered under an extracted base root.
    Scenes {
        #[arg(long)]
        base_root: PathBuf,
    },
    /// List assets discovered for a stage, including mounted archive entries.
    Assets {
        #[arg(long)]
        base_root: PathBuf,
        #[arg(long)]
        stage: String,
        #[arg(long)]
        kind: Option<String>,
    },
    /// Extract a discovered asset path, including archive virtual paths with !/.
    ExtractAsset {
        #[arg(long)]
        asset: String,
        #[arg(long)]
        out: PathBuf,
    },
    /// Open a stage and print a compact summary.
    OpenStage {
        #[arg(long)]
        base_root: PathBuf,
        #[arg(long)]
        stage: String,
        #[arg(long, default_value = "..")]
        repo_root: PathBuf,
    },
    /// List parsed retail placement objects for a stage.
    Objects {
        #[arg(long)]
        base_root: PathBuf,
        #[arg(long)]
        stage: String,
        #[arg(long, default_value_t = 80)]
        limit: usize,
    },
    /// Print per-model preview diagnostics for the stage renderer.
    PreviewStats {
        #[arg(long)]
        base_root: PathBuf,
        #[arg(long)]
        stage: String,
        #[arg(long)]
        filter: Option<String>,
        #[arg(long, default_value_t = false)]
        map_only: bool,
        #[arg(long, default_value_t = false)]
        materials: bool,
    },
    /// Validate a stage document.
    Validate {
        #[arg(long)]
        base_root: PathBuf,
        #[arg(long)]
        stage: String,
        #[arg(long, default_value = "..")]
        repo_root: PathBuf,
    },
    /// Save the editable stage overlay and sms-project.toml to an editor project folder.
    #[command(alias = "export-mod")]
    ExportProject {
        #[arg(long)]
        base_root: PathBuf,
        #[arg(long)]
        stage: String,
        #[arg(long)]
        project_root: PathBuf,
    },
    /// Launch Dolphin with an isolated user directory when provided.
    LaunchDolphin {
        #[arg(long)]
        dolphin: PathBuf,
        #[arg(long)]
        game: PathBuf,
        #[arg(long)]
        user_dir: Option<PathBuf>,
        #[arg(long, default_value_t = true)]
        batch: bool,
    },
}

#[derive(Debug, Default)]
struct TextureTriangleStats {
    count: usize,
    area_sum: f32,
    normal_y_abs_sum: f32,
    normal_y_sum: f32,
    min_y: f32,
    max_y: f32,
    bounds_min: [f32; 3],
    bounds_max: [f32; 3],
    vertex_alpha_sum: usize,
    vertex_alpha_count: usize,
    vertex_luminance_sum: usize,
    vertex_luminance_count: usize,
}

impl TextureTriangleStats {
    fn add_triangle(&mut self, triangle: &J3dTriangle) {
        let normal = triangle_normal(triangle.vertices);
        self.count += 1;
        self.area_sum += triangle_area(triangle.vertices);
        self.normal_y_sum += normal[1];
        self.normal_y_abs_sum += normal[1].abs();
        let tri_min_y = triangle
            .vertices
            .iter()
            .map(|vertex| vertex[1])
            .fold(f32::INFINITY, f32::min);
        let tri_max_y = triangle
            .vertices
            .iter()
            .map(|vertex| vertex[1])
            .fold(f32::NEG_INFINITY, f32::max);
        if self.count == 1 {
            self.min_y = tri_min_y;
            self.max_y = tri_max_y;
            self.bounds_min = [f32::INFINITY; 3];
            self.bounds_max = [f32::NEG_INFINITY; 3];
        } else {
            self.min_y = self.min_y.min(tri_min_y);
            self.max_y = self.max_y.max(tri_max_y);
        }
        for vertex in triangle.vertices {
            for (axis, value) in vertex.into_iter().enumerate() {
                self.bounds_min[axis] = self.bounds_min[axis].min(value);
                self.bounds_max[axis] = self.bounds_max[axis].max(value);
            }
        }
        if let Some(colors) = triangle.vertex_colors {
            for color in colors {
                self.vertex_alpha_sum += color[3] as usize;
                self.vertex_alpha_count += 1;
                self.vertex_luminance_sum +=
                    (color[0] as usize + color[1] as usize + color[2] as usize) / 3;
                self.vertex_luminance_count += 1;
            }
        }
    }

    fn to_json(&self, texture_index: usize) -> serde_json::Value {
        let mut value = self.to_json_base();
        value["texture_index"] = serde_json::json!(texture_index);
        value
    }

    fn to_json_base(&self) -> serde_json::Value {
        let count = self.count.max(1) as f32;
        serde_json::json!({
            "triangles": self.count,
            "average_area": self.area_sum / count,
            "average_normal_y": self.normal_y_sum / count,
            "average_abs_normal_y": self.normal_y_abs_sum / count,
            "min_y": self.min_y,
            "max_y": self.max_y,
            "bounds_min": self.bounds_min,
            "bounds_max": self.bounds_max,
            "average_vertex_alpha": average_usize(self.vertex_alpha_sum, self.vertex_alpha_count),
            "average_vertex_luminance": average_usize(self.vertex_luminance_sum, self.vertex_luminance_count),
        })
    }
}

fn main() -> Result<()> {
    let args = Args::parse();

    match args.command {
        Commands::Extract {
            image,
            out,
            nodtool,
        } => extract_with_nodtool(nodtool, image, out),
        Commands::Schema { repo_root } => {
            let registry = SchemaGenerator::new(repo_root).generate()?;
            println!("{}", serde_json::to_string_pretty(&registry)?);
            Ok(())
        }
        Commands::Scenes { base_root } => {
            let scenes = discover_scene_archives(base_root)?;
            println!("{}", serde_json::to_string_pretty(&scenes)?);
            Ok(())
        }
        Commands::Assets {
            base_root,
            stage,
            kind,
        } => {
            let document = StageDocument::open(base_root, stage)?;
            let kind = kind.map(|kind| kind.to_ascii_lowercase());
            let assets: Vec<_> = document
                .assets
                .iter()
                .filter(|asset| {
                    kind.as_ref()
                        .map(|kind| format!("{:?}", asset.kind).to_ascii_lowercase() == *kind)
                        .unwrap_or(true)
                })
                .map(|asset| -> Result<_> {
                    let bytes = read_stage_asset_bytes(&asset.path).with_context(|| {
                        format!("failed to read stage asset {}", asset.path.display())
                    })?;
                    let header: Vec<String> = bytes
                        .iter()
                        .take(16)
                        .map(|byte| format!("{byte:02X}"))
                        .collect();
                    Ok(serde_json::json!({
                        "kind": format!("{:?}", asset.kind),
                        "path": asset.path,
                        "size": bytes.len(),
                        "header": header,
                    }))
                })
                .collect::<Result<_>>()?;
            println!("{}", serde_json::to_string_pretty(&assets)?);
            Ok(())
        }
        Commands::ExtractAsset { asset, out } => {
            let bytes = read_stage_asset_bytes(asset)?;
            if let Some(parent) = out.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(&out, bytes)?;
            Ok(())
        }
        Commands::OpenStage {
            base_root,
            stage,
            repo_root,
        } => {
            let registry = SchemaGenerator::new(repo_root).generate()?;
            let document = StageDocument::open(base_root, stage)?.with_registry(registry);
            let preview = model_preview_summary(&document)?;
            println!(
                "{}",
                serde_json::json!({
                    "stage": document.stage_id,
                    "asset_count": document.assets.len(),
                    "model_count": count_assets(&document, StageAssetKind::Model),
                    "collision_count": count_assets(&document, StageAssetKind::Collision),
                    "archive_count": count_assets(&document, StageAssetKind::Archive),
                    "preview_model_count": preview.0,
                    "preview_vertex_count": preview.1,
                    "preview_triangle_count": preview.2,
                    "preview_texture_count": preview.3,
                    "preview_textured_triangle_count": preview.4,
                    "object_count": document.objects.len(),
                    "issues": document.validate(),
                })
            );
            Ok(())
        }
        Commands::Objects {
            base_root,
            stage,
            limit,
        } => {
            let document = StageDocument::open(base_root, stage)?;
            let objects: Vec<_> = document
                .objects
                .iter()
                .take(limit)
                .map(|object| {
                    serde_json::json!({
                        "id": object.id,
                        "factory_name": object.factory_name,
                        "class_name": object.class_name,
                        "transform": object.transform,
                        "raw_params": object.raw_params,
                        "asset_hints": object.asset_hints,
                    })
                })
                .collect();
            println!("{}", serde_json::to_string_pretty(&objects)?);
            Ok(())
        }
        Commands::PreviewStats {
            base_root,
            stage,
            filter,
            map_only,
            materials,
        } => {
            let document = StageDocument::open(base_root, stage)?;
            let stats = preview_stats(&document, filter.as_deref(), map_only, materials)?;
            println!("{}", serde_json::to_string_pretty(&stats)?);
            Ok(())
        }
        Commands::Validate {
            base_root,
            stage,
            repo_root,
        } => {
            let registry = SchemaGenerator::new(repo_root).generate()?;
            let document = StageDocument::open(base_root, stage)?.with_registry(registry);
            let issues = document.validate();
            println!("{}", serde_json::to_string_pretty(&issues)?);
            if issues
                .iter()
                .any(|issue| matches!(issue.severity, sms_scene::ValidationSeverity::Error))
            {
                bail!("validation failed");
            }
            Ok(())
        }
        Commands::ExportProject {
            base_root,
            stage,
            project_root,
        } => {
            let mut document = StageDocument::open(base_root, stage)?;
            document.load_project_folder(&project_root)?;
            let outcome = document.save_project_folder(project_root)?;
            for warning in &outcome.warnings {
                eprintln!(
                    "save warning (recovery path {}): {}",
                    warning.recovery_path.display(),
                    warning.message
                );
            }
            println!("{}", serde_json::to_string_pretty(&outcome.manifest)?);
            Ok(())
        }
        Commands::LaunchDolphin {
            dolphin,
            game,
            user_dir,
            batch,
        } => launch_dolphin(dolphin, game, user_dir, batch),
    }
}

fn count_assets(document: &StageDocument, kind: StageAssetKind) -> usize {
    document
        .assets
        .iter()
        .filter(|asset| asset.kind == kind)
        .count()
}

fn model_preview_summary(document: &StageDocument) -> Result<(usize, usize, usize, usize, usize)> {
    let mut model_count = 0;
    let mut vertex_count = 0;
    let mut triangle_count = 0;
    let mut texture_count = 0;
    let mut textured_triangle_count = 0;
    for asset in &document.assets {
        if asset.kind != StageAssetKind::Model {
            continue;
        }

        let bytes = read_stage_asset_bytes(&asset.path)
            .with_context(|| format!("failed to read model asset {}", asset.path.display()))?;
        let file = J3dFile::parse(&bytes)
            .with_context(|| format!("failed to parse model asset {}", asset.path.display()))?;
        match file.geometry_preview() {
            Ok(preview) => {
                model_count += 1;
                vertex_count += preview.positions.len();
                triangle_count += preview.triangles.len();
                texture_count += preview.textures.len();
                textured_triangle_count += preview
                    .triangles
                    .iter()
                    .filter(|triangle| {
                        triangle.texture_index.is_some() && triangle.tex_coords.is_some()
                    })
                    .count();
            }
            Err(geometry_error) => {
                let preview = file.vertex_preview().with_context(|| {
                    format!(
                        "failed to build preview for {} (geometry preview error: {geometry_error})",
                        asset.path.display()
                    )
                })?;
                model_count += 1;
                vertex_count += preview.positions.len();
            }
        }
    }

    Ok((
        model_count,
        vertex_count,
        triangle_count,
        texture_count,
        textured_triangle_count,
    ))
}

fn preview_stats(
    document: &StageDocument,
    filter: Option<&str>,
    map_only: bool,
    include_materials: bool,
) -> Result<serde_json::Value> {
    let filter = filter.map(|filter| filter.to_ascii_lowercase());
    let mut models = Vec::new();
    for asset in &document.assets {
        if asset.kind != StageAssetKind::Model {
            continue;
        }
        let path = asset.path.to_string_lossy().replace('\\', "/");
        if let Some(filter) = &filter {
            if !path.to_ascii_lowercase().contains(filter) {
                continue;
            }
        }
        if map_only && !(path.contains("!/map/") || path.contains("/scene/map/")) {
            continue;
        }

        let bytes = read_stage_asset_bytes(&asset.path)
            .with_context(|| format!("failed to read model asset {}", asset.path.display()))?;
        let file = J3dFile::parse(&bytes)
            .with_context(|| format!("failed to parse model asset {}", asset.path.display()))?;
        let preview = file.geometry_preview().with_context(|| {
            format!(
                "failed to build geometry preview for {}",
                asset.path.display()
            )
        })?;

        let mut uv_min = [f32::INFINITY; 2];
        let mut uv_max = [f32::NEG_INFINITY; 2];
        let mut textured_triangles = 0usize;
        let mut masked_triangles = 0usize;
        let mut textureless_uv_triangles = 0usize;
        let mut invalid_uv_triangles = 0usize;
        let mut used_textures = std::collections::BTreeSet::new();
        let mut used_mask_textures = std::collections::BTreeSet::new();
        let mut combine_modes = std::collections::BTreeMap::<String, usize>::new();
        let mut material_colors = std::collections::BTreeMap::<String, usize>::new();
        let mut used_texture_formats = std::collections::BTreeMap::<u8, usize>::new();
        let mut billboard_modes = std::collections::BTreeMap::<String, usize>::new();
        let mut texture_triangle_stats =
            std::collections::BTreeMap::<usize, TextureTriangleStats>::new();
        let mut triangle_group_stats =
            std::collections::BTreeMap::<String, TextureTriangleStats>::new();
        for triangle in &preview.triangles {
            if let Some(billboard) = triangle.billboard {
                *billboard_modes
                    .entry(format!("{:?}", billboard.mode))
                    .or_default() += 1;
            }
            *combine_modes
                .entry(format!("{:?}", triangle.combine_mode))
                .or_default() += 1;
            let color_key = triangle
                .color
                .map(|color| {
                    format!(
                        "#{:02X}{:02X}{:02X}{:02X}",
                        color[0], color[1], color[2], color[3]
                    )
                })
                .unwrap_or_else(|| "none".to_string());
            let group_key = format!(
                "{:?}|{}|tex={}",
                triangle.combine_mode,
                color_key,
                triangle
                    .texture_index
                    .map(|index| index.to_string())
                    .unwrap_or_else(|| "none".to_string())
            );
            triangle_group_stats
                .entry(group_key)
                .or_default()
                .add_triangle(triangle);
            if let Some(color) = triangle.color {
                *material_colors
                    .entry(format!(
                        "#{:02X}{:02X}{:02X}{:02X}",
                        color[0], color[1], color[2], color[3]
                    ))
                    .or_default() += 1;
            }
            if let Some(index) = triangle.texture_index {
                used_textures.insert(index);
                if let Some(texture) = preview.textures.get(index) {
                    *used_texture_formats.entry(texture.format).or_default() += 1;
                }
                texture_triangle_stats
                    .entry(index)
                    .or_default()
                    .add_triangle(triangle);
            }
            if let Some(index) = triangle.mask_texture_index {
                masked_triangles += 1;
                used_mask_textures.insert(index);
            }
            if let Some(tex_coords) = triangle.tex_coords {
                if triangle.texture_index.is_some() {
                    textured_triangles += 1;
                } else {
                    textureless_uv_triangles += 1;
                }
                for coord in tex_coords {
                    if coord[0].is_finite() && coord[1].is_finite() {
                        uv_min[0] = uv_min[0].min(coord[0]);
                        uv_min[1] = uv_min[1].min(coord[1]);
                        uv_max[0] = uv_max[0].max(coord[0]);
                        uv_max[1] = uv_max[1].max(coord[1]);
                    } else {
                        invalid_uv_triangles += 1;
                    }
                }
            }
        }
        let uv_min = if uv_min[0].is_finite() {
            serde_json::json!(uv_min)
        } else {
            serde_json::Value::Null
        };
        let uv_max = if uv_max[0].is_finite() {
            serde_json::json!(uv_max)
        } else {
            serde_json::Value::Null
        };

        let mut texture_formats = std::collections::BTreeMap::<u8, usize>::new();
        let mut texture_stats = Vec::new();
        for (texture_index, texture) in preview.textures.iter().enumerate() {
            *texture_formats.entry(texture.format).or_default() += 1;
            let mut transparent_pixels = 0usize;
            let mut partial_alpha_pixels = 0usize;
            let mut alpha_sum = 0usize;
            let mut luminance_sum = 0usize;
            for pixel in texture.rgba.chunks_exact(4) {
                let alpha = pixel[3] as usize;
                alpha_sum += alpha;
                luminance_sum += (pixel[0] as usize + pixel[1] as usize + pixel[2] as usize) / 3;
                if alpha < 8 {
                    transparent_pixels += 1;
                } else if alpha < 245 {
                    partial_alpha_pixels += 1;
                }
            }
            let pixel_count = (texture.rgba.len() / 4).max(1);
            texture_stats.push(serde_json::json!({
                "index": texture_index,
                "name": texture.name,
                "width": texture.width,
                "height": texture.height,
                "format": texture.format,
                "wrap_s": texture.wrap_s,
                "wrap_t": texture.wrap_t,
                "min_filter": texture.min_filter,
                "mag_filter": texture.mag_filter,
                "mipmap_enabled": texture.mipmap_enabled,
                "do_edge_lod": texture.do_edge_lod,
                "bias_clamp": texture.bias_clamp,
                "max_anisotropy": texture.max_anisotropy,
                "min_lod": texture.min_lod,
                "max_lod": texture.max_lod,
                "lod_bias": texture.lod_bias,
                "mipmap_count": texture.mipmap_count,
                "decoded_mips": texture.mips.len(),
                "transparent_pixels": transparent_pixels,
                "partial_alpha_pixels": partial_alpha_pixels,
                "average_alpha": alpha_sum as f32 / pixel_count as f32,
                "average_luminance": luminance_sum as f32 / pixel_count as f32,
            }));
        }

        let mut model = serde_json::json!({
            "path": path,
            "positions": preview.positions.len(),
            "triangles": preview.triangles.len(),
            "textured_triangles": textured_triangles,
            "masked_triangles": masked_triangles,
            "textureless_uv_triangles": textureless_uv_triangles,
            "invalid_uv_triangles": invalid_uv_triangles,
            "textures": preview.textures.len(),
            "used_texture_slots": used_textures.into_iter().collect::<Vec<_>>(),
            "used_mask_texture_slots": used_mask_textures.into_iter().collect::<Vec<_>>(),
            "texture_formats": texture_formats,
            "texture_stats": texture_stats,
            "texture_triangle_stats": texture_triangle_stats
                .into_iter()
                .map(|(index, stats)| stats.to_json(index))
                .collect::<Vec<_>>(),
            "triangle_group_stats": triangle_group_stats
                .into_iter()
                .map(|(group, stats)| {
                    let mut value = stats.to_json_base();
                    value["group"] = serde_json::json!(group);
                    value
                })
                .collect::<Vec<_>>(),
            "used_texture_formats": used_texture_formats,
            "combine_modes": combine_modes,
            "material_colors": material_colors,
            "billboard_modes": billboard_modes,
            "uv_min": uv_min,
            "uv_max": uv_max,
            "bounds_min": preview.bounds_min,
            "bounds_max": preview.bounds_max,
        });
        if include_materials {
            model["materials"] =
                serde_json::to_value(file.material_diagnostics().with_context(|| {
                    format!(
                        "failed to inspect materials for model asset {}",
                        asset.path.display()
                    )
                })?)
                .with_context(|| {
                    format!(
                        "failed to serialize material diagnostics for {}",
                        asset.path.display()
                    )
                })?;
        }
        models.push(model);
    }

    Ok(serde_json::json!({
        "stage": document.stage_id,
        "filter": filter,
        "map_only": map_only,
        "model_count": models.len(),
        "models": models,
    }))
}

fn average_usize(sum: usize, count: usize) -> Option<f32> {
    (count > 0).then_some(sum as f32 / count as f32)
}

fn triangle_area(vertices: [[f32; 3]; 3]) -> f32 {
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
    let cross = [
        ab[1] * ac[2] - ab[2] * ac[1],
        ab[2] * ac[0] - ab[0] * ac[2],
        ab[0] * ac[1] - ab[1] * ac[0],
    ];
    ((cross[0] * cross[0] + cross[1] * cross[1] + cross[2] * cross[2]).sqrt()) * 0.5
}

fn triangle_normal(vertices: [[f32; 3]; 3]) -> [f32; 3] {
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

fn extract_with_nodtool(nodtool: PathBuf, image: PathBuf, out: PathBuf) -> Result<()> {
    let status = Command::new(&nodtool)
        .arg("extract")
        .arg(&image)
        .arg(&out)
        .status()
        .with_context(|| format!("failed to run {}", nodtool.display()))?;

    if !status.success() {
        bail!("nodtool extract failed with status {status}");
    }

    Ok(())
}

fn launch_dolphin(
    dolphin: PathBuf,
    game: PathBuf,
    user_dir: Option<PathBuf>,
    batch: bool,
) -> Result<()> {
    let mut command = Command::new(&dolphin);
    if let Some(user_dir) = user_dir {
        command.arg("-u").arg(user_dir);
    }
    if batch {
        command.arg("-b");
    }
    command.arg("-e").arg(game);

    let status = command
        .status()
        .with_context(|| format!("failed to run {}", dolphin.display()))?;
    if !status.success() {
        bail!("Dolphin exited with status {status}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn export_project_command_uses_explicit_project_root() {
        let args = Args::try_parse_from([
            "sms-cli",
            "export-project",
            "--base-root",
            "base",
            "--stage",
            "dolpic0",
            "--project-root",
            "project",
        ])
        .unwrap();

        assert!(matches!(
            args.command,
            Commands::ExportProject {
                base_root,
                stage,
                project_root,
            } if base_root == std::path::Path::new("base")
                && stage == "dolpic0"
                && project_root == std::path::Path::new("project")
        ));
    }
}
