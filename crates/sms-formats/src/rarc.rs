use encoding_rs::SHIFT_JIS;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::binary::{be_u16, be_u32, checked_slice, require_len, require_magic};
use crate::{FormatError, PreserveBytes, Result};

const FORMAT: &str = "RARC";
const MAX_TABLE_BYTES: usize = 256 * 1024 * 1024;
const MAX_ARCHIVE_NODE_COUNT: usize = 1_048_576;
const MAX_ARCHIVE_ENTRY_COUNT: usize = 1_048_576;
const MAX_ARCHIVE_DIRECTORY_DEPTH: usize = 1_024;
const MAX_ARCHIVE_PATH_BYTES: usize = 16 * 1024;
const MAX_INDEXED_PATH_BYTES: usize = 256 * 1024 * 1024;
const RARC_ALIGNMENT: usize = 0x20;
const RARC_HEADER_SIZE: u32 = 0x20;
const RARC_FILE_FLAGS: u8 = 0x11;
const RARC_DIRECTORY_FLAGS: u8 = 0x02;
const RARC_DIRECTORY_SIZE: u32 = 0x10;
const RARC_SYNC_FILE_IDS: u8 = 1;
const RARC_PADDING_BYTE: u8 = 0;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RarcHeader {
    pub file_size: u32,
    pub header_size: u32,
    pub data_offset: u32,
    pub data_size: u32,
}

/// Complete structural representation of a RARC archive.
///
/// This document stores ordered records, names, layout decisions, and the
/// individual file payloads, but never stores the original archive buffer.
/// Imported child payloads are an ingestion handoff: a fully semantic stage
/// importer must take, decode, and replace each one before final export.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RarcDocument {
    pub layout: RarcLayout,
    pub nodes: Vec<RarcNodeRecord>,
    pub entries: Vec<RarcEntryRecord>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RarcLayout {
    /// Derived output size. Recomputed before every export.
    pub file_size: u32,
    /// Canonical creator constant (`0x20`).
    pub header_size: u32,
    /// Derived data offset, relative to the end of the fixed header.
    pub data_offset: u32,
    /// Derived aligned payload-section size.
    pub data_size: u32,
    pub mram_data_size: u32,
    pub aram_data_size: u32,
    pub dvd_data_size: u32,
    pub metadata_present: bool,
    /// The remaining offsets are derived relative to the metadata/info block.
    pub node_offset: u32,
    pub entry_offset: u32,
    pub string_table_offset: u32,
    pub string_table_size: u32,
    pub next_free_file_id: u16,
    pub sync_file_ids: u8,
    /// Canonical creator constant; strict imports require all zeroes.
    pub info_reserved: [u8; 5],
    /// Canonical creator alignment (`0x20`).
    pub alignment: u32,
    /// Canonical creator fill (`0`).
    pub padding_byte: u8,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RarcNodeRecord {
    pub node_type: [u8; 4],
    pub name_offset: u32,
    pub name_hash: u16,
    pub raw_name: Vec<u8>,
    pub entry_count: u16,
    pub first_entry_index: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RarcEntryRecord {
    pub file_id: u16,
    pub name_hash: u16,
    pub flags: u8,
    pub name_offset: u32,
    pub raw_name: Vec<u8>,
    /// A node index for directories and a data-section offset for files.
    pub data_offset: u32,
    pub size: u32,
    pub reserved: u32,
    /// Present only for files. On import this is the child-resource ingestion
    /// handoff, not a slice or cache of the source archive. Strict callers take
    /// it and later install independently generated child bytes.
    pub data: Option<Vec<u8>>,
}

/// High-level, source-free RARC tree builder.
///
/// Directories are inferred from slash-separated raw paths. The builder owns
/// every file payload and regenerates node records, dot entries, hashes, file
/// IDs, string slots, and aligned data offsets when [`RarcBuilder::build`] is
/// called. This is intentionally separate from [`RarcDocument`], whose public
/// record representation is also used by detached semantic stage documents
/// after their child payloads have been consumed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RarcBuilder {
    root: RarcBuilderDirectory,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RarcBuilderDirectory {
    raw_name: Vec<u8>,
    entries: Vec<RarcBuilderTreeEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum RarcBuilderTreeEntry {
    Directory(RarcBuilderDirectory),
    File { raw_name: Vec<u8>, data: Vec<u8> },
}

#[derive(Debug)]
struct FlatRarcDirectory {
    raw_name: Vec<u8>,
    parent_index: Option<usize>,
    entries: Vec<FlatRarcEntry>,
}

#[derive(Debug)]
enum FlatRarcEntry {
    Directory {
        raw_name: Vec<u8>,
        node_index: usize,
    },
    File {
        raw_name: Vec<u8>,
        data: Vec<u8>,
    },
}

impl RarcBuilder {
    /// Creates an empty archive with the conventional stage-archive root.
    pub fn new_scene() -> Self {
        Self {
            root: RarcBuilderDirectory {
                raw_name: b"scene".to_vec(),
                entries: Vec::new(),
            },
        }
    }

    /// Creates an empty archive with a caller-supplied raw root name.
    pub fn new(raw_root_name: impl Into<Vec<u8>>) -> Result<Self> {
        let raw_name = raw_root_name.into();
        validate_rarc_name("builder root", 0, &raw_name)?;
        if matches!(raw_name.as_slice(), b"." | b"..") {
            return Err(unsupported_rarc("the RARC root name cannot be '.' or '..'"));
        }
        Ok(Self {
            root: RarcBuilderDirectory {
                raw_name,
                entries: Vec::new(),
            },
        })
    }

    /// Reconstructs a high-level tree from a complete structural document.
    /// Every file must still own its payload; detached semantic containers
    /// should temporarily regenerate their typed child bytes first.
    pub fn from_document(document: &RarcDocument) -> Result<Self> {
        let mut canonical = document.clone();
        canonical.canonicalize_layout()?;
        let mut visited = vec![false; canonical.nodes.len()];
        let root = builder_directory_from_node(&canonical, 0, 0, &mut visited)?;
        if let Some(index) = visited.iter().position(|visited| !visited) {
            return Err(unsupported_rarc(format!(
                "directory node {index} is unreachable from the builder root"
            )));
        }
        Ok(Self { root })
    }

    pub fn root_name(&self) -> &[u8] {
        &self.root.raw_name
    }

    pub fn contains_file(&self, raw_path: &[u8]) -> bool {
        let Ok(components) = rarc_builder_path_components(raw_path) else {
            return false;
        };
        builder_file(&self.root, &components).is_some()
    }

    /// Inserts a new file, creating missing parent directories. Existing
    /// files or a directory at the same path are rejected.
    pub fn insert_file(&mut self, raw_path: &[u8], data: Vec<u8>) -> Result<()> {
        let components = rarc_builder_path_components(raw_path)?;
        let (leaf, parents) = components
            .split_last()
            .expect("validated RARC builder paths are non-empty");
        let directory = builder_directory_mut(&mut self.root, parents, true)?;
        if directory
            .entries
            .iter()
            .any(|entry| entry.raw_name() == *leaf)
        {
            return Err(unsupported_rarc(format!(
                "archive path {raw_path:02X?} already exists"
            )));
        }
        directory.entries.push(RarcBuilderTreeEntry::File {
            raw_name: leaf.to_vec(),
            data,
        });
        Ok(())
    }

    /// Replaces an existing file and returns its previous payload.
    pub fn replace_file(&mut self, raw_path: &[u8], data: Vec<u8>) -> Result<Vec<u8>> {
        let components = rarc_builder_path_components(raw_path)?;
        let (leaf, parents) = components
            .split_last()
            .expect("validated RARC builder paths are non-empty");
        let directory = builder_directory_mut(&mut self.root, parents, false)?;
        let entry = directory
            .entries
            .iter_mut()
            .find(|entry| entry.raw_name() == *leaf)
            .ok_or_else(|| {
                unsupported_rarc(format!("archive file {raw_path:02X?} was not found"))
            })?;
        match entry {
            RarcBuilderTreeEntry::File { data: current, .. } => {
                Ok(std::mem::replace(current, data))
            }
            RarcBuilderTreeEntry::Directory(_) => Err(unsupported_rarc(format!(
                "archive path {raw_path:02X?} is a directory"
            ))),
        }
    }

    /// Removes a file and prunes parent directories that become empty.
    pub fn remove_file(&mut self, raw_path: &[u8]) -> Result<Vec<u8>> {
        let components = rarc_builder_path_components(raw_path)?;
        remove_builder_file(&mut self.root, &components).map(|(data, _)| data)
    }

    /// Builds a canonical structural document containing independently owned
    /// file payloads. Repeated calls are deterministic.
    pub fn build(&self) -> Result<RarcDocument> {
        self.clone().into_document()
    }

    pub fn into_document(self) -> Result<RarcDocument> {
        let mut directories = Vec::new();
        flatten_rarc_directory(self.root, None, 0, &mut directories)?;
        check_item_limit(
            "archive node count",
            directories.len(),
            MAX_ARCHIVE_NODE_COUNT,
        )?;

        let mut nodes = Vec::with_capacity(directories.len());
        let mut entries = Vec::new();
        for directory in directories {
            let first_entry_index = u32::try_from(entries.len()).map_err(|_| {
                resource_limit("archive entry count", entries.len(), u32::MAX as usize)
            })?;
            let entry_count = directory.entries.len().checked_add(2).ok_or_else(|| {
                resource_limit("directory entries", usize::MAX, u16::MAX as usize)
            })?;
            let entry_count = u16::try_from(entry_count)
                .map_err(|_| resource_limit("directory entries", entry_count, u16::MAX as usize))?;
            nodes.push(RarcNodeRecord {
                node_type: [0; 4],
                name_offset: 0,
                name_hash: 0,
                raw_name: directory.raw_name,
                entry_count,
                first_entry_index,
            });

            for entry in directory.entries {
                match entry {
                    FlatRarcEntry::Directory {
                        raw_name,
                        node_index,
                    } => entries.push(RarcEntryRecord {
                        file_id: u16::MAX,
                        name_hash: 0,
                        flags: RARC_DIRECTORY_FLAGS,
                        name_offset: 0,
                        raw_name,
                        data_offset: u32::try_from(node_index).map_err(|_| {
                            resource_limit("archive node index", node_index, u32::MAX as usize)
                        })?,
                        size: RARC_DIRECTORY_SIZE,
                        reserved: 0,
                        data: None,
                    }),
                    FlatRarcEntry::File { raw_name, data } => {
                        entries.push(RarcEntryRecord {
                            file_id: 0,
                            name_hash: 0,
                            flags: RARC_FILE_FLAGS,
                            name_offset: 0,
                            raw_name,
                            data_offset: 0,
                            size: 0,
                            reserved: 0,
                            data: Some(data),
                        });
                    }
                }
            }
            let node_index = nodes.len() - 1;
            entries.push(RarcEntryRecord {
                file_id: u16::MAX,
                name_hash: 0,
                flags: RARC_DIRECTORY_FLAGS,
                name_offset: 0,
                raw_name: b".".to_vec(),
                data_offset: node_index as u32,
                size: RARC_DIRECTORY_SIZE,
                reserved: 0,
                data: None,
            });
            entries.push(RarcEntryRecord {
                file_id: u16::MAX,
                name_hash: 0,
                flags: RARC_DIRECTORY_FLAGS,
                name_offset: 0,
                raw_name: b"..".to_vec(),
                data_offset: directory
                    .parent_index
                    .map(|index| index as u32)
                    .unwrap_or(u32::MAX),
                size: RARC_DIRECTORY_SIZE,
                reserved: 0,
                data: None,
            });
        }

        let mut document = RarcDocument {
            layout: empty_rarc_layout(),
            nodes,
            entries,
        };
        document.canonicalize_layout()?;
        Ok(document)
    }
}

impl RarcBuilderTreeEntry {
    fn raw_name(&self) -> &[u8] {
        match self {
            Self::Directory(directory) => &directory.raw_name,
            Self::File { raw_name, .. } => raw_name,
        }
    }
}

fn rarc_builder_path_components(raw_path: &[u8]) -> Result<Vec<&[u8]>> {
    let normalized = raw_path.strip_prefix(b"/").unwrap_or(raw_path);
    check_item_limit(
        "archive path bytes",
        normalized.len(),
        MAX_ARCHIVE_PATH_BYTES,
    )?;
    if normalized.is_empty() {
        return Err(unsupported_rarc("archive file path cannot be empty"));
    }
    let components = normalized.split(|byte| *byte == b'/').collect::<Vec<_>>();
    for (index, component) in components.iter().enumerate() {
        validate_rarc_name("builder path component", index, component)?;
        if matches!(*component, b"." | b"..") {
            return Err(unsupported_rarc(format!(
                "archive path {raw_path:02X?} contains a traversal component"
            )));
        }
    }
    Ok(components)
}

fn builder_directory_mut<'a>(
    directory: &'a mut RarcBuilderDirectory,
    components: &[&[u8]],
    create: bool,
) -> Result<&'a mut RarcBuilderDirectory> {
    let Some((component, remaining)) = components.split_first() else {
        return Ok(directory);
    };
    let position = directory
        .entries
        .iter()
        .position(|entry| entry.raw_name() == *component);
    let position = match position {
        Some(position) => position,
        None if create => {
            directory
                .entries
                .push(RarcBuilderTreeEntry::Directory(RarcBuilderDirectory {
                    raw_name: component.to_vec(),
                    entries: Vec::new(),
                }));
            directory.entries.len() - 1
        }
        None => {
            return Err(unsupported_rarc(format!(
                "archive directory component {component:02X?} was not found"
            )));
        }
    };
    let RarcBuilderTreeEntry::Directory(child) = &mut directory.entries[position] else {
        return Err(unsupported_rarc(format!(
            "archive path component {component:02X?} is a file"
        )));
    };
    builder_directory_mut(child, remaining, create)
}

fn builder_file<'a>(directory: &'a RarcBuilderDirectory, components: &[&[u8]]) -> Option<&'a [u8]> {
    let (component, remaining) = components.split_first()?;
    let entry = directory
        .entries
        .iter()
        .find(|entry| entry.raw_name() == *component)?;
    match (entry, remaining.is_empty()) {
        (RarcBuilderTreeEntry::File { data, .. }, true) => Some(data),
        (RarcBuilderTreeEntry::Directory(child), false) => builder_file(child, remaining),
        _ => None,
    }
}

