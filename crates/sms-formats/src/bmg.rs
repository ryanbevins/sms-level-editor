use std::collections::BTreeMap;

use encoding_rs::SHIFT_JIS;
use serde::{Deserialize, Serialize};

use crate::binary::{be_u16, be_u32, checked_slice, require_len, require_magic};
use crate::{FormatError, Result};

const FORMAT: &str = "BMG message archive";
const HEADER_SIZE: usize = 0x20;
const INFO_HEADER_SIZE: usize = 0x10;
const DATA_HEADER_SIZE: usize = 8;
const FILE_ALIGNMENT: usize = 0x20;
const MAX_MESSAGES: usize = 65_535;
const MAX_MESSAGE_BYTES: usize = 16 * 1024 * 1024;

/// A source-free `MESGbmg1` message archive.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BmgFile {
    pub header_reserved: [u8; 16],
    pub info_section_size: u32,
    pub data_section_size: u32,
    pub entry_size: u16,
    pub group_id: u16,
    pub default_color: u8,
    pub info_reserved: u8,
    pub entries: Vec<BmgEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BmgEntry {
    /// Offset from the first byte after the DAT1 section header.
    pub message_offset: u32,
    /// Per-message INF1 attributes. Its length is `entry_size - 4`.
    pub attributes: Vec<u8>,
    pub message: BmgMessage,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, Default)]
pub struct BmgMessage {
    pub tokens: Vec<BmgMessageToken>,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum BmgMessageToken {
    /// Shift-JIS text, represented as Unicode in the authoring model.
    Text(String),
    /// A BMG `0x1A` escape. The length byte is regenerated from the payload.
    Control(Vec<u8>),
}

impl BmgFile {
    pub fn parse(bytes: impl AsRef<[u8]>) -> Result<Self> {
        let bytes = bytes.as_ref();
        require_len(FORMAT, bytes, HEADER_SIZE)?;
        require_magic(FORMAT, bytes, b"MESGbmg1")?;
        let size_units = be_u32(bytes, 0x08, FORMAT)? as usize;
        let declared_size = size_units
            .checked_mul(FILE_ALIGNMENT)
            .ok_or_else(|| invalid_offset(usize::MAX, bytes.len()))?;
        if declared_size != bytes.len() {
            return Err(FormatError::Unsupported {
                format: FORMAT,
                message: format!(
                    "declared size {declared_size:#x} does not equal supplied size {:#x}",
                    bytes.len()
                ),
            });
        }
        let section_count = be_u32(bytes, 0x0C, FORMAT)? as usize;
        if section_count != 2 {
            return Err(unsupported(format!(
                "source-free BMG writer currently requires INF1 + DAT1, found {section_count} sections"
            )));
        }
        let mut header_reserved = [0; 16];
        header_reserved.copy_from_slice(&bytes[0x10..0x20]);

        let info_start = HEADER_SIZE;
        require_magic_at(bytes, info_start, b"INF1")?;
        let info_section_size = be_u32(bytes, info_start + 4, FORMAT)?;
        let info_end = checked_end(info_start, info_section_size as usize, bytes.len())?;
        checked_slice(FORMAT, bytes, info_start, INFO_HEADER_SIZE)?;
        let message_count = be_u16(bytes, info_start + 8, FORMAT)? as usize;
        if message_count > MAX_MESSAGES {
            return Err(resource_limit("messages", message_count, MAX_MESSAGES));
        }
        let entry_size = be_u16(bytes, info_start + 0x0A, FORMAT)?;
        if entry_size < 4 {
            return Err(unsupported(format!(
                "INF1 entry size {entry_size} is smaller than its message offset"
            )));
        }
        let group_id = be_u16(bytes, info_start + 0x0C, FORMAT)?;
        let default_color = bytes[info_start + 0x0E];
        let info_reserved = bytes[info_start + 0x0F];
        let entries_size = message_count
            .checked_mul(entry_size as usize)
            .ok_or_else(|| invalid_offset(usize::MAX, info_end))?;
        let entries_end = checked_end(info_start + INFO_HEADER_SIZE, entries_size, info_end)?;
        if bytes[entries_end..info_end].iter().any(|byte| *byte != 0) {
            return Err(unsupported(
                "INF1 alignment contains non-zero unmodeled bytes".to_string(),
            ));
        }

        let data_start = info_end;
        require_magic_at(bytes, data_start, b"DAT1")?;
        let data_section_size = be_u32(bytes, data_start + 4, FORMAT)?;
        let data_end = checked_end(data_start, data_section_size as usize, bytes.len())?;
        if data_end != bytes.len() {
            return Err(unsupported(format!(
                "{} bytes follow DAT1",
                bytes.len() - data_end
            )));
        }
        let data = checked_slice(
            FORMAT,
            bytes,
            data_start + DATA_HEADER_SIZE,
            data_section_size as usize - DATA_HEADER_SIZE,
        )?;
        let mut claimed = vec![false; data.len()];

        let mut entries = Vec::with_capacity(message_count);
        let mut decoded_by_offset = BTreeMap::<u32, (BmgMessage, usize)>::new();
        for index in 0..message_count {
            let entry = info_start + INFO_HEADER_SIZE + index * entry_size as usize;
            let message_offset = be_u32(bytes, entry, FORMAT)?;
            let (message, consumed) = if let Some(cached) = decoded_by_offset.get(&message_offset) {
                cached.clone()
            } else {
                let decoded = parse_message(data, message_offset as usize)?;
                decoded_by_offset.insert(message_offset, decoded.clone());
                decoded
            };
            let start = message_offset as usize;
            let end = start
                .checked_add(consumed)
                .ok_or_else(|| invalid_offset(start, data.len()))?;
            let span = claimed
                .get_mut(start..end)
                .ok_or_else(|| invalid_offset(end, data.len()))?;
            span.fill(true);
            entries.push(BmgEntry {
                message_offset,
                attributes: bytes[entry + 4..entry + entry_size as usize].to_vec(),
                message,
            });
        }
        if data
            .iter()
            .zip(&claimed)
            .any(|(byte, claimed)| !claimed && *byte != 0)
        {
            return Err(unsupported(
                "DAT1 has non-zero bytes outside typed messages".to_string(),
            ));
        }

        Ok(Self {
            header_reserved,
            info_section_size,
            data_section_size,
            entry_size,
            group_id,
            default_color,
            info_reserved,
            entries,
        })
    }

