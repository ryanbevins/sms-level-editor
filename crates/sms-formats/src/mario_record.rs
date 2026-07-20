use serde::{Deserialize, Serialize};

use crate::{FormatError, Result};

const FORMAT: &str = "Mario input record v0.2";
const MAGIC: &[u8; 16] = b"MARIO RECORDv0.2";
const HEADER_SIZE: usize = 0x40;

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct MarioRecordRun<T> {
    pub frames: u32,
    pub value: T,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MarioRecordChannel<T> {
    pub duration_offset: u32,
    pub value_offset: u32,
    pub runs: Vec<MarioRecordRun<T>>,
}

/// Source-free form of the parallel run-length streams consumed by
/// `TMarioInputReplay::init` in the SMS decompilation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MarioRecordFile {
    pub allocation_size: u32,
    pub replay_frames: u32,
    pub magnitude: MarioRecordChannel<f32>,
    pub yaw: MarioRecordChannel<i16>,
    pub button_mask: MarioRecordChannel<u16>,
    pub analog_left: MarioRecordChannel<u8>,
    pub analog_right: MarioRecordChannel<u8>,
}

/// A `.pad` resource may contain several complete authoring records. Each
/// member is decoded independently instead of retaining an ignored trailer.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MarioRecordBundle {
    pub records: Vec<MarioRecordFile>,
}

impl MarioRecordBundle {
    pub fn parse(bytes: impl AsRef<[u8]>) -> Result<Self> {
        let bytes = bytes.as_ref();
        let mut starts = bytes
            .windows(MAGIC.len())
            .enumerate()
            .filter_map(|(offset, window)| (window == MAGIC).then_some(offset))
            .collect::<Vec<_>>();
        if starts.first().copied() != Some(0) {
            return Err(FormatError::BadMagic {
                format: FORMAT,
                expected: MAGIC,
                actual: bytes.iter().take(MAGIC.len()).copied().collect(),
            });
        }
        starts.push(bytes.len());
        let mut records = Vec::with_capacity(starts.len() - 1);
        for pair in starts.windows(2) {
            records.push(MarioRecordFile::parse(&bytes[pair[0]..pair[1]])?);
        }
        Ok(Self { records })
    }

    pub fn encode(&self) -> Result<Vec<u8>> {
        if self.records.is_empty() {
            return Err(FormatError::Unsupported {
                format: FORMAT,
                message: "a Mario record bundle must contain at least one record".to_string(),
            });
        }
        let mut bytes = Vec::new();
        for record in &self.records {
            bytes.extend_from_slice(&record.encode()?);
        }
        Ok(bytes)
    }
}

impl MarioRecordFile {
    pub fn parse(bytes: impl AsRef<[u8]>) -> Result<Self> {
        let bytes = bytes.as_ref();
        if bytes.len() < HEADER_SIZE {
            return Err(FormatError::TooSmall {
                format: FORMAT,
                expected: HEADER_SIZE,
                actual: bytes.len(),
            });
        }
        if &bytes[..MAGIC.len()] != MAGIC {
            return Err(FormatError::BadMagic {
                format: FORMAT,
                expected: MAGIC,
                actual: bytes[..MAGIC.len()].to_vec(),
            });
        }
        let allocation_size = u32::try_from(bytes.len())
            .map_err(|_| resource_limit("record allocation bytes", bytes.len()))?;
        let replay_frames = be_u32(bytes, 0x10)?;
        let mut claimed = vec![false; bytes.len()];
        claimed[..HEADER_SIZE].fill(true);

        let magnitude = parse_channel(
            bytes,
            &mut claimed,
            replay_frames,
            be_u32(bytes, 0x14)?,
            be_u32(bytes, 0x18)?,
            |bytes, offset| Ok(f32::from_bits(be_u32(bytes, offset)?)),
            4,
        )?;
        let yaw = parse_channel(
            bytes,
            &mut claimed,
            replay_frames,
            be_u32(bytes, 0x1C)?,
            be_u32(bytes, 0x20)?,
            |bytes, offset| Ok(be_u16(bytes, offset)? as i16),
            2,
        )?;
        let button_mask = parse_channel(
            bytes,
            &mut claimed,
            replay_frames,
            be_u32(bytes, 0x24)?,
            be_u32(bytes, 0x28)?,
            be_u16,
            2,
        )?;
        let analog_left = parse_channel(
            bytes,
            &mut claimed,
            replay_frames,
            be_u32(bytes, 0x2C)?,
            be_u32(bytes, 0x30)?,
            read_u8,
            1,
        )?;
        let analog_right = parse_channel(
            bytes,
            &mut claimed,
            replay_frames,
            be_u32(bytes, 0x34)?,
            be_u32(bytes, 0x38)?,
            read_u8,
            1,
        )?;

        if bytes
            .iter()
            .zip(claimed)
            .any(|(byte, claimed)| !claimed && *byte != 0)
        {
            return Err(FormatError::Unsupported {
                format: FORMAT,
                message: "non-zero bytes exist outside replay header and typed channel arrays"
                    .to_string(),
            });
        }

        Ok(Self {
            allocation_size,
            replay_frames,
            magnitude,
            yaw,
            button_mask,
            analog_left,
            analog_right,
        })
    }

