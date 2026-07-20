use serde::{Deserialize, Serialize};

use crate::binary::{be_i16, be_u16, be_u32, checked_slice, require_len};
use crate::{FormatError, Result};

const FORMAT: &str = "BTI texture";
const HEADER_SIZE: usize = 0x20;

/// Source-free BTI texture retaining native GX tiled blocks as its semantic
/// pixel representation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BtiFile {
    pub allocation_size: u32,
    pub format: u8,
    pub transparency: u8,
    pub width: u16,
    pub height: u16,
    pub wrap_s: u8,
    pub wrap_t: u8,
    pub palette_enabled: u8,
    pub palette_format: u8,
    pub palette_entries: Vec<u16>,
    pub palette_offset: u32,
    pub mipmap_enabled: u8,
    pub edge_lod: u8,
    pub bias_clamp: u8,
    pub max_anisotropy: u8,
    pub min_filter: u8,
    pub mag_filter: u8,
    pub min_lod: i8,
    pub max_lod: i8,
    pub mipmap_count: u8,
    pub reserved_19: u8,
    pub lod_bias: i16,
    pub image_offset: u32,
    pub encoded_mip_levels: Vec<Vec<u8>>,
}

impl BtiFile {
    pub fn parse(bytes: impl AsRef<[u8]>) -> Result<Self> {
        let bytes = bytes.as_ref();
        require_len(FORMAT, bytes, HEADER_SIZE)?;
        let format = bytes[0];
        let width = be_u16(bytes, 2, FORMAT)?;
        let height = be_u16(bytes, 4, FORMAT)?;
        let palette_enabled = bytes[8];
        let palette_entry_count = be_u16(bytes, 0x0A, FORMAT)? as usize;
        let palette_offset = be_u32(bytes, 0x0C, FORMAT)?;
        let mipmap_count = bytes[0x18].max(1);
        let image_offset = be_u32(bytes, 0x1C, FORMAT)?;
        let mut claimed = vec![false; bytes.len()];
        claimed[..HEADER_SIZE].fill(true);

        let mut encoded_mip_levels = Vec::with_capacity(mipmap_count as usize);
        let mut cursor = image_offset as usize;
        for level in 0..mipmap_count {
            let level_width = (width >> level).max(1);
            let level_height = (height >> level).max(1);
            let length = encoded_texture_size(format, level_width, level_height)?;
            let block = checked_slice(FORMAT, bytes, cursor, length)?.to_vec();
            claimed[cursor..cursor + length].fill(true);
            encoded_mip_levels.push(block);
            cursor = cursor
                .checked_add(length)
                .ok_or_else(|| invalid_offset(cursor, bytes.len()))?;
        }

        let mut palette_entries = Vec::with_capacity(palette_entry_count);
        if palette_enabled != 0 || palette_entry_count != 0 {
            let start = palette_offset as usize;
            let length = palette_entry_count
                .checked_mul(2)
                .ok_or_else(|| invalid_offset(start, bytes.len()))?;
            checked_slice(FORMAT, bytes, start, length)?;
            claimed[start..start + length].fill(true);
            for index in 0..palette_entry_count {
                palette_entries.push(be_u16(bytes, start + index * 2, FORMAT)?);
            }
        }

        if bytes
            .iter()
            .zip(&claimed)
            .any(|(byte, claimed)| !claimed && *byte != 0)
        {
            return Err(FormatError::Unsupported {
                format: FORMAT,
                message: "non-zero bytes exist outside the header, palette, and GX image blocks"
                    .to_string(),
            });
        }

        Ok(Self {
            allocation_size: u32::try_from(bytes.len()).map_err(|_| {
                FormatError::ResourceLimit {
                    format: FORMAT,
                    resource: "texture allocation bytes",
                    requested: bytes.len(),
                    limit: u32::MAX as usize,
                }
            })?,
            format,
            transparency: bytes[1],
            width,
            height,
            wrap_s: bytes[6],
            wrap_t: bytes[7],
            palette_enabled,
            palette_format: bytes[9],
            palette_entries,
            palette_offset,
            mipmap_enabled: bytes[0x10],
            edge_lod: bytes[0x11],
            bias_clamp: bytes[0x12],
            max_anisotropy: bytes[0x13],
            min_filter: bytes[0x14],
            mag_filter: bytes[0x15],
            min_lod: bytes[0x16] as i8,
            max_lod: bytes[0x17] as i8,
            mipmap_count: bytes[0x18],
            reserved_19: bytes[0x19],
            lod_bias: be_i16(bytes, 0x1A, FORMAT)?,
            image_offset,
            encoded_mip_levels,
        })
    }

