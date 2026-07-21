use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::thread;
use std::time::{Duration, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use sms_formats::{
    index_game_files_with_events, GameFileId, GameFileIndex, GameFileIndexEvent, GameFileMetadata,
    GameFileWarning, GameResourceKind,
};

use super::*;

const CACHE_FORMAT_VERSION: u32 = 1;
const INDEX_BATCH_SIZE: usize = 256;
const MAX_BATCHES_PER_FRAME: usize = 48;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) enum GameContentIndexPhase {
    #[default]
    Idle,
    Loading,
    Ready {
        from_cache: bool,
    },
    Failed,
}

pub(super) struct GameContentIndexState {
    pub(super) generation: u64,
    pub(super) revision: u64,
    pub(super) root_identity: String,
    pub(super) entries: Vec<GameFileMetadata>,
    pub(super) warnings: Vec<GameFileWarning>,
    pub(super) by_id: BTreeMap<GameFileId, usize>,
    pub(super) by_stable_id: BTreeMap<String, usize>,
    pub(super) by_physical_path: BTreeMap<PathBuf, Vec<usize>>,
    pub(super) kind_counts: BTreeMap<GameResourceKind, usize>,
    pub(super) phase: GameContentIndexPhase,
    pub(super) error: Option<String>,
    receiver: Option<Receiver<GameContentIndexMessage>>,
}

impl Default for GameContentIndexState {
    fn default() -> Self {
        Self {
            generation: 0,
            revision: 0,
            root_identity: String::new(),
            entries: Vec::new(),
            warnings: Vec::new(),
            by_id: BTreeMap::new(),
            by_stable_id: BTreeMap::new(),
            by_physical_path: BTreeMap::new(),
            kind_counts: BTreeMap::new(),
            phase: GameContentIndexPhase::Idle,
            error: None,
            receiver: None,
        }
    }
}

