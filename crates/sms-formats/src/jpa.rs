use crate::{decode_bti_texture, FormatError, J3dTexturePreview, Result};

const FORMAT: &str = "JPA1 particle effect";

#[derive(Debug, Clone, PartialEq)]
pub struct JpaEffect {
    pub emitter: JpaEmitter,
    pub base_shape: JpaBaseShape,
    pub extra_shape: Option<JpaExtraShape>,
    pub child_shape: Option<JpaChildShape>,
    pub fields: Vec<JpaField>,
    pub keyframes: Vec<JpaKeyframeCurve>,
    pub textures: Vec<J3dTexturePreview>,
    pub uses_screen_texture: bool,
    pub indirect_texture_index: Option<u8>,
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
    pub base_lifetime: u16,
    pub lifetime_random_scale: f32,
    pub base_weight: f32,
    pub weight_random_scale: f32,
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
    pub size: [f32; 2],
    pub particle_type: u8,
    pub tiling: [f32; 2],
    pub texture_index: u8,
    pub color: [u8; 4],
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
    pub inherit_flags: u8,
    pub draw_parent: bool,
    pub texture_index: u8,
    pub color: [u8; 4],
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct JpaField {
    pub kind: u8,
    pub status: u16,
    pub magnitude: f32,
    pub direction: [f32; 3],
}

#[derive(Debug, Clone, PartialEq)]
pub struct JpaKeyframeCurve {
    pub parameter_index: u8,
    pub looping: bool,
    pub keys: Vec<[f32; 4]>,
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
        let mut textures = Vec::new();
        let mut uses_screen_texture = false;
        let mut indirect_texture_index = None;
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
                b"BSP1" => base_shape = Some(parse_base_shape(block)?),
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
                _ => {}
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
        let parameter_indices = emitter.keyframe_mask.trailing_zeros();
        let mut remaining_mask = emitter.keyframe_mask;
        let mut next_parameter = parameter_indices as u8;
        for curve in &mut raw_curves {
            while remaining_mask & 1 == 0 && remaining_mask != 0 {
                remaining_mask >>= 1;
                next_parameter = next_parameter.saturating_add(1);
            }
            curve.parameter_index = next_parameter;
            if remaining_mask != 0 {
                remaining_mask >>= 1;
                next_parameter = next_parameter.saturating_add(1);
            }
        }

        Ok(Self {
            emitter,
            base_shape,
            extra_shape,
            child_shape,
            fields,
            keyframes: raw_curves,
            textures,
            uses_screen_texture,
            indirect_texture_index,
        })
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
        base_lifetime: be_u16(block, 0x40)?,
        lifetime_random_scale: fixed(block, 0x42)?,
        base_weight: fixed(block, 0x44)?,
        weight_random_scale: fixed(block, 0x46)?,
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
        size: [be_f32(block, 0x1c)?, be_f32(block, 0x18)?],
        particle_type: block[0x24],
        tiling: [fixed(block, 0x88)? * 10.0, fixed(block, 0x8a)? * 10.0],
        texture_index: block[0x4f],
        color: block[0x64..0x68].try_into().unwrap(),
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

fn parse_child_shape(block: &[u8]) -> Result<JpaChildShape> {
    require(block, 0x62)?;
    Ok(JpaChildShape {
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
        inherit_flags: block[0x57],
        draw_parent: block[0x44] & 1 != 0,
        texture_index: block[0x47],
        color: block[0x58..0x5c].try_into().unwrap(),
        inherit_rgb: fixed(block, 0x60)?,
    })
}

fn parse_field(block: &[u8]) -> Result<JpaField> {
    require(block, 0x48)?;
    Ok(JpaField {
        kind: block[0x0c],
        status: be_u16(block, 0x10)?,
        magnitude: be_f32(block, 0x14)?,
        direction: vec3(block, 0x2c)?,
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
    fn parses_child_particle_shape() {
        let mut block = vec![0; 0x62];
        block[..4].copy_from_slice(b"SSP1");
        let block_size = block.len() as u32;
        block[4..8].copy_from_slice(&block_size.to_be_bytes());
        block[0x14..0x16].copy_from_slice(&90i16.to_be_bytes());
        block[0x16..0x18].copy_from_slice(&2i16.to_be_bytes());
        block[0x18..0x1a].copy_from_slice(&16384i16.to_be_bytes());
        block[0x1a] = 3;
        block[0x44] = 1;
        block[0x47] = 2;
        block[0x4c..0x50].copy_from_slice(&1.5f32.to_be_bytes());
        block[0x50..0x54].copy_from_slice(&2.0f32.to_be_bytes());
        block[0x58..0x5c].copy_from_slice(&[10, 20, 30, 40]);

        let child = parse_child_shape(&block).unwrap();
        assert_eq!(child.lifetime, 90);
        assert_eq!(child.spawn_count, 2);
        assert_eq!(child.spawn_timing, 0.5);
        assert_eq!(child.spawn_step, 3);
        assert!(child.draw_parent);
        assert_eq!(child.texture_index, 2);
        assert_eq!(child.size, [2.0, 1.5]);
        assert_eq!(child.color, [10, 20, 30, 40]);
    }
}
