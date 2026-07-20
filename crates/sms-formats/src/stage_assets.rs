use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::SystemTime;

use serde::{Deserialize, Serialize};
use walkdir::WalkDir;

use crate::{decode_yaz0, FormatError, PreserveBytes, RarcArchive, Result};

const ARCHIVE_CACHE_CAPACITY: usize = 8;
const ARCHIVE_CACHE_MAX_BYTES: usize = 512 * 1024 * 1024;
const ARCHIVE_STABLE_READ_ATTEMPTS: usize = 3;
static ARCHIVE_CACHE: OnceLock<Mutex<ArchiveCache>> = OnceLock::new();

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum StageAssetKind {
    Archive,
    Model,
    MaterialTable,
    Texture,
    Collision,
    Message,
    Particle,
    Animation,
    Placement,
    Other,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StageAsset {
    pub path: PathBuf,
    pub kind: StageAssetKind,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SceneArchiveInfo {
    pub stage_id: String,
    pub group: String,
    pub relative_path: PathBuf,
    pub path: PathBuf,
    pub size_bytes: u64,
}

pub fn scan_stage_assets(base_root: impl AsRef<Path>, stage_id: &str) -> Result<Vec<StageAsset>> {
    let base_root = base_root.as_ref();
    if !base_root.exists() {
        return Err(FormatError::Io(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!("base root does not exist: {}", base_root.display()),
        )));
    }

    let needle = stage_id.to_ascii_lowercase();
    let mut assets = Vec::new();
    let scene_archives = discover_scene_archives(base_root)?;
    let selected_archives = select_scene_archives(&scene_archives, &needle)?;

    for entry in WalkDir::new(base_root).follow_links(false) {
        let entry = entry.map_err(walk_error)?;
        if !entry.file_type().is_file() {
            continue;
        }
        let path = entry.path();
        if is_repo_workspace_file(base_root, path) {
            continue;
        }

        let lower = path.to_string_lossy().to_ascii_lowercase();
        let is_selected_archive = selected_archives
            .iter()
            .any(|archive| archive.path.as_path() == path);
        let is_stage_match = needle.is_empty()
            || lower.contains(&format!("/scene/{needle}"))
            || lower.contains(&format!("\\scene\\{needle}"))
            || is_selected_archive;
        let is_common = lower.contains("/common/") || lower.contains("\\common\\");
        if !(is_stage_match || is_common) {
            continue;
        }

        assets.push(StageAsset {
            path: path.to_path_buf(),
            kind: classify_asset(path),
        });
    }

    for archive in selected_archives {
        let mounted_assets = mount_scene_archive(&archive.path)?;
        assets.extend(mounted_assets);
    }

    assets.sort_by(|a, b| a.path.cmp(&b.path));
    assets.dedup_by(|a, b| a.path == b.path);
    Ok(assets)
}

/// Lists only release-global assets under `common` without selecting or
/// mounting any retail scene archive.
///
/// Genuinely new authored stages use this path so a stage ID that happens to
/// be a substring of one retail archive cannot pull that level into the new
/// scene through fuzzy discovery.
pub fn scan_common_stage_assets(base_root: impl AsRef<Path>) -> Result<Vec<StageAsset>> {
    let base_root = base_root.as_ref();
    if !base_root.exists() {
        return Err(FormatError::Io(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!("base root does not exist: {}", base_root.display()),
        )));
    }

    let mut assets = Vec::new();
    for entry in WalkDir::new(base_root).follow_links(false) {
        let entry = entry.map_err(walk_error)?;
        if !entry.file_type().is_file() {
            continue;
        }
        let path = entry.path();
        if is_repo_workspace_file(base_root, path) {
            continue;
        }
        let lower = path.to_string_lossy().to_ascii_lowercase();
        if !lower.contains("/common/") && !lower.contains("\\common\\") {
            continue;
        }
        assets.push(StageAsset {
            path: path.to_path_buf(),
            kind: classify_asset(path),
        });
    }
    assets.sort_by(|left, right| left.path.cmp(&right.path));
    assets.dedup_by(|left, right| left.path == right.path);
    Ok(assets)
}

