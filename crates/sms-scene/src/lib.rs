//! Editable stage documents and safe editor-project persistence.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};
use sms_formats::{
    parse_jdrama_object_records, read_stage_asset_bytes, scan_stage_assets, SourceLocation,
    StageAsset, StageAssetKind,
};
use sms_schema::ObjectRegistry;
use thiserror::Error;

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
}

pub type Result<T> = std::result::Result<T, SceneError>;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EditorProjectManifest {
    pub format_version: u32,
    pub kind: String,
    pub base_path: PathBuf,
    pub project_files_path: PathBuf,
    pub created_with: String,
    pub changed_files: Vec<PathBuf>,
}

impl EditorProjectManifest {
    pub fn new(base_path: PathBuf, project_files_path: PathBuf) -> Self {
        Self {
            format_version: 1,
            kind: "sms-editor-project".to_string(),
            base_path,
            project_files_path,
            created_with: env!("CARGO_PKG_VERSION").to_string(),
            changed_files: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StageDocument {
    pub stage_id: String,
    pub base_root: PathBuf,
    pub assets: Vec<StageAsset>,
    pub objects: Vec<SceneObject>,
    pub changed_files: BTreeMap<PathBuf, Vec<u8>>,
    pub registry: Option<ObjectRegistry>,
    pub load_issues: Vec<ValidationIssue>,
}

impl StageDocument {
    pub fn open(base_root: impl AsRef<Path>, stage_id: impl Into<String>) -> Result<Self> {
        let base_root = base_root.as_ref().to_path_buf();
        if !base_root.exists() {
            return Err(SceneError::MissingBaseRoot(base_root));
        }

        let stage_id = stage_id.into();
        let assets = scan_stage_assets(&base_root, &stage_id)?;
        let (objects, load_issues) = load_scene_objects_from_assets(&assets);
        Ok(Self {
            stage_id,
            base_root,
            assets,
            objects,
            changed_files: BTreeMap::new(),
            registry: None,
            load_issues,
        })
    }

    pub fn with_registry(mut self, registry: ObjectRegistry) -> Self {
        self.registry = Some(registry);
        self
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
        if self.objects.is_empty() {
            self.changed_files.remove(&path);
            return Ok(());
        }

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
        &self,
        project_root: impl AsRef<Path>,
    ) -> Result<EditorProjectManifest> {
        let project_root = project_root.as_ref();
        if project_root
            .components()
            .any(|component| matches!(component, Component::ParentDir))
        {
            return Err(SceneError::InvalidProjectRoot(project_root.to_path_buf()));
        }
        let project_comparison = normalized_absolute_for_comparison(project_root)?;
        let base_comparison = normalized_absolute_for_comparison(&self.base_root)?;
        if path_is_same_or_child(&project_comparison, &base_comparison)
            || path_is_same_or_child(&base_comparison, &project_comparison)
        {
            return Err(SceneError::ProjectOverlapsBase(project_root.to_path_buf()));
        }
        let parent = project_root
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        let name = project_root
            .file_name()
            .ok_or_else(|| SceneError::InvalidProjectRoot(project_root.to_path_buf()))?
            .to_string_lossy();
        fs::create_dir_all(parent)?;

        let unique = std::process::id();
        let staging_root = parent.join(format!(".{name}.staging-{unique}"));
        let backup_root = parent.join(format!(".{name}.backup-{unique}"));
        remove_dir_if_exists(&staging_root)?;
        remove_dir_if_exists(&backup_root)?;

        let files_root = staging_root.join("files");
        fs::create_dir_all(&files_root)?;

        let mut changed_files = Vec::new();
        for (relative_path, bytes) in &self.changed_files {
            validate_project_relative_path(relative_path)?;
            let out_path = files_root.join(relative_path);
            if let Some(parent) = out_path.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::write(&out_path, bytes)?;
            changed_files.push(relative_path.clone());
        }

        changed_files.sort();
        let mut manifest =
            EditorProjectManifest::new(self.base_root.clone(), project_root.join("files"));
        manifest.changed_files = changed_files;

        let manifest_text = toml::to_string_pretty(&manifest)?;
        fs::write(staging_root.join("sms-project.toml"), manifest_text)?;

        if project_root.exists() {
            fs::rename(project_root, &backup_root)?;
        }
        if let Err(err) = fs::rename(&staging_root, project_root) {
            if backup_root.exists() {
                let _ = fs::rename(&backup_root, project_root);
            }
            return Err(SceneError::Io(err));
        }
        remove_dir_if_exists(&backup_root)?;
        Ok(manifest)
    }

    pub fn validate(&self) -> Vec<ValidationIssue> {
        let mut issues = self.load_issues.clone();

        if !self.base_root.exists() {
            issues.push(ValidationIssue::error(
                "missing-base-root",
                format!("Base root does not exist: {}", self.base_root.display()),
            ));
        }

        if self.assets.is_empty() {
            issues.push(ValidationIssue::warning(
                "no-stage-assets",
                format!("No assets found for stage '{}'", self.stage_id),
            ));
        }

        if validate_stage_id(&self.stage_id).is_err() {
            issues.push(ValidationIssue::error(
                "invalid-stage-id",
                format!(
                    "Stage id '{}' is not safe for project output",
                    self.stage_id
                ),
            ));
        }

        for path in self.changed_files.keys() {
            if validate_project_relative_path(path).is_err() {
                issues.push(ValidationIssue::error(
                    "unsafe-project-path",
                    format!("Changed file path is unsafe: {}", path.display()),
                ));
            }
        }

        let mut object_ids = BTreeSet::new();
        for object in &self.objects {
            if !object_ids.insert(object.id.as_str()) {
                issues.push(ValidationIssue::error(
                    "duplicate-object-id",
                    format!("Object id '{}' is duplicated", object.id),
                ));
            }
            if object.factory_name.trim().is_empty() {
                issues.push(ValidationIssue::error(
                    "empty-factory-name",
                    format!("Object {} has no factory name", object.id),
                ));
            }

            if !object.transform.is_finite() {
                issues.push(ValidationIssue::error(
                    "invalid-transform",
                    format!("Object {} has a non-finite transform", object.id),
                ));
            }
            if object
                .transform
                .scale
                .iter()
                .any(|value| value.abs() <= f32::EPSILON)
            {
                issues.push(ValidationIssue::warning(
                    "zero-scale",
                    format!("Object {} has a non-invertible scale", object.id),
                ));
            }

            if let Some(registry) = &self.registry {
                if registry.find_object(&object.factory_name).is_none() && object.source.is_none() {
                    issues.push(ValidationIssue::warning(
                        "unknown-factory",
                        format!(
                            "Object '{}' is not in the generated registry",
                            object.factory_name
                        ),
                    ));
                }
            }
        }

        issues
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
    pub raw_params: BTreeMap<String, String>,
    pub decoded_params: BTreeMap<String, ParamValue>,
    pub asset_hints: Vec<AssetRef>,
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
            decoded_params: BTreeMap::new(),
            asset_hints: Vec::new(),
        }
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
        && stage_id != "..";
    if valid {
        Ok(())
    } else {
        Err(SceneError::InvalidStageId(stage_id.to_string()))
    }
}

fn validate_project_relative_path(path: &Path) -> Result<()> {
    let valid = !path.as_os_str().is_empty()
        && path
            .components()
            .all(|component| matches!(component, Component::Normal(_)));
    if valid {
        Ok(())
    } else {
        Err(SceneError::UnsafeProjectPath(path.to_path_buf()))
    }
}

fn remove_dir_if_exists(path: &Path) -> Result<()> {
    match fs::remove_dir_all(path) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(SceneError::Io(err)),
    }
}

fn normalized_absolute_for_comparison(path: &Path) -> Result<String> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()?.join(path)
    };
    let canonical = canonicalize_with_missing_tail(&absolute);
    let normalized = canonical
        .to_string_lossy()
        .replace('/', "\\")
        .trim_end_matches('\\')
        .to_ascii_lowercase();
    Ok(normalized
        .strip_prefix("\\\\?\\")
        .unwrap_or(&normalized)
        .to_string())
}