fn remove_builder_file(
    directory: &mut RarcBuilderDirectory,
    components: &[&[u8]],
) -> Result<(Vec<u8>, bool)> {
    let (component, remaining) = components
        .split_first()
        .expect("validated RARC builder paths are non-empty");
    let position = directory
        .entries
        .iter()
        .position(|entry| entry.raw_name() == *component)
        .ok_or_else(|| {
            unsupported_rarc(format!(
                "archive path component {component:02X?} was not found"
            ))
        })?;

    let data = if remaining.is_empty() {
        match directory.entries.get(position) {
            Some(RarcBuilderTreeEntry::File { .. }) => {}
            Some(RarcBuilderTreeEntry::Directory(_)) => {
                return Err(unsupported_rarc(format!(
                    "archive path component {component:02X?} is a directory"
                )));
            }
            None => unreachable!("the entry position was found above"),
        }
        let RarcBuilderTreeEntry::File { data, .. } = directory.entries.remove(position) else {
            unreachable!("the entry kind was checked above");
        };
        data
    } else {
        let RarcBuilderTreeEntry::Directory(child) = &mut directory.entries[position] else {
            return Err(unsupported_rarc(format!(
                "archive path component {component:02X?} is a file"
            )));
        };
        let (data, child_is_empty) = remove_builder_file(child, remaining)?;
        if child_is_empty {
            directory.entries.remove(position);
        }
        data
    };
    Ok((data, directory.entries.is_empty()))
}

fn builder_directory_from_node(
    document: &RarcDocument,
    node_index: usize,
    depth: usize,
    visited: &mut [bool],
) -> Result<RarcBuilderDirectory> {
    if depth > MAX_ARCHIVE_DIRECTORY_DEPTH {
        return Err(resource_limit(
            "archive directory depth",
            depth,
            MAX_ARCHIVE_DIRECTORY_DEPTH,
        ));
    }
    let node = document
        .nodes
        .get(node_index)
        .ok_or_else(|| invalid_offset(node_index, document.nodes.len()))?;
    if std::mem::replace(&mut visited[node_index], true) {
        return Err(unsupported_rarc(format!(
            "directory node {node_index} is referenced more than once"
        )));
    }
    let (start, end) = rarc_node_entry_range(node, document.entries.len())?;
    let mut entries = Vec::with_capacity(node.entry_count.saturating_sub(2) as usize);
    for entry in &document.entries[start..end] {
        if matches!(entry.raw_name.as_slice(), b"." | b"..") {
            continue;
        }
        if entry.is_directory() {
            let child = builder_directory_from_node(
                document,
                entry.data_offset as usize,
                depth + 1,
                visited,
            )?;
            entries.push(RarcBuilderTreeEntry::Directory(child));
        } else {
            let data = entry.data.clone().ok_or_else(|| {
                unsupported_rarc(format!(
                    "file entry {node_index}:{:?} has no generated payload",
                    entry.raw_name
                ))
            })?;
            entries.push(RarcBuilderTreeEntry::File {
                raw_name: entry.raw_name.clone(),
                data,
            });
        }
    }
    Ok(RarcBuilderDirectory {
        raw_name: node.raw_name.clone(),
        entries,
    })
}

fn flatten_rarc_directory(
    directory: RarcBuilderDirectory,
    parent_index: Option<usize>,
    depth: usize,
    output: &mut Vec<FlatRarcDirectory>,
) -> Result<usize> {
    if depth > MAX_ARCHIVE_DIRECTORY_DEPTH {
        return Err(resource_limit(
            "archive directory depth",
            depth,
            MAX_ARCHIVE_DIRECTORY_DEPTH,
        ));
    }
    let index = output.len();
    output.push(FlatRarcDirectory {
        raw_name: directory.raw_name,
        parent_index,
        entries: Vec::new(),
    });
    for entry in directory.entries {
        match entry {
            RarcBuilderTreeEntry::Directory(child) => {
                let raw_name = child.raw_name.clone();
                let node_index = flatten_rarc_directory(child, Some(index), depth + 1, output)?;
                output[index].entries.push(FlatRarcEntry::Directory {
                    raw_name,
                    node_index,
                });
            }
            RarcBuilderTreeEntry::File { raw_name, data } => output[index]
                .entries
                .push(FlatRarcEntry::File { raw_name, data }),
        }
    }
    Ok(index)
}

fn empty_rarc_layout() -> RarcLayout {
    RarcLayout {
        file_size: 0,
        header_size: RARC_HEADER_SIZE,
        data_offset: 0,
        data_size: 0,
        mram_data_size: 0,
        aram_data_size: 0,
        dvd_data_size: 0,
        metadata_present: true,
        node_offset: 0,
        entry_offset: 0,
        string_table_offset: 0,
        string_table_size: 0,
        next_free_file_id: 0,
        sync_file_ids: RARC_SYNC_FILE_IDS,
        info_reserved: [0; 5],
        alignment: RARC_ALIGNMENT as u32,
        padding_byte: RARC_PADDING_BYTE,
    }
}

impl RarcEntryRecord {
    pub fn is_directory(&self) -> bool {
        (self.flags & 0x02) != 0
    }

    pub fn set_file_data(&mut self, data: Vec<u8>) -> Result<()> {
        if self.is_directory() {
            return Err(FormatError::Unsupported {
                format: FORMAT,
                message: "cannot attach file data to a directory entry".to_string(),
            });
        }
        self.size = u32::try_from(data.len())
            .map_err(|_| resource_limit("archive file bytes", data.len(), u32::MAX as usize))?;
        self.data = Some(data);
        Ok(())
    }

    /// Removes and returns the child payload so a format-specific importer can
    /// replace it with a semantic document rather than retaining archive data.
    pub fn take_file_data(&mut self) -> Option<Vec<u8>> {
        self.data.take()
    }
}

impl RarcDocument {
    pub fn parse(bytes: impl AsRef<[u8]>) -> Result<Self> {
        let bytes = bytes.as_ref();
        let archive = RarcArchive::parse(bytes)?;
        let header = archive.header().clone();
        let declared_size = header.file_size as usize;
        if bytes.len() != declared_size {
            return Err(FormatError::Unsupported {
                format: FORMAT,
                message: format!(
                    "source-free RARC import rejects {} physical trailing bytes",
                    bytes.len() - declared_size
                ),
            });
        }

        let mram_data_size = be_u32(bytes, 0x14, FORMAT)?;
        let aram_data_size = be_u32(bytes, 0x18, FORMAT)?;
        let dvd_data_size = be_u32(bytes, 0x1C, FORMAT)?;
        if declared_size == 0x20 && header.header_size == 0x20 && header.data_size == 0 {
            let mut document = Self {
                layout: RarcLayout {
                    file_size: header.file_size,
                    header_size: header.header_size,
                    data_offset: header.data_offset,
                    data_size: header.data_size,
                    mram_data_size,
                    aram_data_size,
                    dvd_data_size,
                    metadata_present: false,
                    node_offset: 0,
                    entry_offset: 0,
                    string_table_offset: 0,
                    string_table_size: 0,
                    next_free_file_id: 0,
                    sync_file_ids: 0,
                    info_reserved: [0; 5],
                    alignment: RARC_ALIGNMENT as u32,
                    padding_byte: RARC_PADDING_BYTE,
                },
                nodes: Vec::new(),
                entries: Vec::new(),
            };
            document.canonicalize_layout()?;
            reject_noncanonical_import(bytes, &build_rarc_document(&document)?)?;
            return Ok(document);
        }

        let info = header.header_size as usize;
        let node_count = be_u32(bytes, info, FORMAT)? as usize;
        let node_offset = be_u32(bytes, info + 0x04, FORMAT)?;
        let entry_count = be_u32(bytes, info + 0x08, FORMAT)? as usize;
        let entry_offset = be_u32(bytes, info + 0x0C, FORMAT)?;
        let string_table_size = be_u32(bytes, info + 0x10, FORMAT)?;
        let string_table_offset = be_u32(bytes, info + 0x14, FORMAT)?;
        let next_free_file_id = be_u16(bytes, info + 0x18, FORMAT)?;
        let sync_file_ids = bytes[info + 0x1A];
        let mut info_reserved = [0; 5];
        info_reserved.copy_from_slice(&bytes[info + 0x1B..info + 0x20]);

        let node_start = checked_add(info, node_offset as usize, declared_size)?;
        let entry_start = checked_add(info, entry_offset as usize, declared_size)?;
        let string_start = checked_add(info, string_table_offset as usize, declared_size)?;
        let data_start = checked_add(info, header.data_offset as usize, declared_size)?;
        let string_table = checked_slice(FORMAT, bytes, string_start, string_table_size as usize)?;

        let mut nodes = Vec::new();
        nodes
            .try_reserve_exact(node_count)
            .map_err(|error| allocation_error("source-free RARC nodes", node_count, error))?;
        for index in 0..node_count {
            let offset = checked_add(node_start, index * 0x10, declared_size)?;
            let mut node_type = [0; 4];
            node_type.copy_from_slice(checked_slice(FORMAT, bytes, offset, 4)?);
            let name_offset = be_u32(bytes, offset + 0x04, FORMAT)?;
            let (_, raw_name) = read_string(string_table, name_offset as usize)?;
            nodes.push(RarcNodeRecord {
                node_type,
                name_offset,
                name_hash: be_u16(bytes, offset + 0x08, FORMAT)?,
                raw_name,
                entry_count: be_u16(bytes, offset + 0x0A, FORMAT)?,
                first_entry_index: be_u32(bytes, offset + 0x0C, FORMAT)?,
            });
        }

        let mut entries = Vec::new();
        entries
            .try_reserve_exact(entry_count)
            .map_err(|error| allocation_error("source-free RARC entries", entry_count, error))?;
        for index in 0..entry_count {
            let offset = checked_add(entry_start, index * 0x14, declared_size)?;
            let flags_and_name = be_u32(bytes, offset + 0x04, FORMAT)?;
            let flags = (flags_and_name >> 24) as u8;
            let name_offset = flags_and_name & 0x00FF_FFFF;
            let (_, raw_name) = read_string(string_table, name_offset as usize)?;
            let data_offset = be_u32(bytes, offset + 0x08, FORMAT)?;
            let size = be_u32(bytes, offset + 0x0C, FORMAT)?;
            let data = if (flags & 0x02) != 0 {
                None
            } else {
                let start = checked_add(data_start, data_offset as usize, declared_size)?;
                Some(checked_slice(FORMAT, bytes, start, size as usize)?.to_vec())
            };
            entries.push(RarcEntryRecord {
                file_id: be_u16(bytes, offset, FORMAT)?,
                name_hash: be_u16(bytes, offset + 0x02, FORMAT)?,
                flags,
                name_offset,
                raw_name,
                data_offset,
                size,
                reserved: be_u32(bytes, offset + 0x10, FORMAT)?,
                data,
            });
        }

        let mut document = Self {
            layout: RarcLayout {
                file_size: header.file_size,
                header_size: header.header_size,
                data_offset: header.data_offset,
                data_size: header.data_size,
                mram_data_size,
                aram_data_size,
                dvd_data_size,
                metadata_present: true,
                node_offset,
                entry_offset,
                string_table_offset,
                string_table_size,
                next_free_file_id,
                sync_file_ids,
                info_reserved,
                alignment: RARC_ALIGNMENT as u32,
                padding_byte: RARC_PADDING_BYTE,
            },
            nodes,
            entries,
        };

        // Imported hashes, offsets, counts, reserved words, and padding are
        // not source data. Rebuild them solely from the semantic tree and
        // payloads, then require the retail creator layout to agree exactly.
        document.canonicalize_layout()?;
        reject_noncanonical_import(bytes, &build_rarc_document(&document)?)?;
        Ok(document)
    }

