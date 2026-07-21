use std::collections::BTreeSet;
use std::ffi::OsString;
use std::fs::{self, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use serde::{Deserialize, Serialize};
use sms_formats::{
    parse_jdrama_document, parse_jdrama_scenario_archive_entries, JDramaScenarioArchiveEntry,
};
use sms_scene::StageDocument;

use crate::direct_boot::{
    patch_sms_direct_boot_dol, patch_sms_sound_assignments_dol, patch_sms_stage_music_dol,
    RuntimeSoundAssignment, RuntimeSoundAssignmentKind, RuntimeStageMusicOverride,
    RuntimeStageTarget,
};
#[cfg(test)]
use crate::project::ProjectSoundAssignment;
use crate::project::{
    normalized_absolute_with_missing_tail, path_is_same_or_child, OpenProject,
    ProjectSoundAssignmentKind,
};

const MANAGED_BUILD_MARKER_NAME: &str = ".smsbuild-owner.toml";
const MANAGED_BUILD_MARKER_KIND: &str = "sms-editor-managed-build";
const MANAGED_BUILD_MARKER_VERSION: u32 = 1;
#[cfg(test)]
const MOD_ROOT_NAME: &str = "mod-root";
const RUN_ROOT_NAME: &str = "run-root";
const MAX_MARKER_BYTES: u64 = 64 * 1024;
const MAX_RUNTIME_STAGE_TABLE_BYTES: u64 = 16 * 1024 * 1024;
const PROJECT_RUNTIME_STAGE_TABLE_PATH: &[&str] = &["files", "data", "stageArc.bin"];
static TEMP_FILE_SEQUENCE: AtomicU64 = AtomicU64::new(0);

pub(super) const MANAGED_BUILD_CANCELLED: &str = "Managed game build cancelled";

pub(super) fn check_cancelled(cancelled: &AtomicBool) -> Result<(), String> {
    if cancelled.load(Ordering::Acquire) {
        Err(format!(
            "{MANAGED_BUILD_CANCELLED}; partial files, if any, remain only in the editor-owned managed build directory and will be reconciled by the next build"
        ))
    } else {
        Ok(())
    }
}

fn check_cancelled_io(cancelled: &AtomicBool) -> io::Result<()> {
    check_cancelled(cancelled).map_err(|error| io::Error::new(io::ErrorKind::Interrupted, error))
}

pub(super) fn is_cancelled_error(error: &str) -> bool {
    error.starts_with(MANAGED_BUILD_CANCELLED)
}

#[cfg(test)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ManagedStageBuildOutcome {
    pub(super) build_root: PathBuf,
    pub(super) mod_root: PathBuf,
    pub(super) source_relative_path: PathBuf,
    pub(super) output_path: PathBuf,
    pub(super) marker_path: PathBuf,
    pub(super) size_bytes: usize,
    pub(super) replaced: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ManagedRunMirrorOutcome {
    pub(super) build_root: PathBuf,
    pub(super) run_root: PathBuf,
    pub(super) run_main_dol: PathBuf,
    pub(super) source_relative_path: PathBuf,
    pub(super) stage_output_path: PathBuf,
    pub(super) stage_size_bytes: usize,
    pub(super) stage_replaced: bool,
    pub(super) copied_files: usize,
    pub(super) reused_files: usize,
    pub(super) removed_entries: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ManagedGameBuildOutcome {
    pub(super) run: ManagedRunMirrorOutcome,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ManagedDirectBootOutcome {
    pub(super) launch_dol: PathBuf,
    pub(super) target: RuntimeStageTarget,
    pub(super) matching_contexts: usize,
    pub(super) size_bytes: usize,
    pub(super) reused: bool,
    pub(super) logo_bypass_address: u32,
    pub(super) hook_address: u32,
    pub(super) movie_hook_address: u32,
    pub(super) stub_address: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ManagedGameLaunchOutcome {
    pub(super) run: ManagedRunMirrorOutcome,
    pub(super) direct_boot: ManagedDirectBootOutcome,
}

#[derive(Debug)]
struct MirrorFile {
    source: PathBuf,
    relative: PathBuf,
}

#[derive(Debug, Default)]
struct MirrorInventory {
    directories: BTreeSet<PathBuf>,
    files: Vec<MirrorFile>,
    file_paths: BTreeSet<PathBuf>,
}

#[derive(Debug, Default)]
struct MirrorStats {
    copied_files: usize,
    reused_files: usize,
    removed_entries: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MirroredFileAction {
    Copied,
    Reused,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct ManagedBuildMarker {
    format_version: u32,
    kind: String,
    project_id: String,
    base_game_root: PathBuf,
    created_with: String,
}

/// Resolves and validates the project's dedicated build directory without
/// creating it. Existing symlinks and reparse points are rejected.
pub(super) fn resolve_managed_build_root(project: &OpenProject) -> Result<PathBuf, String> {
    project
        .descriptor
        .validate_locations(&project.descriptor_path)?;

    let configured_root = project.managed_build_root();
    if let Ok(metadata) = fs::symlink_metadata(&configured_root) {
        reject_link_or_reparse(&metadata, &configured_root, "managed build root")?;
    }
    let build_root = normalized_absolute_with_missing_tail(&configured_root)?;
    let base_root = normalized_absolute_with_missing_tail(&project.descriptor.base_game_root)?;
    let data_root = normalized_absolute_with_missing_tail(&project.data_root())?;
    reject_overlap(
        &build_root,
        &base_root,
        "managed build root",
        "extracted base game",
    )?;
    reject_overlap(
        &build_root,
        &data_root,
        "managed build root",
        "project data",
    )?;
    Ok(build_root)
}

/// Builds a complete runnable game directory. Every file in the runnable
/// directory is an independent copy, so no write through that tree can mutate
/// the extracted base game.
pub(super) fn build_managed_game(
    project: &OpenProject,
    document: &StageDocument,
    archive_bytes: &[u8],
    cancelled: &AtomicBool,
) -> Result<ManagedGameBuildOutcome, String> {
    check_cancelled(cancelled)?;
    let source_path = document
        .stage_archive_source_path
        .as_deref()
        .ok_or_else(|| {
            format!(
                "Stage '{}' has no semantic archive source identity",
                document.stage_id
            )
        })?;
    let run = prepare_managed_run_mirror_from_source_with_cancel(
        project,
        &document.base_root,
        source_path,
        archive_bytes,
        cancelled,
    )?;
    install_managed_stage_music(project, &run, cancelled)?;
    Ok(ManagedGameBuildOutcome { run })
}

fn install_managed_stage_music(
    project: &OpenProject,
    run: &ManagedRunMirrorOutcome,
    cancelled: &AtomicBool,
) -> Result<(), String> {
    if project.descriptor.stage_music.is_empty() && project.descriptor.sound_assignments.is_empty()
    {
        return Ok(());
    }
    check_cancelled(cancelled)?;
    let mut overrides = Vec::new();
    if !project.descriptor.stage_music.is_empty() {
        let stage_table = find_case_insensitive_path(
            &run.run_root,
            &["files", "data", "stageArc.bin"],
            "runtime stage archive table",
        )?;
        let entries =
            parse_jdrama_scenario_archive_entries(&fs::read(&stage_table).map_err(|error| {
                format!(
                    "Could not read runtime stage archive table '{}': {error}",
                    stage_table.display()
                )
            })?)
            .map_err(|error| {
                format!(
                    "Could not parse runtime stage archive table '{}': {error}",
                    stage_table.display()
                )
            })?;
        for (stage_id, music) in &project.descriptor.stage_music {
            let matching = entries
                .iter()
                .filter(|entry| {
                    runtime_archive_stem(&entry.archive_name)
                        .is_some_and(|stem| stem.eq_ignore_ascii_case(stage_id))
                })
                .collect::<Vec<_>>();
            if matching.is_empty() {
                return Err(format!(
                    "Stage music override '{}' is not mapped by the packaged stageArc.bin",
                    stage_id
                ));
            }
            for entry in matching {
                overrides.push(RuntimeStageMusicOverride {
                    area_index: u8::try_from(entry.area_index).map_err(|_| {
                        format!(
                            "Stage music area {} for '{}' does not fit Sunshine's u8 game sequence",
                            entry.area_index, stage_id
                        )
                    })?,
                    scenario_index: u8::try_from(entry.scenario_index).map_err(|_| {
                        format!(
                            "Stage music scenario {} for '{}' does not fit Sunshine's u8 game sequence",
                            entry.scenario_index, stage_id
                        )
                    })?,
                    bgm_id: music.bgm_id,
                    wave_scene_id: music.wave_scene_id,
                    secondary_bgm_id: music.secondary_bgm_id,
                    secondary_wave_scene_id: music.secondary_wave_scene_id,
                });
            }
        }
    }
    overrides.sort_by_key(|override_| (override_.area_index, override_.scenario_index));
    check_cancelled(cancelled)?;
    let source_dol = fs::read(&run.run_main_dol).map_err(|error| {
        format!(
            "Could not read managed game executable '{}': {error}",
            run.run_main_dol.display()
        )
    })?;
    let sound_assignments = project
        .descriptor
        .sound_assignments
        .values()
        .map(|assignment| RuntimeSoundAssignment {
            kind: match assignment.kind {
                ProjectSoundAssignmentKind::MapStatic => RuntimeSoundAssignmentKind::MapStatic,
                ProjectSoundAssignmentKind::Graph => RuntimeSoundAssignmentKind::Graph,
            },
            source_name: assignment.source_name.clone(),
            original_sound_id: assignment.original_sound_id,
            sound_id: assignment.sound_id,
        })
        .collect::<Vec<_>>();
    let mut patched_bytes = patch_sms_sound_assignments_dol(&source_dol, &sound_assignments)
        .map_err(|error| {
            format!(
                "Could not install packaged sound helper assignments into '{}': {error}",
                run.run_main_dol.display()
            )
        })?;
    if !overrides.is_empty() {
        patched_bytes = patch_sms_stage_music_dol(&patched_bytes, &overrides)
            .map_err(|error| {
                format!(
                    "Could not install packaged stage music into '{}': {error}",
                    run.run_main_dol.display()
                )
            })?
            .bytes;
    }
    check_cancelled(cancelled)?;
    atomic_write_if_changed_with_cancel(&run.run_main_dol, &patched_bytes, cancelled).map_err(
        |error| {
            if is_cancelled_error(&error.to_string()) {
                error.to_string()
            } else {
                format!(
                    "Could not install packaged stage music executable '{}': {error}",
                    run.run_main_dol.display()
                )
            }
        },
    )?;
    Ok(())
}

pub(super) fn prepare_managed_game_launch(
    build: ManagedGameBuildOutcome,
    cancelled: &AtomicBool,
) -> Result<ManagedGameLaunchOutcome, String> {
    check_cancelled(cancelled)?;
    let direct_boot = prepare_managed_direct_boot(&build.run, cancelled)?;
    Ok(ManagedGameLaunchOutcome {
        run: build.run,
        direct_boot,
    })
}

fn prepare_managed_direct_boot(
    run: &ManagedRunMirrorOutcome,
    cancelled: &AtomicBool,
) -> Result<ManagedDirectBootOutcome, String> {
    check_cancelled(cancelled)?;
    let stage_table = find_case_insensitive_path(
        &run.run_root,
        &["files", "data", "stageArc.bin"],
        "runtime stage archive table",
    )?;
    let stage_table_bytes = fs::read(&stage_table).map_err(|error| {
        format!(
            "Could not read runtime stage archive table '{}': {error}",
            stage_table.display()
        )
    })?;
    check_cancelled(cancelled)?;
    let entries = parse_jdrama_scenario_archive_entries(&stage_table_bytes).map_err(|error| {
        format!(
            "Could not parse runtime stage archive table '{}': {error}",
            stage_table.display()
        )
    })?;
    let (target, matching_contexts) =
        resolve_runtime_stage_target(&entries, &run.source_relative_path)?;

    let source_dol = fs::read(&run.run_main_dol).map_err(|error| {
        format!(
            "Could not read managed game executable '{}': {error}",
            run.run_main_dol.display()
        )
    })?;
    check_cancelled(cancelled)?;
    let patched = patch_sms_direct_boot_dol(&source_dol, &target).map_err(|error| {
        format!(
            "Could not prepare version-independent direct boot from '{}': {error}",
            run.run_main_dol.display()
        )
    })?;
    check_cancelled(cancelled)?;

    // Dolphin recognizes an extracted directory as a mounted game only when
    // the executable is named exactly `sys/main.dol`. This path is an
    // independent managed copy: the build immediately before every launch
    // refreshes it from the configured base executable, then this atomic
    // replacement installs the target-specific launch image. The extracted
    // base executable is never opened for modification.
    let launch_dol = run.run_main_dol.clone();
    let reused = atomic_write_if_changed_with_cancel(&launch_dol, &patched.bytes, cancelled)
        .map_err(|error| {
            if is_cancelled_error(&error.to_string()) {
                error.to_string()
            } else {
                format!(
                    "Could not install managed direct-boot executable '{}': {error}",
                    launch_dol.display()
                )
            }
        })?;
    check_cancelled(cancelled)?;

    Ok(ManagedDirectBootOutcome {
        launch_dol,
        target,
        matching_contexts,
        size_bytes: patched.bytes.len(),
        reused,
        logo_bypass_address: patched.logo_bypass_address,
        hook_address: patched.hook_address,
        movie_hook_address: patched.movie_hook_address,
        stub_address: patched.stub_address,
    })
}

fn resolve_runtime_stage_target(
    entries: &[JDramaScenarioArchiveEntry],
    source_relative_path: &Path,
) -> Result<(RuntimeStageTarget, usize), String> {
    let source_stem = source_relative_path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .ok_or_else(|| {
            format!(
                "Stage source has no Unicode archive stem: {}",
                source_relative_path.display()
            )
        })?;
    let matching = entries
        .iter()
        .filter(|entry| {
            runtime_archive_stem(&entry.archive_name)
                .is_some_and(|stem| stem.eq_ignore_ascii_case(source_stem))
        })
        .collect::<Vec<_>>();
    let first = matching.first().ok_or_else(|| {
        format!(
            "Stage archive '{}' is not mapped by the staged game's stageArc.bin",
            source_relative_path.display()
        )
    })?;
    let area_index = u8::try_from(first.area_index).map_err(|_| {
        format!(
            "Runtime area index {} for '{}' does not fit Sunshine's u8 game sequence",
            first.area_index, first.archive_name
        )
    })?;
    let scenario_index = u8::try_from(first.scenario_index).map_err(|_| {
        format!(
            "Runtime scenario index {} for '{}' does not fit Sunshine's u8 game sequence",
            first.scenario_index, first.archive_name
        )
    })?;
    Ok((
        RuntimeStageTarget {
            area_index,
            scenario_index,
            archive_name: first.archive_name.clone(),
        },
        matching.len(),
    ))
}

/// Reads the project-owned runtime table when present, otherwise the selected
/// release's retail table. The project copy is the durable source of authored
/// runtime slots; neither path is ever opened for modification here.
pub(super) fn read_effective_runtime_stage_table(project: &OpenProject) -> Result<Vec<u8>, String> {
    project
        .descriptor
        .validate_locations(&project.descriptor_path)?;
    let project_table = PROJECT_RUNTIME_STAGE_TABLE_PATH
        .iter()
        .fold(project.data_root(), |path, component| path.join(component));
    match read_runtime_stage_table_file(
        &project_table,
        "project runtime stage archive table",
        true,
    )? {
        Some(bytes) => Ok(bytes),
        None => {
            let base_table = find_case_insensitive_path(
                &project.descriptor.base_game_root,
                &["files", "data", "stageArc.bin"],
                "runtime stage archive table",
            )?;
            read_runtime_stage_table_file(&base_table, "runtime stage archive table", false)?
                .ok_or_else(|| {
                    format!(
                        "Runtime stage archive table disappeared while reading '{}'",
                        base_table.display()
                    )
                })
        }
    }
}

fn read_runtime_stage_table_file(
    path: &Path,
    description: &str,
    optional: bool,
) -> Result<Option<Vec<u8>>, String> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if optional && error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(format!(
                "Could not inspect {description} '{}': {error}",
                path.display()
            ));
        }
    };
    reject_link_or_reparse(&metadata, path, description)?;
    if !metadata.is_file() {
        return Err(format!(
            "{description} is not a regular file: {}",
            path.display()
        ));
    }
    if metadata.len() > MAX_RUNTIME_STAGE_TABLE_BYTES {
        return Err(format!(
            "{description} '{}' is larger than {MAX_RUNTIME_STAGE_TABLE_BYTES} bytes",
            path.display()
        ));
    }
    let bytes = fs::read(path)
        .map_err(|error| format!("Could not read {description} '{}': {error}", path.display()))?;
    let document = parse_jdrama_document(&bytes).map_err(|error| {
        format!(
            "Could not parse {description} '{}': {error}",
            path.display()
        )
    })?;
    let rebuilt = document.to_bytes().map_err(|error| {
        format!(
            "Could not rebuild {description} '{}' semantically: {error}",
            path.display()
        )
    })?;
    if rebuilt != bytes {
        return Err(format!(
            "{description} '{}' is not a byte-exact typed JDrama document",
            path.display()
        ));
    }
    parse_jdrama_scenario_archive_entries(&bytes).map_err(|error| {
        format!(
            "Could not locate the runtime area/scenario registry in {description} '{}': {error}",
            path.display()
        )
    })?;
    Ok(Some(bytes))
}

fn runtime_archive_stem(archive_name: &str) -> Option<&str> {
    let file_name = archive_name
        .rsplit(['/', '\\'])
        .next()
        .filter(|name| !name.is_empty())?;
    let stem = file_name
        .rsplit_once('.')
        .map_or(file_name, |(stem, extension)| {
            if extension.eq_ignore_ascii_case("arc") || extension.eq_ignore_ascii_case("szs") {
                stem
            } else {
                file_name
            }
        });
    if stem.is_empty() {
        None
    } else {
        Some(stem)
    }
}

fn find_case_insensitive_path(
    root: &Path,
    components: &[&str],
    description: &str,
) -> Result<PathBuf, String> {
    let mut current = root.to_path_buf();
    for (index, expected) in components.iter().enumerate() {
        let entries = fs::read_dir(&current).map_err(|error| {
            format!(
                "Could not enumerate {description} parent '{}': {error}",
                current.display()
            )
        })?;
        let mut matches = Vec::new();
        for entry in entries {
            let entry = entry.map_err(|error| {
                format!(
                    "Could not enumerate an entry in {description} parent '{}': {error}",
                    current.display()
                )
            })?;
            if entry
                .file_name()
                .to_string_lossy()
                .eq_ignore_ascii_case(expected)
            {
                matches.push(entry);
            }
        }
        matches.sort_by_key(fs::DirEntry::file_name);
        if matches.len() != 1 {
            return Err(if matches.is_empty() {
                format!(
                    "Could not find {description} component '{}' beneath '{}'",
                    expected,
                    current.display()
                )
            } else {
                format!(
                    "Found multiple case-insensitive {description} components named '{}' beneath '{}'",
                    expected,
                    current.display()
                )
            });
        }
        current = matches.remove(0).path();
        let metadata = fs::symlink_metadata(&current).map_err(|error| {
            format!(
                "Could not inspect {description} component '{}': {error}",
                current.display()
            )
        })?;
        reject_link_or_reparse(&metadata, &current, description)?;
        let is_last = index + 1 == components.len();
        if (!is_last && !metadata.is_dir()) || (is_last && !metadata.is_file()) {
            return Err(format!(
                "{description} component has the wrong file type: {}",
                current.display()
            ));
        }
    }
    Ok(current)
}

#[cfg(test)]
fn prepare_managed_run_mirror_from_source(
    project: &OpenProject,
    document_base_root: &Path,
    source_path: &Path,
    archive_bytes: &[u8],
) -> Result<ManagedRunMirrorOutcome, String> {
    let cancelled = AtomicBool::new(false);
    prepare_managed_run_mirror_from_source_with_cancel(
        project,
        document_base_root,
        source_path,
        archive_bytes,
        &cancelled,
    )
}

fn prepare_managed_run_mirror_from_source_with_cancel(
    project: &OpenProject,
    document_base_root: &Path,
    source_path: &Path,
    archive_bytes: &[u8],
    cancelled: &AtomicBool,
) -> Result<ManagedRunMirrorOutcome, String> {
    check_cancelled(cancelled)?;
    if archive_bytes.is_empty() {
        return Err("Refusing to write an empty managed stage archive".to_string());
    }
    let source_relative_path =
        managed_stage_relative_path_from_source(project, document_base_root, source_path)?;
    if !relative_starts_with_ascii_case(&source_relative_path, "files") {
        return Err(format!(
            "Runnable stage source must be inside the extracted game's files directory: {}",
            source_relative_path.display()
        ));
    }

    let base_root = normalized_absolute_with_missing_tail(&project.descriptor.base_game_root)?;
    let inventory = inventory_base_game(&base_root, cancelled)?;
    check_cancelled(cancelled)?;
    let main_relative = PathBuf::from("sys").join("main.dol");
    if !inventory.file_paths.contains(&main_relative) {
        return Err(format!(
            "Extracted base game has no regular non-link sys/main.dol: {}",
            base_root.join(&main_relative).display()
        ));
    }
    let build_root = ensure_owned_build_root(project)?;
    let run_root = ensure_child_directory(&build_root, Path::new(RUN_ROOT_NAME))?;
    let mut stats = MirrorStats::default();
    clean_stale_run_entries(&run_root, &inventory, &mut stats, cancelled)?;
    for directory in &inventory.directories {
        check_cancelled(cancelled)?;
        ensure_child_directory(&run_root, directory)?;
    }
    for file in &inventory.files {
        check_cancelled(cancelled)?;
        let destination = run_root.join(&file.relative);
        match mirror_regular_file(&file.source, &destination, cancelled)? {
            MirroredFileAction::Copied => stats.copied_files += 1,
            MirroredFileAction::Reused => stats.reused_files += 1,
        }
    }
    check_cancelled(cancelled)?;

    install_project_runtime_stage_table(project, &run_root, &mut stats, cancelled)?;
    check_cancelled(cancelled)?;

    let run_main_dol = run_root.join(&main_relative);
    let main_metadata = fs::symlink_metadata(&run_main_dol).map_err(|error| {
        format!(
            "Could not inspect mirrored main.dol '{}': {error}",
            run_main_dol.display()
        )
    })?;
    reject_link_or_reparse(&main_metadata, &run_main_dol, "mirrored main.dol")?;
    if !main_metadata.is_file() {
        return Err(format!(
            "Mirrored main.dol is not a regular file: {}",
            run_main_dol.display()
        ));
    }

    let stage_parent = source_relative_path.parent().ok_or_else(|| {
        format!(
            "Runnable stage source has no parent directory: {}",
            source_relative_path.display()
        )
    })?;
    let stage_parent = ensure_child_directory(&run_root, stage_parent)?;
    let stage_name = source_relative_path.file_name().ok_or_else(|| {
        format!(
            "Runnable stage source has no file name: {}",
            source_relative_path.display()
        )
    })?;
    let stage_output_path = stage_parent.join(stage_name);
    // `atomic_write` creates a new sibling file and replaces this directory
    // entry. The runnable tree already contains independent copies, and the
    // atomic replacement also prevents Dolphin from observing a partial SZS.
    let stage_replaced = atomic_write_with_cancel(&stage_output_path, archive_bytes, cancelled)
        .map_err(|error| {
            if is_cancelled_error(&error.to_string()) {
                error.to_string()
            } else {
                format!(
                    "Could not install authored stage in runnable mirror '{}': {error}",
                    stage_output_path.display()
                )
            }
        })?;
    Ok(ManagedRunMirrorOutcome {
        build_root,
        run_root,
        run_main_dol,
        source_relative_path,
        stage_output_path,
        stage_size_bytes: archive_bytes.len(),
        stage_replaced,
        copied_files: stats.copied_files,
        reused_files: stats.reused_files,
        removed_entries: stats.removed_entries,
    })
}

fn install_project_runtime_stage_table(
    project: &OpenProject,
    run_root: &Path,
    stats: &mut MirrorStats,
    cancelled: &AtomicBool,
) -> Result<(), String> {
    let source = PROJECT_RUNTIME_STAGE_TABLE_PATH
        .iter()
        .fold(project.data_root(), |path, component| path.join(component));
    let Some(bytes) =
        read_runtime_stage_table_file(&source, "project runtime stage archive table", true)?
    else {
        return Ok(());
    };
    check_cancelled(cancelled)?;
    let destination = PROJECT_RUNTIME_STAGE_TABLE_PATH
        .iter()
        .fold(run_root.to_path_buf(), |path, component| {
            path.join(component)
        });
    let reused =
        atomic_write_if_changed_with_cancel(&destination, &bytes, cancelled).map_err(|error| {
            format!(
                "Could not install project runtime stage archive table '{}': {error}",
                destination.display()
            )
        })?;
    if reused {
        stats.reused_files += 1;
    } else {
        stats.copied_files += 1;
    }
    Ok(())
}

#[cfg(test)]
fn write_managed_stage_archive_from_source(
    project: &OpenProject,
    document_base_root: &Path,
    source_path: &Path,
    archive_bytes: &[u8],
) -> Result<ManagedStageBuildOutcome, String> {
    let cancelled = AtomicBool::new(false);
    write_managed_stage_archive_from_source_with_cancel(
        project,
        document_base_root,
        source_path,
        archive_bytes,
        &cancelled,
    )
}

#[cfg(test)]
fn write_managed_stage_archive_from_source_with_cancel(
    project: &OpenProject,
    document_base_root: &Path,
    source_path: &Path,
    archive_bytes: &[u8],
    cancelled: &AtomicBool,
) -> Result<ManagedStageBuildOutcome, String> {
    check_cancelled(cancelled)?;
    if archive_bytes.is_empty() {
        return Err("Refusing to write an empty managed stage archive".to_string());
    }

    let source_relative_path =
        managed_stage_relative_path_from_source(project, document_base_root, source_path)?;
    let build_root = ensure_owned_build_root(project)?;
    let marker_path = build_root.join(MANAGED_BUILD_MARKER_NAME);
    let mod_root = ensure_child_directory(&build_root, Path::new(MOD_ROOT_NAME))?;
    let output_parent = match source_relative_path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => {
            ensure_child_directory(&mod_root, parent)?
        }
        _ => mod_root.clone(),
    };
    let output_name = source_relative_path.file_name().ok_or_else(|| {
        format!(
            "Stage source identity '{}' has no file name",
            source_relative_path.display()
        )
    })?;
    let output_path = output_parent.join(output_name);
    let replaced =
        atomic_write_with_cancel(&output_path, archive_bytes, cancelled).map_err(|error| {
            if is_cancelled_error(&error.to_string()) {
                error.to_string()
            } else {
                format!(
                    "Could not write managed stage archive '{}': {error}",
                    output_path.display()
                )
            }
        })?;
    Ok(ManagedStageBuildOutcome {
        build_root,
        mod_root,
        source_relative_path,
        output_path,
        marker_path,
        size_bytes: archive_bytes.len(),
        replaced,
    })
}

fn managed_stage_relative_path_from_source(
    project: &OpenProject,
    document_base_root: &Path,
    source_path: &Path,
) -> Result<PathBuf, String> {
    let project_base = normalized_absolute_with_missing_tail(&project.descriptor.base_game_root)?;
    let document_base = normalized_absolute_with_missing_tail(document_base_root)?;
    if !paths_equal_normalized(&project_base, &document_base) {
        return Err(format!(
            "Stage base '{}' does not match project base '{}'",
            document_base.display(),
            project_base.display()
        ));
    }

    let source_path = normalized_absolute_with_missing_tail(source_path)?;
    if !path_is_same_or_child(&source_path, &project_base)
        || paths_equal_normalized(&source_path, &project_base)
    {
        return Err(format!(
            "Stage source identity must be a file beneath the extracted base game: {}",
            source_path.display()
        ));
    }

    let base_component_count = project_base.components().count();
    let mut relative = PathBuf::new();
    for component in source_path.components().skip(base_component_count) {
        match component {
            Component::Normal(name) => relative.push(name),
            _ => {
                return Err(format!(
                    "Stage source identity produced an unsafe relative path: {}",
                    source_path.display()
                ));
            }
        }
    }
    if relative.as_os_str().is_empty() {
        return Err(format!(
            "Stage source identity produced an empty relative path: {}",
            source_path.display()
        ));
    }
    let supported_extension = relative
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            extension.eq_ignore_ascii_case("szs") || extension.eq_ignore_ascii_case("arc")
        });
    if !supported_extension {
        return Err(format!(
            "Managed stage source must use a .szs or .arc extension: {}",
            relative.display()
        ));
    }
    Ok(relative)
}

