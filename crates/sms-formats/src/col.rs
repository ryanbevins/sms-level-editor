use serde::{Deserialize, Serialize};

use crate::binary::{be_f32, be_i16, be_u16, be_u32, checked_slice, require_len};
use crate::{FormatError, Result};

const FORMAT: &str = "COL";
const HEADER_SIZE: usize = 0x10;
const GROUP_SIZE: usize = 0x18;
const VERTEX_SIZE: usize = 0x0C;
const TRIANGLE_INDEX_SIZE: usize = 0x06;
const GROUP_RESERVED_VALUE: i16 = -1;
const GROUP_HAS_PER_TRIANGLE_DATA: u16 = 0x0001;

/// The four-word header read by `TMapCollisionBase::init`.
///
/// `triangle_count_or_flags` is retained as a public field name for API
/// compatibility. The decomp shows that it is the vertex count, not a
/// triangle count or a flags word; prefer [`ColHeader::vertex_count`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ColHeader {
    pub triangle_count_or_flags: u32,
    pub vertex_offset: u32,
    pub group_count: u32,
    pub group_offset: u32,
}

impl ColHeader {
    pub fn vertex_count(&self) -> u32 {
        self.triangle_count_or_flags
    }
}

/// One position in the collision mesh.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct ColVertex {
    pub position: [f32; 3],
}

impl ColVertex {
    pub const fn new(x: f32, y: f32, z: f32) -> Self {
        Self {
            position: [x, y, z],
        }
    }
}

impl From<[f32; 3]> for ColVertex {
    fn from(position: [f32; 3]) -> Self {
        Self { position }
    }
}

/// A collision triangle and the two byte-sized attributes copied into
/// `TBGCheckData::unk6` and `TBGCheckData::unk7` by the retail game.
///
/// The neighboring decomp has not assigned behavioral names to those two
/// attributes yet, so the format layer exposes them without guessing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ColTriangle {
    pub vertex_indices: [u16; 3],
    pub attribute_0: u8,
    pub attribute_1: u8,
    /// Per-triangle `TBGCheckData::mData`. This is present only when the
    /// containing group's `has_per_triangle_data` flag is true.
    pub data: Option<i16>,
}

/// Triangles sharing one SMS background/surface type.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ColGroup {
    pub surface_type: u16,
    pub has_per_triangle_data: bool,
    pub triangles: Vec<ColTriangle>,
}

/// A source-free semantic SMS collision document.
///
/// This type intentionally stores no original file buffer, opaque payload, or
/// parsed offsets. Encoding reconstructs the canonical retail layout entirely
/// from `vertices` and `groups`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ColFile {
    vertices: Vec<ColVertex>,
    groups: Vec<ColGroup>,
}

impl ColFile {
    pub fn new(vertices: Vec<ColVertex>, groups: Vec<ColGroup>) -> Self {
        Self { vertices, groups }
    }

