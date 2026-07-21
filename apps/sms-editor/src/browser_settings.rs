use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

const BROWSER_SETTINGS_VERSION: u32 = 1;
const MAX_RECENT_ITEMS: usize = 30;
pub(super) const BROWSER_DOCK_MIN_HEIGHT: f32 = 180.0;
pub(super) const BROWSER_DOCK_DEFAULT_HEIGHT: f32 = 420.0;
const BROWSER_DOCK_SETTINGS_MAX_HEIGHT: f32 = 1_600.0;
static NEXT_TEMP_FILE: AtomicU64 = AtomicU64::new(1);

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum BrowserViewPreference {
    #[default]
    Grid,
    List,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum BrowserSortPreference {
    #[default]
    Name,
    Type,
    Source,
    Recent,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct BrowserCollectionPreference {
    #[serde(default)]
    pub(super) view: BrowserViewPreference,
    #[serde(default)]
    pub(super) sort: BrowserSortPreference,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct BrowserRecentItem {
    pub(super) id: String,
    pub(super) label: String,
    pub(super) last_used_unix: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(super) struct BrowserSettings {
    pub(super) format_version: u32,
    #[serde(default)]
    pub(super) favorites: BTreeSet<String>,
    #[serde(default)]
    pub(super) recent: Vec<BrowserRecentItem>,
    #[serde(default)]
    pub(super) last_collection: String,
    #[serde(default)]
    pub(super) collections: BTreeMap<String, BrowserCollectionPreference>,
    #[serde(default)]
    pub(super) expanded_roots: BTreeSet<String>,
    #[serde(default = "default_tree_width")]
    pub(super) tree_width: f32,
    #[serde(default = "default_dock_height")]
    pub(super) dock_height: f32,
}

impl Default for BrowserSettings {
    fn default() -> Self {
        Self {
            format_version: BROWSER_SETTINGS_VERSION,
            favorites: BTreeSet::new(),
            recent: Vec::new(),
            last_collection: "all".to_string(),
            collections: BTreeMap::new(),
            expanded_roots: ["project".to_string(), "game".to_string()]
                .into_iter()
                .collect(),
            tree_width: default_tree_width(),
            dock_height: default_dock_height(),
        }
    }
}

impl BrowserSettings {
    #[cfg_attr(test, allow(dead_code))]
    pub(super) fn load_default() -> (Self, Option<String>) {
        let Some(path) = browser_settings_path() else {
            return (
                Self::default(),
                Some(
                    "The operating system did not provide an application settings directory."
                        .to_string(),
                ),
            );
        };
        match Self::load_from(&path) {
            Ok(settings) => (settings, None),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => (Self::default(), None),
            Err(error) => (
                Self::default(),
                Some(format!(
                    "Could not load Content Browser settings from '{}': {error}",
                    path.display()
                )),
            ),
        }
    }

    pub(super) fn load_from(path: &Path) -> std::io::Result<Self> {
        let text = fs::read_to_string(path)?;
        let mut settings = toml::from_str::<Self>(&text).map_err(|error| {
            std::io::Error::new(std::io::ErrorKind::InvalidData, error.to_string())
        })?;
        if settings.format_version != BROWSER_SETTINGS_VERSION {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!(
                    "unsupported Content Browser settings version {}",
                    settings.format_version
                ),
            ));
        }
        settings.normalize();
        Ok(settings)
    }

    pub(super) fn save_default(&self) -> Result<(), String> {
        let path = browser_settings_path().ok_or_else(|| {
            "the operating system has no application settings directory".to_string()
        })?;
        self.save_to(&path)
            .map_err(|error| format!("save {}: {error}", path.display()))
    }

    pub(super) fn save_to(&self, path: &Path) -> std::io::Result<()> {
        let mut normalized = self.clone();
        normalized.normalize();
        let text = toml::to_string_pretty(&normalized).map_err(|error| {
            std::io::Error::new(std::io::ErrorKind::InvalidData, error.to_string())
        })?;
        write_atomic(path, text.as_bytes())
    }

    pub(super) fn toggle_favorite(&mut self, id: &str) -> bool {
        if self.favorites.remove(id) {
            false
        } else {
            self.favorites.insert(id.to_string());
            true
        }
    }

    pub(super) fn touch_recent(&mut self, id: &str, label: &str) {
        self.recent.retain(|entry| entry.id != id);
        self.recent.insert(
            0,
            BrowserRecentItem {
                id: id.to_string(),
                label: label.to_string(),
                last_used_unix: SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs(),
            },
        );
        self.recent.truncate(MAX_RECENT_ITEMS);
    }

    fn normalize(&mut self) {
        self.format_version = BROWSER_SETTINGS_VERSION;
        if !self.tree_width.is_finite() {
            self.tree_width = default_tree_width();
        }
        self.tree_width = self.tree_width.clamp(150.0, 360.0);
        self.recent.retain(|entry| !entry.id.trim().is_empty());
        if !self.dock_height.is_finite() {
            self.dock_height = default_dock_height();
        }
        self.dock_height = self
            .dock_height
            .clamp(BROWSER_DOCK_MIN_HEIGHT, BROWSER_DOCK_SETTINGS_MAX_HEIGHT);
        let mut seen = BTreeSet::new();
        self.recent.retain(|entry| seen.insert(entry.id.clone()));
        self.recent.truncate(MAX_RECENT_ITEMS);
        self.favorites.retain(|id| !id.trim().is_empty());
        self.collections.retain(|key, _| !key.trim().is_empty());
        self.expanded_roots.retain(|key| !key.trim().is_empty());
        if self.last_collection.trim().is_empty() {
            self.last_collection = "all".to_string();
        }
    }
}

fn default_tree_width() -> f32 {
    208.0
}

fn default_dock_height() -> f32 {
    BROWSER_DOCK_DEFAULT_HEIGHT
}

pub(super) fn browser_settings_path() -> Option<PathBuf> {
    #[cfg(target_os = "windows")]
    {
        std::env::var_os("APPDATA")
            .map(PathBuf::from)
            .map(|root| root.join("Graffito-Editor").join("content-browser.toml"))
    }
    #[cfg(target_os = "macos")]
    {
        std::env::var_os("HOME").map(PathBuf::from).map(|root| {
            root.join("Library")
                .join("Application Support")
                .join("Graffito-Editor")
                .join("content-browser.toml")
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
            .map(|root| root.join("graffito-editor").join("content-browser.toml"))
    }
}

pub(super) fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let parent = path.parent().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("{} has no parent directory", path.display()),
        )
    })?;
    fs::create_dir_all(parent)?;
    let serial = NEXT_TEMP_FILE.fetch_add(1, Ordering::Relaxed);
    let temp = parent.join(format!(
        ".{}.{}.{}.tmp",
        path.file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("content-browser"),
        std::process::id(),
        serial
    ));
    let result = (|| {
        let mut file = fs::File::create(&temp)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        drop(file);
        replace_file(&temp, path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    result
}

#[cfg(windows)]
fn replace_file(source: &Path, destination: &Path) -> std::io::Result<()> {
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
    // SAFETY: Both buffers are NUL-terminated UTF-16 paths and remain alive
    // for the call. Source and destination share a parent directory.
    let moved = unsafe {
        MoveFileExW(
            source.as_ptr(),
            destination.as_ptr(),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    };
    if moved == 0 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(())
    }
}

#[cfg(not(windows))]
fn replace_file(source: &Path, destination: &Path) -> std::io::Result<()> {
    fs::rename(source, destination)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_round_trip_and_normalize_personal_state() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("content-browser.toml");
        let mut settings = BrowserSettings {
            tree_width: 900.0,
            dock_height: 512.0,
            ..BrowserSettings::default()
        };
        settings.last_collection = "game-files/textures".to_string();
        settings.expanded_roots.insert("favorites".to_string());
        settings.toggle_favorite("game:stage:bianco0");
        settings.touch_recent("game:stage:bianco0", "Bianco Hills");
        settings.touch_recent("game:stage:bianco0", "Bianco Hills Updated");
        settings.collections.insert(
            "game-files".to_string(),
            BrowserCollectionPreference {
                view: BrowserViewPreference::List,
                sort: BrowserSortPreference::Type,
            },
        );

        settings.save_to(&path).unwrap();
        let loaded = BrowserSettings::load_from(&path).unwrap();
        assert_eq!(loaded.tree_width, 360.0);
        assert_eq!(loaded.recent.len(), 1);
        assert_eq!(loaded.recent[0].label, "Bianco Hills Updated");
        assert!(loaded.favorites.contains("game:stage:bianco0"));
        assert_eq!(loaded.dock_height, 512.0);
        assert_eq!(loaded.last_collection, "game-files/textures");
        assert!(loaded.expanded_roots.contains("favorites"));
        assert_eq!(
            loaded.collections["game-files"].view,
            BrowserViewPreference::List
        );
    }

    #[test]
    fn corrupt_or_unknown_settings_are_rejected_without_panicking() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("content-browser.toml");
        fs::write(&path, "format_version = 99\n").unwrap();
        assert_eq!(
            BrowserSettings::load_from(&path).unwrap_err().kind(),
            std::io::ErrorKind::InvalidData
        );
        fs::write(&path, "not valid toml = [").unwrap();
        assert_eq!(
            BrowserSettings::load_from(&path).unwrap_err().kind(),
            std::io::ErrorKind::InvalidData
        );
    }

    #[test]
    fn recent_items_are_unique_and_bounded() {
        let mut settings = BrowserSettings::default();
        for index in 0..40 {
            settings.touch_recent(&format!("item-{index}"), &format!("Item {index}"));
        }
        assert_eq!(settings.recent.len(), MAX_RECENT_ITEMS);
        settings.touch_recent("item-20", "Updated 20");
        assert_eq!(settings.recent.len(), MAX_RECENT_ITEMS);
        assert_eq!(settings.recent[0].id, "item-20");
        assert_eq!(settings.recent[0].label, "Updated 20");
    }
    #[test]
    fn favorite_toggle_removes_an_existing_item() {
        let mut settings = BrowserSettings::default();
        assert!(settings.toggle_favorite("game:object:coin"));
        assert!(!settings.toggle_favorite("game:object:coin"));
        assert!(settings.favorites.is_empty());
    }

    #[test]
    fn repeated_save_replaces_atomically_without_leaking_temporary_files() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("content-browser.toml");
        let first = BrowserSettings::default();
        first.save_to(&path).unwrap();

        let mut second = first.clone();
        second.last_collection = "game-objects".to_string();
        second.save_to(&path).unwrap();

        assert_eq!(
            BrowserSettings::load_from(&path).unwrap().last_collection,
            "game-objects"
        );
        let leftovers = fs::read_dir(root.path())
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| entry.path() != path)
            .collect::<Vec<_>>();
        assert!(leftovers.is_empty(), "leftover files: {leftovers:?}");
    }

    #[test]
    fn normalize_recovers_non_finite_layout_sizes() {
        let mut settings = BrowserSettings {
            tree_width: f32::NAN,
            dock_height: f32::INFINITY,
            ..BrowserSettings::default()
        };
        settings.normalize();
        assert_eq!(settings.tree_width, default_tree_width());
        assert_eq!(settings.dock_height, default_dock_height());
    }

    #[test]
    fn normalize_clamps_dock_height() {
        let mut settings = BrowserSettings {
            dock_height: 8.0,
            ..BrowserSettings::default()
        };
        settings.normalize();
        assert_eq!(settings.dock_height, BROWSER_DOCK_MIN_HEIGHT);

        settings.dock_height = 20_000.0;
        settings.normalize();
        assert_eq!(settings.dock_height, BROWSER_DOCK_SETTINGS_MAX_HEIGHT);
    }
}
