use serde::{Deserialize, Serialize};

use crate::{FormatError, Result};

const FORMAT: &str = "SMS pollution bitmap";
const FILE_HEADER_SIZE: usize = 14;
const INFO_HEADER_SIZE: usize = 40;

/// Source-free representation of the 8-bit Windows bitmaps used by SMS
/// pollution maps. Pixel indices and palette entries are semantic image data;
/// no slice of the imported file is retained.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BmpFile {
    pub reserved_1: u16,
    pub reserved_2: u16,
    pub width: i32,
    pub height: i32,
    pub planes: u16,
    pub bits_per_pixel: u16,
    pub compression: u32,
    pub declared_image_size: u32,
    pub horizontal_pixels_per_meter: i32,
    pub vertical_pixels_per_meter: i32,
    pub colors_used: u32,
    pub important_colors: u32,
    pub palette: Vec<[u8; 4]>,
    /// Bottom-up scanlines in the BMP's native row-stride encoding.
    pub encoded_pixels: Vec<u8>,
    /// Retail pollution bitmaps contain a file-creator terminator after the
    /// final scanline. Only zero-filled terminators are accepted.
    pub trailing_zero_bytes: u32,
}

impl BmpFile {
    pub fn parse(bytes: impl AsRef<[u8]>) -> Result<Self> {
        let bytes = bytes.as_ref();
        require_len(bytes, FILE_HEADER_SIZE + INFO_HEADER_SIZE)?;
        if &bytes[..2] != b"BM" {
            return Err(FormatError::BadMagic {
                format: FORMAT,
                expected: b"BM",
                actual: bytes[..2].to_vec(),
            });
        }

        let file_size = le_u32(bytes, 2)? as usize;
        if file_size != bytes.len() {
            return Err(FormatError::Unsupported {
                format: FORMAT,
                message: format!(
                    "declared file size {file_size} differs from allocation {}",
                    bytes.len()
                ),
            });
        }
        let pixel_offset = le_u32(bytes, 10)? as usize;
        let dib_size = le_u32(bytes, 14)? as usize;
        if dib_size != INFO_HEADER_SIZE {
            return Err(FormatError::Unsupported {
                format: FORMAT,
                message: format!("unsupported DIB header size {dib_size}"),
            });
        }
        let width = le_i32(bytes, 18)?;
        let height = le_i32(bytes, 22)?;
        let planes = le_u16(bytes, 26)?;
        let bits_per_pixel = le_u16(bytes, 28)?;
        let compression = le_u32(bytes, 30)?;
        if width <= 0 || height == 0 || planes != 1 || bits_per_pixel != 8 || compression != 0 {
            return Err(FormatError::Unsupported {
                format: FORMAT,
                message: format!(
                    "expected an uncompressed 8-bit bitmap, got {width}x{height}, planes={planes}, bpp={bits_per_pixel}, compression={compression}"
                ),
            });
        }

        let palette_start = FILE_HEADER_SIZE + INFO_HEADER_SIZE;
        if pixel_offset < palette_start || pixel_offset > bytes.len() {
            return Err(invalid_offset(pixel_offset, bytes.len()));
        }
        let palette_bytes = pixel_offset - palette_start;
        if !palette_bytes.is_multiple_of(4) {
            return Err(FormatError::Unsupported {
                format: FORMAT,
                message: format!("palette allocation has {palette_bytes} bytes"),
            });
        }
        let palette = bytes[palette_start..pixel_offset]
            .chunks_exact(4)
            .map(|entry| entry.try_into().expect("four-byte palette chunk"))
            .collect::<Vec<[u8; 4]>>();

        let row_stride = row_stride(width, bits_per_pixel)?;
        let row_count = height.unsigned_abs() as usize;
        let pixel_bytes = row_stride
            .checked_mul(row_count)
            .ok_or_else(|| resource_limit("pixel bytes", usize::MAX))?;
        let pixel_end = pixel_offset
            .checked_add(pixel_bytes)
            .ok_or_else(|| invalid_offset(pixel_offset, bytes.len()))?;
        if pixel_end > bytes.len() {
            return Err(invalid_offset(pixel_end, bytes.len()));
        }
        let trailing = &bytes[pixel_end..];
        if trailing.iter().any(|byte| *byte != 0) {
            return Err(FormatError::Unsupported {
                format: FORMAT,
                message: "non-zero file-creator bytes follow the final scanline".to_string(),
            });
        }

        Ok(Self {
            reserved_1: le_u16(bytes, 6)?,
            reserved_2: le_u16(bytes, 8)?,
            width,
            height,
            planes,
            bits_per_pixel,
            compression,
            declared_image_size: le_u32(bytes, 34)?,
            horizontal_pixels_per_meter: le_i32(bytes, 38)?,
            vertical_pixels_per_meter: le_i32(bytes, 42)?,
            colors_used: le_u32(bytes, 46)?,
            important_colors: le_u32(bytes, 50)?,
            palette,
            encoded_pixels: bytes[pixel_offset..pixel_end].to_vec(),
            trailing_zero_bytes: u32::try_from(trailing.len())
                .map_err(|_| resource_limit("trailing terminator bytes", trailing.len()))?,
        })
    }