pub fn discover_scene_archives(base_root: impl AsRef<Path>) -> Result<Vec<SceneArchiveInfo>> {
    let base_root = base_root.as_ref();
    if !base_root.exists() {
        return Err(FormatError::Io(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!("base root does not exist: {}", base_root.display()),
        )));
    }

    let mut archives = Vec::new();
    for entry in WalkDir::new(base_root).follow_links(false) {
        let entry = entry.map_err(walk_error)?;
        if !entry.file_type().is_file() {
            continue;
        }
        let path = entry.path();
        if is_repo_workspace_file(base_root, path) || !is_archive_path(path) {
            continue;
        }

        let lower = path.to_string_lossy().to_ascii_lowercase();
        let in_scene_dir = lower.contains("/data/scene/") || lower.contains("\\data\\scene\\");
        if !in_scene_dir {
            continue;
        }

        let stage_id = path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .unwrap_or_default()
            .to_string();
        if stage_id.is_empty() {
            continue;
        }

        archives.push(SceneArchiveInfo {
            group: scene_group(&stage_id),
            stage_id,
            relative_path: path
                .strip_prefix(base_root)
                .map(Path::to_path_buf)
                .unwrap_or_else(|_| path.to_path_buf()),
            path: path.to_path_buf(),
            size_bytes: entry.metadata().map_err(walk_error)?.len(),
        });
    }

    archives.sort_by(|a, b| a.stage_id.cmp(&b.stage_id));
    Ok(archives)
}

pub fn mount_scene_archive(path: impl AsRef<Path>) -> Result<Vec<StageAsset>> {
    let path = path.as_ref();
    let archive = load_scene_archive(path)?;
    let mut assets = Vec::new();
    for entry in archive.file_entries() {
        let virtual_path = format!("{}!/{}", path.display(), entry.path);
        let virtual_path = PathBuf::from(virtual_path);
        assets.push(StageAsset {
            kind: classify_asset(&virtual_path),
            path: virtual_path,
        });
    }

    Ok(assets)
}

pub fn read_stage_asset_bytes(path: impl AsRef<Path>) -> Result<Vec<u8>> {
    let path = path.as_ref();
    let path_text = path.to_string_lossy();
    if let Some((archive_path, internal_path)) = path_text.split_once("!/") {
        return extract_archive_file(archive_path, internal_path);
    }

    Ok(fs::read(path)?)
}

