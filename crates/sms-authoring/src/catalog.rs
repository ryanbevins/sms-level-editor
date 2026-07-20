use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsStr;
use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};
use std::str::FromStr;

use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

use crate::import::normalize_legacy_render_collision_winding;
use crate::{
    import_model, AuthoringError, CollisionDocument, ColorSet, Diagnostic, DiagnosticCode,
    ModelAssetDocument, ModelCoordinateSpace, ModelImportOptions, ModelMaterial, ModelMesh,
    ModelNode, ModelPrimitive, ModelTexture, TexCoordSet, MODEL_ASSET_FORMAT_VERSION,
};

pub const MODEL_ASSET_MANIFEST_VERSION: u32 = 1;
const GEOMETRY_BLOB_VERSION: u32 = 1;
const COLLISION_BLOB_VERSION: u32 = 1;
const MANAGED_DIRECTORY: &str = ".sms-assets";
const MAX_MANIFEST_BYTES: u64 = 64 * 1024 * 1024;
const MAX_MANAGED_BLOB_BYTES: u64 = 512 * 1024 * 1024;
const MAX_MANAGED_TOTAL_BYTES: u64 = 2 * 1024 * 1024 * 1024;

/// Stable project-local identity for a model asset.
///
/// Paths and display names may change without invalidating references to this
/// value. A duplicate intentionally receives a new ID.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct AssetId(Uuid);

impl AssetId {
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }

    pub fn as_uuid(self) -> Uuid {
        self.0
    }
}

impl Default for AssetId {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for AssetId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

impl FromStr for AssetId {
    type Err = uuid::Error;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Uuid::parse_str(value).map(Self)
    }
}

