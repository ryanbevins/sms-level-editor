use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::Write;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use sms_scene::EditorProjectManifest;

pub(super) const SMS_PROJECT_EXTENSION: &str = "sms";
const SMS_PROJECT_KIND: &str = "sms-editor-project";
const SMS_PROJECT_FORMAT_VERSION: u32 = 1;
const RECENT_PROJECTS_KIND: &str = "sms-editor-recent-projects";
const RECENT_PROJECTS_FORMAT_VERSION: u32 = 1;
const MAX_PROJECT_FILE_BYTES: u64 = 1024 * 1024;
const MAX_RECENT_PROJECTS: usize = 12;
static PROJECT_ID_SEQUENCE: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct ProjectLaunchConfiguration {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) dolphin_executable: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) game_image: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) dolphin_user_directory: Option<PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(super) struct ProjectCameraState {
    pub(super) focus: [f32; 3],
    pub(super) distance: f32,
    pub(super) yaw_degrees: f32,
    pub(super) pitch_degrees: f32,
    #[serde(default)]
    pub(super) viewport_pan: [f32; 2],
    #[serde(default = "default_viewport_zoom")]
    pub(super) viewport_zoom: f32,
    #[serde(default = "default_camera_speed")]
    pub(super) camera_speed: f32,
}

impl ProjectCameraState {
    pub(super) fn is_valid(&self) -> bool {
        self.focus.iter().all(|value| value.is_finite())
            && self.distance.is_finite()
            && self.distance > 0.0
            && self.yaw_degrees.is_finite()
            && self.pitch_degrees.is_finite()
            && self.viewport_pan.iter().all(|value| value.is_finite())
            && self.viewport_zoom.is_finite()
            && self.viewport_zoom > 0.0
            && self.camera_speed.is_finite()
            && self.camera_speed > 0.0
    }
}

fn default_viewport_zoom() -> f32 {
    1.0
}

fn default_camera_speed() -> f32 {
    1.0
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(super) struct SmsProjectFile {
    pub(super) format_version: u32,
    pub(super) kind: String,
    pub(super) name: String,
    pub(super) project_id: String,
    pub(super) created_with: String,
    pub(super) base_game_root: PathBuf,
    pub(super) project_data_root: PathBuf,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) schema_source_root: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) last_stage: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub(super) stage_cameras: BTreeMap<String, ProjectCameraState>,
    #[serde(default)]
    pub(super) launch: ProjectLaunchConfiguration,
}

impl SmsProjectFile {
    pub(super) fn new(
        name: impl Into<String>,
        base_game_root: PathBuf,
        project_data_root: PathBuf,
        schema_source_root: Option<PathBuf>,
    ) -> Self {
        Self {
            format_version: SMS_PROJECT_FORMAT_VERSION,
            kind: SMS_PROJECT_KIND.to_string(),
            name: name.into(),
            project_id: new_project_id(),
            created_with: env!("CARGO_PKG_VERSION").to_string(),
            base_game_root,
            project_data_root,
            schema_source_root,
            last_stage: None,
            stage_cameras: BTreeMap::new(),
            launch: ProjectLaunchConfiguration::default(),
        }
    }

    pub(super) fn load(path: &Path) -> Result<Self, String> {
        ensure_sms_extension(path)?;
        let metadata = fs::metadata(path)
            .map_err(|error| format!("Could not read project '{}': {error}", path.display()))?;
        if !metadata.is_file() {
            return Err(format!("Project is not a file: {}", path.display()));
        }
        if metadata.len() > MAX_PROJECT_FILE_BYTES {
            return Err(format!(
                "Project file is larger than {MAX_PROJECT_FILE_BYTES} bytes: {}",
                path.display()
            ));
        }
        let text = fs::read_to_string(path)
            .map_err(|error| format!("Could not read project '{}': {error}", path.display()))?;
        let project: Self = toml::from_str(&text)
            .map_err(|error| format!("Could not parse project '{}': {error}", path.display()))?;
        project.validate(path)?;
        project.validate_locations(path)?;
        Ok(project)
    }

