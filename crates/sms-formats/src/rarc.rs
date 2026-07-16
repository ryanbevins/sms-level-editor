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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RarcHeader {
    pub file_size: u32,
    pub header_size: u32,
    pub data_offset: u32,
    pub data_size: u32,
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
        const INFO_OFFSET: usize = 0x20;
        const NODE_OFFSET: usize = 0x20;
        const ENTRY_OFFSET: usize = 0x30;
        const STRING_OFFSET: usize = 0x44;

        let mut string_table = b"root\0".to_vec();
        let name_offset = string_table.len() as u32;
        string_table.extend_from_slice(raw_name);
        string_table.push(0);
        let data_offset = STRING_OFFSET + string_table.len();
        let file_size = INFO_OFFSET + data_offset + data.len();
        let mut bytes = vec![0; file_size];
        bytes[..4].copy_from_slice(b"RARC");
        bytes[4..8].copy_from_slice(&(file_size as u32).to_be_bytes());
        bytes[8..12].copy_from_slice(&(INFO_OFFSET as u32).to_be_bytes());
        bytes[12..16].copy_from_slice(&(data_offset as u32).to_be_bytes());
        bytes[16..20].copy_from_slice(&(data.len() as u32).to_be_bytes());

        bytes[INFO_OFFSET..INFO_OFFSET + 4].copy_from_slice(&1u32.to_be_bytes());
        bytes[INFO_OFFSET + 4..INFO_OFFSET + 8]
            .copy_from_slice(&(NODE_OFFSET as u32).to_be_bytes());
        bytes[INFO_OFFSET + 8..INFO_OFFSET + 12].copy_from_slice(&1u32.to_be_bytes());
        bytes[INFO_OFFSET + 12..INFO_OFFSET + 16]
            .copy_from_slice(&(ENTRY_OFFSET as u32).to_be_bytes());
        bytes[INFO_OFFSET + 16..INFO_OFFSET + 20]
            .copy_from_slice(&(string_table.len() as u32).to_be_bytes());
        bytes[INFO_OFFSET + 20..INFO_OFFSET + 24]
            .copy_from_slice(&(STRING_OFFSET as u32).to_be_bytes());

        let node = INFO_OFFSET + NODE_OFFSET;
        bytes[node..node + 4].copy_from_slice(b"ROOT");
        bytes[node + 10..node + 12].copy_from_slice(&1u16.to_be_bytes());
        write_entry(
            &mut bytes,
            INFO_OFFSET + ENTRY_OFFSET,
            0x11,
            name_offset,
            0,
            data.len() as u32,
        );
        bytes[INFO_OFFSET + STRING_OFFSET..INFO_OFFSET + STRING_OFFSET + string_table.len()]
            .copy_from_slice(&string_table);
        bytes[INFO_OFFSET + data_offset..].copy_from_slice(data);
        bytes
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
        let bytes = single_file_rarc(&oversized_name, b"");

        assert!(matches!(
            RarcArchive::parse(&bytes),
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
