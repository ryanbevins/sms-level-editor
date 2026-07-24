use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use walkdir::WalkDir;

use crate::{Result, SchemaError};

const SOURCE_EXTENSIONS: &[&str] = &["c", "cpp", "h", "hpp"];

/// A deterministic, in-memory view of the decompilation sources used during a
/// single schema generation pass.
///
/// Building this once avoids rereading the same source files for each domain
/// extractor. Paths are normalized to forward slashes and stored in a
/// `BTreeMap`, so extractor order does not depend on filesystem enumeration.
#[derive(Debug)]
pub(crate) struct SourceInventory {
    repo_root: PathBuf,
    files: BTreeMap<String, SourceFile>,
}

#[derive(Debug)]
pub(crate) struct SourceFile {
    relative_path: String,
    extension: String,
    text: String,
}

impl SourceInventory {
    pub(crate) fn metadata_fingerprint(repo_root: &Path) -> Result<u64> {
        const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;

        let canonical_root =
            fs::canonicalize(repo_root).unwrap_or_else(|_| repo_root.to_path_buf());
        let mut hash = fnv1a_bytes(FNV_OFFSET, canonical_root.to_string_lossy().as_bytes());
        let mut source_count = 0_u64;

        for directory in ["src", "include"] {
            let scan_root = repo_root.join(directory);
            if !scan_root.is_dir() {
                return Err(SchemaError::MissingSource(scan_root));
            }

            for entry in WalkDir::new(&scan_root).sort_by_file_name() {
                let entry = entry.map_err(|source| SchemaError::SourceTraversal {
                    root: scan_root.clone(),
                    source,
                })?;
                if !entry.file_type().is_file() {
                    continue;
                }

                let path = entry.path();
                let Some(extension) = path.extension().and_then(|value| value.to_str()) else {
                    continue;
                };
                if !SOURCE_EXTENSIONS.contains(&extension) {
                    continue;
                }

                let relative_path = normalize_source_path(repo_root, path);
                let metadata = entry
                    .metadata()
                    .map_err(|source| SchemaError::SourceTraversal {
                        root: scan_root.clone(),
                        source,
                    })?;
                hash = fnv1a_bytes(hash, relative_path.as_bytes());
                hash = fnv1a_bytes(hash, &[0]);
                hash = fnv1a_bytes(hash, &metadata.len().to_le_bytes());
                if let Some(modified) = metadata
                    .modified()
                    .ok()
                    .and_then(|modified| modified.duration_since(UNIX_EPOCH).ok())
                {
                    hash = fnv1a_bytes(hash, &modified.as_secs().to_le_bytes());
                    hash = fnv1a_bytes(hash, &modified.subsec_nanos().to_le_bytes());
                } else {
                    hash = fnv1a_bytes(hash, &[0xff]);
                }
                source_count += 1;
            }
        }

        if source_count == 0 {
            return Err(SchemaError::EmptySourceInventory(repo_root.to_path_buf()));
        }
        Ok(fnv1a_bytes(hash, &source_count.to_le_bytes()))
    }

    pub(crate) fn build(repo_root: &Path) -> Result<Self> {
        let mut files = BTreeMap::new();

        for directory in ["src", "include"] {
            let scan_root = repo_root.join(directory);
            if !scan_root.is_dir() {
                return Err(SchemaError::MissingSource(scan_root));
            }

            for entry in WalkDir::new(&scan_root).sort_by_file_name() {
                let entry = entry.map_err(|source| SchemaError::SourceTraversal {
                    root: scan_root.clone(),
                    source,
                })?;
                if !entry.file_type().is_file() {
                    continue;
                }

                let path = entry.path();
                let Some(extension) = path.extension().and_then(|value| value.to_str()) else {
                    continue;
                };
                if !SOURCE_EXTENSIONS.contains(&extension) {
                    continue;
                }

                let relative_path = normalize_source_path(repo_root, path);
                let text = fs::read_to_string(path).map_err(|source| SchemaError::SourceRead {
                    path: path.to_path_buf(),
                    source,
                })?;
                files.insert(
                    relative_path.clone(),
                    SourceFile {
                        relative_path,
                        extension: extension.to_string(),
                        text,
                    },
                );
            }
        }

        if files.is_empty() {
            return Err(SchemaError::EmptySourceInventory(repo_root.to_path_buf()));
        }

        Ok(Self {
            repo_root: repo_root.to_path_buf(),
            files,
        })
    }