fn relative_starts_with_ascii_case(path: &Path, expected: &str) -> bool {
    path.components().next().is_some_and(|component| {
        matches!(component, Component::Normal(name) if name.to_string_lossy().eq_ignore_ascii_case(expected))
    })
}

fn inventory_base_game(
    base_root: &Path,
    cancelled: &AtomicBool,
) -> Result<MirrorInventory, String> {
    check_cancelled(cancelled)?;
    let metadata = fs::symlink_metadata(base_root).map_err(|error| {
        format!(
            "Could not inspect extracted base game '{}': {error}",
            base_root.display()
        )
    })?;
    validate_directory_metadata(&metadata, base_root, "extracted base game")?;

    let mut inventory = MirrorInventory::default();
    for directory_name in ["sys", "files"] {
        check_cancelled(cancelled)?;
        let source = base_root.join(directory_name);
        let relative = PathBuf::from(directory_name);
        inventory_base_directory(&source, &relative, &mut inventory, cancelled)?;
    }
    inventory
        .files
        .sort_by(|left, right| left.relative.cmp(&right.relative));
    Ok(inventory)
}

fn inventory_base_directory(
    source: &Path,
    relative: &Path,
    inventory: &mut MirrorInventory,
    cancelled: &AtomicBool,
) -> Result<(), String> {
    check_cancelled(cancelled)?;
    let metadata = fs::symlink_metadata(source).map_err(|error| {
        format!(
            "Could not inspect extracted game directory '{}': {error}",
            source.display()
        )
    })?;
    validate_directory_metadata(&metadata, source, "extracted game directory")?;
    inventory.directories.insert(relative.to_path_buf());

    let directory_entries = fs::read_dir(source).map_err(|error| {
        format!(
            "Could not enumerate extracted game directory '{}': {error}",
            source.display()
        )
    })?;
    let mut entries = Vec::new();
    for entry in directory_entries {
        check_cancelled(cancelled)?;
        entries.push(entry.map_err(|error| {
            format!(
                "Could not enumerate an entry in extracted game directory '{}': {error}",
                source.display()
            )
        })?);
    }
    entries.sort_by_key(fs::DirEntry::file_name);
    for entry in entries {
        check_cancelled(cancelled)?;
        let entry_path = entry.path();
        let entry_relative = relative.join(entry.file_name());
        let metadata = fs::symlink_metadata(&entry_path).map_err(|error| {
            format!(
                "Could not inspect extracted game entry '{}': {error}",
                entry_path.display()
            )
        })?;
        reject_link_or_reparse(&metadata, &entry_path, "extracted game entry")?;
        if metadata.is_dir() {
            inventory_base_directory(&entry_path, &entry_relative, inventory, cancelled)?;
        } else if metadata.is_file() {
            if !inventory.file_paths.insert(entry_relative.clone()) {
                return Err(format!(
                    "Extracted game contains a duplicate mirrored path: {}",
                    entry_relative.display()
                ));
            }
            inventory.files.push(MirrorFile {
                source: entry_path,
                relative: entry_relative,
            });
        } else {
            return Err(format!(
                "Extracted game entry is not a regular file or directory: {}",
                entry_path.display()
            ));
        }
    }
    Ok(())
}

