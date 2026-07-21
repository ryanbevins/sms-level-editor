use std::fs;
use std::path::{Component, Path, PathBuf};
use std::time::UNIX_EPOCH;

use serde::{Deserialize, Serialize};
use walkdir::WalkDir;

use crate::{decode_yaz0, FormatError, RarcArchive, Result};

const FORMAT: &str = "game file index";

/// Stable identity for a physical retail file or a file stored in a RARC.
///
/// Archive-entry identity deliberately uses the exact raw path bytes rather
/// than the decoded display path. Invalid or ambiguous Shift-JIS paths can
/// therefore never alias one another in browser state or a metadata cache.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum GameFileId {
    Physical {
        relative_path: PathBuf,
    },
    ArchiveEntry {
        archive_relative_path: PathBuf,
        raw_entry_path: Vec<u8>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum GameResourceKind {
    Archive,
    Model,
    MaterialTable,
    Texture,
    Animation,
    Particle,
    Collision,
    Message,
    PlacementData,
    ParameterData,
    Script,
    Audio,
    Video,
    System,
    Other,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GameArchiveEntryMetadata {
    /// Exact RARC path bytes, including `/` separators between raw names.
    pub raw_path: Vec<u8>,
    /// Shift-JIS display path. Invalid byte sequences use RARC's escaped form.
    pub display_path: String,
    pub flags: u8,
    pub uncompressed_size: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GameFileMetadata {
    pub id: GameFileId,
    /// Slash-separated path suitable for display and search. Archive entries
    /// use `physical/path.arc!/entry/path.bmd`.
    pub display_path: String,
    /// The physical file under `files/` or `sys/`. For archive entries this is
    /// the containing archive and can be joined to [`GameFileIndex::base_root`].
    pub physical_relative_path: PathBuf,
    pub kind: GameResourceKind,
    /// Physical byte length for physical files and uncompressed RARC entry
    /// length for archive entries.
    pub size_bytes: u64,
    /// Stable cache stamp copied from the containing physical file when the
    /// filesystem exposes a post-Unix-epoch modification time.
    pub modified_unix_nanos: Option<u128>,
    pub archive_entry: Option<GameArchiveEntryMetadata>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum GameFileWarningKind {
    Traversal,
    Metadata,
    ArchiveRead,
    ArchiveDecompress,
    ArchiveParse,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GameFileWarning {
    /// Physical path relative to the validated base root when one is known.
    pub relative_path: PathBuf,
    pub kind: GameFileWarningKind,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GameFileIndex {
    /// Canonicalized root that owned the validated `files/` and `sys/` trees.
    pub base_root: PathBuf,
    /// Deterministically sorted physical files and archive entries.
    pub entries: Vec<GameFileMetadata>,
    /// Non-fatal problems. A malformed archive remains in `entries` as a
    /// physical file and has one of these warnings instead of losing content.
    pub warnings: Vec<GameFileWarning>,
}

impl GameFileIndex {
    pub fn entry(&self, id: &GameFileId) -> Option<&GameFileMetadata> {
        self.entries.iter().find(|entry| &entry.id == id)
    }
}

/// Incremental observation hook for callers that build the index on a worker
/// and want to stream partial counts or records to the UI.
#[derive(Debug, Clone, Copy)]
pub enum GameFileIndexEvent<'a> {
    Entry(&'a GameFileMetadata),
    Warning(&'a GameFileWarning),
}

/// Builds the complete read-only metadata index for an extracted game root.
pub fn index_game_files(base_root: impl AsRef<Path>) -> Result<GameFileIndex> {
    index_game_files_with_events(base_root, |_| {})
}

/// Builds the complete index while reporting entries and non-fatal warnings as
/// they are discovered. Events borrow the same records retained by the result.
pub fn index_game_files_with_events<F>(
    base_root: impl AsRef<Path>,
    mut on_event: F,
) -> Result<GameFileIndex>
where
    F: for<'a> FnMut(GameFileIndexEvent<'a>),
{
    let roots = ValidatedGameRoots::new(base_root.as_ref())?;
    let mut index = GameFileIndex {
        base_root: roots.base,
        entries: Vec::new(),
        warnings: Vec::new(),
    };

    for root in [&roots.files, &roots.sys] {
        index_root(root, &mut index, &mut on_event);
    }

    index.entries.sort_by(compare_metadata);
    index.warnings.sort_by(|left, right| {
        left.relative_path
            .cmp(&right.relative_path)
            .then_with(|| left.kind.cmp(&right.kind))
            .then_with(|| left.message.cmp(&right.message))
    });
    Ok(index)
}

/// Classifies a physical resource by its final extension and known system-file
/// names. Matching is ASCII case-insensitive.
pub fn classify_game_resource_path(path: &Path) -> GameResourceKind {
    let file_name = path
        .file_name()
        .map(|name| name.to_string_lossy())
        .unwrap_or_default();
    if ascii_extension(file_name.as_bytes()).as_deref() == Some(b"arc")
        && path.components().any(|component| {
            matches!(
                component,
                Component::Normal(value) if value.to_string_lossy().eq_ignore_ascii_case("AudioRes")
            )
        })
    {
        return GameResourceKind::Audio;
    }
    classify_name(file_name.as_bytes())
}

/// Classifies an exact raw RARC path without decoding it first.
pub fn classify_game_resource_raw_path(raw_path: &[u8]) -> GameResourceKind {
    let file_name = raw_path
        .rsplit(|byte| *byte == b'/' || *byte == b'\\')
        .next()
        .unwrap_or(raw_path);
    classify_name(file_name)
}

struct ValidatedGameRoots {
    base: PathBuf,
    files: PathBuf,
    sys: PathBuf,
}

impl ValidatedGameRoots {
    fn new(base_root: &Path) -> Result<Self> {
        let base = fs::canonicalize(base_root)?;
        if !fs::metadata(&base)?.is_dir() {
            return Err(unsupported(format!(
                "base root is not a directory: {}",
                base.display()
            )));
        }

        let files = validate_content_root(&base, "files")?;
        let sys = validate_content_root(&base, "sys")?;
        Ok(Self { base, files, sys })
    }
}

fn validate_content_root(base: &Path, name: &str) -> Result<PathBuf> {
    let requested = base.join(name);
    let symlink_metadata = fs::symlink_metadata(&requested).map_err(|error| {
        FormatError::Io(std::io::Error::new(
            error.kind(),
            format!(
                "extracted game root is missing its {name}/ directory at {}: {error}",
                requested.display()
            ),
        ))
    })?;
    if symlink_metadata.file_type().is_symlink() {
        return Err(unsupported(format!(
            "refusing symlinked extracted-game root: {}",
            requested.display()
        )));
    }
    if !symlink_metadata.is_dir() {
        return Err(unsupported(format!(
            "extracted-game {name}/ root is not a directory: {}",
            requested.display()
        )));
    }

    let canonical = fs::canonicalize(&requested)?;
    if !canonical.starts_with(base) {
        return Err(unsupported(format!(
            "extracted-game {name}/ root escapes the selected base: {}",
            canonical.display()
        )));
    }
    Ok(canonical)
}

fn index_root<F>(root: &Path, index: &mut GameFileIndex, on_event: &mut F)
where
    F: for<'a> FnMut(GameFileIndexEvent<'a>),
{
    for item in WalkDir::new(root).follow_links(false).sort_by_file_name() {
        let entry = match item {
            Ok(entry) => entry,
            Err(error) => {
                let relative_path = error
                    .path()
                    .and_then(|path| path.strip_prefix(&index.base_root).ok())
                    .map(Path::to_path_buf)
                    .unwrap_or_else(|| relative_to_base(&index.base_root, root));
                push_warning(
                    index,
                    on_event,
                    GameFileWarning {
                        relative_path,
                        kind: GameFileWarningKind::Traversal,
                        message: error.to_string(),
                    },
                );
                continue;
            }
        };
        if !entry.file_type().is_file() {
            continue;
        }

        let relative_path = relative_to_base(&index.base_root, entry.path());
        let metadata = match entry.metadata() {
            Ok(metadata) => metadata,
            Err(error) => {
                push_warning(
                    index,
                    on_event,
                    GameFileWarning {
                        relative_path,
                        kind: GameFileWarningKind::Metadata,
                        message: error.to_string(),
                    },
                );
                continue;
            }
        };
        let modified_unix_nanos = metadata
            .modified()
            .ok()
            .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
            .map(|duration| duration.as_nanos());
        let physical_display = slash_display(&relative_path);
        let physical = GameFileMetadata {
            id: GameFileId::Physical {
                relative_path: relative_path.clone(),
            },
            display_path: physical_display.clone(),
            physical_relative_path: relative_path.clone(),
            kind: classify_game_resource_path(entry.path()),
            size_bytes: metadata.len(),
            modified_unix_nanos,
            archive_entry: None,
        };
        let inspect_archive = physical.kind == GameResourceKind::Archive;
        push_entry(index, on_event, physical);

        if !inspect_archive {
            continue;
        }
        let source = match fs::read(entry.path()) {
            Ok(source) => source,
            Err(error) => {
                push_warning(
                    index,
                    on_event,
                    GameFileWarning {
                        relative_path,
                        kind: GameFileWarningKind::ArchiveRead,
                        message: error.to_string(),
                    },
                );
                continue;
            }
        };
        let decoded = if source.starts_with(b"Yaz0") {
            match decode_yaz0(&source) {
                Ok(decoded) => decoded,
                Err(error) => {
                    push_warning(
                        index,
                        on_event,
                        GameFileWarning {
                            relative_path,
                            kind: GameFileWarningKind::ArchiveDecompress,
                            message: error.to_string(),
                        },
                    );
                    continue;
                }
            }
        } else {
            source
        };
        let archive = match RarcArchive::parse(decoded) {
            Ok(archive) => archive,
            Err(error) => {
                push_warning(
                    index,
                    on_event,
                    GameFileWarning {
                        relative_path,
                        kind: GameFileWarningKind::ArchiveParse,
                        message: error.to_string(),
                    },
                );
                continue;
            }
        };
        for archive_entry in archive.file_entries() {
            let entry_metadata = GameArchiveEntryMetadata {
                raw_path: archive_entry.raw_path.clone(),
                display_path: archive_entry.path.clone(),
                flags: archive_entry.flags,
                uncompressed_size: u64::from(archive_entry.size),
            };
            let record = GameFileMetadata {
                id: GameFileId::ArchiveEntry {
                    archive_relative_path: relative_path.clone(),
                    raw_entry_path: archive_entry.raw_path.clone(),
                },
                display_path: format!("{physical_display}!/{}", archive_entry.path),
                physical_relative_path: relative_path.clone(),
                kind: classify_game_resource_raw_path(&archive_entry.raw_path),
                size_bytes: u64::from(archive_entry.size),
                modified_unix_nanos,
                archive_entry: Some(entry_metadata),
            };
            push_entry(index, on_event, record);
        }
    }
}

fn push_entry<F>(index: &mut GameFileIndex, on_event: &mut F, entry: GameFileMetadata)
where
    F: for<'a> FnMut(GameFileIndexEvent<'a>),
{
    on_event(GameFileIndexEvent::Entry(&entry));
    index.entries.push(entry);
}

fn push_warning<F>(index: &mut GameFileIndex, on_event: &mut F, warning: GameFileWarning)
where
    F: for<'a> FnMut(GameFileIndexEvent<'a>),
{
    on_event(GameFileIndexEvent::Warning(&warning));
    index.warnings.push(warning);
}

fn compare_metadata(left: &GameFileMetadata, right: &GameFileMetadata) -> std::cmp::Ordering {
    left.physical_relative_path
        .cmp(&right.physical_relative_path)
        .then_with(|| {
            left.archive_entry
                .is_some()
                .cmp(&right.archive_entry.is_some())
        })
        .then_with(|| match (&left.archive_entry, &right.archive_entry) {
            (Some(left), Some(right)) => left.raw_path.cmp(&right.raw_path),
            _ => std::cmp::Ordering::Equal,
        })
}

fn relative_to_base(base: &Path, path: &Path) -> PathBuf {
    path.strip_prefix(base)
        .map(Path::to_path_buf)
        .unwrap_or_else(|_| path.to_path_buf())
}

fn slash_display(path: &Path) -> String {
    path.components()
        .filter_map(|component| match component {
            Component::Normal(value) => Some(value.to_string_lossy().into_owned()),
            Component::ParentDir => Some("..".to_string()),
            Component::CurDir => None,
            Component::RootDir | Component::Prefix(_) => None,
        })
        .collect::<Vec<_>>()
        .join("/")
}

fn classify_name(file_name: &[u8]) -> GameResourceKind {
    let lower_name = ascii_lower(file_name);
    if matches!(
        lower_name.as_slice(),
        b"boot.bin" | b"bi2.bin" | b"apploader.img" | b"main.dol" | b"fst.bin" | b"opening.bnr"
    ) {
        return GameResourceKind::System;
    }

    match ascii_extension(&lower_name).as_deref() {
        Some(b"arc" | b"szs") => GameResourceKind::Archive,
        Some(b"bmd" | b"bdl") => GameResourceKind::Model,
        Some(b"bmt") => GameResourceKind::MaterialTable,
        Some(b"bti" | b"bmp" | b"tpl") => GameResourceKind::Texture,
        Some(
            b"bck" | b"bca" | b"btk" | b"brk" | b"bpk" | b"btp" | b"bva" | b"bla" | b"bxa" | b"bas",
        ) => GameResourceKind::Animation,
        Some(b"jpa" | b"jpc") => GameResourceKind::Particle,
        Some(b"col") => GameResourceKind::Collision,
        Some(b"bmg") => GameResourceKind::Message,
        Some(b"map" | b"scene" | b"ral" | b"ymp" | b"me" | b"sb") => {
            GameResourceKind::PlacementData
        }
        Some(b"prm") => GameResourceKind::ParameterData,
        Some(b"lua" | b"js" | b"py" | b"sh" | b"bat" | b"cmd") => GameResourceKind::Script,
        Some(
            b"aaf" | b"aw" | b"afc" | b"baa" | b"bms" | b"bst" | b"bstn" | b"ws" | b"bnk" | b"dsp"
            | b"adp" | b"ast" | b"hps" | b"wav" | b"mp3" | b"ogg" | b"aac",
        ) => GameResourceKind::Audio,
        Some(b"thp" | b"mth" | b"mp4") => GameResourceKind::Video,
        Some(b"dol" | b"rel" | b"rso" | b"elf" | b"img" | b"bnr") => GameResourceKind::System,
        // Sunshine's generic `.bin` files include placement and parameter
        // tables. Keep them together with authoring data rather than claiming
        // a narrower semantic type from the extension alone.
        Some(b"bin") => GameResourceKind::PlacementData,
        _ => GameResourceKind::Other,
    }
}

fn ascii_extension(file_name: &[u8]) -> Option<Vec<u8>> {
    let dot = file_name.iter().rposition(|byte| *byte == b'.')?;
    let extension = file_name.get(dot + 1..)?;
    (!extension.is_empty()).then(|| ascii_lower(extension))
}

fn ascii_lower(bytes: &[u8]) -> Vec<u8> {
    bytes.iter().map(u8::to_ascii_lowercase).collect()
}

fn unsupported(message: String) -> FormatError {
    FormatError::Unsupported {
        format: FORMAT,
        message,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::RarcBuilder;
    use std::sync::atomic::{AtomicU64, Ordering};

    static TEMP_ROOT_ID: AtomicU64 = AtomicU64::new(0);

    struct TestRoot(PathBuf);

    impl TestRoot {
        fn new() -> Self {
            let id = TEMP_ROOT_ID.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "sms-formats-game-index-{}-{id}",
                std::process::id()
            ));
            fs::create_dir_all(path.join("files/data")).unwrap();
            fs::create_dir_all(path.join("sys")).unwrap();
            Self(path)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TestRoot {
        fn drop(&mut self) {
            fs::remove_dir_all(&self.0).unwrap();
        }
    }

    #[test]
    fn indexes_only_validated_files_and_sys_trees() {
        let root = TestRoot::new();
        fs::write(root.path().join("files/data/music.bms"), b"music").unwrap();
        fs::create_dir_all(root.path().join("files/AudioRes/Seqs")).unwrap();
        fs::write(
            root.path().join("files/AudioRes/Seqs/sequence.arc"),
            b"j-audio sequence archive",
        )
        .unwrap();
        fs::write(root.path().join("files/movie.thp"), b"movie").unwrap();
        fs::write(root.path().join("sys/main.dol"), b"dol").unwrap();
        fs::write(root.path().join("outside.prm"), b"ignored").unwrap();

        let index = index_game_files(root.path()).unwrap();
        let paths = index
            .entries
            .iter()
            .map(|entry| (entry.display_path.as_str(), entry.kind))
            .collect::<Vec<_>>();
        assert_eq!(
            paths,
            vec![
                ("files/AudioRes/Seqs/sequence.arc", GameResourceKind::Audio,),
                ("files/data/music.bms", GameResourceKind::Audio),
                ("files/movie.thp", GameResourceKind::Video),
                ("sys/main.dol", GameResourceKind::System),
            ]
        );
        assert!(index.warnings.is_empty());
    }

    #[test]
    fn raw_archive_paths_remain_exact_stable_identity() {
        let root = TestRoot::new();
        let archive_path = root.path().join("files/data/raw.arc");
        let invalid_name = [0x81, b'.', b'b', b't', b'i'];
        let escaped_name = b"%81.bti";
        let mut builder = RarcBuilder::new_scene();
        builder.insert_file(&invalid_name, vec![1]).unwrap();
        builder.insert_file(escaped_name, vec![2]).unwrap();
        fs::write(&archive_path, builder.build().unwrap().to_bytes().unwrap()).unwrap();

        let index = index_game_files(root.path()).unwrap();
        let entries = index
            .entries
            .iter()
            .filter(|entry| entry.archive_entry.is_some())
            .collect::<Vec<_>>();
        assert_eq!(entries.len(), 2);
        assert_ne!(entries[0].display_path, entries[1].display_path);
        assert_ne!(entries[0].id, entries[1].id);
        assert!(entries.iter().any(|entry| matches!(
            &entry.id,
            GameFileId::ArchiveEntry { raw_entry_path, .. } if raw_entry_path == &invalid_name
        )));
        assert!(entries.iter().all(|entry| {
            entry.kind == GameResourceKind::Texture
                && entry.archive_entry.as_ref().unwrap().flags == 0x11
                && entry.archive_entry.as_ref().unwrap().uncompressed_size == 1
        }));

        let serialized = serde_json::to_vec(&entries[0].id).unwrap();
        let restored: GameFileId = serde_json::from_slice(&serialized).unwrap();
        assert_eq!(restored, entries[0].id);
    }

    #[test]
    fn indexes_yaz0_wrapped_szs_entries_and_streams_events() {
        let root = TestRoot::new();
        let archive_path = root.path().join("files/data/wrapped.szs");
        let mut builder = RarcBuilder::new_scene();
        builder
            .insert_file(b"map/map/model.bmd", vec![1, 2, 3, 4])
            .unwrap();
        let rarc = builder.build().unwrap().to_bytes().unwrap();
        fs::write(&archive_path, crate::encode_yaz0(&rarc).unwrap()).unwrap();

        let mut entry_events = 0;
        let mut warning_events = 0;
        let index = index_game_files_with_events(root.path(), |event| match event {
            GameFileIndexEvent::Entry(_) => entry_events += 1,
            GameFileIndexEvent::Warning(_) => warning_events += 1,
        })
        .unwrap();

        assert_eq!(entry_events, 2);
        assert_eq!(warning_events, 0);
        assert_eq!(index.entries.len(), 2);
        assert_eq!(index.entries[0].kind, GameResourceKind::Archive);
        assert_eq!(index.entries[1].kind, GameResourceKind::Model);
        assert_eq!(index.entries[1].size_bytes, 4);
        assert_eq!(
            index.entries[1]
                .archive_entry
                .as_ref()
                .unwrap()
                .uncompressed_size,
            4
        );
    }

    #[test]
    fn corrupt_archive_remains_a_physical_entry_with_a_warning() {
        let root = TestRoot::new();
        fs::write(root.path().join("files/data/broken.szs"), b"not an archive").unwrap();

        let index = index_game_files(root.path()).unwrap();
        assert_eq!(index.entries.len(), 1);
        assert_eq!(index.entries[0].kind, GameResourceKind::Archive);
        assert!(matches!(index.entries[0].id, GameFileId::Physical { .. }));
        assert_eq!(index.warnings.len(), 1);
        assert_eq!(index.warnings[0].kind, GameFileWarningKind::ArchiveParse);
        assert_eq!(
            index.warnings[0].relative_path,
            PathBuf::from("files/data/broken.szs")
        );
    }

    #[test]
    fn repeated_indexes_have_identical_sorted_metadata() {
        let root = TestRoot::new();
        fs::write(root.path().join("sys/boot.bin"), b"boot").unwrap();
        fs::write(root.path().join("files/data/z.prm"), b"z").unwrap();
        fs::write(root.path().join("files/data/a.lua"), b"a").unwrap();

        let first = index_game_files(root.path()).unwrap();
        let second = index_game_files(root.path()).unwrap();
        assert_eq!(first, second);
        assert_eq!(first.entries[0].kind, GameResourceKind::Script);
        assert_eq!(first.entries[1].kind, GameResourceKind::ParameterData);
    }

    #[test]
    fn both_retail_roots_are_required() {
        let id = TEMP_ROOT_ID.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "sms-formats-invalid-game-index-{}-{id}",
            std::process::id()
        ));
        fs::create_dir_all(root.join("files")).unwrap();

        let error = index_game_files(&root).unwrap_err();
        assert!(error.to_string().contains("sys/ directory"));

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    #[ignore = "requires SMS_BASE_ROOT with a complete extracted retail filesystem"]
    fn indexes_an_external_retail_filesystem() {
        let base_root = std::env::var_os("SMS_BASE_ROOT").expect("set SMS_BASE_ROOT");
        let index = index_game_files(base_root).unwrap();
        let physical_count = index
            .entries
            .iter()
            .filter(|entry| entry.archive_entry.is_none())
            .count();
        let archive_entry_count = index.entries.len() - physical_count;

        eprintln!(
            "indexed {physical_count} physical files, {archive_entry_count} archive entries, and {} warning(s)",
            index.warnings.len()
        );
        for warning in &index.warnings {
            eprintln!(
                "warning [{}]: {}",
                warning.relative_path.display(),
                warning.message
            );
        }
        assert!(physical_count > 100);
        assert!(archive_entry_count > 1_000);
        assert!(index.entries.iter().any(|entry| {
            entry.physical_relative_path.starts_with("sys")
                && entry.kind == GameResourceKind::System
        }));
    }

    #[test]
    fn classifies_browser_resource_families() {
        assert_eq!(
            classify_game_resource_raw_path(b"map/map.bmd"),
            GameResourceKind::Model
        );
        assert_eq!(
            classify_game_resource_raw_path(b"params/object.prm"),
            GameResourceKind::ParameterData
        );
        assert_eq!(
            classify_game_resource_raw_path(b"script/boot.LUA"),
            GameResourceKind::Script
        );
        assert_eq!(
            classify_game_resource_path(Path::new("sys/MAIN.DOL")),
            GameResourceKind::System
        );
    }
}
