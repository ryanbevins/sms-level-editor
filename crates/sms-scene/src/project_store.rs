use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use super::{
    validate_project_relative_path, EditorProjectManifest, ProjectSaveOutcome, ProjectSaveWarning,
    Result, SceneError, StageDocument,
};

pub(super) const PROJECT_KIND: &str = "sms-editor-project";
pub(super) const PROJECT_FORMAT_VERSION: u32 = 2;
const MAX_PROJECT_MANIFEST_BYTES: u64 = 1024 * 1024;
static PROJECT_TRANSACTION_COUNTER: AtomicU64 = AtomicU64::new(0);

pub(super) fn save_project_folder(
    document: &StageDocument,
    project_root: &Path,
) -> Result<(ProjectSaveOutcome, u128)> {
    document.validate_for_export()?;
    if project_root
        .components()
        .any(|component| matches!(component, Component::ParentDir))
    {
        return Err(SceneError::InvalidProjectRoot(project_root.to_path_buf()));
    }
    let project_comparison = normalized_absolute_for_comparison(project_root)?;
    let base_comparison = normalized_absolute_for_comparison(&document.base_root)?;
    let manifest_base_path = fs::canonicalize(&document.base_root)?;
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

    let existing_manifest = inspect_existing_project(project_root)?;
    if let Some(existing_manifest) = &existing_manifest {
        let existing_base = normalized_absolute_for_comparison(&existing_manifest.base_path)?;
        if existing_base != base_comparison {
            return Err(SceneError::UnsupportedProjectManifest {
                path: project_root.join("sms-project.toml"),
                reason: format!(
                    "manifest base path '{}' does not match the open base root '{}'",
                    existing_manifest.base_path.display(),
                    document.base_root.display()
                ),
            });
        }
    }
    let original_snapshot = snapshot_project_root(project_root)?;
    if let Some(existing_manifest) = &existing_manifest {
        reject_unmanaged_output_collisions(
            project_root,
            &existing_manifest.changed_files,
            document.changed_files.keys(),
        )?;
        validate_loaded_project(
            document,
            project_root,
            existing_manifest,
            &original_snapshot,
        )?;
    }
    if inspect_existing_project(project_root)? != existing_manifest {
        return Err(SceneError::ProjectChangedDuringSave(
            project_root.to_path_buf(),
        ));
    }
    let project_id = existing_manifest
        .as_ref()
        .map(|manifest| manifest.project_id.as_str())
        .filter(|id| !id.is_empty())
        .map(str::to_owned)
        .unwrap_or_else(new_project_id);
    let (staging_root, backup_root) = unique_transaction_paths(parent, &name)?;
    fs::create_dir(&staging_root)?;
    let staged_manifest = (|| -> Result<(EditorProjectManifest, u128)> {
        if existing_manifest.is_some() {
            copy_unmanaged_project_entries(
                project_root,
                &staging_root,
                document.changed_files.keys(),
            )?;
        }

        let files_root = staging_root.join("files");
        fs::create_dir_all(&files_root)?;

        let mut changed_files = existing_manifest
            .as_ref()
            .map(|manifest| manifest.changed_files.clone())
            .unwrap_or_default();
        for (relative_path, bytes) in &document.changed_files {
            validate_project_relative_path(relative_path)?;
            let out_path = files_root.join(relative_path);
            if let Some(parent) = out_path.parent() {
                fs::create_dir_all(parent)?;
            }
            write_file_synced(&out_path, bytes)?;
            changed_files.push(relative_path.clone());
        }

        changed_files = dedup_project_paths(changed_files);
        let mut manifest =
            EditorProjectManifest::new(manifest_base_path, PathBuf::from("files"), project_id);
        manifest.revision = existing_manifest.as_ref().map_or(Ok(1), |existing| {
            existing
                .revision
                .checked_add(1)
                .ok_or_else(|| SceneError::UnsupportedProjectManifest {
                    path: project_root.join("sms-project.toml"),
                    reason: "project revision overflowed".to_string(),
                })
        })?;
        manifest.changed_files = changed_files;

        let manifest_text = toml::to_string_pretty(&manifest)?;
        write_file_synced(
            &staging_root.join("sms-project.toml"),
            manifest_text.as_bytes(),
        )?;
        sync_directory(&staging_root)?;
        let project_fingerprint =
            fingerprint_managed_project_snapshot(&snapshot_project_root(&staging_root)?, &manifest);
        Ok((manifest, project_fingerprint))
    })();
    let (manifest, project_fingerprint) = match staged_manifest {
        Ok(staged) => staged,
        Err(err) => {
            if let Err(cleanup_error) = fs::remove_dir_all(&staging_root) {
                return Err(SceneError::ProjectTransactionFailed {
                    project_root: project_root.to_path_buf(),
                    message: format!(
                        "staging failed: {err}; staging cleanup failed: {cleanup_error}"
                    ),
                    recovery_path: staging_root,
                });
            }
            return Err(err);
        }
    };

    match snapshot_project_root(project_root) {
        Ok(snapshot) if snapshot == original_snapshot => {}
        Ok(_) => {
            return Err(remove_staging_after_concurrent_change(
                project_root,
                &staging_root,
            ));
        }
        Err(error) => {
            return Err(remove_staging_after_precommit_error(
                project_root,
                &staging_root,
                error,
            ));
        }
    }
    let had_existing_root = original_snapshot.exists;
    if had_existing_root {
        if let Err(err) = fs::rename(project_root, &backup_root) {
            let cleanup_error = fs::remove_dir_all(&staging_root).err();
            let mut message = format!("could not move the existing project aside: {err}");
            if let Some(cleanup_error) = cleanup_error {
                message.push_str(&format!("; staging cleanup failed: {cleanup_error}"));
            }
            return Err(SceneError::ProjectTransactionFailed {
                project_root: project_root.to_path_buf(),
                message,
                recovery_path: if staging_root.exists() {
                    staging_root
                } else {
                    project_root.to_path_buf()
                },
            });
        }
        let backup_snapshot_error = match snapshot_project_root(&backup_root) {
            Ok(snapshot) if snapshot == original_snapshot => None,
            Ok(_) => Some(SceneError::ProjectChangedDuringSave(
                project_root.to_path_buf(),
            )),
            Err(error) => Some(error),
        };
        if let Some(identity_error) = backup_snapshot_error {
            let rollback_error = fs::rename(&backup_root, project_root).err();
            let cleanup_error = fs::remove_dir_all(&staging_root).err();
            if rollback_error.is_none() && cleanup_error.is_none() {
                return Err(identity_error);
            }
            return Err(SceneError::ProjectTransactionFailed {
                project_root: project_root.to_path_buf(),
                message: format!(
                    "project identity could not be verified during swap ({identity_error}); rollback error: {}; staging cleanup error: {}",
                    rollback_error
                        .map(|error| error.to_string())
                        .unwrap_or_else(|| "none".to_string()),
                    cleanup_error
                        .map(|error| error.to_string())
                        .unwrap_or_else(|| "none".to_string())
                ),
                recovery_path: if backup_root.exists() {
                    backup_root.clone()
                } else {
                    staging_root.clone()
                },
            });
        }
    }
    if let Err(err) = fs::rename(&staging_root, project_root) {
        let rollback_error = if had_existing_root && backup_root.exists() {
            fs::rename(&backup_root, project_root).err()
        } else {
            None
        };
        let recovery_path = if backup_root.exists() {
            backup_root.clone()
        } else {
            staging_root.clone()
        };
        let mut message = format!("install failed: {err}");
        if let Some(rollback_error) = rollback_error {
            message.push_str(&format!("; rollback failed: {rollback_error}"));
        }
        return Err(SceneError::ProjectTransactionFailed {
            project_root: project_root.to_path_buf(),
            message,
            recovery_path,
        });
    }
    let mut warnings = Vec::new();
    if let Err(error) = sync_directory(parent) {
        warnings.push(ProjectSaveWarning {
            recovery_path: project_root.to_path_buf(),
            message: format!(
                "project installed, but its parent directory could not be synced: {error}"
            ),
        });
    }
    if backup_root.exists() {
        match snapshot_project_root(&backup_root) {
            Ok(snapshot) if snapshot == original_snapshot => {
                if let Err(error) = fs::remove_dir_all(&backup_root) {
                    warnings.push(ProjectSaveWarning {
                        recovery_path: backup_root.clone(),
                        message: format!(
                            "project installed, but the old backup could not be removed: {error}"
                        ),
                    });
                } else if let Err(error) = sync_directory(parent) {
                    warnings.push(ProjectSaveWarning {
                        recovery_path: project_root.to_path_buf(),
                        message: format!(
                            "project installed and its backup was removed, but the parent directory could not be synced: {error}"
                        ),
                    });
                }
            }
            Ok(_) => warnings.push(ProjectSaveWarning {
                recovery_path: backup_root.clone(),
                message: "project installed, but the old backup changed during cleanup and was preserved"
                    .to_string(),
            }),
            Err(error) => warnings.push(ProjectSaveWarning {
                recovery_path: backup_root.clone(),
                message: format!(
                    "project installed, but the old backup could not be verified and was preserved: {error}"
                ),
            }),
        }
    }
    Ok((
        ProjectSaveOutcome { manifest, warnings },
        project_fingerprint,
    ))
}

