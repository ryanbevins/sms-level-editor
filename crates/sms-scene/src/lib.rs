//! Editable stage documents and safe editor-project persistence.

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsStr;
use std::fmt;
use std::fs;
use std::ops::Deref;
use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};
use sms_formats::{
    parse_jdrama_object_records, read_stage_asset_bytes, scan_stage_assets, JDramaAmbient,
    JDramaLight, JDramaObjectRecord, SourceLocation, StageAsset, StageAssetKind,
};
use sms_schema::{
    EnemyActorDefinition, EnemyManagerDefinition, EnemyModelDefinition, ObjectRegistry,
};
use thiserror::Error;

mod project_store;
mod validation;

#[derive(Debug, Error)]
pub enum SceneError {
    #[error("base root does not exist: {0}")]
    MissingBaseRoot(PathBuf),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("format error: {0}")]
    Format(#[from] sms_formats::FormatError),
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
    #[error("refusing to replace a directory that is not an owned SMS Editor project: {0}")]
    UnownedProjectRoot(PathBuf),
    #[error("unsupported SMS Editor project manifest at {path}: {reason}")]
    UnsupportedProjectManifest { path: PathBuf, reason: String },
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
    pub registry: Option<ObjectRegistry>,
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActorPreview {
    pub model_path: String,
    pub load_flags: u32,
    pub manager_factory: String,
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
        let (objects, load_issues, lighting) = load_scene_objects_from_assets(&assets);
        Ok(Self {
            stage_id,
            base_root,
            assets,
            objects,
            changed_files: BTreeMap::new(),
            registry: None,
            load_issues,
            lighting,
            actor_previews: BTreeMap::new(),
            loaded_project: None,
        })
    }

    pub fn with_registry(mut self, registry: ObjectRegistry) -> Self {
        self.set_registry(registry);
        self
    }

    pub fn set_registry(&mut self, registry: ObjectRegistry) {
        let (actor_previews, preview_issues) =
            build_actor_preview_catalog(&self.base_root, &self.assets, &registry);
        let object_preview_issues =
            apply_registry_preview_hints(&mut self.objects, &self.assets, &registry);
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
        };
        let bytes = serde_json::to_vec_pretty(&overlay)?;
        self.mark_changed_file(path, bytes)?;
        Ok(())
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
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SceneObject {
    pub id: String,
    pub source: Option<SourceLocation>,
    pub factory_name: String,
    pub class_name: Option<String>,
    pub transform: Transform,
    pub raw_params: BTreeMap<String, SceneParameter>,
    pub asset_hints: Vec<AssetRef>,
    #[serde(skip, default)]
    pub source_record_bytes: Option<Vec<u8>>,
}

impl SceneObject {
    pub fn new(id: impl Into<String>, factory_name: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            source: None,
            factory_name: factory_name.into(),
            class_name: None,
            transform: Transform::default(),
            raw_params: BTreeMap::new(),
            asset_hints: Vec::new(),
            source_record_bytes: None,
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

fn load_scene_objects_from_assets(
    assets: &[StageAsset],
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

        let bytes = match read_stage_asset_bytes(&asset.path) {
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
            object.class_name = Some(type_name);
            object.source_record_bytes = record
                .offset
                .checked_add(record.size)
                .and_then(|end| bytes.get(record.offset..end))
                .map(<[u8]>::to_vec);
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
        let resource_name = object.raw_param("actor_tail_string");
        let object_definition = registry.find_object(&object.factory_name);
        let is_map_static =
            object_definition.is_some_and(|definition| definition.class_name == "TMapStaticObj");
        let is_map_obj = registry.is_map_obj_factory(&object.factory_name);
        let map_obj_resource = if is_map_obj {
            resource_name.and_then(|resource_name| registry.find_map_obj_resource(resource_name))
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

        let map_static_binding = if is_map_static {
            resource_name.and_then(|resource_name| {
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
        let binding = map_static_binding
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
        let Ok(bytes) = read_stage_asset_bytes(&asset.path) else {
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
                let Some(preview) =
                    resolve_manager_actor_preview(actor, manager, folder, &model_index)
                else {
                    continue;
                };
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
                    let Some(preview) =
                        resolve_manager_actor_preview(actor, target_manager, folder, &model_index)
                    else {
                        continue;
                    };
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
                        )
                    })
                })
            })();
            let Some((model_path, load_flags, manager_factory)) =
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
            registry: None,
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

    #[test]
    fn queues_editor_overlay_as_changed_file() {
        let mut doc = StageDocument {
            stage_id: "dolpic".to_string(),
            base_root: PathBuf::from("."),
            assets: vec![],
            objects: vec![SceneObject::new("obj-1", "coin")],
            changed_files: BTreeMap::new(),
            registry: None,
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
    }

    #[test]
    fn scene_parameters_keep_one_raw_decoded_dirty_state_and_legacy_json_shape() {
        let mut object = SceneObject::new("fixture", "Fixture");
        object.insert_source_raw_param("source", "17");
        object.set_decoded_param("edited", "23", ParamValue::Int(23));
        object.source_record_bytes = Some(vec![1, 2, 3, 4]);

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
        assert!(restored.source_record_bytes.is_none());

        assert_eq!(
            SceneParameter::from_source("23"),
            SceneParameter::edited("23", Some(ParamValue::Int(23)))
        );
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
        saved.queue_editor_overlay_change().unwrap();
        saved.save_project_folder(&root).unwrap();

        let mut reopened = empty_document("dolpic");
        assert!(reopened.load_project_folder(&root).unwrap());
        assert_eq!(reopened.objects, saved.objects);
        assert!(reopened.loaded_project.is_some());

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn project_overlay_reload_reattaches_source_bytes_and_refreshes_derived_metadata() {
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
        base_object.source_record_bytes = Some(vec![1, 2, 3, 4]);

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
        assert_eq!(
            reopened.objects[0].source_record_bytes.as_deref(),
            Some([1, 2, 3, 4].as_slice())
        );
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
    fn project_export_blocks_validation_errors_before_writing() {
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

        assert!(matches!(
            doc.save_project_folder(&root).unwrap_err(),
            SceneError::ValidationFailed(_)
        ));
        assert!(!root.exists());
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
            registry: None,
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
                primary_model: Some("kibako.bmd".to_string()),
                load_flags: 0x1022_0000,
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
                primary_model: Some("efDokanGate.bmd".to_string()),
                load_flags: 0x1122_0000,
                source_file: "src/MoveBG/MapObjInit.cpp".to_string(),
            }],
            map_static_models: vec![sms_schema::MapStaticModelDefinition {
                actor_name: "mareSeaPollutionS34567".to_string(),
                model_path: None,
                load_flags: 0x1021_0000,
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
    fn model_less_map_obj_resource_authoritatively_clears_basename_guesses() {
        let registry = ObjectRegistry {
            objects: vec![schema_object("MapObjBase", "TMapObjBase", "MapObj", None)],
            map_obj_factories: vec!["MapObjBase".to_string()],
            map_obj_resources: vec![sms_schema::MapObjResourceDefinition {
                resource_name: "MapSmoke".to_string(),
                actor_type: 0x4000_0004,
                primary_model: None,
                load_flags: 0x1022_0000,
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
                primary_model: None,
                load_flags: 0x1022_0000,
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
                primary_model: Some("kibako.bmd".to_string()),
                load_flags: 0x1022_0000,
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
                primary_model: Some("wrong_map_obj_model.bmd".to_string()),
                load_flags: 0x1022_0000,
                source_file: "src/MoveBG/MapObjInit.cpp".to_string(),
            });
        registry
            .map_static_models
            .push(sms_schema::MapStaticModelDefinition {
                actor_name: "MareGate".to_string(),
                model_path: Some("/scene/map/map/mare_gate_model.bmd".to_string()),
                load_flags: 0,
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
                primary_model: Some("kibako.bmd".to_string()),
                load_flags: 0x1022_0000,
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
            let (catalog, issues) = build_actor_preview_catalog(&base_root, &assets, &registry);
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
        };
        let manager = EnemyManagerDefinition {
            factory_name: "EMarioManager".to_string(),
            class_name: "TEMarioManager".to_string(),
            model_index: None,
            spawned_actor_class: Some("TEMario".to_string()),
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
        };
        let manager = EnemyManagerDefinition {
            factory_name: "ButterflyManager".to_string(),
            class_name: "TButterfloidManager".to_string(),
            model_index: None,
            spawned_actor_class: None,
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
        };
        let target_manager = EnemyManagerDefinition {
            factory_name: "LimitKoopaManager".to_string(),
            class_name: "TLimitKoopaManager".to_string(),
            model_index: None,
            spawned_actor_class: None,
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
        };
        let base_manager = EnemyManagerDefinition {
            factory_name: "HamuKuriManager".to_string(),
            class_name: "THamuKuriManager".to_string(),
            model_index: None,
            spawned_actor_class: Some("THamuKuri".to_string()),
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