    pub fn to_bytes(&self) -> Result<Vec<u8>> {
        let mut canonical = self.clone();
        canonical.canonicalize_layout()?;
        build_rarc_document(&canonical)
    }

    pub fn entry_by_raw_path(&self, archive_path: &[u8]) -> Option<&RarcEntryRecord> {
        self.find_entry_index(archive_path)
            .and_then(|index| self.entries.get(index))
    }

    pub fn entry_by_raw_path_mut(&mut self, archive_path: &[u8]) -> Option<&mut RarcEntryRecord> {
        self.find_entry_index(archive_path)
            .and_then(|index| self.entries.get_mut(index))
    }

    pub fn take_file_data(&mut self, archive_path: &[u8]) -> Result<Vec<u8>> {
        let entry =
            self.entry_by_raw_path_mut(archive_path)
                .ok_or_else(|| FormatError::Unsupported {
                    format: FORMAT,
                    message: format!("archive entry not found for raw path {archive_path:02X?}"),
                })?;
        if entry.is_directory() {
            return Err(FormatError::Unsupported {
                format: FORMAT,
                message: format!("archive path {archive_path:02X?} is a directory"),
            });
        }
        entry
            .take_file_data()
            .ok_or_else(|| FormatError::Unsupported {
                format: FORMAT,
                message: format!("archive path {archive_path:02X?} has no file payload"),
            })
    }

    pub fn set_file_data(&mut self, archive_path: &[u8], data: Vec<u8>) -> Result<()> {
        let entry =
            self.entry_by_raw_path_mut(archive_path)
                .ok_or_else(|| FormatError::Unsupported {
                    format: FORMAT,
                    message: format!("archive entry not found for raw path {archive_path:02X?}"),
                })?;
        entry.set_file_data(data)
    }

    fn find_entry_index(&self, archive_path: &[u8]) -> Option<usize> {
        let normalized = archive_path.strip_prefix(b"/").unwrap_or(archive_path);
        if normalized.is_empty() || self.nodes.is_empty() {
            return None;
        }
        let components: Vec<&[u8]> = normalized.split(|byte| *byte == b'/').collect();
        let mut node_index = 0usize;
        for (component_index, component) in components.iter().enumerate() {
            let node = self.nodes.get(node_index)?;
            let start = node.first_entry_index as usize;
            let end = start.checked_add(node.entry_count as usize)?;
            let is_last = component_index + 1 == components.len();
            let (index, entry) =
                self.entries
                    .get(start..end)?
                    .iter()
                    .enumerate()
                    .find(|(_, entry)| {
                        entry.raw_name.as_slice() == *component
                            && entry.raw_name != b"."
                            && entry.raw_name != b".."
                    })?;
            let absolute_index = start + index;
            if is_last {
                return Some(absolute_index);
            }
            if !entry.is_directory() {
                return None;
            }
            node_index = entry.data_offset as usize;
        }
        None
    }

    /// Recomputes all derived hashes, string offsets, table offsets, file
    /// offsets, partition sizes, and archive sizes without changing record
    /// ordering or file contents.
    pub fn canonicalize_layout(&mut self) -> Result<()> {
        self.rebuild_metadata_layout()?;
        self.rebuild_data_layout()?;
        Ok(())
    }

    pub fn rebuild_metadata_layout(&mut self) -> Result<()> {
        self.layout.header_size = RARC_HEADER_SIZE;
        self.layout.alignment = RARC_ALIGNMENT as u32;
        self.layout.padding_byte = RARC_PADDING_BYTE;
        self.layout.info_reserved = [0; 5];

        if self.nodes.is_empty() && self.entries.is_empty() {
            self.layout.file_size = RARC_HEADER_SIZE;
            self.layout.data_offset = 0;
            self.layout.data_size = 0;
            self.layout.mram_data_size = 0;
            self.layout.aram_data_size = 0;
            self.layout.dvd_data_size = 0;
            self.layout.metadata_present = false;
            self.layout.node_offset = 0;
            self.layout.entry_offset = 0;
            self.layout.string_table_offset = 0;
            self.layout.string_table_size = 0;
            self.layout.next_free_file_id = 0;
            self.layout.sync_file_ids = 0;
            return Ok(());
        }
        if self.nodes.is_empty() {
            return Err(unsupported_rarc(
                "archive entries exist without a semantic root node",
            ));
        }

        let string_length = canonicalize_tree_records(&mut self.nodes, &mut self.entries)?;
        self.layout.metadata_present = true;
        self.layout.next_free_file_id = u16::try_from(self.entries.len()).map_err(|_| {
            resource_limit("archive entry IDs", self.entries.len(), u16::MAX as usize)
        })?;
        self.layout.sync_file_ids = RARC_SYNC_FILE_IDS;
        self.layout.node_offset = RARC_ALIGNMENT as u32;

        let node_length = checked_table_length(self.nodes.len(), 0x10, "node table bytes")?;
        let nodes_end = (self.layout.node_offset as usize)
            .checked_add(node_length)
            .ok_or_else(|| {
                resource_limit("archive metadata bytes", usize::MAX, u32::MAX as usize)
            })?;
        self.layout.entry_offset = u32::try_from(align_up(nodes_end, RARC_ALIGNMENT)?)
            .map_err(|_| resource_limit("archive metadata bytes", nodes_end, u32::MAX as usize))?;

        let entry_length =
            checked_table_length(self.entries.len(), 0x14, "file-entry table bytes")?;
        let entries_end = (self.layout.entry_offset as usize)
            .checked_add(entry_length)
            .ok_or_else(|| {
                resource_limit("archive metadata bytes", usize::MAX, u32::MAX as usize)
            })?;
        self.layout.string_table_offset = u32::try_from(align_up(entries_end, RARC_ALIGNMENT)?)
            .map_err(|_| {
                resource_limit("archive metadata bytes", entries_end, u32::MAX as usize)
            })?;
        self.layout.string_table_size = u32::try_from(align_up(string_length, RARC_ALIGNMENT)?)
            .map_err(|_| {
                resource_limit(
                    "archive string-table bytes",
                    string_length,
                    u32::MAX as usize,
                )
            })?;
        self.layout.data_offset = self
            .layout
            .string_table_offset
            .checked_add(self.layout.string_table_size)
            .ok_or_else(|| {
                resource_limit("archive metadata bytes", usize::MAX, u32::MAX as usize)
            })?;
        Ok(())
    }

    pub fn rebuild_data_layout(&mut self) -> Result<()> {
        if !self.layout.metadata_present {
            if !self.nodes.is_empty() || !self.entries.is_empty() {
                return Err(unsupported_rarc(
                    "archive records exist without a metadata block",
                ));
            }
            self.layout.file_size = RARC_HEADER_SIZE;
            self.layout.header_size = RARC_HEADER_SIZE;
            self.layout.data_offset = 0;
            self.layout.data_size = 0;
            self.layout.mram_data_size = 0;
            self.layout.aram_data_size = 0;
            self.layout.dvd_data_size = 0;
            return Ok(());
        }

        let mut position = 0usize;
        let mut partition_ends = [0usize; 3];
        for (partition, partition_end) in partition_ends.iter_mut().enumerate() {
            for (index, entry) in self.entries.iter_mut().enumerate() {
                if entry.is_directory() || rarc_data_partition(entry.flags) != partition {
                    continue;
                }
                position = align_up(position, RARC_ALIGNMENT)?;
                let data = entry
                    .data
                    .as_ref()
                    .ok_or_else(|| FormatError::Unsupported {
                        format: FORMAT,
                        message: format!("file entry {index} has no generated payload"),
                    })?;
                entry.data_offset = u32::try_from(position).map_err(|_| {
                    resource_limit("archive data bytes", position, u32::MAX as usize)
                })?;
                entry.size = u32::try_from(data.len()).map_err(|_| {
                    resource_limit("archive file bytes", data.len(), u32::MAX as usize)
                })?;
                position = position.checked_add(data.len()).ok_or_else(|| {
                    resource_limit("archive data bytes", usize::MAX, u32::MAX as usize)
                })?;
            }
            position = align_up(position, RARC_ALIGNMENT)?;
            *partition_end = position;
        }
        self.layout.mram_data_size = u32::try_from(partition_ends[0]).map_err(|_| {
            resource_limit(
                "archive MRAM data bytes",
                partition_ends[0],
                u32::MAX as usize,
            )
        })?;
        self.layout.aram_data_size =
            u32::try_from(partition_ends[1] - partition_ends[0]).map_err(|_| {
                resource_limit(
                    "archive ARAM data bytes",
                    partition_ends[1] - partition_ends[0],
                    u32::MAX as usize,
                )
            })?;
        self.layout.dvd_data_size =
            u32::try_from(partition_ends[2] - partition_ends[1]).map_err(|_| {
                resource_limit(
                    "archive DVD data bytes",
                    partition_ends[2] - partition_ends[1],
                    u32::MAX as usize,
                )
            })?;
        self.layout.data_size = u32::try_from(position)
            .map_err(|_| resource_limit("archive data bytes", position, u32::MAX as usize))?;
        let file_size = (RARC_HEADER_SIZE as usize)
            .checked_add(self.layout.data_offset as usize)
            .and_then(|size| size.checked_add(self.layout.data_size as usize))
            .ok_or_else(|| resource_limit("archive bytes", usize::MAX, u32::MAX as usize))?;
        self.layout.file_size = u32::try_from(file_size)
            .map_err(|_| resource_limit("archive bytes", file_size, u32::MAX as usize))?;
        Ok(())
    }
}

#[derive(Debug, Clone, Copy)]
struct RarcTraversalFrame {
    node_index: usize,
    next_entry: usize,
    end_entry: usize,
    depth: usize,
}