    pub fn encode(&self) -> Result<Vec<u8>> {
        self.validate()?;
        let palette_bytes = self
            .palette
            .len()
            .checked_mul(4)
            .ok_or_else(|| resource_limit("palette bytes", usize::MAX))?;
        let pixel_offset = FILE_HEADER_SIZE
            .checked_add(INFO_HEADER_SIZE)
            .and_then(|size| size.checked_add(palette_bytes))
            .ok_or_else(|| resource_limit("bitmap bytes", usize::MAX))?;
        let file_size = pixel_offset
            .checked_add(self.encoded_pixels.len())
            .and_then(|size| size.checked_add(self.trailing_zero_bytes as usize))
            .ok_or_else(|| resource_limit("bitmap bytes", usize::MAX))?;
        let file_size_u32 =
            u32::try_from(file_size).map_err(|_| resource_limit("bitmap bytes", file_size))?;
        let pixel_offset_u32 = u32::try_from(pixel_offset)
            .map_err(|_| resource_limit("bitmap header offset", pixel_offset))?;

        let mut bytes = vec![0; file_size];
        bytes[..2].copy_from_slice(b"BM");
        put_u32(&mut bytes, 2, file_size_u32)?;
        put_u16(&mut bytes, 6, self.reserved_1)?;
        put_u16(&mut bytes, 8, self.reserved_2)?;
        put_u32(&mut bytes, 10, pixel_offset_u32)?;
        put_u32(&mut bytes, 14, INFO_HEADER_SIZE as u32)?;
        put_i32(&mut bytes, 18, self.width)?;
        put_i32(&mut bytes, 22, self.height)?;
        put_u16(&mut bytes, 26, self.planes)?;
        put_u16(&mut bytes, 28, self.bits_per_pixel)?;
        put_u32(&mut bytes, 30, self.compression)?;
        put_u32(&mut bytes, 34, self.declared_image_size)?;
        put_i32(&mut bytes, 38, self.horizontal_pixels_per_meter)?;
        put_i32(&mut bytes, 42, self.vertical_pixels_per_meter)?;
        put_u32(&mut bytes, 46, self.colors_used)?;
        put_u32(&mut bytes, 50, self.important_colors)?;
        for (index, entry) in self.palette.iter().enumerate() {
            let start = FILE_HEADER_SIZE + INFO_HEADER_SIZE + index * 4;
            bytes[start..start + 4].copy_from_slice(entry);
        }
        bytes[pixel_offset..pixel_offset + self.encoded_pixels.len()]
            .copy_from_slice(&self.encoded_pixels);
        Ok(bytes)
    }

    pub fn row_stride(&self) -> Result<usize> {
        row_stride(self.width, self.bits_per_pixel)
    }

    fn validate(&self) -> Result<()> {
        if self.width <= 0
            || self.height == 0
            || self.planes != 1
            || self.bits_per_pixel != 8
            || self.compression != 0
        {
            return Err(FormatError::Unsupported {
                format: FORMAT,
                message: "only uncompressed 8-bit pollution bitmaps can be encoded".to_string(),
            });
        }
        let expected = self
            .row_stride()?
            .checked_mul(self.height.unsigned_abs() as usize)
            .ok_or_else(|| resource_limit("pixel bytes", usize::MAX))?;
        if self.encoded_pixels.len() != expected {
            return Err(FormatError::Unsupported {
                format: FORMAT,
                message: format!(
                    "encoded pixel allocation has {} bytes, expected {expected}",
                    self.encoded_pixels.len()
                ),
            });
        }
        Ok(())
    }
}

fn row_stride(width: i32, bits_per_pixel: u16) -> Result<usize> {
    let width = usize::try_from(width).map_err(|_| resource_limit("bitmap width", usize::MAX))?;
    width
        .checked_mul(bits_per_pixel as usize)
        .and_then(|bits| bits.checked_add(31))
        .map(|bits| (bits / 32) * 4)
        .ok_or_else(|| resource_limit("scanline bytes", usize::MAX))
}