    pub fn encode(&self) -> Result<Vec<u8>> {
        self.validate_layout()?;
        let info_size = self.info_section_size as usize;
        let data_size = self.data_section_size as usize;
        let file_size = HEADER_SIZE
            .checked_add(info_size)
            .and_then(|size| size.checked_add(data_size))
            .ok_or_else(|| invalid_offset(usize::MAX, usize::MAX))?;
        if !file_size.is_multiple_of(FILE_ALIGNMENT) {
            return Err(unsupported(format!(
                "encoded BMG size {file_size:#x} is not {FILE_ALIGNMENT:#x}-byte aligned"
            )));
        }
        let size_units = u32::try_from(file_size / FILE_ALIGNMENT)
            .map_err(|_| resource_limit("file size units", file_size, u32::MAX as usize))?;
        let mut bytes = vec![0; file_size];
        bytes[..8].copy_from_slice(b"MESGbmg1");
        put_u32(&mut bytes, 0x08, size_units)?;
        put_u32(&mut bytes, 0x0C, 2)?;
        bytes[0x10..0x20].copy_from_slice(&self.header_reserved);

        let info = HEADER_SIZE;
        bytes[info..info + 4].copy_from_slice(b"INF1");
        put_u32(&mut bytes, info + 4, self.info_section_size)?;
        put_u16(&mut bytes, info + 8, self.entries.len() as u16)?;
        put_u16(&mut bytes, info + 0x0A, self.entry_size)?;
        put_u16(&mut bytes, info + 0x0C, self.group_id)?;
        bytes[info + 0x0E] = self.default_color;
        bytes[info + 0x0F] = self.info_reserved;
        for (index, entry) in self.entries.iter().enumerate() {
            let offset = info + INFO_HEADER_SIZE + index * self.entry_size as usize;
            put_u32(&mut bytes, offset, entry.message_offset)?;
            bytes[offset + 4..offset + self.entry_size as usize].copy_from_slice(&entry.attributes);
        }

        let data_start = info + info_size;
        bytes[data_start..data_start + 4].copy_from_slice(b"DAT1");
        put_u32(&mut bytes, data_start + 4, self.data_section_size)?;
        let payload_start = data_start + DATA_HEADER_SIZE;
        let payload_len = data_size - DATA_HEADER_SIZE;
        let mut emitted = BTreeMap::<u32, Vec<u8>>::new();
        for entry in &self.entries {
            let encoded = encode_message(&entry.message)?;
            if let Some(existing) = emitted.get(&entry.message_offset) {
                if existing != &encoded {
                    return Err(unsupported(format!(
                        "entries sharing DAT1 offset {:#x} contain different messages",
                        entry.message_offset
                    )));
                }
                continue;
            }
            let start = entry.message_offset as usize;
            let end = start
                .checked_add(encoded.len())
                .ok_or_else(|| invalid_offset(start, payload_len))?;
            bytes
                .get_mut(payload_start + start..payload_start + end)
                .ok_or_else(|| invalid_offset(end, payload_len))?
                .copy_from_slice(&encoded);
            emitted.insert(entry.message_offset, encoded);
        }
        Ok(bytes)
    }

