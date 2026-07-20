use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::binary::{be_u32, require_len, require_magic};
use crate::{FormatError, PreserveBytes, Result};

const FORMAT: &str = "Yaz0";
const MAX_DECOMPRESSED_SIZE: usize = 512 * 1024 * 1024;
const YAZ0_WINDOW_SIZE: usize = 0x1000;
const YAZ0_MAX_MATCH: usize = 0x111;
const YAZ0_MIN_MATCH: usize = 3;
const YAZ0_RESERVED: [u8; 8] = [0; 8];

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Yaz0File {
    decompressed_size: u32,
    bytes: Vec<u8>,
}

/// A source-free Yaz0 document.
///
/// Unlike [`Yaz0File`], this type never retains the compressed source stream.
/// Re-encoding is therefore always performed from `data` by the deterministic
/// Nintendo-style encoder in this module.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Yaz0Document {
    /// The canonical format-reserved header bytes at offsets `0x08..0x10`.
    ///
    /// Strict imports accept only the all-zero creator constant. The field is
    /// retained for API compatibility, but it can never carry source data.
    pub reserved: [u8; 8],
    /// The semantic, decompressed payload.
    pub data: Vec<u8>,
}

impl Yaz0Document {
    pub fn parse(bytes: impl AsRef<[u8]>) -> Result<Self> {
        let bytes = bytes.as_ref();
        require_len(FORMAT, bytes, 0x10)?;
        require_magic(FORMAT, bytes, b"Yaz0")?;
        if bytes[0x08..0x10] != YAZ0_RESERVED {
            return Err(FormatError::Unsupported {
                format: FORMAT,
                message: format!(
                    "source-free import requires zero Yaz0 reserved bytes, found {:02X?}",
                    &bytes[0x08..0x10]
                ),
            });
        }
        Ok(Self {
            reserved: YAZ0_RESERVED,
            data: decode_yaz0(bytes)?,
        })
    }

    pub fn to_bytes(&self) -> Result<Vec<u8>> {
        encode_yaz0_with_reserved(&self.data, self.reserved)
    }
}

impl Yaz0File {
    pub fn parse(bytes: impl AsRef<[u8]>) -> Result<Self> {
        let bytes = bytes.as_ref();
        require_len(FORMAT, bytes, 0x10)?;
        require_magic(FORMAT, bytes, b"Yaz0")?;
        let decompressed_size = be_u32(bytes, 0x04, FORMAT)?;
        Ok(Self {
            decompressed_size,
            bytes: bytes.to_vec(),
        })
    }

    pub fn decompressed_size(&self) -> u32 {
        self.decompressed_size
    }

    pub fn decode(&self) -> Result<Vec<u8>> {
        decode_yaz0(&self.bytes)
    }
}

impl PreserveBytes for Yaz0File {
    fn source_bytes(&self) -> &[u8] {
        &self.bytes
    }
}

pub fn decode_yaz0(bytes: &[u8]) -> Result<Vec<u8>> {
    require_len(FORMAT, bytes, 0x10)?;
    require_magic(FORMAT, bytes, b"Yaz0")?;
    let decoded_size = be_u32(bytes, 0x04, FORMAT)? as usize;
    if decoded_size > MAX_DECOMPRESSED_SIZE {
        return Err(FormatError::ResourceLimit {
            format: FORMAT,
            resource: "decompressed byte count",
            requested: decoded_size,
            limit: MAX_DECOMPRESSED_SIZE,
        });
    }

    let mut src = 0x10;
    let mut dst = Vec::new();
    dst.try_reserve_exact(decoded_size)
        .map_err(|error| FormatError::Unsupported {
            format: FORMAT,
            message: format!("could not reserve {decoded_size} decompressed bytes: {error}"),
        })?;

    while dst.len() < decoded_size {
        if src >= bytes.len() {
            return Err(FormatError::InvalidOffset {
                format: FORMAT,
                offset: src,
                len: bytes.len(),
            });
        }

        let code = bytes[src];
        src += 1;

        for bit in 0..8 {
            if dst.len() >= decoded_size {
                break;
            }

            if (code & (0x80 >> bit)) != 0 {
                if src >= bytes.len() {
                    return Err(FormatError::InvalidOffset {
                        format: FORMAT,
                        offset: src,
                        len: bytes.len(),
                    });
                }
                dst.push(bytes[src]);
                src += 1;
            } else {
                if src + 1 >= bytes.len() {
                    return Err(FormatError::InvalidOffset {
                        format: FORMAT,
                        offset: src,
                        len: bytes.len(),
                    });
                }

                let byte1 = bytes[src];
                let byte2 = bytes[src + 1];
                src += 2;

                let dist = ((((byte1 & 0x0F) as usize) << 8) | byte2 as usize) + 1;
                let mut count = (byte1 >> 4) as usize;
                if count == 0 {
                    if src >= bytes.len() {
                        return Err(FormatError::InvalidOffset {
                            format: FORMAT,
                            offset: src,
                            len: bytes.len(),
                        });
                    }
                    count = bytes[src] as usize + 0x12;
                    src += 1;
                } else {
                    count += 2;
                }

                if dist > dst.len() {
                    return Err(FormatError::Unsupported {
                        format: FORMAT,
                        message: format!("back-reference distance {dist} before start"),
                    });
                }

                for _ in 0..count {
                    let value = dst[dst.len() - dist];
                    dst.push(value);
                    if dst.len() >= decoded_size {
                        break;
                    }
                }
            }
        }
    }

    Ok(dst)
}

