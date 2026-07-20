use super::*;

const MAX_TEXTURE_DECODE_BYTES: usize = 256 * 1024 * 1024;

pub(super) fn decoded_timg_retained_bytes(bytes: &[u8], header_offset: usize) -> Result<usize> {
    let format = checked_slice(FORMAT, bytes, header_offset, 1)?[0];
    let width = be_u16(bytes, header_offset + 0x02, FORMAT)?;
    let height = be_u16(bytes, header_offset + 0x04, FORMAT)?;
    if width == 0 || height == 0 {
        return Err(FormatError::Unsupported {
            format: FORMAT,
            message: "texture has zero size".to_string(),
        });
    }
    let mipmap_count = checked_slice(FORMAT, bytes, header_offset + 0x18, 1)?[0];
    let relative_image_offset = match be_u32(bytes, header_offset + 0x1C, FORMAT)? as usize {
        0 => 0x20,
        offset => offset,
    };
    let mut level_offset = header_offset
        .checked_add(relative_image_offset)
        .ok_or_else(|| invalid_offset(header_offset, bytes.len()))?;
    let mut level_width = width;
    let mut level_height = height;
    let mut decoded_mip_bytes = 0usize;
    let mut base_mip_bytes = 0usize;

    for level in 0..mipmap_count.max(1) {
        let rgba_len = decoded_texture_level_size(level_width, level_height)?;
        if level == 0 {
            base_mip_bytes = rgba_len;
        }
        decoded_mip_bytes =
            decoded_mip_bytes
                .checked_add(rgba_len)
                .ok_or(FormatError::ResourceLimit {
                    format: FORMAT,
                    resource: "retained decoded texture bytes",
                    requested: usize::MAX,
                    limit: MAX_TEXTURE_DECODE_BYTES,
                })?;
        let encoded_len = encoded_texture_level_size(format, level_width, level_height)?;
        checked_slice(FORMAT, bytes, level_offset, encoded_len)?;
        level_offset = level_offset
            .checked_add(encoded_len)
            .ok_or_else(|| invalid_offset(level_offset, bytes.len()))?;
        if level_width == 1 && level_height == 1 {
            break;
        }
        level_width = (level_width / 2).max(1);
        level_height = (level_height / 2).max(1);
    }

    decoded_mip_bytes
        .checked_add(base_mip_bytes)
        .ok_or(FormatError::ResourceLimit {
            format: FORMAT,
            resource: "retained decoded texture bytes",
            requested: usize::MAX,
            limit: MAX_TEXTURE_DECODE_BYTES,
        })
}

