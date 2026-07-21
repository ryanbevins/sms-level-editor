use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    mpsc::Receiver,
    Arc,
};

use serde::{Deserialize, Serialize};
use sms_authoring::{
    merge_model_instances, AssetId, AssetReference, CatalogAssetEntry,
    CollisionSimplificationOptions, CollisionSource, CollisionSurface, GxMaterial,
    GxTextureEncodeOptions, ModelAssetCatalog, ModelAssetDocument, ModelInstanceExportMode,
    ModelInstancePlacement, ResolvedModelInstance, TargetLoaderProfile,
};
use sms_formats::{
    validate_materials_for_loader, ColFile, GxDiagnosticSeverity, GxMaterialDiagnostic,
    J3dRebuildDocument, JDramaDocument, JDramaField, JDramaFieldValue, JDramaLightMap,
    JDramaRecord, JDramaRecordPayload, JDramaTransform, SMS_DEFAULT_OBJECT_MODEL_LOAD_FLAGS,
    SMS_SM_J3D_ACT_MODEL_LOAD_FLAGS,
};
use sms_scene::{SourceFreeStageArchive, StageResourceDocument, Transform};
use sms_schema::ObjectRegistry;

use super::*;

const MODEL_INSTANCE_MANIFEST_VERSION: u32 = 1;
const MODEL_INSTANCE_MANIFEST_NAME: &str = ".sms-model-instances.json";
const MAX_MODEL_UNDO_RECORDS: usize = 40;
const STANDALONE_ACTOR_BMD_SAFETY_BUDGET: u64 = 12 * 1024 * 1024;
const STANDALONE_ACTOR_TEX1_SAFETY_BUDGET: u64 = 8 * 1024 * 1024;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) enum ContentBrowserKind {
    #[default]
    Stages,
    Objects,
    Models,
    GameSkyboxes,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) enum ModelEditorSection {
    #[default]
    Hierarchy,
    Materials,
    Textures,
    Collision,
    Diagnostics,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) enum ModelTargetProfile {
    Full,
    #[default]
    SunshineMap,
    SunshineObject,
    SunshinePollution,
}

impl ModelTargetProfile {
    fn label(self) -> &'static str {
        match self {
            Self::Full => "Full J3D",
            Self::SunshineMap => "Sunshine map",
            Self::SunshineObject => "Sunshine object",
            Self::SunshinePollution => "Sunshine pollution",
        }
    }

    fn authoring_profile(self) -> TargetLoaderProfile {
        match self {
            Self::Full => TargetLoaderProfile::Full,
            Self::SunshineMap => TargetLoaderProfile::SunshineMap,
            Self::SunshineObject => TargetLoaderProfile::SunshineObject,
            Self::SunshinePollution => TargetLoaderProfile::SunshinePollution,
        }
    }
}

fn model_instance_loader_flags(
    placement: &ModelInstancePlacement,
    registry: Option<&ObjectRegistry>,
) -> Result<u32, String> {
    match placement.export_mode {
        ModelInstanceExportMode::SeparateRuntimeObject => Ok(SMS_SM_J3D_ACT_MODEL_LOAD_FLAGS),
        ModelInstanceExportMode::MapTerrain => Ok(SMS_MAP_MODEL_LOAD_FLAGS),
        ModelInstanceExportMode::Skybox => Ok(SMS_DEFAULT_OBJECT_MODEL_LOAD_FLAGS),
        ModelInstanceExportMode::StockMapObjBase => {
            let selected = placement.stock_map_obj_resource.trim();
            if selected.is_empty() {
                return Err(format!(
                    "model instance {} has no stock MapObjBase resource slot selected",
                    placement.instance_id
                ));
            }
            let registry = registry.ok_or_else(|| {
                format!(
                    "model instance {} cannot resolve stock loader flags because the decomp-derived registry is unavailable",
                    placement.instance_id
                )
            })?;
            registry
                .find_map_obj_resource(selected)
                .map(|slot| slot.load_flags)
                .ok_or_else(|| {
                    format!(
                        "model instance {} selected unknown stock MapObjBase resource {selected:?}",
                        placement.instance_id
                    )
                })
        }
    }
}

fn model_instance_preview_key(
    instance: &EditorModelInstance,
    registry: Option<&ObjectRegistry>,
) -> Result<AuthoredModelPreviewKey, String> {
    Ok(AuthoredModelPreviewKey {
        asset_id: instance.placement.asset_id,
        loader_flags: model_instance_loader_flags(&instance.placement, registry)?,
    })
}

fn should_default_to_map_terrain(
    authored_blank_stage: bool,
    asset_has_collision: bool,
    stage_id: &str,
    instances: &[EditorModelInstance],
) -> bool {
    authored_blank_stage
        && asset_has_collision
        && !instances.iter().any(|instance| {
            instance.stage_id.eq_ignore_ascii_case(stage_id)
                && instance.placement.export_mode == ModelInstanceExportMode::MapTerrain
        })
}

fn loader_diagnostics_for_document(
    document: &ModelAssetDocument,
    loader_flags: u32,
) -> Vec<GxMaterialDiagnostic> {
    let materials = document
        .materials
        .iter()
        .map(|material| material.gx.clone())
        .collect::<Vec<_>>();
    validate_materials_for_loader(&materials, TargetLoaderProfile::Custom(loader_flags))
}

fn apply_opaque_material_preset(material: &mut GxMaterial) {
    // Sunshine loaders without the full pixel-engine block derive these states
    // from material_mode, while full J3D loaders consume the explicit fields.
    // Keep both representations in agreement so switching profiles cannot
    // leave a supposedly opaque material blending or skipping depth writes.
    material.material_mode = 1;
    material.cull_mode = 2;
    material.alpha_compare = sms_authoring::GxAlphaCompare::default();
    material.blend_mode = sms_authoring::GxBlendMode {
        mode: 0,
        source_factor: 1,
        destination_factor: 0,
        logic_operation: 3,
    };
    material.depth_mode = sms_authoring::GxDepthMode::default();
    material.z_compare_location = 1;
}

fn apply_alpha_blend_material_preset(material: &mut GxMaterial) {
    material.material_mode = 4;
    material.alpha_compare = sms_authoring::GxAlphaCompare::default();
    material.blend_mode = sms_authoring::GxBlendMode {
        mode: 1,
        source_factor: 4,
        destination_factor: 5,
        logic_operation: 3,
    };
    material.depth_mode = sms_authoring::GxDepthMode {
        update_enabled: 0,
        ..sms_authoring::GxDepthMode::default()
    };
    material.z_compare_location = 1;
}

fn conservative_base_tev_stage(
    textured: bool,
) -> (sms_authoring::GxTevOrder, sms_authoring::GxTevStage) {
    let order = sms_authoring::GxTevOrder {
        tex_coord: textured.then_some(0),
        tex_map: textured.then_some(0),
        color_channel: 4,
    };
    let stage = if textured {
        sms_authoring::GxTevStage {
            color_inputs: [15, 8, 10, 15],
            alpha_inputs: [7, 4, 5, 7],
            ..sms_authoring::GxTevStage::default()
        }
    } else {
        sms_authoring::GxTevStage {
            color_inputs: [10, 15, 15, 15],
            alpha_inputs: [5, 7, 7, 7],
            ..sms_authoring::GxTevStage::default()
        }
    };
    (order, stage)
}

fn conservative_specular_tev_stage() -> (
    sms_authoring::GxTevOrder,
    sms_authoring::GxTevStage,
    sms_authoring::GxTevSwapMode,
) {
    (
        sms_authoring::GxTevOrder {
            tex_coord: None,
            tex_map: None,
            color_channel: 5,
        },
        sms_authoring::GxTevStage {
            // CPREV + RASC. ZERO/RASC/ONE selects the complete secondary
            // channel before adding CPREV; alpha remains APREV from stage 0.
            color_inputs: [15, 10, 12, 0],
            alpha_inputs: [7, 7, 7, 0],
            ..sms_authoring::GxTevStage::default()
        },
        sms_authoring::GxTevSwapMode {
            raster_swap_table: 0,
            texture_swap_table: 0,
        },
    )
}

fn has_canonical_conservative_base_program(material: &sms_authoring::ModelMaterial) -> bool {
    let gx = &material.gx;
    let (order, stage) = conservative_base_tev_stage(material.base_color_texture.is_some());
    gx.tev_orders[0] == Some(order)
        && gx.tev_stages[0] == Some(stage)
        && gx.tev_swap_modes[0]
            == Some(sms_authoring::GxTevSwapMode {
                raster_swap_table: 0,
                texture_swap_table: 0,
            })
}

fn has_conservative_specular_stage(material: &sms_authoring::ModelMaterial) -> bool {
    let gx = &material.gx;
    let (order, stage, swap) = conservative_specular_tev_stage();
    gx.tev_stage_count == 2
        && has_canonical_conservative_base_program(material)
        && gx.tev_orders[1] == Some(order)
        && gx.tev_stages[1] == Some(stage)
        && gx.tev_swap_modes[1] == Some(swap)
}

fn apply_conservative_diffuse_specular_preset(material: &mut sms_authoring::ModelMaterial) -> bool {
    let is_existing_preset = has_conservative_specular_stage(material);
    let has_free_second_stage = material.gx.tev_stage_count == 1
        && material.gx.tev_orders[1].is_none()
        && material.gx.tev_stages[1].is_none()
        && material.gx.tev_swap_modes[1].is_none()
        && material.gx.material_colors[1].is_none()
        && material.gx.ambient_colors[1].is_none();
    if !has_canonical_conservative_base_program(material)
        || (!is_existing_preset && !has_free_second_stage)
    {
        return false;
    }

    let gx = &mut material.gx;
    gx.color_channel_count = 2;
    gx.color_channels = [
        Some(sms_authoring::GxColorChannel {
            enable: 1,
            material_source: 0,
            light_mask: 0x01,
            diffuse_function: 2,
            attenuation_function: 1,
            ambient_source: 0,
        }),
        Some(sms_authoring::GxColorChannel::default()),
        Some(sms_authoring::GxColorChannel {
            enable: 1,
            material_source: 0,
            light_mask: 0x04,
            diffuse_function: 1,
            attenuation_function: 0,
            ambient_source: 0,
        }),
        Some(sms_authoring::GxColorChannel::default()),
    ];
    gx.material_colors[1] = Some([255; 4]);
    gx.ambient_colors[1] = Some([0; 4]);
    let (order, stage, swap) = conservative_specular_tev_stage();
    gx.tev_stage_count = 2;
    gx.tev_orders[1] = Some(order);
    gx.tev_stages[1] = Some(stage);
    gx.tev_swap_modes[1] = Some(swap);
    gx.tev_konst_color_selectors[1] = 0x0c;
    gx.tev_konst_alpha_selectors[1] = 0x1c;
    true
}

fn apply_conservative_unlit_preset(material: &mut sms_authoring::ModelMaterial) {
    if has_conservative_specular_stage(material) {
        let gx = &mut material.gx;
        gx.color_channel_count = 1;
        gx.color_channels = [
            Some(sms_authoring::GxColorChannel::default()),
            Some(sms_authoring::GxColorChannel::default()),
            None,
            None,
        ];
        gx.material_colors[1] = None;
        gx.ambient_colors[1] = None;
        gx.tev_stage_count = 1;
        gx.tev_orders[1] = None;
        gx.tev_stages[1] = None;
        gx.tev_swap_modes[1] = None;
        gx.tev_konst_color_selectors[1] = 0x0c;
        gx.tev_konst_alpha_selectors[1] = 0x1c;
        return;
    }

    // Advanced TEV programs may use either raster channel in ways the simple
    // preset cannot reconstruct. Disable their existing channel controls in
    // place without deleting stages, selectors, colors, or texture state.
    for channel in material.gx.color_channels.iter_mut().flatten() {
        channel.enable = 0;
    }
}

fn validate_instance_loader_compatibility(
    placement: &ModelInstancePlacement,
    document: &ModelAssetDocument,
    loader_flags: u32,
    target: &str,
) -> Result<(), String> {
    if let Some(diagnostic) = loader_diagnostics_for_document(document, loader_flags)
        .into_iter()
        .find(|diagnostic| diagnostic.severity == GxDiagnosticSeverity::Error)
    {
        return Err(format!(
            "model instance {} (asset {}) cannot use {target} loader flags {loader_flags:#010x}: material {}: {} ({})",
            placement.instance_id,
            placement.asset_id,
            diagnostic.material_index,
            diagnostic.message,
            diagnostic.code
        ));
    }
    Ok(())
}

fn check_model_export_cancelled(cancelled: Option<&AtomicBool>) -> Result<(), String> {
    match cancelled {
        Some(cancelled) => managed_build::check_cancelled(cancelled),
        None => Ok(()),
    }
}