    pub fn encode(&self) -> Result<Vec<u8>> {
        self.validate()?;
        let mut bytes = vec![0; self.allocation_size as usize];
        bytes[0] = self.format;
        bytes[1] = self.transparency;
        put_u16(&mut bytes, 2, self.width)?;
        put_u16(&mut bytes, 4, self.height)?;
        bytes[6] = self.wrap_s;
        bytes[7] = self.wrap_t;
        bytes[8] = self.palette_enabled;
        bytes[9] = self.palette_format;
        put_u16(&mut bytes, 0x0A, self.palette_entries.len() as u16)?;
        put_u32(&mut bytes, 0x0C, self.palette_offset)?;
        bytes[0x10] = self.mipmap_enabled;
        bytes[0x11] = self.edge_lod;
        bytes[0x12] = self.bias_clamp;
        bytes[0x13] = self.max_anisotropy;
        bytes[0x14] = self.min_filter;
        bytes[0x15] = self.mag_filter;
        bytes[0x16] = self.min_lod as u8;
        bytes[0x17] = self.max_lod as u8;
        bytes[0x18] = self.mipmap_count;
        bytes[0x19] = self.reserved_19;
        put_u16(&mut bytes, 0x1A, self.lod_bias as u16)?;
        put_u32(&mut bytes, 0x1C, self.image_offset)?;

        if !self.palette_entries.is_empty() {
            let mut offset = self.palette_offset as usize;
            for entry in &self.palette_entries {
                put_u16(&mut bytes, offset, *entry)?;
                offset += 2;
            }
        }
        let mut offset = self.image_offset as usize;
        let allocation_len = bytes.len();
        for block in &self.encoded_mip_levels {
            let end = offset
                .checked_add(block.len())
                .ok_or_else(|| invalid_offset(offset, allocation_len))?;
            bytes
                .get_mut(offset..end)
                .ok_or_else(|| invalid_offset(end, allocation_len))?
                .copy_from_slice(block);
            offset = end;
        }
        Ok(bytes)
    }

    /// Recomputes a compact palette/image allocation after an edit.
    pub fn canonicalize_layout(&mut self) -> Result<()> {
        let palette_bytes =
            self.palette_entries
                .len()
                .checked_mul(2)
                .ok_or(FormatError::ResourceLimit {
                    format: FORMAT,
                    resource: "palette bytes",
                    requested: usize::MAX,
                    limit: u32::MAX as usize,
                })?;
        self.palette_offset = if palette_bytes == 0 {
            0
        } else {
            HEADER_SIZE as u32
        };
        self.image_offset = (HEADER_SIZE + palette_bytes) as u32;
        let image_bytes = self
            .encoded_mip_levels
            .iter()
            .try_fold(0usize, |sum, block| sum.checked_add(block.len()))
            .ok_or_else(|| invalid_offset(usize::MAX, usize::MAX))?;
        let size = self.image_offset as usize + image_bytes;
        self.allocation_size = u32::try_from(size).map_err(|_| FormatError::ResourceLimit {
            format: FORMAT,
            resource: "texture allocation bytes",
            requested: size,
            limit: u32::MAX as usize,
        })?;
        Ok(())
    }