fn canonicalize_tree_records(
    nodes: &mut [RarcNodeRecord],
    entries: &mut [RarcEntryRecord],
) -> Result<usize> {
    check_item_limit("archive node count", nodes.len(), MAX_ARCHIVE_NODE_COUNT)?;
    check_item_limit(
        "archive entry count",
        entries.len(),
        MAX_ARCHIVE_ENTRY_COUNT,
    )?;
    if nodes.is_empty() {
        return Err(unsupported_rarc("archive has no semantic root node"));
    }
    if entries.len() > u16::MAX as usize {
        return Err(resource_limit(
            "archive entry IDs",
            entries.len(),
            u16::MAX as usize,
        ));
    }

    for (index, node) in nodes.iter_mut().enumerate() {
        validate_rarc_name("node", index, &node.raw_name)?;
        node.name_hash = rarc_name_hash(&node.raw_name);
        node.node_type = canonical_node_type(index, &node.raw_name);
        node.name_offset = 0;
        rarc_node_entry_range(node, entries.len())?;
    }
    for (index, entry) in entries.iter_mut().enumerate() {
        validate_rarc_name("entry", index, &entry.raw_name)?;
        entry.name_hash = rarc_name_hash(&entry.raw_name);
        entry.name_offset = 0;
        entry.reserved = 0;
        if entry.is_directory() {
            if entry.flags != RARC_DIRECTORY_FLAGS {
                return Err(unsupported_rarc(format!(
                    "directory entry {index} uses unsupported flags {:#04X}",
                    entry.flags
                )));
            }
            if entry.data.is_some() {
                return Err(unsupported_rarc(format!(
                    "directory entry {index} contains file data"
                )));
            }
            entry.file_id = u16::MAX;
            entry.size = RARC_DIRECTORY_SIZE;
        } else {
            if entry.flags != RARC_FILE_FLAGS {
                return Err(unsupported_rarc(format!(
                    "file entry {index} uses unsupported flags {:#04X}; strict stage archives require 0x11",
                    entry.flags
                )));
            }
            if entry.data.is_none() {
                return Err(unsupported_rarc(format!(
                    "file entry {index} has no generated payload"
                )));
            }
            entry.file_id = index as u16;
        }
    }

    // The Nintendo stage-archive creator seeds the shared dot strings, then
    // writes the root name and performs a depth-first walk. A directory node
    // and the entry pointing to it share one slot; equal names in unrelated
    // directories intentionally receive distinct slots.
    let mut string_length = 5usize; // ".\0..\0"
    nodes[0].name_offset = canonical_string_offset(&mut string_length, &nodes[0].raw_name)?;

    let mut visited_nodes = vec![false; nodes.len()];
    let mut claimed_entries = vec![false; entries.len()];
    let mut parent_nodes = vec![None; nodes.len()];
    let mut dot_counts = vec![[0u8; 2]; nodes.len()];
    visited_nodes[0] = true;
    let (root_start, root_end) = rarc_node_entry_range(&nodes[0], entries.len())?;
    let mut stack = vec![RarcTraversalFrame {
        node_index: 0,
        next_entry: root_start,
        end_entry: root_end,
        depth: 0,
    }];

    while let Some(frame) = stack.last_mut() {
        if frame.next_entry == frame.end_entry {
            stack.pop();
            continue;
        }
        let node_index = frame.node_index;
        let depth = frame.depth;
        let entry_index = frame.next_entry;
        frame.next_entry += 1;

        if claimed_entries[entry_index] {
            return Err(unsupported_rarc(format!(
                "entry {entry_index} belongs to more than one directory node"
            )));
        }
        claimed_entries[entry_index] = true;

        let raw_name = entries[entry_index].raw_name.clone();
        let is_directory = entries[entry_index].is_directory();
        let target = entries[entry_index].data_offset;
        if raw_name == b"." {
            if !is_directory || target as usize != node_index {
                return Err(unsupported_rarc(format!(
                    "node {node_index} has a malformed '.' directory entry"
                )));
            }
            dot_counts[node_index][0] = dot_counts[node_index][0].saturating_add(1);
            entries[entry_index].name_offset = 0;
            continue;
        }
        if raw_name == b".." {
            let expected_parent = parent_nodes[node_index]
                .map(|parent| parent as u32)
                .unwrap_or(u32::MAX);
            if !is_directory || target != expected_parent {
                return Err(unsupported_rarc(format!(
                    "node {node_index} has a malformed '..' directory entry"
                )));
            }
            dot_counts[node_index][1] = dot_counts[node_index][1].saturating_add(1);
            entries[entry_index].name_offset = 2;
            continue;
        }

        let offset = canonical_string_offset(&mut string_length, &raw_name)?;
        entries[entry_index].name_offset = offset;
        if !is_directory {
            continue;
        }

        let child_index = target as usize;
        if child_index >= nodes.len() {
            return Err(invalid_offset(child_index, nodes.len()));
        }
        if visited_nodes[child_index] {
            return Err(unsupported_rarc(format!(
                "directory entry {entry_index} points to node {child_index} more than once"
            )));
        }
        if nodes[child_index].raw_name != raw_name {
            return Err(unsupported_rarc(format!(
                "directory entry {entry_index} name does not match node {child_index}"
            )));
        }
        let child_depth = depth.checked_add(1).ok_or_else(|| {
            resource_limit(
                "archive directory depth",
                usize::MAX,
                MAX_ARCHIVE_DIRECTORY_DEPTH,
            )
        })?;
        if child_depth > MAX_ARCHIVE_DIRECTORY_DEPTH {
            return Err(resource_limit(
                "archive directory depth",
                child_depth,
                MAX_ARCHIVE_DIRECTORY_DEPTH,
            ));
        }
        visited_nodes[child_index] = true;
        parent_nodes[child_index] = Some(node_index);
        nodes[child_index].name_offset = offset;
        let (child_start, child_end) = rarc_node_entry_range(&nodes[child_index], entries.len())?;
        stack.push(RarcTraversalFrame {
            node_index: child_index,
            next_entry: child_start,
            end_entry: child_end,
            depth: child_depth,
        });
    }

    if let Some(index) = visited_nodes.iter().position(|visited| !visited) {
        return Err(unsupported_rarc(format!(
            "directory node {index} is unreachable from the root"
        )));
    }
    if let Some(index) = claimed_entries.iter().position(|claimed| !claimed) {
        return Err(unsupported_rarc(format!(
            "entry {index} is not owned by a directory node"
        )));
    }
    if let Some((index, counts)) = dot_counts
        .iter()
        .enumerate()
        .find(|(_, counts)| **counts != [1, 1])
    {
        return Err(unsupported_rarc(format!(
            "directory node {index} must contain exactly one '.' and one '..' entry, found {} and {}",
            counts[0], counts[1]
        )));
    }
    Ok(string_length)
}

fn rarc_node_entry_range(node: &RarcNodeRecord, entry_len: usize) -> Result<(usize, usize)> {
    let start = node.first_entry_index as usize;
    let end = start
        .checked_add(node.entry_count as usize)
        .ok_or_else(|| invalid_offset(start, entry_len))?;
    if end > entry_len {
        return Err(invalid_offset(end, entry_len));
    }
    Ok((start, end))
}

fn canonical_string_offset(position: &mut usize, name: &[u8]) -> Result<u32> {
    if *position > 0x00FF_FFFF {
        return Err(resource_limit(
            "archive string-table bytes",
            *position,
            0x00FF_FFFF,
        ));
    }
    let offset = *position as u32;
    *position = (*position)
        .checked_add(name.len() + 1)
        .ok_or_else(|| resource_limit("archive string-table bytes", usize::MAX, 0x00FF_FFFF))?;
    if *position > 0x0100_0000 {
        return Err(resource_limit(
            "archive string-table bytes",
            *position,
            0x0100_0000,
        ));
    }
    Ok(offset)
}

fn canonical_node_type(index: usize, name: &[u8]) -> [u8; 4] {
    if index == 0 {
        return *b"ROOT";
    }
    let mut node_type = [b' '; 4];
    for (target, byte) in node_type.iter_mut().zip(name.iter().copied()) {
        *target = byte.to_ascii_uppercase();
    }
    node_type
}

fn validate_rarc_name(kind: &str, index: usize, name: &[u8]) -> Result<()> {
    if name.is_empty() || name.contains(&0) || name.contains(&b'/') {
        return Err(unsupported_rarc(format!(
            "{kind} {index} has an invalid raw archive name"
        )));
    }
    check_item_limit("archive name bytes", name.len(), MAX_ARCHIVE_PATH_BYTES)
}

