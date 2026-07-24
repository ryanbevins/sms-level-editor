use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    mpsc::{self, Receiver, TryRecvError},
    Arc,
};
use std::thread;
use std::time::{Duration, Instant};

use eframe::egui;
#[cfg(test)]
use sms_formats::read_stage_asset_bytes;
use sms_formats::{
    decode_bti_texture, discover_scene_archives, mount_scene_archive, parse_jdrama_object_records,
    ColFile, J3dAlphaCompare, J3dBillboard, J3dBillboardMode, J3dBlendMode, J3dFile,
    J3dGeometryPreview, J3dJointAnimation, J3dJointTransformOverride, J3dMaterial, J3dMatrix34,
    J3dPreparedAnimatedTriangles, J3dPreviewCombineMode, J3dTexturePatternAnimation,
    J3dTextureSrtAnimation, J3dTriangle, J3dZMode, JpaEffect, SceneArchiveInfo, StageAsset,
    StageAssetKind, SMS_ANIMATION_FRAMES_PER_SECOND, SMS_DEFAULT_OBJECT_MODEL_LOAD_FLAGS,
    SMS_MAP_MODEL_LOAD_FLAGS, SMS_POLLUTION_MODEL_LOAD_FLAGS,
};
use sms_render::{
    gx_blend_compatibility, GxBlendCompatibility, RenderScene, RendererConfig, ViewportRenderer,
};
use sms_scene::{
    AssetRef, AssetRole, DialogueAuthoringDocument, DialogueGameConsumerIndex, DialogueRouteIndex,
    ObjectAuthoringCatalog, ObjectAuthoringCatalogWarning, ProjectDialogueLibrary,
    RouteAuthoringDocument, SceneError, SceneObject, StageArchiveEdits, StageDocument,
    StageLighting, StageResourceDocument, StageResourceEdit, Transform, ValidationIssue,
    ValidationSeverity,
};
use sms_schema::{ObjectDefinition, ObjectRegistry, ParticleBindingTarget, SchemaGenerator};

mod active_placement;
mod audio_helpers;
mod audio_preview;
mod browser_settings;
mod camera;
mod content_browser;
mod content_thumbnails;
mod dialogue;
mod direct_boot;
mod document_commands;
mod dolphin_graphics;
mod game_content_index;
mod game_text;
mod goop;
mod gpu_viewport;
mod managed_build;
mod model_assets;
mod music_library;
mod outliner;
mod play_in_editor;
mod project;
mod project_ui;
mod routes;
mod scene_labels;
mod skybox_library;
mod stage_creation;
mod ui_panels;
mod viewport_ui;

use active_placement::*;
use audio_helpers::*;
use audio_preview::*;
use browser_settings::*;
use content_browser::*;
use content_thumbnails::*;
use dialogue::*;
use game_content_index::*;
use game_text::*;
use goop::*;
use model_assets::*;
use music_library::*;
use outliner::*;
use project::*;
use project_ui::{path_display_row, NewProjectDraft};
use scene_labels::*;
use skybox_library::*;
use stage_creation::{insert_authored_scene_archive, NewStageDraft};

const VIEWPORT_NEAR_CLIP: f32 = 8.0;
const FULL_DELFINO_PROGRESSION: f32 = 1.0;