    pub fn encode(&self) -> Result<Vec<u8>> {
        if self.allocation_size < HEADER_SIZE as u32 {
            return Err(FormatError::TooSmall {
                format: FORMAT,
                expected: HEADER_SIZE,
                actual: self.allocation_size as usize,
            });
        }
        let mut bytes = vec![0; self.allocation_size as usize];
        bytes[..MAGIC.len()].copy_from_slice(MAGIC);
        put_u32(&mut bytes, 0x10, self.replay_frames)?;
        let channels = [
            (self.magnitude.duration_offset, self.magnitude.value_offset),
            (self.yaw.duration_offset, self.yaw.value_offset),
            (
                self.button_mask.duration_offset,
                self.button_mask.value_offset,
            ),
            (
                self.analog_left.duration_offset,
                self.analog_left.value_offset,
            ),
            (
                self.analog_right.duration_offset,
                self.analog_right.value_offset,
            ),
        ];
        for (index, (duration, value)) in channels.into_iter().enumerate() {
            put_u32(&mut bytes, 0x14 + index * 8, duration)?;
            put_u32(&mut bytes, 0x18 + index * 8, value)?;
        }

        encode_channel(
            &mut bytes,
            self.replay_frames,
            &self.magnitude,
            |bytes, offset, value| put_u32(bytes, offset, value.to_bits()),
            4,
        )?;
        encode_channel(
            &mut bytes,
            self.replay_frames,
            &self.yaw,
            |bytes, offset, value| put_u16(bytes, offset, value as u16),
            2,
        )?;
        encode_channel(
            &mut bytes,
            self.replay_frames,
            &self.button_mask,
            put_u16,
            2,
        )?;
        encode_channel(&mut bytes, self.replay_frames, &self.analog_left, put_u8, 1)?;
        encode_channel(
            &mut bytes,
            self.replay_frames,
            &self.analog_right,
            put_u8,
            1,
        )?;
        validate_duration_words(&bytes, &self.magnitude)?;
        validate_duration_words(&bytes, &self.yaw)?;
        validate_duration_words(&bytes, &self.button_mask)?;
        validate_duration_words(&bytes, &self.analog_left)?;
        validate_duration_words(&bytes, &self.analog_right)?;
        Ok(bytes)
    }
}