pub(super) fn load_project_overlay(
    document: &mut StageDocument,
    project_root: &Path,
) -> Result<bool> {
    let initial_snapshot = snapshot_project_root(project_root)?;
    let Some(manifest) = inspect_existing_project(project_root)? else {
        return Ok(false);
    };
    let manifest_base = normalized_absolute_for_comparison(&manifest.base_path)?;
    let document_base = normalized_absolute_for_comparison(&document.base_root)?;
    if manifest_base != document_base {
        return Err(SceneError::UnsupportedProjectManifest {
            path: project_root.join("sms-project.toml"),
            reason: format!(
                "manifest base path '{}' does not match the open base root '{}'",
                manifest.base_path.display(),
                document.base_root.display()
            ),
        });
    }

    let relative_overlay = document.editor_overlay_path()?;
    let overlay_is_managed = manifest
        .changed_files
        .iter()
        .any(|path| project_relative_key(path) == project_relative_key(&relative_overlay));
    if !overlay_is_managed {
        let canonical_project_root = fs::canonicalize(project_root)?;
        let project_fingerprint =
            fingerprint_managed_project_snapshot(&initial_snapshot, &manifest);
        ensure_project_unchanged_during_load(project_root, &initial_snapshot)?;
        document.loaded_project = Some(super::LoadedProjectState {
            project_root: canonical_project_root,
            project_id: manifest.project_id,
            revision: manifest.revision,
            project_fingerprint,
        });
        return Ok(false);
    }

    let overlay_path = project_root.join("files").join(&relative_overlay);
    let metadata = fs::symlink_metadata(&overlay_path).map_err(|error| {
        SceneError::UnsupportedProjectManifest {
            path: project_root.join("sms-project.toml"),
            reason: format!(
                "managed overlay '{}' could not be read: {error}",
                relative_overlay.display()
            ),
        }
    })?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err(SceneError::UnsupportedProjectEntry(overlay_path));
    }
    let mut overlay: super::EditorSceneOverlay = serde_json::from_slice(&read_file_bounded(
        &overlay_path,
        MAX_PROJECT_OVERLAY_BYTES,
    )?)?;
    if overlay.stage_id != document.stage_id {
        return Err(SceneError::ProjectOverlayStageMismatch {
            overlay_stage: overlay.stage_id,
            stage: document.stage_id.clone(),
        });
    }
    reattach_overlay_source_records(&document.objects, &mut overlay.objects)?;
    let canonical_project_root = fs::canonicalize(project_root)?;
    ensure_project_unchanged_during_load(project_root, &initial_snapshot)?;
    let project_fingerprint = fingerprint_managed_project_snapshot(&initial_snapshot, &manifest);
    let loaded_project = super::LoadedProjectState {
        project_root: canonical_project_root,
        project_id: manifest.project_id,
        revision: manifest.revision,
        project_fingerprint,
    };
    document.objects = overlay.objects;
    document.loaded_project = Some(loaded_project);
    Ok(true)
}