/// Deterministically encodes bytes using Nintendo's Yaz0 look-ahead strategy.
///
/// The encoder searches the preceding `0x1000` bytes, selects the earliest
/// longest match, and emits a literal when the next position has a match at
/// least two bytes longer. These tie-breaking rules are part of the encoded
/// representation and are intentionally kept stable.
pub fn encode_yaz0(bytes: &[u8]) -> Result<Vec<u8>> {
    encode_yaz0_with_reserved(bytes, YAZ0_RESERVED)
}

pub fn encode_yaz0_with_reserved(bytes: &[u8], reserved: [u8; 8]) -> Result<Vec<u8>> {
    if reserved != YAZ0_RESERVED {
        return Err(FormatError::Unsupported {
            format: FORMAT,
            message: format!(
                "source-free export requires zero Yaz0 reserved bytes, found {reserved:02X?}"
            ),
        });
    }
    if bytes.len() > MAX_DECOMPRESSED_SIZE {
        return Err(FormatError::ResourceLimit {
            format: FORMAT,
            resource: "uncompressed byte count",
            requested: bytes.len(),
            limit: MAX_DECOMPRESSED_SIZE,
        });
    }
    let decoded_size = u32::try_from(bytes.len()).map_err(|_| FormatError::ResourceLimit {
        format: FORMAT,
        resource: "uncompressed byte count",
        requested: bytes.len(),
        limit: u32::MAX as usize,
    })?;

    let finder = Yaz0MatchFinder::new(bytes)?;
    let mut output = Vec::new();
    let estimated = bytes
        .len()
        .checked_add(bytes.len().div_ceil(8))
        .and_then(|size| size.checked_add(0x10))
        .unwrap_or(bytes.len());
    output
        .try_reserve(estimated)
        .map_err(|error| FormatError::Unsupported {
            format: FORMAT,
            message: format!("could not reserve Yaz0 output bytes: {error}"),
        })?;
    output.extend_from_slice(b"Yaz0");
    output.extend_from_slice(&decoded_size.to_be_bytes());
    output.extend_from_slice(&reserved);

    let mut position = 0usize;
    let mut deferred_match = None;
    while position < bytes.len() {
        let code_offset = output.len();
        output.push(0);
        let mut code = 0u8;

        for bit in 0..8 {
            if position >= bytes.len() {
                break;
            }

            let deferred = deferred_match.take();
            let was_deferred = deferred.is_some();
            let current_match = deferred.unwrap_or_else(|| finder.find(position));
            let selected = if !was_deferred
                && current_match.length >= YAZ0_MIN_MATCH
                && position + 1 < bytes.len()
            {
                let next_match = finder.find(position + 1);
                if next_match.length >= current_match.length + 2 {
                    deferred_match = Some(next_match);
                    Yaz0Match::default()
                } else {
                    current_match
                }
            } else {
                current_match
            };

            if selected.length < YAZ0_MIN_MATCH {
                code |= 0x80 >> bit;
                output.push(bytes[position]);
                position += 1;
            } else {
                let distance = position - selected.position - 1;
                if selected.length >= 0x12 {
                    output.push((distance >> 8) as u8 & 0x0F);
                    output.push(distance as u8);
                    output.push((selected.length - 0x12) as u8);
                } else {
                    output.push((((selected.length - 2) as u8) << 4) | (distance >> 8) as u8);
                    output.push(distance as u8);
                }
                position += selected.length;
            }
        }

        output[code_offset] = code;
    }

    Ok(output)
}

#[derive(Debug, Clone, Copy, Default)]
struct Yaz0Match {
    position: usize,
    length: usize,
}

struct Yaz0MatchFinder<'a> {
    bytes: &'a [u8],
    positions_by_prefix: HashMap<u32, Vec<u32>>,
}

impl<'a> Yaz0MatchFinder<'a> {
    fn new(bytes: &'a [u8]) -> Result<Self> {
        let mut positions_by_prefix: HashMap<u32, Vec<u32>> = HashMap::new();
        for position in 0..bytes.len().saturating_sub(2) {
            let key = prefix_key(bytes, position);
            positions_by_prefix
                .entry(key)
                .or_default()
                .push(position as u32);
        }
        Ok(Self {
            bytes,
            positions_by_prefix,
        })
    }