fn clean_stale_run_entries(
    run_root: &Path,
    inventory: &MirrorInventory,
    stats: &mut MirrorStats,
    cancelled: &AtomicBool,
) -> Result<(), String> {
    clean_stale_run_directory(run_root, Path::new(""), inventory, stats, cancelled)
}

fn clean_stale_run_directory(
    directory: &Path,
    relative: &Path,
    inventory: &MirrorInventory,
    stats: &mut MirrorStats,
    cancelled: &AtomicBool,
) -> Result<(), String> {
    check_cancelled(cancelled)?;
    let metadata = fs::symlink_metadata(directory).map_err(|error| {
        format!(
            "Could not inspect runnable mirror directory '{}': {error}",
            directory.display()
        )
    })?;
    validate_directory_metadata(&metadata, directory, "runnable mirror directory")?;
    let directory_entries = fs::read_dir(directory).map_err(|error| {
        format!(
            "Could not enumerate runnable mirror directory '{}': {error}",
            directory.display()
        )
    })?;
    let mut entries = Vec::new();
    for entry in directory_entries {
        check_cancelled(cancelled)?;
        entries.push(entry.map_err(|error| {
            format!(
                "Could not enumerate an entry in runnable mirror directory '{}': {error}",
                directory.display()
            )
        })?);
    }
    entries.sort_by_key(fs::DirEntry::file_name);
    for entry in entries {
        check_cancelled(cancelled)?;
        let entry_path = entry.path();
        let entry_relative = relative.join(entry.file_name());
        let metadata = fs::symlink_metadata(&entry_path).map_err(|error| {
            format!(
                "Could not inspect runnable mirror entry '{}': {error}",
                entry_path.display()
            )
        })?;
        reject_link_or_reparse(&metadata, &entry_path, "runnable mirror entry")?;
        if metadata.is_dir() {
            clean_stale_run_directory(&entry_path, &entry_relative, inventory, stats, cancelled)?;
            if !inventory.directories.contains(&entry_relative) {
                check_cancelled(cancelled)?;
                fs::remove_dir(&entry_path).map_err(|error| {
                    format!(
                        "Could not remove stale owned mirror directory '{}': {error}",
                        entry_path.display()
                    )
                })?;
                stats.removed_entries += 1;
            }
        } else if metadata.is_file() {
            if !inventory.file_paths.contains(&entry_relative) {
                check_cancelled(cancelled)?;
                fs::remove_file(&entry_path).map_err(|error| {
                    format!(
                        "Could not remove stale owned mirror file '{}': {error}",
                        entry_path.display()
                    )
                })?;
                stats.removed_entries += 1;
            }
        } else {
            return Err(format!(
                "Runnable mirror entry is not a regular file or directory: {}",
                entry_path.display()
            ));
        }
    }
    Ok(())
}

