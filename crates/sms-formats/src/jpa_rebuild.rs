//! Source-free semantic reconstruction for SMS's JParticle 1 effect files.

use encoding_rs::SHIFT_JIS;
use serde::{Deserialize, Serialize};

use crate::binary::{be_f32, be_i16, be_u16, be_u32, require_len, require_magic};
use crate::{FormatError, Result};

const FORMAT: &str = "JPA1 rebuild";
const HEADER_SIZE: usize = 0x20;

#[derive(Debug, Clone, Copy)]
struct JsonF32(f32);

impl Serialize for JsonF32 {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        if self.0.is_finite() {
            self.0.serialize(serializer)
        } else {
            #[derive(Serialize)]
            struct BitPattern {
                f32_bits: u32,
            }
            BitPattern {
                f32_bits: self.0.to_bits(),
            }
            .serialize(serializer)
        }
    }
}

impl<'de> Deserialize<'de> for JsonF32 {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Representation {
            Number(f32),
            BitPattern { f32_bits: u32 },
            LegacyNull(()),
        }
        Ok(match Representation::deserialize(deserializer)? {
            Representation::Number(value) => Self(value),
            Representation::BitPattern { f32_bits } => Self(f32::from_bits(f32_bits)),
            // serde_json historically wrote every non-finite f32 as null. The
            // affected retail JPA corpus uses 0xffffffff as its field sentinel.
            Representation::LegacyNull(()) => Self(f32::from_bits(u32::MAX)),
        })
    }
}

mod f32_serde {
    use super::JsonF32;
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    pub fn serialize<S>(value: &f32, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        JsonF32(*value).serialize(serializer)
    }

    pub fn deserialize<'de, D>(deserializer: D) -> std::result::Result<f32, D::Error>
    where
        D: Deserializer<'de>,
    {
        Ok(JsonF32::deserialize(deserializer)?.0)
    }
}

mod f32_array_serde {
    use super::JsonF32;
    use serde::de::Error as _;
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    pub fn serialize<S, const N: usize>(
        values: &[f32; N],
        serializer: S,
    ) -> std::result::Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        values
            .iter()
            .copied()
            .map(JsonF32)
            .collect::<Vec<_>>()
            .serialize(serializer)
    }

    pub fn deserialize<'de, D, const N: usize>(
        deserializer: D,
    ) -> std::result::Result<[f32; N], D::Error>
    where
        D: Deserializer<'de>,
    {
        let values = Vec::<JsonF32>::deserialize(deserializer)?;
        let length = values.len();
        let values: [JsonF32; N] = values
            .try_into()
            .map_err(|_| D::Error::custom(format!("expected {N} f32 values, found {length}")))?;
        Ok(values.map(|value| value.0))
    }
}

mod f32_vec4_serde {
    use super::JsonF32;
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    pub fn serialize<S>(values: &[[f32; 4]], serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        values
            .iter()
            .map(|value| value.map(JsonF32))
            .collect::<Vec<_>>()
            .serialize(serializer)
    }