    fn find(&self, position: usize) -> Yaz0Match {
        if position + YAZ0_MIN_MATCH > self.bytes.len() {
            return Yaz0Match::default();
        }
        let Some(candidates) = self
            .positions_by_prefix
            .get(&prefix_key(self.bytes, position))
        else {
            return Yaz0Match::default();
        };

        let search_start = position.saturating_sub(YAZ0_WINDOW_SIZE) as u32;
        let position_u32 = position as u32;
        let start = candidates.partition_point(|candidate| *candidate < search_start);
        let end = candidates.partition_point(|candidate| *candidate < position_u32);
        let maximum = YAZ0_MAX_MATCH.min(self.bytes.len() - position);
        let mut best = Yaz0Match::default();

        // Ascending candidates exactly match the original exhaustive search:
        // equal-length matches keep the earliest position in the window.
        for &candidate in &candidates[start..end] {
            let candidate = candidate as usize;
            let mut length = YAZ0_MIN_MATCH;
            while length < maximum
                && self.bytes[candidate + length] == self.bytes[position + length]
            {
                length += 1;
            }
            if length > best.length {
                best = Yaz0Match {
                    position: candidate,
                    length,
                };
                if length == maximum {
                    break;
                }
            }
        }
        best
    }
}

fn prefix_key(bytes: &[u8], position: usize) -> u32 {
    ((bytes[position] as u32) << 16)
        | ((bytes[position + 1] as u32) << 8)
        | bytes[position + 2] as u32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_literal_only_stream() {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"Yaz0");
        bytes.extend_from_slice(&(3u32.to_be_bytes()));
        bytes.extend_from_slice(&[0; 8]);
        bytes.push(0xE0);
        bytes.extend_from_slice(b"SMS");

        let file = Yaz0File::parse(&bytes).unwrap();
        assert_eq!(file.decompressed_size(), 3);
        assert_eq!(file.decode().unwrap(), b"SMS");
        assert_eq!(file.to_bytes(), bytes);
    }

    #[test]
    fn rejects_unreasonable_declared_output_before_allocating() {
        let mut bytes = vec![0; 0x10];
        bytes[..4].copy_from_slice(b"Yaz0");
        bytes[4..8].copy_from_slice(&u32::MAX.to_be_bytes());

        assert!(matches!(
            decode_yaz0(&bytes),
            Err(FormatError::ResourceLimit { .. })
        ));
    }

    #[test]
    fn rejects_a_truncated_header() {
        assert!(matches!(
            Yaz0File::parse(b"Yaz0\0\0\0\0"),
            Err(FormatError::TooSmall { .. })
        ));
    }

    #[test]
    fn deterministic_encoder_round_trips_literals_and_back_references() {
        let source = b"SMS level editor: SMS level editor: SMS level editor";
        let encoded = encode_yaz0(source).unwrap();

        assert_eq!(decode_yaz0(&encoded).unwrap(), source);
        assert_eq!(encode_yaz0(source).unwrap(), encoded);
        assert_eq!(&encoded[..4], b"Yaz0");
        assert_eq!(&encoded[8..16], &[0; 8]);
        assert!(encoded.len() < source.len() + 0x10);
    }

    #[test]
    fn source_free_document_does_not_retain_the_compressed_stream() {
        let encoded = encode_yaz0(b"abcabcabcabcabcabc").unwrap();
        let document = Yaz0Document::parse(&encoded).unwrap();

        assert_eq!(document.data, b"abcabcabcabcabcabc");
        assert_eq!(document.to_bytes().unwrap(), encoded);
    }

    #[test]
    fn source_free_document_rejects_nonzero_reserved_header_bytes() {
        let mut encoded = encode_yaz0(b"semantic payload").unwrap();
        encoded[0x0F] = 1;

        assert!(matches!(
            Yaz0Document::parse(&encoded),
            Err(FormatError::Unsupported { .. })
        ));
        assert!(matches!(
            encode_yaz0_with_reserved(b"semantic payload", [1; 8]),
            Err(FormatError::Unsupported { .. })
        ));

        let mut document = Yaz0Document {
            reserved: [0; 8],
            data: b"semantic payload".to_vec(),
        };
        document.reserved[0] = 1;
        assert!(matches!(
            document.to_bytes(),
            Err(FormatError::Unsupported { .. })
        ));
    }

    #[test]
    #[ignore = "requires SMS_YAZ0_PATH to point to a retail Yaz0 stream"]
    fn matches_an_external_retail_yaz0_stream_byte_for_byte() {
        let path = std::env::var_os("SMS_YAZ0_PATH")
            .map(std::path::PathBuf::from)
            .expect("set SMS_YAZ0_PATH to a retail Yaz0 stream");
        let source = std::fs::read(path).expect("read retail Yaz0 stream");
        let document = Yaz0Document::parse(&source).expect("decode retail Yaz0 stream");

        assert_eq!(document.to_bytes().expect("re-encode retail Yaz0"), source);
    }
}
