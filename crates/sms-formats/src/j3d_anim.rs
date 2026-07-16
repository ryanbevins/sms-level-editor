use serde::{Deserialize, Serialize};

use crate::binary::{
    be_f32, be_i16, be_u16, be_u32, checked_slice, read_jut_name_table, require_len, require_magic,
};
use crate::{FormatError, PreserveBytes, Result};

const FORMAT: &str = "J3D animation";
pub const SMS_ANIMATION_FRAMES_PER_SECOND: f32 = 60.0;
const FILE_HEADER_SIZE: usize = 0x20;
const TTK1_HEADER_SIZE: usize = 0x60;
const KEY_TABLE_SIZE: usize = 0x06;
const TRANSFORM_TABLE_SIZE: usize = 0x12;
const JOINT_TRANSFORM_TABLE_SIZE: usize = 0x36;
const TEXTURE_PATTERN_TABLE_SIZE: usize = 0x08;

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct J3dJointTransform {
    pub scale: [f32; 3],
    pub rotation_degrees: [f32; 3],
    pub translation: [f32; 3],
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct J3dJointAnimation {
    pub attribute: u8,
    pub max_frame: u16,
    joints: Vec<JointTracks>,
    bytes: Vec<u8>,
}

impl J3dJointAnimation {
    pub fn parse(bytes: impl AsRef<[u8]>) -> Result<Self> {
        let bytes = bytes.as_ref();
        require_len(FORMAT, bytes, FILE_HEADER_SIZE)?;
        require_magic(FORMAT, bytes, b"J3D1bck1")?;
        let (_declared_size, base, section_end) =
            animation_section(bytes, b"ANK1", "missing ANK1 section")?;
        checked_slice(FORMAT, &bytes[..section_end], base, 0x24)?;

        let attribute = bytes[base + 0x08];
        let angle_scale = bytes[base + 0x09];
        let max_frame = be_u16(bytes, base + 0x0A, FORMAT)?;
        let joint_count = be_u16(bytes, base + 0x0C, FORMAT)? as usize;
        let scale_count = be_u16(bytes, base + 0x0E, FORMAT)? as usize;
        let rotation_count = be_u16(bytes, base + 0x10, FORMAT)? as usize;
        let translation_count = be_u16(bytes, base + 0x12, FORMAT)? as usize;
        let table_offset = section_relative(bytes, base, section_end, 0x14)?;
        checked_slice(
            FORMAT,
            &bytes[..section_end],
            table_offset,
            joint_count
                .checked_mul(JOINT_TRANSFORM_TABLE_SIZE)
                .ok_or_else(|| invalid_offset(table_offset, section_end))?,
        )?;
        let scales = read_f32_block(bytes, base, section_end, 0x18, scale_count)?;
        let rotations = read_i16_block(bytes, base, section_end, 0x1C, rotation_count)?;
        let translations = read_f32_block(bytes, base, section_end, 0x20, translation_count)?;
        let rotation_multiplier = (1_u32 << angle_scale.min(15)) as f32 * (180.0 / 32768.0);

        let mut joints = Vec::with_capacity(joint_count);
        for joint in 0..joint_count {
            let table = read_joint_transform_table(bytes, table_offset, joint)?;
            joints.push(JointTracks {
                scale: decode_track_triplet(table.scale, &scales)?,
                rotation: decode_track_triplet(table.rotation, &rotations)?,
                translation: decode_track_triplet(table.translation, &translations)?,
                rotation_multiplier,
            });
        }

        Ok(Self {
            attribute,
            max_frame,
            joints,
            bytes: bytes.to_vec(),
        })
    }

    pub fn joint_count(&self) -> usize {
        self.joints.len()
    }

    pub fn playback_frame(&self, elapsed_seconds: f32) -> f32 {
        playback_frame(self.attribute, self.max_frame, elapsed_seconds)
    }

    pub fn sample_joint(&self, joint: usize, frame: f32) -> Option<J3dJointTransform> {
        self.joints.get(joint).map(|tracks| tracks.sample(frame))
    }
}

impl PreserveBytes for J3dJointAnimation {
    fn source_bytes(&self) -> &[u8] {
        &self.bytes
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct J3dTexturePatternBinding {
    pub material_name: String,
    pub texture_slot: u8,
    texture_indices: Vec<u16>,
}

impl J3dTexturePatternBinding {
    pub fn texture_index(&self, frame: f32) -> Option<usize> {
        let last = self.texture_indices.len().checked_sub(1)?;
        let index = if frame.is_finite() {
            (frame.max(0.0) as usize).min(last)
        } else {
            0
        };
        Some(self.texture_indices[index] as usize)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct J3dTexturePatternAnimation {
    pub attribute: u8,
    pub max_frame: u16,
    pub bindings: Vec<J3dTexturePatternBinding>,
    bytes: Vec<u8>,
}

impl J3dTexturePatternAnimation {
    pub fn parse(bytes: impl AsRef<[u8]>) -> Result<Self> {
        let bytes = bytes.as_ref();
        require_len(FORMAT, bytes, FILE_HEADER_SIZE)?;
        require_magic(FORMAT, bytes, b"J3D1btp1")?;
        let (_declared_size, base, section_end) =
            animation_section(bytes, b"TPT1", "missing TPT1 section")?;
        checked_slice(FORMAT, &bytes[..section_end], base, 0x20)?;

        let attribute = bytes[base + 0x08];
        let max_frame = be_u16(bytes, base + 0x0A, FORMAT)?;
        let binding_count = be_u16(bytes, base + 0x0C, FORMAT)? as usize;
        let texture_index_count = be_u16(bytes, base + 0x0E, FORMAT)? as usize;
        let table_offset = section_relative(bytes, base, section_end, 0x10)?;
        checked_slice(
            FORMAT,
            &bytes[..section_end],
            table_offset,
            binding_count
                .checked_mul(TEXTURE_PATTERN_TABLE_SIZE)
                .ok_or_else(|| invalid_offset(table_offset, section_end))?,
        )?;
        let values_offset = section_relative(bytes, base, section_end, 0x14)?;
        let values = read_u16_block_at(bytes, values_offset, section_end, texture_index_count)?;
        let name_offset = section_relative(bytes, base, section_end, 0x1C)?;
        let names = read_jut_name_table(bytes, name_offset, section_end)?;
        if names.len() != binding_count {
            return Err(FormatError::Unsupported {
                format: FORMAT,
                message: format!(
                    "texture pattern has {binding_count} binding(s) but {} material name(s)",
                    names.len()
                ),
            });
        }

        let mut bindings = Vec::with_capacity(binding_count);
        for (index, material_name) in names.into_iter().enumerate() {
            let table = table_offset + index * TEXTURE_PATTERN_TABLE_SIZE;
            let frame_count = be_u16(bytes, table, FORMAT)? as usize;
            let first_value = be_u16(bytes, table + 2, FORMAT)? as usize;
            let end = first_value
                .checked_add(frame_count)
                .ok_or_else(|| invalid_offset(first_value, values.len()))?;
            if end > values.len() {
                return Err(invalid_offset(end, values.len()));
            }
            bindings.push(J3dTexturePatternBinding {
                material_name,
                texture_slot: bytes[table + 4],
                texture_indices: values[first_value..end].to_vec(),
            });
        }

        Ok(Self {
            attribute,
            max_frame,
            bindings,
            bytes: bytes.to_vec(),
        })
    }

    pub fn playback_frame(&self, elapsed_seconds: f32) -> f32 {
        playback_frame(self.attribute, self.max_frame, elapsed_seconds)
    }
}

impl PreserveBytes for J3dTexturePatternAnimation {
    fn source_bytes(&self) -> &[u8] {
        &self.bytes
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct JointTracks {
    scale: [KeyTrack; 3],
    rotation: [KeyTrack; 3],
    translation: [KeyTrack; 3],
    rotation_multiplier: f32,
}

impl JointTracks {
    fn sample(&self, frame: f32) -> J3dJointTransform {
        J3dJointTransform {
            scale: std::array::from_fn(|axis| self.scale[axis].sample_or(frame, 1.0)),
            rotation_degrees: std::array::from_fn(|axis| {
                self.rotation[axis].sample_or(frame, 0.0) * self.rotation_multiplier
            }),
            translation: std::array::from_fn(|axis| self.translation[axis].sample_or(frame, 0.0)),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct J3dTextureSrt {
    pub scale: [f32; 2],
    pub rotation: i16,
    pub translation: [f32; 2],
}

impl Default for J3dTextureSrt {
    fn default() -> Self {
        Self {
            scale: [1.0, 1.0],
            rotation: 0,
            translation: [0.0, 0.0],
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct J3dTextureSrtBinding {
    pub material_name: String,
    pub texture_matrix_index: u8,
    pub center: [f32; 3],
    scale_x: KeyTrack,
    scale_y: KeyTrack,
    rotation: KeyTrack,
    translation_x: KeyTrack,
    translation_y: KeyTrack,
    rotation_shift: u8,
}

impl J3dTextureSrtBinding {
    pub fn sample(&self, frame: f32) -> J3dTextureSrt {
        let rotation =
            (self.rotation.sample(frame) as i32).wrapping_shl(self.rotation_shift.into());
        J3dTextureSrt {
            scale: [
                self.scale_x.sample_or(frame, 1.0),
                self.scale_y.sample_or(frame, 1.0),
            ],
            rotation: rotation as i16,
            translation: [
                self.translation_x.sample_or(frame, 0.0),
                self.translation_y.sample_or(frame, 0.0),
            ],
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct J3dTextureSrtAnimation {
    pub attribute: u8,
    pub max_frame: u16,
    pub bindings: Vec<J3dTextureSrtBinding>,
    bytes: Vec<u8>,
}

impl J3dTextureSrtAnimation {
    pub fn parse(bytes: impl AsRef<[u8]>) -> Result<Self> {
        let bytes = bytes.as_ref();
        require_len(FORMAT, bytes, FILE_HEADER_SIZE)?;
        require_magic(FORMAT, bytes, b"J3D1btk1")?;
        let (_declared_size, base, section_end) =
            animation_section(bytes, b"TTK1", "missing TTK1 section")?;
        checked_slice(FORMAT, &bytes[..section_end], base, TTK1_HEADER_SIZE)?;

        let attribute = bytes[base + 0x08];
        let rotation_shift = bytes[base + 0x09];
        let max_frame = be_u16(bytes, base + 0x0A, FORMAT)?;
        let track_count = be_u16(bytes, base + 0x0C, FORMAT)? as usize;
        if !track_count.is_multiple_of(3) {
            return Err(FormatError::Unsupported {
                format: FORMAT,
                message: format!("texture SRT track count {track_count} is not divisible by 3"),
            });
        }
        let binding_count = track_count / 3;
        let scale_count = be_u16(bytes, base + 0x0E, FORMAT)? as usize;
        let rotation_count = be_u16(bytes, base + 0x10, FORMAT)? as usize;
        let translation_count = be_u16(bytes, base + 0x12, FORMAT)? as usize;

        let table_offset = section_relative(bytes, base, section_end, 0x14)?;
        checked_slice(
            FORMAT,
            &bytes[..section_end],
            table_offset,
            track_count
                .checked_mul(TRANSFORM_TABLE_SIZE)
                .ok_or_else(|| invalid_offset(table_offset, section_end))?,
        )?;
        let name_offset = section_relative(bytes, base, section_end, 0x1C)?;
        let names = read_jut_name_table(bytes, name_offset, section_end)?;
        if names.len() != binding_count {
            return Err(FormatError::Unsupported {
                format: FORMAT,
                message: format!(
                    "texture SRT has {binding_count} binding(s) but {} material name(s)",
                    names.len()
                ),
            });
        }
        let tex_mtx_id_offset = section_relative(bytes, base, section_end, 0x20)?;
        let tex_mtx_ids = checked_slice(
            FORMAT,
            &bytes[..section_end],
            tex_mtx_id_offset,
            binding_count,
        )?;
        let center_offset = section_relative(bytes, base, section_end, 0x24)?;
        checked_slice(
            FORMAT,
            &bytes[..section_end],
            center_offset,
            binding_count
                .checked_mul(12)
                .ok_or_else(|| invalid_offset(center_offset, section_end))?,
        )?;

        let scale_data = read_f32_block(bytes, base, section_end, 0x28, scale_count)?;
        let rotation_data = read_i16_block(bytes, base, section_end, 0x2C, rotation_count)?;
        let translation_data = read_f32_block(bytes, base, section_end, 0x30, translation_count)?;

        let mut bindings = Vec::with_capacity(binding_count);
        for binding_index in 0..binding_count {
            let x_table = read_transform_table(bytes, table_offset, binding_index * 3)?;
            let y_table = read_transform_table(bytes, table_offset, binding_index * 3 + 1)?;
            let rotation_table = read_transform_table(bytes, table_offset, binding_index * 3 + 2)?;
            bindings.push(J3dTextureSrtBinding {
                material_name: names[binding_index].clone(),
                texture_matrix_index: tex_mtx_ids[binding_index],
                center: [
                    be_f32(bytes, center_offset + binding_index * 12, FORMAT)?,
                    be_f32(bytes, center_offset + binding_index * 12 + 4, FORMAT)?,
                    be_f32(bytes, center_offset + binding_index * 12 + 8, FORMAT)?,
                ],
                scale_x: decode_track(x_table.scale, &scale_data)?,
                scale_y: decode_track(y_table.scale, &scale_data)?,
                rotation: decode_track(rotation_table.rotation, &rotation_data)?,
                translation_x: decode_track(x_table.translation, &translation_data)?,
                translation_y: decode_track(y_table.translation, &translation_data)?,
                rotation_shift,
            });
        }

        Ok(Self {
            attribute,
            max_frame,
            bindings,
            bytes: bytes.to_vec(),
        })
    }

    pub fn playback_frame(&self, elapsed_seconds: f32) -> f32 {
        playback_frame(self.attribute, self.max_frame, elapsed_seconds)
    }
}

fn playback_frame(attribute: u8, max_frame: u16, elapsed_seconds: f32) -> f32 {
    let end = max_frame as f32;
    if !elapsed_seconds.is_finite() || end <= 0.0 {
        return 0.0;
    }
    let frame = elapsed_seconds.max(0.0) * SMS_ANIMATION_FRAMES_PER_SECOND;
    match attribute {
        0 => frame.min((end - 0.001).max(0.0)),
        1 => {
            if frame < end {
                frame
            } else {
                0.0
            }
        }
        2 => frame.rem_euclid(end),
        3 => {
            if frame < end {
                frame
            } else if frame < end * 2.0 {
                end * 2.0 - frame - 0.001
            } else {
                0.0
            }
        }
        4 => {
            let phase = frame.rem_euclid(end * 2.0);
            if phase < end {
                phase
            } else {
                end * 2.0 - phase - 0.001
            }
        }
        _ => frame.min((end - 0.001).max(0.0)),
    }
}

impl PreserveBytes for J3dTextureSrtAnimation {
    fn source_bytes(&self) -> &[u8] {
        &self.bytes
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct KeyTrack {
    keys: Vec<KeyFrame>,
}

impl KeyTrack {
    fn sample_or(&self, frame: f32, default: f32) -> f32 {
        if self.keys.is_empty() {
            default
        } else {
            self.sample(frame)
        }
    }

    fn sample(&self, frame: f32) -> f32 {
        let Some(first) = self.keys.first() else {
            return 0.0;
        };
        if self.keys.len() == 1 || frame <= first.time {
            return first.value;
        }
        let last = self.keys.last().expect("track has a first key");
        if frame >= last.time {
            return last.value;
        }
        let upper = self.keys.partition_point(|key| key.time <= frame);
        let left = &self.keys[upper - 1];
        let right = &self.keys[upper];
        hermite(frame, left, right)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
struct KeyFrame {
    time: f32,
    value: f32,
    tangent_in: f32,
    tangent_out: f32,
}

#[derive(Clone, Copy)]
struct KeyTable {
    count: usize,
    offset: usize,
    tangent_type: u16,
}

struct TransformTable {
    scale: KeyTable,
    rotation: KeyTable,
    translation: KeyTable,
}

struct JointTransformTable {
    scale: [KeyTable; 3],
    rotation: [KeyTable; 3],
    translation: [KeyTable; 3],
}

fn read_joint_transform_table(
    bytes: &[u8],
    base: usize,
    index: usize,
) -> Result<JointTransformTable> {
    let offset = base
        .checked_add(index * JOINT_TRANSFORM_TABLE_SIZE)
        .ok_or_else(|| invalid_offset(base, bytes.len()))?;
    let mut scale = [read_key_table(bytes, offset)?; 3];
    let mut rotation = scale;
    let mut translation = scale;
    for axis in 0..3 {
        let axis_offset = offset + axis * TRANSFORM_TABLE_SIZE;
        scale[axis] = read_key_table(bytes, axis_offset)?;
        rotation[axis] = read_key_table(bytes, axis_offset + KEY_TABLE_SIZE)?;
        translation[axis] = read_key_table(bytes, axis_offset + KEY_TABLE_SIZE * 2)?;
    }
    Ok(JointTransformTable {
        scale,
        rotation,
        translation,
    })
}

fn read_transform_table(bytes: &[u8], base: usize, index: usize) -> Result<TransformTable> {
    let offset = base
        .checked_add(index * TRANSFORM_TABLE_SIZE)
        .ok_or_else(|| invalid_offset(base, bytes.len()))?;
    Ok(TransformTable {
        scale: read_key_table(bytes, offset)?,
        rotation: read_key_table(bytes, offset + KEY_TABLE_SIZE)?,
        translation: read_key_table(bytes, offset + KEY_TABLE_SIZE * 2)?,
    })
}

fn read_key_table(bytes: &[u8], offset: usize) -> Result<KeyTable> {
    Ok(KeyTable {
        count: be_u16(bytes, offset, FORMAT)? as usize,
        offset: be_u16(bytes, offset + 2, FORMAT)? as usize,
        tangent_type: be_u16(bytes, offset + 4, FORMAT)?,
    })
}

fn decode_track<T: Copy + Into<f32>>(table: KeyTable, data: &[T]) -> Result<KeyTrack> {
    if table.count == 0 {
        return Ok(KeyTrack { keys: Vec::new() });
    }
    let stride = match table.count {
        1 => 1,
        _ if table.tangent_type == 0 => 3,
        _ if table.tangent_type == 1 => 4,
        _ => {
            return Err(FormatError::Unsupported {
                format: FORMAT,
                message: format!("unsupported key tangent type {}", table.tangent_type),
            })
        }
    };
    let required = table
        .offset
        .checked_add(table.count * stride)
        .ok_or_else(|| invalid_offset(table.offset, data.len()))?;
    if required > data.len() {
        return Err(invalid_offset(required, data.len()));
    }
    if table.count == 1 {
        return Ok(KeyTrack {
            keys: vec![KeyFrame {
                time: 0.0,
                value: data[table.offset].into(),
                tangent_in: 0.0,
                tangent_out: 0.0,
            }],
        });
    }
    let mut keys = Vec::with_capacity(table.count);
    for index in 0..table.count {
        let offset = table.offset + index * stride;
        let tangent_in = data[offset + 2].into();
        keys.push(KeyFrame {
            time: data[offset].into(),
            value: data[offset + 1].into(),
            tangent_in,
            tangent_out: if stride == 4 {
                data[offset + 3].into()
            } else {
                tangent_in
            },
        });
    }
    Ok(KeyTrack { keys })
}

fn decode_track_triplet<T: Copy + Into<f32>>(
    tables: [KeyTable; 3],
    data: &[T],
) -> Result<[KeyTrack; 3]> {
    Ok([
        decode_track(tables[0], data)?,
        decode_track(tables[1], data)?,
        decode_track(tables[2], data)?,
    ])
}

fn hermite(frame: f32, left: &KeyFrame, right: &KeyFrame) -> f32 {
    let duration = right.time - left.time;
    if duration <= f32::EPSILON {
        return right.value;
    }
    let t = ((frame - left.time) / duration).clamp(0.0, 1.0);
    let t2 = t * t;
    let t3 = t2 * t;
    let h00 = 2.0 * t3 - 3.0 * t2 + 1.0;
    let h10 = t3 - 2.0 * t2 + t;
    let h01 = -2.0 * t3 + 3.0 * t2;
    let h11 = t3 - t2;
    h00 * left.value
        + h10 * duration * left.tangent_out
        + h01 * right.value
        + h11 * duration * right.tangent_in
}

fn read_f32_array(bytes: &[u8], offset: usize, count: usize, limit: usize) -> Result<Vec<f32>> {
    let length = count
        .checked_mul(4)
        .ok_or_else(|| invalid_offset(offset, limit))?;
    if offset.checked_add(length).is_none_or(|end| end > limit) {
        return Err(invalid_offset(offset.saturating_add(length), limit));
    }
    (0..count)
        .map(|index| be_f32(bytes, offset + index * 4, FORMAT))
        .collect()
}

fn read_f32_block(
    bytes: &[u8],
    base: usize,
    limit: usize,
    field: usize,
    count: usize,
) -> Result<Vec<f32>> {
    if count == 0 {
        return Ok(Vec::new());
    }
    let offset = section_relative(bytes, base, limit, field)?;
    read_f32_array(bytes, offset, count, limit)
}

fn read_i16_array(bytes: &[u8], offset: usize, count: usize, limit: usize) -> Result<Vec<i16>> {
    let length = count
        .checked_mul(2)
        .ok_or_else(|| invalid_offset(offset, limit))?;
    if offset.checked_add(length).is_none_or(|end| end > limit) {
        return Err(invalid_offset(offset.saturating_add(length), limit));
    }
    (0..count)
        .map(|index| be_i16(bytes, offset + index * 2, FORMAT))
        .collect()
}

fn read_u16_block_at(bytes: &[u8], offset: usize, limit: usize, count: usize) -> Result<Vec<u16>> {
    let length = count
        .checked_mul(2)
        .ok_or_else(|| invalid_offset(offset, limit))?;
    if offset.checked_add(length).is_none_or(|end| end > limit) {
        return Err(invalid_offset(offset.saturating_add(length), limit));
    }
    (0..count)
        .map(|index| be_u16(bytes, offset + index * 2, FORMAT))
        .collect()
}

fn read_i16_block(
    bytes: &[u8],
    base: usize,
    limit: usize,
    field: usize,
    count: usize,
) -> Result<Vec<i16>> {
    if count == 0 {
        return Ok(Vec::new());
    }
    let offset = section_relative(bytes, base, limit, field)?;
    read_i16_array(bytes, offset, count, limit)
}

fn animation_section(
    bytes: &[u8],
    section_magic: &[u8; 4],
    missing_message: &str,
) -> Result<(usize, usize, usize)> {
    let declared_size = be_u32(bytes, 0x08, FORMAT)? as usize;
    if declared_size > bytes.len() || declared_size < FILE_HEADER_SIZE {
        return Err(invalid_offset(declared_size, bytes.len()));
    }

    let file = &bytes[..declared_size];
    let section_count = be_u32(file, 0x0C, FORMAT)? as usize;
    let mut section_offset = FILE_HEADER_SIZE;
    let mut found = None;
    for section_index in 0..section_count {
        let header = checked_slice(FORMAT, file, section_offset, 8)?;
        let size = be_u32(file, section_offset + 4, FORMAT)? as usize;
        if size < 8 {
            return Err(invalid_offset(section_offset, declared_size));
        }
        let claimed_end = section_offset
            .checked_add(size)
            .ok_or_else(|| invalid_offset(section_offset, declared_size))?;
        let section_end = if claimed_end > declared_size {
            if section_index + 1 != section_count {
                return Err(invalid_offset(claimed_end, declared_size));
            }
            // Some retail J3D animations include absent trailing padding in
            // the final block's size. J3D uses that size only to find another
            // block, so cap the last block at the real file boundary while
            // keeping every table and data read strictly bounds-checked.
            declared_size
        } else {
            claimed_end
        };
        if &header[..4] == section_magic {
            found = Some((section_offset, section_end));
        }
        section_offset = section_end;
    }

    let (section_offset, section_end) = found.ok_or_else(|| FormatError::Unsupported {
        format: FORMAT,
        message: missing_message.to_string(),
    })?;
    Ok((declared_size, section_offset, section_end))
}

fn section_relative(bytes: &[u8], base: usize, limit: usize, field: usize) -> Result<usize> {
    let relative = be_u32(bytes, base + field, FORMAT)? as usize;
    let offset = base
        .checked_add(relative)
        .ok_or_else(|| invalid_offset(relative, limit))?;
    if relative == 0 || offset >= limit {
        return Err(invalid_offset(offset, limit));
    }
    Ok(offset)
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

    fn test_btp() -> Vec<u8> {
        const FILE_SIZE: usize = 0x80;
        const BLOCK: usize = 0x20;
        let mut bytes = vec![0_u8; FILE_SIZE];
        bytes[..8].copy_from_slice(b"J3D1btp1");
        put_u32(&mut bytes, 0x08, FILE_SIZE as u32);
        put_u32(&mut bytes, 0x0C, 1);
        bytes[BLOCK..BLOCK + 4].copy_from_slice(b"TPT1");
        put_u32(&mut bytes, BLOCK + 0x04, (FILE_SIZE - BLOCK) as u32);
        bytes[BLOCK + 0x08] = 2;
        put_u16(&mut bytes, BLOCK + 0x0A, 3);
        put_u16(&mut bytes, BLOCK + 0x0C, 1);
        put_u16(&mut bytes, BLOCK + 0x0E, 3);
        for (field, relative) in [(0x10, 0x20), (0x14, 0x28), (0x18, 0x30), (0x1C, 0x38)] {
            put_u32(&mut bytes, BLOCK + field, relative);
        }
        put_u16(&mut bytes, BLOCK + 0x20, 3);
        put_u16(&mut bytes, BLOCK + 0x22, 0);
        bytes[BLOCK + 0x24] = 0;
        for (index, value) in [4_u16, 7, 9].into_iter().enumerate() {
            put_u16(&mut bytes, BLOCK + 0x28 + index * 2, value);
        }
        put_u16(&mut bytes, BLOCK + 0x30, 0);
        let names = BLOCK + 0x38;
        put_u16(&mut bytes, names, 1);
        put_u16(&mut bytes, names + 2, 0xFFFF);
        put_u16(&mut bytes, names + 6, 0x000C);
        bytes[names + 0x0C..names + 0x15].copy_from_slice(b"_eye_mat\0");
        bytes
    }

    fn test_btk() -> Vec<u8> {
        const FILE_SIZE: usize = 0x100;
        const BLOCK: usize = 0x20;
        let mut bytes = vec![0_u8; FILE_SIZE];
        bytes[..8].copy_from_slice(b"J3D1btk1");
        put_u32(&mut bytes, 0x08, FILE_SIZE as u32);
        put_u32(&mut bytes, 0x0C, 1);
        bytes[0x10..0x14].copy_from_slice(b"SVR1");
        bytes[BLOCK..BLOCK + 4].copy_from_slice(b"TTK1");
        put_u32(&mut bytes, BLOCK + 0x04, 0xE0);
        bytes[BLOCK + 0x08] = 2;
        put_u16(&mut bytes, BLOCK + 0x0A, 60);
        put_u16(&mut bytes, BLOCK + 0x0C, 3);
        put_u16(&mut bytes, BLOCK + 0x0E, 1);
        put_u16(&mut bytes, BLOCK + 0x10, 1);
        put_u16(&mut bytes, BLOCK + 0x12, 6);
        for (field, relative) in [
            (0x14, 0x60),
            (0x18, 0x96),
            (0x1C, 0x98),
            (0x20, 0xAA),
            (0x24, 0xAC),
            (0x28, 0xB8),
            (0x2C, 0xBC),
            (0x30, 0xC0),
        ] {
            put_u32(&mut bytes, BLOCK + field, relative);
        }

        let table = BLOCK + 0x60;
        put_key_table(&mut bytes, table, 1, 0, 0);
        put_key_table(&mut bytes, table + 0x0C, 2, 0, 0);
        put_key_table(&mut bytes, table + 0x12, 1, 0, 0);
        put_key_table(&mut bytes, table + 0x24 + 0x06, 1, 0, 0);

        let names = BLOCK + 0x98;
        put_u16(&mut bytes, names, 1);
        put_u16(&mut bytes, names + 2, 0xFFFF);
        put_u16(&mut bytes, names + 6, 0x000C);
        bytes[names + 0x0C..names + 0x12].copy_from_slice(b"water\0");
        bytes[BLOCK + 0xAA] = 0;
        put_f32(&mut bytes, BLOCK + 0xAC, 0.5);
        put_f32(&mut bytes, BLOCK + 0xB0, 0.5);
        put_f32(&mut bytes, BLOCK + 0xB4, 0.0);
        put_f32(&mut bytes, BLOCK + 0xB8, 1.0);
        put_i16(&mut bytes, BLOCK + 0xBC, 0);
        for (index, value) in [0.0, 0.0, 0.0, 60.0, 1.0, 0.0].into_iter().enumerate() {
            put_f32(&mut bytes, BLOCK + 0xC0 + index * 4, value);
        }
        bytes
    }

    fn test_bck() -> Vec<u8> {
        const FILE_SIZE: usize = 0xC0;
        const BLOCK: usize = 0x20;
        let mut bytes = vec![0_u8; FILE_SIZE];
        bytes[..8].copy_from_slice(b"J3D1bck1");
        put_u32(&mut bytes, 0x08, FILE_SIZE as u32);
        put_u32(&mut bytes, 0x0C, 1);
        bytes[BLOCK..BLOCK + 4].copy_from_slice(b"ANK1");
        put_u32(&mut bytes, BLOCK + 0x04, (FILE_SIZE - BLOCK) as u32);
        bytes[BLOCK + 0x08] = 2;
        bytes[BLOCK + 0x09] = 0;
        put_u16(&mut bytes, BLOCK + 0x0A, 60);
        put_u16(&mut bytes, BLOCK + 0x0C, 1);
        put_u16(&mut bytes, BLOCK + 0x0E, 3);
        put_u16(&mut bytes, BLOCK + 0x10, 3);
        put_u16(&mut bytes, BLOCK + 0x12, 3);
        for (field, relative) in [(0x14, 0x24), (0x18, 0x60), (0x1C, 0x6C), (0x20, 0x74)] {
            put_u32(&mut bytes, BLOCK + field, relative);
        }
        for axis in 0..3 {
            let table = BLOCK + 0x24 + axis * TRANSFORM_TABLE_SIZE;
            put_key_table(&mut bytes, table, 1, axis as u16, 0);
            put_key_table(&mut bytes, table + KEY_TABLE_SIZE, 1, axis as u16, 0);
            put_key_table(&mut bytes, table + KEY_TABLE_SIZE * 2, 1, axis as u16, 0);
        }
        for (axis, value) in [1.0, 2.0, 3.0].into_iter().enumerate() {
            put_f32(&mut bytes, BLOCK + 0x60 + axis * 4, value);
        }
        for (axis, value) in [0_i16, 0x2000, -0x2000].into_iter().enumerate() {
            put_i16(&mut bytes, BLOCK + 0x6C + axis * 2, value);
        }
        for (axis, value) in [10.0, 20.0, 30.0].into_iter().enumerate() {
            put_f32(&mut bytes, BLOCK + 0x74 + axis * 4, value);
        }
        bytes
    }

    fn put_key_table(
        bytes: &mut [u8],
        offset: usize,
        count: u16,
        value_offset: u16,
        tangent_type: u16,
    ) {
        put_u16(bytes, offset, count);
        put_u16(bytes, offset + 2, value_offset);
        put_u16(bytes, offset + 4, tangent_type);
    }

    fn put_u16(bytes: &mut [u8], offset: usize, value: u16) {
        bytes[offset..offset + 2].copy_from_slice(&value.to_be_bytes());
    }

    fn put_i16(bytes: &mut [u8], offset: usize, value: i16) {
        bytes[offset..offset + 2].copy_from_slice(&value.to_be_bytes());
    }

    fn put_u32(bytes: &mut [u8], offset: usize, value: u32) {
        bytes[offset..offset + 4].copy_from_slice(&value.to_be_bytes());
    }

    fn put_f32(bytes: &mut [u8], offset: usize, value: f32) {
        put_u32(bytes, offset, value.to_bits());
    }

    #[test]
    fn parses_and_preserves_texture_srt_animation() {
        let bytes = test_btk();
        let animation = J3dTextureSrtAnimation::parse(&bytes).unwrap();

        assert_eq!(animation.attribute, 2);
        assert_eq!(animation.max_frame, 60);
        assert_eq!(animation.bindings.len(), 1);
        assert_eq!(animation.bindings[0].material_name, "water");
        assert_eq!(animation.bindings[0].center, [0.5, 0.5, 0.0]);
        assert!((animation.bindings[0].sample(30.0).translation[0] - 0.5).abs() < 0.0001);
        assert_eq!(animation.to_bytes(), bytes);
    }

    #[test]
    fn parses_and_samples_texture_pattern_animation() {
        let bytes = test_btp();
        let animation = J3dTexturePatternAnimation::parse(&bytes).unwrap();

        assert_eq!(animation.bindings.len(), 1);
        assert_eq!(animation.bindings[0].material_name, "_eye_mat");
        assert_eq!(animation.bindings[0].texture_slot, 0);
        assert_eq!(animation.bindings[0].texture_index(0.0), Some(4));
        assert_eq!(animation.bindings[0].texture_index(1.0), Some(7));
        assert_eq!(animation.bindings[0].texture_index(99.0), Some(9));
        assert_eq!(animation.to_bytes(), bytes);
    }

    #[test]
    fn parses_and_samples_joint_animation() {
        let bytes = test_bck();
        let animation = J3dJointAnimation::parse(&bytes).unwrap();

        assert_eq!(animation.max_frame, 60);
        assert_eq!(animation.joint_count(), 1);
        assert_eq!(animation.playback_frame(1.25), 15.0);
        let joint = animation.sample_joint(0, 15.0).unwrap();
        assert_eq!(joint.scale, [1.0, 2.0, 3.0]);
        assert_eq!(joint.rotation_degrees, [0.0, 45.0, -45.0]);
        assert_eq!(joint.translation, [10.0, 20.0, 30.0]);
        assert_eq!(animation.to_bytes(), bytes);
    }

    #[test]
    fn rejects_texture_srt_tables_outside_the_section() {
        let mut bytes = test_btk();
        put_u32(&mut bytes, 0x20 + 0x14, 0xF0);
        let oversized = (bytes.len() - FILE_HEADER_SIZE + 4) as u32;
        put_u32(&mut bytes, FILE_HEADER_SIZE + 4, oversized);

        assert!(matches!(
            J3dTextureSrtAnimation::parse(bytes),
            Err(FormatError::InvalidOffset { .. })
        ));
    }

    #[test]
    fn accepts_absent_padding_claimed_by_final_animation_section() {
        let mut bck = test_bck();
        let bck_section_size = (bck.len() - FILE_HEADER_SIZE + 4) as u32;
        put_u32(&mut bck, FILE_HEADER_SIZE + 4, bck_section_size);
        assert_eq!(J3dJointAnimation::parse(&bck).unwrap().to_bytes(), bck);

        let mut btp = test_btp();
        let btp_section_size = (btp.len() - FILE_HEADER_SIZE + 4) as u32;
        put_u32(&mut btp, FILE_HEADER_SIZE + 4, btp_section_size);
        assert_eq!(
            J3dTexturePatternAnimation::parse(&btp).unwrap().to_bytes(),
            btp
        );

        let mut btk = test_btk();
        let btk_section_size = (btk.len() - FILE_HEADER_SIZE + 4) as u32;
        put_u32(&mut btk, FILE_HEADER_SIZE + 4, btk_section_size);
        assert_eq!(J3dTextureSrtAnimation::parse(&btk).unwrap().to_bytes(), btk);
    }

    #[test]
    fn preserve_bytes_includes_data_after_the_declared_animation_size() {
        let mut bck = test_bck();
        bck.extend_from_slice(b"bck tail");
        assert_eq!(J3dJointAnimation::parse(&bck).unwrap().to_bytes(), bck);

        let mut btp = test_btp();
        btp.extend_from_slice(b"btp tail");
        assert_eq!(
            J3dTexturePatternAnimation::parse(&btp).unwrap().to_bytes(),
            btp
        );

        let mut btk = test_btk();
        btk.extend_from_slice(b"btk tail");
        assert_eq!(J3dTextureSrtAnimation::parse(&btk).unwrap().to_bytes(), btk);
    }

    #[test]
    fn rejects_oversized_animation_section_before_another_section() {
        let mut bytes = test_btk();
        put_u32(&mut bytes, 0x0C, 2);
        let oversized = (bytes.len() - FILE_HEADER_SIZE + 4) as u32;
        put_u32(&mut bytes, FILE_HEADER_SIZE + 4, oversized);

        assert!(matches!(
            J3dTextureSrtAnimation::parse(bytes),
            Err(FormatError::InvalidOffset { .. })
        ));
    }

    #[test]
    fn hermite_tracks_match_j3d_tangent_layouts() {
        let track = decode_track(
            KeyTable {
                count: 2,
                offset: 0,
                tangent_type: 0,
            },
            &[0.0_f32, 2.0, 0.5, 10.0, 8.0, 0.5],
        )
        .unwrap();
        assert_eq!(track.sample(0.0), 2.0);
        assert_eq!(track.sample(10.0), 8.0);
        assert!((track.sample(5.0) - 5.0).abs() < 0.0001);
    }

    #[test]
    fn loop_playback_uses_sunshine_animation_frame_rate() {
        let animation = J3dTextureSrtAnimation {
            attribute: 2,
            max_frame: 1200,
            bindings: Vec::new(),
            bytes: Vec::new(),
        };
        assert_eq!(animation.playback_frame(1.0), 60.0);
        assert_eq!(animation.playback_frame(20.0), 0.0);
    }
}