    pub(super) fn save(&self, path: &Path) -> Result<(), String> {
        ensure_sms_extension(path)?;
        self.validate(path)?;
        self.validate_locations(path)?;
        let parent = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty());
        if let Some(parent) = parent {
            fs::create_dir_all(parent).map_err(|error| {
                format!(
                    "Could not create the project directory '{}': {error}",
                    parent.display()
                )
            })?;
        }
        let text = toml::to_string_pretty(self).map_err(|error| {
            format!("Could not serialize project '{}': {error}", path.display())
        })?;
        write_synced(path, text.as_bytes())
            .map_err(|error| format!("Could not save project '{}': {error}", path.display()))
    }

    fn validate_locations(&self, path: &Path) -> Result<(), String> {
        let descriptor_path = normalized_absolute_with_missing_tail(path)?;
        let base_root = normalized_absolute_with_missing_tail(&self.base_game_root)?;
        let data_root = normalized_absolute_with_missing_tail(&self.resolved_data_root(path))?;
        if path_is_same_or_child(&descriptor_path, &base_root) {
            return Err(format!(
                "Project descriptor must be outside the extracted base game directory: {}",
                path.display()
            ));
        }
        if path_is_same_or_child(&data_root, &base_root)
            || path_is_same_or_child(&base_root, &data_root)
        {
            return Err(format!(
                "Project data must not overlap the extracted base game directory: {}",
                data_root.display()
            ));
        }
        Ok(())
    }

    pub(super) fn resolved_data_root(&self, descriptor_path: &Path) -> PathBuf {
        if self.project_data_root.is_absolute() {
            self.project_data_root.clone()
        } else {
            descriptor_path
                .parent()
                .filter(|parent| !parent.as_os_str().is_empty())
                .unwrap_or_else(|| Path::new("."))
                .join(&self.project_data_root)
        }
    }

    fn validate(&self, path: &Path) -> Result<(), String> {
        if self.format_version != SMS_PROJECT_FORMAT_VERSION {
            return Err(format!(
                "Project '{}' uses format version {}; this editor supports version {SMS_PROJECT_FORMAT_VERSION}",
                path.display(),
                self.format_version
            ));
        }
        if self.kind != SMS_PROJECT_KIND {
            return Err(format!(
                "Project '{}' has unsupported kind '{}'",
                path.display(),
                self.kind
            ));
        }
        if self.name.trim().is_empty() {
            return Err(format!("Project '{}' has no name", path.display()));
        }
        if self.project_id.trim().is_empty() {
            return Err(format!("Project '{}' has no project id", path.display()));
        }
        if self.stage_cameras.iter().any(|(stage, camera)| {
            stage.trim().is_empty()
                || !stage
                    .chars()
                    .all(|character| character.is_ascii_alphanumeric() || "_-".contains(character))
                || !camera.is_valid()
        }) {
            return Err(format!(
                "Project '{}' has an invalid saved stage camera",
                path.display()
            ));
        }
        if self.base_game_root.as_os_str().is_empty() {
            return Err(format!(
                "Project '{}' has no extracted base game directory",
                path.display()
            ));
        }
        if self.project_data_root.as_os_str().is_empty() {
            return Err(format!(
                "Project '{}' has no project data path",
                path.display()
            ));
        }
        if !self.project_data_root.is_absolute()
            && self.project_data_root.components().any(|component| {
                matches!(
                    component,
                    Component::ParentDir | Component::RootDir | Component::Prefix(_)
                )
            })
        {
            return Err(format!(
                "Project '{}' has an unsafe relative data path '{}'",
                path.display(),
                self.project_data_root.display()
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone)]
pub(super) struct OpenProject {
    pub(super) descriptor_path: PathBuf,
    pub(super) descriptor: SmsProjectFile,
}

impl OpenProject {
    pub(super) fn load(path: impl AsRef<Path>) -> Result<Self, String> {
        let descriptor_path = absolute_path(path.as_ref())?;
        let descriptor = SmsProjectFile::load(&descriptor_path)?;
        Ok(Self {
            descriptor_path,
            descriptor,
        })
    }

    pub(super) fn save(&self) -> Result<(), String> {
        self.descriptor.save(&self.descriptor_path)
    }

    pub(super) fn data_root(&self) -> PathBuf {
        self.descriptor.resolved_data_root(&self.descriptor_path)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct RecentProjectEntry {
    pub(super) path: PathBuf,
    pub(super) last_opened_unix: u64,
    #[serde(default)]
    pub(super) name: String,
    #[serde(default)]
    pub(super) base_game_root: PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct RecentProjectIndex {
    format_version: u32,
    kind: String,
    #[serde(default)]
    projects: Vec<RecentProjectEntry>,
}

impl Default for RecentProjectIndex {
    fn default() -> Self {
        Self {
            format_version: RECENT_PROJECTS_FORMAT_VERSION,
            kind: RECENT_PROJECTS_KIND.to_string(),
            projects: Vec::new(),
        }
    }
}

#[derive(Debug, Clone)]
pub(super) struct RecentProjects {
    config_path: Option<PathBuf>,
    entries: Vec<RecentProjectEntry>,
}

impl RecentProjects {
    #[cfg(not(test))]
    pub(super) fn load_default() -> Self {
        let Some(config_path) = recent_projects_config_path() else {
            return Self::empty();
        };
        Self::load_from(config_path)
    }

    pub(super) fn empty() -> Self {
        Self {
            config_path: None,
            entries: Vec::new(),
        }
    }

    pub(super) fn load_from(config_path: PathBuf) -> Self {
        let entries = fs::read_to_string(&config_path)
            .ok()
            .and_then(|text| toml::from_str::<RecentProjectIndex>(&text).ok())
            .filter(|index| {
                index.kind == RECENT_PROJECTS_KIND
                    && index.format_version == RECENT_PROJECTS_FORMAT_VERSION
            })
            .map_or_else(Vec::new, |index| index.projects);
        Self {
            config_path: Some(config_path),
            entries,
        }
    }

    pub(super) fn entries(&self) -> &[RecentProjectEntry] {
        &self.entries
    }

    pub(super) fn touch(&mut self, project: &OpenProject) -> Result<(), String> {
        let path = absolute_path(&project.descriptor_path)?;
        self.entries
            .retain(|entry| !paths_equal(&entry.path, &path));
        self.entries.insert(
            0,
            RecentProjectEntry {
                path,
                last_opened_unix: unix_timestamp(),
                name: project.descriptor.name.clone(),
                base_game_root: project.descriptor.base_game_root.clone(),
            },
        );
        self.entries.truncate(MAX_RECENT_PROJECTS);
        self.save()
    }

    pub(super) fn remove(&mut self, path: &Path) -> Result<(), String> {
        self.entries.retain(|entry| !paths_equal(&entry.path, path));
        self.save()
    }

    fn save(&self) -> Result<(), String> {
        let Some(config_path) = &self.config_path else {
            return Ok(());
        };
        if let Some(parent) = config_path.parent() {
            fs::create_dir_all(parent).map_err(|error| {
                format!(
                    "Could not create recent-project directory '{}': {error}",
                    parent.display()
                )
            })?;
        }
        let index = RecentProjectIndex {
            format_version: RECENT_PROJECTS_FORMAT_VERSION,
            kind: RECENT_PROJECTS_KIND.to_string(),
            projects: self.entries.clone(),
        };
        let text = toml::to_string_pretty(&index)
            .map_err(|error| format!("Could not serialize recent projects: {error}"))?;
        write_synced(config_path, text.as_bytes()).map_err(|error| {
            format!(
                "Could not save recent projects '{}': {error}",
                config_path.display()
            )
        })
    }
}

pub(super) fn default_project_data_root(descriptor_path: &Path) -> PathBuf {
    let stem = descriptor_path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .filter(|stem| !stem.trim().is_empty())
        .unwrap_or("project");
    PathBuf::from(format!("{stem}.smsdata"))
}

pub(super) fn ensure_sms_extension(path: &Path) -> Result<(), String> {
    let is_sms = path
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case(SMS_PROJECT_EXTENSION));
    if is_sms {
        Ok(())
    } else {
        Err(format!(
            "SMS Editor projects must use the .{SMS_PROJECT_EXTENSION} extension: {}",
            path.display()
        ))
    }
}

pub(super) fn with_sms_extension(mut path: PathBuf) -> PathBuf {
    if ensure_sms_extension(&path).is_err() {
        path.set_extension(SMS_PROJECT_EXTENSION);
    }
    path
}

pub(super) fn import_legacy_project(
    legacy_root: &Path,
    descriptor_path: &Path,
    schema_source_root: Option<PathBuf>,
) -> Result<OpenProject, String> {
    let manifest_path = legacy_root.join("sms-project.toml");
    let manifest_text = fs::read_to_string(&manifest_path).map_err(|error| {
        format!(
            "Could not read legacy project manifest '{}': {error}",
            manifest_path.display()
        )
    })?;
    let manifest: EditorProjectManifest = toml::from_str(&manifest_text).map_err(|error| {
        format!(
            "Could not parse legacy project manifest '{}': {error}",
            manifest_path.display()
        )
    })?;
    if manifest.kind != SMS_PROJECT_KIND {
        return Err(format!(
            "Legacy project manifest '{}' has unsupported kind '{}'",
            manifest_path.display(),
            manifest.kind
        ));
    }
    let name = descriptor_path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .or_else(|| legacy_root.file_name().and_then(|name| name.to_str()))
        .unwrap_or("Imported Project")
        .to_string();
    let legacy_root = absolute_path(legacy_root)?;
    let mut descriptor =
        SmsProjectFile::new(name, manifest.base_path, legacy_root, schema_source_root);
    descriptor.last_stage = manifest.changed_files.iter().find_map(|path| {
        path.file_name()
            .and_then(|name| name.to_str())
            .and_then(|name| name.strip_suffix(".scene.json"))
            .map(str::to_owned)
    });
    let descriptor_path = absolute_path(&with_sms_extension(descriptor_path.to_path_buf()))?;
    descriptor.save(&descriptor_path)?;
    Ok(OpenProject {
        descriptor_path,
        descriptor,
    })
}

pub(super) fn recent_age_label(last_opened_unix: u64) -> String {
    let elapsed = unix_timestamp().saturating_sub(last_opened_unix);
    match elapsed {
        0..=59 => "Opened just now".to_string(),
        60..=3_599 => format!("Opened {}m ago", elapsed / 60),
        3_600..=86_399 => format!("Opened {}h ago", elapsed / 3_600),
        _ => format!("Opened {}d ago", elapsed / 86_400),
    }
}

#[cfg(not(test))]
fn recent_projects_config_path() -> Option<PathBuf> {
    #[cfg(target_os = "windows")]
    {
        std::env::var_os("APPDATA")
            .map(PathBuf::from)
            .map(|root| root.join("SMS Editor").join("recent-projects.toml"))
    }
    #[cfg(target_os = "macos")]
    {
        std::env::var_os("HOME").map(PathBuf::from).map(|root| {
            root.join("Library")
                .join("Application Support")
                .join("SMS Editor")
                .join("recent-projects.toml")
        })
    }
    #[cfg(all(not(target_os = "windows"), not(target_os = "macos")))]
    {
        std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| {
                std::env::var_os("HOME")
                    .map(PathBuf::from)
                    .map(|home| home.join(".config"))
            })
            .map(|root| root.join("sms-editor").join("recent-projects.toml"))
    }
}

fn absolute_path(path: &Path) -> Result<PathBuf, String> {
    if path.is_absolute() {
        Ok(path.to_path_buf())
    } else {
        std::env::current_dir()
            .map(|current| current.join(path))
            .map_err(|error| format!("Could not resolve path '{}': {error}", path.display()))
    }
}

fn paths_equal(left: &Path, right: &Path) -> bool {
    #[cfg(windows)]
    {
        left.to_string_lossy()
            .eq_ignore_ascii_case(&right.to_string_lossy())
    }
    #[cfg(not(windows))]
    {
        left == right
    }
}

fn normalized_absolute_with_missing_tail(path: &Path) -> Result<PathBuf, String> {
    let absolute = absolute_path(path)?;
    if let Ok(canonical) = fs::canonicalize(&absolute) {
        return Ok(canonical);
    }
    let mut cursor = absolute.as_path();
    let mut missing = Vec::new();
    while !cursor.exists() {
        let Some(name) = cursor.file_name() else {
            return Ok(absolute);
        };
        missing.push(name.to_os_string());
        let Some(parent) = cursor.parent() else {
            return Ok(absolute);
        };
        cursor = parent;
    }
    let mut normalized = fs::canonicalize(cursor).unwrap_or_else(|_| cursor.to_path_buf());
    for name in missing.into_iter().rev() {
        normalized.push(name);
    }
    Ok(normalized)
}

fn path_is_same_or_child(candidate: &Path, parent: &Path) -> bool {
    let candidate = comparable_components(candidate);
    let parent = comparable_components(parent);
    candidate.len() >= parent.len()
        && candidate
            .iter()
            .zip(parent.iter())
            .all(|(candidate, parent)| candidate == parent)
}

fn comparable_components(path: &Path) -> Vec<String> {
    path.components()
        .map(|component| {
            let value = component.as_os_str().to_string_lossy();
            #[cfg(windows)]
            {
                value.to_ascii_lowercase()
            }
            #[cfg(not(windows))]
            {
                value.into_owned()
            }
        })
        .collect()
}

fn write_synced(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let mut file = File::create(path)?;
    file.write_all(bytes)?;
    file.sync_all()
}

fn unix_timestamp() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn new_project_id() -> String {
    let sequence = PROJECT_ID_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    format!("{nanos:032x}-{:08x}-{sequence:08x}", std::process::id())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temporary_path(label: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "sms-editor-project-file-{label}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
    }

    #[test]
    fn sms_project_round_trip_defines_and_resolves_its_data_root() {
        let root = temporary_path("round-trip");
        fs::create_dir_all(&root).unwrap();
        let descriptor_path = root.join("Isle Delfino.sms");
        let mut project = SmsProjectFile::new(
            "Isle Delfino",
            PathBuf::from(r"C:\Games\SunshineJPExtract"),
            PathBuf::from("Isle Delfino.smsdata"),
            Some(PathBuf::from(r"C:\src\sms")),
        );
        project.stage_cameras.insert(
            "dolpic0".to_string(),
            ProjectCameraState {
                focus: [100.0, 200.0, 300.0],
                distance: 4_000.0,
                yaw_degrees: 215.0,
                pitch_degrees: -28.0,
                viewport_pan: [12.0, -8.0],
                viewport_zoom: 1.25,
                camera_speed: 0.75,
            },
        );

        project.save(&descriptor_path).unwrap();
        let reopened = OpenProject::load(&descriptor_path).unwrap();

        assert_eq!(reopened.descriptor, project);
        assert_eq!(reopened.data_root(), root.join("Isle Delfino.smsdata"));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn sms_project_rejects_parent_traversal_in_relative_data_root() {
        let project = SmsProjectFile::new(
            "Unsafe",
            PathBuf::from(r"C:\Games\SunshineJPExtract"),
            PathBuf::from("../outside"),
            None,
        );
        let error = project
            .save(Path::new("unsafe.sms"))
            .expect_err("parent traversal must be rejected");
        assert!(error.contains("unsafe relative data path"));
    }

    #[test]
    fn sms_project_refuses_descriptor_and_data_paths_inside_the_base_game() {
        let root = temporary_path("base-overlap");
        let base_root = root.join("SunshineJPExtract");
        fs::create_dir_all(&base_root).unwrap();
        let project = SmsProjectFile::new(
            "Unsafe Location",
            base_root.clone(),
            PathBuf::from("Unsafe Location.smsdata"),
            None,
        );

        let error = project
            .save(&base_root.join("Unsafe Location.sms"))
            .expect_err("the project descriptor must stay outside the base game");

        assert!(error.contains("descriptor must be outside"));
        assert!(!base_root.join("Unsafe Location.sms").exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn legacy_import_wraps_the_existing_data_without_moving_it() {
        let root = temporary_path("legacy-import");
        let base_root = root.join("SunshineJPExtract");
        let legacy_root = root.join("old-project");
        fs::create_dir_all(&base_root).unwrap();
        fs::create_dir_all(&legacy_root).unwrap();
        let mut manifest = EditorProjectManifest::new(
            fs::canonicalize(&base_root).unwrap(),
            PathBuf::from("files"),
            "legacy-id",
        );
        manifest
            .changed_files
            .push(PathBuf::from("editor/stages/dolpic0.scene.json"));
        fs::write(
            legacy_root.join("sms-project.toml"),
            toml::to_string_pretty(&manifest).unwrap(),
        )
        .unwrap();
        let descriptor_path = root.join("Imported.sms");

        let imported = import_legacy_project(&legacy_root, &descriptor_path, None).unwrap();

        assert_eq!(
            fs::canonicalize(imported.data_root()).unwrap(),
            fs::canonicalize(&legacy_root).unwrap()
        );
        assert_eq!(imported.descriptor.last_stage.as_deref(), Some("dolpic0"));
        assert!(legacy_root.join("sms-project.toml").is_file());
        assert!(descriptor_path.is_file());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn recent_projects_are_deduplicated_newest_first_and_persisted() {
        let root = temporary_path("recents");
        fs::create_dir_all(&root).unwrap();
        let config_path = root.join("recent-projects.toml");
        let first_path = root.join("First.sms");
        let second_path = root.join("Second.sms");
        let first = OpenProject {
            descriptor_path: first_path,
            descriptor: SmsProjectFile::new(
                "First",
                PathBuf::from(r"C:\Games\SunshineJPExtract"),
                PathBuf::from("First.smsdata"),
                None,
            ),
        };
        let second = OpenProject {
            descriptor_path: second_path,
            descriptor: SmsProjectFile::new(
                "Second",
                PathBuf::from(r"C:\Games\SunshineJPExtract"),
                PathBuf::from("Second.smsdata"),
                None,
            ),
        };
        let mut recents = RecentProjects::load_from(config_path.clone());

        recents.touch(&first).unwrap();
        recents.touch(&second).unwrap();
        recents.touch(&first).unwrap();

        assert_eq!(recents.entries().len(), 2);
        assert_eq!(recents.entries()[0].name, "First");
        assert_eq!(recents.entries()[1].name, "Second");
        let reopened = RecentProjects::load_from(config_path);
        assert_eq!(reopened.entries(), recents.entries());
        fs::remove_dir_all(root).unwrap();
    }
}
