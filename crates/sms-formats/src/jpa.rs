use crate::{decode_bti_texture, FormatError, J3dTexturePreview, PreserveBytes, Result};

const FORMAT: &str = "JPA1 particle effect";

#[derive(Debug, Clone, PartialEq)]
pub struct JpaEffect {
    pub emitter: JpaEmitter,
    pub base_shape: JpaBaseShape,
    pub extra_shape: Option<JpaExtraShape>,
    pub child_shape: Option<JpaChildShape>,
    pub fields: Vec<JpaField>,
    pub keyframes: Vec<JpaKeyframeCurve>,
    pub color_animation: Option<JpaColorAnimation>,
    pub textures: Vec<J3dTexturePreview>,
    pub uses_screen_texture: bool,
    pub indirect_texture_index: Option<u8>,
    /// Blocks that this parser does not understand yet, retained verbatim.
    pub unknown_blocks: Vec<JpaRawBlock>,
    bytes: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JpaRawBlock {
    pub tag: [u8; 4],
    pub offset: usize,
    pub bytes: Vec<u8>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct JpaEmitter {
    pub scale: [f32; 3],
    pub translation: [f32; 3],
    pub volume_type: u8,
    pub emit_interval: u8,
    pub spawn_rate: f32,
    pub spawn_rate_variance: f32,
    pub max_frame: i16,
    pub start_frame: i16,
    pub volume_size: u16,
    pub volume_yaw_sweep: f32,
    pub volume_min_radius: f32,
    pub base_lifetime: u16,
    pub lifetime_random_scale: f32,
    pub base_weight: f32,
    pub weight_random_scale: f32,
    pub initial_velocity_random_scale: f32,
    pub base_air_resistance: f32,
    pub air_resistance_variance: f32,
    pub initial_velocity: [f32; 4],
    pub direction: [f32; 3],
    pub direction_spread: f32,
    pub flags: u32,
    pub keyframe_mask: u32,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct JpaBaseShape {
    pub flags: u8,
    pub size: [f32; 2],
    pub particle_type: u8,
    pub direction_type: u8,
    pub rotation_type: u8,
    pub tiling: [f32; 2],
    pub texture_index: u8,
    pub color_mode: u8,
    pub color: [u8; 4],
    pub environment_color: [u8; 4],
    pub blend_mode: u8,
    pub source_blend_factor: u8,
    pub destination_blend_factor: u8,
    pub alpha_compare: [u8; 5],
    pub z_compare_enable: bool,
    pub z_compare_function: u8,
    pub z_update_enable: bool,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct JpaExtraShape {
    pub alpha_enabled: bool,
    pub alpha_in_timing: f32,
    pub alpha_out_timing: f32,
    pub alpha_in_value: f32,
    pub alpha_base_value: f32,
    pub alpha_out_value: f32,
    pub scale_enabled: bool,
    pub scale_flags: u8,
    /// Authored ESP1 sprite pivot, where 1 is centered and 0/2 select an edge.
    pub scale_pivot: [u8; 2],
    pub random_scale: f32,
    pub scale_cycle: [i16; 2],
    pub scale_cycle_reverse: [bool; 2],
    pub scale_in_timing: f32,
    pub scale_out_timing: f32,
    pub scale_in_value: [f32; 2],
    pub scale_out_value: [f32; 2],
    pub rotate_enabled: bool,
    pub rotate_angle: f32,
    pub rotate_speed: f32,
    pub rotate_random_speed: f32,
    pub rotate_random_angle: f32,
    pub rotate_direction: f32,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct JpaChildShape {
    pub particle_type: u8,
    pub direction_type: u8,
    pub rotation_type: u8,
    pub lifetime: i16,
    pub spawn_count: i16,
    pub spawn_timing: f32,
    pub spawn_step: u8,
    pub position_random: f32,
    pub velocity: f32,
    pub inherit_velocity: f32,
    pub velocity_random: f32,
    pub inherit_scale: f32,
    pub inherit_alpha: f32,
    pub inherit_rgb: f32,
    pub size: [f32; 2],
    pub rotate_speed: f32,
    pub rotate_enabled: bool,
    pub children_affected_by_fields: bool,
    pub scale_out_enabled: bool,
    pub alpha_out_enabled: bool,
    pub inherit_flags: u8,
    pub draw_parent: bool,
    pub texture_index: u8,
    pub color: [u8; 4],
    pub environment_color: [u8; 4],
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct JpaField {
    pub kind: u8,
    pub add_type: u8,
    pub cycle: u8,
    pub status: u16,
    pub magnitude: f32,
    pub secondary_magnitude: f32,
    pub max_distance: f32,
    pub position: [f32; 3],
    pub direction: [f32; 3],
    pub parameter: [f32; 3],
    pub fade: [f32; 4],
}

#[derive(Debug, Clone, PartialEq)]
pub struct JpaKeyframeCurve {
    pub parameter_index: u8,
    pub looping: bool,
    pub keys: Vec<[f32; 4]>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct JpaColorAnimation {
    pub max_frame: i16,
    pub mode: u8,
    pub global: bool,
    pub random_offset: bool,
    pub primary: Vec<JpaColorKey>,
    pub environment: Vec<JpaColorKey>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct JpaColorKey {
    pub frame: i16,
    pub color: [u8; 4],
}

impl JpaEffect {
    pub fn parse(bytes: impl AsRef<[u8]>) -> Result<Self> {
        let bytes = bytes.as_ref();
        if bytes.len() < 0x20 {
            return Err(FormatError::TooSmall {
                format: FORMAT,
                expected: 0x20,
                actual: bytes.len(),
            });
        }
        if &bytes[..8] != b"JEFFjpa1" {
            return Err(FormatError::BadMagic {
                format: FORMAT,
                expected: b"JEFFjpa1",
                actual: bytes[..8].to_vec(),
            });
        }
        let declared_size = be_u32(bytes, 8)? as usize;
        if declared_size > bytes.len() {
            return Err(invalid(0, declared_size));
        }
        let block_count = be_u32(bytes, 0x0c)? as usize;
        let mut emitter = None;
        let mut base_shape = None;
        let mut extra_shape = None;
        let mut child_shape = None;
        let mut fields = Vec::new();
        let mut raw_curves = Vec::new();
        let mut color_animation = None;
        let mut textures = Vec::new();
        let mut uses_screen_texture = false;
        let mut indirect_texture_index = None;
        let mut unknown_blocks = Vec::new();
        let mut offset = 0x20usize;

        for _ in 0..block_count {
            let header = bytes
                .get(offset..offset + 8)
                .ok_or_else(|| invalid(offset, 8))?;
            let size = u32::from_be_bytes(header[4..8].try_into().unwrap()) as usize;
            if size < 8
                || offset
                    .checked_add(size)
                    .is_none_or(|end| end > declared_size)
            {
                return Err(invalid(offset, size));
            }
            let block = &bytes[offset..offset + size];
            match &header[..4] {
                b"BEM1" => emitter = Some(parse_emitter(block)?),
                b"BSP1" => {
                    base_shape = Some(parse_base_shape(block)?);
                    color_animation = parse_color_animation(block)?;
                }
                b"ESP1" => extra_shape = Some(parse_extra_shape(block)?),
                b"SSP1" => child_shape = Some(parse_child_shape(block)?),
                b"FLD1" => fields.push(parse_field(block)?),
                b"KFA1" => raw_curves.push(parse_keyframes(block)?),
                b"ETX1" => {
                    require(block, 0x20)?;
                    uses_screen_texture = true;
                    indirect_texture_index = Some(block[0x1f]);
                }
                b"TEX1" => {
                    let mut texture = decode_bti_texture(
                        block.get(0x20..).ok_or_else(|| invalid(offset + 0x20, 1))?,
                    )?;
                    texture.name = block
                        .get(0x0c..0x20)
                        .map(|name| {
                            String::from_utf8_lossy(name)
                                .trim_end_matches('\0')
                                .to_string()
                        })
                        .unwrap_or_default();
                    textures.push(texture);
                }
                _ => unknown_blocks.push(JpaRawBlock {
                    tag: header[..4].try_into().unwrap(),
                    offset,
                    bytes: block.to_vec(),
                }),
            }
            offset += size;
        }

        let emitter = emitter.ok_or_else(|| FormatError::Unsupported {
            format: FORMAT,
            message: "missing BEM1 emitter block".to_string(),
        })?;
        let base_shape = base_shape.ok_or_else(|| FormatError::Unsupported {
            format: FORMAT,
            message: "missing BSP1 base-shape block".to_string(),
        })?;
        let mut remaining_mask = emitter.keyframe_mask;
        for curve in &mut raw_curves {
            if remaining_mask == 0 {
                return Err(FormatError::Unsupported {
                    format: FORMAT,
                    message: "more KFA1 curves than enabled BEM1 keyframe-mask parameters"
                        .to_string(),
                });
            }
            curve.parameter_index = remaining_mask.trailing_zeros() as u8;
            remaining_mask &= remaining_mask - 1;
        }

        Ok(Self {
            emitter,
            base_shape,
            extra_shape,
            child_shape,
            fields,
            keyframes: raw_curves,
            color_animation,
            textures,
            uses_screen_texture,
            indirect_texture_index,
            unknown_blocks,
            bytes: bytes.to_vec(),
        })
    }
}

impl PreserveBytes for JpaEffect {
    fn source_bytes(&self) -> &[u8] {
        &self.bytes
    }
}

impl JpaKeyframeCurve {
    pub fn sample(&self, frame: f32) -> Option<f32> {
        let first = *self.keys.first()?;
        let last = *self.keys.last()?;
        let frame = if self.looping && last[0] >= 0.0 {
            frame % (last[0] + 1.0)
        } else {
            frame
        };
        if frame <= first[0] {
            return Some(first[1]);
        }
        if frame >= last[0] {
            return Some(last[1]);
        }
        let pair = self
            .keys
            .windows(2)
            .find(|pair| frame >= pair[0][0] && frame < pair[1][0])?;
        let [x0, y0, _in0, out0] = pair[0];
        let [x1, y1, in1, _out1] = pair[1];
        let span = x1 - x0;
        let t = (frame - x0) / span;
        let t2 = t * t;
        let t3 = t2 * t;
        Some(
            (2.0 * t3 - 3.0 * t2 + 1.0) * y0
                + (t3 - 2.0 * t2 + t) * span * out0
                + (-2.0 * t3 + 3.0 * t2) * y1
                + (t3 - t2) * span * in1,
        )
    }
}

fn parse_emitter(block: &[u8]) -> Result<JpaEmitter> {
    require(block, 0x90)?;
    Ok(JpaEmitter {
        scale: vec3(block, 0x0c)?,
        translation: vec3(block, 0x18)?,
        volume_type: block[0x2a],
        emit_interval: block[0x2b],
        spawn_rate: be_f32(block, 0x30)?,
        spawn_rate_variance: fixed(block, 0x34)?,
        max_frame: be_i16(block, 0x36)?,
        start_frame: be_i16(block, 0x38)?,
        volume_size: be_u16(block, 0x3a)?,
        volume_yaw_sweep: fixed(block, 0x3c)?,
        volume_min_radius: fixed(block, 0x3e)?,
        base_lifetime: be_u16(block, 0x40)?,
        lifetime_random_scale: fixed(block, 0x42)?,
        base_weight: fixed(block, 0x44)?,
        weight_random_scale: fixed(block, 0x46)?,
        initial_velocity_random_scale: fixed(block, 0x48)?,
        base_air_resistance: fixed(block, 0x4c)?,
        air_resistance_variance: fixed(block, 0x4e)?,
        initial_velocity: [
            be_f32(block, 0x50)?,
            be_f32(block, 0x54)?,
            be_f32(block, 0x58)?,
            be_f32(block, 0x5c)?,
        ],
        direction: [
            fixed(block, 0x64)?,
            fixed(block, 0x66)?,
            fixed(block, 0x68)?,
        ],
        direction_spread: fixed(block, 0x6a)?,
        flags: be_u32(block, 0x6c)?,
        keyframe_mask: be_u32(block, 0x70)?,
    })
}

fn parse_base_shape(block: &[u8]) -> Result<JpaBaseShape> {
    require(block, 0x98)?;
    const BLEND_FACTORS: [u8; 10] = [0, 1, 2, 3, 2, 3, 4, 5, 6, 7];
    const COMPARES: [u8; 8] = [0, 1, 3, 2, 5, 6, 4, 7];
    Ok(JpaBaseShape {
        flags: block[0x44],
        size: [be_f32(block, 0x1c)?, be_f32(block, 0x18)?],
        particle_type: block[0x24],
        direction_type: block[0x25],
        rotation_type: block[0x26],
        tiling: [fixed(block, 0x88)? * 10.0, fixed(block, 0x8a)? * 10.0],
        texture_index: block[0x4f],
        color_mode: block[0x30],
        color: block[0x64..0x68].try_into().unwrap(),
        environment_color: block[0x68..0x6c].try_into().unwrap(),
        blend_mode: [0, 1, 2, 0].get(block[0x35] as usize).copied().unwrap_or(1),
        source_blend_factor: BLEND_FACTORS
            .get(block[0x36] as usize)
            .copied()
            .unwrap_or(4),
        destination_blend_factor: BLEND_FACTORS
            .get(block[0x37] as usize)
            .copied()
            .unwrap_or(5),
        alpha_compare: [
            COMPARES.get(block[0x39] as usize).copied().unwrap_or(7),
            block[0x3a],
            block[0x3b],
            COMPARES.get(block[0x3c] as usize).copied().unwrap_or(7),
            block[0x3d],
        ],
        z_compare_enable: block[0x3f] != 0,
        z_compare_function: COMPARES.get(block[0x40] as usize).copied().unwrap_or(3),
        z_update_enable: block[0x41] != 0,
    })
}

fn parse_extra_shape(block: &[u8]) -> Result<JpaExtraShape> {
    require(block, 0x65)?;
    Ok(JpaExtraShape {
        alpha_enabled: block[0x1e] & 1 != 0,
        alpha_in_timing: fixed(block, 0x14)?,
        alpha_out_timing: fixed(block, 0x16)?,
        alpha_in_value: fixed(block, 0x18)?,
        alpha_base_value: fixed(block, 0x1a)?,
        alpha_out_value: fixed(block, 0x1c)?,
        scale_enabled: block[0x4e] & 1 != 0,
        scale_flags: block[0x4e],
        scale_pivot: [block[0x4a], block[0x40]],
        random_scale: fixed(block, 0x34)?,
        scale_cycle: [be_i16(block, 0x4c)?, be_i16(block, 0x42)?],
        scale_cycle_reverse: [block[0x4b] != 0, block[0x41] != 0],
        scale_in_timing: fixed(block, 0x36)?,
        scale_out_timing: fixed(block, 0x38)?,
        scale_in_value: [fixed(block, 0x44)? * 10.0, fixed(block, 0x3a)? * 10.0],
        scale_out_value: [fixed(block, 0x48)? * 10.0, fixed(block, 0x3e)? * 10.0],
        rotate_enabled: block[0x64] != 0,
        rotate_angle: fixed(block, 0x5a)?,
        rotate_speed: fixed(block, 0x5c)?,
        rotate_random_speed: fixed(block, 0x5e)?,
        rotate_random_angle: fixed(block, 0x60)?,
        rotate_direction: fixed(block, 0x62)?,
    })
}

fn parse_color_animation(block: &[u8]) -> Result<Option<JpaColorAnimation>> {
    require(block, 0x64)?;
    let primary_count = block[0x62] as usize;
    let environment_count = block[0x63] as usize;
    if primary_count == 0 && environment_count == 0 {
        return Ok(None);
    }
    let keys = |offset_field: usize, count: usize| -> Result<Vec<JpaColorKey>> {
        if count == 0 {
            return Ok(Vec::new());
        }
        let offset = be_u16(block, offset_field)? as usize;
        require(block, offset.saturating_add(count.saturating_mul(6)))?;
        (0..count)
            .map(|index| {
                let key = offset + index * 6;
                Ok(JpaColorKey {
                    frame: be_i16(block, key)?,
                    color: block[key + 2..key + 6].try_into().unwrap(),
                })
            })
            .collect()
    };
    Ok(Some(JpaColorAnimation {
        max_frame: be_i16(block, 0x5c)?,
        mode: block[0x5e],
        global: block[0x22] & 2 != 0 || matches!(block[0x24], 5 | 6),
        random_offset: block[0x22] & 1 != 0,
        primary: keys(0x14, primary_count)?,
        environment: keys(0x16, environment_count)?,
    }))
}

fn parse_child_shape(block: &[u8]) -> Result<JpaChildShape> {
    require(block, 0x62)?;
    Ok(JpaChildShape {
        particle_type: block[0x10],
        direction_type: block[0x11],
        rotation_type: block[0x12],
        lifetime: be_i16(block, 0x14)?,
        spawn_count: be_i16(block, 0x16)?,
        spawn_timing: fixed(block, 0x18)?,
        spawn_step: block[0x1a],
        position_random: be_f32(block, 0x28)?,
        velocity: be_f32(block, 0x2c)?,
        inherit_velocity: fixed(block, 0x30)?,
        velocity_random: fixed(block, 0x32)?,
        inherit_scale: fixed(block, 0x48)?,
        inherit_alpha: fixed(block, 0x4a)?,
        size: [be_f32(block, 0x50)?, be_f32(block, 0x4c)?],
        rotate_speed: fixed(block, 0x54)?,
        rotate_enabled: block[0x56] != 0,
        children_affected_by_fields: block[0x36] != 0,
        scale_out_enabled: block[0x45] != 0,
        alpha_out_enabled: block[0x46] != 0,
        inherit_flags: block[0x57],
        draw_parent: block[0x44] & 1 != 0,
        texture_index: block[0x47],
        color: block[0x58..0x5c].try_into().unwrap(),
        environment_color: block[0x5c..0x60].try_into().unwrap(),
        inherit_rgb: fixed(block, 0x60)?,
    })
}

fn parse_field(block: &[u8]) -> Result<JpaField> {
    require(block, 0x4c)?;
    Ok(JpaField {
        kind: block[0x0c],
        add_type: block[0x0e],
        cycle: block[0x0f],
        status: be_u16(block, 0x10)?,
        magnitude: be_f32(block, 0x14)?,
        secondary_magnitude: be_f32(block, 0x18)?,
        max_distance: be_f32(block, 0x1c)?,
        position: vec3(block, 0x20)?,
        direction: vec3(block, 0x2c)?,
        parameter: [
            be_f32(block, 0x38)?,
            be_f32(block, 0x3c)?,
            be_f32(block, 0x40)?,
        ],
        fade: [
            fixed(block, 0x44)?,
            fixed(block, 0x46)?,
            fixed(block, 0x48)?,
            fixed(block, 0x4a)?,
        ],
    })
}

fn parse_keyframes(block: &[u8]) -> Result<JpaKeyframeCurve> {
    require(block, 0x20)?;
    let count = block[0x10] as usize;
    let data_len = count
        .checked_mul(16)
        .ok_or_else(|| invalid(0x20, usize::MAX))?;
    require(block, 0x20 + data_len)?;
    let mut keys = Vec::with_capacity(count);
    for index in 0..count {
        let offset = 0x20 + index * 16;
        keys.push([
            be_f32(block, offset)?,
            be_f32(block, offset + 4)?,
            be_f32(block, offset + 8)?,
            be_f32(block, offset + 12)?,
        ]);
    }
    Ok(JpaKeyframeCurve {
        parameter_index: 0,
        looping: block[0x12] != 0,
        keys,
    })
}

fn require(bytes: &[u8], expected: usize) -> Result<()> {
    if bytes.len() < expected {
        Err(FormatError::TooSmall {
            format: FORMAT,
            expected,
            actual: bytes.len(),
        })
    } else {
        Ok(())
    }
}

fn invalid(offset: usize, len: usize) -> FormatError {
    FormatError::InvalidOffset {
        format: FORMAT,
        offset,
        len,
    }
}

fn be_u16(bytes: &[u8], offset: usize) -> Result<u16> {
    let value = bytes
        .get(offset..offset + 2)
        .ok_or_else(|| invalid(offset, 2))?;
    Ok(u16::from_be_bytes(value.try_into().unwrap()))
}

fn be_i16(bytes: &[u8], offset: usize) -> Result<i16> {
    Ok(be_u16(bytes, offset)? as i16)
}

fn be_u32(bytes: &[u8], offset: usize) -> Result<u32> {
    let value = bytes
        .get(offset..offset + 4)
        .ok_or_else(|| invalid(offset, 4))?;
    Ok(u32::from_be_bytes(value.try_into().unwrap()))
}

fn be_f32(bytes: &[u8], offset: usize) -> Result<f32> {
    Ok(f32::from_bits(be_u32(bytes, offset)?))
}

fn fixed(bytes: &[u8], offset: usize) -> Result<f32> {
    Ok(be_i16(bytes, offset)? as f32 / 32768.0)
}

fn vec3(bytes: &[u8], offset: usize) -> Result<[f32; 3]> {
    Ok([
        be_f32(bytes, offset)?,
        be_f32(bytes, offset + 4)?,
        be_f32(bytes, offset + 8)?,
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn effect_with_keyframe_mask(mask: u32) -> Vec<u8> {
        let declared_size = 0x20 + 0x90 + 0x98 + 0x30 + 8;
        let mut bytes = vec![0; declared_size];
        bytes[..8].copy_from_slice(b"JEFFjpa1");
        bytes[8..12].copy_from_slice(&(declared_size as u32).to_be_bytes());
        bytes[12..16].copy_from_slice(&4u32.to_be_bytes());

        let emitter = 0x20;
        bytes[emitter..emitter + 4].copy_from_slice(b"BEM1");
        bytes[emitter + 4..emitter + 8].copy_from_slice(&0x90u32.to_be_bytes());
        bytes[emitter + 0x70..emitter + 0x74].copy_from_slice(&mask.to_be_bytes());

        let base_shape = emitter + 0x90;
        bytes[base_shape..base_shape + 4].copy_from_slice(b"BSP1");
        bytes[base_shape + 4..base_shape + 8].copy_from_slice(&0x98u32.to_be_bytes());

        let keyframes = base_shape + 0x98;
        bytes[keyframes..keyframes + 4].copy_from_slice(b"KFA1");
        bytes[keyframes + 4..keyframes + 8].copy_from_slice(&0x30u32.to_be_bytes());
        bytes[keyframes + 0x10] = 1;
        bytes[keyframes + 0x20..keyframes + 0x24].copy_from_slice(&1.0f32.to_be_bytes());

        let unknown = keyframes + 0x30;
        bytes[unknown..unknown + 4].copy_from_slice(b"NEW1");
        bytes[unknown + 4..unknown + 8].copy_from_slice(&8u32.to_be_bytes());
        bytes
    }

    #[test]
    fn hermite_keyframes_match_endpoints_and_midpoint() {
        let curve = JpaKeyframeCurve {
            parameter_index: 0,
            looping: false,
            keys: vec![[0.0, 1.0, 0.0, 0.0], [10.0, 3.0, 0.0, 0.0]],
        };
        assert_eq!(curve.sample(-1.0), Some(1.0));
        assert_eq!(curve.sample(5.0), Some(2.0));
        assert_eq!(curve.sample(11.0), Some(3.0));
    }

    #[test]
    fn malformed_block_bounds_are_rejected() {
        let mut bytes = vec![0; 0x28];
        bytes[..8].copy_from_slice(b"JEFFjpa1");
        let size = bytes.len() as u32;
        bytes[8..12].copy_from_slice(&size.to_be_bytes());
        bytes[12..16].copy_from_slice(&1u32.to_be_bytes());
        bytes[0x20..0x24].copy_from_slice(b"BEM1");
        bytes[0x24..0x28].copy_from_slice(&0x100u32.to_be_bytes());
        assert!(matches!(
            JpaEffect::parse(&bytes),
            Err(FormatError::InvalidOffset { .. })
        ));
    }

    #[test]
    fn sparse_keyframe_mask_uses_the_set_bit_index_and_preserves_unknown_data() {
        let mut bytes = effect_with_keyframe_mask(0b1000);
        bytes.extend_from_slice(b"trailing bytes");

        let effect = JpaEffect::parse(&bytes).unwrap();
        assert_eq!(effect.keyframes[0].parameter_index, 3);
        assert_eq!(effect.unknown_blocks.len(), 1);
        assert_eq!(effect.unknown_blocks[0].tag, *b"NEW1");
        assert_eq!(effect.unknown_blocks[0].bytes, b"NEW1\0\0\0\x08");
        assert_eq!(effect.to_bytes(), bytes);
    }

    #[test]
    fn keyframe_curve_without_a_mask_bit_is_rejected() {
        assert!(matches!(
            JpaEffect::parse(effect_with_keyframe_mask(0)),
            Err(FormatError::Unsupported { .. })
        ));
    }

    #[test]
    fn every_singleton_keyframe_mask_maps_to_its_exact_parameter_bit() {
        for parameter in 0..u32::BITS {
            let effect = JpaEffect::parse(effect_with_keyframe_mask(1 << parameter)).unwrap();
            assert_eq!(effect.keyframes[0].parameter_index, parameter as u8);
        }
    }

    #[test]
    fn every_truncated_effect_prefix_is_rejected_without_panicking() {
        let bytes = effect_with_keyframe_mask(1);
        for length in 0..bytes.len() {
            assert!(
                JpaEffect::parse(&bytes[..length]).is_err(),
                "truncated prefix of {length} bytes was accepted"
            );
        }
    }

    #[test]
    fn parses_child_particle_shape() {
        let mut block = vec![0; 0x62];
        block[..4].copy_from_slice(b"SSP1");
        let block_size = block.len() as u32;
        block[4..8].copy_from_slice(&block_size.to_be_bytes());
        block[0x14..0x16].copy_from_slice(&90i16.to_be_bytes());
        block[0x10] = 3;
        block[0x11] = 2;
        block[0x12] = 1;
        block[0x16..0x18].copy_from_slice(&2i16.to_be_bytes());
        block[0x18..0x1a].copy_from_slice(&16384i16.to_be_bytes());
        block[0x1a] = 3;
        block[0x36] = 1;
        block[0x44] = 1;
        block[0x45] = 1;
        block[0x46] = 1;
        block[0x47] = 2;
        block[0x4c..0x50].copy_from_slice(&1.5f32.to_be_bytes());
        block[0x50..0x54].copy_from_slice(&2.0f32.to_be_bytes());
        block[0x58..0x5c].copy_from_slice(&[10, 20, 30, 40]);
        block[0x5c..0x60].copy_from_slice(&[50, 60, 70, 80]);

        let child = parse_child_shape(&block).unwrap();
        assert_eq!(child.lifetime, 90);
        assert_eq!(child.particle_type, 3);
        assert_eq!(child.direction_type, 2);
        assert_eq!(child.rotation_type, 1);
        assert_eq!(child.spawn_count, 2);
        assert_eq!(child.spawn_timing, 0.5);
        assert_eq!(child.spawn_step, 3);
        assert!(child.children_affected_by_fields);
        assert!(child.scale_out_enabled);
        assert!(child.alpha_out_enabled);
        assert!(child.draw_parent);
        assert_eq!(child.texture_index, 2);
        assert_eq!(child.size, [2.0, 1.5]);
        assert_eq!(child.color, [10, 20, 30, 40]);
        assert_eq!(child.environment_color, [50, 60, 70, 80]);
    }

    #[test]
    fn parses_base_shape_draw_semantics() {
        let mut block = vec![0; 0x98];
        block[..4].copy_from_slice(b"BSP1");
        let block_size = block.len() as u32;
        block[4..8].copy_from_slice(&block_size.to_be_bytes());
        block[0x18..0x1c].copy_from_slice(&4.0f32.to_be_bytes());
        block[0x1c..0x20].copy_from_slice(&1.5f32.to_be_bytes());
        block[0x24] = 3;
        block[0x25] = 1;
        block[0x26] = 2;
        block[0x30] = 3;
        block[0x64..0x68].copy_from_slice(&[1, 2, 3, 4]);
        block[0x68..0x6c].copy_from_slice(&[5, 6, 7, 8]);
        block[0x88..0x8a].copy_from_slice(&3277i16.to_be_bytes());
        block[0x8a..0x8c].copy_from_slice(&6554i16.to_be_bytes());

        let shape = parse_base_shape(&block).unwrap();
        assert_eq!(shape.size, [1.5, 4.0]);
        assert_eq!(shape.particle_type, 3);
        assert_eq!(shape.direction_type, 1);
        assert_eq!(shape.rotation_type, 2);
        assert_eq!(shape.color_mode, 3);
        assert_eq!(shape.color, [1, 2, 3, 4]);
        assert_eq!(shape.environment_color, [5, 6, 7, 8]);
        assert!((shape.tiling[0] - 1.0).abs() < 0.001);
        assert!((shape.tiling[1] - 2.0).abs() < 0.001);
    }

    #[test]
    fn parses_extra_shape_sprite_pivot() {
        let mut block = vec![0; 0x65];
        block[..4].copy_from_slice(b"ESP1");
        block[0x40] = 2;
        block[0x4a] = 0;

        let shape = parse_extra_shape(&block).unwrap();
        assert_eq!(shape.scale_pivot, [0, 2]);
    }

    #[test]
    fn parses_base_shape_color_animation_keys() {
        let mut block = vec![0; 0x80];
        block[..4].copy_from_slice(b"BSP1");
        let block_size = block.len() as u32;
        block[4..8].copy_from_slice(&block_size.to_be_bytes());
        block[0x14..0x16].copy_from_slice(&0x68u16.to_be_bytes());
        block[0x16..0x18].copy_from_slice(&0x74u16.to_be_bytes());
        block[0x22] = 1;
        block[0x5c..0x5e].copy_from_slice(&15i16.to_be_bytes());
        block[0x5e] = 3;
        block[0x62] = 2;
        block[0x63] = 1;
        block[0x68..0x6a].copy_from_slice(&0i16.to_be_bytes());
        block[0x6a..0x6e].copy_from_slice(&[10, 20, 30, 40]);
        block[0x6e..0x70].copy_from_slice(&15i16.to_be_bytes());
        block[0x70..0x74].copy_from_slice(&[50, 60, 70, 80]);
        block[0x74..0x76].copy_from_slice(&0i16.to_be_bytes());
        block[0x76..0x7a].copy_from_slice(&[90, 100, 110, 120]);

        let animation = parse_color_animation(&block).unwrap().unwrap();
        assert_eq!(animation.max_frame, 15);
        assert_eq!(animation.mode, 3);
        assert!(animation.random_offset);
        assert!(!animation.global);
        assert_eq!(animation.primary[1].color, [50, 60, 70, 80]);
        assert_eq!(animation.environment[0].color, [90, 100, 110, 120]);
    }
}