fn parse_channel<T>(
    bytes: &[u8],
    claimed: &mut [bool],
    replay_frames: u32,
    duration_offset: u32,
    value_offset: u32,
    read_value: impl Fn(&[u8], usize) -> Result<T>,
    value_size: usize,
) -> Result<MarioRecordChannel<T>> {
    let duration_start = duration_offset as usize;
    let value_start = value_offset as usize;
    let mut durations = Vec::new();
    let mut sum = 0u32;
    while sum < replay_frames {
        let offset = duration_start
            .checked_add(durations.len() * 4)
            .ok_or_else(|| invalid_offset(duration_start, bytes.len()))?;
        let duration = be_u32(bytes, offset)?;
        sum = sum
            .checked_add(duration)
            .ok_or_else(|| FormatError::Unsupported {
                format: FORMAT,
                message: "replay run durations overflow u32".to_string(),
            })?;
        claim(claimed, offset, 4)?;
        durations.push(duration);
    }

    let mut runs = Vec::with_capacity(durations.len());
    for (index, frames) in durations.into_iter().enumerate() {
        let offset = value_start
            .checked_add(index * value_size)
            .ok_or_else(|| invalid_offset(value_start, bytes.len()))?;
        claim(claimed, offset, value_size)?;
        runs.push(MarioRecordRun {
            frames,
            value: read_value(bytes, offset)?,
        });
    }
    Ok(MarioRecordChannel {
        duration_offset,
        value_offset,
        runs,
    })
}

fn encode_channel<T: Copy>(
    bytes: &mut [u8],
    replay_frames: u32,
    channel: &MarioRecordChannel<T>,
    write_value: impl Fn(&mut [u8], usize, T) -> Result<()>,
    value_size: usize,
) -> Result<()> {
    let total = channel.runs.iter().try_fold(0u32, |total, run| {
        total
            .checked_add(run.frames)
            .ok_or_else(|| FormatError::Unsupported {
                format: FORMAT,
                message: "replay run durations overflow u32".to_string(),
            })
    })?;
    if total < replay_frames {
        return Err(FormatError::Unsupported {
            format: FORMAT,
            message: format!(
                "replay channel durations total {total}, below declared {replay_frames}"
            ),
        });
    }
    let before_last = channel
        .runs
        .iter()
        .take(channel.runs.len().saturating_sub(1))
        .fold(0u32, |total, run| total.saturating_add(run.frames));
    if replay_frames != 0 && before_last >= replay_frames {
        return Err(FormatError::Unsupported {
            format: FORMAT,
            message: "replay channel contains runs after the declared frame range".to_string(),
        });
    }
    for (index, run) in channel.runs.iter().enumerate() {
        let duration_offset = channel.duration_offset as usize + index * 4;
        let value_offset = channel.value_offset as usize + index * value_size;
        put_u32(bytes, duration_offset, run.frames)?;
        write_value(bytes, value_offset, run.value)?;
    }
    Ok(())
}

fn claim(claimed: &mut [bool], offset: usize, size: usize) -> Result<()> {
    let len = claimed.len();
    let region = claimed
        .get_mut(offset..offset + size)
        .ok_or_else(|| invalid_offset(offset, len))?;
    if offset < HEADER_SIZE {
        return Err(FormatError::Unsupported {
            format: FORMAT,
            message: format!("replay array overlaps the header at {offset:#x}"),
        });
    }
    region.fill(true);
    Ok(())
}

fn validate_duration_words<T>(bytes: &[u8], channel: &MarioRecordChannel<T>) -> Result<()> {
    for (index, run) in channel.runs.iter().enumerate() {
        let offset = channel.duration_offset as usize + index * 4;
        if be_u32(bytes, offset)? != run.frames {
            return Err(FormatError::Unsupported {
                format: FORMAT,
                message: format!(
                    "edited value bytes conflict with an overlapping duration word at {offset:#x}"
                ),
            });
        }
    }
    Ok(())
}

fn read_u8(bytes: &[u8], offset: usize) -> Result<u8> {
    bytes
        .get(offset)
        .copied()
        .ok_or_else(|| invalid_offset(offset, bytes.len()))
}

fn be_u16(bytes: &[u8], offset: usize) -> Result<u16> {
    let value = bytes
        .get(offset..offset + 2)
        .ok_or_else(|| invalid_offset(offset, bytes.len()))?;
    Ok(u16::from_be_bytes(value.try_into().unwrap()))
}