pub fn extract_archive_file(
    archive_path: impl AsRef<Path>,
    internal_path: impl AsRef<str>,
) -> Result<Vec<u8>> {
    let archive = load_scene_archive(archive_path.as_ref())?;
    archive.file_bytes(internal_path.as_ref())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ArchiveStamp {
    len: u64,
    modified: SystemTime,
}

#[derive(Debug)]
struct CachedArchive {
    path: PathBuf,
    stamp: ArchiveStamp,
    last_used: u64,
    size_bytes: usize,
    archive: Arc<RarcArchive>,
}

#[derive(Debug, Default)]
struct ArchiveCache {
    entries: Vec<CachedArchive>,
    clock: u64,
    size_bytes: usize,
}

struct StableArchiveRead {
    archive: Arc<RarcArchive>,
    cache_stamp: Option<ArchiveStamp>,
}

impl ArchiveCache {
    fn get(&mut self, path: &Path, stamp: ArchiveStamp) -> Option<Arc<RarcArchive>> {
        let index = self.entries.iter().position(|entry| entry.path == path)?;
        if self.entries[index].stamp != stamp {
            let removed = self.entries.swap_remove(index);
            self.size_bytes = self.size_bytes.saturating_sub(removed.size_bytes);
            return None;
        }
        self.clock = self.clock.wrapping_add(1);
        self.entries[index].last_used = self.clock;
        Some(Arc::clone(&self.entries[index].archive))
    }

    fn insert(&mut self, path: PathBuf, stamp: ArchiveStamp, archive: Arc<RarcArchive>) {
        self.clock = self.clock.wrapping_add(1);
        let size_bytes = archive.source_bytes().len();
        if let Some(index) = self.entries.iter().position(|entry| entry.path == path) {
            let removed = self.entries.swap_remove(index);
            self.size_bytes = self.size_bytes.saturating_sub(removed.size_bytes);
        }
        if size_bytes > ARCHIVE_CACHE_MAX_BYTES {
            return;
        }
        while !self.entries.is_empty()
            && (self.entries.len() >= ARCHIVE_CACHE_CAPACITY
                || self.size_bytes.saturating_add(size_bytes) > ARCHIVE_CACHE_MAX_BYTES)
        {
            let oldest = self
                .entries
                .iter()
                .enumerate()
                .min_by_key(|(_, entry)| entry.last_used)
                .map(|(index, _)| index)
                .expect("a non-empty archive cache has an oldest entry");
            let removed = self.entries.swap_remove(oldest);
            self.size_bytes = self.size_bytes.saturating_sub(removed.size_bytes);
        }
        self.size_bytes = self.size_bytes.saturating_add(size_bytes);
        self.entries.push(CachedArchive {
            path,
            stamp,
            last_used: self.clock,
            size_bytes,
            archive,
        });
    }
}

fn load_scene_archive(path: &Path) -> Result<Arc<RarcArchive>> {
    let path = fs::canonicalize(path)?;
    let stamp = archive_stamp(&path)?;
    if let Some(archive) = archive_cache()?.get(&path, stamp) {
        return Ok(archive);
    }

    let loaded = read_scene_archive_stably(
        &path,
        |path| fs::read(path).map_err(FormatError::from),
        archive_stamp,
    )?;
    if let Some(stamp) = loaded.cache_stamp {
        archive_cache()?.insert(path, stamp, Arc::clone(&loaded.archive));
    }
    Ok(loaded.archive)
}

fn read_scene_archive_stably<ReadSource, ReadStamp>(
    path: &Path,
    mut read_source: ReadSource,
    mut read_stamp: ReadStamp,
) -> Result<StableArchiveRead>
where
    ReadSource: FnMut(&Path) -> Result<Vec<u8>>,
    ReadStamp: FnMut(&Path) -> Result<ArchiveStamp>,
{
    let mut latest_source = None;
    for _ in 0..ARCHIVE_STABLE_READ_ATTEMPTS {
        let before = read_stamp(path)?;
        let source = read_source(path)?;
        let after = read_stamp(path)?;
        if before == after {
            return Ok(StableArchiveRead {
                archive: parse_scene_archive_source(source)?,
                cache_stamp: Some(after),
            });
        }
        latest_source = Some(source);
    }

    let source = latest_source.ok_or_else(|| FormatError::Unsupported {
        format: "stage archive cache",
        message: "no archive read attempts were made".to_string(),
    })?;
    Ok(StableArchiveRead {
        archive: parse_scene_archive_source(source)?,
        cache_stamp: None,
    })
}

fn parse_scene_archive_source(source: Vec<u8>) -> Result<Arc<RarcArchive>> {
    let archive_bytes = if source.starts_with(b"Yaz0") {
        decode_yaz0(&source)?
    } else {
        source
    };
    Ok(Arc::new(RarcArchive::parse(archive_bytes)?))
}

fn archive_stamp(path: &Path) -> Result<ArchiveStamp> {
    let metadata = fs::metadata(path)?;
    Ok(ArchiveStamp {
        len: metadata.len(),
        modified: metadata.modified()?,
    })
}

fn archive_cache() -> Result<std::sync::MutexGuard<'static, ArchiveCache>> {
    ARCHIVE_CACHE
        .get_or_init(|| Mutex::new(ArchiveCache::default()))
        .lock()
        .map_err(|_| FormatError::Unsupported {
            format: "stage archive cache",
            message: "archive cache lock was poisoned".to_string(),
        })
}

fn walk_error(error: walkdir::Error) -> FormatError {
    let kind = error
        .io_error()
        .map(|source| source.kind())
        .unwrap_or(io::ErrorKind::Other);
    FormatError::Io(io::Error::new(
        kind,
        format!("failed while traversing stage assets: {error}"),
    ))
}