pub(super) fn decode_timg(bytes: &[u8], header_offset: usize) -> Result<J3dTexturePreview> {
    let retained_bytes = decoded_timg_retained_bytes(bytes, header_offset)?;
    if retained_bytes > MAX_TEXTURE_DECODE_BYTES {
        return Err(FormatError::ResourceLimit {
            format: FORMAT,
            resource: "retained decoded texture bytes",
            requested: retained_bytes,
            limit: MAX_TEXTURE_DECODE_BYTES,
        });
    }
    let format = *checked_slice(FORMAT, bytes, header_offset, 1)?
        .first()
        .unwrap_or(&0);
    let width = be_u16(bytes, header_offset + 0x02, FORMAT)?;
    let height = be_u16(bytes, header_offset + 0x04, FORMAT)?;
    let wrap_s = checked_slice(FORMAT, bytes, header_offset + 0x06, 1)?[0];
    let wrap_t = checked_slice(FORMAT, bytes, header_offset + 0x07, 1)?[0];
    let mipmap_enabled = checked_slice(FORMAT, bytes, header_offset + 0x10, 1)?[0] != 0;
    let do_edge_lod = checked_slice(FORMAT, bytes, header_offset + 0x11, 1)?[0] != 0;
    let bias_clamp = checked_slice(FORMAT, bytes, header_offset + 0x12, 1)?[0] != 0;
    let max_anisotropy = checked_slice(FORMAT, bytes, header_offset + 0x13, 1)?[0];
    let min_filter = checked_slice(FORMAT, bytes, header_offset + 0x14, 1)?[0];
    let mag_filter = checked_slice(FORMAT, bytes, header_offset + 0x15, 1)?[0];
    let min_lod = checked_slice(FORMAT, bytes, header_offset + 0x16, 1)?[0] as i8 as f32 / 8.0;
    let max_lod = checked_slice(FORMAT, bytes, header_offset + 0x17, 1)?[0] as i8 as f32 / 8.0;
    let mipmap_count = checked_slice(FORMAT, bytes, header_offset + 0x18, 1)?[0];
    let lod_bias = be_i16(bytes, header_offset + 0x1A, FORMAT)? as f32 / 100.0;
    let palette_format = checked_slice(FORMAT, bytes, header_offset + 0x09, 1)?[0];
    let palette_entry_count = be_u16(bytes, header_offset + 0x0A, FORMAT)? as usize;
    let palette_offset = be_u32(bytes, header_offset + 0x0C, FORMAT)? as usize;
    let palette = if matches!(format, GX_TF_C4 | GX_TF_C8 | GX_TF_C14X2) {
        if palette_entry_count == 0 {
            return Err(FormatError::Unsupported {
                format: FORMAT,
                message: "paletted texture has no palette entries".to_string(),
            });
        }
        let capacity = match format {
            GX_TF_C4 => 16,
            GX_TF_C8 => 256,
            GX_TF_C14X2 => 16_384,
            _ => unreachable!(),
        };
        if palette_entry_count > capacity {
            return Err(FormatError::ResourceLimit {
                format: FORMAT,
                resource: "GX palette entries",
                requested: palette_entry_count,
                limit: capacity,
            });
        }
        if palette_offset == 0 {
            return Err(FormatError::Unsupported {
                format: FORMAT,
                message: "paletted texture has a null palette offset".to_string(),
            });
        }
        let absolute = header_offset
            .checked_add(palette_offset)
            .ok_or_else(|| invalid_offset(header_offset, bytes.len()))?;
        let encoded = checked_slice(FORMAT, bytes, absolute, palette_entry_count * 2)?;
        let mut colors = Vec::with_capacity(palette_entry_count);
        for entry in encoded.chunks_exact(2) {
            colors.push(decode_palette_entry(
                palette_format,
                u16::from_be_bytes([entry[0], entry[1]]),
            )?);
        }
        Some(colors)
    } else {
        None
    };
    if width == 0 || height == 0 {
        return Err(FormatError::Unsupported {
            format: FORMAT,
            message: "texture has zero size".to_string(),
        });
    }
    let image_offset = be_u32(bytes, header_offset + 0x1C, FORMAT)? as usize;
    let relative_image_offset = if image_offset == 0 {
        0x20
    } else {
        image_offset
    };
    let image_offset = header_offset
        .checked_add(relative_image_offset)
        .ok_or_else(|| invalid_offset(header_offset, bytes.len()))?;
    let mut mips = Vec::new();
    let mut level_offset = image_offset;
    let mut level_width = width;
    let mut level_height = height;
    let mut total_decoded_bytes = 0usize;
    for _level in 0..mipmap_count.max(1) {
        let rgba_len = decoded_texture_level_size(level_width, level_height)?;
        total_decoded_bytes =
            total_decoded_bytes
                .checked_add(rgba_len)
                .ok_or(FormatError::ResourceLimit {
                    format: FORMAT,
                    resource: "decoded texture bytes",
                    requested: usize::MAX,
                    limit: MAX_TEXTURE_DECODE_BYTES,
                })?;
        if total_decoded_bytes > MAX_TEXTURE_DECODE_BYTES {
            return Err(FormatError::ResourceLimit {
                format: FORMAT,
                resource: "decoded texture bytes",
                requested: total_decoded_bytes,
                limit: MAX_TEXTURE_DECODE_BYTES,
            });
        }
        let level_size = encoded_texture_level_size(format, level_width, level_height)?;
        checked_slice(FORMAT, bytes, level_offset, level_size)?;
        let mut rgba = Vec::new();
        rgba.try_reserve_exact(rgba_len)
            .map_err(|error| FormatError::Unsupported {
                format: FORMAT,
                message: format!("could not reserve {rgba_len} decoded texture bytes: {error}"),
            })?;
        rgba.resize(rgba_len, 0);
        decode_texture_level(
            bytes,
            level_offset,
            format,
            level_width,
            level_height,
            palette.as_deref(),
            &mut rgba,
        )?;
        mips.push(J3dTextureMipPreview {
            width: level_width,
            height: level_height,
            rgba,
        });

        level_offset = level_offset
            .checked_add(level_size)
            .ok_or_else(|| invalid_offset(level_offset, bytes.len()))?;
        if level_width == 1 && level_height == 1 {
            break;
        }
        level_width = (level_width / 2).max(1);
        level_height = (level_height / 2).max(1);
    }
    let base_mip = mips.first().ok_or_else(|| FormatError::Unsupported {
        format: FORMAT,
        message: "texture did not contain a decodable image level".to_string(),
    })?;
    let actual_retained_bytes =
        total_decoded_bytes
            .checked_add(base_mip.rgba.len())
            .ok_or(FormatError::ResourceLimit {
                format: FORMAT,
                resource: "retained decoded texture bytes",
                requested: usize::MAX,
                limit: MAX_TEXTURE_DECODE_BYTES,
            })?;
    if actual_retained_bytes > MAX_TEXTURE_DECODE_BYTES {
        return Err(FormatError::ResourceLimit {
            format: FORMAT,
            resource: "retained decoded texture bytes",
            requested: actual_retained_bytes,
            limit: MAX_TEXTURE_DECODE_BYTES,
        });
    }
    debug_assert_eq!(actual_retained_bytes, retained_bytes);
    let rgba = base_mip.rgba.clone();

    Ok(J3dTexturePreview {
        name: String::new(),
        width,
        height,
        format,
        wrap_s,
        wrap_t,
        min_filter,
        mag_filter,
        mipmap_enabled,
        do_edge_lod,
        bias_clamp,
        max_anisotropy,
        min_lod,
        max_lod,
        lod_bias,
        mipmap_count,
        rgba,
        mips,
    })
}