fn be_u32(bytes: &[u8], offset: usize) -> Result<u32> {
    let value = bytes
        .get(offset..offset + 4)
        .ok_or_else(|| invalid_offset(offset, bytes.len()))?;
    Ok(u32::from_be_bytes(value.try_into().unwrap()))
}

fn put_u8(bytes: &mut [u8], offset: usize, value: u8) -> Result<()> {
    let len = bytes.len();
    *bytes
        .get_mut(offset)
        .ok_or_else(|| invalid_offset(offset, len))? = value;
    Ok(())
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

fn resource_limit(resource: &'static str, requested: usize) -> FormatError {
    FormatError::ResourceLimit {
        format: FORMAT,
        resource,
        requested,
        limit: u32::MAX as usize,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn semantic_record_bundle_rebuilds_after_source_is_destroyed() {
        let record = MarioRecordFile {
            allocation_size: 0x68,
            replay_frames: 3,
            magnitude: MarioRecordChannel {
                duration_offset: 0x40,
                value_offset: 0x44,
                runs: vec![MarioRecordRun {
                    frames: 3,
                    value: 0.75,
                }],
            },
            yaw: MarioRecordChannel {
                duration_offset: 0x48,
                value_offset: 0x4C,
                runs: vec![MarioRecordRun {
                    frames: 3,
                    value: -120,
                }],
            },
            button_mask: MarioRecordChannel {
                duration_offset: 0x50,
                value_offset: 0x54,
                runs: vec![MarioRecordRun {
                    frames: 3,
                    value: 0x0100,
                }],
            },
            analog_left: MarioRecordChannel {
                duration_offset: 0x58,
                value_offset: 0x5C,
                runs: vec![MarioRecordRun {
                    frames: 3,
                    value: 0x40,
                }],
            },
            analog_right: MarioRecordChannel {
                duration_offset: 0x60,
                value_offset: 0x64,
                runs: vec![MarioRecordRun {
                    frames: 3,
                    value: 0x80,
                }],
            },
        };
        let bundle = MarioRecordBundle {
            records: vec![record.clone(), record],
        };
        let mut source = bundle.encode().unwrap();
        let expected = source.clone();
        let mut parsed = MarioRecordBundle::parse(&source).unwrap();
        source.fill(0xA5);
        assert_eq!(parsed.encode().unwrap(), expected);
        parsed.records[1].yaw.runs[0].value += 1;
        assert_ne!(parsed.encode().unwrap(), expected);
    }

    #[test]
    fn extracted_record_rebuilds_after_source_is_destroyed() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .join(".codex_scratch/tutorialab.pad");
        if !path.is_file() {
            return;
        }
        let mut source = std::fs::read(path).unwrap();
        let expected = source.clone();
        let mut bundle = MarioRecordBundle::parse(&source).unwrap();
        source.fill(0xA5);
        assert_eq!(bundle.encode().unwrap(), expected);
        bundle.records[0].magnitude.runs[0].value += 1.0;
        assert_ne!(bundle.encode().unwrap(), expected);
    }

    #[test]
    #[ignore = "requires SMS_BASE_ROOT with extracted retail stage archives"]
    fn source_free_rebuilds_every_retail_stage_record() {
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
                    .ends_with(".pad")
                {
                    continue;
                }
                let source = crate::read_stage_asset_bytes(&asset.path)
                    .unwrap_or_else(|error| panic!("read {}: {error}", asset.path.display()));
                let document = MarioRecordBundle::parse(&source)
                    .unwrap_or_else(|error| panic!("parse {}: {error}", asset.path.display()));
                assert_eq!(
                    document.encode().unwrap_or_else(|error| {
                        panic!("encode {}: {error}", asset.path.display())
                    }),
                    source,
                    "source-free Mario record rebuild differs for {}",
                    asset.path.display()
                );
                rebuilt += 1;
            }
        }
        assert_eq!(rebuilt, 381, "retail Mario record count drifted");
        eprintln!("source-free Mario record census rebuilt {rebuilt} files");
    }
}
