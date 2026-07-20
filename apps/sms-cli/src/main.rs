use std::collections::BTreeMap;
use std::io::Write;
use std::path::PathBuf;
use std::process::Command;

use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand, ValueEnum};
use sms_authoring::{
    built_in_blank_stage_proxy, import_model, merge_model_instances, CollisionImportOptions,
    CollisionNodeSelection, CollisionSource, ModelAssetCatalog, ModelAssetDocument,
    ModelImportOptions, ModelInstanceExportMode, ModelInstancePlacement, ResolvedModelInstance,
    TargetLoaderProfile,
};
use sms_formats::{
    discover_scene_archives, read_stage_asset_bytes, validate_materials_for_loader,
    GxDiagnosticSeverity, J3dFile, J3dTriangle, JDramaDocument, JDramaField, JDramaFieldValue,
    JDramaLightMap, JDramaRecord, JDramaRecordPayload, JDramaTransform, StageAssetKind,
};
use sms_scene::{
    BlankStageBootstrapManifest, BlankStageBootstrapResource, BlankStagePreset,
    SourceFreeStageArchive, StageDocument, StageResourceDocument,
    BLANK_STAGE_BOOTSTRAP_REQUIREMENTS,
};
use sms_schema::SchemaGenerator;

#[derive(Debug, Parser)]
#[command(name = "sms-cli")]
#[command(about = "Super Mario Sunshine editor automation CLI")]
struct Args {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum ModelCollisionMode {
    Render,
    Embedded,
    Separate,
    None,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum LoaderProfileArg {
    Full,
    SunshineMap,
    SunshineObject,
    SunshinePollution,
    Custom,
}

impl LoaderProfileArg {
    fn resolve(self, custom_flags: Option<u32>) -> Result<TargetLoaderProfile> {
        match (self, custom_flags) {
            (Self::Full, None) => Ok(TargetLoaderProfile::Full),
            (Self::SunshineMap, None) => Ok(TargetLoaderProfile::SunshineMap),
            (Self::SunshineObject, None) => Ok(TargetLoaderProfile::SunshineObject),
            (Self::SunshinePollution, None) => Ok(TargetLoaderProfile::SunshinePollution),
            (Self::Custom, Some(flags)) => Ok(TargetLoaderProfile::Custom(flags)),
            (Self::Custom, None) => bail!("--loader-flags is required for the custom profile"),
            (_, Some(_)) => bail!("--loader-flags is only valid with --loader-profile custom"),
        }
    }
}

#[derive(Debug, Subcommand)]
enum Commands {
    /// Import a project-authored glTF/GLB into a source-free native model asset.
    ImportModel {
        #[arg(long)]
        input: PathBuf,
        /// New `.smsmodel` output. Existing files are never replaced.
        #[arg(long)]
        asset_out: PathBuf,
        /// Optionally compile a standalone canonical BMD3 at the same time.
        #[arg(long)]
        bmd_out: Option<PathBuf>,
        /// Optionally compile Sunshine COL at the same time.
        #[arg(long)]
        col_out: Option<PathBuf>,
        #[arg(long, value_enum, default_value_t = ModelCollisionMode::Render)]
        collision: ModelCollisionMode,
        /// Separate collision glTF/GLB, required with `--collision separate`.
        #[arg(long)]
        collision_file: Option<PathBuf>,
        #[arg(long, default_value = "COL_")]
        collision_prefix: String,
        #[arg(long, default_value_t = 100.0)]
        units_per_meter: f32,
        /// Confirm intentionally unmapped PBR inputs before compiled output is emitted.
        #[arg(long, default_value_t = false)]
        acknowledge_warnings: bool,
    },
    /// Compile a source-free native model asset to standalone BMD3 and optional COL.
    CompileModelAsset {
        #[arg(long)]
        asset: PathBuf,
        #[arg(long)]
        bmd_out: PathBuf,
        #[arg(long)]
        col_out: Option<PathBuf>,
        #[arg(long, value_enum, default_value_t = LoaderProfileArg::SunshineMap)]
        loader_profile: LoaderProfileArg,
        /// Exact loader flags for `--loader-profile custom` (decimal or 0x-prefixed).
        #[arg(long, value_parser = parse_u32_auto)]
        loader_flags: Option<u32>,
        #[arg(long, default_value_t = false)]
        acknowledge_warnings: bool,
    },
    /// Validate and optionally emit files for a decomp-verified stock resource slot.
    ValidateStockReplacement {
        /// Neighboring Sunshine decompilation root used to derive the stock table.
        #[arg(long, default_value = "..")]
        repo_root: PathBuf,
        #[arg(long)]
        asset: PathBuf,
        /// Exact, case-sensitive `TMapObjData` resource identity.
        #[arg(long)]
        resource: String,
        /// Optional new external BMD output. Existing files are never replaced.
        #[arg(long)]
        bmd_out: Option<PathBuf>,
        /// Optional new external COL output. Existing files are never replaced.
        #[arg(long)]
        col_out: Option<PathBuf>,
        /// Acknowledge that stock resources can be shared globally and that loader warnings apply.
        #[arg(long, default_value_t = false)]
        acknowledge_warnings: bool,
    },
    /// Build an empty source-free stage archive for a new authored runtime id.
    CreateBlankStage {
        /// Extracted base root, used only to enforce the no-write boundary.
        #[arg(long)]
        base_root: PathBuf,
        /// New stage archive id. The managed release must author its stageArc.bin mapping.
        #[arg(long = "stage-id", alias = "target-slot")]
        stage_id: String,
        /// New external `.szs` output. Existing files are never replaced.
        #[arg(long)]
        out: PathBuf,
    },
    /// Upgrade one project-owned authored stage to the current runtime shell.
    UpgradeAuthoredProjectStage {
        #[arg(long)]
        base_root: PathBuf,
        #[arg(long)]
        project_root: PathBuf,
        #[arg(long)]
        stage: String,
    },
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
    /// Rebuild every resource in a scene archive from semantic documents.
    RebuildStage {
        #[arg(long)]
        base_root: PathBuf,
        #[arg(long)]
        stage: String,
        /// Existing output directory plus archive filename; never the base tree.
        #[arg(long)]
        out: PathBuf,
    },
    /// Apply a saved editor object overlay and create a rebuilt external stage archive.
    #[command(alias = "export-project-stage")]
    ExportStage {
        #[arg(long)]
        base_root: PathBuf,
        #[arg(long)]
        stage: String,
        /// Optional Graffito-Editor project whose object overlay should be applied.
        #[arg(long)]
        project_root: Option<PathBuf>,
        /// Project Content directory containing `.sms-model-instances.json` and `.smsmodel` assets.
        #[arg(long)]
        model_content_root: Option<PathBuf>,
        /// Existing output directory plus a new archive filename; never the base tree.
        #[arg(long)]
        out: PathBuf,
    },
    /// Import an archive into a standalone typed JSON document with no source payload cache.
    ImportStageDocument {
        /// Extracted base root, used only to enforce the no-write safety boundary.
        #[arg(long)]
        base_root: PathBuf,
        /// Retail or rebuilt RARC/Yaz0 stage archive to import.
        #[arg(long)]
        archive: PathBuf,
        /// Existing output directory plus a new semantic JSON filename.
        #[arg(long)]
        out: PathBuf,
    },
    /// Rebuild an archive from a standalone typed JSON document.
    ExportStageDocument {
        /// Extracted base root, used only to enforce the no-write safety boundary.
        #[arg(long)]
        base_root: PathBuf,
        #[arg(long)]
        document: PathBuf,
        /// Existing output directory plus a new archive filename.
        #[arg(long)]
        out: PathBuf,
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
        Commands::ImportModel {
            input,
            asset_out,
            bmd_out,
            col_out,
            collision,
            collision_file,
            collision_prefix,
            units_per_meter,
            acknowledge_warnings,
        } => import_model_command(
            input,
            asset_out,
            bmd_out,
            col_out,
            collision,
            collision_file,
            collision_prefix,
            units_per_meter,
            acknowledge_warnings,
        ),
        Commands::CompileModelAsset {
            asset,
            bmd_out,
            col_out,
            loader_profile,
            loader_flags,
            acknowledge_warnings,
        } => compile_model_asset_command(
            asset,
            bmd_out,
            col_out,
            loader_profile.resolve(loader_flags)?,
            acknowledge_warnings,
        ),
        Commands::ValidateStockReplacement {
            repo_root,
            asset,
            resource,
            bmd_out,
            col_out,
            acknowledge_warnings,
        } => validate_stock_replacement_command(
            repo_root,
            asset,
            resource,
            bmd_out,
            col_out,
            acknowledge_warnings,
        ),
        Commands::CreateBlankStage {
            base_root,
            stage_id,
            out,
        } => create_blank_stage_command(base_root, stage_id, out),
        Commands::UpgradeAuthoredProjectStage {
            base_root,
            project_root,
            stage,
        } => {
            let mut document =
                StageDocument::open_authored_project_stage(base_root, &stage, &project_root)
                    .with_context(|| format!("open authored project stage '{stage}'"))?;
            let outcome = document
                .save_project_folder(&project_root)
                .with_context(|| format!("save upgraded authored project stage '{stage}'"))?;
            for warning in &outcome.warnings {
                eprintln!(
                    "save warning (recovery path {}): {}",
                    warning.recovery_path.display(),
                    warning.message
                );
            }
            println!(
                "{}",
                serde_json::json!({
                    "stage": stage,
                    "project_root": project_root,
                    "preset_version": sms_scene::BLANK_STAGE_PRESET_VERSION,
                    "runtime_shell_upgraded": true,
                    "project_revision": outcome.manifest.revision,
                })
            );
            Ok(())
        }
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
            let mut document = StageDocument::open(base_root, &stage)?;
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
        Commands::RebuildStage {
            base_root,
            stage,
            out,
        } => rebuild_stage_archive(base_root, &stage, out),
        Commands::ExportStage {
            base_root,
            stage,
            project_root,
            model_content_root,
            out,
        } => {
            let mut document = StageDocument::open(base_root, &stage)?;
            if let Some(project_root) = project_root {
                document.load_project_folder(&project_root)?;
                if document.loaded_project.is_none() {
                    bail!(
                        "no Graffito-Editor project manifest was found at {}",
                        project_root.display()
                    );
                }
            }
            let (edits, placed_model_instances) = match model_content_root.as_ref() {
                Some(content_root) => project_stage_edits_with_models(
                    content_root,
                    &stage,
                    &document.archive_edits,
                    document.stage_archive.as_ref(),
                )?,
                None => (document.archive_edits.clone(), 0),
            };
            let outcome = document.export_stage_archive_with_edits_new(out, &edits)?;
            println!(
                "{}",
                serde_json::json!({
                    "source": outcome.source_path,
                    "output": outcome.output_path,
                    "size_bytes": outcome.size_bytes,
                    "changed": outcome.changed,
                    "second_rebuild_stable": true,
                    "source_buffers_retained": false,
                    "placed_model_instances": placed_model_instances,
                    "model_content_root": model_content_root,
                })
            );
            Ok(())
        }
        Commands::ImportStageDocument {
            base_root,
            archive,
            out,
        } => {
            let source = std::fs::read(&archive)
                .with_context(|| format!("read stage archive {}", archive.display()))?;
            let document = SourceFreeStageArchive::parse(&source)
                .with_context(|| format!("semantic import of {}", archive.display()))?;
            let rebuilt = document.encode()?;
            if rebuilt != source {
                bail!(
                    "semantic rebuild of {} was not byte-identical ({} source bytes, {} rebuilt bytes)",
                    archive.display(),
                    source.len(),
                    rebuilt.len()
                );
            }
            let semantic_json = document.to_semantic_json()?;
            let output = write_create_new_external_synced(&base_root, &out, &semantic_json)?;
            println!(
                "{}",
                serde_json::json!({
                    "source": archive,
                    "output": output,
                    "semantic_document_bytes": semantic_json.len(),
                    "source_archive_bytes_retained": false,
                    "byte_identical_rebuild_proved": true,
                })
            );
            Ok(())
        }
        Commands::ExportStageDocument {
            base_root,
            document,
            out,
        } => {
            let semantic_json = std::fs::read(&document)
                .with_context(|| format!("read semantic document {}", document.display()))?;
            let archive = SourceFreeStageArchive::from_semantic_json(&semantic_json)
                .with_context(|| format!("load semantic document {}", document.display()))?;
            let rebuilt = archive.encode()?;
            let output = write_create_new_external_synced(&base_root, &out, &rebuilt)?;
            println!(
                "{}",
                serde_json::json!({
                    "source": document,
                    "output": output,
                    "size_bytes": rebuilt.len(),
                    "second_rebuild_stable": true,
                    "source_archive_required": false,
                })
            );
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

fn write_create_new_synced(path: &std::path::Path, bytes: &[u8]) -> Result<PathBuf> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .context("--out must include an existing parent directory")?;
    if !parent.is_dir() {
        bail!("output parent does not exist: {}", parent.display());
    }
    let file_name = path.file_name().context("--out must include a filename")?;
    let output = std::fs::canonicalize(parent)
        .with_context(|| format!("canonicalize output parent {}", parent.display()))?
        .join(file_name);
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&output)
        .with_context(|| format!("create new output {}", output.display()))?;
    file.write_all(bytes)
        .with_context(|| format!("write output {}", output.display()))?;
    file.sync_all()
        .with_context(|| format!("sync output {}", output.display()))?;
    Ok(output)
}

#[allow(clippy::too_many_arguments)]
fn import_model_command(
    input: PathBuf,
    asset_out: PathBuf,
    bmd_out: Option<PathBuf>,
    col_out: Option<PathBuf>,
    collision_mode: ModelCollisionMode,
    collision_file: Option<PathBuf>,
    collision_prefix: String,
    units_per_meter: f32,
    acknowledge_warnings: bool,
) -> Result<()> {
    if !units_per_meter.is_finite() || units_per_meter <= 0.0 {
        bail!("--units-per-meter must be finite and greater than zero");
    }
    if collision_mode != ModelCollisionMode::Separate && collision_file.is_some() {
        bail!("--collision-file is only valid with --collision separate");
    }
    let mut options = ModelImportOptions::default();
    options.coordinate_conversion.units_per_meter = units_per_meter;
    options.collision = match collision_mode {
        ModelCollisionMode::Render => CollisionSource::RenderGeometry {
            surface: Default::default(),
        },
        ModelCollisionMode::Embedded => CollisionSource::EmbeddedNodes {
            prefix: collision_prefix,
            selected_nodes: Default::default(),
            surfaces_by_node: Default::default(),
            default_surface: Default::default(),
        },
        ModelCollisionMode::Separate => {
            let path = collision_file
                .context("--collision-file is required when --collision separate is selected")?;
            CollisionSource::SeparateFile {
                path,
                options: CollisionImportOptions {
                    coordinate_conversion: options.coordinate_conversion,
                    node_selection: CollisionNodeSelection::AllGeometry,
                    ..CollisionImportOptions::default()
                },
            }
        }
        ModelCollisionMode::None => CollisionSource::None,
    };

    let imported = import_model(&input, &options)
        .with_context(|| format!("import project-authored model {}", input.display()))?;
    require_acknowledged_diagnostics(&imported.asset, acknowledge_warnings)?;
    let bounds = imported.asset.converted_bounds()?;
    let native = imported.asset.to_native_bytes()?;
    let bmd = bmd_out
        .as_ref()
        .map(|_| imported.asset.compile_bmd())
        .transpose()?;
    let col = col_out
        .as_ref()
        .map(|_| imported.asset.compile_col())
        .transpose()?;

    let mut outputs = vec![asset_out.clone()];
    outputs.extend(bmd_out.iter().cloned());
    outputs.extend(col_out.iter().cloned());
    preflight_new_outputs(&outputs)?;
    let native_path = write_create_new_synced(&asset_out, &native)?;
    let bmd_path = match (bmd_out.as_ref(), bmd.as_ref()) {
        (Some(path), Some(bytes)) => Some(write_create_new_synced(path, bytes)?),
        _ => None,
    };
    let col_path = match (col_out.as_ref(), col.as_ref()) {
        (Some(path), Some(bytes)) => Some(write_create_new_synced(path, bytes)?),
        _ => None,
    };
    println!(
        "{}",
        serde_json::to_string_pretty(&serde_json::json!({
            "source": input,
            "native_asset": native_path,
            "bmd": bmd_path,
            "col": col_path,
            "node_count": imported.asset.nodes.len(),
            "mesh_count": imported.asset.meshes.len(),
            "material_count": imported.asset.materials.len(),
            "texture_count": imported.asset.textures.len(),
            "collision_group_count": imported.asset.collision.as_ref().map(|collision| collision.groups.len()).unwrap_or(0),
            "converted_bounds": bounds,
            "diagnostics": imported.diagnostics,
            "source_gltf_retained": false,
            "reimport_recipe_retained": false,
        }))?
    );
    Ok(())
}

fn compile_model_asset_command(
    asset_path: PathBuf,
    bmd_out: PathBuf,
    col_out: Option<PathBuf>,
    profile: TargetLoaderProfile,
    acknowledge_warnings: bool,
) -> Result<()> {
    let asset = load_model_asset(&asset_path)?;
    require_acknowledged_diagnostics(&asset, acknowledge_warnings)?;
    let materials = asset
        .materials
        .iter()
        .map(|material| material.gx.clone())
        .collect::<Vec<_>>();
    let loader_diagnostics = validate_materials_for_loader(&materials, profile);
    let bmd = asset.compile_bmd()?;
    let col = col_out.as_ref().map(|_| asset.compile_col()).transpose()?;
    let mut outputs = vec![bmd_out.clone()];
    outputs.extend(col_out.iter().cloned());
    preflight_new_outputs(&outputs)?;
    let bmd_path = write_create_new_synced(&bmd_out, &bmd)?;
    let col_path = match (col_out.as_ref(), col.as_ref()) {
        (Some(path), Some(bytes)) => Some(write_create_new_synced(path, bytes)?),
        _ => None,
    };
    println!(
        "{}",
        serde_json::to_string_pretty(&serde_json::json!({
            "source": asset_path,
            "bmd": bmd_path,
            "col": col_path,
            "loader_profile": profile,
            "loader_flags": profile.flags(),
            "loader_diagnostics": loader_diagnostics,
            "bmd_size_bytes": bmd.len(),
            "col_size_bytes": col.as_ref().map(Vec::len),
            "source_free": true,
        }))?
    );
    Ok(())
}

fn validate_stock_replacement_command(
    repo_root: PathBuf,
    asset_path: PathBuf,
    resource_name: String,
    bmd_out: Option<PathBuf>,
    col_out: Option<PathBuf>,
    acknowledge_warnings: bool,
) -> Result<()> {
    let registry = SchemaGenerator::new(&repo_root)
        .generate()
        .with_context(|| format!("derive stock resource table from {}", repo_root.display()))?;
    let slot = registry
        .find_map_obj_resource(&resource_name)
        .with_context(|| {
            format!(
                "stock resource {resource_name:?} was not found in the decomp-derived table (names are case-sensitive)"
            )
        })?;
    if slot.has_hold_dependency {
        bail!(
            "stock resource {resource_name:?} has compiled TMapObjData::mHold model/joint dependencies and cannot be replaced by a standalone primary BMD/COL"
        );
    }
    if slot.has_move_dependency {
        bail!(
            "stock resource {resource_name:?} has compiled TMapObjData::mMove BCK/joint dependencies and cannot be replaced by a standalone primary BMD/COL"
        );
    }
    let primary_model = slot.primary_model.as_ref().with_context(|| {
        format!("stock resource {resource_name:?} does not instantiate a model")
    })?;

    let asset = load_model_asset(&asset_path)?;
    require_acknowledged_diagnostics(&asset, acknowledge_warnings)?;
    let materials = asset
        .materials
        .iter()
        .map(|material| material.gx.clone())
        .collect::<Vec<_>>();
    let profile = TargetLoaderProfile::Custom(slot.load_flags);
    let loader_diagnostics = validate_materials_for_loader(&materials, profile);
    let loader_has_errors = loader_diagnostics
        .iter()
        .any(|diagnostic| diagnostic.severity == GxDiagnosticSeverity::Error);
    if loader_has_errors {
        bail!("the authored GX state is invalid for stock resource {resource_name:?}");
    }

    let shared_resource_names = registry
        .map_obj_resources
        .iter()
        .filter(|candidate| candidate.primary_model.as_ref() == Some(primary_model))
        .map(|candidate| candidate.resource_name.clone())
        .collect::<Vec<_>>();
    let collision_vertex_count = asset
        .collision
        .as_ref()
        .map(|collision| collision.vertices.len());
    let mut collision_limits = Vec::new();
    for collision in &slot.collision_resources {
        if let (Some(limit), Some(actual)) = (collision.max_vertices, collision_vertex_count) {
            if actual > usize::from(limit) {
                bail!(
                    "stock collision {} permits at most {limit} vertices, but the asset has {actual}",
                    collision.resource_name
                );
            }
        }
        collision_limits.push(serde_json::json!({
            "resource": collision.resource_name,
            "flags": collision.flags,
            "collision_kind": collision.collision_kind,
            "max_vertices": collision.max_vertices,
            "asset_vertices": collision_vertex_count,
        }));
    }
    if !slot.collision_resources.is_empty() && asset.collision.is_none() {
        bail!("stock resource {resource_name:?} requires collision, but the asset has none");
    }
    if col_out.is_some() && slot.collision_resources.is_empty() {
        bail!("stock resource {resource_name:?} has no decomp-verified collision slot");
    }

    let has_warnings = !loader_diagnostics.is_empty() || !shared_resource_names.is_empty();
    let emits_output = bmd_out.is_some() || col_out.is_some();
    if emits_output && has_warnings && !acknowledge_warnings {
        bail!(
            "stock replacement can affect shared/global users and has target diagnostics; inspect the report first, then rerun with --acknowledge-warnings"
        );
    }

    let bmd = bmd_out.as_ref().map(|_| asset.compile_bmd()).transpose()?;
    let col = col_out.as_ref().map(|_| asset.compile_col()).transpose()?;
    let mut outputs = Vec::new();
    outputs.extend(bmd_out.iter().cloned());
    outputs.extend(col_out.iter().cloned());
    preflight_new_outputs(&outputs)?;
    let bmd_path = match (bmd_out.as_ref(), bmd.as_ref()) {
        (Some(path), Some(bytes)) => Some(write_create_new_synced(path, bytes)?),
        _ => None,
    };
    let col_path = match (col_out.as_ref(), col.as_ref()) {
        (Some(path), Some(bytes)) => Some(write_create_new_synced(path, bytes)?),
        _ => None,
    };

    println!(
        "{}",
        serde_json::to_string_pretty(&serde_json::json!({
            "source": asset_path,
            "stock_resource": resource_name,
            "required_scene_manager": slot.required_manager_name,
            "primary_model": primary_model,
            "loader_flags": slot.load_flags,
            "loader_diagnostics": loader_diagnostics,
            "collision_slots": collision_limits,
            "shared_resource_names": shared_resource_names,
            "shared_or_global_replacement_warning": true,
            "bmd": bmd_path,
            "col": col_path,
            "source_free": true,
        }))?
    );
    Ok(())
}

fn create_blank_stage_command(base_root: PathBuf, stage_id: String, out: PathBuf) -> Result<()> {
    let bootstrap_resources = BLANK_STAGE_BOOTSTRAP_REQUIREMENTS
        .map(|requirement| -> Result<BlankStageBootstrapResource> {
            let bytes = match requirement.kind {
                sms_scene::BlankStageBootstrapKind::Model => {
                    built_in_blank_stage_proxy(requirement.raw_path).compile_bmd()?
                }
                sms_scene::BlankStageBootstrapKind::Collision => {
                    built_in_blank_stage_proxy(requirement.raw_path)
                        .collision
                        .as_ref()
                        .expect("the built-in bootstrap proxy always has collision")
                        .to_col_bytes()?
                }
            };
            Ok(BlankStageBootstrapResource {
                raw_path: requirement.raw_path.to_vec(),
                bytes,
            })
        })
        .into_iter()
        .collect::<Result<Vec<_>>>()?;
    let bootstrap = BlankStageBootstrapManifest::from_authored_bytes(bootstrap_resources)?;
    let preset = BlankStagePreset {
        target_slot: stage_id.clone(),
        ..BlankStagePreset::default()
    };
    let metadata = preset.target_metadata()?;
    let archive = preset.build(bootstrap)?;
    let encoded = archive.encode()?;
    let declared_size = yaz0_declared_size(&encoded)
        .context("blank-stage output did not encode as a canonical Yaz0 stream")?;
    validate_blank_stage_rarc_size(declared_size)?;
    let reopened = SourceFreeStageArchive::parse(&encoded)?;
    if reopened.encode()? != encoded {
        bail!("blank-stage semantic reopen was not byte-stable");
    }
    let output = write_create_new_external_synced(&base_root, &out, &encoded)?;
    println!(
        "{}",
        serde_json::to_string_pretty(&serde_json::json!({
            "stage_id": stage_id,
            "world_content": "empty_scene_authoring_baseline",
            "mario_placement": "not_authored",
            "skybox": "not_authored",
            "lighting": "neutral_editable_runtime_baseline",
            "bootstrap_source": "built_in_authored",
            "output": output,
            "target": metadata,
            "size_bytes": encoded.len(),
            "decompressed_size_bytes": declared_size,
            "compression": "yaz0",
            "semantic_reopen_stable": true,
            "stage_table_entry_required": true,
            "retail_assets_copied": false,
            "base_game_modified": false,
        }))?
    );
    Ok(())
}

fn yaz0_declared_size(bytes: &[u8]) -> Option<u32> {
    (bytes.len() >= 16 && bytes.get(..4) == Some(b"Yaz0")).then(|| {
        u32::from_be_bytes(
            bytes[4..8]
                .try_into()
                .expect("the header length was checked"),
        )
    })
}

fn validate_blank_stage_rarc_size(declared_size: u32) -> Result<()> {
    const BLANK_STAGE_RARC_SAFETY_BUDGET: u32 = 12 * 1024 * 1024;
    if declared_size > BLANK_STAGE_RARC_SAFETY_BUDGET {
        bail!(
            "blank-stage RARC expands to {declared_size} bytes, exceeding the editor's {}-byte safety budget for Sunshine's 24 MiB MEM1; reduce the authored bootstrap resources",
            BLANK_STAGE_RARC_SAFETY_BUDGET
        );
    }
    Ok(())
}

fn load_model_asset(path: &std::path::Path) -> Result<ModelAssetDocument> {
    let bytes = std::fs::read(path)
        .with_context(|| format!("read native model asset {}", path.display()))?;
    match ModelAssetDocument::from_native_bytes(&bytes) {
        Ok(document) => Ok(document),
        Err(native_error) => {
            let canonical = std::fs::canonicalize(path)
                .with_context(|| format!("canonicalize model asset {}", path.display()))?;
            for content_root in canonical.ancestors().skip(1) {
                if !content_root.join(".sms-assets").is_dir() {
                    continue;
                }
                let relative = canonical.strip_prefix(content_root).with_context(|| {
                    format!(
                        "resolve catalog asset {} under {}",
                        canonical.display(),
                        content_root.display()
                    )
                })?;
                return ModelAssetCatalog::open_content_root(content_root)
                    .and_then(|catalog| catalog.load_asset_path(relative))
                    .with_context(|| format!("load catalog model asset {}", canonical.display()));
            }
            Err(native_error).with_context(|| {
                format!(
                    "parse native model asset {} (no managed Content catalog was found)",
                    path.display()
                )
            })
        }
    }
}

fn require_acknowledged_diagnostics(asset: &ModelAssetDocument, acknowledged: bool) -> Result<()> {
    let required = asset.unacknowledged_required_diagnostics();
    if !acknowledged && !required.is_empty() {
        let codes = required
            .iter()
            .map(|diagnostic| format!("{:?}", diagnostic.code))
            .collect::<Vec<_>>()
            .join(", ");
        bail!(
            "model has acknowledgement-required diagnostics ({codes}); inspect them and rerun with --acknowledge-warnings"
        );
    }
    Ok(())
}

#[derive(Debug, serde::Deserialize)]
struct ProjectModelInstanceManifest {
    format_version: u32,
    #[serde(default)]
    instances: Vec<ProjectModelInstance>,
}

#[derive(Debug, serde::Deserialize)]
struct ProjectModelInstance {
    #[serde(default)]
    stage_id: String,
    placement: ModelInstancePlacement,
}

fn load_project_model_instances(
    content_root: &std::path::Path,
    stage: &str,
) -> Result<Vec<ProjectModelInstance>> {
    let manifest_path = content_root.join(".sms-model-instances.json");
    let bytes = std::fs::read(&manifest_path)
        .with_context(|| format!("read model-instance manifest {}", manifest_path.display()))?;
    let manifest: ProjectModelInstanceManifest = serde_json::from_slice(&bytes)
        .with_context(|| format!("parse model-instance manifest {}", manifest_path.display()))?;
    if manifest.format_version != 1 {
        bail!(
            "unsupported model-instance manifest version {}; expected 1",
            manifest.format_version
        );
    }
    Ok(manifest
        .instances
        .into_iter()
        .filter(|instance| instance.stage_id.eq_ignore_ascii_case(stage))
        .collect())
}

fn project_stage_edits_with_models(
    content_root: &std::path::Path,
    stage: &str,
    base: &sms_scene::StageArchiveEdits,
    archive: Option<&SourceFreeStageArchive>,
) -> Result<(sms_scene::StageArchiveEdits, usize)> {
    let instances = load_project_model_instances(content_root, stage)?;
    let instance_count = instances.len();
    if instances.is_empty() {
        return Ok((base.clone(), 0));
    }
    let catalog = ModelAssetCatalog::open_content_root(content_root)
        .with_context(|| format!("open model catalog {}", content_root.display()))?;
    let mut assets = BTreeMap::<sms_authoring::AssetId, ModelAssetDocument>::new();
    let mut separate = Vec::new();
    let mut map_terrain = Vec::new();
    let mut skybox = Vec::new();
    for instance in instances {
        let asset = if let Some(asset) = assets.get(&instance.placement.asset_id) {
            asset.clone()
        } else {
            let asset = catalog
                .load_asset(instance.placement.asset_id)
                .with_context(|| {
                    format!(
                        "resolve model instance {} asset {}",
                        instance.placement.instance_id, instance.placement.asset_id
                    )
                })?;
            assets.insert(instance.placement.asset_id, asset.clone());
            asset
        };
        let unacknowledged = asset.unacknowledged_required_diagnostics();
        if !unacknowledged.is_empty() {
            let codes = unacknowledged
                .iter()
                .map(|diagnostic| format!("{:?}", diagnostic.code))
                .collect::<Vec<_>>()
                .join(", ");
            bail!(
                "model instance {} has unacknowledged import diagnostics ({codes})",
                instance.placement.instance_id
            );
        }
        let resolved = ResolvedModelInstance {
            placement: instance.placement,
            asset,
        };
        match resolved.placement.export_mode {
            ModelInstanceExportMode::SeparateRuntimeObject => separate.push(resolved),
            ModelInstanceExportMode::MapTerrain => map_terrain.push(resolved),
            ModelInstanceExportMode::Skybox => skybox.push(resolved),
            ModelInstanceExportMode::StockMapObjBase => {
                let selected = resolved.placement.stock_map_obj_resource.trim();
                let detail = if selected.is_empty() {
                    "no stock resource slot was selected".to_string()
                } else {
                    format!("slot {selected:?} has not been decomp-validated for this asset")
                };
                bail!(
                    "model instance {} cannot export through Stock MapObjBase: {detail}; arbitrary resource keys are unsafe because Sunshine resolves MapObjBase resources through a compiled registry",
                    resolved.placement.instance_id
                );
            }
        }
    }

    let mut edits = base.clone();
    if !separate.is_empty() {
        let archive = archive.context(
            "separate runtime-object export requires the open source-free stage archive so map/scene.bin and map/tables.bin can be edited semantically",
        )?;
        let scene_parent = cli_runtime_actor_parent_path(archive)?;

        let mut separate_assets = BTreeMap::new();
        for resolved in &separate {
            separate_assets
                .entry(resolved.placement.asset_id)
                .or_insert_with(|| resolved.asset.clone());
        }
        let mut characters = Vec::with_capacity(separate_assets.len());
        for (asset_id, asset) in separate_assets {
            let resource_key = cli_runtime_resource_key(asset_id);
            let model = asset
                .compile_bmd_document()
                .with_context(|| format!("compile separate runtime BMD3 for asset {asset_id}"))?;
            edits.upsert_model(
                format!("mapobj/{resource_key}/default.bmd").into_bytes(),
                model,
            );
            characters.push(cli_runtime_obj_chara_record(asset_id, &resource_key)?);
        }

        if archive.resource(b"map/tables.bin").is_some() {
            let tables_parent = archive
                .find_group_record_path(b"map/tables.bin", "NameRefGrp", None)?
                .context("map/tables.bin has no unambiguous NameRefGrp root")?;
            for character in characters {
                edits.insert_placement(
                    b"map/tables.bin".to_vec(),
                    tables_parent.clone(),
                    character,
                );
            }
        } else {
            let root = JDramaRecord::new(
                "NameRefGrp",
                "SMS authored model characters",
                JDramaRecordPayload::Group {
                    fields: Vec::new(),
                    children: characters,
                },
            )?;
            edits.insert_resource(
                b"map/tables.bin".to_vec(),
                StageResourceDocument::Placement(JDramaDocument { root }),
            );
        }

        for resolved in &separate {
            edits.insert_placement(
                b"map/scene.bin".to_vec(),
                scene_parent.clone(),
                cli_runtime_sm_j3d_actor_record(&resolved.placement)?,
            );
        }
    }

    if !map_terrain.is_empty() {
        let merged = merge_model_instances("AuthoredMapTerrain", &map_terrain)?;
        edits.replace_model(b"map/map/map.bmd".to_vec(), merged.compile_bmd_document()?);
    }

    if skybox.len() > 1 {
        bail!(
            "stage export has {} authored skybox instances; Sunshine resolves one map/map/sky.bmd",
            skybox.len()
        );
    }
    if let Some(resolved) = skybox.first() {
        edits.upsert_model(
            b"map/map/sky.bmd".to_vec(),
            resolved.asset.compile_bmd_document()?,
        );
        let archive = archive.context(
            "skybox export requires the open source-free stage archive so the typed Sky actor can be verified",
        )?;
        let has_sky_actor = archive.object_placements().iter().any(|placement| {
            placement
                .type_name
                .rsplit("::")
                .next()
                .is_some_and(|type_name| type_name == "Sky")
        });
        if !has_sky_actor {
            let sky_group = archive
                .find_group_record_path(b"map/scene.bin", "IdxGroup", Some(1))?
                .context("map/scene.bin has no unambiguous IdxGroup 1 sky group")?;
            edits.insert_placement(
                b"map/scene.bin".to_vec(),
                sky_group,
                sms_scene::blank_stage_sky_record(cli_runtime_transform(
                    resolved.placement.transform,
                )?)?,
            );
        }
    }

    let collision_instances = separate
        .iter()
        .chain(map_terrain.iter())
        .cloned()
        .collect::<Vec<_>>();
    if !collision_instances.is_empty() {
        let merged = merge_model_instances("AuthoredInstanceCollision", &collision_instances)?;
        let collision = merged
            .collision
            .as_ref()
            .map(|collision| collision.to_col_file())
            .transpose()?;
        if let Some(collision) = collision {
            edits.append_collision(b"map/map.col".to_vec(), collision);
        }
    }
    Ok((edits, instance_count))
}

fn cli_runtime_resource_key(asset_id: sms_authoring::AssetId) -> String {
    format!("sms_{}", asset_id.as_uuid().simple())
}

const SMS_RUNTIME_MAP_GROUP_TYPE: &str = "IdxGroup";
const SMS_RUNTIME_MAP_GROUP_NAME: &str = "マップグループ";
const SMS_RUNTIME_MAP_GROUP_INDEX: u32 = 0;
const SMS_RUNTIME_MAP_GROUP_OWNER_TYPE: &str = "Strategy";

fn cli_runtime_actor_parent_path(archive: &SourceFreeStageArchive) -> Result<Vec<usize>> {
    archive
        .find_unique_owned_indexed_group_record_path(
            b"map/scene.bin",
            SMS_RUNTIME_MAP_GROUP_TYPE,
            SMS_RUNTIME_MAP_GROUP_NAME,
            SMS_RUNTIME_MAP_GROUP_INDEX,
            SMS_RUNTIME_MAP_GROUP_OWNER_TYPE,
        )
        .context("locate Sunshine's scheduled runtime map group in map/scene.bin")?
        .with_context(|| {
            format!(
                "map/scene.bin has no {SMS_RUNTIME_MAP_GROUP_TYPE} group named {SMS_RUNTIME_MAP_GROUP_NAME:?} in the unique {SMS_RUNTIME_MAP_GROUP_OWNER_TYPE} group_index {SMS_RUNTIME_MAP_GROUP_INDEX} slot; authored SmJ3DAct actors would never be scheduled for calc, entry, and viewCalc"
            )
        })
}

fn cli_runtime_character_name(asset_id: sms_authoring::AssetId) -> String {
    format!("{}_character", cli_runtime_resource_key(asset_id))
}

fn cli_runtime_obj_chara_record(
    asset_id: sms_authoring::AssetId,
    resource_key: &str,
) -> Result<JDramaRecord> {
    Ok(JDramaRecord::new(
        "ObjChara",
        cli_runtime_character_name(asset_id),
        JDramaRecordPayload::Fields {
            fields: vec![JDramaField {
                name: "resource_folder".to_string(),
                value: JDramaFieldValue::String(format!("/scene/mapObj/{resource_key}")),
            }],
        },
    )?)
}

fn cli_runtime_sm_j3d_actor_record(placement: &ModelInstancePlacement) -> Result<JDramaRecord> {
    Ok(JDramaRecord::new(
        "SmJ3DAct",
        format!("sms_instance_{}", placement.instance_id.simple()),
        JDramaRecordPayload::Actor {
            transform: cli_runtime_transform(placement.transform)?,
            character_name: cli_runtime_character_name(placement.asset_id),
            light_map: JDramaLightMap::default(),
            fields: Vec::new(),
        },
    )?)
}

fn cli_runtime_transform(matrix: [[f32; 4]; 4]) -> Result<JDramaTransform> {
    if matrix.iter().flatten().any(|value| !value.is_finite()) {
        bail!("runtime model transform contains a non-finite value");
    }
    let mut scale = [0.0; 3];
    let mut rotation = [[0.0; 3]; 3];
    for column in 0..3 {
        scale[column] =
            (matrix[column][0].powi(2) + matrix[column][1].powi(2) + matrix[column][2].powi(2))
                .sqrt();
        let divisor = scale[column].max(f32::EPSILON);
        for row in 0..3 {
            rotation[column][row] = matrix[column][row] / divisor;
        }
    }
    let y = (-rotation[0][2]).clamp(-1.0, 1.0).asin();
    let cosine_y = y.cos();
    let (x, z) = if cosine_y.abs() > 0.000_01 {
        (
            rotation[1][2].atan2(rotation[2][2]),
            rotation[0][1].atan2(rotation[0][0]),
        )
    } else {
        ((-rotation[2][1]).atan2(rotation[1][1]), 0.0)
    };
    Ok(JDramaTransform {
        translation: [matrix[3][0], matrix[3][1], matrix[3][2]],
        rotation: [x.to_degrees(), y.to_degrees(), z.to_degrees()],
        scale,
    })
}

fn preflight_new_outputs(paths: &[PathBuf]) -> Result<()> {
    let mut resolved = std::collections::BTreeSet::new();
    for path in paths {
        let parent = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .context("every output must include an existing parent directory")?;
        if !parent.is_dir() {
            bail!("output parent does not exist: {}", parent.display());
        }
        let file_name = path.file_name().context("output must include a filename")?;
        let candidate = std::fs::canonicalize(parent)
            .with_context(|| format!("canonicalize output parent {}", parent.display()))?
            .join(file_name);
        if !resolved.insert(candidate.clone()) {
            bail!("multiple outputs resolve to {}", candidate.display());
        }
        if candidate.exists() {
            bail!("output already exists: {}", candidate.display());
        }
    }
    Ok(())
}

fn parse_u32_auto(value: &str) -> std::result::Result<u32, String> {
    let value = value.trim();
    if let Some(hex) = value
        .strip_prefix("0x")
        .or_else(|| value.strip_prefix("0X"))
    {
        u32::from_str_radix(hex, 16).map_err(|error| error.to_string())
    } else {
        value.parse::<u32>().map_err(|error| error.to_string())
    }
}

fn write_create_new_external_synced(
    base_root: &std::path::Path,
    path: &std::path::Path,
    bytes: &[u8],
) -> Result<PathBuf> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .context("--out must include an existing parent directory")?;
    if !parent.is_dir() {
        bail!("output parent does not exist: {}", parent.display());
    }
    let file_name = path.file_name().context("--out must include a filename")?;
    let canonical_base = std::fs::canonicalize(base_root)
        .with_context(|| format!("canonicalize base root {}", base_root.display()))?;
    let canonical_output = std::fs::canonicalize(parent)
        .with_context(|| format!("canonicalize output parent {}", parent.display()))?
        .join(file_name);
    if path_is_same_or_child(&canonical_output, &canonical_base) {
        bail!(
            "refusing to write output inside extracted base root: {}",
            canonical_output.display()
        );
    }
    write_create_new_synced(&canonical_output, bytes)
}

fn rebuild_stage_archive(base_root: PathBuf, stage: &str, out: PathBuf) -> Result<()> {
    let archives = discover_scene_archives(&base_root)?;
    let matches = archives
        .into_iter()
        .filter(|archive| archive.stage_id.eq_ignore_ascii_case(stage))
        .collect::<Vec<_>>();
    let archive = match matches.as_slice() {
        [archive] => archive,
        [] => bail!("no scene archive exactly matches stage '{stage}'"),
        _ => bail!("multiple scene archives exactly match stage '{stage}'"),
    };

    let output_parent = out
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .context("--out must include an existing parent directory")?;
    if !output_parent.is_dir() {
        bail!(
            "output parent must already exist so its location can be verified: {}",
            output_parent.display()
        );
    }
    let output_name = out
        .file_name()
        .context("--out must include an archive filename")?;
    let canonical_base = std::fs::canonicalize(&base_root)
        .with_context(|| format!("canonicalize base root {}", base_root.display()))?;
    let canonical_output = std::fs::canonicalize(output_parent)
        .with_context(|| format!("canonicalize output parent {}", output_parent.display()))?
        .join(output_name);
    if path_is_same_or_child(&canonical_output, &canonical_base) {
        bail!(
            "refusing to write rebuilt archive inside extracted base root: {}",
            canonical_output.display()
        );
    }

    let source = std::fs::read(&archive.path)
        .with_context(|| format!("read source archive {}", archive.path.display()))?;
    let document = SourceFreeStageArchive::parse(&source)
        .with_context(|| format!("semantic import of stage '{stage}'"))?;
    let rebuilt = document
        .encode()
        .with_context(|| format!("semantic export of stage '{stage}'"))?;
    if rebuilt != source {
        bail!(
            "semantic rebuild of stage '{stage}' was not byte-identical ({} source bytes, {} rebuilt bytes)",
            source.len(),
            rebuilt.len()
        );
    }
    let reopened = SourceFreeStageArchive::parse(&rebuilt)
        .with_context(|| format!("verification reimport of stage '{stage}'"))?;
    if reopened.encode()? != rebuilt {
        bail!("second semantic rebuild of stage '{stage}' was not stable");
    }
    let mut output = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&canonical_output)
        .with_context(|| {
            format!(
                "create new rebuilt archive {} (existing outputs are never replaced)",
                canonical_output.display()
            )
        })?;
    output
        .write_all(&rebuilt)
        .with_context(|| format!("write rebuilt archive {}", canonical_output.display()))?;
    output
        .sync_all()
        .with_context(|| format!("sync rebuilt archive {}", canonical_output.display()))?;
    println!(
        "{}",
        serde_json::json!({
            "stage": archive.stage_id,
            "source": archive.path,
            "output": canonical_output,
            "size_bytes": rebuilt.len(),
            "byte_identical": true,
            "source_buffers_retained": false,
        })
    );
    Ok(())
}

fn path_is_same_or_child(path: &std::path::Path, parent: &std::path::Path) -> bool {
    let normalize = |value: &std::path::Path| {
        value
            .to_string_lossy()
            .replace('/', "\\")
            .trim_end_matches('\\')
            .to_ascii_lowercase()
    };
    let path = normalize(path);
    let parent = normalize(parent);
    path == parent
        || path
            .strip_prefix(&parent)
            .is_some_and(|tail| tail.starts_with('\\'))
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
    fn blank_stage_cli_authors_a_new_empty_id_without_retail_mapping_or_content_inputs() {
        let temporary = tempfile::tempdir().unwrap();
        let base_root = temporary.path().join("base");
        std::fs::create_dir_all(&base_root).unwrap();
        let output = temporary.path().join("custom_stage0.szs");

        create_blank_stage_command(base_root, "custom_stage0".to_string(), output.clone()).unwrap();

        let encoded = std::fs::read(output).unwrap();
        let archive = SourceFreeStageArchive::parse(&encoded).unwrap();
        assert_eq!(archive.encode().unwrap(), encoded);
        assert!(archive.resource(b"map/map/map.bmd").is_some());
        assert!(archive.resource(b"map/map.col").is_some());
        assert!(archive
            .object_placements()
            .iter()
            .all(|placement| !matches!(placement.type_name.as_str(), "Mario" | "Sky")));
    }

    #[test]
    fn blank_stage_cli_help_surface_requires_only_base_id_and_output() {
        let parsed = Args::try_parse_from([
            "sms-cli",
            "create-blank-stage",
            "--base-root",
            "base",
            "--stage-id",
            "custom_stage0",
            "--out",
            "custom_stage0.szs",
        ])
        .unwrap();
        let Commands::CreateBlankStage {
            base_root,
            stage_id,
            out,
        } = parsed.command
        else {
            panic!("wrong command parsed")
        };
        assert_eq!(base_root, PathBuf::from("base"));
        assert_eq!(stage_id, "custom_stage0");
        assert_eq!(out, PathBuf::from("custom_stage0.szs"));
    }

    #[test]
    fn built_in_blank_stage_proxies_are_small_deterministic_and_reparseable() {
        let mut model_sizes = Vec::new();
        for requirement in BLANK_STAGE_BOOTSTRAP_REQUIREMENTS {
            let proxy = built_in_blank_stage_proxy(requirement.raw_path);
            let primitive = &proxy.meshes[0].primitives[0];
            for triangle in primitive.indices.chunks_exact(3) {
                let vertices = [
                    primitive.positions[triangle[0] as usize],
                    primitive.positions[triangle[1] as usize],
                    primitive.positions[triangle[2] as usize],
                ];
                let normals = [
                    primitive.normals[triangle[0] as usize],
                    primitive.normals[triangle[1] as usize],
                    primitive.normals[triangle[2] as usize],
                ];
                let face = triangle_normal(vertices);
                let average_normal = [
                    normals.iter().map(|normal| normal[0]).sum::<f32>(),
                    normals.iter().map(|normal| normal[1]).sum::<f32>(),
                    normals.iter().map(|normal| normal[2]).sum::<f32>(),
                ];
                let winding_dot = face
                    .iter()
                    .zip(average_normal)
                    .map(|(face, normal)| face * normal)
                    .sum::<f32>();
                assert!(
                    winding_dot > 0.0,
                    "{} must retain canonical outward authoring and COL winding",
                    String::from_utf8_lossy(requirement.raw_path)
                );
            }
            match requirement.kind {
                sms_scene::BlankStageBootstrapKind::Model => {
                    let first = proxy.compile_bmd().unwrap();
                    let second = proxy.compile_bmd().unwrap();
                    assert_eq!(first, second);
                    assert!(first.len() < 64 * 1024, "{}", first.len());
                    assert_eq!(
                        sms_formats::J3dRebuildDocument::parse(&first)
                            .unwrap()
                            .to_bytes()
                            .unwrap(),
                        first
                    );
                    let preview = J3dFile::parse(first.clone())
                        .unwrap()
                        .geometry_preview()
                        .unwrap();
                    for triangle in preview.triangles {
                        let face = triangle_normal(triangle.vertices);
                        let normals = triangle.normals.expect("proxy BMD retains normals");
                        let average_normal = [
                            normals.iter().map(|normal| normal[0]).sum::<f32>(),
                            normals.iter().map(|normal| normal[1]).sum::<f32>(),
                            normals.iter().map(|normal| normal[2]).sum::<f32>(),
                        ];
                        let winding_dot = face
                            .iter()
                            .zip(average_normal)
                            .map(|(face, normal)| face * normal)
                            .sum::<f32>();
                        assert!(
                            winding_dot < 0.0,
                            "{} must emit Sunshine/GX clockwise runtime winding",
                            String::from_utf8_lossy(requirement.raw_path)
                        );
                    }
                    model_sizes.push(first.len());
                }
                sms_scene::BlankStageBootstrapKind::Collision => {
                    let bytes = proxy.collision.as_ref().unwrap().to_col_bytes().unwrap();
                    assert_eq!(
                        sms_formats::ColFile::parse(&bytes)
                            .unwrap()
                            .to_bytes()
                            .unwrap(),
                        bytes
                    );
                }
            }
        }
        assert_eq!(model_sizes.len(), 4);
        assert!(model_sizes.into_iter().sum::<usize>() < 256 * 1024);
    }

    #[test]
    fn yaz0_declared_size_reads_only_canonical_headers() {
        let mut bytes = [0_u8; 16];
        bytes[..4].copy_from_slice(b"Yaz0");
        bytes[4..8].copy_from_slice(&0x0056_b300_u32.to_be_bytes());
        assert_eq!(yaz0_declared_size(&bytes), Some(0x0056_b300));
        bytes[0] = b'X';
        assert_eq!(yaz0_declared_size(&bytes), None);
        assert_eq!(yaz0_declared_size(&bytes[..15]), None);
    }

    #[test]
    fn blank_stage_rarc_budget_rejects_archives_that_cannot_fit_mem1() {
        validate_blank_stage_rarc_size(12 * 1024 * 1024).unwrap();
        let error = validate_blank_stage_rarc_size(28_467_200).unwrap_err();
        assert!(error.to_string().contains("24 MiB MEM1"), "{error}");
        assert!(error.to_string().contains("12"), "{error}");
    }

    #[test]
    fn model_authoring_commands_parse_explicit_outputs_and_profiles() {
        let import = Args::try_parse_from([
            "sms-cli",
            "import-model",
            "--input",
            "fixture/model.gltf",
            "--asset-out",
            "Content/model.smsmodel",
            "--bmd-out",
            "out/model.bmd",
            "--collision",
            "embedded",
        ])
        .unwrap();
        assert!(matches!(
            import.command,
            Commands::ImportModel {
                collision: ModelCollisionMode::Embedded,
                bmd_out: Some(_),
                ..
            }
        ));

        let compile = Args::try_parse_from([
            "sms-cli",
            "compile-model-asset",
            "--asset",
            "Content/model.smsmodel",
            "--bmd-out",
            "out/model.bmd",
            "--loader-profile",
            "custom",
            "--loader-flags",
            "0x10220000",
        ])
        .unwrap();
        assert!(matches!(
            compile.command,
            Commands::CompileModelAsset {
                loader_profile: LoaderProfileArg::Custom,
                loader_flags: Some(0x1022_0000),
                ..
            }
        ));

        let stock = Args::try_parse_from([
            "sms-cli",
            "validate-stock-replacement",
            "--repo-root",
            "../sms",
            "--asset",
            "Content/block.smsmodel",
            "--resource",
            "NormalBlock",
            "--bmd-out",
            "out/NormalBlock.bmd",
        ])
        .unwrap();
        assert!(matches!(
            stock.command,
            Commands::ValidateStockReplacement {
                resource,
                bmd_out: Some(_),
                ..
            } if resource == "NormalBlock"
        ));
    }

    #[test]
    fn fixture_import_and_native_recompile_are_byte_identical() {
        let temporary = tempfile::tempdir().unwrap();
        let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(
            "../../crates/sms-authoring/tests/fixtures/gltf/valid/minimal-external/model.gltf",
        );
        let asset = temporary.path().join("fixture.smsmodel");
        let first_bmd = temporary.path().join("first.bmd");
        let first_col = temporary.path().join("first.col");
        import_model_command(
            fixture,
            asset.clone(),
            Some(first_bmd.clone()),
            Some(first_col.clone()),
            ModelCollisionMode::Render,
            None,
            "COL_".to_string(),
            100.0,
            false,
        )
        .unwrap();

        let second_bmd = temporary.path().join("second.bmd");
        let second_col = temporary.path().join("second.col");
        compile_model_asset_command(
            asset,
            second_bmd.clone(),
            Some(second_col.clone()),
            TargetLoaderProfile::SunshineMap,
            false,
        )
        .unwrap();
        assert_eq!(
            std::fs::read(first_bmd).unwrap(),
            std::fs::read(second_bmd).unwrap()
        );
        assert_eq!(
            std::fs::read(first_col).unwrap(),
            std::fs::read(second_col).unwrap()
        );
    }

    #[test]
    fn project_stage_model_manifest_exports_separate_runtime_asset_by_default() {
        let temporary = tempfile::tempdir().unwrap();
        let content = temporary.path().join("Content");
        let catalog = ModelAssetCatalog::open_content_root(&content).unwrap();
        let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(
            "../../crates/sms-authoring/tests/fixtures/gltf/valid/minimal-external/model.gltf",
        );
        let asset = import_model(fixture, &ModelImportOptions::default())
            .unwrap()
            .asset;
        let entry = catalog.create_asset("world.smsmodel", &asset).unwrap();
        assert_eq!(
            load_model_asset(&content.join(&entry.relative_path)).unwrap(),
            asset
        );
        let placement = ModelInstancePlacement::new(entry.id, "WorldPart");
        std::fs::write(
            content.join(".sms-model-instances.json"),
            serde_json::to_vec_pretty(&serde_json::json!({
                "format_version": 1,
                "instances": [{
                    "stage_id": "test11",
                    "placement": placement,
                    "local_bounds": [[-50.0, -50.0, -50.0], [50.0, 50.0, 50.0]]
                }]
            }))
            .unwrap(),
        )
        .unwrap();
        let archive = cli_runtime_export_test_archive();
        let runtime_parent = cli_runtime_actor_parent_path(&archive).unwrap();
        let (edits, count) = project_stage_edits_with_models(
            &content,
            "test11",
            &sms_scene::StageArchiveEdits::default(),
            Some(&archive),
        )
        .unwrap();
        assert_eq!(count, 1);
        assert_eq!(edits.models.len(), 1);
        assert_eq!(
            edits.models[0].raw_resource_path,
            format!("mapobj/{}/default.bmd", cli_runtime_resource_key(entry.id)).into_bytes()
        );
        assert!(edits
            .models
            .iter()
            .all(|edit| edit.raw_resource_path != b"map/map/map.bmd"));
        assert_eq!(edits.resources[0].raw_resource_path, b"map/tables.bin");
        assert_eq!(edits.placement_inserts.len(), 1);
        assert_eq!(edits.placement_inserts[0].record.type_name, "SmJ3DAct");
        assert_eq!(
            edits.placement_inserts[0].parent_record_path,
            runtime_parent
        );
        assert_eq!(edits.collisions[0].raw_resource_path, b"map/map.col");
    }

    fn cli_runtime_export_test_archive() -> SourceFreeStageArchive {
        cli_runtime_export_test_archive_with_map_groups(1)
    }

    fn cli_runtime_export_test_archive_with_map_groups(
        map_group_count: usize,
    ) -> SourceFreeStageArchive {
        let map_groups = (0..map_group_count)
            .map(|_| {
                JDramaRecord::new(
                    SMS_RUNTIME_MAP_GROUP_TYPE,
                    SMS_RUNTIME_MAP_GROUP_NAME,
                    JDramaRecordPayload::Group {
                        fields: vec![JDramaField {
                            name: "group_index".to_string(),
                            value: JDramaFieldValue::U32(0),
                        }],
                        children: Vec::new(),
                    },
                )
                .unwrap()
            })
            .collect();
        let strategy = JDramaRecord::new(
            "Strategy",
            "strategy",
            JDramaRecordPayload::Group {
                fields: Vec::new(),
                children: map_groups,
            },
        )
        .unwrap();
        let mar_scene = JDramaRecord::new(
            "MarScene",
            "normal scene",
            JDramaRecordPayload::Group {
                fields: vec![JDramaField {
                    name: "light_map".to_string(),
                    value: JDramaFieldValue::LightMap(JDramaLightMap::default()),
                }],
                children: vec![strategy],
            },
        )
        .unwrap();
        let root = JDramaRecord::new(
            "GroupObj",
            "whole scene",
            JDramaRecordPayload::Group {
                fields: Vec::new(),
                children: vec![mar_scene],
            },
        )
        .unwrap();
        let mut archive = SourceFreeStageArchive::new().unwrap();
        archive
            .insert_resource(
                b"map/scene.bin".to_vec(),
                StageResourceDocument::Placement(JDramaDocument { root }),
            )
            .unwrap();
        archive
    }

    #[test]
    fn runtime_actor_parent_requires_one_exact_scheduled_map_group() {
        let missing =
            cli_runtime_actor_parent_path(&cli_runtime_export_test_archive_with_map_groups(0))
                .unwrap_err()
                .to_string();
        assert!(missing.contains("IdxGroup"), "{missing}");
        assert!(missing.contains(SMS_RUNTIME_MAP_GROUP_NAME), "{missing}");
        assert!(missing.contains("never be scheduled"), "{missing}");

        let ambiguous =
            cli_runtime_actor_parent_path(&cli_runtime_export_test_archive_with_map_groups(2))
                .unwrap_err();
        let ambiguous = format!("{ambiguous:#}");
        assert!(ambiguous.contains("ambiguous"), "{ambiguous}");
        assert!(
            ambiguous.contains(SMS_RUNTIME_MAP_GROUP_NAME),
            "{ambiguous}"
        );
    }

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

    #[test]
    fn rebuild_stage_command_requires_explicit_external_output() {
        let args = Args::try_parse_from([
            "sms-cli",
            "rebuild-stage",
            "--base-root",
            "base",
            "--stage",
            "dolpic0",
            "--out",
            "mod/dolpic0.szs",
        ])
        .unwrap();

        assert!(matches!(
            args.command,
            Commands::RebuildStage {
                base_root,
                stage,
                out,
            } if base_root == std::path::Path::new("base")
                && stage == "dolpic0"
                && out == std::path::Path::new("mod/dolpic0.szs")
        ));
    }

    #[test]
    fn authored_project_stage_upgrade_requires_explicit_project_identity() {
        let args = Args::try_parse_from([
            "sms-cli",
            "upgrade-authored-project-stage",
            "--base-root",
            "base",
            "--project-root",
            "project",
            "--stage",
            "test01",
        ])
        .unwrap();

        assert!(matches!(
            args.command,
            Commands::UpgradeAuthoredProjectStage {
                base_root,
                project_root,
                stage,
            } if base_root == std::path::Path::new("base")
                && project_root == std::path::Path::new("project")
                && stage == "test01"
        ));
    }

    #[test]
    fn export_stage_command_accepts_a_project_overlay_and_external_output() {
        let args = Args::try_parse_from([
            "sms-cli",
            "export-stage",
            "--base-root",
            "base",
            "--stage",
            "dolpic0",
            "--project-root",
            "project",
            "--model-content-root",
            "project-data/Content",
            "--out",
            "mod/dolpic0.szs",
        ])
        .unwrap();

        assert!(matches!(
            args.command,
            Commands::ExportStage {
                base_root,
                stage,
                project_root: Some(project_root),
                model_content_root: Some(model_content_root),
                out,
            } if base_root == std::path::Path::new("base")
                && stage == "dolpic0"
                && project_root == std::path::Path::new("project")
                && model_content_root == std::path::Path::new("project-data/Content")
                && out == std::path::Path::new("mod/dolpic0.szs")
        ));
    }

    #[test]
    fn standalone_semantic_stage_document_commands_require_explicit_paths() {
        let import = Args::try_parse_from([
            "sms-cli",
            "import-stage-document",
            "--base-root",
            "base",
            "--archive",
            "base/dolpic0.szs",
            "--out",
            "project/dolpic0.stage.json",
        ])
        .unwrap();
        assert!(matches!(
            import.command,
            Commands::ImportStageDocument { base_root, archive, out }
                if base_root == std::path::Path::new("base")
                    && archive == std::path::Path::new("base/dolpic0.szs")
                    && out == std::path::Path::new("project/dolpic0.stage.json")
        ));

        let export = Args::try_parse_from([
            "sms-cli",
            "export-stage-document",
            "--base-root",
            "base",
            "--document",
            "project/dolpic0.stage.json",
            "--out",
            "mod/dolpic0.szs",
        ])
        .unwrap();
        assert!(matches!(
            export.command,
            Commands::ExportStageDocument { base_root, document, out }
                if base_root == std::path::Path::new("base")
                    && document == std::path::Path::new("project/dolpic0.stage.json")
                    && out == std::path::Path::new("mod/dolpic0.szs")
        ));
    }

    #[test]
    fn rebuilt_stage_output_boundary_is_component_aware() {
        let base = std::path::Path::new(r"C:\game\base");
        assert!(path_is_same_or_child(
            std::path::Path::new(r"C:\game\base\files\scene.szs"),
            base
        ));
        assert!(path_is_same_or_child(base, base));
        assert!(!path_is_same_or_child(
            std::path::Path::new(r"C:\game\base-mod\scene.szs"),
            base
        ));
    }
}