fn validate_standalone_actor_model_budget(
    placement: &ModelInstancePlacement,
    model: &J3dRebuildDocument,
    target: &str,
) -> Result<(), String> {
    let total_size = 0x20u64
        + model
            .sections
            .iter()
            .map(|section| u64::from(section.declared_size))
            .sum::<u64>();
    let tex1_size = model
        .sections
        .iter()
        .find(|section| section.tag() == *b"TEX1")
        .map_or(0, |section| u64::from(section.declared_size));
    if total_size <= STANDALONE_ACTOR_BMD_SAFETY_BUDGET
        && tex1_size <= STANDALONE_ACTOR_TEX1_SAFETY_BUDGET
    {
        return Ok(());
    }
    Err(format!(
        "model instance {} (asset {}) compiles to a {}-byte BMD3 with a {}-byte TEX1, exceeding the Graffito-Editor standalone {target} safety budget ({} bytes total / {} bytes TEX1) for Sunshine's 24 MiB MEM1. This is an editor safety budget, not a BMD format limit; prune unused textures, downsize them, or use map-terrain mode when the model is intentionally the stage terrain",
        placement.instance_id,
        placement.asset_id,
        total_size,
        tex1_size,
        STANDALONE_ACTOR_BMD_SAFETY_BUDGET,
        STANDALONE_ACTOR_TEX1_SAFETY_BUDGET,
    ))
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(super) struct EditorModelInstance {
    #[serde(default)]
    pub(super) stage_id: String,
    pub(super) placement: ModelInstancePlacement,
    pub(super) local_bounds: [[f32; 3]; 2],
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct ModelInstanceManifest {
    format_version: u32,
    #[serde(default)]
    instances: Vec<EditorModelInstance>,
}

impl Default for ModelInstanceManifest {
    fn default() -> Self {
        Self {
            format_version: MODEL_INSTANCE_MANIFEST_VERSION,
            instances: Vec::new(),
        }
    }
}

#[derive(Debug, Clone)]
pub(super) struct ModelAssetUndoRecord {
    pub(super) label: String,
    pub(super) before: ModelAssetDocument,
    pub(super) after: ModelAssetDocument,
}

#[derive(Debug, Clone)]
pub(super) struct ModelInstanceUndoRecord {
    pub(super) label: String,
    pub(super) before: Vec<EditorModelInstance>,
    pub(super) after: Vec<EditorModelInstance>,
}

#[derive(Debug, Clone)]
pub(super) struct ModelAssetDragPayload {
    pub(super) asset_id: AssetId,
}

pub(super) struct ModelImportJob {
    pub(super) cancel: Arc<AtomicBool>,
    pub(super) receiver: Receiver<Result<PreparedModelImport, String>>,
}

#[derive(Debug)]
pub(super) struct AuthoredModelPreviewGeometry {
    pub(super) preview: J3dGeometryPreview,
    pub(super) loader_diagnostics: Vec<GxMaterialDiagnostic>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(super) struct AuthoredModelPreviewKey {
    pub(super) asset_id: AssetId,
    pub(super) loader_flags: u32,
}

#[derive(Debug, Clone, Copy)]
pub(super) struct AuthoredModelPreviewBase {
    had_stage_preview: bool,
    triangle_count: usize,
    texture_count: usize,
    material_count: usize,
    material_binding_count: usize,
    bounds_min: [f32; 3],
    bounds_max: [f32; 3],
    camera_bounds_min: [f32; 3],
    camera_bounds_max: [f32; 3],
}

pub(super) struct PreparedModelImport {
    pub(super) relative_path: PathBuf,
    pub(super) source_path: PathBuf,
    pub(super) document: ModelAssetDocument,
}

impl SmsEditorApp {
    pub(super) fn model_content_root(&self) -> Option<PathBuf> {
        let root = self.project_root.trim();
        (!root.is_empty()).then(|| PathBuf::from(root).join("Content"))
    }

    fn model_catalog(&self) -> Result<ModelAssetCatalog, String> {
        let root = self
            .model_content_root()
            .ok_or_else(|| "Project data root is not configured".to_string())?;
        ModelAssetCatalog::open_content_root(root).map_err(|error| error.to_string())
    }

    fn content_catalog_mutation_allowed(&mut self, action: &str) -> bool {
        if self.background_receiver.is_none() {
            return true;
        }
        let message = format!(
            "Cannot {action} while a stage build or another background operation is reading the project Content snapshot; wait for it to finish."
        );
        self.model_editor_error = Some(message.clone());
        self.log.push(message);
        false
    }

    pub(super) fn refresh_model_catalog(&mut self) {
        let root = self.model_content_root();
        if root == self.model_catalog_root {
            return;
        }
        self.model_catalog_root = root.clone();
        self.model_catalog_entries.clear();
        self.model_catalog_issues.clear();
        let Some(root) = root else {
            return;
        };
        match ModelAssetCatalog::open_content_root(&root).and_then(|catalog| catalog.scan()) {
            Ok(scan) => {
                self.model_catalog_entries = scan.assets;
                self.model_catalog_issues = scan
                    .issues
                    .into_iter()
                    .map(|issue| {
                        format!(
                            "{:?} at '{}': {}",
                            issue.kind,
                            issue.relative_path.display(),
                            issue.message
                        )
                    })
                    .collect();
                self.load_model_instances();
            }
            Err(error) => {
                self.model_catalog_issues.push(error.to_string());
                self.log.push(format!(
                    "Could not scan model assets under '{}': {error}",
                    root.display()
                ));
            }
        }
    }

    pub(super) fn force_refresh_model_catalog(&mut self) {
        self.model_catalog_entries.clear();
        self.model_catalog_root = None;
        self.refresh_model_catalog();
    }

    pub(super) fn begin_model_import(&mut self) {
        if !self.content_catalog_mutation_allowed("start a model import") {
            return;
        }
        if self.model_import_job.is_some() {
            self.log
                .push("A model import is already running.".to_string());
            return;
        }
        let Some(source_path) = rfd::FileDialog::new()
            .set_title("Import glTF or GLB Model")
            .add_filter("glTF model", &["gltf", "glb"])
            .pick_file()
        else {
            return;
        };
        let Some(file_stem) = source_path.file_stem().and_then(|stem| stem.to_str()) else {
            self.log
                .push("The selected model has no usable file name.".to_string());
            return;
        };
        let relative_path = self
            .model_folder_filter
            .as_ref()
            .map_or_else(PathBuf::new, PathBuf::from)
            .join(format!("{file_stem}.smsmodel"));
        let mut options = self.model_import_options.clone();
        if let CollisionSource::SeparateFile {
            options: collision_options,
            ..
        } = &mut options.collision
        {
            collision_options.coordinate_conversion = options.coordinate_conversion;
        }
        let cancel = Arc::new(AtomicBool::new(false));
        let worker_cancel = Arc::clone(&cancel);
        let (sender, receiver) = std::sync::mpsc::channel();
        let task_source = source_path.clone();
        std::thread::spawn(move || {
            if worker_cancel.load(Ordering::Acquire) {
                return;
            }
            let result = sms_authoring::import_model(&task_source, &options)
                .map(|imported| PreparedModelImport {
                    relative_path,
                    source_path: task_source,
                    document: imported.asset,
                })
                .map_err(|error| error.to_string());
            if !worker_cancel.load(Ordering::Acquire) {
                let _ = sender.send(result);
            }
        });
        self.model_import_job = Some(ModelImportJob { cancel, receiver });
        self.log.push(format!(
            "Importing model '{}' in the background...",
            source_path.display()
        ));
    }

    pub(super) fn cancel_model_import(&mut self) {
        if let Some(job) = self.model_import_job.take() {
            job.cancel.store(true, Ordering::Release);
            self.log
                .push("Canceled the pending model import; no asset was committed.".to_string());
        }
    }

    pub(super) fn poll_model_import(&mut self, ctx: &egui::Context) {
        if self.background_receiver.is_some() && self.model_import_job.is_some() {
            ctx.request_repaint_after(Duration::from_millis(33));
            return;
        }
        let result = self
            .model_import_job
            .as_ref()
            .map(|job| job.receiver.try_recv());
        match result {
            Some(Ok(Ok(prepared))) => {
                self.model_import_job = None;
                self.commit_prepared_model_import(prepared);
            }
            Some(Ok(Err(error))) => {
                self.model_import_job = None;
                self.log.push(format!("Model import failed: {error}"));
            }
            Some(Err(std::sync::mpsc::TryRecvError::Disconnected)) => {
                self.model_import_job = None;
            }
            Some(Err(std::sync::mpsc::TryRecvError::Empty)) => {
                ctx.request_repaint_after(Duration::from_millis(33));
            }
            None => {}
        }
    }

    fn commit_prepared_model_import(&mut self, prepared: PreparedModelImport) {
        if !self.content_catalog_mutation_allowed("commit the imported model") {
            return;
        }
        let catalog = match self.model_catalog() {
            Ok(catalog) => catalog,
            Err(error) => {
                self.log
                    .push(format!("Model import was not committed: {error}"));
                return;
            }
        };
        match catalog.create_asset(&prepared.relative_path, &prepared.document) {
            Ok(entry) => {
                let source = prepared.source_path.display().to_string();
                self.force_refresh_model_catalog();
                self.select_model_asset(entry.id);
                self.log.push(format!(
                    "Imported '{}' as Content/{}; the source glTF was not retained.",
                    source,
                    entry.relative_path.display()
                ));
            }
            Err(error) => self
                .log
                .push(format!("Model import could not be committed: {error}")),
        }
    }

    pub(super) fn select_model_asset(&mut self, id: AssetId) {
        if self.asset_dirty && !self.save_selected_model_asset() {
            return;
        }
        let catalog = match self.model_catalog() {
            Ok(catalog) => catalog,
            Err(error) => {
                self.log.push(error);
                return;
            }
        };
        match catalog.load_asset(id) {
            Ok(document) => {
                if let Some(entry) = self
                    .model_catalog_entries
                    .iter()
                    .find(|entry| entry.id == id)
                {
                    self.model_asset_rename_draft = entry.name.clone();
                    self.model_asset_move_draft = entry
                        .relative_path
                        .parent()
                        .unwrap_or_else(|| Path::new(""))
                        .to_string_lossy()
                        .replace('\\', "/");
                }
                self.selected_model_asset = Some(id);
                self.cache_model_previews_for_document(id, &document);
                self.selected_model_document = Some(document.clone());
                self.saved_model_document = Some(document);
                self.selected_model_material = 0;
                self.selected_model_texture = 0;
                self.selected_model_instance_id = None;
                self.selected_object_id = None;
                self.asset_dirty = false;
                self.asset_undo_stack.clear();
                self.asset_redo_stack.clear();
                self.sync_gx_json_draft();
                self.sync_texture_json_draft();
            }
            Err(error) => self
                .log
                .push(format!("Could not open model asset: {error}")),
        }
    }

    pub(super) fn save_selected_model_asset(&mut self) -> bool {
        if !self.asset_dirty {
            return true;
        }
        if !self.content_catalog_mutation_allowed("save the model asset") {
            return false;
        }
        let (Some(id), Some(document)) = (
            self.selected_model_asset,
            self.selected_model_document.clone(),
        ) else {
            return true;
        };
        if let Err(error) = document.validate() {
            self.model_editor_error = Some(format!("Asset is invalid: {error}"));
            return false;
        }
        match self.model_catalog().and_then(|catalog| {
            catalog
                .save_asset(id, &document)
                .map_err(|error| error.to_string())
        }) {
            Ok(entry) => {
                self.saved_model_document = Some(document);
                self.asset_dirty = false;
                self.model_asset_preview_cache
                    .retain(|key, _| key.asset_id != id);
                self.force_refresh_model_catalog();
                self.selected_model_asset = Some(entry.id);
                self.log
                    .push(format!("Saved model asset '{}'.", entry.name));
                true
            }
            Err(error) => {
                self.model_editor_error = Some(format!("Could not save model asset: {error}"));
                false
            }
        }
    }

    pub(super) fn rename_selected_model_asset(&mut self) {
        if !self.content_catalog_mutation_allowed("rename the model asset") {
            return;
        }
        let Some(id) = self.selected_model_asset else {
            return;
        };
        let name = self.model_asset_rename_draft.trim().to_string();
        if name.is_empty() {
            self.model_editor_error = Some("Asset name cannot be empty.".to_string());
            return;
        }
        match self.model_catalog().and_then(|catalog| {
            catalog
                .rename_asset(id, &name)
                .map_err(|error| error.to_string())
        }) {
            Ok(entry) => {
                self.force_refresh_model_catalog();
                self.select_model_asset(entry.id);
                self.log
                    .push(format!("Renamed model asset to '{}'.", entry.name));
            }
            Err(error) => self.model_editor_error = Some(format!("Rename failed: {error}")),
        }
    }

    pub(super) fn move_selected_model_asset(&mut self) {
        if !self.content_catalog_mutation_allowed("move the model asset") {
            return;
        }
        let Some(id) = self.selected_model_asset else {
            return;
        };
        let folder = PathBuf::from(self.model_asset_move_draft.trim());
        match self.model_catalog().and_then(|catalog| {
            catalog
                .move_asset(id, folder)
                .map_err(|error| error.to_string())
        }) {
            Ok(entry) => {
                self.force_refresh_model_catalog();
                self.select_model_asset(entry.id);
                self.log.push(format!(
                    "Moved model asset to Content/{}.",
                    entry.relative_path.display()
                ));
            }
            Err(error) => self.model_editor_error = Some(format!("Move failed: {error}")),
        }
    }

    pub(super) fn duplicate_selected_model_asset(&mut self) {
        if !self.content_catalog_mutation_allowed("duplicate the model asset") {
            return;
        }
        let Some(id) = self.selected_model_asset else {
            return;
        };
        let Some(entry) = self
            .model_catalog_entries
            .iter()
            .find(|entry| entry.id == id)
            .cloned()
        else {
            return;
        };
        let parent = entry
            .relative_path
            .parent()
            .unwrap_or_else(|| Path::new(""));
        let stem = entry
            .relative_path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .unwrap_or("model");
        let existing = self
            .model_catalog_entries
            .iter()
            .map(|entry| entry.relative_path.clone())
            .collect::<BTreeSet<_>>();
        let destination = (1u32..)
            .map(|serial| {
                let suffix = if serial == 1 {
                    "copy".to_string()
                } else {
                    format!("copy_{serial}")
                };
                parent.join(format!("{stem}_{suffix}.smsmodel"))
            })
            .find(|path| !existing.contains(path))
            .expect("an unbounded serial range contains an available asset path");
        match self.model_catalog().and_then(|catalog| {
            catalog
                .duplicate_asset(id, &destination)
                .map_err(|error| error.to_string())
        }) {
            Ok(duplicate) => {
                self.force_refresh_model_catalog();
                self.select_model_asset(duplicate.id);
                self.log.push(format!(
                    "Duplicated model asset to Content/{}.",
                    duplicate.relative_path.display()
                ));
            }
            Err(error) => self.model_editor_error = Some(format!("Duplicate failed: {error}")),
        }
    }

    pub(super) fn delete_selected_model_asset(&mut self) {
        if !self.content_catalog_mutation_allowed("delete the model asset") {
            return;
        }
        let Some(id) = self.selected_model_asset else {
            return;
        };
        let references = self
            .model_instances
            .iter()
            .filter(|instance| instance.placement.asset_id == id)
            .map(|instance| AssetReference {
                owner: format!(
                    "stage {} instance {}",
                    instance.stage_id, instance.placement.instance_id
                ),
                project_path: Some(PathBuf::from(MODEL_INSTANCE_MANIFEST_NAME)),
            })
            .collect::<Vec<_>>();
        match self.model_catalog().and_then(|catalog| {
            catalog
                .delete_asset(id, &references)
                .map_err(|error| error.to_string())
        }) {
            Ok(report) => {
                self.selected_model_asset = None;
                self.selected_model_document = None;
                self.saved_model_document = None;
                self.asset_dirty = false;
                self.force_refresh_model_catalog();
                self.log.push(format!(
                    "Deleted Content/{} and its managed blobs.",
                    report.relative_path.display()
                ));
            }
            Err(error) => {
                self.model_editor_error = Some(format!(
                    "Delete blocked; remove placed-instance references first: {error}"
                ));
            }
        }
    }

    pub(super) fn create_model_folder(&mut self) {
        if !self.content_catalog_mutation_allowed("create a Content folder") {
            return;
        }
        let folder = self.new_model_folder_draft.trim().to_string();
        if folder.is_empty() {
            return;
        }
        match self.model_catalog().and_then(|catalog| {
            catalog
                .create_folder(&folder)
                .map_err(|error| error.to_string())
        }) {
            Ok(folder) => {
                self.model_folder_filter = Some(folder.to_string_lossy().replace('\\', "/"));
                self.new_model_folder_draft.clear();
                self.force_refresh_model_catalog();
            }
            Err(error) => self.model_editor_error = Some(format!("Create folder failed: {error}")),
        }
    }

    pub(super) fn mutate_model_asset(
        &mut self,
        label: impl Into<String>,
        mutate: impl FnOnce(&mut ModelAssetDocument),
    ) {
        let Some(before) = self.selected_model_document.clone() else {
            return;
        };
        let Some(document) = self.selected_model_document.as_mut() else {
            return;
        };
        mutate(document);
        if *document == before {
            return;
        }
        let after = document.clone();
        self.asset_undo_stack.push_back(ModelAssetUndoRecord {
            label: label.into(),
            before,
            after,
        });
        if self.asset_undo_stack.len() > MAX_MODEL_UNDO_RECORDS {
            self.asset_undo_stack.pop_front();
        }
        self.asset_redo_stack.clear();
        self.asset_dirty = self.saved_model_document.as_ref() != Some(document);
        self.sync_gx_json_draft();
    }

    pub(super) fn undo_model_asset(&mut self) -> bool {
        let Some(record) = self.asset_undo_stack.pop_back() else {
            return false;
        };
        self.selected_model_document = Some(record.before.clone());
        self.asset_dirty = self.saved_model_document.as_ref() != Some(&record.before);
        self.log.push(format!("Undo model edit: {}.", record.label));
        self.asset_redo_stack.push_back(record);
        self.sync_gx_json_draft();
        self.sync_texture_json_draft();
        true
    }

    pub(super) fn redo_model_asset(&mut self) -> bool {
        let Some(record) = self.asset_redo_stack.pop_back() else {
            return false;
        };
        self.selected_model_document = Some(record.after.clone());
        self.asset_dirty = self.saved_model_document.as_ref() != Some(&record.after);
        self.log.push(format!("Redo model edit: {}.", record.label));
        self.asset_undo_stack.push_back(record);
        self.sync_gx_json_draft();
        self.sync_texture_json_draft();
        true
    }

    pub(super) fn sync_gx_json_draft(&mut self) {
        self.gx_json_draft = self
            .selected_model_document
            .as_ref()
            .and_then(|document| document.materials.get(self.selected_model_material))
            .and_then(|material| serde_json::to_string_pretty(&material.gx).ok())
            .unwrap_or_default();
        self.model_editor_error = None;
    }

    pub(super) fn sync_texture_json_draft(&mut self) {
        self.texture_json_draft = self
            .selected_model_document
            .as_ref()
            .and_then(|document| document.textures.get(self.selected_model_texture))
            .and_then(|texture| serde_json::to_string_pretty(&texture.encode_options).ok())
            .unwrap_or_default();
        self.model_editor_error = None;
    }

    pub(super) fn apply_texture_json_draft(&mut self) {
        let texture_index = self.selected_model_texture;
        let options = match serde_json::from_str::<GxTextureEncodeOptions>(&self.texture_json_draft)
        {
            Ok(options) => options,
            Err(error) => {
                self.model_editor_error = Some(format!("Invalid TEX1 encode state: {error}"));
                return;
            }
        };
        self.mutate_model_asset("Applied TEX1 encode state", move |document| {
            if let Some(texture) = document.textures.get_mut(texture_index) {
                texture.encode_options = options;
            }
        });
        self.sync_texture_json_draft();
    }

    pub(super) fn apply_gx_json_draft(&mut self) {
        let material_index = self.selected_model_material;
        let gx = match serde_json::from_str::<GxMaterial>(&self.gx_json_draft) {
            Ok(gx) => gx,
            Err(error) => {
                self.model_editor_error = Some(format!("Invalid complete GX state: {error}"));
                return;
            }
        };
        self.mutate_model_asset("Applied complete GX state", move |document| {
            if let Some(material) = document.materials.get_mut(material_index) {
                material.gx = gx;
            }
        });
        if let Some(document) = &self.selected_model_document {
            if let Err(error) = document.validate() {
                let _ = self.undo_model_asset();
                self.model_editor_error = Some(format!("GX state is not valid for BMD3: {error}"));
            }
        }
    }

    pub(super) fn model_target_diagnostics(&self) -> Vec<String> {
        let Some(document) = &self.selected_model_document else {
            return Vec::new();
        };
        let materials = document
            .materials
            .iter()
            .map(|material| material.gx.clone())
            .collect::<Vec<_>>();
        validate_materials_for_loader(&materials, self.model_target_profile.authoring_profile())
            .into_iter()
            .map(|diagnostic| {
                format!(
                    "{:?} [{}] material {}: {}",
                    diagnostic.severity,
                    diagnostic.code,
                    diagnostic.material_index,
                    diagnostic.message
                )
            })
            .collect()
    }

    pub(super) fn arm_model_placement(&mut self, id: AssetId) {
        if self.document.is_none() || self.stage_id.trim().is_empty() {
            self.log
                .push("Open a stage before placing a model asset.".to_string());
            return;
        }
        self.active_placement = Some(ActivePlacement::Model { asset_id: id });
        self.tool = EditorTool::Place;
        let label = self
            .model_catalog_entries
            .iter()
            .find(|entry| entry.id == id)
            .map_or_else(|| id.to_string(), |entry| entry.name.clone());
        self.log.push(format!(
            "Placing model '{label}': click in the viewport to choose its position."
        ));
    }

    pub(super) fn spawn_model_instance_at(&mut self, id: AssetId, position: [f32; 3]) {
        if self.document.is_none() || self.stage_id.trim().is_empty() {
            self.log
                .push("Open a stage before placing a model asset.".to_string());
            return;
        }
        if self.asset_dirty && !self.save_selected_model_asset() {
            return;
        }
        let document = if self.selected_model_asset == Some(id) {
            self.selected_model_document.clone()
        } else {
            self.model_catalog()
                .ok()
                .and_then(|catalog| catalog.load_asset(id).ok())
        };
        let Some(document) = document else {
            self.log
                .push("Could not load the model asset for placement.".to_string());
            return;
        };
        let bounds = model_document_bounds(&document).unwrap_or([[-50.0; 3], [50.0; 3]]);
        let mut placement = ModelInstancePlacement::new(id, document.name.clone());
        let authored_blank_stage = self
            .document
            .as_ref()
            .and_then(|document| document.stage_archive.as_ref())
            .is_some_and(|archive| {
                matches!(archive.origin(), sms_scene::StageOrigin::Blank { .. })
            });
        let first_authored_stage_model = authored_blank_stage
            && !self
                .model_instances
                .iter()
                .any(|instance| instance.stage_id.eq_ignore_ascii_case(&self.stage_id));
        if should_default_to_map_terrain(
            authored_blank_stage,
            document.collision.is_some(),
            &self.stage_id,
            &self.model_instances,
        ) {
            placement.export_mode = ModelInstanceExportMode::MapTerrain;
            self.log.push(
                "The first collision-bearing model in this authored stage was assigned as map terrain."
                .to_string(),
            );
        }
        let preview_loader_flags = model_instance_loader_flags(&placement, self.registry.as_ref())
            .unwrap_or(SMS_SM_J3D_ACT_MODEL_LOAD_FLAGS);
        self.cache_model_preview(id, preview_loader_flags, &document);
        placement.transform = transform_to_matrix(Transform {
            translation: position,
            ..Transform::default()
        });
        let before = self.model_instances.clone();
        let instance = EditorModelInstance {
            stage_id: self.stage_id.clone(),
            placement,
            local_bounds: bounds,
        };
        self.selected_model_instance_id = Some(instance.placement.instance_id);
        self.selected_model_asset = None;
        self.selected_model_document = None;
        self.saved_model_document = None;
        self.selected_object_id = None;
        self.model_instances.push(instance);
        self.push_model_instance_undo("Placed model instance", before);
        self.model_instances_dirty = true;
        if let Err(error) = self.save_model_instances() {
            self.log.push(error);
        }
        self.rebuild_gpu_viewport_scene();
        self.clear_viewport_preview_cache();
        self.log.push(format!(
            "Placed model '{}' at {:.1}, {:.1}, {:.1}.",
            document.name, position[0], position[1], position[2]
        ));
        if first_authored_stage_model {
            self.frame_selected_model_instance();
        }
    }

    pub(super) fn selected_model_instance(&self) -> Option<&EditorModelInstance> {
        let id = self.selected_model_instance_id?;
        self.model_instances
            .iter()
            .find(|instance| instance.placement.instance_id == id)
    }

    pub(super) fn frame_selected_model_instance(&mut self) -> bool {
        let Some(instance) = self.selected_model_instance().cloned() else {
            return false;
        };
        let corners = transformed_bounds_corners(&instance);
        let mut minimum = [f32::INFINITY; 3];
        let mut maximum = [f32::NEG_INFINITY; 3];
        for corner in corners {
            for axis in 0..3 {
                minimum[axis] = minimum[axis].min(corner[axis]);
                maximum[axis] = maximum[axis].max(corner[axis]);
            }
        }
        if minimum
            .into_iter()
            .chain(maximum)
            .any(|value| !value.is_finite())
        {
            return false;
        }
        let focus = std::array::from_fn(|axis| (minimum[axis] + maximum[axis]) * 0.5);
        let extent = std::array::from_fn::<_, 3, _>(|axis| maximum[axis] - minimum[axis]);
        let radius =
            (extent[0] * extent[0] + extent[1] * extent[1] + extent[2] * extent[2]).sqrt() * 0.5;
        self.stop_camera_fly();
        let camera = self.renderer.camera_mut();
        camera.focus = focus;
        camera.distance = (radius * 2.8).clamp(250.0, 600_000.0);
        self.viewport_pan = egui::Vec2::ZERO;
        self.viewport_zoom = 1.0;
        self.queue_camera_state_save();
        true
    }

    pub(super) fn update_selected_model_instance(&mut self, updated: EditorModelInstance) {
        let Some(id) = self.selected_model_instance_id else {
            return;
        };
        let before = self.model_instances.clone();
        let Some(index) = self
            .model_instances
            .iter()
            .position(|instance| instance.placement.instance_id == id)
        else {
            return;
        };
        if self.model_instances[index] != updated {
            let previous_loader = model_instance_loader_flags(
                &self.model_instances[index].placement,
                self.registry.as_ref(),
            )
            .ok();
            let updated_loader =
                model_instance_loader_flags(&updated.placement, self.registry.as_ref()).ok();
            self.model_instances[index] = updated;
            self.push_model_instance_undo("Edited model instance", before);
            self.model_instances_dirty = true;
            if let Err(error) = self.save_model_instances() {
                self.log.push(error);
            }
            if previous_loader != updated_loader {
                self.rebuild_model_preview_cache();
            } else {
                self.rebuild_gpu_viewport_scene();
                self.clear_viewport_preview_cache();
            }
        }
    }

    pub(super) fn delete_selected_model_instance(&mut self) -> bool {
        let Some(id) = self.selected_model_instance_id.take() else {
            return false;
        };
        let before = self.model_instances.clone();
        let before_len = before.len();
        self.model_instances
            .retain(|instance| instance.placement.instance_id != id);
        if self.model_instances.len() == before_len {
            return false;
        }
        self.push_model_instance_undo("Deleted model instance", before);
        self.model_instances_dirty = true;
        if let Err(error) = self.save_model_instances() {
            self.log.push(error);
        }
        self.rebuild_model_preview_cache();
        true
    }

    fn push_model_instance_undo(
        &mut self,
        label: impl Into<String>,
        before: Vec<EditorModelInstance>,
    ) {
        let after = self.model_instances.clone();
        if before == after {
            return;
        }
        self.model_instance_undo_stack
            .push_back(ModelInstanceUndoRecord {
                label: label.into(),
                before,
                after,
            });
        if self.model_instance_undo_stack.len() > MAX_MODEL_UNDO_RECORDS {
            self.model_instance_undo_stack.pop_front();
        }
        self.model_instance_redo_stack.clear();
    }

    pub(super) fn undo_model_instance(&mut self) -> bool {
        let Some(record) = self.model_instance_undo_stack.pop_back() else {
            return false;
        };
        self.model_instances = record.before.clone();
        self.model_instances_dirty = true;
        self.log
            .push(format!("Undo instance edit: {}.", record.label));
        self.model_instance_redo_stack.push_back(record);
        if let Err(error) = self.save_model_instances() {
            self.log.push(error);
        }
        self.ensure_model_instance_selection_exists();
        self.rebuild_model_preview_cache();
        true
    }

    pub(super) fn redo_model_instance(&mut self) -> bool {
        let Some(record) = self.model_instance_redo_stack.pop_back() else {
            return false;
        };
        self.model_instances = record.after.clone();
        self.model_instances_dirty = true;
        self.log
            .push(format!("Redo instance edit: {}.", record.label));
        self.model_instance_undo_stack.push_back(record);
        if let Err(error) = self.save_model_instances() {
            self.log.push(error);
        }
        self.ensure_model_instance_selection_exists();
        self.rebuild_model_preview_cache();
        true
    }

    fn ensure_model_instance_selection_exists(&mut self) {
        if self.selected_model_instance_id.is_some_and(|id| {
            !self
                .model_instances
                .iter()
                .any(|instance| instance.placement.instance_id == id)
        }) {
            self.selected_model_instance_id = None;
        }
    }

    pub(super) fn model_instance_at_screen_position(
        &self,
        rect: egui::Rect,
        position: egui::Pos2,
    ) -> Option<uuid::Uuid> {
        let projection = self.camera_projection(rect);
        self.model_instances
            .iter()
            .filter(|instance| instance.stage_id.eq_ignore_ascii_case(&self.stage_id))
            .filter_map(|instance| {
                let transform = matrix_to_transform(instance.placement.transform);
                let (screen, depth) = projection.project_world_to_screen(transform.translation)?;
                (screen.distance(position) <= 28.0)
                    .then_some((depth, instance.placement.instance_id))
            })
            .min_by(|left, right| left.0.total_cmp(&right.0))
            .map(|(_, id)| id)
    }

    pub(super) fn paint_model_instances(&self, painter: &egui::Painter, rect: egui::Rect) {
        let projection = self.camera_projection(rect);
        for instance in self
            .model_instances
            .iter()
            .filter(|instance| instance.stage_id.eq_ignore_ascii_case(&self.stage_id))
        {
            let selected = self.selected_model_instance_id == Some(instance.placement.instance_id);
            let color = if selected {
                egui::Color32::from_rgb(255, 202, 80)
            } else {
                egui::Color32::from_rgb(90, 188, 242)
            };
            let corners = transformed_bounds_corners(instance);
            const EDGES: [[usize; 2]; 12] = [
                [0, 1],
                [0, 2],
                [0, 4],
                [1, 3],
                [1, 5],
                [2, 3],
                [2, 6],
                [3, 7],
                [4, 5],
                [4, 6],
                [5, 7],
                [6, 7],
            ];
            for [a, b] in EDGES {
                if let (Some((a, _)), Some((b, _))) = (
                    projection.project_world_to_screen(corners[a]),
                    projection.project_world_to_screen(corners[b]),
                ) {
                    painter.line_segment(
                        [a, b],
                        egui::Stroke::new(if selected { 2.2 } else { 1.2 }, color),
                    );
                }
            }
            if let Some((center, _)) = projection.project_world_to_screen(
                matrix_to_transform(instance.placement.transform).translation,
            ) {
                painter.circle_filled(center, if selected { 5.0 } else { 3.5 }, color);
            }
        }
    }

    pub(super) fn load_model_instances(&mut self) {
        let Some(root) = self.model_content_root() else {
            self.model_instances.clear();
            self.rebuild_gpu_viewport_scene();
            self.clear_viewport_preview_cache();
            return;
        };
        let path = root.join(MODEL_INSTANCE_MANIFEST_NAME);
        if !path.is_file() {
            self.model_instances.clear();
            self.model_instances_dirty = false;
            self.rebuild_gpu_viewport_scene();
            self.clear_viewport_preview_cache();
            return;
        }
        match fs::read(&path)
            .map_err(|error| error.to_string())
            .and_then(|bytes| {
                serde_json::from_slice::<ModelInstanceManifest>(&bytes)
                    .map_err(|error| error.to_string())
            }) {
            Ok(manifest) if manifest.format_version == MODEL_INSTANCE_MANIFEST_VERSION => {
                self.model_instances = manifest.instances;
                self.model_instances_dirty = false;
                self.rebuild_model_preview_cache();
            }
            Ok(manifest) => self.log.push(format!(
                "Ignored model instance manifest version {} (expected {}).",
                manifest.format_version, MODEL_INSTANCE_MANIFEST_VERSION
            )),
            Err(error) => self
                .log
                .push(format!("Could not load '{}': {error}", path.display())),
        }
    }

    pub(super) fn save_model_instances(&mut self) -> Result<(), String> {
        if !self.model_instances_dirty {
            return Ok(());
        }
        let root = self
            .model_content_root()
            .ok_or_else(|| "Project data root is not configured".to_string())?;
        fs::create_dir_all(&root).map_err(|error| {
            format!(
                "Could not create model content '{}': {error}",
                root.display()
            )
        })?;
        let manifest = ModelInstanceManifest {
            format_version: MODEL_INSTANCE_MANIFEST_VERSION,
            instances: self.model_instances.clone(),
        };
        let mut bytes = serde_json::to_vec_pretty(&manifest).map_err(|error| error.to_string())?;
        bytes.push(b'\n');
        write_atomic(&root.join(MODEL_INSTANCE_MANIFEST_NAME), &bytes)?;
        self.model_instances_dirty = false;
        Ok(())
    }

    fn cache_model_preview(
        &mut self,
        id: AssetId,
        loader_flags: u32,
        document: &ModelAssetDocument,
    ) {
        let key = AuthoredModelPreviewKey {
            asset_id: id,
            loader_flags,
        };
        match build_authored_model_preview(document, loader_flags) {
            Ok(preview) => {
                self.model_asset_preview_cache
                    .insert(key, Arc::new(preview));
            }
            Err(error) => {
                self.model_asset_preview_cache.remove(&key);
                self.log.push(format!(
                    "Could not build preview for model asset {id} with loader flags {loader_flags:#010x}: {error}"
                ));
            }
        }
    }

    fn cache_model_previews_for_document(&mut self, id: AssetId, document: &ModelAssetDocument) {
        let flags = self
            .model_instances
            .iter()
            .filter(|instance| instance.placement.asset_id == id)
            .filter_map(|instance| {
                model_instance_loader_flags(&instance.placement, self.registry.as_ref()).ok()
            })
            .collect::<BTreeSet<_>>();
        for loader_flags in flags {
            self.cache_model_preview(id, loader_flags, document);
        }
    }

    pub(super) fn rebuild_model_preview_cache(&mut self) {
        let mut preview_keys = BTreeSet::new();
        for instance in &self.model_instances {
            match model_instance_preview_key(instance, self.registry.as_ref()) {
                Ok(key) => {
                    preview_keys.insert(key);
                }
                Err(error) => self
                    .log
                    .push(format!("Could not resolve model preview: {error}")),
            }
        }
        let ids = preview_keys
            .iter()
            .map(|key| key.asset_id)
            .collect::<BTreeSet<_>>();
        self.model_asset_preview_cache
            .retain(|key, _| preview_keys.contains(key));
        let Ok(catalog) = self.model_catalog() else {
            return;
        };
        let mut repaired_bounds = false;
        for id in ids {
            match catalog.load_asset(id) {
                Ok(document) => {
                    if let Some(bounds) = model_document_bounds(&document) {
                        for instance in self
                            .model_instances
                            .iter_mut()
                            .filter(|instance| instance.placement.asset_id == id)
                        {
                            if instance.local_bounds != bounds {
                                instance.local_bounds = bounds;
                                repaired_bounds = true;
                            }
                        }
                    }
                    for key in preview_keys.iter().filter(|key| key.asset_id == id) {
                        if !self.model_asset_preview_cache.contains_key(key) {
                            self.cache_model_preview(id, key.loader_flags, &document);
                        }
                    }
                }
                Err(error) => self.log.push(format!(
                    "Could not build preview for model asset {id}: {error}"
                )),
            }
        }
        if repaired_bounds {
            self.model_instances_dirty = true;
            if let Err(error) = self.save_model_instances() {
                self.log.push(error);
            }
        }
        self.rebuild_gpu_viewport_scene();
        self.clear_viewport_preview_cache();
    }

    pub(super) fn reset_authored_model_preview_base(&mut self) {
        self.authored_model_preview_base = None;
    }

    pub(super) fn sync_authored_model_instance_preview(&mut self) {
        self.remove_authored_model_instance_preview();
        if !self.model_instances.iter().any(|instance| {
            instance.stage_id.eq_ignore_ascii_case(&self.stage_id)
                && model_instance_preview_key(instance, self.registry.as_ref())
                    .is_ok_and(|key| self.model_asset_preview_cache.contains_key(&key))
        }) {
            return;
        }

        let had_stage_preview = self.model_preview.is_some();
        let preview = self
            .model_preview
            .get_or_insert_with(empty_authored_model_preview);
        self.authored_model_preview_base = Some(AuthoredModelPreviewBase {
            had_stage_preview,
            triangle_count: preview.triangles.len(),
            texture_count: preview.textures.len(),
            material_count: preview.materials.len(),
            material_binding_count: preview.material_animation_bindings.len(),
            bounds_min: preview.bounds_min,
            bounds_max: preview.bounds_max,
            camera_bounds_min: preview.camera_bounds_min,
            camera_bounds_max: preview.camera_bounds_max,
        });
        let appended = append_authored_model_instances(
            preview,
            &self.model_asset_preview_cache,
            &self.model_instances,
            &self.stage_id,
            self.registry.as_ref(),
        );
        if appended == 0 {
            self.remove_authored_model_instance_preview();
            return;
        }
        recompute_model_preview_bounds(preview);
        if !had_stage_preview {
            preview.camera_bounds_min = preview.bounds_min;
            preview.camera_bounds_max = preview.bounds_max;
        }
    }

    fn remove_authored_model_instance_preview(&mut self) {
        let Some(base) = self.authored_model_preview_base.take() else {
            return;
        };
        let Some(preview) = self.model_preview.as_mut() else {
            return;
        };
        preview.triangles.truncate(base.triangle_count);
        preview.textures.truncate(base.texture_count);
        preview.materials.truncate(base.material_count);
        preview
            .material_animation_bindings
            .truncate(base.material_binding_count);
        preview.bounds_min = base.bounds_min;
        preview.bounds_max = base.bounds_max;
        preview.camera_bounds_min = base.camera_bounds_min;
        preview.camera_bounds_max = base.camera_bounds_max;
        if !base.had_stage_preview {
            self.model_preview = None;
        }
    }

    #[cfg(test)]
    #[cfg(test)]
    pub(super) fn stage_edits_with_model_instances(
        content_root: &Path,
        instances: &[EditorModelInstance],
        base: &sms_scene::StageArchiveEdits,
    ) -> Result<sms_scene::StageArchiveEdits, String> {
        Self::stage_edits_with_model_instances_for_archive(content_root, instances, base, None)
    }

    #[cfg(test)]
    #[cfg(test)]
    pub(super) fn stage_edits_with_model_instances_for_archive(
        content_root: &Path,
        instances: &[EditorModelInstance],
        base: &sms_scene::StageArchiveEdits,
        archive: Option<&SourceFreeStageArchive>,
    ) -> Result<sms_scene::StageArchiveEdits, String> {
        Self::stage_edits_with_model_instances_for_archive_and_registry(
            content_root,
            instances,
            base,
            archive,
            None,
        )
    }

    #[cfg(test)]
    pub(super) fn stage_edits_with_model_instances_for_archive_and_registry(
        content_root: &Path,
        instances: &[EditorModelInstance],
        base: &sms_scene::StageArchiveEdits,
        archive: Option<&SourceFreeStageArchive>,
        registry: Option<&ObjectRegistry>,
    ) -> Result<sms_scene::StageArchiveEdits, String> {
        let assets = Self::load_model_asset_snapshot(content_root, instances)?;
        Self::stage_edits_with_model_instances_from_snapshot(
            &assets, instances, base, archive, registry,
        )
    }

    /// Loads every referenced model asset before a background build starts.
    /// The returned map is the complete immutable Content snapshot consumed by
    /// `stage_edits_with_model_instances_from_snapshot`; that export path does
    /// not reopen any `.smsmodel` or managed blob from disk.
    pub(super) fn load_model_asset_snapshot(
        content_root: &Path,
        instances: &[EditorModelInstance],
    ) -> Result<BTreeMap<AssetId, ModelAssetDocument>, String> {
        if instances.is_empty() {
            return Ok(BTreeMap::new());
        }
        let catalog = ModelAssetCatalog::open_content_root(content_root)
            .map_err(|error| error.to_string())?;
        let mut assets = BTreeMap::new();
        for instance in instances {
            if assets.contains_key(&instance.placement.asset_id) {
                continue;
            }
            let asset = catalog
                .load_asset(instance.placement.asset_id)
                .map_err(|error| {
                    format!(
                        "could not snapshot model instance {} (asset {}): {error}",
                        instance.placement.instance_id, instance.placement.asset_id
                    )
                })?;
            assets.insert(instance.placement.asset_id, asset);
        }
        Ok(assets)
    }

    #[cfg(test)]
    pub(super) fn stage_edits_with_model_instances_from_snapshot(
        assets: &BTreeMap<AssetId, ModelAssetDocument>,
        instances: &[EditorModelInstance],
        base: &sms_scene::StageArchiveEdits,
        archive: Option<&SourceFreeStageArchive>,
        registry: Option<&ObjectRegistry>,
    ) -> Result<sms_scene::StageArchiveEdits, String> {
        Self::stage_edits_with_model_instances_from_snapshot_impl(
            assets, instances, base, archive, registry, None,
        )
    }

    pub(super) fn stage_edits_with_model_instances_from_snapshot_cancellable(
        assets: &BTreeMap<AssetId, ModelAssetDocument>,
        instances: &[EditorModelInstance],
        base: &sms_scene::StageArchiveEdits,
        archive: Option<&SourceFreeStageArchive>,
        registry: Option<&ObjectRegistry>,
        cancelled: &AtomicBool,
    ) -> Result<sms_scene::StageArchiveEdits, String> {
        Self::stage_edits_with_model_instances_from_snapshot_impl(
            assets,
            instances,
            base,
            archive,
            registry,
            Some(cancelled),
        )
    }

    fn stage_edits_with_model_instances_from_snapshot_impl(
        assets: &BTreeMap<AssetId, ModelAssetDocument>,
        instances: &[EditorModelInstance],
        base: &sms_scene::StageArchiveEdits,
        archive: Option<&SourceFreeStageArchive>,
        registry: Option<&ObjectRegistry>,
        cancelled: Option<&AtomicBool>,
    ) -> Result<sms_scene::StageArchiveEdits, String> {
        check_model_export_cancelled(cancelled)?;
        let mut edits = base.clone();
        if instances.is_empty() {
            synchronize_runtime_sky_reflection_bundle(archive, &mut edits)?;
            return Ok(edits);
        }
        let mut separate = Vec::new();
        let mut stock = Vec::new();
        let mut map_terrain = Vec::new();
        let mut skybox = Vec::new();
        for instance in instances {
            check_model_export_cancelled(cancelled)?;
            let asset = assets
                .get(&instance.placement.asset_id)
                .cloned()
                .ok_or_else(|| {
                    format!(
                        "model snapshot is missing instance {} asset {}; take a new Content snapshot before starting the build",
                        instance.placement.instance_id, instance.placement.asset_id
                    )
                })?;
            let unacknowledged = asset.unacknowledged_required_diagnostics();
            if !unacknowledged.is_empty() {
                let codes = unacknowledged
                    .iter()
                    .map(|diagnostic| format!("{:?}", diagnostic.code))
                    .collect::<Vec<_>>()
                    .join(", ");
                return Err(format!(
                    "model instance {} uses asset {} with unacknowledged import diagnostics ({codes}); review and save the asset before export",
                    instance.placement.instance_id, instance.placement.asset_id
                ));
            }
            let resolved = ResolvedModelInstance {
                placement: instance.placement.clone(),
                asset,
            };
            match instance.placement.export_mode {
                ModelInstanceExportMode::SeparateRuntimeObject => separate.push(resolved),
                ModelInstanceExportMode::MapTerrain => map_terrain.push(resolved),
                ModelInstanceExportMode::StockMapObjBase => stock.push(resolved),
                ModelInstanceExportMode::Skybox => skybox.push(resolved),
            }
        }

        if !separate.is_empty() {
            for resolved in &separate {
                check_model_export_cancelled(cancelled)?;
                validate_instance_loader_compatibility(
                    &resolved.placement,
                    &resolved.asset,
                    SMS_SM_J3D_ACT_MODEL_LOAD_FLAGS,
                    "SmJ3DAct",
                )?;
            }
            let archive = archive.ok_or_else(|| {
                "separate runtime-object export requires the open source-free stage archive so map/scene.bin and map/tables.bin can be edited semantically"
                    .to_string()
            })?;
            let scene_parent = runtime_actor_parent_path(archive)?;

            let mut characters = Vec::new();
            let mut separate_assets = BTreeMap::new();
            for resolved in &separate {
                separate_assets
                    .entry(resolved.placement.asset_id)
                    .or_insert_with(|| resolved.asset.clone());
            }
            for (asset_id, asset) in separate_assets {
                check_model_export_cancelled(cancelled)?;
                let resource_key = model_runtime_resource_key(asset_id);
                let model = asset.compile_bmd_document().map_err(|error| {
                    format!("could not compile separate runtime BMD3 for asset {asset_id}: {error}")
                })?;
                check_model_export_cancelled(cancelled)?;
                let placement = &separate
                    .iter()
                    .find(|resolved| resolved.placement.asset_id == asset_id)
                    .expect("the deduplicated separate asset came from a placement")
                    .placement;
                validate_standalone_actor_model_budget(placement, &model, "SmJ3DAct")?;
                edits.upsert_model(
                    format!("mapobj/{resource_key}/default.bmd").into_bytes(),
                    model,
                );
                characters.push(runtime_obj_chara_record(asset_id, &resource_key)?);
            }

            if archive.resource(b"map/tables.bin").is_some() {
                let tables_parent =
                    find_jdrama_group_path(archive, b"map/tables.bin", "NameRefGrp")?;
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
                )
                .map_err(|error| format!("could not build authored map/tables.bin: {error}"))?;
                edits.insert_resource(
                    b"map/tables.bin".to_vec(),
                    StageResourceDocument::Placement(JDramaDocument { root }),
                );
            }

            for resolved in &separate {
                edits.insert_placement(
                    b"map/scene.bin".to_vec(),
                    scene_parent.clone(),
                    runtime_sm_j3d_actor_record(&resolved.placement)?,
                );
            }
        }

        if !stock.is_empty() {
            let archive = archive.ok_or_else(|| {
                "stock MapObjBase export requires the open source-free stage archive so map/scene.bin can be edited semantically"
                    .to_string()
            })?;
            let registry = registry.ok_or_else(|| {
                "stock MapObjBase export requires the decomp-derived object registry; arbitrary resource keys are not safe"
                    .to_string()
            })?;
            let object_group = archive
                .find_group_record_path(b"map/scene.bin", "IdxGroup", Some(3))
                .map_err(|error| {
                    format!("could not locate the stock MapObjBase object group: {error}")
                })?
                .ok_or_else(|| {
                    "map/scene.bin has no unambiguous IdxGroup with group_index 3 for stock MapObjBase insertion"
                        .to_string()
                })?;
            let scene_record_names = stage_scene_record_names(archive, base)?;

            let mut slot_assets = BTreeMap::<String, AssetId>::new();
            let mut model_assets = BTreeMap::<Vec<u8>, (AssetId, String)>::new();
            let mut collision_assets =
                BTreeMap::<Vec<u8>, (AssetId, String, bool, Option<CollisionSurface>)>::new();
            for resolved in &stock {
                check_model_export_cancelled(cancelled)?;
                let placement = &resolved.placement;
                let selected = placement.stock_map_obj_resource.trim();
                if selected.is_empty() {
                    return Err(format!(
                        "model instance {} cannot export through Stock MapObjBase because no stock resource slot was selected",
                        placement.instance_id
                    ));
                }
                let slot = registry.find_map_obj_resource(selected).ok_or_else(|| {
                    format!(
                        "model instance {} selected unknown stock MapObjBase resource {selected:?}; resource names are exact and case-sensitive",
                        placement.instance_id
                    )
                })?;
                if slot.required_manager_name.is_empty() {
                    return Err(format!(
                        "stock MapObjBase resource {selected:?} has no decomp-derived TMapObjData::unk8 manager dependency; a source-free export cannot guess which TLiveManager must own it"
                    ));
                }
                if !scene_record_names.contains(&slot.required_manager_name) {
                    return Err(format!(
                        "stock MapObjBase resource {selected:?} requires the exact scene manager record {:?} from TMapObjData::unk8, but map/scene.bin does not contain it",
                        slot.required_manager_name
                    ));
                }
                if slot.has_hold_dependency {
                    return Err(format!(
                        "stock MapObjBase resource {selected:?} has compiled TMapObjData::mHold model/joint dependencies; replacing only its primary BMD/COL is not source-free safe"
                    ));
                }
                if slot.has_move_dependency {
                    return Err(format!(
                        "stock MapObjBase resource {selected:?} has compiled TMapObjData::mMove BCK/joint dependencies; replacing only its primary BMD/COL is not source-free safe"
                    ));
                }
                if slot.object_flags & 0x80 != 0 {
                    return Err(format!(
                        "stock MapObjBase resource {selected:?} uses object flag 0x80 and requires an additional decomp-defined damage-height field; this generic placement path refuses to invent it"
                    ));
                }
                if !slot.uses_resource_name_model_fallback {
                    return Err(format!(
                        "stock MapObjBase resource {selected:?} uses compiled animation/model metadata instead of the verified <resource_name>.bmd fallback and is not safe for source-free replacement"
                    ));
                }
                let primary_model = slot.primary_model.as_deref().ok_or_else(|| {
                    format!(
                        "stock MapObjBase resource {selected:?} does not instantiate a primary model"
                    )
                })?;
                let model_path = stock_mapobj_archive_path(primary_model, ".bmd")?;

                if let Some(previous_asset) =
                    slot_assets.insert(slot.resource_name.clone(), placement.asset_id)
                {
                    if previous_asset != placement.asset_id {
                        return Err(format!(
                            "stock MapObjBase resource {:?} is assigned to both asset {} and asset {}; one compiled stock slot cannot resolve two different authored models",
                            slot.resource_name, previous_asset, placement.asset_id
                        ));
                    }
                }
                if let Some((previous_asset, previous_slot)) = model_assets.insert(
                    model_path.clone(),
                    (placement.asset_id, slot.resource_name.clone()),
                ) {
                    if previous_asset != placement.asset_id {
                        return Err(format!(
                            "stock MapObjBase resources {previous_slot:?} and {:?} share model {}, but were assigned different assets {} and {}; replacing that global BMD would be ambiguous",
                            slot.resource_name,
                            String::from_utf8_lossy(&model_path),
                            previous_asset,
                            placement.asset_id
                        ));
                    }
                }

                let materials = resolved
                    .asset
                    .materials
                    .iter()
                    .map(|material| material.gx.clone())
                    .collect::<Vec<_>>();
                let invalid_loader_state = validate_materials_for_loader(
                    &materials,
                    TargetLoaderProfile::Custom(slot.load_flags),
                )
                .into_iter()
                .find(|diagnostic| diagnostic.severity == GxDiagnosticSeverity::Error);
                if let Some(diagnostic) = invalid_loader_state {
                    return Err(format!(
                        "asset {} cannot replace stock MapObjBase resource {selected:?} with loader flags {:#010x}: {} ({})",
                        placement.asset_id, slot.load_flags, diagnostic.message, diagnostic.code
                    ));
                }

                let model = resolved.asset.compile_bmd_document().map_err(|error| {
                    format!(
                        "could not compile stock MapObjBase BMD3 for asset {} in resource {selected:?}: {error}",
                        placement.asset_id
                    )
                })?;
                check_model_export_cancelled(cancelled)?;
                validate_standalone_actor_model_budget(placement, &model, "MapObjBase")?;
                edits.upsert_model(model_path, model);
                edits.insert_placement(
                    b"map/scene.bin".to_vec(),
                    object_group.clone(),
                    stock_map_obj_base_record(placement, &slot.resource_name)?,
                );

                if placement.collision_enabled && slot.collision_resources.is_empty() {
                    return Err(format!(
                        "stock MapObjBase resource {selected:?} has no decomp-verified collision resource, but collision is enabled for instance {}",
                        placement.instance_id
                    ));
                }
                let collision_document = if placement.collision_enabled {
                    let mut collision = resolved.asset.collision.clone().ok_or_else(|| {
                        format!(
                            "stock MapObjBase resource {selected:?} requires enabled collision, but asset {} has no collision document",
                            placement.asset_id
                        )
                    })?;
                    if let Some(surface) = &placement.collision_surface_override {
                        for group in &mut collision.groups {
                            group.surface = surface.clone();
                        }
                    }
                    Some(
                        collision
                            .to_col_file()
                            .map_err(|error| format!("could not compile stock MapObjBase collision for {selected:?}: {error}"))?,
                    )
                } else {
                    None
                };
                for collision_slot in &slot.collision_resources {
                    check_model_export_cancelled(cancelled)?;
                    if let Some(limit) = collision_slot.max_vertices {
                        let actual = resolved
                            .asset
                            .collision
                            .as_ref()
                            .map_or(0, |collision| collision.vertices.len());
                        if placement.collision_enabled && actual > usize::from(limit) {
                            return Err(format!(
                                "stock collision resource {:?} permits at most {limit} vertices, but asset {} has {actual}",
                                collision_slot.resource_name, placement.asset_id
                            ));
                        }
                    }
                    let collision_path =
                        stock_mapobj_archive_path(&collision_slot.resource_name, ".col")?;
                    let collision_owner = (
                        placement.asset_id,
                        slot.resource_name.clone(),
                        placement.collision_enabled,
                        placement.collision_surface_override.clone(),
                    );
                    if let Some(previous) =
                        collision_assets.insert(collision_path.clone(), collision_owner.clone())
                    {
                        if previous != collision_owner {
                            return Err(format!(
                                "stock collision resource {} is shared by incompatible authored placements in slots {:?} and {:?}",
                                String::from_utf8_lossy(&collision_path),
                                previous.1,
                                slot.resource_name
                            ));
                        }
                    }
                    edits.upsert_collision(
                        collision_path,
                        collision_document.clone().unwrap_or_else(empty_col_file),
                    );
                }
            }
        }

        if !map_terrain.is_empty() {
            for resolved in &map_terrain {
                check_model_export_cancelled(cancelled)?;
                validate_instance_loader_compatibility(
                    &resolved.placement,
                    &resolved.asset,
                    SMS_MAP_MODEL_LOAD_FLAGS,
                    "map terrain",
                )?;
            }
            check_model_export_cancelled(cancelled)?;
            let merged = merge_model_instances("AuthoredMapTerrain", &map_terrain)
                .map_err(|error| format!("could not bake map-terrain instances: {error}"))?;
            let model = merged
                .compile_bmd_document()
                .map_err(|error| format!("could not compile replacement map BMD3: {error}"))?;
            check_model_export_cancelled(cancelled)?;
            // This mode is deliberately opt-in: replacing the terrain BMD is
            // destructive and remains distinct from the safe runtime-object
            // path above.
            edits.replace_model(b"map/map/map.bmd".to_vec(), model);
        }

        if skybox.len() > 1 {
            return Err(format!(
                "stage export has {} authored skybox instances; Sunshine's TSky resolves one stage-global map/map/sky.bmd resource",
                skybox.len()
            ));
        }
        if let Some(resolved) = skybox.first() {
            check_model_export_cancelled(cancelled)?;
            validate_instance_loader_compatibility(
                &resolved.placement,
                &resolved.asset,
                SMS_DEFAULT_OBJECT_MODEL_LOAD_FLAGS,
                "TSky",
            )?;
            let model = resolved.asset.compile_bmd_document().map_err(|error| {
                format!(
                    "could not compile authored skybox BMD3 for asset {}: {error}",
                    resolved.placement.asset_id
                )
            })?;
            edits.upsert_model(b"map/map/sky.bmd".to_vec(), model);

            let archive = archive.ok_or_else(|| {
                "skybox export requires the open source-free stage archive so the typed Sky actor can be verified"
                    .to_string()
            })?;
            let has_sky_actor = archive.object_placements().iter().any(|placement| {
                placement
                    .type_name
                    .rsplit("::")
                    .next()
                    .is_some_and(|type_name| type_name == "Sky")
            });
            if !has_sky_actor {
                let sky_group = archive
                    .find_group_record_path(b"map/scene.bin", "IdxGroup", Some(1))
                    .map_err(|error| format!("could not locate the typed sky group: {error}"))?
                    .ok_or_else(|| {
                        "map/scene.bin has no unambiguous IdxGroup with group_index 1 for TSky insertion"
                            .to_string()
                    })?;
                edits.insert_placement(
                    b"map/scene.bin".to_vec(),
                    sky_group,
                    authored_sky_record(&resolved.placement)?,
                );
            }
        }

        let collision_instances = separate
            .iter()
            .chain(map_terrain.iter())
            .cloned()
            .collect::<Vec<_>>();
        if !collision_instances.is_empty() {
            check_model_export_cancelled(cancelled)?;
            let merged =
                merge_model_instances("AuthoredInstanceCollision", &collision_instances)
                    .map_err(|error| format!("could not merge placed model collision: {error}"))?;
            check_model_export_cancelled(cancelled)?;
            let collision = merged
                .collision
                .as_ref()
                .map(|collision| collision.to_col_file())
                .transpose()
                .map_err(|error| format!("could not compile placed world COL: {error}"))?;
            if let Some(collision) = collision {
                edits.append_collision(b"map/map.col".to_vec(), collision);
            }
        }
        synchronize_runtime_sky_reflection_bundle(archive, &mut edits)?;
        Ok(edits)
    }

    pub(super) fn visible_model_assets(&self) -> Vec<&CatalogAssetEntry> {
        let filter = self.model_asset_filter.trim().to_ascii_lowercase();
        self.model_catalog_entries
            .iter()
            .filter(|entry| {
                self.model_folder_filter.as_ref().is_none_or(|folder| {
                    entry
                        .relative_path
                        .parent()
                        .is_some_and(|parent| parent == Path::new(folder))
                })
            })
            .filter(|entry| {
                filter.is_empty()
                    || entry.name.to_ascii_lowercase().contains(&filter)
                    || entry
                        .relative_path
                        .to_string_lossy()
                        .to_ascii_lowercase()
                        .contains(&filter)
            })
            .collect()
    }

    pub(super) fn model_asset_folders(&self) -> BTreeSet<String> {
        self.model_catalog_entries
            .iter()
            .filter_map(|entry| entry.relative_path.parent())
            .filter(|parent| !parent.as_os_str().is_empty())
            .map(|parent| parent.to_string_lossy().replace('\\', "/"))
            .collect()
    }

    pub(super) fn model_content_browser_panel(&mut self, ui: &mut egui::Ui) {
        self.refresh_model_catalog();
        ui.horizontal(|ui| {
            ui.heading("Model Assets");
            if ui
                .add_enabled(
                    self.model_import_job.is_none() && self.background_receiver.is_none(),
                    egui::Button::new("Import glTF/GLB"),
                )
                .on_hover_text("Import source geometry into a managed .smsmodel asset")
                .clicked()
            {
                self.begin_model_import();
            }
            if self.model_import_job.is_some() {
                ui.spinner();
                ui.label("Importing and validating...");
                if ui.button("Cancel").clicked() {
                    self.cancel_model_import();
                }
            }
            if ui.small_button("Refresh").clicked() {
                self.force_refresh_model_catalog();
            }
            ui.separator();
            ui.label("Search");
            ui.add(
                egui::TextEdit::singleline(&mut self.model_asset_filter)
                    .desired_width(220.0)
                    .hint_text("Name, folder, or .smsmodel"),
            );
        });
        if !self.model_catalog_issues.is_empty() {
            for issue in &self.model_catalog_issues {
                ui.colored_label(egui::Color32::from_rgb(235, 190, 92), issue);
            }
        }
        egui::CollapsingHeader::new("Import Settings")
            .id_salt("model-import-settings")
            .show(ui, |ui| self.model_import_options_panel(ui));
        ui.separator();

        let folders = self.model_asset_folders();
        let assets = self
            .visible_model_assets()
            .into_iter()
            .cloned()
            .collect::<Vec<_>>();
        let mut select = None;
        let mut place_at_focus = None;
        ui.columns(2, |columns| {
            columns[0].set_min_width(150.0);
            columns[0].set_max_width(220.0);
            columns[0].heading("Folders");
            if columns[0]
                .selectable_label(self.model_folder_filter.is_none(), "Content")
                .clicked()
            {
                self.model_folder_filter = None;
            }
            for folder in folders {
                let selected = self.model_folder_filter.as_deref() == Some(folder.as_str());
                if columns[0]
                    .selectable_label(selected, format!("  {folder}"))
                    .clicked()
                {
                    self.model_folder_filter = Some(folder);
                }
            }
            columns[0].separator();
            columns[0].add(
                egui::TextEdit::singleline(&mut self.new_model_folder_draft)
                    .hint_text("New folder"),
            );
            if columns[0]
                .add_enabled(
                    !self.new_model_folder_draft.trim().is_empty()
                        && self.background_receiver.is_none(),
                    egui::Button::new("Create Folder"),
                )
                .clicked()
            {
                self.create_model_folder();
            }

            columns[1].horizontal(|ui| {
                ui.small(format!(
                    "{} asset{}",
                    assets.len(),
                    if assets.len() == 1 { "" } else { "s" }
                ));
                if self.selected_model_document.is_some()
                    && ui
                        .add_enabled(
                            self.asset_dirty && self.background_receiver.is_none(),
                            egui::Button::new("Save Asset"),
                        )
                        .clicked()
                {
                    self.save_selected_model_asset();
                }
            });
            egui::ScrollArea::vertical().show(&mut columns[1], |ui| {
                let layout = content_browser_layout(ui.available_width(), assets.len());
                egui::Grid::new("model-content-browser-grid")
                    .num_columns(layout.columns)
                    .min_col_width(layout.card_width)
                    .max_col_width(layout.card_width)
                    .spacing(egui::vec2(8.0, 8.0))
                    .show(ui, |ui| {
                        for (index, entry) in assets.iter().enumerate() {
                            let selected = self.selected_model_asset == Some(entry.id);
                            let response = model_asset_thumbnail_card(
                                ui,
                                egui::vec2(layout.card_width, 92.0),
                                selected,
                                entry,
                            )
                                .on_hover_text(format!(
                                    "Content/{}\nDrag onto the viewport to place a typed model instance.",
                                    entry.relative_path.display()
                                ));
                            response.dnd_set_drag_payload(ModelAssetDragPayload {
                                asset_id: entry.id,
                            });
                            if response.clicked() {
                                select = Some(entry.id);
                            }
                            response.context_menu(|ui| {
                                if ui.button("Place in Viewport").clicked() {
                                    place_at_focus = Some(entry.id);
                                    ui.close();
                                }
                            });
                            if (index + 1) % layout.columns == 0 {
                                ui.end_row();
                            }
                        }
                    });
                if assets.is_empty() {
                    ui.add_space(16.0);
                    ui.vertical_centered(|ui| {
                        ui.label("No model assets match this folder and search.");
                        ui.small("Import a project-authored .gltf or .glb to begin.");
                    });
                }
            });
        });
        if let Some(id) = select {
            self.select_model_asset(id);
        }
        if let Some(id) = place_at_focus {
            self.spawn_model_instance_at(id, self.default_spawn_position());
        }
    }

    pub(super) fn model_import_options_panel(&mut self, ui: &mut egui::Ui) {
        let collision_surfaces = self
            .registry
            .as_ref()
            .map(|registry| registry.collision_surfaces.clone())
            .unwrap_or_default();
        ui.horizontal(|ui| {
            ui.label("Sunshine units per meter");
            ui.add(
                egui::DragValue::new(
                    &mut self
                        .model_import_options
                        .coordinate_conversion
                        .units_per_meter,
                )
                .range(0.0001..=1_000_000.0)
                .speed(1.0),
            );
            ui.checkbox(
                &mut self
                    .model_import_options
                    .coordinate_conversion
                    .reverse_winding,
                "Correct winding",
            );
        });
        ui.small("Default: glTF/Blender Y-up identity basis, 100 Sunshine units per meter.");
        ui.label("Axis basis (rows)");
        for row in 0..3 {
            ui.horizontal(|ui| {
                for column in 0..3 {
                    ui.add(
                        egui::DragValue::new(
                            &mut self.model_import_options.coordinate_conversion.basis[row][column],
                        )
                        .speed(0.05),
                    );
                }
            });
        }
        let mut collision_mode = match self.model_import_options.collision {
            CollisionSource::None => 0,
            CollisionSource::RenderGeometry { .. } => 1,
            CollisionSource::EmbeddedNodes { .. } => 2,
            CollisionSource::SeparateFile { .. } => 3,
        };
        let before = collision_mode;
        egui::ComboBox::from_label("Collision source")
            .selected_text(match collision_mode {
                0 => "None",
                1 => "Render geometry",
                2 => "COL_ / selected nodes",
                _ => "Separate glTF/GLB",
            })
            .show_ui(ui, |ui| {
                ui.selectable_value(&mut collision_mode, 0, "None");
                ui.selectable_value(&mut collision_mode, 1, "Render geometry");
                ui.selectable_value(&mut collision_mode, 2, "COL_ / selected nodes");
                ui.selectable_value(&mut collision_mode, 3, "Separate glTF/GLB");
            });
        if collision_mode != before {
            self.model_import_options.collision = match collision_mode {
                0 => CollisionSource::None,
                1 => CollisionSource::RenderGeometry {
                    surface: CollisionSurface::default(),
                },
                2 => CollisionSource::EmbeddedNodes {
                    prefix: "COL_".to_string(),
                    selected_nodes: BTreeSet::new(),
                    surfaces_by_node: Default::default(),
                    default_surface: CollisionSurface::default(),
                },
                _ => CollisionSource::SeparateFile {
                    path: PathBuf::new(),
                    options: Default::default(),
                },
            };
        }
        match &mut self.model_import_options.collision {
            CollisionSource::RenderGeometry { surface } => {
                collision_surface_controls(ui, surface, &collision_surfaces)
            }
            CollisionSource::EmbeddedNodes {
                prefix,
                default_surface,
                ..
            } => {
                ui.horizontal(|ui| {
                    ui.label("Node prefix");
                    ui.text_edit_singleline(prefix);
                });
                collision_surface_controls(ui, default_surface, &collision_surfaces);
            }
            CollisionSource::SeparateFile { path, .. } => {
                ui.horizontal(|ui| {
                    let path_label = if path.as_os_str().is_empty() {
                        "No separate collision source selected".to_string()
                    } else {
                        path.to_string_lossy().into_owned()
                    };
                    ui.label(path_label);
                    if ui.button("Choose Collision glTF/GLB").clicked() {
                        if let Some(selected) = rfd::FileDialog::new()
                            .set_title("Choose Separate Collision Model")
                            .add_filter("glTF model", &["gltf", "glb"])
                            .pick_file()
                        {
                            *path = selected;
                        }
                    }
                });
            }
            CollisionSource::None => {}
        }
        let mut simplify = self.model_import_options.collision_simplification.is_some();
        if ui
            .checkbox(&mut simplify, "Deterministic QEM simplification")
            .on_hover_text("Off by default; exact-coordinate cleanup always runs")
            .changed()
        {
            self.model_import_options.collision_simplification =
                simplify.then_some(CollisionSimplificationOptions {
                    target_ratio: 0.5,
                    max_error: 1.0,
                });
        }
        if let Some(options) = &mut self.model_import_options.collision_simplification {
            ui.add(egui::Slider::new(&mut options.target_ratio, 0.01..=1.0).text("Target ratio"));
            ui.add(
                egui::DragValue::new(&mut options.max_error)
                    .range(0.0..=f32::MAX)
                    .speed(0.1)
                    .prefix("Max error "),
            );
        }
    }

    pub(super) fn model_asset_inspector_panel(&mut self, ui: &mut egui::Ui) {
        let Some(document) = self.selected_model_document.as_ref() else {
            return;
        };
        ui.horizontal(|ui| {
            ui.heading(&document.name);
            if self.asset_dirty {
                ui.colored_label(egui::Color32::from_rgb(245, 190, 90), "modified");
            }
        });
        if let Some(id) = self.selected_model_asset {
            ui.small(format!("Asset UUID: {id}"));
        }
        ui.horizontal_wrapped(|ui| {
            for (section, label) in [
                (ModelEditorSection::Hierarchy, "Hierarchy"),
                (ModelEditorSection::Materials, "Materials / GX"),
                (ModelEditorSection::Textures, "Textures"),
                (ModelEditorSection::Collision, "Collision"),
                (ModelEditorSection::Diagnostics, "Diagnostics"),
            ] {
                ui.selectable_value(&mut self.model_editor_section, section, label);
            }
        });
        ui.separator();
        if let Some(error) = &self.model_editor_error {
            ui.colored_label(egui::Color32::from_rgb(255, 116, 104), error);
            ui.separator();
        }

        match self.model_editor_section {
            ModelEditorSection::Hierarchy => self.model_hierarchy_inspector(ui),
            ModelEditorSection::Materials => self.model_material_inspector(ui),
            ModelEditorSection::Textures => self.model_texture_inspector(ui),
            ModelEditorSection::Collision => self.model_collision_inspector(ui),
            ModelEditorSection::Diagnostics => self.model_diagnostics_inspector(ui),
        }
        ui.separator();
        egui::CollapsingHeader::new("Asset Management").show(ui, |ui| {
            let catalog_writable = self.background_receiver.is_none();
            ui.horizontal(|ui| {
                ui.label("Name");
                ui.text_edit_singleline(&mut self.model_asset_rename_draft);
                if ui
                    .add_enabled(catalog_writable, egui::Button::new("Rename"))
                    .clicked()
                {
                    self.rename_selected_model_asset();
                }
            });
            ui.horizontal(|ui| {
                ui.label("Folder");
                ui.text_edit_singleline(&mut self.model_asset_move_draft);
                if ui
                    .add_enabled(catalog_writable, egui::Button::new("Move"))
                    .clicked()
                {
                    self.move_selected_model_asset();
                }
            });
            ui.horizontal(|ui| {
                if ui
                    .add_enabled(catalog_writable, egui::Button::new("Duplicate"))
                    .clicked()
                {
                    self.duplicate_selected_model_asset();
                }
                if ui
                    .add_enabled(catalog_writable, egui::Button::new("Reference-safe Delete"))
                    .clicked()
                {
                    self.delete_selected_model_asset();
                }
            });
            if !catalog_writable {
                ui.small(
                    "Content changes are locked while the background build reads its snapshot.",
                );
            }
            ui.small("Delete is rejected while any typed viewport instance references this UUID.");
        });
        ui.separator();
        ui.horizontal(|ui| {
            if ui.button("Place").clicked() {
                if let Some(id) = self.selected_model_asset {
                    self.arm_model_placement(id);
                }
            }
            if ui
                .add_enabled(
                    self.asset_dirty && self.background_receiver.is_none(),
                    egui::Button::new("Save Asset"),
                )
                .clicked()
            {
                self.save_selected_model_asset();
            }
        });
    }

    fn model_hierarchy_inspector(&mut self, ui: &mut egui::Ui) {
        let Some(document) = self.selected_model_document.as_ref() else {
            return;
        };
        ui.label(format!(
            "{} node(s), {} mesh(es)",
            document.nodes.len(),
            document.meshes.len()
        ));
        if let Some([minimum, maximum]) = model_document_bounds(document) {
            ui.small(format!(
                "Bounds min [{:.2}, {:.2}, {:.2}] / max [{:.2}, {:.2}, {:.2}] Sunshine units",
                minimum[0], minimum[1], minimum[2], maximum[0], maximum[1], maximum[2]
            ));
        }
        for (index, node) in document.nodes.iter().enumerate() {
            let depth = node_depth(&document.nodes, index);
            ui.horizontal(|ui| {
                ui.add_space(depth as f32 * 12.0);
                ui.label(if node.children.is_empty() { "-" } else { "+" });
                ui.label(&node.name);
                ui.small(format!(
                    "{:?}{}",
                    node.purpose,
                    node.mesh
                        .map_or_else(String::new, |mesh| format!(" / mesh {mesh}"))
                ));
            });
            ui.indent(("node-transform", index), |ui| {
                ui.small(format!(
                    "T [{:.2}, {:.2}, {:.2}]",
                    node.local_transform[3][0],
                    node.local_transform[3][1],
                    node.local_transform[3][2]
                ));
            });
        }
        ui.separator();
        for (mesh_index, mesh) in document.meshes.iter().enumerate() {
            egui::CollapsingHeader::new(format!("Mesh {mesh_index}: {}", mesh.name)).show(
                ui,
                |ui| {
                    for (primitive_index, primitive) in mesh.primitives.iter().enumerate() {
                        ui.label(format!(
                            "Primitive {primitive_index}: {} vertices, {} triangles, {} UV set(s), {} color set(s), material {}",
                            primitive.positions.len(),
                            primitive.indices.len() / 3,
                            primitive.tex_coords.len(),
                            primitive.colors.len(),
                            primitive.material.map_or_else(|| "default".to_string(), |value| value.to_string())
                        ));
                    }
                },
            );
        }
    }

    fn model_material_inspector(&mut self, ui: &mut egui::Ui) {
        let material_names = self
            .selected_model_document
            .as_ref()
            .map(|document| {
                document
                    .materials
                    .iter()
                    .map(|material| material.gx.name.clone())
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        if material_names.is_empty() {
            ui.label("This asset has no authored materials.");
            return;
        }
        self.selected_model_material = self
            .selected_model_material
            .min(material_names.len().saturating_sub(1));
        let old_index = self.selected_model_material;
        egui::ComboBox::from_label("Material")
            .selected_text(&material_names[self.selected_model_material])
            .show_ui(ui, |ui| {
                for (index, name) in material_names.iter().enumerate() {
                    ui.selectable_value(&mut self.selected_model_material, index, name);
                }
            });
        if old_index != self.selected_model_material {
            self.sync_gx_json_draft();
        }
        let material_index = self.selected_model_material;
        let Some(material) = self
            .selected_model_document
            .as_ref()
            .and_then(|document| document.materials.get(material_index))
            .cloned()
        else {
            return;
        };
        ui.label("Conservative glTF mapping");
        ui.small(format!(
            "Base [{:.3}, {:.3}, {:.3}, {:.3}] / {:?} / {}",
            material.source_base_color[0],
            material.source_base_color[1],
            material.source_base_color[2],
            material.source_base_color[3],
            material.source_alpha_mode,
            if material.source_double_sided {
                "double-sided"
            } else {
                "culled"
            }
        ));
        ui.small(format!(
            "TEV {} stage(s), {} texgen(s), {} color channel(s), cull {}, mode {}, blend {}, Z write {}",
            material.gx.tev_stage_count,
            material.gx.tex_gen_count,
            material.gx.color_channel_count,
            material.gx.cull_mode,
            material.gx.material_mode,
            material.gx.blend_mode.mode,
            material.gx.depth_mode.update_enabled
        ));
        if matches!(
            material.source_alpha_mode,
            sms_authoring::ImportedAlphaMode::Opaque
        ) && (material.gx.material_mode & 4 != 0
            || material.gx.blend_mode.mode != 0
            || material.gx.depth_mode.update_enabled == 0
            || (!material.source_double_sided && material.gx.cull_mode == 0))
        {
            ui.colored_label(
                egui::Color32::from_rgb(244, 142, 105),
                "Source glTF is OPAQUE, but GX transparency or disabled Z writes can expose rear polygons. Use Opaque (Back Cull + Z Write) unless transparency is intentional.",
            );
        }
        ui.horizontal_wrapped(|ui| {
            if ui
                .button("Lit (Diffuse + Specular)")
                .on_hover_text(
                    "Use Sunshine LIGHT0 for diffuse shading and LIGHT2 for a secondary specular highlight",
                )
                .clicked()
            {
                let mut candidate = material.clone();
                if apply_conservative_diffuse_specular_preset(&mut candidate) {
                    self.mutate_model_asset(
                        "Applied conservative diffuse and specular preset",
                        move |document| {
                            if let Some(material) = document.materials.get_mut(material_index) {
                                let _ = apply_conservative_diffuse_specular_preset(material);
                            }
                        },
                    );
                } else {
                    self.model_editor_error = Some(
                        "The diffuse + specular preset only applies to the canonical conservative glTF TEV program. Revert advanced TEV edits before applying it."
                            .to_string(),
                    );
                }
            }
            if ui
                .button("Unlit (No GX Lights)")
                .on_hover_text(
                    "Disable GX color-channel lighting; an exact preset-authored specular stage is removed safely",
                )
                .clicked()
            {
                self.mutate_model_asset("Applied unlit preset", move |document| {
                    if let Some(material) = document.materials.get_mut(material_index) {
                        apply_conservative_unlit_preset(material);
                    }
                });
            }
            if ui
                .button("Opaque (Back Cull + Z Write)")
                .on_hover_text(
                    "Restore solid rendering: back-face culling, no blending, depth testing, and depth writes",
                )
                .clicked()
            {
                self.mutate_model_asset("Applied opaque material preset", move |document| {
                    if let Some(material) = document.materials.get_mut(material_index) {
                        apply_opaque_material_preset(&mut material.gx);
                    }
                });
            }
            if ui
                .button("Two Sided (No Cull)")
                .on_hover_text("Render front and back faces; transparency and depth state are unchanged")
                .clicked()
            {
                self.mutate_model_asset("Enabled two-sided rendering", move |document| {
                    if let Some(material) = document.materials.get_mut(material_index) {
                        material.gx.cull_mode = 0;
                    }
                });
            }
            if ui
                .button("Alpha Blend (No Z Write)")
                .on_hover_text(
                    "Use source-alpha blending and disable depth writes; culling is unchanged",
                )
                .clicked()
            {
                self.mutate_model_asset("Applied alpha blend preset", move |document| {
                    if let Some(material) = document.materials.get_mut(material_index) {
                        apply_alpha_blend_material_preset(&mut material.gx);
                    }
                });
            }
        });
        ui.separator();
        ui.label("Complete GX/MAT3 state");
        ui.small(
            "Every MAT3 field is represented below as version-stable JSON. Apply validates the full state before it can be saved or compiled.",
        );
        ui.add(
            egui::TextEdit::multiline(&mut self.gx_json_draft)
                .code_editor()
                .desired_rows(22)
                .desired_width(f32::INFINITY),
        );
        ui.horizontal(|ui| {
            if ui.button("Apply Complete GX State").clicked() {
                self.apply_gx_json_draft();
            }
            if ui.button("Revert Draft").clicked() {
                self.sync_gx_json_draft();
            }
        });
    }

    fn model_texture_inspector(&mut self, ui: &mut egui::Ui) {
        let textures = self
            .selected_model_document
            .as_ref()
            .map(|document| document.textures.clone())
            .unwrap_or_default();
        if textures.is_empty() {
            ui.label("This asset has no textures.");
            return;
        }
        self.selected_model_texture = self
            .selected_model_texture
            .min(textures.len().saturating_sub(1));
        let old_texture_index = self.selected_model_texture;
        egui::ComboBox::from_label("Texture")
            .selected_text(&textures[self.selected_model_texture].name)
            .show_ui(ui, |ui| {
                for (index, texture) in textures.iter().enumerate() {
                    ui.selectable_value(&mut self.selected_model_texture, index, &texture.name);
                }
            });
        if old_texture_index != self.selected_model_texture {
            self.sync_texture_json_draft();
        }
        let texture = &textures[self.selected_model_texture];
        ui.label(format!(
            "{} x {} RGBA8 source",
            texture.width, texture.height
        ));
        ui.label(format!("Encoding: {:?}", texture.encode_options.encoding));
        ui.label(format!(
            "Palette: {:?}; mip count: {}",
            texture.encode_options.palette_format, texture.encode_options.mip_count
        ));
        let sampler = texture.encode_options.sampler;
        ui.small(format!(
            "Sampler wrap S/T {}/{}; min/mag {}/{}; LOD {}..{} bias {}; anisotropy {}",
            sampler.wrap_s,
            sampler.wrap_t,
            sampler.min_filter,
            sampler.mag_filter,
            sampler.min_lod,
            sampler.max_lod,
            sampler.lod_bias,
            sampler.max_anisotropy
        ));
        egui::CollapsingHeader::new("Complete TEX1 encode state")
            .default_open(true)
            .show(ui, |ui| {
                ui.add(
                    egui::TextEdit::multiline(&mut self.texture_json_draft)
                        .code_editor()
                        .desired_width(f32::INFINITY)
                        .desired_rows(14),
                );
                ui.horizontal(|ui| {
                    if ui.button("Apply TEX1 State").clicked() {
                        self.apply_texture_json_draft();
                    }
                    if ui.button("Revert Draft").clicked() {
                        self.sync_texture_json_draft();
                    }
                });
            });
    }

    fn model_collision_inspector(&mut self, ui: &mut egui::Ui) {
        let collision_surfaces = self
            .registry
            .as_ref()
            .map(|registry| registry.collision_surfaces.clone())
            .unwrap_or_default();
        let Some(collision) = self
            .selected_model_document
            .as_ref()
            .and_then(|document| document.collision.as_ref())
            .cloned()
        else {
            ui.label("Collision is disabled for this model asset.");
            return;
        };
        ui.label(format!(
            "{} welded vertices / {} group(s) / {} triangles",
            collision.vertices.len(),
            collision.groups.len(),
            collision
                .groups
                .iter()
                .map(|group| group.triangles.len())
                .sum::<usize>()
        ));
        for (index, group) in collision.groups.iter().enumerate() {
            let mut name = group.name.clone();
            let mut surface = group.surface.clone();
            let mut changed = false;
            egui::CollapsingHeader::new(format!("{} ({})", group.name, group.triangles.len()))
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.label("Name");
                        changed |= ui.text_edit_singleline(&mut name).changed();
                    });
                    ui.horizontal(|ui| {
                        ui.label("Surface");
                        changed |= ui
                            .add(
                                egui::DragValue::new(&mut surface.surface_type)
                                    .hexadecimal(4, false, true),
                            )
                            .changed();
                    });
                    ui.small(collision_surface_description(
                        surface.surface_type,
                        &collision_surfaces,
                    ));
                    ui.horizontal(|ui| {
                        ui.label("Attributes");
                        changed |= ui
                            .add(egui::DragValue::new(&mut surface.attribute_0))
                            .changed();
                        changed |= ui
                            .add(egui::DragValue::new(&mut surface.attribute_1))
                            .changed();
                    });
                    if ui.button("Use as instance override template").clicked() {
                        self.model_collision_override_template = Some(surface.clone());
                    }
                    ui.small(format!(
                        "Group index {index}; raw numeric values are preserved."
                    ));
                });
            if changed {
                self.mutate_model_asset("Edited collision group", move |document| {
                    if let Some(group) = document
                        .collision
                        .as_mut()
                        .and_then(|collision| collision.groups.get_mut(index))
                    {
                        group.name = name;
                        group.surface = surface;
                    }
                });
            }
        }
    }

    fn model_diagnostics_inspector(&mut self, ui: &mut egui::Ui) {
        egui::ComboBox::from_label("Target loader")
            .selected_text(self.model_target_profile.label())
            .show_ui(ui, |ui| {
                for profile in [
                    ModelTargetProfile::Full,
                    ModelTargetProfile::SunshineMap,
                    ModelTargetProfile::SunshineObject,
                    ModelTargetProfile::SunshinePollution,
                ] {
                    ui.selectable_value(&mut self.model_target_profile, profile, profile.label());
                }
            });
        ui.small(format!(
            "Loader flags: {:#010x}",
            self.model_target_profile.authoring_profile().flags()
        ));
        let diagnostics = self
            .selected_model_document
            .as_ref()
            .map(|document| document.diagnostics.clone())
            .unwrap_or_default();
        if diagnostics.is_empty() {
            ui.colored_label(
                egui::Color32::from_rgb(111, 220, 168),
                "No import diagnostics",
            );
        }
        for diagnostic in &diagnostics {
            let color = match diagnostic.severity {
                sms_authoring::Severity::Info => egui::Color32::from_rgb(150, 180, 220),
                sms_authoring::Severity::Warning => egui::Color32::from_rgb(235, 190, 92),
                sms_authoring::Severity::Error => egui::Color32::from_rgb(255, 116, 104),
            };
            ui.colored_label(
                color,
                format!("{:?} [{:?}]", diagnostic.severity, diagnostic.code),
            );
            ui.label(&diagnostic.message);
            if diagnostic.acknowledgement_required {
                let code = diagnostic.code;
                let mut acknowledged = self
                    .selected_model_document
                    .as_ref()
                    .is_some_and(|document| document.acknowledged_diagnostics.contains(&code));
                if ui
                    .checkbox(
                        &mut acknowledged,
                        "I acknowledge this unmapped source input",
                    )
                    .changed()
                {
                    self.mutate_model_asset("Reviewed import diagnostic", move |document| {
                        if acknowledged {
                            document.acknowledged_diagnostics.insert(code);
                        } else {
                            document.acknowledged_diagnostics.remove(&code);
                        }
                    });
                }
            }
            ui.separator();
        }
        let target = self.model_target_diagnostics();
        if target.is_empty() {
            ui.colored_label(
                egui::Color32::from_rgb(111, 220, 168),
                "Selected loader consumes the authored GX state.",
            );
        } else {
            ui.heading("Target compatibility");
            for diagnostic in target {
                ui.colored_label(egui::Color32::from_rgb(235, 190, 92), diagnostic);
            }
        }
    }

    pub(super) fn model_instance_inspector_panel(&mut self, ui: &mut egui::Ui) {
        let collision_surfaces = self
            .registry
            .as_ref()
            .map(|registry| registry.collision_surfaces.clone())
            .unwrap_or_default();
        let stock_scene_record_names = self
            .document
            .as_ref()
            .and_then(|document| {
                document
                    .stage_archive
                    .as_ref()
                    .map(|archive| (document, archive))
            })
            .and_then(|(document, archive)| {
                stage_scene_record_names(archive, &document.archive_edits).ok()
            });
        let stock_slot_candidates = self
            .registry
            .as_ref()
            .map(|registry| {
                registry
                    .map_obj_resources
                    .iter()
                    .filter(|slot| {
                        slot.object_flags & 0x80 == 0
                            && !slot.has_hold_dependency
                            && !slot.has_move_dependency
                            && slot.uses_resource_name_model_fallback
                            && slot.primary_model.as_deref().is_some_and(|model| {
                                stock_mapobj_archive_path(model, ".bmd").is_ok()
                            })
                    })
                    .map(|slot| {
                        let manager_available = !slot.required_manager_name.is_empty()
                            && stock_scene_record_names
                                .as_ref()
                                .is_some_and(|names| names.contains(&slot.required_manager_name));
                        (
                            slot.resource_name.clone(),
                            slot.load_flags,
                            slot.required_manager_name.clone(),
                            manager_available,
                        )
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let Some(mut instance) = self.selected_model_instance().cloned() else {
            self.selected_model_instance_id = None;
            return;
        };
        ui.heading(&instance.placement.name);
        ui.small(format!(
            "Typed model instance: {}",
            instance.placement.instance_id
        ));
        ui.small(format!("Asset UUID: {}", instance.placement.asset_id));
        ui.separator();
        let mut transform = matrix_to_transform(instance.placement.transform);
        let mut changed = false;
        ui.label("Translation");
        changed |= model_vector_drag(ui, &mut transform.translation, 1.0);
        ui.label("Rotation");
        changed |= model_vector_drag(ui, &mut transform.rotation_degrees, 0.5);
        ui.label("Scale");
        changed |= model_vector_drag(ui, &mut transform.scale, 0.01);
        ui.label("Stage export");
        egui::ComboBox::from_id_salt((
            "model_instance_export_mode",
            instance.placement.instance_id,
        ))
        .selected_text(match instance.placement.export_mode {
            ModelInstanceExportMode::SeparateRuntimeObject => "Separate runtime object",
            ModelInstanceExportMode::StockMapObjBase => "Stock MapObjBase slot",
            ModelInstanceExportMode::MapTerrain => "Bake as map terrain",
            ModelInstanceExportMode::Skybox => "Stage skybox",
        })
        .show_ui(ui, |ui| {
            changed |= ui
                .selectable_value(
                    &mut instance.placement.export_mode,
                    ModelInstanceExportMode::SeparateRuntimeObject,
                    "Separate runtime object (recommended)",
                )
                .on_hover_text(
                    "Creates a standalone authored BMD resource and a typed SmJ3DAct placement. The terrain BMD stays untouched; enabled static collision is appended to map collision.",
                )
                .changed();
            changed |= ui
                .selectable_value(
                    &mut instance.placement.export_mode,
                    ModelInstanceExportMode::StockMapObjBase,
                    "Replace verified stock MapObjBase",
                )
                .on_hover_text(
                    "Requires a decomp-verified compiled stock resource slot and compatible loader limits.",
                )
                .changed();
            changed |= ui
                .selectable_value(
                    &mut instance.placement.export_mode,
                    ModelInstanceExportMode::MapTerrain,
                    "Bake as map terrain (destructive)",
                )
                .on_hover_text(
                    "Replaces map/map/map.bmd with authored instances. Use only when intentionally replacing the stage terrain model.",
                )
                .changed();
            changed |= ui
                .selectable_value(
                    &mut instance.placement.export_mode,
                    ModelInstanceExportMode::Skybox,
                    "Stage skybox",
                )
                .on_hover_text(
                    "Authors map/map/sky.bmd and ensures the scene has the typed TSky actor that consumes it.",
                )
                .changed();
        });
        match instance.placement.export_mode {
            ModelInstanceExportMode::SeparateRuntimeObject => {
                ui.small(
                    "Exports a deduplicated standalone BMD. Static collision remains part of map/map.col.",
                );
            }
            ModelInstanceExportMode::StockMapObjBase => {
                ui.colored_label(
                    egui::Color32::from_rgb(235, 190, 92),
                    "Shared/global replacement: every use of the selected retail BMD/COL slot in this stage resolves to the authored asset.",
                );
                ui.horizontal(|ui| {
                    ui.label("Stock slot");
                    let selected = if instance.placement.stock_map_obj_resource.is_empty() {
                        "Select a verified slot...".to_string()
                    } else {
                        instance.placement.stock_map_obj_resource.clone()
                    };
                    egui::ComboBox::from_id_salt((
                        "stock_map_obj_resource",
                        instance.placement.instance_id,
                    ))
                    .selected_text(selected)
                    .show_ui(ui, |ui| {
                        for (resource_name, load_flags, manager_name, manager_available) in
                            &stock_slot_candidates
                        {
                            if !manager_available {
                                continue;
                            }
                            changed |= ui
                                .selectable_value(
                                    &mut instance.placement.stock_map_obj_resource,
                                    resource_name.clone(),
                                    format!(
                                        "{resource_name} ({load_flags:#010x}; manager {manager_name})"
                                    ),
                                )
                                .changed();
                        }
                    });
                });
                if stock_slot_candidates.is_empty() {
                    ui.colored_label(
                        egui::Color32::from_rgb(244, 142, 105),
                        "No compatible source-free stock slots are available in the loaded decomp registry.",
                    );
                } else if !stock_slot_candidates
                    .iter()
                    .any(|(_, _, _, manager_available)| *manager_available)
                {
                    ui.colored_label(
                        egui::Color32::from_rgb(244, 142, 105),
                        "This scene contains none of the exact managers required by the otherwise compatible stock slots.",
                    );
                } else if !instance.placement.stock_map_obj_resource.is_empty()
                    && !stock_slot_candidates.iter().any(
                        |(resource_name, _, _, manager_available)| {
                            *manager_available
                                && resource_name == &instance.placement.stock_map_obj_resource
                        },
                    )
                {
                    if let Some((_, _, required_manager, false)) = stock_slot_candidates
                        .iter()
                        .find(|(resource_name, _, _, _)| {
                            resource_name == &instance.placement.stock_map_obj_resource
                        })
                    {
                        ui.colored_label(
                            egui::Color32::from_rgb(244, 142, 105),
                            format!(
                                "The saved slot requires scene manager {required_manager:?}, which is not present under the open map/scene.bin hierarchy."
                            ),
                        );
                    } else {
                        ui.colored_label(
                            egui::Color32::from_rgb(244, 142, 105),
                            "The saved slot is not compatible with generic stock replacement; select another verified slot.",
                        );
                    }
                }
            }
            ModelInstanceExportMode::MapTerrain => {
                ui.colored_label(
                    egui::Color32::from_rgb(244, 142, 105),
                    "Destructive export: replaces map/map/map.bmd rather than adding a runtime object.",
                );
            }
            ModelInstanceExportMode::Skybox => {
                ui.small(
                    "Authors this model as the stage-global TSky resource. Collision is not exported for skybox instances.",
                );
            }
        }
        ui.separator();
        ui.label("Runtime loader compatibility");
        match model_instance_loader_flags(&instance.placement, self.registry.as_ref()) {
            Ok(loader_flags) => {
                ui.small(format!("Loader flags: {loader_flags:#010x}"));
                let key = AuthoredModelPreviewKey {
                    asset_id: instance.placement.asset_id,
                    loader_flags,
                };
                match self.model_asset_preview_cache.get(&key) {
                    Some(geometry) if geometry.loader_diagnostics.is_empty() => {
                        ui.colored_label(
                            egui::Color32::from_rgb(111, 220, 168),
                            "This loader consumes the authored GX state.",
                        );
                    }
                    Some(geometry) => {
                        for diagnostic in &geometry.loader_diagnostics {
                            let color = match diagnostic.severity {
                                GxDiagnosticSeverity::Warning => {
                                    egui::Color32::from_rgb(235, 190, 92)
                                }
                                GxDiagnosticSeverity::Error => {
                                    egui::Color32::from_rgb(255, 116, 104)
                                }
                            };
                            ui.colored_label(
                                color,
                                format!(
                                    "{:?} [{}] material {}: {}",
                                    diagnostic.severity,
                                    diagnostic.code,
                                    diagnostic.material_index,
                                    diagnostic.message
                                ),
                            );
                        }
                    }
                    None => {
                        ui.colored_label(
                            egui::Color32::from_rgb(235, 190, 92),
                            "The mode-specific preview is rebuilding; export will revalidate this asset.",
                        );
                    }
                }
            }
            Err(error) => {
                ui.colored_label(egui::Color32::from_rgb(255, 116, 104), error);
            }
        }
        changed |= ui
            .checkbox(
                &mut instance.placement.collision_enabled,
                "Collision enabled",
            )
            .changed();
        if instance.placement.export_mode == ModelInstanceExportMode::StockMapObjBase
            && !instance.placement.collision_enabled
        {
            ui.colored_label(
                egui::Color32::from_rgb(244, 142, 105),
                "Global stock-slot effect: this writes an empty replacement for every decomp-listed COL resource used by the selected slot, disabling collision for all of its uses in this stage.",
            );
        }
        if instance.placement.collision_enabled {
            let mut override_enabled = instance.placement.collision_surface_override.is_some();
            if ui
                .checkbox(&mut override_enabled, "Override collision surface")
                .changed()
            {
                instance.placement.collision_surface_override = override_enabled.then(|| {
                    self.model_collision_override_template
                        .clone()
                        .unwrap_or_default()
                });
                changed = true;
            }
            if let Some(surface) = &mut instance.placement.collision_surface_override {
                ui.horizontal(|ui| {
                    ui.label("Surface");
                    changed |= ui
                        .add(
                            egui::DragValue::new(&mut surface.surface_type)
                                .hexadecimal(4, false, true),
                        )
                        .changed();
                });
                ui.small(collision_surface_description(
                    surface.surface_type,
                    &collision_surfaces,
                ));
                ui.horizontal(|ui| {
                    ui.label("Attributes");
                    changed |= ui
                        .add(egui::DragValue::new(&mut surface.attribute_0))
                        .changed();
                    changed |= ui
                        .add(egui::DragValue::new(&mut surface.attribute_1))
                        .changed();
                });
            }
        }
        if changed && transform.is_finite() {
            instance.placement.transform = transform_to_matrix(transform);
            self.update_selected_model_instance(instance.clone());
        }
        ui.separator();
        ui.horizontal(|ui| {
            if ui.button("Select Asset").clicked() {
                let id = instance.placement.asset_id;
                self.select_model_asset(id);
            }
            if ui.button("Delete Instance").clicked() {
                self.delete_selected_model_instance();
            }
        });
    }
}

fn model_runtime_resource_key(asset_id: AssetId) -> String {
    format!("sms_{}", asset_id.as_uuid().simple())
}

const SMS_RUNTIME_MAP_GROUP_TYPE: &str = "IdxGroup";
const SMS_RUNTIME_MAP_GROUP_NAME: &str = "マップグループ";
const SMS_RUNTIME_MAP_GROUP_INDEX: u32 = 0;
const SMS_RUNTIME_MAP_GROUP_OWNER_TYPE: &str = "Strategy";

fn runtime_actor_parent_path(archive: &SourceFreeStageArchive) -> Result<Vec<usize>, String> {
    archive
        .find_unique_owned_indexed_group_record_path(
            b"map/scene.bin",
            SMS_RUNTIME_MAP_GROUP_TYPE,
            SMS_RUNTIME_MAP_GROUP_NAME,
            SMS_RUNTIME_MAP_GROUP_INDEX,
            SMS_RUNTIME_MAP_GROUP_OWNER_TYPE,
        )
        .map_err(|error| {
            format!(
                "could not locate Sunshine's scheduled runtime map group in map/scene.bin: {error}"
            )
        })?
        .ok_or_else(|| {
            format!(
                "map/scene.bin has no {SMS_RUNTIME_MAP_GROUP_TYPE} group named {SMS_RUNTIME_MAP_GROUP_NAME:?} in the unique {SMS_RUNTIME_MAP_GROUP_OWNER_TYPE} group_index {SMS_RUNTIME_MAP_GROUP_INDEX} slot; authored SmJ3DAct actors would never be scheduled for calc, entry, and viewCalc"
            )
        })
}

fn runtime_character_name(asset_id: AssetId) -> String {
    format!("{}_character", model_runtime_resource_key(asset_id))
}

fn runtime_obj_chara_record(asset_id: AssetId, resource_key: &str) -> Result<JDramaRecord, String> {
    JDramaRecord::new(
        "ObjChara",
        runtime_character_name(asset_id),
        JDramaRecordPayload::Fields {
            fields: vec![JDramaField {
                name: "resource_folder".to_string(),
                value: JDramaFieldValue::String(format!("/scene/mapObj/{resource_key}")),
            }],
        },
    )
    .map_err(|error| format!("could not author runtime ObjChara record: {error}"))
}

fn runtime_sm_j3d_actor_record(placement: &ModelInstancePlacement) -> Result<JDramaRecord, String> {
    let transform = matrix_to_transform(placement.transform);
    if !transform.is_finite() {
        return Err(format!(
            "model instance {} contains a non-finite runtime transform",
            placement.instance_id
        ));
    }
    JDramaRecord::new(
        "SmJ3DAct",
        format!("sms_instance_{}", placement.instance_id.simple()),
        JDramaRecordPayload::Actor {
            transform: JDramaTransform {
                translation: transform.translation,
                rotation: transform.rotation_degrees,
                scale: transform.scale,
            },
            character_name: runtime_character_name(placement.asset_id),
            light_map: JDramaLightMap::default(),
            fields: Vec::new(),
        },
    )
    .map_err(|error| format!("could not author runtime SmJ3DAct record: {error}"))
}

fn authored_sky_record(placement: &ModelInstancePlacement) -> Result<JDramaRecord, String> {
    let transform = matrix_to_transform(placement.transform);
    if !transform.is_finite() {
        return Err(format!(
            "skybox instance {} contains a non-finite runtime transform",
            placement.instance_id
        ));
    }
    sms_scene::blank_stage_sky_record(JDramaTransform {
        translation: transform.translation,
        rotation: transform.rotation_degrees,
        scale: transform.scale,
    })
    .map_err(|error| format!("could not author typed TSky record: {error}"))
}

fn synchronize_runtime_sky_reflection_bundle(
    archive: Option<&SourceFreeStageArchive>,
    edits: &mut sms_scene::StageArchiveEdits,
) -> Result<(), String> {
    let sky_changed = edits
        .resource_removals
        .iter()
        .any(|path| runtime_sky_resource_stem(path) == Some("sky"))
        || edits
            .resources
            .iter()
            .any(|edit| runtime_sky_resource_stem(&edit.raw_resource_path) == Some("sky"))
        || edits
            .models
            .iter()
            .any(|edit| runtime_sky_resource_stem(&edit.raw_resource_path) == Some("sky"));
    if !sky_changed {
        return Ok(());
    }

    let mut effective = BTreeMap::<Vec<u8>, StageResourceDocument>::new();
    if let Some(archive) = archive {
        effective.extend(
            archive
                .resources()
                .iter()
                .map(|resource| (resource.raw_path.clone(), resource.document.clone())),
        );
    }
    for path in &edits.resource_removals {
        effective.retain(|candidate, _| !candidate.eq_ignore_ascii_case(path));
    }
    for edit in &edits.resources {
        effective.retain(|candidate, _| !candidate.eq_ignore_ascii_case(&edit.raw_resource_path));
        effective.insert(edit.raw_resource_path.clone(), edit.document.clone());
    }
    for edit in &edits.models {
        effective.retain(|candidate, _| !candidate.eq_ignore_ascii_case(&edit.raw_resource_path));
        effective.insert(
            edit.raw_resource_path.clone(),
            StageResourceDocument::Model(edit.document.clone()),
        );
    }

    let sky_model = effective
        .iter()
        .find_map(|(path, document)| {
            runtime_sky_resource_path_is(path, "sky", "bmd").then_some(document)
        })
        .and_then(|document| match document {
            StageResourceDocument::Model(model) => Some(model.clone()),
            _ => None,
        });
    let Some(sky_model) = sky_model else {
        for path in effective
            .keys()
            .filter(|path| runtime_sky_resource_stem(path) == Some("reflectsky"))
            .cloned()
            .collect::<Vec<_>>()
        {
            edits.remove_resource(path);
        }
        return Ok(());
    };

    if !effective
        .keys()
        .any(|path| runtime_sky_resource_path_is(path, "sky", "bmt"))
    {
        let table = sms_scene::runtime_sky_material_table(&sky_model)
            .map_err(|error| format!("could not derive shared sky.bmt: {error}"))?;
        edits.upsert_model(b"map/map/sky.bmt".to_vec(), table);
    }

    let reflection_model_explicit = edits.resources.iter().any(|edit| {
        runtime_sky_resource_path_is(&edit.raw_resource_path, "reflectsky", "bmd")
            && matches!(&edit.document, StageResourceDocument::Model(_))
    }) || edits
        .models
        .iter()
        .any(|edit| runtime_sky_resource_path_is(&edit.raw_resource_path, "reflectsky", "bmd"));
    if reflection_model_explicit {
        return Ok(());
    }

    // Legacy project overlays changed only sky.*. Replace the old helper and
    // its animations with source-free derivatives of that effective sky so a
    // managed build cannot keep reflecting the previous stage environment.
    for path in effective
        .keys()
        .filter(|path| runtime_sky_resource_stem(path) == Some("reflectsky"))
        .cloned()
        .collect::<Vec<_>>()
    {
        edits.remove_resource(path);
    }
    edits.upsert_model(b"map/map/reflectsky.bmd".to_vec(), sky_model);
    for (path, document) in &effective {
        let Some(extension) = runtime_sky_visual_animation_extension(path, "sky") else {
            continue;
        };
        let StageResourceDocument::Animation(animation) = document else {
            continue;
        };
        edits.upsert_resource(
            format!("map/map/reflectsky.{extension}").into_bytes(),
            StageResourceDocument::Animation(animation.clone()),
        );
    }
    Ok(())
}

fn runtime_sky_resource_stem(path: &[u8]) -> Option<&'static str> {
    let normalized = String::from_utf8_lossy(path).replace('\\', "/");
    let (directory, file_name) = normalized.rsplit_once('/')?;
    if !directory.eq_ignore_ascii_case("map/map") {
        return None;
    }
    let (stem, extension) = file_name.rsplit_once('.')?;
    if extension.is_empty() {
        return None;
    }
    if stem.eq_ignore_ascii_case("sky") {
        Some("sky")
    } else if stem.eq_ignore_ascii_case("reflectsky") {
        Some("reflectsky")
    } else {
        None
    }
}

fn runtime_sky_resource_path_is(
    path: &[u8],
    expected_stem: &str,
    expected_extension: &str,
) -> bool {
    let normalized = String::from_utf8_lossy(path).replace('\\', "/");
    let Some((directory, file_name)) = normalized.rsplit_once('/') else {
        return false;
    };
    let Some((stem, extension)) = file_name.rsplit_once('.') else {
        return false;
    };
    directory.eq_ignore_ascii_case("map/map")
        && stem.eq_ignore_ascii_case(expected_stem)
        && extension.eq_ignore_ascii_case(expected_extension)
}

fn runtime_sky_visual_animation_extension(path: &[u8], expected_stem: &str) -> Option<String> {
    let normalized = String::from_utf8_lossy(path).replace('\\', "/");
    let (directory, file_name) = normalized.rsplit_once('/')?;
    let (stem, extension) = file_name.rsplit_once('.')?;
    if !directory.eq_ignore_ascii_case("map/map") || !stem.eq_ignore_ascii_case(expected_stem) {
        return None;
    }
    matches!(
        extension.to_ascii_lowercase().as_str(),
        "bck" | "bpk" | "btp" | "brk" | "btk"
    )
    .then(|| extension.to_ascii_lowercase())
}

fn stock_map_obj_base_record(
    placement: &ModelInstancePlacement,
    resource_name: &str,
) -> Result<JDramaRecord, String> {
    let transform = matrix_to_transform(placement.transform);
    if !transform.is_finite() {
        return Err(format!(
            "model instance {} contains a non-finite stock MapObjBase transform",
            placement.instance_id
        ));
    }
    JDramaRecord::new(
        "MapObjBase",
        format!("sms_stock_instance_{}", placement.instance_id.simple()),
        JDramaRecordPayload::Actor {
            transform: JDramaTransform {
                translation: transform.translation,
                rotation: transform.rotation_degrees,
                scale: transform.scale,
            },
            // TMapObjBase resolves its model/collision through the exact
            // subclass resource name below, not through an ObjChara record.
            character_name: String::new(),
            light_map: JDramaLightMap::default(),
            fields: vec![JDramaField {
                name: "resource_name".to_string(),
                value: JDramaFieldValue::String(resource_name.to_string()),
            }],
        },
    )
    .map_err(|error| format!("could not author stock MapObjBase record: {error}"))
}

fn stock_mapobj_archive_path(resource: &str, required_extension: &str) -> Result<Vec<u8>, String> {
    let normalized = resource.replace('\\', "/");
    if normalized.trim() != normalized || normalized.is_empty() || normalized.contains('\0') {
        return Err(format!("invalid stock resource path {resource:?}"));
    }
    let lower = normalized.to_ascii_lowercase();
    let relative = if lower.starts_with("/scene/mapobj/") {
        &normalized["/scene/mapobj/".len()..]
    } else if lower.starts_with("scene/mapobj/") {
        &normalized["scene/mapobj/".len()..]
    } else if lower.starts_with("mapobj/") {
        &normalized["mapobj/".len()..]
    } else {
        if normalized.starts_with('/') || normalized.contains(':') {
            return Err(format!(
                "stock resource path {resource:?} is outside /scene/mapObj"
            ));
        }
        normalized.as_str()
    };
    if relative.is_empty()
        || relative
            .split('/')
            .any(|component| component.is_empty() || matches!(component, "." | ".."))
    {
        return Err(format!(
            "stock resource path {resource:?} has an unsafe archive-relative component"
        ));
    }

    let lower_relative = relative.to_ascii_lowercase();
    let relative = if lower_relative.ends_with(required_extension) {
        relative.to_string()
    } else if required_extension == ".col" && !relative.rsplit('/').next().unwrap().contains('.') {
        format!("{relative}.col")
    } else {
        return Err(format!(
            "stock resource path {resource:?} must name a {required_extension} resource"
        ));
    };
    Ok(format!("mapobj/{relative}").into_bytes())
}

fn empty_col_file() -> ColFile {
    ColFile::new(Vec::new(), Vec::new())
}

fn find_jdrama_group_path(
    archive: &SourceFreeStageArchive,
    raw_resource_path: &[u8],
    type_name: &str,
) -> Result<Vec<usize>, String> {
    let Some(StageResourceDocument::Placement(document)) = archive.resource(raw_resource_path)
    else {
        return Err(format!(
            "stage resource {} is missing or is not a typed JDrama document",
            String::from_utf8_lossy(raw_resource_path)
        ));
    };
    find_jdrama_group_path_in_record(&document.root, type_name, &mut Vec::new()).ok_or_else(|| {
        format!(
            "stage resource {} has no {type_name} group for authored model insertion",
            String::from_utf8_lossy(raw_resource_path)
        )
    })
}

fn stage_scene_record_names(
    archive: &SourceFreeStageArchive,
    edits: &sms_scene::StageArchiveEdits,
) -> Result<BTreeSet<String>, String> {
    let edited_scene = edits
        .resources
        .iter()
        .find(|edit| edit.raw_resource_path == b"map/scene.bin");
    let scene_resource = edited_scene
        .map(|edit| &edit.document)
        .or_else(|| archive.resource(b"map/scene.bin"));
    let Some(StageResourceDocument::Placement(document)) = scene_resource else {
        return Err(
            "stage resource map/scene.bin is missing or is not a typed JDrama document".to_string(),
        );
    };
    let mut names = BTreeSet::new();
    collect_jdrama_record_names(&document.root, &mut names);
    for insertion in edits
        .placement_inserts
        .iter()
        .filter(|insertion| insertion.raw_resource_path == b"map/scene.bin")
    {
        collect_jdrama_record_names(&insertion.record, &mut names);
    }
    Ok(names)
}

fn collect_jdrama_record_names(record: &JDramaRecord, names: &mut BTreeSet<String>) {
    names.insert(record.name.clone());
    if let JDramaRecordPayload::Group { children, .. } = &record.payload {
        for child in children {
            collect_jdrama_record_names(child, names);
        }
    }
}

fn find_jdrama_group_path_in_record(
    record: &JDramaRecord,
    type_name: &str,
    path: &mut Vec<usize>,
) -> Option<Vec<usize>> {
    let JDramaRecordPayload::Group { children, .. } = &record.payload else {
        return None;
    };
    if record.type_name == type_name {
        return Some(path.clone());
    }
    for (index, child) in children.iter().enumerate() {
        path.push(index);
        if let Some(found) = find_jdrama_group_path_in_record(child, type_name, path) {
            return Some(found);
        }
        path.pop();
    }
    None
}

fn node_depth(nodes: &[sms_authoring::ModelNode], mut index: usize) -> usize {
    let mut depth = 0usize;
    while let Some(parent) = nodes.get(index).and_then(|node| node.parent) {
        depth = depth.saturating_add(1);
        if depth >= nodes.len() {
            break;
        }
        index = parent as usize;
    }
    depth
}

fn model_vector_drag(ui: &mut egui::Ui, value: &mut [f32; 3], speed: f32) -> bool {
    let mut changed = false;
    ui.horizontal(|ui| {
        for (axis, label) in ["X", "Y", "Z"].into_iter().enumerate() {
            ui.label(label);
            changed |= ui
                .add(egui::DragValue::new(&mut value[axis]).speed(speed))
                .changed();
        }
    });
    changed
}

fn collision_surface_controls(
    ui: &mut egui::Ui,
    surface: &mut CollisionSurface,
    definitions: &[sms_schema::CollisionSurfaceDefinition],
) {
    ui.horizontal(|ui| {
        ui.label("Default surface");
        ui.add(
            egui::DragValue::new(&mut surface.surface_type)
                .hexadecimal(4, false, true)
                .prefix("type "),
        );
        ui.add(egui::DragValue::new(&mut surface.attribute_0).prefix("attr0 "));
        ui.add(egui::DragValue::new(&mut surface.attribute_1).prefix("attr1 "));
    });
    ui.small(collision_surface_description(
        surface.surface_type,
        definitions,
    ));
}

fn collision_surface_description(
    surface_type: u16,
    definitions: &[sms_schema::CollisionSurfaceDefinition],
) -> String {
    let mut names = definitions
        .iter()
        .filter(|definition| {
            if definition.is_property_flag {
                definition.value != 0 && surface_type & definition.value == definition.value
            } else {
                definition.value == surface_type
            }
        })
        .map(|definition| definition.name.as_str())
        .collect::<Vec<_>>();
    names.sort_unstable();
    names.dedup();
    if names.is_empty() {
        format!("Unknown raw collision surface {surface_type:#06x}")
    } else {
        format!("Decomp-derived: {}", names.join(" | "))
    }
}

fn model_asset_thumbnail_card(
    ui: &mut egui::Ui,
    size: egui::Vec2,
    selected: bool,
    entry: &CatalogAssetEntry,
) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(size, egui::Sense::click_and_drag());
    let visuals = if selected {
        ui.visuals().widgets.active
    } else if response.hovered() {
        ui.visuals().widgets.hovered
    } else {
        ui.visuals().widgets.inactive
    };
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 5.0, visuals.bg_fill);
    painter.rect_stroke(rect, 5.0, visuals.bg_stroke, egui::StrokeKind::Inside);

    let thumbnail = egui::Rect::from_min_max(
        rect.min + egui::vec2(7.0, 7.0),
        egui::pos2(rect.right() - 7.0, rect.top() + 49.0),
    );
    painter.rect_filled(thumbnail, 3.0, egui::Color32::from_rgb(27, 34, 39));
    let center = thumbnail.center();
    let radius = (thumbnail.height() * 0.34).min(thumbnail.width() * 0.18);
    let hue = (entry.mesh_count as u8)
        .wrapping_mul(37)
        .wrapping_add((entry.material_count as u8).wrapping_mul(17));
    let color = egui::Color32::from_rgb(
        90u8.saturating_add(hue / 4),
        150u8.saturating_add(hue / 5),
        205u8.saturating_sub(hue / 6),
    );
    let top = center + egui::vec2(0.0, -radius);
    let left = center + egui::vec2(-radius, radius * 0.75);
    let right = center + egui::vec2(radius, radius * 0.75);
    painter.add(egui::Shape::convex_polygon(
        vec![top, right, left],
        color,
        egui::Stroke::new(1.0, egui::Color32::from_white_alpha(150)),
    ));
    if entry.has_collision {
        painter.text(
            thumbnail.right_top() + egui::vec2(-4.0, 3.0),
            egui::Align2::RIGHT_TOP,
            "COL",
            egui::FontId::monospace(9.0),
            egui::Color32::from_rgb(111, 220, 168),
        );
    }
    painter.text(
        egui::pos2(rect.left() + 7.0, rect.top() + 55.0),
        egui::Align2::LEFT_TOP,
        &entry.name,
        egui::FontId::proportional(12.0),
        visuals.fg_stroke.color,
    );
    painter.text(
        egui::pos2(rect.left() + 7.0, rect.top() + 73.0),
        egui::Align2::LEFT_TOP,
        format!(
            "{} mesh / {} mat / {} tex",
            entry.mesh_count, entry.material_count, entry.texture_count
        ),
        egui::FontId::proportional(10.0),
        egui::Color32::GRAY,
    );
    response
}

fn model_document_bounds(document: &ModelAssetDocument) -> Option<[[f32; 3]; 2]> {
    document
        .converted_bounds()
        .ok()
        .flatten()
        .map(|bounds| [bounds.min, bounds.max])
}

pub(super) fn build_authored_model_preview(
    document: &ModelAssetDocument,
    loader_flags: u32,
) -> Result<AuthoredModelPreviewGeometry, String> {
    let bytes = document
        .compile_bmd()
        .map_err(|error| format!("compile authored BMD3: {error}"))?;
    let file = J3dFile::parse(&bytes).map_err(|error| format!("parse authored BMD3: {error}"))?;
    let preview = file
        .geometry_preview_with_loader_flags(loader_flags)
        .map_err(|error| format!("decode authored BMD3 preview: {error}"))?;
    Ok(AuthoredModelPreviewGeometry {
        preview,
        loader_diagnostics: loader_diagnostics_for_document(document, loader_flags),
    })
}

pub(super) fn append_authored_model_instances(
    preview: &mut ModelPreview,
    cache: &BTreeMap<AuthoredModelPreviewKey, Arc<AuthoredModelPreviewGeometry>>,
    instances: &[EditorModelInstance],
    stage_id: &str,
    registry: Option<&ObjectRegistry>,
) -> usize {
    let triangle_start = preview.triangles.len();
    let mut resources = BTreeMap::<AuthoredModelPreviewKey, (usize, usize)>::new();
    let mut next_model_index = preview
        .triangles
        .iter()
        .map(|triangle| triangle.model_index)
        .chain(preview.points.iter().map(|point| point.model_index))
        .max()
        .map_or(0, |index| index + 1);
    let mut next_packet_index = preview
        .triangles
        .iter()
        .map(|triangle| triangle.packet_index)
        .max()
        .map_or(0, |index| index + 1);

    for instance in instances
        .iter()
        .filter(|instance| instance.stage_id.eq_ignore_ascii_case(stage_id))
    {
        let Ok(key) = model_instance_preview_key(instance, registry) else {
            continue;
        };
        let Some(geometry) = cache.get(&key) else {
            continue;
        };
        let (texture_base, material_base) = *resources.entry(key).or_insert_with(|| {
            let texture_base = push_preview_textures(&mut preview.textures, &geometry.preview);
            let material_base =
                push_preview_materials(&mut preview.materials, &geometry.preview, texture_base);
            preview
                .material_animation_bindings
                .resize_with(preview.materials.len(), Vec::new);
            (texture_base, material_base)
        });
        let transform = matrix_to_transform(instance.placement.transform);
        let render_layer = if instance.placement.export_mode == ModelInstanceExportMode::Skybox {
            PreviewRenderLayer::Sky
        } else {
            PreviewRenderLayer::Main
        };
        let packet_base = next_packet_index;
        next_packet_index += geometry
            .preview
            .triangles
            .iter()
            .map(|triangle| triangle.packet_index)
            .max()
            .map_or(1, |index| index + 1);

        for triangle in &geometry.preview.triangles {
            let vertices = triangle
                .vertices
                .map(|point| transform_matrix_point(instance.placement.transform, point));
            if !triangle_vertices_are_finite(vertices) {
                continue;
            }
            let normals = triangle
                .normals
                .map(|normals| transform_preview_normals(normals, transform));
            preview.triangles.push(PreviewTriangle {
                vertices,
                normals,
                color_channels: triangle.color_channels,
                tex_coord_sets: triangle.tex_coord_sets,
                material_index: triangle
                    .material_index
                    .map(|index| material_base + index)
                    .filter(|index| *index < preview.materials.len()),
                packet_index: packet_base + triangle.packet_index,
                model_index: next_model_index,
                render_layer,
                color: triangle.color,
                vertex_colors: triangle.vertex_colors,
                combine_mode: triangle.combine_mode,
                tex_coords: triangle.tex_coords,
                texture_index: triangle
                    .texture_index
                    .map(|index| texture_base + index)
                    .filter(|index| *index < preview.textures.len()),
                mask_tex_coords: triangle.mask_tex_coords,
                mask_texture_index: triangle
                    .mask_texture_index
                    .map(|index| texture_base + index)
                    .filter(|index| *index < preview.textures.len()),
                cull_mode: triangle.cull_mode,
                alpha_compare: triangle.alpha_compare,
                blend_mode: triangle.blend_mode,
                z_mode: triangle.z_mode,
                billboard: triangle
                    .billboard
                    .and_then(|billboard| transform_j3d_billboard(billboard, transform, normals)),
                particle_type: None,
                particle_pivot: None,
                particle_direction: None,
                particle_color_mode: None,
                particle_environment_color: None,
            });
        }
        next_model_index += 1;
    }
    preview.triangles.len() - triangle_start
}

fn empty_authored_model_preview() -> ModelPreview {
    ModelPreview {
        points: Vec::new(),
        triangles: Vec::new(),
        collision_triangles: Vec::new(),
        collision_file_count: 0,
        collision_surface_count: 0,
        failed_collision_files: 0,
        collision_failures: Vec::new(),
        textures: Vec::new(),
        materials: Vec::new(),
        texture_srt_animations: Vec::new(),
        texture_pattern_animations: Vec::new(),
        material_animation_bindings: Vec::new(),
        bounds_min: [0.0; 3],
        bounds_max: [1.0; 3],
        camera_bounds_min: [0.0; 3],
        camera_bounds_max: [1.0; 3],
        loaded_models: 0,
        failed_models: 0,
        model_failures: Vec::new(),
        source_vertices: 0,
        source_triangles: 0,
        source_textures: 0,
        object_model_indices: BTreeMap::new(),
        mirror_actor_positions: BTreeMap::new(),
        mirror_cubes: Vec::new(),
        mirror_model_slots: BTreeMap::new(),
        animated_models: Vec::new(),
        animated_flags: Vec::new(),
        rotating_models: Vec::new(),
        level_transform_models: Vec::new(),
        level_transform_particles: Vec::new(),
        actor_particles: Vec::new(),
        level_transform_duration_frames: 0.0,
        level_transform_particle_end_frames: 0.0,
    }
}

fn transform_matrix_point(matrix: [[f32; 4]; 4], point: [f32; 3]) -> [f32; 3] {
    std::array::from_fn(|row| {
        matrix[0][row] * point[0]
            + matrix[1][row] * point[1]
            + matrix[2][row] * point[2]
            + matrix[3][row]
    })
}

fn transformed_bounds_corners(instance: &EditorModelInstance) -> [[f32; 3]; 8] {
    let [minimum, maximum] = instance.local_bounds;
    let transform = matrix_to_transform(instance.placement.transform);
    std::array::from_fn(|index| {
        let local = [
            if index & 1 == 0 {
                minimum[0]
            } else {
                maximum[0]
            },
            if index & 2 == 0 {
                minimum[1]
            } else {
                maximum[1]
            },
            if index & 4 == 0 {
                minimum[2]
            } else {
                maximum[2]
            },
        ];
        transform_instance_point(local, transform)
    })
}

fn transform_to_matrix(transform: Transform) -> [[f32; 4]; 4] {
    let origin = transform.translation;
    let linear_transform = Transform {
        translation: [0.0; 3],
        ..transform
    };
    let basis = [
        transform_instance_point([1.0, 0.0, 0.0], linear_transform),
        transform_instance_point([0.0, 1.0, 0.0], linear_transform),
        transform_instance_point([0.0, 0.0, 1.0], linear_transform),
    ];
    [
        [basis[0][0], basis[0][1], basis[0][2], 0.0],
        [basis[1][0], basis[1][1], basis[1][2], 0.0],
        [basis[2][0], basis[2][1], basis[2][2], 0.0],
        [origin[0], origin[1], origin[2], 1.0],
    ]
}

fn matrix_to_transform(matrix: [[f32; 4]; 4]) -> Transform {
    let mut scale = [0.0; 3];
    for column in 0..3 {
        scale[column] =
            (matrix[column][0].powi(2) + matrix[column][1].powi(2) + matrix[column][2].powi(2))
                .sqrt();
    }
    let determinant = matrix[0][0] * (matrix[1][1] * matrix[2][2] - matrix[1][2] * matrix[2][1])
        - matrix[1][0] * (matrix[0][1] * matrix[2][2] - matrix[0][2] * matrix[2][1])
        + matrix[2][0] * (matrix[0][1] * matrix[1][2] - matrix[0][2] * matrix[1][1]);
    if determinant < 0.0 {
        // Euler angles represent a proper rotation, so retain a reflected
        // basis by assigning its determinant sign to one scale component.
        // Choosing the largest component gives the most stable normalization.
        let reflected_axis = (0..3)
            .max_by(|left, right| scale[*left].total_cmp(&scale[*right]))
            .unwrap_or(0);
        scale[reflected_axis] = -scale[reflected_axis];
    }

    let mut rotation = [[0.0; 3]; 3];
    for column in 0..3 {
        let divisor = if scale[column].abs() > f32::EPSILON {
            scale[column]
        } else {
            1.0
        };
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
    Transform {
        translation: [matrix[3][0], matrix[3][1], matrix[3][2]],
        rotation_degrees: [x.to_degrees(), y.to_degrees(), z.to_degrees()],
        scale,
    }
}

fn transform_instance_point(mut point: [f32; 3], transform: Transform) -> [f32; 3] {
    for (value, scale) in point.iter_mut().zip(transform.scale) {
        *value *= scale;
    }
    for axis in 0..3 {
        let radians = transform.rotation_degrees[axis].to_radians();
        let (sin, cos) = radians.sin_cos();
        point = match axis {
            0 => [
                point[0],
                point[1] * cos - point[2] * sin,
                point[1] * sin + point[2] * cos,
            ],
            1 => [
                point[0] * cos + point[2] * sin,
                point[1],
                -point[0] * sin + point[2] * cos,
            ],
            _ => [
                point[0] * cos - point[1] * sin,
                point[0] * sin + point[1] * cos,
                point[2],
            ],
        };
    }
    std::array::from_fn(|axis| point[axis] + transform.translation[axis])
}

fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let parent = path
        .parent()
        .ok_or_else(|| format!("'{}' has no parent directory", path.display()))?;
    let temp = parent.join(format!(
        ".{}.{}.tmp",
        path.file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("instances"),
        std::process::id()
    ));
    let backup = parent.join(format!(
        ".{}.{}.bak",
        path.file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("instances"),
        std::process::id()
    ));
    let mut file = File::create(&temp)
        .map_err(|error| format!("Could not create '{}': {error}", temp.display()))?;
    file.write_all(bytes)
        .and_then(|()| file.sync_all())
        .map_err(|error| format!("Could not write '{}': {error}", temp.display()))?;
    if path.exists() {
        fs::rename(path, &backup).map_err(|error| {
            format!(
                "Could not prepare atomic update '{}': {error}",
                path.display()
            )
        })?;
    }
    if let Err(error) = fs::rename(&temp, path) {
        if backup.exists() {
            let _ = fs::rename(&backup, path);
        }
        return Err(format!("Could not commit '{}': {error}", path.display()));
    }
    if backup.exists() {
        fs::remove_file(&backup).map_err(|error| {
            format!(
                "Could not remove update backup '{}': {error}",
                backup.display()
            )
        })?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_collision_model_defaults_to_authored_map_terrain_only_once() {
        assert!(should_default_to_map_terrain(true, true, "custom0", &[]));
        assert!(!should_default_to_map_terrain(false, true, "custom0", &[]));
        assert!(!should_default_to_map_terrain(true, false, "custom0", &[]));

        let mut placement = ModelInstancePlacement::new(AssetId::new(), "world");
        placement.export_mode = ModelInstanceExportMode::MapTerrain;
        let instances = [EditorModelInstance {
            stage_id: "custom0".to_string(),
            placement,
            local_bounds: [[-1.0; 3], [1.0; 3]],
        }];
        assert!(!should_default_to_map_terrain(
            true, true, "custom0", &instances
        ));
        assert!(should_default_to_map_terrain(
            true, true, "custom1", &instances
        ));
    }

    #[test]
    fn first_authored_stage_model_is_placed_as_terrain_and_framed() {
        let temporary = tempfile::tempdir().unwrap();
        let content = temporary.path().join("Content");
        let catalog = ModelAssetCatalog::open_content_root(&content).unwrap();
        let source = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(
            "../../crates/sms-authoring/tests/fixtures/gltf/valid/minimal-external/model.gltf",
        );
        let mut options = sms_authoring::ModelImportOptions::default();
        options.coordinate_conversion.units_per_meter = 100_000.0;
        let imported = sms_authoring::import_model(source, &options).unwrap().asset;
        let entry = catalog
            .create_asset("Geometry/large-terrain.smsmodel", &imported)
            .unwrap();

        let mut app = SmsEditorApp::default();
        app.project_root = temporary.path().to_string_lossy().into_owned();
        app.stage_id = "custom0".to_string();
        app.document = Some(StageDocument {
            stage_id: app.stage_id.clone(),
            base_root: temporary.path().to_path_buf(),
            assets: Vec::new(),
            objects: Vec::new(),
            changed_files: BTreeMap::new(),
            stage_archive: Some(SourceFreeStageArchive::new_for_blank("custom0", 1).unwrap()),
            stage_archive_source_path: None,
            archive_edits: sms_scene::StageArchiveEdits::default(),
            registry: None,
            route_authoring: None,
            load_issues: Vec::new(),
            lighting: sms_scene::StageLighting::default(),
            actor_previews: BTreeMap::new(),
            loaded_project: None,
        });
        app.renderer.camera_mut().distance = 2_500.0;

        app.spawn_model_instance_at(entry.id, [0.0, 0.0, 0.0]);

        assert_eq!(app.model_instances.len(), 1);
        assert_eq!(
            app.model_instances[0].placement.export_mode,
            ModelInstanceExportMode::MapTerrain
        );
        assert_eq!(
            app.selected_model_instance_id,
            Some(app.model_instances[0].placement.instance_id)
        );
        assert!(app.renderer.camera().distance > 2_500.0);
        assert!(app
            .model_asset_preview_cache
            .contains_key(&AuthoredModelPreviewKey {
                asset_id: entry.id,
                loader_flags: SMS_MAP_MODEL_LOAD_FLAGS,
            }));
        assert!(content.join(MODEL_INSTANCE_MANIFEST_NAME).is_file());
    }

    #[test]
    fn opaque_material_preset_restores_solid_pixel_and_depth_state() {
        let mut material = GxMaterial {
            material_mode: 4,
            cull_mode: 0,
            alpha_compare: sms_authoring::GxAlphaCompare {
                comparison_0: 1,
                reference_0: 128,
                operation: 1,
                comparison_1: 2,
                reference_1: 64,
            },
            blend_mode: sms_authoring::GxBlendMode {
                mode: 1,
                source_factor: 4,
                destination_factor: 5,
                logic_operation: 3,
            },
            depth_mode: sms_authoring::GxDepthMode {
                comparison_enabled: 0,
                function: 7,
                update_enabled: 0,
            },
            z_compare_location: 0,
            ..GxMaterial::default()
        };

        apply_opaque_material_preset(&mut material);

        assert_eq!(material.material_mode, 1);
        assert_eq!(material.cull_mode, 2);
        assert_eq!(
            material.alpha_compare,
            sms_authoring::GxAlphaCompare::default()
        );
        assert_eq!(
            material.blend_mode,
            sms_authoring::GxBlendMode {
                mode: 0,
                source_factor: 1,
                destination_factor: 0,
                logic_operation: 3,
            }
        );
        assert_eq!(material.depth_mode, sms_authoring::GxDepthMode::default());
        assert_eq!(material.z_compare_location, 1);
    }

    #[test]
    fn alpha_blend_material_preset_is_explicit_and_preserves_culling_choice() {
        let mut material = GxMaterial {
            material_mode: 1,
            cull_mode: 0,
            alpha_compare: sms_authoring::GxAlphaCompare {
                comparison_0: 1,
                reference_0: 128,
                operation: 1,
                comparison_1: 2,
                reference_1: 64,
            },
            blend_mode: sms_authoring::GxBlendMode::default(),
            depth_mode: sms_authoring::GxDepthMode {
                comparison_enabled: 0,
                function: 7,
                update_enabled: 1,
            },
            z_compare_location: 0,
            ..GxMaterial::default()
        };

        apply_alpha_blend_material_preset(&mut material);

        assert_eq!(material.material_mode, 4);
        assert_eq!(material.cull_mode, 0);
        assert_eq!(
            material.alpha_compare,
            sms_authoring::GxAlphaCompare::default()
        );
        assert_eq!(
            material.blend_mode,
            sms_authoring::GxBlendMode {
                mode: 1,
                source_factor: 4,
                destination_factor: 5,
                logic_operation: 3,
            }
        );
        assert_eq!(
            material.depth_mode,
            sms_authoring::GxDepthMode {
                update_enabled: 0,
                ..sms_authoring::GxDepthMode::default()
            }
        );
        assert_eq!(material.z_compare_location, 1);
    }

    #[test]
    fn conservative_diffuse_specular_preset_is_exactly_reversible() {
        let source = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(
            "../../crates/sms-authoring/tests/fixtures/gltf/valid/material-textures/model.gltf",
        );
        let mut document =
            sms_authoring::import_model(source, &sms_authoring::ModelImportOptions::default())
                .unwrap()
                .asset;
        let material = document
            .materials
            .iter_mut()
            .find(|material| material.gx.name == "Opaque")
            .unwrap();
        let original = material.gx.clone();

        assert!(apply_conservative_diffuse_specular_preset(material));
        assert_eq!(material.gx.color_channel_count, 2);
        assert_eq!(material.gx.color_channels[0].unwrap().light_mask, 0x01);
        let specular = material.gx.color_channels[2].unwrap();
        assert_eq!(specular.enable, 1);
        assert_eq!(specular.light_mask, 0x04);
        assert_eq!(
            material.gx.color_channels[0].unwrap().attenuation_function,
            1
        );
        assert_eq!(specular.diffuse_function, 1);
        assert_eq!(specular.attenuation_function, 0);
        assert_eq!(material.gx.material_colors[1], Some([255; 4]));
        assert_eq!(material.gx.ambient_colors[1], Some([0; 4]));
        assert_eq!(material.gx.tev_stage_count, 2);
        assert_eq!(
            material.gx.tev_orders[1],
            Some(sms_authoring::GxTevOrder {
                tex_coord: None,
                tex_map: None,
                color_channel: 5,
            })
        );
        assert_eq!(
            material.gx.tev_stages[1].unwrap().color_inputs,
            [15, 10, 12, 0]
        );
        assert_eq!(
            material.gx.tev_stages[1].unwrap().alpha_inputs,
            [7, 7, 7, 0]
        );

        apply_conservative_unlit_preset(material);
        assert_eq!(material.gx, original);
    }

    #[test]
    fn conservative_unlit_preserves_arbitrary_advanced_tev_state() {
        let mut material = sms_authoring::ModelMaterial {
            gx: GxMaterial::default(),
            source_base_color: [1.0; 4],
            base_color_texture: None,
            vertex_color_set: None,
            source_double_sided: false,
            source_alpha_mode: sms_authoring::ImportedAlphaMode::Opaque,
            source_pbr: sms_authoring::SourcePbrMetadata {
                metallic_factor: 1.0,
                roughness_factor: 1.0,
                has_metallic_roughness_texture: false,
                has_normal_texture: false,
                has_occlusion_texture: false,
                emissive_factor: [0.0; 3],
                has_emissive_texture: false,
            },
        };
        material.gx.tev_stage_count = 2;
        material.gx.tev_orders[1] = Some(sms_authoring::GxTevOrder {
            tex_coord: Some(3),
            tex_map: Some(2),
            color_channel: 5,
        });
        material.gx.tev_stages[1] = Some(sms_authoring::GxTevStage {
            color_inputs: [0, 2, 4, 6],
            alpha_inputs: [0, 1, 2, 3],
            ..sms_authoring::GxTevStage::default()
        });
        material.gx.color_channel_count = 2;
        material.gx.color_channels = [
            Some(sms_authoring::GxColorChannel {
                enable: 1,
                ..sms_authoring::GxColorChannel::default()
            }),
            Some(sms_authoring::GxColorChannel::default()),
            Some(sms_authoring::GxColorChannel {
                enable: 1,
                light_mask: 0x04,
                ..sms_authoring::GxColorChannel::default()
            }),
            None,
        ];
        let tev_orders = material.gx.tev_orders;
        let tev_stages = material.gx.tev_stages;
        let stage_count = material.gx.tev_stage_count;

        apply_conservative_unlit_preset(&mut material);

        assert_eq!(material.gx.tev_orders, tev_orders);
        assert_eq!(material.gx.tev_stages, tev_stages);
        assert_eq!(material.gx.tev_stage_count, stage_count);
        assert!(material
            .gx
            .color_channels
            .iter()
            .flatten()
            .all(|channel| channel.enable == 0));
    }

    #[test]
    fn conservative_diffuse_specular_program_survives_bmd_compile_and_reparse() {
        let source = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(
            "../../crates/sms-authoring/tests/fixtures/gltf/valid/material-textures/model.gltf",
        );
        let mut document =
            sms_authoring::import_model(source, &sms_authoring::ModelImportOptions::default())
                .unwrap()
                .asset;
        let material = document
            .materials
            .iter_mut()
            .find(|material| material.gx.name == "Opaque")
            .unwrap();
        assert!(apply_conservative_diffuse_specular_preset(material));

        let preview = build_authored_model_preview(&document, SMS_SM_J3D_ACT_MODEL_LOAD_FLAGS)
            .expect("compile and reparse conservative diffuse + specular material");
        let material = preview
            .preview
            .materials
            .iter()
            .find(|material| material.name == "Opaque")
            .unwrap();
        assert_eq!(material.color_channel_count, 2);
        assert_eq!(material.color_channels[0].light_mask, 0x01);
        assert_eq!(material.color_channels[2].enable, 1);
        assert_eq!(material.color_channels[2].light_mask, 0x04);
        assert_eq!(material.color_channels[0].attenuation_fn, 1);
        assert_eq!(material.color_channels[2].diffuse_fn, 1);
        assert_eq!(material.color_channels[2].attenuation_fn, 0);
        assert_eq!(material.material_colors[1], [255; 4]);
        assert_eq!(material.ambient_colors[1], [0; 4]);
        assert_eq!(material.tev_stages.len(), 2);
        assert_eq!(material.tev_stages[0].order.color_channel, 4);
        assert_eq!(material.tev_stages[1].order.color_channel, 5);
        assert_eq!(material.tev_stages[1].color_args, [15, 10, 12, 0]);
        assert_eq!(material.tev_stages[1].alpha_args, [7, 7, 7, 0]);
    }

    #[test]
    fn model_bounds_use_converted_node_hierarchy() {
        let mut document = ModelAssetDocument::new("bounds");
        document.scene_roots.push(0);
        document.nodes.push(sms_authoring::ModelNode {
            name: "mesh-node".to_string(),
            parent: None,
            children: Vec::new(),
            mesh: Some(0),
            purpose: sms_authoring::NodePurpose::Render,
            local_transform: [
                [1.0, 0.0, 0.0, 0.0],
                [0.0, 1.0, 0.0, 0.0],
                [0.0, 0.0, 1.0, 0.0],
                [10.0, 20.0, 30.0, 1.0],
            ],
        });
        document.meshes.push(sms_authoring::ModelMesh {
            name: "mesh".to_string(),
            primitives: vec![sms_authoring::ModelPrimitive {
                positions: vec![[-2.0, 3.0, 8.0], [5.0, -4.0, 1.0], [0.0, 2.0, 4.0]],
                normals: vec![[0.0, 1.0, 0.0]; 3],
                tangents: Vec::new(),
                tex_coords: Vec::new(),
                colors: Vec::new(),
                indices: vec![0, 1, 2],
                material: None,
            }],
        });
        assert_eq!(
            model_document_bounds(&document),
            Some([[8.0, 16.0, 31.0], [15.0, 23.0, 38.0]])
        );
    }

    #[test]
    fn transformed_bounds_apply_scale_rotation_and_translation() {
        let point = transform_instance_point(
            [1.0, 0.0, 0.0],
            Transform {
                translation: [10.0, 20.0, 30.0],
                rotation_degrees: [0.0, 0.0, 90.0],
                scale: [2.0, 1.0, 1.0],
            },
        );
        assert!((point[0] - 10.0).abs() < 0.001);
        assert!((point[1] - 22.0).abs() < 0.001);
        assert!((point[2] - 30.0).abs() < 0.001);
    }

    #[test]
    fn shared_placement_matrix_round_trips_editor_transform() {
        let transform = Transform {
            translation: [125.0, -40.0, 900.0],
            rotation_degrees: [15.0, -25.0, 70.0],
            scale: [2.0, 0.5, 1.25],
        };
        let round_trip = matrix_to_transform(transform_to_matrix(transform));
        for axis in 0..3 {
            assert!((round_trip.translation[axis] - transform.translation[axis]).abs() < 0.001);
            assert!(
                (round_trip.rotation_degrees[axis] - transform.rotation_degrees[axis]).abs()
                    < 0.001,
                "round trip {round_trip:?} from {transform:?}"
            );
            assert!((round_trip.scale[axis] - transform.scale[axis]).abs() < 0.001);
        }
    }

    #[test]
    fn reflected_nonuniform_placement_matrix_round_trips_without_losing_handedness() {
        let source = transform_to_matrix(Transform {
            translation: [-125.0, 80.0, 900.0],
            rotation_degrees: [23.0, -41.0, 67.0],
            scale: [2.5, -0.4, 1.75],
        });
        let rebuilt = transform_to_matrix(matrix_to_transform(source));
        for column in 0..4 {
            for row in 0..4 {
                assert!(
                    (rebuilt[column][row] - source[column][row]).abs() < 0.001,
                    "matrix[{column}][{row}] changed from {} to {}",
                    source[column][row],
                    rebuilt[column][row]
                );
            }
        }
        let transform = matrix_to_transform(source);
        assert!(transform.scale.into_iter().any(|scale| scale < 0.0));
    }

    #[test]
    fn instance_preview_keys_follow_the_exact_runtime_loader_for_each_export_mode() {
        let registry = stock_export_test_registry();
        let asset_id = AssetId::new();
        let mut placement = ModelInstancePlacement::new(asset_id, "profile");
        let mut instance = EditorModelInstance {
            stage_id: "test11".to_string(),
            placement: placement.clone(),
            local_bounds: [[0.0; 3], [1.0; 3]],
        };

        assert_eq!(
            model_instance_preview_key(&instance, Some(&registry)).unwrap(),
            AuthoredModelPreviewKey {
                asset_id,
                loader_flags: SMS_SM_J3D_ACT_MODEL_LOAD_FLAGS,
            }
        );

        placement.export_mode = ModelInstanceExportMode::MapTerrain;
        instance.placement = placement.clone();
        assert_eq!(
            model_instance_preview_key(&instance, Some(&registry))
                .unwrap()
                .loader_flags,
            SMS_MAP_MODEL_LOAD_FLAGS
        );

        placement.export_mode = ModelInstanceExportMode::Skybox;
        instance.placement = placement.clone();
        assert_eq!(
            model_instance_preview_key(&instance, Some(&registry))
                .unwrap()
                .loader_flags,
            SMS_DEFAULT_OBJECT_MODEL_LOAD_FLAGS
        );

        placement.export_mode = ModelInstanceExportMode::StockMapObjBase;
        placement.stock_map_obj_resource = "VerifiedBlock".to_string();
        instance.placement = placement;
        assert_eq!(
            model_instance_preview_key(&instance, Some(&registry))
                .unwrap()
                .loader_flags,
            registry.map_obj_resources[0].load_flags
        );
    }

    #[test]
    fn explicit_map_terrain_mode_replaces_world_model_and_appends_collision() {
        let temporary = tempfile::tempdir().unwrap();
        let content = temporary.path().join("Content");
        let catalog = ModelAssetCatalog::open_content_root(&content).unwrap();
        let source = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(
            "../../crates/sms-authoring/tests/fixtures/gltf/valid/minimal-external/model.gltf",
        );
        let imported =
            sms_authoring::import_model(source, &sms_authoring::ModelImportOptions::default())
                .unwrap()
                .asset;
        let entry = catalog
            .create_asset("Geometry/test.smsmodel", &imported)
            .unwrap();
        let mut placement = ModelInstancePlacement::new(entry.id, "TestInstance");
        placement.export_mode = ModelInstanceExportMode::MapTerrain;
        let instance = EditorModelInstance {
            stage_id: "test11".to_string(),
            placement,
            local_bounds: model_document_bounds(&imported).unwrap(),
        };

        let edits = SmsEditorApp::stage_edits_with_model_instances(
            &content,
            &[instance],
            &sms_scene::StageArchiveEdits::default(),
        )
        .unwrap();
        assert_eq!(edits.models.len(), 1);
        assert_eq!(edits.models[0].raw_resource_path, b"map/map/map.bmd");
        assert_eq!(edits.collisions.len(), 1);
        assert_eq!(edits.collisions[0].raw_resource_path, b"map/map.col");
    }

    #[test]
    fn skybox_mode_authors_global_model_and_typed_sky_actor_without_collision() {
        let temporary = tempfile::tempdir().unwrap();
        let content = temporary.path().join("Content");
        let catalog = ModelAssetCatalog::open_content_root(&content).unwrap();
        let source = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(
            "../../crates/sms-authoring/tests/fixtures/gltf/valid/minimal-external/model.gltf",
        );
        let imported =
            sms_authoring::import_model(source, &sms_authoring::ModelImportOptions::default())
                .unwrap()
                .asset;
        let entry = catalog
            .create_asset("Environment/sky.smsmodel", &imported)
            .unwrap();
        let mut placement = ModelInstancePlacement::new(entry.id, "AuthoredSky");
        placement.export_mode = ModelInstanceExportMode::Skybox;
        let instance = EditorModelInstance {
            stage_id: "custom0".to_string(),
            placement,
            local_bounds: model_document_bounds(&imported).unwrap(),
        };

        let edits = SmsEditorApp::stage_edits_with_model_instances_for_archive(
            &content,
            &[instance],
            &sms_scene::StageArchiveEdits::default(),
            Some(&runtime_export_test_archive()),
        )
        .unwrap();
        assert_eq!(
            edits
                .models
                .iter()
                .map(|edit| edit.raw_resource_path.as_slice())
                .collect::<Vec<_>>(),
            [
                b"map/map/sky.bmd".as_slice(),
                b"map/map/sky.bmt",
                b"map/map/reflectsky.bmd",
            ]
        );
        assert_eq!(edits.models[1].document.file_type, *b"bmt3");
        assert_eq!(edits.models[2].document, edits.models[0].document);
        assert!(edits.collisions.is_empty());
        assert_eq!(edits.placement_inserts.len(), 1);
        assert_eq!(edits.placement_inserts[0].record.type_name, "Sky");
        assert_eq!(
            edits.placement_inserts[0].raw_resource_path,
            b"map/scene.bin"
        );
    }

    #[test]
    fn legacy_sky_only_project_edit_derives_the_runtime_reflection_bundle() {
        let source = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(
            "../../crates/sms-authoring/tests/fixtures/gltf/valid/minimal-external/model.gltf",
        );
        let sky =
            sms_authoring::import_model(source, &sms_authoring::ModelImportOptions::default())
                .unwrap()
                .asset
                .compile_bmd_document()
                .unwrap();
        let old_reflection = sms_authoring::built_in_blank_stage_proxy(b"old-reflection")
            .compile_bmd_document()
            .unwrap();
        let mut archive = runtime_export_test_archive();
        archive
            .insert_resource(
                b"map/map/reflectsky.bmd".to_vec(),
                StageResourceDocument::Model(old_reflection),
            )
            .unwrap();

        let mut base = sms_scene::StageArchiveEdits::default();
        base.upsert_resource(
            b"map/map/sky.bmd".to_vec(),
            StageResourceDocument::Model(sky.clone()),
        );
        base.remove_resource(b"map/map/sky.bmt".to_vec());

        let edits = SmsEditorApp::stage_edits_with_model_instances_from_snapshot(
            &BTreeMap::new(),
            &[],
            &base,
            Some(&archive),
            None,
        )
        .unwrap();
        let material_table = edits
            .models
            .iter()
            .find(|edit| edit.raw_resource_path == b"map/map/sky.bmt")
            .expect("derived shared sky material table");
        assert_eq!(material_table.document.file_type, *b"bmt3");
        let reflection = edits
            .models
            .iter()
            .find(|edit| edit.raw_resource_path == b"map/map/reflectsky.bmd")
            .expect("derived ReflectSky helper");
        assert_eq!(reflection.document, sky);
    }

    #[test]
    fn default_runtime_mode_leaves_terrain_bmd_untouched_and_deduplicates_assets() {
        let temporary = tempfile::tempdir().unwrap();
        let content = temporary.path().join("Content");
        let catalog = ModelAssetCatalog::open_content_root(&content).unwrap();
        let source = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(
            "../../crates/sms-authoring/tests/fixtures/gltf/valid/minimal-external/model.gltf",
        );
        let imported =
            sms_authoring::import_model(source, &sms_authoring::ModelImportOptions::default())
                .unwrap()
                .asset;
        let entry = catalog
            .create_asset("Geometry/runtime.smsmodel", &imported)
            .unwrap();
        let bounds = model_document_bounds(&imported).unwrap();
        let instances = [
            EditorModelInstance {
                stage_id: "test11".to_string(),
                placement: ModelInstancePlacement::new(entry.id, "Runtime A"),
                local_bounds: bounds,
            },
            EditorModelInstance {
                stage_id: "test11".to_string(),
                placement: ModelInstancePlacement::new(entry.id, "Runtime B"),
                local_bounds: bounds,
            },
        ];
        let archive = runtime_export_test_archive();
        let runtime_parent = runtime_actor_parent_path(&archive).unwrap();

        let edits = SmsEditorApp::stage_edits_with_model_instances_for_archive(
            &content,
            &instances,
            &sms_scene::StageArchiveEdits::default(),
            Some(&archive),
        )
        .unwrap();

        assert_eq!(edits.models.len(), 1, "one BMD per distinct asset");
        assert_eq!(
            edits.models[0].raw_resource_path,
            format!(
                "mapobj/{}/default.bmd",
                model_runtime_resource_key(entry.id)
            )
            .into_bytes()
        );
        assert!(edits
            .models
            .iter()
            .all(|edit| edit.raw_resource_path != b"map/map/map.bmd"));
        assert_eq!(edits.resources.len(), 1);
        assert_eq!(edits.resources[0].raw_resource_path, b"map/tables.bin");
        let StageResourceDocument::Placement(tables) = &edits.resources[0].document else {
            panic!("authored tables resource is not placement data");
        };
        let JDramaRecordPayload::Group { children, .. } = &tables.root.payload else {
            panic!("authored tables root is not a group");
        };
        assert_eq!(children.len(), 1, "one ObjChara per distinct asset");
        assert_eq!(children[0].type_name, "ObjChara");
        assert_eq!(edits.placement_inserts.len(), 2);
        assert!(edits
            .placement_inserts
            .iter()
            .all(|insert| insert.record.type_name == "SmJ3DAct"));
        assert!(edits
            .placement_inserts
            .iter()
            .all(|insert| insert.parent_record_path == runtime_parent));
        assert_eq!(edits.collisions.len(), 1);
        assert_eq!(edits.collisions[0].raw_resource_path, b"map/map.col");
    }

    #[test]
    fn runtime_actor_parent_requires_one_exact_scheduled_map_group() {
        let missing =
            runtime_actor_parent_path(&runtime_export_test_archive_with_map_groups(0)).unwrap_err();
        assert!(missing.contains("IdxGroup"), "{missing}");
        assert!(missing.contains(SMS_RUNTIME_MAP_GROUP_NAME), "{missing}");
        assert!(missing.contains("never be scheduled"), "{missing}");

        let ambiguous =
            runtime_actor_parent_path(&runtime_export_test_archive_with_map_groups(2)).unwrap_err();
        assert!(ambiguous.contains("ambiguous"), "{ambiguous}");
        assert!(
            ambiguous.contains(SMS_RUNTIME_MAP_GROUP_NAME),
            "{ambiguous}"
        );
    }

    #[test]
    fn separate_actor_survives_full_stage_archive_rebuild_and_reparse() {
        let temporary = tempfile::tempdir().unwrap();
        let content = temporary.path().join("Content");
        let catalog = ModelAssetCatalog::open_content_root(&content).unwrap();
        let source = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(
            "../../crates/sms-authoring/tests/fixtures/gltf/valid/minimal-external/model.gltf",
        );
        let imported =
            sms_authoring::import_model(source, &sms_authoring::ModelImportOptions::default())
                .unwrap()
                .asset;
        let entry = catalog
            .create_asset("Geometry/roundtrip.smsmodel", &imported)
            .unwrap();
        let instance = EditorModelInstance {
            stage_id: "test11".to_string(),
            placement: ModelInstancePlacement::new(entry.id, "Round trip"),
            local_bounds: model_document_bounds(&imported).unwrap(),
        };

        let terrain_model = imported.compile_bmd_document().unwrap();
        let base_collision = imported.collision.as_ref().unwrap().to_col_file().unwrap();
        let base_collision_vertices = base_collision.vertices().len();
        let mut archive = runtime_export_test_archive();
        archive
            .insert_resource(
                b"map/map/map.bmd".to_vec(),
                StageResourceDocument::Model(terrain_model.clone()),
            )
            .unwrap();
        archive
            .insert_resource(
                b"map/map.col".to_vec(),
                StageResourceDocument::Collision(base_collision),
            )
            .unwrap();
        let source_path = temporary.path().join("files/data/scene/test11.szs");
        fs::create_dir_all(source_path.parent().unwrap()).unwrap();
        fs::write(&source_path, archive.encode().unwrap()).unwrap();

        let edits = SmsEditorApp::stage_edits_with_model_instances_for_archive(
            &content,
            std::slice::from_ref(&instance),
            &sms_scene::StageArchiveEdits::default(),
            Some(&archive),
        )
        .unwrap();
        let document = sms_scene::StageDocument {
            stage_id: "test11".to_string(),
            base_root: temporary.path().to_path_buf(),
            assets: Vec::new(),
            objects: Vec::new(),
            changed_files: BTreeMap::new(),
            stage_archive: Some(archive),
            stage_archive_source_path: Some(source_path),
            archive_edits: sms_scene::StageArchiveEdits::default(),
            route_authoring: None,
            registry: None,
            load_issues: Vec::new(),
            lighting: sms_scene::StageLighting::default(),
            actor_previews: BTreeMap::new(),
            loaded_project: None,
        };
        let rebuilt = document.build_stage_archive_with_edits(&edits).unwrap();
        let reopened = SourceFreeStageArchive::parse(&rebuilt).unwrap();
        assert_eq!(reopened.encode().unwrap(), rebuilt);

        let runtime_key = model_runtime_resource_key(entry.id);
        assert!(matches!(
            reopened.resource(format!("mapobj/{runtime_key}/default.bmd").as_bytes()),
            Some(StageResourceDocument::Model(_))
        ));
        let StageResourceDocument::Model(reopened_terrain) =
            reopened.resource(b"map/map/map.bmd").unwrap()
        else {
            panic!("terrain BMD has the wrong resource type");
        };
        assert_eq!(
            reopened_terrain.to_bytes().unwrap(),
            terrain_model.to_bytes().unwrap(),
            "separate actor export must leave the terrain BMD byte-identical"
        );

        let StageResourceDocument::Collision(reopened_collision) =
            reopened.resource(b"map/map.col").unwrap()
        else {
            panic!("map collision has the wrong resource type");
        };
        assert!(reopened_collision.vertices().len() > base_collision_vertices);

        let StageResourceDocument::Placement(tables) =
            reopened.resource(b"map/tables.bin").unwrap()
        else {
            panic!("tables resource has the wrong type");
        };
        let character = find_test_record(&tables.root, "ObjChara").unwrap();
        let JDramaRecordPayload::Fields { fields } = &character.payload else {
            panic!("ObjChara has the wrong payload");
        };
        assert_eq!(
            fields[0].value,
            JDramaFieldValue::String(format!("/scene/mapObj/{runtime_key}"))
        );

        let StageResourceDocument::Placement(scene) = reopened.resource(b"map/scene.bin").unwrap()
        else {
            panic!("scene resource has the wrong type");
        };
        let runtime_parent = reopened
            .find_named_group_record_path(
                b"map/scene.bin",
                SMS_RUNTIME_MAP_GROUP_TYPE,
                SMS_RUNTIME_MAP_GROUP_NAME,
            )
            .unwrap()
            .unwrap();
        let actor_parent = find_test_record_at_path(&scene.root, &runtime_parent).unwrap();
        assert_eq!(actor_parent.type_name, SMS_RUNTIME_MAP_GROUP_TYPE);
        assert_eq!(actor_parent.name, SMS_RUNTIME_MAP_GROUP_NAME);
        let JDramaRecordPayload::Group { children, .. } = &actor_parent.payload else {
            panic!("runtime actor parent is not a group");
        };
        let actor = children
            .iter()
            .find(|child| child.type_name == "SmJ3DAct")
            .expect("the source-free reopen lost the scheduled SmJ3DAct child");
        let JDramaRecordPayload::Actor { character_name, .. } = &actor.payload else {
            panic!("SmJ3DAct has the wrong payload");
        };
        assert_eq!(character_name, &runtime_character_name(entry.id));
    }

    #[test]
    fn separate_runtime_export_checks_the_exact_sm_j3d_act_loader_profile() {
        let temporary = tempfile::tempdir().unwrap();
        let content = temporary.path().join("Content");
        let catalog = ModelAssetCatalog::open_content_root(&content).unwrap();
        let source = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(
            "../../crates/sms-authoring/tests/fixtures/gltf/valid/material-textures/model.gltf",
        );
        let mut imported =
            sms_authoring::import_model(source, &sms_authoring::ModelImportOptions::default())
                .unwrap()
                .asset;
        imported.acknowledged_diagnostics = imported
            .diagnostics
            .iter()
            .filter(|diagnostic| diagnostic.acknowledgement_required)
            .map(|diagnostic| diagnostic.code)
            .collect();
        imported.materials[0].gx.indirect.enabled = true;
        let diagnostics =
            loader_diagnostics_for_document(&imported, SMS_SM_J3D_ACT_MODEL_LOAD_FLAGS);
        assert!(diagnostics.iter().any(|diagnostic| {
            diagnostic.code == "loader-ignores-indirect"
                && diagnostic.message.contains("0x00240000")
        }));
        let entry = catalog
            .create_asset("Geometry/runtime-loader.smsmodel", &imported)
            .unwrap();
        let instance = EditorModelInstance {
            stage_id: "test11".to_string(),
            placement: ModelInstancePlacement::new(entry.id, "Runtime loader"),
            local_bounds: model_document_bounds(&imported).unwrap(),
        };

        let edits = SmsEditorApp::stage_edits_with_model_instances_for_archive(
            &content,
            &[instance],
            &sms_scene::StageArchiveEdits::default(),
            Some(&runtime_export_test_archive()),
        )
        .unwrap();
        assert_eq!(edits.models.len(), 1, "warnings do not block export");
    }

    #[test]
    fn standalone_actor_budget_rejects_oversized_tex1_but_not_map_terrain() {
        let source = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(
            "../../crates/sms-authoring/tests/fixtures/gltf/valid/minimal-external/model.gltf",
        );
        let imported =
            sms_authoring::import_model(source, &sms_authoring::ModelImportOptions::default())
                .unwrap()
                .asset;
        let mut model = imported.compile_bmd_document().unwrap();
        model
            .sections
            .iter_mut()
            .find(|section| section.tag() == *b"TEX1")
            .unwrap()
            .declared_size = (STANDALONE_ACTOR_TEX1_SAFETY_BUDGET + 0x20) as u32;
        let placement = ModelInstancePlacement::new(AssetId::new(), "oversized");
        let error =
            validate_standalone_actor_model_budget(&placement, &model, "SmJ3DAct").unwrap_err();
        assert!(error.contains("24 MiB MEM1"), "{error}");
        assert!(error.contains("not a BMD format limit"), "{error}");
        assert!(error.contains("map-terrain mode"), "{error}");

        // Map-terrain export intentionally does not call the standalone actor
        // budget check because it replaces the stage's world model.
        let mut terrain = placement;
        terrain.export_mode = ModelInstanceExportMode::MapTerrain;
        assert_eq!(
            model_instance_loader_flags(&terrain, None).unwrap(),
            SMS_MAP_MODEL_LOAD_FLAGS
        );
    }

    #[test]
    fn verified_stock_map_obj_base_exports_exact_resources_and_local_collision() {
        let temporary = tempfile::tempdir().unwrap();
        let content = temporary.path().join("Content");
        let catalog = ModelAssetCatalog::open_content_root(&content).unwrap();
        let source = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(
            "../../crates/sms-authoring/tests/fixtures/gltf/valid/minimal-external/model.gltf",
        );
        let imported =
            sms_authoring::import_model(source, &sms_authoring::ModelImportOptions::default())
                .unwrap()
                .asset;
        let expected_collision_vertices = imported.collision.as_ref().unwrap().vertices.clone();
        let entry = catalog
            .create_asset("Geometry/stock.smsmodel", &imported)
            .unwrap();
        let mut placement = ModelInstancePlacement::new(entry.id, "Stock block");
        placement.export_mode = ModelInstanceExportMode::StockMapObjBase;
        placement.stock_map_obj_resource = "VerifiedBlock".to_string();
        placement.transform = transform_to_matrix(Transform {
            translation: [4_000.0, 5_000.0, 6_000.0],
            rotation_degrees: [10.0, 20.0, 30.0],
            scale: [2.0, 3.0, 4.0],
        });
        placement.collision_surface_override = Some(CollisionSurface {
            surface_type: 0x4004,
            attribute_0: 7,
            attribute_1: 9,
            data: Some(11),
        });
        let instance = EditorModelInstance {
            stage_id: "test11".to_string(),
            placement,
            local_bounds: model_document_bounds(&imported).unwrap(),
        };
        let archive = runtime_export_test_archive();
        let registry = stock_export_test_registry();

        let edits = SmsEditorApp::stage_edits_with_model_instances_for_archive_and_registry(
            &content,
            &[instance],
            &sms_scene::StageArchiveEdits::default(),
            Some(&archive),
            Some(&registry),
        )
        .unwrap();

        assert_eq!(edits.models.len(), 1);
        assert_eq!(
            edits.models[0].raw_resource_path,
            b"mapobj/VerifiedBlock.bmd"
        );
        assert_eq!(edits.collisions.len(), 1);
        assert_eq!(
            edits.collisions[0].raw_resource_path,
            b"mapobj/VerifiedBlockCollision.col"
        );
        assert!(edits
            .collisions
            .iter()
            .all(|edit| edit.raw_resource_path != b"map/map.col"));
        assert_eq!(
            edits.collisions[0]
                .document
                .vertices()
                .iter()
                .map(|vertex| vertex.position)
                .collect::<Vec<_>>(),
            expected_collision_vertices,
            "stock collision must remain actor-local rather than receiving the placement transform"
        );
        assert!(edits.collisions[0].document.groups().iter().all(|group| {
            group.surface_type == 0x4004
                && group.triangles.iter().all(|triangle| {
                    triangle.attribute_0 == 7
                        && triangle.attribute_1 == 9
                        && triangle.data == Some(11)
                })
        }));

        assert_eq!(edits.placement_inserts.len(), 1);
        let insertion = &edits.placement_inserts[0];
        assert_eq!(
            insertion.parent_record_path,
            archive
                .find_group_record_path(b"map/scene.bin", "IdxGroup", Some(3))
                .unwrap()
                .unwrap()
        );
        assert_eq!(insertion.record.type_name, "MapObjBase");
        let JDramaRecordPayload::Actor {
            transform, fields, ..
        } = &insertion.record.payload
        else {
            panic!("stock placement is not a typed actor");
        };
        assert_eq!(transform.translation, [4_000.0, 5_000.0, 6_000.0]);
        assert_eq!(fields.len(), 1);
        assert_eq!(fields[0].name, "resource_name");
        assert_eq!(
            fields[0].value,
            JDramaFieldValue::String("VerifiedBlock".to_string())
        );
    }

    #[test]
    fn stock_map_obj_base_rejects_unknown_and_conflicting_slots() {
        let temporary = tempfile::tempdir().unwrap();
        let content = temporary.path().join("Content");
        let catalog = ModelAssetCatalog::open_content_root(&content).unwrap();
        let source = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(
            "../../crates/sms-authoring/tests/fixtures/gltf/valid/minimal-external/model.gltf",
        );
        let imported =
            sms_authoring::import_model(source, &sms_authoring::ModelImportOptions::default())
                .unwrap()
                .asset;
        let first = catalog.create_asset("first.smsmodel", &imported).unwrap();
        let second = catalog.create_asset("second.smsmodel", &imported).unwrap();
        let archive = runtime_export_test_archive();
        let registry = stock_export_test_registry();
        let make_instance = |asset_id, resource: &str| {
            let mut placement = ModelInstancePlacement::new(asset_id, "Stock block");
            placement.export_mode = ModelInstanceExportMode::StockMapObjBase;
            placement.stock_map_obj_resource = resource.to_string();
            EditorModelInstance {
                stage_id: "test11".to_string(),
                placement,
                local_bounds: model_document_bounds(&imported).unwrap(),
            }
        };

        let unknown = make_instance(first.id, "verifiedblock");
        let error = SmsEditorApp::stage_edits_with_model_instances_for_archive_and_registry(
            &content,
            &[unknown],
            &sms_scene::StageArchiveEdits::default(),
            Some(&archive),
            Some(&registry),
        )
        .unwrap_err();
        assert!(
            error.contains("unknown stock MapObjBase resource"),
            "{error}"
        );
        assert!(error.contains("case-sensitive"), "{error}");

        let conflicting = [
            make_instance(first.id, "VerifiedBlock"),
            make_instance(second.id, "VerifiedBlock"),
        ];
        let error = SmsEditorApp::stage_edits_with_model_instances_for_archive_and_registry(
            &content,
            &conflicting,
            &sms_scene::StageArchiveEdits::default(),
            Some(&archive),
            Some(&registry),
        )
        .unwrap_err();
        assert!(error.contains("assigned to both asset"), "{error}");
    }

    #[test]
    fn disabled_stock_collision_replaces_the_fixed_resource_with_an_empty_col() {
        let temporary = tempfile::tempdir().unwrap();
        let content = temporary.path().join("Content");
        let catalog = ModelAssetCatalog::open_content_root(&content).unwrap();
        let source = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(
            "../../crates/sms-authoring/tests/fixtures/gltf/valid/minimal-external/model.gltf",
        );
        let imported =
            sms_authoring::import_model(source, &sms_authoring::ModelImportOptions::default())
                .unwrap()
                .asset;
        let entry = catalog
            .create_asset("disabled.smsmodel", &imported)
            .unwrap();
        let mut placement = ModelInstancePlacement::new(entry.id, "No collision");
        placement.export_mode = ModelInstanceExportMode::StockMapObjBase;
        placement.stock_map_obj_resource = "VerifiedBlock".to_string();
        placement.collision_enabled = false;
        let instance = EditorModelInstance {
            stage_id: "test11".to_string(),
            placement,
            local_bounds: model_document_bounds(&imported).unwrap(),
        };

        let edits = SmsEditorApp::stage_edits_with_model_instances_for_archive_and_registry(
            &content,
            &[instance],
            &sms_scene::StageArchiveEdits::default(),
            Some(&runtime_export_test_archive()),
            Some(&stock_export_test_registry()),
        )
        .unwrap();
        assert_eq!(edits.collisions.len(), 1);
        assert!(edits.collisions[0].document.vertices().is_empty());
        assert!(edits.collisions[0].document.groups().is_empty());
    }

    #[test]
    fn unsafe_stock_slot_metadata_is_rejected_before_authoring() {
        let temporary = tempfile::tempdir().unwrap();
        let content = temporary.path().join("Content");
        let catalog = ModelAssetCatalog::open_content_root(&content).unwrap();
        let source = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(
            "../../crates/sms-authoring/tests/fixtures/gltf/valid/minimal-external/model.gltf",
        );
        let imported =
            sms_authoring::import_model(source, &sms_authoring::ModelImportOptions::default())
                .unwrap()
                .asset;
        let entry = catalog.create_asset("unsafe.smsmodel", &imported).unwrap();
        let mut placement = ModelInstancePlacement::new(entry.id, "Unsafe slot");
        placement.export_mode = ModelInstanceExportMode::StockMapObjBase;
        placement.stock_map_obj_resource = "VerifiedBlock".to_string();
        let instance = EditorModelInstance {
            stage_id: "test11".to_string(),
            placement,
            local_bounds: model_document_bounds(&imported).unwrap(),
        };
        let archive = runtime_export_test_archive();

        let mut registry = stock_export_test_registry();
        registry.map_obj_resources[0].required_manager_name = "absent manager".to_string();
        let error = SmsEditorApp::stage_edits_with_model_instances_for_archive_and_registry(
            &content,
            std::slice::from_ref(&instance),
            &sms_scene::StageArchiveEdits::default(),
            Some(&archive),
            Some(&registry),
        )
        .unwrap_err();
        assert!(
            error.contains("requires the exact scene manager record"),
            "{error}"
        );
        assert!(error.contains("absent manager"), "{error}");

        registry.map_obj_resources[0].required_manager_name =
            "fixture map object manager".to_string();
        registry.map_obj_resources[0].has_hold_dependency = true;
        let error = SmsEditorApp::stage_edits_with_model_instances_for_archive_and_registry(
            &content,
            std::slice::from_ref(&instance),
            &sms_scene::StageArchiveEdits::default(),
            Some(&archive),
            Some(&registry),
        )
        .unwrap_err();
        assert!(error.contains("TMapObjData::mHold"), "{error}");

        registry.map_obj_resources[0].has_hold_dependency = false;
        registry.map_obj_resources[0].has_move_dependency = true;
        let error = SmsEditorApp::stage_edits_with_model_instances_for_archive_and_registry(
            &content,
            std::slice::from_ref(&instance),
            &sms_scene::StageArchiveEdits::default(),
            Some(&archive),
            Some(&registry),
        )
        .unwrap_err();
        assert!(error.contains("TMapObjData::mMove"), "{error}");

        registry.map_obj_resources[0].has_move_dependency = false;
        registry.map_obj_resources[0].object_flags = 0x80;
        let error = SmsEditorApp::stage_edits_with_model_instances_for_archive_and_registry(
            &content,
            std::slice::from_ref(&instance),
            &sms_scene::StageArchiveEdits::default(),
            Some(&archive),
            Some(&registry),
        )
        .unwrap_err();
        assert!(error.contains("damage-height"), "{error}");

        registry.map_obj_resources[0].object_flags = 0;
        registry.map_obj_resources[0].uses_resource_name_model_fallback = false;
        let error = SmsEditorApp::stage_edits_with_model_instances_for_archive_and_registry(
            &content,
            std::slice::from_ref(&instance),
            &sms_scene::StageArchiveEdits::default(),
            Some(&archive),
            Some(&registry),
        )
        .unwrap_err();
        assert!(
            error.contains("compiled animation/model metadata"),
            "{error}"
        );

        registry.map_obj_resources[0].uses_resource_name_model_fallback = true;
        registry.map_obj_resources[0].primary_model = None;
        let error = SmsEditorApp::stage_edits_with_model_instances_for_archive_and_registry(
            &content,
            &[instance],
            &sms_scene::StageArchiveEdits::default(),
            Some(&archive),
            Some(&registry),
        )
        .unwrap_err();
        assert!(
            error.contains("does not instantiate a primary model"),
            "{error}"
        );
    }

    #[test]
    fn stage_export_requires_persisted_import_diagnostic_acknowledgements() {
        let temporary = tempfile::tempdir().unwrap();
        let content = temporary.path().join("Content");
        let catalog = ModelAssetCatalog::open_content_root(&content).unwrap();
        let source = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../crates/sms-authoring/tests/fixtures/gltf/valid/pbr-diagnostics/model.glb");
        let imported =
            sms_authoring::import_model(source, &sms_authoring::ModelImportOptions::default())
                .unwrap()
                .asset;
        assert!(!imported.unacknowledged_required_diagnostics().is_empty());
        let entry = catalog.create_asset("pbr.smsmodel", &imported).unwrap();
        let instance = EditorModelInstance {
            stage_id: "test11".to_string(),
            placement: ModelInstancePlacement::new(entry.id, "PbrInstance"),
            local_bounds: model_document_bounds(&imported).unwrap(),
        };
        let error = SmsEditorApp::stage_edits_with_model_instances(
            &content,
            std::slice::from_ref(&instance),
            &sms_scene::StageArchiveEdits::default(),
        )
        .unwrap_err();
        assert!(error.contains("unacknowledged import diagnostics"));

        let mut acknowledged = catalog.load_asset(entry.id).unwrap();
        acknowledged.acknowledged_diagnostics = acknowledged
            .diagnostics
            .iter()
            .filter(|diagnostic| diagnostic.acknowledgement_required)
            .map(|diagnostic| diagnostic.code)
            .collect();
        catalog.save_asset(entry.id, &acknowledged).unwrap();
        let archive = runtime_export_test_archive();
        SmsEditorApp::stage_edits_with_model_instances_for_archive(
            &content,
            &[instance],
            &sms_scene::StageArchiveEdits::default(),
            Some(&archive),
        )
        .unwrap();
    }

    #[test]
    fn model_export_consumes_a_preloaded_content_snapshot_without_reopening_disk() {
        let temporary = tempfile::tempdir().unwrap();
        let content = temporary.path().join("Content");
        let catalog = ModelAssetCatalog::open_content_root(&content).unwrap();
        let source = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(
            "../../crates/sms-authoring/tests/fixtures/gltf/valid/minimal-external/model.gltf",
        );
        let imported =
            sms_authoring::import_model(source, &sms_authoring::ModelImportOptions::default())
                .unwrap()
                .asset;
        let entry = catalog
            .create_asset("snapshot.smsmodel", &imported)
            .unwrap();
        let instance = EditorModelInstance {
            stage_id: "test11".to_string(),
            placement: ModelInstancePlacement::new(entry.id, "snapshot"),
            local_bounds: model_document_bounds(&imported).unwrap(),
        };
        let snapshot =
            SmsEditorApp::load_model_asset_snapshot(&content, std::slice::from_ref(&instance))
                .unwrap();
        fs::remove_file(content.join(&entry.relative_path)).unwrap();

        let edits = SmsEditorApp::stage_edits_with_model_instances_from_snapshot(
            &snapshot,
            &[instance],
            &sms_scene::StageArchiveEdits::default(),
            Some(&runtime_export_test_archive()),
            None,
        )
        .unwrap();
        assert_eq!(edits.models.len(), 1);
    }

    #[test]
    fn model_export_honors_managed_build_cancellation_before_compilation() {
        let cancelled = AtomicBool::new(true);
        let error = SmsEditorApp::stage_edits_with_model_instances_from_snapshot_cancellable(
            &BTreeMap::new(),
            &[],
            &sms_scene::StageArchiveEdits::default(),
            None,
            None,
            &cancelled,
        )
        .unwrap_err();
        assert!(managed_build::is_cancelled_error(&error), "{error}");
    }

    #[test]
    fn background_build_locks_content_catalog_mutations() {
        let (_sender, receiver) = std::sync::mpsc::channel();
        let mut app = SmsEditorApp {
            background_receiver: Some(receiver),
            ..SmsEditorApp::default()
        };
        assert!(!app.content_catalog_mutation_allowed("save the model asset"));
        assert!(app
            .model_editor_error
            .as_deref()
            .is_some_and(|message| message.contains("Content snapshot")));
    }

    fn find_test_record<'a>(record: &'a JDramaRecord, type_name: &str) -> Option<&'a JDramaRecord> {
        if record.type_name == type_name {
            return Some(record);
        }
        let JDramaRecordPayload::Group { children, .. } = &record.payload else {
            return None;
        };
        children
            .iter()
            .find_map(|child| find_test_record(child, type_name))
    }

    fn find_test_record_at_path<'a>(
        mut record: &'a JDramaRecord,
        path: &[usize],
    ) -> Option<&'a JDramaRecord> {
        for index in path {
            let JDramaRecordPayload::Group { children, .. } = &record.payload else {
                return None;
            };
            record = children.get(*index)?;
        }
        Some(record)
    }

    fn runtime_export_test_archive() -> SourceFreeStageArchive {
        runtime_export_test_archive_with_map_groups(1)
    }

    fn runtime_export_test_archive_with_map_groups(
        map_group_count: usize,
    ) -> SourceFreeStageArchive {
        let map_obj_manager = JDramaRecord::new(
            "MapObjManager",
            "fixture map object manager",
            JDramaRecordPayload::Fields {
                fields: vec![
                    JDramaField {
                        name: "character_name".to_string(),
                        value: JDramaFieldValue::String("fixture manager character".to_string()),
                    },
                    JDramaField {
                        name: "capacity".to_string(),
                        value: JDramaFieldValue::U32(300),
                    },
                    JDramaField {
                        name: "clip_distance".to_string(),
                        value: JDramaFieldValue::F32(5_000.0),
                    },
                    JDramaField {
                        name: "clip_radius".to_string(),
                        value: JDramaFieldValue::F32(500.0),
                    },
                ],
            },
        )
        .unwrap();
        let map = JDramaRecord::new(
            "Map",
            "fixture map",
            JDramaRecordPayload::Fields {
                fields: vec![
                    JDramaField {
                        name: "translucent_group_count".to_string(),
                        value: JDramaFieldValue::U32(0),
                    },
                    JDramaField {
                        name: "collision_grid_width".to_string(),
                        value: JDramaFieldValue::I32(60),
                    },
                    JDramaField {
                        name: "collision_grid_height".to_string(),
                        value: JDramaFieldValue::I32(60),
                    },
                    JDramaField {
                        name: "collision_triangle_capacity".to_string(),
                        value: JDramaFieldValue::I32(12_000),
                    },
                    JDramaField {
                        name: "collision_list_capacity".to_string(),
                        value: JDramaFieldValue::I32(30_000),
                    },
                    JDramaField {
                        name: "collision_warp_capacity".to_string(),
                        value: JDramaFieldValue::I32(3_000),
                    },
                    JDramaField {
                        name: "warp_pair_count".to_string(),
                        value: JDramaFieldValue::U32(0),
                    },
                ],
            },
        )
        .unwrap();
        let map_group = JDramaRecord::new(
            SMS_RUNTIME_MAP_GROUP_TYPE,
            SMS_RUNTIME_MAP_GROUP_NAME,
            JDramaRecordPayload::Group {
                fields: vec![JDramaField {
                    name: "group_index".to_string(),
                    value: JDramaFieldValue::U32(0),
                }],
                children: vec![map],
            },
        )
        .unwrap();
        let object_group = JDramaRecord::new(
            "IdxGroup",
            "object group",
            JDramaRecordPayload::Group {
                fields: vec![JDramaField {
                    name: "group_index".to_string(),
                    value: JDramaFieldValue::U32(3),
                }],
                children: Vec::new(),
            },
        )
        .unwrap();
        let sky_group = JDramaRecord::new(
            "IdxGroup",
            "sky group",
            JDramaRecordPayload::Group {
                fields: vec![JDramaField {
                    name: "group_index".to_string(),
                    value: JDramaFieldValue::U32(1),
                }],
                children: Vec::new(),
            },
        )
        .unwrap();
        let strategy = JDramaRecord::new(
            "Strategy",
            "strategy",
            JDramaRecordPayload::Group {
                fields: Vec::new(),
                children: {
                    let mut children = vec![map_group; map_group_count];
                    children.push(sky_group);
                    children.push(object_group);
                    children
                },
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
                children: vec![map_obj_manager, mar_scene],
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

    fn stock_export_test_registry() -> ObjectRegistry {
        ObjectRegistry {
            map_obj_resources: vec![sms_schema::MapObjResourceDefinition {
                resource_name: "VerifiedBlock".to_string(),
                actor_type: 0x4000_0003,
                object_flags: 0,
                required_manager_name: "fixture map object manager".to_string(),
                has_hold_dependency: false,
                has_move_dependency: false,
                uses_resource_name_model_fallback: true,
                primary_model: Some("/scene/mapObj/VerifiedBlock.bmd".to_string()),
                animation_resources: Vec::new(),
                hold_model_path: None,
                move_bck_path: None,
                load_flags: 0x1022_0000,
                collision_resources: vec![sms_schema::MapObjCollisionResourceDefinition {
                    resource_name: "/scene/mapObj/VerifiedBlockCollision.col".to_string(),
                    flags: 1,
                    collision_kind: 1,
                    max_vertices: Some(350),
                }],
                source_file: "src/MoveBG/MapObjInit.cpp".to_string(),
            }],
            ..ObjectRegistry::default()
        }
    }
}
