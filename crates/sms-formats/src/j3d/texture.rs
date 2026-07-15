use super::*;

pub(super) fn decode_timg(bytes: &[u8], header_offset: usize) -> Result<J3dTexturePreview> {
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
    if width == 0 || height == 0 {
        return Err(FormatError::Unsupported {
            format: FORMAT,
            message: "texture has zero size".to_string(),
        });
    }
    let image_offset = be_u32(bytes, header_offset + 0x1C, FORMAT)? as usize;
    let image_offset = header_offset
        + if image_offset == 0 {
            0x20
        } else {
            image_offset
        };
    let mut mips = Vec::new();
    let mut level_offset = image_offset;
    let mut level_width = width;
    let mut level_height = height;
    for level in 0..mipmap_count.max(1) {
        let mut rgba = vec![0; level_width as usize * level_height as usize * 4];
        let decoded = decode_texture_level(
            bytes,
            level_offset,
            format,
            level_width,
            level_height,
            &mut rgba,
        );
        if let Err(err) = decoded {
            if level == 0 {
                return Err(err);
            }
            break;
        }
        mips.push(J3dTextureMipPreview {
            width: level_width,
            height: level_height,
            rgba,
        });

        let level_size = encoded_texture_level_size(format, level_width, level_height)?;
        level_offset = level_offset
            .checked_add(level_size)
            .ok_or_else(|| invalid_offset(level_offset, bytes.len()))?;
        if level_width == 1 && level_height == 1 {
            break;
        }
        level_width = (level_width / 2).max(1);
        level_height = (level_height / 2).max(1);
    }
    let rgba = mips
        .first()
        .map(|mip| mip.rgba.clone())
        .unwrap_or_else(|| vec![255, 255, 255, 255]);

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
        GX_TF_CMPR => decode_cmpr(bytes, offset, width, height, rgba),
        _ => Err(FormatError::Unsupported {
            format: FORMAT,
            message: format!("unsupported texture format {format}"),
        }),
    }
}

pub(super) fn encoded_texture_level_size(format: u8, width: u16, height: u16) -> Result<usize> {
    let (tile_width, tile_height, tile_bytes) = match format {
        GX_TF_I4 | GX_TF_CMPR => (8usize, 8usize, 32usize),
        GX_TF_I8 | GX_TF_IA4 => (8usize, 4usize, 32usize),
        GX_TF_IA8 | GX_TF_RGB565 | GX_TF_RGB5A3 => (4usize, 4usize, 32usize),
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
    Ok(tiles_x * tiles_y * tile_bytes)
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
    let total = tiles_x * tiles_y * 32;
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
    checked_slice(FORMAT, bytes, offset, tiles_x * tiles_y * tile_bytes)?;
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