fn canonicalize_with_missing_tail(path: &Path) -> PathBuf {
    let mut existing = path;
    let mut missing = Vec::new();

    loop {
        if let Ok(mut canonical) = fs::canonicalize(existing) {
            for component in missing.iter().rev() {
                canonical.push(component);
            }
            return canonical;
        }

        let Some(name) = existing.file_name() else {
            return path.to_path_buf();
        };
        missing.push(name.to_os_string());
        let Some(parent) = existing.parent() else {
            return path.to_path_buf();
        };
        existing = parent;
    }
}

fn path_is_same_or_child(path: &str, parent: &str) -> bool {
    path == parent
        || path
            .strip_prefix(parent)
            .is_some_and(|suffix| suffix.starts_with('\\'))
}

fn load_scene_objects_from_assets(
    assets: &[StageAsset],
) -> (Vec<SceneObject>, Vec<ValidationIssue>) {
    let mut objects = Vec::new();
    let mut issues = Vec::new();
    let model_index = stage_model_index(assets);
    let mut placement_files = 0usize;

    for asset in assets
        .iter()
        .filter(|asset| asset.kind == StageAssetKind::Placement)
    {
        let path_text = asset.path.to_string_lossy().replace('\\', "/");
        if !path_text.to_ascii_lowercase().ends_with("/map/scene.bin") {
            continue;
        }
        placement_files += 1;

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
            let Some(transform) = record.transform else {
                continue;
            };

            let type_name = record.type_name.clone();
            let object_name = record
                .object_name
                .clone()
                .unwrap_or_else(|| type_name.clone());
            let mut object =
                SceneObject::new(format!("retail-{:08x}", record.offset), type_name.clone());
            object.source = Some(SourceLocation {
                path: asset.path.clone(),
                offset: Some(record.offset as u64),
                length: Some(record.size as u64),
            });
            object.class_name = Some(type_name);
            object.transform = Transform {
                translation: transform.translation,
                rotation_degrees: transform.rotation,
                scale: transform.scale,
            };
            object
                .raw_params
                .insert("name".to_string(), object_name.clone());
            for (index, value) in record.stream_strings.iter().enumerate() {
                object
                    .raw_params
                    .insert(format!("stream_string_{index}"), value.clone());
            }
            if let Some(params) = record.npc_params {
                object.raw_params.insert(
                    "npc_body_color_index".to_string(),
                    params.color_indices[0].to_string(),
                );
                object.raw_params.insert(
                    "npc_cloth_color_index".to_string(),
                    params.color_indices[1].to_string(),
                );
                object.raw_params.insert(
                    "npc_pollution_amount".to_string(),
                    params.pollution_amount.to_string(),
                );
                object
                    .raw_params
                    .insert("npc_parts_mask".to_string(), params.parts_mask.to_string());
                for (index, value) in params.parts_color_indices.into_iter().enumerate() {
                    object
                        .raw_params
                        .insert(format!("npc_parts_color_index_{index}"), value.to_string());
                }
                object.raw_params.insert(
                    "npc_action_flags".to_string(),
                    params.action_flags.to_string(),
                );
            }

            if let Some(model_path) = infer_preview_model_path(&object, &model_index) {
                object.asset_hints.push(AssetRef {
                    path: model_path,
                    role: AssetRole::PreviewModel,
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

    (objects, issues)
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

fn infer_preview_model_path(
    object: &SceneObject,
    model_index: &[(String, String)],
) -> Option<String> {
    if let Some((directory, model_name)) = npc_preview_model_identity(&object.factory_name) {
        let archive_directory = format!("!/{directory}/");
        if let Some((path, _)) = model_index.iter().find(|(path, _)| {
            let lower = path.to_ascii_lowercase();
            lower.contains(&archive_directory)
                && lower
                    .rsplit('/')
                    .next()
                    .is_some_and(|name| name.eq_ignore_ascii_case(model_name))
        }) {
            return Some(path.clone());
        }
    }

    // TMapObjBase::load and TShimmer::load read their resource identity from
    // the first placement stream string. Prefer that authored basename over
    // generic factories such as `Palm` and `Shimmer`.
    if object.factory_name.eq_ignore_ascii_case("Palm")
        || object.factory_name.eq_ignore_ascii_case("Shimmer")
    {
        if let Some(model_name) = object.raw_params.get("stream_string_0") {
            let key = normalize_model_key(model_name);
            if let Some(path) = exact_model_key_match(&key, model_index) {
                return Some(path);
            }
        }
    }

    let mut keys = Vec::new();
    keys.push(normalize_model_key(&object.factory_name));
    if let Some(class_name) = &object.class_name {
        keys.push(normalize_model_key(class_name));
        if let Some(short_name) = class_name.rsplit("::").next() {
            keys.push(normalize_model_key(short_name));
        }
    }
    if let Some(name) = object.raw_params.get("name") {
        keys.push(normalize_model_key(name));
    }
    keys.retain(|key| key.len() >= 3);
    keys.sort();
    keys.dedup();

    for key in &keys {
        if let Some(path) = exact_model_key_match(key, model_index) {
            return Some(path);
        }
    }

    for key in keys {
        if let Some(path) = fuzzy_model_key_match(&key, model_index) {
            return Some(path);
        }
    }

    None
}

fn npc_preview_model_identity(factory_name: &str) -> Option<(&'static str, &'static str)> {
    match factory_name.to_ascii_lowercase().as_str() {
        "npcmontem" => Some(("montem", "mom_model.bmd")),
        "npcmontema" => Some(("montema", "moma_model.bmd")),
        "npcmontemb" => Some(("montemb", "momb_model.bmd")),
        "npcmontemc" => Some(("montemc", "momc_model.bmd")),
        "npcmontemd" => Some(("montemd", "momd_model.bmd")),
        "npcmonteme" => Some(("monteme", "mome_model.bmd")),
        // These variants deliberately reuse another Monte model in the game.
        "npcmontemf" => Some(("montem", "mom_model.bmd")),
        "npcmontemg" => Some(("montemc", "momc_model.bmd")),
        "npcmontemh" => Some(("montema", "moma_model.bmd")),
        "npcmontew" => Some(("montew", "mow_model.bmd")),
        "npcmontewa" => Some(("montewa", "mowa_model.bmd")),
        "npcmontewb" => Some(("montewb", "mowb_model.bmd")),
        "npcmontewc" => Some(("montew", "mow_model.bmd")),
        "npcmarem" | "npcmarema" | "npcmaremb" | "npcmaremc" | "npcmaremd" => {
            Some(("marem", "marem.bmd"))
        }
        "npcmarew" | "npcmarewa" | "npcmarewb" => Some(("marew", "marew.bmd")),
        "npckinopio" => Some(("kinopio", "kinopio_body.bmd")),
        "npckinojii" => Some(("kinojii", "kinoji_body.bmd")),
        "npcpeach" => Some(("peach", "peach_model.bmd")),
        "npcraccoondog" => Some(("raccoondog", "tanuki.bmd")),
        "npcboard" => Some(("boardnpc", "boardnpc.bmd")),
        _ => None,
    }
}

fn exact_model_key_match(key: &str, model_index: &[(String, String)]) -> Option<String> {
    model_index
        .iter()
        .find(|(_, model_key)| model_key == key)
        .map(|(path, _)| path.clone())
}

fn fuzzy_model_key_match(key: &str, model_index: &[(String, String)]) -> Option<String> {
    let aliases = object_model_aliases(key);
    for alias in &aliases {
        if let Some((path, _)) = model_index.iter().find(|(path, model_key)| {
            let lower = path.to_ascii_lowercase();
            (lower.contains("!/mapobj/") || lower.contains("/scene/mapobj/")) && model_key == alias
        }) {
            return Some(path.clone());
        }
    }

    model_index
        .iter()
        .filter(|(path, _)| {
            let lower = path.to_ascii_lowercase();
            lower.contains("!/mapobj/") || lower.contains("/scene/mapobj/")
        })
        .find(|(_, model_key)| {
            model_key.contains(key)
                || key.contains(model_key.as_str())
                || aliases
                    .iter()
                    .any(|alias| model_key.contains(alias) || alias.contains(model_key.as_str()))
        })
        .map(|(path, _)| path.clone())
}

fn object_model_aliases(key: &str) -> Vec<&'static str> {
    let mut aliases = Vec::new();
    if key.contains("palm") {
        aliases.push("palmnormal");
    }
    if key.contains("manhole") {
        aliases.push("manhole");
    }
    if key.contains("kibako") || key.contains("crate") || key.contains("box") {
        aliases.push("kibako");
    }
    if key.contains("barrel") {
        aliases.push("barrelnormal");
    }
    if key.contains("coin") {
        aliases.push("coin");
    }
    aliases
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

    for prefix in ["t", "m", "sm"] {
        if key.len() > prefix.len() + 3 && key.starts_with(prefix) {
            return key[prefix.len()..].to_string();
        }
    }
    key
}

#[cfg(test)]
mod tests {
    use super::*;

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
        };
        let mut object = SceneObject::new("obj-1", "coin");
        object.transform.translation[0] = f32::NAN;
        doc.add_object(object);

        let issues = doc.validate();
        assert!(issues.iter().any(|issue| issue.code == "invalid-transform"));
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
        };

        doc.queue_editor_overlay_change().unwrap();
        assert!(doc
            .changed_files
            .contains_key(&PathBuf::from("editor/stages/dolpic.scene.json")));
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
    }

    #[test]
    fn project_export_replaces_stale_files() {
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

        doc.changed_files.clear();
        doc.mark_changed_file("second.bin", vec![2]).unwrap();
        doc.save_project_folder(&root).unwrap();
        assert!(!root.join("files/first.bin").exists());
        assert!(root.join("files/second.bin").exists());
        assert!(root.join("sms-project.toml").exists());

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
            registry: None,
            load_issues: Vec::new(),
        }
    }

    #[test]
    fn resolves_npc_models_from_decomp_manager_resource_names() {
        let models = vec![
            (
                "stage.szs!/montema/moma_model.bmd".to_string(),
                "omamodel".to_string(),
            ),
            (
                "stage.szs!/kinopio/kinopio_body.bmd".to_string(),
                "kinopiobody".to_string(),
            ),
        ];
        let monte = SceneObject::new("monte", "NPCMonteMA");
        let kinopio = SceneObject::new("kinopio", "NPCKinopio");

        assert_eq!(
            infer_preview_model_path(&monte, &models).as_deref(),
            Some("stage.szs!/montema/moma_model.bmd")
        );
        assert_eq!(
            infer_preview_model_path(&kinopio, &models).as_deref(),
            Some("stage.szs!/kinopio/kinopio_body.bmd")
        );
    }

    #[test]
    fn special_monte_variants_reuse_the_game_model_directory() {
        let models = vec![(
            "stage.szs!/montema/moma_model.bmd".to_string(),
            "omamodel".to_string(),
        )];
        let object = SceneObject::new("map-shop", "NPCMonteMH");

        assert_eq!(
            infer_preview_model_path(&object, &models).as_deref(),
            Some("stage.szs!/montema/moma_model.bmd")
        );
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
        object
            .raw_params
            .insert("stream_string_0".to_string(), "ShimmerLowFar".to_string());

        assert_eq!(
            infer_preview_model_path(&object, &models).as_deref(),
            Some("stage.szs!/mapobj/shimmerlowfar.bmd")
        );

        object
            .raw_params
            .insert("stream_string_0".to_string(), "ShimmerHi".to_string());
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
        object
            .raw_params
            .insert("name".to_string(), "PalmLeaf 2".to_string());
        object
            .raw_params
            .insert("stream_string_0".to_string(), "palmLeaf".to_string());

        assert_eq!(
            infer_preview_model_path(&object, &models).as_deref(),
            Some("stage.szs!/mapobj/palmleaf.bmd")
        );
    }
}