    /// Packs edited messages contiguously and recomputes INF1/DAT1 sizes.
    pub fn canonicalize_layout(&mut self) -> Result<()> {
        let mut cursor = usize::from(self.entries.iter().all(|entry| entry.message_offset != 0));
        let mut offsets = BTreeMap::<BmgMessage, u32>::new();
        for entry in &mut self.entries {
            if let Some(offset) = offsets.get(&entry.message) {
                entry.message_offset = *offset;
                continue;
            }
            let encoded = encode_message(&entry.message)?;
            let offset = u32::try_from(cursor)
                .map_err(|_| resource_limit("DAT1 bytes", cursor, u32::MAX as usize))?;
            entry.message_offset = offset;
            offsets.insert(entry.message.clone(), offset);
            cursor = cursor
                .checked_add(encoded.len())
                .ok_or_else(|| resource_limit("DAT1 bytes", usize::MAX, u32::MAX as usize))?;
        }
        let info_used = INFO_HEADER_SIZE + self.entries.len() * self.entry_size as usize;
        self.info_section_size = align_up(info_used, FILE_ALIGNMENT)? as u32;
        self.data_section_size = align_up(DATA_HEADER_SIZE + cursor, FILE_ALIGNMENT)? as u32;
        Ok(())
    }

    fn validate_layout(&self) -> Result<()> {
        if self.entries.len() > MAX_MESSAGES {
            return Err(resource_limit("messages", self.entries.len(), MAX_MESSAGES));
        }
        if self.entry_size < 4 {
            return Err(unsupported("INF1 entry size is smaller than 4".to_string()));
        }
        let attributes = self.entry_size as usize - 4;
        if let Some((index, entry)) = self
            .entries
            .iter()
            .enumerate()
            .find(|(_, entry)| entry.attributes.len() != attributes)
        {
            return Err(unsupported(format!(
                "entry {index} has {} attribute bytes; expected {attributes}",
                entry.attributes.len()
            )));
        }
        let info_required = INFO_HEADER_SIZE + self.entries.len() * self.entry_size as usize;
        if (self.info_section_size as usize) < info_required {
            return Err(invalid_offset(
                info_required,
                self.info_section_size as usize,
            ));
        }
        if self.data_section_size < DATA_HEADER_SIZE as u32 {
            return Err(invalid_offset(
                DATA_HEADER_SIZE,
                self.data_section_size as usize,
            ));
        }
        Ok(())
    }
}

fn parse_message(data: &[u8], start: usize) -> Result<(BmgMessage, usize)> {
    if start >= data.len() {
        return Err(invalid_offset(start, data.len()));
    }
    let mut tokens = Vec::new();
    let mut cursor = start;
    let mut text_start = start;
    while cursor < data.len() {
        match data[cursor] {
            0 => {
                push_text_token(&mut tokens, &data[text_start..cursor])?;
                return Ok((BmgMessage { tokens }, cursor - start + 1));
            }
            0x1A => {
                push_text_token(&mut tokens, &data[text_start..cursor])?;
                let length = *data
                    .get(cursor + 1)
                    .ok_or_else(|| invalid_offset(cursor + 1, data.len()))?
                    as usize;
                if length < 2 {
                    return Err(unsupported(format!(
                        "BMG control at {cursor:#x} has invalid length {length}"
                    )));
                }
                let end = cursor
                    .checked_add(length)
                    .ok_or_else(|| invalid_offset(cursor, data.len()))?;
                let control = data
                    .get(cursor + 2..end)
                    .ok_or_else(|| invalid_offset(end, data.len()))?;
                tokens.push(BmgMessageToken::Control(control.to_vec()));
                cursor = end;
                text_start = cursor;
            }
            _ => cursor += 1,
        }
    }
    Err(invalid_offset(cursor, data.len()))
}

fn push_text_token(tokens: &mut Vec<BmgMessageToken>, bytes: &[u8]) -> Result<()> {
    if bytes.is_empty() {
        return Ok(());
    }
    let text = SHIFT_JIS
        .decode_without_bom_handling_and_without_replacement(bytes)
        .ok_or_else(|| unsupported("message contains invalid Shift-JIS text".to_string()))?;
    tokens.push(BmgMessageToken::Text(text.into_owned()));
    Ok(())
}

fn encode_message(message: &BmgMessage) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    for token in &message.tokens {
        match token {
            BmgMessageToken::Text(text) => {
                let (encoded, _, had_errors) = SHIFT_JIS.encode(text);
                if had_errors {
                    return Err(unsupported(format!(
                        "message text cannot be represented in Shift-JIS: {text:?}"
                    )));
                }
                bytes.extend_from_slice(&encoded);
            }
            BmgMessageToken::Control(payload) => {
                let length = payload
                    .len()
                    .checked_add(2)
                    .ok_or_else(|| resource_limit("control bytes", usize::MAX, u8::MAX as usize))?;
                let length = u8::try_from(length)
                    .map_err(|_| resource_limit("control bytes", length, u8::MAX as usize))?;
                bytes.push(0x1A);
                bytes.push(length);
                bytes.extend_from_slice(payload);
            }
        }
    }
    bytes.push(0);
    if bytes.len() > MAX_MESSAGE_BYTES {
        return Err(resource_limit(
            "message bytes",
            bytes.len(),
            MAX_MESSAGE_BYTES,
        ));
    }
    Ok(bytes)
}