fn select_scene_archives<'a>(
    scene_archives: &'a [SceneArchiveInfo],
    needle: &str,
) -> Result<Vec<&'a SceneArchiveInfo>> {
    if needle.is_empty() {
        return Err(FormatError::Unsupported {
            format: "stage selection",
            message: "stage id cannot be empty".to_string(),
        });
    }

    let exact: Vec<&SceneArchiveInfo> = scene_archives
        .iter()
        .filter(|archive| archive.stage_id.eq_ignore_ascii_case(needle))
        .collect();
    if exact.len() == 1 {
        return Ok(exact);
    }
    if exact.len() > 1 {
        return Err(FormatError::Unsupported {
            format: "stage selection",
            message: format!("stage id '{needle}' matches multiple archives"),
        });
    }

    let fuzzy: Vec<_> = scene_archives
        .iter()
        .filter(|archive| archive.stage_id.to_ascii_lowercase().contains(needle))
        .collect();
    match fuzzy.len() {
        0 | 1 => Ok(fuzzy),
        count => Err(FormatError::Unsupported {
            format: "stage selection",
            message: format!("stage id '{needle}' is ambiguous across {count} archives"),
        }),
    }
}

fn classify_asset(path: &Path) -> StageAssetKind {
    match path
        .extension()
        .and_then(|ext| ext.to_str())
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

fn is_archive_path(path: &Path) -> bool {
    matches!(
        path.extension()
            .and_then(|ext| ext.to_str())
            .map(|ext| ext.to_ascii_lowercase())
            .as_deref(),
        Some("arc" | "szs")
    )
}

fn scene_group(stage_id: &str) -> String {
    let mut group = String::new();
    for ch in stage_id.chars() {
        if ch.is_ascii_digit() {
            break;
        }
        group.push(ch);
    }

    group.trim_end_matches('_').to_string()
}

fn is_repo_workspace_file(base_root: &Path, path: &Path) -> bool {
    let Ok(relative) = path.strip_prefix(base_root) else {
        return false;
    };

    let first = relative
        .components()
        .next()
        .map(|component| component.as_os_str().to_string_lossy().to_ascii_lowercase());

    matches!(
        first.as_deref(),
        Some(
            ".git"
                | ".github"
                | ".codex"
                | ".claude"
                | "build"
                | "config"
                | "docs"
                | "editor"
                | "include"
                | "src"
                | "tools"
        )
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::Duration;

    static TEMP_FILE_ID: AtomicU64 = AtomicU64::new(0);

    #[test]
    fn groups_scene_names_by_prefix() {
        assert_eq!(scene_group("dolpic0"), "dolpic");
        assert_eq!(scene_group("dolpic_ex4"), "dolpic_ex");
        assert_eq!(scene_group("biancoBoss"), "biancoBoss");
    }

    #[test]
    fn exact_scene_archive_match_wins() {
        let archives = vec![scene("dolpic0"), scene("dolpic1"), scene("dolpic_ex0")];
        let selected = select_scene_archives(&archives, "dolpic0").unwrap();
        assert_eq!(selected.len(), 1);
        assert_eq!(selected[0].stage_id, "dolpic0");
    }

    #[test]
    fn fuzzy_scene_archive_match_rejects_ambiguous_stage_ids() {
        let archives = vec![scene("dolpic0"), scene("dolpic1"), scene("bianco0")];
        assert!(select_scene_archives(&archives, "dolpic").is_err());
    }

    #[test]
    fn common_scan_never_selects_a_retail_scene_archive() {
        let id = TEMP_FILE_ID.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "sms-formats-common-assets-{}-{id}",
            std::process::id()
        ));
        let common = root.join("files/common/shared.bmd");
        let retail = root.join("files/data/scene/dolpic0.szs");
        fs::create_dir_all(common.parent().unwrap()).unwrap();
        fs::create_dir_all(retail.parent().unwrap()).unwrap();
        fs::write(&common, b"common").unwrap();
        fs::write(&retail, b"retail").unwrap();

        let assets = scan_common_stage_assets(&root).unwrap();
        assert_eq!(assets.len(), 1);
        assert_eq!(assets[0].path, common);

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn repeated_archive_loads_reuse_the_parsed_archive() {
        let path = temporary_archive_path();
        fs::write(&path, minimal_rarc()).unwrap();

        let first = load_scene_archive(&path).unwrap();
        let second = load_scene_archive(&path).unwrap();
        assert!(Arc::ptr_eq(&first, &second));

        fs::remove_file(path).unwrap();
    }

    #[test]
    fn archive_cache_is_invalidated_when_file_length_changes() {
        let path = temporary_archive_path();
        fs::write(&path, minimal_rarc()).unwrap();
        let first = load_scene_archive(&path).unwrap();

        let mut changed = minimal_rarc();
        changed.push(0);
        fs::write(&path, changed).unwrap();
        let second = load_scene_archive(&path).unwrap();
        assert!(!Arc::ptr_eq(&first, &second));

        fs::remove_file(path).unwrap();
    }

    #[test]
    fn racing_read_is_only_cacheable_under_the_matching_stamp() {
        let path = Path::new("injected-racing-archive.arc");
        let old_source = minimal_rarc_with_tail(b'A');
        let new_source = minimal_rarc_with_tail(b'B');
        let old_stamp = test_stamp(old_source.len(), 1);
        let new_stamp = test_stamp(new_source.len(), 2);
        let mut sources = [old_source, new_source.clone()].into_iter();
        let mut stamps = [old_stamp, new_stamp, new_stamp, new_stamp].into_iter();

        let loaded = read_scene_archive_stably(
            path,
            |_| Ok(sources.next().expect("one source per read attempt")),
            |_| Ok(stamps.next().expect("two stamps per read attempt")),
        )
        .unwrap();

        assert_eq!(loaded.archive.source_bytes(), new_source);
        assert_eq!(loaded.cache_stamp, Some(new_stamp));

        let mut cache = ArchiveCache::default();
        cache.insert(
            path.to_path_buf(),
            loaded.cache_stamp.unwrap(),
            Arc::clone(&loaded.archive),
        );
        assert_eq!(cache.entries.len(), 1);
        assert_eq!(cache.entries[0].stamp, new_stamp);
        assert_eq!(cache.entries[0].archive.source_bytes(), new_source);
    }

    #[test]
    fn continuously_changing_archive_is_parsed_but_not_cached() {
        let path = Path::new("injected-unstable-archive.arc");
        let sources = [
            minimal_rarc_with_tail(b'A'),
            minimal_rarc_with_tail(b'B'),
            minimal_rarc_with_tail(b'C'),
        ];
        let expected = sources[2].clone();
        let mut sources = sources.into_iter();
        let stamps = [
            test_stamp(0x21, 1),
            test_stamp(0x21, 2),
            test_stamp(0x21, 2),
            test_stamp(0x21, 3),
            test_stamp(0x21, 3),
            test_stamp(0x21, 4),
        ];
        let mut stamps = stamps.into_iter();

        let loaded = read_scene_archive_stably(
            path,
            |_| Ok(sources.next().expect("one source per read attempt")),
            |_| Ok(stamps.next().expect("two stamps per read attempt")),
        )
        .unwrap();

        assert_eq!(loaded.archive.source_bytes(), expected);
        assert_eq!(loaded.cache_stamp, None);
    }

    fn temporary_archive_path() -> PathBuf {
        let id = TEMP_FILE_ID.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!(
            "sms-formats-stage-assets-{}-{id}.arc",
            std::process::id()
        ))
    }

    fn minimal_rarc() -> Vec<u8> {
        let mut bytes = vec![0; 0x20];
        bytes[..4].copy_from_slice(b"RARC");
        bytes[4..8].copy_from_slice(&0x20u32.to_be_bytes());
        bytes[8..12].copy_from_slice(&0x20u32.to_be_bytes());
        bytes
    }

    fn minimal_rarc_with_tail(marker: u8) -> Vec<u8> {
        let mut bytes = minimal_rarc();
        bytes.push(marker);
        bytes
    }

    fn test_stamp(len: usize, seconds: u64) -> ArchiveStamp {
        ArchiveStamp {
            len: len as u64,
            modified: SystemTime::UNIX_EPOCH + Duration::from_secs(seconds),
        }
    }

    fn scene(stage_id: &str) -> SceneArchiveInfo {
        SceneArchiveInfo {
            stage_id: stage_id.to_string(),
            group: scene_group(stage_id),
            relative_path: PathBuf::from(format!("{stage_id}.szs")),
            path: PathBuf::from(format!("{stage_id}.szs")),
            size_bytes: 0,
        }
    }
}