pub(super) fn decode_texture_level(
    bytes: &[u8],
    offset: usize,
    format: u8,
    width: u16,
    height: u16,
    palette: Option<&[[u8; 4]]>,
    rgba: &mut [u8],
) -> Result<()> {
    match format {
        GX_TF_I4 => decode_i4(bytes, offset, width, height, rgba),
        GX_TF_I8 => decode_i8(bytes, offset, width, height, rgba),
        GX_TF_IA4 => decode_ia4(bytes, offset, width, height, rgba),
        GX_TF_IA8 => decode_ia8(bytes, offset, width, height, rgba),
        GX_TF_RGB565 => decode_rgb565(bytes, offset, width, height, rgba),
        GX_TF_RGB5A3 => decode_rgb5a3(bytes, offset, width, height, rgba),
        GX_TF_RGBA8 => decode_rgba8(bytes, offset, width, height, rgba),
        GX_TF_C4 => decode_c4(
            bytes,
            offset,
            width,
            height,
            required_palette(palette)?,
            rgba,
        ),
        GX_TF_C8 => decode_c8(
            bytes,
            offset,
            width,
            height,
            required_palette(palette)?,
            rgba,
        ),
        GX_TF_C14X2 => decode_c14x2(
            bytes,
            offset,
            width,
            height,
            required_palette(palette)?,
            rgba,
        ),
        GX_TF_CMPR => decode_cmpr(bytes, offset, width, height, rgba),
        _ => Err(FormatError::Unsupported {
            format: FORMAT,
            message: format!("unsupported texture format {format}"),
        }),
    }
}