#[derive(Debug, Error)]
pub enum CatalogError {
    #[error("catalog I/O failed for {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("model authoring failed: {0}")]
    Authoring(#[from] AuthoringError),
    #[error("catalog JSON serialization failed: {0}")]
    Serialization(#[from] serde_json::Error),
    #[error("unsafe catalog path {path}: {message}")]
    UnsafePath { path: PathBuf, message: String },
    #[error("invalid model manifest {path}: {message}")]
    InvalidManifest { path: PathBuf, message: String },
    #[error("catalog asset {0} was not found")]
    NotFound(AssetId),
    #[error("catalog contains more than one manifest for asset ID {0}")]
    DuplicateAssetId(AssetId),
    #[error("catalog destination already exists: {0}")]
    AlreadyExists(PathBuf),
    #[error("catalog asset {asset_id} is still referenced by {references:?}")]
    Referenced {
        asset_id: AssetId,
        references: Vec<AssetReference>,
    },
    #[error("managed blob {path} is invalid: {message}")]
    InvalidBlob { path: PathBuf, message: String },
}

pub type CatalogResult<T> = Result<T, CatalogError>;

impl CatalogError {
    fn io(path: impl Into<PathBuf>, source: std::io::Error) -> Self {
        Self::Io {
            path: path.into(),
            source,
        }
    }

    fn unsafe_path(path: impl Into<PathBuf>, message: impl Into<String>) -> Self {
        Self::UnsafePath {
            path: path.into(),
            message: message.into(),
        }
    }

    fn manifest(path: impl Into<PathBuf>, message: impl Into<String>) -> Self {
        Self::InvalidManifest {
            path: path.into(),
            message: message.into(),
        }
    }

    fn blob(path: impl Into<PathBuf>, message: impl Into<String>) -> Self {
        Self::InvalidBlob {
            path: path.into(),
            message: message.into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AssetReference {
    /// Human-readable owner, such as a stage or placed-instance name.
    pub owner: String,
    /// Optional source-free project path used to locate the referencing data.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_path: Option<PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CatalogAssetEntry {
    pub id: AssetId,
    pub name: String,
    /// Path relative to the project `Content` directory.
    pub relative_path: PathBuf,
    pub mesh_count: usize,
    pub material_count: usize,
    pub texture_count: usize,
    pub has_collision: bool,
}

impl CatalogAssetEntry {
    pub fn folder(&self) -> &Path {
        self.relative_path.parent().unwrap_or_else(|| Path::new(""))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CatalogIssueKind {
    Io,
    UnsafePath,
    InvalidManifest,
    MissingBlob,
    DuplicateAssetId,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CatalogIssue {
    pub kind: CatalogIssueKind,
    pub relative_path: PathBuf,
    pub message: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CatalogScan {
    pub assets: Vec<CatalogAssetEntry>,
    pub issues: Vec<CatalogIssue>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CatalogFilter {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub folder: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub has_collision: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CatalogImportResult {
    pub entry: CatalogAssetEntry,
    pub diagnostics: Vec<Diagnostic>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CatalogDeleteReport {
    pub id: AssetId,
    pub relative_path: PathBuf,
    pub managed_blob_directory_removed: bool,
}

/// Source-free asset storage rooted at a project's `Content` directory.
#[derive(Debug, Clone)]
pub struct ModelAssetCatalog {
    content_root: PathBuf,
    canonical_content_root: PathBuf,
}

impl ModelAssetCatalog {
    pub fn open_project(project_root: impl AsRef<Path>) -> CatalogResult<Self> {
        let project_root = project_root.as_ref();
        let metadata =
            fs::metadata(project_root).map_err(|source| CatalogError::io(project_root, source))?;
        if !metadata.is_dir() {
            return Err(CatalogError::unsafe_path(
                project_root,
                "project root is not a directory",
            ));
        }
        let canonical_project_root = fs::canonicalize(project_root)
            .map_err(|source| CatalogError::io(project_root, source))?;
        let catalog = Self::open_content_root(project_root.join("Content"))?;
        if !catalog
            .canonical_content_root
            .starts_with(&canonical_project_root)
        {
            return Err(CatalogError::unsafe_path(
                catalog.content_root,
                "the project Content directory resolves outside the project root",
            ));
        }
        Ok(catalog)
    }

    pub fn open_content_root(content_root: impl AsRef<Path>) -> CatalogResult<Self> {
        let content_root = content_root.as_ref();
        fs::create_dir_all(content_root)
            .map_err(|source| CatalogError::io(content_root, source))?;
        let canonical_content_root = fs::canonicalize(content_root)
            .map_err(|source| CatalogError::io(content_root, source))?;
        let metadata = fs::metadata(&canonical_content_root)
            .map_err(|source| CatalogError::io(&canonical_content_root, source))?;
        if !metadata.is_dir() {
            return Err(CatalogError::unsafe_path(
                content_root,
                "content root is not a directory",
            ));
        }
        let catalog = Self {
            content_root: canonical_content_root.clone(),
            canonical_content_root,
        };
        catalog.ensure_relative_directory(Path::new(MANAGED_DIRECTORY))?;
        catalog.ensure_relative_directory(Path::new(MANAGED_DIRECTORY).join(".staging"))?;
        catalog.ensure_relative_directory(Path::new(MANAGED_DIRECTORY).join(".backup"))?;
        Ok(catalog)
    }

    pub fn content_root(&self) -> &Path {
        &self.content_root
    }

    pub fn managed_storage_root(&self) -> PathBuf {
        self.content_root.join(MANAGED_DIRECTORY)
    }

    pub fn create_folder(&self, relative_folder: impl AsRef<Path>) -> CatalogResult<PathBuf> {
        let relative = normalize_folder_path(relative_folder.as_ref())?;
        self.ensure_relative_directory(&relative)?;
        Ok(relative)
    }

    pub fn folders(&self) -> CatalogResult<Vec<PathBuf>> {
        let mut folders = Vec::new();
        self.collect_folders(&self.content_root, Path::new(""), &mut folders)?;
        folders.sort();
        Ok(folders)
    }

    pub fn scan(&self) -> CatalogResult<CatalogScan> {
        let mut paths = Vec::new();
        let mut issues = Vec::new();
        self.collect_manifest_paths(&self.content_root, Path::new(""), &mut paths, &mut issues)?;
        paths.sort();

        let mut assets = Vec::new();
        for (absolute_path, relative_path) in paths {
            match self.read_manifest_index(&absolute_path, &relative_path) {
                Ok(entry) => assets.push(entry),
                Err(error) => issues.push(issue_from_error(relative_path, error)),
            }
        }
        assets.sort_by(|left, right| left.relative_path.cmp(&right.relative_path));

        let mut ids: BTreeMap<AssetId, Vec<PathBuf>> = BTreeMap::new();
        for asset in &assets {
            ids.entry(asset.id)
                .or_default()
                .push(asset.relative_path.clone());
        }
        for (id, paths) in ids {
            if paths.len() <= 1 {
                continue;
            }
            for path in &paths {
                issues.push(CatalogIssue {
                    kind: CatalogIssueKind::DuplicateAssetId,
                    relative_path: path.clone(),
                    message: format!(
                        "asset ID {id} is duplicated by {} manifests: {}",
                        paths.len(),
                        paths
                            .iter()
                            .map(|value| value.display().to_string())
                            .collect::<Vec<_>>()
                            .join(", ")
                    ),
                });
            }
        }
        issues.sort_by(|left, right| {
            left.relative_path
                .cmp(&right.relative_path)
                .then_with(|| left.message.cmp(&right.message))
        });
        Ok(CatalogScan { assets, issues })
    }

    pub fn search(&self, filter: &CatalogFilter) -> CatalogResult<CatalogScan> {
        let mut scan = self.scan()?;
        let folder = filter
            .folder
            .as_deref()
            .map(normalize_folder_path)
            .transpose()?;
        let tokens = filter
            .text
            .as_deref()
            .unwrap_or_default()
            .split_whitespace()
            .map(str::to_lowercase)
            .collect::<Vec<_>>();
        scan.assets.retain(|asset| {
            if let Some(folder) = &folder {
                if !asset.folder().starts_with(folder) {
                    return false;
                }
            }
            if filter
                .has_collision
                .is_some_and(|expected| asset.has_collision != expected)
            {
                return false;
            }
            if tokens.is_empty() {
                return true;
            }
            let haystack =
                format!("{} {}", asset.name, asset.relative_path.display()).to_lowercase();
            tokens.iter().all(|token| haystack.contains(token))
        });
        Ok(scan)
    }

    pub fn create_asset(
        &self,
        relative_path: impl AsRef<Path>,
        document: &ModelAssetDocument,
    ) -> CatalogResult<CatalogAssetEntry> {
        let relative_path = normalize_asset_path(relative_path.as_ref())?;
        let absolute_path = self.prepare_manifest_target(&relative_path)?;
        if absolute_path
            .try_exists()
            .map_err(|source| CatalogError::io(&absolute_path, source))?
        {
            return Err(CatalogError::AlreadyExists(relative_path));
        }
        let id = AssetId::new();
        self.write_document_at(&absolute_path, &relative_path, id, document, false)?;
        self.read_manifest_index(&absolute_path, &relative_path)
    }

    pub fn import_gltf(
        &self,
        relative_path: impl AsRef<Path>,
        source: impl AsRef<Path>,
        options: &ModelImportOptions,
    ) -> CatalogResult<CatalogImportResult> {
        let relative_path = normalize_asset_path(relative_path.as_ref())?;
        let mut imported = import_model(source.as_ref(), options)?;
        imported.asset.name = asset_display_name(&relative_path)?;
        let diagnostics = imported.diagnostics;
        let entry = self.create_asset(&relative_path, &imported.asset)?;
        Ok(CatalogImportResult { entry, diagnostics })
    }

    pub fn load_asset(&self, id: AssetId) -> CatalogResult<ModelAssetDocument> {
        let entry = self.find_entry(id)?;
        self.load_asset_path(&entry.relative_path)
    }

    pub fn load_asset_path(
        &self,
        relative_path: impl AsRef<Path>,
    ) -> CatalogResult<ModelAssetDocument> {
        let relative_path = normalize_asset_path(relative_path.as_ref())?;
        let absolute_path = self.safe_existing_relative_file(&relative_path)?;
        let manifest = self.read_manifest(&absolute_path)?;
        manifest.validate_structure(&absolute_path)?;
        self.load_document_from_manifest(&manifest)
    }

    pub fn save_asset(
        &self,
        id: AssetId,
        document: &ModelAssetDocument,
    ) -> CatalogResult<CatalogAssetEntry> {
        let entry = self.find_entry(id)?;
        let absolute_path = self.safe_existing_relative_file(&entry.relative_path)?;
        self.write_document_at(&absolute_path, &entry.relative_path, id, document, true)?;
        self.read_manifest_index(&absolute_path, &entry.relative_path)
    }

    pub fn rename_asset(&self, id: AssetId, new_name: &str) -> CatalogResult<CatalogAssetEntry> {
        let clean_name = normalize_asset_filename(new_name)?;
        let entry = self.find_entry(id)?;
        let mut destination = entry.folder().to_path_buf();
        destination.push(format!("{clean_name}.smsmodel"));
        let destination = normalize_asset_path(&destination)?;

        let mut document = self.load_asset(id)?;
        document.name = clean_name;
        if destination == entry.relative_path {
            return self.save_asset(id, &document);
        }

        self.relocate_manifest(&entry.relative_path, &destination)?;
        let absolute_destination = self.safe_existing_relative_file(&destination)?;
        if let Err(error) =
            self.write_document_at(&absolute_destination, &destination, id, &document, true)
        {
            let _ = self.relocate_manifest(&destination, &entry.relative_path);
            return Err(error);
        }
        self.read_manifest_index(&absolute_destination, &destination)
    }

    pub fn move_asset(
        &self,
        id: AssetId,
        destination_folder: impl AsRef<Path>,
    ) -> CatalogResult<CatalogAssetEntry> {
        let entry = self.find_entry(id)?;
        let folder = normalize_folder_path(destination_folder.as_ref())?;
        let file_name = entry
            .relative_path
            .file_name()
            .ok_or_else(|| CatalogError::unsafe_path(&entry.relative_path, "missing file name"))?;
        let destination = normalize_asset_path(&folder.join(file_name))?;
        if destination == entry.relative_path {
            return Ok(entry);
        }
        self.relocate_manifest(&entry.relative_path, &destination)?;
        let absolute = self.safe_existing_relative_file(&destination)?;
        self.read_manifest_index(&absolute, &destination)
    }

    pub fn duplicate_asset(
        &self,
        id: AssetId,
        destination_path: impl AsRef<Path>,
    ) -> CatalogResult<CatalogAssetEntry> {
        let destination_path = normalize_asset_path(destination_path.as_ref())?;
        let mut document = self.load_asset(id)?;
        document.name = asset_display_name(&destination_path)?;
        self.create_asset(destination_path, &document)
    }

    /// Deletes only after the caller supplies a complete, empty reference set.
    /// The editor/scene layer remains responsible for discovering references.
    pub fn delete_asset(
        &self,
        id: AssetId,
        references: &[AssetReference],
    ) -> CatalogResult<CatalogDeleteReport> {
        if !references.is_empty() {
            return Err(CatalogError::Referenced {
                asset_id: id,
                references: references.to_vec(),
            });
        }
        let entry = self.find_entry(id)?;
        let manifest = self.safe_existing_relative_file(&entry.relative_path)?;
        let operation_id = Uuid::new_v4();
        let deleted_manifest = manifest.with_file_name(format!(
            ".{}.{}.deleted",
            manifest
                .file_name()
                .and_then(OsStr::to_str)
                .unwrap_or("asset.smsmodel"),
            operation_id
        ));
        fs::rename(&manifest, &deleted_manifest)
            .map_err(|source| CatalogError::io(&manifest, source))?;

        let managed_asset = self.managed_asset_directory(id);
        let backup_asset = self
            .content_root
            .join(MANAGED_DIRECTORY)
            .join(".backup")
            .join(format!("delete-{id}-{operation_id}"));
        let managed_exists = managed_asset
            .try_exists()
            .map_err(|source| CatalogError::io(&managed_asset, source))?;
        if managed_exists {
            if let Err(source) = fs::rename(&managed_asset, &backup_asset) {
                let _ = fs::rename(&deleted_manifest, &manifest);
                return Err(CatalogError::io(&managed_asset, source));
            }
        }

        let _ = fs::remove_file(&deleted_manifest);
        let blobs_removed = !managed_exists || fs::remove_dir_all(&backup_asset).is_ok();
        Ok(CatalogDeleteReport {
            id,
            relative_path: entry.relative_path,
            managed_blob_directory_removed: blobs_removed,
        })
    }

    fn find_entry(&self, id: AssetId) -> CatalogResult<CatalogAssetEntry> {
        let matching = self
            .scan()?
            .assets
            .into_iter()
            .filter(|entry| entry.id == id)
            .collect::<Vec<_>>();
        match matching.as_slice() {
            [] => Err(CatalogError::NotFound(id)),
            [entry] => Ok(entry.clone()),
            _ => Err(CatalogError::DuplicateAssetId(id)),
        }
    }

    fn relocate_manifest(&self, source: &Path, destination: &Path) -> CatalogResult<()> {
        let source_absolute = self.safe_existing_relative_file(source)?;
        let destination_absolute = self.prepare_manifest_target(destination)?;
        if destination_absolute
            .try_exists()
            .map_err(|source| CatalogError::io(&destination_absolute, source))?
        {
            return Err(CatalogError::AlreadyExists(destination.to_path_buf()));
        }
        fs::rename(&source_absolute, &destination_absolute)
            .map_err(|source| CatalogError::io(&source_absolute, source))
    }

    fn write_document_at(
        &self,
        manifest_path: &Path,
        relative_path: &Path,
        id: AssetId,
        document: &ModelAssetDocument,
        replace: bool,
    ) -> CatalogResult<()> {
        let (manifest, blobs) = ModelAssetManifest::from_document(id, document)?;
        let total_blob_bytes = blobs.iter().try_fold(0_u64, |total, blob| {
            total
                .checked_add(blob.reference.byte_length)
                .ok_or_else(|| {
                    CatalogError::blob(&blob.reference.path, "blob byte total overflowed")
                })
        })?;
        if total_blob_bytes > MAX_MANAGED_TOTAL_BYTES {
            return Err(CatalogError::blob(
                relative_path,
                format!(
                    "asset contains {total_blob_bytes} managed bytes; limit is {MAX_MANAGED_TOTAL_BYTES}"
                ),
            ));
        }
        let mut manifest_bytes = serde_json::to_vec_pretty(&manifest)?;
        manifest_bytes.push(b'\n');
        if manifest_bytes.len() as u64 > MAX_MANIFEST_BYTES {
            return Err(CatalogError::manifest(
                relative_path,
                format!(
                    "manifest is {} bytes; limit is {MAX_MANIFEST_BYTES}",
                    manifest_bytes.len()
                ),
            ));
        }

        let operation_id = Uuid::new_v4();
        let staging_root = self
            .content_root
            .join(MANAGED_DIRECTORY)
            .join(".staging")
            .join(format!("{id}-{operation_id}"));
        fs::create_dir(&staging_root).map_err(|source| CatalogError::io(&staging_root, source))?;
        let staged_result = (|| {
            for blob in &blobs {
                let tail = blob_tail(id, &blob.reference.path)?;
                let destination = staging_root.join(tail);
                let parent = destination.parent().ok_or_else(|| {
                    CatalogError::unsafe_path(&destination, "managed blob has no parent")
                })?;
                fs::create_dir_all(parent).map_err(|source| CatalogError::io(parent, source))?;
                write_new_file(&destination, &blob.bytes)?;
            }
            Ok::<(), CatalogError>(())
        })();
        if let Err(error) = staged_result {
            let _ = fs::remove_dir_all(&staging_root);
            return Err(error);
        }

        let manifest_parent = manifest_path.parent().ok_or_else(|| {
            CatalogError::unsafe_path(manifest_path, "manifest has no parent directory")
        })?;
        let file_name = manifest_path
            .file_name()
            .and_then(OsStr::to_str)
            .ok_or_else(|| {
                CatalogError::unsafe_path(manifest_path, "manifest name is not UTF-8")
            })?;
        let temp_manifest = manifest_parent.join(format!(".{file_name}.{operation_id}.temporary"));
        if let Err(error) = write_new_file(&temp_manifest, &manifest_bytes) {
            let _ = fs::remove_dir_all(&staging_root);
            return Err(error);
        }

        let result = self.commit_staged_asset(
            id,
            manifest_path,
            &temp_manifest,
            &staging_root,
            operation_id,
            replace,
        );
        if result.is_err() {
            let _ = fs::remove_file(&temp_manifest);
            let _ = fs::remove_dir_all(&staging_root);
        }
        result
    }

    fn commit_staged_asset(
        &self,
        id: AssetId,
        manifest_path: &Path,
        temp_manifest: &Path,
        staging_root: &Path,
        operation_id: Uuid,
        replace: bool,
    ) -> CatalogResult<()> {
        let manifest_exists = manifest_path
            .try_exists()
            .map_err(|source| CatalogError::io(manifest_path, source))?;
        if manifest_exists && !replace {
            return Err(CatalogError::AlreadyExists(manifest_path.to_path_buf()));
        }

        let manifest_backup = manifest_path.with_file_name(format!(
            ".{}.{}.backup",
            manifest_path
                .file_name()
                .and_then(OsStr::to_str)
                .unwrap_or("asset.smsmodel"),
            operation_id
        ));
        if manifest_exists {
            fs::rename(manifest_path, &manifest_backup)
                .map_err(|source| CatalogError::io(manifest_path, source))?;
        }

        let asset_directory = self.managed_asset_directory(id);
        let asset_exists = asset_directory
            .try_exists()
            .map_err(|source| CatalogError::io(&asset_directory, source))?;
        let asset_backup = self
            .content_root
            .join(MANAGED_DIRECTORY)
            .join(".backup")
            .join(format!("{id}-{operation_id}"));
        if asset_exists {
            if let Err(source) = fs::rename(&asset_directory, &asset_backup) {
                if manifest_exists {
                    let _ = fs::rename(&manifest_backup, manifest_path);
                }
                return Err(CatalogError::io(&asset_directory, source));
            }
        }

        if let Err(source) = fs::rename(staging_root, &asset_directory) {
            if asset_exists {
                let _ = fs::rename(&asset_backup, &asset_directory);
            }
            if manifest_exists {
                let _ = fs::rename(&manifest_backup, manifest_path);
            }
            return Err(CatalogError::io(staging_root, source));
        }

        if let Err(source) = fs::rename(temp_manifest, manifest_path) {
            let rollback_new = self
                .content_root
                .join(MANAGED_DIRECTORY)
                .join(".staging")
                .join(format!("rollback-{id}-{operation_id}"));
            let _ = fs::rename(&asset_directory, &rollback_new);
            if asset_exists {
                let _ = fs::rename(&asset_backup, &asset_directory);
            }
            if manifest_exists {
                let _ = fs::rename(&manifest_backup, manifest_path);
            }
            let _ = fs::remove_dir_all(&rollback_new);
            return Err(CatalogError::io(temp_manifest, source));
        }

        if manifest_exists {
            let _ = fs::remove_file(&manifest_backup);
        }
        if asset_exists {
            let _ = fs::remove_dir_all(&asset_backup);
        }
        Ok(())
    }

    fn load_document_from_manifest(
        &self,
        manifest: &ModelAssetManifest,
    ) -> CatalogResult<ModelAssetDocument> {
        let mut total_blob_bytes = 0_u64;
        for blob in manifest.blob_references() {
            total_blob_bytes = total_blob_bytes
                .checked_add(blob.byte_length)
                .ok_or_else(|| {
                    CatalogError::blob(&blob.path, "managed blob byte total overflowed")
                })?;
            if total_blob_bytes > MAX_MANAGED_TOTAL_BYTES {
                return Err(CatalogError::blob(
                    &blob.path,
                    format!(
                        "asset references {total_blob_bytes} managed bytes; limit is {MAX_MANAGED_TOTAL_BYTES}"
                    ),
                ));
            }
        }

        let mut meshes = Vec::with_capacity(manifest.meshes.len());
        for mesh in &manifest.meshes {
            let mut primitives = Vec::with_capacity(mesh.primitives.len());
            for primitive in &mesh.primitives {
                let bytes = self.read_managed_blob(manifest.asset_id, &primitive.geometry)?;
                let geometry: PrimitiveGeometryBlob =
                    serde_json::from_slice(&bytes).map_err(|error| {
                        CatalogError::blob(&primitive.geometry.path, error.to_string())
                    })?;
                if geometry.format_version != GEOMETRY_BLOB_VERSION {
                    return Err(CatalogError::blob(
                        &primitive.geometry.path,
                        format!(
                            "unsupported geometry blob version {}; expected {GEOMETRY_BLOB_VERSION}",
                            geometry.format_version
                        ),
                    ));
                }
                primitives.push(ModelPrimitive {
                    positions: geometry.positions,
                    normals: geometry.normals,
                    tangents: geometry.tangents,
                    tex_coords: geometry.tex_coords,
                    colors: geometry.colors,
                    indices: geometry.indices,
                    material: primitive.material,
                });
            }
            meshes.push(ModelMesh {
                name: mesh.name.clone(),
                primitives,
            });
        }

        let mut textures = Vec::with_capacity(manifest.textures.len());
        for texture in &manifest.textures {
            textures.push(ModelTexture {
                name: texture.name.clone(),
                width: texture.width,
                height: texture.height,
                rgba8: self.read_managed_blob(manifest.asset_id, &texture.rgba8)?,
                encode_options: texture.encode_options,
            });
        }

        let collision = if let Some(reference) = &manifest.collision {
            let bytes = self.read_managed_blob(manifest.asset_id, reference)?;
            let blob: CollisionGeometryBlob = serde_json::from_slice(&bytes)
                .map_err(|error| CatalogError::blob(&reference.path, error.to_string()))?;
            if blob.format_version != COLLISION_BLOB_VERSION {
                return Err(CatalogError::blob(
                    &reference.path,
                    format!(
                        "unsupported collision blob version {}; expected {COLLISION_BLOB_VERSION}",
                        blob.format_version
                    ),
                ));
            }
            Some(blob.collision)
        } else {
            None
        };

        let mut document = ModelAssetDocument {
            format_version: manifest.document_format_version,
            coordinate_space: manifest.coordinate_space,
            name: manifest.name.clone(),
            scene_roots: manifest.scene_roots.clone(),
            nodes: manifest.nodes.clone(),
            meshes,
            materials: manifest.materials.clone(),
            textures,
            collision,
            diagnostics: manifest.diagnostics.clone(),
            acknowledged_diagnostics: manifest.acknowledged_diagnostics.clone(),
        };
        if document.migrate_legacy_reflected_z_coordinate_space() {
            document.diagnostics.push(Diagnostic::info(
                DiagnosticCode::CoordinateSpaceMigrated,
                "migrated legacy reflected-Z model coordinates to the canonical glTF-compatible basis",
                None,
            ));
        }
        document.validate()?;
        document.repair_legacy_conservative_materials();
        if normalize_legacy_render_collision_winding(&mut document)? {
            document.diagnostics.push(Diagnostic::info(
                DiagnosticCode::CollisionWindingNormalized,
                "repaired legacy render-derived collision whose walkable terrain faced downward",
                None,
            ));
        }
        document.validate()?;
        Ok(document)
    }

    fn read_manifest_index(
        &self,
        absolute_path: &Path,
        relative_path: &Path,
    ) -> CatalogResult<CatalogAssetEntry> {
        let manifest = self.read_manifest(absolute_path)?;
        manifest.validate_structure(absolute_path)?;
        for reference in manifest.blob_references() {
            self.validate_blob_presence(manifest.asset_id, reference)?;
        }
        Ok(CatalogAssetEntry {
            id: manifest.asset_id,
            name: manifest.name,
            relative_path: relative_path.to_path_buf(),
            mesh_count: manifest.meshes.len(),
            material_count: manifest.materials.len(),
            texture_count: manifest.textures.len(),
            has_collision: manifest.collision.is_some(),
        })
    }

    fn read_manifest(&self, path: &Path) -> CatalogResult<ModelAssetManifest> {
        let bytes = read_limited(path, MAX_MANIFEST_BYTES)?;
        serde_json::from_slice(&bytes)
            .map_err(|error| CatalogError::manifest(path, error.to_string()))
    }

    fn validate_blob_presence(
        &self,
        id: AssetId,
        reference: &ManagedBlobReference,
    ) -> CatalogResult<()> {
        if reference.byte_length > MAX_MANAGED_BLOB_BYTES {
            return Err(CatalogError::blob(
                &reference.path,
                format!(
                    "declared size {} exceeds {MAX_MANAGED_BLOB_BYTES}",
                    reference.byte_length
                ),
            ));
        }
        let path = self.resolve_managed_blob(id, &reference.path)?;
        let metadata = fs::metadata(&path).map_err(|source| CatalogError::io(&path, source))?;
        if !metadata.is_file() {
            return Err(CatalogError::blob(path, "managed blob is not a file"));
        }
        if metadata.len() != reference.byte_length {
            return Err(CatalogError::blob(
                path,
                format!(
                    "size is {}; manifest declares {}",
                    metadata.len(),
                    reference.byte_length
                ),
            ));
        }
        Ok(())
    }

    fn read_managed_blob(
        &self,
        id: AssetId,
        reference: &ManagedBlobReference,
    ) -> CatalogResult<Vec<u8>> {
        self.validate_blob_presence(id, reference)?;
        let path = self.resolve_managed_blob(id, &reference.path)?;
        let bytes = read_limited(&path, MAX_MANAGED_BLOB_BYTES)?;
        let actual = fnv1a64_hex(&bytes);
        if actual != reference.fnv1a64 {
            return Err(CatalogError::blob(
                path,
                format!(
                    "checksum is {actual}; manifest declares {}",
                    reference.fnv1a64
                ),
            ));
        }
        Ok(bytes)
    }

    fn resolve_managed_blob(&self, id: AssetId, value: &str) -> CatalogResult<PathBuf> {
        if value.contains('\\') {
            return Err(CatalogError::unsafe_path(
                value,
                "managed paths must use canonical forward slashes",
            ));
        }
        let path = Path::new(value);
        let expected_prefix = Path::new(MANAGED_DIRECTORY).join(id.to_string());
        if !path.starts_with(&expected_prefix) {
            return Err(CatalogError::unsafe_path(
                path,
                format!("managed blob must be under {}/{}", MANAGED_DIRECTORY, id),
            ));
        }
        validate_relative_components(path, true)?;
        self.safe_existing_relative_file(path)
    }

    fn managed_asset_directory(&self, id: AssetId) -> PathBuf {
        self.content_root
            .join(MANAGED_DIRECTORY)
            .join(id.to_string())
    }

    fn prepare_manifest_target(&self, relative_path: &Path) -> CatalogResult<PathBuf> {
        validate_relative_components(relative_path, false)?;
        let parent = relative_path.parent().unwrap_or_else(|| Path::new(""));
        self.ensure_relative_directory(parent)?;
        Ok(self.content_root.join(relative_path))
    }

    fn safe_existing_relative_file(&self, relative_path: &Path) -> CatalogResult<PathBuf> {
        validate_relative_components(relative_path, true)?;
        let mut current = self.content_root.clone();
        for component in relative_path.components() {
            let Component::Normal(component) = component else {
                return Err(CatalogError::unsafe_path(
                    relative_path,
                    "path is not a normalized relative path",
                ));
            };
            current.push(component);
            let metadata = fs::symlink_metadata(&current)
                .map_err(|source| CatalogError::io(&current, source))?;
            if metadata.file_type().is_symlink() {
                return Err(CatalogError::unsafe_path(
                    relative_path,
                    "symbolic links are not allowed in catalog paths",
                ));
            }
        }
        let canonical =
            fs::canonicalize(&current).map_err(|source| CatalogError::io(&current, source))?;
        if !canonical.starts_with(&self.canonical_content_root) {
            return Err(CatalogError::unsafe_path(
                relative_path,
                "resolved path leaves the Content directory",
            ));
        }
        Ok(canonical)
    }

    fn ensure_relative_directory(&self, relative_path: impl AsRef<Path>) -> CatalogResult<PathBuf> {
        let relative_path = relative_path.as_ref();
        validate_relative_components(relative_path, true)?;
        let mut current = self.content_root.clone();
        for component in relative_path.components() {
            let Component::Normal(component) = component else {
                return Err(CatalogError::unsafe_path(
                    relative_path,
                    "directory is not a normalized relative path",
                ));
            };
            current.push(component);
            match fs::symlink_metadata(&current) {
                Ok(metadata) => {
                    if metadata.file_type().is_symlink() {
                        return Err(CatalogError::unsafe_path(
                            relative_path,
                            "symbolic links are not allowed in catalog paths",
                        ));
                    }
                    if !metadata.is_dir() {
                        return Err(CatalogError::unsafe_path(
                            relative_path,
                            "a path component is not a directory",
                        ));
                    }
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    fs::create_dir(&current)
                        .map_err(|source| CatalogError::io(&current, source))?;
                }
                Err(source) => return Err(CatalogError::io(&current, source)),
            }
        }
        let canonical =
            fs::canonicalize(&current).map_err(|source| CatalogError::io(&current, source))?;
        if !canonical.starts_with(&self.canonical_content_root) {
            return Err(CatalogError::unsafe_path(
                relative_path,
                "resolved directory leaves the Content directory",
            ));
        }
        Ok(canonical)
    }

    fn collect_manifest_paths(
        &self,
        absolute_directory: &Path,
        relative_directory: &Path,
        output: &mut Vec<(PathBuf, PathBuf)>,
        issues: &mut Vec<CatalogIssue>,
    ) -> CatalogResult<()> {
        let entries = fs::read_dir(absolute_directory)
            .map_err(|source| CatalogError::io(absolute_directory, source))?;
        for entry in entries {
            let entry = match entry {
                Ok(value) => value,
                Err(error) => {
                    issues.push(CatalogIssue {
                        kind: CatalogIssueKind::Io,
                        relative_path: relative_directory.to_path_buf(),
                        message: error.to_string(),
                    });
                    continue;
                }
            };
            let name = entry.file_name();
            if relative_directory.as_os_str().is_empty() && name == OsStr::new(MANAGED_DIRECTORY) {
                continue;
            }
            let relative = relative_directory.join(&name);
            let metadata = match fs::symlink_metadata(entry.path()) {
                Ok(value) => value,
                Err(error) => {
                    issues.push(CatalogIssue {
                        kind: CatalogIssueKind::Io,
                        relative_path: relative,
                        message: error.to_string(),
                    });
                    continue;
                }
            };
            if metadata.file_type().is_symlink() {
                issues.push(CatalogIssue {
                    kind: CatalogIssueKind::UnsafePath,
                    relative_path: relative,
                    message: "catalog scans do not follow symbolic links".to_string(),
                });
            } else if metadata.is_dir() {
                self.collect_manifest_paths(&entry.path(), &relative, output, issues)?;
            } else if metadata.is_file() && has_smsmodel_extension(&entry.path()) {
                output.push((entry.path(), relative));
            }
        }
        Ok(())
    }

    fn collect_folders(
        &self,
        absolute_directory: &Path,
        relative_directory: &Path,
        output: &mut Vec<PathBuf>,
    ) -> CatalogResult<()> {
        let entries = fs::read_dir(absolute_directory)
            .map_err(|source| CatalogError::io(absolute_directory, source))?;
        for entry in entries {
            let entry = entry.map_err(|source| CatalogError::io(absolute_directory, source))?;
            let name = entry.file_name();
            if relative_directory.as_os_str().is_empty() && name == OsStr::new(MANAGED_DIRECTORY) {
                continue;
            }
            let metadata = fs::symlink_metadata(entry.path())
                .map_err(|source| CatalogError::io(entry.path(), source))?;
            if metadata.file_type().is_symlink() || !metadata.is_dir() {
                continue;
            }
            let relative = relative_directory.join(name);
            output.push(relative.clone());
            self.collect_folders(&entry.path(), &relative, output)?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum CatalogAssetKind {
    Model,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ModelAssetManifest {
    manifest_version: u32,
    asset_kind: CatalogAssetKind,
    asset_id: AssetId,
    document_format_version: u32,
    #[serde(default)]
    coordinate_space: ModelCoordinateSpace,
    name: String,
    scene_roots: Vec<u32>,
    nodes: Vec<ModelNode>,
    meshes: Vec<ManifestMesh>,
    materials: Vec<ModelMaterial>,
    textures: Vec<ManifestTexture>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    collision: Option<ManagedBlobReference>,
    #[serde(default)]
    diagnostics: Vec<Diagnostic>,
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    acknowledged_diagnostics: BTreeSet<crate::DiagnosticCode>,
}

impl ModelAssetManifest {
    fn from_document(
        asset_id: AssetId,
        document: &ModelAssetDocument,
    ) -> CatalogResult<(Self, Vec<BlobWrite>)> {
        document.validate()?;
        let mut blobs = Vec::new();
        let mut meshes = Vec::with_capacity(document.meshes.len());
        for (mesh_index, mesh) in document.meshes.iter().enumerate() {
            let mut primitives = Vec::with_capacity(mesh.primitives.len());
            for (primitive_index, primitive) in mesh.primitives.iter().enumerate() {
                let geometry = PrimitiveGeometryBlob {
                    format_version: GEOMETRY_BLOB_VERSION,
                    positions: primitive.positions.clone(),
                    normals: primitive.normals.clone(),
                    tangents: primitive.tangents.clone(),
                    tex_coords: primitive.tex_coords.clone(),
                    colors: primitive.colors.clone(),
                    indices: primitive.indices.clone(),
                };
                let mut bytes = serde_json::to_vec(&geometry)?;
                bytes.push(b'\n');
                let path = geometry_blob_path(asset_id, mesh_index, primitive_index);
                let reference = ManagedBlobReference::new(path, &bytes)?;
                blobs.push(BlobWrite {
                    reference: reference.clone(),
                    bytes,
                });
                primitives.push(ManifestPrimitive {
                    material: primitive.material,
                    geometry: reference,
                });
            }
            meshes.push(ManifestMesh {
                name: mesh.name.clone(),
                primitives,
            });
        }

        let mut textures = Vec::with_capacity(document.textures.len());
        for (index, texture) in document.textures.iter().enumerate() {
            let path = texture_blob_path(asset_id, index);
            let reference = ManagedBlobReference::new(path, &texture.rgba8)?;
            blobs.push(BlobWrite {
                reference: reference.clone(),
                bytes: texture.rgba8.clone(),
            });
            textures.push(ManifestTexture {
                name: texture.name.clone(),
                width: texture.width,
                height: texture.height,
                rgba8: reference,
                encode_options: texture.encode_options,
            });
        }

        let collision = document
            .collision
            .as_ref()
            .map(|collision| {
                let blob = CollisionGeometryBlob {
                    format_version: COLLISION_BLOB_VERSION,
                    collision: collision.clone(),
                };
                let mut bytes = serde_json::to_vec(&blob)?;
                bytes.push(b'\n');
                let reference = ManagedBlobReference::new(collision_blob_path(asset_id), &bytes)?;
                blobs.push(BlobWrite {
                    reference: reference.clone(),
                    bytes,
                });
                Ok::<_, CatalogError>(reference)
            })
            .transpose()?;

        Ok((
            Self {
                manifest_version: MODEL_ASSET_MANIFEST_VERSION,
                asset_kind: CatalogAssetKind::Model,
                asset_id,
                document_format_version: document.format_version,
                coordinate_space: document.coordinate_space,
                name: document.name.clone(),
                scene_roots: document.scene_roots.clone(),
                nodes: document.nodes.clone(),
                meshes,
                materials: document.materials.clone(),
                textures,
                collision,
                diagnostics: document.diagnostics.clone(),
                acknowledged_diagnostics: document.acknowledged_diagnostics.clone(),
            },
            blobs,
        ))
    }

    fn validate_structure(&self, path: &Path) -> CatalogResult<()> {
        if self.manifest_version != MODEL_ASSET_MANIFEST_VERSION {
            return Err(CatalogError::manifest(
                path,
                format!(
                    "unsupported manifest version {}; expected {MODEL_ASSET_MANIFEST_VERSION}",
                    self.manifest_version
                ),
            ));
        }
        if self.document_format_version != MODEL_ASSET_FORMAT_VERSION {
            return Err(CatalogError::manifest(
                path,
                format!(
                    "unsupported model document version {}; expected {MODEL_ASSET_FORMAT_VERSION}",
                    self.document_format_version
                ),
            ));
        }
        if self.name.is_empty() {
            return Err(CatalogError::manifest(path, "asset name is empty"));
        }
        for (mesh_index, mesh) in self.meshes.iter().enumerate() {
            for (primitive_index, primitive) in mesh.primitives.iter().enumerate() {
                let expected = geometry_blob_path(self.asset_id, mesh_index, primitive_index);
                if primitive.geometry.path != expected {
                    return Err(CatalogError::manifest(
                        path,
                        format!(
                            "mesh {mesh_index} primitive {primitive_index} must use managed blob {expected}"
                        ),
                    ));
                }
                primitive.geometry.validate(path)?;
            }
        }
        for (index, texture) in self.textures.iter().enumerate() {
            let expected = texture_blob_path(self.asset_id, index);
            if texture.rgba8.path != expected {
                return Err(CatalogError::manifest(
                    path,
                    format!("texture {index} must use managed blob {expected}"),
                ));
            }
            texture.rgba8.validate(path)?;
        }
        if let Some(collision) = &self.collision {
            let expected = collision_blob_path(self.asset_id);
            if collision.path != expected {
                return Err(CatalogError::manifest(
                    path,
                    format!("collision must use managed blob {expected}"),
                ));
            }
            collision.validate(path)?;
        }
        Ok(())
    }

    fn blob_references(&self) -> Vec<&ManagedBlobReference> {
        let mut output = Vec::new();
        for mesh in &self.meshes {
            for primitive in &mesh.primitives {
                output.push(&primitive.geometry);
            }
        }
        for texture in &self.textures {
            output.push(&texture.rgba8);
        }
        if let Some(collision) = &self.collision {
            output.push(collision);
        }
        output
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ManifestMesh {
    name: String,
    primitives: Vec<ManifestPrimitive>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ManifestPrimitive {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    material: Option<u32>,
    geometry: ManagedBlobReference,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ManifestTexture {
    name: String,
    width: u32,
    height: u32,
    rgba8: ManagedBlobReference,
    encode_options: sms_formats::GxTextureEncodeOptions,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ManagedBlobReference {
    path: String,
    byte_length: u64,
    fnv1a64: String,
}

impl ManagedBlobReference {
    fn new(path: String, bytes: &[u8]) -> CatalogResult<Self> {
        let byte_length = u64::try_from(bytes.len())
            .map_err(|_| CatalogError::blob(&path, "blob size does not fit u64"))?;
        if byte_length > MAX_MANAGED_BLOB_BYTES {
            return Err(CatalogError::blob(
                &path,
                format!("blob is {byte_length} bytes; limit is {MAX_MANAGED_BLOB_BYTES}"),
            ));
        }
        Ok(Self {
            path,
            byte_length,
            fnv1a64: fnv1a64_hex(bytes),
        })
    }

    fn validate(&self, manifest_path: &Path) -> CatalogResult<()> {
        if self.byte_length > MAX_MANAGED_BLOB_BYTES {
            return Err(CatalogError::manifest(
                manifest_path,
                format!(
                    "managed blob {} declares {} bytes; limit is {MAX_MANAGED_BLOB_BYTES}",
                    self.path, self.byte_length
                ),
            ));
        }
        if self.fnv1a64.len() != 16
            || !self
                .fnv1a64
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
        {
            return Err(CatalogError::manifest(
                manifest_path,
                format!("managed blob {} has an invalid FNV-1a checksum", self.path),
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PrimitiveGeometryBlob {
    format_version: u32,
    positions: Vec<[f32; 3]>,
    normals: Vec<[f32; 3]>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    tangents: Vec<[f32; 4]>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    tex_coords: Vec<TexCoordSet>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    colors: Vec<ColorSet>,
    indices: Vec<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CollisionGeometryBlob {
    format_version: u32,
    collision: CollisionDocument,
}

struct BlobWrite {
    reference: ManagedBlobReference,
    bytes: Vec<u8>,
}

fn geometry_blob_path(id: AssetId, mesh: usize, primitive: usize) -> String {
    format!("{MANAGED_DIRECTORY}/{id}/geometry/mesh-{mesh:04}-primitive-{primitive:04}.smsgeom")
}

fn texture_blob_path(id: AssetId, texture: usize) -> String {
    format!("{MANAGED_DIRECTORY}/{id}/textures/texture-{texture:04}.rgba8")
}

fn collision_blob_path(id: AssetId) -> String {
    format!("{MANAGED_DIRECTORY}/{id}/geometry/collision.smscolgeom")
}

fn blob_tail(id: AssetId, value: &str) -> CatalogResult<PathBuf> {
    if value.contains('\\') {
        return Err(CatalogError::unsafe_path(
            value,
            "managed paths must use forward slashes",
        ));
    }
    let path = Path::new(value);
    validate_relative_components(path, true)?;
    let prefix = Path::new(MANAGED_DIRECTORY).join(id.to_string());
    path.strip_prefix(&prefix)
        .map(Path::to_path_buf)
        .map_err(|_| {
            CatalogError::unsafe_path(value, format!("blob does not belong to asset {id}"))
        })
}

fn normalize_asset_path(value: &Path) -> CatalogResult<PathBuf> {
    let mut path = normalized_relative_path(value, false)?;
    match path.extension().and_then(OsStr::to_str) {
        None => {
            path.set_extension("smsmodel");
        }
        Some(extension) if extension.eq_ignore_ascii_case("smsmodel") => {
            path.set_extension("smsmodel");
        }
        Some(_) => {
            return Err(CatalogError::unsafe_path(
                value,
                "model asset paths must use the .smsmodel extension",
            ));
        }
    }
    let stem = path.file_stem().and_then(OsStr::to_str).ok_or_else(|| {
        CatalogError::unsafe_path(value, "model asset file name is not valid UTF-8")
    })?;
    validate_portable_name(stem, value)?;
    Ok(path)
}

fn normalize_folder_path(value: &Path) -> CatalogResult<PathBuf> {
    normalized_relative_path(value, true)
}

fn normalized_relative_path(value: &Path, allow_empty: bool) -> CatalogResult<PathBuf> {
    let mut output = PathBuf::new();
    for component in value.components() {
        let Component::Normal(component) = component else {
            return Err(CatalogError::unsafe_path(
                value,
                "only normalized relative paths are allowed",
            ));
        };
        let text = component
            .to_str()
            .ok_or_else(|| CatalogError::unsafe_path(value, "catalog paths must be valid UTF-8"))?;
        validate_portable_name(text, value)?;
        if output.as_os_str().is_empty() && text.eq_ignore_ascii_case(MANAGED_DIRECTORY) {
            return Err(CatalogError::unsafe_path(
                value,
                format!("{MANAGED_DIRECTORY} is reserved for managed asset data"),
            ));
        }
        output.push(text);
    }
    if output.as_os_str().is_empty() && !allow_empty {
        return Err(CatalogError::unsafe_path(value, "path is empty"));
    }
    Ok(output)
}

fn validate_relative_components(value: &Path, allow_managed: bool) -> CatalogResult<()> {
    if value.as_os_str().is_empty() {
        return Ok(());
    }
    for (index, component) in value.components().enumerate() {
        let Component::Normal(component) = component else {
            return Err(CatalogError::unsafe_path(
                value,
                "only normalized relative paths are allowed",
            ));
        };
        let text = component
            .to_str()
            .ok_or_else(|| CatalogError::unsafe_path(value, "catalog paths must be valid UTF-8"))?;
        if index == 0 && text.eq_ignore_ascii_case(MANAGED_DIRECTORY) && !allow_managed {
            return Err(CatalogError::unsafe_path(
                value,
                format!("{MANAGED_DIRECTORY} is reserved for managed asset data"),
            ));
        }
    }
    Ok(())
}

fn normalize_asset_filename(value: &str) -> CatalogResult<String> {
    let path = Path::new(value);
    let normalized = normalized_relative_path(path, false)?;
    if normalized.components().count() != 1 {
        return Err(CatalogError::unsafe_path(
            path,
            "asset rename accepts a file name, not a path",
        ));
    }
    let name = normalized
        .file_name()
        .and_then(OsStr::to_str)
        .ok_or_else(|| CatalogError::unsafe_path(path, "asset name is not UTF-8"))?;
    let stem = if name.to_ascii_lowercase().ends_with(".smsmodel") {
        &name[..name.len() - ".smsmodel".len()]
    } else {
        name
    };
    validate_portable_name(stem, path)?;
    Ok(stem.to_string())
}

fn asset_display_name(relative_path: &Path) -> CatalogResult<String> {
    let stem = relative_path
        .file_stem()
        .and_then(OsStr::to_str)
        .ok_or_else(|| CatalogError::unsafe_path(relative_path, "missing UTF-8 asset name"))?;
    Ok(stem.to_string())
}

fn validate_portable_name(name: &str, context: &Path) -> CatalogResult<()> {
    let device_stem = name.split('.').next().unwrap_or(name).to_ascii_uppercase();
    let reserved_device = matches!(device_stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || device_stem.strip_prefix("COM").is_some_and(|suffix| {
            matches!(suffix, "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9")
        })
        || device_stem.strip_prefix("LPT").is_some_and(|suffix| {
            matches!(suffix, "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9")
        });
    if name.is_empty()
        || name == "."
        || name == ".."
        || reserved_device
        || name.ends_with(' ')
        || name.ends_with('.')
        || name.chars().any(|character| {
            character.is_control()
                || matches!(
                    character,
                    '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*'
                )
        })
    {
        return Err(CatalogError::unsafe_path(
            context,
            format!("{name:?} is not a portable Content path component"),
        ));
    }
    Ok(())
}

fn has_smsmodel_extension(path: &Path) -> bool {
    path.extension()
        .and_then(OsStr::to_str)
        .is_some_and(|extension| extension.eq_ignore_ascii_case("smsmodel"))
}

fn read_limited(path: &Path, limit: u64) -> CatalogResult<Vec<u8>> {
    let metadata = fs::metadata(path).map_err(|source| CatalogError::io(path, source))?;
    if metadata.len() > limit {
        return Err(CatalogError::blob(
            path,
            format!("file is {} bytes; limit is {limit}", metadata.len()),
        ));
    }
    let capacity = usize::try_from(metadata.len())
        .map_err(|_| CatalogError::blob(path, "file length does not fit usize"))?;
    let mut bytes = Vec::with_capacity(capacity);
    File::open(path)
        .map_err(|source| CatalogError::io(path, source))?
        .take(limit + 1)
        .read_to_end(&mut bytes)
        .map_err(|source| CatalogError::io(path, source))?;
    if bytes.len() as u64 > limit {
        return Err(CatalogError::blob(
            path,
            format!("file exceeded the {limit}-byte read limit"),
        ));
    }
    Ok(bytes)
}

fn write_new_file(path: &Path, bytes: &[u8]) -> CatalogResult<()> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|source| CatalogError::io(path, source))?;
    file.write_all(bytes)
        .and_then(|()| file.sync_all())
        .map_err(|source| CatalogError::io(path, source))
}

fn fnv1a64_hex(bytes: &[u8]) -> String {
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for &byte in bytes {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{hash:016x}")
}

fn issue_from_error(relative_path: PathBuf, error: CatalogError) -> CatalogIssue {
    let kind = match &error {
        CatalogError::Io { .. } => CatalogIssueKind::MissingBlob,
        CatalogError::UnsafePath { .. } => CatalogIssueKind::UnsafePath,
        CatalogError::InvalidBlob { .. } => CatalogIssueKind::MissingBlob,
        _ => CatalogIssueKind::InvalidManifest,
    };
    CatalogIssue {
        kind,
        relative_path,
        message: error.to_string(),
    }
}
