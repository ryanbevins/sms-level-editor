use encoding_rs::SHIFT_JIS;

use crate::{FormatError, Result};

pub fn require_len(format: &'static str, bytes: &[u8], expected: usize) -> Result<()> {
    if bytes.len() < expected {
        return Err(FormatError::TooSmall {
            format,
            expected,
            actual: bytes.len(),
        });
    }

    Ok(())
}

pub fn require_magic(format: &'static str, bytes: &[u8], expected: &'static [u8]) -> Result<()> {
    require_len(format, bytes, expected.len())?;
    if &bytes[..expected.len()] != expected {
        return Err(FormatError::BadMagic {
            format,
            expected,
            actual: bytes[..expected.len()].to_vec(),
        });
    }

    Ok(())
}

pub fn be_u32(bytes: &[u8], offset: usize, format: &'static str) -> Result<u32> {
    require_len(format, bytes, offset + 4)?;
    Ok(u32::from_be_bytes([
        bytes[offset],
        bytes[offset + 1],
        bytes[offset + 2],
        bytes[offset + 3],
    ]))
}

pub fn be_u16(bytes: &[u8], offset: usize, format: &'static str) -> Result<u16> {
    require_len(format, bytes, offset + 2)?;
    Ok(u16::from_be_bytes([bytes[offset], bytes[offset + 1]]))
}

pub fn be_i16(bytes: &[u8], offset: usize, format: &'static str) -> Result<i16> {
    require_len(format, bytes, offset + 2)?;
    Ok(i16::from_be_bytes([bytes[offset], bytes[offset + 1]]))
}

pub fn be_f32(bytes: &[u8], offset: usize, format: &'static str) -> Result<f32> {
    Ok(f32::from_bits(be_u32(bytes, offset, format)?))
}

pub fn checked_slice<'a>(
    format: &'static str,
    bytes: &'a [u8],
    offset: usize,
    length: usize,
) -> Result<&'a [u8]> {
    let end = offset
        .checked_add(length)
        .ok_or(FormatError::InvalidOffset {
            format,
            offset,
            len: bytes.len(),
        })?;

    if end > bytes.len() {
        return Err(FormatError::InvalidOffset {
            format,
            offset,
            len: bytes.len(),
        });
    }

    Ok(&bytes[offset..end])
}

pub fn read_jut_name_table(bytes: &[u8], offset: usize, limit: usize) -> Result<Vec<String>> {
    const FORMAT: &str = "JUT name table";
    if limit > bytes.len() || offset > limit {
        return Err(FormatError::InvalidOffset {
            format: FORMAT,
            offset,
            len: bytes.len(),
        });
    }
    checked_slice(FORMAT, &bytes[..limit], offset, 4)?;

    let count = be_u16(bytes, offset, FORMAT)? as usize;
    let entries_offset = offset.checked_add(4).ok_or(FormatError::InvalidOffset {
        format: FORMAT,
        offset,
        len: limit,
    })?;
    checked_slice(
        FORMAT,
        &bytes[..limit],
        entries_offset,
        count.checked_mul(4).ok_or(FormatError::InvalidOffset {
            format: FORMAT,
            offset: entries_offset,
            len: limit,
        })?,
    )?;

    let mut names = Vec::with_capacity(count);
    for index in 0..count {
        let relative = be_u16(bytes, entries_offset + index * 4 + 2, FORMAT)? as usize;
        let start = offset
            .checked_add(relative)
            .ok_or(FormatError::InvalidOffset {
                format: FORMAT,
                offset: relative,
                len: limit,
            })?;
        if start >= limit {
            return Err(FormatError::InvalidOffset {
                format: FORMAT,
                offset: start,
                len: limit,
            });
        }
        let end = bytes[start..limit]
            .iter()
            .position(|byte| *byte == 0)
            .map(|length| start + length)
            .ok_or(FormatError::InvalidOffset {
                format: FORMAT,
                offset: start,
                len: limit,
            })?;
        let (name, _) = SHIFT_JIS.decode_without_bom_handling(&bytes[start..end]);
        names.push(name.into_owned());
    }
    Ok(names)
}