pub fn rarc_name_hash(name: &[u8]) -> u16 {
    name.iter().fold(0u16, |hash, byte| {
        hash.wrapping_mul(3).wrapping_add(*byte as u16)
    })
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RarcFileEntry {
    pub path: String,
    /// Exact archive path bytes, with `/` separators inserted between raw names.
    #[serde(default)]
    pub raw_path: Vec<u8>,
    pub flags: u8,
    pub data_offset: u32,
    pub size: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RarcArchive {
    header: RarcHeader,
    bytes: Vec<u8>,
    index: RarcFileIndex,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct RarcFileIndex {
    files: Vec<RarcFileEntry>,
    raw_order: Vec<usize>,
}

#[derive(Serialize, Deserialize)]
struct RarcArchiveSerde {
    header: RarcHeader,
    bytes: Vec<u8>,
}

impl RarcArchive {
    pub fn parse(bytes: impl AsRef<[u8]>) -> Result<Self> {
        let bytes = bytes.as_ref();
        require_len(FORMAT, bytes, 0x20)?;
        require_magic(FORMAT, bytes, b"RARC")?;

        let header = RarcHeader {
            file_size: be_u32(bytes, 0x04, FORMAT)?,
            header_size: be_u32(bytes, 0x08, FORMAT)?,
            data_offset: be_u32(bytes, 0x0C, FORMAT)?,
            data_size: be_u32(bytes, 0x10, FORMAT)?,
        };

        let declared_size = header.file_size as usize;
        let header_size = header.header_size as usize;
        if declared_size < 0x20 || declared_size > bytes.len() {
            return Err(invalid_offset(declared_size, bytes.len()));
        }
        if !(0x20..=declared_size).contains(&header_size) {
            return Err(invalid_offset(header_size, declared_size));
        }
        let data_start = header_size
            .checked_add(header.data_offset as usize)
            .ok_or_else(|| invalid_offset(header.data_offset as usize, declared_size))?;
        let data_end = data_start
            .checked_add(header.data_size as usize)
            .ok_or_else(|| invalid_offset(header.data_size as usize, declared_size))?;
        if data_start > declared_size || data_end > declared_size {
            return Err(invalid_offset(data_end.max(data_start), declared_size));
        }

        let semantic_bytes = &bytes[..declared_size];
        let index = if declared_size == 0x20 && header_size == 0x20 && header.data_size == 0 {
            RarcFileIndex::default()
        } else {
            let tables = RarcTables::parse(
                semantic_bytes,
                header_size,
                data_start,
                header.data_size as usize,
            )?;
            RarcFileIndex::build(&tables)?
        };

        Ok(Self {
            header,
            bytes: bytes.to_vec(),
            index,
        })
    }

    pub fn header(&self) -> &RarcHeader {
        &self.header
    }

    pub fn files(&self) -> Result<Vec<RarcFileEntry>> {
        Ok(self.index.files.clone())
    }

    /// Returns the validated archive entries without rebuilding the RARC tables.
    pub fn file_entries(&self) -> &[RarcFileEntry] {
        &self.index.files
    }

    pub fn file_bytes(&self, archive_path: &str) -> Result<Vec<u8>> {
        let normalized = archive_path.trim_start_matches('/').replace('\\', "/");
        let entry =
            self.index
                .find_display(&normalized)
                .ok_or_else(|| FormatError::Unsupported {
                    format: FORMAT,
                    message: format!("archive entry not found: {archive_path}"),
                })?;

        self.entry_bytes(entry)
    }

    /// Reads an entry by its exact raw Shift-JIS path bytes.
    pub fn file_bytes_raw(&self, archive_path: &[u8]) -> Result<Vec<u8>> {
        let normalized = archive_path.strip_prefix(b"/").unwrap_or(archive_path);
        let entry = self
            .index
            .find_raw(normalized)
            .ok_or_else(|| FormatError::Unsupported {
                format: FORMAT,
                message: format!("archive entry not found for raw path {normalized:02X?}"),
            })?;
        self.entry_bytes(entry)
    }

    fn entry_bytes(&self, entry: &RarcFileEntry) -> Result<Vec<u8>> {
        let relative_end = (entry.data_offset as usize)
            .checked_add(entry.size as usize)
            .ok_or_else(|| {
                invalid_offset(entry.data_offset as usize, self.header.data_size as usize)
            })?;
        if relative_end > self.header.data_size as usize {
            return Err(invalid_offset(relative_end, self.header.data_size as usize));
        }
        let data_start = (self.header.header_size as usize)
            .checked_add(self.header.data_offset as usize)
            .and_then(|offset| offset.checked_add(entry.data_offset as usize))
            .ok_or_else(|| {
                invalid_offset(entry.data_offset as usize, self.header.file_size as usize)
            })?;
        let semantic_bytes = &self.bytes[..self.header.file_size as usize];
        let bytes = checked_slice(FORMAT, semantic_bytes, data_start, entry.size as usize)?;
        Ok(bytes.to_vec())
    }
}

impl Serialize for RarcArchive {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        RarcArchiveSerde {
            header: self.header.clone(),
            bytes: self.bytes.clone(),
        }
        .serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for RarcArchive {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let decoded = RarcArchiveSerde::deserialize(deserializer)?;
        let archive = Self::parse(decoded.bytes).map_err(serde::de::Error::custom)?;
        if archive.header != decoded.header {
            return Err(serde::de::Error::custom(
                "serialized RARC header does not match its source bytes",
            ));
        }
        Ok(archive)
    }
}

impl PreserveBytes for RarcArchive {
    fn source_bytes(&self) -> &[u8] {
        &self.bytes
    }
}

#[derive(Debug, Clone)]
struct RarcTables {
    nodes: Vec<RarcNode>,
    entries: Vec<RarcEntry>,
}

#[derive(Debug, Clone)]
struct RarcNode {
    entry_count: u16,
    first_entry_index: u32,
}

#[derive(Debug, Clone)]
struct RarcEntry {
    name: String,
    raw_name: Vec<u8>,
    flags: u8,
    data_offset: u32,
    size: u32,
}

impl RarcEntry {
    fn is_directory(&self) -> bool {
        (self.flags & 0x02) != 0
    }

    fn is_dot_entry(&self) -> bool {
        self.raw_name == b"." || self.raw_name == b".."
    }
}

impl RarcFileIndex {
    fn build(tables: &RarcTables) -> Result<Self> {
        if tables.nodes.is_empty() {
            return Err(FormatError::Unsupported {
                format: FORMAT,
                message: "archive metadata has no root node".to_string(),
            });
        }

        let mut visited_nodes = Vec::new();
        visited_nodes
            .try_reserve_exact(tables.nodes.len())
            .map_err(|error| allocation_error("RARC traversal flags", tables.nodes.len(), error))?;
        visited_nodes.resize(tables.nodes.len(), false);

        let mut files = Vec::new();
        files
            .try_reserve_exact(tables.entries.len().min(MAX_ARCHIVE_ENTRY_COUNT))
            .map_err(|error| allocation_error("RARC file index", tables.entries.len(), error))?;
        let mut stack = Vec::new();
        stack
            .try_reserve_exact(tables.nodes.len().min(MAX_ARCHIVE_DIRECTORY_DEPTH + 1))
            .map_err(|error| allocation_error("RARC traversal stack", tables.nodes.len(), error))?;
        stack.push(PendingNode {
            node_index: 0,
            prefix: String::new(),
            raw_prefix: Vec::new(),
            depth: 0,
        });
        let mut indexed_path_bytes = 0usize;

        while let Some(pending) = stack.pop() {
            if pending.depth > MAX_ARCHIVE_DIRECTORY_DEPTH {
                return Err(resource_limit(
                    "archive directory depth",
                    pending.depth,
                    MAX_ARCHIVE_DIRECTORY_DEPTH,
                ));
            }
            if visited_nodes[pending.node_index] {
                continue;
            }
            visited_nodes[pending.node_index] = true;

            let node = &tables.nodes[pending.node_index];
            let start = node.first_entry_index as usize;
            let end = start
                .checked_add(node.entry_count as usize)
                .ok_or_else(|| invalid_offset(start, tables.entries.len()))?;
            let mut children = Vec::new();
            children
                .try_reserve_exact(node.entry_count as usize)
                .map_err(|error| {
                    allocation_error(
                        "RARC pending directory entries",
                        node.entry_count as usize,
                        error,
                    )
                })?;

            for entry in &tables.entries[start..end] {
                if entry.is_dot_entry() {
                    continue;
                }

                let path = join_archive_path(&pending.prefix, &entry.name)?;
                let raw_path = join_raw_archive_path(&pending.raw_prefix, &entry.raw_name)?;
                let path_bytes = path.len().checked_add(raw_path.len()).ok_or_else(|| {
                    resource_limit(
                        "indexed archive path bytes",
                        usize::MAX,
                        MAX_INDEXED_PATH_BYTES,
                    )
                })?;
                indexed_path_bytes =
                    indexed_path_bytes.checked_add(path_bytes).ok_or_else(|| {
                        resource_limit(
                            "indexed archive path bytes",
                            usize::MAX,
                            MAX_INDEXED_PATH_BYTES,
                        )
                    })?;
                if indexed_path_bytes > MAX_INDEXED_PATH_BYTES {
                    return Err(resource_limit(
                        "indexed archive path bytes",
                        indexed_path_bytes,
                        MAX_INDEXED_PATH_BYTES,
                    ));
                }

                if entry.is_directory() {
                    let depth = pending.depth.checked_add(1).ok_or_else(|| {
                        resource_limit(
                            "archive directory depth",
                            usize::MAX,
                            MAX_ARCHIVE_DIRECTORY_DEPTH,
                        )
                    })?;
                    if depth > MAX_ARCHIVE_DIRECTORY_DEPTH {
                        return Err(resource_limit(
                            "archive directory depth",
                            depth,
                            MAX_ARCHIVE_DIRECTORY_DEPTH,
                        ));
                    }
                    children.push(PendingNode {
                        node_index: entry.data_offset as usize,
                        prefix: path,
                        raw_prefix: raw_path,
                        depth,
                    });
                } else {
                    if files.len() >= MAX_ARCHIVE_ENTRY_COUNT {
                        return Err(resource_limit(
                            "indexed archive file count",
                            files.len() + 1,
                            MAX_ARCHIVE_ENTRY_COUNT,
                        ));
                    }
                    files.push(RarcFileEntry {
                        path,
                        raw_path,
                        flags: entry.flags,
                        data_offset: entry.data_offset,
                        size: entry.size,
                    });
                }
            }

            // Reverse-push preserves the source's depth-first child ordering
            // while keeping traversal on the heap instead of the call stack.
            stack.extend(children.into_iter().rev());
        }

        files.sort_by(|left, right| {
            left.path
                .cmp(&right.path)
                .then_with(|| left.raw_path.cmp(&right.raw_path))
        });
        let mut raw_order = Vec::new();
        raw_order
            .try_reserve_exact(files.len())
            .map_err(|error| allocation_error("RARC raw-path index", files.len(), error))?;
        raw_order.extend(0..files.len());
        raw_order.sort_unstable_by(|left, right| {
            files[*left]
                .raw_path
                .cmp(&files[*right].raw_path)
                .then_with(|| left.cmp(right))
        });

        Ok(Self { files, raw_order })
    }

    fn find_display(&self, path: &str) -> Option<&RarcFileEntry> {
        let index = self
            .files
            .partition_point(|entry| entry.path.as_str() < path);
        self.files.get(index).filter(|entry| entry.path == path)
    }

    fn find_raw(&self, path: &[u8]) -> Option<&RarcFileEntry> {
        let index = self
            .raw_order
            .partition_point(|index| self.files[*index].raw_path.as_slice() < path);
        self.raw_order
            .get(index)
            .map(|index| &self.files[*index])
            .filter(|entry| entry.raw_path == path)
    }
}

struct PendingNode {
    node_index: usize,
    prefix: String,
    raw_prefix: Vec<u8>,
    depth: usize,
}

impl RarcTables {
    fn parse(
        bytes: &[u8],
        info_offset: usize,
        metadata_end: usize,
        data_size: usize,
    ) -> Result<Self> {
        checked_metadata_slice(bytes, info_offset, 0x20, info_offset, metadata_end)?;

        let node_count = be_u32(bytes, info_offset, FORMAT)? as usize;
        check_item_limit("archive node count", node_count, MAX_ARCHIVE_NODE_COUNT)?;
        let node_offset = info_relative_offset(bytes, info_offset, 0x04)?;
        let file_entry_count = be_u32(bytes, info_offset + 0x08, FORMAT)? as usize;
        check_item_limit(
            "archive entry count",
            file_entry_count,
            MAX_ARCHIVE_ENTRY_COUNT,
        )?;
        let file_entry_offset = info_relative_offset(bytes, info_offset, 0x0C)?;
        let string_table_length = be_u32(bytes, info_offset + 0x10, FORMAT)? as usize;
        check_item_limit(
            "archive string-table bytes",
            string_table_length,
            MAX_TABLE_BYTES,
        )?;
        let string_table_offset = info_relative_offset(bytes, info_offset, 0x14)?;
        let string_table = checked_metadata_slice(
            bytes,
            string_table_offset,
            string_table_length,
            info_offset,
            metadata_end,
        )?;

        let node_table_length = checked_table_length(node_count, 0x10, "node table bytes")?;
        checked_metadata_slice(
            bytes,
            node_offset,
            node_table_length,
            info_offset,
            metadata_end,
        )?;
        let mut nodes = Vec::new();
        nodes
            .try_reserve_exact(node_count)
            .map_err(|error| allocation_error("RARC nodes", node_count, error))?;
        for index in 0..node_count {
            let offset = node_offset
                .checked_add(index * 0x10)
                .ok_or_else(|| invalid_offset(node_offset, bytes.len()))?;
            let _node_type = be_u32(bytes, offset, FORMAT)?;
            let _name_offset = be_u32(bytes, offset + 0x04, FORMAT)?;
            let _name_hash = be_u16(bytes, offset + 0x08, FORMAT)?;
            nodes.push(RarcNode {
                entry_count: be_u16(bytes, offset + 0x0A, FORMAT)?,
                first_entry_index: be_u32(bytes, offset + 0x0C, FORMAT)?,
            });
        }

        let entry_table_length =
            checked_table_length(file_entry_count, 0x14, "file-entry table bytes")?;
        checked_metadata_slice(
            bytes,
            file_entry_offset,
            entry_table_length,
            info_offset,
            metadata_end,
        )?;
        let mut entries = Vec::new();
        entries
            .try_reserve_exact(file_entry_count)
            .map_err(|error| allocation_error("RARC entries", file_entry_count, error))?;
        let mut decoded_name_bytes = 0usize;
        for index in 0..file_entry_count {
            let offset = file_entry_offset
                .checked_add(index * 0x14)
                .ok_or_else(|| invalid_offset(file_entry_offset, bytes.len()))?;

            let _file_id = be_u16(bytes, offset, FORMAT)?;
            let _name_hash = be_u16(bytes, offset + 0x02, FORMAT)?;
            let flags_and_name_offset = be_u32(bytes, offset + 0x04, FORMAT)?;
            let flags = (flags_and_name_offset >> 24) as u8;
            let name_offset = (flags_and_name_offset & 0x00FF_FFFF) as usize;
            let (name, raw_name) = read_string(string_table, name_offset)?;
            decoded_name_bytes = decoded_name_bytes
                .checked_add(name.len())
                .and_then(|total| total.checked_add(raw_name.len()))
                .ok_or_else(|| {
                    resource_limit(
                        "decoded archive name bytes",
                        usize::MAX,
                        MAX_INDEXED_PATH_BYTES,
                    )
                })?;
            if decoded_name_bytes > MAX_INDEXED_PATH_BYTES {
                return Err(resource_limit(
                    "decoded archive name bytes",
                    decoded_name_bytes,
                    MAX_INDEXED_PATH_BYTES,
                ));
            }
            entries.push(RarcEntry {
                name,
                raw_name,
                flags,
                data_offset: be_u32(bytes, offset + 0x08, FORMAT)?,
                size: be_u32(bytes, offset + 0x0C, FORMAT)?,
            });
        }

        for node in &nodes {
            let start = node.first_entry_index as usize;
            let end = start
                .checked_add(node.entry_count as usize)
                .ok_or_else(|| invalid_offset(start, entries.len()))?;
            if end > entries.len() {
                return Err(invalid_offset(end, entries.len()));
            }
        }
        for entry in &entries {
            if entry.is_directory() {
                // Retail root `..` entries use `0xFFFF_FFFF` because there is
                // no parent node. Dot entries are never traversal targets.
                if !entry.is_dot_entry() && entry.data_offset as usize >= nodes.len() {
                    return Err(invalid_offset(entry.data_offset as usize, nodes.len()));
                }
            } else {
                let end = (entry.data_offset as usize)
                    .checked_add(entry.size as usize)
                    .ok_or_else(|| invalid_offset(entry.data_offset as usize, data_size))?;
                if end > data_size {
                    return Err(invalid_offset(end, data_size));
                }
            }
        }

        Ok(Self { nodes, entries })
    }
}

fn info_relative_offset(bytes: &[u8], info_offset: usize, field_offset: usize) -> Result<usize> {
    let relative = be_u32(bytes, info_offset + field_offset, FORMAT)? as usize;
    info_offset
        .checked_add(relative)
        .ok_or_else(|| invalid_offset(relative, bytes.len()))
}

fn read_string(string_table: &[u8], offset: usize) -> Result<(String, Vec<u8>)> {
    if offset >= string_table.len() {
        return Err(invalid_offset(offset, string_table.len()));
    }

    let tail = &string_table[offset..];
    let end = tail
        .iter()
        .position(|byte| *byte == 0)
        .ok_or_else(|| invalid_offset(offset, string_table.len()))?;
    check_item_limit("archive name bytes", end, MAX_ARCHIVE_PATH_BYTES)?;
    let raw = tail[..end].to_vec();
    let (decoded, had_errors) = SHIFT_JIS.decode_without_bom_handling(&raw);
    let display = if had_errors {
        percent_encode_name(&raw)
    } else {
        decoded.into_owned()
    };
    Ok((display, raw))
}

fn join_archive_path(prefix: &str, name: &str) -> Result<String> {
    let separator_bytes = usize::from(!prefix.is_empty());
    let length = prefix
        .len()
        .checked_add(separator_bytes)
        .and_then(|length| length.checked_add(name.len()))
        .ok_or_else(|| resource_limit("archive path bytes", usize::MAX, MAX_ARCHIVE_PATH_BYTES))?;
    check_item_limit("archive path bytes", length, MAX_ARCHIVE_PATH_BYTES)?;

    let mut path = String::new();
    path.try_reserve_exact(length)
        .map_err(|error| allocation_error("RARC display-path bytes", length, error))?;
    path.push_str(prefix);
    if separator_bytes != 0 {
        path.push('/');
    }
    path.push_str(name);
    Ok(path)
}

fn join_raw_archive_path(prefix: &[u8], name: &[u8]) -> Result<Vec<u8>> {
    let separator_bytes = usize::from(!prefix.is_empty());
    let length = prefix
        .len()
        .checked_add(separator_bytes)
        .and_then(|length| length.checked_add(name.len()))
        .ok_or_else(|| {
            resource_limit("raw archive path bytes", usize::MAX, MAX_ARCHIVE_PATH_BYTES)
        })?;
    check_item_limit("raw archive path bytes", length, MAX_ARCHIVE_PATH_BYTES)?;

    let mut path = Vec::new();
    path.try_reserve_exact(length)
        .map_err(|error| allocation_error("RARC raw-path bytes", length, error))?;
    path.extend_from_slice(prefix);
    if separator_bytes != 0 {
        path.push(b'/');
    }
    path.extend_from_slice(name);
    Ok(path)
}

fn percent_encode_name(bytes: &[u8]) -> String {
    let mut encoded = String::with_capacity(bytes.len() * 3);
    for byte in bytes {
        use std::fmt::Write as _;
        let _ = write!(encoded, "%{byte:02X}");
    }
    encoded
}

fn build_rarc_document(document: &RarcDocument) -> Result<Vec<u8>> {
    let layout = &document.layout;
    let file_size = layout.file_size as usize;
    let header_size = layout.header_size as usize;
    if header_size < 0x20 || header_size > file_size {
        return Err(invalid_offset(header_size, file_size));
    }
    let data_start = checked_add(header_size, layout.data_offset as usize, file_size)?;
    let data_end = checked_add(data_start, layout.data_size as usize, file_size)?;
    if data_end > file_size {
        return Err(invalid_offset(data_end, file_size));
    }

    let mut output = vec![layout.padding_byte; file_size];
    write_bytes(&mut output, 0, b"RARC")?;
    write_u32(&mut output, 0x04, layout.file_size)?;
    write_u32(&mut output, 0x08, layout.header_size)?;
    write_u32(&mut output, 0x0C, layout.data_offset)?;
    write_u32(&mut output, 0x10, layout.data_size)?;
    write_u32(&mut output, 0x14, layout.mram_data_size)?;
    write_u32(&mut output, 0x18, layout.aram_data_size)?;
    write_u32(&mut output, 0x1C, layout.dvd_data_size)?;

    if !layout.metadata_present {
        if !document.nodes.is_empty() || !document.entries.is_empty() {
            return Err(FormatError::Unsupported {
                format: FORMAT,
                message: "RARC document has records but no metadata block".to_string(),
            });
        }
        return Ok(output);
    }

    let node_count = u32::try_from(document.nodes.len()).map_err(|_| {
        resource_limit(
            "archive node count",
            document.nodes.len(),
            u32::MAX as usize,
        )
    })?;
    let entry_count = u32::try_from(document.entries.len()).map_err(|_| {
        resource_limit(
            "archive entry count",
            document.entries.len(),
            u32::MAX as usize,
        )
    })?;
    let node_start = checked_add(header_size, layout.node_offset as usize, file_size)?;
    let entry_start = checked_add(header_size, layout.entry_offset as usize, file_size)?;
    let string_start = checked_add(header_size, layout.string_table_offset as usize, file_size)?;
    let node_length = document
        .nodes
        .len()
        .checked_mul(0x10)
        .ok_or_else(|| resource_limit("node table bytes", usize::MAX, MAX_TABLE_BYTES))?;
    let entry_length = document
        .entries
        .len()
        .checked_mul(0x14)
        .ok_or_else(|| resource_limit("file-entry table bytes", usize::MAX, MAX_TABLE_BYTES))?;
    let sections = [
        (header_size, 0x20usize),
        (node_start, node_length),
        (entry_start, entry_length),
        (string_start, layout.string_table_size as usize),
    ];
    for &(start, length) in &sections {
        let end = checked_add(start, length, file_size)?;
        if start < header_size || end > data_start {
            return Err(invalid_offset(end.max(start), data_start));
        }
    }
    for left in 0..sections.len() {
        for right in left + 1..sections.len() {
            if ranges_overlap(sections[left], sections[right]) {
                return Err(FormatError::Unsupported {
                    format: FORMAT,
                    message: "RARC metadata sections overlap".to_string(),
                });
            }
        }
    }

    write_u32(&mut output, header_size, node_count)?;
    write_u32(&mut output, header_size + 0x04, layout.node_offset)?;
    write_u32(&mut output, header_size + 0x08, entry_count)?;
    write_u32(&mut output, header_size + 0x0C, layout.entry_offset)?;
    write_u32(&mut output, header_size + 0x10, layout.string_table_size)?;
    write_u32(&mut output, header_size + 0x14, layout.string_table_offset)?;
    write_u16(&mut output, header_size + 0x18, layout.next_free_file_id)?;
    write_bytes(&mut output, header_size + 0x1A, &[layout.sync_file_ids])?;
    write_bytes(&mut output, header_size + 0x1B, &layout.info_reserved)?;

    let mut string_table = vec![layout.padding_byte; layout.string_table_size as usize];
    let mut claimed_strings = vec![false; string_table.len()];
    for (index, node) in document.nodes.iter().enumerate() {
        let offset = node_start + index * 0x10;
        write_bytes(&mut output, offset, &node.node_type)?;
        write_u32(&mut output, offset + 0x04, node.name_offset)?;
        write_u16(&mut output, offset + 0x08, node.name_hash)?;
        write_u16(&mut output, offset + 0x0A, node.entry_count)?;
        write_u32(&mut output, offset + 0x0C, node.first_entry_index)?;
        write_name(
            &mut string_table,
            &mut claimed_strings,
            node.name_offset,
            &node.raw_name,
        )?;
    }

    let mut data_section = vec![layout.padding_byte; layout.data_size as usize];
    let mut claimed_data = vec![false; data_section.len()];
    for (index, entry) in document.entries.iter().enumerate() {
        if entry.name_offset > 0x00FF_FFFF {
            return Err(resource_limit(
                "archive name offset",
                entry.name_offset as usize,
                0x00FF_FFFF,
            ));
        }
        let offset = entry_start + index * 0x14;
        write_u16(&mut output, offset, entry.file_id)?;
        write_u16(&mut output, offset + 0x02, entry.name_hash)?;
        write_u32(
            &mut output,
            offset + 0x04,
            ((entry.flags as u32) << 24) | entry.name_offset,
        )?;
        write_u32(&mut output, offset + 0x08, entry.data_offset)?;
        write_u32(&mut output, offset + 0x0C, entry.size)?;
        write_u32(&mut output, offset + 0x10, entry.reserved)?;
        write_name(
            &mut string_table,
            &mut claimed_strings,
            entry.name_offset,
            &entry.raw_name,
        )?;

        match (entry.is_directory(), &entry.data) {
            (true, None) => {}
            (true, Some(_)) => {
                return Err(FormatError::Unsupported {
                    format: FORMAT,
                    message: format!("directory entry {index} contains file data"),
                });
            }
            (false, Some(data)) => {
                if data.len() != entry.size as usize {
                    return Err(FormatError::Unsupported {
                        format: FORMAT,
                        message: format!(
                            "file entry {index} declares {} bytes but contains {}",
                            entry.size,
                            data.len()
                        ),
                    });
                }
                write_claimed(
                    &mut data_section,
                    &mut claimed_data,
                    entry.data_offset as usize,
                    data,
                    "overlapping RARC file payloads disagree",
                )?;
            }
            (false, None) => {
                return Err(FormatError::Unsupported {
                    format: FORMAT,
                    message: format!("file entry {index} has no generated payload"),
                });
            }
        }
    }
    write_bytes(&mut output, string_start, &string_table)?;
    write_bytes(&mut output, data_start, &data_section)?;
    Ok(output)
}

fn reject_noncanonical_import(source: &[u8], rebuilt: &[u8]) -> Result<()> {
    if source == rebuilt {
        return Ok(());
    }
    let difference = source
        .iter()
        .zip(rebuilt)
        .position(|(source, rebuilt)| source != rebuilt)
        .unwrap_or_else(|| source.len().min(rebuilt.len()));
    Err(FormatError::Unsupported {
        format: FORMAT,
        message: format!(
            "source-free import rejects noncanonical RARC creator data at offset {difference:#X}"
        ),
    })
}

fn write_name(table: &mut [u8], claimed: &mut [bool], offset: u32, name: &[u8]) -> Result<()> {
    let mut encoded = Vec::new();
    encoded
        .try_reserve_exact(name.len() + 1)
        .map_err(|error| allocation_error("archive name bytes", name.len() + 1, error))?;
    encoded.extend_from_slice(name);
    encoded.push(0);
    write_claimed(
        table,
        claimed,
        offset as usize,
        &encoded,
        "overlapping RARC names disagree",
    )
}

fn write_claimed(
    destination: &mut [u8],
    claimed: &mut [bool],
    offset: usize,
    bytes: &[u8],
    conflict: &'static str,
) -> Result<()> {
    let end = offset
        .checked_add(bytes.len())
        .ok_or_else(|| invalid_offset(offset, destination.len()))?;
    if end > destination.len() {
        return Err(invalid_offset(end, destination.len()));
    }
    for (index, value) in bytes.iter().copied().enumerate() {
        let target = offset + index;
        if claimed[target] && destination[target] != value {
            return Err(FormatError::Unsupported {
                format: FORMAT,
                message: conflict.to_string(),
            });
        }
        destination[target] = value;
        claimed[target] = true;
    }
    Ok(())
}

fn write_bytes(destination: &mut [u8], offset: usize, bytes: &[u8]) -> Result<()> {
    let destination_len = destination.len();
    let end = offset
        .checked_add(bytes.len())
        .ok_or_else(|| invalid_offset(offset, destination_len))?;
    let target = destination
        .get_mut(offset..end)
        .ok_or_else(|| invalid_offset(end, destination_len))?;
    target.copy_from_slice(bytes);
    Ok(())
}

fn write_u16(destination: &mut [u8], offset: usize, value: u16) -> Result<()> {
    write_bytes(destination, offset, &value.to_be_bytes())
}

fn write_u32(destination: &mut [u8], offset: usize, value: u32) -> Result<()> {
    write_bytes(destination, offset, &value.to_be_bytes())
}

fn align_up(value: usize, alignment: usize) -> Result<usize> {
    if alignment == 0 || !alignment.is_power_of_two() {
        return Err(FormatError::Unsupported {
            format: FORMAT,
            message: format!("RARC alignment {alignment} is not a power of two"),
        });
    }
    value
        .checked_add(alignment - 1)
        .map(|value| value & !(alignment - 1))
        .ok_or_else(|| resource_limit("aligned archive bytes", usize::MAX, u32::MAX as usize))
}

fn checked_add(left: usize, right: usize, limit: usize) -> Result<usize> {
    let value = left
        .checked_add(right)
        .ok_or_else(|| invalid_offset(left, limit))?;
    if value > limit {
        return Err(invalid_offset(value, limit));
    }
    Ok(value)
}

fn ranges_overlap(left: (usize, usize), right: (usize, usize)) -> bool {
    if left.1 == 0 || right.1 == 0 {
        return false;
    }
    left.0 < right.0 + right.1 && right.0 < left.0 + left.1
}

fn rarc_data_partition(flags: u8) -> usize {
    if (flags & 0x10) != 0 {
        0
    } else if (flags & 0x20) != 0 {
        1
    } else {
        2
    }
}

fn checked_table_length(count: usize, stride: usize, resource: &'static str) -> Result<usize> {
    let requested = count
        .checked_mul(stride)
        .ok_or(FormatError::ResourceLimit {
            format: FORMAT,
            resource,
            requested: usize::MAX,
            limit: MAX_TABLE_BYTES,
        })?;
    if requested > MAX_TABLE_BYTES {
        return Err(FormatError::ResourceLimit {
            format: FORMAT,
            resource,
            requested,
            limit: MAX_TABLE_BYTES,
        });
    }
    Ok(requested)
}

fn checked_metadata_slice(
    bytes: &[u8],
    offset: usize,
    length: usize,
    metadata_start: usize,
    metadata_end: usize,
) -> Result<&[u8]> {
    if offset < metadata_start {
        return Err(invalid_offset(offset, metadata_end));
    }
    let end = offset
        .checked_add(length)
        .ok_or_else(|| invalid_offset(offset, metadata_end))?;
    if end > metadata_end {
        return Err(invalid_offset(end, metadata_end));
    }
    checked_slice(FORMAT, bytes, offset, length)
}

fn check_item_limit(resource: &'static str, requested: usize, limit: usize) -> Result<()> {
    if requested > limit {
        return Err(resource_limit(resource, requested, limit));
    }
    Ok(())
}

fn resource_limit(resource: &'static str, requested: usize, limit: usize) -> FormatError {
    FormatError::ResourceLimit {
        format: FORMAT,
        resource,
        requested,
        limit,
    }
}

fn unsupported_rarc(message: impl Into<String>) -> FormatError {
    FormatError::Unsupported {
        format: FORMAT,
        message: message.into(),
    }
}

fn allocation_error(
    resource: &'static str,
    count: usize,
    error: std::collections::TryReserveError,
) -> FormatError {
    FormatError::Unsupported {
        format: FORMAT,
        message: format!("could not reserve {count} {resource}: {error}"),
    }
}

fn invalid_offset(offset: usize, len: usize) -> FormatError {
    FormatError::InvalidOffset {
        format: FORMAT,
        offset,
        len,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn single_file_rarc(raw_name: &[u8], data: &[u8]) -> Vec<u8> {
        RarcDocument {
            layout: blank_layout(),
            nodes: vec![RarcNodeRecord {
                node_type: [0; 4],
                name_offset: 0,
                name_hash: 0,
                raw_name: b"root".to_vec(),
                entry_count: 3,
                first_entry_index: 0,
            }],
            entries: vec![
                RarcEntryRecord {
                    file_id: 0,
                    name_hash: 0,
                    flags: RARC_FILE_FLAGS,
                    name_offset: 0,
                    raw_name: raw_name.to_vec(),
                    data_offset: 0,
                    size: 0,
                    reserved: 0,
                    data: Some(data.to_vec()),
                },
                RarcEntryRecord {
                    file_id: u16::MAX,
                    name_hash: 0,
                    flags: RARC_DIRECTORY_FLAGS,
                    name_offset: 0,
                    raw_name: b".".to_vec(),
                    data_offset: 0,
                    size: 0,
                    reserved: 0,
                    data: None,
                },
                RarcEntryRecord {
                    file_id: u16::MAX,
                    name_hash: 0,
                    flags: RARC_DIRECTORY_FLAGS,
                    name_offset: 0,
                    raw_name: b"..".to_vec(),
                    data_offset: u32::MAX,
                    size: 0,
                    reserved: 0,
                    data: None,
                },
            ],
        }
        .to_bytes()
        .unwrap()
    }

    fn blank_layout() -> RarcLayout {
        RarcLayout {
            file_size: 0,
            header_size: 0,
            data_offset: 0,
            data_size: 0,
            mram_data_size: 0,
            aram_data_size: 0,
            dvd_data_size: 0,
            metadata_present: true,
            node_offset: 0,
            entry_offset: 0,
            string_table_offset: 0,
            string_table_size: 0,
            next_free_file_id: 0,
            sync_file_ids: 0,
            info_reserved: [0; 5],
            alignment: 0,
            padding_byte: 0,
        }
    }

    #[test]
    fn preserves_minimal_rarc_bytes() {
        let mut bytes = vec![0; 0x20];
        bytes[0..4].copy_from_slice(b"RARC");
        bytes[4..8].copy_from_slice(&(0x20u32.to_be_bytes()));
        bytes[8..12].copy_from_slice(&(0x20u32.to_be_bytes()));

        let archive = RarcArchive::parse(&bytes).unwrap();
        assert_eq!(archive.header().file_size, 0x20);
        assert_eq!(archive.to_bytes(), bytes);
    }

    #[test]
    fn structural_document_rebuilds_without_an_archive_buffer() {
        let source = single_file_rarc(b"file.bin", b"generated child");
        let document = RarcDocument::parse(&source).unwrap();

        assert_eq!(document.nodes.len(), 1);
        assert_eq!(
            document
                .entries
                .iter()
                .filter(|entry| !entry.is_directory())
                .count(),
            1
        );
        assert_eq!(document.to_bytes().unwrap(), source);
    }

    #[test]
    fn structural_export_recomputes_derived_creator_fields() {
        let source = single_file_rarc(b"file.bin", b"generated child");
        let mut document = RarcDocument::parse(&source).unwrap();

        document.layout.file_size = 1;
        document.layout.header_size = 1;
        document.layout.data_offset = 1;
        document.layout.data_size = 1;
        document.layout.next_free_file_id = 0;
        document.layout.sync_file_ids = 0;
        document.layout.info_reserved = [0xCC; 5];
        document.layout.alignment = 1;
        document.layout.padding_byte = 0xCC;
        document.nodes[0].node_type = *b"NOPE";
        document.nodes[0].name_offset = 0x1234;
        document.nodes[0].name_hash = 0x5678;
        document.entries[0].file_id = 0xBEEF;
        document.entries[0].name_offset = 0x1234;
        document.entries[0].name_hash = 0x5678;
        document.entries[0].data_offset = 0x1234;
        document.entries[0].size = 1;
        document.entries[0].reserved = 0xDEAD_BEEF;

        assert_eq!(document.to_bytes().unwrap(), source);
    }

    #[test]
    fn structural_import_rejects_loader_ignored_creator_words() {
        let source = single_file_rarc(b"file.bin", b"generated child");

        let mut info_reserved = source.clone();
        info_reserved[0x20 + 0x1B] = 1;
        assert!(matches!(
            RarcDocument::parse(info_reserved),
            Err(FormatError::Unsupported { .. })
        ));

        let mut entry_reserved = source;
        let info = be_u32(&entry_reserved, 0x08, FORMAT).unwrap() as usize;
        let entry_offset = be_u32(&entry_reserved, info + 0x0C, FORMAT).unwrap() as usize;
        entry_reserved[info + entry_offset + 0x13] = 1;
        assert!(matches!(
            RarcDocument::parse(entry_reserved),
            Err(FormatError::Unsupported { .. })
        ));
    }

    #[test]
    fn every_ingested_payload_can_be_taken_and_regenerated() {
        let source = single_file_rarc(b"file.bin", b"generated child");
        let mut document = RarcDocument::parse(&source).unwrap();
        let mut generated = Vec::new();
        for (index, entry) in document.entries.iter_mut().enumerate() {
            if !entry.is_directory() {
                generated.push((index, entry.take_file_data().unwrap()));
            }
        }

        assert!(document.to_bytes().is_err());
        for (index, data) in generated {
            document.entries[index].set_file_data(data).unwrap();
        }
        assert_eq!(document.to_bytes().unwrap(), source);

        let data = document.take_file_data(b"file.bin").unwrap();
        assert_eq!(data, b"generated child");
        document.set_file_data(b"file.bin", data).unwrap();
        assert_eq!(document.to_bytes().unwrap(), source);
    }

    #[test]
    fn rarc_hash_uses_the_jkernel_wrapping_multiplier() {
        assert_eq!(rarc_name_hash(b"."), 0x2E);
        assert_eq!(rarc_name_hash(b".."), 0xB8);
        assert_eq!(rarc_name_hash(b"scene"), 0x3410);
    }

    #[test]
    fn high_level_builder_creates_a_deterministic_scene_tree() {
        let mut builder = RarcBuilder::new_scene();
        builder
            .insert_file(b"map/map/map.bmd", b"model".to_vec())
            .unwrap();
        builder
            .insert_file(b"map/map.col", b"collision".to_vec())
            .unwrap();
        builder
            .insert_file(b"map/scene.bin", b"placement".to_vec())
            .unwrap();

        let document = builder.build().unwrap();
        let first = document.to_bytes().unwrap();
        let second = builder.build().unwrap().to_bytes().unwrap();
        assert_eq!(first, second);
        let reopened = RarcDocument::parse(&first).unwrap();
        assert_eq!(reopened.nodes[0].raw_name, b"scene");
        assert_eq!(
            reopened.entry_by_raw_path(b"map/map/map.bmd").unwrap().data,
            Some(b"model".to_vec())
        );
        assert_eq!(
            reopened.entry_by_raw_path(b"map/map.col").unwrap().data,
            Some(b"collision".to_vec())
        );
        assert_eq!(reopened.to_bytes().unwrap(), first);
    }

    #[test]
    fn high_level_builder_replaces_removes_and_rehydrates_documents() {
        let mut builder = RarcBuilder::new_scene();
        builder
            .insert_file(b"map/map.col", b"old".to_vec())
            .unwrap();
        assert_eq!(
            builder
                .replace_file(b"map/map.col", b"new".to_vec())
                .unwrap(),
            b"old"
        );
        builder
            .insert_file(b"map/map/map.bmd", b"model".to_vec())
            .unwrap();
        let document = builder.build().unwrap();
        let mut rebuilt = RarcBuilder::from_document(&document).unwrap();
        assert!(rebuilt.contains_file(b"/map/map.col"));
        assert_eq!(rebuilt.remove_file(b"map/map.col").unwrap(), b"new");
        assert!(!rebuilt.contains_file(b"map/map.col"));
        assert!(rebuilt.contains_file(b"map/map/map.bmd"));
        let reopened = RarcDocument::parse(rebuilt.build().unwrap().to_bytes().unwrap()).unwrap();
        assert!(reopened.entry_by_raw_path(b"map/map.col").is_none());
        assert!(reopened.entry_by_raw_path(b"map/map/map.bmd").is_some());
    }

    #[test]
    fn high_level_builder_rejects_conflicts_and_traversal() {
        let mut builder = RarcBuilder::new_scene();
        builder.insert_file(b"map/file.bin", Vec::new()).unwrap();
        assert!(builder.insert_file(b"map/file.bin", Vec::new()).is_err());
        assert!(builder
            .insert_file(b"map/file.bin/child", Vec::new())
            .is_err());
        assert!(builder.insert_file(b"map/../file.bin", Vec::new()).is_err());
        assert!(builder.insert_file(b"/", Vec::new()).is_err());
        assert!(builder.remove_file(b"map").is_err());
    }

    #[test]
    fn high_level_builder_encodes_an_empty_scene_root() {
        let bytes = RarcBuilder::new_scene()
            .build()
            .unwrap()
            .to_bytes()
            .unwrap();
        let reopened = RarcDocument::parse(&bytes).unwrap();
        assert_eq!(reopened.nodes.len(), 1);
        assert_eq!(reopened.nodes[0].raw_name, b"scene");
        assert_eq!(reopened.nodes[0].entry_count, 2);
        assert!(RarcArchive::parse(&bytes)
            .unwrap()
            .file_entries()
            .is_empty());
    }

    #[test]
    fn lists_file_entries_from_rarc_tables() {
        let string_table = b"root\0.\0..\0map.bmd\0";
        let root_name = 0u32;
        let dot_name = 5u32;
        let dotdot_name = 7u32;
        let file_name = 10u32;

        let info_offset = 0x20usize;
        let node_offset = 0x20usize;
        let file_entry_offset = 0x30usize;
        let string_table_offset = 0x6Cusize;
        let file_data_offset = string_table_offset + string_table.len();
        let file_size = info_offset + file_data_offset + 4;

        let mut bytes = vec![0; file_size];
        bytes[0..4].copy_from_slice(b"RARC");
        bytes[4..8].copy_from_slice(&(file_size as u32).to_be_bytes());
        bytes[8..12].copy_from_slice(&(info_offset as u32).to_be_bytes());
        bytes[12..16].copy_from_slice(&(file_data_offset as u32).to_be_bytes());
        bytes[16..20].copy_from_slice(&(4u32.to_be_bytes()));

        bytes[info_offset..info_offset + 4].copy_from_slice(&(1u32.to_be_bytes()));
        bytes[info_offset + 4..info_offset + 8]
            .copy_from_slice(&(node_offset as u32).to_be_bytes());
        bytes[info_offset + 8..info_offset + 12].copy_from_slice(&(3u32.to_be_bytes()));
        bytes[info_offset + 12..info_offset + 16]
            .copy_from_slice(&(file_entry_offset as u32).to_be_bytes());
        bytes[info_offset + 16..info_offset + 20]
            .copy_from_slice(&(string_table.len() as u32).to_be_bytes());
        bytes[info_offset + 20..info_offset + 24]
            .copy_from_slice(&(string_table_offset as u32).to_be_bytes());

        let node = info_offset + node_offset;
        bytes[node..node + 4].copy_from_slice(b"ROOT");
        bytes[node + 4..node + 8].copy_from_slice(&root_name.to_be_bytes());
        bytes[node + 10..node + 12].copy_from_slice(&(3u16.to_be_bytes()));

        write_entry(
            &mut bytes,
            info_offset + file_entry_offset,
            0x02,
            dot_name,
            0,
            0,
        );
        write_entry(
            &mut bytes,
            info_offset + file_entry_offset + 0x14,
            0x02,
            dotdot_name,
            u32::MAX,
            0,
        );
        write_entry(
            &mut bytes,
            info_offset + file_entry_offset + 0x28,
            0x11,
            file_name,
            0,
            4,
        );
        bytes[info_offset + string_table_offset
            ..info_offset + string_table_offset + string_table.len()]
            .copy_from_slice(string_table);

        let archive = RarcArchive::parse(&bytes).unwrap();
        let files = archive.files().unwrap();
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].path, "map.bmd");
        assert_eq!(files[0].raw_path, b"map.bmd");
        assert_eq!(files[0].size, 4);
    }

    #[test]
    fn decodes_shift_jis_names_and_retains_the_source_bytes() {
        // Shift-JIS for `テスト`.
        let encoded = [0x83, 0x65, 0x83, 0x58, 0x83, 0x67, 0];
        let (name, raw) = read_string(&encoded, 0).unwrap();
        assert_eq!(name, "テスト");
        assert_eq!(raw, &encoded[..encoded.len() - 1]);
    }

    #[test]
    fn invalid_shift_jis_names_have_a_lossless_display_fallback() {
        let (name, raw) = read_string(&[0x81, 0], 0).unwrap();
        assert_eq!(name, "%81");
        assert_eq!(raw, [0x81]);
    }

    #[test]
    fn shift_jis_archive_paths_round_trip_through_text_and_raw_lookup() {
        let raw_name = [0x83, 0x65, 0x83, 0x58, 0x83, 0x67];
        let bytes = single_file_rarc(&raw_name, b"SMS");
        let archive = RarcArchive::parse(&bytes).unwrap();
        let files = archive.files().unwrap();

        assert_eq!(files[0].path, "テスト");
        assert_eq!(files[0].raw_path, raw_name);
        assert_eq!(archive.file_bytes("テスト").unwrap(), b"SMS");
        assert_eq!(archive.file_bytes_raw(&raw_name).unwrap(), b"SMS");
        assert_eq!(archive.to_bytes(), bytes);
    }

    #[test]
    fn invalid_shift_jis_archive_path_remains_addressable_by_raw_bytes() {
        let bytes = single_file_rarc(&[0x81], b"raw");
        let archive = RarcArchive::parse(bytes).unwrap();
        assert_eq!(archive.files().unwrap()[0].path, "%81");
        assert_eq!(archive.file_bytes_raw(&[0x81]).unwrap(), b"raw");
    }

    #[test]
    fn every_truncated_archive_prefix_fails_before_returning_file_data() {
        let bytes = single_file_rarc(b"file.bin", b"data");
        assert_eq!(
            RarcArchive::parse(&bytes)
                .and_then(|archive| archive.file_bytes("file.bin"))
                .unwrap(),
            b"data"
        );
        for length in 0..bytes.len() {
            assert!(
                RarcArchive::parse(&bytes[..length])
                    .and_then(|archive| archive.file_bytes("file.bin"))
                    .is_err(),
                "truncated prefix of {length} bytes returned file data"
            );
        }
    }

    #[test]
    fn rejects_unreasonable_table_counts_before_reserving() {
        let mut bytes = vec![0; 0x60];
        bytes[..4].copy_from_slice(b"RARC");
        bytes[4..8].copy_from_slice(&0x60u32.to_be_bytes());
        bytes[8..12].copy_from_slice(&0x20u32.to_be_bytes());
        bytes[12..16].copy_from_slice(&0x40u32.to_be_bytes());
        bytes[0x20..0x24].copy_from_slice(&u32::MAX.to_be_bytes());

        assert!(matches!(
            RarcArchive::parse(&bytes),
            Err(FormatError::ResourceLimit { .. })
        ));
    }

    #[test]
    fn preserves_physical_bytes_after_the_declared_archive() {
        let declared = single_file_rarc(b"file.bin", b"data");
        let mut physical = declared.clone();
        physical.extend_from_slice(b"unrelated trailing bytes");

        let archive = RarcArchive::parse(&physical).unwrap();
        assert_eq!(archive.file_bytes("file.bin").unwrap(), b"data");
        assert_eq!(archive.source_bytes(), physical);
        assert_eq!(archive.header().file_size as usize, declared.len());
    }

    #[test]
    fn physical_trailing_bytes_cannot_satisfy_declared_data_bounds() {
        let mut bytes = single_file_rarc(b"file.bin", b"data");
        let data_start = be_u32(&bytes, 0x08, FORMAT).unwrap() as usize
            + be_u32(&bytes, 0x0C, FORMAT).unwrap() as usize;
        bytes[4..8].copy_from_slice(&((data_start + 2) as u32).to_be_bytes());
        bytes[16..20].copy_from_slice(&2u32.to_be_bytes());

        assert!(matches!(
            RarcArchive::parse(&bytes),
            Err(FormatError::InvalidOffset { .. })
        ));
    }

    #[test]
    fn rejects_archive_paths_over_the_explicit_budget() {
        let oversized_name = vec![b'a'; MAX_ARCHIVE_PATH_BYTES + 1];

        assert!(matches!(
            validate_rarc_name("entry", 0, &oversized_name),
            Err(FormatError::ResourceLimit {
                resource: "archive name bytes",
                ..
            })
        ));
    }

    #[test]
    fn deeply_nested_directories_hit_a_depth_limit_without_recursing() {
        let bytes = directory_chain_rarc(MAX_ARCHIVE_DIRECTORY_DEPTH + 1);

        assert!(matches!(
            RarcArchive::parse(&bytes),
            Err(FormatError::ResourceLimit {
                resource: "archive directory depth",
                ..
            })
        ));
    }

    #[test]
    #[ignore = "requires SMS_RARC_PATH to point to a retail RARC or Yaz0-wrapped RARC"]
    fn parses_an_external_retail_archive() {
        let path = std::env::var_os("SMS_RARC_PATH")
            .map(std::path::PathBuf::from)
            .expect("set SMS_RARC_PATH to a retail RARC or Yaz0-wrapped RARC");
        let source = std::fs::read(path).expect("read retail archive");
        let bytes = if source.starts_with(b"Yaz0") {
            crate::decode_yaz0(&source).expect("decode retail Yaz0 archive")
        } else {
            source
        };
        let archive = RarcArchive::parse(bytes).expect("parse retail RARC");

        assert!(!archive.file_entries().is_empty());
        for entry in archive.file_entries().iter().take(16) {
            archive
                .file_bytes_raw(&entry.raw_path)
                .expect("read indexed retail entry");
        }
    }

    #[test]
    #[ignore = "requires SMS_RARC_PATH to point to a retail RARC or Yaz0-wrapped RARC"]
    fn structurally_rebuilds_an_external_retail_archive_byte_for_byte() {
        let path = std::env::var_os("SMS_RARC_PATH")
            .map(std::path::PathBuf::from)
            .expect("set SMS_RARC_PATH to a retail RARC or Yaz0-wrapped RARC");
        let source = std::fs::read(path).expect("read retail archive");
        let raw = if source.starts_with(b"Yaz0") {
            crate::decode_yaz0(&source).expect("decode retail Yaz0 archive")
        } else {
            source
        };
        let mut document = RarcDocument::parse(&raw).expect("parse structural retail RARC");

        assert_eq!(document.to_bytes().expect("rebuild imported layout"), raw);

        let mut generated = Vec::new();
        for (index, entry) in document.entries.iter_mut().enumerate() {
            if !entry.is_directory() {
                generated.push((index, entry.take_file_data().expect("take child payload")));
            }
        }
        assert!(document.to_bytes().is_err());
        for (index, data) in generated {
            document.entries[index]
                .set_file_data(data)
                .expect("install regenerated child payload");
        }
        document
            .canonicalize_layout()
            .expect("canonicalize retail layout");
        let rebuilt = document.to_bytes().expect("rebuild canonical layout");
        let first_difference = rebuilt
            .iter()
            .zip(&raw)
            .position(|(left, right)| left != right);
        assert!(
            rebuilt == raw,
            "canonical RARC differs at {first_difference:?} (rebuilt {}, retail {})",
            rebuilt.len(),
            raw.len()
        );
    }

    #[test]
    #[ignore = "requires SMS_RETAIL_STAGE_DIR to point to the 107 retail stage archives"]
    fn census_validates_strict_source_free_rarc_creator_layouts() {
        let root = std::env::var_os("SMS_RETAIL_STAGE_DIR")
            .map(std::path::PathBuf::from)
            .expect("set SMS_RETAIL_STAGE_DIR to the retail stage archive directory");
        let mut paths: Vec<_> = std::fs::read_dir(root)
            .expect("read retail stage directory")
            .map(|entry| entry.expect("read retail stage entry").path())
            .filter(|path| path.extension().is_some_and(|extension| extension == "szs"))
            .collect();
        paths.sort();

        for path in &paths {
            let source = std::fs::read(path).expect("read retail stage archive");
            assert_eq!(&source[8..16], &[0; 8], "{}", path.display());
            let raw = crate::decode_yaz0(&source).expect("decode retail Yaz0");
            let document = RarcDocument::parse(&raw).unwrap_or_else(|error| {
                panic!("strict RARC parse failed for {}: {error}", path.display())
            });
            assert_eq!(
                document.to_bytes().expect("rebuild strict RARC"),
                raw,
                "{}",
                path.display()
            );
        }
        assert_eq!(paths.len(), 107, "unexpected retail stage count");
    }

    #[test]
    #[ignore = "requires SMS_RETAIL_STAGE_DIR to point to the 107 retail stage archives"]
    fn census_rebuilds_all_retail_stage_containers_and_yaz0_streams() {
        let root = std::env::var_os("SMS_RETAIL_STAGE_DIR")
            .map(std::path::PathBuf::from)
            .expect("set SMS_RETAIL_STAGE_DIR to the retail stage archive directory");
        let mut paths: Vec<_> = std::fs::read_dir(root)
            .expect("read retail stage directory")
            .map(|entry| entry.expect("read retail stage entry").path())
            .filter(|path| path.extension().is_some_and(|extension| extension == "szs"))
            .collect();
        paths.sort();

        let mut yaz0_mismatches = Vec::new();
        let mut rarc_mismatches = Vec::new();
        let mut canonical_mismatches = Vec::new();
        for path in &paths {
            let source = std::fs::read(path).expect("read retail stage archive");
            let yaz0 = crate::Yaz0Document::parse(&source).expect("decode retail Yaz0");
            let encoded = yaz0.to_bytes().expect("re-encode retail Yaz0");
            if encoded != source {
                yaz0_mismatches.push((
                    path.file_name().unwrap().to_owned(),
                    first_difference(&encoded, &source),
                ));
            }

            let mut rarc = RarcDocument::parse(&yaz0.data).expect("parse structural retail RARC");
            let rebuilt = rarc.to_bytes().expect("rebuild imported RARC layout");
            if rebuilt != yaz0.data {
                rarc_mismatches.push((
                    path.file_name().unwrap().to_owned(),
                    first_difference(&rebuilt, &yaz0.data),
                ));
            }
            rarc.canonicalize_layout()
                .expect("canonicalize retail RARC layout");
            let canonical = rarc.to_bytes().expect("rebuild canonical RARC layout");
            if canonical != yaz0.data {
                canonical_mismatches.push((
                    path.file_name().unwrap().to_owned(),
                    first_difference(&canonical, &yaz0.data),
                ));
            }
        }

        eprintln!(
            "retail census: {} stages, {} Yaz0 mismatches, {} imported-layout RARC mismatches, {} canonical-layout RARC mismatches",
            paths.len(),
            yaz0_mismatches.len(),
            rarc_mismatches.len(),
            canonical_mismatches.len()
        );
        eprintln!("Yaz0 mismatches: {yaz0_mismatches:?}");
        eprintln!("RARC mismatches: {rarc_mismatches:?}");
        eprintln!("canonical RARC mismatches: {canonical_mismatches:?}");
        assert_eq!(paths.len(), 107, "unexpected retail stage count");
        assert!(yaz0_mismatches.is_empty());
        assert!(rarc_mismatches.is_empty());
        assert!(canonical_mismatches.is_empty());
    }

    fn first_difference(left: &[u8], right: &[u8]) -> Option<usize> {
        left.iter()
            .zip(right)
            .position(|(left, right)| left != right)
            .or_else(|| (left.len() != right.len()).then_some(left.len().min(right.len())))
    }

    fn directory_chain_rarc(directory_count: usize) -> Vec<u8> {
        const INFO_OFFSET: usize = 0x20;
        const NODE_OFFSET: usize = 0x20;
        const STRING_TABLE: &[u8] = b"root\0d\0f\0";
        const DIRECTORY_NAME_OFFSET: u32 = 5;
        const FILE_NAME_OFFSET: u32 = 7;

        let node_count = directory_count + 1;
        let entry_count = node_count;
        let entry_offset = NODE_OFFSET + node_count * 0x10;
        let string_offset = entry_offset + entry_count * 0x14;
        let data_offset = string_offset + STRING_TABLE.len();
        let file_size = INFO_OFFSET + data_offset;
        let mut bytes = vec![0; file_size];

        bytes[..4].copy_from_slice(b"RARC");
        bytes[4..8].copy_from_slice(&(file_size as u32).to_be_bytes());
        bytes[8..12].copy_from_slice(&(INFO_OFFSET as u32).to_be_bytes());
        bytes[12..16].copy_from_slice(&(data_offset as u32).to_be_bytes());

        bytes[INFO_OFFSET..INFO_OFFSET + 4].copy_from_slice(&(node_count as u32).to_be_bytes());
        bytes[INFO_OFFSET + 4..INFO_OFFSET + 8]
            .copy_from_slice(&(NODE_OFFSET as u32).to_be_bytes());
        bytes[INFO_OFFSET + 8..INFO_OFFSET + 12]
            .copy_from_slice(&(entry_count as u32).to_be_bytes());
        bytes[INFO_OFFSET + 12..INFO_OFFSET + 16]
            .copy_from_slice(&(entry_offset as u32).to_be_bytes());
        bytes[INFO_OFFSET + 16..INFO_OFFSET + 20]
            .copy_from_slice(&(STRING_TABLE.len() as u32).to_be_bytes());
        bytes[INFO_OFFSET + 20..INFO_OFFSET + 24]
            .copy_from_slice(&(string_offset as u32).to_be_bytes());

        for index in 0..node_count {
            let node = INFO_OFFSET + NODE_OFFSET + index * 0x10;
            bytes[node..node + 4].copy_from_slice(b"DIR ");
            bytes[node + 10..node + 12].copy_from_slice(&1u16.to_be_bytes());
            bytes[node + 12..node + 16].copy_from_slice(&(index as u32).to_be_bytes());

            let entry = INFO_OFFSET + entry_offset + index * 0x14;
            if index < directory_count {
                write_entry(
                    &mut bytes,
                    entry,
                    0x02,
                    DIRECTORY_NAME_OFFSET,
                    (index + 1) as u32,
                    0,
                );
            } else {
                write_entry(&mut bytes, entry, 0x11, FILE_NAME_OFFSET, 0, 0);
            }
        }

        bytes[INFO_OFFSET + string_offset..INFO_OFFSET + string_offset + STRING_TABLE.len()]
            .copy_from_slice(STRING_TABLE);
        bytes
    }

    fn write_entry(
        bytes: &mut [u8],
        offset: usize,
        flags: u8,
        name_offset: u32,
        data_offset: u32,
        size: u32,
    ) {
        let flags_and_name = ((flags as u32) << 24) | name_offset;
        bytes[offset + 4..offset + 8].copy_from_slice(&flags_and_name.to_be_bytes());
        bytes[offset + 8..offset + 12].copy_from_slice(&data_offset.to_be_bytes());
        bytes[offset + 12..offset + 16].copy_from_slice(&size.to_be_bytes());
    }
}