fn reattach_overlay_source_records(
    base_objects: &[super::SceneObject],
    overlay_objects: &mut [super::SceneObject],
) -> Result<()> {
    let mut base_records = BTreeMap::new();
    for object in base_objects {
        let Some(source) = object.source.as_ref() else {
            continue;
        };
        base_records.insert(source_record_key(source)?, object);
    }

    for object in overlay_objects {
        let Some(source) = object.source.as_ref() else {
            continue;
        };
        let key = source_record_key(source)?;
        let Some(base_object) = base_records.get(&key) else {
            return Err(SceneError::ProjectOverlaySourceMismatch {
                object_id: object.id.clone(),
                source_path: source.path.clone(),
                offset: source.offset,
            });
        };
        let Some(base_source) = base_object.source.as_ref() else {
            unreachable!("base source records are indexed only when they have a source");
        };
        let Some(source_record_bytes) = base_object.source_record_bytes.as_deref() else {
            return Err(SceneError::ProjectOverlaySourceMismatch {
                object_id: object.id.clone(),
                source_path: source.path.clone(),
                offset: source.offset,
            });
        };
        object.source = Some(base_source.clone());
        object.source_record_bytes = Some(source_record_bytes.to_vec());

        // Overlay values win, but newly understood source fields must become
        // available when an older project is reopened against a newer parser.
        for (name, value) in &base_object.raw_params {
            object
                .raw_params
                .entry(name.clone())
                .or_insert_with(|| value.clone());
        }

        // Inferred previews are source-derived cache data. Replace stale
        // overlay copies with the fresh base-stage result while preserving
        // every explicitly authored or non-derived asset reference.
        object
            .asset_hints
            .retain(|hint| hint.role != super::AssetRole::InferredPreviewModel);
        for hint in base_object
            .asset_hints
            .iter()
            .filter(|hint| hint.role == super::AssetRole::InferredPreviewModel)
        {
            if !object.asset_hints.contains(hint) {
                object.asset_hints.push(hint.clone());
            }
        }
    }
    Ok(())
}