    pub fn parse(bytes: impl AsRef<[u8]>) -> Result<Self> {
        let bytes = bytes.as_ref();
        require_len(FORMAT, bytes, HEADER_SIZE)?;

        let header = ColHeader {
            triangle_count_or_flags: be_u32(bytes, 0x00, FORMAT)?,
            vertex_offset: be_u32(bytes, 0x04, FORMAT)?,
            group_count: be_u32(bytes, 0x08, FORMAT)?,
            group_offset: be_u32(bytes, 0x0C, FORMAT)?,
        };
        let vertex_count = header.vertex_count() as usize;
        let group_count = header.group_count as usize;
        let vertex_offset = header.vertex_offset as usize;
        let group_offset = header.group_offset as usize;

        validate_counted_span(bytes, vertex_offset, vertex_count, VERTEX_SIZE)?;
        validate_counted_span(bytes, group_offset, group_count, GROUP_SIZE)?;

        let mut descriptors = Vec::with_capacity(group_count);
        for group_index in 0..group_count {
            let offset = checked_add(
                group_offset,
                checked_mul(group_index, GROUP_SIZE, bytes.len())?,
                bytes.len(),
            )?;
            let triangle_count = be_i16(bytes, offset + 0x02, FORMAT)?;
            if triangle_count < 0 {
                return Err(unsupported(format!(
                    "group {group_index} has negative triangle count {triangle_count}"
                )));
            }
            let flags = be_u16(bytes, offset + 0x04, FORMAT)?;
            if flags & !GROUP_HAS_PER_TRIANGLE_DATA != 0 {
                return Err(unsupported(format!(
                    "group {group_index} uses unknown on-disk flags {flags:#06x}"
                )));
            }
            let reserved = be_i16(bytes, offset + 0x06, FORMAT)?;
            if reserved != GROUP_RESERVED_VALUE {
                return Err(unsupported(format!(
                    "group {group_index} reserved value is {reserved:#06x}, expected -1"
                )));
            }

            descriptors.push(GroupDescriptor {
                surface_type: be_u16(bytes, offset, FORMAT)?,
                triangle_count: triangle_count as usize,
                has_per_triangle_data: flags & GROUP_HAS_PER_TRIANGLE_DATA != 0,
                triangle_indices_offset: be_u32(bytes, offset + 0x08, FORMAT)? as usize,
                attribute_0_offset: be_u32(bytes, offset + 0x0C, FORMAT)? as usize,
                attribute_1_offset: be_u32(bytes, offset + 0x10, FORMAT)? as usize,
                triangle_data_offset: be_u32(bytes, offset + 0x14, FORMAT)? as usize,
            });
        }

        let group_shapes = descriptors
            .iter()
            .map(|descriptor| GroupShape {
                triangle_count: descriptor.triangle_count,
                has_per_triangle_data: descriptor.has_per_triangle_data,
            })
            .collect::<Vec<_>>();
        let layout = ColLayout::build(vertex_count, &group_shapes)?;

        if group_offset != layout.header.group_offset as usize
            || vertex_offset != layout.header.vertex_offset as usize
        {
            return Err(unsupported(format!(
                "non-canonical section offsets: groups at {group_offset:#x}, vertices at {vertex_offset:#x}"
            )));
        }
        for (group_index, (descriptor, expected)) in
            descriptors.iter().zip(&layout.groups).enumerate()
        {
            if descriptor.triangle_indices_offset != expected.triangle_indices_offset
                || descriptor.attribute_0_offset != expected.attribute_0_offset
                || descriptor.attribute_1_offset != expected.attribute_1_offset
                || descriptor.triangle_data_offset != expected.triangle_data_offset
            {
                return Err(unsupported(format!(
                    "group {group_index} uses non-canonical stream offsets"
                )));
            }
        }

        if bytes.len() < layout.file_size {
            return Err(invalid_offset(layout.file_size, bytes.len()));
        }
        if bytes.len() != layout.file_size {
            return Err(unsupported(format!(
                "{} trailing bytes are not part of the canonical COL layout",
                bytes.len() - layout.file_size
            )));
        }

        let mut vertices = Vec::with_capacity(vertex_count);
        for vertex_index in 0..vertex_count {
            let offset = vertex_offset + vertex_index * VERTEX_SIZE;
            vertices.push(ColVertex::new(
                be_f32(bytes, offset, FORMAT)?,
                be_f32(bytes, offset + 4, FORMAT)?,
                be_f32(bytes, offset + 8, FORMAT)?,
            ));
        }

        let mut groups = Vec::with_capacity(group_count);
        for (group_index, (descriptor, group_layout)) in
            descriptors.iter().zip(&layout.groups).enumerate()
        {
            let mut triangles = Vec::with_capacity(descriptor.triangle_count);
            for triangle_index in 0..descriptor.triangle_count {
                let index_offset =
                    group_layout.triangle_indices_offset + triangle_index * TRIANGLE_INDEX_SIZE;
                let vertex_indices = [
                    be_u16(bytes, index_offset, FORMAT)?,
                    be_u16(bytes, index_offset + 2, FORMAT)?,
                    be_u16(bytes, index_offset + 4, FORMAT)?,
                ];
                for vertex_index in vertex_indices {
                    if usize::from(vertex_index) >= vertex_count {
                        return Err(unsupported(format!(
                            "group {group_index} triangle {triangle_index} references vertex {vertex_index}, but there are {vertex_count} vertices"
                        )));
                    }
                }

                let data = descriptor.has_per_triangle_data.then(|| {
                    be_i16(
                        bytes,
                        group_layout.triangle_data_offset + triangle_index * 2,
                        FORMAT,
                    )
                });
                triangles.push(ColTriangle {
                    vertex_indices,
                    attribute_0: bytes[group_layout.attribute_0_offset + triangle_index],
                    attribute_1: bytes[group_layout.attribute_1_offset + triangle_index],
                    data: data.transpose()?,
                });
            }
            groups.push(ColGroup {
                surface_type: descriptor.surface_type,
                has_per_triangle_data: descriptor.has_per_triangle_data,
                triangles,
            });
        }

        Ok(Self { vertices, groups })
    }