fn require_magic_at(bytes: &[u8], offset: usize, expected: &'static [u8]) -> Result<()> {
    let actual = checked_slice(FORMAT, bytes, offset, expected.len())?;
    if actual != expected {
        return Err(FormatError::BadMagic {
            format: FORMAT,
            expected,
            actual: actual.to_vec(),
        });
    }
    Ok(())
}

fn checked_end(start: usize, length: usize, limit: usize) -> Result<usize> {
    let end = start
        .checked_add(length)
        .ok_or_else(|| invalid_offset(start, limit))?;
    if end > limit {
        return Err(invalid_offset(end, limit));
    }
    Ok(end)
}

fn align_up(value: usize, alignment: usize) -> Result<usize> {
    value
        .checked_add(alignment - 1)
        .map(|value| value / alignment * alignment)
        .ok_or_else(|| resource_limit("aligned bytes", usize::MAX, u32::MAX as usize))
}

fn put_u16(bytes: &mut [u8], offset: usize, value: u16) -> Result<()> {
    let len = bytes.len();
    bytes
        .get_mut(offset..offset + 2)
        .ok_or_else(|| invalid_offset(offset, len))?
        .copy_from_slice(&value.to_be_bytes());
    Ok(())
}

fn put_u32(bytes: &mut [u8], offset: usize, value: u32) -> Result<()> {
    let len = bytes.len();
    bytes
        .get_mut(offset..offset + 4)
        .ok_or_else(|| invalid_offset(offset, len))?
        .copy_from_slice(&value.to_be_bytes());
    Ok(())
}

fn invalid_offset(offset: usize, len: usize) -> FormatError {
    FormatError::InvalidOffset {
        format: FORMAT,
        offset,
        len,
    }
}

fn unsupported(message: String) -> FormatError {
    FormatError::Unsupported {
        format: FORMAT,
        message,
    }
}

fn resource_limit(resource: &'static str, requested: usize, limit: usize) -> FormatError {
    FormatError::ResourceLimit {
        format: FORMAT,
        resource,
        requested,
        limit,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracted_stage_message_rebuilds_when_fixture_exists() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .join(".codex_scratch/message.bmg");
        if !path.is_file() {
            return;
        }
        let source = std::fs::read(path).unwrap();
        let file = BmgFile::parse(&source).unwrap();
        assert_eq!(file.entries.len(), 31);
        assert_eq!(file.encode().unwrap(), source);
    }

    #[test]
    #[ignore = "requires SMS_BASE_ROOT with extracted retail stage archives"]
    fn source_free_rebuilds_every_retail_stage_bmg_file() {
        let root = std::env::var_os("SMS_BASE_ROOT")
            .map(std::path::PathBuf::from)
            .expect("set SMS_BASE_ROOT to an extracted retail game root");
        let archives = crate::discover_scene_archives(root).expect("discover stage archives");
        let mut rebuilt = 0usize;
        for archive in archives {
            for asset in crate::mount_scene_archive(&archive.path)
                .unwrap_or_else(|error| panic!("mount {}: {error}", archive.path.display()))
            {
                if !asset
                    .path
                    .to_string_lossy()
                    .to_ascii_lowercase()
                    .ends_with(".bmg")
                {
                    continue;
                }
                let source = crate::read_stage_asset_bytes(&asset.path)
                    .unwrap_or_else(|error| panic!("read {}: {error}", asset.path.display()));
                let document = BmgFile::parse(&source)
                    .unwrap_or_else(|error| panic!("parse {}: {error}", asset.path.display()));
                assert_eq!(
                    document.encode().expect("encode semantic BMG"),
                    source,
                    "source-free BMG rebuild differs for {}",
                    asset.path.display()
                );
                rebuilt += 1;
            }
        }
        assert!(rebuilt > 0, "retail census found no BMG files");
        eprintln!("source-free BMG census rebuilt {rebuilt} files");
    }
}