pub(super) fn encoded_texture_level_size(format: u8, width: u16, height: u16) -> Result<usize> {
    let (tile_width, tile_height, tile_bytes) = match format {
        GX_TF_I4 | GX_TF_C4 | GX_TF_CMPR => (8usize, 8usize, 32usize),
        GX_TF_I8 | GX_TF_IA4 | GX_TF_C8 => (8usize, 4usize, 32usize),
        GX_TF_IA8 | GX_TF_RGB565 | GX_TF_RGB5A3 | GX_TF_C14X2 => (4usize, 4usize, 32usize),
        GX_TF_RGBA8 => (4usize, 4usize, 64usize),
        _ => {
            return Err(FormatError::Unsupported {
                format: FORMAT,
                message: format!("unsupported texture format {format}"),
            });
        }
    };
    let tiles_x = (width as usize).div_ceil(tile_width);
    let tiles_y = (height as usize).div_ceil(tile_height);
    tiles_x
        .checked_mul(tiles_y)
        .and_then(|tiles| tiles.checked_mul(tile_bytes))
        .ok_or(FormatError::ResourceLimit {
            format: FORMAT,
            resource: "encoded texture bytes",
            requested: usize::MAX,
            limit: bytes_addressable_limit(),
        })
}

fn decoded_texture_level_size(width: u16, height: u16) -> Result<usize> {
    let requested = (width as usize)
        .checked_mul(height as usize)
        .and_then(|pixels| pixels.checked_mul(4))
        .ok_or(FormatError::ResourceLimit {
            format: FORMAT,
            resource: "decoded texture bytes",
            requested: usize::MAX,
            limit: MAX_TEXTURE_DECODE_BYTES,
        })?;
    if requested > MAX_TEXTURE_DECODE_BYTES {
        return Err(FormatError::ResourceLimit {
            format: FORMAT,
            resource: "decoded texture bytes",
            requested,
            limit: MAX_TEXTURE_DECODE_BYTES,
        });
    }
    Ok(requested)
}

const fn bytes_addressable_limit() -> usize {
    isize::MAX as usize
}

pub(super) fn decode_i4(
    bytes: &[u8],
    offset: usize,
    width: u16,
    height: u16,
    rgba: &mut [u8],
) -> Result<()> {
    decode_tiled(
        bytes,
        offset,
        width,
        height,
        8,
        8,
        32,
        |tile, x, y| {
            let pixel = y * 8 + x;
            let packed = tile[pixel / 2];
            let value = if pixel % 2 == 0 {
                packed >> 4
            } else {
                packed & 0x0F
            } * 17;
            [value, value, value, value]
        },
        rgba,
    )
}

pub(super) fn decode_i8(
    bytes: &[u8],
    offset: usize,
    width: u16,
    height: u16,
    rgba: &mut [u8],
) -> Result<()> {
    decode_tiled(
        bytes,
        offset,
        width,
        height,
        8,
        4,
        32,
        |tile, x, y| {
            let value = tile[y * 8 + x];
            [value, value, value, value]
        },
        rgba,
    )
}

pub(super) fn decode_ia4(
    bytes: &[u8],
    offset: usize,
    width: u16,
    height: u16,
    rgba: &mut [u8],
) -> Result<()> {
    decode_tiled(
        bytes,
        offset,
        width,
        height,
        8,
        4,
        32,
        |tile, x, y| {
            let packed = tile[y * 8 + x];
            let alpha = (packed >> 4) * 17;
            let intensity = (packed & 0x0F) * 17;
            [intensity, intensity, intensity, alpha]
        },
        rgba,
    )
}