    /// Returns the canonical header that would be emitted for this document.
    pub fn header(&self) -> ColHeader {
        let group_bytes = self.groups.len().saturating_mul(GROUP_SIZE);
        ColHeader {
            triangle_count_or_flags: self.vertices.len() as u32,
            vertex_offset: HEADER_SIZE.saturating_add(group_bytes) as u32,
            group_count: self.groups.len() as u32,
            group_offset: HEADER_SIZE as u32,
        }
    }

    pub fn vertices(&self) -> &[ColVertex] {
        &self.vertices
    }

    pub fn vertices_mut(&mut self) -> &mut Vec<ColVertex> {
        &mut self.vertices
    }

    pub fn groups(&self) -> &[ColGroup] {
        &self.groups
    }

    pub fn groups_mut(&mut self) -> &mut Vec<ColGroup> {
        &mut self.groups
    }

    /// Reconstructs the canonical big-endian COL file without consulting any
    /// original source bytes.
    pub fn encode(&self) -> Result<Vec<u8>> {
        let group_shapes = self
            .groups
            .iter()
            .map(|group| GroupShape {
                triangle_count: group.triangles.len(),
                has_per_triangle_data: group.has_per_triangle_data,
            })
            .collect::<Vec<_>>();
        let layout = ColLayout::build(self.vertices.len(), &group_shapes)?;
        self.validate_for_encoding()?;

        let mut bytes = Vec::with_capacity(layout.file_size);
        push_u32(&mut bytes, layout.header.vertex_count());
        push_u32(&mut bytes, layout.header.vertex_offset);
        push_u32(&mut bytes, layout.header.group_count);
        push_u32(&mut bytes, layout.header.group_offset);

        for (group, group_layout) in self.groups.iter().zip(&layout.groups) {
            push_u16(&mut bytes, group.surface_type);
            push_i16(&mut bytes, group.triangles.len() as i16);
            push_u16(
                &mut bytes,
                if group.has_per_triangle_data {
                    GROUP_HAS_PER_TRIANGLE_DATA
                } else {
                    0
                },
            );
            push_i16(&mut bytes, GROUP_RESERVED_VALUE);
            push_offset(&mut bytes, group_layout.triangle_indices_offset)?;
            push_offset(&mut bytes, group_layout.attribute_0_offset)?;
            push_offset(&mut bytes, group_layout.attribute_1_offset)?;
            push_offset(&mut bytes, group_layout.triangle_data_offset)?;
        }

        for vertex in &self.vertices {
            for component in vertex.position {
                push_u32(&mut bytes, component.to_bits());
            }
        }
        for group in &self.groups {
            for triangle in &group.triangles {
                for vertex_index in triangle.vertex_indices {
                    push_u16(&mut bytes, vertex_index);
                }
            }
        }
        for group in &self.groups {
            bytes.extend(group.triangles.iter().map(|triangle| triangle.attribute_0));
        }
        for group in &self.groups {
            bytes.extend(group.triangles.iter().map(|triangle| triangle.attribute_1));
        }
        for group in &self.groups {
            if group.has_per_triangle_data {
                for triangle in &group.triangles {
                    push_i16(
                        &mut bytes,
                        triangle
                            .data
                            .expect("COL data presence was validated before encoding"),
                    );
                }
            }
        }

        debug_assert_eq!(bytes.len(), layout.file_size);
        Ok(bytes)
    }

