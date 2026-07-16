use serde::{Deserialize, Serialize};

use crate::binary::{be_u32, checked_slice, require_len};
use crate::{FormatError, PreserveBytes, Result};

const FORMAT: &str = "COL";
const VERTEX_SIZE: usize = 0x0C;
// FabricatedUnk1CStruct in the SMS decomp is 0x18 bytes on GameCube.
const GROUP_SIZE: usize = 0x18;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ColHeader {
    pub triangle_count_or_flags: u32,
    pub vertex_offset: u32,
    pub group_count: u32,
    pub group_offset: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ColFile {
    header: ColHeader,
    bytes: Vec<u8>,
}

impl ColFile {
    pub fn parse(bytes: impl AsRef<[u8]>) -> Result<Self> {
        let bytes = bytes.as_ref();
        require_len(FORMAT, bytes, 0x10)?;
        let header = ColHeader {
            triangle_count_or_flags: be_u32(bytes, 0x00, FORMAT)?,
            vertex_offset: be_u32(bytes, 0x04, FORMAT)?,
            group_count: be_u32(bytes, 0x08, FORMAT)?,
            group_offset: be_u32(bytes, 0x0C, FORMAT)?,
        };

        validate_counted_span(
            bytes,
            header.vertex_offset as usize,
            header.triangle_count_or_flags as usize,
            VERTEX_SIZE,
        )?;
        validate_counted_span(
            bytes,
            header.group_offset as usize,
            header.group_count as usize,
            GROUP_SIZE,
        )?;

        Ok(Self {
            header,
            bytes: bytes.to_vec(),
        })
    }

    pub fn header(&self) -> &ColHeader {
        &self.header
    }
}

fn validate_counted_span(bytes: &[u8], offset: usize, count: usize, stride: usize) -> Result<()> {
    let length = count
        .checked_mul(stride)
        .ok_or(FormatError::InvalidOffset {
            format: FORMAT,
            offset,
            len: bytes.len(),
        })?;
    checked_slice(FORMAT, bytes, offset, length)?;
    Ok(())
}

impl PreserveBytes for ColFile {
    fn source_bytes(&self) -> &[u8] {
        &self.bytes
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_sms_collision_header_shape() {
        let mut bytes = vec![0; 0x30];
        bytes[4..8].copy_from_slice(&(0x10u32.to_be_bytes()));
        bytes[12..16].copy_from_slice(&(0x20u32.to_be_bytes()));

        let col = ColFile::parse(&bytes).unwrap();
        assert_eq!(col.header().vertex_offset, 0x10);
        assert_eq!(col.to_bytes(), bytes);
    }

    #[test]
    fn rejects_vertex_count_whose_full_span_is_out_of_bounds() {
        let mut bytes = vec![0; 0x30];
        bytes[..4].copy_from_slice(&2u32.to_be_bytes());
        bytes[4..8].copy_from_slice(&0x20u32.to_be_bytes());
        bytes[12..16].copy_from_slice(&0x30u32.to_be_bytes());

        assert!(matches!(
            ColFile::parse(bytes),
            Err(FormatError::InvalidOffset { .. })
        ));
    }

    #[test]
    fn rejects_group_count_whose_full_span_is_out_of_bounds() {
        let mut bytes = vec![0; 0x30];
        bytes[4..8].copy_from_slice(&0x10u32.to_be_bytes());
        bytes[8..12].copy_from_slice(&1u32.to_be_bytes());
        bytes[12..16].copy_from_slice(&0x20u32.to_be_bytes());

        assert!(matches!(
            ColFile::parse(bytes),
            Err(FormatError::InvalidOffset { .. })
        ));
    }

    #[test]
    fn counted_collision_spans_reject_every_truncated_prefix() {
        let mut bytes = vec![0; 0x34];
        bytes[..4].copy_from_slice(&1u32.to_be_bytes());
        bytes[4..8].copy_from_slice(&0x10u32.to_be_bytes());
        bytes[8..12].copy_from_slice(&1u32.to_be_bytes());
        bytes[12..16].copy_from_slice(&0x1Cu32.to_be_bytes());

        assert!(ColFile::parse(&bytes).is_ok());
        for length in 0..bytes.len() {
            assert!(
                ColFile::parse(&bytes[..length]).is_err(),
                "truncated prefix of {length} bytes was accepted"
            );
        }
    }

    #[test]
    fn extreme_counts_are_rejected_without_integer_wraparound() {
        let mut bytes = vec![0; 0x10];
        bytes[..4].copy_from_slice(&u32::MAX.to_be_bytes());
        bytes[4..8].copy_from_slice(&0x10u32.to_be_bytes());
        bytes[12..16].copy_from_slice(&0x10u32.to_be_bytes());
        assert!(matches!(
            ColFile::parse(bytes),
            Err(FormatError::InvalidOffset { .. })
        ));
    }
}