pub(super) fn decode_ia8(
    bytes: &[u8],
    offset: usize,
    width: u16,
    height: u16,
    rgba: &mut [u8],
) -> Result<()> {
    decode_tiled(
        bytes,
        offset,
        width,
        height,
        4,
        4,
        32,
        |tile, x, y| {
            let pixel = (y * 4 + x) * 2;
            let alpha = tile[pixel];
            let intensity = tile[pixel + 1];
            [intensity, intensity, intensity, alpha]
        },
        rgba,
    )
}

pub(super) fn decode_rgb565(
    bytes: &[u8],
    offset: usize,
    width: u16,
    height: u16,
    rgba: &mut [u8],
) -> Result<()> {
    decode_tiled(
        bytes,
        offset,
        width,
        height,
        4,
        4,
        32,
        |tile, x, y| {
            let pixel = (y * 4 + x) * 2;
            let value = u16::from_be_bytes([tile[pixel], tile[pixel + 1]]);
            rgb565_to_rgba(value)
        },
        rgba,
    )
}

pub(super) fn decode_rgb5a3(
    bytes: &[u8],
    offset: usize,
    width: u16,
    height: u16,
    rgba: &mut [u8],
) -> Result<()> {
    decode_tiled(
        bytes,
        offset,
        width,
        height,
        4,
        4,
        32,
        |tile, x, y| {
            let pixel = (y * 4 + x) * 2;
            let value = u16::from_be_bytes([tile[pixel], tile[pixel + 1]]);
            rgb5a3_to_rgba(value)
        },
        rgba,
    )
}

pub(super) fn decode_rgba8(
    bytes: &[u8],
    offset: usize,
    width: u16,
    height: u16,
    rgba: &mut [u8],
) -> Result<()> {
    decode_tiled(
        bytes,
        offset,
        width,
        height,
        4,
        4,
        64,
        |tile, x, y| {
            let pixel = y * 4 + x;
            let ar = pixel * 2;
            let gb = 32 + pixel * 2;
            [tile[ar + 1], tile[gb], tile[gb + 1], tile[ar]]
        },
        rgba,
    )
}

fn required_palette(palette: Option<&[[u8; 4]]>) -> Result<&[[u8; 4]]> {
    palette.ok_or_else(|| FormatError::Unsupported {
        format: FORMAT,
        message: "paletted texture is missing decoded palette data".to_string(),
    })
}

fn decode_palette_entry(format: u8, value: u16) -> Result<[u8; 4]> {
    match format {
        0 => {
            let intensity = value as u8;
            Ok([intensity, intensity, intensity, (value >> 8) as u8])
        }
        1 => Ok(rgb565_to_rgba(value)),
        2 => Ok(rgb5a3_to_rgba(value)),
        _ => Err(FormatError::Unsupported {
            format: FORMAT,
            message: format!("unsupported GX palette format {format}"),
        }),
    }
}

fn palette_color(palette: &[[u8; 4]], index: usize) -> [u8; 4] {
    palette.get(index).copied().unwrap_or([0, 0, 0, 0])
}

pub(super) fn decode_c4(
    bytes: &[u8],
    offset: usize,
    width: u16,
    height: u16,
    palette: &[[u8; 4]],
    rgba: &mut [u8],
) -> Result<()> {
    decode_tiled(
        bytes,
        offset,
        width,
        height,
        8,
        8,
        32,
        |tile, x, y| {
            let pixel = y * 8 + x;
            let packed = tile[pixel / 2];
            let index = if pixel.is_multiple_of(2) {
                packed >> 4
            } else {
                packed & 0x0f
            };
            palette_color(palette, index as usize)
        },
        rgba,
    )
}

pub(super) fn decode_c8(
    bytes: &[u8],
    offset: usize,
    width: u16,
    height: u16,
    palette: &[[u8; 4]],
    rgba: &mut [u8],
) -> Result<()> {
    decode_tiled(
        bytes,
        offset,
        width,
        height,
        8,
        4,
        32,
        |tile, x, y| palette_color(palette, tile[y * 8 + x] as usize),
        rgba,
    )
}