fn mirror_regular_file(
    source: &Path,
    destination: &Path,
    cancelled: &AtomicBool,
) -> Result<MirroredFileAction, String> {
    check_cancelled(cancelled)?;
    let source_metadata = fs::symlink_metadata(source).map_err(|error| {
        format!(
            "Could not inspect extracted game file '{}': {error}",
            source.display()
        )
    })?;
    reject_link_or_reparse(&source_metadata, source, "extracted game file")?;
    if !source_metadata.is_file() {
        return Err(format!(
            "Extracted game file is not a regular file: {}",
            source.display()
        ));
    }

    match fs::symlink_metadata(destination) {
        Ok(destination_metadata) => {
            reject_link_or_reparse(&destination_metadata, destination, "runnable mirror file")?;
            if !destination_metadata.is_file() {
                return Err(format!(
                    "Runnable mirror path is not a regular file: {}",
                    destination.display()
                ));
            }
            let shares_base_identity =
                same_file_identity(source, &source_metadata, destination, &destination_metadata);
            if !shares_base_identity
                && regular_files_equal(
                    source,
                    &source_metadata,
                    destination,
                    &destination_metadata,
                    cancelled,
                )?
            {
                return Ok(MirroredFileAction::Reused);
            }
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(format!(
                "Could not inspect runnable mirror file '{}': {error}",
                destination.display()
            ));
        }
    }

    for _ in 0..64 {
        check_cancelled(cancelled)?;
        let temporary_path = temporary_path_candidate(destination).map_err(|error| {
            format!(
                "Could not reserve a temporary mirror path for '{}': {error}",
                destination.display()
            )
        })?;
        match copy_regular_file_new(source, &temporary_path, cancelled) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => {
                let _ = fs::remove_file(&temporary_path);
                if is_cancelled_error(&error.to_string()) {
                    return Err(error.to_string());
                }
                return Err(format!(
                    "Could not copy mirror file '{}' from '{}': {error}",
                    destination.display(),
                    source.display()
                ));
            }
        }
        check_cancelled(cancelled).inspect_err(|_| {
            let _ = fs::remove_file(&temporary_path);
        })?;
        if let Err(error) = replace_file(&temporary_path, destination) {
            let _ = fs::remove_file(&temporary_path);
            return Err(format!(
                "Could not install copied mirror file '{}' from '{}': {error}",
                destination.display(),
                source.display()
            ));
        }
        return Ok(MirroredFileAction::Copied);
    }
    Err(format!(
        "Could not reserve a temporary mirror file beside '{}'",
        destination.display()
    ))
}

fn copy_regular_file_new(
    source: &Path,
    destination: &Path,
    cancelled: &AtomicBool,
) -> io::Result<()> {
    let mut input = fs::File::open(source)?;
    let mut output = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(destination)?;
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        check_cancelled_io(cancelled)?;
        let read = input.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        output.write_all(&buffer[..read])?;
    }
    check_cancelled_io(cancelled)?;
    output.sync_all()
}