#[derive(Debug)]
enum GameContentIndexMessage {
    Batch {
        generation: u64,
        entries: Vec<GameFileMetadata>,
        warnings: Vec<GameFileWarning>,
    },
    Finished {
        generation: u64,
        result: Result<(GameFileIndex, bool), String>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct PhysicalStamp {
    relative_path: PathBuf,
    size_bytes: u64,
    modified_unix_nanos: Option<u128>,
}

#[derive(Debug, Serialize, Deserialize)]
struct CachedGameFileIndex {
    format_version: u32,
    root_identity: String,
    physical_inventory: Vec<PhysicalStamp>,
    index: GameFileIndex,
}

impl GameContentIndexState {
    fn reset_for_root(&mut self, root_identity: String) -> u64 {
        self.generation = self.generation.wrapping_add(1);
        self.revision = self.revision.wrapping_add(1);
        self.root_identity = root_identity;
        self.entries.clear();
        self.warnings.clear();
        self.by_id.clear();
        self.by_stable_id.clear();
        self.by_physical_path.clear();
        self.kind_counts.clear();
        self.error = None;
        self.receiver = None;
        self.phase = GameContentIndexPhase::Idle;
        self.generation
    }

    fn rebuild_lookup(&mut self) {
        self.by_id = self
            .entries
            .iter()
            .enumerate()
            .map(|(index, entry)| (entry.id.clone(), index))
            .collect();
        self.by_stable_id = self
            .entries
            .iter()
            .enumerate()
            .map(|(index, entry)| (raw_game_file_id(&entry.id), index))
            .collect();
        self.by_physical_path.clear();
        self.kind_counts.clear();
        for (index, entry) in self.entries.iter().enumerate() {
            *self.kind_counts.entry(entry.kind).or_default() += 1;
            self.by_physical_path
                .entry(entry.physical_relative_path.clone())
                .or_default()
                .push(index);
        }
    }

    fn apply_message(&mut self, message: GameContentIndexMessage) -> bool {
        let generation = match &message {
            GameContentIndexMessage::Batch { generation, .. }
            | GameContentIndexMessage::Finished { generation, .. } => *generation,
        };
        if generation != self.generation {
            return false;
        }

        match message {
            GameContentIndexMessage::Batch {
                entries, warnings, ..
            } => {
                for entry in entries {
                    *self.kind_counts.entry(entry.kind).or_default() += 1;
                    self.by_physical_path
                        .entry(entry.physical_relative_path.clone())
                        .or_default()
                        .push(self.entries.len());
                    self.by_stable_id
                        .insert(raw_game_file_id(&entry.id), self.entries.len());
                    self.by_id.insert(entry.id.clone(), self.entries.len());
                    self.entries.push(entry);
                }
                self.warnings.extend(warnings);
                self.revision = self.revision.wrapping_add(1);
                false
            }
            GameContentIndexMessage::Finished { result, .. } => {
                match result {
                    Ok((index, from_cache)) => {
                        self.entries = index.entries;
                        self.warnings = index.warnings;
                        self.rebuild_lookup();
                        self.phase = GameContentIndexPhase::Ready { from_cache };
                        self.error = None;
                    }
                    Err(error) => {
                        self.phase = GameContentIndexPhase::Failed;
                        self.error = Some(error);
                    }
                }
                self.revision = self.revision.wrapping_add(1);
                true
            }
        }
    }
}

impl SmsEditorApp {
    pub(super) fn sync_game_content_index(&mut self) {
        let root = self
            .current_project
            .as_ref()
            .map(|project| project.descriptor.base_game_root.clone())
            .or_else(|| {
                let value = self.base_root.trim();
                (!value.is_empty()).then(|| PathBuf::from(value))
            });
        let root = (!self.show_project_hub).then_some(root).flatten();
        let identity = root
            .as_deref()
            .map(normalized_base_root_identity)
            .unwrap_or_default();
        if identity == self.game_content_index.root_identity {
            return;
        }

        let generation = self.game_content_index.reset_for_root(identity.clone());
        let Some(root) = root else {
            return;
        };

        let (sender, receiver) = mpsc::channel();
        self.game_content_index.receiver = Some(receiver);
        self.game_content_index.phase = GameContentIndexPhase::Loading;
        thread::spawn(move || {
            build_game_content_index(root, identity, generation, sender);
        });
    }

    pub(super) fn refresh_game_content_index(&mut self) {
        let identity = self.game_content_index.root_identity.clone();
        if identity.is_empty() {
            return;
        }
        self.game_content_index.root_identity.clear();
        self.sync_game_content_index();
    }

    pub(super) fn poll_game_content_index(&mut self, ctx: &egui::Context) {
        let Some(receiver) = self.game_content_index.receiver.take() else {
            return;
        };

        let mut finished = false;
        for _ in 0..MAX_BATCHES_PER_FRAME {
            match receiver.try_recv() {
                Ok(message) => {
                    finished |= self.game_content_index.apply_message(message);
                    if finished {
                        break;
                    }
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    if self.game_content_index.phase == GameContentIndexPhase::Loading {
                        self.game_content_index.phase = GameContentIndexPhase::Failed;
                        self.game_content_index.error = Some(
                            "The Game Files indexing worker stopped unexpectedly.".to_string(),
                        );
                        self.game_content_index.revision =
                            self.game_content_index.revision.wrapping_add(1);
                    }
                    finished = true;
                    break;
                }
            }
        }

        if !finished {
            self.game_content_index.receiver = Some(receiver);
            ctx.request_repaint_after(Duration::from_millis(33));
        }
    }
}

fn build_game_content_index(
    root: PathBuf,
    root_identity: String,
    generation: u64,
    sender: mpsc::Sender<GameContentIndexMessage>,
) {
    let pre_inventory = physical_inventory(&root);
    if let Ok(inventory) = &pre_inventory {
        if let Some(index) = load_cache(&root_identity, inventory) {
            let _ = sender.send(GameContentIndexMessage::Finished {
                generation,
                result: Ok((index, true)),
            });
            return;
        }
    }

    let mut entry_batch = Vec::with_capacity(INDEX_BATCH_SIZE);
    let mut warning_batch = Vec::new();
    let event_sender = sender.clone();
    let result = index_game_files_with_events(&root, |event| {
        match event {
            GameFileIndexEvent::Entry(entry) => entry_batch.push(entry.clone()),
            GameFileIndexEvent::Warning(warning) => warning_batch.push(warning.clone()),
        }
        if entry_batch.len() + warning_batch.len() >= INDEX_BATCH_SIZE {
            let _ = event_sender.send(GameContentIndexMessage::Batch {
                generation,
                entries: std::mem::take(&mut entry_batch),
                warnings: std::mem::take(&mut warning_batch),
            });
        }
    })
    .map_err(|error| error.to_string());

    if !entry_batch.is_empty() || !warning_batch.is_empty() {
        let _ = sender.send(GameContentIndexMessage::Batch {
            generation,
            entries: entry_batch,
            warnings: warning_batch,
        });
    }

    let result = result.map(|index| {
        if let Ok(before) = pre_inventory {
            if physical_inventory(&root).as_ref() == Ok(&before) {
                let _ = save_cache(&root_identity, before, &index);
            }
        }
        (index, false)
    });
    let _ = sender.send(GameContentIndexMessage::Finished { generation, result });
}

fn cache_path(root_identity: &str) -> Option<PathBuf> {
    let base = {
        #[cfg(target_os = "windows")]
        {
            std::env::var_os("LOCALAPPDATA")
                .map(PathBuf::from)
                .map(|root| root.join("Graffito-Editor"))
                .or_else(|| {
                    browser_settings_path().and_then(|path| path.parent().map(Path::to_path_buf))
                })
        }
        #[cfg(not(target_os = "windows"))]
        {
            browser_settings_path().and_then(|path| path.parent().map(Path::to_path_buf))
        }
    }?;
    let hash = root_identity
        .as_bytes()
        .iter()
        .fold(0xcbf2_9ce4_8422_2325_u64, |hash, byte| {
            (hash ^ u64::from(*byte)).wrapping_mul(0x0000_0100_0000_01b3)
        });
    Some(base.join("cache").join(format!(
        "game-files-v{CACHE_FORMAT_VERSION}-{hash:016x}.json"
    )))
}

fn load_cache(root_identity: &str, inventory: &[PhysicalStamp]) -> Option<GameFileIndex> {
    let path = cache_path(root_identity)?;
    let bytes = fs::read(path).ok()?;
    let cache = serde_json::from_slice::<CachedGameFileIndex>(&bytes).ok()?;
    cache_matches(&cache, root_identity, inventory).then_some(cache.index)
}

fn cache_matches(
    cache: &CachedGameFileIndex,
    root_identity: &str,
    inventory: &[PhysicalStamp],
) -> bool {
    cache.format_version == CACHE_FORMAT_VERSION
        && cache.root_identity == root_identity
        && cache.physical_inventory == inventory
}

fn save_cache(
    root_identity: &str,
    inventory: Vec<PhysicalStamp>,
    index: &GameFileIndex,
) -> Result<(), String> {
    let path = cache_path(root_identity)
        .ok_or_else(|| "the operating system has no application cache directory".to_string())?;
    let bytes = serde_json::to_vec(&CachedGameFileIndex {
        format_version: CACHE_FORMAT_VERSION,
        root_identity: root_identity.to_string(),
        physical_inventory: inventory,
        index: index.clone(),
    })
    .map_err(|error| format!("serialize Game Files cache: {error}"))?;
    browser_settings::write_atomic(&path, &bytes)
        .map_err(|error| format!("save Game Files cache '{}': {error}", path.display()))
}

fn physical_inventory(base_root: &Path) -> Result<Vec<PhysicalStamp>, String> {
    let base = fs::canonicalize(base_root).map_err(|error| {
        format!(
            "open extracted game root '{}': {error}",
            base_root.display()
        )
    })?;
    let mut output = Vec::new();
    for name in ["files", "sys"] {
        let root = base.join(name);
        let metadata = fs::symlink_metadata(&root)
            .map_err(|error| format!("read extracted game {name}/ root: {error}"))?;
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            return Err(format!(
                "extracted game {name}/ root must be a physical directory"
            ));
        }
        collect_physical_stamps(&base, &root, &mut output)?;
    }
    output.sort_by(|left, right| left.relative_path.cmp(&right.relative_path));
    Ok(output)
}

fn collect_physical_stamps(
    base_root: &Path,
    directory: &Path,
    output: &mut Vec<PhysicalStamp>,
) -> Result<(), String> {
    for entry in fs::read_dir(directory)
        .map_err(|error| format!("read directory '{}': {error}", directory.display()))?
    {
        let entry = entry
            .map_err(|error| format!("read directory entry '{}': {error}", directory.display()))?;
        let file_type = entry
            .file_type()
            .map_err(|error| format!("inspect '{}': {error}", entry.path().display()))?;
        if file_type.is_symlink() {
            continue;
        }
        if file_type.is_dir() {
            collect_physical_stamps(base_root, &entry.path(), output)?;
            continue;
        }
        if !file_type.is_file() {
            continue;
        }
        let metadata = entry
            .metadata()
            .map_err(|error| format!("read metadata '{}': {error}", entry.path().display()))?;
        let relative_path = entry
            .path()
            .strip_prefix(base_root)
            .map(Path::to_path_buf)
            .map_err(|error| format!("relativize '{}': {error}", entry.path().display()))?;
        let modified_unix_nanos = metadata
            .modified()
            .ok()
            .and_then(|modified| modified.duration_since(UNIX_EPOCH).ok())
            .map(|duration| duration.as_nanos());
        output.push(PhysicalStamp {
            relative_path,
            size_bytes: metadata.len(),
            modified_unix_nanos,
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stale_worker_messages_are_discarded() {
        let mut state = GameContentIndexState {
            generation: 7,
            ..GameContentIndexState::default()
        };
        let finished = state.apply_message(GameContentIndexMessage::Finished {
            generation: 6,
            result: Err("stale".to_string()),
        });
        assert!(!finished);
        assert_eq!(state.phase, GameContentIndexPhase::Idle);
        assert!(state.error.is_none());
    }

    fn sample_metadata(path: &str) -> GameFileMetadata {
        GameFileMetadata {
            id: GameFileId::Physical {
                relative_path: path.into(),
            },
            display_path: path.to_string(),
            physical_relative_path: path.into(),
            kind: sms_formats::GameResourceKind::Other,
            size_bytes: 3,
            modified_unix_nanos: Some(5),
            archive_entry: None,
        }
    }

    #[test]
    fn stale_partial_batches_do_not_change_entries_or_revision() {
        let mut state = GameContentIndexState {
            generation: 9,
            revision: 4,
            ..GameContentIndexState::default()
        };
        let changed = state.apply_message(GameContentIndexMessage::Batch {
            generation: 8,
            entries: vec![sample_metadata("files/stale.bin")],
            warnings: Vec::new(),
        });
        assert!(!changed);
        assert!(state.entries.is_empty());
        assert!(state.by_id.is_empty());
        assert_eq!(state.revision, 4);
    }

    #[test]
    fn matching_partial_batches_update_lookup_and_revision() {
        let mut state = GameContentIndexState {
            generation: 9,
            revision: 4,
            ..GameContentIndexState::default()
        };
        let metadata = sample_metadata("files/live.bin");
        let finished = state.apply_message(GameContentIndexMessage::Batch {
            generation: 9,
            entries: vec![metadata.clone()],
            warnings: Vec::new(),
        });
        assert!(!finished);
        assert_eq!(state.entries, vec![metadata.clone()]);
        assert_eq!(state.by_id.get(&metadata.id), Some(&0));
        assert_eq!(
            state.by_stable_id.get(&raw_game_file_id(&metadata.id)),
            Some(&0)
        );
        assert_eq!(
            state.by_physical_path.get(&metadata.physical_relative_path),
            Some(&vec![0])
        );
        assert_eq!(
            state.kind_counts.get(&sms_formats::GameResourceKind::Other),
            Some(&1)
        );
        assert_eq!(state.revision, 5);
    }

    #[test]
    fn cache_validation_rejects_version_root_and_inventory_drift() {
        let inventory = vec![PhysicalStamp {
            relative_path: "files/a.arc".into(),
            size_bytes: 12,
            modified_unix_nanos: Some(34),
        }];
        let mut cache = CachedGameFileIndex {
            format_version: CACHE_FORMAT_VERSION,
            root_identity: "root-a".to_string(),
            physical_inventory: inventory.clone(),
            index: GameFileIndex {
                base_root: "root-a".into(),
                entries: vec![sample_metadata("files/a.arc")],
                warnings: Vec::new(),
            },
        };
        assert!(cache_matches(&cache, "root-a", &inventory));
        cache.format_version += 1;
        assert!(!cache_matches(&cache, "root-a", &inventory));
        cache.format_version = CACHE_FORMAT_VERSION;
        assert!(!cache_matches(&cache, "root-b", &inventory));
        let changed = vec![PhysicalStamp {
            size_bytes: 13,
            ..inventory[0].clone()
        }];
        assert!(!cache_matches(&cache, "root-a", &changed));
    }

    #[test]
    fn metadata_cache_json_round_trips_without_payload_bytes() {
        let cache = CachedGameFileIndex {
            format_version: CACHE_FORMAT_VERSION,
            root_identity: "root-a".to_string(),
            physical_inventory: vec![PhysicalStamp {
                relative_path: "files/a.bin".into(),
                size_bytes: 3,
                modified_unix_nanos: Some(u128::from(u64::MAX) + 1),
            }],
            index: GameFileIndex {
                base_root: "root-a".into(),
                entries: vec![sample_metadata("files/a.bin")],
                warnings: Vec::new(),
            },
        };
        let bytes = serde_json::to_vec(&cache).unwrap();
        let decoded: CachedGameFileIndex = serde_json::from_slice(&bytes).unwrap();
        assert!(cache_matches(&decoded, "root-a", &cache.physical_inventory));
        assert_eq!(decoded.index.entries, cache.index.entries);
    }

    #[test]
    fn physical_inventory_detects_added_changed_and_removed_files() {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir_all(root.path().join("files/a")).unwrap();
        fs::create_dir_all(root.path().join("sys")).unwrap();
        let first_path = root.path().join("files/a/one.bin");
        fs::write(&first_path, b"one").unwrap();
        let first = physical_inventory(root.path()).unwrap();

        fs::write(&first_path, b"changed bytes").unwrap();
        let changed = physical_inventory(root.path()).unwrap();
        assert_ne!(first, changed);

        let second_path = root.path().join("sys/main.dol");
        fs::write(&second_path, b"dol").unwrap();
        let added = physical_inventory(root.path()).unwrap();
        assert_ne!(changed, added);

        fs::remove_file(second_path).unwrap();
        let removed = physical_inventory(root.path()).unwrap();
        assert_eq!(changed, removed);
    }
}