fn source_record_key(
    source: &sms_formats::SourceLocation,
) -> Result<(String, Option<u64>, Option<u64>)> {
    let path = source.path.to_string_lossy().replace('\\', "/");
    let normalized_path = if let Some((archive_path, internal_path)) = path.split_once("!/") {
        format!(
            "{}!/{internal_path}",
            normalized_absolute_for_comparison(Path::new(archive_path))?
        )
    } else {
        normalized_absolute_for_comparison(&source.path)?
    };
    Ok((normalized_path, source.offset, source.length))
}

fn validate_loaded_project(
    document: &StageDocument,
    project_root: &Path,
    manifest: &EditorProjectManifest,
    current_snapshot: &ProjectSnapshot,
) -> Result<()> {
    let Some(loaded) = &document.loaded_project else {
        return Err(SceneError::ProjectNotLoaded(project_root.to_path_buf()));
    };
    if normalized_absolute_for_comparison(&loaded.project_root)?
        != normalized_absolute_for_comparison(project_root)?
        || loaded.project_id != manifest.project_id
        || loaded.revision != manifest.revision
        || loaded.project_fingerprint
            != fingerprint_managed_project_snapshot(current_snapshot, manifest)
    {
        return Err(SceneError::StaleProject(project_root.to_path_buf()));
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ProjectSnapshot {
    exists: bool,
    entries: BTreeMap<PathBuf, ProjectEntryFingerprint>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ProjectEntryFingerprint {
    Directory,
    File { length: u64, hash: u128 },
}

const MAX_PROJECT_OVERLAY_BYTES: u64 = 64 * 1024 * 1024;

fn snapshot_project_root(project_root: &Path) -> Result<ProjectSnapshot> {
    let root_metadata = match fs::symlink_metadata(project_root) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(ProjectSnapshot {
                exists: false,
                entries: BTreeMap::new(),
            });
        }
        Err(error) => return Err(error.into()),
    };
    if !root_metadata.is_dir() || root_metadata.file_type().is_symlink() {
        return Err(SceneError::UnownedProjectRoot(project_root.to_path_buf()));
    }

    let mut entries = BTreeMap::new();
    let mut pending = vec![(project_root.to_path_buf(), PathBuf::new())];
    while let Some((directory, relative_directory)) = pending.pop() {
        for entry in fs::read_dir(directory)? {
            let entry = entry?;
            let relative_path = relative_directory.join(entry.file_name());
            let metadata = fs::symlink_metadata(entry.path())?;
            if metadata.file_type().is_symlink() {
                return Err(SceneError::UnsupportedProjectEntry(entry.path()));
            }
            if metadata.is_dir() {
                entries.insert(relative_path.clone(), ProjectEntryFingerprint::Directory);
                pending.push((entry.path(), relative_path));
            } else if metadata.is_file() {
                entries.insert(
                    relative_path,
                    ProjectEntryFingerprint::File {
                        length: metadata.len(),
                        hash: hash_file(&entry.path())?,
                    },
                );
            } else {
                return Err(SceneError::UnsupportedProjectEntry(entry.path()));
            }
        }
    }
    Ok(ProjectSnapshot {
        exists: true,
        entries,
    })
}

fn hash_file(path: &Path) -> Result<u128> {
    let mut file = fs::File::open(path)?;
    let mut hash = 0x6c62_272e_07bb_0142_62b8_2175_6295_c58du128;
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        for byte in &buffer[..read] {
            hash ^= u128::from(*byte);
            hash = hash.wrapping_mul(0x0000_0000_0100_0000_0000_0000_0000_013b);
        }
    }
    Ok(hash)
}

