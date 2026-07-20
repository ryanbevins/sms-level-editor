use serde::{Deserialize, Serialize};

use crate::binary::{be_f32, be_u16, be_u32, require_len};
use crate::{FormatError, Result};

const FORMAT: &str = "JAI animation sound (BAS)";
const HEADER_SIZE: usize = 8;
const CUE_SIZE: usize = 0x20;
const MAX_CUES: usize = 65_535;

/// Source-free representation of the animation sound table consumed by
/// `JAIAnimeSound::initActorAnimSound`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BasFile {
    /// The six bytes following the big-endian cue count. Retail SMS files use
    /// `0C 00 00 00 00 00`; they are explicit format-header fields rather than
    /// a retained file buffer.
    pub header_fields: [u8; 6],
    pub cues: Vec<BasSoundCue>,
}

/// On-disc `JAIAnimeFrameSoundData` (0x20 bytes).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct BasSoundCue {
    pub sound_id: u32,
    pub frame: f32,
    pub value_08: f32,
    pub value_0c: f32,
    pub flags: u32,
    pub value_14: u8,
    pub value_15: i8,
    pub value_16: u8,
    pub value_17: u8,
    pub value_18: i8,
    pub value_19: [u8; 7],
}

impl BasFile {
    pub fn parse(bytes: impl AsRef<[u8]>) -> Result<Self> {
        let bytes = bytes.as_ref();
        require_len(FORMAT, bytes, HEADER_SIZE)?;
        let cue_count = be_u16(bytes, 0, FORMAT)? as usize;
        if cue_count > MAX_CUES {
            return Err(FormatError::ResourceLimit {
                format: FORMAT,
                resource: "sound cues",
                requested: cue_count,
                limit: MAX_CUES,
            });
        }
        let expected = cue_count
            .checked_mul(CUE_SIZE)
            .and_then(|length| length.checked_add(HEADER_SIZE))
            .ok_or_else(|| invalid_offset(usize::MAX, bytes.len()))?;
        if bytes.len() != expected {
            return Err(if bytes.len() < expected {
                invalid_offset(expected, bytes.len())
            } else {
                FormatError::Unsupported {
                    format: FORMAT,
                    message: format!(
                        "{} trailing byte(s) follow the declared cue table",
                        bytes.len() - expected
                    ),
                }
            });
        }

        let mut header_fields = [0; 6];
        header_fields.copy_from_slice(&bytes[2..8]);
        let mut cues = Vec::with_capacity(cue_count);
        for index in 0..cue_count {
            let offset = HEADER_SIZE + index * CUE_SIZE;
            let mut value_19 = [0; 7];
            value_19.copy_from_slice(&bytes[offset + 0x19..offset + 0x20]);
            cues.push(BasSoundCue {
                sound_id: be_u32(bytes, offset, FORMAT)?,
                frame: be_f32(bytes, offset + 0x04, FORMAT)?,
                value_08: be_f32(bytes, offset + 0x08, FORMAT)?,
                value_0c: be_f32(bytes, offset + 0x0C, FORMAT)?,
                flags: be_u32(bytes, offset + 0x10, FORMAT)?,
                value_14: bytes[offset + 0x14],
                value_15: bytes[offset + 0x15] as i8,
                value_16: bytes[offset + 0x16],
                value_17: bytes[offset + 0x17],
                value_18: bytes[offset + 0x18] as i8,
                value_19,
            });
        }
        Ok(Self {
            header_fields,
            cues,
        })
    }

    pub fn encode(&self) -> Result<Vec<u8>> {
        if self.cues.len() > MAX_CUES {
            return Err(FormatError::ResourceLimit {
                format: FORMAT,
                resource: "sound cues",
                requested: self.cues.len(),
                limit: MAX_CUES,
            });
        }
        let cue_count = u16::try_from(self.cues.len()).map_err(|_| FormatError::ResourceLimit {
            format: FORMAT,
            resource: "sound cues",
            requested: self.cues.len(),
            limit: u16::MAX as usize,
        })?;
        let capacity = HEADER_SIZE
            .checked_add(self.cues.len().saturating_mul(CUE_SIZE))
            .ok_or_else(|| invalid_offset(usize::MAX, usize::MAX))?;
        let mut bytes = Vec::with_capacity(capacity);
        bytes.extend_from_slice(&cue_count.to_be_bytes());
        bytes.extend_from_slice(&self.header_fields);
        for cue in &self.cues {
            bytes.extend_from_slice(&cue.sound_id.to_be_bytes());
            bytes.extend_from_slice(&cue.frame.to_bits().to_be_bytes());
            bytes.extend_from_slice(&cue.value_08.to_bits().to_be_bytes());
            bytes.extend_from_slice(&cue.value_0c.to_bits().to_be_bytes());
            bytes.extend_from_slice(&cue.flags.to_be_bytes());
            bytes.push(cue.value_14);
            bytes.push(cue.value_15 as u8);
            bytes.push(cue.value_16);
            bytes.push(cue.value_17);
            bytes.push(cue.value_18 as u8);
            bytes.extend_from_slice(&cue.value_19);
        }
        debug_assert_eq!(bytes.len(), capacity);
        Ok(bytes)
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

    fn fixture() -> BasFile {
        BasFile {
            header_fields: [0x0C, 0, 0, 0, 0, 0],
            cues: vec![BasSoundCue {
                sound_id: 0x3874,
                frame: 10.2421875,
                value_08: 0.0,
                value_0c: 1.0,
                flags: 0,
                value_14: 0x7F,
                value_15: 0,
                value_16: 0,
                value_17: 0x40,
                value_18: 0,
                value_19: [0; 7],
            }],
        }
    }

    #[test]
    fn semantic_round_trip_is_byte_identical() {
        let source = fixture().encode().unwrap();
        let parsed = BasFile::parse(&source).unwrap();
        assert_eq!(parsed, fixture());
        assert_eq!(parsed.encode().unwrap(), source);
    }

    #[test]
    fn every_truncated_prefix_is_rejected() {
        let source = fixture().encode().unwrap();
        for length in 0..source.len() {
            assert!(BasFile::parse(&source[..length]).is_err());
        }
    }

    #[test]
    #[ignore = "requires SMS_BASE_ROOT with extracted retail stage archives"]
    fn source_free_rebuilds_every_retail_stage_bas_file() {
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
                    .ends_with(".bas")
                {
                    continue;
                }
                let source = crate::read_stage_asset_bytes(&asset.path)
                    .unwrap_or_else(|error| panic!("read {}: {error}", asset.path.display()));
                let document = BasFile::parse(&source)
                    .unwrap_or_else(|error| panic!("parse {}: {error}", asset.path.display()));
                assert_eq!(
                    document.encode().expect("encode semantic BAS"),
                    source,
                    "source-free BAS rebuild differs for {}",
                    asset.path.display()
                );
                rebuilt += 1;
            }
        }
        assert!(rebuilt > 0, "retail census found no BAS files");
        eprintln!("source-free BAS census rebuilt {rebuilt} files");
    }
}