    pub fn to_bytes(&self) -> Result<Vec<u8>> {
        self.encode()
    }

    fn validate_for_encoding(&self) -> Result<()> {
        for (group_index, group) in self.groups.iter().enumerate() {
            if group.triangles.len() > i16::MAX as usize {
                return Err(unsupported(format!(
                    "group {group_index} has {} triangles; the on-disk signed count supports at most {}",
                    group.triangles.len(),
                    i16::MAX
                )));
            }
            for (triangle_index, triangle) in group.triangles.iter().enumerate() {
                if triangle.data.is_some() != group.has_per_triangle_data {
                    return Err(unsupported(format!(
                        "group {group_index} triangle {triangle_index} data presence disagrees with has_per_triangle_data"
                    )));
                }
                for vertex_index in triangle.vertex_indices {
                    if usize::from(vertex_index) >= self.vertices.len() {
                        return Err(unsupported(format!(
                            "group {group_index} triangle {triangle_index} references vertex {vertex_index}, but there are {} vertices",
                            self.vertices.len()
                        )));
                    }
                }
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy)]
struct GroupDescriptor {
    surface_type: u16,
    triangle_count: usize,
    has_per_triangle_data: bool,
    triangle_indices_offset: usize,
    attribute_0_offset: usize,
    attribute_1_offset: usize,
    triangle_data_offset: usize,
}

#[derive(Debug, Clone, Copy)]
struct GroupShape {
    triangle_count: usize,
    has_per_triangle_data: bool,
}

#[derive(Debug, Clone, Copy)]
struct GroupLayout {
    triangle_indices_offset: usize,
    attribute_0_offset: usize,
    attribute_1_offset: usize,
    triangle_data_offset: usize,
}

struct ColLayout {
    header: ColHeader,
    groups: Vec<GroupLayout>,
    file_size: usize,
}

impl ColLayout {
    fn build(vertex_count: usize, groups: &[GroupShape]) -> Result<Self> {
        let group_table_size = checked_layout_mul(groups.len(), GROUP_SIZE)?;
        let vertex_offset = checked_layout_add(HEADER_SIZE, group_table_size)?;
        let vertex_table_size = checked_layout_mul(vertex_count, VERTEX_SIZE)?;
        let indices_start = checked_layout_add(vertex_offset, vertex_table_size)?;

        let total_triangle_count = groups.iter().try_fold(0usize, |total, group| {
            if group.triangle_count > i16::MAX as usize {
                return Err(unsupported(format!(
                    "a collision group has {} triangles; maximum is {}",
                    group.triangle_count,
                    i16::MAX
                )));
            }
            checked_layout_add(total, group.triangle_count)
        })?;
        let index_bytes = checked_layout_mul(total_triangle_count, TRIANGLE_INDEX_SIZE)?;
        let attribute_0_start = checked_layout_add(indices_start, index_bytes)?;
        let attribute_1_start = checked_layout_add(attribute_0_start, total_triangle_count)?;
        let triangle_data_start = checked_layout_add(attribute_1_start, total_triangle_count)?;

        let mut index_cursor = indices_start;
        let mut attribute_0_cursor = attribute_0_start;
        let mut attribute_1_cursor = attribute_1_start;
        let mut triangle_data_cursor = triangle_data_start;
        let mut layouts = Vec::with_capacity(groups.len());
        for group in groups {
            let triangle_data_offset = if group.has_per_triangle_data {
                triangle_data_cursor
            } else {
                0
            };
            layouts.push(GroupLayout {
                triangle_indices_offset: index_cursor,
                attribute_0_offset: attribute_0_cursor,
                attribute_1_offset: attribute_1_cursor,
                triangle_data_offset,
            });
            index_cursor = checked_layout_add(
                index_cursor,
                checked_layout_mul(group.triangle_count, TRIANGLE_INDEX_SIZE)?,
            )?;
            attribute_0_cursor = checked_layout_add(attribute_0_cursor, group.triangle_count)?;
            attribute_1_cursor = checked_layout_add(attribute_1_cursor, group.triangle_count)?;
            if group.has_per_triangle_data {
                triangle_data_cursor = checked_layout_add(
                    triangle_data_cursor,
                    checked_layout_mul(group.triangle_count, 2)?,
                )?;
            }
        }

        let vertex_count_u32 = u32::try_from(vertex_count).map_err(|_| {
            unsupported(format!(
                "COL vertex count {vertex_count} does not fit in u32"
            ))
        })?;
        let group_count_u32 = u32::try_from(groups.len()).map_err(|_| {
            unsupported(format!(
                "COL group count {} does not fit in u32",
                groups.len()
            ))
        })?;
        let vertex_offset_u32 = u32::try_from(vertex_offset).map_err(|_| {
            unsupported(format!("COL vertex offset {vertex_offset:#x} exceeds u32"))
        })?;
        u32::try_from(triangle_data_cursor).map_err(|_| {
            unsupported(format!(
                "COL encoded size {triangle_data_cursor:#x} exceeds u32 offsets"
            ))
        })?;

        Ok(Self {
            header: ColHeader {
                triangle_count_or_flags: vertex_count_u32,
                vertex_offset: vertex_offset_u32,
                group_count: group_count_u32,
                group_offset: HEADER_SIZE as u32,
            },
            groups: layouts,
            file_size: triangle_data_cursor,
        })
    }
}

fn validate_counted_span(bytes: &[u8], offset: usize, count: usize, stride: usize) -> Result<()> {
    let length = count
        .checked_mul(stride)
        .ok_or_else(|| invalid_offset(offset, bytes.len()))?;
    checked_slice(FORMAT, bytes, offset, length)?;
    Ok(())
}

fn checked_mul(left: usize, right: usize, len: usize) -> Result<usize> {
    left.checked_mul(right)
        .ok_or_else(|| invalid_offset(left, len))
}

fn checked_add(left: usize, right: usize, len: usize) -> Result<usize> {
    left.checked_add(right)
        .ok_or_else(|| invalid_offset(left, len))
}

fn checked_layout_mul(left: usize, right: usize) -> Result<usize> {
    left.checked_mul(right).ok_or_else(|| {
        unsupported(format!(
            "COL layout multiplication overflow: {left} * {right}"
        ))
    })
}

fn checked_layout_add(left: usize, right: usize) -> Result<usize> {
    left.checked_add(right)
        .ok_or_else(|| unsupported(format!("COL layout addition overflow: {left} + {right}")))
}

fn push_u16(bytes: &mut Vec<u8>, value: u16) {
    bytes.extend_from_slice(&value.to_be_bytes());
}

fn push_i16(bytes: &mut Vec<u8>, value: i16) {
    bytes.extend_from_slice(&value.to_be_bytes());
}

fn push_u32(bytes: &mut Vec<u8>, value: u32) {
    bytes.extend_from_slice(&value.to_be_bytes());
}

fn push_offset(bytes: &mut Vec<u8>, offset: usize) -> Result<()> {
    let offset = u32::try_from(offset)
        .map_err(|_| unsupported(format!("COL offset {offset:#x} exceeds u32")))?;
    push_u32(bytes, offset);
    Ok(())
}

fn invalid_offset(offset: usize, len: usize) -> FormatError {
    FormatError::InvalidOffset {
        format: FORMAT,
        offset,
        len,
    }
}

fn unsupported(message: String) -> FormatError {
    FormatError::Unsupported {
        format: FORMAT,
        message,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn synthetic_col() -> ColFile {
        ColFile::new(
            vec![
                ColVertex::new(-0.0, 1.0, 2.0),
                ColVertex::new(3.0, 4.0, 5.0),
                ColVertex::new(6.0, 7.0, 8.0),
                ColVertex::new(9.0, 10.0, 11.0),
            ],
            vec![
                ColGroup {
                    surface_type: 0x0000,
                    has_per_triangle_data: false,
                    triangles: vec![
                        ColTriangle {
                            vertex_indices: [0, 1, 2],
                            attribute_0: 7,
                            attribute_1: 9,
                            data: None,
                        },
                        ColTriangle {
                            vertex_indices: [0, 2, 3],
                            attribute_0: 8,
                            attribute_1: 10,
                            data: None,
                        },
                    ],
                },
                ColGroup {
                    surface_type: 0xC004,
                    has_per_triangle_data: true,
                    triangles: vec![ColTriangle {
                        vertex_indices: [3, 2, 1],
                        attribute_0: 11,
                        attribute_1: 12,
                        data: Some(-1234),
                    }],
                },
            ],
        )
    }

    #[test]
    fn encodes_canonical_semantic_layout() {
        let bytes = synthetic_col().encode().unwrap();

        assert_eq!(
            &bytes[0x00..0x10],
            &[0, 0, 0, 4, 0, 0, 0, 0x40, 0, 0, 0, 2, 0, 0, 0, 0x10]
        );
        assert_eq!(
            &bytes[0x10..0x28],
            &[
                0, 0, 0, 2, 0, 0, 0xFF, 0xFF, 0, 0, 0, 0x70, 0, 0, 0, 0x82, 0, 0, 0, 0x85, 0, 0, 0,
                0
            ]
        );
        assert_eq!(
            &bytes[0x28..0x40],
            &[
                0xC0, 4, 0, 1, 0, 1, 0xFF, 0xFF, 0, 0, 0, 0x7C, 0, 0, 0, 0x84, 0, 0, 0, 0x87, 0, 0,
                0, 0x88
            ]
        );
        assert_eq!(&bytes[0x40..0x44], &(-0.0f32).to_bits().to_be_bytes());
        assert_eq!(bytes.len(), 0x8A);
    }

    #[test]
    fn source_free_parse_encode_is_byte_identical() {
        let original = synthetic_col().encode().unwrap();
        let parsed = ColFile::parse(&original).unwrap();

        assert_eq!(parsed.vertices().len(), 4);
        assert_eq!(parsed.groups().len(), 2);
        assert_eq!(parsed.groups()[1].triangles[0].data, Some(-1234));
        assert_eq!(parsed.encode().unwrap(), original);
    }

    #[test]
    fn every_truncated_prefix_is_rejected() {
        let bytes = synthetic_col().encode().unwrap();
        assert!(ColFile::parse(&bytes).is_ok());
        for length in 0..bytes.len() {
            assert!(
                ColFile::parse(&bytes[..length]).is_err(),
                "truncated prefix of {length} bytes was accepted"
            );
        }
    }

    #[test]
    fn rejects_unmodeled_trailing_bytes() {
        let mut bytes = synthetic_col().encode().unwrap();
        bytes.push(0);
        assert!(matches!(
            ColFile::parse(bytes),
            Err(FormatError::Unsupported { .. })
        ));
    }

    #[test]
    fn rejects_noncanonical_group_fields() {
        let original = synthetic_col().encode().unwrap();
        for (offset, value) in [(0x15, 2u8), (0x16, 0u8), (0x1B, 0x71u8)] {
            let mut bytes = original.clone();
            bytes[offset] = value;
            assert!(matches!(
                ColFile::parse(bytes),
                Err(FormatError::Unsupported { .. })
            ));
        }
    }

    #[test]
    fn rejects_triangle_vertex_out_of_range() {
        let mut bytes = synthetic_col().encode().unwrap();
        bytes[0x70..0x72].copy_from_slice(&4u16.to_be_bytes());
        assert!(matches!(
            ColFile::parse(bytes),
            Err(FormatError::Unsupported { .. })
        ));
    }

    #[test]
    fn rejects_inconsistent_authored_triangle_data() {
        let mut col = synthetic_col();
        col.groups_mut()[0].triangles[0].data = Some(5);
        assert!(matches!(col.encode(), Err(FormatError::Unsupported { .. })));
    }

    #[test]
    fn extreme_counts_are_rejected_without_integer_wraparound() {
        let mut bytes = vec![0; HEADER_SIZE];
        bytes[..4].copy_from_slice(&u32::MAX.to_be_bytes());
        bytes[4..8].copy_from_slice(&(HEADER_SIZE as u32).to_be_bytes());
        bytes[12..16].copy_from_slice(&(HEADER_SIZE as u32).to_be_bytes());
        assert!(matches!(
            ColFile::parse(bytes),
            Err(FormatError::InvalidOffset { .. })
        ));
    }

    #[test]
    #[ignore = "requires SMS_RETAIL_ROOT to point to an extracted Sunshine filesystem root"]
    fn retail_collision_census_rebuilds_without_source_bytes() {
        use std::env;
        use std::fs;

        use crate::{decode_yaz0, discover_scene_archives, RarcArchive};

        let root = env::var_os("SMS_RETAIL_ROOT")
            .expect("set SMS_RETAIL_ROOT to an extracted Sunshine filesystem root");
        let max_archives = env::var("SMS_COL_CENSUS_MAX_ARCHIVES")
            .ok()
            .and_then(|value| value.parse().ok())
            .unwrap_or(128usize);
        let max_files = env::var("SMS_COL_CENSUS_MAX_FILES")
            .ok()
            .and_then(|value| value.parse().ok())
            .unwrap_or(4096usize);

        let archives = discover_scene_archives(root).expect("discover retail scene archives");
        let mut archive_count = 0usize;
        let mut collision_count = 0usize;
        'archives: for archive_info in archives.into_iter().take(max_archives) {
            let source = fs::read(&archive_info.path).expect("read retail scene archive");
            let decoded = if source.starts_with(b"Yaz0") {
                decode_yaz0(&source).expect("decode retail scene archive")
            } else {
                source
            };
            let archive = RarcArchive::parse(decoded).expect("parse retail scene archive");
            archive_count += 1;
            for entry in archive.file_entries() {
                if !entry.path.to_ascii_lowercase().ends_with(".col") {
                    continue;
                }
                let original = archive
                    .file_bytes_raw(&entry.raw_path)
                    .expect("read retail collision entry");
                let parsed = ColFile::parse(&original).unwrap_or_else(|error| {
                    panic!(
                        "parse {} in {}: {error}",
                        entry.path,
                        archive_info.path.display()
                    )
                });
                let rebuilt = parsed.encode().unwrap_or_else(|error| {
                    panic!(
                        "encode {} in {}: {error}",
                        entry.path,
                        archive_info.path.display()
                    )
                });
                assert_eq!(
                    rebuilt,
                    original,
                    "source-free rebuild differs for {} in {}",
                    entry.path,
                    archive_info.path.display()
                );
                collision_count += 1;
                if collision_count >= max_files {
                    break 'archives;
                }
            }
        }

        assert!(archive_count > 0, "retail census found no scene archives");
        assert!(collision_count > 0, "retail census found no COL files");
        eprintln!(
            "source-free COL census rebuilt {collision_count} files from {archive_count} archives"
        );
    }
}