fn fingerprint_managed_project_snapshot(
    snapshot: &ProjectSnapshot,
    manifest: &EditorProjectManifest,
) -> u128 {
    let mut hash = 0x6c62_272e_07bb_0142_62b8_2175_6295_c58du128;
    let mut update = |bytes: &[u8]| {
        for byte in bytes {
            hash ^= u128::from(*byte);
            hash = hash.wrapping_mul(0x0000_0000_0100_0000_0000_0000_0000_013b);
        }
    };
    let managed_paths = std::iter::once(PathBuf::from("sms-project.toml"))
        .chain(
            manifest
                .changed_files
                .iter()
                .map(|path| PathBuf::from("files").join(path)),
        )
        .collect::<Vec<_>>();
    let entries_by_key = snapshot
        .entries
        .iter()
        .map(|(path, entry)| (project_relative_key(path), entry))
        .collect::<BTreeMap<_, _>>();
    for path in managed_paths {
        let path_key = project_relative_key(&path);
        update(path_key.as_bytes());
        update(&[0]);
        match entries_by_key.get(&path_key).copied() {
            Some(ProjectEntryFingerprint::Directory) => update(&[0]),
            Some(ProjectEntryFingerprint::File { length, hash }) => {
                update(&[1]);
                update(&length.to_le_bytes());
                update(&hash.to_le_bytes());
            }
            None => update(&[0xff]),
        }
    }
    hash
}

fn ensure_project_unchanged_during_load(
    project_root: &Path,
    initial_snapshot: &ProjectSnapshot,
) -> Result<()> {
    if snapshot_project_root(project_root)? == *initial_snapshot {
        Ok(())
    } else {
        Err(SceneError::ProjectChangedDuringLoad(
            project_root.to_path_buf(),
        ))
    }
}

fn remove_staging_after_concurrent_change(project_root: &Path, staging_root: &Path) -> SceneError {
    remove_staging_after_precommit_error(
        project_root,
        staging_root,
        SceneError::ProjectChangedDuringSave(project_root.to_path_buf()),
    )
}

