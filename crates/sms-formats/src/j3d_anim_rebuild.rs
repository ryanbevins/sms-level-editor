//! Source-free, encoded-domain J3D animation documents.
//!
//! These types intentionally retain serialization decisions (table offsets,
//! key-bank ordering, name hashes, and recognized padding policy) as typed
//! metadata. They never retain the input file or an opaque section blob.

use encoding_rs::SHIFT_JIS;
use serde::{Deserialize, Serialize};

use crate::binary::{be_f32, be_i16, be_u16, be_u32, require_len};
use crate::{FormatError, Result};

const FORMAT: &str = "J3D animation rebuild";
const FILE_HEADER_SIZE: usize = 0x20;
const RETAIL_PADDING: &[u8] = b"This is padding data to alignment";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum J3dAnimationHeaderTag {
    AllFf,
    Svr1,
    AllZero,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum J3dAnimationPaddingStyle {
    Zero,
    Ff,
    RetailPhrase,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct J3dAnimationPaddingRegion {
    pub section_offset: u32,
    pub length: u32,
    pub style: J3dAnimationPaddingStyle,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct J3dAnimationLayout {
    pub actual_section_size: u32,
    pub declared_section_size: u32,
    pub padding: Vec<J3dAnimationPaddingRegion>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct J3dKeyTable {
    pub key_count: u16,
    pub value_offset: u16,
    pub tangent_type: u16,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct J3dTransformKeyTable {
    pub scale: J3dKeyTable,
    pub rotation: J3dKeyTable,
    pub translation: J3dKeyTable,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct J3dAnimationNameEntry {
    pub relative_offset: u16,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct J3dAnimationNameTable {
    pub entries: Vec<J3dAnimationNameEntry>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct J3dJointKeyAnimationSection {
    pub attribute: u8,
    pub angle_scale: u8,
    pub max_frame: u16,
    pub joint_tables: Vec<[J3dTransformKeyTable; 3]>,
    pub scales: Vec<f32>,
    pub rotations: Vec<i16>,
    pub translations: Vec<f32>,
    pub offsets: [u32; 4],
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct J3dTexturePatternTable {
    pub frame_count: u16,
    pub first_value: u16,
    pub texture_slot: u8,
    pub padding: u8,
    pub reserved: u16,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct J3dTexturePatternAnimationSection {
    pub attribute: u8,
    pub header_padding: u8,
    pub max_frame: u16,
    pub tables: Vec<J3dTexturePatternTable>,
    pub texture_indices: Vec<u16>,
    pub material_remap: Vec<u16>,
    pub material_names: J3dAnimationNameTable,
    pub offsets: [u32; 4],
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct J3dTextureSrtSet {
    pub tables: Vec<J3dTransformKeyTable>,
    pub material_remap: Vec<u16>,
    pub material_names: J3dAnimationNameTable,
    pub texture_matrix_ids: Vec<u8>,
    pub centers: Vec<[f32; 3]>,
    pub scales: Vec<f32>,
    pub rotations: Vec<i16>,
    pub translations: Vec<f32>,
    pub offsets: [u32; 8],
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct J3dTextureSrtAnimationSection {
    pub attribute: u8,
    pub rotation_shift: u8,
    pub max_frame: u16,
    pub primary: J3dTextureSrtSet,
    pub post: J3dTextureSrtSet,
    /// Loader-ignored values needed only to reproduce the creator's exact
    /// encoded file. This is bounded typed metadata, never an input buffer.
    #[serde(default)]
    pub reconstruction: J3dTextureSrtReconstructionMetadata,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct J3dTextureSrtReconstructionMetadata {
    pub creator_word: J3dTextureSrtCreatorWord,
    pub residue_regions: Vec<J3dTextureSrtResidueRegion>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(transparent)]
pub struct J3dTextureSrtCreatorWord(pub u32);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct J3dTextureSrtResidueRegion {
    pub section_offset: u32,
    pub words: J3dTextureSrtResidueWords,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct J3dTextureSrtResidueWords(pub Vec<u32>);

impl J3dTextureSrtResidueWords {
    fn parse(bytes: &[u8]) -> Result<Self> {
        if bytes.is_empty() || !bytes.len().is_multiple_of(4) {
            return Err(unsupported(format!(
                "TTK1 creator residue length {} is not a nonempty whole-word region",
                bytes.len()
            )));
        }
        Ok(Self(
            bytes
                .chunks_exact(4)
                .map(|chunk| {
                    u32::from_be_bytes(chunk.try_into().expect("four-byte creator residue word"))
                })
                .collect(),
        ))
    }

    fn as_slice(&self) -> &[u32] {
        &self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct J3dColorKeyTable {
    pub channels: [J3dKeyTable; 4],
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct J3dMaterialColorAnimationSection {
    pub attribute: u8,
    pub header_reserved: [u8; 3],
    pub max_frame: u16,
    pub tables: Vec<J3dColorKeyTable>,
    pub material_remap: Vec<u16>,
    pub material_names: J3dAnimationNameTable,
    pub channels: [Vec<i16>; 4],
    pub offsets: [u32; 7],
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct J3dTevColorKeyTable {
    pub channels: [J3dKeyTable; 4],
    pub register_id: u8,
    pub padding: [u8; 3],
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct J3dTevRegisterSet {
    pub tables: Vec<J3dTevColorKeyTable>,
    pub material_remap: Vec<u16>,
    pub material_names: J3dAnimationNameTable,
    pub channels: [Vec<i16>; 4],
    pub offsets: [u32; 7],
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct J3dTevRegisterAnimationSection {
    pub attribute: u8,
    pub header_padding: u8,
    pub max_frame: u16,
    pub color_registers: J3dTevRegisterSet,
    pub konst_registers: J3dTevRegisterSet,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum J3dAnimationSection {
    JointKey(J3dJointKeyAnimationSection),
    TexturePattern(J3dTexturePatternAnimationSection),
    TextureSrt(J3dTextureSrtAnimationSection),
    MaterialColor(J3dMaterialColorAnimationSection),
    TevRegister(J3dTevRegisterAnimationSection),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct J3dAnimationRebuildDocument {
    pub header_tag: J3dAnimationHeaderTag,
    pub layout: J3dAnimationLayout,
    pub section: J3dAnimationSection,
}

impl J3dAnimationRebuildDocument {
    pub fn parse(bytes: impl AsRef<[u8]>) -> Result<Self> {
        let bytes = bytes.as_ref();
        require_len(FORMAT, bytes, FILE_HEADER_SIZE + 8)?;
        let (file_magic, section_magic, header_size) = match &bytes[..8] {
            b"J3D1bck1" => (b"J3D1bck1" as &[u8], b"ANK1" as &[u8], 0x24),
            b"J3D1btp1" => (b"J3D1btp1" as &[u8], b"TPT1" as &[u8], 0x20),
            b"J3D1btk1" => (b"J3D1btk1" as &[u8], b"TTK1" as &[u8], 0x60),
            b"J3D1bpk1" => (b"J3D1bpk1" as &[u8], b"PAK1" as &[u8], 0x34),
            b"J3D1brk1" => (b"J3D1brk1" as &[u8], b"TRK1" as &[u8], 0x58),
            actual => {
                return Err(FormatError::BadMagic {
                    format: FORMAT,
                    expected: b"J3D1bck1/btp1/btk1/bpk1/brk1",
                    actual: actual.to_vec(),
                });
            }
        };
        let _ = file_magic;
        let declared_file_size = be_u32(bytes, 0x08, FORMAT)? as usize;
        if declared_file_size != bytes.len() {
            return Err(unsupported(format!(
                "declared file size {declared_file_size:#x} differs from input size {:#x}; trailing source bytes are not semantic",
                bytes.len()
            )));
        }
        if be_u32(bytes, 0x0C, FORMAT)? != 1 {
            return Err(unsupported(
                "only one-section v1 animations are rebuildable",
            ));
        }
        let header_tag = parse_header_tag(&bytes[0x10..0x20])?;
        if &bytes[0x20..0x24] != section_magic {
            return Err(unsupported(format!(
                "expected {} section",
                String::from_utf8_lossy(section_magic)
            )));
        }
        let actual_section_size = bytes.len() - FILE_HEADER_SIZE;
        let declared_section_size = be_u32(bytes, 0x24, FORMAT)? as usize;
        if declared_section_size > actual_section_size + 0x20 {
            return Err(unsupported(format!(
                "final section overclaims {} bytes",
                declared_section_size - actual_section_size
            )));
        }
        if actual_section_size < header_size {
            return Err(invalid(actual_section_size, header_size));
        }

        let section_bytes = &bytes[FILE_HEADER_SIZE..];
        let mut coverage = Coverage::new(section_bytes.len());
        coverage.mark(0, header_size)?;
        let mut section = match section_magic {
            b"ANK1" => J3dAnimationSection::JointKey(parse_ank1(section_bytes, &mut coverage)?),
            b"TPT1" => {
                J3dAnimationSection::TexturePattern(parse_tpt1(section_bytes, &mut coverage)?)
            }
            b"TTK1" => J3dAnimationSection::TextureSrt(parse_ttk1(section_bytes, &mut coverage)?),
            b"PAK1" => {
                J3dAnimationSection::MaterialColor(parse_pak1(section_bytes, &mut coverage)?)
            }
            b"TRK1" => J3dAnimationSection::TevRegister(parse_trk1(section_bytes, &mut coverage)?),
            _ => unreachable!(),
        };
        let (padding, residue_regions) =
            coverage.classify_uncovered(section_bytes, header_size, section_magic == b"TTK1")?;
        if let J3dAnimationSection::TextureSrt(value) = &mut section {
            value.reconstruction.residue_regions = residue_regions;
        } else if !residue_regions.is_empty() {
            return Err(unsupported(
                "creator residue is only supported for TTK1 sections",
            ));
        }
        Ok(Self {
            header_tag,
            layout: J3dAnimationLayout {
                actual_section_size: to_u32(actual_section_size, "section size")?,
                declared_section_size: to_u32(declared_section_size, "declared section size")?,
                padding,
            },
            section,
        })
    }

    pub fn encode(&self) -> Result<Vec<u8>> {
        let section_size = self.layout.actual_section_size as usize;
        let mut bytes = vec![0_u8; FILE_HEADER_SIZE + section_size];
        let (file_magic, section_magic, header_size) = match &self.section {
            J3dAnimationSection::JointKey(_) => (b"J3D1bck1" as &[u8], b"ANK1", 0x24),
            J3dAnimationSection::TexturePattern(_) => (b"J3D1btp1" as &[u8], b"TPT1", 0x20),
            J3dAnimationSection::TextureSrt(_) => (b"J3D1btk1" as &[u8], b"TTK1", 0x60),
            J3dAnimationSection::MaterialColor(_) => (b"J3D1bpk1" as &[u8], b"PAK1", 0x34),
            J3dAnimationSection::TevRegister(_) => (b"J3D1brk1" as &[u8], b"TRK1", 0x58),
        };
        if section_size < header_size {
            return Err(invalid(section_size, header_size));
        }
        bytes[..8].copy_from_slice(file_magic);
        let file_size = to_u32(bytes.len(), "file size")?;
        put_u32(&mut bytes, 0x08, file_size)?;
        put_u32(&mut bytes, 0x0C, 1)?;
        write_header_tag(&mut bytes[0x10..0x20], self.header_tag);
        let section = &mut bytes[FILE_HEADER_SIZE..];
        for region in &self.layout.padding {
            fill_padding(section, region)?;
        }
        put_bytes(section, 0, section_magic)?;
        put_u32(section, 4, self.layout.declared_section_size)?;
        match &self.section {
            J3dAnimationSection::JointKey(value) => encode_ank1(section, value)?,
            J3dAnimationSection::TexturePattern(value) => encode_tpt1(section, value)?,
            J3dAnimationSection::TextureSrt(value) => encode_ttk1(section, value)?,
            J3dAnimationSection::MaterialColor(value) => encode_pak1(section, value)?,
            J3dAnimationSection::TevRegister(value) => encode_trk1(section, value)?,
        }
        Ok(bytes)
    }

    pub fn to_bytes(&self) -> Result<Vec<u8>> {
        self.encode()
    }
}

fn parse_ank1(bytes: &[u8], coverage: &mut Coverage) -> Result<J3dJointKeyAnimationSection> {
    let joint_count = be_u16(bytes, 0x0C, FORMAT)? as usize;
    let counts = [
        be_u16(bytes, 0x0E, FORMAT)? as usize,
        be_u16(bytes, 0x10, FORMAT)? as usize,
        be_u16(bytes, 0x12, FORMAT)? as usize,
    ];
    let offsets = read_offsets::<4>(bytes, [0x14, 0x18, 0x1C, 0x20])?;
    let mut joint_tables = Vec::with_capacity(joint_count);
    coverage.mark(offsets[0] as usize, checked_mul(joint_count, 0x36)?)?;
    for joint in 0..joint_count {
        let base = offsets[0] as usize + joint * 0x36;
        joint_tables.push(std::array::from_fn(|axis| {
            read_transform_table(bytes, base + axis * 0x12).expect("covered transform table")
        }));
    }
    let scales = read_f32_bank(bytes, coverage, offsets[1], counts[0])?;
    let rotations = read_i16_bank(bytes, coverage, offsets[2], counts[1])?;
    let translations = read_f32_bank(bytes, coverage, offsets[3], counts[2])?;
    validate_transform_tables(
        joint_tables.iter().flat_map(|joint| joint.iter()),
        scales.len(),
        rotations.len(),
        translations.len(),
    )?;
    Ok(J3dJointKeyAnimationSection {
        attribute: bytes[0x08],
        angle_scale: bytes[0x09],
        max_frame: be_u16(bytes, 0x0A, FORMAT)?,
        joint_tables,
        scales,
        rotations,
        translations,
        offsets,
    })
}

fn encode_ank1(bytes: &mut [u8], value: &J3dJointKeyAnimationSection) -> Result<()> {
    bytes[0x08] = value.attribute;
    bytes[0x09] = value.angle_scale;
    put_u16(bytes, 0x0A, value.max_frame)?;
    put_u16(
        bytes,
        0x0C,
        to_u16(value.joint_tables.len(), "joint count")?,
    )?;
    put_u16(bytes, 0x0E, to_u16(value.scales.len(), "scale bank count")?)?;
    put_u16(
        bytes,
        0x10,
        to_u16(value.rotations.len(), "rotation bank count")?,
    )?;
    put_u16(
        bytes,
        0x12,
        to_u16(value.translations.len(), "translation bank count")?,
    )?;
    write_offsets(bytes, [0x14, 0x18, 0x1C, 0x20], value.offsets)?;
    for (joint_index, joint) in value.joint_tables.iter().enumerate() {
        for (axis, table) in joint.iter().enumerate() {
            write_transform_table(
                bytes,
                value.offsets[0] as usize + joint_index * 0x36 + axis * 0x12,
                table,
            )?;
        }
    }
    write_f32_bank(bytes, value.offsets[1], &value.scales)?;
    write_i16_bank(bytes, value.offsets[2], &value.rotations)?;
    write_f32_bank(bytes, value.offsets[3], &value.translations)
}

fn parse_tpt1(bytes: &[u8], coverage: &mut Coverage) -> Result<J3dTexturePatternAnimationSection> {
    let table_count = be_u16(bytes, 0x0C, FORMAT)? as usize;
    let value_count = be_u16(bytes, 0x0E, FORMAT)? as usize;
    let offsets = read_offsets::<4>(bytes, [0x10, 0x14, 0x18, 0x1C])?;
    coverage.mark(offsets[0] as usize, checked_mul(table_count, 8)?)?;
    let mut tables = Vec::with_capacity(table_count);
    for index in 0..table_count {
        let offset = offsets[0] as usize + index * 8;
        tables.push(J3dTexturePatternTable {
            frame_count: be_u16(bytes, offset, FORMAT)?,
            first_value: be_u16(bytes, offset + 2, FORMAT)?,
            texture_slot: bytes[offset + 4],
            padding: bytes[offset + 5],
            reserved: be_u16(bytes, offset + 6, FORMAT)?,
        });
    }
    let texture_indices = read_u16_bank(bytes, coverage, offsets[1], value_count)?;
    let material_remap = read_u16_bank(bytes, coverage, offsets[2], table_count)?;
    let material_names = read_name_table(bytes, coverage, offsets[3], table_count)?;
    for table in &tables {
        let end = table.first_value as usize + table.frame_count as usize;
        if end > texture_indices.len() {
            return Err(invalid(end, texture_indices.len()));
        }
    }
    Ok(J3dTexturePatternAnimationSection {
        attribute: bytes[0x08],
        header_padding: bytes[0x09],
        max_frame: be_u16(bytes, 0x0A, FORMAT)?,
        tables,
        texture_indices,
        material_remap,
        material_names,
        offsets,
    })
}

fn encode_tpt1(bytes: &mut [u8], value: &J3dTexturePatternAnimationSection) -> Result<()> {
    bytes[0x08] = value.attribute;
    bytes[0x09] = value.header_padding;
    put_u16(bytes, 0x0A, value.max_frame)?;
    put_u16(
        bytes,
        0x0C,
        to_u16(value.tables.len(), "pattern table count")?,
    )?;
    put_u16(
        bytes,
        0x0E,
        to_u16(value.texture_indices.len(), "texture index count")?,
    )?;
    write_offsets(bytes, [0x10, 0x14, 0x18, 0x1C], value.offsets)?;
    require_equal(
        value.material_remap.len(),
        value.tables.len(),
        "pattern remap count",
    )?;
    require_equal(
        value.material_names.entries.len(),
        value.tables.len(),
        "pattern name count",
    )?;
    for (index, table) in value.tables.iter().enumerate() {
        let offset = value.offsets[0] as usize + index * 8;
        put_u16(bytes, offset, table.frame_count)?;
        put_u16(bytes, offset + 2, table.first_value)?;
        put_byte(bytes, offset + 4, table.texture_slot)?;
        put_byte(bytes, offset + 5, table.padding)?;
        put_u16(bytes, offset + 6, table.reserved)?;
    }
    write_u16_bank(bytes, value.offsets[1], &value.texture_indices)?;
    write_u16_bank(bytes, value.offsets[2], &value.material_remap)?;
    write_name_table(bytes, value.offsets[3], &value.material_names)
}

fn parse_ttk1(bytes: &[u8], coverage: &mut Coverage) -> Result<J3dTextureSrtAnimationSection> {
    let primary_counts = [
        be_u16(bytes, 0x0C, FORMAT)? as usize,
        be_u16(bytes, 0x0E, FORMAT)? as usize,
        be_u16(bytes, 0x10, FORMAT)? as usize,
        be_u16(bytes, 0x12, FORMAT)? as usize,
    ];
    let post_counts = [
        be_u16(bytes, 0x34, FORMAT)? as usize,
        be_u16(bytes, 0x36, FORMAT)? as usize,
        be_u16(bytes, 0x38, FORMAT)? as usize,
        be_u16(bytes, 0x3A, FORMAT)? as usize,
    ];
    let primary_offsets =
        read_offsets::<8>(bytes, [0x14, 0x18, 0x1C, 0x20, 0x24, 0x28, 0x2C, 0x30])?;
    let post_offsets = read_offsets::<8>(bytes, [0x3C, 0x40, 0x44, 0x48, 0x4C, 0x50, 0x54, 0x58])?;
    let creator_word = J3dTextureSrtCreatorWord(be_u32(bytes, 0x5C, FORMAT)?);
    let primary = parse_srt_set(bytes, coverage, primary_counts, primary_offsets)?;
    let post = parse_srt_set(bytes, coverage, post_counts, post_offsets)?;
    Ok(J3dTextureSrtAnimationSection {
        attribute: bytes[0x08],
        rotation_shift: bytes[0x09],
        max_frame: be_u16(bytes, 0x0A, FORMAT)?,
        primary,
        post,
        reconstruction: J3dTextureSrtReconstructionMetadata {
            creator_word,
            residue_regions: Vec::new(),
        },
    })
}

fn parse_srt_set(
    bytes: &[u8],
    coverage: &mut Coverage,
    counts: [usize; 4],
    offsets: [u32; 8],
) -> Result<J3dTextureSrtSet> {
    let [track_count, scale_count, rotation_count, translation_count] = counts;
    if !track_count.is_multiple_of(3) {
        return Err(unsupported(format!(
            "SRT track count {track_count} is not divisible by 3"
        )));
    }
    let binding_count = track_count / 3;
    let tables = read_transform_tables(bytes, coverage, offsets[0], track_count)?;
    let material_remap = read_u16_bank(bytes, coverage, offsets[1], binding_count)?;
    let material_names = read_name_table(bytes, coverage, offsets[2], binding_count)?;
    let texture_matrix_ids = read_u8_bank(bytes, coverage, offsets[3], binding_count)?;
    let centers = read_vec3_bank(bytes, coverage, offsets[4], binding_count)?;
    let scales = read_f32_bank(bytes, coverage, offsets[5], scale_count)?;
    let rotations = read_i16_bank(bytes, coverage, offsets[6], rotation_count)?;
    let translations = read_f32_bank(bytes, coverage, offsets[7], translation_count)?;
    validate_transform_tables(
        tables.iter(),
        scales.len(),
        rotations.len(),
        translations.len(),
    )?;
    Ok(J3dTextureSrtSet {
        tables,
        material_remap,
        material_names,
        texture_matrix_ids,
        centers,
        scales,
        rotations,
        translations,
        offsets,
    })
}

fn encode_ttk1(bytes: &mut [u8], value: &J3dTextureSrtAnimationSection) -> Result<()> {
    for region in &value.reconstruction.residue_regions {
        write_ttk1_residue_region(bytes, region)?;
    }
    bytes[0x08] = value.attribute;
    bytes[0x09] = value.rotation_shift;
    put_u16(bytes, 0x0A, value.max_frame)?;
    encode_srt_set(
        bytes,
        0x0C,
        [0x14, 0x18, 0x1C, 0x20, 0x24, 0x28, 0x2C, 0x30],
        &value.primary,
    )?;
    encode_srt_set(
        bytes,
        0x34,
        [0x3C, 0x40, 0x44, 0x48, 0x4C, 0x50, 0x54, 0x58],
        &value.post,
    )?;
    put_u32(bytes, 0x5C, value.reconstruction.creator_word.0)
}

fn write_ttk1_residue_region(bytes: &mut [u8], region: &J3dTextureSrtResidueRegion) -> Result<()> {
    let offset = region.section_offset as usize;
    if offset < 0x60 || !offset.is_multiple_of(4) {
        return Err(unsupported(format!(
            "TTK1 creator residue offset {offset:#x} is not word-aligned data after the header"
        )));
    }
    let byte_length = region
        .words
        .as_slice()
        .len()
        .checked_mul(4)
        .ok_or_else(|| invalid(offset, bytes.len()))?;
    let end = offset
        .checked_add(byte_length)
        .ok_or_else(|| invalid(offset, bytes.len()))?;
    if byte_length == 0 || end > bytes.len() {
        return Err(invalid(end, bytes.len()));
    }
    for (index, word) in region.words.as_slice().iter().copied().enumerate() {
        put_u32(bytes, offset + index * 4, word)?;
    }
    Ok(())
}

fn encode_srt_set(
    bytes: &mut [u8],
    count_offset: usize,
    offset_fields: [usize; 8],
    value: &J3dTextureSrtSet,
) -> Result<()> {
    if !value.tables.len().is_multiple_of(3) {
        return Err(unsupported("SRT table count must be divisible by 3"));
    }
    let binding_count = value.tables.len() / 3;
    require_equal(value.material_remap.len(), binding_count, "SRT remap count")?;
    require_equal(
        value.material_names.entries.len(),
        binding_count,
        "SRT name count",
    )?;
    require_equal(
        value.texture_matrix_ids.len(),
        binding_count,
        "SRT matrix ID count",
    )?;
    require_equal(value.centers.len(), binding_count, "SRT center count")?;
    put_u16(
        bytes,
        count_offset,
        to_u16(value.tables.len(), "SRT table count")?,
    )?;
    put_u16(
        bytes,
        count_offset + 2,
        to_u16(value.scales.len(), "SRT scale count")?,
    )?;
    put_u16(
        bytes,
        count_offset + 4,
        to_u16(value.rotations.len(), "SRT rotation count")?,
    )?;
    put_u16(
        bytes,
        count_offset + 6,
        to_u16(value.translations.len(), "SRT translation count")?,
    )?;
    write_offsets(bytes, offset_fields, value.offsets)?;
    write_transform_tables(bytes, value.offsets[0], &value.tables)?;
    write_u16_bank(bytes, value.offsets[1], &value.material_remap)?;
    write_name_table(bytes, value.offsets[2], &value.material_names)?;
    write_u8_bank(bytes, value.offsets[3], &value.texture_matrix_ids)?;
    write_vec3_bank(bytes, value.offsets[4], &value.centers)?;
    write_f32_bank(bytes, value.offsets[5], &value.scales)?;
    write_i16_bank(bytes, value.offsets[6], &value.rotations)?;
    write_f32_bank(bytes, value.offsets[7], &value.translations)
}

fn parse_pak1(bytes: &[u8], coverage: &mut Coverage) -> Result<J3dMaterialColorAnimationSection> {
    let table_count = be_u16(bytes, 0x0E, FORMAT)? as usize;
    let counts = [
        be_u16(bytes, 0x10, FORMAT)? as usize,
        be_u16(bytes, 0x12, FORMAT)? as usize,
        be_u16(bytes, 0x14, FORMAT)? as usize,
        be_u16(bytes, 0x16, FORMAT)? as usize,
    ];
    let offsets = read_offsets::<7>(bytes, [0x18, 0x1C, 0x20, 0x24, 0x28, 0x2C, 0x30])?;
    let tables = read_color_tables(bytes, coverage, offsets[0], table_count)?;
    let material_remap = read_u16_bank(bytes, coverage, offsets[1], table_count)?;
    let material_names = read_name_table(bytes, coverage, offsets[2], table_count)?;
    let channels = [
        read_i16_bank(bytes, coverage, offsets[3], counts[0])?,
        read_i16_bank(bytes, coverage, offsets[4], counts[1])?,
        read_i16_bank(bytes, coverage, offsets[5], counts[2])?,
        read_i16_bank(bytes, coverage, offsets[6], counts[3])?,
    ];
    validate_color_tables(tables.iter().map(|table| &table.channels), &channels)?;
    Ok(J3dMaterialColorAnimationSection {
        attribute: bytes[0x08],
        header_reserved: [bytes[0x09], bytes[0x0A], bytes[0x0B]],
        max_frame: be_u16(bytes, 0x0C, FORMAT)?,
        tables,
        material_remap,
        material_names,
        channels,
        offsets,
    })
}

fn encode_pak1(bytes: &mut [u8], value: &J3dMaterialColorAnimationSection) -> Result<()> {
    bytes[0x08] = value.attribute;
    put_bytes(bytes, 0x09, &value.header_reserved)?;
    put_u16(bytes, 0x0C, value.max_frame)?;
    put_u16(
        bytes,
        0x0E,
        to_u16(value.tables.len(), "color table count")?,
    )?;
    for (index, channel) in value.channels.iter().enumerate() {
        put_u16(
            bytes,
            0x10 + index * 2,
            to_u16(channel.len(), "color bank count")?,
        )?;
    }
    write_offsets(
        bytes,
        [0x18, 0x1C, 0x20, 0x24, 0x28, 0x2C, 0x30],
        value.offsets,
    )?;
    require_equal(
        value.material_remap.len(),
        value.tables.len(),
        "color remap count",
    )?;
    require_equal(
        value.material_names.entries.len(),
        value.tables.len(),
        "color name count",
    )?;
    write_color_tables(bytes, value.offsets[0], &value.tables)?;
    write_u16_bank(bytes, value.offsets[1], &value.material_remap)?;
    write_name_table(bytes, value.offsets[2], &value.material_names)?;
    for (offset, channel) in value.offsets[3..].iter().zip(&value.channels) {
        write_i16_bank(bytes, *offset, channel)?;
    }
    Ok(())
}

fn parse_trk1(bytes: &[u8], coverage: &mut Coverage) -> Result<J3dTevRegisterAnimationSection> {
    let color_count = be_u16(bytes, 0x0C, FORMAT)? as usize;
    let konst_count = be_u16(bytes, 0x0E, FORMAT)? as usize;
    let color_bank_counts = std::array::from_fn(|index| {
        be_u16(bytes, 0x10 + index * 2, FORMAT).expect("covered TRK1 color bank count") as usize
    });
    let konst_bank_counts = std::array::from_fn(|index| {
        be_u16(bytes, 0x18 + index * 2, FORMAT).expect("covered TRK1 konst bank count") as usize
    });
    let color_offsets = read_offsets::<7>(bytes, [0x20, 0x28, 0x30, 0x38, 0x3C, 0x40, 0x44])?;
    let konst_offsets = read_offsets::<7>(bytes, [0x24, 0x2C, 0x34, 0x48, 0x4C, 0x50, 0x54])?;
    let color_registers = parse_tev_set(
        bytes,
        coverage,
        color_count,
        color_bank_counts,
        color_offsets,
    )?;
    let konst_registers = parse_tev_set(
        bytes,
        coverage,
        konst_count,
        konst_bank_counts,
        konst_offsets,
    )?;
    Ok(J3dTevRegisterAnimationSection {
        attribute: bytes[0x08],
        header_padding: bytes[0x09],
        max_frame: be_u16(bytes, 0x0A, FORMAT)?,
        color_registers,
        konst_registers,
    })
}

fn parse_tev_set(
    bytes: &[u8],
    coverage: &mut Coverage,
    table_count: usize,
    bank_counts: [usize; 4],
    offsets: [u32; 7],
) -> Result<J3dTevRegisterSet> {
    coverage.mark(offsets[0] as usize, checked_mul(table_count, 0x1C)?)?;
    let mut tables = Vec::with_capacity(table_count);
    for index in 0..table_count {
        let offset = offsets[0] as usize + index * 0x1C;
        tables.push(J3dTevColorKeyTable {
            channels: std::array::from_fn(|channel| {
                read_key_table(bytes, offset + channel * 6).expect("covered TEV key table")
            }),
            register_id: bytes[offset + 0x18],
            padding: [
                bytes[offset + 0x19],
                bytes[offset + 0x1A],
                bytes[offset + 0x1B],
            ],
        });
    }
    let material_remap = read_u16_bank(bytes, coverage, offsets[1], table_count)?;
    let material_names = read_name_table(bytes, coverage, offsets[2], table_count)?;
    let channels = [
        read_i16_bank(bytes, coverage, offsets[3], bank_counts[0])?,
        read_i16_bank(bytes, coverage, offsets[4], bank_counts[1])?,
        read_i16_bank(bytes, coverage, offsets[5], bank_counts[2])?,
        read_i16_bank(bytes, coverage, offsets[6], bank_counts[3])?,
    ];
    validate_color_tables(tables.iter().map(|table| &table.channels), &channels)?;
    Ok(J3dTevRegisterSet {
        tables,
        material_remap,
        material_names,
        channels,
        offsets,
    })
}

fn encode_trk1(bytes: &mut [u8], value: &J3dTevRegisterAnimationSection) -> Result<()> {
    bytes[0x08] = value.attribute;
    bytes[0x09] = value.header_padding;
    put_u16(bytes, 0x0A, value.max_frame)?;
    put_u16(
        bytes,
        0x0C,
        to_u16(value.color_registers.tables.len(), "C register table count")?,
    )?;
    put_u16(
        bytes,
        0x0E,
        to_u16(value.konst_registers.tables.len(), "K register table count")?,
    )?;
    for index in 0..4 {
        put_u16(
            bytes,
            0x10 + index * 2,
            to_u16(
                value.color_registers.channels[index].len(),
                "C register bank count",
            )?,
        )?;
        put_u16(
            bytes,
            0x18 + index * 2,
            to_u16(
                value.konst_registers.channels[index].len(),
                "K register bank count",
            )?,
        )?;
    }
    write_offsets(
        bytes,
        [0x20, 0x28, 0x30, 0x38, 0x3C, 0x40, 0x44],
        value.color_registers.offsets,
    )?;
    write_offsets(
        bytes,
        [0x24, 0x2C, 0x34, 0x48, 0x4C, 0x50, 0x54],
        value.konst_registers.offsets,
    )?;
    encode_tev_set(bytes, &value.color_registers)?;
    encode_tev_set(bytes, &value.konst_registers)
}

fn encode_tev_set(bytes: &mut [u8], value: &J3dTevRegisterSet) -> Result<()> {
    require_equal(
        value.material_remap.len(),
        value.tables.len(),
        "TEV remap count",
    )?;
    require_equal(
        value.material_names.entries.len(),
        value.tables.len(),
        "TEV name count",
    )?;
    for (index, table) in value.tables.iter().enumerate() {
        let offset = value.offsets[0] as usize + index * 0x1C;
        for (channel, key) in table.channels.iter().enumerate() {
            write_key_table(bytes, offset + channel * 6, key)?;
        }
        put_byte(bytes, offset + 0x18, table.register_id)?;
        put_bytes(bytes, offset + 0x19, &table.padding)?;
    }
    write_u16_bank(bytes, value.offsets[1], &value.material_remap)?;
    write_name_table(bytes, value.offsets[2], &value.material_names)?;
    for (offset, channel) in value.offsets[3..].iter().zip(&value.channels) {
        write_i16_bank(bytes, *offset, channel)?;
    }
    Ok(())
}

fn read_key_table(bytes: &[u8], offset: usize) -> Result<J3dKeyTable> {
    Ok(J3dKeyTable {
        key_count: be_u16(bytes, offset, FORMAT)?,
        value_offset: be_u16(bytes, offset + 2, FORMAT)?,
        tangent_type: be_u16(bytes, offset + 4, FORMAT)?,
    })
}

fn read_transform_table(bytes: &[u8], offset: usize) -> Result<J3dTransformKeyTable> {
    Ok(J3dTransformKeyTable {
        scale: read_key_table(bytes, offset)?,
        rotation: read_key_table(bytes, offset + 6)?,
        translation: read_key_table(bytes, offset + 12)?,
    })
}

fn read_transform_tables(
    bytes: &[u8],
    coverage: &mut Coverage,
    offset: u32,
    count: usize,
) -> Result<Vec<J3dTransformKeyTable>> {
    if count == 0 {
        return Ok(Vec::new());
    }
    coverage.mark(offset as usize, checked_mul(count, 0x12)?)?;
    (0..count)
        .map(|index| read_transform_table(bytes, offset as usize + index * 0x12))
        .collect()
}

fn write_key_table(bytes: &mut [u8], offset: usize, table: &J3dKeyTable) -> Result<()> {
    put_u16(bytes, offset, table.key_count)?;
    put_u16(bytes, offset + 2, table.value_offset)?;
    put_u16(bytes, offset + 4, table.tangent_type)
}

fn write_transform_table(
    bytes: &mut [u8],
    offset: usize,
    table: &J3dTransformKeyTable,
) -> Result<()> {
    write_key_table(bytes, offset, &table.scale)?;
    write_key_table(bytes, offset + 6, &table.rotation)?;
    write_key_table(bytes, offset + 12, &table.translation)
}

fn write_transform_tables(
    bytes: &mut [u8],
    offset: u32,
    tables: &[J3dTransformKeyTable],
) -> Result<()> {
    for (index, table) in tables.iter().enumerate() {
        write_transform_table(bytes, offset as usize + index * 0x12, table)?;
    }
    Ok(())
}

fn read_color_tables(
    bytes: &[u8],
    coverage: &mut Coverage,
    offset: u32,
    count: usize,
) -> Result<Vec<J3dColorKeyTable>> {
    coverage.mark(offset as usize, checked_mul(count, 0x18)?)?;
    (0..count)
        .map(|index| {
            let base = offset as usize + index * 0x18;
            Ok(J3dColorKeyTable {
                channels: std::array::from_fn(|channel| {
                    read_key_table(bytes, base + channel * 6).expect("covered color key table")
                }),
            })
        })
        .collect()
}

fn write_color_tables(bytes: &mut [u8], offset: u32, tables: &[J3dColorKeyTable]) -> Result<()> {
    for (index, table) in tables.iter().enumerate() {
        for (channel, key) in table.channels.iter().enumerate() {
            write_key_table(bytes, offset as usize + index * 0x18 + channel * 6, key)?;
        }
    }
    Ok(())
}

fn validate_transform_tables<'a>(
    tables: impl Iterator<Item = &'a J3dTransformKeyTable>,
    scale_len: usize,
    rotation_len: usize,
    translation_len: usize,
) -> Result<()> {
    for table in tables {
        validate_key_table(table.scale, scale_len)?;
        validate_key_table(table.rotation, rotation_len)?;
        validate_key_table(table.translation, translation_len)?;
    }
    Ok(())
}

fn validate_color_tables<'a>(
    tables: impl Iterator<Item = &'a [J3dKeyTable; 4]>,
    channels: &[Vec<i16>; 4],
) -> Result<()> {
    for table in tables {
        for channel in 0..4 {
            validate_key_table(table[channel], channels[channel].len())?;
        }
    }
    Ok(())
}

fn validate_key_table(table: J3dKeyTable, bank_len: usize) -> Result<()> {
    let stride = match table.key_count {
        0 => return Ok(()),
        1 => 1,
        _ if table.tangent_type == 0 => 3,
        _ if table.tangent_type == 1 => 4,
        _ => {
            return Err(unsupported(format!(
                "unsupported tangent type {}",
                table.tangent_type
            )));
        }
    };
    let end = table.value_offset as usize + table.key_count as usize * stride;
    if end > bank_len {
        return Err(invalid(end, bank_len));
    }
    Ok(())
}

fn read_name_table(
    bytes: &[u8],
    coverage: &mut Coverage,
    offset: u32,
    expected_count: usize,
) -> Result<J3dAnimationNameTable> {
    if expected_count == 0 && offset == 0 {
        return Ok(J3dAnimationNameTable {
            entries: Vec::new(),
        });
    }
    let offset = offset as usize;
    coverage.mark(offset, 4 + checked_mul(expected_count, 4)?)?;
    let count = be_u16(bytes, offset, FORMAT)? as usize;
    if count != expected_count {
        return Err(unsupported(format!(
            "name table count {count} differs from expected {expected_count}"
        )));
    }
    let mut entries = Vec::with_capacity(count);
    for index in 0..count {
        let entry = offset + 4 + index * 4;
        let stored_hash = be_u16(bytes, entry, FORMAT)?;
        let relative_offset = be_u16(bytes, entry + 2, FORMAT)?;
        let start = offset + relative_offset as usize;
        if start >= bytes.len() {
            return Err(invalid(start, bytes.len()));
        }
        let end = bytes[start..]
            .iter()
            .position(|byte| *byte == 0)
            .map(|length| start + length)
            .ok_or_else(|| invalid(start, bytes.len()))?;
        coverage.mark(start, end - start + 1)?;
        let encoded = &bytes[start..end];
        let (decoded, had_errors) = SHIFT_JIS.decode_without_bom_handling(encoded);
        if had_errors {
            return Err(unsupported("name table contains invalid Shift-JIS"));
        }
        let name = decoded.into_owned();
        let (roundtrip, _, had_errors) = SHIFT_JIS.encode(&name);
        if had_errors || roundtrip.as_ref() != encoded {
            return Err(unsupported(
                "name table spelling is not canonical Shift-JIS",
            ));
        }
        let expected_hash = j3d_name_hash_encoded(encoded);
        if stored_hash != expected_hash {
            return Err(unsupported(format!(
                "name table entry {index} stores hash {stored_hash:#06x}, but {:?} derives {expected_hash:#06x}",
                name
            )));
        }
        entries.push(J3dAnimationNameEntry {
            relative_offset,
            name,
        });
    }
    Ok(J3dAnimationNameTable { entries })
}

fn write_name_table(bytes: &mut [u8], offset: u32, table: &J3dAnimationNameTable) -> Result<()> {
    if table.entries.is_empty() && offset == 0 {
        return Ok(());
    }
    let offset = offset as usize;
    put_u16(bytes, offset, to_u16(table.entries.len(), "name count")?)?;
    put_u16(bytes, offset + 2, 0xFFFF)?;
    for (index, entry) in table.entries.iter().enumerate() {
        let descriptor = offset + 4 + index * 4;
        let (encoded, _, had_errors) = SHIFT_JIS.encode(&entry.name);
        if had_errors {
            return Err(unsupported("name cannot be represented in Shift-JIS"));
        }
        put_u16(bytes, descriptor, j3d_name_hash_encoded(encoded.as_ref()))?;
        put_u16(bytes, descriptor + 2, entry.relative_offset)?;
        put_bytes(
            bytes,
            offset + entry.relative_offset as usize,
            encoded.as_ref(),
        )?;
        put_byte(
            bytes,
            offset + entry.relative_offset as usize + encoded.len(),
            0,
        )?;
    }
    Ok(())
}

fn j3d_name_hash_encoded(encoded: &[u8]) -> u16 {
    encoded.iter().fold(0_u16, |hash, byte| {
        hash.wrapping_mul(3).wrapping_add(u16::from(*byte))
    })
}

struct Coverage {
    bytes: Vec<bool>,
}

impl Coverage {
    fn new(len: usize) -> Self {
        Self {
            bytes: vec![false; len],
        }
    }

    fn mark(&mut self, offset: usize, length: usize) -> Result<()> {
        let end = offset
            .checked_add(length)
            .ok_or_else(|| invalid(offset, self.bytes.len()))?;
        if end > self.bytes.len() {
            return Err(invalid(end, self.bytes.len()));
        }
        self.bytes[offset..end].fill(true);
        Ok(())
    }

    fn classify_uncovered(
        &self,
        source: &[u8],
        start: usize,
        allow_ttk1_residue: bool,
    ) -> Result<(
        Vec<J3dAnimationPaddingRegion>,
        Vec<J3dTextureSrtResidueRegion>,
    )> {
        let mut padding = Vec::new();
        let mut residue = Vec::new();
        let mut offset = start;
        while offset < self.bytes.len() {
            if self.bytes[offset] {
                offset += 1;
                continue;
            }
            let end = self.bytes[offset..]
                .iter()
                .position(|covered| *covered)
                .map_or(self.bytes.len(), |length| offset + length);
            let data = &source[offset..end];
            let style = if data.iter().all(|byte| *byte == 0) {
                Some(J3dAnimationPaddingStyle::Zero)
            } else if data.iter().all(|byte| *byte == 0xFF) {
                Some(J3dAnimationPaddingStyle::Ff)
            } else if data
                .iter()
                .enumerate()
                .all(|(index, byte)| *byte == RETAIL_PADDING[index % RETAIL_PADDING.len()])
            {
                Some(J3dAnimationPaddingStyle::RetailPhrase)
            } else {
                None
            };
            if let Some(style) = style {
                padding.push(J3dAnimationPaddingRegion {
                    section_offset: to_u32(offset, "padding offset")?,
                    length: to_u32(end - offset, "padding length")?,
                    style,
                });
            } else if allow_ttk1_residue && offset.is_multiple_of(4) {
                residue.push(J3dTextureSrtResidueRegion {
                    section_offset: to_u32(offset, "TTK1 creator residue offset")?,
                    words: J3dTextureSrtResidueWords::parse(data)?,
                });
            } else {
                return Err(unsupported(format!(
                    "unmodelled non-padding bytes at section range {offset:#x}..{end:#x}"
                )));
            }
            offset = end;
        }
        Ok((padding, residue))
    }
}

fn parse_header_tag(bytes: &[u8]) -> Result<J3dAnimationHeaderTag> {
    if bytes == [0xFF; 16] {
        Ok(J3dAnimationHeaderTag::AllFf)
    } else if bytes[..4] == *b"SVR1" && bytes[4..] == [0xFF; 12] {
        Ok(J3dAnimationHeaderTag::Svr1)
    } else if bytes == [0; 16] {
        Ok(J3dAnimationHeaderTag::AllZero)
    } else {
        Err(unsupported("unrecognized 16-byte J3D file header tag"))
    }
}

fn write_header_tag(bytes: &mut [u8], tag: J3dAnimationHeaderTag) {
    match tag {
        J3dAnimationHeaderTag::AllFf => bytes.fill(0xFF),
        J3dAnimationHeaderTag::Svr1 => {
            bytes.fill(0xFF);
            bytes[..4].copy_from_slice(b"SVR1");
        }
        J3dAnimationHeaderTag::AllZero => bytes.fill(0),
    }
}

fn fill_padding(bytes: &mut [u8], region: &J3dAnimationPaddingRegion) -> Result<()> {
    let start = region.section_offset as usize;
    let length = region.length as usize;
    let end = start
        .checked_add(length)
        .ok_or_else(|| invalid(start, bytes.len()))?;
    if end > bytes.len() {
        return Err(invalid(end, bytes.len()));
    }
    match region.style {
        J3dAnimationPaddingStyle::Zero => bytes[start..end].fill(0),
        J3dAnimationPaddingStyle::Ff => bytes[start..end].fill(0xFF),
        J3dAnimationPaddingStyle::RetailPhrase => {
            for (index, byte) in bytes[start..end].iter_mut().enumerate() {
                *byte = RETAIL_PADDING[index % RETAIL_PADDING.len()];
            }
        }
    }
    Ok(())
}

fn read_offsets<const N: usize>(bytes: &[u8], fields: [usize; N]) -> Result<[u32; N]> {
    let mut offsets = [0; N];
    for (index, field) in fields.into_iter().enumerate() {
        offsets[index] = be_u32(bytes, field, FORMAT)?;
    }
    Ok(offsets)
}

fn write_offsets<const N: usize>(
    bytes: &mut [u8],
    fields: [usize; N],
    offsets: [u32; N],
) -> Result<()> {
    for (field, offset) in fields.into_iter().zip(offsets) {
        put_u32(bytes, field, offset)?;
    }
    Ok(())
}

fn read_u8_bank(
    bytes: &[u8],
    coverage: &mut Coverage,
    offset: u32,
    count: usize,
) -> Result<Vec<u8>> {
    if count == 0 {
        return Ok(Vec::new());
    }
    coverage.mark(offset as usize, count)?;
    Ok(bytes[offset as usize..offset as usize + count].to_vec())
}

fn read_u16_bank(
    bytes: &[u8],
    coverage: &mut Coverage,
    offset: u32,
    count: usize,
) -> Result<Vec<u16>> {
    if count == 0 {
        return Ok(Vec::new());
    }
    coverage.mark(offset as usize, checked_mul(count, 2)?)?;
    (0..count)
        .map(|index| be_u16(bytes, offset as usize + index * 2, FORMAT))
        .collect()
}

fn read_i16_bank(
    bytes: &[u8],
    coverage: &mut Coverage,
    offset: u32,
    count: usize,
) -> Result<Vec<i16>> {
    if count == 0 {
        return Ok(Vec::new());
    }
    coverage.mark(offset as usize, checked_mul(count, 2)?)?;
    (0..count)
        .map(|index| be_i16(bytes, offset as usize + index * 2, FORMAT))
        .collect()
}

fn read_f32_bank(
    bytes: &[u8],
    coverage: &mut Coverage,
    offset: u32,
    count: usize,
) -> Result<Vec<f32>> {
    if count == 0 {
        return Ok(Vec::new());
    }
    coverage.mark(offset as usize, checked_mul(count, 4)?)?;
    (0..count)
        .map(|index| be_f32(bytes, offset as usize + index * 4, FORMAT))
        .collect()
}

fn read_vec3_bank(
    bytes: &[u8],
    coverage: &mut Coverage,
    offset: u32,
    count: usize,
) -> Result<Vec<[f32; 3]>> {
    if count == 0 {
        return Ok(Vec::new());
    }
    coverage.mark(offset as usize, checked_mul(count, 12)?)?;
    (0..count)
        .map(|index| {
            let base = offset as usize + index * 12;
            Ok([
                be_f32(bytes, base, FORMAT)?,
                be_f32(bytes, base + 4, FORMAT)?,
                be_f32(bytes, base + 8, FORMAT)?,
            ])
        })
        .collect()
}

fn write_u8_bank(bytes: &mut [u8], offset: u32, values: &[u8]) -> Result<()> {
    if values.is_empty() {
        return Ok(());
    }
    put_bytes(bytes, offset as usize, values)
}

fn write_u16_bank(bytes: &mut [u8], offset: u32, values: &[u16]) -> Result<()> {
    for (index, value) in values.iter().enumerate() {
        put_u16(bytes, offset as usize + index * 2, *value)?;
    }
    Ok(())
}

fn write_i16_bank(bytes: &mut [u8], offset: u32, values: &[i16]) -> Result<()> {
    for (index, value) in values.iter().enumerate() {
        put_i16(bytes, offset as usize + index * 2, *value)?;
    }
    Ok(())
}

fn write_f32_bank(bytes: &mut [u8], offset: u32, values: &[f32]) -> Result<()> {
    for (index, value) in values.iter().enumerate() {
        put_u32(bytes, offset as usize + index * 4, value.to_bits())?;
    }
    Ok(())
}

fn write_vec3_bank(bytes: &mut [u8], offset: u32, values: &[[f32; 3]]) -> Result<()> {
    for (index, value) in values.iter().enumerate() {
        for (component, scalar) in value.iter().enumerate() {
            put_u32(
                bytes,
                offset as usize + index * 12 + component * 4,
                scalar.to_bits(),
            )?;
        }
    }
    Ok(())
}

fn put_byte(bytes: &mut [u8], offset: usize, value: u8) -> Result<()> {
    if offset >= bytes.len() {
        return Err(invalid(offset, bytes.len()));
    }
    bytes[offset] = value;
    Ok(())
}

fn put_bytes(bytes: &mut [u8], offset: usize, value: &[u8]) -> Result<()> {
    let end = offset
        .checked_add(value.len())
        .ok_or_else(|| invalid(offset, bytes.len()))?;
    if end > bytes.len() {
        return Err(invalid(end, bytes.len()));
    }
    bytes[offset..end].copy_from_slice(value);
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

fn checked_mul(value: usize, stride: usize) -> Result<usize> {
    value
        .checked_mul(stride)
        .ok_or_else(|| invalid(value, stride))
}

fn to_u16(value: usize, resource: &'static str) -> Result<u16> {
    u16::try_from(value).map_err(|_| FormatError::ResourceLimit {
        format: FORMAT,
        resource,
        requested: value,
        limit: u16::MAX as usize,
    })
}

fn to_u32(value: usize, resource: &'static str) -> Result<u32> {
    u32::try_from(value).map_err(|_| FormatError::ResourceLimit {
        format: FORMAT,
        resource,
        requested: value,
        limit: u32::MAX as usize,
    })
}

fn require_equal(actual: usize, expected: usize, what: &str) -> Result<()> {
    if actual != expected {
        return Err(unsupported(format!(
            "{what}: expected {expected}, got {actual}"
        )));
    }
    Ok(())
}

fn invalid(offset: usize, len: usize) -> FormatError {
    FormatError::InvalidOffset {
        format: FORMAT,
        offset,
        len,
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

    #[test]
    fn j3d_name_hash_is_derived_from_the_encoded_name() {
        assert_eq!(j3d_name_hash_encoded(b"material"), 0x56bf);
        // Shift-JIS bytes for `Mario` in katakana. Hashing Unicode scalar
        // values instead would produce a different on-disk descriptor.
        assert_eq!(
            j3d_name_hash_encoded(&[0x83, 0x7D, 0x83, 0x8A, 0x83, 0x49]),
            0xB863
        );
        assert_ne!(
            j3d_name_hash_encoded(b"material"),
            j3d_name_hash_encoded(b"Material")
        );
    }

    #[test]
    fn name_table_writer_recomputes_hash_after_a_name_edit() {
        let mut table = J3dAnimationNameTable {
            entries: vec![J3dAnimationNameEntry {
                relative_offset: 8,
                name: "material".to_owned(),
            }],
        };
        let mut bytes = [0_u8; 64];
        write_name_table(&mut bytes, 0, &table).expect("write original name table");
        assert_eq!(be_u16(&bytes, 4, FORMAT).expect("stored hash"), 0x56bf);

        table.entries[0].name = "Material".to_owned();
        bytes.fill(0);
        write_name_table(&mut bytes, 0, &table).expect("write edited name table");
        assert_eq!(
            be_u16(&bytes, 4, FORMAT).expect("edited hash"),
            j3d_name_hash_encoded(b"Material")
        );
    }

    #[test]
    fn name_table_reader_rejects_a_noncanonical_stored_hash() {
        let table = J3dAnimationNameTable {
            entries: vec![J3dAnimationNameEntry {
                relative_offset: 8,
                name: "material".to_owned(),
            }],
        };
        let mut bytes = [0_u8; 64];
        write_name_table(&mut bytes, 0, &table).expect("write name table");
        bytes[4..6].copy_from_slice(&0x1234_u16.to_be_bytes());
        let mut coverage = Coverage::new(bytes.len());
        let error = read_name_table(&bytes, &mut coverage, 0, 1)
            .expect_err("stored hash is not derived from its encoded name");
        assert!(error.to_string().contains("stores hash 0x1234"));
    }

    #[test]
    fn ttk1_creator_word_is_transparent_typed_reconstruction_metadata() {
        for word in [0, 1, 0x5468_6973, 0x8015_0C68, 0x8015_14A8, u32::MAX] {
            let metadata = J3dTextureSrtCreatorWord(word);
            assert_eq!(metadata.0, word);
        }
    }

    #[test]
    fn ttk1_residue_words_are_alignment_and_section_bounded() {
        let mut section = [0_u8; 0x68];
        let valid = J3dTextureSrtResidueRegion {
            section_offset: 0x60,
            words: J3dTextureSrtResidueWords(vec![0x1234_5678, 0x9ABC_DEF0]),
        };
        write_ttk1_residue_region(&mut section, &valid).expect("bounded word region");
        assert_eq!(
            &section[0x60..0x68],
            &[0x12, 0x34, 0x56, 0x78, 0x9A, 0xBC, 0xDE, 0xF0]
        );

        for invalid in [
            J3dTextureSrtResidueRegion {
                section_offset: 0x5C,
                words: J3dTextureSrtResidueWords(vec![1]),
            },
            J3dTextureSrtResidueRegion {
                section_offset: 0x61,
                words: J3dTextureSrtResidueWords(vec![1]),
            },
            J3dTextureSrtResidueRegion {
                section_offset: 0x64,
                words: J3dTextureSrtResidueWords(vec![1, 2]),
            },
            J3dTextureSrtResidueRegion {
                section_offset: 0x60,
                words: J3dTextureSrtResidueWords(Vec::new()),
            },
        ] {
            write_ttk1_residue_region(&mut section, &invalid)
                .expect_err("invalid residue region must be rejected");
        }
        J3dTextureSrtResidueWords::parse(&[0; 3])
            .expect_err("partial creator words are not typed reconstruction metadata");
    }

    fn fixture(extension: &str) -> std::path::PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .join(format!("target/rebuild-audit.{extension}"))
    }

    #[test]
    fn source_free_rebuilds_local_animation_audit_fixtures() {
        for extension in ["bck", "btp", "btk", "bpk", "brk"] {
            let path = fixture(extension);
            if !path.is_file() {
                continue;
            }
            let mut source = std::fs::read(&path).expect("read local animation fixture");
            let expected = source.clone();
            let document = J3dAnimationRebuildDocument::parse(&source)
                .unwrap_or_else(|error| panic!("parse {}: {error}", path.display()));
            source.fill(0);
            let rebuilt = document.encode().expect("encode semantic animation");
            assert_eq!(rebuilt, expected, "source-free {extension} rebuild differs");
        }
    }

    #[test]
    fn local_btk_uses_typed_reconstruction_metadata() {
        let path = fixture("btk");
        if !path.is_file() {
            return;
        }
        let mut original = std::fs::read(path).expect("read local BTK fixture");
        let expected = original.clone();
        let document = J3dAnimationRebuildDocument::parse(&original).expect("parse local BTK");
        let J3dAnimationSection::TextureSrt(texture_srt) = &document.section else {
            panic!("BTK parsed as the wrong animation section");
        };
        assert_ne!(
            texture_srt.reconstruction.creator_word,
            J3dTextureSrtCreatorWord(0)
        );
        assert!(!texture_srt.reconstruction.residue_regions.is_empty());
        original.fill(0);
        assert_eq!(document.encode().expect("rebuild local BTK"), expected);
    }

    #[test]
    #[ignore = "requires SMS_BASE_ROOT with extracted retail stage archives"]
    fn source_free_rebuilds_every_retail_stage_animation() {
        let root = std::env::var_os("SMS_BASE_ROOT")
            .map(std::path::PathBuf::from)
            .expect("set SMS_BASE_ROOT to an extracted retail game root");
        let archives = crate::discover_scene_archives(root).expect("discover stage archives");
        let mut total_animations = 0usize;
        let mut total_btk = 0usize;
        let mut rebuilt = 0usize;
        let mut rejected_creator_word = 0usize;
        let mut rejected_residue = 0usize;
        let mut creator_and_residue = 0usize;
        let mut typed_residue_files = 0usize;
        let mut typed_residue_regions = 0usize;
        let mut residue_region_lengths = std::collections::BTreeMap::<usize, usize>::new();
        let mut residue_patterns = std::collections::BTreeMap::<String, usize>::new();
        let mut unknown_gap_lengths = std::collections::BTreeMap::<usize, usize>::new();
        let mut unknown_gap_patterns = std::collections::BTreeMap::<String, usize>::new();
        let mut section_size_deltas = std::collections::BTreeMap::<isize, usize>::new();
        let mut creator_words = std::collections::BTreeMap::<u32, usize>::new();
        let mut rejected_archives = std::collections::BTreeSet::<String>::new();
        let mut creator_word_archives = std::collections::BTreeSet::<String>::new();
        let mut residue_archives = std::collections::BTreeSet::<String>::new();
        let mut independently_residual_archives = std::collections::BTreeSet::<String>::new();
        let mut failures = Vec::new();
        for archive_info in archives {
            let source = std::fs::read(&archive_info.path).expect("read stage archive");
            let decoded = if source.starts_with(b"Yaz0") {
                crate::decode_yaz0(&source).expect("decode stage archive")
            } else {
                source
            };
            let archive = crate::RarcArchive::parse(decoded).expect("parse stage archive");
            for entry in archive.file_entries() {
                let path = entry.path.to_ascii_lowercase();
                if ![".bck", ".btp", ".btk", ".bpk", ".brk"]
                    .iter()
                    .any(|extension| path.ends_with(extension))
                {
                    continue;
                }
                let mut original = archive
                    .file_bytes_raw(&entry.raw_path)
                    .expect("read animation entry");
                total_animations += 1;
                let is_btk = path.ends_with(".btk");
                let creator_word = if is_btk && original.len() >= 0x80 {
                    total_btk += 1;
                    let word = u32::from_be_bytes(
                        original[0x7C..0x80].try_into().expect("TTK1 creator word"),
                    );
                    *creator_words.entry(word).or_default() += 1;
                    Some(word)
                } else {
                    None
                };
                let has_creator_residue = creator_word.is_some_and(|word| word != 0);
                let mut canonical_creator_probe = original.to_vec();
                if is_btk && canonical_creator_probe.len() >= 0x80 {
                    canonical_creator_probe[0x7C..0x80].fill(0);
                }
                let has_other_residue =
                    J3dAnimationRebuildDocument::parse(&canonical_creator_probe)
                        .ok()
                        .and_then(|document| match document.section {
                            J3dAnimationSection::TextureSrt(texture_srt) => {
                                Some(!texture_srt.reconstruction.residue_regions.is_empty())
                            }
                            _ => None,
                        })
                        .unwrap_or(false);
                if has_creator_residue && has_other_residue {
                    creator_and_residue += 1;
                }
                if has_other_residue {
                    independently_residual_archives.insert(archive_info.path.display().to_string());
                }
                match J3dAnimationRebuildDocument::parse(&original) {
                    Ok(document) => {
                        if let J3dAnimationSection::TextureSrt(texture_srt) = &document.section {
                            if !texture_srt.reconstruction.residue_regions.is_empty() {
                                typed_residue_files += 1;
                            }
                            for region in &texture_srt.reconstruction.residue_regions {
                                typed_residue_regions += 1;
                                *residue_region_lengths
                                    .entry(region.words.as_slice().len() * 4)
                                    .or_default() += 1;
                            }
                        }
                        let expected = original.clone();
                        original.fill(0);
                        match document.encode() {
                            Ok(encoded) if encoded == expected => {
                                match J3dAnimationRebuildDocument::parse(&encoded)
                                    .and_then(|reparsed| reparsed.encode())
                                {
                                    Ok(second) if second == encoded => rebuilt += 1,
                                    Ok(_) => failures.push(format!(
                                        "{}!/{}: second rebuild mismatch",
                                        archive_info.path.display(),
                                        entry.path
                                    )),
                                    Err(error) => failures.push(format!(
                                        "{}!/{}: second rebuild: {error}",
                                        archive_info.path.display(),
                                        entry.path
                                    )),
                                }
                            }
                            Ok(_) => failures.push(format!(
                                "{}!/{}: byte mismatch",
                                archive_info.path.display(),
                                entry.path
                            )),
                            Err(error) => failures.push(format!(
                                "{}!/{}: encode: {error}",
                                archive_info.path.display(),
                                entry.path
                            )),
                        }
                    }
                    Err(FormatError::Unsupported { message, .. })
                        if message.contains("unmodelled non-padding bytes")
                            || message.contains("loader-ignored TTK1 creator word") =>
                    {
                        let archive_name = archive_info.path.display().to_string();
                        rejected_archives.insert(archive_name.clone());
                        if message.contains("loader-ignored TTK1 creator word") {
                            rejected_creator_word += 1;
                            creator_word_archives.insert(archive_name);
                        } else {
                            rejected_residue += 1;
                            residue_archives.insert(archive_name);
                        }
                        let declared_section = u32::from_be_bytes(
                            original[0x24..0x28].try_into().expect("section size"),
                        ) as usize;
                        let actual_section = original.len().saturating_sub(FILE_HEADER_SIZE);
                        let trailer = original
                            .get(FILE_HEADER_SIZE + declared_section..)
                            .unwrap_or_default();
                        let reserved_word = if path.ends_with(".btk") && original.len() >= 0x80 {
                            u32::from_be_bytes(
                                original[0x7C..0x80].try_into().expect("TTK1 reserved word"),
                            )
                        } else {
                            0
                        };
                        let trailer_hex = trailer
                            .iter()
                            .map(|byte| format!("{byte:02x}"))
                            .collect::<String>();
                        let extension = path.rsplit('.').next().unwrap_or("?");
                        *section_size_deltas
                            .entry(actual_section as isize - declared_section as isize)
                            .or_default() += 1;
                        if let Some(range) =
                            message.strip_prefix("unmodelled non-padding bytes at section range 0x")
                        {
                            if let Some((start, end)) = range.split_once("..0x") {
                                if let (Ok(start), Ok(end)) = (
                                    usize::from_str_radix(start, 16),
                                    usize::from_str_radix(end, 16),
                                ) {
                                    let unknown =
                                        &original[FILE_HEADER_SIZE + start..FILE_HEADER_SIZE + end];
                                    *unknown_gap_lengths.entry(unknown.len()).or_default() += 1;
                                    let pattern = unknown
                                        .iter()
                                        .map(|byte| format!("{byte:02x}"))
                                        .collect::<String>();
                                    *unknown_gap_patterns.entry(pattern).or_default() += 1;
                                }
                            }
                        }
                        *residue_patterns
                            .entry(format!(
                                "{extension}: actual={actual_section:#x} declared={declared_section:#x} tail={trailer_hex} reserved={reserved_word:#010x}"
                            ))
                            .or_default() += 1;
                    }
                    Err(error) => failures.push(format!(
                        "{}!/{}: parse: {error}",
                        archive_info.path.display(),
                        entry.path
                    )),
                }
            }
        }
        assert!(
            total_animations > 0,
            "retail animation census found no supported files"
        );
        assert!(
            failures.is_empty(),
            "{} semantic animation failure(s); rebuilt={rebuilt}, rejected creator word={rejected_creator_word}, rejected other residue={rejected_residue}:\n{}",
            failures.len(),
            failures.into_iter().take(20).collect::<Vec<_>>().join("\n")
        );
        assert_eq!(
            total_animations, 20_456,
            "retail animation inventory count drifted"
        );
        assert_eq!(total_btk, 945, "retail BTK inventory count drifted");
        assert_eq!(
            rebuilt, 20_456,
            "source-free byte-exact animation rebuild count drifted"
        );
        assert_eq!(
            rejected_creator_word, 0,
            "typed TTK1 creator words must not reject retail animations"
        );
        assert_eq!(
            rejected_residue, 0,
            "typed TTK1 residue regions must not reject retail animations"
        );
        assert_eq!(
            creator_and_residue, 931,
            "BTKs with both a creator word and additional residue drifted"
        );
        assert_eq!(
            creator_words,
            [(0x5468_6973, 40), (0x8015_0C68, 1), (0x8015_14A8, 904)]
                .into_iter()
                .collect(),
            "retail loader-ignored TTK1 creator-word distribution drifted"
        );
        assert_eq!(
            typed_residue_files, 931,
            "BTKs carrying typed reconstruction residue drifted"
        );
        assert_eq!(
            typed_residue_regions, 931,
            "typed TTK1 reconstruction-region count drifted"
        );
        assert_eq!(
            residue_region_lengths,
            [
                (4, 28),
                (8, 85),
                (12, 218),
                (16, 74),
                (20, 324),
                (24, 140),
                (28, 62)
            ]
            .into_iter()
            .collect(),
            "typed TTK1 reconstruction-region length distribution drifted"
        );
        assert!(rejected_archives.is_empty());
        assert!(creator_word_archives.is_empty());
        assert_eq!(
            independently_residual_archives.len(),
            106,
            "stage archives with additional BTK creator residue drifted"
        );
        assert_eq!(
            rebuilt + rejected_creator_word + rejected_residue,
            total_animations,
            "strict census did not classify every animation"
        );
        eprintln!(
            "source-free animations total={total_animations}, rebuilt={rebuilt}, typed_residue_files={typed_residue_files}, typed_residue_regions={typed_residue_regions}, rejected_creator_word={rejected_creator_word}, rejected_other_residue={rejected_residue}"
        );
        eprintln!("BTK creator words across all files: {creator_words:?}");
        eprintln!("typed TTK1 residue region lengths: {residue_region_lengths:?}");
        eprintln!(
            "affected archives: any={}, creator_word={}, strict_other_residue={}, independently_other_residue={}",
            rejected_archives.len(),
            creator_word_archives.len(),
            residue_archives.len(),
            independently_residual_archives.len()
        );
        eprintln!("section actual-minus-declared sizes: {section_size_deltas:?}");
        eprintln!("first unknown gap lengths: {unknown_gap_lengths:?}");
        eprintln!(
            "distinct first unknown gap patterns={}",
            unknown_gap_patterns.len()
        );
        let mut diagnostic = String::new();
        use std::fmt::Write as _;
        for (pattern, count) in residue_patterns {
            let _ = writeln!(diagnostic, "residue count={count}: {pattern}");
        }
        let _ = writeln!(diagnostic, "\nfirst unknown gap patterns:");
        for (pattern, count) in unknown_gap_patterns {
            let _ = writeln!(diagnostic, "count={count} bytes={pattern}");
        }
        let diagnostic_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .join("target/rebuild-audit-animation-residue.txt");
        std::fs::write(diagnostic_path, diagnostic).expect("write residue census diagnostic");
    }
}