pub fn run() -> eframe::Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Graffito-Editor")
            .with_inner_size([1560.0, 940.0]),
        renderer: eframe::Renderer::Wgpu,
        depth_buffer: 24,
        ..Default::default()
    };

    eframe::run_native(
        "Graffito-Editor",
        options,
        Box::new(|cc| {
            install_style(&cc.egui_ctx);
            Ok(Box::new(SmsEditorApp::new(cc)))
        }),
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EditorTool {
    Select,
    Move,
    Rotate,
    Scale,
    Goop,
    Place,
}

#[derive(Debug, Clone)]
struct ObjectPaletteDragPayload {
    factory_name: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GizmoAxis {
    X,
    Y,
    Z,
}

impl GizmoAxis {
    fn index(self) -> usize {
        match self {
            Self::X => 0,
            Self::Y => 1,
            Self::Z => 2,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::X => "X",
            Self::Y => "Y",
            Self::Z => "Z",
        }
    }

    fn world_direction(self) -> [f32; 3] {
        match self {
            Self::X => [1.0, 0.0, 0.0],
            Self::Y => [0.0, 1.0, 0.0],
            Self::Z => [0.0, 0.0, 1.0],
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct GizmoDrag {
    axis: GizmoAxis,
    tool: EditorTool,
    start_pointer: egui::Pos2,
    screen_origin: egui::Pos2,
    screen_direction: egui::Vec2,
    world_units_per_pixel: f32,
    start_transform: Transform,
}

impl EditorTool {
    fn label(self) -> &'static str {
        match self {
            Self::Select => "Select",
            Self::Move => "Move",
            Self::Rotate => "Rotate",
            Self::Scale => "Scale",
            Self::Goop => "Goop",
            Self::Place => "Place",
        }
    }

    fn after_keyboard_shortcut(self, key: egui::Key) -> Self {
        if self == Self::Goop && key != egui::Key::G {
            return self;
        }

        match key {
            egui::Key::Q => Self::Select,
            egui::Key::W => Self::Move,
            egui::Key::E => Self::Rotate,
            egui::Key::R => Self::Scale,
            egui::Key::G => Self::Goop,
            _ => self,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ViewMode {
    Lit,
    Collision,
    Objects,
}

impl ViewMode {
    fn label(self) -> &'static str {
        match self {
            Self::Lit => "Lit",
            Self::Collision => "Collision",
            Self::Objects => "Objects",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BottomTab {
    Content,
    Console,
}

#[derive(Debug, Clone, Copy)]
struct PreviewVisibility {
    environment: bool,
    goop: bool,
    effects: bool,
}

struct LoadedStage {
    base_root: String,
    requested_project_root: String,
    project_root: String,
    has_scene_index: bool,
    archives: Vec<SceneArchiveInfo>,
    registry: Option<ObjectRegistry>,
    schema_warning: Option<String>,
    object_authoring_catalog_key: Option<ObjectAuthoringCatalogCacheKey>,
    object_authoring_catalog: Arc<ObjectAuthoringCatalog>,
    object_authoring_catalog_warnings: Arc<Vec<ObjectAuthoringCatalogWarning>>,
    project_warning: Option<String>,
    document: StageDocument,
    scene: RenderScene,
    preview: Option<ModelPreview>,
    scene_labels: BTreeMap<String, SceneArchiveLabel>,
    scene_label_warning: Option<String>,
    retail_skyboxes: Vec<RetailSkyboxEntry>,
    skybox_warnings: Vec<String>,
    retail_music: Vec<RetailMusicEntry>,
    retail_sounds: Vec<RetailSoundEntry>,
    retail_dialogue_voices: Vec<RetailDialogueVoiceEntry>,
    retail_stage_audio: Vec<RetailStageAudioProfile>,
    music_warning: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ObjectAuthoringCatalogCacheKey {
    base_root: String,
    registry_fingerprint: u64,
    retail_archive_fingerprint: u64,
}

#[derive(Debug, Clone)]
struct ObjectAuthoringCatalogCache {
    key: ObjectAuthoringCatalogCacheKey,
    catalog: Arc<ObjectAuthoringCatalog>,
    warnings: Arc<Vec<ObjectAuthoringCatalogWarning>>,
}

struct LoadedSchema {
    registry: ObjectRegistry,
    object_authoring_catalog_cache: Option<ObjectAuthoringCatalogCache>,
}

struct ProjectLoadSelection {
    project_root: String,
    warning: Option<String>,
}

#[derive(Clone, Default)]
struct SceneScanResult {
    archives: Vec<SceneArchiveInfo>,
    labels: BTreeMap<String, SceneArchiveLabel>,
    label_warning: Option<String>,
    retail_skyboxes: Vec<RetailSkyboxEntry>,
    skybox_warnings: Vec<String>,
    retail_music: Vec<RetailMusicEntry>,
    retail_sounds: Vec<RetailSoundEntry>,
    retail_dialogue_voices: Vec<RetailDialogueVoiceEntry>,
    retail_stage_audio: Vec<RetailStageAudioProfile>,
    music_warning: Option<String>,
    object_authoring_catalog_cache: Option<ObjectAuthoringCatalogCache>,
}

fn build_object_authoring_catalog(
    base_root: &Path,
    archives: &[SceneArchiveInfo],
    registry: &ObjectRegistry,
) -> ObjectAuthoringCatalogCache {
    let retail_archives: Vec<_> = archives
        .iter()
        .filter(|archive| archive.size_bytes > 0)
        .cloned()
        .collect();
    let build = ObjectAuthoringCatalog::build_with_base_root(&retail_archives, registry, base_root);
    ObjectAuthoringCatalogCache {
        key: object_authoring_catalog_cache_key(base_root, archives, registry),
        catalog: Arc::new(build.catalog),
        warnings: Arc::new(build.warnings),
    }
}

fn object_authoring_catalog_cache_key(
    base_root: &Path,
    archives: &[SceneArchiveInfo],
    registry: &ObjectRegistry,
) -> ObjectAuthoringCatalogCacheKey {
    ObjectAuthoringCatalogCacheKey {
        base_root: normalized_base_root_identity(base_root),
        registry_fingerprint: object_registry_fingerprint(registry),
        retail_archive_fingerprint: retail_archive_fingerprint(archives),
    }
}

fn object_registry_fingerprint(registry: &ObjectRegistry) -> u64 {
    const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;

    let registry_bytes = serde_json::to_vec(registry)
        .expect("ObjectRegistry serialization cannot fail for its data-only schema");
    fnv1a_bytes(FNV_OFFSET, &registry_bytes)
}

fn retail_archive_fingerprint(archives: &[SceneArchiveInfo]) -> u64 {
    const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;

    let mut retail_archives: Vec<_> = archives
        .iter()
        .filter(|archive| archive.size_bytes > 0)
        .collect();
    retail_archives.sort_by(|left, right| {
        left.stage_id
            .cmp(&right.stage_id)
            .then_with(|| left.relative_path.cmp(&right.relative_path))
            .then_with(|| left.path.cmp(&right.path))
    });
    retail_archives
        .into_iter()
        .fold(FNV_OFFSET, |hash, archive| {
            let relative_path = archive
                .relative_path
                .to_string_lossy()
                .replace('/', "\\")
                .to_ascii_lowercase();
            let hash = fnv1a_bytes(hash, archive.stage_id.as_bytes());
            let hash = fnv1a_bytes(hash, &[0]);
            let hash = fnv1a_bytes(hash, relative_path.as_bytes());
            let hash = fnv1a_bytes(hash, &archive.size_bytes.to_le_bytes());
            match std::fs::metadata(&archive.path)
                .and_then(|metadata| metadata.modified())
                .ok()
                .and_then(|modified| modified.duration_since(std::time::UNIX_EPOCH).ok())
            {
                Some(modified) => {
                    let hash = fnv1a_bytes(hash, &modified.as_secs().to_le_bytes());
                    fnv1a_bytes(hash, &modified.subsec_nanos().to_le_bytes())
                }
                None => fnv1a_bytes(hash, &[0xff]),
            }
        })
}

fn fnv1a_bytes(mut hash: u64, bytes: &[u8]) -> u64 {
    const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;
    for byte in bytes {
        hash = (hash ^ u64::from(*byte)).wrapping_mul(FNV_PRIME);
    }
    hash
}

fn normalized_base_root_identity(base_root: &Path) -> String {
    let absolute = std::fs::canonicalize(base_root).unwrap_or_else(|_| {
        if base_root.is_absolute() {
            base_root.to_path_buf()
        } else {
            std::env::current_dir().unwrap_or_default().join(base_root)
        }
    });
    let normalized = absolute
        .to_string_lossy()
        .replace('/', "\\")
        .trim_end_matches('\\')
        .to_string();
    if cfg!(windows) {
        normalized.to_ascii_lowercase()
    } else {
        normalized
    }
}

fn editor_schema_cache_path(repo_root: &Path) -> Option<PathBuf> {
    const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;

    let cache_root = std::env::var_os("LOCALAPPDATA")
        .or_else(|| std::env::var_os("APPDATA"))
        .map(PathBuf::from)?;
    let identity = normalized_base_root_identity(repo_root);
    let key = fnv1a_bytes(FNV_OFFSET, identity.as_bytes());
    Some(
        cache_root
            .join("Graffito-Editor")
            .join("cache")
            .join("schema")
            .join(format!("{key:016x}.json")),
    )
}

fn generate_editor_schema(repo_root: &Path) -> sms_schema::Result<ObjectRegistry> {
    let generator = SchemaGenerator::new(repo_root);
    match editor_schema_cache_path(repo_root) {
        Some(cache_path) => generator.generate_cached(cache_path),
        None => generator.generate(),
    }
}

fn resolve_object_authoring_catalog(
    base_root: &Path,
    archives: &[SceneArchiveInfo],
    registry: Option<&ObjectRegistry>,
    cached: Option<ObjectAuthoringCatalogCache>,
) -> Option<ObjectAuthoringCatalogCache> {
    let registry = registry?;
    let key = object_authoring_catalog_cache_key(base_root, archives, registry);
    if let Some(cached) = cached.filter(|cached| cached.key == key) {
        return Some(cached);
    }
    Some(build_object_authoring_catalog(
        base_root, archives, registry,
    ))
}

fn split_object_authoring_catalog_cache(
    cache: Option<ObjectAuthoringCatalogCache>,
) -> (
    Option<ObjectAuthoringCatalogCacheKey>,
    Arc<ObjectAuthoringCatalog>,
    Arc<Vec<ObjectAuthoringCatalogWarning>>,
) {
    cache.map_or_else(
        || {
            (
                None,
                Arc::new(ObjectAuthoringCatalog::default()),
                Arc::new(Vec::new()),
            )
        },
        |cache| (Some(cache.key), cache.catalog, cache.warnings),
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DolphinLaunchMode {
    Editor,
    External,
}

impl DolphinLaunchMode {
    fn progress_label(self) -> &'static str {
        match self {
            Self::Editor => "Launch in Editor",
            Self::External => "Launch in Dolphin",
        }
    }
}

enum BackgroundResult {
    Schema(Box<Result<LoadedSchema, String>>),
    Scan {
        base_root: String,
        result: Result<SceneScanResult, String>,
    },
    Open(Result<Box<LoadedStage>, String>),
    CreateStage(Result<Box<LoadedStage>, String>),
    RetailSkybox(Result<RetailSkyboxSelection, String>),
    GoopRebuild(Result<Box<GoopRebuildOutcome>, String>),
    Build(Result<managed_build::ManagedGameBuildOutcome, String>),
    BuildAndRun {
        mode: DolphinLaunchMode,
        result: Result<managed_build::ManagedGameLaunchOutcome, String>,
    },
}

mod preview_types;
use preview_types::*;

fn stage_document_differs_from_saved(
    document: &StageDocument,
    saved_objects: &[SceneObject],
    saved_lighting: &StageLighting,
    saved_archive_edits: &StageArchiveEdits,
    saved_dialogue_authoring: &Option<DialogueAuthoringDocument>,
    saved_dialogue_library: &ProjectDialogueLibrary,
) -> bool {
    document.objects != saved_objects
        || document.lighting != *saved_lighting
        || document.archive_edits != *saved_archive_edits
        || document.dialogue_authoring != *saved_dialogue_authoring
        || document.dialogue_library != *saved_dialogue_library
}

fn validation_issues_for_preview(
    document: &StageDocument,
    preview: Option<&ModelPreview>,
) -> Vec<ValidationIssue> {
    let mut issues = document.validate();
    let Some(preview) = preview else {
        return issues;
    };
    for (index, failure) in preview.model_failures.iter().enumerate() {
        issues.push(ValidationIssue::warning(
            format!("renderer-model-preview-failed-{index}"),
            format!(
                "Model preview failed for '{}': {}",
                failure.asset_path, failure.error
            ),
        ));
    }
    if preview.failed_models > preview.model_failures.len() {
        issues.push(ValidationIssue::warning(
            "renderer-model-preview-failures-truncated",
            format!(
                "{} additional model asset failure(s) were omitted after the first {} details",
                preview.failed_models - preview.model_failures.len(),
                preview.model_failures.len()
            ),
        ));
    }
    for (index, failure) in preview.collision_failures.iter().enumerate() {
        issues.push(ValidationIssue::warning(
            format!("renderer-collision-preview-failed-{index}"),
            format!(
                "Collision preview failed for '{}': {}",
                failure.asset_path, failure.error
            ),
        ));
    }
    if preview.failed_collision_files > preview.collision_failures.len() {
        issues.push(ValidationIssue::warning(
            "renderer-collision-preview-failures-truncated",
            format!(
                "{} additional collision asset failure(s) were omitted after the first {} details",
                preview.failed_collision_files - preview.collision_failures.len(),
                preview.collision_failures.len()
            ),
        ));
    }
    for (index, material) in preview.materials.iter().enumerate() {
        let blend = material.blend_mode;
        append_gpu_blend_validation_issue(
            &mut issues,
            index,
            &material.name,
            blend.mode,
            blend.logic_op,
        );
    }
    for (index, texture) in preview.textures.iter().enumerate() {
        let mut unsupported_flags = Vec::new();
        if texture.do_edge_lod {
            unsupported_flags.push("edge LOD");
        }
        if texture.bias_clamp {
            unsupported_flags.push("LOD bias clamp");
        }
        if !unsupported_flags.is_empty() {
            issues.push(ValidationIssue::warning(
                format!("renderer-gx-texture-lod-unsupported-{index}"),
                format!(
                    "Texture #{index} requests GX {}, which the wgpu preview does not emulate",
                    unsupported_flags.join(" and ")
                ),
            ));
        }
        if !gpu_viewport::authored_sampler_anisotropy_is_supported(texture) {
            issues.push(ValidationIssue::warning(
                format!("renderer-gx-texture-anisotropy-filter-conflict-{index}"),
                format!(
                    "Texture #{index} requests GX {}x anisotropy with non-linear minification filters; WebGPU cannot combine those states, so the preview preserves the authored filters and disables anisotropy",
                    1u16 << texture.max_anisotropy.min(2)
                ),
            ));
        }
    }
    issues
}

fn append_gpu_blend_validation_issue(
    issues: &mut Vec<ValidationIssue>,
    material_index: usize,
    material_name: &str,
    mode: u8,
    logic_op: u8,
) {
    match gx_blend_compatibility(mode, logic_op) {
        GxBlendCompatibility::UnsupportedLogicOperation { logic_operation } => {
            issues.push(ValidationIssue::warning(
                format!("renderer-gx-logic-op-unsupported-{material_index}"),
                format!(
                    "Material #{material_index} '{material_name}' uses GX framebuffer logic operation {logic_operation}; the wgpu preview uses overwrite fallback because this operation depends on the existing framebuffer value"
                ),
            ));
        }
        GxBlendCompatibility::UnsupportedMode { mode } => {
            issues.push(ValidationIssue::warning(
                format!("renderer-gx-blend-mode-unsupported-{material_index}"),
                format!(
                    "Material #{material_index} '{material_name}' uses unknown GX blend mode {mode}; the wgpu preview uses overwrite fallback"
                ),
            ));
        }
        GxBlendCompatibility::Native
        | GxBlendCompatibility::SourceIndependentLogicOperation { .. } => {}
    }
}

const MAX_MODEL_FAILURE_DETAILS: usize = 32;

fn record_model_preview_failure(
    failed_assets: &mut BTreeSet<String>,
    failures: &mut Vec<PreviewModelFailure>,
    asset_path: &str,
    error: String,
) {
    if !failed_assets.insert(normalized_preview_asset_path(asset_path)) {
        return;
    }
    if failures.len() < MAX_MODEL_FAILURE_DETAILS {
        failures.push(PreviewModelFailure {
            asset_path: asset_path.to_string(),
            error,
        });
    }
}

#[derive(Default)]
struct CollisionPreviewBuild {
    triangles: Vec<CollisionPreviewTriangle>,
    file_count: usize,
    surface_types: BTreeSet<u16>,
    failed_assets: BTreeSet<String>,
    failures: Vec<PreviewModelFailure>,
}

impl CollisionPreviewBuild {
    fn append_file(&mut self, collision: &ColFile) {
        self.file_count += 1;
        for group in collision.groups() {
            self.surface_types.insert(group.surface_type);
            for triangle in &group.triangles {
                let [a, b, c] = triangle.vertex_indices;
                let (Some(a), Some(b), Some(c)) = (
                    collision.vertices().get(usize::from(a)),
                    collision.vertices().get(usize::from(b)),
                    collision.vertices().get(usize::from(c)),
                ) else {
                    continue;
                };
                self.triangles.push(CollisionPreviewTriangle {
                    vertices: [a.position, b.position, c.position],
                    surface_type: group.surface_type,
                });
            }
        }
    }

    fn record_failure(&mut self, asset_path: &str, error: String) {
        record_model_preview_failure(
            &mut self.failed_assets,
            &mut self.failures,
            asset_path,
            error,
        );
    }
}

fn normalized_collision_asset_id(path: &str) -> String {
    path.replace('\\', "/").to_ascii_lowercase()
}

fn semantic_collision_asset_id(document: &StageDocument, raw_path: &[u8]) -> String {
    let raw_path = String::from_utf8_lossy(raw_path).replace('\\', "/");
    document.stage_archive_source_path.as_ref().map_or_else(
        || normalized_collision_asset_id(&raw_path),
        |source| {
            normalized_collision_asset_id(&format!("{}!/{raw_path}", source.to_string_lossy()))
        },
    )
}

fn build_collision_preview(document: &StageDocument) -> CollisionPreviewBuild {
    let mut preview = CollisionPreviewBuild::default();
    let mut loaded_assets = BTreeSet::new();

    if let Some(archive) = &document.stage_archive {
        for resource in archive.resources() {
            let StageResourceDocument::Collision(base_collision) = &resource.document else {
                continue;
            };
            let asset_id = semantic_collision_asset_id(document, &resource.raw_path);
            let collision = document
                .archive_edits
                .collisions
                .iter()
                .find(|edit| edit.raw_resource_path == resource.raw_path)
                .map_or(base_collision, |edit| &edit.document);
            preview.append_file(collision);
            loaded_assets.insert(asset_id);
        }
    }

    for edit in &document.archive_edits.collisions {
        let asset_id = semantic_collision_asset_id(document, &edit.raw_resource_path);
        if loaded_assets.insert(asset_id) {
            preview.append_file(&edit.document);
        }
    }

    for asset in document
        .assets
        .iter()
        .filter(|asset| asset.kind == StageAssetKind::Collision)
    {
        let asset_path = asset.path.to_string_lossy().replace('\\', "/");
        if !loaded_assets.insert(normalized_collision_asset_id(&asset_path)) {
            continue;
        }
        let bytes = match document.read_asset_bytes(&asset.path) {
            Ok(bytes) => bytes,
            Err(error) => {
                preview.record_failure(&asset_path, format!("read asset: {error}"));
                continue;
            }
        };
        match ColFile::parse(&bytes) {
            Ok(collision) => preview.append_file(&collision),
            Err(error) => preview.record_failure(&asset_path, format!("parse COL: {error}")),
        }
    }

    preview
}

#[derive(Debug, Clone, PartialEq)]
enum ObjectDelta {
    Insert {
        index: usize,
        object: SceneObject,
    },
    Remove {
        index: usize,
        object: SceneObject,
    },
    Update {
        before: Box<SceneObject>,
        after: Box<SceneObject>,
    },
}

#[derive(Debug, Clone, PartialEq)]
struct ResourceEditState {
    edit: Option<StageResourceEdit>,
    edit_index: Option<usize>,
    removal_index: Option<usize>,
}

#[derive(Debug, Clone, PartialEq)]
struct ResourceEditDelta {
    raw_resource_path: Vec<u8>,
    before: ResourceEditState,
    after: ResourceEditState,
}

#[derive(Debug, Clone, PartialEq)]
struct RouteAuthoringDelta {
    before: Option<RouteAuthoringDocument>,
    after: Option<RouteAuthoringDocument>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct DialogueAuthoringDelta {
    before_authoring: Option<DialogueAuthoringDocument>,
    after_authoring: Option<DialogueAuthoringDocument>,
    before_library: ProjectDialogueLibrary,
    after_library: ProjectDialogueLibrary,
}

#[derive(Debug, Clone, PartialEq)]
struct ObjectUndoRecord {
    deltas: Vec<ObjectDelta>,
    resource_deltas: Vec<ResourceEditDelta>,
    route_delta: Option<RouteAuthoringDelta>,
    dialogue_delta: Option<DialogueAuthoringDelta>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ObjectUndoTransactionKind {
    Transform,
    Parameter,
}

#[derive(Debug, Clone, PartialEq)]
struct ObjectUndoTransaction {
    index: usize,
    before: SceneObject,
    kind: ObjectUndoTransactionKind,
}
#[derive(Debug, Clone, PartialEq)]
struct RouteUndoTransaction {
    before_objects: Vec<SceneObject>,
    before_archive_edits: StageArchiveEdits,
    before_route: Option<RouteAuthoringDocument>,
}

#[derive(Debug, Clone, PartialEq)]
struct RouteHandleDrag {
    graph_id: String,
    link_id: String,
    from_handle: bool,
    plane_y: f32,
}

struct SmsEditorApp {
    current_project: Option<OpenProject>,
    recent_projects: RecentProjects,
    show_project_hub: bool,
    project_hub_error: Option<String>,
    new_project_draft: Option<NewProjectDraft>,
    new_stage_draft: Option<NewStageDraft>,
    project_name_draft: String,
    repo_root: String,
    base_root: String,
    project_root: String,
    stage_id: String,
    dolphin_path: String,
    game_path: String,
    dolphin_user_dir: String,
    registry: Option<ObjectRegistry>,
    object_authoring_catalog_cache_key: Option<ObjectAuthoringCatalogCacheKey>,
    object_authoring_catalog: Arc<ObjectAuthoringCatalog>,
    object_authoring_catalog_warnings: Arc<Vec<ObjectAuthoringCatalogWarning>>,
    document: Option<StageDocument>,
    render_scene: Option<RenderScene>,
    scene_archives: Vec<SceneArchiveInfo>,
    scene_labels: BTreeMap<String, SceneArchiveLabel>,
    retail_skyboxes: Vec<RetailSkyboxEntry>,
    retail_goop_templates: Vec<RetailGoopTemplate>,
    goop_templates_indexed: bool,
    retail_music: Vec<RetailMusicEntry>,
    retail_sounds: Vec<RetailSoundEntry>,
    retail_dialogue_voices: Vec<RetailDialogueVoiceEntry>,
    retail_stage_audio: Vec<RetailStageAudioProfile>,
    game_content_index: GameContentIndexState,
    content_thumbnails: ContentThumbnailService,
    model_preview: Option<ModelPreview>,
    gpu_viewport: Option<gpu_viewport::GpuViewportScene>,
    gpu_target_format: Option<eframe::wgpu::TextureFormat>,
    model_framebuffer: Option<egui::TextureHandle>,
    model_framebuffer_key: Option<ModelFramebufferKey>,
    issues: Vec<ValidationIssue>,
    log: Vec<String>,
    renderer: ViewportRenderer,
    selected_object_id: Option<String>,
    active_placement: Option<ActivePlacement>,
    object_filter: String,
    scene_filter: String,
    skybox_filter: String,
    content_browser_kind: ContentBrowserKind,
    content_browser: ContentBrowserState,
    model_asset_filter: String,
    model_folder_filter: Option<String>,
    model_asset_rename_draft: String,
    model_asset_move_draft: String,
    new_model_folder_draft: String,
    model_catalog_root: Option<PathBuf>,
    model_catalog_entries: Vec<sms_authoring::CatalogAssetEntry>,
    model_catalog_issues: Vec<String>,
    model_asset_preview_cache: BTreeMap<AuthoredModelPreviewKey, Arc<AuthoredModelPreviewGeometry>>,
    authored_model_preview_base: Option<AuthoredModelPreviewBase>,
    selected_model_asset: Option<sms_authoring::AssetId>,
    selected_model_document: Option<sms_authoring::ModelAssetDocument>,
    saved_model_document: Option<sms_authoring::ModelAssetDocument>,
    selected_model_material: usize,
    selected_model_texture: usize,
    model_editor_section: ModelEditorSection,
    model_target_profile: ModelTargetProfile,
    model_editor_error: Option<String>,
    gx_json_draft: String,
    texture_json_draft: String,
    asset_dirty: bool,
    asset_undo_stack: VecDeque<ModelAssetUndoRecord>,
    asset_redo_stack: VecDeque<ModelAssetUndoRecord>,
    model_import_options: sms_authoring::ModelImportOptions,
    model_import_job: Option<ModelImportJob>,
    model_instances: Vec<EditorModelInstance>,
    model_instances_dirty: bool,
    model_instance_undo_stack: VecDeque<ModelInstanceUndoRecord>,
    model_instance_redo_stack: VecDeque<ModelInstanceUndoRecord>,
    model_collision_override_template: Option<sms_authoring::CollisionSurface>,
    selected_model_instance_id: Option<uuid::Uuid>,
    last_scanned_base_root: String,
    pending_auto_refresh_root: Option<String>,
    last_auto_refresh_attempt_root: String,
    tool: EditorTool,
    selected_goop_layer: usize,
    selected_goop_template: usize,
    show_incompatible_goop_templates: bool,
    goop_brush_radius: f32,
    goop_brush_hardness: f32,
    goop_brush_opacity: f32,
    goop_fill_mode: bool,
    goop_use_custom_region: bool,
    goop_region_min_x: f32,
    goop_region_min_z: f32,
    goop_region_width_cells: u16,
    goop_region_height_cells: u16,
    goop_cursor_world: Option<[f32; 3]>,
    goop_stroke: Option<GoopStroke>,
    goop_undo_stack: VecDeque<GoopUndoRecord>,
    goop_redo_stack: VecDeque<GoopUndoRecord>,
    view_mode: ViewMode,
    bottom_tab: BottomTab,
    show_project_settings: bool,
    show_issues: bool,
    show_console: bool,
    show_stats: bool,
    show_fps: bool,
    applied_window_title: String,
    snap_enabled: bool,
    snap_translation: f32,
    snap_rotation: f32,
    snap_scale: f32,
    show_environment_meshes: bool,
    show_goop_meshes: bool,
    show_effects: bool,
    show_audio_helpers: bool,
    selected_audio_helper_id: Option<String>,
    audio_cube_edit_before: Option<StageArchiveEdits>,
    audio_cube_helpers_cache: Vec<AudioHelper>,
    audio_preview_playback: Option<AudioPreviewPlayback>,
    audio_preview_receiver: Option<Receiver<AudioPreviewRenderResult>>,
    audio_preview_loading_target: Option<AudioPreviewTarget>,
    audio_preview_generation: u64,
    outliner_filter: String,
    startup_camera_focus: Option<[f32; 3]>,
    route_mode: bool,
    show_all_routes: bool,
    active_route_graph: Option<String>,
    selected_route_controls: BTreeSet<String>,
    selected_route_link: Option<String>,
    route_curve_confirmation: Option<(String, String)>,
    pending_route_assignment: Option<(String, String)>,
    route_handle_drag: Option<RouteHandleDrag>,
    dialogue_route_index: Option<DialogueRouteIndex>,
    dialogue_consumer_index: Option<DialogueGameConsumerIndex>,
    dialogue_index_receiver: Option<Receiver<DialogueIndexBuildResult>>,
    dialogue_consumer_receiver: Option<Receiver<Result<DialogueGameConsumerIndex, String>>>,
    dialogue_consumer_cancel: Option<Arc<AtomicBool>>,
    dialogue_index_error: Option<String>,
    dialogue_consumer_error: Option<String>,
    dialogue_shared_confirmation: Option<DialogueSharedConfirmation>,
    dialogue_undo_transaction: Option<DialogueUndoTransaction>,
    startup_focus_object: Option<String>,
    startup_camera_distance: Option<f32>,
    startup_camera_yaw: Option<f32>,
    startup_camera_pitch: Option<f32>,
    viewport_pan: egui::Vec2,
    viewport_zoom: f32,
    camera_speed: f32,
    camera_fly_velocity: [f32; 3],
    camera_state_save_pending: bool,
    camera_state_changed_at: Instant,
    hovered_gizmo_axis: Option<GizmoAxis>,
    gizmo_drag: Option<GizmoDrag>,
    next_object_serial: u32,
    saved_objects: Vec<SceneObject>,
    saved_lighting: StageLighting,
    saved_archive_edits: StageArchiveEdits,
    saved_dialogue_authoring: Option<DialogueAuthoringDocument>,
    saved_dialogue_library: ProjectDialogueLibrary,
    document_dirty: bool,
    undo_stack: VecDeque<ObjectUndoRecord>,
    redo_stack: VecDeque<ObjectUndoRecord>,
    undo_transaction: Option<ObjectUndoTransaction>,
    route_undo_transaction: Option<RouteUndoTransaction>,
    pending_stage_open: Option<String>,
    pending_project_hub: bool,
    close_confirmation_requested: bool,
    close_authorized: bool,
    background_receiver: Option<Receiver<BackgroundResult>>,
    background_label: Option<String>,
    active_build_cancel: Option<Arc<AtomicBool>>,
    embedded_dolphin: Option<play_in_editor::EmbeddedDolphinSession>,
    animation_started_at: Instant,
    last_skeletal_animation_tick: u64,
    level_transform_progress: f32,
    level_transform_playing: bool,
    level_transform_started_at: Instant,
    level_transform_playback_origin: f32,
    last_level_transform_progress_bits: u32,
}

impl Default for SmsEditorApp {
    fn default() -> Self {
        let args = editor_startup_args();
        let has_project_startup = args.project_file.is_some();
        let startup_project = args
            .project_file
            .as_deref()
            .map(OpenProject::load)
            .transpose();
        let (current_project, project_hub_error) = match startup_project {
            Ok(project) => (project, None),
            Err(error) => (None, Some(error)),
        };
        #[cfg(test)]
        let mut recent_projects = RecentProjects::empty();
        #[cfg(not(test))]
        let mut recent_projects = RecentProjects::load_default();
        if let Some(project) = &current_project {
            let _ = recent_projects.touch(project);
        }
        let has_legacy_startup = args.base_root.is_some() && !has_project_startup;
        let base_root = current_project.as_ref().map_or_else(
            || args.base_root.unwrap_or_default(),
            |project| {
                project
                    .descriptor
                    .base_game_root
                    .to_string_lossy()
                    .into_owned()
            },
        );
        let repo_root = current_project
            .as_ref()
            .and_then(|project| project.descriptor.schema_source_root.as_ref())
            .map(|path| path.to_string_lossy().into_owned())
            .or(args.repo_root)
            .unwrap_or_else(default_repo_root);
        let project_root = current_project.as_ref().map_or_else(
            || "sms-editor-project".to_string(),
            |project| project.data_root().to_string_lossy().into_owned(),
        );
        let stage_id = args
            .stage_id
            .or_else(|| {
                current_project
                    .as_ref()
                    .and_then(|project| project.descriptor.last_stage.clone())
            })
            .unwrap_or_default();
        let launch = current_project
            .as_ref()
            .map(|project| project.descriptor.launch.clone())
            .unwrap_or_default();
        let show_project_hub = !has_legacy_startup && current_project.is_none();
        Self {
            project_name_draft: current_project
                .as_ref()
                .map(|project| project.descriptor.name.clone())
                .unwrap_or_default(),
            current_project,
            recent_projects,
            show_project_hub,
            project_hub_error,
            new_project_draft: None,
            new_stage_draft: None,
            repo_root,
            base_root,
            project_root,
            stage_id,
            dolphin_path: launch
                .dolphin_executable
                .map(|path| path.to_string_lossy().into_owned())
                .unwrap_or_default(),
            game_path: launch
                .game_image
                .map(|path| path.to_string_lossy().into_owned())
                .unwrap_or_default(),
            dolphin_user_dir: launch
                .dolphin_user_directory
                .map(|path| path.to_string_lossy().into_owned())
                .unwrap_or_default(),
            registry: None,
            object_authoring_catalog_cache_key: None,
            object_authoring_catalog: Arc::new(ObjectAuthoringCatalog::default()),
            object_authoring_catalog_warnings: Arc::new(Vec::new()),
            document: None,
            render_scene: None,
            scene_archives: Vec::new(),
            scene_labels: BTreeMap::new(),
            retail_skyboxes: Vec::new(),
            retail_goop_templates: Vec::new(),
            goop_templates_indexed: false,
            retail_music: Vec::new(),
            retail_sounds: Vec::new(),
            retail_dialogue_voices: Vec::new(),
            retail_stage_audio: Vec::new(),
            game_content_index: GameContentIndexState::default(),
            content_thumbnails: ContentThumbnailService::default(),
            model_preview: None,
            gpu_viewport: None,
            gpu_target_format: None,
            model_framebuffer: None,
            model_framebuffer_key: None,
            issues: Vec::new(),
            log: vec!["Ready.".to_string()],
            renderer: ViewportRenderer::new(RendererConfig::default()),
            selected_object_id: None,
            active_placement: None,
            object_filter: String::new(),
            scene_filter: String::new(),
            skybox_filter: String::new(),
            content_browser_kind: ContentBrowserKind::default(),
            content_browser: ContentBrowserState::default(),
            model_asset_filter: String::new(),
            model_folder_filter: None,
            model_asset_rename_draft: String::new(),
            model_asset_move_draft: String::new(),
            new_model_folder_draft: String::new(),
            model_catalog_root: None,
            model_catalog_entries: Vec::new(),
            model_catalog_issues: Vec::new(),
            model_asset_preview_cache: BTreeMap::new(),
            authored_model_preview_base: None,
            selected_model_asset: None,
            selected_model_document: None,
            saved_model_document: None,
            selected_model_material: 0,
            selected_model_texture: 0,
            model_editor_section: ModelEditorSection::default(),
            model_target_profile: ModelTargetProfile::default(),
            model_editor_error: None,
            gx_json_draft: String::new(),
            texture_json_draft: String::new(),
            asset_dirty: false,
            asset_undo_stack: VecDeque::new(),
            asset_redo_stack: VecDeque::new(),
            model_import_options: sms_authoring::ModelImportOptions::default(),
            model_import_job: None,
            model_instances: Vec::new(),
            model_instances_dirty: false,
            model_instance_undo_stack: VecDeque::new(),
            model_instance_redo_stack: VecDeque::new(),
            model_collision_override_template: None,
            selected_model_instance_id: None,
            last_scanned_base_root: String::new(),
            pending_auto_refresh_root: None,
            last_auto_refresh_attempt_root: String::new(),
            tool: EditorTool::Select,
            selected_goop_layer: 0,
            selected_goop_template: 0,
            show_incompatible_goop_templates: false,
            goop_brush_radius: 200.0,
            goop_brush_hardness: 0.65,
            goop_brush_opacity: 1.0,
            goop_fill_mode: false,
            goop_use_custom_region: false,
            goop_region_min_x: 0.0,
            goop_region_min_z: 0.0,
            goop_region_width_cells: 8,
            goop_region_height_cells: 4,
            goop_cursor_world: None,
            goop_stroke: None,
            goop_undo_stack: VecDeque::new(),
            goop_redo_stack: VecDeque::new(),
            view_mode: ViewMode::Lit,
            bottom_tab: BottomTab::Content,
            show_project_settings: false,
            show_issues: false,
            show_console: false,
            show_stats: false,
            show_fps: false,
            applied_window_title: String::new(),
            snap_enabled: true,
            snap_translation: 50.0,
            snap_rotation: 15.0,
            snap_scale: 0.1,
            show_environment_meshes: true,
            show_goop_meshes: true,
            show_effects: true,
            show_audio_helpers: true,
            selected_audio_helper_id: None,
            audio_cube_edit_before: None,
            audio_cube_helpers_cache: Vec::new(),
            audio_preview_playback: None,
            audio_preview_receiver: None,
            audio_preview_loading_target: None,
            audio_preview_generation: 0,
            outliner_filter: String::new(),
            route_mode: false,
            show_all_routes: true,
            active_route_graph: None,
            selected_route_controls: BTreeSet::new(),
            selected_route_link: None,
            route_curve_confirmation: None,
            pending_route_assignment: None,
            route_handle_drag: None,
            dialogue_route_index: None,
            dialogue_consumer_index: None,
            dialogue_index_receiver: None,
            dialogue_consumer_receiver: None,
            dialogue_consumer_cancel: None,
            dialogue_index_error: None,
            dialogue_consumer_error: None,
            dialogue_shared_confirmation: None,
            dialogue_undo_transaction: None,
            startup_camera_focus: args.camera_focus,
            startup_focus_object: args.focus_object,
            startup_camera_distance: args.camera_distance,
            startup_camera_yaw: args.camera_yaw,
            startup_camera_pitch: args.camera_pitch,
            viewport_pan: egui::Vec2::ZERO,
            viewport_zoom: 1.0,
            camera_speed: 1.0,
            camera_fly_velocity: [0.0; 3],
            camera_state_save_pending: false,
            camera_state_changed_at: Instant::now(),
            hovered_gizmo_axis: None,
            gizmo_drag: None,
            next_object_serial: 1,
            saved_objects: Vec::new(),
            saved_lighting: StageLighting::default(),
            saved_archive_edits: StageArchiveEdits::default(),
            saved_dialogue_authoring: None,
            saved_dialogue_library: ProjectDialogueLibrary::default(),
            document_dirty: false,
            undo_stack: VecDeque::new(),
            redo_stack: VecDeque::new(),
            undo_transaction: None,
            route_undo_transaction: None,
            pending_stage_open: None,
            pending_project_hub: false,
            close_confirmation_requested: false,
            close_authorized: false,
            background_receiver: None,
            background_label: None,
            active_build_cancel: None,
            embedded_dolphin: None,
            animation_started_at: Instant::now(),
            last_skeletal_animation_tick: u64::MAX,
            level_transform_progress: FULL_DELFINO_PROGRESSION,
            level_transform_playing: false,
            level_transform_started_at: Instant::now(),
            level_transform_playback_origin: 0.0,
            last_level_transform_progress_bits: u32::MAX,
        }
    }
}

impl SmsEditorApp {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        Self {
            gpu_target_format: cc
                .wgpu_render_state
                .as_ref()
                .map(|render_state| render_state.target_format),
            ..Self::default()
        }
    }
}

impl eframe::App for SmsEditorApp {
    fn logic(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
        self.finish_dialogue_transaction_if_selection_changed();
        self.poll_background_task(ctx, Some(frame));
        self.poll_dialogue_index(ctx);
        self.sync_game_content_index();
        self.poll_game_content_index(ctx);
        self.poll_content_thumbnails(ctx);
        self.poll_audio_preview(ctx);
        self.poll_embedded_dolphin(ctx);
        self.poll_model_import(ctx);
        self.persist_camera_state_if_due();
        self.sync_window_title(ctx);
        let close_requested = ctx.input(|input| input.viewport().close_requested());
        if close_requested {
            self.flush_camera_state();
        }
        if close_requested && self.is_dirty() && !self.close_authorized {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            self.close_confirmation_requested = true;
        }

        if self.show_project_hub {
            return;
        }

        if self.embedded_dolphin.is_some() {
            return;
        }

        if ctx.egui_wants_keyboard_input() {
            return;
        }

        self.content_browser_keyboard(ctx);

        if ctx.input(|i| i.modifiers.ctrl && !i.modifiers.shift && i.key_pressed(egui::Key::Z)) {
            self.undo();
        }
        if ctx.input(|i| i.modifiers.ctrl && i.modifiers.shift && i.key_pressed(egui::Key::Z)) {
            self.redo();
        }
        if ctx.input(|i| i.key_pressed(egui::Key::Delete)) {
            self.delete_selected();
        }
        if ctx.input(|i| i.pointer.secondary_down()) {
            return;
        }
        if ctx.input(|i| i.key_pressed(egui::Key::W)) {
            self.tool = self.tool.after_keyboard_shortcut(egui::Key::W);
        }
        if ctx.input(|i| i.key_pressed(egui::Key::E)) {
            self.tool = self.tool.after_keyboard_shortcut(egui::Key::E);
        }
        if ctx.input(|i| i.key_pressed(egui::Key::R)) {
            self.tool = self.tool.after_keyboard_shortcut(egui::Key::R);
        }
        if ctx.input(|i| i.key_pressed(egui::Key::Q)) {
            self.tool = self.tool.after_keyboard_shortcut(egui::Key::Q);
        }
        if ctx.input(|i| i.key_pressed(egui::Key::G)) {
            self.tool = self.tool.after_keyboard_shortcut(egui::Key::G);
        }
    }

    fn ui(&mut self, root_ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        if self.show_project_hub {
            egui::CentralPanel::default().show(root_ui, |ui| self.project_hub(ui));
            self.new_project_dialog(root_ui.ctx());
            self.unsaved_changes_dialog(root_ui.ctx());
            return;
        }
        self.refresh_scene_browser_if_needed();
        self.refresh_model_catalog();

        egui::Panel::top("toolbar")
            .default_size(34.0)
            .show(root_ui, |ui| self.toolbar(ui));

        egui::Panel::right("right_dock")
            .resizable(true)
            .default_size(360.0)
            .show(root_ui, |ui| self.right_dock(ui));

        let max_content_dock_height =
            (root_ui.available_height() - 140.0).max(BROWSER_DOCK_MIN_HEIGHT);
        let content_dock_height = self
            .content_browser
            .settings
            .dock_height
            .clamp(BROWSER_DOCK_MIN_HEIGHT, max_content_dock_height);
        egui::Panel::bottom("content_dock_v2")
            .exact_size(content_dock_height)
            .show_separator_line(false)
            .show(root_ui, |ui| self.bottom_dock(ui, max_content_dock_height));

        egui::CentralPanel::default().show(root_ui, |ui| {
            self.viewport_toolbar(ui);
            ui.separator();
            if self.embedded_dolphin.is_some() {
                self.embedded_dolphin_viewport(ui, frame);
            } else {
                self.viewport(ui);
            }
        });
        let primary_pointer_down = root_ui.input(|input| input.pointer.primary_down());
        self.finish_pointer_undo_transaction_if_released(primary_pointer_down);
        self.project_settings_window(root_ui.ctx());
        self.new_stage_dialog(root_ui.ctx());
        self.issues_window(root_ui.ctx());
        self.content_browser_preview_window(root_ui.ctx());
        self.unsaved_changes_dialog(root_ui.ctx());
    }

    fn on_exit(&mut self) {
        if let Some(mut session) = self.embedded_dolphin.take() {
            session.detach_window();
        }
    }
}

impl SmsEditorApp {
    // Panel implementations live in ui_panels.rs.

    // Viewport interaction and painting live in viewport_ui.rs.

    fn sync_window_title(&mut self, ctx: &egui::Context) {
        let active_stage = (!self.show_project_hub).then_some(self.stage_id.as_str());
        let desired = editor_window_title(
            self.current_project
                .as_ref()
                .map(|project| project.descriptor.name.as_str()),
            active_stage,
        );
        if self.applied_window_title != desired {
            ctx.send_viewport_cmd(egui::ViewportCommand::Title(desired.clone()));
            self.applied_window_title = desired;
        }
    }

    fn reusable_object_authoring_catalog_cache(
        &self,
        base_root: &Path,
        registry: Option<&ObjectRegistry>,
    ) -> Option<ObjectAuthoringCatalogCache> {
        let registry = registry?;
        let key = self.object_authoring_catalog_cache_key.as_ref()?;
        (key.base_root == normalized_base_root_identity(base_root)
            && key.registry_fingerprint == object_registry_fingerprint(registry))
        .then(|| ObjectAuthoringCatalogCache {
            key: key.clone(),
            catalog: Arc::clone(&self.object_authoring_catalog),
            warnings: Arc::clone(&self.object_authoring_catalog_warnings),
        })
    }

    fn reusable_scene_scan(&self, base_root: &Path) -> Option<SceneScanResult> {
        (!self.last_scanned_base_root.trim().is_empty()
            && normalized_base_root_identity(Path::new(self.last_scanned_base_root.trim()))
                == normalized_base_root_identity(base_root))
        .then(|| SceneScanResult {
            archives: self.scene_archives.clone(),
            labels: self.scene_labels.clone(),
            label_warning: None,
            retail_skyboxes: self.retail_skyboxes.clone(),
            skybox_warnings: Vec::new(),
            retail_music: self.retail_music.clone(),
            retail_sounds: self.retail_sounds.clone(),
            retail_dialogue_voices: self.retail_dialogue_voices.clone(),
            retail_stage_audio: self.retail_stage_audio.clone(),
            music_warning: None,
            object_authoring_catalog_cache: self
                .reusable_object_authoring_catalog_cache(base_root, self.registry.as_ref()),
        })
    }

    fn install_object_authoring_catalog_cache(
        &mut self,
        cache: Option<ObjectAuthoringCatalogCache>,
    ) {
        let (key, catalog, warnings) = split_object_authoring_catalog_cache(cache);
        self.object_authoring_catalog_cache_key = key;
        self.object_authoring_catalog = catalog;
        self.object_authoring_catalog_warnings = warnings;
    }

    fn invalidate_object_authoring_catalog_for_changed_base_root(&mut self) {
        let Some(key) = self.object_authoring_catalog_cache_key.as_ref() else {
            return;
        };
        let selected_base_root = normalized_base_root_identity(Path::new(self.base_root.trim()));
        if key.base_root != selected_base_root {
            self.install_object_authoring_catalog_cache(None);
        }
    }

    fn generate_schema(&mut self, force: bool) {
        if self.background_receiver.is_some() {
            self.log
                .push("Another background operation is already running.".to_string());
            return;
        }
        let repo_root = self.repo_root.clone();
        let base_root = self.base_root.trim().to_string();
        let archives = if !base_root.is_empty()
            && normalized_base_root_identity(Path::new(self.last_scanned_base_root.trim()))
                == normalized_base_root_identity(Path::new(&base_root))
        {
            self.scene_archives.clone()
        } else {
            Vec::new()
        };
        let (sender, receiver) = mpsc::channel();
        thread::spawn(move || {
            let result = if force {
                SchemaGenerator::new(&repo_root).generate()
            } else {
                generate_editor_schema(Path::new(&repo_root))
            }
            .map(|registry| {
                let object_authoring_catalog_cache = (!base_root.is_empty()
                    && archives.iter().any(|archive| archive.size_bytes > 0))
                .then(|| {
                    build_object_authoring_catalog(Path::new(&base_root), &archives, &registry)
                });
                LoadedSchema {
                    registry,
                    object_authoring_catalog_cache,
                }
            })
            .map_err(|err| err.to_string());
            let _ = sender.send(BackgroundResult::Schema(Box::new(result)));
        });
        self.background_receiver = Some(receiver);
        self.background_label = Some("Generating schema".to_string());
        self.log.push("Generating object schema...".to_string());
    }

    fn refresh_scene_browser_if_needed(&mut self) {
        self.invalidate_object_authoring_catalog_for_changed_base_root();
        let base_root = self.base_root.trim().to_string();
        if base_root.is_empty() {
            self.pending_auto_refresh_root = None;
            self.last_auto_refresh_attempt_root.clear();
            return;
        }

        if self.last_scanned_base_root == base_root {
            self.pending_auto_refresh_root = None;
            self.last_auto_refresh_attempt_root = base_root;
            return;
        }

        if self.last_auto_refresh_attempt_root != base_root
            && self.pending_auto_refresh_root.as_deref() != Some(base_root.as_str())
        {
            self.pending_auto_refresh_root = Some(base_root.clone());
        }

        if self.background_receiver.is_some()
            || self.pending_auto_refresh_root.as_deref() != Some(base_root.as_str())
            || !PathBuf::from(&base_root).exists()
        {
            return;
        }

        self.pending_auto_refresh_root = None;
        self.last_auto_refresh_attempt_root = base_root;
        let should_open = self.document.is_none();
        if should_open && !self.stage_id.trim().is_empty() {
            self.open_stage();
        } else {
            self.scan_scenes();
        }
    }

    fn scan_scenes(&mut self) {
        if self.background_receiver.is_some() {
            self.log
                .push("Another background operation is already running.".to_string());
            return;
        }
        let base_root = self.base_root.trim().to_string();
        if base_root.is_empty() {
            self.log
                .push("Base root is required for scene scan.".to_string());
            return;
        }

        let (sender, receiver) = mpsc::channel();
        let task_base_root = base_root.clone();
        let repo_root = self.repo_root.trim().to_string();
        let project_root = self.project_root.trim().to_string();
        let registry = self.registry.clone();
        let existing_object_authoring_catalog_cache =
            self.reusable_object_authoring_catalog_cache(Path::new(&base_root), registry.as_ref());
        thread::spawn(move || {
            let result = (|| -> Result<SceneScanResult, String> {
                let mut archives = discover_scene_archives(PathBuf::from(&task_base_root))
                    .map_err(|error| error.to_string())?;
                if !project_root.is_empty() {
                    let authored = sms_scene::discover_authored_project_stage_ids(&project_root)
                        .map_err(|error| error.to_string())?;
                    for stage_id in authored {
                        insert_authored_scene_archive(
                            &mut archives,
                            Path::new(&task_base_root),
                            &stage_id,
                        );
                    }
                }
                let (labels, label_warning) = match load_scene_archive_labels(
                    PathBuf::from(&task_base_root).as_path(),
                    PathBuf::from(&repo_root).as_path(),
                    &archives,
                ) {
                    Ok(labels) => (labels, None),
                    Err(error) => (BTreeMap::new(), Some(error)),
                };
                let (retail_skyboxes, skybox_warnings) = index_retail_skyboxes(&archives);
                let (retail_music, music_warning) = match index_retail_music(
                    Path::new(&repo_root),
                    Path::new(&task_base_root),
                    &labels,
                ) {
                    Ok(entries) => (entries, None),
                    Err(error) => (Vec::new(), Some(error)),
                };
                let retail_sounds =
                    index_retail_sounds(Path::new(&task_base_root)).unwrap_or_default();
                let retail_dialogue_voices =
                    index_retail_dialogue_voices(Path::new(&repo_root), &retail_sounds)
                        .unwrap_or_default();
                let retail_stage_audio = index_retail_stage_audio_profiles(
                    Path::new(&repo_root),
                    Path::new(&task_base_root),
                )
                .unwrap_or_default();
                let object_authoring_catalog_cache = resolve_object_authoring_catalog(
                    Path::new(&task_base_root),
                    &archives,
                    registry.as_ref(),
                    existing_object_authoring_catalog_cache,
                );
                Ok(SceneScanResult {
                    archives,
                    labels,
                    label_warning,
                    retail_skyboxes,
                    skybox_warnings,
                    retail_music,
                    retail_sounds,
                    retail_dialogue_voices,
                    retail_stage_audio,
                    music_warning,
                    object_authoring_catalog_cache,
                })
            })();
            let _ = sender.send(BackgroundResult::Scan {
                base_root: task_base_root,
                result,
            });
        });
        self.background_receiver = Some(receiver);
        self.background_label = Some("Scanning scenes".to_string());
        self.log
            .push(format!("Scanning scenes under {base_root}..."));
    }

    fn open_scene_archive(&mut self, archive: SceneArchiveInfo) {
        self.log
            .push(format!("Selected scene '{}'.", archive.stage_id));
        self.request_open_stage(archive.stage_id);
    }

    fn request_open_stage(&mut self, stage_id: String) {
        self.flush_camera_state();
        if self.is_dirty() {
            self.pending_stage_open = Some(stage_id);
        } else if self.background_receiver.is_some() {
            self.log
                .push(format!("Queued stage '{stage_id}' to open next."));
            self.pending_stage_open = Some(stage_id);
        } else {
            self.stage_id = stage_id;
            self.open_stage();
        }
    }

    fn open_stage(&mut self) {
        if self.background_receiver.is_some() {
            self.log
                .push("Another background operation is already running.".to_string());
            return;
        }
        let base_root = self.base_root.trim().to_string();
        let project_root = self.project_root.trim().to_string();
        let stage_id = self.stage_id.trim().to_string();
        if base_root.is_empty() || stage_id.is_empty() {
            self.log
                .push("Base root and stage are required.".to_string());
            return;
        }

        let visibility = self.preview_visibility();
        let existing_scene_scan = self.reusable_scene_scan(Path::new(&base_root));
        let existing_registry = self.registry.clone();
        let (sender, receiver) = mpsc::channel();
        thread::spawn(move || {
            let result = (|| -> Result<Box<LoadedStage>, String> {
                let authored_stage_ids = if project_root.is_empty() {
                    Vec::new()
                } else {
                    sms_scene::discover_authored_project_stage_ids(&project_root)
                        .map_err(|error| error.to_string())?
                };
                let requested_project_root = project_root;
                let (mut document, project_selection) = if authored_stage_ids
                    .iter()
                    .any(|authored| authored.eq_ignore_ascii_case(&stage_id))
                {
                    let document = StageDocument::open_authored_project_stage(
                        PathBuf::from(&base_root),
                        stage_id,
                        PathBuf::from(&requested_project_root),
                    )
                    .map_err(|error| error.to_string())?;
                    (
                        document,
                        ProjectLoadSelection {
                            project_root: requested_project_root.clone(),
                            warning: None,
                        },
                    )
                } else {
                    let mut document = StageDocument::open(PathBuf::from(&base_root), stage_id)
                        .map_err(|error| error.to_string())?;
                    let selection = load_project_for_stage(&mut document, &requested_project_root)
                        .map_err(|error| error.to_string())?;
                    (document, selection)
                };
                let registry = existing_registry;
                let schema_warning = None;
                if let Some(registry) = registry.clone() {
                    document = document.with_registry(registry);
                }
                let has_scene_index = existing_scene_scan.is_some();
                let SceneScanResult {
                    archives,
                    labels: scene_labels,
                    label_warning: scene_label_warning,
                    retail_skyboxes,
                    skybox_warnings,
                    retail_music,
                    retail_sounds,
                    retail_dialogue_voices,
                    retail_stage_audio,
                    music_warning,
                    object_authoring_catalog_cache,
                } = existing_scene_scan.unwrap_or_default();
                let (
                    object_authoring_catalog_key,
                    object_authoring_catalog,
                    object_authoring_catalog_warnings,
                ) = split_object_authoring_catalog_cache(object_authoring_catalog_cache);
                let scene = RenderScene::from_document(&document);
                let preview = SmsEditorApp::build_model_preview(&document, visibility);
                Ok(Box::new(LoadedStage {
                    base_root,
                    requested_project_root,
                    project_root: project_selection.project_root,
                    has_scene_index,
                    archives,
                    registry,
                    schema_warning,
                    object_authoring_catalog_key,
                    object_authoring_catalog,
                    object_authoring_catalog_warnings,
                    project_warning: project_selection.warning,
                    document,
                    scene,
                    preview,
                    scene_labels,
                    scene_label_warning,
                    retail_skyboxes,
                    skybox_warnings,
                    retail_music,
                    retail_sounds,
                    retail_dialogue_voices,
                    retail_stage_audio,
                    music_warning,
                }))
            })();
            let _ = sender.send(BackgroundResult::Open(result));
        });
        self.background_receiver = Some(receiver);
        self.background_label = Some(format!("Opening {}", self.stage_id));
        self.log
            .push(format!("Opening stage '{}'...", self.stage_id));
    }

    fn poll_background_task(&mut self, ctx: &egui::Context, frame: Option<&eframe::Frame>) {
        let result = self.background_receiver.as_ref().map(Receiver::try_recv);
        match result {
            Some(Ok(result)) => {
                self.background_receiver = None;
                self.background_label = None;
                self.active_build_cancel = None;
                match result {
                    BackgroundResult::Schema(result) => match *result {
                        Ok(loaded_schema) => {
                            let LoadedSchema {
                                registry,
                                object_authoring_catalog_cache,
                            } = loaded_schema;
                            self.log.push(format!(
                                "Generated {} object entries.",
                                registry.objects.len()
                            ));
                            if let Some(document) = &mut self.document {
                                document.refresh_registry_derived_object_fields(
                                    &mut self.saved_objects,
                                    &registry,
                                );
                                document.set_registry(registry.clone());
                                self.document_dirty = stage_document_differs_from_saved(
                                    document,
                                    &self.saved_objects,
                                    &self.saved_lighting,
                                    &self.saved_archive_edits,
                                    &self.saved_dialogue_authoring,
                                    &self.saved_dialogue_library,
                                );
                                self.issues = document.validate();
                            }
                            self.install_object_authoring_catalog_cache(None);
                            self.registry = Some(registry);
                            let expected_cache_key = self.registry.as_ref().map(|registry| {
                                object_authoring_catalog_cache_key(
                                    Path::new(self.base_root.trim()),
                                    &self.scene_archives,
                                    registry,
                                )
                            });
                            let refreshed_cache = object_authoring_catalog_cache
                                .filter(|cache| expected_cache_key.as_ref() == Some(&cache.key));
                            self.install_object_authoring_catalog_cache(refreshed_cache);
                            self.rebuild_model_preview_cache();
                            if self.document.is_some() {
                                self.rebuild_model_preview_from_document();
                            }
                            if self.pending_stage_open.is_none()
                                && self
                                    .reusable_scene_scan(Path::new(self.base_root.trim()))
                                    .is_none()
                            {
                                self.scan_scenes();
                            }
                        }
                        Err(err) => {
                            self.log.push(format!("Schema generation failed: {err}"));
                            if self.pending_stage_open.is_none()
                                && self
                                    .reusable_scene_scan(Path::new(self.base_root.trim()))
                                    .is_none()
                            {
                                self.scan_scenes();
                            }
                        }
                    },
                    BackgroundResult::Scan { base_root, result } => match result {
                        Ok(scan) => {
                            if self.base_root.trim() != base_root {
                                self.log.push(format!(
                                    "Discarded scene scan for superseded base root {base_root}."
                                ));
                                return;
                            }
                            let count = scan.archives.len();
                            let label_count = scan.labels.len();
                            let skybox_count = scan.retail_skyboxes.len();
                            let music_count = scan.retail_music.len();
                            let sound_count = scan.retail_sounds.len();
                            self.scene_archives = scan.archives;
                            self.retail_goop_templates.clear();
                            self.goop_templates_indexed = false;
                            self.scene_labels = scan.labels;
                            self.retail_skyboxes = scan.retail_skyboxes;
                            self.retail_music = scan.retail_music;
                            self.retail_sounds = scan.retail_sounds;
                            self.retail_dialogue_voices = scan.retail_dialogue_voices;
                            self.retail_stage_audio = scan.retail_stage_audio;
                            self.install_object_authoring_catalog_cache(
                                scan.object_authoring_catalog_cache,
                            );
                            self.last_scanned_base_root = base_root;
                            if self.stage_id.trim().is_empty() {
                                if let Some(first) = self.scene_archives.first() {
                                    self.stage_id = first.stage_id.clone();
                                }
                            }
                            self.log
                                .push(format!("Discovered {count} scene archive(s)."));
                            if label_count > 0 {
                                self.log.push(format!(
                                    "Loaded {label_count} localized scene label(s) from the extracted game."
                                ));
                            }
                            self.log.push(format!(
                                "Indexed {skybox_count} complete retail skybox bundle(s)."
                            ));
                            self.log.push(format!(
                                "Indexed {music_count} decomp-derived stage music choice(s)."
                            ));
                            self.log
                                .push(format!("Indexed {sound_count} exact retail sound name(s)."));
                            if self.registry.is_some() {
                                self.log.push(format!(
                                    "Indexed {} typed object class(es) for content-browser placement.",
                                    self.object_authoring_catalog.len()
                                ));
                            }
                            for warning in scan.skybox_warnings {
                                self.log.push(warning);
                            }
                            if let Some(warning) = scan.label_warning {
                                self.log.push(format!(
                                    "Scene names are unavailable; archive IDs remain active: {warning}"
                                ));
                            }
                            if let Some(warning) = scan.music_warning {
                                self.log.push(format!(
                                    "Stage music choices are unavailable: {warning}"
                                ));
                            }
                        }
                        Err(err) => self.log.push(format!("Scene scan failed: {err}")),
                    },
                    BackgroundResult::Open(result) => match result {
                        Ok(loaded) => self.apply_loaded_stage(*loaded),
                        Err(err) => self.log.push(format!("Open stage failed: {err}")),
                    },
                    BackgroundResult::CreateStage(result) => match result {
                        Ok(loaded) => {
                            let target = loaded.document.stage_id.clone();
                            let runtime_slot = loaded
                                .document
                                .changed_files
                                .get(Path::new("data/stageArc.bin"))
                                .and_then(|bytes| {
                                    sms_formats::parse_jdrama_scenario_archive_entries(bytes).ok()
                                })
                                .and_then(|entries| {
                                    entries.into_iter().find(|entry| {
                                        entry
                                            .archive_name
                                            .eq_ignore_ascii_case(&format!("{target}.arc"))
                                    })
                                });
                            self.stage_id = target.clone();
                            self.apply_loaded_stage(*loaded);
                            self.persist_project_settings(false);
                            if let Some(runtime_slot) = runtime_slot {
                                self.log.push(format!(
                                    "Created source-free stage '{target}' in new runtime slot area {}, scenario {}.",
                                    runtime_slot.area_index, runtime_slot.scenario_index
                                ));
                            } else {
                                self.log.push(format!(
                                    "Created and saved source-free stage '{target}' with a new project runtime slot."
                                ));
                            }
                            self.log.push(
                                "Author the scene by dropping terrain and Mario into the viewport, then set the skybox role, Stage Music, and Stage Lighting before runtime testing."
                                    .to_string(),
                            );
                        }
                        Err(err) => self.log.push(format!("Create stage failed: {err}")),
                    },
                    BackgroundResult::RetailSkybox(result) => match result {
                        Ok(selection) => self.apply_retail_skybox(selection),
                        Err(err) => self
                            .log
                            .push(format!("Retail skybox selection failed: {err}")),
                    },
                    BackgroundResult::GoopRebuild(result) => match result {
                        Ok(outcome) => self.apply_goop_rebuild(*outcome),
                        Err(err) if err == "goop rebuild cancelled" => {
                            self.log.push("Goop rebuild cancelled.".to_string())
                        }
                        Err(err) => self.log.push(format!("Goop rebuild failed: {err}")),
                    },
                    BackgroundResult::Build(result) => match result {
                        Ok(outcome) => {
                            self.log.push(format!(
                                "Built runnable {}-byte stage at '{}' ({} independent copies, {} reused).",
                                outcome.run.stage_size_bytes,
                                outcome.run.stage_output_path.display(),
                                outcome.run.copied_files,
                                outcome.run.reused_files,
                            ));
                            self.log.push(format!(
                                "Managed game directory: '{}'. The extracted base game was not modified.",
                                outcome.run.run_root.display(),
                            ));
                            if let Some(count) = self
                                .current_project
                                .as_ref()
                                .map(|project| project.descriptor.stage_music.len())
                                .filter(|count| *count > 0)
                            {
                                self.log.push(format!(
                                    "Installed {count} stage music choice(s) into the packaged sys/main.dol for normal Dolphin boot."
                                ));
                            }
                        }
                        Err(err) if managed_build::is_cancelled_error(&err) => {
                            self.log.push(format!("Game build cancelled: {err}"))
                        }
                        Err(err) => self.log.push(format!("Game build failed: {err}")),
                    },
                    BackgroundResult::BuildAndRun { mode, result } => match result {
                        Ok(outcome) => {
                            self.log.push(format!(
                                "Built runnable {}-byte stage at '{}' ({} independent copies, {} reused).",
                                outcome.run.stage_size_bytes,
                                outcome.run.stage_output_path.display(),
                                outcome.run.copied_files,
                                outcome.run.reused_files,
                            ));
                            self.log.push(format!(
                                "Managed game directory: '{}'. The extracted base game was not modified.",
                                outcome.run.run_root.display(),
                            ));
                            if let Some(count) = self
                                .current_project
                                .as_ref()
                                .map(|project| project.descriptor.stage_music.len())
                                .filter(|count| *count > 0)
                            {
                                self.log.push(format!(
                                    "Installed {count} stage music choice(s) into the packaged sys/main.dol for normal Dolphin boot."
                                ));
                            }
                            self.log.push(format!(
                                "Prepared {} {}-byte direct-boot executable '{}' for '{}' at runtime area {}, scenario {} (logo bypass 0x{:08X}, hook 0x{:08X}, movie hook 0x{:08X}, stub 0x{:08X}).",
                                if outcome.direct_boot.reused {
                                    "unchanged"
                                } else {
                                    "updated"
                                },
                                outcome.direct_boot.size_bytes,
                                outcome.direct_boot.launch_dol.display(),
                                outcome.direct_boot.target.archive_name,
                                outcome.direct_boot.target.area_index,
                                outcome.direct_boot.target.scenario_index,
                                outcome.direct_boot.logo_bypass_address,
                                outcome.direct_boot.hook_address,
                                outcome.direct_boot.movie_hook_address,
                                outcome.direct_boot.stub_address,
                            ));
                            if outcome.direct_boot.matching_contexts > 1 {
                                self.log.push(format!(
                                    "The archive has {} runtime contexts in stageArc.bin; direct boot uses the first table entry (area {}, scenario {}).",
                                    outcome.direct_boot.matching_contexts,
                                    outcome.direct_boot.target.area_index,
                                    outcome.direct_boot.target.scenario_index,
                                ));
                            }
                            self.launch_managed_dolphin(&outcome, mode, frame);
                        }
                        Err(err) if managed_build::is_cancelled_error(&err) => self
                            .log
                            .push(format!("{} cancelled: {err}", mode.progress_label())),
                        Err(err) => self
                            .log
                            .push(format!("{} failed: {err}", mode.progress_label())),
                    },
                }
                if self.background_receiver.is_none() && !self.is_dirty() {
                    if let Some(stage_id) = self.pending_stage_open.take() {
                        self.stage_id = stage_id;
                        self.open_stage();
                    }
                }
            }
            Some(Err(TryRecvError::Empty)) => {
                ctx.request_repaint_after(std::time::Duration::from_millis(33));
            }
            Some(Err(TryRecvError::Disconnected)) => {
                self.background_receiver = None;
                self.background_label = None;
                self.active_build_cancel = None;
                self.log
                    .push("Background operation ended unexpectedly.".to_string());
            }
            None => {}
        }
    }

    fn apply_loaded_stage(&mut self, loaded: LoadedStage) {
        self.stop_audio_preview();
        let LoadedStage {
            base_root,
            requested_project_root,
            project_root,
            has_scene_index,
            archives,
            registry,
            schema_warning,
            object_authoring_catalog_key,
            object_authoring_catalog,
            object_authoring_catalog_warnings,
            project_warning,
            mut document,
            scene,
            preview,
            scene_labels,
            scene_label_warning,
            retail_skyboxes,
            skybox_warnings,
            retail_music,
            retail_sounds,
            retail_dialogue_voices,
            retail_stage_audio,
            music_warning,
        } = loaded;
        if self.base_root.trim() != base_root {
            self.log.push(format!(
                "Discarded stage load for superseded base root {base_root}."
            ));
            return;
        }
        if self.project_root.trim() != requested_project_root {
            self.log.push(format!(
                "Discarded stage load for superseded project root {requested_project_root}."
            ));
            return;
        }
        if self.stage_id.trim() != document.stage_id {
            self.log.push(format!(
                "Discarded stage load for superseded stage {}.",
                document.stage_id
            ));
            return;
        }
        if let Some(warning) = schema_warning {
            self.log.push(format!(
                "Schema generation failed; stage opened without it: {warning}"
            ));
        }
        if let Some(warning) = project_warning {
            self.log.push(warning);
        }
        if let Some(warning) = scene_label_warning {
            self.log.push(format!(
                "Scene names are unavailable; archive IDs remain active: {warning}"
            ));
        }
        if let Some(warning) = music_warning {
            self.log
                .push(format!("Stage music choices are unavailable: {warning}"));
        }
        self.project_root = project_root;
        self.adopt_resolved_project_data_root();
        self.registry = registry;
        if self.registry.is_some() && has_scene_index {
            self.log.push(format!(
                "Object authoring catalog indexed {} typed class(es); content-browser placement can add them with automatic manager and resource dependencies.",
                object_authoring_catalog.len()
            ));
        } else if self.registry.is_none() {
            self.log.push(
                "Object authoring catalog is unavailable until object schema generation succeeds."
                    .to_string(),
            );
        } else {
            self.log.push(
                "Opened the level first; base-game content and typed object choices are continuing to index in the background."
                    .to_string(),
            );
        }
        for warning in object_authoring_catalog_warnings.iter().take(12) {
            self.log.push(format!(
                "Object authoring catalog warning [{}]: {}",
                warning.source_stage, warning.message
            ));
        }
        if object_authoring_catalog_warnings.len() > 12 {
            self.log.push(format!(
                "Object authoring catalog omitted {} additional warning(s).",
                object_authoring_catalog_warnings.len() - 12
            ));
        }
        if has_scene_index {
            let object_authoring_catalog_cache =
                object_authoring_catalog_key.map(|key| ObjectAuthoringCatalogCache {
                    key,
                    catalog: object_authoring_catalog,
                    warnings: object_authoring_catalog_warnings,
                });
            self.install_object_authoring_catalog_cache(object_authoring_catalog_cache);
            self.scene_archives = archives;
        } else {
            self.install_object_authoring_catalog_cache(None);
        }
        self.retail_goop_templates.clear();
        self.goop_templates_indexed = false;
        self.selected_goop_layer = 0;
        self.selected_goop_template = 0;
        self.goop_stroke = None;
        self.goop_undo_stack.clear();
        self.goop_redo_stack.clear();
        if has_scene_index {
            self.scene_labels = scene_labels;
            self.retail_skyboxes = retail_skyboxes;
            self.retail_music = retail_music;
            self.retail_sounds = retail_sounds;
            self.retail_dialogue_voices = retail_dialogue_voices;
            self.retail_stage_audio = retail_stage_audio;
            self.last_scanned_base_root = self.base_root.trim().to_string();
        }
        self.log.push(format!(
            "Opened stage '{}' with {} asset(s), {} model(s), {} collision file(s). You can add typed object classes with automatic dependencies from the content browser.",
            document.stage_id,
            document.assets.len(),
            scene.model_paths.len(),
            scene.collision_paths.len()
        ));
        for warning in skybox_warnings {
            self.log.push(warning);
        }
        if let Some(preview) = &preview {
            self.log.push(format!(
                "Viewport preview loaded {} model(s), {} sampled point(s), {} triangle(s), {} texture(s), {} BTK material animation(s), {} BCK skeletal animation(s), {} procedural map-joint transformation(s), {} level-change JPA effect(s), {} actor-bound JPA effect(s), {} source vertex/vertices.",
                preview.loaded_models,
                preview.points.len(),
                preview.triangles.len(),
                preview.textures.len(),
                preview.texture_srt_animations.len(),
                preview.animated_models.len(),
                preview
                    .level_transform_models
                    .iter()
                    .map(|model| model.targets.len())
                    .sum::<usize>(),
                preview.level_transform_particles.len(),
                preview.actor_particles.len(),
                preview.source_vertices
            ));
            self.log.push(format!(
                "Collision preview loaded {} file(s), {} triangle(s), and {} surface type(s).",
                preview.collision_file_count,
                preview.collision_triangles.len(),
                preview.collision_surface_count
            ));
        } else if !scene.model_paths.is_empty() {
            self.log
                .push("Viewport preview could not decode BMD vertex data.".to_string());
        }
        self.issues = validation_issues_for_preview(&document, preview.as_ref());
        for issue in self
            .issues
            .iter()
            .filter(|issue| issue.code.starts_with("renderer-"))
        {
            self.log.push(format!(
                "Renderer warning [{}] {}",
                issue.code, issue.message
            ));
        }
        if let Err(error) = document.ensure_route_authoring() {
            self.log.push(format!("Routes unavailable: {error}"));
        }
        self.active_route_graph = document
            .route_authoring
            .as_ref()
            .and_then(|routes| routes.graphs.first())
            .map(|graph| graph.id.clone());
        self.selected_route_controls.clear();
        self.selected_route_link = None;
        self.route_curve_confirmation = None;
        self.pending_route_assignment = None;
        self.route_handle_drag = None;
        self.saved_objects = document.objects.clone();
        self.saved_lighting = document.lighting.clone();
        self.saved_archive_edits = document.archive_edits.clone();
        self.saved_dialogue_authoring = document.dialogue_authoring.clone();
        self.saved_dialogue_library = document.dialogue_library.clone();
        self.document_dirty = false;
        self.stage_id = document.stage_id.clone();
        self.document = Some(document);
        self.dialogue_route_index = None;
        self.dialogue_consumer_index = None;
        self.dialogue_index_error = None;
        self.dialogue_consumer_error = None;
        self.dialogue_shared_confirmation = None;
        self.dialogue_undo_transaction = None;
        self.schedule_dialogue_index_rebuild();
        self.schedule_dialogue_consumer_index_rebuild();
        self.rebuild_audio_cube_helpers_cache();
        self.render_scene = Some(scene);
        self.reset_authored_model_preview_base();
        self.model_preview = preview;
        self.reconcile_loaded_authored_catalog_resources();
        self.animation_started_at = Instant::now();
        self.last_skeletal_animation_tick = u64::MAX;
        self.level_transform_progress = FULL_DELFINO_PROGRESSION;
        self.level_transform_playing = false;
        self.level_transform_playback_origin = 0.0;
        self.last_level_transform_progress_bits = u32::MAX;
        self.rebuild_gpu_viewport_scene();
        self.clear_viewport_preview_cache();
        self.selected_object_id = None;
        self.selected_model_instance_id = None;
        self.active_placement = None;
        self.undo_stack.clear();
        self.redo_stack.clear();
        self.undo_transaction = None;
        self.route_undo_transaction = None;
        self.reset_camera();
        self.restore_project_camera_state();
        self.apply_startup_camera_focus();
        if !has_scene_index && self.pending_stage_open.is_none() {
            if self.registry.is_some() {
                self.scan_scenes();
            } else {
                self.generate_schema(false);
            }
        }
    }

    fn build_model_preview(
        document: &StageDocument,
        visibility: PreviewVisibility,
    ) -> Option<ModelPreview> {
        const POINT_BUDGET: usize = 16_000;
        const POINTS_PER_MODEL: usize = 1_200;
        const POINTS_PER_OBJECT_INSTANCE: usize = 500;

        let active_pollution_layers = active_pollution_layer_count(document);
        let mirror_cubes = mirror_cubes(document);
        let active_mirror_model_slots = mirror_cubes
            .iter()
            .map(|cube| cube.model_slot)
            .collect::<BTreeSet<_>>();
        let instanced_model_paths = document
            .objects
            .iter()
            .flat_map(|object| object.asset_hints.iter())
            .filter(|hint| {
                matches!(
                    hint.role,
                    AssetRole::PreviewModel | AssetRole::InferredPreviewModel
                )
            })
            .map(|hint| normalized_preview_asset_path(&hint.path))
            .collect::<BTreeSet<_>>();
        let models: Vec<_> = document
            .assets
            .iter()
            .filter(|asset| asset.kind == StageAssetKind::Model)
            .filter(|asset| {
                !instanced_model_paths.contains(&normalized_preview_asset_path(
                    &asset.path.to_string_lossy(),
                ))
            })
            .filter(|asset| {
                generated_goop_model_visibility(document, &asset.path.to_string_lossy())
                    .unwrap_or_else(|| {
                        map_static_model_is_active(document, &asset.path.to_string_lossy())
                    })
            })
            .filter(|asset| {
                mirror_surface_model_is_active(
                    &document.stage_id,
                    &asset.path.to_string_lossy(),
                    &active_mirror_model_slots,
                )
            })
            .filter(|asset| {
                pollution_layer_model_is_active(
                    &asset.path.to_string_lossy(),
                    active_pollution_layers,
                )
            })
            .collect();
        let has_default_preview_models = models.iter().any(|asset| {
            is_default_preview_model_path(
                &asset.path.to_string_lossy().replace('\\', "/"),
                true,
                true,
                true,
            )
        });
        let preferred: Vec<_> = models
            .iter()
            .copied()
            .filter(|asset| {
                let path = asset.path.to_string_lossy().replace('\\', "/");
                is_default_preview_model_path(
                    &path,
                    visibility.environment,
                    visibility.goop,
                    visibility.effects,
                )
            })
            .collect();
        let models = if preferred.is_empty() && !has_default_preview_models {
            models
        } else {
            preferred
        };
        let world_model_paths: BTreeSet<_> = models
            .iter()
            .map(|asset| normalized_preview_asset_path(&asset.path.to_string_lossy()))
            .collect();

        let mut points = Vec::new();
        let mut triangles = Vec::new();
        let mut textures = Vec::new();
        let mut materials = Vec::new();
        let mut texture_srt_animations = Vec::new();
        let mut texture_pattern_animations = Vec::new();
        let mut material_animation_bindings = Vec::new();
        let mut pollution_texture_indices = BTreeMap::<usize, Vec<usize>>::new();
        let mut animated_flags = Vec::new();
        let mut level_transform_models = Vec::new();
        let mut level_transform_particles = Vec::new();
        let mut actor_particles = Vec::new();
        let actor_particle_effects = if visibility.effects {
            load_actor_particle_effects(document)
        } else {
            BTreeMap::new()
        };
        let mut next_packet_index = 0usize;
        let mut bounds_min = [f32::INFINITY; 3];
        let mut bounds_max = [f32::NEG_INFINITY; 3];
        let mut camera_bounds_min = [f32::INFINITY; 3];
        let mut camera_bounds_max = [f32::NEG_INFINITY; 3];
        let mut camera_bound_points = Vec::new();
        let mut loaded_models = 0;
        let mut failed_model_assets = BTreeSet::new();
        let mut model_failures = Vec::new();
        let mut source_vertices = 0;
        let mut source_triangles = 0;
        let mut source_textures = 0;
        let mut goop_surface_model_indices = BTreeSet::new();
        let mut object_model_indices = BTreeMap::new();
        let mut mirror_actor_positions = BTreeMap::new();
        let mut mirror_model_slots = BTreeMap::new();
        let collision_preview = build_collision_preview(document);

        for asset in models {
            let asset_path = asset.path.to_string_lossy().replace('\\', "/");
            let include_in_camera_bounds = is_camera_bounds_model_path(&asset_path);
            let is_goop_surface_model = model_path_is_map_terrain(&asset_path);
            let model_render_layer = preview_render_layer_for_model_path(&asset_path);
            if !visibility.effects && preview_render_layer_is_effect(model_render_layer) {
                continue;
            }
            let is_sky = model_render_layer == PreviewRenderLayer::Sky;
            let bytes = match document.read_asset_bytes(&asset.path) {
                Ok(bytes) => bytes,
                Err(error) => {
                    record_model_preview_failure(
                        &mut failed_model_assets,
                        &mut model_failures,
                        &asset_path,
                        format!("read asset: {error}"),
                    );
                    continue;
                }
            };
            let file = match J3dFile::parse(&bytes) {
                Ok(file) => file,
                Err(error) => {
                    record_model_preview_failure(
                        &mut failed_model_assets,
                        &mut model_failures,
                        &asset_path,
                        format!("parse J3D: {error}"),
                    );
                    continue;
                }
            };

            let loader_flags = map_static_model_loader_flags(document, &asset_path)
                .unwrap_or_else(|| model_loader_flags_for_path(&asset_path));
            let level_targets = level_transform_targets(document, &asset_path, &file);
            let initial_overrides =
                level_transform_overrides(&level_targets, FULL_DELFINO_PROGRESSION);
            let preview_result = if initial_overrides.is_empty() {
                file.geometry_preview_with_loader_flags(loader_flags)
            } else {
                file.geometry_preview_with_joint_overrides(loader_flags, &initial_overrides)
            };
            match preview_result {
                Ok(mut preview) => {
                    apply_level_transform_visibility(
                        &file,
                        &level_targets,
                        FULL_DELFINO_PROGRESSION,
                        &mut preview.triangles,
                    );
                    apply_model_material_table(document, &asset_path, loader_flags, &mut preview);
                    apply_pollution_bitmap_mask(document, &asset_path, &mut preview);
                    loaded_models += 1;
                    let model_index = loaded_models;
                    if let Some(slot) = mirror_surface_model_slot(&document.stage_id, &asset_path) {
                        mirror_model_slots.insert(model_index, slot);
                    }
                    let texture_base = push_preview_textures(&mut textures, &preview);
                    if let (Some(layer_index), Some(dynamic_name)) = (
                        pollution_layer_model_index(&asset_path),
                        preview
                            .textures
                            .first()
                            .map(|texture| texture.name.as_str()),
                    ) {
                        pollution_texture_indices.insert(
                            layer_index,
                            preview
                                .textures
                                .iter()
                                .enumerate()
                                .filter_map(|(index, texture)| {
                                    (texture.name == dynamic_name).then_some(texture_base + index)
                                })
                                .collect(),
                        );
                    }
                    let material_base =
                        push_preview_materials(&mut materials, &preview, texture_base);
                    material_animation_bindings.resize_with(materials.len(), Vec::new);
                    attach_model_texture_srt_animation(
                        document,
                        &asset_path,
                        material_base,
                        &preview.materials,
                        &mut texture_srt_animations,
                        &mut material_animation_bindings,
                    );
                    let packet_base = next_packet_index;
                    next_packet_index += preview
                        .triangles
                        .iter()
                        .map(|triangle| triangle.packet_index)
                        .max()
                        .map(|index| index + 1)
                        .unwrap_or(1);
                    source_vertices += preview.positions.len();
                    source_triangles += preview.triangles.len();
                    source_textures += preview.textures.len();
                    if !is_sky {
                        merge_bounds(
                            &mut bounds_min,
                            &mut bounds_max,
                            preview.bounds_min,
                            preview.bounds_max,
                        );
                    }
                    if include_in_camera_bounds {
                        merge_bounds(
                            &mut camera_bounds_min,
                            &mut camera_bounds_max,
                            preview.bounds_min,
                            preview.bounds_max,
                        );
                        camera_bound_points.extend(preview.positions.iter().copied());
                    }

                    let point_stride = (preview.positions.len() / POINTS_PER_MODEL).max(1);
                    let point_start = points.len();
                    if !is_sky {
                        for position in preview.positions.iter().step_by(point_stride) {
                            points.push(PreviewPoint {
                                position: *position,
                                model_index,
                            });
                        }
                    }

                    let triangle_start = triangles.len();
                    for triangle in &preview.triangles {
                        if triangle_vertices_are_finite(triangle.vertices) {
                            triangles.push(PreviewTriangle {
                                vertices: triangle.vertices,
                                normals: triangle.normals,
                                color_channels: triangle.color_channels,
                                tex_coord_sets: triangle.tex_coord_sets,
                                material_index: triangle.material_index.and_then(|index| {
                                    let global_index = material_base + index;
                                    (global_index < materials.len()).then_some(global_index)
                                }),
                                packet_index: packet_base + triangle.packet_index,
                                model_index,
                                render_layer: model_render_layer,
                                color: triangle.color,
                                vertex_colors: triangle.vertex_colors,
                                combine_mode: triangle.combine_mode,
                                tex_coords: triangle.tex_coords,
                                texture_index: triangle.texture_index.and_then(|index| {
                                    let global_index = texture_base + index;
                                    (global_index < textures.len()).then_some(global_index)
                                }),
                                mask_tex_coords: triangle.mask_tex_coords,
                                mask_texture_index: triangle.mask_texture_index.and_then(|index| {
                                    let global_index = texture_base + index;
                                    (global_index < textures.len()).then_some(global_index)
                                }),
                                cull_mode: triangle.cull_mode,
                                alpha_compare: triangle.alpha_compare,
                                blend_mode: triangle.blend_mode,
                                z_mode: triangle.z_mode,
                                billboard: triangle.billboard,
                                particle_type: None,
                                particle_pivot: None,
                                particle_direction: None,
                                particle_color_mode: None,
                                particle_environment_color: None,
                            });
                        }
                    }
                    if is_goop_surface_model && triangles.len() > triangle_start {
                        goop_surface_model_indices.insert(model_index);
                    }
                    if !level_targets.is_empty() {
                        level_transform_models.push(LevelTransformModelPreview {
                            file: file.clone(),
                            loader_flags,
                            targets: level_targets,
                            point_range: point_start..points.len(),
                            point_stride,
                            triangle_range: triangle_start..triangles.len(),
                        });
                    }
                }
                Err(geometry_error) => {
                    let preview = match file.vertex_preview() {
                        Ok(preview) => preview,
                        Err(vertex_error) => {
                            record_model_preview_failure(
                                &mut failed_model_assets,
                                &mut model_failures,
                                &asset_path,
                                format!(
                                    "decode geometry: {geometry_error}; fallback vertex preview: {vertex_error}"
                                ),
                            );
                            continue;
                        }
                    };

                    loaded_models += 1;
                    let model_index = loaded_models;
                    source_vertices += preview.positions.len();
                    if !is_sky {
                        merge_bounds(
                            &mut bounds_min,
                            &mut bounds_max,
                            preview.bounds_min,
                            preview.bounds_max,
                        );
                    }
                    if include_in_camera_bounds {
                        merge_bounds(
                            &mut camera_bounds_min,
                            &mut camera_bounds_max,
                            preview.bounds_min,
                            preview.bounds_max,
                        );
                        camera_bound_points.extend(preview.positions.iter().copied());
                    }

                    if !is_sky {
                        let stride = (preview.positions.len() / POINTS_PER_MODEL).max(1);
                        for position in preview.positions.iter().step_by(stride) {
                            points.push(PreviewPoint {
                                position: *position,
                                model_index,
                            });
                        }
                    }
                }
            }
        }

        let world_triangle_end = triangles.len();
        if visibility.environment {
            let wave = build_procedural_wave_preview(
                document,
                loaded_models + 1,
                next_packet_index,
                &mut textures,
                &mut materials,
            );
            if wave.triangle_count != 0 {
                loaded_models += 1;
                source_vertices += wave.source_vertices;
                source_triangles += wave.triangle_count;
                source_textures += 1;
                triangles.extend(wave.triangles);
                next_packet_index += 1;
                material_animation_bindings.resize_with(materials.len(), Vec::new);
            }

            let grass =
                build_procedural_grass_preview(document, loaded_models + 1, next_packet_index);
            if grass.group_count != 0 {
                loaded_models += grass.group_count;
                source_vertices += grass.blade_count * 3;
                source_triangles += grass.blade_count;
                merge_bounds(
                    &mut bounds_min,
                    &mut bounds_max,
                    grass.bounds_min,
                    grass.bounds_max,
                );
                points.extend(grass.points);
                triangles.extend(grass.triangles);
                for (object_id, model_index) in grass.object_model_indices {
                    if let Some(object) = document
                        .objects
                        .iter()
                        .find(|object| object.id == object_id)
                    {
                        mirror_actor_positions.insert(model_index, object.transform.translation);
                    }
                    object_model_indices.insert(object_id, model_index);
                }
                next_packet_index += 1;
            }

            let wires = build_procedural_wire_preview(
                document,
                loaded_models + 1,
                next_packet_index,
                &mut textures,
                &mut materials,
            );
            if wires.wire_count != 0 {
                loaded_models += wires.wire_count;
                source_vertices += wires.source_vertices;
                source_triangles += wires.source_triangles;
                source_textures += wires.source_textures;
                merge_bounds(
                    &mut bounds_min,
                    &mut bounds_max,
                    wires.bounds_min,
                    wires.bounds_max,
                );
                points.extend(wires.points);
                triangles.extend(wires.triangles);
                next_packet_index += wires.packet_count;
                material_animation_bindings.resize_with(materials.len(), Vec::new);
            }

            let flags = build_procedural_flag_preview(
                document,
                loaded_models + 1,
                next_packet_index,
                &mut textures,
                &mut materials,
            );
            if flags.flag_count != 0 {
                let point_base = points.len();
                let triangle_base = triangles.len();
                loaded_models += flags.flag_count;
                source_vertices += flags.source_vertices;
                source_triangles += flags.source_triangles;
                source_textures += flags.source_textures;
                merge_bounds(
                    &mut bounds_min,
                    &mut bounds_max,
                    flags.bounds_min,
                    flags.bounds_max,
                );
                points.extend(flags.points);
                triangles.extend(flags.triangles);
                animated_flags.extend(flags.animated_flags.into_iter().map(|mut animation| {
                    animation.point_range = (animation.point_range.start + point_base)
                        ..(animation.point_range.end + point_base);
                    animation.triangle_range = (animation.triangle_range.start + triangle_base)
                        ..(animation.triangle_range.end + triangle_base);
                    animation
                }));
                for (object_id, model_index) in flags.object_model_indices {
                    if let Some(object) = document
                        .objects
                        .iter()
                        .find(|object| object.id == object_id)
                    {
                        mirror_actor_positions.insert(model_index, object.transform.translation);
                    }
                    object_model_indices.insert(object_id, model_index);
                }
                next_packet_index += flags.packet_count;
                material_animation_bindings.resize_with(materials.len(), Vec::new);
            }
        }
        let mut object_model_cache =
            BTreeMap::<(String, u32, String), CachedObjectModelPreview>::new();
        let mut accessory_model_cache = BTreeMap::<String, CachedAccessoryModelPreview>::new();
        for object in &document.objects {
            let explicit_model_path = object_preview_model_path(object, &world_model_paths);
            let catalog_preview = explicit_model_path
                .is_none()
                .then(|| document.actor_preview(object))
                .flatten()
                .filter(|preview| is_supported_object_preview_model_path(&preview.model_path));
            let model_path = explicit_model_path
                .or_else(|| catalog_preview.map(|preview| preview.model_path.clone()))
                .or_else(|| object_inferred_preview_model_path(object, &world_model_paths));
            let Some(model_path) = model_path else {
                continue;
            };
            // TMapStaticObj::initUnique submits ReflectSky only to Sunshine's
            // mirror-sky draw buffers. The editor mirrors the already-loaded
            // main sky instead, so instantiating this helper as a world object
            // produces a second, enormous sky dome at the stage origin.
            if path_is_mirror_sky_helper_model_path(&model_path) {
                continue;
            }
            let loader_flags = catalog_preview
                .map(|preview| preview.load_flags)
                .or_else(|| document.object_preview_load_flags(object))
                .or_else(|| actor_model_loader_flags(object))
                .unwrap_or_else(|| model_loader_flags_for_path(&model_path));
            let object_render_layer = preview_render_layer_for_model_path(&model_path);
            if !visibility.effects && preview_render_layer_is_effect(object_render_layer) {
                continue;
            }
            let object_preview_transform = if object_render_layer == PreviewRenderLayer::Heatwave {
                shimmer_preview_transform(object.transform)
            } else {
                reset_fruit_preview_transform(object, object.transform, document.registry.as_ref())
            };
            let object_preview_transform = actor_runtime_preview_transform(
                object_preview_transform,
                document.actor_preview(object),
            );
            let animation_profile = std::iter::once(object.factory_name.clone())
                .chain(
                    npc_accessory_specs(document, object)
                        .into_iter()
                        .filter(|part| part.joint_name.is_none())
                        .map(|part| part.asset_suffix.to_ascii_lowercase()),
                )
                .collect::<Vec<_>>()
                .join("|");
            let model_cache_key = (model_path.clone(), loader_flags, animation_profile);

            if !object_model_cache.contains_key(&model_cache_key) {
                if failed_model_assets.contains(&normalized_preview_asset_path(&model_path)) {
                    continue;
                }
                let bytes = match document.read_asset_bytes(&model_path) {
                    Ok(bytes) => bytes,
                    Err(error) => {
                        record_model_preview_failure(
                            &mut failed_model_assets,
                            &mut model_failures,
                            &model_path,
                            format!("read actor model: {error}"),
                        );
                        continue;
                    }
                };
                let file = match J3dFile::parse(&bytes) {
                    Ok(file) => file,
                    Err(error) => {
                        record_model_preview_failure(
                            &mut failed_model_assets,
                            &mut model_failures,
                            &model_path,
                            format!("parse actor J3D: {error}"),
                        );
                        continue;
                    }
                };
                let joint_animation = starting_joint_animation(document, object, &model_path);
                let prepared_triangles = joint_animation
                    .as_ref()
                    .and_then(|_| file.prepare_animated_triangles().ok())
                    .map(Arc::new);
                let preview_result = joint_animation.as_ref().map_or_else(
                    || file.geometry_preview_with_loader_flags(loader_flags),
                    |animation| {
                        file.geometry_preview_with_joint_animation(loader_flags, animation, 0.0)
                    },
                );
                let mut preview = match preview_result {
                    Ok(preview) => preview,
                    Err(error) => {
                        record_model_preview_failure(
                            &mut failed_model_assets,
                            &mut model_failures,
                            &model_path,
                            format!("decode actor geometry: {error}"),
                        );
                        continue;
                    }
                };
                apply_model_material_table(document, &model_path, loader_flags, &mut preview);
                apply_actor_runtime_textures(document, object, &mut preview);
                apply_npc_eye_decal_culling(object, &mut preview);
                let texture_base = push_preview_textures(&mut textures, &preview);
                let material_base = push_preview_materials(&mut materials, &preview, texture_base);
                let joint_names = file.joint_names().unwrap_or_default();
                object_model_cache.insert(
                    model_cache_key.clone(),
                    CachedObjectModelPreview {
                        file,
                        joint_animation,
                        prepared_triangles,
                        loader_flags,
                        preview,
                        texture_base,
                        material_base,
                        joint_names,
                        instances: Vec::new(),
                    },
                );
            }

            let Some(cached) = object_model_cache.get(&model_cache_key) else {
                continue;
            };
            let object_material_base = push_object_preview_materials(
                &mut materials,
                cached,
                object,
                document.registry.as_ref(),
            );
            material_animation_bindings.resize_with(materials.len(), Vec::new);
            attach_model_texture_srt_animation(
                document,
                &model_path,
                object_material_base,
                &cached.preview.materials,
                &mut texture_srt_animations,
                &mut material_animation_bindings,
            );
            attach_npc_texture_pattern_animation(
                document,
                object,
                &model_path,
                object_material_base,
                cached.texture_base,
                &cached.preview.materials,
                &mut materials,
                &mut texture_pattern_animations,
            );
            loaded_models += 1;
            let model_index = loaded_models;
            object_model_indices.insert(object.id.clone(), model_index);
            mirror_actor_positions.insert(model_index, object_preview_transform.translation);
            source_vertices += cached.preview.positions.len();
            source_triangles += cached.preview.triangles.len();
            source_textures += cached.preview.textures.len();
            let packet_base = next_packet_index;
            next_packet_index += cached
                .preview
                .triangles
                .iter()
                .map(|triangle| triangle.packet_index)
                .max()
                .map(|index| index + 1)
                .unwrap_or(1);

            if object_render_layer != PreviewRenderLayer::Heatwave {
                let transformed_bounds_min =
                    transform_preview_point(cached.preview.bounds_min, object_preview_transform);
                let transformed_bounds_max =
                    transform_preview_point(cached.preview.bounds_max, object_preview_transform);
                merge_bounds(
                    &mut bounds_min,
                    &mut bounds_max,
                    [
                        transformed_bounds_min[0].min(transformed_bounds_max[0]),
                        transformed_bounds_min[1].min(transformed_bounds_max[1]),
                        transformed_bounds_min[2].min(transformed_bounds_max[2]),
                    ],
                    [
                        transformed_bounds_min[0].max(transformed_bounds_max[0]),
                        transformed_bounds_min[1].max(transformed_bounds_max[1]),
                        transformed_bounds_min[2].max(transformed_bounds_max[2]),
                    ],
                );
            }

            let point_stride = (cached.preview.positions.len() / POINTS_PER_OBJECT_INSTANCE).max(1);
            let point_start = points.len();
            if object_render_layer != PreviewRenderLayer::Heatwave {
                for position in cached.preview.positions.iter().step_by(point_stride) {
                    points.push(PreviewPoint {
                        position: transform_preview_point(*position, object_preview_transform),
                        model_index,
                    });
                }
            }

            let triangle_start = triangles.len();
            for triangle in &cached.preview.triangles {
                let vertices =
                    transform_preview_vertices(triangle.vertices, object_preview_transform);
                if triangle_vertices_are_finite(vertices) {
                    triangles.push(PreviewTriangle {
                        vertices,
                        normals: triangle.normals.map(|normals| {
                            transform_preview_normals(normals, object_preview_transform)
                        }),
                        color_channels: triangle.color_channels,
                        tex_coord_sets: triangle.tex_coord_sets,
                        material_index: triangle.material_index.and_then(|index| {
                            let global_index = object_material_base + index;
                            (global_index < materials.len()).then_some(global_index)
                        }),
                        packet_index: packet_base + triangle.packet_index,
                        model_index,
                        render_layer: object_render_layer,
                        color: triangle.color,
                        vertex_colors: triangle.vertex_colors,
                        combine_mode: triangle.combine_mode,
                        tex_coords: triangle.tex_coords,
                        texture_index: triangle.texture_index.and_then(|index| {
                            let global_index = cached.texture_base + index;
                            (global_index < textures.len()).then_some(global_index)
                        }),
                        mask_tex_coords: triangle.mask_tex_coords,
                        mask_texture_index: triangle.mask_texture_index.and_then(|index| {
                            let global_index = cached.texture_base + index;
                            (global_index < textures.len()).then_some(global_index)
                        }),
                        cull_mode: triangle.cull_mode,
                        alpha_compare: triangle.alpha_compare,
                        blend_mode: triangle.blend_mode,
                        z_mode: triangle.z_mode,
                        billboard: triangle.billboard.and_then(|billboard| {
                            transform_j3d_billboard(
                                billboard,
                                object_preview_transform,
                                triangle.normals.map(|normals| {
                                    transform_preview_normals(normals, object_preview_transform)
                                }),
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
            let body_triangle_end = triangles.len();
            let mut accessories = Vec::new();
            let joint_matrices = cached
                .joint_animation
                .as_ref()
                .map_or_else(
                    || cached.file.joint_matrices(cached.loader_flags),
                    |animation| {
                        cached.file.joint_matrices_with_joint_animation(
                            cached.loader_flags,
                            animation,
                            0.0,
                        )
                    },
                )
                .unwrap_or_default();
            push_actor_particle_previews(
                document,
                object,
                &actor_particle_effects,
                &joint_matrices,
                object_preview_transform,
                model_index,
                &mut textures,
                &mut triangles,
                &mut next_packet_index,
                &mut actor_particles,
            );
            for spec in npc_accessory_specs(document, object) {
                let joint_index = match spec.joint_name.as_deref() {
                    Some(joint_name) => {
                        let Some(index) = cached
                            .joint_names
                            .iter()
                            .position(|name| name.eq_ignore_ascii_case(joint_name))
                        else {
                            continue;
                        };
                        Some(index)
                    }
                    None => None,
                };
                let joint_matrix = match joint_index {
                    Some(index) => {
                        let Some(matrix) = joint_matrices.get(index).copied() else {
                            continue;
                        };
                        matrix
                    }
                    None => j3d_identity_matrix(),
                };
                let Some(asset) =
                    find_stage_asset_by_suffix(document, StageAssetKind::Model, &spec.asset_suffix)
                else {
                    continue;
                };
                let asset_path = asset.path.to_string_lossy().replace('\\', "/");
                let accessory_cache_key = normalized_preview_asset_path(&asset_path);
                if !accessory_model_cache.contains_key(&accessory_cache_key) {
                    if failed_model_assets.contains(&accessory_cache_key) {
                        continue;
                    }
                    let bytes = match document.read_asset_bytes(&asset.path) {
                        Ok(bytes) => bytes,
                        Err(error) => {
                            record_model_preview_failure(
                                &mut failed_model_assets,
                                &mut model_failures,
                                &asset_path,
                                format!("read accessory model: {error}"),
                            );
                            continue;
                        }
                    };
                    let file = match J3dFile::parse(&bytes) {
                        Ok(file) => file,
                        Err(error) => {
                            record_model_preview_failure(
                                &mut failed_model_assets,
                                &mut model_failures,
                                &asset_path,
                                format!("parse accessory J3D: {error}"),
                            );
                            continue;
                        }
                    };
                    let loader_flags = 0x1021_0000;
                    let joint_animation =
                        accessory_joint_animation(document, &asset_path).map(Arc::new);
                    let prepared_triangles = joint_animation
                        .as_ref()
                        .and_then(|_| file.prepare_animated_triangles().ok())
                        .map(Arc::new);
                    let preview_result = joint_animation.as_ref().map_or_else(
                        || file.geometry_preview_with_loader_flags(loader_flags),
                        |animation| {
                            file.geometry_preview_with_joint_animation(
                                loader_flags,
                                animation.as_ref(),
                                0.0,
                            )
                        },
                    );
                    let mut preview = match preview_result {
                        Ok(preview) => preview,
                        Err(error) => {
                            record_model_preview_failure(
                                &mut failed_model_assets,
                                &mut model_failures,
                                &asset_path,
                                format!("decode accessory geometry: {error}"),
                            );
                            continue;
                        }
                    };
                    // Parts are ordinary J3D models. Give them the same BMT
                    // resolution as bodies and map objects so dummy textures
                    // (including custom-model placeholders) resolve by asset
                    // names instead of accessory-specific exceptions.
                    apply_model_material_table(document, &asset_path, loader_flags, &mut preview);
                    let file = Arc::new(file);
                    let local_triangles = Arc::new(preview.triangles.clone());
                    let texture_base = push_preview_textures(&mut textures, &preview);
                    let material_base =
                        push_preview_materials(&mut materials, &preview, texture_base);
                    accessory_model_cache.insert(
                        accessory_cache_key.clone(),
                        CachedAccessoryModelPreview {
                            file,
                            joint_animation,
                            prepared_triangles,
                            loader_flags,
                            preview,
                            local_triangles,
                            texture_base,
                            material_base,
                        },
                    );
                }
                let Some(accessory) = accessory_model_cache.get(&accessory_cache_key) else {
                    continue;
                };
                source_vertices += accessory.preview.positions.len();
                source_triangles += accessory.preview.triangles.len();
                source_textures += accessory.preview.textures.len();
                let accessory_packet_base = next_packet_index;
                next_packet_index += accessory
                    .preview
                    .triangles
                    .iter()
                    .map(|triangle| triangle.packet_index)
                    .max()
                    .map(|index| index + 1)
                    .unwrap_or(1);
                let accessory_triangle_start = triangles.len();
                let accessory_material_base =
                    push_accessory_instance_materials(&mut materials, accessory, object, &spec);
                push_attached_preview_triangles(
                    &mut triangles,
                    accessory,
                    joint_matrix,
                    object.transform,
                    model_index,
                    accessory_packet_base,
                    accessory_material_base,
                    materials.len(),
                    textures.len(),
                );
                accessories.push(AnimatedAccessoryInstance {
                    joint_index,
                    file: accessory.file.clone(),
                    joint_animation: accessory.joint_animation.clone(),
                    prepared_triangles: accessory.prepared_triangles.clone(),
                    loader_flags: accessory.loader_flags,
                    local_triangles: accessory.local_triangles.clone(),
                    triangle_range: accessory_triangle_start..triangles.len(),
                });
            }
            if object.factory_name.starts_with("NPC") {
                push_npc_circle_shadow(
                    &mut triangles,
                    object.transform,
                    model_index,
                    next_packet_index,
                );
                next_packet_index += 1;
            }
            if is_coin_object(object) {
                if let Some(ground_y) = shadow_ground_height(
                    object_preview_transform.translation,
                    &triangles[..world_triangle_end],
                ) {
                    push_coin_circle_shadow(
                        &mut triangles,
                        object_preview_transform,
                        ground_y,
                        model_index,
                        next_packet_index,
                    );
                    next_packet_index += 1;
                }
            }
            let instance = AnimatedModelInstance {
                transform: object.transform,
                model_index,
                point_range: point_start..points.len(),
                point_stride,
                triangle_range: triangle_start..body_triangle_end,
                accessories,
                runtime_yaw_degrees_per_frame: runtime_yaw_degrees_per_frame(object),
            };
            if let Some(cached) = object_model_cache.get_mut(&model_cache_key) {
                cached.instances.push(instance);
            }
        }

        let rotating_models = object_model_cache
            .values()
            .filter_map(|cached| {
                let instances = cached
                    .instances
                    .iter()
                    .filter(|instance| instance.runtime_yaw_degrees_per_frame != 0.0)
                    .cloned()
                    .collect::<Vec<_>>();
                (!instances.is_empty()).then(|| RuntimeRotatingModelPreview {
                    positions: Arc::new(cached.preview.positions.clone()),
                    triangles: Arc::new(cached.preview.triangles.clone()),
                    instances,
                })
            })
            .collect::<Vec<_>>();
        let animated_models: Vec<AnimatedModelPreview> = object_model_cache
            .into_values()
            .filter_map(|cached| {
                cached
                    .joint_animation
                    .map(|animation| AnimatedModelPreview {
                        file: cached.file,
                        animation,
                        prepared_triangles: cached.prepared_triangles,
                        loader_flags: cached.loader_flags,
                        instances: cached.instances,
                    })
            })
            .collect();

        if visibility.effects {
            push_level_transform_particle_previews(
                document,
                &level_transform_models,
                &mut textures,
                &mut triangles,
                &mut next_packet_index,
                &mut level_transform_particles,
            );
        }
        let level_transform_duration_frames =
            level_transform_duration_frames(&level_transform_particles);
        let level_transform_particle_end_frames =
            level_transform_particle_end_frames(&level_transform_particles)
                .max(level_transform_duration_frames);

        if points.len() > POINT_BUDGET
            && animated_models.is_empty()
            && animated_flags.is_empty()
            && rotating_models.is_empty()
        {
            let stride = (points.len() / POINT_BUDGET).max(1);
            points = points
                .into_iter()
                .step_by(stride)
                .take(POINT_BUDGET)
                .collect();
        }

        material_animation_bindings.resize_with(materials.len(), Vec::new);

        if (loaded_models == 0 || (points.is_empty() && triangles.is_empty()))
            && collision_preview.triangles.is_empty()
        {
            if failed_model_assets.is_empty() {
                return None;
            }
            // Keep a failure-only preview so validation can report the exact
            // assets that failed instead of collapsing the entire result to
            // `None` and losing every diagnostic.
            bounds_min = [0.0; 3];
            bounds_max = [1.0; 3];
            camera_bounds_min = bounds_min;
            camera_bounds_max = bounds_max;
        }

        if !bounds_are_finite(bounds_min, bounds_max) {
            if let Some((robust_min, robust_max)) = robust_preview_bounds(&triangles, &points) {
                bounds_min = robust_min;
                bounds_max = robust_max;
            } else if let Some((collision_min, collision_max)) = robust_position_bounds(
                &collision_preview
                    .triangles
                    .iter()
                    .flat_map(|triangle| triangle.vertices)
                    .collect::<Vec<_>>(),
            ) {
                bounds_min = collision_min;
                bounds_max = collision_max;
            }
        }

        if !bounds_are_finite(camera_bounds_min, camera_bounds_max) {
            camera_bounds_min = bounds_min;
            camera_bounds_max = bounds_max;
        }
        if let Some((robust_min, robust_max)) = robust_position_bounds(&camera_bound_points) {
            camera_bounds_min = robust_min;
            camera_bounds_max = robust_max;
        }

        Some(ModelPreview {
            points,
            triangles,
            collision_triangles: collision_preview.triangles,
            collision_file_count: collision_preview.file_count,
            collision_surface_count: collision_preview.surface_types.len(),
            failed_collision_files: collision_preview.failed_assets.len(),
            collision_failures: collision_preview.failures,
            textures,
            materials,
            texture_srt_animations,
            texture_pattern_animations,
            material_animation_bindings,
            pollution_texture_indices,
            bounds_min,
            bounds_max,
            camera_bounds_min,
            camera_bounds_max,
            loaded_models,
            failed_models: failed_model_assets.len(),
            model_failures,
            source_vertices,
            source_triangles,
            source_textures,
            goop_surface_model_indices,
            object_model_indices,
            mirror_actor_positions,
            mirror_cubes,
            mirror_model_slots,
            animated_models,
            animated_flags,
            rotating_models,
            level_transform_models,
            level_transform_particles,
            actor_particles,
            level_transform_duration_frames,
            level_transform_particle_end_frames,
        })
    }

    fn preview_visibility(&self) -> PreviewVisibility {
        PreviewVisibility {
            environment: self.show_environment_meshes,
            goop: self.show_goop_meshes,
            effects: self.show_effects,
        }
    }

    // Document lifecycle and edit commands live in document_commands.rs.

    // Camera projection and selection queries live in camera.rs.
}

fn install_style(ctx: &egui::Context) {
    install_japanese_font(ctx);

    let mut visuals = egui::Visuals::dark();
    visuals.panel_fill = egui::Color32::from_rgb(28, 30, 31);
    visuals.window_fill = egui::Color32::from_rgb(32, 34, 35);
    visuals.faint_bg_color = egui::Color32::from_rgb(38, 41, 42);
    visuals.extreme_bg_color = egui::Color32::from_rgb(18, 20, 21);
    visuals.selection.bg_fill = egui::Color32::from_rgb(54, 124, 116);
    visuals.hyperlink_color = egui::Color32::from_rgb(104, 186, 214);
    ctx.set_visuals(visuals);

    let mut style = (*ctx.style_of(egui::Theme::Dark)).clone();
    style.spacing.item_spacing = egui::vec2(8.0, 7.0);
    style.spacing.button_padding = egui::vec2(10.0, 5.0);
    style.spacing.indent = 16.0;
    ctx.set_style_of(egui::Theme::Dark, style);
}

fn install_japanese_font(ctx: &egui::Context) {
    let Some(font_bytes) = japanese_font_candidates()
        .into_iter()
        .find_map(|path| std::fs::read(path).ok())
    else {
        return;
    };

    const FONT_NAME: &str = "sms-japanese-fallback";
    let mut fonts = egui::FontDefinitions::default();
    fonts.font_data.insert(
        FONT_NAME.to_owned(),
        std::sync::Arc::new(egui::FontData::from_owned(font_bytes)),
    );
    for family in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
        if let Some(family_fonts) = fonts.families.get_mut(&family) {
            family_fonts.push(FONT_NAME.to_owned());
        }
    }
    ctx.set_fonts(fonts);
}

fn japanese_font_candidates() -> Vec<PathBuf> {
    #[cfg(target_os = "windows")]
    {
        let fonts = std::env::var_os("WINDIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(r"C:\Windows"))
            .join("Fonts");
        ["YuGothR.ttc", "meiryo.ttc", "msgothic.ttc"]
            .into_iter()
            .map(|name| fonts.join(name))
            .collect()
    }

    #[cfg(target_os = "macos")]
    {
        [
            "/System/Library/Fonts/ヒラギノ角ゴシック W3.ttc",
            "/System/Library/Fonts/ヒラギノ丸ゴ ProN W4.ttc",
        ]
        .into_iter()
        .map(PathBuf::from)
        .collect()
    }

    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    {
        [
            "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc",
            "/usr/share/fonts/truetype/noto/NotoSansCJK-Regular.ttc",
            "/usr/share/fonts/opentype/noto/NotoSansJP-Regular.otf",
        ]
        .into_iter()
        .map(PathBuf::from)
        .collect()
    }
}

fn load_project_for_stage(
    document: &mut StageDocument,
    project_root: &str,
) -> Result<ProjectLoadSelection, SceneError> {
    if project_root.is_empty() {
        return Ok(ProjectLoadSelection {
            project_root: String::new(),
            warning: None,
        });
    }
    let requested = PathBuf::from(project_root);
    if document.project_root_overlaps_base(&requested)? {
        let selected = load_first_compatible_project(
            document,
            external_project_roots_for_base(&document.base_root),
        )?;
        return Ok(ProjectLoadSelection {
            warning: Some(format!(
                "Project Folder automatically changed from '{}' to '{}' because editor projects must be outside the extracted base game directory.",
                requested.display(),
                selected.display()
            )),
            project_root: selected.to_string_lossy().into_owned(),
        });
    }
    match document.load_project_folder(&requested) {
        Ok(_) => Ok(ProjectLoadSelection {
            project_root: project_root.to_string(),
            warning: None,
        }),
        Err(SceneError::ProjectBaseMismatch {
            path,
            manifest_base,
            open_base,
        }) => {
            let selected = load_first_compatible_project(
                document,
                regional_project_roots(&requested, &document.base_root, &manifest_base),
            )?;
            Ok(ProjectLoadSelection {
                warning: Some(format!(
                    "Project Folder automatically switched from '{}' to '{}' because '{}' belongs to base root '{}', not '{}'.",
                    requested.display(),
                    selected.display(),
                    path.display(),
                    manifest_base.display(),
                    open_base.display()
                )),
                project_root: selected.to_string_lossy().into_owned(),
            })
        }
        Err(error) => Err(error),
    }
}

fn document_uses_selected_base(document: &StageDocument, selected_base_root: &str) -> bool {
    let selected = PathBuf::from(selected_base_root);
    let document_base = std::fs::canonicalize(&document.base_root);
    let selected_base = std::fs::canonicalize(selected);
    match (document_base, selected_base) {
        (Ok(document_base), Ok(selected_base)) => {
            #[cfg(windows)]
            {
                document_base
                    .to_string_lossy()
                    .eq_ignore_ascii_case(&selected_base.to_string_lossy())
            }
            #[cfg(not(windows))]
            {
                document_base == selected_base
            }
        }
        _ => false,
    }
}

fn load_first_compatible_project(
    document: &mut StageDocument,
    candidates: Vec<PathBuf>,
) -> Result<PathBuf, SceneError> {
    let mut last_mismatch = None;
    for candidate in candidates {
        match document.load_project_folder(&candidate) {
            Ok(_) => return Ok(candidate),
            Err(error @ SceneError::ProjectBaseMismatch { .. }) => last_mismatch = Some(error),
            Err(error) => return Err(error),
        }
    }
    Err(last_mismatch.unwrap_or_else(|| SceneError::InvalidProjectRoot(PathBuf::new())))
}

fn regional_project_roots(
    requested: &std::path::Path,
    base_root: &std::path::Path,
    manifest_base: &std::path::Path,
) -> Vec<PathBuf> {
    const DEFAULT_PROJECT_NAME: &str = "graffito-editor-project";
    let parent = requested
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| std::path::Path::new("."));
    let requested_name = requested
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(DEFAULT_PROJECT_NAME);
    let manifest_suffix = format!("-{}", project_base_slug(manifest_base));
    let family_name = requested_name
        .strip_suffix(&manifest_suffix)
        .unwrap_or(requested_name);

    let mut candidates = Vec::new();
    let legacy_root = parent.join(family_name);
    if requested != legacy_root && legacy_root.join("sms-project.toml").is_file() {
        candidates.push(legacy_root);
    }
    let slug = project_base_slug(base_root);
    let regional_root = parent.join(format!("{family_name}-{slug}"));
    if regional_root != requested && !candidates.contains(&regional_root) {
        candidates.push(regional_root.clone());
    }
    candidates.push(parent.join(format!(
        "{family_name}-{slug}-{:08x}",
        project_base_hash(base_root) as u32
    )));
    candidates
}

fn external_project_roots_for_base(base_root: &std::path::Path) -> Vec<PathBuf> {
    let parent = base_root
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| std::path::Path::new("."));
    let slug = project_base_slug(base_root);
    vec![
        parent.join(format!("{slug}-graffito-editor-project")),
        parent.join(format!(
            "{slug}-graffito-editor-project-{:08x}",
            project_base_hash(base_root) as u32
        )),
    ]
}

fn project_base_slug(base_root: &std::path::Path) -> String {
    let source = base_root
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .unwrap_or("base");
    let mut slug = String::new();
    for character in source.chars() {
        if character.is_ascii_alphanumeric() || matches!(character, '-' | '_') {
            slug.push(character);
        } else if !slug.ends_with('-') {
            slug.push('-');
        }
    }
    let slug = slug.trim_matches('-');
    if slug.is_empty() {
        "base".to_string()
    } else {
        slug.to_string()
    }
}

fn project_base_hash(base_root: &std::path::Path) -> u64 {
    const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;
    base_root
        .to_string_lossy()
        .replace('/', "\\")
        .to_ascii_lowercase()
        .bytes()
        .fold(FNV_OFFSET, |hash, byte| {
            (hash ^ u64::from(byte)).wrapping_mul(FNV_PRIME)
        })
}

mod preview_assets;
use preview_assets::*;

mod preview_particles;
use preview_particles::*;

mod preview_grass;
use preview_grass::*;

mod preview_flags;
use preview_flags::*;

mod preview_wires;
use preview_wires::*;

mod preview_waves;
use preview_waves::*;

mod npc_accessories;
use npc_accessories::*;

#[allow(clippy::too_many_arguments)]
fn push_attached_preview_triangles(
    triangles: &mut Vec<PreviewTriangle>,
    accessory: &CachedAccessoryModelPreview,
    joint_matrix: J3dMatrix34,
    transform: Transform,
    model_index: usize,
    packet_base: usize,
    material_base: usize,
    material_count: usize,
    texture_count: usize,
) {
    for triangle in &accessory.preview.triangles {
        let vertices = transform_preview_vertices(
            triangle
                .vertices
                .map(|vertex| transform_j3d_matrix_point(joint_matrix, vertex)),
            transform,
        );
        if !triangle_vertices_are_finite(vertices) {
            continue;
        }
        triangles.push(PreviewTriangle {
            vertices,
            normals: triangle.normals.map(|normals| {
                transform_preview_normals(
                    normals.map(|normal| transform_j3d_matrix_normal(joint_matrix, normal)),
                    transform,
                )
            }),
            color_channels: triangle.color_channels,
            tex_coord_sets: triangle.tex_coord_sets,
            material_index: triangle.material_index.and_then(|index| {
                let global_index = material_base + index;
                (global_index < material_count).then_some(global_index)
            }),
            packet_index: packet_base + triangle.packet_index,
            model_index,
            render_layer: PreviewRenderLayer::Main,
            color: triangle.color,
            vertex_colors: triangle.vertex_colors,
            combine_mode: triangle.combine_mode,
            tex_coords: triangle.tex_coords,
            texture_index: triangle.texture_index.and_then(|index| {
                let global_index = accessory.texture_base + index;
                (global_index < texture_count).then_some(global_index)
            }),
            mask_tex_coords: triangle.mask_tex_coords,
            mask_texture_index: triangle.mask_texture_index.and_then(|index| {
                let global_index = accessory.texture_base + index;
                (global_index < texture_count).then_some(global_index)
            }),
            cull_mode: triangle.cull_mode,
            alpha_compare: triangle.alpha_compare,
            blend_mode: triangle.blend_mode,
            z_mode: triangle.z_mode,
            billboard: triangle.billboard.and_then(|billboard| {
                let joint_normals = triangle.normals.map(|normals| {
                    normals.map(|normal| transform_j3d_matrix_normal(joint_matrix, normal))
                });
                let billboard =
                    transform_j3d_billboard_matrix(billboard, joint_matrix, joint_normals)?;
                transform_j3d_billboard(
                    billboard,
                    transform,
                    joint_normals.map(|normals| transform_preview_normals(normals, transform)),
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

fn j3d_identity_matrix() -> J3dMatrix34 {
    [
        [1.0, 0.0, 0.0, 0.0],
        [0.0, 1.0, 0.0, 0.0],
        [0.0, 0.0, 1.0, 0.0],
    ]
}

fn push_npc_circle_shadow(
    triangles: &mut Vec<PreviewTriangle>,
    transform: Transform,
    model_index: usize,
    packet_index: usize,
) {
    let radius = 60.0 * transform.scale[0].abs().max(transform.scale[2].abs());
    let center = [
        transform.translation[0],
        transform.translation[1] + 1.5,
        transform.translation[2],
    ];
    push_circle_shadow(triangles, center, radius, model_index, packet_index);
}

fn push_coin_circle_shadow(
    triangles: &mut Vec<PreviewTriangle>,
    transform: Transform,
    ground_y: f32,
    model_index: usize,
    packet_index: usize,
) {
    // TMapObjBase::initActorData derives mScaledBodyRadius from the coin's
    // TMapObjData value (50.0) and its X scale before TLiveActor submits the
    // type-0 circle-shadow request.
    let radius = 50.0 * transform.scale[0].abs();
    let center = [
        transform.translation[0],
        ground_y + 1.5,
        transform.translation[2],
    ];
    push_circle_shadow(triangles, center, radius, model_index, packet_index);
}

fn push_circle_shadow(
    triangles: &mut Vec<PreviewTriangle>,
    center: [f32; 3],
    radius: f32,
    model_index: usize,
    packet_index: usize,
) {
    const SEGMENTS: usize = 20;
    let color = [0, 0, 0, 82];
    for segment in 0..SEGMENTS {
        let angle0 = segment as f32 * std::f32::consts::TAU / SEGMENTS as f32;
        let angle1 = (segment + 1) as f32 * std::f32::consts::TAU / SEGMENTS as f32;
        triangles.push(PreviewTriangle {
            vertices: [
                center,
                [
                    center[0] + angle0.cos() * radius,
                    center[1],
                    center[2] + angle0.sin() * radius,
                ],
                [
                    center[0] + angle1.cos() * radius,
                    center[1],
                    center[2] + angle1.sin() * radius,
                ],
            ],
            normals: Some([[0.0, 1.0, 0.0]; 3]),
            color_channels: [Some([color; 3]), None],
            tex_coord_sets: [None; 8],
            material_index: None,
            packet_index,
            model_index,
            render_layer: PreviewRenderLayer::Shadow,
            color: Some(color),
            vertex_colors: Some([color; 3]),
            combine_mode: J3dPreviewCombineMode::VertexOnly,
            tex_coords: None,
            texture_index: None,
            mask_tex_coords: None,
            mask_texture_index: None,
            cull_mode: Some(0),
            alpha_compare: None,
            blend_mode: Some(J3dBlendMode {
                mode: 1,
                src_factor: 4,
                dst_factor: 5,
                logic_op: 3,
            }),
            z_mode: Some(J3dZMode {
                compare_enable: 1,
                func: 3,
                update_enable: 0,
            }),
            billboard: None,
            particle_type: None,
            particle_pivot: None,
            particle_direction: None,
            particle_color_mode: None,
            particle_environment_color: None,
        });
    }
}

fn shadow_ground_height(position: [f32; 3], world_triangles: &[PreviewTriangle]) -> Option<f32> {
    world_triangles
        .iter()
        .filter(|triangle| triangle.render_layer == PreviewRenderLayer::Main)
        .filter_map(|triangle| {
            vertical_triangle_height(position[0], position[2], triangle.vertices)
        })
        // TMBindShadowManager starts the retail ground query 30 units above
        // the actor and searches downward, so slightly embedded placements
        // still resolve to the surface above them.
        .filter(|height| *height <= position[1] + 30.0)
        .max_by(f32::total_cmp)
}

fn vertical_triangle_height(x: f32, z: f32, vertices: [[f32; 3]; 3]) -> Option<f32> {
    let [a, b, c] = vertices;
    let denominator = (b[2] - c[2]) * (a[0] - c[0]) + (c[0] - b[0]) * (a[2] - c[2]);
    if denominator.abs() <= f32::EPSILON {
        return None;
    }

    let a_weight = ((b[2] - c[2]) * (x - c[0]) + (c[0] - b[0]) * (z - c[2])) / denominator;
    let b_weight = ((c[2] - a[2]) * (x - c[0]) + (a[0] - c[0]) * (z - c[2])) / denominator;
    let c_weight = 1.0 - a_weight - b_weight;
    const EDGE_EPSILON: f32 = 0.0001;
    (a_weight >= -EDGE_EPSILON && b_weight >= -EDGE_EPSILON && c_weight >= -EDGE_EPSILON)
        .then_some(a_weight * a[1] + b_weight * b[1] + c_weight * c[1])
}

fn material_table_candidates_for_model(model_path: &str) -> Vec<String> {
    let normalized = model_path.replace('\\', "/").to_ascii_lowercase();
    let Some(base) = normalized.rsplit_once('.').map(|(base, _)| base) else {
        return Vec::new();
    };
    let mut candidates = vec![format!("{base}.bmt")];
    let (directory, stem) = base
        .rsplit_once('/')
        .map(|(directory, stem)| (format!("{directory}/"), stem))
        .unwrap_or_else(|| (String::new(), base));
    for suffix in [
        "_crash", "crash", "_normal", "normal", "_offset", "offset", "_alpha", "alpha",
    ] {
        if let Some(base_stem) = stem.strip_suffix(suffix) {
            if !base_stem.is_empty() {
                candidates.push(format!("{directory}{base_stem}.bmt"));
            }
            break;
        }
    }
    let model_key = material_table_match_key(stem);
    let shared_table = if model_key.starts_with("miniwindmill")
        || model_key.starts_with("lampbianco")
        || model_key.starts_with("biabell")
        || model_key.starts_with("biawatermill")
        || model_key.starts_with("biadoor")
    {
        Some("bianco")
    } else if model_key.starts_with("riccoship")
        || model_key.starts_with("riccoyacht")
        || model_key.starts_with("riccoboat")
    {
        Some("riccoship")
    } else if model_key.starts_with("sandleafbase")
        || model_key.starts_with("sandbombbase")
        || model_key.starts_with("sandcastle")
    {
        Some("sandbombbase")
    } else {
        None
    };
    if let Some(shared_table) = shared_table {
        let candidate = format!("{directory}{shared_table}.bmt");
        if !candidates.contains(&candidate) {
            candidates.push(candidate);
        }
    }

    candidates
}

fn material_table_match_key(value: &str) -> String {
    value
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

fn material_table_asset_score(
    model_path: &str,
    model_textures: &[sms_formats::J3dTexturePreview],
    table_path: &str,
) -> Option<(u8, usize)> {
    let model_path = normalized_preview_asset_path(model_path);
    let table_path = normalized_preview_asset_path(table_path);
    let candidates = material_table_candidates_for_model(&model_path);
    if let Some(index) = candidates
        .iter()
        .position(|candidate| candidate == &table_path)
    {
        return Some((3, candidates.len().saturating_sub(index)));
    }

    let (model_directory, model_file) = model_path.rsplit_once('/')?;
    let (table_directory, table_file) = table_path.rsplit_once('/')?;
    let same_archive = model_path
        .split_once("!/")
        .zip(table_path.split_once("!/"))
        .is_some_and(|((model_archive, _), (table_archive, _))| model_archive == table_archive);
    if model_directory != table_directory && !same_archive {
        return None;
    }

    let model_stem = model_file
        .rsplit_once('.')
        .map_or(model_file, |(stem, _)| stem);
    let table_stem = table_file
        .rsplit_once('.')
        .map_or(table_file, |(stem, _)| stem);
    let model_key = material_table_match_key(model_stem);
    let table_key = material_table_match_key(table_stem);
    if table_key.is_empty() {
        return None;
    }

    let dummy_texture_match = model_textures.iter().any(|texture| {
        let name = texture.name.to_ascii_lowercase();
        (name.contains("dummy") || name.contains("dammy"))
            && material_table_match_key(&name).contains(&table_key)
    });
    let dummy_model_match = model_directory == table_directory
        && model_textures.iter().any(|texture| {
            let name = texture.name.to_ascii_lowercase();
            name.contains("dummy") || name.contains("dammy")
        })
        && model_key.contains(&table_key);

    (dummy_texture_match || dummy_model_match).then_some((2, table_key.len()))
}

mod npc_materials;
use npc_materials::*;

mod preview_geometry;
use preview_geometry::*;

fn normalized_preview_asset_path(path: &str) -> String {
    path.replace('\\', "/").to_ascii_lowercase()
}

fn is_supported_object_preview_model_path(path: &str) -> bool {
    let path = path.to_ascii_lowercase();
    (path.ends_with(".bmd") || path.ends_with(".bdl"))
        && (path.contains("!/") || path.contains("/scene/"))
}

fn should_instance_object_preview_model(path: &str, world_model_paths: &BTreeSet<String>) -> bool {
    is_supported_object_preview_model_path(path)
        && !world_model_paths.contains(&normalized_preview_asset_path(path))
}

fn object_preview_model_path(
    object: &SceneObject,
    world_model_paths: &BTreeSet<String>,
) -> Option<String> {
    object_preview_model_path_for_role(object, world_model_paths, AssetRole::PreviewModel)
}

fn object_inferred_preview_model_path(
    object: &SceneObject,
    world_model_paths: &BTreeSet<String>,
) -> Option<String> {
    object_preview_model_path_for_role(object, world_model_paths, AssetRole::InferredPreviewModel)
}

fn object_preview_model_path_for_role(
    object: &SceneObject,
    world_model_paths: &BTreeSet<String>,
    role: AssetRole,
) -> Option<String> {
    object
        .asset_hints
        .iter()
        .find(|hint| hint.role == role)
        .map(|hint| hint.path.as_str())
        .filter(|path| should_instance_object_preview_model(path, world_model_paths))
        .map(str::to_owned)
}

fn object_matches_focus(object: &SceneObject, needle: &str) -> bool {
    object.id.to_ascii_lowercase().contains(needle)
        || object.factory_name.to_ascii_lowercase().contains(needle)
        || object
            .class_name
            .as_deref()
            .unwrap_or_default()
            .to_ascii_lowercase()
            .contains(needle)
        || object.raw_params.values().any(|value| {
            value
                .trim_matches('"')
                .to_ascii_lowercase()
                .contains(needle)
        })
}

fn object_display_name(object: &SceneObject) -> String {
    bilingual_object_name(object)
}

fn labeled_text(ui: &mut egui::Ui, label: &str, value: &mut String) {
    ui.horizontal(|ui| {
        ui.label(label);
        ui.text_edit_singleline(value);
    });
}

mod startup;
use startup::*;
fn format_bytes_short(bytes: u64) -> String {
    if bytes >= 1_000_000 {
        format!("{:.1} MB", bytes as f64 / 1_000_000.0)
    } else if bytes >= 1_000 {
        format!("{:.0} KB", bytes as f64 / 1_000.0)
    } else {
        format!("{bytes} B")
    }
}

fn content_browser_card_text(
    archive: &SceneArchiveInfo,
    localized: Option<&SceneArchiveLabel>,
) -> String {
    let mut lines = vec![archive.stage_id.clone()];
    if let Some(stage_name) = localized.and_then(|label| label.stage_name.as_deref()) {
        lines.push(stage_name.to_string());
    }
    if let Some(label) = localized {
        if let Some(first) = label.scenario_names.first() {
            let remaining = label.scenario_names.len().saturating_sub(1);
            let suffix = if remaining == 0 {
                String::new()
            } else {
                format!(" (+{remaining})")
            };
            lines.push(format!("{first}{suffix}"));
        }
    }
    lines.push(format_bytes_short(archive.size_bytes));
    lines.join("\n")
}

fn content_browser_card_button(
    ui: &mut egui::Ui,
    size: egui::Vec2,
    selected: bool,
    label: &str,
) -> egui::Response {
    let response = ui.add_sized(size, egui::Button::selectable(selected, ""));
    response.widget_info(|| {
        egui::WidgetInfo::selected(egui::WidgetType::Button, ui.is_enabled(), selected, label)
    });
    if !ui.is_rect_visible(response.rect) {
        return response;
    }

    let lines = label.lines().collect::<Vec<_>>();
    let Some(title) = lines.first().copied() else {
        return response;
    };
    let size_text = lines.last().copied().unwrap_or_default();
    let details = if lines.len() > 2 {
        &lines[1..lines.len() - 1]
    } else {
        &[]
    };

    let inner = response.rect.shrink2(egui::vec2(8.0, 6.0));
    let text_color = ui.style().interact(&response).text_color();
    let size_galley = egui::WidgetText::from(egui::RichText::new(size_text).small().weak())
        .into_galley(
            ui,
            Some(egui::TextWrapMode::Truncate),
            inner.width() * 0.35,
            egui::TextStyle::Small,
        );
    let title_width = (inner.width() - size_galley.size().x - 8.0).max(24.0);
    let title_galley = egui::WidgetText::from(egui::RichText::new(title).strong()).into_galley(
        ui,
        Some(egui::TextWrapMode::Truncate),
        title_width,
        egui::TextStyle::Button,
    );
    let detail_galleys = details
        .iter()
        .enumerate()
        .map(|(index, line)| {
            let text = if index == 0 {
                egui::RichText::new(*line).strong()
            } else {
                egui::RichText::new(*line).small()
            };
            egui::WidgetText::from(text).into_galley(
                ui,
                Some(egui::TextWrapMode::Truncate),
                inner.width(),
                egui::TextStyle::Body,
            )
        })
        .collect::<Vec<_>>();

    let top_height = title_galley.size().y.max(size_galley.size().y);
    let details_height = detail_galleys
        .iter()
        .map(|galley| galley.size().y)
        .sum::<f32>();
    let row_gap = 3.0;
    let total_height = top_height
        + if detail_galleys.is_empty() {
            0.0
        } else {
            5.0 + details_height + row_gap * detail_galleys.len().saturating_sub(1) as f32
        };
    let painter = ui.painter().with_clip_rect(inner);
    let mut y = inner.center().y - total_height * 0.5;
    painter.galley(
        egui::pos2(inner.left(), y + (top_height - title_galley.size().y) * 0.5),
        title_galley,
        text_color,
    );
    painter.galley(
        egui::pos2(
            inner.right() - size_galley.size().x,
            y + (top_height - size_galley.size().y) * 0.5,
        ),
        size_galley,
        text_color,
    );

    if !detail_galleys.is_empty() {
        y += top_height + 5.0;
        for galley in detail_galleys {
            let height = galley.size().y;
            painter.galley(egui::pos2(inner.left(), y), galley, text_color);
            y += height + row_gap;
        }
    }

    response
}

fn content_browser_hover_text(
    archive: &SceneArchiveInfo,
    localized: Option<&SceneArchiveLabel>,
) -> String {
    let mut lines = Vec::new();
    if let Some(stage_name) = localized.and_then(|label| label.stage_name.as_deref()) {
        lines.push(format!("Stage: {stage_name}"));
    }
    if let Some(label) = localized {
        lines.extend(label.scenario_names.iter().map(|name| format!("• {name}")));
    }
    if !lines.is_empty() {
        lines.push(String::new());
    }
    lines.push(archive.relative_path.display().to_string());
    lines.push(archive.path.display().to_string());
    lines.join("\n")
}

fn merge_bounds(
    bounds_min: &mut [f32; 3],
    bounds_max: &mut [f32; 3],
    next_min: [f32; 3],
    next_max: [f32; 3],
) {
    for axis in 0..3 {
        bounds_min[axis] = bounds_min[axis].min(next_min[axis]);
        bounds_max[axis] = bounds_max[axis].max(next_max[axis]);
    }
}

fn preview_triangle_world_vertices(
    vertices: [[f32; 3]; 3],
    render_layer: PreviewRenderLayer,
    camera_position: [f32; 3],
) -> [[f32; 3]; 3] {
    if render_layer == PreviewRenderLayer::Sky {
        vertices.map(|vertex| vec3_add(vertex, camera_position))
    } else {
        vertices
    }
}

fn clip_world_segment_to_near_plane(
    camera: CameraFrame,
    start: [f32; 3],
    end: [f32; 3],
    near: f32,
) -> Option<[[f32; 3]; 2]> {
    let mut clipped = [start, end];
    let mut depths = [
        vec3_dot(vec3_sub(start, camera.position), camera.forward),
        vec3_dot(vec3_sub(end, camera.position), camera.forward),
    ];
    if !near.is_finite() || depths.iter().any(|depth| !depth.is_finite()) {
        return None;
    }
    if depths[0] < near && depths[1] < near {
        return None;
    }

    for endpoint in 0..2 {
        if depths[endpoint] >= near {
            continue;
        }
        let other = 1 - endpoint;
        let depth_span = depths[other] - depths[endpoint];
        if depth_span.abs() <= f32::EPSILON {
            return None;
        }
        let t = ((near - depths[endpoint]) / depth_span).clamp(0.0, 1.0);
        clipped[endpoint] = vec3_add(
            clipped[endpoint],
            vec3_scale(vec3_sub(clipped[other], clipped[endpoint]), t),
        );
        depths[endpoint] = near;
    }

    Some(clipped)
}

fn recompute_model_preview_bounds(preview: &mut ModelPreview) {
    if let Some((bounds_min, bounds_max)) =
        robust_preview_bounds(&preview.triangles, &preview.points)
    {
        preview.bounds_min = bounds_min;
        preview.bounds_max = bounds_max;
        if !bounds_are_finite(preview.camera_bounds_min, preview.camera_bounds_max) {
            preview.camera_bounds_min = bounds_min;
            preview.camera_bounds_max = bounds_max;
        }
    }
}

fn expand_model_preview_bounds(
    preview: &mut ModelPreview,
    model_index: usize,
    triangle_ranges: &[std::ops::Range<usize>],
) {
    let mut bounds_min = [f32::INFINITY; 3];
    let mut bounds_max = [f32::NEG_INFINITY; 3];
    let mut found = false;
    for range in triangle_ranges {
        for triangle in preview.triangles[range.clone()].iter().filter(|triangle| {
            !matches!(
                triangle.render_layer,
                PreviewRenderLayer::Sky
                    | PreviewRenderLayer::MirrorScene
                    | PreviewRenderLayer::Heatwave
            )
        }) {
            for vertex in triangle.vertices {
                if !vertex.iter().all(|value| value.is_finite()) {
                    continue;
                }
                found = true;
                for axis in 0..3 {
                    bounds_min[axis] = bounds_min[axis].min(vertex[axis]);
                    bounds_max[axis] = bounds_max[axis].max(vertex[axis]);
                }
            }
        }
    }
    if !found {
        for point in preview
            .points
            .iter()
            .filter(|point| point.model_index == model_index)
        {
            if !point.position.iter().all(|value| value.is_finite()) {
                continue;
            }
            found = true;
            for axis in 0..3 {
                bounds_min[axis] = bounds_min[axis].min(point.position[axis]);
                bounds_max[axis] = bounds_max[axis].max(point.position[axis]);
            }
        }
    }
    if !found {
        return;
    }

    for axis in 0..3 {
        let padding = ((bounds_max[axis] - bounds_min[axis]) * 0.06).max(120.0);
        preview.bounds_min[axis] = preview.bounds_min[axis].min(bounds_min[axis] - padding);
        preview.bounds_max[axis] = preview.bounds_max[axis].max(bounds_max[axis] + padding);
    }
}

fn bounds_are_finite(bounds_min: [f32; 3], bounds_max: [f32; 3]) -> bool {
    bounds_min
        .iter()
        .chain(bounds_max.iter())
        .all(|value| value.is_finite())
        && (0..3).all(|axis| bounds_max[axis] > bounds_min[axis])
}

fn triangle_vertices_are_finite(vertices: [[f32; 3]; 3]) -> bool {
    vertices
        .iter()
        .flatten()
        .all(|value| value.is_finite() && value.abs() < 2_000_000.0)
}

fn robust_preview_bounds(
    triangles: &[PreviewTriangle],
    points: &[PreviewPoint],
) -> Option<([f32; 3], [f32; 3])> {
    let mut axes = [Vec::new(), Vec::new(), Vec::new()];
    let has_world_triangles = triangles.iter().any(|triangle| {
        !matches!(
            triangle.render_layer,
            PreviewRenderLayer::Sky
                | PreviewRenderLayer::MirrorScene
                | PreviewRenderLayer::Heatwave
        )
    });
    if !has_world_triangles {
        for point in points {
            for (axis, value) in axes.iter_mut().zip(point.position) {
                if value.is_finite() {
                    axis.push(value);
                }
            }
        }
    } else {
        for triangle in triangles.iter().filter(|triangle| {
            !matches!(
                triangle.render_layer,
                PreviewRenderLayer::Sky
                    | PreviewRenderLayer::MirrorScene
                    | PreviewRenderLayer::Heatwave
            )
        }) {
            for vertex in triangle.vertices {
                for (axis, value) in axes.iter_mut().zip(vertex) {
                    if value.is_finite() {
                        axis.push(value);
                    }
                }
            }
        }
    }

    robust_axes_bounds(axes)
}

fn robust_position_bounds(points: &[[f32; 3]]) -> Option<([f32; 3], [f32; 3])> {
    let mut axes = [Vec::new(), Vec::new(), Vec::new()];
    for point in points {
        for axis in 0..3 {
            if point[axis].is_finite() {
                axes[axis].push(point[axis]);
            }
        }
    }

    robust_axes_bounds(axes)
}

fn robust_axes_bounds(mut axes: [Vec<f32>; 3]) -> Option<([f32; 3], [f32; 3])> {
    if axes[0].len() < 16 || axes.iter().any(Vec::is_empty) {
        return None;
    }

    let mut min = [0.0; 3];
    let mut max = [0.0; 3];
    for axis in 0..3 {
        axes[axis].sort_by(|a, b| a.total_cmp(b));
        let trim = (axes[axis].len() / 20).min(axes[axis].len().saturating_sub(1));
        let high = axes[axis].len().saturating_sub(1 + trim);
        min[axis] = axes[axis][trim];
        max[axis] = axes[axis][high];
        if !min[axis].is_finite() || !max[axis].is_finite() || max[axis] <= min[axis] {
            return None;
        }
    }

    for axis in 0..3 {
        let pad = ((max[axis] - min[axis]) * 0.06).max(120.0);
        min[axis] -= pad;
        max[axis] += pad;
    }

    Some((min, max))
}

fn perspective_focal_length(rect: egui::Rect, viewport_zoom: f32) -> f32 {
    const VERTICAL_FOV_DEGREES: f32 = 50.0;
    let fov = VERTICAL_FOV_DEGREES.to_radians();
    rect.height().min(rect.width()) * 0.5 / (fov * 0.5).tan() * viewport_zoom.max(0.05)
}

fn vec3_add(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [a[0] + b[0], a[1] + b[1], a[2] + b[2]]
}

fn vec3_sub(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

fn vec3_scale(value: [f32; 3], scale: f32) -> [f32; 3] {
    [value[0] * scale, value[1] * scale, value[2] * scale]
}

fn vec3_dot(a: [f32; 3], b: [f32; 3]) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

fn vec3_cross(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

fn vec3_normalize(value: [f32; 3]) -> [f32; 3] {
    let length = vec3_dot(value, value).sqrt();
    if length <= 0.0001 || !length.is_finite() {
        [0.0, 0.0, 0.0]
    } else {
        vec3_scale(value, 1.0 / length)
    }
}

mod software_renderer;
use software_renderer::*;

fn is_default_preview_model_path(
    path: &str,
    show_environment_meshes: bool,
    show_goop_meshes: bool,
    show_effect_meshes: bool,
) -> bool {
    let path = path.to_ascii_lowercase();
    if !(path.contains("!/map/") || path.contains("/scene/map/")) {
        return false;
    }
    if path_is_mirror_sky_helper_model_path(&path) {
        return false;
    }
    if path_is_sky_model_path(&path) {
        return true;
    }
    if path_is_sea_indirect_model_path(&path) {
        return show_environment_meshes;
    }
    if path_is_indirect_water_model_path(&path) {
        return false;
    }
    if path_is_mirror_surface_model_path(&path) || path_is_water_reflection_model_path(&path) {
        return show_environment_meshes;
    }
    if path_is_water_model_path(&path) {
        return show_environment_meshes;
    }
    if path_is_goop_model_path(&path) {
        return show_goop_meshes;
    }
    if path.contains("/map/mirror/") {
        return show_effect_meshes;
    }
    if path.contains("/map/map/reflect") || path.contains("/map/reflect") {
        return show_effect_meshes;
    }
    true
}

fn is_camera_bounds_model_path(path: &str) -> bool {
    let path = path.to_ascii_lowercase();
    if !(path.contains("!/map/") || path.contains("/scene/map/")) {
        return false;
    }
    !(path_is_sky_model_path(&path)
        || path_is_water_model_path(&path)
        || path_is_indirect_water_model_path(&path)
        || path_is_goop_model_path(&path)
        || path.contains("/map/mirror/")
        || path.contains("/map/map/reflect")
        || path.contains("/map/reflect"))
}

fn preview_render_layer_for_model_path(path: &str) -> PreviewRenderLayer {
    let path = path.to_ascii_lowercase();
    if path_is_shimmer_model_path(&path) {
        PreviewRenderLayer::Heatwave
    } else if path_is_mirror_sky_helper_model_path(&path) {
        PreviewRenderLayer::MirrorScene
    } else if path_is_sea_indirect_model_path(&path) {
        PreviewRenderLayer::IndirectWater
    } else if path_is_water_reflection_model_path(&path) {
        PreviewRenderLayer::MirrorScene
    } else if path_is_mirror_surface_model_path(&path) {
        PreviewRenderLayer::MirrorSurface
    } else if path_is_sky_model_path(&path) {
        PreviewRenderLayer::Sky
    } else if path_is_goop_model_path(&path) {
        PreviewRenderLayer::Goop
    } else if path_is_water_model_path(&path) {
        PreviewRenderLayer::Water
    } else {
        PreviewRenderLayer::Main
    }
}

fn preview_render_layer_is_effect(layer: PreviewRenderLayer) -> bool {
    matches!(
        layer,
        PreviewRenderLayer::Heatwave
            | PreviewRenderLayer::Particle
            | PreviewRenderLayer::ParticleDistortion
    )
}

fn path_is_shimmer_model_path(path: &str) -> bool {
    let path = path.replace('\\', "/").to_ascii_lowercase();
    path.rsplit('/').next().is_some_and(|name| {
        matches!(
            name,
            "shimmerlow.bmd"
                | "shimmerlow.bdl"
                | "shimmerlowfar.bmd"
                | "shimmerlowfar.bdl"
                | "shimmerhi.bmd"
                | "shimmerhi.bdl"
                | "shimmerhifar.bmd"
                | "shimmerhifar.bdl"
        )
    })
}

fn shimmer_preview_transform(transform: Transform) -> Transform {
    // TShimmer::perform builds inverse(view) * translate(runtime position) *
    // scale. In normal outdoor preview state that runtime position is zero;
    // the placement translation and rotation are not part of the draw matrix.
    Transform {
        translation: [0.0; 3],
        rotation_degrees: [0.0; 3],
        scale: transform.scale,
    }
}

fn actor_runtime_preview_transform(
    mut transform: Transform,
    preview: Option<&sms_scene::ActorPreview>,
) -> Transform {
    if let Some(scale) = preview.and_then(|preview| preview.runtime_uniform_scale) {
        transform.scale = [scale; 3];
    }
    transform
}

fn reset_fruit_preview_transform(
    object: &SceneObject,
    mut transform: Transform,
    registry: Option<&ObjectRegistry>,
) -> Transform {
    if object.factory_name != "ResetFruit" {
        return transform;
    }

    let Some(resource_name) = object
        .raw_param("actor_tail_string")
        .or_else(|| object.raw_param("stream_string_0"))
    else {
        return transform;
    };
    let Some(definition) = registry.and_then(|registry| {
        registry
            .find_map_obj_resource(resource_name)
            .and_then(|resource| registry.find_map_obj_ball_transform(resource.actor_type))
    }) else {
        return transform;
    };

    // TMapObjBall::initMapObj sets the fruit-specific body radius, then
    // TResetFruit::makeObjAppeared places the model at position.y + radius.
    // Banana and pineapple apply the additional matrix corrections below.
    let [rotation_x, rotation_y, rotation_z] = transform.rotation_degrees.map(f32::to_radians);
    let matrix_y_axis_y = transform.scale[1]
        * (rotation_x.sin() * rotation_y.sin() * rotation_z.sin()
            + rotation_z.cos() * rotation_x.cos());
    let mut y_offset = f32::from(definition.body_radius) * transform.scale[1];
    if let Some(correction) = definition.positive_y_axis_subtract {
        if matrix_y_axis_y > 0.0 {
            y_offset -= f32::from(correction) * matrix_y_axis_y;
        }
    } else if let Some(correction) = definition.one_minus_y_axis_subtract {
        y_offset -= f32::from(correction) * (1.0 - matrix_y_axis_y);
    }
    transform.translation[1] += y_offset;
    transform
}

fn path_is_sky_model_path(path: &str) -> bool {
    let path = path.replace('\\', "/").to_ascii_lowercase();
    path.ends_with("/map/map/sky.bmd") || path.ends_with("/map/map/sky.bdl")
}

fn path_is_water_model_path(path: &str) -> bool {
    let path = path.to_ascii_lowercase();
    if path_is_indirect_water_model_path(&path) {
        return false;
    }
    let model_name = path
        .rsplit('/')
        .next()
        .unwrap_or_default()
        .trim_end_matches(".bmd")
        .trim_end_matches(".bdl");
    matches!(model_name, "sea" | "biancoriver" | "monteriver")
        || path.contains("/map/map/water")
        || path.contains("/map/water/")
        || path.contains("/map/mirror/puddle")
        || path.contains("/map/map/puddle")
        || path.contains("/map/map/yogan")
        || path.contains("/map/map/lava")
}

fn path_is_water_reflection_model_path(path: &str) -> bool {
    let path = path.replace('\\', "/").to_ascii_lowercase();
    path.rsplit('/')
        .next()
        .is_some_and(|name| matches!(name, "reflectparts.bmd" | "reflectparts.bdl"))
}

fn path_is_mirror_sky_helper_model_path(path: &str) -> bool {
    let path = path.replace('\\', "/").to_ascii_lowercase();
    path.rsplit('/')
        .next()
        .is_some_and(|name| matches!(name, "reflectsky.bmd" | "reflectsky.bdl"))
}

fn path_is_mirror_surface_model_path(path: &str) -> bool {
    let path = path.replace('\\', "/").to_ascii_lowercase();
    if !path.contains("/map/mirror/") {
        return false;
    }
    path.rsplit('/').next().is_some_and(|name| {
        let stem = name.trim_end_matches(".bmd").trim_end_matches(".bdl");
        stem.strip_prefix("mirror")
            .is_some_and(|suffix| !suffix.is_empty() && suffix.chars().all(|c| c.is_ascii_digit()))
    })
}

fn mirror_cubes(document: &StageDocument) -> Vec<PreviewMirrorCube> {
    const MIRROR_CUBE_TABLE_NAME: &str = "鏡キューブテーブル";

    let mut cubes = Vec::new();
    for asset in document.assets.iter().filter(|asset| {
        asset.kind == StageAssetKind::Placement
            && asset
                .path
                .to_string_lossy()
                .replace('\\', "/")
                .to_ascii_lowercase()
                .ends_with("/map/tables.bin")
    }) {
        let Ok(bytes) = document.read_asset_bytes(&asset.path) else {
            continue;
        };
        let Ok(records) = parse_jdrama_object_records(&bytes) else {
            continue;
        };
        let mirror_tables = records
            .iter()
            .filter(|record| {
                record.type_name.rsplit("::").next() == Some("CubeGeneralInfoTable")
                    && record.object_name.as_deref() == Some(MIRROR_CUBE_TABLE_NAME)
            })
            .map(|record| record.offset..record.offset + record.size)
            .collect::<Vec<_>>();
        for cube in records.iter().filter(|record| {
            record.cube_general_info.is_some()
                && mirror_tables.iter().any(|table| {
                    record.offset > table.start && record.offset + record.size <= table.end
                })
        }) {
            let Some(info) = cube.cube_general_info else {
                continue;
            };
            let Ok(model_slot) = usize::try_from(info.data_no) else {
                continue;
            };
            let preview_cube = PreviewMirrorCube {
                center: info.center,
                rotation_degrees: info.rotation_degrees,
                dimensions: info.dimensions,
                model_slot,
            };
            if preview_cube
                .center
                .iter()
                .chain(preview_cube.rotation_degrees.iter())
                .chain(preview_cube.dimensions.iter())
                .all(|value| value.is_finite())
                && preview_cube
                    .dimensions
                    .iter()
                    .all(|dimension| *dimension > 0.0)
            {
                cubes.push(preview_cube);
            }
        }
    }
    cubes
}

#[cfg(test)]
fn active_mirror_model_slots(document: &StageDocument) -> BTreeSet<usize> {
    mirror_cubes(document)
        .into_iter()
        .map(|cube| cube.model_slot)
        .collect()
}

fn mirror_surface_model_is_active(
    stage_id: &str,
    path: &str,
    active_slots: &BTreeSet<usize>,
) -> bool {
    mirror_surface_model_slot(stage_id, path).is_none_or(|slot| active_slots.contains(&slot))
}

fn mirror_surface_model_slot(stage_id: &str, path: &str) -> Option<usize> {
    if !path_is_mirror_surface_model_path(path) {
        return None;
    }
    let path = path.replace('\\', "/").to_ascii_lowercase();
    let stem = path
        .rsplit('/')
        .next()?
        .trim_end_matches(".bmd")
        .trim_end_matches(".bdl");
    let suffix = stem.strip_prefix("mirror")?;
    if stage_id.to_ascii_lowercase().starts_with("pinna") && suffix == "205" {
        // TMirrorModelManager maps Pinna Park's first mirror slot to mirror205.
        Some(0)
    } else {
        suffix.parse().ok()
    }
}

fn path_is_indirect_water_model_path(path: &str) -> bool {
    let path = path.to_ascii_lowercase();
    path.contains("seaindirect") || path.contains("puddle_ind")
}

fn path_is_sea_indirect_model_path(path: &str) -> bool {
    let path = path.replace('\\', "/").to_ascii_lowercase();
    path.rsplit('/')
        .next()
        .is_some_and(|name| matches!(name, "seaindirect.bmd" | "seaindirect.bdl"))
}

fn path_is_goop_model_path(path: &str) -> bool {
    path.contains("/map/pollution/") || path.contains("pollution")
}

fn map_static_model_is_active(document: &StageDocument, model_path: &str) -> bool {
    let Some(registry) = document.registry.as_ref() else {
        return true;
    };
    let model_path = normalized_preview_asset_path(model_path);
    let matching_definitions = registry
        .map_static_models
        .iter()
        .filter(|definition| map_static_definition_matches_asset(definition, &model_path))
        .collect::<Vec<_>>();
    if matching_definitions.is_empty() {
        return true;
    }
    if matching_definitions
        .iter()
        .any(|definition| definition.stage_bootstrap_created)
    {
        return true;
    }

    matching_definitions
        .iter()
        .any(|definition| map_static_definition_is_active(document, definition))
}

fn map_static_model_loader_flags(document: &StageDocument, model_path: &str) -> Option<u32> {
    let registry = document.registry.as_ref()?;
    let model_path = normalized_preview_asset_path(model_path);
    let flags = registry
        .map_static_models
        .iter()
        .filter(|definition| map_static_definition_matches_asset(definition, &model_path))
        .filter(|definition| map_static_definition_is_active(document, definition))
        .map(|definition| definition.load_flags)
        .collect::<BTreeSet<_>>();
    (flags.len() == 1).then(|| *flags.first().expect("one map-static loader flag"))
}

fn map_static_definition_matches_asset(
    definition: &sms_schema::MapStaticModelDefinition,
    model_path: &str,
) -> bool {
    definition
        .model_path
        .as_deref()
        .is_some_and(|runtime_path| runtime_model_path_matches_asset(runtime_path, model_path))
}

fn map_static_definition_is_active(
    document: &StageDocument,
    definition: &sms_schema::MapStaticModelDefinition,
) -> bool {
    definition.stage_bootstrap_created
        || document.objects.iter().any(|object| {
            object.factory_name == "MapStaticObj"
                && object
                    .raw_param("actor_tail_string")
                    .or_else(|| object.raw_param("stream_string_0"))
                    .is_some_and(|name| name == definition.actor_name)
        })
}

fn runtime_model_path_matches_asset(runtime_path: &str, asset_path: &str) -> bool {
    let runtime_path = runtime_path.replace('\\', "/").to_ascii_lowercase();
    let asset_path = asset_path.replace('\\', "/").to_ascii_lowercase();
    if asset_path.ends_with(&runtime_path) {
        return true;
    }
    let mounted_path = asset_path
        .split_once("!/")
        .map(|(_, path)| format!("/{path}"))
        .unwrap_or_else(|| asset_path.clone());
    runtime_path.ends_with(&mounted_path)
}

fn active_pollution_layer_count(document: &StageDocument) -> usize {
    document
        .assets
        .iter()
        .find(|asset| {
            asset
                .path
                .to_string_lossy()
                .replace('\\', "/")
                .to_ascii_lowercase()
                .ends_with("/map/ymap.ymp")
        })
        .and_then(|asset| document.read_asset_bytes(&asset.path).ok())
        .and_then(|bytes| pollution_layer_count_from_bytes(&bytes))
        .unwrap_or(0) as usize
}

fn pollution_layer_count_from_bytes(bytes: &[u8]) -> Option<u32> {
    Some(u32::from_be_bytes(bytes.get(..4)?.try_into().ok()?))
}

fn pollution_layer_model_is_active(path: &str, active_layer_count: usize) -> bool {
    pollution_layer_model_index(path).is_none_or(|index| index < active_layer_count)
}

fn pollution_layer_model_index(path: &str) -> Option<usize> {
    let path = path.replace('\\', "/").to_ascii_lowercase();
    if !path.contains("/map/pollution/") {
        return None;
    }

    let stem = path
        .rsplit('/')
        .next()?
        .strip_suffix(".bmd")
        .or_else(|| path.rsplit('/').next()?.strip_suffix(".bdl"))?;
    let suffix = stem.strip_prefix("pollution")?;
    match suffix {
        "a" => Some(7),
        "b" => Some(8),
        digits if !digits.is_empty() && digits.bytes().all(|byte| byte.is_ascii_digit()) => {
            digits.parse().ok()
        }
        _ => None,
    }
}

fn generated_goop_model_visibility(document: &StageDocument, path: &str) -> Option<bool> {
    let path = path.replace('\\', "/");
    let stem = Path::new(
        path.rsplit_once("!/")
            .map_or(path.as_str(), |(_, path)| path),
    )
    .file_stem()?
    .to_str()?;
    document
        .goop_authoring
        .as_ref()?
        .layers
        .iter()
        .find_map(|layer| {
            (layer.origin == sms_scene::GoopLayerOrigin::Generated
                && layer.resource_stem.eq_ignore_ascii_case(stem))
            .then_some(layer.visible)
        })
}

fn model_loader_flags_for_path(path: &str) -> u32 {
    let path = path.replace('\\', "/").to_ascii_lowercase();
    let model_name = path
        .rsplit('/')
        .next()
        .unwrap_or_default()
        .trim_end_matches(".bmd")
        .trim_end_matches(".bdl");

    match model_name {
        // TShimmer::load passes 0x11010000 so the authored indirect block is
        // retained and can displace the screen-copy texture.
        "shimmerlow" | "shimmerlowfar" | "shimmerhi" | "shimmerhifar" => 0x1101_0000,
        // TSky::load uses SMS_MakeMActorWithAnmData(..., 0x10220000).
        "sky" if path_is_sky_model_path(&path) => SMS_DEFAULT_OBJECT_MODEL_LOAD_FLAGS,
        // TMapStaticObj::actor_data_table in MapStaticObject.cpp.
        "seaindirect" => 0x1121_0000,
        "sea" => 0x1022_0000,
        "riccosea" => 0x1021_0000,
        name if name.starts_with("riccoseapollution") => 0x1121_0000,
        _ if path_is_goop_model_path(&path) => SMS_POLLUTION_MODEL_LOAD_FLAGS,
        _ if path.contains("/mapobj/") || path.contains("/scene/mapobj/") => {
            // TMapObjBase::makeMActors uses this unless the object's indirect flag is set.
            SMS_DEFAULT_OBJECT_MODEL_LOAD_FLAGS
        }
        _ => SMS_MAP_MODEL_LOAD_FLAGS,
    }
}

fn actor_model_loader_flags(object: &SceneObject) -> Option<u32> {
    Some(match object.factory_name.as_str() {
        "NPCMonteM" | "NPCMonteMA" | "NPCMonteMC" | "NPCMonteW" | "NPCMonteWA" => 0x1030_0000,
        "NPCMonteMB" | "NPCMonteMD" | "NPCMonteMF" | "NPCMonteMG" | "NPCMonteMH" | "NPCMonteWB"
        | "NPCMonteWC" => 0x1021_0000,
        "NPCMonteME" => 0x1001_0000,
        // TMareMBaseManager/TMareWBaseManager::createModelData use these flags
        // for the shared Mare body models and their material programs.
        name if name.starts_with("NPCMareM") || name.starts_with("NPCMareW") => 0x1030_0000,
        _ => return None,
    })
}

fn preview_texture_tints(
    material_color: Option<[u8; 4]>,
    vertex_colors: Option<[[u8; 4]; 3]>,
    combine_mode: J3dPreviewCombineMode,
    render_layer: PreviewRenderLayer,
) -> [egui::Color32; 3] {
    let useful_material = material_color.filter(|color| preview_color_is_useful(*color));
    let base = match combine_mode {
        J3dPreviewCombineMode::TextureModulateMaterial => useful_material
            .map(material_color_rgb_tint)
            .unwrap_or([255, 255, 255, 255]),
        _ => [255, 255, 255, 255],
    };
    std::array::from_fn(|index| {
        let vertex = if combine_mode == J3dPreviewCombineMode::TextureModulateVertex {
            vertex_colors
                .map(|colors| colors[index])
                .filter(|color| preview_color_is_useful(*color))
                .unwrap_or([255, 255, 255, 255])
        } else {
            [255, 255, 255, 255]
        };
        let has_alpha_source = useful_material.is_some()
            || (combine_mode == J3dPreviewCombineMode::TextureModulateVertex
                && vertex_colors.is_some());
        apply_layer_preview_tint(
            modulated_color(base, vertex, if has_alpha_source { 0 } else { 255 }),
            render_layer,
        )
    })
}

fn material_color_rgb_tint(mut color: [u8; 4]) -> [u8; 4] {
    color[3] = 255;
    color
}

fn preview_triangle_color(
    model_index: usize,
    normal: [f32; 3],
    average_y: f32,
    material_color: Option<[u8; 4]>,
    vertex_colors: Option<[[u8; 4]; 3]>,
    combine_mode: J3dPreviewCombineMode,
) -> egui::Color32 {
    let palette = [
        [89.0, 123.0, 102.0],
        [129.0, 118.0, 86.0],
        [86.0, 111.0, 130.0],
        [120.0, 101.0, 82.0],
        [91.0, 128.0, 122.0],
        [139.0, 132.0, 102.0],
    ];
    let mut base =
        if let Some(color) = material_color.filter(|color| preview_color_is_useful(*color)) {
            [color[0] as f32, color[1] as f32, color[2] as f32]
        } else {
            palette[model_index % palette.len()]
        };
    if material_color.is_none() {
        if average_y < -3500.0 {
            base = [42.0, 94.0, 104.0];
        } else if average_y > 2800.0 {
            base = [117.0, 129.0, 122.0];
        }
    }

    let up = normal[1].abs();
    let side = (normal[0].abs() + normal[2].abs()) * 0.5;
    let shade = (0.58 + up * 0.26 + side * 0.08).clamp(0.42, 1.0);
    if combine_mode == J3dPreviewCombineMode::VertexOnly {
        if let Some(colors) = vertex_colors {
            let average = average_vertex_color(colors);
            if preview_color_is_useful(average) {
                return egui::Color32::from_rgba_unmultiplied(
                    average[0], average[1], average[2], average[3],
                );
            }
        }
    }
    if let Some(color) = material_color.filter(|color| preview_color_is_useful(*color)) {
        return egui::Color32::from_rgba_unmultiplied(color[0], color[1], color[2], color[3]);
    }
    egui::Color32::from_rgba_unmultiplied(
        (base[0] * shade).clamp(0.0, 255.0) as u8,
        (base[1] * shade).clamp(0.0, 255.0) as u8,
        (base[2] * shade).clamp(0.0, 255.0) as u8,
        198,
    )
}

fn preview_solid_triangle_colors(
    triangle: &PreviewTriangle,
    normal: [f32; 3],
    average_y: f32,
) -> [egui::Color32; 3] {
    if triangle.combine_mode == J3dPreviewCombineMode::VertexOnly {
        if let Some(colors) = triangle.vertex_colors {
            if colors.iter().any(|color| preview_color_is_useful(*color)) {
                return colors.map(|color| {
                    apply_layer_preview_tint(
                        egui::Color32::from_rgba_unmultiplied(
                            color[0], color[1], color[2], color[3],
                        ),
                        triangle.render_layer,
                    )
                });
            }
        }
    }

    let color = preview_triangle_color(
        triangle.model_index,
        normal,
        average_y,
        triangle.color,
        triangle.vertex_colors,
        triangle.combine_mode,
    );
    [apply_layer_preview_tint(color, triangle.render_layer); 3]
}

fn apply_layer_preview_tint(
    color: egui::Color32,
    render_layer: PreviewRenderLayer,
) -> egui::Color32 {
    let rgba = color32_to_rgba(color);
    match render_layer {
        PreviewRenderLayer::Water | PreviewRenderLayer::MirrorSurface => {
            let water = [0.35, 0.78, 0.96];
            rgba_to_color32([
                rgba[0] * 0.72 + water[0] * 0.28,
                rgba[1] * 0.72 + water[1] * 0.28,
                rgba[2] * 0.72 + water[2] * 0.28,
                rgba[3].min(0.82),
            ])
        }
        PreviewRenderLayer::Goop => {
            let goop = [0.28, 0.12, 0.36];
            rgba_to_color32([
                rgba[0] * 0.82 + goop[0] * 0.18,
                rgba[1] * 0.82 + goop[1] * 0.18,
                rgba[2] * 0.82 + goop[2] * 0.18,
                rgba[3].min(0.78),
            ])
        }
        PreviewRenderLayer::Sky
        | PreviewRenderLayer::Main
        | PreviewRenderLayer::WaveFoam
        | PreviewRenderLayer::IndirectWater
        | PreviewRenderLayer::MirrorScene
        | PreviewRenderLayer::Shadow
        | PreviewRenderLayer::Heatwave
        | PreviewRenderLayer::Particle
        | PreviewRenderLayer::ParticleDistortion => color,
    }
}

fn preview_color_is_useful(color: [u8; 4]) -> bool {
    color[3] > 12
        && !(color[0] > 242 && color[1] > 242 && color[2] > 242)
        && !(color[0] < 8 && color[1] < 8 && color[2] < 8)
}

fn average_vertex_color(colors: [[u8; 4]; 3]) -> [u8; 4] {
    [
        ((colors[0][0] as u16 + colors[1][0] as u16 + colors[2][0] as u16) / 3) as u8,
        ((colors[0][1] as u16 + colors[1][1] as u16 + colors[2][1] as u16) / 3) as u8,
        ((colors[0][2] as u16 + colors[1][2] as u16 + colors[2][2] as u16) / 3) as u8,
        ((colors[0][3] as u16 + colors[1][3] as u16 + colors[2][3] as u16) / 3) as u8,
    ]
}

fn modulated_color(material: [u8; 4], vertex: [u8; 4], fallback_alpha: u8) -> egui::Color32 {
    egui::Color32::from_rgba_unmultiplied(
        (material[0] as u16 * vertex[0] as u16 / 255) as u8,
        (material[1] as u16 * vertex[1] as u16 / 255) as u8,
        (material[2] as u16 * vertex[2] as u16 / 255) as u8,
        ((material[3] as u16 * vertex[3] as u16 / 255) as u8).max(fallback_alpha),
    )
}

#[derive(Debug, Default, Clone, Copy)]
struct VectorDragResponse {
    changed: bool,
    started: bool,
    stopped: bool,
}

impl VectorDragResponse {
    fn merge(&mut self, other: Self) {
        self.changed |= other.changed;
        self.started |= other.started;
        self.stopped |= other.stopped;
    }
}

fn vector_drag(ui: &mut egui::Ui, values: &mut [f32; 3], speed: f32) -> VectorDragResponse {
    let mut edit = VectorDragResponse::default();
    ui.horizontal(|ui| {
        for (label, value) in ["X", "Y", "Z"].iter().zip(values.iter_mut()) {
            ui.label(*label);
            let response = ui.add(egui::DragValue::new(value).speed(speed));
            edit.changed |= response.changed();
            edit.started |= response.drag_started();
            edit.stopped |= response.drag_stopped();
        }
    });
    edit
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct ContentBrowserLayout {
    columns: usize,
    card_width: f32,
}

fn content_browser_layout(available_width: f32, item_count: usize) -> ContentBrowserLayout {
    const MIN_CARD_WIDTH: f32 = 180.0;
    const MAX_CARD_WIDTH: f32 = 260.0;
    const GAP: f32 = 8.0;

    let available_width = available_width.max(MIN_CARD_WIDTH);
    let maximum_columns =
        (((available_width + GAP) / (MIN_CARD_WIDTH + GAP)).floor() as usize).max(1);
    let columns = item_count.max(1).min(maximum_columns);
    let used_gaps = GAP * columns.saturating_sub(1) as f32;
    let card_width =
        ((available_width - used_gaps) / columns as f32).clamp(MIN_CARD_WIDTH, MAX_CARD_WIDTH);
    ContentBrowserLayout {
        columns,
        card_width,
    }
}

fn editor_window_title(project_name: Option<&str>, stage_id: Option<&str>) -> String {
    let project_name = project_name.map(str::trim).filter(|name| !name.is_empty());
    let stage_id = stage_id.map(str::trim).filter(|stage| !stage.is_empty());
    match (project_name, stage_id) {
        (Some(project), Some(stage)) => format!("{project} - {stage} - Graffito-Editor"),
        (Some(project), None) => format!("{project} - Graffito-Editor"),
        (None, Some(stage)) => format!("{stage} - Graffito-Editor"),
        (None, None) => "Graffito-Editor".to_string(),
    }
}

fn runtime_yaw_degrees_per_frame(object: &SceneObject) -> f32 {
    let factory = object.factory_name.as_str();
    let class = object.class_name.as_deref().unwrap_or_default();

    // TItemManager::perform advances its shared item matrix by two degrees
    // every retail frame. TItem::calc copies that matrix into coins, including
    // the red, blue, flower, and joint-attached TCoin variants.
    let is_coin = is_coin_object(object);
    // TShine::control advances the normal live state by unk16C, whose
    // constructor default is also two degrees per retail frame.
    if is_coin || factory == "Shine" || class == "TShine" {
        2.0
    } else {
        0.0
    }
}

fn is_coin_object(object: &SceneObject) -> bool {
    let factory = object.factory_name.as_str();
    let class = object.class_name.as_deref().unwrap_or_default();
    let resource = object
        .raw_param("actor_tail_string")
        .or_else(|| object.raw_param("stream_string_0"))
        .map(str::to_ascii_lowercase);
    if resource.as_deref() == Some("invisible_coin") {
        return false;
    }

    matches!(
        factory,
        "coin" | "CoinRed" | "CoinBlue" | "coin_red" | "coin_blue" | "FlowerCoin" | "joint_coin"
    ) || class.starts_with("TCoin")
        || class == "Coin"
        || class == "CoinRed"
        || class == "CoinBlue"
        || class == "TFlowerCoin"
        || matches!(
            resource.as_deref(),
            Some("coin" | "coin_red" | "coin_blue" | "joint_coin")
        )
}

fn runtime_rotated_transform(
    mut transform: Transform,
    elapsed_seconds: f32,
    yaw_degrees_per_frame: f32,
) -> Transform {
    transform.rotation_degrees[1] = (transform.rotation_degrees[1]
        + elapsed_seconds.max(0.0) * SMS_ANIMATION_FRAMES_PER_SECOND * yaw_degrees_per_frame)
        .rem_euclid(360.0);
    transform
}

fn snap_value(value: f32, step: f32) -> f32 {
    (value / step).round() * step
}

fn sanitize_id(value: &str) -> String {
    value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '_' || ch == '-' {
                ch
            } else {
                '_'
            }
        })
        .collect()
}

#[cfg(test)]
mod noki_render_test;

#[cfg(test)]
mod tests;
