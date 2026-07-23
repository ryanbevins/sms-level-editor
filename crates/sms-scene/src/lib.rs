//! Editable stage documents and safe editor-project persistence.

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsStr;
use std::fmt;
use std::fs;
use std::ops::Deref;
use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};
use sms_formats::{
    mount_scene_archive, parse_jdrama_object_records, read_stage_asset_bytes,
    scan_common_stage_assets, scan_stage_assets, JDramaAmbient, JDramaDocument, JDramaField,
    JDramaFieldValue, JDramaLight, JDramaObjectRecord, JDramaRecord, JDramaRecordPayload, PrmFile,
    SourceLocation, StageAsset, StageAssetKind,
};
use sms_schema::{
    EnemyActorDefinition, EnemyManagerDefinition, EnemyModelDefinition, ObjectRegistry,
};
use thiserror::Error;

mod blank_stage;
mod dialogue_authoring;
mod goop_authoring;
mod object_authoring;
mod object_parameters;
mod project_store;
mod route_authoring;
mod stage_archive;
mod stage_export;
mod validation;

pub use blank_stage::{
    blank_stage_mario_record, blank_stage_sky_record, runtime_sky_material_table,
    BlankStageBootstrapKind, BlankStageBootstrapManifest, BlankStageBootstrapRequirement,
    BlankStageBootstrapResource, BlankStageLightingPreset, BlankStagePreset,
    BlankStageSkyboxPreset, BlankStageTargetMetadata, BLANK_STAGE_BOOTSTRAP_REQUIREMENTS,
    BLANK_STAGE_CAMERA_DIRECTORY_MARKER_PATH, BLANK_STAGE_COIN_PARTICLE_PATH,
    BLANK_STAGE_PRESET_VERSION, DEFAULT_BLANK_STAGE_TARGET_SLOT,
};
pub use dialogue_authoring::{
    CommonDialogueResourceEdit, CompiledDialogueEdits, DialogueAuthoringDocument,
    DialogueAuthoringToken, DialogueCallsiteClassification, DialogueCallsiteStatus,
    DialogueConsumer, DialogueContent, DialogueDomain, DialogueEditScope,
    DialogueGameConsumerIndex, DialogueMessageRef, DialogueObjectAuthoring, DialogueProvenance,
    DialogueResolutionIssue, DialogueResolutionSeverity, DialogueRouteIndex, DialogueRouteKind,
    DialogueSourceAnchor, DialogueStableAllocation, DialogueVariant, DialogueVariantKey,
    DialogueVariantOverride, ProjectDialogueAllocation, ProjectDialogueLibrary,
    ProjectDialogueOverride, RuntimeDialogueGuard, RuntimeDialogueOverride,
    RuntimeDialogueOverrideRequest, BALLOON_DIALOGUE_MESSAGE_PATH,
    DIALOGUE_AUTHORING_FORMAT_VERSION, GENERATED_DIALOGUE_SCRIPT_MARKER,
    GENERATED_DIALOGUE_SCRIPT_PATH, PROJECT_DIALOGUE_LIBRARY_FORMAT_VERSION,
    PROJECT_DIALOGUE_LIBRARY_PATH, STAGE_DIALOGUE_MESSAGE_PATH, SYSTEM_DIALOGUE_MESSAGE_PATH,
};
pub use goop_authoring::{
    generate_floor_depth_map, generate_floor_pollution_model, terrain_fingerprint,
    whole_terrain_region, GoopAuthoringDocument, GoopBehavior, GoopLayerAuthoring, GoopLayerOrigin,
    GoopPlane, GoopRegion, GoopRenderTriangle, GoopStyleSource, GoopTerrainTriangle,
    GOOP_AUTHORING_FORMAT_VERSION, GOOP_CELL_SIZE, GOOP_DEPTH_WORLD_UNITS_PER_CODE,
    GOOP_MAX_DIMENSION, GOOP_MAX_LAYERS, GOOP_RESOURCE_PATH,
};
pub use object_authoring::{
    ObjectAuthoringCatalog, ObjectAuthoringCatalogBuild, ObjectAuthoringCatalogWarning,
    ObjectAuthoringDependency, ObjectAuthoringResource, ObjectAuthoringRuntimeActorReference,
    ObjectAuthoringTableDependency, ObjectAuthoringTemplate, SHINE_QUICK_CAMERA_NAME,
};
pub(crate) use object_parameters::validate_object_parameter_links_with_owned_name;
pub use object_parameters::{
    apply_all_object_parameters, apply_dirty_object_parameter_edits, apply_object_parameter_edits,
    editable_object_parameters, editable_parameters_for_object, seed_scene_object_parameters,
    sync_scene_object_parameter_aliases, EditableSceneParameter, ObjectParameterBitFlag,
    ObjectParameterChoice, ObjectParameterIndexedChoice, ObjectParameterInfo, ObjectParameterKind,
    ParameterApplyMode, OBJECT_PARAMETER_CHARACTER_NAME, OBJECT_PARAMETER_NAME,
};
pub use route_authoring::{
    BezierHandles, RouteAssignmentSuggestion, RouteAuthoringDocument, RouteAuthoringError,
    RouteControlPoint, RouteDirection, RouteGraph, RouteLink, RoutePeriod,
    DEFAULT_ROUTE_BAKE_TOLERANCE, MAX_ROUTE_SAMPLES_PER_LINK, ROUTE_RESOURCE_PATH,
};
pub use stage_archive::{
    SourceFreeStageArchive, StageCompression, StageObjectPlacement, StageOrigin, StageResource,
    StageResourceDocument,
};
pub use stage_export::{
    StageArchiveEdits, StageArchiveExportOutcome, StageCollisionEdit, StageCollisionEditMode,
    StageModelEdit, StageModelEditMode, StagePlacementInsert, StageResourceEdit,
    StageResourceEditMode,
};

#[derive(Debug, Error)]
pub enum SceneError {
    #[error("base root does not exist: {0}")]
    MissingBaseRoot(PathBuf),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("format error: {0}")]
    Format(#[from] sms_formats::FormatError),
    #[error("stage resource {path}: {source}")]
    StageResource {
        path: String,
        #[source]
        source: sms_formats::FormatError,
    },
    #[error("stage archive export failed: {0}")]
    StageExport(String),
    #[error("rebuilt stage archive output overlaps the extracted base directory: {0}")]
    StageArchiveOutputOverlapsBase(PathBuf),
    #[error("manifest serialization error: {0}")]
    TomlSer(#[from] toml::ser::Error),
    #[error("manifest parsing error: {0}")]
    TomlDe(#[from] toml::de::Error),
    #[error("scene overlay serialization error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("invalid stage id for an editor project path: {0}")]
    InvalidStageId(String),
    #[error("project output path must be relative and traversal-free: {0}")]
    UnsafeProjectPath(PathBuf),
    #[error("project output folder must have a parent and file name: {0}")]
    InvalidProjectRoot(PathBuf),
    #[error("project output folder overlaps the extracted base game directory: {0}")]
    ProjectOverlapsBase(PathBuf),
    #[error("refusing to replace a directory that is not an owned Graffito-Editor project: {0}")]
    UnownedProjectRoot(PathBuf),
    #[error("unsupported Graffito-Editor project manifest at {path}: {reason}")]
    UnsupportedProjectManifest { path: PathBuf, reason: String },
    #[error(
        "Graffito-Editor project manifest at {path} belongs to base root '{manifest_base}', not the open base root '{open_base}'"
    )]
    ProjectBaseMismatch {
        path: PathBuf,
        manifest_base: PathBuf,
        open_base: PathBuf,
    },
    #[error("project export is blocked by validation errors: {0}")]
    ValidationFailed(String),
    #[error(
        "project transaction failed for {project_root}: {message}; recovery data: {recovery_path}"
    )]
    ProjectTransactionFailed {
        project_root: PathBuf,
        message: String,
        recovery_path: PathBuf,
    },
    #[error("project contains an unsupported filesystem entry: {0}")]
    UnsupportedProjectEntry(PathBuf),
    #[error("project file exceeds the {limit}-byte read limit: {path}")]
    ProjectFileTooLarge { path: PathBuf, limit: u64 },
    #[error("project output would overwrite an unmanaged file: {0}")]
    UnmanagedProjectFileConflict(PathBuf),
    #[error("existing project must be loaded before it can be updated: {0}")]
    ProjectNotLoaded(PathBuf),
    #[error("project changed since it was loaded: {0}")]
    StaleProject(PathBuf),
    #[error("project changed on disk while it was being saved: {0}")]
    ProjectChangedDuringSave(PathBuf),
    #[error("project changed on disk while it was being loaded: {0}")]
    ProjectChangedDuringLoad(PathBuf),
    #[error("project overlay stage '{overlay_stage}' does not match requested stage '{stage}'")]
    ProjectOverlayStageMismatch {
        overlay_stage: String,
        stage: String,
    },
    #[error(
        "project object '{object_id}' references source record {source_path}@{offset:?}, which is not present in the open base stage"
    )]
    ProjectOverlaySourceMismatch {
        object_id: String,
        source_path: PathBuf,
        offset: Option<u64>,
    },
}