fn regular_files_equal(
    left_path: &Path,
    left_metadata: &fs::Metadata,
    right_path: &Path,
    right_metadata: &fs::Metadata,
    cancelled: &AtomicBool,
) -> Result<bool, String> {
    check_cancelled(cancelled)?;
    if left_metadata.len() != right_metadata.len() {
        return Ok(false);
    }
    let mut left = fs::File::open(left_path).map_err(|error| {
        format!(
            "Could not open base file '{}' for comparison: {error}",
            left_path.display()
        )
    })?;
    let mut right = fs::File::open(right_path).map_err(|error| {
        format!(
            "Could not open mirror file '{}' for comparison: {error}",
            right_path.display()
        )
    })?;
    let mut left_buffer = [0_u8; 64 * 1024];
    let mut right_buffer = [0_u8; 64 * 1024];
    let mut remaining = left_metadata.len();
    while remaining > 0 {
        check_cancelled(cancelled)?;
        let chunk = usize::try_from(remaining.min(left_buffer.len() as u64)).unwrap();
        left.read_exact(&mut left_buffer[..chunk])
            .map_err(|error| {
                format!(
                    "Could not read base file '{}' for comparison: {error}",
                    left_path.display()
                )
            })?;
        right
            .read_exact(&mut right_buffer[..chunk])
            .map_err(|error| {
                format!(
                    "Could not read mirror file '{}' for comparison: {error}",
                    right_path.display()
                )
            })?;
        if left_buffer[..chunk] != right_buffer[..chunk] {
            return Ok(false);
        }
        remaining -= chunk as u64;
    }
    Ok(true)
}

#[cfg(windows)]
fn same_file_identity(
    left_path: &Path,
    _left: &fs::Metadata,
    right_path: &Path,
    _right: &fs::Metadata,
) -> bool {
    windows_file_identity(left_path)
        .ok()
        .zip(windows_file_identity(right_path).ok())
        .is_some_and(|(left, right)| left == right)
}

#[cfg(windows)]
fn windows_file_identity(path: &Path) -> io::Result<(u32, u64)> {
    use std::ffi::c_void;
    use std::mem::MaybeUninit;
    use std::os::windows::io::AsRawHandle;

    #[repr(C)]
    struct FileTime {
        low_date_time: u32,
        high_date_time: u32,
    }

    #[repr(C)]
    struct ByHandleFileInformation {
        file_attributes: u32,
        creation_time: FileTime,
        last_access_time: FileTime,
        last_write_time: FileTime,
        volume_serial_number: u32,
        file_size_high: u32,
        file_size_low: u32,
        number_of_links: u32,
        file_index_high: u32,
        file_index_low: u32,
    }

    #[link(name = "Kernel32")]
    extern "system" {
        fn GetFileInformationByHandle(
            file: *mut c_void,
            information: *mut ByHandleFileInformation,
        ) -> i32;
    }

    let file = fs::File::open(path)?;
    let mut information = MaybeUninit::<ByHandleFileInformation>::uninit();
    // SAFETY: `file` remains open for the call and `information` points to a
    // correctly sized writable C-layout structure. The OS initializes the
    // structure before reporting success.
    let succeeded =
        unsafe { GetFileInformationByHandle(file.as_raw_handle(), information.as_mut_ptr()) };
    if succeeded == 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: GetFileInformationByHandle returned success and initialized the
    // complete output structure.
    let information = unsafe { information.assume_init() };
    let file_index =
        (u64::from(information.file_index_high) << 32) | u64::from(information.file_index_low);
    Ok((information.volume_serial_number, file_index))
}

#[cfg(unix)]
fn same_file_identity(
    _left_path: &Path,
    left: &fs::Metadata,
    _right_path: &Path,
    right: &fs::Metadata,
) -> bool {
    use std::os::unix::fs::MetadataExt;

    left.dev() == right.dev() && left.ino() == right.ino()
}

#[cfg(not(any(windows, unix)))]
fn same_file_identity(
    _left_path: &Path,
    _left: &fs::Metadata,
    _right_path: &Path,
    _right: &fs::Metadata,
) -> bool {
    false
}

fn ensure_owned_build_root(project: &OpenProject) -> Result<PathBuf, String> {
    let build_root = resolve_managed_build_root(project)?;
    let expected = expected_marker(project)?;

    match fs::symlink_metadata(&build_root) {
        Ok(metadata) => {
            validate_directory_metadata(&metadata, &build_root, "managed build root")?;
            validate_marker(&build_root, &expected)?;
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            let parent = build_root.parent().ok_or_else(|| {
                format!(
                    "Managed build root has no parent directory: {}",
                    build_root.display()
                )
            })?;
            let parent_metadata = fs::symlink_metadata(parent).map_err(|error| {
                format!(
                    "Managed build parent '{}' must already exist: {error}",
                    parent.display()
                )
            })?;
            validate_directory_metadata(
                &parent_metadata,
                parent,
                "managed build parent directory",
            )?;
            match fs::create_dir(&build_root) {
                Ok(()) => write_marker(&build_root, &expected)?,
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                    let metadata = fs::symlink_metadata(&build_root).map_err(|error| {
                        format!(
                            "Could not inspect managed build root '{}': {error}",
                            build_root.display()
                        )
                    })?;
                    validate_directory_metadata(&metadata, &build_root, "managed build root")?;
                    validate_marker(&build_root, &expected)?;
                }
                Err(error) => {
                    return Err(format!(
                        "Could not create managed build root '{}': {error}",
                        build_root.display()
                    ));
                }
            }
        }
        Err(error) => {
            return Err(format!(
                "Could not inspect managed build root '{}': {error}",
                build_root.display()
            ));
        }
    }

    let metadata = fs::symlink_metadata(&build_root).map_err(|error| {
        format!(
            "Could not revalidate managed build root '{}': {error}",
            build_root.display()
        )
    })?;
    validate_directory_metadata(&metadata, &build_root, "managed build root")?;
    validate_marker(&build_root, &expected)?;
    Ok(build_root)
}

fn expected_marker(project: &OpenProject) -> Result<ManagedBuildMarker, String> {
    let base_game_root = normalized_absolute_with_missing_tail(&project.descriptor.base_game_root)?;
    let metadata = fs::metadata(&base_game_root).map_err(|error| {
        format!(
            "Could not inspect extracted base game '{}': {error}",
            base_game_root.display()
        )
    })?;
    if !metadata.is_dir() {
        return Err(format!(
            "Extracted base game is not a directory: {}",
            base_game_root.display()
        ));
    }
    Ok(ManagedBuildMarker {
        format_version: MANAGED_BUILD_MARKER_VERSION,
        kind: MANAGED_BUILD_MARKER_KIND.to_string(),
        project_id: project.descriptor.project_id.clone(),
        base_game_root,
        created_with: env!("CARGO_PKG_VERSION").to_string(),
    })
}

fn write_marker(root: &Path, marker: &ManagedBuildMarker) -> Result<(), String> {
    let marker_path = root.join(MANAGED_BUILD_MARKER_NAME);
    let text = toml::to_string_pretty(marker).map_err(|error| {
        format!(
            "Could not serialize managed build ownership marker '{}': {error}",
            marker_path.display()
        )
    })?;
    let mut output = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&marker_path)
        .map_err(|error| {
            format!(
                "Could not create managed build ownership marker '{}': {error}",
                marker_path.display()
            )
        })?;
    output.write_all(text.as_bytes()).map_err(|error| {
        format!(
            "Could not write managed build ownership marker '{}': {error}",
            marker_path.display()
        )
    })?;
    output.sync_all().map_err(|error| {
        format!(
            "Could not synchronize managed build ownership marker '{}': {error}",
            marker_path.display()
        )
    })
}

fn validate_marker(root: &Path, expected: &ManagedBuildMarker) -> Result<(), String> {
    let marker_path = root.join(MANAGED_BUILD_MARKER_NAME);
    let metadata = fs::symlink_metadata(&marker_path).map_err(|error| {
        if error.kind() == io::ErrorKind::NotFound {
            format!(
                "Refusing to use unowned managed build directory '{}': ownership marker '{}' is missing",
                root.display(),
                marker_path.display()
            )
        } else {
            format!(
                "Could not inspect managed build ownership marker '{}': {error}",
                marker_path.display()
            )
        }
    })?;
    reject_link_or_reparse(&metadata, &marker_path, "managed build ownership marker")?;
    if !metadata.is_file() {
        return Err(format!(
            "Managed build ownership marker is not a regular file: {}",
            marker_path.display()
        ));
    }
    if metadata.len() > MAX_MARKER_BYTES {
        return Err(format!(
            "Managed build ownership marker is larger than {MAX_MARKER_BYTES} bytes: {}",
            marker_path.display()
        ));
    }
    let text = fs::read_to_string(&marker_path).map_err(|error| {
        format!(
            "Could not read managed build ownership marker '{}': {error}",
            marker_path.display()
        )
    })?;
    let actual: ManagedBuildMarker = toml::from_str(&text).map_err(|error| {
        format!(
            "Could not parse managed build ownership marker '{}': {error}",
            marker_path.display()
        )
    })?;
    if actual.format_version != MANAGED_BUILD_MARKER_VERSION
        || actual.kind != MANAGED_BUILD_MARKER_KIND
    {
        return Err(format!(
            "Managed build ownership marker '{}' has an unsupported format",
            marker_path.display()
        ));
    }
    if actual.project_id != expected.project_id {
        return Err(format!(
            "Managed build directory '{}' belongs to project id '{}', not '{}'",
            root.display(),
            actual.project_id,
            expected.project_id
        ));
    }
    if !actual.base_game_root.is_absolute() {
        return Err(format!(
            "Managed build ownership marker '{}' contains a relative base-game identity",
            marker_path.display()
        ));
    }
    let actual_base = normalized_absolute_with_missing_tail(&actual.base_game_root)?;
    if !paths_equal_normalized(&actual_base, &expected.base_game_root) {
        return Err(format!(
            "Managed build directory '{}' belongs to base game '{}', not '{}'",
            root.display(),
            actual_base.display(),
            expected.base_game_root.display()
        ));
    }
    Ok(())
}