fn remove_staging_after_precommit_error(
    project_root: &Path,
    staging_root: &Path,
    error: SceneError,
) -> SceneError {
    match fs::remove_dir_all(staging_root) {
        Ok(()) => error,
        Err(cleanup_error) => SceneError::ProjectTransactionFailed {
            project_root: project_root.to_path_buf(),
            message: format!(
                "pre-commit save failed ({error}) and staging cleanup failed: {cleanup_error}"
            ),
            recovery_path: staging_root.to_path_buf(),
        },
    }
}

fn inspect_existing_project(project_root: &Path) -> Result<Option<EditorProjectManifest>> {
    if !project_root.exists() {
        return Ok(None);
    }

    let metadata = fs::symlink_metadata(project_root)?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err(SceneError::UnownedProjectRoot(project_root.to_path_buf()));
    }

    let mut entries = fs::read_dir(project_root)?;
    if entries.next().transpose()?.is_none() {
        return Ok(None);
    }

    let manifest_path = project_root.join("sms-project.toml");
    let manifest_metadata = fs::symlink_metadata(&manifest_path).map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            SceneError::UnownedProjectRoot(project_root.to_path_buf())
        } else {
            error.into()
        }
    })?;
    if !manifest_metadata.is_file() || manifest_metadata.file_type().is_symlink() {
        return Err(SceneError::UnownedProjectRoot(project_root.to_path_buf()));
    }
    let manifest_bytes = read_file_bounded(&manifest_path, MAX_PROJECT_MANIFEST_BYTES)?;
    let manifest_text = std::str::from_utf8(&manifest_bytes).map_err(|error| {
        SceneError::UnsupportedProjectManifest {
            path: manifest_path.clone(),
            reason: format!("manifest is not valid UTF-8: {error}"),
        }
    })?;
    let manifest: EditorProjectManifest = toml::from_str(manifest_text)?;
    if manifest.kind != PROJECT_KIND {
        return Err(SceneError::UnsupportedProjectManifest {
            path: manifest_path,
            reason: format!("unexpected project kind '{}'", manifest.kind),
        });
    }
    if !(1..=PROJECT_FORMAT_VERSION).contains(&manifest.format_version) {
        return Err(SceneError::UnsupportedProjectManifest {
            path: manifest_path,
            reason: format!(
                "format version {} is not supported (maximum {})",
                manifest.format_version, PROJECT_FORMAT_VERSION
            ),
        });
    }
    let valid_files_path = if manifest.format_version >= 2 {
        manifest.project_files_path == Path::new("files")
    } else {
        manifest.project_files_path.file_name() == Some(std::ffi::OsStr::new("files"))
    };
    if !valid_files_path {
        return Err(SceneError::UnsupportedProjectManifest {
            path: manifest_path,
            reason: "project_files_path must end in the managed 'files' directory".to_string(),
        });
    }
    let files_root = project_root.join("files");
    let files_metadata = fs::symlink_metadata(&files_root).map_err(|error| {
        SceneError::UnsupportedProjectManifest {
            path: manifest_path.clone(),
            reason: format!("managed files directory could not be inspected: {error}"),
        }
    })?;
    if !files_metadata.is_dir() || files_metadata.file_type().is_symlink() {
        return Err(SceneError::UnsupportedProjectManifest {
            path: manifest_path.clone(),
            reason: "managed files path is not a regular directory".to_string(),
        });
    }
    let mut changed_file_keys = BTreeSet::new();
    for changed_file in &manifest.changed_files {
        validate_project_relative_path(changed_file).map_err(|error| {
            SceneError::UnsupportedProjectManifest {
                path: manifest_path.clone(),
                reason: format!("invalid changed file '{}': {error}", changed_file.display()),
            }
        })?;
        if !changed_file_keys.insert(project_relative_key(changed_file)) {
            return Err(SceneError::UnsupportedProjectManifest {
                path: manifest_path.clone(),
                reason: format!(
                    "changed file '{}' is duplicated under this platform's path rules",
                    changed_file.display()
                ),
            });
        }
        validate_managed_file_entry(&files_root, changed_file).map_err(|reason| {
            SceneError::UnsupportedProjectManifest {
                path: manifest_path.clone(),
                reason,
            }
        })?;
    }
    Ok(Some(manifest))
}

