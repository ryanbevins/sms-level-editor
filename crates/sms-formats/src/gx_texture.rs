//! Deterministic source-image to native GX texture authoring.
//!
//! The encoder deliberately accepts RGBA8 pixels rather than an image-file
//! format.  PNG/JPEG/glTF decoding belongs to the importer; this module owns
//! the exact tiled bytes stored by BTI and J3D TEX1 resources.

use std::collections::BTreeMap;

use encoding_rs::SHIFT_JIS;
use serde::{Deserialize, Serialize};

use crate::{
    BtiFile, FormatError, J3dNameEntry, J3dNameTable, J3dPaddingSpan, J3dRebuildSection,
    J3dRebuildSectionData, J3dTextureBlock, J3dTextureRecord, J3dTextureSection, Result,
};

const FORMAT: &str = "GX texture authoring";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[repr(u8)]
pub enum GxTextureFormat {
    I4 = 0,
    I8 = 1,
    Ia4 = 2,
    Ia8 = 3,
    Rgb565 = 4,
    Rgb5A3 = 5,
    Rgba8 = 6,
    C4 = 8,
    C8 = 9,
    C14X2 = 10,
    Cmpr = 14,
}

impl GxTextureFormat {
    pub const ALL: [Self; 11] = [
        Self::I4,
        Self::I8,
        Self::Ia4,
        Self::Ia8,
        Self::Rgb565,
        Self::Rgb5A3,
        Self::Rgba8,
        Self::C4,
        Self::C8,
        Self::C14X2,
        Self::Cmpr,
    ];