    pub fn deserialize<'de, D>(deserializer: D) -> std::result::Result<Vec<[f32; 4]>, D::Error>
    where
        D: Deserializer<'de>,
    {
        Ok(Vec::<[JsonF32; 4]>::deserialize(deserializer)?
            .into_iter()
            .map(|value| value.map(|component| component.0))
            .collect())
    }
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct JpaxDocument {
    pub blocks: Vec<JpaxBlock>,
    pub layout: JpaxLayout,
    /// Zero-filled allocation bytes after the physical block list.
    pub trailing_zero_padding: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum JpaxLayout {
    Standard,
    /// The retail `ms_m_spinos.jpa` has one zero byte before its final TEX1.
    /// The declared size excludes that byte, so the retail loader's walk is
    /// malformed even though the physical texture and final padding are intact.
    RetailMalformedTexturePrefix,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum JpaxBlock {
    Dynamics(JpaxDynamicsBlock),
    BaseShape(JpaxBaseShapeBlock),
    ExtraShape(JpaxExtraShapeBlock),
    ChildShape(JpaxChildShapeBlock),
    Field(JpaxFieldBlock),
    Keyframes(JpaxKeyframeBlock),
    ExtraTexture(JpaxExtraTextureBlock),
    Texture(JpaxTextureBlock),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct JpaxDynamicsBlock {
    #[serde(with = "f32_array_serde")]
    pub scale: [f32; 3],
    #[serde(with = "f32_array_serde")]
    pub translation: [f32; 3],
    pub rotation: [i16; 3],
    pub volume_type: u8,
    pub emit_interval: u8,
    pub volume_subdivision: u16,
    #[serde(with = "f32_serde")]
    pub spawn_rate: f32,
    pub spawn_rate_random: i16,
    pub max_frame: i16,
    pub start_frame: i16,
    pub volume_size: u16,
    pub volume_yaw_sweep: i16,
    pub volume_min_radius: i16,
    pub base_lifetime: u16,
    pub lifetime_random: i16,
    pub base_weight: i16,
    pub weight_random: i16,
    pub initial_velocity_random: i16,
    pub momentum_random: i16,
    pub base_air_resistance: i16,
    pub air_resistance_random: i16,
    #[serde(with = "f32_array_serde")]
    pub initial_velocity: [f32; 4],
    #[serde(with = "f32_serde")]
    pub initial_moment: f32,
    pub direction: [i16; 3],
    pub direction_spread: i16,
    pub emit_flags: u32,
    pub keyframe_mask: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct JpaxColorKey {
    pub frame: i16,
    pub color: [u8; 4],
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct JpaxBaseShapeBlock {
    pub allocation_size: u32,
    pub parameter_offsets: [u8; 6],
    pub texture_indices_offset: u16,
    pub primary_colors_offset: u16,
    pub environment_colors_offset: u16,
    #[serde(with = "f32_serde")]
    pub base_size_y: f32,
    #[serde(with = "f32_serde")]
    pub base_size_x: f32,
    pub loop_offset: i16,
    pub color_animation_flags: u8,
    pub texture_animation_flags: u8,
    pub particle_type: u8,
    pub direction_type: u8,
    pub rotation_type: u8,
    pub color_combine_mode: u8,
    pub alpha_combine_mode: u8,
    /// Authored JPAC tool bytes not consumed by SMS's JPABaseShape runtime.
    pub reserved_32_34: [u8; 3],
    pub blend_mode: u8,
    pub source_blend_factor: u8,
    pub destination_blend_factor: u8,
    pub blend_operation: u8,
    pub alpha_compare_0: u8,
    pub alpha_reference_0: u8,
    pub alpha_operation: u8,
    pub alpha_compare_1: u8,
    pub alpha_reference_1: u8,
    pub z_compare_location_flags: u8,
    pub z_compare_enable: u8,
    pub z_compare_function: u8,
    pub z_update_enable: u8,
    pub draw_flags_42: u8,
    pub draw_flags_43: u8,
    pub shape_flags: u8,
    pub texture_key_flags: u8,
    pub texture_animation_mode: u8,
    pub texture_index: u8,
    pub color_animation_max_frame: i16,
    pub color_animation_mode: u8,
    pub primary_color_flags: u8,
    pub environment_color_flags: u8,
    pub primary_color: [u8; 4],
    pub environment_color: [u8; 4],
    pub texture_transform: [i16; 11],
    pub texture_animation_random_mask: u8,
    pub texture_indices: Vec<u8>,
    pub primary_colors: Vec<JpaxColorKey>,
    pub environment_colors: Vec<JpaxColorKey>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct JpaxExtraShapeBlock {
    pub parameter_offsets: [u8; 8],
    pub alpha_in_timing: i16,
    pub alpha_out_timing: i16,
    pub alpha_in_value: i16,
    pub alpha_base_value: i16,
    pub alpha_out_value: i16,
    pub alpha_flags: u8,
    pub alpha_wave_type: u8,
    pub alpha_wave_parameters: [i16; 4],
    pub random_scale: i16,
    pub scale_in_timing: i16,
    pub scale_out_timing: i16,
    pub scale_values_y: [i16; 3],
    pub scale_pivot_y: u8,
    pub scale_cycle_reverse_y: u8,
    pub scale_cycle_y: i16,
    pub scale_values_x: [i16; 3],
    pub scale_pivot_x: u8,
    pub scale_cycle_reverse_x: u8,
    pub scale_cycle_x: i16,
    pub scale_flags: u8,
    /// Authored tool byte at 0x4f, nonzero only in retail `ms_m_spinos.jpa`.
    pub reserved_4f: u8,
    pub rotation: [i16; 5],
    pub rotation_enabled: u8,
    /// Authored tool byte at 0x65, nonzero only in retail `ms_m_spinos.jpa`.
    pub reserved_65: u8,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct JpaxChildShapeBlock {
    pub parameter_offsets: [u8; 4],
    pub particle_type: u8,
    pub direction_type: u8,
    pub rotation_type: u8,
    pub lifetime: i16,
    pub spawn_count: i16,
    pub spawn_timing: i16,
    pub spawn_step: u8,
    #[serde(with = "f32_serde")]
    pub position_random: f32,
    #[serde(with = "f32_serde")]
    pub base_velocity: f32,
    pub inherit_velocity: i16,
    pub velocity_random: i16,
    pub velocity_direction_spread: i16,
    pub children_affected_by_fields: u8,
    pub draw_flags: u8,
    pub scale_out_enabled: u8,
    pub alpha_out_enabled: u8,
    pub texture_index: u8,
    pub inherit_scale: i16,
    pub inherit_alpha: i16,
    #[serde(with = "f32_serde")]
    pub base_size_y: f32,
    #[serde(with = "f32_serde")]
    pub base_size_x: f32,
    pub rotate_speed: i16,
    pub rotate_enabled: u8,
    pub inherit_flags: u8,
    pub primary_color: [u8; 4],
    pub environment_color: [u8; 4],
    pub inherit_rgb: i16,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct JpaxFieldBlock {
    pub kind: u8,
    pub field_flags: u8,
    pub add_type: u8,
    pub cycle: u8,
    pub status: u16,
    #[serde(with = "f32_serde")]
    pub magnitude: f32,
    #[serde(with = "f32_serde")]
    pub secondary_magnitude: f32,
    #[serde(with = "f32_serde")]
    pub max_distance: f32,
    #[serde(with = "f32_array_serde")]
    pub position: [f32; 3],
    #[serde(with = "f32_array_serde")]
    pub direction: [f32; 3],
    #[serde(with = "f32_array_serde")]
    pub parameters: [f32; 3],
    pub fade: [i16; 4],
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct JpaxKeyframeBlock {
    pub allocation_size: u32,
    pub parameter_type: u8,
    pub interpolation_type: u8,
    pub loop_enabled: u8,
    pub reserved_13: u8,
    #[serde(with = "f32_vec4_serde")]
    pub keys: Vec<[f32; 4]>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct JpaxExtraTextureBlock {
    pub parameter_offsets: [u8; 4],
    pub indirect_mode: u8,
    pub matrix_mode: u8,
    pub indirect_matrix: [i16; 6],
    pub exponent: i8,
    pub indirect_texture_index: u8,
    pub sub_texture_index: u8,
    pub second_texture_flags: u8,
    pub second_texture_index: u8,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JpaxTextureBlock {
    pub name: String,
    pub texture: JpaxTextureImage,
}

/// A TEX1 ResTIMG with the complete native GX mip chain authored by JPAC.
///
/// `active_mipmap_count` is the runtime-visible header value. For mipmapped
/// textures JPAC stores all tiled levels down to 1x1 even when only a prefix is
/// active; non-mipmapped textures store only the base level.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JpaxTextureImage {
    pub allocation_size: u32,
    pub format: u8,
    pub transparency: u8,
    pub width: u16,
    pub height: u16,
    pub wrap_s: u8,
    pub wrap_t: u8,
    pub palette_enabled: u8,
    pub palette_format: u8,
    pub palette_entries: Vec<u16>,
    pub palette_offset: u32,
    pub mipmap_enabled: u8,
    pub edge_lod: u8,
    pub bias_clamp: u8,
    pub max_anisotropy: u8,
    pub min_filter: u8,
    pub mag_filter: u8,
    pub min_lod: i8,
    pub max_lod: i8,
    pub active_mipmap_count: u8,
    pub reserved_19: u8,
    pub lod_bias: i16,
    pub image_offset: u32,
    pub encoded_mip_levels: Vec<Vec<u8>>,
}

impl JpaxDocument {
    /// Builds a complete source-free effect that registers safely but emits no
    /// particles. This is useful for runtime-required effect slots whose
    /// visual effect is intentionally absent from authored content.
    pub fn authored_noop() -> Self {
        Self {
            blocks: vec![
                JpaxBlock::Dynamics(JpaxDynamicsBlock {
                    scale: [1.0; 3],
                    translation: [0.0; 3],
                    rotation: [0; 3],
                    volume_type: 0,
                    emit_interval: 0,
                    volume_subdivision: 1,
                    spawn_rate: 0.0,
                    spawn_rate_random: 0,
                    max_frame: 0,
                    start_frame: 0,
                    volume_size: 0,
                    volume_yaw_sweep: 0,
                    volume_min_radius: 0,
                    base_lifetime: 1,
                    lifetime_random: 0,
                    base_weight: 0,
                    weight_random: 0,
                    initial_velocity_random: 0,
                    momentum_random: 0,
                    base_air_resistance: 0,
                    air_resistance_random: 0,
                    initial_velocity: [0.0; 4],
                    initial_moment: 0.0,
                    // The runtime normalizes this vector even when the spawn
                    // rate is zero, so it must remain nonzero.
                    direction: [0, 0, i16::MAX],
                    direction_spread: 0,
                    emit_flags: 0,
                    keyframe_mask: 0,
                }),
                JpaxBlock::BaseShape(JpaxBaseShapeBlock {
                    allocation_size: 0xa0,
                    parameter_offsets: [0; 6],
                    texture_indices_offset: 0,
                    primary_colors_offset: 0,
                    environment_colors_offset: 0,
                    base_size_y: 0.0,
                    base_size_x: 0.0,
                    loop_offset: 0,
                    color_animation_flags: 0,
                    texture_animation_flags: 0,
                    particle_type: 0,
                    direction_type: 0,
                    rotation_type: 0,
                    color_combine_mode: 0,
                    alpha_combine_mode: 0,
                    reserved_32_34: [0; 3],
                    blend_mode: 0,
                    source_blend_factor: 0,
                    destination_blend_factor: 0,
                    blend_operation: 0,
                    alpha_compare_0: 0,
                    alpha_reference_0: 0,
                    alpha_operation: 0,
                    alpha_compare_1: 0,
                    alpha_reference_1: 0,
                    z_compare_location_flags: 0,
                    z_compare_enable: 0,
                    z_compare_function: 0,
                    z_update_enable: 0,
                    draw_flags_42: 0,
                    draw_flags_43: 0,
                    shape_flags: 0,
                    texture_key_flags: 0,
                    texture_animation_mode: 0,
                    texture_index: 0,
                    color_animation_max_frame: 0,
                    color_animation_mode: 0,
                    primary_color_flags: 0,
                    environment_color_flags: 0,
                    primary_color: [0; 4],
                    environment_color: [0; 4],
                    texture_transform: [0; 11],
                    texture_animation_random_mask: 0,
                    texture_indices: Vec::new(),
                    primary_colors: Vec::new(),
                    environment_colors: Vec::new(),
                }),
            ],
            layout: JpaxLayout::Standard,
            trailing_zero_padding: 0,
        }
    }

    pub fn parse(bytes: impl AsRef<[u8]>) -> Result<Self> {
        let bytes = bytes.as_ref();
        require_len(FORMAT, bytes, HEADER_SIZE)?;
        require_magic(FORMAT, bytes, b"JEFFjpa1")?;
        let declared_size = be_u32(bytes, 8, FORMAT)? as usize;
        let block_count = be_u32(bytes, 0x0c, FORMAT)? as usize;
        if declared_size < HEADER_SIZE || declared_size > bytes.len() {
            return Err(invalid(declared_size, bytes.len()));
        }
        ensure_zero("JPA header reserved", bytes, 0x10, 0x10)?;
        if block_count > 0x10000 {
            return Err(resource("blocks", block_count, 0x10000));
        }
        let mut blocks = Vec::with_capacity(block_count);
        let mut offset = HEADER_SIZE;
        let mut layout = JpaxLayout::Standard;
        for index in 0..block_count {
            require_range(bytes, offset, 8)?;
            let malformed_texture_prefix =
                bytes[offset] == 0 && bytes.get(offset + 1..offset + 5) == Some(b"TEX1");
            if malformed_texture_prefix {
                if layout != JpaxLayout::Standard || index + 1 != block_count {
                    return Err(unsupported(format!(
                        "unexpected zero-prefixed TEX1 at block {index}"
                    )));
                }
                layout = JpaxLayout::RetailMalformedTexturePrefix;
            }
            let header_offset = offset + usize::from(malformed_texture_prefix);
            require_range(bytes, header_offset, 8)?;
            let tag: [u8; 4] = bytes[header_offset..header_offset + 4]
                .try_into()
                .expect("covered JPA tag");
            let size = be_u32(bytes, header_offset + 4, FORMAT)? as usize;
            let logical_end = offset.checked_add(size);
            let physical_end = header_offset.checked_add(size);
            if size < 8
                || logical_end.is_none_or(|end| end > declared_size)
                || physical_end.is_none_or(|end| end > bytes.len())
            {
                return Err(unsupported(format!(
                    "block {index} {:?} has invalid size {size:#x} at {offset:#x}",
                    String::from_utf8_lossy(&tag)
                )));
            }
            let physical_end = physical_end.expect("validated JPA physical block end");
            let block = &bytes[header_offset..physical_end];
            blocks.push(match &tag {
                b"BEM1" => JpaxBlock::Dynamics(parse_dynamics(block)?),
                b"BSP1" => JpaxBlock::BaseShape(parse_base_shape(block)?),
                b"ESP1" => JpaxBlock::ExtraShape(parse_extra_shape(block)?),
                b"SSP1" => JpaxBlock::ChildShape(parse_child_shape(block)?),
                b"FLD1" => JpaxBlock::Field(parse_field(block)?),
                b"KFA1" => JpaxBlock::Keyframes(parse_keyframes(block)?),
                b"ETX1" => JpaxBlock::ExtraTexture(parse_extra_texture(block)?),
                b"TEX1" => JpaxBlock::Texture(parse_texture(block)?),
                _ => {
                    return Err(unsupported(format!(
                        "unknown JPA block {:?} at {offset:#x}",
                        String::from_utf8_lossy(&tag)
                    )))
                }
            });
            offset = physical_end;
        }
        let prefix_size = usize::from(layout == JpaxLayout::RetailMalformedTexturePrefix);
        let logical_block_end = offset
            .checked_sub(prefix_size)
            .ok_or_else(|| invalid(offset, bytes.len()))?;
        if logical_block_end != declared_size {
            return Err(unsupported(format!(
                "JPA logical block list ends at {logical_block_end:#x}, declared size is {declared_size:#x}"
            )));
        }
        ensure_zero(
            "JPA trailing allocation",
            bytes,
            offset,
            bytes.len() - offset,
        )?;
        Ok(Self {
            blocks,
            layout,
            trailing_zero_padding: usize_u32(bytes.len() - offset, "trailing bytes")?,
        })
    }

    pub fn encode(&self) -> Result<Vec<u8>> {
        if self.layout == JpaxLayout::RetailMalformedTexturePrefix
            && !matches!(self.blocks.last(), Some(JpaxBlock::Texture(_)))
        {
            return Err(unsupported(
                "malformed retail JPA layout requires a final TEX1 block",
            ));
        }
        let encoded_blocks = self
            .blocks
            .iter()
            .map(encode_block)
            .collect::<Result<Vec<_>>>()?;
        let logical_size = encoded_blocks.iter().try_fold(HEADER_SIZE, |size, block| {
            size.checked_add(block.len())
                .ok_or_else(|| invalid(size, usize::MAX))
        })?;
        let mut bytes = vec![0; HEADER_SIZE];
        bytes[..8].copy_from_slice(b"JEFFjpa1");
        put_u32(
            &mut bytes,
            8,
            usize_u32(logical_size, "declared file size")?,
        )?;
        put_u32(
            &mut bytes,
            0x0c,
            usize_u32(self.blocks.len(), "block count")?,
        )?;
        for (index, block) in encoded_blocks.iter().enumerate() {
            if self.layout == JpaxLayout::RetailMalformedTexturePrefix
                && index + 1 == encoded_blocks.len()
            {
                bytes.push(0);
            }
            bytes.extend_from_slice(block);
        }
        let final_size = bytes
            .len()
            .checked_add(self.trailing_zero_padding as usize)
            .ok_or_else(|| invalid(bytes.len(), usize::MAX))?;
        bytes.resize(final_size, 0);
        Ok(bytes)
    }

    pub fn to_bytes(&self) -> Result<Vec<u8>> {
        self.encode()
    }
}

fn parse_dynamics(block: &[u8]) -> Result<JpaxDynamicsBlock> {
    expect_size(block, b"BEM1", 0xa0)?;
    let mut coverage = BlockCoverage::new(block.len());
    coverage.mark(0, 8)?;
    coverage.mark(0x0c, 0x68)?;
    let result = JpaxDynamicsBlock {
        scale: read_f32_array(block, 0x0c)?,
        translation: read_f32_array(block, 0x18)?,
        rotation: read_i16_array(block, 0x24)?,
        volume_type: block[0x2a],
        emit_interval: block[0x2b],
        volume_subdivision: be_u16(block, 0x2e, FORMAT)?,
        spawn_rate: be_f32(block, 0x30, FORMAT)?,
        spawn_rate_random: be_i16(block, 0x34, FORMAT)?,
        max_frame: be_i16(block, 0x36, FORMAT)?,
        start_frame: be_i16(block, 0x38, FORMAT)?,
        volume_size: be_u16(block, 0x3a, FORMAT)?,
        volume_yaw_sweep: be_i16(block, 0x3c, FORMAT)?,
        volume_min_radius: be_i16(block, 0x3e, FORMAT)?,
        base_lifetime: be_u16(block, 0x40, FORMAT)?,
        lifetime_random: be_i16(block, 0x42, FORMAT)?,
        base_weight: be_i16(block, 0x44, FORMAT)?,
        weight_random: be_i16(block, 0x46, FORMAT)?,
        initial_velocity_random: be_i16(block, 0x48, FORMAT)?,
        momentum_random: be_i16(block, 0x4a, FORMAT)?,
        base_air_resistance: be_i16(block, 0x4c, FORMAT)?,
        air_resistance_random: be_i16(block, 0x4e, FORMAT)?,
        initial_velocity: read_f32_array(block, 0x50)?,
        initial_moment: be_f32(block, 0x60, FORMAT)?,
        direction: read_i16_array(block, 0x64)?,
        direction_spread: be_i16(block, 0x6a, FORMAT)?,
        emit_flags: be_u32(block, 0x6c, FORMAT)?,
        keyframe_mask: be_u32(block, 0x70, FORMAT)?,
    };
    coverage.ensure_zero(block, "BEM1")?;
    Ok(result)
}

fn parse_base_shape(block: &[u8]) -> Result<JpaxBaseShapeBlock> {
    expect_tag(block, b"BSP1")?;
    if !matches!(block.len(), 0xa0 | 0xc0 | 0xe0) {
        return Err(unsupported(format!(
            "BSP1 allocation size {:#x} is not a retail layout",
            block.len()
        )));
    }
    let mut coverage = BlockCoverage::new(block.len());
    for (offset, length) in [
        (0, 8),
        (0x0c, 0x1b),
        (0x30, 5),
        (0x35, 0x10),
        (0x4c, 4),
        (0x5c, 3),
        (0x60, 0x0c),
        (0x80, 0x17),
    ] {
        coverage.mark(offset, length)?;
    }
    let texture_count = block[0x4e] as usize;
    let primary_count = block[0x62] as usize;
    let environment_count = block[0x63] as usize;
    let texture_indices_offset = be_u16(block, 0x12, FORMAT)?;
    let primary_colors_offset = be_u16(block, 0x14, FORMAT)?;
    let environment_colors_offset = be_u16(block, 0x16, FORMAT)?;
    let texture_indices = read_u8_table(
        block,
        &mut coverage,
        texture_indices_offset,
        texture_count,
        "BSP1 texture indices",
    )?;
    let primary_colors = read_color_keys(
        block,
        &mut coverage,
        primary_colors_offset,
        primary_count,
        "BSP1 primary colors",
    )?;
    let environment_colors = read_color_keys(
        block,
        &mut coverage,
        environment_colors_offset,
        environment_count,
        "BSP1 environment colors",
    )?;
    let mut texture_transform = [0i16; 11];
    for (index, value) in texture_transform.iter_mut().enumerate() {
        *value = be_i16(block, 0x80 + index * 2, FORMAT)?;
    }
    let result = JpaxBaseShapeBlock {
        allocation_size: usize_u32(block.len(), "BSP1 allocation")?,
        parameter_offsets: block[0x0c..0x12]
            .try_into()
            .expect("covered BSP1 parameter offsets"),
        texture_indices_offset,
        primary_colors_offset,
        environment_colors_offset,
        base_size_y: be_f32(block, 0x18, FORMAT)?,
        base_size_x: be_f32(block, 0x1c, FORMAT)?,
        loop_offset: be_i16(block, 0x20, FORMAT)?,
        color_animation_flags: block[0x22],
        texture_animation_flags: block[0x23],
        particle_type: block[0x24],
        direction_type: block[0x25],
        rotation_type: block[0x26],
        color_combine_mode: block[0x30],
        alpha_combine_mode: block[0x31],
        reserved_32_34: block[0x32..0x35]
            .try_into()
            .expect("covered BSP1 tool bytes"),
        blend_mode: block[0x35],
        source_blend_factor: block[0x36],
        destination_blend_factor: block[0x37],
        blend_operation: block[0x38],
        alpha_compare_0: block[0x39],
        alpha_reference_0: block[0x3a],
        alpha_operation: block[0x3b],
        alpha_compare_1: block[0x3c],
        alpha_reference_1: block[0x3d],
        z_compare_location_flags: block[0x3e],
        z_compare_enable: block[0x3f],
        z_compare_function: block[0x40],
        z_update_enable: block[0x41],
        draw_flags_42: block[0x42],
        draw_flags_43: block[0x43],
        shape_flags: block[0x44],
        texture_key_flags: block[0x4c],
        texture_animation_mode: block[0x4d],
        texture_index: block[0x4f],
        color_animation_max_frame: be_i16(block, 0x5c, FORMAT)?,
        color_animation_mode: block[0x5e],
        primary_color_flags: block[0x60],
        environment_color_flags: block[0x61],
        primary_color: block[0x64..0x68]
            .try_into()
            .expect("covered BSP1 primary color"),
        environment_color: block[0x68..0x6c]
            .try_into()
            .expect("covered BSP1 environment color"),
        texture_transform,
        texture_animation_random_mask: block[0x96],
        texture_indices,
        primary_colors,
        environment_colors,
    };
    coverage.ensure_zero(block, "BSP1")?;
    Ok(result)
}

fn parse_extra_shape(block: &[u8]) -> Result<JpaxExtraShapeBlock> {
    expect_size(block, b"ESP1", 0x80)?;
    let mut coverage = BlockCoverage::new(block.len());
    for (offset, length) in [(0, 8), (0x0c, 0x1c), (0x34, 0x1c), (0x5a, 0x0c)] {
        coverage.mark(offset, length)?;
    }
    let result = JpaxExtraShapeBlock {
        parameter_offsets: block[0x0c..0x14]
            .try_into()
            .expect("covered ESP1 parameter offsets"),
        alpha_in_timing: be_i16(block, 0x14, FORMAT)?,
        alpha_out_timing: be_i16(block, 0x16, FORMAT)?,
        alpha_in_value: be_i16(block, 0x18, FORMAT)?,
        alpha_base_value: be_i16(block, 0x1a, FORMAT)?,
        alpha_out_value: be_i16(block, 0x1c, FORMAT)?,
        alpha_flags: block[0x1e],
        alpha_wave_type: block[0x1f],
        alpha_wave_parameters: read_i16_array(block, 0x20)?,
        random_scale: be_i16(block, 0x34, FORMAT)?,
        scale_in_timing: be_i16(block, 0x36, FORMAT)?,
        scale_out_timing: be_i16(block, 0x38, FORMAT)?,
        scale_values_y: read_i16_array(block, 0x3a)?,
        scale_pivot_y: block[0x40],
        scale_cycle_reverse_y: block[0x41],
        scale_cycle_y: be_i16(block, 0x42, FORMAT)?,
        scale_values_x: read_i16_array(block, 0x44)?,
        scale_pivot_x: block[0x4a],
        scale_cycle_reverse_x: block[0x4b],
        scale_cycle_x: be_i16(block, 0x4c, FORMAT)?,
        scale_flags: block[0x4e],
        reserved_4f: block[0x4f],
        rotation: read_i16_array(block, 0x5a)?,
        rotation_enabled: block[0x64],
        reserved_65: block[0x65],
    };
    coverage.ensure_zero(block, "ESP1")?;
    Ok(result)
}

fn parse_child_shape(block: &[u8]) -> Result<JpaxChildShapeBlock> {
    expect_size(block, b"SSP1", 0x80)?;
    let mut coverage = BlockCoverage::new(block.len());
    for (offset, length) in [(0, 8), (0x0c, 7), (0x14, 7), (0x28, 0x0f), (0x44, 0x1e)] {
        coverage.mark(offset, length)?;
    }
    let result = JpaxChildShapeBlock {
        parameter_offsets: block[0x0c..0x10]
            .try_into()
            .expect("covered SSP1 parameter offsets"),
        particle_type: block[0x10],
        direction_type: block[0x11],
        rotation_type: block[0x12],
        lifetime: be_i16(block, 0x14, FORMAT)?,
        spawn_count: be_i16(block, 0x16, FORMAT)?,
        spawn_timing: be_i16(block, 0x18, FORMAT)?,
        spawn_step: block[0x1a],
        position_random: be_f32(block, 0x28, FORMAT)?,
        base_velocity: be_f32(block, 0x2c, FORMAT)?,
        inherit_velocity: be_i16(block, 0x30, FORMAT)?,
        velocity_random: be_i16(block, 0x32, FORMAT)?,
        velocity_direction_spread: be_i16(block, 0x34, FORMAT)?,
        children_affected_by_fields: block[0x36],
        draw_flags: block[0x44],
        scale_out_enabled: block[0x45],
        alpha_out_enabled: block[0x46],
        texture_index: block[0x47],
        inherit_scale: be_i16(block, 0x48, FORMAT)?,
        inherit_alpha: be_i16(block, 0x4a, FORMAT)?,
        base_size_y: be_f32(block, 0x4c, FORMAT)?,
        base_size_x: be_f32(block, 0x50, FORMAT)?,
        rotate_speed: be_i16(block, 0x54, FORMAT)?,
        rotate_enabled: block[0x56],
        inherit_flags: block[0x57],
        primary_color: block[0x58..0x5c]
            .try_into()
            .expect("covered SSP1 primary color"),
        environment_color: block[0x5c..0x60]
            .try_into()
            .expect("covered SSP1 environment color"),
        inherit_rgb: be_i16(block, 0x60, FORMAT)?,
    };
    coverage.ensure_zero(block, "SSP1")?;
    Ok(result)
}

fn parse_field(block: &[u8]) -> Result<JpaxFieldBlock> {
    expect_size(block, b"FLD1", 0x60)?;
    let mut coverage = BlockCoverage::new(block.len());
    for (offset, length) in [(0, 8), (0x0c, 6), (0x14, 0x38)] {
        coverage.mark(offset, length)?;
    }
    let result = JpaxFieldBlock {
        kind: block[0x0c],
        field_flags: block[0x0d],
        add_type: block[0x0e],
        cycle: block[0x0f],
        status: be_u16(block, 0x10, FORMAT)?,
        magnitude: be_f32(block, 0x14, FORMAT)?,
        secondary_magnitude: be_f32(block, 0x18, FORMAT)?,
        max_distance: be_f32(block, 0x1c, FORMAT)?,
        position: read_f32_array(block, 0x20)?,
        direction: read_f32_array(block, 0x2c)?,
        parameters: read_f32_array(block, 0x38)?,
        fade: read_i16_array(block, 0x44)?,
    };
    coverage.ensure_zero(block, "FLD1")?;
    Ok(result)
}

fn parse_keyframes(block: &[u8]) -> Result<JpaxKeyframeBlock> {
    expect_tag(block, b"KFA1")?;
    if !matches!(block.len(), 0x40 | 0x60 | 0x80) {
        return Err(unsupported(format!(
            "KFA1 allocation size {:#x} is not a retail layout",
            block.len()
        )));
    }
    let mut coverage = BlockCoverage::new(block.len());
    coverage.mark(0, 8)?;
    coverage.mark(0x0c, 1)?;
    coverage.mark(0x10, 4)?;
    let count = block[0x10] as usize;
    let key_bytes = count
        .checked_mul(16)
        .ok_or_else(|| invalid(count, block.len()))?;
    coverage.mark(0x20, key_bytes)?;
    let mut keys = Vec::with_capacity(count);
    for index in 0..count {
        keys.push(read_f32_array(block, 0x20 + index * 16)?);
    }
    let result = JpaxKeyframeBlock {
        allocation_size: usize_u32(block.len(), "KFA1 allocation")?,
        parameter_type: block[0x0c],
        interpolation_type: block[0x11],
        loop_enabled: block[0x12],
        reserved_13: block[0x13],
        keys,
    };
    coverage.ensure_zero(block, "KFA1")?;
    Ok(result)
}

fn parse_extra_texture(block: &[u8]) -> Result<JpaxExtraTextureBlock> {
    expect_size(block, b"ETX1", 0x40)?;
    let mut coverage = BlockCoverage::new(block.len());
    for (offset, length) in [(0, 8), (0x0c, 4), (0x10, 0x11), (0x30, 1), (0x33, 1)] {
        coverage.mark(offset, length)?;
    }
    let result = JpaxExtraTextureBlock {
        parameter_offsets: block[0x0c..0x10]
            .try_into()
            .expect("covered ETX1 parameter offsets"),
        indirect_mode: block[0x10],
        matrix_mode: block[0x11],
        indirect_matrix: read_i16_array(block, 0x12)?,
        exponent: block[0x1e] as i8,
        indirect_texture_index: block[0x1f],
        sub_texture_index: block[0x20],
        second_texture_flags: block[0x30],
        second_texture_index: block[0x33],
    };
    coverage.ensure_zero(block, "ETX1")?;
    Ok(result)
}

fn parse_texture(block: &[u8]) -> Result<JpaxTextureBlock> {
    require_len(FORMAT, block, 0x40)?;
    if &block[..4] != b"TEX1" {
        return Err(bad_magic(block[..4].to_vec(), b"TEX1"));
    }
    if be_u32(block, 4, FORMAT)? as usize != block.len() {
        return Err(unsupported("TEX1 size field does not match its allocation"));
    }
    ensure_zero("TEX1 reserved", block, 8, 4)?;
    let name = read_fixed_string(block, 0x0c, 0x14)?;
    let texture = JpaxTextureImage::parse(&block[0x20..]).map_err(|error| {
        unsupported(format!(
            "TEX1 {name:?} texture payload ({} bytes, header {:02x?}): {error}",
            block.len() - 0x20,
            &block[0x20..0x40]
        ))
    })?;
    Ok(JpaxTextureBlock { name, texture })
}

impl JpaxTextureImage {
    fn parse(bytes: &[u8]) -> Result<Self> {
        require_len(FORMAT, bytes, 0x20)?;
        let width = be_u16(bytes, 2, FORMAT)?;
        let height = be_u16(bytes, 4, FORMAT)?;
        if width == 0 || height == 0 {
            return Err(unsupported(format!(
                "TEX1 has invalid {width}x{height} texture dimensions"
            )));
        }
        let palette_enabled = bytes[8];
        let palette_count = be_u16(bytes, 0x0a, FORMAT)? as usize;
        let palette_offset = be_u32(bytes, 0x0c, FORMAT)?;
        let image_offset = be_u32(bytes, 0x1c, FORMAT)?;
        let mut coverage = BlockCoverage::new(bytes.len());
        coverage.mark(0, 0x20)?;

        let mut palette_entries = Vec::with_capacity(palette_count);
        if palette_enabled != 0 || palette_count != 0 {
            let start = palette_offset as usize;
            let length = palette_count
                .checked_mul(2)
                .ok_or_else(|| invalid(start, bytes.len()))?;
            coverage.mark(start, length)?;
            for index in 0..palette_count {
                palette_entries.push(be_u16(bytes, start + index * 2, FORMAT)?);
            }
        }

        let level_count = stored_mip_level_count(width, height, bytes[0x10]);
        let mut encoded_mip_levels = Vec::with_capacity(level_count);
        let mut cursor = image_offset as usize;
        for level in 0..level_count {
            let level_width = (width >> level).max(1);
            let level_height = (height >> level).max(1);
            let length = encoded_gx_texture_size(bytes[0], level_width, level_height)?;
            coverage.mark(cursor, length)?;
            encoded_mip_levels.push(bytes[cursor..cursor + length].to_vec());
            cursor = cursor
                .checked_add(length)
                .ok_or_else(|| invalid(cursor, bytes.len()))?;
        }
        coverage.ensure_zero(bytes, "TEX1 ResTIMG")?;

        Ok(Self {
            allocation_size: usize_u32(bytes.len(), "TEX1 ResTIMG allocation")?,
            format: bytes[0],
            transparency: bytes[1],
            width,
            height,
            wrap_s: bytes[6],
            wrap_t: bytes[7],
            palette_enabled,
            palette_format: bytes[9],
            palette_entries,
            palette_offset,
            mipmap_enabled: bytes[0x10],
            edge_lod: bytes[0x11],
            bias_clamp: bytes[0x12],
            max_anisotropy: bytes[0x13],
            min_filter: bytes[0x14],
            mag_filter: bytes[0x15],
            min_lod: bytes[0x16] as i8,
            max_lod: bytes[0x17] as i8,
            active_mipmap_count: bytes[0x18],
            reserved_19: bytes[0x19],
            lod_bias: be_i16(bytes, 0x1a, FORMAT)?,
            image_offset,
            encoded_mip_levels,
        })
    }

    fn encode(&self) -> Result<Vec<u8>> {
        let size = self.allocation_size as usize;
        if size < 0x20 || self.width == 0 || self.height == 0 {
            return Err(unsupported(format!(
                "invalid TEX1 ResTIMG allocation {size:#x} or dimensions {}x{}",
                self.width, self.height
            )));
        }
        let expected_levels = stored_mip_level_count(self.width, self.height, self.mipmap_enabled);
        if self.encoded_mip_levels.len() != expected_levels {
            return Err(unsupported(format!(
                "TEX1 has {} native mip levels, expected complete {expected_levels}-level chain",
                self.encoded_mip_levels.len()
            )));
        }
        if usize::from(self.active_mipmap_count.max(1)) > expected_levels {
            return Err(unsupported(format!(
                "TEX1 activates {} mip levels but stores only {expected_levels}",
                self.active_mipmap_count
            )));
        }
        if self.palette_entries.len() > u16::MAX as usize {
            return Err(resource(
                "TEX1 palette entries",
                self.palette_entries.len(),
                u16::MAX as usize,
            ));
        }

        let mut bytes = vec![0; size];
        bytes[0] = self.format;
        bytes[1] = self.transparency;
        put_u16(&mut bytes, 2, self.width)?;
        put_u16(&mut bytes, 4, self.height)?;
        bytes[6] = self.wrap_s;
        bytes[7] = self.wrap_t;
        bytes[8] = self.palette_enabled;
        bytes[9] = self.palette_format;
        put_u16(&mut bytes, 0x0a, self.palette_entries.len() as u16)?;
        put_u32(&mut bytes, 0x0c, self.palette_offset)?;
        bytes[0x10] = self.mipmap_enabled;
        bytes[0x11] = self.edge_lod;
        bytes[0x12] = self.bias_clamp;
        bytes[0x13] = self.max_anisotropy;
        bytes[0x14] = self.min_filter;
        bytes[0x15] = self.mag_filter;
        bytes[0x16] = self.min_lod as u8;
        bytes[0x17] = self.max_lod as u8;
        bytes[0x18] = self.active_mipmap_count;
        bytes[0x19] = self.reserved_19;
        put_i16(&mut bytes, 0x1a, self.lod_bias)?;
        put_u32(&mut bytes, 0x1c, self.image_offset)?;

        let mut palette_cursor = self.palette_offset as usize;
        for entry in &self.palette_entries {
            put_u16(&mut bytes, palette_cursor, *entry)?;
            palette_cursor += 2;
        }
        let mut image_cursor = self.image_offset as usize;
        for (level, encoded) in self.encoded_mip_levels.iter().enumerate() {
            let width = (self.width >> level).max(1);
            let height = (self.height >> level).max(1);
            let expected = encoded_gx_texture_size(self.format, width, height)?;
            if encoded.len() != expected {
                return Err(unsupported(format!(
                    "TEX1 mip level {level} has {} bytes, expected {expected}",
                    encoded.len()
                )));
            }
            put_bytes(&mut bytes, image_cursor, encoded)?;
            image_cursor = image_cursor
                .checked_add(encoded.len())
                .ok_or_else(|| invalid(image_cursor, size))?;
        }
        Ok(bytes)
    }
}

fn stored_mip_level_count(width: u16, height: u16, mipmap_enabled: u8) -> usize {
    if mipmap_enabled == 0 {
        return 1;
    }
    let mut width = width;
    let mut height = height;
    let mut count = 1usize;
    while width > 1 || height > 1 {
        width = (width >> 1).max(1);
        height = (height >> 1).max(1);
        count += 1;
    }
    count
}

fn encoded_gx_texture_size(format: u8, width: u16, height: u16) -> Result<usize> {
    let (tile_width, tile_height, block_bytes) = match format {
        0 => (8usize, 8usize, 32usize),
        1 | 2 | 8 => (8, 4, 32),
        3 | 4 | 5 | 9 | 10 => (4, 4, 32),
        6 => (4, 4, 64),
        14 => (8, 8, 32),
        _ => return Err(unsupported(format!("unknown GX texture format {format}"))),
    };
    (width as usize)
        .div_ceil(tile_width)
        .checked_mul((height as usize).div_ceil(tile_height))
        .and_then(|blocks| blocks.checked_mul(block_bytes))
        .ok_or_else(|| invalid(usize::MAX, usize::MAX))
}

fn encode_block(block: &JpaxBlock) -> Result<Vec<u8>> {
    match block {
        JpaxBlock::Dynamics(value) => encode_dynamics(value),
        JpaxBlock::BaseShape(value) => encode_base_shape(value),
        JpaxBlock::ExtraShape(value) => encode_extra_shape(value),
        JpaxBlock::ChildShape(value) => encode_child_shape(value),
        JpaxBlock::Field(value) => encode_field(value),
        JpaxBlock::Keyframes(value) => encode_keyframes(value),
        JpaxBlock::ExtraTexture(value) => encode_extra_texture(value),
        JpaxBlock::Texture(value) => encode_texture(value),
    }
}

fn encode_dynamics(value: &JpaxDynamicsBlock) -> Result<Vec<u8>> {
    let mut block = new_block(b"BEM1", 0xa0)?;
    put_f32_array(&mut block, 0x0c, &value.scale)?;
    put_f32_array(&mut block, 0x18, &value.translation)?;
    put_i16_array(&mut block, 0x24, &value.rotation)?;
    block[0x2a] = value.volume_type;
    block[0x2b] = value.emit_interval;
    put_u16(&mut block, 0x2e, value.volume_subdivision)?;
    put_f32(&mut block, 0x30, value.spawn_rate)?;
    for (offset, item) in [
        (0x34, value.spawn_rate_random),
        (0x36, value.max_frame),
        (0x38, value.start_frame),
    ] {
        put_i16(&mut block, offset, item)?;
    }
    put_u16(&mut block, 0x3a, value.volume_size)?;
    for (offset, item) in [
        (0x3c, value.volume_yaw_sweep),
        (0x3e, value.volume_min_radius),
    ] {
        put_i16(&mut block, offset, item)?;
    }
    put_u16(&mut block, 0x40, value.base_lifetime)?;
    for (offset, item) in [
        (0x42, value.lifetime_random),
        (0x44, value.base_weight),
        (0x46, value.weight_random),
        (0x48, value.initial_velocity_random),
        (0x4a, value.momentum_random),
        (0x4c, value.base_air_resistance),
        (0x4e, value.air_resistance_random),
    ] {
        put_i16(&mut block, offset, item)?;
    }
    put_f32_array(&mut block, 0x50, &value.initial_velocity)?;
    put_f32(&mut block, 0x60, value.initial_moment)?;
    put_i16_array(&mut block, 0x64, &value.direction)?;
    put_i16(&mut block, 0x6a, value.direction_spread)?;
    put_u32(&mut block, 0x6c, value.emit_flags)?;
    put_u32(&mut block, 0x70, value.keyframe_mask)?;
    Ok(block)
}

fn encode_base_shape(value: &JpaxBaseShapeBlock) -> Result<Vec<u8>> {
    let size = value.allocation_size as usize;
    if !matches!(size, 0xa0 | 0xc0 | 0xe0) {
        return Err(unsupported(format!(
            "BSP1 allocation size {size:#x} is not a retail layout"
        )));
    }
    let mut block = new_block(b"BSP1", size)?;
    put_bytes(&mut block, 0x0c, &value.parameter_offsets)?;
    put_u16(&mut block, 0x12, value.texture_indices_offset)?;
    put_u16(&mut block, 0x14, value.primary_colors_offset)?;
    put_u16(&mut block, 0x16, value.environment_colors_offset)?;
    put_f32(&mut block, 0x18, value.base_size_y)?;
    put_f32(&mut block, 0x1c, value.base_size_x)?;
    put_i16(&mut block, 0x20, value.loop_offset)?;
    block[0x22] = value.color_animation_flags;
    block[0x23] = value.texture_animation_flags;
    block[0x24] = value.particle_type;
    block[0x25] = value.direction_type;
    block[0x26] = value.rotation_type;
    block[0x30] = value.color_combine_mode;
    block[0x31] = value.alpha_combine_mode;
    put_bytes(&mut block, 0x32, &value.reserved_32_34)?;
    for (offset, item) in [
        (0x35, value.blend_mode),
        (0x36, value.source_blend_factor),
        (0x37, value.destination_blend_factor),
        (0x38, value.blend_operation),
        (0x39, value.alpha_compare_0),
        (0x3a, value.alpha_reference_0),
        (0x3b, value.alpha_operation),
        (0x3c, value.alpha_compare_1),
        (0x3d, value.alpha_reference_1),
        (0x3e, value.z_compare_location_flags),
        (0x3f, value.z_compare_enable),
        (0x40, value.z_compare_function),
        (0x41, value.z_update_enable),
        (0x42, value.draw_flags_42),
        (0x43, value.draw_flags_43),
        (0x44, value.shape_flags),
    ] {
        block[offset] = item;
    }
    block[0x4c] = value.texture_key_flags;
    block[0x4d] = value.texture_animation_mode;
    block[0x4e] = usize_u8(value.texture_indices.len(), "BSP1 texture indices")?;
    block[0x4f] = value.texture_index;
    put_i16(&mut block, 0x5c, value.color_animation_max_frame)?;
    block[0x5e] = value.color_animation_mode;
    block[0x60] = value.primary_color_flags;
    block[0x61] = value.environment_color_flags;
    block[0x62] = usize_u8(value.primary_colors.len(), "BSP1 primary colors")?;
    block[0x63] = usize_u8(value.environment_colors.len(), "BSP1 environment colors")?;
    put_bytes(&mut block, 0x64, &value.primary_color)?;
    put_bytes(&mut block, 0x68, &value.environment_color)?;
    put_i16_array(&mut block, 0x80, &value.texture_transform)?;
    block[0x96] = value.texture_animation_random_mask;
    write_u8_table(
        &mut block,
        value.texture_indices_offset,
        &value.texture_indices,
        "BSP1 texture indices",
    )?;
    write_color_keys(
        &mut block,
        value.primary_colors_offset,
        &value.primary_colors,
        "BSP1 primary colors",
    )?;
    write_color_keys(
        &mut block,
        value.environment_colors_offset,
        &value.environment_colors,
        "BSP1 environment colors",
    )?;
    Ok(block)
}

fn encode_extra_shape(value: &JpaxExtraShapeBlock) -> Result<Vec<u8>> {
    let mut block = new_block(b"ESP1", 0x80)?;
    put_bytes(&mut block, 0x0c, &value.parameter_offsets)?;
    for (offset, item) in [
        (0x14, value.alpha_in_timing),
        (0x16, value.alpha_out_timing),
        (0x18, value.alpha_in_value),
        (0x1a, value.alpha_base_value),
        (0x1c, value.alpha_out_value),
    ] {
        put_i16(&mut block, offset, item)?;
    }
    block[0x1e] = value.alpha_flags;
    block[0x1f] = value.alpha_wave_type;
    put_i16_array(&mut block, 0x20, &value.alpha_wave_parameters)?;
    put_i16(&mut block, 0x34, value.random_scale)?;
    put_i16(&mut block, 0x36, value.scale_in_timing)?;
    put_i16(&mut block, 0x38, value.scale_out_timing)?;
    put_i16_array(&mut block, 0x3a, &value.scale_values_y)?;
    block[0x40] = value.scale_pivot_y;
    block[0x41] = value.scale_cycle_reverse_y;
    put_i16(&mut block, 0x42, value.scale_cycle_y)?;
    put_i16_array(&mut block, 0x44, &value.scale_values_x)?;
    block[0x4a] = value.scale_pivot_x;
    block[0x4b] = value.scale_cycle_reverse_x;
    put_i16(&mut block, 0x4c, value.scale_cycle_x)?;
    block[0x4e] = value.scale_flags;
    block[0x4f] = value.reserved_4f;
    put_i16_array(&mut block, 0x5a, &value.rotation)?;
    block[0x64] = value.rotation_enabled;
    block[0x65] = value.reserved_65;
    Ok(block)
}

fn encode_child_shape(value: &JpaxChildShapeBlock) -> Result<Vec<u8>> {
    let mut block = new_block(b"SSP1", 0x80)?;
    put_bytes(&mut block, 0x0c, &value.parameter_offsets)?;
    block[0x10] = value.particle_type;
    block[0x11] = value.direction_type;
    block[0x12] = value.rotation_type;
    put_i16(&mut block, 0x14, value.lifetime)?;
    put_i16(&mut block, 0x16, value.spawn_count)?;
    put_i16(&mut block, 0x18, value.spawn_timing)?;
    block[0x1a] = value.spawn_step;
    put_f32(&mut block, 0x28, value.position_random)?;
    put_f32(&mut block, 0x2c, value.base_velocity)?;
    put_i16(&mut block, 0x30, value.inherit_velocity)?;
    put_i16(&mut block, 0x32, value.velocity_random)?;
    put_i16(&mut block, 0x34, value.velocity_direction_spread)?;
    block[0x36] = value.children_affected_by_fields;
    block[0x44] = value.draw_flags;
    block[0x45] = value.scale_out_enabled;
    block[0x46] = value.alpha_out_enabled;
    block[0x47] = value.texture_index;
    put_i16(&mut block, 0x48, value.inherit_scale)?;
    put_i16(&mut block, 0x4a, value.inherit_alpha)?;
    put_f32(&mut block, 0x4c, value.base_size_y)?;
    put_f32(&mut block, 0x50, value.base_size_x)?;
    put_i16(&mut block, 0x54, value.rotate_speed)?;
    block[0x56] = value.rotate_enabled;
    block[0x57] = value.inherit_flags;
    put_bytes(&mut block, 0x58, &value.primary_color)?;
    put_bytes(&mut block, 0x5c, &value.environment_color)?;
    put_i16(&mut block, 0x60, value.inherit_rgb)?;
    Ok(block)
}

fn encode_field(value: &JpaxFieldBlock) -> Result<Vec<u8>> {
    let mut block = new_block(b"FLD1", 0x60)?;
    block[0x0c] = value.kind;
    block[0x0d] = value.field_flags;
    block[0x0e] = value.add_type;
    block[0x0f] = value.cycle;
    put_u16(&mut block, 0x10, value.status)?;
    put_f32(&mut block, 0x14, value.magnitude)?;
    put_f32(&mut block, 0x18, value.secondary_magnitude)?;
    put_f32(&mut block, 0x1c, value.max_distance)?;
    put_f32_array(&mut block, 0x20, &value.position)?;
    put_f32_array(&mut block, 0x2c, &value.direction)?;
    put_f32_array(&mut block, 0x38, &value.parameters)?;
    put_i16_array(&mut block, 0x44, &value.fade)?;
    Ok(block)
}

fn encode_keyframes(value: &JpaxKeyframeBlock) -> Result<Vec<u8>> {
    let size = value.allocation_size as usize;
    if !matches!(size, 0x40 | 0x60 | 0x80) {
        return Err(unsupported(format!(
            "KFA1 allocation size {size:#x} is not a retail layout"
        )));
    }
    let required = 0x20usize
        .checked_add(
            value
                .keys
                .len()
                .checked_mul(16)
                .ok_or_else(|| invalid(value.keys.len(), size))?,
        )
        .ok_or_else(|| invalid(value.keys.len(), size))?;
    if required > size {
        return Err(unsupported(format!(
            "KFA1 has {} keys but only {size:#x} allocation bytes",
            value.keys.len()
        )));
    }
    let mut block = new_block(b"KFA1", size)?;
    block[0x0c] = value.parameter_type;
    block[0x10] = usize_u8(value.keys.len(), "KFA1 keys")?;
    block[0x11] = value.interpolation_type;
    block[0x12] = value.loop_enabled;
    block[0x13] = value.reserved_13;
    for (index, key) in value.keys.iter().enumerate() {
        put_f32_array(&mut block, 0x20 + index * 16, key)?;
    }
    Ok(block)
}

fn encode_extra_texture(value: &JpaxExtraTextureBlock) -> Result<Vec<u8>> {
    let mut block = new_block(b"ETX1", 0x40)?;
    put_bytes(&mut block, 0x0c, &value.parameter_offsets)?;
    block[0x10] = value.indirect_mode;
    block[0x11] = value.matrix_mode;
    put_i16_array(&mut block, 0x12, &value.indirect_matrix)?;
    block[0x1e] = value.exponent as u8;
    block[0x1f] = value.indirect_texture_index;
    block[0x20] = value.sub_texture_index;
    block[0x30] = value.second_texture_flags;
    block[0x33] = value.second_texture_index;
    Ok(block)
}

fn encode_texture(value: &JpaxTextureBlock) -> Result<Vec<u8>> {
    let texture = value.texture.encode().map_err(|error| {
        unsupported(format!(
            "cannot encode TEX1 native texture payload: {error}"
        ))
    })?;
    let size = 0x20usize
        .checked_add(texture.len())
        .ok_or_else(|| invalid(texture.len(), usize::MAX))?;
    let mut block = vec![0; size];
    block[..4].copy_from_slice(b"TEX1");
    put_u32(&mut block, 4, usize_u32(size, "TEX1 allocation")?)?;
    write_fixed_string(&mut block, 0x0c, 0x14, &value.name)?;
    put_bytes(&mut block, 0x20, &texture)?;
    Ok(block)
}

struct BlockCoverage {
    claimed: Vec<bool>,
}

impl BlockCoverage {
    fn new(length: usize) -> Self {
        Self {
            claimed: vec![false; length],
        }
    }

    fn mark(&mut self, offset: usize, length: usize) -> Result<()> {
        let end = offset
            .checked_add(length)
            .ok_or_else(|| invalid(offset, self.claimed.len()))?;
        if end > self.claimed.len() {
            return Err(invalid(end, self.claimed.len()));
        }
        self.claimed[offset..end].fill(true);
        Ok(())
    }

    fn ensure_zero(&self, bytes: &[u8], tag: &str) -> Result<()> {
        if let Some(start) = bytes
            .iter()
            .zip(&self.claimed)
            .position(|(byte, claimed)| !claimed && *byte != 0)
        {
            let end = (start + 16).min(bytes.len());
            return Err(unsupported(format!(
                "{tag} nonsemantic residue at {start:#x}: {}",
                bytes[start..end]
                    .iter()
                    .map(|byte| format!("{byte:02x}"))
                    .collect::<String>()
            )));
        }
        Ok(())
    }
}

fn expect_tag(block: &[u8], tag: &'static [u8; 4]) -> Result<()> {
    require_len(FORMAT, block, 8)?;
    if &block[..4] != tag {
        return Err(bad_magic(block[..4].to_vec(), tag));
    }
    if be_u32(block, 4, FORMAT)? as usize != block.len() {
        return Err(unsupported(format!(
            "{} size field does not match its allocation",
            String::from_utf8_lossy(tag)
        )));
    }
    Ok(())
}

fn expect_size(block: &[u8], tag: &'static [u8; 4], size: usize) -> Result<()> {
    expect_tag(block, tag)?;
    if block.len() != size {
        return Err(unsupported(format!(
            "{} has {:#x} bytes, expected {size:#x}",
            String::from_utf8_lossy(tag),
            block.len()
        )));
    }
    Ok(())
}

fn new_block(tag: &[u8; 4], size: usize) -> Result<Vec<u8>> {
    if size < 8 {
        return Err(invalid(size, 8));
    }
    let mut block = vec![0; size];
    block[..4].copy_from_slice(tag);
    put_u32(&mut block, 4, usize_u32(size, "block allocation")?)?;
    Ok(block)
}

fn read_f32_array<const N: usize>(bytes: &[u8], offset: usize) -> Result<[f32; N]> {
    let mut result = [0.0; N];
    for (index, value) in result.iter_mut().enumerate() {
        *value = be_f32(bytes, offset + index * 4, FORMAT)?;
    }
    Ok(result)
}

fn read_i16_array<const N: usize>(bytes: &[u8], offset: usize) -> Result<[i16; N]> {
    let mut result = [0; N];
    for (index, value) in result.iter_mut().enumerate() {
        *value = be_i16(bytes, offset + index * 2, FORMAT)?;
    }
    Ok(result)
}

fn read_u8_table(
    bytes: &[u8],
    coverage: &mut BlockCoverage,
    offset: u16,
    count: usize,
    label: &'static str,
) -> Result<Vec<u8>> {
    if count == 0 {
        return Ok(Vec::new());
    }
    let offset = offset as usize;
    if offset == 0 {
        return Err(unsupported(format!("{label} has no data offset")));
    }
    coverage.mark(offset, count)?;
    Ok(bytes[offset..offset + count].to_vec())
}

fn read_color_keys(
    bytes: &[u8],
    coverage: &mut BlockCoverage,
    offset: u16,
    count: usize,
    label: &'static str,
) -> Result<Vec<JpaxColorKey>> {
    if count == 0 {
        return Ok(Vec::new());
    }
    let offset = offset as usize;
    if offset == 0 {
        return Err(unsupported(format!("{label} has no data offset")));
    }
    let length = count
        .checked_mul(6)
        .ok_or_else(|| invalid(count, bytes.len()))?;
    coverage.mark(offset, length)?;
    let mut keys = Vec::with_capacity(count);
    for index in 0..count {
        let base = offset + index * 6;
        keys.push(JpaxColorKey {
            frame: be_i16(bytes, base, FORMAT)?,
            color: bytes[base + 2..base + 6]
                .try_into()
                .expect("covered JPA color key"),
        });
    }
    Ok(keys)
}

fn write_u8_table(bytes: &mut [u8], offset: u16, values: &[u8], label: &'static str) -> Result<()> {
    if values.is_empty() {
        return Ok(());
    }
    if offset == 0 {
        return Err(unsupported(format!("{label} has no data offset")));
    }
    put_bytes(bytes, offset as usize, values)
}

fn write_color_keys(
    bytes: &mut [u8],
    offset: u16,
    keys: &[JpaxColorKey],
    label: &'static str,
) -> Result<()> {
    if keys.is_empty() {
        return Ok(());
    }
    if offset == 0 {
        return Err(unsupported(format!("{label} has no data offset")));
    }
    let mut cursor = offset as usize;
    for key in keys {
        put_i16(bytes, cursor, key.frame)?;
        put_bytes(bytes, cursor + 2, &key.color)?;
        cursor += 6;
    }
    Ok(())
}

fn read_fixed_string(bytes: &[u8], offset: usize, length: usize) -> Result<String> {
    require_range(bytes, offset, length)?;
    let field = &bytes[offset..offset + length];
    let encoded_length = field
        .iter()
        .position(|byte| *byte == 0)
        .unwrap_or(field.len());
    if field[encoded_length..].iter().any(|byte| *byte != 0) {
        return Err(unsupported(
            "TEX1 name has nonzero bytes after its terminator",
        ));
    }
    let encoded = &field[..encoded_length];
    let (decoded, had_errors) = SHIFT_JIS.decode_without_bom_handling(encoded);
    if had_errors {
        return Err(unsupported("TEX1 name is not valid Shift-JIS"));
    }
    let value = decoded.into_owned();
    let (roundtrip, _, had_errors) = SHIFT_JIS.encode(&value);
    if had_errors || roundtrip.as_ref() != encoded {
        return Err(unsupported("TEX1 name is not canonical Shift-JIS"));
    }
    Ok(value)
}

fn write_fixed_string(bytes: &mut [u8], offset: usize, length: usize, value: &str) -> Result<()> {
    let (encoded, _, had_errors) = SHIFT_JIS.encode(value);
    if had_errors || encoded.len() >= length {
        return Err(unsupported(format!(
            "TEX1 name needs {} bytes, fixed field permits {}",
            encoded.len(),
            length.saturating_sub(1)
        )));
    }
    require_range(bytes, offset, length)?;
    bytes[offset..offset + length].fill(0);
    bytes[offset..offset + encoded.len()].copy_from_slice(encoded.as_ref());
    Ok(())
}

fn put_f32_array<const N: usize>(bytes: &mut [u8], offset: usize, values: &[f32; N]) -> Result<()> {
    for (index, value) in values.iter().enumerate() {
        put_f32(bytes, offset + index * 4, *value)?;
    }
    Ok(())
}

fn put_i16_array<const N: usize>(bytes: &mut [u8], offset: usize, values: &[i16; N]) -> Result<()> {
    for (index, value) in values.iter().enumerate() {
        put_i16(bytes, offset + index * 2, *value)?;
    }
    Ok(())
}

fn ensure_zero(label: &'static str, bytes: &[u8], offset: usize, length: usize) -> Result<()> {
    require_range(bytes, offset, length)?;
    if let Some(relative) = bytes[offset..offset + length]
        .iter()
        .position(|byte| *byte != 0)
    {
        return Err(unsupported(format!(
            "{label} is nonzero at {:#x}",
            offset + relative
        )));
    }
    Ok(())
}

fn require_range(bytes: &[u8], offset: usize, length: usize) -> Result<()> {
    let end = offset
        .checked_add(length)
        .ok_or_else(|| invalid(offset, bytes.len()))?;
    if end > bytes.len() {
        return Err(invalid(end, bytes.len()));
    }
    Ok(())
}

fn put_bytes(bytes: &mut [u8], offset: usize, values: &[u8]) -> Result<()> {
    require_range(bytes, offset, values.len())?;
    bytes[offset..offset + values.len()].copy_from_slice(values);
    Ok(())
}

fn put_u16(bytes: &mut [u8], offset: usize, value: u16) -> Result<()> {
    put_bytes(bytes, offset, &value.to_be_bytes())
}

fn put_i16(bytes: &mut [u8], offset: usize, value: i16) -> Result<()> {
    put_bytes(bytes, offset, &value.to_be_bytes())
}

fn put_u32(bytes: &mut [u8], offset: usize, value: u32) -> Result<()> {
    put_bytes(bytes, offset, &value.to_be_bytes())
}

fn put_f32(bytes: &mut [u8], offset: usize, value: f32) -> Result<()> {
    put_u32(bytes, offset, value.to_bits())
}

fn usize_u8(value: usize, label: &'static str) -> Result<u8> {
    u8::try_from(value).map_err(|_| resource(label, value, u8::MAX as usize))
}

fn usize_u32(value: usize, label: &'static str) -> Result<u32> {
    u32::try_from(value).map_err(|_| resource(label, value, u32::MAX as usize))
}

fn resource(resource: &'static str, requested: usize, limit: usize) -> FormatError {
    FormatError::ResourceLimit {
        format: FORMAT,
        resource,
        requested,
        limit,
    }
}

fn invalid(offset: usize, len: usize) -> FormatError {
    FormatError::InvalidOffset {
        format: FORMAT,
        offset,
        len,
    }
}

fn bad_magic(actual: Vec<u8>, expected: &'static [u8]) -> FormatError {
    FormatError::BadMagic {
        format: FORMAT,
        expected,
        actual,
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

    fn zero_dynamics() -> JpaxDynamicsBlock {
        JpaxDynamicsBlock {
            scale: [0.0; 3],
            translation: [0.0; 3],
            rotation: [0; 3],
            volume_type: 0,
            emit_interval: 0,
            volume_subdivision: 0,
            spawn_rate: 0.0,
            spawn_rate_random: 0,
            max_frame: 0,
            start_frame: 0,
            volume_size: 0,
            volume_yaw_sweep: 0,
            volume_min_radius: 0,
            base_lifetime: 0,
            lifetime_random: 0,
            base_weight: 0,
            weight_random: 0,
            initial_velocity_random: 0,
            momentum_random: 0,
            base_air_resistance: 0,
            air_resistance_random: 0,
            initial_velocity: [0.0; 4],
            initial_moment: 0.0,
            direction: [0; 3],
            direction_spread: 0,
            emit_flags: 0,
            keyframe_mask: 0,
        }
    }

    #[test]
    fn json_preserves_non_finite_float_bits_and_migrates_legacy_nulls() {
        let mut dynamics = zero_dynamics();
        dynamics.scale = [1.0, f32::from_bits(0x7fc0_1234), f32::INFINITY];
        dynamics.spawn_rate = f32::NEG_INFINITY;
        dynamics.initial_velocity = [0.0, 1.0, f32::from_bits(u32::MAX), 3.0];
        let field = JpaxFieldBlock {
            kind: 1,
            field_flags: u8::MAX,
            add_type: 1,
            cycle: u8::MAX,
            status: 2,
            magnitude: 1.8,
            secondary_magnitude: 0.0,
            max_distance: 0.0,
            position: [0.0; 3],
            direction: [0.0, 1.0, 0.0],
            parameters: [0.25, f32::from_bits(u32::MAX), f32::from_bits(u32::MAX)],
            fade: [0; 4],
        };
        let document = JpaxDocument {
            blocks: vec![
                JpaxBlock::Dynamics(dynamics),
                JpaxBlock::Field(field),
                JpaxBlock::Keyframes(JpaxKeyframeBlock {
                    allocation_size: 0x40,
                    parameter_type: 0,
                    interpolation_type: 0,
                    loop_enabled: 0,
                    reserved_13: 0,
                    keys: vec![[
                        1.0,
                        f32::from_bits(0x7fa0_4567),
                        f32::INFINITY,
                        f32::NEG_INFINITY,
                    ]],
                }),
            ],
            layout: JpaxLayout::Standard,
            trailing_zero_padding: 0,
        };

        let json = serde_json::to_string(&document).unwrap();

        assert!(!json.contains(":null"));
        assert!(json.contains("\"f32_bits\":4294967295"));
        let restored: JpaxDocument = serde_json::from_str(&json).unwrap();
        let JpaxBlock::Dynamics(restored_dynamics) = &restored.blocks[0] else {
            unreachable!()
        };
        assert_eq!(restored_dynamics.scale[1].to_bits(), 0x7fc0_1234);
        assert_eq!(
            restored_dynamics.scale[2].to_bits(),
            f32::INFINITY.to_bits()
        );
        assert_eq!(
            restored_dynamics.spawn_rate.to_bits(),
            f32::NEG_INFINITY.to_bits()
        );
        let JpaxBlock::Keyframes(restored_keys) = &restored.blocks[2] else {
            unreachable!()
        };
        assert_eq!(restored_keys.keys[0][1].to_bits(), 0x7fa0_4567);
        assert_eq!(restored_keys.keys[0][2].to_bits(), f32::INFINITY.to_bits());
        assert_eq!(
            restored_keys.keys[0][3].to_bits(),
            f32::NEG_INFINITY.to_bits()
        );

        let legacy_json = json.replace("{\"f32_bits\":4294967295}", "null");
        let migrated: JpaxDocument = serde_json::from_str(&legacy_json).unwrap();
        let JpaxBlock::Field(migrated_field) = &migrated.blocks[1] else {
            unreachable!()
        };
        assert_eq!(migrated_field.parameters[1].to_bits(), u32::MAX);
        assert_eq!(migrated_field.parameters[2].to_bits(), u32::MAX);
        assert!(!serde_json::to_string(&migrated).unwrap().contains(":null"));
    }
    #[test]
    fn semantic_mutation_changes_rebuilt_jpa_without_source_bytes() {
        let source_document = JpaxDocument {
            blocks: vec![JpaxBlock::Dynamics(zero_dynamics())],
            layout: JpaxLayout::Standard,
            trailing_zero_padding: 0,
        };
        let source = source_document.encode().expect("encode source JPA");
        let mut document = JpaxDocument::parse(source).expect("parse source-free JPA");
        let JpaxBlock::Dynamics(dynamics) = &mut document.blocks[0] else {
            panic!("expected dynamics block");
        };
        dynamics.scale[0] = 2.5;
        let rebuilt = document.encode().expect("encode mutated JPA");
        let reparsed = JpaxDocument::parse(rebuilt).expect("reparse mutated JPA");
        let JpaxBlock::Dynamics(dynamics) = &reparsed.blocks[0] else {
            panic!("expected dynamics block");
        };
        assert_eq!(dynamics.scale[0], 2.5);
    }

    #[test]
    fn authored_noop_effect_is_complete_deterministic_and_source_free() {
        let document = JpaxDocument::authored_noop();
        let first = document.encode().unwrap();
        let second = document.encode().unwrap();
        assert_eq!(first, second);
        assert_eq!(JpaxDocument::parse(&first).unwrap(), document);
        assert!(matches!(document.blocks[0], JpaxBlock::Dynamics(_)));
        assert!(matches!(document.blocks[1], JpaxBlock::BaseShape(_)));
    }

    #[test]
    #[ignore = "requires SMS_BASE_ROOT with extracted retail stage archives"]
    fn source_free_rebuilds_retail_jpa_corpus() {
        let root = std::env::var_os("SMS_BASE_ROOT")
            .map(std::path::PathBuf::from)
            .expect("set SMS_BASE_ROOT to an extracted retail game root");
        let archives = crate::discover_scene_archives(root).expect("discover stage archives");
        let mut rebuilt_count = 0usize;
        let mut failure_patterns = std::collections::BTreeMap::<String, usize>::new();
        let mut failure_examples = std::collections::BTreeMap::<String, String>::new();
        for archive_info in archives {
            let source = std::fs::read(&archive_info.path).expect("read stage archive");
            let decoded = if source.starts_with(b"Yaz0") {
                crate::decode_yaz0(&source).expect("decode stage archive")
            } else {
                source
            };
            let archive = crate::RarcArchive::parse(decoded).expect("parse stage archive");
            for entry in archive.file_entries() {
                if !entry.path.to_ascii_lowercase().ends_with(".jpa") {
                    continue;
                }
                let source = archive
                    .file_bytes_raw(&entry.raw_path)
                    .expect("read JPA entry");
                let path = format!("{}!/{}", archive_info.path.display(), entry.path);
                match JpaxDocument::parse(&source) {
                    Ok(document) => {
                        drop(source);
                        match document.encode() {
                            Ok(rebuilt) => {
                                let original = archive
                                    .file_bytes_raw(&entry.raw_path)
                                    .expect("reread JPA entry");
                                if rebuilt == original {
                                    rebuilt_count += 1;
                                } else {
                                    let pattern = "byte mismatch".to_string();
                                    *failure_patterns.entry(pattern.clone()).or_default() += 1;
                                    failure_examples.entry(pattern).or_insert(path);
                                }
                            }
                            Err(error) => {
                                let pattern = format!("encode: {error}");
                                *failure_patterns.entry(pattern.clone()).or_default() += 1;
                                failure_examples.entry(pattern).or_insert(path);
                            }
                        }
                    }
                    Err(error) => {
                        let pattern = format!("parse: {error}");
                        *failure_patterns.entry(pattern.clone()).or_default() += 1;
                        failure_examples.entry(pattern).or_insert(path);
                    }
                }
            }
        }
        if !failure_patterns.is_empty() {
            let report = failure_patterns
                .iter()
                .map(|(pattern, count)| {
                    format!(
                        "count={count}: {pattern}: {}",
                        failure_examples.get(pattern).expect("failure example")
                    )
                })
                .collect::<Vec<_>>()
                .join("\n");
            panic!("rebuilt {rebuilt_count}/1934 retail JPA files; residue patterns:\n{report}");
        }
        assert_eq!(rebuilt_count, 1934);
        eprintln!("source-free exact JPA rebuild census: {rebuilt_count}/1934");
    }
}