pub(super) fn decode_c14x2(
    bytes: &[u8],
    offset: usize,
    width: u16,
    height: u16,
    palette: &[[u8; 4]],
    rgba: &mut [u8],
) -> Result<()> {
    decode_tiled(
        bytes,
        offset,
        width,
        height,
        4,
        4,
        32,
        |tile, x, y| {
            let pixel = (y * 4 + x) * 2;
            let index = u16::from_be_bytes([tile[pixel], tile[pixel + 1]]) & 0x3fff;
            palette_color(palette, index as usize)
        },
        rgba,
    )
}

pub(super) fn decode_cmpr(
    bytes: &[u8],
    offset: usize,
    width: u16,
    height: u16,
    rgba: &mut [u8],
) -> Result<()> {
    let tile_width = 8usize;
    let tile_height = 8usize;
    let tiles_x = (width as usize).div_ceil(tile_width);
    let tiles_y = (height as usize).div_ceil(tile_height);
    let total = tiles_x
        .checked_mul(tiles_y)
        .and_then(|tiles| tiles.checked_mul(32))
        .ok_or(FormatError::ResourceLimit {
            format: FORMAT,
            resource: "encoded CMPR bytes",
            requested: usize::MAX,
            limit: bytes_addressable_limit(),
        })?;
    checked_slice(FORMAT, bytes, offset, total)?;

    for tile_y in 0..tiles_y {
        for tile_x in 0..tiles_x {
            let tile_base = offset + (tile_y * tiles_x + tile_x) * 32;
            for sub in 0..4 {
                let sub_x = sub % 2;
                let sub_y = sub / 2;
                let block = checked_slice(FORMAT, bytes, tile_base + sub * 8, 8)?;
                let colors = cmpr_palette(
                    u16::from_be_bytes([block[0], block[1]]),
                    u16::from_be_bytes([block[2], block[3]]),
                );
                let bits = u32::from_be_bytes([block[4], block[5], block[6], block[7]]);
                for y in 0..4 {
                    for x in 0..4 {
                        let dst_x = tile_x * 8 + sub_x * 4 + x;
                        let dst_y = tile_y * 8 + sub_y * 4 + y;
                        if dst_x >= width as usize || dst_y >= height as usize {
                            continue;
                        }
                        let shift = 30 - ((y * 4 + x) * 2);
                        let color_index = ((bits >> shift) & 0x03) as usize;
                        write_rgba(rgba, width, dst_x, dst_y, colors[color_index]);
                    }
                }
            }
        }
    }

    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub(super) fn decode_tiled(
    bytes: &[u8],
    offset: usize,
    width: u16,
    height: u16,
    tile_width: usize,
    tile_height: usize,
    tile_bytes: usize,
    pixel: impl Fn(&[u8], usize, usize) -> [u8; 4],
    rgba: &mut [u8],
) -> Result<()> {
    let tiles_x = (width as usize).div_ceil(tile_width);
    let tiles_y = (height as usize).div_ceil(tile_height);
    let total = tiles_x
        .checked_mul(tiles_y)
        .and_then(|tiles| tiles.checked_mul(tile_bytes))
        .ok_or(FormatError::ResourceLimit {
            format: FORMAT,
            resource: "encoded tiled texture bytes",
            requested: usize::MAX,
            limit: bytes_addressable_limit(),
        })?;
    checked_slice(FORMAT, bytes, offset, total)?;
    for tile_y in 0..tiles_y {
        for tile_x in 0..tiles_x {
            let tile = checked_slice(
                FORMAT,
                bytes,
                offset + (tile_y * tiles_x + tile_x) * tile_bytes,
                tile_bytes,
            )?;
            for y in 0..tile_height {
                for x in 0..tile_width {
                    let dst_x = tile_x * tile_width + x;
                    let dst_y = tile_y * tile_height + y;
                    if dst_x >= width as usize || dst_y >= height as usize {
                        continue;
                    }
                    write_rgba(rgba, width, dst_x, dst_y, pixel(tile, x, y));
                }
            }
        }
    }
    Ok(())
}

pub(super) fn write_rgba(rgba: &mut [u8], width: u16, x: usize, y: usize, color: [u8; 4]) {
    let offset = (y * width as usize + x) * 4;
    rgba[offset..offset + 4].copy_from_slice(&color);
}

pub(super) fn rgb565_to_rgba(value: u16) -> [u8; 4] {
    [
        expand_bits((value >> 11) & 0x1F, 5),
        expand_bits((value >> 5) & 0x3F, 6),
        expand_bits(value & 0x1F, 5),
        255,
    ]
}

pub(super) fn rgb5a3_to_rgba(value: u16) -> [u8; 4] {
    if value & 0x8000 != 0 {
        [
            expand_bits((value >> 10) & 0x1F, 5),
            expand_bits((value >> 5) & 0x1F, 5),
            expand_bits(value & 0x1F, 5),
            255,
        ]
    } else {
        [
            expand_bits((value >> 8) & 0x0F, 4),
            expand_bits((value >> 4) & 0x0F, 4),
            expand_bits(value & 0x0F, 4),
            expand_bits((value >> 12) & 0x07, 3),
        ]
    }
}

pub(super) fn cmpr_palette(color0: u16, color1: u16) -> [[u8; 4]; 4] {
    let c0 = rgb565_to_rgba(color0);
    let c1 = rgb565_to_rgba(color1);
    let mut colors = [c0, c1, [0, 0, 0, 255], [0, 0, 0, 0]];
    if color0 > color1 {
        colors[2] = [
            ((2 * c0[0] as u16 + c1[0] as u16) / 3) as u8,
            ((2 * c0[1] as u16 + c1[1] as u16) / 3) as u8,
            ((2 * c0[2] as u16 + c1[2] as u16) / 3) as u8,
            255,
        ];
        colors[3] = [
            ((c0[0] as u16 + 2 * c1[0] as u16) / 3) as u8,
            ((c0[1] as u16 + 2 * c1[1] as u16) / 3) as u8,
            ((c0[2] as u16 + 2 * c1[2] as u16) / 3) as u8,
            255,
        ];
    } else {
        colors[2] = [
            ((c0[0] as u16 + c1[0] as u16) / 2) as u8,
            ((c0[1] as u16 + c1[1] as u16) / 2) as u8,
            ((c0[2] as u16 + c1[2] as u16) / 2) as u8,
            255,
        ];
    }
    colors
}

pub(super) fn expand_bits(value: u16, bits: u8) -> u8 {
    ((value * 255) / ((1u16 << bits) - 1)) as u8
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_unreasonable_texture_dimensions_before_allocating() {
        let mut bytes = vec![0; 0x20];
        bytes[0x02..0x04].copy_from_slice(&u16::MAX.to_be_bytes());
        bytes[0x04..0x06].copy_from_slice(&u16::MAX.to_be_bytes());

        assert!(matches!(
            decode_timg(&bytes, 0),
            Err(FormatError::ResourceLimit { .. })
        ));
    }

    #[test]
    fn truncated_non_base_mip_is_a_structural_error() {
        let mut bytes = vec![0; 0x40];
        bytes[0x02..0x04].copy_from_slice(&8u16.to_be_bytes());
        bytes[0x04..0x06].copy_from_slice(&8u16.to_be_bytes());
        bytes[0x10] = 1;
        bytes[0x18] = 2;

        assert!(matches!(
            decode_timg(&bytes, 0),
            Err(FormatError::InvalidOffset { .. })
        ));
    }
}