fn ensure_child_directory(root: &Path, relative: &Path) -> Result<PathBuf, String> {
    let mut current = root.to_path_buf();
    for component in relative.components() {
        let Component::Normal(name) = component else {
            return Err(format!(
                "Managed build directory contains an unsafe relative component: {}",
                relative.display()
            ));
        };
        current.push(name);
        match fs::symlink_metadata(&current) {
            Ok(metadata) => {
                validate_directory_metadata(&metadata, &current, "managed build directory")?;
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                match fs::create_dir(&current) {
                    Ok(()) => {}
                    Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
                    Err(error) => {
                        return Err(format!(
                            "Could not create managed build directory '{}': {error}",
                            current.display()
                        ));
                    }
                }
                let metadata = fs::symlink_metadata(&current).map_err(|error| {
                    format!(
                        "Could not inspect managed build directory '{}': {error}",
                        current.display()
                    )
                })?;
                validate_directory_metadata(&metadata, &current, "managed build directory")?;
            }
            Err(error) => {
                return Err(format!(
                    "Could not inspect managed build directory '{}': {error}",
                    current.display()
                ));
            }
        }
    }
    Ok(current)
}

fn validate_directory_metadata(
    metadata: &fs::Metadata,
    path: &Path,
    description: &str,
) -> Result<(), String> {
    reject_link_or_reparse(metadata, path, description)?;
    if metadata.is_dir() {
        Ok(())
    } else {
        Err(format!(
            "{description} is not a directory: {}",
            path.display()
        ))
    }
}

fn reject_link_or_reparse(
    metadata: &fs::Metadata,
    path: &Path,
    description: &str,
) -> Result<(), String> {
    if metadata.file_type().is_symlink() || metadata_is_windows_reparse_point(metadata) {
        Err(format!(
            "Refusing {description} symlink or reparse point: {}",
            path.display()
        ))
    } else {
        Ok(())
    }
}

#[cfg(windows)]
fn metadata_is_windows_reparse_point(metadata: &fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;

    const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
    metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
}

#[cfg(not(windows))]
fn metadata_is_windows_reparse_point(_metadata: &fs::Metadata) -> bool {
    false
}

fn reject_overlap(
    left: &Path,
    right: &Path,
    left_label: &str,
    right_label: &str,
) -> Result<(), String> {
    if path_is_same_or_child(left, right) || path_is_same_or_child(right, left) {
        Err(format!(
            "{left_label} '{}' must not overlap {right_label} '{}'",
            left.display(),
            right.display()
        ))
    } else {
        Ok(())
    }
}

fn paths_equal_normalized(left: &Path, right: &Path) -> bool {
    path_is_same_or_child(left, right) && path_is_same_or_child(right, left)
}

fn atomic_write_if_changed_with_cancel(
    path: &Path,
    bytes: &[u8],
    cancelled: &AtomicBool,
) -> io::Result<bool> {
    check_cancelled_io(cancelled)?;
    match fs::symlink_metadata(path) {
        Ok(metadata) => {
            if metadata.file_type().is_symlink()
                || metadata_is_windows_reparse_point(&metadata)
                || !metadata.is_file()
            {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!(
                        "existing managed output is not a regular non-link file: {}",
                        path.display()
                    ),
                ));
            }
            if metadata.len() == bytes.len() as u64 {
                let mut input = fs::File::open(path)?;
                let mut compared = 0_usize;
                let mut buffer = [0_u8; 64 * 1024];
                while compared < bytes.len() {
                    check_cancelled_io(cancelled)?;
                    let chunk = (bytes.len() - compared).min(buffer.len());
                    input.read_exact(&mut buffer[..chunk])?;
                    if buffer[..chunk] != bytes[compared..compared + chunk] {
                        break;
                    }
                    compared += chunk;
                }
                if compared == bytes.len() {
                    return Ok(true);
                }
            }
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    atomic_write_with_cancel(path, bytes, cancelled)?;
    Ok(false)
}

fn atomic_write_with_cancel(path: &Path, bytes: &[u8], cancelled: &AtomicBool) -> io::Result<bool> {
    check_cancelled_io(cancelled)?;
    let replaced = match fs::symlink_metadata(path) {
        Ok(metadata) => {
            if metadata.file_type().is_symlink()
                || metadata_is_windows_reparse_point(&metadata)
                || !metadata.is_file()
            {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!(
                        "existing managed output is not a regular non-link file: {}",
                        path.display()
                    ),
                ));
            }
            true
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => false,
        Err(error) => return Err(error),
    };

    let (temporary_path, mut output) = create_temporary_file(path)?;
    let result = (|| -> io::Result<()> {
        for chunk in bytes.chunks(64 * 1024) {
            check_cancelled_io(cancelled)?;
            output.write_all(chunk)?;
        }
        check_cancelled_io(cancelled)?;
        output.sync_all()?;
        check_cancelled_io(cancelled)?;
        drop(output);
        replace_file(&temporary_path, path)
    })();
    if let Err(error) = result {
        let _ = fs::remove_file(&temporary_path);
        return Err(error);
    }
    Ok(replaced)
}

fn create_temporary_file(destination: &Path) -> io::Result<(PathBuf, fs::File)> {
    for _ in 0..64 {
        let temporary_path = temporary_path_candidate(destination)?;
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary_path)
        {
            Ok(file) => return Ok((temporary_path, file)),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        format!(
            "could not reserve a temporary output beside '{}'",
            destination.display()
        ),
    ))
}

fn temporary_path_candidate(destination: &Path) -> io::Result<PathBuf> {
    let parent = destination.parent().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("output has no parent directory: {}", destination.display()),
        )
    })?;
    let destination_name = destination.file_name().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("output has no file name: {}", destination.display()),
        )
    })?;
    let sequence = TEMP_FILE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let mut temporary_name = OsString::from(".");
    temporary_name.push(destination_name);
    temporary_name.push(format!(
        ".sms-write-{}-{sequence:016x}.tmp",
        std::process::id()
    ));
    Ok(parent.join(temporary_name))
}