fn validate_managed_file_entry(
    files_root: &Path,
    relative_path: &Path,
) -> std::result::Result<(), String> {
    let mut current = files_root.to_path_buf();
    let components = relative_path.components().collect::<Vec<_>>();
    for (index, component) in components.iter().enumerate() {
        let Component::Normal(name) = component else {
            return Err(format!(
                "managed file '{}' contains an invalid component",
                relative_path.display()
            ));
        };
        current.push(name);
        let metadata = fs::symlink_metadata(&current).map_err(|error| {
            format!(
                "managed file '{}' could not be inspected: {error}",
                relative_path.display()
            )
        })?;
        if metadata.file_type().is_symlink() {
            return Err(format!(
                "managed file '{}' traverses a symbolic link at '{}'",
                relative_path.display(),
                current.display()
            ));
        }
        let is_last = index + 1 == components.len();
        if (is_last && !metadata.is_file()) || (!is_last && !metadata.is_dir()) {
            return Err(format!(
                "managed file '{}' does not resolve to a regular file inside the project",
                relative_path.display()
            ));
        }
    }
    Ok(())
}

fn write_file_synced(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut file = fs::File::create(path)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    if let Some(parent) = path.parent() {
        sync_directory(parent)?;
    }
    Ok(())
}

fn read_file_bounded(path: &Path, limit: u64) -> Result<Vec<u8>> {
    let file = fs::File::open(path)?;
    let mut bytes = Vec::new();
    file.take(limit.saturating_add(1)).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > limit {
        return Err(SceneError::ProjectFileTooLarge {
            path: path.to_path_buf(),
            limit,
        });
    }
    Ok(bytes)
}

#[cfg(unix)]
fn sync_directory(path: &Path) -> Result<()> {
    fs::File::open(path)?.sync_all()?;
    Ok(())
}

#[cfg(not(unix))]
fn sync_directory(_path: &Path) -> Result<()> {
    // Rust's standard library cannot portably open a Windows directory handle
    // for flushing. Every staged file is still flushed before the atomic swap.
    Ok(())
}

fn new_project_id() -> String {
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let sequence = PROJECT_TRANSACTION_COUNTER.fetch_add(1, Ordering::Relaxed);
    format!(
        "{:032x}-{:08x}-{sequence:016x}",
        timestamp,
        std::process::id()
    )
}

fn unique_transaction_paths(parent: &Path, name: &str) -> Result<(PathBuf, PathBuf)> {
    for _ in 0..128 {
        let transaction_id = new_project_id();
        let staging_root = parent.join(format!(".{name}.staging-{transaction_id}"));
        let backup_root = parent.join(format!(".{name}.backup-{transaction_id}"));
        if !staging_root.exists() && !backup_root.exists() {
            return Ok((staging_root, backup_root));
        }
    }
    Err(SceneError::InvalidProjectRoot(parent.join(name)))
}

fn copy_unmanaged_project_entries<'a>(
    source_root: &Path,
    target_root: &Path,
    replaced_files: impl Iterator<Item = &'a PathBuf>,
) -> Result<()> {
    for entry in fs::read_dir(source_root)? {
        let entry = entry?;
        let name = entry.file_name();
        if name == "files" || name == "sms-project.toml" {
            continue;
        }
        copy_project_entry(&entry.path(), &target_root.join(name))?;
    }

    let source_files = source_root.join("files");
    if source_files.exists() {
        let replaced_files = replaced_files
            .map(|path| project_relative_key(path))
            .collect::<BTreeSet<_>>();
        copy_project_files_excluding(&source_files, &target_root.join("files"), &replaced_files)?;
    }
    Ok(())
}