    pub const fn palette_capacity(self) -> Option<usize> {
        match self {
            Self::C4 => Some(16),
            Self::C8 => Some(256),
            Self::C14X2 => Some(16_384),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[repr(u8)]
pub enum GxPaletteFormat {
    Ia8 = 0,
    Rgb565 = 1,
    Rgb5A3 = 2,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum GxTextureEncoding {
    /// I8 when R=G=B=A, IA8 when R=G=B, otherwise lossless RGBA8.
    AutoLossless,
    Exact(GxTextureFormat),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct GxSampler {
    pub wrap_s: u8,
    pub wrap_t: u8,
    pub min_filter: u8,
    pub mag_filter: u8,
    pub edge_lod: bool,
    pub bias_clamp: bool,
    pub max_anisotropy: u8,
    /// Native signed 1/8 LOD units.
    pub min_lod: i8,
    /// Native signed 1/8 LOD units.
    pub max_lod: i8,
    /// Native signed 1/100 LOD units.
    pub lod_bias: i16,
}

impl Default for GxSampler {
    fn default() -> Self {
        Self {
            wrap_s: 1,
            wrap_t: 1,
            min_filter: 1,
            mag_filter: 1,
            edge_lod: false,
            bias_clamp: false,
            max_anisotropy: 0,
            min_lod: 0,
            max_lod: 0,
            lod_bias: 0,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RgbaImage {
    pub width: u16,
    pub height: u16,
    /// Row-major RGBA8 pixels.
    pub pixels: Vec<u8>,
}

impl RgbaImage {
    pub fn new(width: u16, height: u16, pixels: Vec<u8>) -> Result<Self> {
        let image = Self {
            width,
            height,
            pixels,
        };
        image.validate()?;
        Ok(image)
    }

    pub fn validate(&self) -> Result<()> {
        if self.width == 0 || self.height == 0 {
            return Err(unsupported("texture dimensions must be nonzero"));
        }
        let expected = self.width as usize * self.height as usize * 4;
        if self.pixels.len() != expected {
            return Err(unsupported(format!(
                "{}x{} RGBA image has {} bytes, expected {expected}",
                self.width,
                self.height,
                self.pixels.len()
            )));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct GxTextureEncodeOptions {
    pub encoding: GxTextureEncoding,
    pub palette_format: GxPaletteFormat,
    /// One means only the base level. Zero requests the complete chain.
    pub mip_count: u8,
    pub sampler: GxSampler,
}

impl Default for GxTextureEncodeOptions {
    fn default() -> Self {
        Self {
            encoding: GxTextureEncoding::AutoLossless,
            palette_format: GxPaletteFormat::Rgb5A3,
            mip_count: 1,
            sampler: GxSampler::default(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GxEncodedTexture {
    pub name: String,
    pub format: GxTextureFormat,
    pub palette_format: GxPaletteFormat,
    /// Native ResTIMG transparency classification (zero is opaque).
    pub transparency: u8,
    pub width: u16,
    pub height: u16,
    pub sampler: GxSampler,
    pub palette: Vec<u16>,
    pub encoded_mips: Vec<Vec<u8>>,
}

impl GxEncodedTexture {
    pub fn encode_rgba(
        name: impl Into<String>,
        image: &RgbaImage,
        options: GxTextureEncodeOptions,
    ) -> Result<Self> {
        image.validate()?;
        validate_shift_jis_name(&name.into()).and_then(|name| {
            let format = match options.encoding {
                GxTextureEncoding::Exact(format) => format,
                GxTextureEncoding::AutoLossless => auto_lossless_format(&image.pixels),
            };
            let mip_count = requested_mip_count(image.width, image.height, options.mip_count);
            let mut rgba_mips = Vec::with_capacity(mip_count as usize);
            rgba_mips.push(image.clone());
            while rgba_mips.len() < mip_count as usize {
                let next = downsample_box(rgba_mips.last().expect("base mip exists"));
                rgba_mips.push(next);
            }

            let (palette, palette_lookup) = if let Some(capacity) = format.palette_capacity() {
                build_palette(&rgba_mips, options.palette_format, capacity)
            } else {
                (Vec::new(), BTreeMap::new())
            };
            let encoded_mips = rgba_mips
                .iter()
                .map(|mip| {
                    encode_level(
                        format,
                        options.palette_format,
                        &palette,
                        &palette_lookup,
                        mip,
                    )
                })
                .collect::<Result<Vec<_>>>()?;
            let mut sampler = options.sampler;
            if mip_count > 1 {
                sampler.max_lod = ((mip_count - 1) as i16 * 8).min(i8::MAX as i16) as i8;
            }
            Ok(Self {
                name,
                format,
                palette_format: options.palette_format,
                transparency: encoded_transparency(format, options.palette_format, &rgba_mips),
                width: image.width,
                height: image.height,
                sampler,
                palette,
                encoded_mips,
            })
        })
    }

    pub fn to_bti(&self) -> Result<BtiFile> {
        self.validate()?;
        let mut bti = BtiFile {
            allocation_size: 0,
            format: self.format as u8,
            transparency: self.transparency,
            width: self.width,
            height: self.height,
            wrap_s: self.sampler.wrap_s,
            wrap_t: self.sampler.wrap_t,
            palette_enabled: u8::from(!self.palette.is_empty()),
            palette_format: self.palette_format as u8,
            palette_entries: self.palette.clone(),
            palette_offset: 0,
            mipmap_enabled: u8::from(self.encoded_mips.len() > 1),
            edge_lod: self.sampler.edge_lod.into(),
            bias_clamp: self.sampler.bias_clamp.into(),
            max_anisotropy: self.sampler.max_anisotropy,
            min_filter: self.sampler.min_filter,
            mag_filter: self.sampler.mag_filter,
            min_lod: self.sampler.min_lod,
            max_lod: self.sampler.max_lod,
            mipmap_count: self.encoded_mips.len() as u8,
            reserved_19: 0,
            lod_bias: self.sampler.lod_bias,
            image_offset: 0,
            encoded_mip_levels: self.encoded_mips.clone(),
        };
        bti.canonicalize_layout()?;
        Ok(bti)
    }

    fn validate(&self) -> Result<()> {
        if self.encoded_mips.is_empty() || self.encoded_mips.len() > u8::MAX as usize {
            return Err(unsupported("texture must contain 1..=255 mip levels"));
        }
        if self.format.palette_capacity().is_some() == self.palette.is_empty() {
            return Err(unsupported(
                "paletted formats require a palette and direct formats forbid one",
            ));
        }
        if let Some(capacity) = self.format.palette_capacity() {
            if self.palette.len() > capacity {
                return Err(unsupported(format!(
                    "{:?} palette has {} entries, limit is {capacity}",
                    self.format,
                    self.palette.len()
                )));
            }
        }
        Ok(())
    }
}

/// Builds a canonical TEX1 section. Texture headers and native allocations are
/// 32-byte aligned and never alias, making repeated compilation byte-stable.
pub fn compile_texture_section(textures: &[GxEncodedTexture]) -> Result<J3dRebuildSection> {
    if textures.len() > u16::MAX as usize {
        return Err(limit("TEX1 textures", textures.len(), u16::MAX as usize));
    }
    for texture in textures {
        texture.validate()?;
    }
    let header_offset = 0x20usize;
    let headers_end = header_offset + textures.len() * 0x20;
    let mut names = name_table(textures.iter().map(|texture| texture.name.as_str()))?;
    let name_table_size = canonicalize_name_table(&mut names)?;
    let name_table_offset = align(headers_end, 4)?;
    let mut cursor = align(name_table_offset + name_table_size, 0x20)?;
    let mut records = Vec::with_capacity(textures.len());

    for (index, texture) in textures.iter().enumerate() {
        let base = header_offset + index * 0x20;
        let encoded_palette = if texture.palette.is_empty() {
            None
        } else {
            let bytes = texture
                .palette
                .iter()
                .flat_map(|entry| entry.to_be_bytes())
                .collect::<Vec<_>>();
            let block = J3dTextureBlock {
                absolute_section_offset: cursor as u32,
                bytes,
            };
            cursor = align(cursor + block.bytes.len(), 0x20)?;
            Some(block)
        };
        let image_start = cursor;
        let mut encoded_mip_levels = Vec::with_capacity(texture.encoded_mips.len());
        for mip in &texture.encoded_mips {
            encoded_mip_levels.push(J3dTextureBlock {
                absolute_section_offset: cursor as u32,
                bytes: mip.clone(),
            });
            cursor = cursor
                .checked_add(mip.len())
                .ok_or_else(|| unsupported("TEX1 allocation overflow"))?;
        }
        cursor = align(cursor, 0x20)?;
        records.push(J3dTextureRecord {
            header_relative_offset: (index * 0x20) as u32,
            format: texture.format as u8,
            transparency: texture.transparency,
            width: texture.width,
            height: texture.height,
            wrap_s: texture.sampler.wrap_s,
            wrap_t: texture.sampler.wrap_t,
            palette_enabled: u8::from(encoded_palette.is_some()),
            palette_format: texture.palette_format as u8,
            palette_entry_count: texture.palette.len() as u16,
            palette_offset: encoded_palette
                .as_ref()
                .map_or(0, |block| block.absolute_section_offset - base as u32),
            mipmap_enabled: u8::from(texture.encoded_mips.len() > 1),
            edge_lod: texture.sampler.edge_lod.into(),
            bias_clamp: texture.sampler.bias_clamp.into(),
            max_anisotropy: texture.sampler.max_anisotropy,
            min_filter: texture.sampler.min_filter,
            mag_filter: texture.sampler.mag_filter,
            min_lod: texture.sampler.min_lod,
            max_lod: texture.sampler.max_lod,
            mipmap_count: texture.encoded_mips.len() as u8,
            reserved_19: 0,
            lod_bias: texture.sampler.lod_bias,
            image_offset: image_start as u32 - base as u32,
            encoded_mip_levels,
            encoded_palette,
        });
    }
    let declared_size = align(cursor, 0x20)? as u32;
    Ok(J3dRebuildSection {
        declared_size,
        data: J3dRebuildSectionData::Textures(J3dTextureSection {
            texture_count: textures.len() as u16,
            reserved: u16::MAX,
            header_offset: header_offset as u32,
            name_table_offset: name_table_offset as u32,
            textures: records,
            names,
        }),
        padding: Vec::<J3dPaddingSpan>::new(),
    })
}

fn auto_lossless_format(pixels: &[u8]) -> GxTextureFormat {
    if pixels
        .chunks_exact(4)
        .all(|pixel| pixel[0] == pixel[1] && pixel[1] == pixel[2] && pixel[2] == pixel[3])
    {
        GxTextureFormat::I8
    } else if pixels
        .chunks_exact(4)
        .all(|pixel| pixel[0] == pixel[1] && pixel[1] == pixel[2])
    {
        GxTextureFormat::Ia8
    } else {
        GxTextureFormat::Rgba8
    }
}

fn encoded_transparency(
    format: GxTextureFormat,
    palette_format: GxPaletteFormat,
    mips: &[RgbaImage],
) -> u8 {
    let has_alpha = mips.iter().any(|mip| {
        mip.pixels.chunks_exact(4).any(|pixel| match format {
            GxTextureFormat::I4 | GxTextureFormat::I8 => {
                intensity(pixel.try_into().expect("RGBA chunk")) < 255
            }
            GxTextureFormat::Ia4 | GxTextureFormat::Ia8 | GxTextureFormat::Rgba8 => pixel[3] < 255,
            GxTextureFormat::Rgb565 => false,
            GxTextureFormat::Rgb5A3 => pixel[3] < 224,
            GxTextureFormat::C4 | GxTextureFormat::C8 | GxTextureFormat::C14X2 => {
                match palette_format {
                    GxPaletteFormat::Ia8 => pixel[3] < 255,
                    GxPaletteFormat::Rgb565 => false,
                    GxPaletteFormat::Rgb5A3 => pixel[3] < 224,
                }
            }
            GxTextureFormat::Cmpr => pixel[3] < 128,
        })
    });
    u8::from(has_alpha)
}

fn requested_mip_count(width: u16, height: u16, requested: u8) -> u8 {
    let mut complete = 1u8;
    let (mut width, mut height) = (width, height);
    while width > 1 || height > 1 {
        width = (width / 2).max(1);
        height = (height / 2).max(1);
        complete = complete.saturating_add(1);
    }
    if requested == 0 {
        complete
    } else {
        requested.min(complete).max(1)
    }
}

fn downsample_box(source: &RgbaImage) -> RgbaImage {
    let width = (source.width / 2).max(1);
    let height = (source.height / 2).max(1);
    let mut pixels = vec![0; width as usize * height as usize * 4];
    for y in 0..height as usize {
        for x in 0..width as usize {
            let mut sum = [0u32; 4];
            let mut count = 0u32;
            for dy in 0..2 {
                for dx in 0..2 {
                    let sx = (x * 2 + dx).min(source.width as usize - 1);
                    let sy = (y * 2 + dy).min(source.height as usize - 1);
                    let offset = (sy * source.width as usize + sx) * 4;
                    for (channel, total) in sum.iter_mut().enumerate() {
                        *total += source.pixels[offset + channel] as u32;
                    }
                    count += 1;
                }
            }
            let offset = (y * width as usize + x) * 4;
            for channel in 0..4 {
                pixels[offset + channel] = ((sum[channel] + count / 2) / count) as u8;
            }
        }
    }
    RgbaImage {
        width,
        height,
        pixels,
    }
}

fn encode_level(
    format: GxTextureFormat,
    palette_format: GxPaletteFormat,
    palette: &[u16],
    exact_palette_lookup: &BTreeMap<u16, usize>,
    image: &RgbaImage,
) -> Result<Vec<u8>> {
    match format {
        GxTextureFormat::I4 => encode_tiled(image, 8, 8, 32, |tile, x, y, color| {
            let value = quantize(intensity(color), 4);
            let index = y * 8 + x;
            if index.is_multiple_of(2) {
                tile[index / 2] |= value << 4;
            } else {
                tile[index / 2] |= value;
            }
        }),
        GxTextureFormat::I8 => encode_tiled(image, 8, 4, 32, |tile, x, y, color| {
            tile[y * 8 + x] = intensity(color);
        }),
        GxTextureFormat::Ia4 => encode_tiled(image, 8, 4, 32, |tile, x, y, color| {
            tile[y * 8 + x] = (quantize(color[3], 4) << 4) | quantize(intensity(color), 4);
        }),
        GxTextureFormat::Ia8 => encode_tiled(image, 4, 4, 32, |tile, x, y, color| {
            let offset = (y * 4 + x) * 2;
            tile[offset] = color[3];
            tile[offset + 1] = intensity(color);
        }),
        GxTextureFormat::Rgb565 => encode_tiled(image, 4, 4, 32, |tile, x, y, color| {
            write_u16(tile, (y * 4 + x) * 2, rgba_to_rgb565(color));
        }),
        GxTextureFormat::Rgb5A3 => encode_tiled(image, 4, 4, 32, |tile, x, y, color| {
            write_u16(tile, (y * 4 + x) * 2, rgba_to_rgb5a3(color));
        }),
        GxTextureFormat::Rgba8 => encode_tiled(image, 4, 4, 64, |tile, x, y, color| {
            let pixel = y * 4 + x;
            tile[pixel * 2] = color[3];
            tile[pixel * 2 + 1] = color[0];
            tile[32 + pixel * 2] = color[1];
            tile[32 + pixel * 2 + 1] = color[2];
        }),
        GxTextureFormat::C4 => encode_palette_indices(
            image,
            8,
            8,
            32,
            palette_format,
            palette,
            exact_palette_lookup,
            4,
        ),
        GxTextureFormat::C8 => encode_palette_indices(
            image,
            8,
            4,
            32,
            palette_format,
            palette,
            exact_palette_lookup,
            8,
        ),
        GxTextureFormat::C14X2 => encode_palette_indices(
            image,
            4,
            4,
            32,
            palette_format,
            palette,
            exact_palette_lookup,
            14,
        ),
        GxTextureFormat::Cmpr => encode_cmpr(image),
    }
}

fn encode_tiled(
    image: &RgbaImage,
    tile_width: usize,
    tile_height: usize,
    tile_bytes: usize,
    mut write_pixel: impl FnMut(&mut [u8], usize, usize, [u8; 4]),
) -> Result<Vec<u8>> {
    let tiles_x = (image.width as usize).div_ceil(tile_width);
    let tiles_y = (image.height as usize).div_ceil(tile_height);
    let mut out = vec![0; tiles_x * tiles_y * tile_bytes];
    for tile_y in 0..tiles_y {
        for tile_x in 0..tiles_x {
            let start = (tile_y * tiles_x + tile_x) * tile_bytes;
            let tile = &mut out[start..start + tile_bytes];
            for y in 0..tile_height {
                for x in 0..tile_width {
                    let sx = tile_x * tile_width + x;
                    let sy = tile_y * tile_height + y;
                    let color = if sx < image.width as usize && sy < image.height as usize {
                        rgba_at(image, sx, sy)
                    } else {
                        [0; 4]
                    };
                    write_pixel(tile, x, y, color);
                }
            }
        }
    }
    Ok(out)
}

#[allow(clippy::too_many_arguments)]
fn encode_palette_indices(
    image: &RgbaImage,
    tile_width: usize,
    tile_height: usize,
    tile_bytes: usize,
    palette_format: GxPaletteFormat,
    palette: &[u16],
    exact_lookup: &BTreeMap<u16, usize>,
    bits: u8,
) -> Result<Vec<u8>> {
    encode_tiled(
        image,
        tile_width,
        tile_height,
        tile_bytes,
        |tile, x, y, color| {
            let encoded = encode_palette_color(palette_format, color);
            let index = exact_lookup
                .get(&encoded)
                .copied()
                .unwrap_or_else(|| nearest_palette(encoded, palette, palette_format));
            let pixel = y * tile_width + x;
            match bits {
                4 if pixel.is_multiple_of(2) => tile[pixel / 2] |= (index as u8) << 4,
                4 => tile[pixel / 2] |= index as u8,
                8 => tile[pixel] = index as u8,
                14 => write_u16(tile, pixel * 2, index as u16 & 0x3fff),
                _ => unreachable!(),
            }
        },
    )
}

fn build_palette(
    mips: &[RgbaImage],
    format: GxPaletteFormat,
    capacity: usize,
) -> (Vec<u16>, BTreeMap<u16, usize>) {
    let mut histogram = BTreeMap::<u16, u64>::new();
    for mip in mips {
        for pixel in mip.pixels.chunks_exact(4) {
            let color = encode_palette_color(format, pixel.try_into().expect("RGBA chunk"));
            *histogram.entry(color).or_default() += 1;
        }
    }
    let mut ranked = histogram.into_iter().collect::<Vec<_>>();
    ranked.sort_by(|(color_a, count_a), (color_b, count_b)| {
        count_b.cmp(count_a).then_with(|| color_a.cmp(color_b))
    });
    let mut palette = ranked
        .into_iter()
        .take(capacity)
        .map(|(color, _)| color)
        .collect::<Vec<_>>();
    palette.sort_unstable();
    if palette.is_empty() {
        palette.push(0);
    }
    let exact_lookup = palette
        .iter()
        .copied()
        .enumerate()
        .map(|(index, color)| (color, index))
        .collect();
    (palette, exact_lookup)
}

fn nearest_palette(color: u16, palette: &[u16], format: GxPaletteFormat) -> usize {
    let source = decode_palette_color(format, color);
    palette
        .iter()
        .enumerate()
        .map(|(index, candidate)| {
            (
                index,
                color_distance(source, decode_palette_color(format, *candidate)),
            )
        })
        .min_by_key(|(index, distance)| (*distance, *index))
        .map_or(0, |(index, _)| index)
}

fn encode_cmpr(image: &RgbaImage) -> Result<Vec<u8>> {
    let tiles_x = (image.width as usize).div_ceil(8);
    let tiles_y = (image.height as usize).div_ceil(8);
    let mut out = vec![0; tiles_x * tiles_y * 32];
    for tile_y in 0..tiles_y {
        for tile_x in 0..tiles_x {
            for sub in 0..4 {
                let sub_x = sub % 2;
                let sub_y = sub / 2;
                let mut colors = [[0; 4]; 16];
                for y in 0..4 {
                    for x in 0..4 {
                        let sx = tile_x * 8 + sub_x * 4 + x;
                        let sy = tile_y * 8 + sub_y * 4 + y;
                        colors[y * 4 + x] =
                            if sx < image.width as usize && sy < image.height as usize {
                                rgba_at(image, sx, sy)
                            } else {
                                [0; 4]
                            };
                    }
                }
                let encoded = encode_cmpr_block(&colors);
                let start = (tile_y * tiles_x + tile_x) * 32 + sub * 8;
                out[start..start + 8].copy_from_slice(&encoded);
            }
        }
    }
    Ok(out)
}

fn encode_cmpr_block(colors: &[[u8; 4]; 16]) -> [u8; 8] {
    let transparent = colors.iter().any(|color| color[3] < 128);
    let mut opaque = colors
        .iter()
        .copied()
        .filter(|color| color[3] >= 128)
        .collect::<Vec<_>>();
    if opaque.is_empty() {
        return [0, 0, 0, 0, 0xff, 0xff, 0xff, 0xff];
    }
    opaque.sort_by_key(|color| (intensity(*color), color[0], color[1], color[2]));
    let mut c0 = rgba_to_rgb565(*opaque.last().expect("opaque color"));
    let mut c1 = rgba_to_rgb565(opaque[0]);
    if transparent {
        if c0 > c1 {
            std::mem::swap(&mut c0, &mut c1);
        }
    } else {
        if c0 <= c1 {
            std::mem::swap(&mut c0, &mut c1);
        }
        if c0 == c1 {
            if c0 == u16::MAX {
                c1 -= 1;
            } else {
                c0 += 1;
            }
        }
    }
    let candidates = cmpr_palette(c0, c1);
    let mut bits = 0u32;
    for color in colors {
        let index = if color[3] < 128 && c0 <= c1 {
            3
        } else {
            candidates
                .iter()
                .enumerate()
                .min_by_key(|(index, candidate)| {
                    let alpha_penalty = if *index == 3 && c0 <= c1 {
                        1_000_000
                    } else {
                        0
                    };
                    color_distance(*color, **candidate) + alpha_penalty
                })
                .map_or(0, |(index, _)| index)
        };
        bits = (bits << 2) | index as u32;
    }
    let mut out = [0; 8];
    out[..2].copy_from_slice(&c0.to_be_bytes());
    out[2..4].copy_from_slice(&c1.to_be_bytes());
    out[4..].copy_from_slice(&bits.to_be_bytes());
    out
}

fn rgba_at(image: &RgbaImage, x: usize, y: usize) -> [u8; 4] {
    let offset = (y * image.width as usize + x) * 4;
    image.pixels[offset..offset + 4]
        .try_into()
        .expect("validated image")
}

fn intensity(color: [u8; 4]) -> u8 {
    ((color[0] as u32 * 77 + color[1] as u32 * 150 + color[2] as u32 * 29 + 128) >> 8) as u8
}

fn quantize(value: u8, bits: u8) -> u8 {
    let max = (1u16 << bits) - 1;
    ((value as u16 * max + 127) / 255) as u8
}

fn rgba_to_rgb565(color: [u8; 4]) -> u16 {
    ((quantize(color[0], 5) as u16) << 11)
        | ((quantize(color[1], 6) as u16) << 5)
        | quantize(color[2], 5) as u16
}

fn rgba_to_rgb5a3(color: [u8; 4]) -> u16 {
    if color[3] >= 224 {
        0x8000
            | ((quantize(color[0], 5) as u16) << 10)
            | ((quantize(color[1], 5) as u16) << 5)
            | quantize(color[2], 5) as u16
    } else {
        ((quantize(color[3], 3) as u16) << 12)
            | ((quantize(color[0], 4) as u16) << 8)
            | ((quantize(color[1], 4) as u16) << 4)
            | quantize(color[2], 4) as u16
    }
}

fn encode_palette_color(format: GxPaletteFormat, color: [u8; 4]) -> u16 {
    match format {
        GxPaletteFormat::Ia8 => ((color[3] as u16) << 8) | intensity(color) as u16,
        GxPaletteFormat::Rgb565 => rgba_to_rgb565(color),
        GxPaletteFormat::Rgb5A3 => rgba_to_rgb5a3(color),
    }
}

fn decode_palette_color(format: GxPaletteFormat, value: u16) -> [u8; 4] {
    match format {
        GxPaletteFormat::Ia8 => {
            let intensity = value as u8;
            [intensity, intensity, intensity, (value >> 8) as u8]
        }
        GxPaletteFormat::Rgb565 => rgb565_to_rgba(value),
        GxPaletteFormat::Rgb5A3 => rgb5a3_to_rgba(value),
    }
}

fn rgb565_to_rgba(value: u16) -> [u8; 4] {
    [
        expand((value >> 11) & 31, 5),
        expand((value >> 5) & 63, 6),
        expand(value & 31, 5),
        255,
    ]
}

fn rgb5a3_to_rgba(value: u16) -> [u8; 4] {
    if value & 0x8000 != 0 {
        [
            expand((value >> 10) & 31, 5),
            expand((value >> 5) & 31, 5),
            expand(value & 31, 5),
            255,
        ]
    } else {
        [
            expand((value >> 8) & 15, 4),
            expand((value >> 4) & 15, 4),
            expand(value & 15, 4),
            expand((value >> 12) & 7, 3),
        ]
    }
}

fn cmpr_palette(c0: u16, c1: u16) -> [[u8; 4]; 4] {
    let a = rgb565_to_rgba(c0);
    let b = rgb565_to_rgba(c1);
    if c0 > c1 {
        [a, b, mix(a, b, 2, 1, 3), mix(a, b, 1, 2, 3)]
    } else {
        [a, b, mix(a, b, 1, 1, 2), [0, 0, 0, 0]]
    }
}

fn mix(a: [u8; 4], b: [u8; 4], wa: u16, wb: u16, div: u16) -> [u8; 4] {
    std::array::from_fn(|channel| ((a[channel] as u16 * wa + b[channel] as u16 * wb) / div) as u8)
}

fn expand(value: u16, bits: u8) -> u8 {
    ((value * 255) / ((1u16 << bits) - 1)) as u8
}

fn color_distance(a: [u8; 4], b: [u8; 4]) -> u32 {
    a.into_iter()
        .zip(b)
        .map(|(a, b)| {
            let delta = a as i32 - b as i32;
            (delta * delta) as u32
        })
        .sum()
}

fn write_u16(bytes: &mut [u8], offset: usize, value: u16) {
    bytes[offset..offset + 2].copy_from_slice(&value.to_be_bytes());
}

fn validate_shift_jis_name(name: &str) -> Result<String> {
    let (_, _, had_errors) = SHIFT_JIS.encode(name);
    if had_errors {
        Err(unsupported(format!(
            "texture name {name:?} cannot be encoded as Shift-JIS"
        )))
    } else {
        Ok(name.to_string())
    }
}

fn name_table<'a>(names: impl IntoIterator<Item = &'a str>) -> Result<J3dNameTable> {
    Ok(J3dNameTable {
        reserved: u16::MAX,
        entries: names
            .into_iter()
            .map(|name| {
                validate_shift_jis_name(name).map(|name| J3dNameEntry {
                    hash: 0,
                    string_offset: 0,
                    name,
                })
            })
            .collect::<Result<Vec<_>>>()?,
    })
}

fn canonicalize_name_table(table: &mut J3dNameTable) -> Result<usize> {
    let mut cursor = 4usize
        .checked_add(table.entries.len() * 4)
        .ok_or_else(|| unsupported("name table size overflow"))?;
    for entry in &mut table.entries {
        let (encoded, _, had_errors) = SHIFT_JIS.encode(&entry.name);
        if had_errors {
            return Err(unsupported(format!(
                "name {:?} cannot be encoded as Shift-JIS",
                entry.name
            )));
        }
        entry.hash = encoded.iter().fold(0u16, |hash, byte| {
            hash.wrapping_mul(3).wrapping_add(*byte as u16)
        });
        entry.string_offset =
            u16::try_from(cursor).map_err(|_| unsupported("name table exceeds 65535 bytes"))?;
        cursor = cursor
            .checked_add(encoded.len() + 1)
            .ok_or_else(|| unsupported("name table size overflow"))?;
    }
    Ok(cursor)
}

fn align(value: usize, alignment: usize) -> Result<usize> {
    value
        .checked_add(alignment - 1)
        .map(|value| value & !(alignment - 1))
        .ok_or_else(|| unsupported("allocation alignment overflow"))
}

fn limit(resource: &'static str, requested: usize, limit: usize) -> FormatError {
    FormatError::ResourceLimit {
        format: FORMAT,
        resource,
        requested,
        limit,
    }
}

fn unsupported(message: impl Into<String>) -> FormatError {
    FormatError::Unsupported {
        format: FORMAT,
        message: message.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{decode_bti_texture, J3dRebuildDocument};

    fn image(width: u16, height: u16) -> RgbaImage {
        let mut pixels = Vec::new();
        for y in 0..height {
            for x in 0..width {
                pixels.extend_from_slice(&[
                    (x * 37) as u8,
                    (y * 53) as u8,
                    ((x + y) * 29) as u8,
                    if (x + y).is_multiple_of(3) { 90 } else { 255 },
                ]);
            }
        }
        RgbaImage::new(width, height, pixels).unwrap()
    }

    #[test]
    fn every_gx_format_is_deterministic_and_decodable() {
        for format in GxTextureFormat::ALL {
            for palette_format in [
                GxPaletteFormat::Ia8,
                GxPaletteFormat::Rgb565,
                GxPaletteFormat::Rgb5A3,
            ] {
                if format.palette_capacity().is_none() && palette_format != GxPaletteFormat::Ia8 {
                    continue;
                }
                let options = GxTextureEncodeOptions {
                    encoding: GxTextureEncoding::Exact(format),
                    palette_format,
                    mip_count: 0,
                    sampler: GxSampler::default(),
                };
                let first = GxEncodedTexture::encode_rgba("fixture", &image(9, 7), options)
                    .expect("encode format");
                let second = GxEncodedTexture::encode_rgba("fixture", &image(9, 7), options)
                    .expect("repeat format");
                assert_eq!(first, second, "{format:?}/{palette_format:?}");
                let bti = first
                    .to_bti()
                    .expect("build BTI")
                    .encode()
                    .expect("encode BTI");
                let preview = decode_bti_texture(&bti).expect("decode authored texture");
                assert_eq!((preview.width, preview.height), (9, 7));
                assert_eq!(preview.mips.len(), 4);
            }
        }
    }

    #[test]
    fn auto_encoding_is_lossless_for_supported_classes() {
        let gray = RgbaImage::new(1, 1, vec![42, 42, 42, 42]).unwrap();
        assert_eq!(
            GxEncodedTexture::encode_rgba("gray", &gray, Default::default())
                .unwrap()
                .format,
            GxTextureFormat::I8
        );
        let alpha = RgbaImage::new(1, 1, vec![42, 42, 42, 99]).unwrap();
        assert_eq!(
            GxEncodedTexture::encode_rgba("alpha", &alpha, Default::default())
                .unwrap()
                .format,
            GxTextureFormat::Ia8
        );
        let color = RgbaImage::new(1, 1, vec![42, 43, 42, 99]).unwrap();
        assert_eq!(
            GxEncodedTexture::encode_rgba("color", &color, Default::default())
                .unwrap()
                .format,
            GxTextureFormat::Rgba8
        );
    }

    #[test]
    fn direct_format_tile_layout_goldens_match_gx() {
        let encode = |format, pixel: [u8; 4]| {
            GxEncodedTexture::encode_rgba(
                "golden",
                &RgbaImage::new(1, 1, pixel.to_vec()).unwrap(),
                GxTextureEncodeOptions {
                    encoding: GxTextureEncoding::Exact(format),
                    ..Default::default()
                },
            )
            .unwrap()
            .encoded_mips
            .remove(0)
        };
        assert_eq!(encode(GxTextureFormat::I4, [255; 4])[0], 0xf0);
        assert_eq!(encode(GxTextureFormat::I8, [42; 4])[0], 42);
        assert_eq!(encode(GxTextureFormat::Ia4, [255, 0, 0, 128])[0], 0x85);
        assert_eq!(
            &encode(GxTextureFormat::Ia8, [10, 20, 30, 40])[..2],
            &[40, 18]
        );
        assert_eq!(
            &encode(GxTextureFormat::Rgb565, [255, 0, 0, 255])[..2],
            &[0xf8, 0x00]
        );
        assert_eq!(
            &encode(GxTextureFormat::Rgb5A3, [255, 0, 0, 255])[..2],
            &[0xfc, 0x00]
        );
        let rgba8 = encode(GxTextureFormat::Rgba8, [1, 2, 3, 4]);
        assert_eq!(&rgba8[..2], &[4, 1]);
        assert_eq!(&rgba8[32..34], &[2, 3]);
    }

    #[test]
    fn palette_index_layout_goldens_match_gx() {
        let image = RgbaImage::new(2, 1, vec![255, 0, 0, 255, 0, 0, 255, 255]).unwrap();
        let encode = |format| {
            GxEncodedTexture::encode_rgba(
                "palette",
                &image,
                GxTextureEncodeOptions {
                    encoding: GxTextureEncoding::Exact(format),
                    palette_format: GxPaletteFormat::Rgb565,
                    ..Default::default()
                },
            )
            .unwrap()
        };
        let c4 = encode(GxTextureFormat::C4);
        assert_eq!(c4.palette, vec![0x001f, 0xf800]);
        assert_eq!(c4.encoded_mips[0][0], 0x10);
        let c8 = encode(GxTextureFormat::C8);
        assert_eq!(&c8.encoded_mips[0][..2], &[1, 0]);
        let c14 = encode(GxTextureFormat::C14X2);
        assert_eq!(&c14.encoded_mips[0][..4], &[0, 1, 0, 0]);
    }

    #[test]
    fn tex1_section_reopens_source_free() {
        let texture = GxEncodedTexture::encode_rgba(
            "texture",
            &image(8, 8),
            GxTextureEncodeOptions {
                encoding: GxTextureEncoding::Exact(GxTextureFormat::C8),
                palette_format: GxPaletteFormat::Rgb5A3,
                mip_count: 0,
                sampler: GxSampler::default(),
            },
        )
        .unwrap();
        let section = compile_texture_section(&[texture]).unwrap();
        let document = J3dRebuildDocument {
            file_type: *b"bmd3",
            version_tag: *b"SVR3",
            reserved_words: [u32::MAX; 3],
            declared_section_count: 1,
            sections: vec![section],
        };
        let first = document.to_bytes().unwrap();
        let reopened = J3dRebuildDocument::parse(&first).unwrap();
        assert_eq!(reopened.to_bytes().unwrap(), first);
    }
}