#[cfg(windows)]
fn replace_file(source: &Path, destination: &Path) -> io::Result<()> {
    use std::iter;
    use std::os::windows::ffi::OsStrExt;

    const MOVEFILE_REPLACE_EXISTING: u32 = 0x1;
    const MOVEFILE_WRITE_THROUGH: u32 = 0x8;

    #[link(name = "Kernel32")]
    extern "system" {
        fn MoveFileExW(
            existing_file_name: *const u16,
            new_file_name: *const u16,
            flags: u32,
        ) -> i32;
    }

    let source = source
        .as_os_str()
        .encode_wide()
        .chain(iter::once(0))
        .collect::<Vec<_>>();
    let destination = destination
        .as_os_str()
        .encode_wide()
        .chain(iter::once(0))
        .collect::<Vec<_>>();
    // SAFETY: Both pointers reference NUL-terminated UTF-16 buffers that live
    // for the duration of the call. The files share a parent and therefore a
    // volume; no other raw handles or pointers are supplied.
    let moved = unsafe {
        MoveFileExW(
            source.as_ptr(),
            destination.as_ptr(),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    };
    if moved == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

#[cfg(not(windows))]
fn replace_file(source: &Path, destination: &Path) -> io::Result<()> {
    fs::rename(source, destination)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::project::{ProjectStageMusic, SmsProjectFile};
    use std::hash::{DefaultHasher, Hash, Hasher};

    struct Fixture {
        _root: tempfile::TempDir,
        project: OpenProject,
        base_root: PathBuf,
        data_root: PathBuf,
        source_path: PathBuf,
    }

    fn fixture() -> Fixture {
        let root = tempfile::tempdir().unwrap();
        let base_root = root.path().join("SunshineExtract");
        let data_root = root.path().join("Authoring.smsdata");
        let source_path = base_root
            .join("files")
            .join("data")
            .join("scene")
            .join("bianco0.szs");
        fs::create_dir_all(source_path.parent().unwrap()).unwrap();
        fs::create_dir_all(base_root.join("sys")).unwrap();
        fs::create_dir_all(&data_root).unwrap();
        fs::write(base_root.join("sys/main.dol"), b"retail-main-dol").unwrap();
        fs::write(base_root.join("files/shared.bin"), b"shared-retail-data").unwrap();
        fs::write(&source_path, b"retail-source-must-not-change").unwrap();
        let descriptor_path = root.path().join("Authoring.sms");
        let descriptor = SmsProjectFile::new(
            "Authoring",
            base_root.clone(),
            PathBuf::from("Authoring.smsdata"),
            None,
        );
        descriptor.save(&descriptor_path).unwrap();
        let project = OpenProject::load(&descriptor_path).unwrap();
        Fixture {
            _root: root,
            project,
            base_root,
            data_root,
            source_path,
        }
    }

    fn runtime_stage_table_bytes() -> Vec<u8> {
        use sms_formats::{
            JDramaDocument, JDramaField, JDramaFieldValue, JDramaRecord, JDramaRecordPayload,
            SMS_AUTHORED_RUNTIME_CARRIER_AREAS,
        };

        let areas = (0_u8..55)
            .map(|area_index| {
                let archive_name = if SMS_AUTHORED_RUNTIME_CARRIER_AREAS.contains(&area_index) {
                    "none.arc".to_string()
                } else {
                    format!("retail{area_index}.arc")
                };
                let scenario = JDramaRecord::new(
                    "ScenarioArchiveName",
                    format!("scenario {area_index} 0"),
                    JDramaRecordPayload::Fields {
                        fields: vec![JDramaField {
                            name: "archive_name".to_string(),
                            value: JDramaFieldValue::String(archive_name),
                        }],
                    },
                )
                .unwrap();
                JDramaRecord::new(
                    "ScenarioArchiveNameTable",
                    format!("area {area_index}"),
                    JDramaRecordPayload::Group {
                        fields: Vec::new(),
                        children: vec![scenario],
                    },
                )
                .unwrap()
            })
            .collect();
        let table = JDramaRecord::new(
            "ScenarioArchiveNamesInStage",
            "runtime stages",
            JDramaRecordPayload::Group {
                fields: Vec::new(),
                children: areas,
            },
        )
        .unwrap();
        let root = JDramaRecord::new(
            "NameRefGrp",
            "root",
            JDramaRecordPayload::Group {
                fields: Vec::new(),
                children: vec![table],
            },
        )
        .unwrap();
        JDramaDocument { root }.to_bytes().unwrap()
    }

    fn file_hash(path: &Path) -> u64 {
        let mut hasher = DefaultHasher::new();
        fs::read(path).unwrap().hash(&mut hasher);
        hasher.finish()
    }

    #[test]
    fn runtime_target_uses_staged_table_order_without_region_assumptions() {
        let entries = vec![
            JDramaScenarioArchiveEntry {
                area_index: 7,
                scenario_index: 4,
                archive_name: "mods/scenes/PINNABEACH4.ARC".to_string(),
            },
            JDramaScenarioArchiveEntry {
                area_index: 7,
                scenario_index: 6,
                archive_name: "pinnaBeach4.arc".to_string(),
            },
        ];

        let (target, count) =
            resolve_runtime_stage_target(&entries, Path::new("files/data/scene/pinnaBeach4.szs"))
                .unwrap();

        assert_eq!(count, 2);
        assert_eq!(target.area_index, 7);
        assert_eq!(target.scenario_index, 4);
        assert_eq!(target.archive_name, "mods/scenes/PINNABEACH4.ARC");
    }

    #[test]
    fn runtime_target_rejects_missing_and_unrepresentable_mappings() {
        let missing = resolve_runtime_stage_target(
            &[JDramaScenarioArchiveEntry {
                area_index: 2,
                scenario_index: 0,
                archive_name: "bianco0.arc".to_string(),
            }],
            Path::new("files/data/scene/mare0.szs"),
        )
        .unwrap_err();
        assert!(missing.contains("not mapped"));

        let too_large = resolve_runtime_stage_target(
            &[JDramaScenarioArchiveEntry {
                area_index: 256,
                scenario_index: 0,
                archive_name: "mare0.arc".to_string(),
            }],
            Path::new("files/data/scene/MARE0.SZS"),
        )
        .unwrap_err();
        assert!(too_large.contains("does not fit"));
    }

    #[test]
    fn project_runtime_table_adds_a_new_slot_to_the_run_mirror_and_direct_boot_resolution() {
        let fixture = fixture();
        let base_table_path = fixture.base_root.join("files/data/stageArc.bin");
        let base_table = runtime_stage_table_bytes();
        fs::write(&base_table_path, &base_table).unwrap();
        assert_eq!(
            read_effective_runtime_stage_table(&fixture.project).unwrap(),
            base_table
        );

        let authored =
            sms_formats::append_jdrama_scenario_archive_slot(&base_table, "myNewStage.arc")
                .unwrap();
        let project_table_path = fixture.data_root.join("files/data/stageArc.bin");
        fs::create_dir_all(project_table_path.parent().unwrap()).unwrap();
        fs::write(&project_table_path, &authored.bytes).unwrap();
        assert_eq!(
            read_effective_runtime_stage_table(&fixture.project).unwrap(),
            authored.bytes
        );

        let virtual_source = fixture.base_root.join("files/data/scene/myNewStage.szs");
        assert!(!virtual_source.exists());
        let run = prepare_managed_run_mirror_from_source(
            &fixture.project,
            &fixture.base_root,
            &virtual_source,
            b"authored-new-stage",
        )
        .unwrap();

        assert_eq!(
            fs::read(run.run_root.join("files/data/stageArc.bin")).unwrap(),
            authored.bytes
        );
        assert_eq!(fs::read(&base_table_path).unwrap(), base_table);
        assert_eq!(
            fs::read(&run.stage_output_path).unwrap(),
            b"authored-new-stage"
        );
        assert!(
            !virtual_source.exists(),
            "extracted base must remain read-only"
        );
        let run_entries = parse_jdrama_scenario_archive_entries(
            &fs::read(run.run_root.join("files/data/stageArc.bin")).unwrap(),
        )
        .unwrap();
        let (target, contexts) =
            resolve_runtime_stage_target(&run_entries, &run.source_relative_path).unwrap();
        assert_eq!(contexts, 1);
        assert_eq!(target.area_index, 17);
        assert_eq!(target.scenario_index, 1);
        assert_eq!(target.archive_name, "myNewStage.arc");
    }

    #[test]
    fn unchanged_atomic_launch_output_is_reused() {
        let root = tempfile::tempdir().unwrap();
        let output = root.path().join("launch.dol");
        let cancelled = AtomicBool::new(false);

        assert!(!atomic_write_if_changed_with_cancel(&output, b"first", &cancelled).unwrap());
        let first_modified = fs::metadata(&output).unwrap().modified().unwrap();
        assert!(atomic_write_if_changed_with_cancel(&output, b"first", &cancelled).unwrap());
        assert_eq!(fs::read(&output).unwrap(), b"first");
        assert_eq!(
            fs::metadata(&output).unwrap().modified().unwrap(),
            first_modified
        );
        assert!(!atomic_write_if_changed_with_cancel(&output, b"second", &cancelled).unwrap());
        assert_eq!(fs::read(output).unwrap(), b"second");
    }

    #[test]
    fn runnable_mirror_preserves_base_and_uses_independent_file_copies() {
        let fixture = fixture();
        let base_stage_before = fs::read(&fixture.source_path).unwrap();
        let base_stage_hash_before = file_hash(&fixture.source_path);

        let outcome = prepare_managed_run_mirror_from_source(
            &fixture.project,
            &fixture.base_root,
            &fixture.source_path,
            b"authored-stage-for-dolphin",
        )
        .unwrap();

        assert_eq!(outcome.run_root, outcome.build_root.join("run-root"));
        assert_eq!(outcome.run_main_dol, outcome.run_root.join("sys/main.dol"));
        assert_eq!(fs::read(&outcome.run_main_dol).unwrap(), b"retail-main-dol");
        let base_main_dol = fixture.base_root.join("sys/main.dol");
        assert!(!same_file_identity(
            &base_main_dol,
            &fs::metadata(&base_main_dol).unwrap(),
            &outcome.run_main_dol,
            &fs::metadata(&outcome.run_main_dol).unwrap(),
        ));
        assert!(outcome.stage_replaced);
        assert_eq!(
            fs::read(&outcome.stage_output_path).unwrap(),
            b"authored-stage-for-dolphin"
        );
        assert_ne!(
            fs::read(&outcome.stage_output_path).unwrap(),
            base_stage_before
        );
        assert_eq!(fs::read(&fixture.source_path).unwrap(), base_stage_before);
        assert_eq!(file_hash(&fixture.source_path), base_stage_hash_before);
        let base_metadata = fs::metadata(&fixture.source_path).unwrap();
        let run_metadata = fs::metadata(&outcome.stage_output_path).unwrap();
        assert!(!same_file_identity(
            &fixture.source_path,
            &base_metadata,
            &outcome.stage_output_path,
            &run_metadata,
        ));
        assert_eq!(outcome.copied_files + outcome.reused_files, 3);
    }

    #[test]
    fn runnable_mirror_refreshes_owned_entries_without_recursive_root_replacement() {
        let fixture = fixture();
        let first = prepare_managed_run_mirror_from_source(
            &fixture.project,
            &fixture.base_root,
            &fixture.source_path,
            b"first-authored-stage",
        )
        .unwrap();
        let stale_file = first.run_root.join("stale").join("obsolete.bin");
        fs::create_dir_all(stale_file.parent().unwrap()).unwrap();
        fs::write(&stale_file, b"stale").unwrap();
        fs::write(&first.run_main_dol, b"target-specific direct-boot patch").unwrap();
        let second = prepare_managed_run_mirror_from_source(
            &fixture.project,
            &fixture.base_root,
            &fixture.source_path,
            b"second-authored-stage",
        )
        .unwrap();

        assert!(!stale_file.exists());
        assert!(!first.run_root.join("stale").exists());
        assert_eq!(second.removed_entries, 2);
        assert_eq!(
            fs::read(&second.stage_output_path).unwrap(),
            b"second-authored-stage"
        );
        assert_eq!(
            fs::read(&fixture.source_path).unwrap(),
            b"retail-source-must-not-change"
        );
        assert_eq!(fs::read(&second.run_main_dol).unwrap(), b"retail-main-dol");
        assert_eq!(
            fs::read(fixture.base_root.join("sys/main.dol")).unwrap(),
            b"retail-main-dol"
        );
        assert!(second.reused_files >= 1);
        assert!(second.copied_files >= 2);
    }

    #[test]
    fn cancelled_run_mirror_keeps_base_untouched_and_next_build_reconciles() {
        let fixture = fixture();
        let base_stage_before = fs::read(&fixture.source_path).unwrap();
        let build_root = ensure_owned_build_root(&fixture.project).unwrap();
        let run_root = ensure_child_directory(&build_root, Path::new(RUN_ROOT_NAME)).unwrap();
        let partial_output = run_root.join("partial-first-build.bin");
        fs::write(&partial_output, b"owned partial output").unwrap();

        let cancelled = AtomicBool::new(true);
        let error = prepare_managed_run_mirror_from_source_with_cancel(
            &fixture.project,
            &fixture.base_root,
            &fixture.source_path,
            b"cancelled-authored-stage",
            &cancelled,
        )
        .unwrap_err();

        assert!(is_cancelled_error(&error));
        assert!(build_root.is_dir());
        assert!(partial_output.is_file());
        assert_eq!(fs::read(&fixture.source_path).unwrap(), base_stage_before);
        assert!(!build_root.join(MOD_ROOT_NAME).exists());

        cancelled.store(false, Ordering::Release);
        let recovered = prepare_managed_run_mirror_from_source_with_cancel(
            &fixture.project,
            &fixture.base_root,
            &fixture.source_path,
            b"recovered-authored-stage",
            &cancelled,
        )
        .unwrap();

        assert!(!partial_output.exists());
        assert_eq!(
            fs::read(recovered.stage_output_path).unwrap(),
            b"recovered-authored-stage"
        );
        assert_eq!(fs::read(&fixture.source_path).unwrap(), base_stage_before);
    }

    #[test]
    fn managed_stage_write_uses_owned_sibling_root_and_atomically_replaces() {
        let fixture = fixture();
        let first = write_managed_stage_archive_from_source(
            &fixture.project,
            &fixture.base_root,
            &fixture.source_path,
            b"first-authored-stage",
        )
        .unwrap();

        assert_eq!(
            first.build_root,
            normalized_absolute_with_missing_tail(
                &fixture.project.descriptor_path.with_extension("smsbuild")
            )
            .unwrap()
        );
        assert_eq!(
            first.source_relative_path,
            PathBuf::from("files/data/scene/bianco0.szs")
        );
        assert_eq!(
            first.output_path,
            first
                .build_root
                .join("mod-root/files/data/scene/bianco0.szs")
        );
        assert_eq!(
            fs::read(&first.output_path).unwrap(),
            b"first-authored-stage"
        );
        assert_eq!(
            fs::read(&fixture.source_path).unwrap(),
            b"retail-source-must-not-change"
        );
        assert!(!first.replaced);
        assert!(first.marker_path.is_file());

        let second = write_managed_stage_archive_from_source(
            &fixture.project,
            &fixture.base_root,
            &fixture.source_path,
            b"second-authored-stage",
        )
        .unwrap();
        assert!(second.replaced);
        assert_eq!(
            fs::read(second.output_path).unwrap(),
            b"second-authored-stage"
        );
        assert_eq!(
            fs::read(&fixture.source_path).unwrap(),
            b"retail-source-must-not-change"
        );
    }

    #[test]
    fn detached_source_identity_does_not_require_the_original_archive() {
        let fixture = fixture();
        fs::remove_file(&fixture.source_path).unwrap();

        let outcome = write_managed_stage_archive_from_source(
            &fixture.project,
            &fixture.base_root,
            &fixture.source_path,
            b"source-free-stage",
        )
        .unwrap();

        assert_eq!(fs::read(outcome.output_path).unwrap(), b"source-free-stage");
        assert!(!fixture.source_path.exists());
    }

    #[test]
    fn existing_unowned_build_directory_is_rejected() {
        let fixture = fixture();
        fs::create_dir(fixture.project.managed_build_root()).unwrap();

        let error = write_managed_stage_archive_from_source(
            &fixture.project,
            &fixture.base_root,
            &fixture.source_path,
            b"stage",
        )
        .unwrap_err();

        assert!(error.contains("unowned managed build directory"));
    }

    #[test]
    fn ownership_marker_must_match_project_and_base_identity() {
        let fixture = fixture();
        let outcome = write_managed_stage_archive_from_source(
            &fixture.project,
            &fixture.base_root,
            &fixture.source_path,
            b"stage",
        )
        .unwrap();
        let mut marker: ManagedBuildMarker =
            toml::from_str(&fs::read_to_string(&outcome.marker_path).unwrap()).unwrap();
        marker.project_id = "another-project".to_string();
        fs::write(
            &outcome.marker_path,
            toml::to_string_pretty(&marker).unwrap(),
        )
        .unwrap();

        let error = write_managed_stage_archive_from_source(
            &fixture.project,
            &fixture.base_root,
            &fixture.source_path,
            b"new-stage",
        )
        .unwrap_err();
        assert!(error.contains("belongs to project id 'another-project'"));
        assert_eq!(fs::read(outcome.output_path).unwrap(), b"stage");
    }

    #[test]
    fn stage_source_and_document_base_must_belong_to_the_project_base() {
        let fixture = fixture();
        let outside = fixture
            .project
            .descriptor_path
            .parent()
            .unwrap()
            .join("outside.szs");
        let error =
            managed_stage_relative_path_from_source(&fixture.project, &fixture.base_root, &outside)
                .unwrap_err();
        assert!(error.contains("beneath the extracted base game"));

        let error = managed_stage_relative_path_from_source(
            &fixture.project,
            fixture.project.descriptor_path.parent().unwrap(),
            &fixture.source_path,
        )
        .unwrap_err();
        assert!(error.contains("does not match project base"));
    }

    #[test]
    fn project_validation_rejects_build_overlap_and_relative_traversal() {
        let fixture = fixture();
        let mut descriptor = fixture.project.descriptor.clone();
        descriptor.managed_build_root = Some(fixture.base_root.join("build"));
        let error = descriptor
            .save(&fixture.project.descriptor_path)
            .unwrap_err();
        assert!(error.contains("must not overlap the extracted base game"));

        descriptor.managed_build_root = Some(fixture.data_root.join("build"));
        let error = descriptor
            .save(&fixture.project.descriptor_path)
            .unwrap_err();
        assert!(error.contains("must not overlap project data"));

        descriptor.managed_build_root = Some(PathBuf::from("../outside.smsbuild"));
        let error = descriptor
            .save(&fixture.project.descriptor_path)
            .unwrap_err();
        assert!(error.contains("unsafe relative managed build path"));
    }

    #[test]
    #[ignore = "requires SMS_BASE_ROOT with an extracted retail game"]
    fn normal_managed_package_installs_audio_without_direct_boot() {
        let base_root = PathBuf::from(std::env::var_os("SMS_BASE_ROOT").expect("SMS_BASE_ROOT"));
        let source_dol = base_root.join("sys/main.dol");
        let source_stage_table = base_root.join("files/data/stageArc.bin");
        let original_dol = fs::read(&source_dol).unwrap();
        let root = tempfile::tempdir().unwrap();
        let run_root = root.path().join("run-root");
        let run_dol = run_root.join("sys/main.dol");
        let run_stage_table = run_root.join("files/data/stageArc.bin");
        fs::create_dir_all(run_dol.parent().unwrap()).unwrap();
        fs::create_dir_all(run_stage_table.parent().unwrap()).unwrap();
        fs::write(&run_dol, &original_dol).unwrap();
        fs::copy(source_stage_table, &run_stage_table).unwrap();
        let descriptor_path = root.path().join("project.sms");
        let mut descriptor = SmsProjectFile::new(
            "Music test",
            base_root.clone(),
            root.path().join("project-data"),
            None,
        );
        descriptor.stage_music.insert(
            "dolpic0".to_string(),
            ProjectStageMusic {
                bgm_id: 0x8001_0002,
                wave_scene_id: 0x202,
                secondary_bgm_id: Some(0x8001_0002),
                secondary_wave_scene_id: Some(0x202),
            },
        );
        descriptor.sound_assignments.insert(
            "map_static:SoundObjRiver".to_string(),
            ProjectSoundAssignment {
                kind: ProjectSoundAssignmentKind::MapStatic,
                source_name: "SoundObjRiver".to_string(),
                original_sound_id: 0x500f,
                sound_id: 0x5000,
            },
        );
        let project = OpenProject {
            descriptor_path,
            descriptor,
        };
        let run = ManagedRunMirrorOutcome {
            build_root: root.path().to_path_buf(),
            run_root,
            run_main_dol: run_dol.clone(),
            source_relative_path: PathBuf::from("files/data/scene/dolpic0.szs"),
            stage_output_path: root.path().join("dolpic0.szs"),
            stage_size_bytes: 0,
            stage_replaced: false,
            copied_files: 0,
            reused_files: 0,
            removed_entries: 0,
        };
        install_managed_stage_music(&project, &run, &AtomicBool::new(false)).unwrap();
        let packaged = fs::read(&run_dol).unwrap();
        assert!(packaged.len() > original_dol.len());
        const MUSIC_MARKER: &[u8] = b"SMS_EDITOR_STAGE_MUSIC_V1\0";
        assert!(packaged
            .windows(MUSIC_MARKER.len())
            .any(|window| window == MUSIC_MARKER));
        assert_eq!(fs::read(source_dol).unwrap(), original_dol);
    }
}