fn reject_unmanaged_output_collisions<'a>(
    project_root: &Path,
    previously_managed: &[PathBuf],
    current_outputs: impl Iterator<Item = &'a PathBuf>,
) -> Result<()> {
    let previously_managed = previously_managed
        .iter()
        .map(|path| project_relative_key(path))
        .collect::<BTreeSet<_>>();
    for relative_path in current_outputs {
        if previously_managed.contains(&project_relative_key(relative_path)) {
            continue;
        }
        let output_path = project_root.join("files").join(relative_path);
        match fs::symlink_metadata(&output_path) {
            Ok(_) => return Err(SceneError::UnmanagedProjectFileConflict(output_path)),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

fn project_relative_key(path: &Path) -> String {
    let key = path.to_string_lossy().replace('\\', "/");
    if cfg!(windows) {
        key.to_ascii_lowercase()
    } else {
        key
    }
}

fn dedup_project_paths(paths: Vec<PathBuf>) -> Vec<PathBuf> {
    paths
        .into_iter()
        .map(|path| (project_relative_key(&path), path))
        .collect::<BTreeMap<_, _>>()
        .into_values()
        .collect()
}

fn copy_project_entry(source: &Path, target: &Path) -> Result<()> {
    let mut pending = vec![(source.to_path_buf(), target.to_path_buf())];
    while let Some((source, target)) = pending.pop() {
        let metadata = fs::symlink_metadata(&source)?;
        if metadata.file_type().is_symlink() {
            return Err(SceneError::UnsupportedProjectEntry(source));
        }
        if metadata.is_file() {
            copy_file_synced(&source, &target)?;
        } else if metadata.is_dir() {
            fs::create_dir(&target)?;
            for entry in fs::read_dir(source)? {
                let entry = entry?;
                pending.push((entry.path(), target.join(entry.file_name())));
            }
        } else {
            return Err(SceneError::UnsupportedProjectEntry(source));
        }
    }
    Ok(())
}

fn copy_project_files_excluding(
    source_root: &Path,
    target_root: &Path,
    excluded_files: &BTreeSet<String>,
) -> Result<()> {
    let mut pending = vec![(
        source_root.to_path_buf(),
        target_root.to_path_buf(),
        PathBuf::new(),
    )];
    while let Some((source, target, relative)) = pending.pop() {
        let metadata = fs::symlink_metadata(&source)?;
        if metadata.file_type().is_symlink() {
            return Err(SceneError::UnsupportedProjectEntry(source));
        }
        if metadata.is_file() {
            if !excluded_files.contains(&project_relative_key(&relative)) {
                if let Some(parent) = target.parent() {
                    fs::create_dir_all(parent)?;
                }
                copy_file_synced(&source, &target)?;
            }
        } else if metadata.is_dir() {
            fs::create_dir_all(&target)?;
            for entry in fs::read_dir(source)? {
                let entry = entry?;
                let name = entry.file_name();
                pending.push((entry.path(), target.join(&name), relative.join(name)));
            }
        } else {
            return Err(SceneError::UnsupportedProjectEntry(source));
        }
    }
    Ok(())
}

fn copy_file_synced(source: &Path, target: &Path) -> Result<()> {
    fs::copy(source, target)?;
    fs::OpenOptions::new()
        .write(true)
        .open(target)?
        .sync_all()?;
    if let Some(parent) = target.parent() {
        sync_directory(parent)?;
    }
    Ok(())
}

fn normalized_absolute_for_comparison(path: &Path) -> Result<String> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()?.join(path)
    };
    let canonical = canonicalize_with_missing_tail(&absolute);
    #[cfg(windows)]
    {
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

    #[cfg(not(windows))]
    {
        Ok(canonical
            .to_string_lossy()
            .trim_end_matches('/')
            .to_string())
    }
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
    let separator = if cfg!(windows) { '\\' } else { '/' };
    path == parent
        || path
            .strip_prefix(parent)
            .is_some_and(|suffix| suffix.starts_with(separator))
}