    fn validate(&self) -> Result<()> {
        if self.palette_entries.len() > u16::MAX as usize {
            return Err(FormatError::ResourceLimit {
                format: FORMAT,
                resource: "palette entries",
                requested: self.palette_entries.len(),
                limit: u16::MAX as usize,
            });
        }
        let expected_levels = self.mipmap_count.max(1) as usize;
        if self.encoded_mip_levels.len() != expected_levels {
            return Err(FormatError::Unsupported {
                format: FORMAT,
                message: format!(
                    "texture has {} encoded mip levels, expected {expected_levels}",
                    self.encoded_mip_levels.len()
                ),
            });
        }
        for (level, block) in self.encoded_mip_levels.iter().enumerate() {
            let width = (self.width >> level).max(1);
            let height = (self.height >> level).max(1);
            let expected = encoded_texture_size(self.format, width, height)?;
            if block.len() != expected {
                return Err(FormatError::Unsupported {
                    format: FORMAT,
                    message: format!(
                        "mip level {level} has {} bytes, expected {expected}",
                        block.len()
                    ),
                });
            }
        }
        Ok(())
    }
}

fn encoded_texture_size(format: u8, width: u16, height: u16) -> Result<usize> {
    let (tile_width, tile_height, block_bytes) = match format {
        0 | 8 => (8usize, 8usize, 32usize),
        1 | 2 | 9 => (8, 4, 32),
        3 | 4 | 5 | 10 => (4, 4, 32),
        6 => (4, 4, 64),
        14 => (8, 8, 32),
        _ => {
            return Err(FormatError::Unsupported {
                format: FORMAT,
                message: format!("unknown GX texture format {format}"),
            })
        }
    };
    (width as usize)
        .div_ceil(tile_width)
        .checked_mul((height as usize).div_ceil(tile_height))
        .and_then(|blocks| blocks.checked_mul(block_bytes))
        .ok_or_else(|| invalid_offset(usize::MAX, usize::MAX))
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn semantic_texture_rebuilds_after_source_is_destroyed() {
        let texture = BtiFile {
            allocation_size: 0x40,
            format: 0,
            transparency: 0,
            width: 8,
            height: 8,
            wrap_s: 1,
            wrap_t: 1,
            palette_enabled: 0,
            palette_format: 0,
            palette_entries: Vec::new(),
            palette_offset: 0,
            mipmap_enabled: 0,
            edge_lod: 0,
            bias_clamp: 0,
            max_anisotropy: 0,
            min_filter: 1,
            mag_filter: 1,
            min_lod: 0,
            max_lod: 0,
            mipmap_count: 1,
            reserved_19: 0,
            lod_bias: 0,
            image_offset: 0x20,
            encoded_mip_levels: vec![(0u8..32).collect()],
        };
        let mut source = texture.encode().unwrap();
        let expected = source.clone();
        let mut parsed = BtiFile::parse(&source).unwrap();
        source.fill(0xA5);
        assert_eq!(parsed.encode().unwrap(), expected);
        parsed.encoded_mip_levels[0][0] ^= 1;
        assert_ne!(parsed.encode().unwrap(), expected);
    }

    #[test]
    fn extracted_wave_texture_rebuilds_when_fixture_exists() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .join(".codex_scratch/wave.bti");
        if !path.is_file() {
            return;
        }
        let source = std::fs::read(path).unwrap();
        let texture = BtiFile::parse(&source).unwrap();
        assert_eq!(texture.encoded_mip_levels.len(), 5);
        assert_eq!(texture.encode().unwrap(), source);
    }

    #[test]
    #[ignore = "requires SMS_BASE_ROOT with extracted retail stage archives"]
    fn source_free_rebuilds_every_retail_stage_bti_file() {
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
                    .ends_with(".bti")
                {
                    continue;
                }
                let source = crate::read_stage_asset_bytes(&asset.path)
                    .unwrap_or_else(|error| panic!("read {}: {error}", asset.path.display()));
                let document = BtiFile::parse(&source)
                    .unwrap_or_else(|error| panic!("parse {}: {error}", asset.path.display()));
                assert_eq!(
                    document.encode().expect("encode semantic BTI"),
                    source,
                    "source-free BTI rebuild differs for {}",
                    asset.path.display()
                );
                rebuilt += 1;
            }
        }
        assert_eq!(rebuilt, 346, "retail BTI count drifted");
        eprintln!("source-free BTI census rebuilt {rebuilt} files");
    }
}