fn require_len(bytes: &[u8], expected: usize) -> Result<()> {
    if bytes.len() < expected {
        return Err(FormatError::TooSmall {
            format: FORMAT,
            expected,
            actual: bytes.len(),
        });
    }
    Ok(())
}

fn le_u16(bytes: &[u8], offset: usize) -> Result<u16> {
    let value = bytes
        .get(offset..offset + 2)
        .ok_or_else(|| invalid_offset(offset, bytes.len()))?;
    Ok(u16::from_le_bytes(value.try_into().unwrap()))
}

fn le_u32(bytes: &[u8], offset: usize) -> Result<u32> {
    let value = bytes
        .get(offset..offset + 4)
        .ok_or_else(|| invalid_offset(offset, bytes.len()))?;
    Ok(u32::from_le_bytes(value.try_into().unwrap()))
}

fn le_i32(bytes: &[u8], offset: usize) -> Result<i32> {
    Ok(le_u32(bytes, offset)? as i32)
}

fn put_u16(bytes: &mut [u8], offset: usize, value: u16) -> Result<()> {
    let len = bytes.len();
    bytes
        .get_mut(offset..offset + 2)
        .ok_or_else(|| invalid_offset(offset, len))?
        .copy_from_slice(&value.to_le_bytes());
    Ok(())
}

fn put_u32(bytes: &mut [u8], offset: usize, value: u32) -> Result<()> {
    let len = bytes.len();
    bytes
        .get_mut(offset..offset + 4)
        .ok_or_else(|| invalid_offset(offset, len))?
        .copy_from_slice(&value.to_le_bytes());
    Ok(())
}

fn put_i32(bytes: &mut [u8], offset: usize, value: i32) -> Result<()> {
    put_u32(bytes, offset, value as u32)
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
    fn semantic_bitmap_rebuilds_after_source_is_destroyed() {
        let bitmap = BmpFile {
            reserved_1: 0,
            reserved_2: 0,
            width: 2,
            height: 2,
            planes: 1,
            bits_per_pixel: 8,
            compression: 0,
            declared_image_size: 0,
            horizontal_pixels_per_meter: 3780,
            vertical_pixels_per_meter: 3780,
            colors_used: 2,
            important_colors: 0,
            palette: vec![[0, 0, 0, 0], [0xFF, 0xFF, 0xFF, 0]],
            encoded_pixels: vec![0, 1, 0, 0, 1, 0, 0, 0],
            trailing_zero_bytes: 2,
        };
        let mut source = bitmap.encode().unwrap();
        let expected = source.clone();
        let mut parsed = BmpFile::parse(&source).unwrap();
        source.fill(0xA5);
        assert_eq!(parsed.encode().unwrap(), expected);
        parsed.encoded_pixels[0] ^= 1;
        assert_ne!(parsed.encode().unwrap(), expected);
    }

    #[test]
    fn pollution_bitmap_rebuilds_after_source_is_destroyed() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .join(".codex_scratch/airport0-pollution00.bmp");
        if !path.is_file() {
            return;
        }
        let mut source = std::fs::read(path).unwrap();
        let expected = source.clone();
        let mut bitmap = BmpFile::parse(&source).unwrap();
        source.fill(0xA5);
        assert_eq!(bitmap.encode().unwrap(), expected);
        bitmap.encoded_pixels[0] ^= 1;
        assert_ne!(bitmap.encode().unwrap(), expected);
    }

    #[test]
    #[ignore = "requires SMS_BASE_ROOT with extracted retail stage archives"]
    fn source_free_rebuilds_every_retail_stage_bitmap() {
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
                    .ends_with(".bmp")
                {
                    continue;
                }
                let source = crate::read_stage_asset_bytes(&asset.path)
                    .unwrap_or_else(|error| panic!("read {}: {error}", asset.path.display()));
                let document = BmpFile::parse(&source)
                    .unwrap_or_else(|error| panic!("parse {}: {error}", asset.path.display()));
                assert_eq!(
                    document.encode().expect("encode semantic BMP"),
                    source,
                    "source-free BMP rebuild differs for {}",
                    asset.path.display()
                );
                rebuilt += 1;
            }
        }
        assert_eq!(rebuilt, 91, "retail BMP count drifted");
        eprintln!("source-free BMP census rebuilt {rebuilt} files");
    }
}