    pub(crate) fn required(&self, relative_path: &str) -> Result<&SourceFile> {
        self.files
            .get(relative_path)
            .ok_or_else(|| SchemaError::MissingSource(self.repo_root.join(relative_path)))
    }

    pub(crate) fn files(&self) -> impl Iterator<Item = &SourceFile> {
        self.files.values()
    }

    pub(crate) fn files_under_any<'a>(
        &'a self,
        prefixes: &'a [&str],
    ) -> impl Iterator<Item = &'a SourceFile> + 'a {
        self.files.values().filter(move |file| {
            prefixes
                .iter()
                .any(|prefix| file.relative_path.starts_with(prefix))
        })
    }
}

fn fnv1a_bytes(mut hash: u64, bytes: &[u8]) -> u64 {
    const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;
    for byte in bytes {
        hash = (hash ^ u64::from(*byte)).wrapping_mul(FNV_PRIME);
    }
    hash
}

impl SourceFile {
    pub(crate) fn relative_path(&self) -> &str {
        &self.relative_path
    }

    pub(crate) fn extension(&self) -> &str {
        &self.extension
    }

    pub(crate) fn text(&self) -> &str {
        &self.text
    }
}

fn normalize_source_path(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::*;

    static NEXT_FIXTURE_ID: AtomicU64 = AtomicU64::new(0);

    #[test]
    fn inventory_is_sorted_and_filters_non_source_files() {
        let root = fixture_root("sorted");
        fs::create_dir_all(root.join("src/Zed")).unwrap();
        fs::create_dir_all(root.join("include/Alpha")).unwrap();
        fs::write(root.join("src/Zed/z.cpp"), "z").unwrap();
        fs::write(root.join("include/Alpha/a.hpp"), "a").unwrap();
        fs::write(root.join("src/ignored.txt"), "ignored").unwrap();

        let inventory = SourceInventory::build(&root).unwrap();
        let paths = inventory
            .files()
            .map(|file| file.relative_path())
            .collect::<Vec<_>>();

        assert_eq!(paths, ["include/Alpha/a.hpp", "src/Zed/z.cpp"]);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn missing_inventory_root_is_an_explicit_error() {
        let root = fixture_root("missing-root");
        fs::create_dir_all(root.join("src")).unwrap();

        let error = SourceInventory::build(&root).unwrap_err();
        assert!(matches!(error, SchemaError::MissingSource(path) if path.ends_with("include")));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn metadata_fingerprint_changes_with_source_contents_but_ignores_other_files() {
        let root = fixture_root("fingerprint");
        fs::create_dir_all(root.join("src")).unwrap();
        fs::create_dir_all(root.join("include")).unwrap();
        let source = root.join("src/example.cpp");
        fs::write(&source, "first").unwrap();

        let initial = SourceInventory::metadata_fingerprint(&root).unwrap();
        fs::write(root.join("src/notes.txt"), "not schema input").unwrap();
        assert_eq!(
            SourceInventory::metadata_fingerprint(&root).unwrap(),
            initial
        );

        fs::write(&source, "second version").unwrap();
        assert_ne!(
            SourceInventory::metadata_fingerprint(&root).unwrap(),
            initial
        );
        fs::remove_dir_all(root).unwrap();
    }

    fn fixture_root(label: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "sms-schema-{label}-{}-{}",
            std::process::id(),
            NEXT_FIXTURE_ID.fetch_add(1, Ordering::Relaxed)
        ))
    }
}