pub type Result<T> = std::result::Result<T, SceneError>;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EditorProjectManifest {
    pub format_version: u32,
    pub kind: String,
    #[serde(default)]
    pub project_id: String,
    #[serde(default)]
    pub revision: u64,
    pub base_path: PathBuf,
    pub project_files_path: PathBuf,
    pub created_with: String,
    pub changed_files: Vec<PathBuf>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ProjectSaveOutcome {
    pub manifest: EditorProjectManifest,
    pub warnings: Vec<ProjectSaveWarning>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectSaveWarning {
    pub recovery_path: PathBuf,
    pub message: String,
}

impl EditorProjectManifest {
    pub fn new(
        base_path: PathBuf,
        project_files_path: PathBuf,
        project_id: impl Into<String>,
    ) -> Self {
        Self {
            format_version: project_store::PROJECT_FORMAT_VERSION,
            kind: project_store::PROJECT_KIND.to_string(),
            project_id: project_id.into(),
            revision: 0,
            base_path,
            project_files_path,
            created_with: env!("CARGO_PKG_VERSION").to_string(),
            changed_files: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct StageDocument {
    pub stage_id: String,
    pub base_root: PathBuf,
    pub assets: Vec<StageAsset>,
    pub objects: Vec<SceneObject>,
    pub changed_files: BTreeMap<PathBuf, Vec<u8>>,
    /// Complete detached semantic import of the stage archive. The original
    /// archive bytes and RARC child payload slots are never retained here.
    pub stage_archive: Option<SourceFreeStageArchive>,
    /// Informational source identity captured at import time. Export never
    /// opens this path again.
    pub stage_archive_source_path: Option<PathBuf>,
    /// Source-free model and collision replacements authored for stage export.
    pub archive_edits: StageArchiveEdits,
    pub registry: Option<ObjectRegistry>,
    /// Project-only control points and Bezier handles for `map/scene.ral`.
    /// The compiled retail representation remains an authored Rail resource.
    pub route_authoring: Option<RouteAuthoringDocument>,
    /// Project-side goop layers and masks. Runtime YMP/BMP/BMD resources are
    /// compiled into the semantic archive overlay.
    pub goop_authoring: Option<GoopAuthoringDocument>,
    /// Per-stage dialogue deltas. The route index is always derived from the
    /// effective scripts and messages and is never serialized.
    pub dialogue_authoring: Option<DialogueAuthoringDocument>,
    /// Project-global common-message deltas and stable allocations.
    pub dialogue_library: ProjectDialogueLibrary,
    pub load_issues: Vec<ValidationIssue>,
    pub lighting: StageLighting,
    pub actor_previews: BTreeMap<String, ActorPreview>,
    pub loaded_project: Option<LoadedProjectState>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoadedProjectState {
    pub project_root: PathBuf,
    pub project_id: String,
    pub revision: u64,
    #[doc(hidden)]
    pub project_fingerprint: u128,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ActorPreview {
    pub model_path: String,
    pub load_flags: u32,
    pub manager_factory: String,
    pub runtime_uniform_scale: Option<f32>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct StageLighting {
    pub lights: Vec<JDramaLight>,
    pub ambients: Vec<JDramaAmbient>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct StageObjectLighting {
    pub position: [f32; 3],
    pub color: [u8; 4],
    pub ambient: [u8; 4],
}

impl StageLighting {
    pub fn object_lighting(&self) -> Option<StageObjectLighting> {
        self.resolve_object_lighting()
            .map(|(lighting, _used_ordinal_fallback)| lighting)
    }

    pub fn object_lighting_uses_ordinal_fallback(&self) -> bool {
        self.resolve_object_lighting()
            .is_some_and(|(_lighting, used_ordinal_fallback)| used_ordinal_fallback)
    }

    fn resolve_object_lighting(&self) -> Option<(StageObjectLighting, bool)> {
        let object_primary = |name: &str| {
            name.contains("オブジェクト")
                && name.contains("太陽")
                && !name.contains("サブ")
                && !name.contains("スペキュラ")
        };
        let (light, light_fallback) = self
            .lights
            .iter()
            .find(|light| light.name.as_deref().is_some_and(object_primary))
            .map(|light| (light, false))
            .or_else(|| self.lights.get(5).map(|light| (light, true)))?;
        let (ambient, ambient_fallback) = self
            .ambients
            .iter()
            .find(|ambient| {
                ambient.name.as_deref().is_some_and(|name| {
                    name.contains("オブジェクト")
                        && name.contains("アンビエント")
                        && !name.contains("サブ")
                })
            })
            .map(|ambient| (ambient, false))
            .or_else(|| self.ambients.get(2).map(|ambient| (ambient, true)))?;
        Some((
            StageObjectLighting {
                position: light.position,
                color: light.color,
                ambient: ambient.color,
            },
            light_fallback || ambient_fallback,
        ))
    }
}

impl StageDocument {
    pub fn open(base_root: impl AsRef<Path>, stage_id: impl Into<String>) -> Result<Self> {
        let base_root = base_root.as_ref().to_path_buf();
        if !base_root.exists() {
            return Err(SceneError::MissingBaseRoot(base_root));
        }

        let stage_id = stage_id.into();
        let assets = scan_stage_assets(&base_root, &stage_id)?;
        let (objects, mut load_issues, lighting) = load_scene_objects_from_assets(&assets);
        let (stage_archive_source_path, stage_archive) =
            match stage_export::import_exact_stage_archive(&base_root, &stage_id) {
                Ok((source_path, archive)) => (Some(source_path), Some(archive)),
                Err(error) => {
                    load_issues.push(ValidationIssue::error(
                        "stage-semantic-import-failed",
                        format!(
                            "Could not import a detached semantic archive for stage '{stage_id}': {error}"
                        ),
                    ));
                    (None, None)
                }
            };
        Ok(Self {
            stage_id,
            base_root,
            assets,
            objects,
            changed_files: BTreeMap::new(),
            stage_archive,
            stage_archive_source_path,
            archive_edits: StageArchiveEdits::default(),
            registry: None,
            load_issues,
            route_authoring: None,
            goop_authoring: None,
            dialogue_authoring: None,
            dialogue_library: ProjectDialogueLibrary::default(),
            lighting,
            actor_previews: BTreeMap::new(),
            loaded_project: None,
        })
    }

    /// Creates a document for a genuinely new authored stage id.
    ///
    /// No retail stage archive is opened or required. The semantic archive is
    /// mounted at the virtual runtime identity
    /// `<base>/files/data/scene/<stage_id>.szs`, which is also the path a
    /// managed release build will author later.
    pub fn from_authored_archive(
        base_root: impl AsRef<Path>,
        stage_id: impl Into<String>,
        mut archive: SourceFreeStageArchive,
    ) -> Result<Self> {
        let base_root = base_root.as_ref().to_path_buf();
        if !base_root.exists() {
            return Err(SceneError::MissingBaseRoot(base_root));
        }
        let stage_id = stage_id.into();
        validate_stage_id(&stage_id)?;
        validate_authored_archive_target(&archive, &stage_id)?;
        blank_stage::ensure_blank_stage_runtime_resources(&mut archive)?;
        let source_path = authored_stage_virtual_source_path(&base_root, &stage_id)?;
        let assets = authored_stage_assets(&base_root, &source_path, &archive)?;
        let (objects, load_issues, lighting) =
            load_scene_objects_from_assets_with_reader(&assets, |path| {
                read_semantic_stage_asset_bytes(&archive, &source_path, path)
            });
        let semantic_json = archive.to_semantic_json()?;
        let archive = SourceFreeStageArchive::from_semantic_json(&semantic_json)?;
        Ok(Self {
            stage_id,
            base_root,
            assets,
            objects,
            changed_files: BTreeMap::new(),
            stage_archive: Some(archive),
            stage_archive_source_path: Some(source_path),
            archive_edits: StageArchiveEdits::default(),
            registry: None,
            load_issues,
            route_authoring: None,
            goop_authoring: None,
            dialogue_authoring: None,
            dialogue_library: ProjectDialogueLibrary::default(),
            lighting,
            actor_previews: BTreeMap::new(),
            loaded_project: None,
        })
    }

    /// Reopens an authored stage directly from a managed project baseline.
    /// This is the safe restart path for stage ids that do not exist in the
    /// extracted retail game.
    pub fn open_authored_project_stage(
        base_root: impl AsRef<Path>,
        stage_id: impl Into<String>,
        project_root: impl AsRef<Path>,
    ) -> Result<Self> {
        let base_root = base_root.as_ref();
        let stage_id = stage_id.into();
        let archive = project_store::load_authored_stage_baseline(
            base_root,
            &stage_id,
            project_root.as_ref(),
        )?
        .ok_or_else(|| {
            SceneError::StageExport(format!(
                "project has no authored stage baseline for '{stage_id}'"
            ))
        })?;
        let mut document = Self::from_authored_archive(base_root, stage_id, archive)?;
        document.load_project_folder(project_root)?;
        Ok(document)
    }

    /// Replaces the current stage baseline with a source-free authored archive
    /// targeting this document's existing retail slot. Project identity and
    /// already loaded project state are preserved; scene-derived state is
    /// regenerated entirely from the authored semantic resources.
    pub fn replace_with_authored_archive(
        &mut self,
        mut archive: SourceFreeStageArchive,
    ) -> Result<()> {
        validate_authored_archive_target(&archive, &self.stage_id)?;
        blank_stage::ensure_blank_stage_runtime_resources(&mut archive)?;

        // Normalize through the public detached document format. Besides
        // proving a stable semantic rebuild, this prevents a caller-owned
        // archive instance from carrying unvalidated container state into the
        // editor document.
        let semantic_json = archive.to_semantic_json()?;
        let archive = SourceFreeStageArchive::from_semantic_json(&semantic_json)?;
        let source_path = match &self.stage_archive_source_path {
            Some(source_path) => source_path.clone(),
            None => authored_stage_virtual_source_path(&self.base_root, &self.stage_id)?,
        };
        let assets = authored_stage_assets(&self.base_root, &source_path, &archive)?;
        let (objects, load_issues, lighting) =
            load_scene_objects_from_assets_with_reader(&assets, |path| {
                read_semantic_stage_asset_bytes(&archive, &source_path, path)
            });

        let registry = self.registry.take();
        self.assets = assets;
        self.objects = objects;
        self.stage_archive = Some(archive);
        self.stage_archive_source_path = Some(source_path);
        self.archive_edits = StageArchiveEdits::default();
        self.load_issues = load_issues;
        self.lighting = lighting;
        self.route_authoring = None;
        self.goop_authoring = None;
        self.dialogue_authoring = None;
        self.actor_previews.clear();
        if let Some(registry) = registry {
            self.set_registry(registry);
        }
        Ok(())
    }

    /// Reads a stage asset from the document's detached semantic archive when
    /// the path addresses its mounted stage source. Common files and other
    /// external assets retain the normal filesystem/archive reader behavior.
    pub fn read_asset_bytes(&self, path: impl AsRef<Path>) -> Result<Vec<u8>> {
        let path = path.as_ref();
        if let (Some(archive), Some(source_path)) =
            (&self.stage_archive, &self.stage_archive_source_path)
        {
            if let Some(raw_resource_path) = semantic_resource_path_for_asset(source_path, path) {
                if let Some(edit) = self
                    .archive_edits
                    .resources
                    .iter()
                    .find(|edit| edit.raw_resource_path == raw_resource_path)
                {
                    return Ok(edit.document.to_bytes()?);
                }
                if let Some(edit) = self
                    .archive_edits
                    .models
                    .iter()
                    .find(|edit| edit.raw_resource_path == raw_resource_path)
                {
                    return Ok(edit.document.to_bytes()?);
                }
            }
            if let Some(bytes) = read_matching_semantic_stage_asset(archive, source_path, path)? {
                return Ok(bytes);
            }
        }
        Ok(read_stage_asset_bytes(path)?)
    }

    /// Clones the semantic resource after applying the current authored
    /// overlay in export order. This lets transactional editors merge into a
    /// detached resource without consulting or mutating the imported source.
    pub fn effective_resource_clone(
        &self,
        raw_resource_path: &[u8],
    ) -> Result<Option<StageResourceDocument>> {
        let mut current = if self
            .archive_edits
            .resource_removals
            .iter()
            .any(|removed| removed == raw_resource_path)
        {
            None
        } else {
            self.stage_archive
                .as_ref()
                .and_then(|archive| archive.resource(raw_resource_path))
                .cloned()
        };
        if let Some(edit) = self
            .archive_edits
            .resources
            .iter()
            .find(|edit| edit.raw_resource_path == raw_resource_path)
        {
            if edit.mode == StageResourceEditMode::Insert && current.is_some() {
                return Err(SceneError::StageExport(format!(
                    "resource {} is already present but has an insert-only authored edit",
                    String::from_utf8_lossy(raw_resource_path)
                )));
            }
            current = Some(edit.document.clone());
        }
        for edit in self
            .archive_edits
            .models
            .iter()
            .filter(|edit| edit.raw_resource_path == raw_resource_path)
        {
            match (&current, edit.mode) {
                (Some(StageResourceDocument::Model(_)), _) | (None, StageModelEditMode::Upsert) => {
                    current = Some(StageResourceDocument::Model(edit.document.clone()));
                }
                (Some(_), _) => {
                    return Err(SceneError::StageExport(format!(
                        "resource {} has a model edit but is not a model",
                        String::from_utf8_lossy(raw_resource_path)
                    )));
                }
                (None, StageModelEditMode::Replace) => {
                    return Err(SceneError::StageExport(format!(
                        "model resource {} was not found",
                        String::from_utf8_lossy(raw_resource_path)
                    )));
                }
            }
        }
        for edit in self
            .archive_edits
            .collisions
            .iter()
            .filter(|edit| edit.raw_resource_path == raw_resource_path)
        {
            let replacement = match (&current, edit.mode) {
                (
                    Some(StageResourceDocument::Collision(existing)),
                    StageCollisionEditMode::Append,
                ) => stage_export::append_collision_document(
                    existing,
                    &edit.document,
                    raw_resource_path,
                )?,
                (Some(StageResourceDocument::Collision(_)), _)
                | (None, StageCollisionEditMode::Upsert) => edit.document.clone(),
                (Some(_), _) => {
                    return Err(SceneError::StageExport(format!(
                        "resource {} has a collision edit but is not collision data",
                        String::from_utf8_lossy(raw_resource_path)
                    )));
                }
                (None, _) => {
                    return Err(SceneError::StageExport(format!(
                        "collision resource {} was not found",
                        String::from_utf8_lossy(raw_resource_path)
                    )));
                }
            };
            current = Some(StageResourceDocument::Collision(replacement));
        }
        Ok(current)
    }
    /// Materializes the effective retail rail as project-side controls on first use.
    pub fn ensure_route_authoring(&mut self) -> Result<&mut RouteAuthoringDocument> {
        if self.route_authoring.is_none() {
            let resource = self.effective_resource_clone(ROUTE_RESOURCE_PATH)?;
            let rail = match resource {
                Some(StageResourceDocument::Rail(rail)) => rail,
                Some(_) => {
                    return Err(SceneError::StageExport(
                        "map/scene.ral is not a RAL resource".to_string(),
                    ));
                }
                None => sms_formats::RalDocument::empty_canonical(),
            };
            self.route_authoring = Some(RouteAuthoringDocument::lift(ROUTE_RESOURCE_PATH, &rail));
        }
        Ok(self
            .route_authoring
            .as_mut()
            .expect("route authoring initialized"))
    }

    /// Compiles project-side route state into the detached semantic archive overlay.
    pub fn compile_route_authoring(&mut self) -> Result<()> {
        let Some(authoring) = self.route_authoring.as_ref() else {
            return Ok(());
        };
        let rail = authoring
            .compile()
            .map_err(|error| SceneError::StageExport(error.to_string()))?;
        self.upsert_authored_resource(
            authoring.raw_resource_path.clone(),
            StageResourceDocument::Rail(rail),
        );
        Ok(())
    }

    pub fn route_consumers(&self, graph_name: &str) -> Vec<&SceneObject> {
        self.objects
            .iter()
            .filter(|object| object.raw_param("graph_name") == Some(graph_name))
            .collect()
    }
    pub fn route_reference_count(&self, graph_name: &str) -> usize {
        self.objects
            .iter()
            .map(|object| {
                usize::from(object.raw_param("graph_name") == Some(graph_name))
                    + match &object.placement {
                        Some(PlacementBinding::Authored(authored)) => authored
                            .dependencies
                            .iter()
                            .map(|dependency| {
                                graph_name_in_record_count(&dependency.record, graph_name)
                            })
                            .sum::<usize>(),
                        _ => 0,
                    }
            })
            .sum()
    }

    pub fn route_assignment_suggestions(&self, object_id: &str) -> Vec<RouteAssignmentSuggestion> {
        let Some(object) = self.objects.iter().find(|object| object.id == object_id) else {
            return Vec::new();
        };
        let Some(routes) = self.route_authoring.as_ref() else {
            return Vec::new();
        };
        let current = object.raw_param("graph_name").unwrap_or("(null)");
        let position = object.transform.translation;
        let mut suggestions = routes
            .graphs
            .iter()
            .map(|graph| {
                let consumers = self.route_consumers(&graph.name);
                RouteAssignmentSuggestion {
                    graph_id: graph.id.clone(),
                    graph_name: graph.name.clone(),
                    current: graph.name == current,
                    same_factory_uses: consumers
                        .iter()
                        .filter(|candidate| candidate.factory_name == object.factory_name)
                        .count(),
                    consumer_count: consumers.len(),
                    nearest_distance: graph
                        .nearest_control_distance(position)
                        .unwrap_or(f32::INFINITY),
                }
            })
            .collect::<Vec<_>>();
        suggestions.sort_by(|left, right| {
            right
                .current
                .cmp(&left.current)
                .then_with(|| right.same_factory_uses.cmp(&left.same_factory_uses))
                .then_with(|| left.nearest_distance.total_cmp(&right.nearest_distance))
                .then_with(|| left.graph_name.cmp(&right.graph_name))
        });
        suggestions
    }

    /// Renames a graph and every exact typed consumer as one transactional mutation.
    pub fn rename_route_graph(&mut self, graph_id: &str, new_name: &str) -> Result<usize> {
        let before_routes = self.route_authoring.clone();
        let before_objects = self.objects.clone();
        let before_edits = self.archive_edits.clone();
        let result = (|| {
            let old_name = self
                .ensure_route_authoring()?
                .graph(graph_id)
                .ok_or_else(|| {
                    SceneError::StageExport(format!("route graph {graph_id:?} was not found"))
                })?
                .name
                .clone();
            self.route_authoring
                .as_mut()
                .expect("route authoring initialized")
                .rename_graph(graph_id, new_name)
                .map_err(|error| SceneError::StageExport(error.to_string()))?;
            let mut changed = 0;
            for object in &mut self.objects {
                if object.raw_param("graph_name") == Some(old_name.as_str()) {
                    object.set_raw_param("graph_name", new_name);
                    changed += 1;
                }
                if let Some(PlacementBinding::Authored(authored)) = &mut object.placement {
                    changed +=
                        rewrite_graph_name_in_record(&mut authored.prototype, &old_name, new_name);
                    for dependency in &mut authored.dependencies {
                        changed += rewrite_graph_name_in_record(
                            &mut dependency.record,
                            &old_name,
                            new_name,
                        );
                    }
                }
            }
            self.compile_route_authoring()?;
            Ok(changed)
        })();
        if result.is_err() {
            self.route_authoring = before_routes;
            self.objects = before_objects;
            self.archive_edits = before_edits;
        }
        result
    }

    pub fn assign_object_route(&mut self, object_id: &str, graph_name: Option<&str>) -> Result<()> {
        let value = graph_name.unwrap_or("(null)");
        if graph_name.is_some() {
            let routes = self.ensure_route_authoring()?;
            if routes.graph_by_name(value).is_none() {
                return Err(SceneError::StageExport(format!(
                    "route graph {value:?} was not found"
                )));
            }
        }
        let object = self
            .objects
            .iter_mut()
            .find(|object| object.id == object_id)
            .ok_or_else(|| {
                SceneError::StageExport(format!("object {object_id:?} was not found"))
            })?;
        object.set_raw_param("graph_name", value);
        Ok(())
    }

    pub fn create_route_from_actor(&mut self, object_id: &str) -> Result<String> {
        let object = self
            .objects
            .iter()
            .find(|object| object.id == object_id)
            .cloned()
            .ok_or_else(|| {
                SceneError::StageExport(format!("object {object_id:?} was not found"))
            })?;
        let routes = self.ensure_route_authoring()?;
        let name = (1u32..)
            .map(|serial| format!("Route{serial:03}"))
            .find(|name| routes.graph_by_name(name).is_none())
            .expect("route name space");
        let yaw = object.transform.rotation_degrees[1].to_radians();
        let first = object.transform.translation.map(|value| {
            value
                .round()
                .clamp(f32::from(i16::MIN), f32::from(i16::MAX)) as i16
        });
        let second = [
            (object.transform.translation[0] + yaw.sin() * 500.0)
                .round()
                .clamp(f32::from(i16::MIN), f32::from(i16::MAX)) as i16,
            first[1],
            (object.transform.translation[2] + yaw.cos() * 500.0)
                .round()
                .clamp(f32::from(i16::MIN), f32::from(i16::MAX)) as i16,
        ];
        let graph_id = routes
            .add_graph(name.clone(), first, second)
            .map_err(|error| SceneError::StageExport(error.to_string()))?;
        self.assign_object_route(object_id, Some(&name))?;
        self.compile_route_authoring()?;
        Ok(graph_id)
    }

    pub fn duplicate_route_and_reassign(&mut self, object_id: &str) -> Result<String> {
        let current_name = self
            .objects
            .iter()
            .find(|object| object.id == object_id)
            .and_then(|object| object.raw_param("graph_name"))
            .filter(|name| !name.is_empty() && *name != "(null)")
            .map(str::to_string)
            .ok_or_else(|| {
                SceneError::StageExport("actor has no route to duplicate".to_string())
            })?;
        let routes = self.ensure_route_authoring()?;
        let source_id = routes
            .graph_by_name(&current_name)
            .map(|graph| graph.id.clone())
            .ok_or_else(|| {
                SceneError::StageExport(format!("route graph {current_name:?} was not found"))
            })?;
        let new_name = (1u32..)
            .map(|serial| format!("{current_name}_copy{serial:02}"))
            .find(|name| routes.graph_by_name(name).is_none())
            .expect("route name space");
        let graph_id = routes
            .duplicate_graph(&source_id, new_name.clone())
            .map_err(|error| SceneError::StageExport(error.to_string()))?;
        self.assign_object_route(object_id, Some(&new_name))?;
        self.compile_route_authoring()?;
        Ok(graph_id)
    }
    #[allow(clippy::empty_line_after_doc_comments)]
    /// Adds a detached semantic resource to the authored archive overlay and
    /// immediately exposes it through the document asset catalog.

    pub fn insert_authored_resource(
        &mut self,
        raw_resource_path: impl Into<Vec<u8>>,
        document: StageResourceDocument,
    ) {
        self.archive_edits
            .insert_resource(raw_resource_path.into(), document);
        self.sync_archive_edit_assets();
    }

    /// Inserts or replaces a detached semantic resource in the authored
    /// archive overlay and immediately exposes it through the asset catalog.
    pub fn upsert_authored_resource(
        &mut self,
        raw_resource_path: impl Into<Vec<u8>>,
        document: StageResourceDocument,
    ) {
        self.archive_edits
            .upsert_resource(raw_resource_path.into(), document);
        self.sync_archive_edit_assets();
    }

    /// Restores the exact generic resource-overlay state for one archive path.
    ///
    /// Object authoring uses this narrow operation for undo/redo so importing
    /// a catalog resource does not require snapshotting the complete archive
    /// edit bundle. The edit and removal positions are intentionally independent:
    /// malformed or forward-compatible project state can therefore be restored
    /// byte-for-byte instead of being silently normalized by undo.
    pub fn set_authored_resource_overlay_state(
        &mut self,
        raw_resource_path: impl Into<Vec<u8>>,
        edit: Option<StageResourceEdit>,
        edit_index: Option<usize>,
        removal_index: Option<usize>,
    ) {
        self.set_authored_resource_overlay_states(std::iter::once((
            raw_resource_path.into(),
            edit,
            edit_index,
            removal_index,
        )));
    }

    /// Restores several generic resource-overlay paths and refreshes the
    /// virtual asset catalog once after the complete transaction.
    pub fn set_authored_resource_overlay_states(
        &mut self,
        states: impl IntoIterator<
            Item = (
                Vec<u8>,
                Option<StageResourceEdit>,
                Option<usize>,
                Option<usize>,
            ),
        >,
    ) {
        let states = states.into_iter().collect::<Vec<_>>();
        for (raw_resource_path, _, _, _) in &states {
            self.archive_edits
                .resources
                .retain(|candidate| candidate.raw_resource_path != *raw_resource_path);
            self.archive_edits
                .resource_removals
                .retain(|candidate| candidate != raw_resource_path);
        }

        let mut resource_edits = states
            .iter()
            .filter_map(|(raw_resource_path, edit, edit_index, _)| {
                edit.clone().map(|mut edit| {
                    edit.raw_resource_path = raw_resource_path.clone();
                    (edit_index.unwrap_or(usize::MAX), edit)
                })
            })
            .collect::<Vec<_>>();
        resource_edits.sort_by_key(|(index, _)| *index);
        for (index, edit) in resource_edits {
            self.archive_edits
                .resources
                .insert(index.min(self.archive_edits.resources.len()), edit);
        }

        let mut removals = states
            .into_iter()
            .filter_map(|(raw_resource_path, _, _, removal_index)| {
                removal_index.map(|index| (index, raw_resource_path))
            })
            .collect::<Vec<_>>();
        removals.sort_by_key(|(index, _)| *index);
        for (index, raw_resource_path) in removals {
            self.archive_edits.resource_removals.insert(
                index.min(self.archive_edits.resource_removals.len()),
                raw_resource_path,
            );
        }
        self.sync_archive_edit_assets();
    }

    /// Returns whether a resource will exist after baseline removals and typed
    /// resource, model, or collision edits are applied.
    pub fn has_effective_resource(&self, raw_resource_path: &[u8]) -> bool {
        let edited = self
            .archive_edits
            .resources
            .iter()
            .any(|edit| edit.raw_resource_path == raw_resource_path)
            || self
                .archive_edits
                .models
                .iter()
                .any(|edit| edit.raw_resource_path == raw_resource_path)
            || self
                .archive_edits
                .collisions
                .iter()
                .any(|edit| edit.raw_resource_path == raw_resource_path);
        if edited {
            return true;
        }
        if self
            .archive_edits
            .resource_removals
            .iter()
            .any(|removed| removed == raw_resource_path)
        {
            return false;
        }
        self.stage_archive
            .as_ref()
            .is_some_and(|archive| archive.resource(raw_resource_path).is_some())
    }

    fn sync_archive_edit_assets(&mut self) {
        let Some(source_path) = self.stage_archive_source_path.as_ref() else {
            return;
        };

        let normalized_path = |raw_path: &[u8]| {
            raw_path
                .iter()
                .map(|byte| match byte {
                    b'\\' => b'/',
                    byte => byte.to_ascii_lowercase(),
                })
                .collect::<Vec<_>>()
        };
        let removed_paths = self
            .archive_edits
            .resource_removals
            .iter()
            .map(|path| normalized_path(path))
            .collect::<BTreeSet<_>>();
        let mut effective_paths = self
            .stage_archive
            .iter()
            .flat_map(|archive| archive.resources())
            .map(|resource| normalized_path(&resource.raw_path))
            .filter(|path| !removed_paths.contains(path))
            .collect::<BTreeSet<_>>();
        effective_paths.extend(
            self.archive_edits
                .resources
                .iter()
                .map(|edit| normalized_path(&edit.raw_resource_path)),
        );
        effective_paths.extend(
            self.archive_edits
                .models
                .iter()
                .map(|edit| normalized_path(&edit.raw_resource_path)),
        );
        effective_paths.extend(
            self.archive_edits
                .collisions
                .iter()
                .map(|edit| normalized_path(&edit.raw_resource_path)),
        );
        self.assets.retain(|asset| {
            semantic_resource_path_for_asset(source_path, &asset.path)
                .is_none_or(|raw_path| effective_paths.contains(&normalized_path(&raw_path)))
        });

        let mut edited_paths = self
            .archive_edits
            .resources
            .iter()
            .map(|edit| edit.raw_resource_path.as_slice())
            .chain(
                self.archive_edits
                    .models
                    .iter()
                    .map(|edit| edit.raw_resource_path.as_slice()),
            )
            .chain(
                self.archive_edits
                    .collisions
                    .iter()
                    .map(|edit| edit.raw_resource_path.as_slice()),
            )
            .collect::<Vec<_>>();
        edited_paths.sort();
        edited_paths.dedup();
        for raw_path in edited_paths {
            let path = PathBuf::from(format!(
                "{}!/{}",
                source_path.display(),
                String::from_utf8_lossy(raw_path)
            ));
            if !self.assets.iter().any(|asset| asset.path == path) {
                self.assets.push(StageAsset {
                    path,
                    kind: semantic_stage_asset_kind(raw_path),
                });
            }
        }
        self.assets
            .sort_by(|left, right| left.path.cmp(&right.path));
    }

    pub fn with_registry(mut self, registry: ObjectRegistry) -> Self {
        self.set_registry(registry);
        self
    }

    pub fn set_registry(&mut self, registry: ObjectRegistry) {
        let (actor_previews, preview_issues) =
            build_actor_preview_catalog(&self.base_root, &self.assets, &registry, |path| {
                self.read_asset_bytes(path)
                    .map_err(|error| error.to_string())
            });
        let object_preview_issues =
            apply_registry_preview_hints(&mut self.objects, &self.assets, &registry);
        refresh_object_manager_capacity_dependencies(&mut self.objects, &registry);
        self.actor_previews = actor_previews;
        self.load_issues.retain(|issue| {
            !issue.code.starts_with("enemy-preview-") && !issue.code.starts_with("object-preview-")
        });
        self.load_issues.extend(preview_issues);
        self.load_issues.extend(object_preview_issues);
        self.registry = Some(registry);
    }

    /// Applies only registry-derived preview metadata to an object baseline.
    ///
    /// The editor uses this when a schema refresh completes so derived preview
    /// hints do not make an otherwise unchanged document appear dirty.
    pub fn refresh_registry_derived_object_fields(
        &self,
        objects: &mut [SceneObject],
        registry: &ObjectRegistry,
    ) {
        let _ = apply_registry_preview_hints(objects, &self.assets, registry);
        refresh_object_manager_capacity_dependencies(objects, registry);
    }

    pub fn actor_preview(&self, object: &SceneObject) -> Option<&ActorPreview> {
        object
            .source
            .as_ref()
            .and_then(actor_preview_source_key)
            .and_then(|key| self.actor_previews.get(&key))
            .or_else(|| {
                self.actor_previews
                    .get(&actor_preview_factory_key(&object.factory_name))
            })
    }

    /// Returns the exact decomp-authored model-loader flags for a typed object placement.
    ///
    /// These flags are instance data for `TMapObjBase` and actor-table data for
    /// `TMapStaticObj`; a model-path heuristic cannot distinguish those cases.
    pub fn object_preview_load_flags(&self, object: &SceneObject) -> Option<u32> {
        let registry = self.registry.as_ref()?;
        if let Some(named_model) = object
            .raw_param("name")
            .and_then(|name| registry.find_named_object_model(&object.factory_name, name))
        {
            return Some(named_model.load_flags);
        }
        let resource_name = object.raw_param("actor_tail_string")?;
        if registry.is_map_obj_factory(&object.factory_name) {
            let resource = registry.find_map_obj_resource(resource_name)?;
            return Some(
                registry
                    .find_map_obj_model_override(&object.factory_name, resource_name)
                    .map_or(resource.load_flags, |model_override| {
                        model_override.load_flags
                    }),
            );
        }
        let is_map_static = registry
            .find_object(&object.factory_name)
            .is_some_and(|definition| definition.class_name == "TMapStaticObj");
        is_map_static.then(|| {
            registry
                .map_static_models
                .iter()
                .find(|definition| definition.actor_name == resource_name)
                .map(|definition| definition.load_flags)
        })?
    }

    pub fn add_object(&mut self, object: SceneObject) {
        self.objects.push(object);
    }

    pub fn mark_changed_file(
        &mut self,
        relative_path: impl Into<PathBuf>,
        bytes: Vec<u8>,
    ) -> Result<()> {
        let relative_path = relative_path.into();
        validate_project_relative_path(&relative_path)?;
        self.changed_files.insert(relative_path, bytes);
        Ok(())
    }

    pub fn queue_editor_overlay_change(&mut self) -> Result<()> {
        let path = self.editor_overlay_path()?;
        let overlay = EditorSceneOverlay {
            stage_id: self.stage_id.clone(),
            objects: self.objects.clone(),
            archive_edits: self.archive_edits.clone(),
            lighting: Some(self.lighting.clone()),
            route_authoring: self.route_authoring.clone(),
            goop_authoring: self.goop_authoring.clone(),
            dialogue_authoring: self.dialogue_authoring.clone(),
        };
        let bytes = serde_json::to_vec_pretty(&overlay)?;
        self.mark_changed_file(path, bytes)?;
        Ok(())
    }

    pub fn authored_stage_baseline_path(&self) -> Result<PathBuf> {
        validate_stage_id(&self.stage_id)?;
        Ok(PathBuf::from("editor")
            .join("stages")
            .join(format!("{}.stage.json", self.stage_id)))
    }

    pub fn dialogue_library_path(&self) -> PathBuf {
        PathBuf::from(PROJECT_DIALOGUE_LIBRARY_PATH)
    }

    fn queue_dialogue_library_change(&mut self) -> Result<()> {
        if self.dialogue_library.is_empty() {
            self.changed_files.remove(&self.dialogue_library_path());
            return Ok(());
        }
        let bytes = serde_json::to_vec_pretty(&self.dialogue_library)?;
        self.mark_changed_file(self.dialogue_library_path(), bytes)
    }

    fn queue_authored_stage_baseline_change(&mut self) -> Result<()> {
        let Some(archive) = self.stage_archive.as_ref() else {
            return Ok(());
        };
        if !matches!(archive.origin(), StageOrigin::Blank { .. }) {
            return Ok(());
        }
        validate_authored_archive_target(archive, &self.stage_id)?;
        let path = self.authored_stage_baseline_path()?;
        let bytes = archive.to_semantic_json()?;
        self.mark_changed_file(path, bytes)
    }

    pub fn editor_overlay_path(&self) -> Result<PathBuf> {
        validate_stage_id(&self.stage_id)?;
        Ok(PathBuf::from("editor")
            .join("stages")
            .join(format!("{}.scene.json", self.stage_id)))
    }

    pub fn save_project_folder(
        &mut self,
        project_root: impl AsRef<Path>,
    ) -> Result<ProjectSaveOutcome> {
        self.queue_editor_overlay_change()?;
        self.queue_dialogue_library_change()?;
        self.queue_authored_stage_baseline_change()?;
        let project_root = project_root.as_ref();
        let loaded_root = if project_root.is_absolute() {
            project_root.to_path_buf()
        } else {
            std::env::current_dir()?.join(project_root)
        };
        let (outcome, project_fingerprint) =
            project_store::save_project_folder(self, project_root)?;
        self.loaded_project = Some(LoadedProjectState {
            project_root: loaded_root,
            project_id: outcome.manifest.project_id.clone(),
            revision: outcome.manifest.revision,
            project_fingerprint,
        });
        Ok(outcome)
    }

    pub fn load_project_folder(&mut self, project_root: impl AsRef<Path>) -> Result<bool> {
        project_store::load_project_overlay(self, project_root.as_ref())
    }

    pub fn project_root_overlaps_base(&self, project_root: impl AsRef<Path>) -> Result<bool> {
        project_store::project_root_overlaps_base(self, project_root.as_ref())
    }

    pub fn validate_for_export(&self) -> Result<()> {
        let errors: Vec<_> = self
            .validate()
            .into_iter()
            .filter(|issue| issue.severity == ValidationSeverity::Error)
            .map(|issue| format!("{}: {}", issue.code, issue.message))
            .collect();
        if errors.is_empty() {
            Ok(())
        } else {
            Err(SceneError::ValidationFailed(errors.join("; ")))
        }
    }

    pub fn validate(&self) -> Vec<ValidationIssue> {
        validation::validate_document(self)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EditorSceneOverlay {
    pub stage_id: String,
    pub objects: Vec<SceneObject>,
    #[serde(default)]
    pub archive_edits: StageArchiveEdits,
    /// Ordered typed `AmbAry`/`LightAry` state authored in the scene.
    ///
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub route_authoring: Option<RouteAuthoringDocument>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub goop_authoring: Option<GoopAuthoringDocument>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dialogue_authoring: Option<DialogueAuthoringDocument>,
    /// `None` preserves compatibility with older overlays, whose lighting is
    /// inherited from the semantic stage baseline.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lighting: Option<StageLighting>,
}

pub const OBJECT_AUTHORING_DEFAULTS_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SceneObject {
    pub id: String,
    pub source: Option<SourceLocation>,
    /// Source-free address of this actor in the JDrama group tree.
    ///
    /// Existing records are edited in place. `CloneOf` retains only the typed
    /// template address so export can clone the semantic record rather than
    /// copying its original bytes. Authored placements own a fully typed prototype and
    /// the support records needed to insert it without a retail source record.
    #[serde(default)]
    pub placement: Option<PlacementBinding>,
    pub factory_name: String,
    pub class_name: Option<String>,
    pub transform: Transform,
    pub raw_params: BTreeMap<String, SceneParameter>,
    pub asset_hints: Vec<AssetRef>,
    /// Exact managers selected through structural fields that do not carry a
    /// direct `manager_name` reference (for example, a map-object resource).
    ///
    /// This decomp-derived closure is persisted so headless project export can
    /// retain clone capacity requirements without regenerating the registry.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub manager_capacity_dependencies: Vec<String>,
    /// Editor-selected actors that satisfy fixed runtime name lookups.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub runtime_references: Vec<SceneRuntimeReferenceBinding>,
    /// Version of editor-owned safe defaults applied to an authored record.
    ///
    /// Retail/imported objects remain zero. Missing values in older project
    /// overlays also deserialize as zero so narrowly scoped migrations can
    /// distinguish legacy authored objects from deliberate current settings.
    #[serde(default, skip_serializing_if = "is_zero_u32")]
    pub authoring_defaults_version: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SceneRuntimeReferenceBinding {
    pub required_factory_name: String,
    pub runtime_name: String,
    #[serde(
        default = "runtime_reference_required_default",
        skip_serializing_if = "is_true"
    )]
    pub required: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_object_id: Option<String>,
}

fn runtime_reference_required_default() -> bool {
    true
}

fn is_true(value: &bool) -> bool {
    *value
}

fn is_zero_u32(value: &u32) -> bool {
    *value == 0
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct PlacementAddress {
    pub raw_resource_path: Vec<u8>,
    pub record_path: Vec<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AuthoredPlacementDependencyTarget {
    IndexedGroup { group_index: u32 },
    NamedGroup { type_name: String, name: String },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AuthoredPlacementDependency {
    /// Exact semantic parent container selected from the retail source.
    ///
    /// Older project overlays only persisted `target_group_index`; a missing
    /// target therefore retains the original IdxGroup behavior.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<AuthoredPlacementDependencyTarget>,
    pub target_group_index: u32,
    pub record: JDramaRecord,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AuthoredPlacement {
    pub raw_resource_path: Vec<u8>,
    pub target_group_index: u32,
    pub prototype: JDramaRecord,
    #[serde(default)]
    pub dependencies: Vec<AuthoredPlacementDependency>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "address", rename_all = "snake_case")]
pub enum PlacementBinding {
    Existing(PlacementAddress),
    CloneOf(PlacementAddress),
    Authored(AuthoredPlacement),
}

impl PlacementBinding {
    pub fn source_address(&self) -> Option<&PlacementAddress> {
        match self {
            Self::Existing(address) | Self::CloneOf(address) => Some(address),
            Self::Authored(_) => None,
        }
    }

    pub fn raw_resource_path(&self) -> &[u8] {
        match self {
            Self::Existing(address) | Self::CloneOf(address) => &address.raw_resource_path,
            Self::Authored(authored) => &authored.raw_resource_path,
        }
    }

    pub fn duplicate_for_new_object(&self) -> Self {
        match self {
            Self::Existing(address) | Self::CloneOf(address) => Self::CloneOf(address.clone()),
            Self::Authored(authored) => Self::Authored(authored.clone()),
        }
    }
}
fn graph_name_in_record_count(record: &JDramaRecord, graph_name: &str) -> usize {
    let fields = match &record.payload {
        JDramaRecordPayload::Actor { fields, .. }
        | JDramaRecordPayload::Fields { fields }
        | JDramaRecordPayload::Group { fields, .. } => fields,
        JDramaRecordPayload::Empty => return 0,
    };
    let own = fields
        .iter()
        .filter(|field| {
            field.name == "graph_name"
                && matches!(&field.value, JDramaFieldValue::String(value) if value == graph_name)
        })
        .count();
    own + match &record.payload {
        JDramaRecordPayload::Group { children, .. } => children
            .iter()
            .map(|child| graph_name_in_record_count(child, graph_name))
            .sum(),
        _ => 0,
    }
}

fn rewrite_graph_name_in_record(
    record: &mut JDramaRecord,
    old_name: &str,
    new_name: &str,
) -> usize {
    let fields = match &mut record.payload {
        JDramaRecordPayload::Actor { fields, .. }
        | JDramaRecordPayload::Fields { fields }
        | JDramaRecordPayload::Group { fields, .. } => fields,
        JDramaRecordPayload::Empty => return 0,
    };
    let mut changed = 0;
    for field in fields.iter_mut().filter(|field| field.name == "graph_name") {
        if let JDramaFieldValue::String(value) = &mut field.value {
            if value == old_name {
                *value = new_name.to_string();
                changed += 1;
            }
        }
    }
    if let JDramaRecordPayload::Group { children, .. } = &mut record.payload {
        changed += children
            .iter_mut()
            .map(|child| rewrite_graph_name_in_record(child, old_name, new_name))
            .sum::<usize>();
    }
    changed
}

impl SceneObject {
    pub fn new(id: impl Into<String>, factory_name: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            source: None,
            placement: None,
            factory_name: factory_name.into(),
            class_name: None,
            transform: Transform::default(),
            raw_params: BTreeMap::new(),
            asset_hints: Vec::new(),
            manager_capacity_dependencies: Vec::new(),
            authoring_defaults_version: 0,
            runtime_references: Vec::new(),
        }
    }

    pub fn insert_source_raw_param(
        &mut self,
        key: impl Into<String>,
        raw_value: impl Into<String>,
    ) {
        self.raw_params
            .insert(key.into(), SceneParameter::from_source(raw_value));
    }

    pub fn set_raw_param(&mut self, key: impl Into<String>, raw_value: impl Into<String>) {
        self.raw_params
            .insert(key.into(), SceneParameter::edited(raw_value, None));
    }

    pub fn set_decoded_param(
        &mut self,
        key: impl Into<String>,
        raw_value: impl Into<String>,
        decoded_value: ParamValue,
    ) {
        self.raw_params.insert(
            key.into(),
            SceneParameter::edited(raw_value, Some(decoded_value)),
        );
    }

    pub fn raw_param(&self, key: &str) -> Option<&str> {
        self.raw_params.get(key).map(SceneParameter::raw)
    }

    /// Refreshes the source-derived manager closure persisted for headless
    /// export. Structural selector parameters are read-only, so duplicating
    /// this object can safely retain the refreshed exact names.
    pub fn refresh_manager_capacity_dependencies(&mut self, registry: &ObjectRegistry) {
        self.manager_capacity_dependencies =
            derived_object_manager_capacity_dependencies(self, registry)
                .into_iter()
                .collect();
    }
}

fn derived_object_manager_capacity_dependencies(
    object: &SceneObject,
    registry: &ObjectRegistry,
) -> BTreeSet<String> {
    let mut manager_names = object
        .raw_param("launched_enemy_name")
        .filter(|name| !name.is_empty())
        .map(str::to_owned)
        .into_iter()
        .collect::<BTreeSet<_>>();
    if let Some(manager_name) = object
        .raw_param("resource_name")
        .and_then(|resource_name| registry.find_map_obj_resource(resource_name))
        .map(|resource| resource.required_manager_name.as_str())
        .filter(|name| !name.is_empty())
    {
        manager_names.insert(manager_name.to_string());
    }
    manager_names
}

fn refresh_object_manager_capacity_dependencies(
    objects: &mut [SceneObject],
    registry: &ObjectRegistry,
) {
    for object in objects {
        object.refresh_manager_capacity_dependencies(registry);
    }
}

pub(crate) fn resolved_object_manager_capacity_dependencies(
    object: &SceneObject,
    registry: Option<&ObjectRegistry>,
) -> BTreeSet<String> {
    let mut manager_names = object
        .manager_capacity_dependencies
        .iter()
        .filter(|name| !name.is_empty())
        .cloned()
        .collect::<BTreeSet<_>>();
    if let Some(registry) = registry {
        manager_names.extend(derived_object_manager_capacity_dependencies(
            object, registry,
        ));
    }
    manager_names
}

#[derive(Debug, Clone)]
pub struct SceneParameter {
    raw_value: String,
    decoded_value: Option<ParamValue>,
    dirty: bool,
}

impl PartialEq for SceneParameter {
    fn eq(&self, other: &Self) -> bool {
        self.raw_value == other.raw_value
    }
}

impl Eq for SceneParameter {}

impl SceneParameter {
    pub fn from_source(raw_value: impl Into<String>) -> Self {
        Self {
            raw_value: raw_value.into(),
            decoded_value: None,
            dirty: false,
        }
    }

    pub fn edited(raw_value: impl Into<String>, decoded_value: Option<ParamValue>) -> Self {
        Self {
            raw_value: raw_value.into(),
            decoded_value,
            dirty: true,
        }
    }

    pub fn raw(&self) -> &str {
        &self.raw_value
    }

    pub fn decoded(&self) -> Option<&ParamValue> {
        self.decoded_value.as_ref()
    }

    pub fn is_dirty(&self) -> bool {
        self.dirty
    }
}

impl Deref for SceneParameter {
    type Target = str;

    fn deref(&self) -> &Self::Target {
        self.raw()
    }
}

impl AsRef<str> for SceneParameter {
    fn as_ref(&self) -> &str {
        self.raw()
    }
}

impl PartialEq<str> for SceneParameter {
    fn eq(&self, other: &str) -> bool {
        self.raw() == other
    }
}

impl PartialEq<&str> for SceneParameter {
    fn eq(&self, other: &&str) -> bool {
        self.raw() == *other
    }
}

impl fmt::Display for SceneParameter {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.raw())
    }
}

impl From<String> for SceneParameter {
    fn from(raw_value: String) -> Self {
        Self::edited(raw_value, None)
    }
}

impl From<&str> for SceneParameter {
    fn from(raw_value: &str) -> Self {
        Self::edited(raw_value, None)
    }
}

impl Serialize for SceneParameter {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(self.raw())
    }
}

impl<'de> Deserialize<'de> for SceneParameter {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        String::deserialize(deserializer).map(Self::from_source)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Transform {
    pub translation: [f32; 3],
    pub rotation_degrees: [f32; 3],
    pub scale: [f32; 3],
}

impl Default for Transform {
    fn default() -> Self {
        Self {
            translation: [0.0, 0.0, 0.0],
            rotation_degrees: [0.0, 0.0, 0.0],
            scale: [1.0, 1.0, 1.0],
        }
    }
}

impl Transform {
    pub fn is_finite(&self) -> bool {
        self.translation
            .iter()
            .chain(self.rotation_degrees.iter())
            .chain(self.scale.iter())
            .all(|value| value.is_finite())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", content = "value")]
pub enum ParamValue {
    Bool(bool),
    Int(i64),
    Float(f32),
    String(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AssetRef {
    pub path: String,
    pub role: AssetRole,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum AssetRole {
    PreviewModel,
    InferredPreviewModel,
    Collision,
    Texture,
    Animation,
    Script,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ValidationIssue {
    pub severity: ValidationSeverity,
    pub code: String,
    pub message: String,
}

impl ValidationIssue {
    pub fn warning(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            severity: ValidationSeverity::Warning,
            code: code.into(),
            message: message.into(),
        }
    }

    pub fn error(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            severity: ValidationSeverity::Error,
            code: code.into(),
            message: message.into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ValidationSeverity {
    Info,
    Warning,
    Error,
}

fn validate_stage_id(stage_id: &str) -> Result<()> {
    let valid = !stage_id.is_empty()
        && stage_id
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-' | '.'))
        && stage_id != "."
        && stage_id != ".."
        && is_portable_project_component(OsStr::new(stage_id));
    if valid {
        Ok(())
    } else {
        Err(SceneError::InvalidStageId(stage_id.to_string()))
    }
}

fn validate_project_relative_path(path: &Path) -> Result<()> {
    let valid = !path.as_os_str().is_empty()
        && path.components().all(|component| match component {
            Component::Normal(name) => is_portable_project_component(name),
            _ => false,
        });
    if valid {
        Ok(())
    } else {
        Err(SceneError::UnsafeProjectPath(path.to_path_buf()))
    }
}

fn is_portable_project_component(component: &OsStr) -> bool {
    let Some(component) = component.to_str() else {
        return false;
    };
    if component.is_empty()
        || component.ends_with(['.', ' '])
        || component.chars().any(|ch| {
            ch <= '\u{1f}' || matches!(ch, '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*')
        })
    {
        return false;
    }

    let basename = component.split('.').next().unwrap_or_default();
    let uppercase = basename.to_ascii_uppercase();
    if matches!(
        uppercase.as_str(),
        "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$"
    ) {
        return false;
    }
    let numbered_device = uppercase
        .strip_prefix("COM")
        .or_else(|| uppercase.strip_prefix("LPT"));
    !numbered_device
        .is_some_and(|suffix| matches!(suffix, "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9"))
}

fn validate_authored_archive_target(
    archive: &SourceFreeStageArchive,
    stage_id: &str,
) -> Result<()> {
    match archive.origin() {
        StageOrigin::Blank { target_slot, .. } if target_slot.eq_ignore_ascii_case(stage_id) => {
            Ok(())
        }
        StageOrigin::Blank { target_slot, .. } => Err(SceneError::StageExport(format!(
            "authored stage target '{target_slot}' does not match document stage '{stage_id}'"
        ))),
        StageOrigin::ImportedArchive => Err(SceneError::StageExport(format!(
            "stage '{stage_id}' requires a blank authored archive origin"
        ))),
    }
}

/// Returns the virtual game-relative archive identity for an authored stage.
/// The path is not created and may intentionally not exist in the extracted
/// retail base.
pub fn authored_stage_virtual_source_path(base_root: &Path, stage_id: &str) -> Result<PathBuf> {
    validate_stage_id(stage_id)?;
    Ok(base_root
        .join("files")
        .join("data")
        .join("scene")
        .join(format!("{stage_id}.szs")))
}

/// Lists validated authored stage ids persisted in a managed project.
/// Retail stages without a semantic authored baseline are not returned.
pub fn discover_authored_project_stage_ids(project_root: impl AsRef<Path>) -> Result<Vec<String>> {
    project_store::discover_authored_stage_ids(project_root.as_ref())
}

fn authored_stage_assets(
    base_root: &Path,
    source_path: &Path,
    archive: &SourceFreeStageArchive,
) -> Result<Vec<StageAsset>> {
    let mut assets = scan_common_stage_assets(base_root)?;
    assets.extend(archive.resources().iter().map(|resource| StageAsset {
        path: PathBuf::from(format!(
            "{}!/{}",
            source_path.display(),
            String::from_utf8_lossy(&resource.raw_path)
        )),
        kind: semantic_stage_asset_kind(&resource.raw_path),
    }));
    assets.sort_by(|left, right| left.path.cmp(&right.path));
    assets.dedup_by(|left, right| left.path == right.path);
    Ok(assets)
}

fn semantic_stage_asset_kind(raw_path: &[u8]) -> StageAssetKind {
    let path = String::from_utf8_lossy(raw_path);
    match Path::new(path.as_ref())
        .extension()
        .and_then(OsStr::to_str)
        .unwrap_or_default()
        .to_ascii_lowercase()
        .as_str()
    {
        "arc" | "szs" => StageAssetKind::Archive,
        "bmd" | "bdl" => StageAssetKind::Model,
        "bmt" => StageAssetKind::MaterialTable,
        "bti" | "bmp" => StageAssetKind::Texture,
        "col" => StageAssetKind::Collision,
        "bmg" => StageAssetKind::Message,
        "jpa" | "jpc" => StageAssetKind::Particle,
        "bck" | "btp" | "btk" | "brk" | "bas" => StageAssetKind::Animation,
        "bin" | "prm" | "map" => StageAssetKind::Placement,
        _ => StageAssetKind::Other,
    }
}

fn read_semantic_stage_asset_bytes(
    archive: &SourceFreeStageArchive,
    source_path: &Path,
    path: &Path,
) -> Result<Vec<u8>> {
    if let Some(bytes) = read_matching_semantic_stage_asset(archive, source_path, path)? {
        Ok(bytes)
    } else {
        Ok(read_stage_asset_bytes(path)?)
    }
}

fn read_matching_semantic_stage_asset(
    archive: &SourceFreeStageArchive,
    source_path: &Path,
    path: &Path,
) -> Result<Option<Vec<u8>>> {
    let Some(raw_resource_path) = semantic_resource_path_for_asset(source_path, path) else {
        return Ok(None);
    };
    archive
        .resource_bytes(&raw_resource_path)?
        .map(Some)
        .ok_or_else(|| {
            SceneError::StageExport(format!(
                "semantic stage resource '{}' is not present in the authored archive",
                String::from_utf8_lossy(&raw_resource_path)
            ))
        })
}

fn semantic_resource_path_for_asset(source_path: &Path, path: &Path) -> Option<Vec<u8>> {
    let path_text = path.to_string_lossy();
    let (archive_path, resource_path) = path_text.split_once("!/")?;
    if !stage_archive_paths_match(Path::new(archive_path), source_path) {
        return None;
    }
    Some(
        resource_path
            .replace('\\', "/")
            .trim_start_matches('/')
            .as_bytes()
            .to_vec(),
    )
}

fn stage_archive_paths_match(left: &Path, right: &Path) -> bool {
    match (fs::canonicalize(left), fs::canonicalize(right)) {
        (Ok(left), Ok(right)) => left == right,
        _ => left
            .to_string_lossy()
            .replace('\\', "/")
            .trim_end_matches('/')
            .eq_ignore_ascii_case(
                right
                    .to_string_lossy()
                    .replace('\\', "/")
                    .trim_end_matches('/'),
            ),
    }
}

fn load_scene_objects_from_assets(
    assets: &[StageAsset],
) -> (Vec<SceneObject>, Vec<ValidationIssue>, StageLighting) {
    load_scene_objects_from_assets_with_reader(assets, |path| Ok(read_stage_asset_bytes(path)?))
}

fn load_scene_objects_from_assets_with_reader(
    assets: &[StageAsset],
    mut read_asset: impl FnMut(&Path) -> Result<Vec<u8>>,
) -> (Vec<SceneObject>, Vec<ValidationIssue>, StageLighting) {
    let mut objects = Vec::new();
    let mut issues = Vec::new();
    let mut lighting = StageLighting::default();
    let model_index = stage_model_index(assets);
    let mut placement_assets: Vec<_> = assets
        .iter()
        .filter(|asset| asset.kind == StageAssetKind::Placement)
        .filter(|asset| {
            asset
                .path
                .to_string_lossy()
                .replace('\\', "/")
                .to_ascii_lowercase()
                .ends_with("/map/scene.bin")
        })
        .collect();
    placement_assets.sort_by(|left, right| {
        let left_virtual = left.path.to_string_lossy().contains("!/");
        let right_virtual = right.path.to_string_lossy().contains("!/");
        left_virtual
            .cmp(&right_virtual)
            .then_with(|| left.path.cmp(&right.path))
    });

    if placement_assets.len() > 1 {
        issues.push(ValidationIssue::warning(
            "multiple-placement-files",
            format!(
                "Found {} map/scene.bin assets; using canonical source {} and ignoring duplicates",
                placement_assets.len(),
                placement_assets[0].path.display()
            ),
        ));
    }

    let placement_files = placement_assets.len();
    for asset in placement_assets.into_iter().take(1) {
        let path_text = asset.path.to_string_lossy().replace('\\', "/");
        let asset_identity = stable_path_identity(&path_text);

        let bytes = match read_asset(&asset.path) {
            Ok(bytes) => bytes,
            Err(err) => {
                issues.push(ValidationIssue::error(
                    "placement-read-failed",
                    format!("Could not read {}: {err}", asset.path.display()),
                ));
                continue;
            }
        };
        let records = match parse_jdrama_object_records(&bytes) {
            Ok(records) => records,
            Err(err) => {
                issues.push(ValidationIssue::error(
                    "placement-parse-failed",
                    format!("Could not parse {}: {err}", asset.path.display()),
                ));
                continue;
            }
        };
        let semantic_placement = JDramaDocument::parse(&bytes).ok();

        for record in records {
            if let Some(light) = record.light.clone() {
                lighting.lights.push(light);
            }
            if let Some(ambient) = record.ambient.clone() {
                lighting.ambients.push(ambient);
            }
            let Some(transform) = record.transform else {
                continue;
            };

            let type_name = record.type_name.clone();
            let object_name = record
                .object_name
                .clone()
                .unwrap_or_else(|| type_name.clone());
            let mut object = SceneObject::new(
                format!("retail-{asset_identity:016x}-{:08x}", record.offset),
                type_name.clone(),
            );
            object.source = Some(SourceLocation {
                path: asset.path.clone(),
                offset: Some(record.offset as u64),
                length: Some(record.size as u64),
            });
            object.placement = archive_resource_path(&asset.path).map(|raw_resource_path| {
                PlacementBinding::Existing(PlacementAddress {
                    raw_resource_path,
                    record_path: record.record_path.clone(),
                })
            });
            object.class_name = Some(type_name);
            object.transform = Transform {
                translation: transform.translation,
                rotation_degrees: transform.rotation,
                scale: transform.scale,
            };
            object.insert_source_raw_param("name", object_name.clone());
            for (index, value) in record.stream_strings.iter().enumerate() {
                object.insert_source_raw_param(format!("stream_string_{index}"), value.clone());
            }
            if let Some(value) = record.actor_tail_string.as_ref() {
                // The common TActor stream ends before this field. For
                // TMapObjBase-derived actors the first subclass string is the
                // exact resource identity consumed by initMapObj (for example
                // FruitPapaya or SandBombBasePyramid). Keep it distinct from
                // the actor character/light-map strings above so callers do
                // not mistake a shared character entry for the model selector.
                object.insert_source_raw_param("actor_tail_string", value.clone());
            }
            if let Some(value) = record.nozzle_box_item.as_ref() {
                object.insert_source_raw_param("nozzle_box_item", value.clone());
            }
            if let Some(params) = record.npc_params {
                object.insert_source_raw_param(
                    "npc_body_color_index",
                    params.color_indices[0].to_string(),
                );
                object.insert_source_raw_param(
                    "npc_cloth_color_index",
                    params.color_indices[1].to_string(),
                );
                object.insert_source_raw_param(
                    "npc_pollution_amount",
                    params.pollution_amount.to_string(),
                );
                object.insert_source_raw_param("npc_parts_mask", params.parts_mask.to_string());
                for (index, value) in params.parts_color_indices.into_iter().enumerate() {
                    object.insert_source_raw_param(
                        format!("npc_parts_color_index_{index}"),
                        value.to_string(),
                    );
                }
                object.insert_source_raw_param("npc_action_flags", params.action_flags.to_string());
            }
            if let Some(blade_count) = record.map_obj_grass_blade_count {
                object.insert_source_raw_param("grass_blade_count", blade_count.to_string());
            }
            if let Some(semantic_record) = semantic_placement
                .as_ref()
                .and_then(|document| jdrama_record_at(&document.root, &record.record_path))
            {
                insert_typed_jdrama_params(&mut object, semantic_record);
            }

            if let Some(model_path) = infer_preview_model_path(&object, &model_index) {
                object.asset_hints.push(AssetRef {
                    path: model_path,
                    role: AssetRole::InferredPreviewModel,
                });
            }

            objects.push(object);
        }
    }

    if placement_files == 0 {
        issues.push(ValidationIssue::warning(
            "missing-placement-file",
            "No map/scene.bin placement file was found for this stage",
        ));
    }

    (objects, issues, lighting)
}

fn jdrama_record_at<'a>(mut record: &'a JDramaRecord, path: &[usize]) -> Option<&'a JDramaRecord> {
    for index in path {
        let JDramaRecordPayload::Group { children, .. } = &record.payload else {
            return None;
        };
        record = children.get(*index)?;
    }
    Some(record)
}

fn insert_typed_jdrama_params(object: &mut SceneObject, record: &JDramaRecord) {
    let fields: &[JDramaField] = match &record.payload {
        JDramaRecordPayload::Actor { fields, .. }
        | JDramaRecordPayload::Fields { fields }
        | JDramaRecordPayload::Group { fields, .. } => fields,
        JDramaRecordPayload::Empty => &[],
    };
    for field in fields {
        let raw_value = match &field.value {
            JDramaFieldValue::U32(value) => value.to_string(),
            JDramaFieldValue::I32(value) => value.to_string(),
            JDramaFieldValue::F32(value) => value.to_string(),
            JDramaFieldValue::Vec2F32(value) => format!("{},{}", value[0], value[1]),
            JDramaFieldValue::Vec3F32(value) => {
                format!("{},{},{}", value[0], value[1], value[2])
            }
            JDramaFieldValue::ColorRgba8(value) => {
                format!("{},{},{},{}", value[0], value[1], value[2], value[3])
            }
            JDramaFieldValue::String(value) => value.clone(),
            JDramaFieldValue::LightMap(_) => continue,
        };
        object.insert_source_raw_param(field.name.clone(), raw_value);
    }
}

fn archive_resource_path(path: &Path) -> Option<Vec<u8>> {
    path.to_string_lossy()
        .split_once("!/")
        .map(|(_, internal_path)| internal_path.as_bytes().to_vec())
}

fn stable_path_identity(path: &str) -> u64 {
    const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;
    path.replace('\\', "/")
        .bytes()
        .fold(FNV_OFFSET, |hash, byte| {
            (hash ^ u64::from(byte)).wrapping_mul(FNV_PRIME)
        })
}

fn stage_model_index(assets: &[StageAsset]) -> Vec<(String, String)> {
    let mut models: Vec<_> = assets
        .iter()
        .filter(|asset| asset.kind == StageAssetKind::Model)
        .filter_map(|asset| {
            let path = asset.path.to_string_lossy().replace('\\', "/");
            let normalized = normalize_model_key(&path);
            (!normalized.is_empty()).then_some((path, normalized))
        })
        .collect();
    models.sort_by(|a, b| a.0.cmp(&b.0));
    models
}

fn apply_registry_preview_hints(
    objects: &mut [SceneObject],
    assets: &[StageAsset],
    registry: &ObjectRegistry,
) -> Vec<ValidationIssue> {
    let model_index = stage_model_index(assets);
    let mut issues = Vec::new();

    for object in objects {
        // Registry refreshes operate on a fresh placement-derived baseline so a
        // binding from registry A cannot survive after switching to registry B.
        object
            .asset_hints
            .retain(|hint| hint.role != AssetRole::InferredPreviewModel);
        if let Some(path) = infer_preview_model_path(object, &model_index) {
            object.asset_hints.push(AssetRef {
                path,
                role: AssetRole::InferredPreviewModel,
            });
        }
        let resource_name = object.raw_param("actor_tail_string").map(str::to_string);
        let object_definition = registry.find_object(&object.factory_name);
        if let Some(definition) = object_definition {
            // Class names are schema provenance rather than serialized stage
            // data. Refresh them alongside the other registry-derived hints
            // so a newer decomp extractor can repair old "Unknown" labels
            // without rewriting the placement record.
            object.class_name = Some(definition.class_name.clone());
        }
        let is_map_static =
            object_definition.is_some_and(|definition| definition.class_name == "TMapStaticObj");
        let is_map_obj = registry.is_map_obj_factory(&object.factory_name);
        let map_obj_resource = if is_map_obj {
            resource_name
                .as_deref()
                .and_then(|resource_name| registry.find_map_obj_resource(resource_name))
        } else {
            None
        };
        if let Some(resource) = map_obj_resource {
            // `TMapObjData` is the authored instance selector and outranks every
            // factory-wide hint. Derived class overrides may replace a zero-actor
            // table entry with a custom/shared model before it is treated as model-less.
            object
                .asset_hints
                .retain(|hint| hint.role != AssetRole::InferredPreviewModel);
            let model_override =
                registry.find_map_obj_model_override(&object.factory_name, &resource.resource_name);
            let authored = model_override
                .map(|definition| {
                    (
                        definition.model_path.as_str(),
                        format!(
                            "{} and {}",
                            definition.binding_source_file, definition.model_source_file
                        ),
                    )
                })
                .or_else(|| {
                    resource
                        .primary_model
                        .as_deref()
                        .map(|model| (model, resource.source_file.clone()))
                });
            let Some((primary_model, source)) = authored else {
                continue;
            };
            match resolve_authored_model_path(primary_model, &model_index) {
                ModelPathResolution::Found(path) => object.asset_hints.push(AssetRef {
                    path,
                    role: AssetRole::InferredPreviewModel,
                }),
                ModelPathResolution::Missing => issues.push(ValidationIssue::warning(
                    "object-preview-model-unresolved",
                    format!(
                        "Could not resolve decomp-authored model '{}' for {} resource {} from {}",
                        primary_model, object.factory_name, resource.resource_name, source
                    ),
                )),
                ModelPathResolution::Ambiguous(paths) => issues.push(ValidationIssue::warning(
                    "object-preview-model-ambiguous",
                    format!(
                        "Decomp-authored model '{}' for {} resource {} matched multiple assets: {}",
                        primary_model,
                        object.factory_name,
                        resource.resource_name,
                        paths.join(", ")
                    ),
                )),
            }
            continue;
        }

        let named_object_binding = object.raw_param("name").and_then(|name| {
            registry
                .find_named_object_model(&object.factory_name, name)
                .map(|model| (model.model_path.clone(), model.source_file.clone()))
        });
        let map_static_binding = if is_map_static {
            resource_name.as_deref().and_then(|resource_name| {
                registry
                    .map_static_models
                    .iter()
                    .find(|model| model.actor_name == resource_name)
                    .and_then(|model| {
                        model
                            .model_path
                            .clone()
                            .map(|path| (path, model.source_file.clone()))
                    })
            })
        } else {
            None
        };
        let binding = named_object_binding
            .or(map_static_binding)
            .or_else(|| {
                registry
                    .find_object(&object.factory_name)
                    .and_then(|definition| definition.preview_model.as_ref())
                    .map(|model| (model.clone(), "object schema".to_string()))
            })
            .or_else(|| {
                registry
                    .primary_object_resource(&object.factory_name)
                    .map(|resource| {
                        let model = resource
                            .resource_base
                            .as_deref()
                            .map(|base| {
                                format!(
                                    "{}/{}",
                                    base.trim_end_matches(['/', '\\']),
                                    resource.model_name.trim_start_matches(['/', '\\'])
                                )
                            })
                            .unwrap_or_else(|| resource.model_name.clone());
                        (model, resource.source_file.clone())
                    })
            });
        let Some((authored_model, source)) = binding else {
            continue;
        };

        // Once a schema binding exists, never retain a weaker basename guess,
        // including when the authored model is missing or ambiguous.
        object
            .asset_hints
            .retain(|hint| hint.role != AssetRole::InferredPreviewModel);

        match resolve_authored_model_path(&authored_model, &model_index) {
            ModelPathResolution::Found(path) => {
                object.asset_hints.push(AssetRef {
                    path,
                    role: AssetRole::InferredPreviewModel,
                });
            }
            ModelPathResolution::Missing => issues.push(ValidationIssue::warning(
                "object-preview-model-unresolved",
                format!(
                    "Could not resolve decomp-authored model '{}' for {} from {}",
                    authored_model, object.factory_name, source
                ),
            )),
            ModelPathResolution::Ambiguous(paths) => issues.push(ValidationIssue::warning(
                "object-preview-model-ambiguous",
                format!(
                    "Decomp-authored model '{}' for {} matched multiple assets: {}",
                    authored_model,
                    object.factory_name,
                    paths.join(", ")
                ),
            )),
        }
    }

    issues
}

enum ModelPathResolution {
    Found(String),
    Missing,
    Ambiguous(Vec<String>),
}

fn resolve_authored_model_path(
    authored_model: &str,
    model_index: &[(String, String)],
) -> ModelPathResolution {
    let normalized = authored_model.replace('\\', "/").to_ascii_lowercase();
    let suffix = normalized
        .strip_prefix("/scene/")
        .unwrap_or_else(|| normalized.trim_start_matches('/'));
    let filename_only = !suffix.contains('/');
    let mut matches: Vec<_> = model_index
        .iter()
        .filter(|(path, _)| {
            let normalized_path = path.replace('\\', "/").to_ascii_lowercase();
            if filename_only {
                normalized_path
                    .rsplit('/')
                    .next()
                    .is_some_and(|name| name == suffix)
            } else {
                normalized_path.ends_with(suffix)
            }
        })
        .map(|(path, _)| path.clone())
        .collect();
    matches.sort();
    matches.dedup();
    match matches.len() {
        0 => ModelPathResolution::Missing,
        1 => ModelPathResolution::Found(matches.pop().expect("one model match")),
        _ => ModelPathResolution::Ambiguous(matches),
    }
}

#[derive(Clone)]
struct StageManagerResource {
    factory_name: String,
    chara_name: String,
}

fn enemy_runtime_uniform_scale(
    actor: &EnemyActorDefinition,
    manager: &EnemyManagerDefinition,
    assets: &[StageAsset],
    read_asset: &mut impl FnMut(&Path) -> std::result::Result<Vec<u8>, String>,
) -> std::result::Result<Option<f32>, String> {
    let Some(definition) = &actor.runtime_uniform_scale else {
        return Ok(None);
    };
    let Some(parameter_path) = &manager.parameter_path else {
        return Ok(None);
    };
    let stage_suffix = format!(
        "/map/params/{}",
        parameter_path
            .replace('\\', "/")
            .trim_start_matches('/')
            .to_ascii_lowercase()
    );
    let global_suffix = format!(
        "!/{}",
        parameter_path
            .replace('\\', "/")
            .trim_start_matches('/')
            .to_ascii_lowercase()
    );
    let stage_matches = assets.iter().filter(|asset| {
        asset
            .path
            .to_string_lossy()
            .replace('\\', "/")
            .to_ascii_lowercase()
            .ends_with(&stage_suffix)
    });
    let mut matches = stage_matches.peekable();
    let use_global = matches.peek().is_none();
    let mut global_matches = assets.iter().filter(|asset| {
        asset
            .path
            .to_string_lossy()
            .replace('\\', "/")
            .to_ascii_lowercase()
            .ends_with(&global_suffix)
    });
    let asset = if use_global {
        global_matches.next()
    } else {
        matches.next()
    };
    let Some(asset) = asset else {
        return Err(format!(
            "Could not resolve decomp-authored parameter path {parameter_path} for {}",
            manager.factory_name
        ));
    };
    if (use_global && global_matches.next().is_some()) || (!use_global && matches.next().is_some())
    {
        return Err(format!(
            "Decomp-authored parameter path {parameter_path} for {} matched multiple stage assets",
            manager.factory_name
        ));
    }
    let bytes = read_asset(&asset.path).map_err(|error| {
        format!(
            "Could not read runtime scale parameters {}: {error}",
            asset.path.display()
        )
    })?;
    let parameters = PrmFile::parse(&bytes).map_err(|error| {
        format!(
            "Could not parse runtime scale parameters {}: {error}",
            asset.path.display()
        )
    })?;
    let low = parameters.f32(&definition.low_parameter).ok_or_else(|| {
        format!(
            "Runtime scale parameter {} from {} is missing or not an f32 in {}",
            definition.low_parameter,
            definition.source_file,
            asset.path.display()
        )
    })?;
    let high = parameters.f32(&definition.high_parameter).ok_or_else(|| {
        format!(
            "Runtime scale parameter {} from {} is missing or not an f32 in {}",
            definition.high_parameter,
            definition.source_file,
            asset.path.display()
        )
    })?;
    let scale = low + (high - low) * 0.5;
    if !scale.is_finite() {
        return Err(format!(
            "Runtime scale range {low}..{high} in {} is not finite",
            asset.path.display()
        ));
    }
    Ok(Some(scale))
}

fn load_global_parameter_assets(base_root: &Path) -> std::result::Result<Vec<StageAsset>, String> {
    let candidates = [
        base_root.join("files/data/params.szs"),
        base_root.join("data/params.szs"),
        base_root.join("params.szs"),
    ];
    let Some(path) = candidates.into_iter().find(|path| path.is_file()) else {
        return Err(format!(
            "Could not find data/params.szs under {} for runtime actor scale binding",
            base_root.display()
        ));
    };
    mount_scene_archive(&path).map_err(|error| {
        format!(
            "Could not mount {} for runtime actor scale binding: {error}",
            path.display()
        )
    })
}

fn enemy_actor_manager_name<'a>(
    record: &'a JDramaObjectRecord,
    registry: &ObjectRegistry,
) -> Option<&'a str> {
    // The tail string is structural JDrama data. Interpret it as a TLiveActor
    // manager only when the exact, decomp-derived factory is an enemy actor.
    registry.find_enemy_actor(&record.type_name)?;
    record.actor_tail_string.as_deref()
}

fn build_actor_preview_catalog(
    base_root: &Path,
    assets: &[StageAsset],
    registry: &ObjectRegistry,
    mut read_asset: impl FnMut(&Path) -> std::result::Result<Vec<u8>, String>,
) -> (BTreeMap<String, ActorPreview>, Vec<ValidationIssue>) {
    let chara_folders = match load_obj_chara_folders(base_root) {
        Ok(folders) => folders,
        Err(_) if registry.enemy_managers.is_empty() => return (BTreeMap::new(), Vec::new()),
        Err(message) => {
            return (
                BTreeMap::new(),
                vec![ValidationIssue::warning(
                    "enemy-preview-catalog-unavailable",
                    message,
                )],
            );
        }
    };
    let model_index = stage_model_index(assets);
    let mut catalog = BTreeMap::new();
    let mut issues = Vec::new();
    let mut unresolved_factories = BTreeSet::new();
    let mut parameter_assets = assets.to_vec();
    if registry
        .enemy_actors
        .iter()
        .any(|actor| actor.runtime_uniform_scale.is_some())
    {
        match load_global_parameter_assets(base_root) {
            Ok(global_assets) => parameter_assets.extend(global_assets),
            Err(message) => issues.push(ValidationIssue::warning(
                "enemy-preview-runtime-parameters-unavailable",
                message,
            )),
        }
    }

    for asset in assets
        .iter()
        .filter(|asset| asset.kind == StageAssetKind::Placement)
        .filter(|asset| {
            asset
                .path
                .to_string_lossy()
                .replace('\\', "/")
                .to_ascii_lowercase()
                .ends_with("/map/scene.bin")
        })
    {
        let Ok(bytes) = read_asset(&asset.path) else {
            issues.push(ValidationIssue::warning(
                "enemy-preview-placement-read-failed",
                format!(
                    "Could not reread {} while resolving enemy previews",
                    asset.path.display()
                ),
            ));
            continue;
        };
        let Ok(records) = parse_jdrama_object_records(&bytes) else {
            issues.push(ValidationIssue::warning(
                "enemy-preview-placement-parse-failed",
                format!(
                    "Could not reparse {} while resolving enemy previews",
                    asset.path.display()
                ),
            ));
            continue;
        };
        let managers = records
            .iter()
            .filter_map(|record| {
                Some((
                    record.object_name.clone()?,
                    StageManagerResource {
                        factory_name: record.type_name.clone(),
                        chara_name: record.obj_manager_chara.clone()?,
                    },
                ))
            })
            .collect::<BTreeMap<_, _>>();
        let mut runtime_uniform_scales = BTreeMap::new();
        for actor in &registry.enemy_actors {
            for manager_factory in &actor.manager_factories {
                let Some(manager_resource) = managers
                    .values()
                    .find(|resource| resource.factory_name == *manager_factory)
                else {
                    continue;
                };
                let Some(manager) = registry.find_enemy_manager(&manager_resource.factory_name)
                else {
                    continue;
                };
                match enemy_runtime_uniform_scale(
                    actor,
                    manager,
                    &parameter_assets,
                    &mut read_asset,
                ) {
                    Ok(scale) => {
                        runtime_uniform_scales.insert(
                            (actor.factory_name.clone(), manager.factory_name.clone()),
                            scale,
                        );
                    }
                    Err(message) => issues.push(ValidationIssue::warning(
                        "enemy-preview-runtime-scale-unresolved",
                        message,
                    )),
                }
            }
        }

        for actor in &registry.enemy_actors {
            let factory_key = actor_preview_factory_key(&actor.factory_name);
            if let Some(model) = actor.indexed_models.iter().find(|model| model.index == 0) {
                if let Some(model_path) =
                    resolve_resource_model_path(&model.model_path, &model_index)
                {
                    catalog
                        .entry(factory_key.clone())
                        .or_insert_with(|| ActorPreview {
                            model_path,
                            load_flags: model.load_flags,
                            manager_factory: format!("{} actor", actor.factory_name),
                            runtime_uniform_scale: None,
                        });
                }
            }
            for manager_factory in &actor.manager_factories {
                let Some(manager_resource) = managers
                    .values()
                    .find(|resource| resource.factory_name == *manager_factory)
                else {
                    continue;
                };
                let Some(manager) = registry.find_enemy_manager(&manager_resource.factory_name)
                else {
                    continue;
                };
                let Some(folder) = chara_folders.get(&manager_resource.chara_name) else {
                    continue;
                };
                let Some(mut preview) =
                    resolve_manager_actor_preview(actor, manager, folder, &model_index)
                else {
                    continue;
                };
                preview.runtime_uniform_scale = runtime_uniform_scales
                    .get(&(actor.factory_name.clone(), manager.factory_name.clone()))
                    .copied()
                    .flatten();
                catalog.entry(factory_key.clone()).or_insert(preview);
                break;
            }
            if catalog.contains_key(&factory_key) {
                continue;
            }
            for manager_factory in &actor.manager_factories {
                let Some(target_manager) = registry.find_enemy_manager(manager_factory) else {
                    continue;
                };
                for manager_resource in managers.values() {
                    let Some(stage_manager) =
                        registry.find_enemy_manager(&manager_resource.factory_name)
                    else {
                        continue;
                    };
                    if !manager_model_tables_are_aliases(target_manager, stage_manager) {
                        continue;
                    }
                    let Some(folder) = chara_folders.get(&manager_resource.chara_name) else {
                        continue;
                    };
                    let Some(mut preview) =
                        resolve_manager_actor_preview(actor, target_manager, folder, &model_index)
                    else {
                        continue;
                    };
                    preview.runtime_uniform_scale = runtime_uniform_scales
                        .get(&(
                            actor.factory_name.clone(),
                            target_manager.factory_name.clone(),
                        ))
                        .copied()
                        .flatten();
                    catalog.insert(factory_key.clone(), preview);
                    break;
                }
                if catalog.contains_key(&factory_key) {
                    break;
                }
            }
        }

        for record in records.iter().filter(|record| record.transform.is_some()) {
            let has_manager_model_binding = enemy_actor_manager_name(record, registry)
                .and_then(|manager_name| managers.get(manager_name))
                .and_then(|resource| {
                    Some((
                        registry.find_enemy_manager(&resource.factory_name)?,
                        chara_folders.get(&resource.chara_name)?,
                    ))
                })
                .is_some();
            let has_actor_model_binding =
                registry
                    .find_enemy_actor(&record.type_name)
                    .is_some_and(|actor| {
                        (!actor.fallback_models.is_empty()
                            && record
                                .actor_character
                                .as_ref()
                                .and_then(|character| chara_folders.get(character))
                                .is_some())
                            || actor.named_models.iter().any(|model| {
                                record
                                    .object_name
                                    .as_ref()
                                    .is_some_and(|name| name == &model.actor_name)
                            })
                            || (!actor.indexed_models.is_empty()
                                && record.mario_modoki_telesa_imitation_index.is_some())
                    });
            let direct_actor_preview = (|| {
                let actor = registry.find_enemy_actor(&record.type_name)?;
                if let Some(selected) = actor
                    .named_models
                    .iter()
                    .find(|model| record.object_name.as_ref() == Some(&model.actor_name))
                {
                    return resolve_resource_model_path(&selected.model_path, &model_index).map(
                        |path| {
                            (
                                path,
                                selected.load_flags,
                                format!("{} actor", actor.factory_name),
                                None,
                            )
                        },
                    );
                }
                let index = record.mario_modoki_telesa_imitation_index?;
                let exact = actor
                    .indexed_models
                    .iter()
                    .find(|model| model.index == index);
                let default = actor.indexed_models.iter().find(|model| model.index == 0);
                exact
                    .into_iter()
                    .chain(default.filter(|_| index != 0))
                    .find_map(|model| {
                        resolve_resource_model_path(&model.model_path, &model_index).map(|path| {
                            (
                                path,
                                model.load_flags,
                                format!("{} actor", actor.factory_name),
                                None,
                            )
                        })
                    })
            })();
            let manager_preview = (|| {
                let manager_name = enemy_actor_manager_name(record, registry)?;
                let manager_resource = managers.get(manager_name)?;
                let manager = registry.find_enemy_manager(&manager_resource.factory_name)?;
                let folder = chara_folders.get(&manager_resource.chara_name)?;
                let actor = registry.find_enemy_actor(&record.type_name);
                resolve_manager_actor_preview(actor?, manager, folder, &model_index).map(
                    |preview| {
                        (
                            preview.model_path,
                            preview.load_flags,
                            preview.manager_factory,
                            runtime_uniform_scales
                                .get(&(record.type_name.clone(), manager.factory_name.clone()))
                                .copied()
                                .flatten(),
                        )
                    },
                )
            })();
            let actor_preview = (|| {
                let actor = registry.find_enemy_actor(&record.type_name)?;
                let character = record.actor_character.as_ref()?;
                let folder = chara_folders.get(character)?;
                actor.fallback_models.iter().find_map(|model| {
                    resolve_chara_model_path(folder, &model.model_name, &model_index).map(|path| {
                        (
                            path,
                            model.load_flags,
                            format!("{} actor", actor.factory_name),
                            None,
                        )
                    })
                })
            })();
            let Some((model_path, load_flags, manager_factory, runtime_uniform_scale)) =
                direct_actor_preview.or(manager_preview).or(actor_preview)
            else {
                if has_manager_model_binding || has_actor_model_binding {
                    unresolved_factories.insert(record.type_name.clone());
                }
                continue;
            };
            let source = SourceLocation {
                path: asset.path.clone(),
                offset: Some(record.offset as u64),
                length: Some(record.size as u64),
            };
            let Some(key) = actor_preview_source_key(&source) else {
                continue;
            };
            let preview = ActorPreview {
                model_path,
                load_flags,
                manager_factory,
                runtime_uniform_scale,
            };
            catalog.insert(key, preview.clone());
            let used_named_actor_preview = registry
                .find_enemy_actor(&record.type_name)
                .is_some_and(|actor| {
                    actor
                        .named_models
                        .iter()
                        .any(|model| record.object_name.as_ref() == Some(&model.actor_name))
                });
            let factory_preview = registry
                .find_enemy_actor(&record.type_name)
                .and_then(|actor| {
                    let model = actor.indexed_models.iter().find(|model| model.index == 0)?;
                    Some(ActorPreview {
                        model_path: resolve_resource_model_path(&model.model_path, &model_index)?,
                        load_flags: model.load_flags,
                        manager_factory: format!("{} actor", actor.factory_name),
                        runtime_uniform_scale: None,
                    })
                })
                .or_else(|| (!used_named_actor_preview).then(|| preview.clone()));
            if let Some(factory_preview) = factory_preview {
                catalog
                    .entry(actor_preview_factory_key(&record.type_name))
                    .or_insert(factory_preview);
            }
        }
    }
    if !unresolved_factories.is_empty() {
        issues.push(ValidationIssue::warning(
            "enemy-preview-model-unresolved",
            format!(
                "Could not resolve stage model assets for: {}",
                unresolved_factories
                    .into_iter()
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        ));
    }
    (catalog, issues)
}

fn load_obj_chara_folders(
    base_root: &Path,
) -> std::result::Result<BTreeMap<String, String>, String> {
    let candidates = [
        base_root.join("files/data/scenecmn.bin"),
        base_root.join("data/scenecmn.bin"),
        base_root.join("scenecmn.bin"),
    ];
    let Some(path) = candidates.into_iter().find(|path| path.is_file()) else {
        return Err(format!(
            "Could not find data/scenecmn.bin under {} for enemy resource binding",
            base_root.display()
        ));
    };
    let bytes = fs::read(&path).map_err(|error| {
        format!(
            "Could not read {} for enemy resource binding: {error}",
            path.display()
        )
    })?;
    let records = parse_jdrama_object_records(&bytes).map_err(|error| {
        format!(
            "Could not parse {} for enemy resource binding: {error}",
            path.display()
        )
    })?;
    Ok(records
        .into_iter()
        .filter_map(|record| Some((record.object_name?, record.obj_chara_folder?)))
        .collect())
}

fn resolve_chara_model_path(
    folder: &str,
    model_name: &str,
    model_index: &[(String, String)],
) -> Option<String> {
    resolve_resource_model_path(&format!("{folder}/{model_name}"), model_index)
}

fn resolve_manager_actor_preview(
    actor: &EnemyActorDefinition,
    manager: &EnemyManagerDefinition,
    folder: &str,
    model_index: &[(String, String)],
) -> Option<ActorPreview> {
    actor_manager_model_candidates(actor, manager)
        .into_iter()
        .find_map(|model| {
            resolve_chara_model_path(folder, &model.model_name, model_index).map(|model_path| {
                ActorPreview {
                    model_path,
                    load_flags: model.load_flags,
                    manager_factory: manager.factory_name.clone(),
                    runtime_uniform_scale: None,
                }
            })
        })
}

fn actor_manager_model_candidates<'a>(
    actor: &'a EnemyActorDefinition,
    manager: &'a EnemyManagerDefinition,
) -> Vec<&'a EnemyModelDefinition> {
    if !actor.fallback_models.is_empty() {
        return actor.fallback_models.iter().collect();
    }
    if let Some(model_index) = actor.model_index.or(manager.model_index) {
        return manager.models.get(model_index).into_iter().collect();
    }
    if let Some(primary_model) = &actor.primary_model {
        return manager
            .models
            .iter()
            .filter(|model| model.model_name.eq_ignore_ascii_case(primary_model))
            .collect();
    }
    manager.models.first().into_iter().collect()
}

fn manager_model_tables_are_aliases(
    target: &EnemyManagerDefinition,
    stage: &EnemyManagerDefinition,
) -> bool {
    !target.models.is_empty()
        && target.models.len() == stage.models.len()
        && target
            .models
            .iter()
            .all(|model| !model.model_name.eq_ignore_ascii_case("default.bmd"))
        && target
            .models
            .iter()
            .zip(&stage.models)
            .all(|(target, stage)| {
                target.model_name.eq_ignore_ascii_case(&stage.model_name)
                    && target.load_flags == stage.load_flags
            })
}

fn resolve_resource_model_path(
    model_path: &str,
    model_index: &[(String, String)],
) -> Option<String> {
    let normalized = model_path.replace('\\', "/").to_ascii_lowercase();
    let suffix = normalized
        .strip_prefix("/scene/")
        .unwrap_or_else(|| normalized.trim_start_matches('/'));
    model_index
        .iter()
        .find(|(path, _)| {
            path.replace('\\', "/")
                .to_ascii_lowercase()
                .ends_with(&suffix)
        })
        .map(|(path, _)| path.clone())
}

fn actor_preview_source_key(source: &SourceLocation) -> Option<String> {
    Some(format!(
        "{}@{:x}",
        source.path.to_string_lossy().replace('\\', "/"),
        source.offset?
    ))
}

fn actor_preview_factory_key(factory_name: &str) -> String {
    format!("factory:{factory_name}")
}

fn infer_preview_model_path(
    object: &SceneObject,
    model_index: &[(String, String)],
) -> Option<String> {
    // Resource-selecting TMapObjBase actors store their authored basename in
    // the first subclass field after the common TActor stream. Prefer that
    // exact selector over the common character name in stream_string_0. The
    // latter remains as a compatibility fallback for synthetic/editor-created
    // objects and older project overlays.
    for parameter in ["actor_tail_string", "stream_string_0"] {
        if let Some(model_name) = object.raw_params.get(parameter) {
            let key = normalize_model_key(model_name);
            if let Some(path) = exact_model_key_match(&key, model_index) {
                return Some(path);
            }
        }
    }
    None
}

fn exact_model_key_match(key: &str, model_index: &[(String, String)]) -> Option<String> {
    model_index
        .iter()
        .find(|(_, model_key)| model_key == key)
        .map(|(path, _)| path.clone())
}

fn normalize_model_key(value: &str) -> String {
    let value = value
        .rsplit("!/")
        .next()
        .unwrap_or(value)
        .rsplit('/')
        .next()
        .unwrap_or(value)
        .strip_suffix(".bmd")
        .or_else(|| value.strip_suffix(".bdl"))
        .unwrap_or(value);

    let mut key = String::new();
    for ch in value.chars() {
        if ch.is_ascii_alphanumeric() {
            key.push(ch.to_ascii_lowercase());
        }
    }

    key
}

#[cfg(test)]
mod tests {
    use super::*;

    fn schema_object(
        factory_name: &str,
        class_name: &str,
        category: &str,
        preview_model: Option<&str>,
    ) -> sms_schema::ObjectDefinition {
        sms_schema::ObjectDefinition {
            factory_name: factory_name.to_string(),
            class_name: class_name.to_string(),
            category: category.to_string(),
            source: sms_schema::SchemaSource::MarNameRefGen,
            display_name: None,
            preview_model: preview_model.map(str::to_string),
            hidden: false,
            unsafe_to_edit: false,
        }
    }

    #[test]
    fn restoring_resource_overlay_drops_stale_virtual_assets_and_keeps_baseline_assets() {
        let parameter_document = || {
            StageResourceDocument::Parameters(PrmFile {
                entries: Vec::new(),
            })
        };
        let source_path = PathBuf::from("virtual/custom0.szs");
        let baseline_path = PathBuf::from("virtual/custom0.szs!/map/base.prm");
        let extra_path = PathBuf::from("virtual/custom0.szs!/map/catalog.prm");
        let mut archive = SourceFreeStageArchive::new().unwrap();
        archive
            .insert_resource(b"map/base.prm".to_vec(), parameter_document())
            .unwrap();
        let mut document = StageDocument {
            stage_id: "custom0".to_string(),
            base_root: PathBuf::from("."),
            assets: vec![StageAsset {
                path: baseline_path.clone(),
                kind: StageAssetKind::Placement,
            }],
            objects: Vec::new(),
            changed_files: BTreeMap::new(),
            stage_archive: Some(archive),
            stage_archive_source_path: Some(source_path),
            archive_edits: StageArchiveEdits::default(),
            registry: None,
            route_authoring: None,
            goop_authoring: None,
            dialogue_authoring: None,
            dialogue_library: ProjectDialogueLibrary::default(),
            load_issues: Vec::new(),
            lighting: StageLighting::default(),
            actor_previews: BTreeMap::new(),
            loaded_project: None,
        };

        document.set_authored_resource_overlay_state(
            b"map/catalog.prm".to_vec(),
            Some(StageResourceEdit {
                raw_resource_path: b"map/catalog.prm".to_vec(),
                document: parameter_document(),
                mode: StageResourceEditMode::Insert,
            }),
            None,
            None,
        );
        document.set_authored_resource_overlay_state(
            b"map/base.prm".to_vec(),
            Some(StageResourceEdit {
                raw_resource_path: b"map/base.prm".to_vec(),
                document: parameter_document(),
                mode: StageResourceEditMode::Upsert,
            }),
            None,
            None,
        );
        assert!(document.assets.iter().any(|asset| asset.path == extra_path));
        assert!(document
            .assets
            .iter()
            .any(|asset| asset.path == baseline_path));

        document.set_authored_resource_overlay_state(b"map/catalog.prm".to_vec(), None, None, None);
        document.set_authored_resource_overlay_state(b"map/base.prm".to_vec(), None, None, None);

        assert!(!document.assets.iter().any(|asset| asset.path == extra_path));
        assert!(document
            .assets
            .iter()
            .any(|asset| asset.path == baseline_path));
        assert!(document.archive_edits.resources.is_empty());
    }

    #[test]
    fn selects_primary_object_light_and_ambient_by_runtime_names() {
        let lighting = StageLighting {
            lights: vec![JDramaLight {
                name: Some("太陽（オブジェクト）".to_string()),
                position: [1.0, 2.0, 3.0],
                color: [4, 5, 6, 255],
            }],
            ambients: vec![JDramaAmbient {
                name: Some("太陽アンビエント（オブジェクト）".to_string()),
                color: [7, 8, 9, 255],
            }],
        };
        assert_eq!(
            lighting.object_lighting(),
            Some(StageObjectLighting {
                position: [1.0, 2.0, 3.0],
                color: [4, 5, 6, 255],
                ambient: [7, 8, 9, 255],
            })
        );
        assert!(!lighting.object_lighting_uses_ordinal_fallback());
    }

    #[test]
    fn reports_when_object_lighting_uses_retail_ordinal_fallbacks() {
        let mut lighting = StageLighting::default();
        lighting.lights.resize_with(6, || JDramaLight {
            name: None,
            position: [0.0; 3],
            color: [255; 4],
        });
        lighting.ambients.resize_with(3, || JDramaAmbient {
            name: None,
            color: [64; 4],
        });
        assert!(lighting.object_lighting().is_some());
        assert!(lighting.object_lighting_uses_ordinal_fallback());
    }

    #[test]
    fn detects_invalid_transform() {
        let mut doc = StageDocument {
            stage_id: "dolpic".to_string(),
            base_root: PathBuf::from("."),
            assets: vec![],
            objects: vec![],
            changed_files: BTreeMap::new(),
            stage_archive: None,
            stage_archive_source_path: None,
            archive_edits: StageArchiveEdits::default(),
            registry: None,
            route_authoring: None,
            goop_authoring: None,
            dialogue_authoring: None,
            dialogue_library: ProjectDialogueLibrary::default(),
            load_issues: Vec::new(),
            lighting: StageLighting::default(),
            actor_previews: BTreeMap::new(),
            loaded_project: None,
        };
        let mut object = SceneObject::new("obj-1", "coin");
        object.transform.translation[0] = f32::NAN;
        doc.add_object(object);

        let issues = doc.validate();
        assert!(issues.iter().any(|issue| issue.code == "invalid-transform"));
    }

    #[test]
    fn rejects_empty_object_ids() {
        let mut doc = empty_document("dolpic");
        doc.objects.push(SceneObject::new("   ", "Coin"));

        assert!(doc
            .validate()
            .iter()
            .any(|issue| issue.code == "empty-object-id"
                && issue.severity == ValidationSeverity::Error));
    }

    fn authored_shine_for_validation(
        id: &str,
        collection_type: &str,
        shine_id: i32,
        in_stage: i32,
    ) -> SceneObject {
        let prototype = JDramaRecord {
            type_name: "Shine".to_string(),
            name: format!("Graffito-Editor Shine {id}"),
            payload: JDramaRecordPayload::Actor {
                transform: sms_formats::JDramaTransform {
                    translation: [0.0; 3],
                    rotation: [0.0; 3],
                    scale: [1.0; 3],
                },
                character_name: "??????".to_string(),
                light_map: sms_formats::JDramaLightMap::default(),
                fields: vec![
                    JDramaField {
                        name: "collection_type".to_string(),
                        value: JDramaFieldValue::String(collection_type.to_string()),
                    },
                    JDramaField {
                        name: "shine_id".to_string(),
                        value: JDramaFieldValue::I32(shine_id),
                    },
                    JDramaField {
                        name: "in_stage".to_string(),
                        value: JDramaFieldValue::I32(in_stage),
                    },
                ],
            },
        };
        let mut object = SceneObject::new(id, "Shine");
        seed_scene_object_parameters(&mut object, &prototype).unwrap();
        object.placement = Some(PlacementBinding::Authored(AuthoredPlacement {
            raw_resource_path: b"map/scene.bin".to_vec(),
            target_group_index: 4,
            prototype,
            dependencies: Vec::new(),
        }));
        object
    }

    #[test]
    fn validates_authored_shine_runtime_modes_flags_and_camera_values() {
        let mut doc = empty_document("fixture0");
        doc.objects
            .push(authored_shine_for_validation("shine-a", "demo", 12, 1));
        doc.objects
            .push(authored_shine_for_validation("shine-b", "normal", 12, -1));
        doc.objects
            .push(authored_shine_for_validation("shine-c", "normal", 120, 0));

        let issues = doc.validate();
        for code in [
            "shine-requires-external-trigger",
            "invalid-shine-camera-mode",
            "invalid-shine-id",
            "duplicate-authored-shine-id",
        ] {
            assert!(
                issues.iter().any(|issue| issue.code == code),
                "missing validation issue {code}: {issues:?}"
            );
        }
    }

    #[test]
    fn authored_normal_shine_with_unique_flag_has_no_shine_warning() {
        let mut doc = empty_document("fixture0");
        doc.objects
            .push(authored_shine_for_validation("shine-a", "normal", 12, -1));

        assert!(doc
            .validate()
            .iter()
            .all(|issue| !issue.code.contains("shine")));
    }

    #[test]
    fn authored_quick_shine_without_retail_camera_is_a_validation_error() {
        let mut doc = empty_document("fixture0");
        doc.objects
            .push(authored_shine_for_validation("shine-a", "quickly", 12, -1));

        assert!(doc.validate().iter().any(|issue| {
            issue.code == "missing-shine-quick-camera"
                && issue.severity == ValidationSeverity::Error
        }));
    }

    #[test]
    fn queues_editor_overlay_as_changed_file() {
        let mut doc = StageDocument {
            stage_id: "dolpic".to_string(),
            base_root: PathBuf::from("."),
            assets: vec![],
            objects: vec![SceneObject::new("obj-1", "coin")],
            changed_files: BTreeMap::new(),
            stage_archive: None,
            stage_archive_source_path: None,
            archive_edits: StageArchiveEdits::default(),
            registry: None,
            route_authoring: None,
            goop_authoring: None,
            dialogue_authoring: None,
            dialogue_library: ProjectDialogueLibrary::default(),
            load_issues: Vec::new(),
            lighting: StageLighting::default(),
            actor_previews: BTreeMap::new(),
            loaded_project: None,
        };

        doc.queue_editor_overlay_change().unwrap();
        assert!(doc
            .changed_files
            .contains_key(&PathBuf::from("editor/stages/dolpic.scene.json")));
    }

    #[test]
    fn preview_cache_keys_preserve_case_sensitive_factory_identity() {
        assert_ne!(
            actor_preview_factory_key("maregate"),
            actor_preview_factory_key("MareGate")
        );
    }

    #[test]
    fn queues_an_explicit_empty_editor_overlay() {
        let mut doc = empty_document("dolpic");
        doc.queue_editor_overlay_change().unwrap();

        let path = PathBuf::from("editor/stages/dolpic.scene.json");
        let bytes = doc.changed_files.get(&path).unwrap();
        let overlay: EditorSceneOverlay = serde_json::from_slice(bytes).unwrap();
        assert_eq!(overlay.stage_id, "dolpic");
        assert!(overlay.objects.is_empty());
        assert!(overlay.goop_authoring.is_none());
    }

    #[test]
    fn ignores_legacy_embedded_archive_with_non_finite_json_nulls() {
        let overlay: EditorSceneOverlay = serde_json::from_str(
            r#"{
                "stage_id": "dolpic",
                "objects": [],
                "archive_edits": {},
                "stage_archive": {
                    "resources": [{"parameters": [1000000.0, 0.000001, null]}]
                }
            }"#,
        )
        .unwrap();

        assert_eq!(overlay.stage_id, "dolpic");
        assert!(overlay.objects.is_empty());
        assert_eq!(overlay.archive_edits, StageArchiveEdits::default());
        assert!(overlay.lighting.is_none());
        assert!(overlay.goop_authoring.is_none());
    }

    #[test]
    fn scene_parameters_keep_one_raw_decoded_dirty_state_and_legacy_json_shape() {
        let mut object = SceneObject::new("fixture", "Fixture");
        object.insert_source_raw_param("source", "17");
        object.set_decoded_param("edited", "23", ParamValue::Int(23));

        assert!(!object.raw_params["source"].is_dirty());
        assert!(object.raw_params["edited"].is_dirty());
        assert_eq!(
            object.raw_params["edited"].decoded(),
            Some(&ParamValue::Int(23))
        );

        let json = serde_json::to_value(&object).unwrap();
        assert_eq!(json["raw_params"]["source"], "17");
        assert_eq!(json["raw_params"]["edited"], "23");
        assert!(json.get("decoded_params").is_none());
        assert!(json.get("source_record_bytes").is_none());

        let restored: SceneObject = serde_json::from_value(json).unwrap();
        assert_eq!(restored.raw_param("source"), Some("17"));
        assert_eq!(restored.raw_param("edited"), Some("23"));
        assert!(!restored.raw_params["edited"].is_dirty());
        assert_eq!(restored.id, "fixture");

        assert_eq!(
            SceneParameter::from_source("23"),
            SceneParameter::edited("23", Some(ParamValue::Int(23)))
        );
    }

    #[test]
    fn typed_jdrama_stream_colors_populate_source_parameters_without_record_bytes() {
        let record = JDramaRecord::new(
            "FixturePaint",
            "paint",
            JDramaRecordPayload::Actor {
                transform: sms_formats::JDramaTransform {
                    translation: [0.0; 3],
                    rotation: [0.0; 3],
                    scale: [1.0; 3],
                },
                character_name: "paint".to_string(),
                light_map: sms_formats::JDramaLightMap::default(),
                fields: vec![
                    JDramaField {
                        name: "tev_red".to_string(),
                        value: JDramaFieldValue::U32(511),
                    },
                    JDramaField {
                        name: "tev_green".to_string(),
                        value: JDramaFieldValue::U32(120),
                    },
                    JDramaField {
                        name: "tev_blue".to_string(),
                        value: JDramaFieldValue::U32(9),
                    },
                ],
            },
        )
        .unwrap();
        let mut object = SceneObject::new("paint", "FixturePaint");
        insert_typed_jdrama_params(&mut object, &record);
        assert_eq!(object.raw_param("tev_red"), Some("511"));
        assert_eq!(object.raw_param("tev_green"), Some("120"));
        assert_eq!(object.raw_param("tev_blue"), Some("9"));
        assert!(object.raw_params.values().all(|value| !value.is_dirty()));
    }

    #[test]
    fn rejects_project_paths_that_escape_the_output_root() {
        let mut doc = empty_document("dolpic");
        let err = doc
            .mark_changed_file(PathBuf::from("../outside.bin"), vec![1, 2, 3])
            .unwrap_err();
        assert!(matches!(err, SceneError::UnsafeProjectPath(_)));

        doc.stage_id = "../../outside".to_string();
        assert!(matches!(
            doc.queue_editor_overlay_change().unwrap_err(),
            SceneError::InvalidStageId(_)
        ));

        for path in [
            "foo:bar",
            "trailing-dot.",
            "trailing-space ",
            "NUL",
            "CON.scene.json",
            "folder/COM1.bin",
        ] {
            assert!(matches!(
                doc.mark_changed_file(path, Vec::new()).unwrap_err(),
                SceneError::UnsafeProjectPath(_)
            ));
        }
        for stage_id in ["NUL", "con", "COM1", "trailing-dot."] {
            assert!(matches!(
                validate_stage_id(stage_id).unwrap_err(),
                SceneError::InvalidStageId(_)
            ));
        }
    }

    #[test]
    fn project_export_preserves_other_managed_and_unmanaged_files() {
        let root = std::env::temp_dir().join(format!(
            "sms-editor-project-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let mut doc = empty_document("dolpic");
        doc.mark_changed_file("first.bin", vec![1]).unwrap();
        doc.save_project_folder(&root).unwrap();
        assert!(root.join("files/first.bin").exists());
        let first_manifest: EditorProjectManifest =
            toml::from_str(&fs::read_to_string(root.join("sms-project.toml")).unwrap()).unwrap();
        assert_eq!(first_manifest.project_files_path, Path::new("files"));
        fs::write(root.join("user-notes.txt"), b"keep me").unwrap();
        fs::create_dir_all(root.join("files/user-content")).unwrap();
        fs::write(root.join("files/user-content/notes.bin"), b"keep this too").unwrap();

        doc.changed_files.clear();
        doc.mark_changed_file("second.bin", vec![2]).unwrap();
        doc.save_project_folder(&root).unwrap();
        assert!(root.join("files/first.bin").exists());
        assert!(root.join("files/second.bin").exists());
        assert!(root.join("sms-project.toml").exists());
        assert_eq!(fs::read(root.join("user-notes.txt")).unwrap(), b"keep me");
        assert_eq!(
            fs::read(root.join("files/user-content/notes.bin")).unwrap(),
            b"keep this too"
        );
        let second_manifest: EditorProjectManifest =
            toml::from_str(&fs::read_to_string(root.join("sms-project.toml")).unwrap()).unwrap();
        assert!(!second_manifest.project_id.is_empty());
        assert_eq!(first_manifest.project_id, second_manifest.project_id);
        assert_eq!(second_manifest.revision, first_manifest.revision + 1);
        assert_eq!(second_manifest.changed_files.len(), 3);
        assert!(second_manifest
            .changed_files
            .contains(&PathBuf::from("editor/stages/dolpic.scene.json")));
        assert!(second_manifest
            .changed_files
            .contains(&PathBuf::from("first.bin")));
        assert!(second_manifest
            .changed_files
            .contains(&PathBuf::from("second.bin")));

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn project_round_trip_loads_the_saved_stage_overlay() {
        let root = unique_test_project_root("overlay-round-trip");
        let mut saved = empty_document("dolpic");
        saved.objects = vec![SceneObject::new("edited-object", "Coin")];
        saved.lighting = StageLighting {
            lights: vec![JDramaLight {
                name: Some("authored key".to_string()),
                position: [10.0, 20.0, 30.0],
                color: [40, 50, 60, 255],
            }],
            ambients: vec![JDramaAmbient {
                name: Some("authored ambient".to_string()),
                color: [70, 80, 90, 255],
            }],
        };
        saved.archive_edits.replace_collision(
            b"map/map.col".to_vec(),
            sms_formats::ColFile::new(vec![], vec![]),
        );
        saved.archive_edits.insert_placement(
            b"map/scene.bin".to_vec(),
            Vec::new(),
            JDramaRecord::new("NameRef", "inserted", JDramaRecordPayload::Empty).unwrap(),
        );
        saved.queue_editor_overlay_change().unwrap();
        saved.save_project_folder(&root).unwrap();

        let mut reopened = empty_document("dolpic");
        assert!(reopened.load_project_folder(&root).unwrap());
        assert_eq!(reopened.objects, saved.objects);
        assert_eq!(reopened.archive_edits, saved.archive_edits);
        assert_eq!(reopened.lighting, saved.lighting);
        assert!(reopened.loaded_project.is_some());

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn authored_stage_project_round_trip_restores_baseline_before_overlay() {
        let fixture_root = unique_test_project_root("authored-stage-round-trip");
        let base_root = fixture_root.join("base");
        let project_root = fixture_root.join("project");
        let scene_root = base_root.join("files/data/scene");
        fs::create_dir_all(&scene_root).unwrap();

        let retail_archive = authored_stage_archive("test11", 1.0, [10, 20, 30, 255], 1);
        fs::write(
            scene_root.join("test11.szs"),
            retail_archive.encode().unwrap(),
        )
        .unwrap();

        let authored_archive = authored_stage_archive("test11", 9.0, [90, 80, 70, 255], 9);
        let mut expected_archive = authored_archive.clone();
        blank_stage::ensure_blank_stage_runtime_resources(&mut expected_archive).unwrap();
        let expected_baseline = expected_archive.to_semantic_json().unwrap();
        let mut saved =
            StageDocument::from_authored_archive(&base_root, "test11", authored_archive).unwrap();

        assert!(matches!(
            saved.stage_archive.as_ref().unwrap().origin(),
            StageOrigin::Blank { target_slot, .. } if target_slot == "test11"
        ));
        assert_eq!(saved.objects.len(), 1);
        assert_eq!(saved.objects[0].factory_name, "Mario");
        assert_eq!(saved.objects[0].transform.translation[0], 9.0);
        assert_eq!(
            saved.lighting.object_lighting().unwrap().color,
            [90, 80, 70, 255]
        );
        assert_authored_parameter_asset(&saved, 9);

        saved.objects[0].transform.translation[0] = 42.0;
        saved.save_project_folder(&project_root).unwrap();
        let baseline_path = project_root.join("files/editor/stages/test11.stage.json");
        assert_eq!(fs::read(&baseline_path).unwrap(), expected_baseline);

        // Repeated saves must not introduce semantic-document drift.
        saved.save_project_folder(&project_root).unwrap();
        assert_eq!(fs::read(&baseline_path).unwrap(), expected_baseline);

        let mut reopened = StageDocument::open(&base_root, "test11").unwrap();
        assert!(matches!(
            reopened.stage_archive.as_ref().unwrap().origin(),
            StageOrigin::ImportedArchive
        ));
        assert_eq!(reopened.objects[0].transform.translation[0], 1.0);
        assert!(reopened.load_project_folder(&project_root).unwrap());
        assert!(matches!(
            reopened.stage_archive.as_ref().unwrap().origin(),
            StageOrigin::Blank { target_slot, .. } if target_slot == "test11"
        ));
        assert_eq!(reopened.objects.len(), 1);
        assert_eq!(reopened.objects[0].transform.translation[0], 42.0);
        assert_eq!(
            reopened.lighting.object_lighting().unwrap().color,
            [90, 80, 70, 255]
        );
        assert_authored_parameter_asset(&reopened, 9);

        fs::remove_dir_all(fixture_root).unwrap();
    }

    #[test]
    fn genuinely_new_authored_stage_reopens_without_a_retail_archive() {
        let fixture_root = unique_test_project_root("new-authored-stage-round-trip");
        let base_root = fixture_root.join("base");
        let project_root = fixture_root.join("project");
        fs::create_dir_all(&base_root).unwrap();

        let authored_archive = authored_stage_archive("custom_stage0", 9.0, [90, 80, 70, 255], 9);
        let mut saved =
            StageDocument::from_authored_archive(&base_root, "custom_stage0", authored_archive)
                .unwrap();
        let virtual_source = base_root.join("files/data/scene/custom_stage0.szs");
        assert_eq!(
            saved.stage_archive_source_path.as_deref(),
            Some(virtual_source.as_path())
        );
        assert!(!virtual_source.exists());
        saved.objects[0].transform.translation[0] = 42.0;
        saved.lighting.lights[5].color = [12, 34, 56, 255];
        saved.lighting.ambients[2].color = [78, 90, 123, 255];
        saved.save_project_folder(&project_root).unwrap();

        assert_eq!(
            discover_authored_project_stage_ids(&project_root).unwrap(),
            ["custom_stage0"]
        );
        let reopened =
            StageDocument::open_authored_project_stage(&base_root, "custom_stage0", &project_root)
                .unwrap();
        assert_eq!(reopened.objects[0].transform.translation[0], 42.0);
        assert_eq!(reopened.lighting.lights[5].color, [12, 34, 56, 255]);
        assert_eq!(reopened.lighting.ambients[2].color, [78, 90, 123, 255]);
        assert_eq!(
            reopened.stage_archive_source_path.as_deref(),
            Some(virtual_source.as_path())
        );
        assert!(!virtual_source.exists());

        // The normal open-then-load path also recovers the authored baseline
        // even though semantic retail import reports no archive for this id.
        let mut open_then_load = StageDocument::open(&base_root, "custom_stage0").unwrap();
        assert!(open_then_load.stage_archive.is_none());
        assert!(open_then_load.load_project_folder(&project_root).unwrap());
        assert_eq!(open_then_load.objects[0].transform.translation[0], 42.0);
        assert_eq!(
            open_then_load.stage_archive_source_path.as_deref(),
            Some(virtual_source.as_path())
        );

        fs::remove_dir_all(fixture_root).unwrap();
    }

    #[test]
    fn authored_stage_id_substring_does_not_import_a_retail_level() {
        let fixture_root = unique_test_project_root("authored-stage-no-fuzzy-retail");
        let base_root = fixture_root.join("base");
        let retail_path = base_root.join("files/data/scene/airport0.szs");
        fs::create_dir_all(retail_path.parent().unwrap()).unwrap();
        fs::write(&retail_path, b"must not be opened").unwrap();
        let authored_archive = authored_stage_archive("airport", 0.0, [1, 2, 3, 255], 1);

        let document =
            StageDocument::from_authored_archive(&base_root, "airport", authored_archive).unwrap();

        assert!(document
            .assets
            .iter()
            .all(|asset| !asset.path.to_string_lossy().contains("airport0.szs")));
        fs::remove_dir_all(fixture_root).unwrap();
    }

    #[test]
    fn authored_stage_project_rejects_a_baseline_for_another_target_slot() {
        let fixture_root = unique_test_project_root("authored-stage-target-validation");
        let base_root = fixture_root.join("base");
        let project_root = fixture_root.join("project");
        let scene_root = base_root.join("files/data/scene");
        fs::create_dir_all(&scene_root).unwrap();

        let retail_archive = authored_stage_archive("test11", 1.0, [10, 20, 30, 255], 1);
        fs::write(
            scene_root.join("test11.szs"),
            retail_archive.encode().unwrap(),
        )
        .unwrap();

        let authored_archive = authored_stage_archive("test11", 9.0, [90, 80, 70, 255], 9);
        let mut saved =
            StageDocument::from_authored_archive(&base_root, "test11", authored_archive).unwrap();
        saved.save_project_folder(&project_root).unwrap();

        let wrong_target = authored_stage_archive("bianco0", 7.0, [70, 60, 50, 255], 7);
        fs::write(
            project_root.join("files/editor/stages/test11.stage.json"),
            wrong_target.to_semantic_json().unwrap(),
        )
        .unwrap();

        let mut reopened = StageDocument::open(&base_root, "test11").unwrap();
        let error = reopened.load_project_folder(&project_root).unwrap_err();
        assert!(error
            .to_string()
            .contains("authored stage target 'bianco0' does not match document stage 'test11'"));

        fs::remove_dir_all(fixture_root).unwrap();
    }

    #[test]
    fn project_overlay_reload_refreshes_source_identity_and_derived_metadata() {
        let root = unique_test_project_root("source-record-round-trip");
        let source_root = unique_test_project_root("source-record-alias");
        fs::create_dir_all(&source_root).unwrap();
        let archive_path = source_root.join("stage.szs");
        fs::write(&archive_path, []).unwrap();
        let overlay_source = SourceLocation {
            path: PathBuf::from(format!(
                "{}/./stage.szs!/map/scene.bin",
                source_root.to_string_lossy().replace('\\', "/")
            )),
            offset: Some(32),
            length: Some(4),
        };
        let mut base_object = SceneObject::new("retail-object", "Coin");
        base_object.source = Some(overlay_source.clone());

        let mut saved = empty_document("dolpic");
        saved.objects = vec![base_object.clone()];
        saved.objects[0].transform.translation[0] = 42.0;
        saved.objects[0].set_raw_param("name", "overlay-edited name");
        saved.objects[0].asset_hints = vec![
            AssetRef {
                path: "stage.szs!/mapobj/stale.bmd".to_string(),
                role: AssetRole::InferredPreviewModel,
            },
            AssetRef {
                path: "mods/user-preview.bmd".to_string(),
                role: AssetRole::PreviewModel,
            },
        ];
        saved.save_project_folder(&root).unwrap();

        let mut reopened = empty_document("dolpic");
        let reopened_source = SourceLocation {
            path: PathBuf::from(format!(
                "{}!/map/scene.bin",
                fs::canonicalize(&archive_path).unwrap().display()
            )),
            offset: Some(32),
            length: Some(4),
        };
        base_object.source = Some(reopened_source.clone());
        base_object.insert_source_raw_param("name", "fresh base name");
        base_object.insert_source_raw_param("actor_tail_string", "FruitPapaya");
        base_object.asset_hints = vec![AssetRef {
            path: "stage.szs!/mapobj/fruitpapaya.bmd".to_string(),
            role: AssetRole::InferredPreviewModel,
        }];
        reopened.objects = vec![base_object];
        reopened.load_project_folder(&root).unwrap();
        assert_eq!(reopened.objects[0].source, Some(reopened_source));
        assert_eq!(reopened.objects[0].transform.translation[0], 42.0);
        assert_eq!(
            reopened.objects[0].raw_param("name"),
            Some("overlay-edited name")
        );
        assert_eq!(
            reopened.objects[0].raw_param("actor_tail_string"),
            Some("FruitPapaya")
        );
        assert_eq!(
            reopened.objects[0].asset_hints,
            [
                AssetRef {
                    path: "mods/user-preview.bmd".to_string(),
                    role: AssetRole::PreviewModel,
                },
                AssetRef {
                    path: "stage.szs!/mapobj/fruitpapaya.bmd".to_string(),
                    role: AssetRole::InferredPreviewModel,
                },
            ]
        );

        fs::remove_dir_all(root).unwrap();
        fs::remove_dir_all(source_root).unwrap();
    }

    #[test]
    fn project_overlay_reload_reattaches_shifted_source_by_semantic_placement() {
        let root = unique_test_project_root("shifted-source-record-round-trip");
        let source_root = unique_test_project_root("shifted-source-record-source");
        fs::create_dir_all(&source_root).unwrap();
        let archive_path = source_root.join("stage.szs");
        fs::write(&archive_path, []).unwrap();
        let source_path = PathBuf::from(format!(
            "{}!/map/scene.bin",
            fs::canonicalize(&archive_path).unwrap().display()
        ));
        let address = PlacementAddress {
            raw_resource_path: b"map/scene.bin".to_vec(),
            record_path: vec![5, 2, 4, 0],
        };

        let mut saved_object = SceneObject::new("retail-object", "Pollution");
        saved_object.source = Some(SourceLocation {
            path: source_path.clone(),
            offset: Some(2_390),
            length: Some(90),
        });
        saved_object.placement = Some(PlacementBinding::Existing(address.clone()));
        saved_object.transform.translation[0] = 42.0;
        let mut saved = empty_document("test01");
        saved.objects = vec![saved_object];
        saved.save_project_folder(&root).unwrap();

        let refreshed_source = SourceLocation {
            path: source_path,
            offset: Some(2_512),
            length: Some(90),
        };
        let mut base_object = SceneObject::new("retail-object", "Pollution");
        base_object.source = Some(refreshed_source.clone());
        base_object.placement = Some(PlacementBinding::Existing(address));
        let mut reopened = empty_document("test01");
        reopened.objects = vec![base_object];

        assert!(reopened.load_project_folder(&root).unwrap());
        assert_eq!(reopened.objects[0].source, Some(refreshed_source));
        assert_eq!(reopened.objects[0].transform.translation[0], 42.0);

        fs::remove_dir_all(root).unwrap();
        fs::remove_dir_all(source_root).unwrap();
    }

    #[test]
    fn version_two_project_sources_migrate_to_semantic_existing_and_clone_bindings() {
        let root = unique_test_project_root("v2-placement-migration");
        let source_root = unique_test_project_root("v2-placement-source");
        fs::create_dir_all(&source_root).unwrap();
        let archive_path = source_root.join("stage.szs");
        fs::write(&archive_path, []).unwrap();
        let source = SourceLocation {
            path: PathBuf::from(format!(
                "{}!/map/scene.bin",
                fs::canonicalize(&archive_path).unwrap().display()
            )),
            offset: Some(32),
            length: Some(16),
        };
        let address = PlacementAddress {
            raw_resource_path: b"map/scene.bin".to_vec(),
            record_path: vec![2, 4],
        };
        let mut original = SceneObject::new("retail-object", "Coin");
        original.source = Some(source.clone());
        original.placement = Some(PlacementBinding::Existing(address.clone()));
        original.insert_source_raw_param("name", "coin");
        let mut duplicate = original.clone();
        duplicate.id = "duplicate".to_string();
        duplicate.placement = Some(PlacementBinding::CloneOf(address.clone()));

        let mut saved = empty_document("dolpic");
        saved.objects = vec![original.clone(), duplicate];
        saved.save_project_folder(&root).unwrap();

        let overlay_path = root.join("files/editor/stages/dolpic.scene.json");
        let mut overlay: serde_json::Value =
            serde_json::from_slice(&fs::read(&overlay_path).unwrap()).unwrap();
        for object in overlay["objects"].as_array_mut().unwrap() {
            object.as_object_mut().unwrap().remove("placement");
        }
        overlay.as_object_mut().unwrap().remove("archive_edits");
        fs::write(&overlay_path, serde_json::to_vec_pretty(&overlay).unwrap()).unwrap();
        let manifest_path = root.join("sms-project.toml");
        let mut manifest: EditorProjectManifest =
            toml::from_str(&fs::read_to_string(&manifest_path).unwrap()).unwrap();
        manifest.format_version = 2;
        fs::write(&manifest_path, toml::to_string_pretty(&manifest).unwrap()).unwrap();

        let mut reopened = empty_document("dolpic");
        reopened.objects = vec![original];
        assert!(reopened.load_project_folder(&root).unwrap());
        assert_eq!(
            reopened.objects[0].placement,
            Some(PlacementBinding::Existing(address.clone()))
        );
        assert_eq!(
            reopened.objects[1].placement,
            Some(PlacementBinding::CloneOf(address))
        );
        assert_eq!(reopened.archive_edits, StageArchiveEdits::default());

        fs::remove_dir_all(root).unwrap();
        fs::remove_dir_all(source_root).unwrap();
    }

    #[test]
    fn project_save_always_persists_the_current_scene_overlay() {
        let root = unique_test_project_root("automatic-overlay");
        let mut document = empty_document("dolpic");
        document.objects = vec![SceneObject::new("saved-object", "Coin")];

        document.save_project_folder(&root).unwrap();

        let overlay: EditorSceneOverlay = serde_json::from_slice(
            &fs::read(root.join("files/editor/stages/dolpic.scene.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(overlay.objects, document.objects);
        assert_eq!(overlay.archive_edits, document.archive_edits);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn project_load_rejects_missing_managed_files() {
        let root = unique_test_project_root("missing-managed-file");
        let mut document = empty_document("dolpic");
        document.save_project_folder(&root).unwrap();
        fs::remove_file(root.join("files/editor/stages/dolpic.scene.json")).unwrap();

        let mut reopened = empty_document("dolpic");
        assert!(matches!(
            reopened.load_project_folder(&root).unwrap_err(),
            SceneError::UnsupportedProjectManifest { .. }
        ));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn project_load_bounds_manifest_reads() {
        let root = unique_test_project_root("oversized-manifest");
        let mut document = empty_document("dolpic");
        document.save_project_folder(&root).unwrap();
        let manifest_path = root.join("sms-project.toml");
        fs::write(&manifest_path, vec![b'x'; 1024 * 1024 + 1]).unwrap();

        let mut reopened = empty_document("dolpic");
        assert!(matches!(
            reopened.load_project_folder(&root).unwrap_err(),
            SceneError::ProjectFileTooLarge { path, .. } if path == manifest_path
        ));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn project_save_rejects_managed_content_changed_since_load() {
        let root = unique_test_project_root("managed-lost-update");
        let mut creator = empty_document("dolpic");
        creator.save_project_folder(&root).unwrap();

        let mut loaded = empty_document("dolpic");
        loaded.load_project_folder(&root).unwrap();
        fs::write(
            root.join("files/editor/stages/dolpic.scene.json"),
            br#"{"stage_id":"dolpic","objects":[]}"#,
        )
        .unwrap();

        assert!(matches!(
            loaded.save_project_folder(&root).unwrap_err(),
            SceneError::StaleProject(path) if path == root
        ));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn project_saves_multiple_stage_overlays_without_deleting_earlier_stages() {
        let root = unique_test_project_root("multiple-stages");
        let mut first = empty_document("dolpic0");
        first.objects = vec![SceneObject::new("dolpic-object", "Coin")];
        first.queue_editor_overlay_change().unwrap();
        first.save_project_folder(&root).unwrap();

        let mut second = empty_document("bianco0");
        assert!(!second.load_project_folder(&root).unwrap());
        second.objects = vec![SceneObject::new("bianco-object", "Coin")];
        second.queue_editor_overlay_change().unwrap();
        second.save_project_folder(&root).unwrap();

        assert!(root
            .join("files/editor/stages/dolpic0.scene.json")
            .is_file());
        assert!(root
            .join("files/editor/stages/bianco0.scene.json")
            .is_file());
        let manifest: EditorProjectManifest =
            toml::from_str(&fs::read_to_string(root.join("sms-project.toml")).unwrap()).unwrap();
        assert_eq!(manifest.changed_files.len(), 2);

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn existing_projects_must_be_loaded_and_reject_stale_writers() {
        let root = unique_test_project_root("stale-writer");
        let mut creator = empty_document("dolpic");
        creator.queue_editor_overlay_change().unwrap();
        creator.save_project_folder(&root).unwrap();

        let mut not_loaded = empty_document("dolpic");
        not_loaded.queue_editor_overlay_change().unwrap();
        assert!(matches!(
            not_loaded.save_project_folder(&root).unwrap_err(),
            SceneError::ProjectNotLoaded(path) if path == root
        ));

        let mut first_writer = empty_document("dolpic");
        first_writer.load_project_folder(&root).unwrap();
        let mut stale_writer = empty_document("dolpic");
        stale_writer.load_project_folder(&root).unwrap();

        first_writer.objects.push(SceneObject::new("first", "Coin"));
        first_writer.queue_editor_overlay_change().unwrap();
        first_writer.save_project_folder(&root).unwrap();

        stale_writer.objects.push(SceneObject::new("stale", "Coin"));
        stale_writer.queue_editor_overlay_change().unwrap();
        assert!(matches!(
            stale_writer.save_project_folder(&root).unwrap_err(),
            SceneError::StaleProject(path) if path == root
        ));

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn project_export_refuses_unowned_nonempty_directory() {
        let root = std::env::temp_dir().join(format!(
            "sms-editor-unowned-project-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("important.txt"), b"do not delete").unwrap();

        let mut doc = empty_document("dolpic");
        doc.mark_changed_file("first.bin", vec![1]).unwrap();
        assert!(matches!(
            doc.save_project_folder(&root).unwrap_err(),
            SceneError::UnownedProjectRoot(path) if path == root
        ));
        assert_eq!(
            fs::read(root.join("important.txt")).unwrap(),
            b"do not delete"
        );

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn project_export_rejects_a_manifest_that_redirects_managed_files() {
        let root = std::env::temp_dir().join(format!(
            "sms-editor-redirected-project-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let mut doc = empty_document("dolpic");
        doc.mark_changed_file("first.bin", vec![1]).unwrap();
        doc.save_project_folder(&root).unwrap();

        let manifest_path = root.join("sms-project.toml");
        let mut manifest: EditorProjectManifest =
            toml::from_str(&fs::read_to_string(&manifest_path).unwrap()).unwrap();
        manifest.project_files_path = PathBuf::from("elsewhere/files");
        fs::write(&manifest_path, toml::to_string_pretty(&manifest).unwrap()).unwrap();

        assert!(matches!(
            doc.save_project_folder(&root).unwrap_err(),
            SceneError::UnsupportedProjectManifest { .. }
        ));
        assert!(root.join("files/first.bin").exists());

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn project_export_refuses_to_take_over_an_unmanaged_file() {
        let root = std::env::temp_dir().join(format!(
            "sms-editor-unmanaged-file-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let mut doc = empty_document("dolpic");
        doc.mark_changed_file("owned.bin", vec![1]).unwrap();
        doc.save_project_folder(&root).unwrap();
        fs::write(root.join("files/user.bin"), b"user data").unwrap();

        doc.changed_files.clear();
        doc.mark_changed_file("user.bin", b"editor data".to_vec())
            .unwrap();
        assert!(matches!(
            doc.save_project_folder(&root).unwrap_err(),
            SceneError::UnmanagedProjectFileConflict(path)
                if path == root.join("files/user.bin")
        ));
        assert_eq!(fs::read(root.join("files/user.bin")).unwrap(), b"user data");

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn project_save_preserves_invalid_authoring_for_later_repair() {
        let root = std::env::temp_dir().join(format!(
            "sms-editor-invalid-project-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let mut doc = empty_document("dolpic");
        doc.objects.push(SceneObject::new("duplicate", "coin"));
        doc.objects.push(SceneObject::new("duplicate", "coin"));
        doc.queue_editor_overlay_change().unwrap();

        doc.save_project_folder(&root).unwrap();
        assert!(root.join("sms-project.toml").is_file());
        assert!(matches!(
            doc.validate_for_export().unwrap_err(),
            SceneError::ValidationFailed(_)
        ));

        let overlay: EditorSceneOverlay = serde_json::from_slice(
            &fs::read(root.join("files/editor/stages/dolpic.scene.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(overlay.objects.len(), 2);
        assert_eq!(overlay.objects[0].id, "duplicate");
        assert_eq!(overlay.objects[1].id, "duplicate");

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn project_load_normalizes_empty_route_authoring_when_ral_is_absent() {
        let root = unique_test_project_root("empty-route-authoring");
        let mut saved = empty_document("dolpic");
        saved.route_authoring = Some(RouteAuthoringDocument::lift(
            ROUTE_RESOURCE_PATH,
            &sms_formats::RalDocument::empty_canonical(),
        ));
        saved.save_project_folder(&root).unwrap();

        let mut reopened = empty_document("dolpic");
        assert!(reopened.load_project_folder(&root).unwrap());
        assert!(reopened.route_authoring.is_none());
        assert!(!reopened
            .validate()
            .iter()
            .any(|issue| issue.code == "route-authoring-resource-missing"));

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn project_export_rejects_base_directory_overlap() {
        let root = std::env::temp_dir().join(format!(
            "sms-editor-overlap-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let base_root = root.join("base");
        fs::create_dir_all(&base_root).unwrap();
        let mut document = empty_document("dolpic0");
        document.base_root = base_root.clone();

        assert!(matches!(
            document
                .save_project_folder(base_root.join("editor-project"))
                .unwrap_err(),
            SceneError::ProjectOverlapsBase(_)
        ));

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn placement_parse_failures_are_reported_as_validation_errors() {
        let root = std::env::temp_dir().join(format!(
            "sms-editor-load-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let scene_dir = root.join("data/scene/dolpic0/map");
        fs::create_dir_all(&scene_dir).unwrap();
        fs::write(scene_dir.join("scene.bin"), b"not a JDrama stream").unwrap();

        let document = StageDocument::open(&root, "dolpic0").unwrap();
        assert!(document
            .validate()
            .iter()
            .any(|issue| issue.code == "placement-parse-failed"
                && issue.severity == ValidationSeverity::Error));

        fs::remove_dir_all(root).unwrap();
    }

    fn empty_document(stage_id: &str) -> StageDocument {
        StageDocument {
            stage_id: stage_id.to_string(),
            base_root: PathBuf::from("."),
            assets: vec![],
            objects: vec![],
            changed_files: BTreeMap::new(),
            stage_archive: None,
            stage_archive_source_path: None,
            archive_edits: StageArchiveEdits::default(),
            registry: None,
            route_authoring: None,
            goop_authoring: None,
            dialogue_authoring: None,
            dialogue_library: ProjectDialogueLibrary::default(),
            load_issues: Vec::new(),
            lighting: StageLighting::default(),
            actor_previews: BTreeMap::new(),
            loaded_project: None,
        }
    }

    fn unique_test_project_root(label: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "sms-editor-{label}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
    }

    fn authored_stage_archive(
        target_slot: &str,
        spawn_x: f32,
        object_light_color: [u8; 4],
        parameter_value: i32,
    ) -> SourceFreeStageArchive {
        let field = |name: &str, value: JDramaFieldValue| JDramaField {
            name: name.to_string(),
            value,
        };
        let mut children = (0..6)
            .map(|index| {
                JDramaRecord::new(
                    "Light",
                    format!("light {index}"),
                    JDramaRecordPayload::Fields {
                        fields: vec![
                            field(
                                "position",
                                JDramaFieldValue::Vec3F32([index as f32, 2.0, 3.0]),
                            ),
                            field(
                                "color",
                                JDramaFieldValue::ColorRgba8(if index == 5 {
                                    object_light_color
                                } else {
                                    [index as u8; 4]
                                }),
                            ),
                            field("range", JDramaFieldValue::F32(50.0)),
                        ],
                    },
                )
                .unwrap()
            })
            .collect::<Vec<_>>();
        children.extend((0..3).map(|index| {
            JDramaRecord::new(
                "AmbColor",
                format!("ambient {index}"),
                JDramaRecordPayload::Fields {
                    fields: vec![field(
                        "color",
                        JDramaFieldValue::ColorRgba8(if index == 2 {
                            [40, 50, 60, 255]
                        } else {
                            [index as u8; 4]
                        }),
                    )],
                },
            )
            .unwrap()
        }));
        children.push(
            JDramaRecord::new(
                "Mario",
                "Mario",
                JDramaRecordPayload::Actor {
                    transform: sms_formats::JDramaTransform {
                        translation: [spawn_x, 20.0, 30.0],
                        rotation: [0.0, 90.0, 0.0],
                        scale: [1.0; 3],
                    },
                    character_name: "Mario Character".to_string(),
                    light_map: sms_formats::JDramaLightMap::default(),
                    fields: vec![
                        field("starting_water", JDramaFieldValue::U32(100)),
                        field("equipment_flags", JDramaFieldValue::U32(0)),
                    ],
                },
            )
            .unwrap(),
        );

        let mut archive = SourceFreeStageArchive::new_for_blank(target_slot, 1).unwrap();
        archive
            .insert_resource(
                b"map/scene.bin".to_vec(),
                StageResourceDocument::Placement(JDramaDocument {
                    root: JDramaRecord::new(
                        "GroupObj",
                        "root",
                        JDramaRecordPayload::Group {
                            fields: Vec::new(),
                            children,
                        },
                    )
                    .unwrap(),
                }),
            )
            .unwrap();
        archive
            .insert_resource(
                b"map/authored.prm".to_vec(),
                StageResourceDocument::Parameters(PrmFile {
                    entries: vec![sms_formats::PrmEntry::new(
                        "mNum",
                        sms_formats::PrmValue::I32(parameter_value),
                    )
                    .unwrap()],
                }),
            )
            .unwrap();
        archive
    }

    fn assert_authored_parameter_asset(document: &StageDocument, expected_value: i32) {
        let asset = document
            .assets
            .iter()
            .find(|asset| {
                asset
                    .path
                    .to_string_lossy()
                    .replace('\\', "/")
                    .ends_with("/map/authored.prm")
            })
            .expect("authored parameter asset");
        let parameters = PrmFile::parse(&document.read_asset_bytes(&asset.path).unwrap()).unwrap();
        assert_eq!(
            parameters.value("mNum"),
            Some(&sms_formats::PrmValue::I32(expected_value))
        );
    }

    #[test]
    fn resolves_object_models_from_primary_decomp_resource_bindings() {
        let mut registry = ObjectRegistry::default();
        registry
            .object_resources
            .push(sms_schema::ObjectResourceBinding {
                factory_name: "NPCFixture".to_string(),
                model_index: 0,
                role: sms_schema::ObjectResourceRole::Primary,
                model_name: "fixture_body.bmd".to_string(),
                resource_base: Some("/scene/fixture".to_string()),
                load_flags: 0x1030_0000,
                source_file: "src/NPC/Fixture.cpp".to_string(),
            });
        let assets = vec![StageAsset {
            path: PathBuf::from("stage.szs!/fixture/fixture_body.bmd"),
            kind: StageAssetKind::Model,
        }];
        let mut objects = vec![SceneObject::new("fixture", "NPCFixture")];

        assert!(apply_registry_preview_hints(&mut objects, &assets, &registry).is_empty());
        assert_eq!(
            objects[0].asset_hints,
            vec![AssetRef {
                path: "stage.szs!/fixture/fixture_body.bmd".to_string(),
                role: AssetRole::InferredPreviewModel,
            }]
        );
    }

    #[test]
    fn exact_map_obj_resource_outranks_factory_level_preview_bindings() {
        let mut registry = ObjectRegistry::default();
        registry.objects.push(schema_object(
            "MapObjBase",
            "TMapObjBase",
            "MapObj",
            Some("wrong_factory_model.bmd"),
        ));
        registry.map_obj_factories.push("MapObjBase".to_string());
        registry
            .map_obj_resources
            .push(sms_schema::MapObjResourceDefinition {
                resource_name: "WoodBox".to_string(),
                actor_type: 0x4000_0003,
                object_flags: 0,
                required_manager_name: "fixture map object manager".to_string(),
                has_hold_dependency: false,
                has_move_dependency: false,
                uses_resource_name_model_fallback: false,
                primary_model: Some("kibako.bmd".to_string()),
                animation_resources: Vec::new(),
                hold_model_path: None,
                move_bck_path: None,
                load_flags: 0x1022_0000,
                collision_resources: Vec::new(),
                source_file: "src/MoveBG/MapObjInit.cpp".to_string(),
            });
        let assets = vec![
            StageAsset {
                path: PathBuf::from("stage.szs!/mapobj/wrong_factory_model.bmd"),
                kind: StageAssetKind::Model,
            },
            StageAsset {
                path: PathBuf::from("stage.szs!/mapobj/kibako.bmd"),
                kind: StageAssetKind::Model,
            },
        ];
        let mut object = SceneObject::new("wood box", "MapObjBase");
        object.set_raw_param("actor_tail_string", "WoodBox");
        object.asset_hints.push(AssetRef {
            path: "stage.szs!/mapobj/woodbox.bmd".to_string(),
            role: AssetRole::InferredPreviewModel,
        });

        assert!(apply_registry_preview_hints(
            std::slice::from_mut(&mut object),
            &assets,
            &registry
        )
        .is_empty());
        assert_eq!(
            object.asset_hints,
            [AssetRef {
                path: "stage.szs!/mapobj/kibako.bmd".to_string(),
                role: AssetRole::InferredPreviewModel,
            }]
        );
    }

    #[test]
    fn typed_object_preview_flags_follow_exact_map_obj_and_map_static_resources() {
        let mut document = empty_document("fixture");
        document.registry = Some(ObjectRegistry {
            objects: vec![
                schema_object("MapObjBase", "TMapObjBase", "MapObj", None),
                schema_object("MapStaticObj", "TMapStaticObj", "MapObj", None),
            ],
            map_obj_factories: vec!["MapObjBase".to_string()],
            map_obj_resources: vec![sms_schema::MapObjResourceDefinition {
                resource_name: "DokanGate".to_string(),
                actor_type: 0x4000_0084,
                object_flags: 0,
                required_manager_name: "fixture map object manager".to_string(),
                has_hold_dependency: false,
                has_move_dependency: false,
                uses_resource_name_model_fallback: false,
                primary_model: Some("efDokanGate.bmd".to_string()),
                animation_resources: Vec::new(),
                hold_model_path: None,
                move_bck_path: None,
                load_flags: 0x1122_0000,
                collision_resources: Vec::new(),
                source_file: "src/MoveBG/MapObjInit.cpp".to_string(),
            }],
            map_static_models: vec![sms_schema::MapStaticModelDefinition {
                actor_name: "mareSeaPollutionS34567".to_string(),
                model_path: None,
                load_flags: 0x1021_0000,
                sound_id: None,
                source_file: "src/Map/MapStaticObject.cpp".to_string(),
                stage_bootstrap_created: false,
            }],
            ..ObjectRegistry::default()
        });
        let mut gate = SceneObject::new("gate", "MapObjBase");
        gate.set_raw_param("actor_tail_string", "DokanGate");
        let mut pollution = SceneObject::new("pollution", "MapStaticObj");
        pollution.set_raw_param("actor_tail_string", "mareSeaPollutionS34567");

        assert_eq!(document.object_preview_load_flags(&gate), Some(0x1122_0000));
        assert_eq!(
            document.object_preview_load_flags(&pollution),
            Some(0x1021_0000)
        );
        pollution.set_raw_param("actor_tail_string", "mareseapollutions34567");
        assert_eq!(document.object_preview_load_flags(&pollution), None);
    }

    #[test]
    fn named_object_models_follow_exact_runtime_name_selection() {
        let registry = ObjectRegistry {
            named_object_models: vec![sms_schema::NamedObjectModelDefinition {
                factory_name: "JellyGate".to_string(),
                object_name: "GateToRicco".to_string(),
                model_path: "/scene/map/map/gate/05_gate02rico.bmd".to_string(),
                load_flags: 0x1110_0000,
                source_file: "src/MoveBG/ModelGate.cpp".to_string(),
            }],
            ..ObjectRegistry::default()
        };
        let assets = vec![StageAsset {
            path: PathBuf::from("dolpic10.szs!/map/map/gate/05_gate02rico.bmd"),
            kind: StageAssetKind::Model,
        }];
        let mut gate = SceneObject::new("gate", "JellyGate");
        gate.insert_source_raw_param("name", "GateToRicco");

        assert!(
            apply_registry_preview_hints(std::slice::from_mut(&mut gate), &assets, &registry)
                .is_empty()
        );
        assert_eq!(
            gate.asset_hints,
            [AssetRef {
                path: "dolpic10.szs!/map/map/gate/05_gate02rico.bmd".to_string(),
                role: AssetRole::InferredPreviewModel,
            }]
        );
        let mut document = empty_document("dolpic10");
        document.registry = Some(registry);
        assert_eq!(document.object_preview_load_flags(&gate), Some(0x1110_0000));
    }

    #[test]
    fn model_less_map_obj_resource_authoritatively_clears_basename_guesses() {
        let registry = ObjectRegistry {
            objects: vec![schema_object("MapObjBase", "TMapObjBase", "MapObj", None)],
            map_obj_factories: vec!["MapObjBase".to_string()],
            map_obj_resources: vec![sms_schema::MapObjResourceDefinition {
                resource_name: "MapSmoke".to_string(),
                actor_type: 0x4000_0004,
                object_flags: 0,
                required_manager_name: "fixture map object manager".to_string(),
                has_hold_dependency: false,
                has_move_dependency: false,
                uses_resource_name_model_fallback: false,
                primary_model: None,
                animation_resources: Vec::new(),
                hold_model_path: None,
                move_bck_path: None,
                load_flags: 0x1022_0000,
                collision_resources: Vec::new(),
                source_file: "src/MoveBG/MapObjInit.cpp".to_string(),
            }],
            ..ObjectRegistry::default()
        };
        let mut object = SceneObject::new("smoke controller", "MapObjBase");
        object.set_raw_param("actor_tail_string", "MapSmoke");
        object.asset_hints.push(AssetRef {
            path: "stage.szs!/mapobj/mapsmoke.bmd".to_string(),
            role: AssetRole::InferredPreviewModel,
        });

        assert!(
            apply_registry_preview_hints(std::slice::from_mut(&mut object), &[], &registry)
                .is_empty()
        );
        assert!(object.asset_hints.is_empty());
    }

    #[test]
    fn exact_custom_model_override_replaces_a_zero_actor_resource() {
        let registry = ObjectRegistry {
            objects: vec![schema_object("SurfGesoRed", "TSurfGesoObj", "MapObj", None)],
            map_obj_factories: vec!["SurfGesoRed".to_string()],
            map_obj_resources: vec![sms_schema::MapObjResourceDefinition {
                resource_name: "SurfGesoRed".to_string(),
                actor_type: 0x4000_0005,
                object_flags: 0,
                required_manager_name: "fixture map object manager".to_string(),
                has_hold_dependency: false,
                has_move_dependency: false,
                uses_resource_name_model_fallback: false,
                primary_model: None,
                animation_resources: Vec::new(),
                hold_model_path: None,
                move_bck_path: None,
                load_flags: 0x1022_0000,
                collision_resources: Vec::new(),
                source_file: "src/MoveBG/MapObjInit.cpp".to_string(),
            }],
            map_obj_model_overrides: vec![sms_schema::MapObjModelOverrideDefinition {
                resource_name: "SurfGesoRed".to_string(),
                class_name: "TSurfGesoObj".to_string(),
                model_path: "/scene/mapObj/surfgeso.bmd".to_string(),
                load_flags: 0x1022_0000,
                tev_color: Some(sms_schema::MapObjTevColorDefinition {
                    register: 1,
                    color: [255, 180, 255, 255],
                }),
                binding_source_file: "src/MoveBG/MapObjRicco.cpp".to_string(),
                model_source_file: "src/MoveBG/MapObjManager.cpp".to_string(),
            }],
            ..ObjectRegistry::default()
        };
        let assets = [StageAsset {
            path: PathBuf::from("stage.szs!/mapobj/surfgeso.bmd"),
            kind: StageAssetKind::Model,
        }];
        let mut object = SceneObject::new("surf geso", "SurfGesoRed");
        object.set_raw_param("actor_tail_string", "SurfGesoRed");

        assert!(apply_registry_preview_hints(
            std::slice::from_mut(&mut object),
            &assets,
            &registry
        )
        .is_empty());
        assert_eq!(
            object.asset_hints,
            [AssetRef {
                path: "stage.szs!/mapobj/surfgeso.bmd".to_string(),
                role: AssetRole::InferredPreviewModel,
            }]
        );
    }

    #[test]
    fn registry_refresh_rebuilds_inferred_hints_before_applying_new_schema() {
        let assets = [StageAsset {
            path: PathBuf::from("stage.szs!/mapobj/registry_a.bmd"),
            kind: StageAssetKind::Model,
        }];
        let mut object = SceneObject::new("changing schema", "Fixture");
        object.set_raw_param("actor_tail_string", "NoBaselineModel");
        let registry_a = ObjectRegistry {
            objects: vec![schema_object(
                "Fixture",
                "TFixture",
                "Fixture",
                Some("registry_a.bmd"),
            )],
            ..ObjectRegistry::default()
        };

        assert!(apply_registry_preview_hints(
            std::slice::from_mut(&mut object),
            &assets,
            &registry_a
        )
        .is_empty());
        assert_eq!(
            object.asset_hints[0].path,
            "stage.szs!/mapobj/registry_a.bmd"
        );

        assert!(apply_registry_preview_hints(
            std::slice::from_mut(&mut object),
            &assets,
            &ObjectRegistry::default()
        )
        .is_empty());
        assert!(object.asset_hints.is_empty());
    }

    #[test]
    fn missing_authoritative_model_does_not_leave_a_basename_guess() {
        let registry = ObjectRegistry {
            objects: vec![schema_object("MapObjBase", "TMapObjBase", "MapObj", None)],
            map_obj_factories: vec!["MapObjBase".to_string()],
            map_obj_resources: vec![sms_schema::MapObjResourceDefinition {
                resource_name: "WoodBox".to_string(),
                actor_type: 0x4000_0003,
                object_flags: 0,
                required_manager_name: "fixture map object manager".to_string(),
                has_hold_dependency: false,
                has_move_dependency: false,
                uses_resource_name_model_fallback: false,
                primary_model: Some("kibako.bmd".to_string()),
                animation_resources: Vec::new(),
                hold_model_path: None,
                move_bck_path: None,
                load_flags: 0x1022_0000,
                collision_resources: Vec::new(),
                source_file: "src/MoveBG/MapObjInit.cpp".to_string(),
            }],
            ..ObjectRegistry::default()
        };
        let mut object = SceneObject::new("wood box", "MapObjBase");
        object.set_raw_param("actor_tail_string", "WoodBox");
        object.asset_hints.push(AssetRef {
            path: "stage.szs!/mapobj/woodbox.bmd".to_string(),
            role: AssetRole::InferredPreviewModel,
        });

        let issues =
            apply_registry_preview_hints(std::slice::from_mut(&mut object), &[], &registry);
        assert!(object.asset_hints.is_empty());
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].code, "object-preview-model-unresolved");
    }

    #[test]
    fn map_static_preview_bindings_use_exact_actor_tail_resource_identity() {
        let mut registry = ObjectRegistry::default();
        registry.objects.push(schema_object(
            "MapStaticObj",
            "TMapStaticObj",
            "MapObj",
            Some("wrong_factory_model.bmd"),
        ));
        registry
            .map_obj_resources
            .push(sms_schema::MapObjResourceDefinition {
                resource_name: "MareGate".to_string(),
                actor_type: 0x4000_0006,
                object_flags: 0,
                required_manager_name: "fixture map object manager".to_string(),
                has_hold_dependency: false,
                has_move_dependency: false,
                uses_resource_name_model_fallback: false,
                primary_model: Some("wrong_map_obj_model.bmd".to_string()),
                animation_resources: Vec::new(),
                hold_model_path: None,
                move_bck_path: None,
                load_flags: 0x1022_0000,
                collision_resources: Vec::new(),
                source_file: "src/MoveBG/MapObjInit.cpp".to_string(),
            });
        registry
            .map_static_models
            .push(sms_schema::MapStaticModelDefinition {
                actor_name: "MareGate".to_string(),
                model_path: Some("/scene/map/map/mare_gate_model.bmd".to_string()),
                load_flags: 0,
                sound_id: None,
                source_file: "fixture.cpp".to_string(),
                stage_bootstrap_created: false,
            });
        let assets = vec![
            StageAsset {
                path: PathBuf::from("stage.szs!/map/map/mare_gate_model.bmd"),
                kind: StageAssetKind::Model,
            },
            StageAsset {
                path: PathBuf::from("stage.szs!/mapobj/wrong_factory_model.bmd"),
                kind: StageAssetKind::Model,
            },
            StageAsset {
                path: PathBuf::from("stage.szs!/mapobj/wrong_map_obj_model.bmd"),
                kind: StageAssetKind::Model,
            },
        ];
        let mut exact = SceneObject::new("exact actor tail", "MapStaticObj");
        exact.set_raw_param("actor_tail_string", "MareGate");
        let mut wrong_case = SceneObject::new("wrong-case actor tail", "MapStaticObj");
        wrong_case.set_raw_param("actor_tail_string", "maregate");
        let mut objects = vec![exact, wrong_case];

        assert!(apply_registry_preview_hints(&mut objects, &assets, &registry).is_empty());
        assert_eq!(
            objects[0].asset_hints,
            [AssetRef {
                path: "stage.szs!/map/map/mare_gate_model.bmd".to_string(),
                role: AssetRole::InferredPreviewModel,
            }]
        );
        assert_eq!(
            objects[1].asset_hints,
            [AssetRef {
                path: "stage.szs!/mapobj/wrong_factory_model.bmd".to_string(),
                role: AssetRole::InferredPreviewModel,
            }]
        );
    }

    #[test]
    fn non_map_obj_actor_tail_collision_keeps_its_typed_factory_binding() {
        let registry = ObjectRegistry {
            objects: vec![schema_object("NPCFixture", "TBaseNPC", "NPC", None)],
            object_resources: vec![sms_schema::ObjectResourceBinding {
                factory_name: "NPCFixture".to_string(),
                model_index: 0,
                role: sms_schema::ObjectResourceRole::Primary,
                model_name: "npc_fixture.bmd".to_string(),
                resource_base: None,
                load_flags: 0,
                source_file: "src/NPC/Fixture.cpp".to_string(),
            }],
            map_obj_resources: vec![sms_schema::MapObjResourceDefinition {
                resource_name: "WoodBox".to_string(),
                actor_type: 0x4000_0003,
                object_flags: 0,
                required_manager_name: "fixture map object manager".to_string(),
                has_hold_dependency: false,
                has_move_dependency: false,
                uses_resource_name_model_fallback: false,
                primary_model: Some("kibako.bmd".to_string()),
                animation_resources: Vec::new(),
                hold_model_path: None,
                move_bck_path: None,
                load_flags: 0x1022_0000,
                collision_resources: Vec::new(),
                source_file: "src/MoveBG/MapObjInit.cpp".to_string(),
            }],
            ..ObjectRegistry::default()
        };
        let assets = vec![
            StageAsset {
                path: PathBuf::from("stage.szs!/npc/npc_fixture.bmd"),
                kind: StageAssetKind::Model,
            },
            StageAsset {
                path: PathBuf::from("stage.szs!/mapobj/kibako.bmd"),
                kind: StageAssetKind::Model,
            },
        ];
        let mut object = SceneObject::new("npc collision", "NPCFixture");
        object.set_raw_param("actor_tail_string", "WoodBox");

        assert!(apply_registry_preview_hints(
            std::slice::from_mut(&mut object),
            &assets,
            &registry
        )
        .is_empty());
        assert_eq!(object.asset_hints[0].path, "stage.szs!/npc/npc_fixture.bmd");
    }

    #[test]
    #[ignore = "requires the extracted retail game and neighboring SMS decomp"]
    fn audits_all_retail_enemy_and_boss_previews() {
        let base_root = std::env::var_os("SMS_BASE_ROOT")
            .map(PathBuf::from)
            .expect("set SMS_BASE_ROOT to the extracted game's root");
        let decomp_root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../..");
        let registry = sms_schema::SchemaGenerator::new(decomp_root)
            .generate()
            .expect("generate enemy schema");
        let boss_factories = registry
            .enemy_actors
            .iter()
            .filter(|actor| {
                registry
                    .find_object(&actor.factory_name)
                    .is_some_and(|object| object.category == "Boss")
            })
            .map(|actor| actor.factory_name.clone())
            .collect::<BTreeSet<_>>();
        assert_eq!(boss_factories.len(), 21, "unexpected boss schema coverage");
        let archives = sms_formats::discover_scene_archives(&base_root)
            .expect("discover retail scene archives");
        assert_eq!(archives.len(), 107, "unexpected retail scene archive count");
        let mut placed = BTreeMap::<String, usize>::new();
        let mut rendered = BTreeMap::<String, usize>::new();
        let mut preview_models = BTreeMap::<(String, u32), String>::new();
        let mut factory_previews = BTreeSet::new();
        let mut boss_factory_previews = BTreeSet::new();
        let mut hino_model_paths = BTreeSet::new();
        let mut named_emario_count = 0;

        for archive in archives {
            let assets = sms_formats::mount_scene_archive(&archive.path)
                .unwrap_or_else(|error| panic!("mount {}: {error}", archive.path.display()));
            hino_model_paths.extend(
                assets
                    .iter()
                    .filter(|asset| asset.kind == StageAssetKind::Model)
                    .map(|asset| asset.path.to_string_lossy().replace('\\', "/"))
                    .filter(|path| path.to_ascii_lowercase().contains("hinokuri2")),
            );
            let (catalog, issues) =
                build_actor_preview_catalog(&base_root, &assets, &registry, |path| {
                    read_stage_asset_bytes(path).map_err(|error| error.to_string())
                });
            assert!(
                issues.is_empty(),
                "enemy preview issues in {}: {issues:?}",
                archive.stage_id
            );
            for actor in &registry.enemy_actors {
                let factory = &actor.factory_name;
                if let Some(preview) = catalog.get(&actor_preview_factory_key(factory)) {
                    factory_previews.insert(factory.clone());
                    if boss_factories.contains(factory) {
                        boss_factory_previews.insert(factory.clone());
                    }
                    preview_models
                        .entry((preview.model_path.clone(), preview.load_flags))
                        .or_insert_with(|| factory.clone());
                }
            }
            for asset in assets.iter().filter(|asset| {
                asset.kind == StageAssetKind::Placement
                    && asset
                        .path
                        .to_string_lossy()
                        .replace('\\', "/")
                        .to_ascii_lowercase()
                        .ends_with("/map/scene.bin")
            }) {
                let bytes = read_stage_asset_bytes(&asset.path).expect("read placement");
                let records = parse_jdrama_object_records(&bytes).expect("parse placement");
                for record in
                    records.iter().filter(|record| {
                        record.transform.is_some()
                            && registry
                                .find_object(&record.type_name)
                                .is_some_and(|object| {
                                    matches!(object.category.as_str(), "Enemy" | "Boss")
                                        && !object.class_name.rsplit("::").next().is_some_and(
                                            |class_name| class_name.ends_with("Manager"),
                                        )
                                })
                    })
                {
                    assert!(
                        registry.find_enemy_actor(&record.type_name).is_some(),
                        "missing enemy actor schema for placed {}",
                        record.type_name
                    );
                    *placed.entry(record.type_name.clone()).or_default() += 1;
                    let source = SourceLocation {
                        path: asset.path.clone(),
                        offset: Some(record.offset as u64),
                        length: Some(record.size as u64),
                    };
                    let preview =
                        actor_preview_source_key(&source).and_then(|key| catalog.get(&key));
                    if let Some(preview) = preview {
                        *rendered.entry(record.type_name.clone()).or_default() += 1;
                        preview_models
                            .entry((preview.model_path.clone(), preview.load_flags))
                            .or_insert_with(|| record.type_name.clone());
                        if record.type_name == "EMario"
                            && record.object_name.as_deref() == Some("モンテマン")
                        {
                            assert!(preview
                                .model_path
                                .replace('\\', "/")
                                .to_ascii_lowercase()
                                .ends_with("/map/map/pad/monteman_model.bmd"));
                            assert_eq!(preview.load_flags, 0x1004_0000);
                            named_emario_count += 1;
                        } else if record.type_name == "EMario" {
                            assert!(preview
                                .model_path
                                .replace('\\', "/")
                                .to_ascii_lowercase()
                                .ends_with("/kagemario/default.bmd"));
                            assert_eq!(preview.load_flags, 0x1130_0000);
                        }
                        if record.type_name == "MarioModokiTelesa" {
                            let actor = registry.find_enemy_actor(&record.type_name).unwrap();
                            let index = record
                                .mario_modoki_telesa_imitation_index
                                .expect("typed imitation selector");
                            let expected = actor
                                .indexed_models
                                .iter()
                                .find(|model| model.index == index)
                                .or_else(|| {
                                    actor.indexed_models.iter().find(|model| model.index == 0)
                                })
                                .unwrap();
                            assert!(preview
                                .model_path
                                .replace('\\', "/")
                                .to_ascii_lowercase()
                                .ends_with(
                                    expected
                                        .model_path
                                        .trim_start_matches("/scene/")
                                        .to_ascii_lowercase()
                                        .as_str()
                                ));
                            assert_eq!(preview.load_flags, expected.load_flags);
                        }
                    }
                }
            }
        }

        assert_eq!(
            placed.len(),
            70,
            "unexpected placed enemy/boss factory count"
        );

        let unresolved = placed
            .iter()
            .filter_map(|(factory, count)| {
                let resolved = rendered.get(factory).copied().unwrap_or(0);
                (resolved != *count).then_some((factory.clone(), *count, resolved))
            })
            .collect::<Vec<_>>();
        eprintln!(
            "enemy preview factories: {} placed, {} fully resolved; unresolved={unresolved:?}",
            placed.len(),
            placed.len() - unresolved.len()
        );
        assert_eq!(
            unresolved
                .iter()
                .map(|(factory, _, _)| factory.as_str())
                .collect::<Vec<_>>(),
            ["EffectBiancoFunsui", "EffectPinnaFunsui"],
            "only the particle-only fountain actors may lack models"
        );
        assert_eq!(rendered.get("EMario"), placed.get("EMario"));
        assert_eq!(named_emario_count, 3);
        for factory in placed.keys().filter(|factory| {
            !matches!(factory.as_str(), "EffectBiancoFunsui" | "EffectPinnaFunsui")
        }) {
            assert!(
                factory_previews.contains(factory),
                "placed enemy {factory} lacks a source-less factory preview"
            );
        }
        for factory in ["EggGenerator", "WickedEggGenerator"] {
            assert!(
                factory_previews.contains(factory),
                "runtime enemy {factory} lacks a source-less factory preview"
            );
        }
        let missing_actor_factory_previews = registry
            .enemy_actors
            .iter()
            .map(|actor| actor.factory_name.clone())
            .filter(|factory| !factory_previews.contains(factory))
            .collect::<Vec<_>>();
        assert_eq!(
            missing_actor_factory_previews,
            [
                "EffectBiancoFunsui",
                "EffectEnemy",
                "EffectPinnaFunsui",
                "HinoKuri2",
                "KageMarioModoki",
                "NamekuriLauncher",
            ],
            "only particle-only or registered-but-unshipped enemy factories may lack a retail preview"
        );

        let unplaced_bosses = boss_factories
            .iter()
            .filter(|factory| !placed.contains_key(*factory))
            .cloned()
            .collect::<Vec<_>>();
        assert_eq!(
            unplaced_bosses.len(),
            6,
            "unexpected runtime-created boss coverage: {unplaced_bosses:?}"
        );
        assert!(
            hino_model_paths.is_empty(),
            "HinoKuri2 unexpectedly has retail model resources: {hino_model_paths:?}"
        );
        let missing_boss_previews = boss_factories
            .difference(&boss_factory_previews)
            .cloned()
            .collect::<Vec<_>>();
        assert_eq!(
            missing_boss_previews,
            ["HinoKuri2"],
            "only the unshipped HinoKuri2 factory may lack a retail preview"
        );

        for (factory, count) in &placed {
            let is_boss = registry
                .find_object(factory)
                .is_some_and(|object| object.category == "Boss");
            if is_boss {
                assert_eq!(
                    rendered.get(factory),
                    Some(count),
                    "unresolved boss {factory}"
                );
            }
        }

        for ((model_path, load_flags), factory) in preview_models {
            let bytes = read_stage_asset_bytes(&model_path).unwrap_or_else(|error| {
                panic!("read {factory} preview {model_path} ({load_flags:#010x}): {error}")
            });
            let file = sms_formats::J3dFile::parse(&bytes)
                .unwrap_or_else(|error| panic!("parse {factory} preview {model_path}: {error}"));
            let geometry = file
                .geometry_preview_with_loader_flags(load_flags)
                .unwrap_or_else(|error| {
                    panic!("prepare {factory} preview {model_path} ({load_flags:#010x}): {error}")
                });
            assert!(
                !geometry.triangles.is_empty(),
                "empty {factory} preview {model_path} ({load_flags:#010x})"
            );
        }
    }

    #[test]
    fn resolves_enemy_model_from_exact_chara_folder_and_decomp_model_name() {
        let models = vec![
            (
                "stage.szs!/gatekeeper/gene_pakkun_model1.bmd".to_string(),
                "genepakkunmodel1".to_string(),
            ),
            (
                "stage.szs!/gatekeeper/stamp_keeper_model1.bmd".to_string(),
                "stampkeepermodel1".to_string(),
            ),
        ];
        assert_eq!(
            resolve_chara_model_path("/scene/gatekeeper", "gene_pakkun_model1.bmd", &models)
                .as_deref(),
            Some("stage.szs!/gatekeeper/gene_pakkun_model1.bmd")
        );
    }

    #[test]
    fn runtime_uniform_scale_comes_from_named_parameter_asset_values() {
        let root = unique_test_project_root("runtime-enemy-scale");
        let path = root.join("map/params/enemy/fixture.prm");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let mut bytes = 2u32.to_be_bytes().to_vec();
        for (name, value) in [("mSLBodyScaleLow", 0.75f32), ("mSLBodyScaleHigh", 1.25f32)] {
            bytes.extend_from_slice(&sms_formats::jdrama_key_code(name).unwrap().to_be_bytes());
            bytes.extend_from_slice(&(name.len() as u16).to_be_bytes());
            bytes.extend_from_slice(name.as_bytes());
            bytes.extend_from_slice(&4u32.to_be_bytes());
            bytes.extend_from_slice(&value.to_bits().to_be_bytes());
        }
        fs::write(&path, bytes).unwrap();
        let actor = EnemyActorDefinition {
            factory_name: "Fixture".to_string(),
            class_name: "TFixture".to_string(),
            model_index: None,
            fallback_models: Vec::new(),
            primary_model: None,
            named_models: Vec::new(),
            indexed_models: Vec::new(),
            manager_factories: vec!["FixtureManager".to_string()],
            runtime_uniform_scale: Some(sms_schema::EnemyRuntimeUniformScaleDefinition {
                low_parameter: "mSLBodyScaleLow".to_string(),
                high_parameter: "mSLBodyScaleHigh".to_string(),
                source_file: "src/Enemy/fixture.cpp".to_string(),
            }),
        };
        let manager = EnemyManagerDefinition {
            factory_name: "FixtureManager".to_string(),
            class_name: "TFixtureManager".to_string(),
            model_index: None,
            spawned_actor_class: Some("TFixture".to_string()),
            parameter_path: Some("/enemy/fixture.prm".to_string()),
            models: Vec::new(),
        };
        let assets = [StageAsset {
            path: path.clone(),
            kind: StageAssetKind::Placement,
        }];

        assert_eq!(
            enemy_runtime_uniform_scale(&actor, &manager, &assets, &mut |path| {
                read_stage_asset_bytes(path).map_err(|error| error.to_string())
            })
            .unwrap(),
            Some(1.0)
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    #[ignore = "requires the extracted retail game and neighboring SMS decomp"]
    fn retail_mare_cannon_uses_its_post_reset_parameter_scale() {
        let base_root = std::env::var_os("SMS_BASE_ROOT")
            .map(PathBuf::from)
            .expect("set SMS_BASE_ROOT to the extracted game's root");
        let decomp_root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../..");
        let registry = sms_schema::SchemaGenerator::new(decomp_root)
            .generate()
            .expect("generate enemy schema");
        let document = StageDocument::open(&base_root, "mare0")
            .expect("open mare0")
            .with_registry(registry);
        let cannon = document
            .objects
            .iter()
            .find(|object| object.factory_name == "Cannon")
            .expect("mare0 cannon placement");

        assert_eq!(cannon.transform.scale, [1.0, 5.0, 1.0]);
        assert_eq!(
            document
                .actor_preview(cannon)
                .and_then(|preview| preview.runtime_uniform_scale),
            Some(1.0)
        );
        assert!(
            document
                .load_issues
                .iter()
                .all(|issue| issue.code != "enemy-preview-runtime-scale-unresolved"),
            "unexpected runtime-scale issues: {:?}",
            document.load_issues
        );
    }

    #[test]
    fn actor_default_model_flags_override_the_manager_table() {
        let actor = EnemyActorDefinition {
            factory_name: "EMario".to_string(),
            class_name: "TEMario".to_string(),
            model_index: None,
            fallback_models: vec![sms_schema::EnemyModelDefinition {
                model_name: "default.bmd".to_string(),
                load_flags: 0x1130_0000,
                source_file: "src/Enemy/emario.cpp".to_string(),
            }],
            primary_model: None,
            named_models: Vec::new(),
            indexed_models: Vec::new(),
            manager_factories: vec!["EMarioManager".to_string()],
            runtime_uniform_scale: None,
        };
        let manager = EnemyManagerDefinition {
            factory_name: "EMarioManager".to_string(),
            class_name: "TEMarioManager".to_string(),
            model_index: None,
            spawned_actor_class: Some("TEMario".to_string()),
            parameter_path: None,
            models: vec![sms_schema::EnemyModelDefinition {
                model_name: "default.bmd".to_string(),
                load_flags: 0x1021_0000,
                source_file: "src/Strategic/ObjModel.cpp".to_string(),
            }],
        };
        let models = vec![(
            "stage.szs!/kagemario/default.bmd".to_string(),
            "kagemariodefault".to_string(),
        )];

        let preview =
            resolve_manager_actor_preview(&actor, &manager, "/scene/kagemario", &models).unwrap();

        assert_eq!(preview.model_path, "stage.szs!/kagemario/default.bmd");
        assert_eq!(preview.load_flags, 0x1130_0000);
        assert_eq!(preview.manager_factory, "EMarioManager");
    }

    #[test]
    fn indexed_enemy_variant_uses_its_decomp_model_slot() {
        let actor = EnemyActorDefinition {
            factory_name: "ButterflyC".to_string(),
            class_name: "TButterfloid".to_string(),
            model_index: Some(2),
            fallback_models: Vec::new(),
            primary_model: None,
            named_models: Vec::new(),
            indexed_models: Vec::new(),
            manager_factories: vec!["ButterflyManager".to_string()],
            runtime_uniform_scale: None,
        };
        let manager = EnemyManagerDefinition {
            factory_name: "ButterflyManager".to_string(),
            class_name: "TButterfloidManager".to_string(),
            model_index: None,
            spawned_actor_class: None,
            parameter_path: None,
            models: ["butterflyA.bmd", "butterflyB.bmd", "butterflyC.bmd"]
                .into_iter()
                .map(|model_name| sms_schema::EnemyModelDefinition {
                    model_name: model_name.to_string(),
                    load_flags: 0x1021_0000,
                    source_file: "src/Animal/Butterfly.cpp".to_string(),
                })
                .collect(),
        };
        let models = ["butterflyA.bmd", "butterflyB.bmd", "butterflyC.bmd"]
            .into_iter()
            .map(|model_name| {
                (
                    format!("stage.szs!/butterfly/{model_name}"),
                    normalize_model_key(model_name),
                )
            })
            .collect::<Vec<_>>();

        let preview =
            resolve_manager_actor_preview(&actor, &manager, "/scene/butterfly", &models).unwrap();

        assert_eq!(preview.model_path, "stage.szs!/butterfly/butterflyC.bmd");
        assert_eq!(preview.load_flags, 0x1021_0000);
    }

    #[test]
    fn managerless_runtime_boss_reuses_an_exact_manager_model_table() {
        let actor = EnemyActorDefinition {
            factory_name: "LimitKoopa".to_string(),
            class_name: "TLimitKoopa".to_string(),
            model_index: None,
            fallback_models: Vec::new(),
            primary_model: None,
            named_models: Vec::new(),
            indexed_models: Vec::new(),
            manager_factories: vec!["LimitKoopaManager".to_string()],
            runtime_uniform_scale: None,
        };
        let target_manager = EnemyManagerDefinition {
            factory_name: "LimitKoopaManager".to_string(),
            class_name: "TLimitKoopaManager".to_string(),
            model_index: None,
            spawned_actor_class: None,
            parameter_path: None,
            models: vec![sms_schema::EnemyModelDefinition {
                model_name: "koopa_model.bmd".to_string(),
                load_flags: 0x1424_0000,
                source_file: "src/Enemy/limitkoopa.cpp".to_string(),
            }],
        };
        let stage_manager = EnemyManagerDefinition {
            factory_name: "KoopaManager".to_string(),
            class_name: "TKoopaManager".to_string(),
            model_index: None,
            spawned_actor_class: None,
            parameter_path: None,
            models: vec![sms_schema::EnemyModelDefinition {
                model_name: "koopa_model.bmd".to_string(),
                load_flags: 0x1424_0000,
                source_file: "src/Enemy/koopa.cpp".to_string(),
            }],
        };
        let models = vec![(
            "coronaBoss.szs!/koopa/koopa_model.bmd".to_string(),
            "koopakoopamodel".to_string(),
        )];

        assert!(manager_model_tables_are_aliases(
            &target_manager,
            &stage_manager
        ));
        let preview =
            resolve_manager_actor_preview(&actor, &target_manager, "/scene/koopa", &models)
                .unwrap();
        assert_eq!(preview.model_path, "coronaBoss.szs!/koopa/koopa_model.bmd");
        assert_eq!(preview.load_flags, 0x1424_0000);

        let mut generic_target = target_manager.clone();
        generic_target.models[0].model_name = "default.bmd".to_string();
        let mut generic_stage = stage_manager;
        generic_stage.models[0].model_name = "default.bmd".to_string();
        assert!(!manager_model_tables_are_aliases(
            &generic_target,
            &generic_stage
        ));
    }

    #[test]
    fn actor_primary_model_is_not_substituted_with_a_base_manager_model() {
        let actor = EnemyActorDefinition {
            factory_name: "HaneHamuKuri2".to_string(),
            class_name: "THaneHamuKuri2".to_string(),
            model_index: None,
            fallback_models: Vec::new(),
            primary_model: Some("hanekuri.bmd".to_string()),
            named_models: Vec::new(),
            indexed_models: Vec::new(),
            manager_factories: vec!["HaneHamuKuriManager".to_string()],
            runtime_uniform_scale: None,
        };
        let base_manager = EnemyManagerDefinition {
            factory_name: "HamuKuriManager".to_string(),
            class_name: "THamuKuriManager".to_string(),
            model_index: None,
            spawned_actor_class: Some("THamuKuri".to_string()),
            parameter_path: None,
            models: vec![sms_schema::EnemyModelDefinition {
                model_name: "default.bmd".to_string(),
                load_flags: 0x1022_0000,
                source_file: "src/Enemy/hamukuri.cpp".to_string(),
            }],
        };
        let models = vec![(
            "stage.szs!/hamukuri/default.bmd".to_string(),
            "hamukuridefault".to_string(),
        )];

        assert!(
            resolve_manager_actor_preview(&actor, &base_manager, "/scene/hamukuri", &models)
                .is_none()
        );
    }

    #[test]
    fn source_less_spawned_enemy_reuses_stage_factory_preview() {
        let mut document = empty_document("bianco0");
        document.actor_previews.insert(
            actor_preview_factory_key("HamuKuri"),
            ActorPreview {
                model_path: "bianco0.szs!/hamukuri/default.bmd".to_string(),
                load_flags: 0x1022_0000,
                manager_factory: "HamuKuriManager".to_string(),
                runtime_uniform_scale: None,
            },
        );
        let spawned = SceneObject::new("spawned", "HamuKuri");

        let preview = document.actor_preview(&spawned).unwrap();
        assert_eq!(preview.model_path, "bianco0.szs!/hamukuri/default.bmd");
        assert_eq!(preview.load_flags, 0x1022_0000);
    }

    #[test]
    fn replacing_registry_rebuilds_catalog_and_replaces_catalog_issues() {
        let mut document = empty_document("bianco0");
        document.actor_previews.insert(
            actor_preview_factory_key("HamuKuri"),
            ActorPreview {
                model_path: "stale.bmd".to_string(),
                load_flags: 0,
                manager_factory: "stale".to_string(),
                runtime_uniform_scale: None,
            },
        );
        document.load_issues.push(ValidationIssue::warning(
            "enemy-preview-stale",
            "old catalog issue",
        ));
        document
            .load_issues
            .push(ValidationIssue::warning("unrelated", "keep me"));

        document.set_registry(ObjectRegistry::default());

        assert!(document.actor_previews.is_empty());
        assert_eq!(document.load_issues.len(), 1);
        assert_eq!(document.load_issues[0].code, "unrelated");
    }

    #[test]
    fn ambiguous_authored_model_bindings_are_reported_instead_of_guessed() {
        let mut registry = ObjectRegistry::default();
        registry
            .object_resources
            .push(sms_schema::ObjectResourceBinding {
                factory_name: "NPCFixture".to_string(),
                model_index: 0,
                role: sms_schema::ObjectResourceRole::Primary,
                model_name: "fixture.bmd".to_string(),
                resource_base: None,
                load_flags: 0,
                source_file: "fixture.cpp".to_string(),
            });
        let assets = vec![
            StageAsset {
                path: PathBuf::from("stage.szs!/first/fixture.bmd"),
                kind: StageAssetKind::Model,
            },
            StageAsset {
                path: PathBuf::from("stage.szs!/second/fixture.bmd"),
                kind: StageAssetKind::Model,
            },
        ];
        let mut objects = vec![SceneObject::new("fixture", "NPCFixture")];

        let issues = apply_registry_preview_hints(&mut objects, &assets, &registry);
        assert!(objects[0].asset_hints.is_empty());
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].code, "object-preview-model-ambiguous");
    }

    #[test]
    fn registry_refresh_replaces_stale_unknown_class_metadata() {
        let mut registry = ObjectRegistry::default();
        registry.objects.push(sms_schema::ObjectDefinition {
            factory_name: "Mario".to_string(),
            class_name: "TMario".to_string(),
            category: "System".to_string(),
            source: sms_schema::SchemaSource::MarNameRefGen,
            display_name: None,
            preview_model: None,
            hidden: false,
            unsafe_to_edit: false,
        });
        let mut objects = vec![SceneObject::new("player", "Mario")];
        objects[0].class_name = Some("Unknown".to_string());

        assert!(apply_registry_preview_hints(&mut objects, &[], &registry).is_empty());
        assert_eq!(objects[0].class_name.as_deref(), Some("TMario"));
    }

    #[test]
    fn map_obj_base_uses_the_resource_basename_stored_in_its_placement_stream() {
        let models = vec![(
            "stage.szs!/mapobj/stagefixture.bmd".to_string(),
            "stagefixture".to_string(),
        )];
        let mut object = SceneObject::new("generic map object", "MapObjBase");
        object.set_raw_param("stream_string_0", "StageFixture");

        assert_eq!(
            infer_preview_model_path(&object, &models).as_deref(),
            Some("stage.szs!/mapobj/stagefixture.bmd")
        );
    }

    #[test]
    fn map_obj_tail_selector_wins_over_the_shared_actor_character() {
        let models = vec![
            (
                "stage.szs!/mapobj/sandbombbasemushroom.bmd".to_string(),
                "sandbombbasemushroom".to_string(),
            ),
            (
                "stage.szs!/mapobj/sandbombbasepyramid.bmd".to_string(),
                "sandbombbasepyramid".to_string(),
            ),
            (
                "stage.szs!/mapobj/fruitpapaya.bmd".to_string(),
                "fruitpapaya".to_string(),
            ),
        ];

        let mut sand = SceneObject::new("SandBombBasePyramid", "SandBombBase");
        sand.set_raw_param("stream_string_0", "SandBombBaseMushroom character");
        sand.set_raw_param("actor_tail_string", "SandBombBasePyramid");
        assert_eq!(
            infer_preview_model_path(&sand, &models).as_deref(),
            Some("stage.szs!/mapobj/sandbombbasepyramid.bmd")
        );

        let mut fruit = SceneObject::new("papaya", "ResetFruit");
        fruit.set_raw_param("stream_string_0", "shared fruit character");
        fruit.set_raw_param("actor_tail_string", "FruitPapaya");
        assert_eq!(
            infer_preview_model_path(&fruit, &models).as_deref(),
            Some("stage.szs!/mapobj/fruitpapaya.bmd")
        );
    }

    #[test]
    fn preview_inference_does_not_fold_semantic_factory_identity_into_model_names() {
        let models = vec![(
            "stage.szs!/mapobj/maregate.bmd".to_string(),
            "maregate".to_string(),
        )];
        let object = SceneObject::new("case-distinct factory", "MareGate");

        assert!(infer_preview_model_path(&object, &models).is_none());
    }

    #[test]
    fn shimmer_uses_the_model_basename_stored_in_its_placement_stream() {
        let models = vec![
            (
                "stage.szs!/mapobj/shimmerhi.bmd".to_string(),
                "shimmerhi".to_string(),
            ),
            (
                "stage.szs!/mapobj/shimmerlow.bmd".to_string(),
                "shimmerlow".to_string(),
            ),
            (
                "stage.szs!/mapobj/shimmerlowfar.bmd".to_string(),
                "shimmerlowfar".to_string(),
            ),
        ];
        let mut object = SceneObject::new("heatwave", "Shimmer");
        object.set_raw_param("stream_string_0", "ShimmerLowFar");

        assert_eq!(
            infer_preview_model_path(&object, &models).as_deref(),
            Some("stage.szs!/mapobj/shimmerlowfar.bmd")
        );

        object.set_raw_param("stream_string_0", "ShimmerHi");
        assert_eq!(
            infer_preview_model_path(&object, &models).as_deref(),
            Some("stage.szs!/mapobj/shimmerhi.bmd")
        );
    }

    #[test]
    fn palm_uses_the_model_basename_stored_in_its_placement_stream() {
        let models = vec![
            (
                "stage.szs!/mapobj/palmnormal.bmd".to_string(),
                "palmnormal".to_string(),
            ),
            (
                "stage.szs!/mapobj/palmleaf.bmd".to_string(),
                "palmleaf".to_string(),
            ),
        ];
        let mut object = SceneObject::new("PalmLeaf 2", "Palm");
        object.set_raw_param("name", "PalmLeaf 2");
        object.set_raw_param("stream_string_0", "palmLeaf");

        assert_eq!(
            infer_preview_model_path(&object, &models).as_deref(),
            Some("stage.szs!/mapobj/palmleaf.bmd")
        );
    }

    #[test]
    fn reset_fruits_use_the_model_basename_stored_in_their_placement_stream() {
        let models = vec![
            (
                "stage.szs!/mapobj/fruitbanana.bmd".to_string(),
                "fruitbanana".to_string(),
            ),
            (
                "stage.szs!/mapobj/fruitcoconut.bmd".to_string(),
                "fruitcoconut".to_string(),
            ),
            (
                "stage.szs!/mapobj/fruitdurian.bmd".to_string(),
                "fruitdurian".to_string(),
            ),
            (
                "stage.szs!/mapobj/fruitpapaya.bmd".to_string(),
                "fruitpapaya".to_string(),
            ),
            (
                "stage.szs!/mapobj/fruitpine.bmd".to_string(),
                "fruitpine".to_string(),
            ),
        ];

        for fruit in [
            "FruitBanana",
            "FruitCoconut",
            "FruitDurian",
            "FruitPapaya",
            "FruitPine",
        ] {
            let mut object = SceneObject::new(fruit, "ResetFruit");
            object.set_raw_param("stream_string_0", fruit);

            assert_eq!(
                infer_preview_model_path(&object, &models).as_deref(),
                Some(format!("stage.szs!/mapobj/{}.bmd", fruit.to_ascii_lowercase()).as_str())
            );
        }
    }

    #[test]
    #[ignore = "requires the extracted retail game and neighboring SMS decomp"]
    fn audits_all_retail_map_obj_resource_previews() {
        let base_root = std::env::var_os("SMS_BASE_ROOT")
            .map(PathBuf::from)
            .expect("set SMS_BASE_ROOT to the extracted game's root");
        let decomp_root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../..");
        let registry = sms_schema::SchemaGenerator::new(decomp_root)
            .generate()
            .expect("generate MapObjInit resource schema");
        assert_eq!(registry.map_obj_resources.len(), 359);

        let archives = sms_formats::discover_scene_archives(&base_root)
            .expect("discover retail scene archives");
        assert_eq!(archives.len(), 107, "unexpected retail scene archive count");

        let mut placed_identities = BTreeSet::new();
        let mut typed_placed_identities = BTreeSet::new();
        let mut model_identities = BTreeSet::new();
        let mut no_primary_placements = BTreeMap::<String, usize>::new();
        let mut model_placement_count = 0usize;
        let mut resolved_placement_count = 0usize;
        let mut missing = Vec::new();
        let mut ambiguous = Vec::new();
        let mut renderable_models = BTreeMap::<String, (String, u32)>::new();
        let mut override_placements = BTreeMap::<String, usize>::new();
        let mut override_stage_placements = BTreeMap::<(String, String), usize>::new();
        let mut procedural_or_composite_placements = BTreeMap::<(String, String), usize>::new();
        let mut particle_only_placements = BTreeMap::<(String, String), usize>::new();
        let mut controller_placements = 0usize;
        let mut preserved_shining_stone_placements = 0usize;
        let mut typed_factories = BTreeMap::<(String, String), usize>::new();
        let mut excluded_tail_collisions = BTreeMap::<(String, String, String), usize>::new();

        for archive in archives {
            let mut document = StageDocument::open(&base_root, &archive.stage_id)
                .unwrap_or_else(|error| panic!("open {}: {error}", archive.stage_id));
            let _ =
                apply_registry_preview_hints(&mut document.objects, &document.assets, &registry);
            let model_index = stage_model_index(&document.assets);

            for object in &document.objects {
                let Some(resource_name) = object.raw_param("actor_tail_string") else {
                    continue;
                };
                let Some(resource) = registry.find_map_obj_resource(resource_name) else {
                    continue;
                };
                let object_definition = registry.find_object(&object.factory_name);
                let is_map_obj = registry.is_map_obj_factory(&object.factory_name);
                placed_identities.insert(resource_name.to_string());
                if !is_map_obj {
                    let class_name = object_definition
                        .map(|definition| definition.class_name.clone())
                        .unwrap_or_else(|| "<unregistered>".to_string());
                    *excluded_tail_collisions
                        .entry((
                            object.factory_name.clone(),
                            class_name,
                            resource_name.to_string(),
                        ))
                        .or_default() += 1;
                } else {
                    typed_placed_identities.insert(resource_name.to_string());
                    *typed_factories
                        .entry((
                            object.factory_name.clone(),
                            object_definition
                                .expect("typed map object has a schema definition")
                                .class_name
                                .clone(),
                        ))
                        .or_default() += 1;
                }
                // Requiring an exact generated declaration here prevents basename
                // guesses from being counted as model-bearing MapObjInit coverage.
                assert_eq!(resource.resource_name, resource_name);
                let model_override = is_map_obj
                    .then(|| {
                        registry.find_map_obj_model_override(&object.factory_name, resource_name)
                    })
                    .flatten();

                if resource.primary_model.is_none() {
                    *no_primary_placements
                        .entry(resource_name.to_string())
                        .or_default() += 1;
                    if model_override.is_some() {
                        *override_placements
                            .entry(resource_name.to_string())
                            .or_default() += 1;
                        *override_stage_placements
                            .entry((archive.stage_id.clone(), resource_name.to_string()))
                            .or_default() += 1;
                    } else if matches!(
                        (object.factory_name.as_str(), resource_name),
                        ("FluffManager", "FluffManager") | ("HangingBridge", "HangingBridge")
                    ) {
                        *procedural_or_composite_placements
                            .entry((object.factory_name.clone(), resource_name.to_string()))
                            .or_default() += 1;
                    } else if matches!(
                        (object.factory_name.as_str(), resource_name),
                        ("MapObjSmoke", "MapSmoke")
                            | ("MapObjWaterSpray", "WaterSprayCylinder")
                            | ("MapObjSteam", "no_data")
                    ) {
                        *particle_only_placements
                            .entry((object.factory_name.clone(), resource_name.to_string()))
                            .or_default() += 1;
                    } else {
                        controller_placements += 1;
                        assert!(
                            object
                                .asset_hints
                                .iter()
                                .all(|hint| hint.role != AssetRole::InferredPreviewModel),
                            "{} {} retained a preview despite authoritative zero actor count: {:?}",
                            archive.stage_id,
                            resource_name,
                            object.asset_hints
                        );
                        continue;
                    }
                    if model_override.is_none() {
                        continue;
                    }
                }

                if !is_map_obj {
                    if object.factory_name == "ShiningStone" && resource_name == "ShiningStone" {
                        let primary = resource
                            .primary_model
                            .as_deref()
                            .expect("ShiningStone has a basename primary");
                        let ModelPathResolution::Found(path) =
                            resolve_authored_model_path(primary, &model_index)
                        else {
                            panic!(
                                "{} ShiningStone did not resolve {}",
                                archive.stage_id, primary
                            );
                        };
                        assert!(object.asset_hints.iter().any(|hint| {
                            hint.role == AssetRole::InferredPreviewModel && hint.path == path
                        }));
                        preserved_shining_stone_placements += 1;
                        renderable_models
                            .entry(path)
                            .or_insert_with(|| (resource_name.to_string(), 0x1022_0000));
                    }
                    continue;
                }
                let (authored_model, load_flags) = model_override
                    .map(|definition| (definition.model_path.as_str(), definition.load_flags))
                    .or_else(|| {
                        resource
                            .primary_model
                            .as_deref()
                            .map(|model| (model, resource.load_flags))
                    })
                    .expect("typed resource has a table primary or exact model override");
                model_identities.insert(resource_name.to_string());
                model_placement_count += 1;
                match resolve_authored_model_path(authored_model, &model_index) {
                    ModelPathResolution::Found(path) => {
                        resolved_placement_count += 1;
                        let inferred = object
                            .asset_hints
                            .iter()
                            .filter(|hint| hint.role == AssetRole::InferredPreviewModel)
                            .collect::<Vec<_>>();
                        assert_eq!(
                            inferred.len(),
                            1,
                            "{} {} has unexpected inferred hints: {:?}",
                            archive.stage_id,
                            resource_name,
                            object.asset_hints
                        );
                        assert_eq!(
                            inferred[0].path, path,
                            "{} {} did not use its declared primary {}",
                            archive.stage_id, resource_name, authored_model
                        );
                        renderable_models
                            .entry(path)
                            .or_insert_with(|| (resource_name.to_string(), load_flags));
                    }
                    ModelPathResolution::Missing => missing.push(format!(
                        "{}:{} -> {}",
                        archive.stage_id, resource_name, authored_model
                    )),
                    ModelPathResolution::Ambiguous(paths) => ambiguous.push(format!(
                        "{}:{} -> {} ({})",
                        archive.stage_id,
                        resource_name,
                        authored_model,
                        paths.join(", ")
                    )),
                }
            }
        }

        let no_primary_placement_count = no_primary_placements.values().sum::<usize>();
        let excluded_placement_count = excluded_tail_collisions.values().sum::<usize>();
        assert_eq!(placed_identities.len(), 236);
        assert_eq!(typed_placed_identities.len(), 234);
        assert_eq!(model_identities.len(), 218);
        assert_eq!(model_placement_count, 10_617);
        assert_eq!(resolved_placement_count, 10_617);
        assert!(
            missing.is_empty(),
            "missing declared primary BMDs: {missing:?}"
        );
        assert!(
            ambiguous.is_empty(),
            "ambiguous declared primary BMDs: {ambiguous:?}"
        );
        assert_eq!(no_primary_placements.len(), 21);
        assert_eq!(no_primary_placement_count, 3_652);
        assert_eq!(
            override_placements,
            BTreeMap::from([
                ("shine".to_string(), 271),
                ("SurfGesoGreen".to_string(), 4),
                ("SurfGesoRed".to_string(), 4),
                ("SurfGesoYellow".to_string(), 4),
            ])
        );
        assert_eq!(
            procedural_or_composite_placements,
            BTreeMap::from([
                (("FluffManager".to_string(), "FluffManager".to_string()), 1),
                (
                    ("HangingBridge".to_string(), "HangingBridge".to_string()),
                    14
                ),
            ])
        );
        assert_eq!(
            particle_only_placements,
            BTreeMap::from([
                (("MapObjSmoke".to_string(), "MapSmoke".to_string()), 6),
                (("MapObjSteam".to_string(), "no_data".to_string()), 8),
                (
                    (
                        "MapObjWaterSpray".to_string(),
                        "WaterSprayCylinder".to_string()
                    ),
                    106
                ),
            ])
        );
        assert_eq!(
            procedural_or_composite_placements.values().sum::<usize>(),
            15
        );
        assert_eq!(particle_only_placements.values().sum::<usize>(), 120);
        assert_eq!(controller_placements, 3_234);
        assert_eq!(
            excluded_tail_collisions,
            BTreeMap::from([
                (
                    (
                        "HangingBridge".to_string(),
                        "THangingBridge".to_string(),
                        "HangingBridge".to_string(),
                    ),
                    14,
                ),
                (
                    (
                        "ShiningStone".to_string(),
                        "TShiningStone".to_string(),
                        "ShiningStone".to_string(),
                    ),
                    8,
                ),
            ]),
            "unexpected non-TMapObjBase exact-tail collisions"
        );
        assert_eq!(excluded_placement_count, 22);
        assert_eq!(preserved_shining_stone_placements, 8);
        assert_eq!(
            registry
                .find_map_obj_resource("HangingBridge")
                .and_then(|resource| resource.primary_model.as_deref()),
            None,
            "zero-actor HangingBridge data must remain model-less"
        );
        assert_eq!(
            registry
                .find_map_obj_resource("ShiningStone")
                .and_then(|resource| resource.primary_model.as_deref()),
            Some("ShiningStone.bmd"),
            "null-animation ShiningStone data must retain its basename fallback"
        );
        for resource_name in no_primary_placements.keys() {
            assert_eq!(
                registry
                    .find_map_obj_resource(resource_name)
                    .and_then(|resource| resource.primary_model.as_ref()),
                None,
                "{resource_name} was misclassified as model-less"
            );
        }

        for (model_path, (resource_name, load_flags)) in &renderable_models {
            let bytes = read_stage_asset_bytes(model_path).unwrap_or_else(|error| {
                panic!("read {resource_name} primary {model_path}: {error}")
            });
            let file = sms_formats::J3dFile::parse(&bytes).unwrap_or_else(|error| {
                panic!("parse {resource_name} primary {model_path}: {error}")
            });
            let geometry = file
                .geometry_preview_with_loader_flags(*load_flags)
                .unwrap_or_else(|error| {
                    panic!("prepare {resource_name} primary {model_path}: {error}")
                });
            assert!(
                !geometry.triangles.is_empty(),
                "empty {resource_name} primary {model_path}"
            );
        }

        eprintln!(
            "MapObjInit retail audit: {} schema resources; {} exact-tail identities / {} typed identities; {} effective model identities / {} uniquely resolved placements; {} raw zero-primary identities / {} placements ({} direct override, {} procedural/composite, {} particle-only, {} controller); {} non-TMapObjBase collisions / {} placements; {} typed factories; {} unique renderable models; zero-primary selectors: {:?}",
            registry.map_obj_resources.len(),
            placed_identities.len(),
            typed_placed_identities.len(),
            model_identities.len(),
            resolved_placement_count,
            no_primary_placements.len(),
            no_primary_placement_count,
            override_placements.values().sum::<usize>(),
            procedural_or_composite_placements.values().sum::<usize>(),
            particle_only_placements.values().sum::<usize>(),
            controller_placements,
            excluded_tail_collisions.len(),
            excluded_placement_count,
            typed_factories.len(),
            renderable_models.len(),
            no_primary_placements
        );
        eprintln!("model override placements by stage: {override_stage_placements:?}");
    }

    #[test]
    #[ignore = "requires the extracted retail game"]
    fn retail_map_obj_resource_selectors_keep_instance_models() {
        let base_root = std::env::var_os("SMS_BASE_ROOT")
            .map(PathBuf::from)
            .expect("set SMS_BASE_ROOT to the extracted game's root");

        let mamma = StageDocument::open(&base_root, "mamma0").expect("open retail mamma0");
        let expected_sand_resources = [
            "SandBombBaseMushroom",
            "SandBombBasePyramid",
            "SandBombBaseShit",
            "SandBombBaseStar",
            "SandBombBaseTurtle",
            "SandBombBaseFoot",
            "SandBombBaseStairs",
        ];
        for resource in expected_sand_resources {
            let object = mamma
                .objects
                .iter()
                .find(|object| {
                    object.factory_name == "SandBombBase"
                        && object
                            .raw_params
                            .get("actor_tail_string")
                            .is_some_and(|value| value.raw() == resource)
                })
                .unwrap_or_else(|| panic!("missing retail {resource}"));
            let expected_suffix = format!("/mapobj/{}.bmd", resource.to_ascii_lowercase());
            assert!(
                object.asset_hints.iter().any(|hint| {
                    hint.role == AssetRole::InferredPreviewModel
                        && hint
                            .path
                            .replace('\\', "/")
                            .to_ascii_lowercase()
                            .ends_with(&expected_suffix)
                }),
                "{resource} did not retain its instance-selected preview: {:?}",
                object.asset_hints
            );
        }

        let mamma_banana_trees: Vec<_> = mamma
            .objects
            .iter()
            .filter(|object| {
                object
                    .raw_params
                    .get("actor_tail_string")
                    .is_some_and(|value| value.raw() == "BananaTree")
            })
            .collect();
        assert!(
            !mamma_banana_trees.is_empty(),
            "missing retail BananaTree objects"
        );
        for object in mamma_banana_trees {
            assert!(
                object.asset_hints.iter().any(|hint| {
                    hint.role == AssetRole::InferredPreviewModel
                        && hint
                            .path
                            .replace('\\', "/")
                            .to_ascii_lowercase()
                            .ends_with("/mapobj/bananatree.bmd")
                }),
                "{} did not resolve BananaTree.bmd: {:?}",
                object.factory_name,
                object.asset_hints
            );
        }

        let dolpic_ten = StageDocument::open(&base_root, "dolpic10").expect("open retail dolpic10");
        for resource in ["palmNormal", "palmLeaf"] {
            let matching: Vec<_> = dolpic_ten
                .objects
                .iter()
                .filter(|object| {
                    object
                        .raw_params
                        .get("actor_tail_string")
                        .is_some_and(|value| value.raw() == resource)
                })
                .collect();
            assert!(!matching.is_empty(), "missing retail Palm {resource}");
            let expected_suffix = format!("/mapobj/{}.bmd", resource.to_ascii_lowercase());
            for object in matching {
                assert!(
                    object.asset_hints.iter().any(|hint| {
                        hint.role == AssetRole::InferredPreviewModel
                            && hint
                                .path
                                .replace('\\', "/")
                                .to_ascii_lowercase()
                                .ends_with(&expected_suffix)
                    }),
                    "{} with {resource} did not resolve its exact Palm model: {:?}",
                    object.factory_name,
                    object.asset_hints
                );
            }
        }

        let dolpic = StageDocument::open(&base_root, "dolpic0").expect("open retail dolpic0");
        let assert_nozzle_items = |document: &StageDocument, stage: &str, expected: &[&str]| {
            let boxes: Vec<_> = document
                .objects
                .iter()
                .filter(|object| object.factory_name == "NozzleBox")
                .collect();
            let mut actual: Vec<_> = boxes
                .iter()
                .map(|object| {
                    assert_eq!(
                        object.raw_param("actor_tail_string"),
                        Some("NozzleBox"),
                        "{stage} NozzleBox lost its TMapObjBase resource selector"
                    );
                    object
                        .raw_param("nozzle_box_item")
                        .unwrap_or_else(|| panic!("{stage} NozzleBox lost its item selector"))
                })
                .collect();
            actual.sort_unstable();
            let mut expected = expected.to_vec();
            expected.sort_unstable();
            assert_eq!(actual, expected, "unexpected {stage} NozzleBox items");
        };
        assert_nozzle_items(
            &mamma,
            "mamma0",
            &[
                "normal_nozzle_item",
                "rocket_nozzle_item",
                "back_nozzle_item",
            ],
        );
        assert_nozzle_items(&dolpic, "dolpic0", &["rocket_nozzle_item"]);
        assert_nozzle_items(
            &dolpic_ten,
            "dolpic10",
            &[
                "normal_nozzle_item",
                "rocket_nozzle_item",
                "rocket_nozzle_item",
                "back_nozzle_item",
            ],
        );

        let assert_fruit_previews =
            |document: &StageDocument, stage: &str, expected: &[(&str, usize)]| {
                for (resource, expected_count) in expected {
                    let matching: Vec<_> = document
                        .objects
                        .iter()
                        .filter(|object| {
                            object.factory_name == "ResetFruit"
                                && object
                                    .raw_params
                                    .get("actor_tail_string")
                                    .is_some_and(|value| value.raw() == *resource)
                        })
                        .collect();
                    assert_eq!(
                        matching.len(),
                        *expected_count,
                        "unexpected {stage} ResetFruit {resource} count"
                    );
                    let expected_suffix = format!("/mapobj/{}.bmd", resource.to_ascii_lowercase());
                    for object in matching {
                        assert!(
                            object.asset_hints.iter().any(|hint| {
                                hint.role == AssetRole::InferredPreviewModel
                                    && hint
                                        .path
                                        .replace('\\', "/")
                                        .to_ascii_lowercase()
                                        .ends_with(&expected_suffix)
                            }),
                            "{stage} ResetFruit {resource} did not resolve its model: {:?}",
                            object.asset_hints
                        );
                    }
                }
            };
        assert_fruit_previews(
            &dolpic,
            "dolpic0",
            &[
                ("FruitBanana", 5),
                ("FruitCoconut", 9),
                ("FruitDurian", 3),
                ("FruitPapaya", 3),
                ("FruitPine", 3),
                ("RedPepper", 4),
            ],
        );
        assert_fruit_previews(
            &dolpic_ten,
            "dolpic10",
            &[
                ("FruitBanana", 6),
                ("FruitCoconut", 9),
                ("FruitDurian", 3),
                ("FruitPapaya", 3),
                ("FruitPine", 3),
                ("RedPepper", 4),
            ],
        );
    }
}
