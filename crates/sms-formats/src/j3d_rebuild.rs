//! Source-free J3D model reconstruction.
//!
//! `J3dFile` is the renderer-oriented reader.  It deliberately keeps a view of
//! the input because decoding a model for display is considerably cheaper that
//! way.  This module is the complementary authoring representation: every byte
//! that is emitted is regenerated from typed J3D/GX values and explicit layout
//! metadata.  It never retains the input file or an opaque copy of a section.

use std::collections::{BTreeMap, BTreeSet};

use encoding_rs::SHIFT_JIS;
use serde::{Deserialize, Serialize};

use crate::binary::{be_f32, be_i16, be_u16, be_u32, checked_slice, require_len, require_magic};
use crate::{FormatError, Result};

const FORMAT: &str = "J3D rebuild";
const RETAIL_PADDING: &[u8] = b"This is padding data to alignment.";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct J3dRebuildDocument {
    pub file_type: [u8; 4],
    pub version_tag: [u8; 4],
    pub reserved_words: [u32; 3],
    pub declared_section_count: u32,
    pub sections: Vec<J3dRebuildSection>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct J3dRebuildSection {
    pub declared_size: u32,
    pub data: J3dRebuildSectionData,
    pub padding: Vec<J3dPaddingSpan>,
}

impl J3dRebuildSection {
    pub fn tag(&self) -> [u8; 4] {
        match &self.data {
            J3dRebuildSectionData::Information(_) => *b"INF1",
            J3dRebuildSectionData::Vertices(_) => *b"VTX1",
            J3dRebuildSectionData::Envelopes(_) => *b"EVP1",
            J3dRebuildSectionData::DrawMatrices(_) => *b"DRW1",
            J3dRebuildSectionData::Joints(_) => *b"JNT1",
            J3dRebuildSectionData::Shapes(_) => *b"SHP1",
            J3dRebuildSectionData::Materials(_) => *b"MAT3",
            J3dRebuildSectionData::Textures(_) => *b"TEX1",
            J3dRebuildSectionData::MaterialDisplayLists(_) => *b"MDL3",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum J3dRebuildSectionData {
    Information(J3dInformationSection),
    Vertices(J3dVertexSection),
    Envelopes(J3dEnvelopeSection),
    DrawMatrices(J3dDrawMatrixSection),
    Joints(J3dJointSection),
    Shapes(J3dShapeSection),
    Materials(J3dMaterialSection),
    Textures(J3dTextureSection),
    MaterialDisplayLists(J3dMaterialDisplayListSection),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct J3dInformationSection {
    pub flags: u16,
    pub reserved: u16,
    pub packet_count: u32,
    pub vertex_count: u32,
    pub hierarchy_offset: u32,
    pub hierarchy: Vec<J3dHierarchyCommand>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct J3dHierarchyCommand {
    pub node_type: u16,
    pub index: u16,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct J3dVertexSection {
    /// Attribute-format list followed by the thirteen J3D vertex arrays.
    pub offsets: [u32; 14],
    pub formats: Vec<J3dVertexAttributeFormat>,
    pub arrays: Vec<J3dVertexArray>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct J3dVertexAttributeFormat {
    pub attribute: u32,
    pub component_count: u32,
    pub component_type: u32,
    pub fractional_bits: u8,
    pub reserved: [u8; 3],
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct J3dVertexArray {
    pub attribute: J3dVertexArrayAttribute,
    pub offset: u32,
    /// The allocation is typed according to the VAT component encoding.  It
    /// includes unused allocation capacity because offsets are part of the
    /// model's serialization contract.
    pub values: J3dScalarArray,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum J3dVertexArrayAttribute {
    Position,
    Normal,
    NormalBinormalTangent,
    Color0,
    Color1,
    TexCoord(u8),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum J3dScalarArray {
    Unsigned8(Vec<u8>),
    Signed8(Vec<i8>),
    Unsigned16(Vec<u16>),
    Signed16(Vec<i16>),
    Unsigned32(Vec<u32>),
    Float32Bits(Vec<u32>),
    /// GX colors are packed encodings, not decoded RGBA pixels.
    PackedColor(Vec<u8>),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct J3dEnvelopeSection {
    pub envelope_count: u16,
    pub reserved: u16,
    pub offsets: [u32; 4],
    pub mix_counts: Vec<u8>,
    pub joint_indices: Vec<u16>,
    pub weights: Vec<f32>,
    pub inverse_bind_matrices: Vec<J3dMatrix34Record>,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct J3dMatrix34Record {
    pub rows: [[f32; 4]; 3],
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct J3dDrawMatrixSection {
    pub matrix_count: u16,
    pub reserved: u16,
    pub flag_offset: u32,
    pub index_offset: u32,
    pub weighted_flags: Vec<u8>,
    pub indices: Vec<u16>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct J3dJointSection {
    pub joint_count: u16,
    pub reserved: u16,
    pub init_offset: u32,
    pub remap_offset: u32,
    pub name_table_offset: u32,
    pub joints: Vec<J3dJointRecord>,
    pub remap: Vec<u16>,
    pub names: J3dNameTable,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct J3dJointRecord {
    pub matrix_type: u16,
    pub scale_compensate: u8,
    pub reserved: u8,
    pub scale: [f32; 3],
    pub rotation: [i16; 3],
    pub rotation_padding: i16,
    pub translation: [f32; 3],
    pub radius: f32,
    pub bounds_min: [f32; 3],
    pub bounds_max: [f32; 3],
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct J3dNameTable {
    pub reserved: u16,
    pub entries: Vec<J3dNameEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct J3dNameEntry {
    pub hash: u16,
    pub string_offset: u16,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct J3dShapeSection {
    pub shape_count: u16,
    pub reserved: u16,
    pub offsets: [u32; 8],
    pub shapes: Vec<J3dShapeRecord>,
    pub remap: Vec<u16>,
    pub names: Option<J3dNameTable>,
    pub vertex_descriptor_sets: Vec<J3dVertexDescriptorSet>,
    pub matrix_table: Vec<u16>,
    pub matrix_groups: Vec<J3dShapeMatrixGroup>,
    pub draws: Vec<J3dShapeDraw>,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct J3dShapeRecord {
    pub matrix_type: u8,
    pub reserved_01: u8,
    pub matrix_group_count: u16,
    pub vertex_descriptor_offset: u16,
    pub matrix_group_start: u16,
    pub draw_start: u16,
    pub reserved_0a: u16,
    pub radius: f32,
    pub bounds_min: [f32; 3],
    pub bounds_max: [f32; 3],
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct J3dVertexDescriptorSet {
    pub relative_offset: u16,
    pub descriptors: Vec<J3dVertexDescriptor>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct J3dVertexDescriptor {
    pub attribute: u32,
    pub input_type: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct J3dShapeMatrixGroup {
    pub matrix_index: u16,
    pub matrix_count: u16,
    pub first_matrix: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct J3dShapeDraw {
    pub display_list_size: u32,
    pub display_list_offset: u32,
    pub commands: Vec<J3dGxCommand>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum J3dGxCommand {
    Nop,
    Primitive {
        opcode: u8,
        vertex_count: u16,
        vertices: Vec<Vec<J3dGxVertexOperand>>,
    },
    LoadCp {
        register: u8,
        value: u32,
    },
    LoadBp {
        value: u32,
    },
    LoadXf {
        address: u16,
        values: Vec<u32>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum J3dGxVertexOperand {
    DirectU8(u8),
    Index8(u8),
    Index16(u16),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct J3dMaterialSection {
    pub material_count: u16,
    pub reserved: u16,
    pub offsets: [u32; 30],
    pub material_init_records: Vec<J3dMaterialInitRecord>,
    pub names: Option<J3dNameTable>,
    pub tables: Vec<J3dMaterialTable>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct J3dMaterialInitRecord {
    pub material_mode: u8,
    pub cull_mode_index: u8,
    pub color_channel_count_index: u8,
    pub tex_gen_count_index: u8,
    pub tev_stage_count_index: u8,
    pub z_compare_location_index: u8,
    pub z_mode_index: u8,
    pub dither_index: u8,
    pub material_color_indices: [u16; 2],
    pub color_channel_indices: [u16; 4],
    pub ambient_color_indices: [u16; 2],
    pub light_indices: [u16; 8],
    pub tex_coord_indices: [u16; 8],
    pub post_tex_coord_indices: [u16; 8],
    pub tex_matrix_indices: [u16; 10],
    pub post_tex_matrix_indices: [u16; 20],
    pub texture_number_indices: [u16; 8],
    pub tev_konst_color_indices: [u16; 4],
    pub tev_konst_color_selectors: [u8; 16],
    pub tev_konst_alpha_selectors: [u8; 16],
    pub tev_order_indices: [u16; 16],
    pub tev_color_indices: [u16; 4],
    pub tev_stage_indices: [u16; 16],
    pub tev_swap_mode_indices: [u16; 16],
    pub tev_swap_table_indices: [u16; 4],
    /// Six loader-ignored big-endian words at 0x12c..0x144. They are explicit
    /// scalar reconstruction metadata rather than an opaque byte payload.
    pub unused_tail_words: J3dMaterialUnusedTailWords,
    pub fog_index: u16,
    pub alpha_compare_index: u16,
    pub blend_index: u16,
    pub nbt_scale_index: u16,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct J3dMaterialUnusedTailWords {
    pub word_00: u32,
    pub word_04: u32,
    pub word_08: u32,
    pub word_0c: u32,
    pub word_10: u32,
    pub word_14: u32,
}

impl J3dMaterialUnusedTailWords {
    pub const ZERO: Self = Self {
        word_00: 0,
        word_04: 0,
        word_08: 0,
        word_0c: 0,
        word_10: 0,
        word_14: 0,
    };

    pub const ONES: Self = Self {
        word_00: u32::MAX,
        word_04: u32::MAX,
        word_08: u32::MAX,
        word_0c: u32::MAX,
        word_10: u32::MAX,
        word_14: u32::MAX,
    };

    fn from_be_bytes(bytes: &[u8], offset: usize) -> Result<Self> {
        Ok(Self {
            word_00: be_u32(bytes, offset, FORMAT)?,
            word_04: be_u32(bytes, offset + 4, FORMAT)?,
            word_08: be_u32(bytes, offset + 8, FORMAT)?,
            word_0c: be_u32(bytes, offset + 0x0c, FORMAT)?,
            word_10: be_u32(bytes, offset + 0x10, FORMAT)?,
            word_14: be_u32(bytes, offset + 0x14, FORMAT)?,
        })
    }

    fn write_be(self, out: &mut [u8], offset: usize) -> Result<()> {
        for (index, word) in [
            self.word_00,
            self.word_04,
            self.word_08,
            self.word_0c,
            self.word_10,
            self.word_14,
        ]
        .into_iter()
        .enumerate()
        {
            put_u32(out, offset + index * 4, word)?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct J3dMaterialTable {
    pub kind: J3dMaterialTableKind,
    pub offset: u32,
    pub allocation: J3dScalarArray,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum J3dMaterialTableKind {
    MaterialInit,
    MaterialRemap,
    Names,
    IndirectInit,
    CullMode,
    MaterialColor,
    ColorChannelCount,
    ColorChannel,
    AmbientColor,
    Light,
    TexGenCount,
    TexCoord,
    TexCoord2,
    TexMatrix,
    PostTexMatrix,
    TextureNumber,
    TevOrder,
    TevColor,
    TevKonstColor,
    TevStageCount,
    TevStage,
    TevSwapMode,
    TevSwapTable,
    Fog,
    AlphaCompare,
    Blend,
    ZMode,
    ZCompareLocation,
    Dither,
    NbtScale,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct J3dTextureSection {
    pub texture_count: u16,
    pub reserved: u16,
    pub header_offset: u32,
    pub name_table_offset: u32,
    pub textures: Vec<J3dTextureRecord>,
    pub names: J3dNameTable,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct J3dTextureRecord {
    pub header_relative_offset: u32,
    pub format: u8,
    pub transparency: u8,
    pub width: u16,
    pub height: u16,
    pub wrap_s: u8,
    pub wrap_t: u8,
    pub palette_enabled: u8,
    pub palette_format: u8,
    pub palette_entry_count: u16,
    pub palette_offset: u32,
    pub mipmap_enabled: u8,
    pub edge_lod: u8,
    pub bias_clamp: u8,
    pub max_anisotropy: u8,
    pub min_filter: u8,
    pub mag_filter: u8,
    pub min_lod: i8,
    pub max_lod: i8,
    pub mipmap_count: u8,
    pub reserved_19: u8,
    pub lod_bias: i16,
    pub image_offset: u32,
    /// Encoded tiled GX blocks are the texture's native semantic payload.
    pub encoded_mip_levels: Vec<J3dTextureBlock>,
    pub encoded_palette: Option<J3dTextureBlock>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct J3dTextureBlock {
    pub absolute_section_offset: u32,
    pub bytes: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct J3dMaterialDisplayListSection {
    pub material_count: u16,
    pub reserved: u16,
    pub offsets: Vec<u32>,
    pub allocations: Vec<J3dMaterialDisplayListAllocation>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct J3dMaterialDisplayListAllocation {
    pub offset: u32,
    pub words: Vec<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct J3dPaddingSpan {
    pub offset: u32,
    pub length: u32,
    pub kind: J3dPaddingKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum J3dPaddingKind {
    Zero,
    Ones,
    RetailMessage { phase: u8 },
}

impl J3dRebuildDocument {
    pub fn parse(bytes: impl AsRef<[u8]>) -> Result<Self> {
        let bytes = bytes.as_ref();
        require_len(FORMAT, bytes, 0x20)?;
        require_magic(FORMAT, bytes, b"J3D2")?;
        let file_size = be_u32(bytes, 0x08, FORMAT)? as usize;
        let declared_section_count = be_u32(bytes, 0x0c, FORMAT)?;
        if file_size != bytes.len() {
            return Err(FormatError::Unsupported {
                format: FORMAT,
                message: format!(
                    "declared size {file_size:#x} does not equal supplied size {:#x}",
                    bytes.len()
                ),
            });
        }
        let mut file_type = [0; 4];
        file_type.copy_from_slice(&bytes[4..8]);
        let mut version_tag = [0; 4];
        version_tag.copy_from_slice(&bytes[0x10..0x14]);
        let reserved_words = [
            be_u32(bytes, 0x14, FORMAT)?,
            be_u32(bytes, 0x18, FORMAT)?,
            be_u32(bytes, 0x1c, FORMAT)?,
        ];

        let mut sections = Vec::new();
        let mut cursor = 0x20usize;
        for section_index in 0..declared_section_count {
            if cursor == bytes.len()
                && file_type.starts_with(b"bmt")
                && section_index + 1 == declared_section_count
                && sections.len() == 1
            {
                break;
            }
            checked_slice(FORMAT, bytes, cursor, 8)?;
            let section_size = be_u32(bytes, cursor + 4, FORMAT)? as usize;
            if section_size < 8 {
                return Err(unsupported(format!(
                    "section {section_index} is smaller than a header"
                )));
            }
            let section_bytes = checked_slice(FORMAT, bytes, cursor, section_size)?;
            sections.push(parse_section(section_bytes)?);
            cursor = cursor
                .checked_add(section_size)
                .ok_or_else(|| invalid_offset(cursor, bytes.len()))?;
        }
        if cursor != bytes.len() {
            return Err(unsupported(format!(
                "{} trailing bytes are outside declared sections",
                bytes.len() - cursor
            )));
        }

        Ok(Self {
            file_type,
            version_tag,
            reserved_words,
            declared_section_count,
            sections,
        })
    }

    pub fn to_bytes(&self) -> Result<Vec<u8>> {
        let mut encoded_sections = Vec::with_capacity(self.sections.len());
        let mut file_size = 0x20usize;
        for section in &self.sections {
            let encoded = encode_section(section)?;
            file_size = file_size
                .checked_add(encoded.len())
                .ok_or_else(|| unsupported("encoded J3D size overflow".to_string()))?;
            encoded_sections.push(encoded);
        }
        let file_size_u32 = u32::try_from(file_size)
            .map_err(|_| unsupported(format!("encoded J3D is too large: {file_size:#x}")))?;
        let mut out = Vec::with_capacity(file_size);
        out.extend_from_slice(b"J3D2");
        out.extend_from_slice(&self.file_type);
        out.extend_from_slice(&file_size_u32.to_be_bytes());
        out.extend_from_slice(&self.declared_section_count.to_be_bytes());
        out.extend_from_slice(&self.version_tag);
        for word in self.reserved_words {
            out.extend_from_slice(&word.to_be_bytes());
        }
        for section in encoded_sections {
            out.extend_from_slice(&section);
        }
        Ok(out)
    }

    /// Rebuilds the size-dependent VTX1 and SHP1 layout from their typed data.
    ///
    /// Parsing and [`Self::to_bytes`] deliberately retain the imported layout
    /// metadata so an untouched retail model still round-trips byte-for-byte.
    /// Call this method after changing vertex-array lengths or GX primitive
    /// command lists. It replaces only the geometry sections' layout metadata
    /// with a deterministic, 32-byte-aligned layout and updates the INF1 counts
    /// that can be derived from those sections.
    pub fn canonicalize_geometry_layout(&mut self) -> Result<()> {
        // Canonicalization touches multiple interdependent offsets. Build it on
        // a semantic clone so validation failures cannot leave half-updated
        // metadata in the caller's document.
        let mut canonical = self.clone();
        canonical.canonicalize_geometry_layout_in_place()?;
        *self = canonical;
        Ok(())
    }

    fn canonicalize_geometry_layout_in_place(&mut self) -> Result<()> {
        let mut vertex_count = None;
        let mut packet_count = None;
        let mut vertex_cardinalities = None;
        let mut draw_matrix_count = None;

        for section in &self.sections {
            if let J3dRebuildSectionData::DrawMatrices(draw_matrices) = &section.data {
                if draw_matrix_count.is_some() {
                    return Err(unsupported(
                        "cannot canonicalize a J3D document with multiple DRW1 sections"
                            .to_string(),
                    ));
                }
                if draw_matrices.weighted_flags.len() != draw_matrices.matrix_count as usize
                    || draw_matrices.indices.len() != draw_matrices.matrix_count as usize
                {
                    return Err(unsupported(format!(
                        "DRW1 declares {} matrices but has {} flags and {} indices",
                        draw_matrices.matrix_count,
                        draw_matrices.weighted_flags.len(),
                        draw_matrices.indices.len()
                    )));
                }
                draw_matrix_count = Some(draw_matrices.matrix_count as usize);
            }
        }

        for section in &mut self.sections {
            let J3dRebuildSectionData::Vertices(vertices) = &mut section.data else {
                continue;
            };
            if vertex_count.is_some() {
                return Err(unsupported(
                    "cannot canonicalize a J3D document with multiple VTX1 sections".to_string(),
                ));
            }
            let layout = canonicalize_vtx1_layout(vertices)?;
            section.declared_size = checked_geometry_u32(layout.section_size, "VTX1")?;
            section.padding = layout.padding;
            vertex_count = Some(layout.vertex_count);
            vertex_cardinalities = Some(layout.vertex_cardinalities);
        }

        for section in &mut self.sections {
            let J3dRebuildSectionData::Shapes(shapes) = &mut section.data else {
                continue;
            };
            if packet_count.is_some() {
                return Err(unsupported(
                    "cannot canonicalize a J3D document with multiple SHP1 sections".to_string(),
                ));
            }
            let empty_cardinalities = BTreeMap::new();
            let layout = canonicalize_shp1_layout(
                shapes,
                vertex_cardinalities
                    .as_ref()
                    .unwrap_or(&empty_cardinalities),
                draw_matrix_count,
            )?;
            section.declared_size = checked_geometry_u32(layout.section_size, "SHP1")?;
            section.padding = layout.padding;
            packet_count = Some(layout.packet_count);
        }

        for section in &mut self.sections {
            if let J3dRebuildSectionData::Information(information) = &mut section.data {
                if let Some(vertex_count) = vertex_count {
                    information.vertex_count = vertex_count;
                }
                if let Some(packet_count) = packet_count {
                    information.packet_count = packet_count;
                }
            }
        }
        Ok(())
    }
}

#[derive(Debug)]
struct CanonicalGeometryLayout {
    section_size: usize,
    padding: Vec<J3dPaddingSpan>,
    vertex_count: u32,
    vertex_cardinalities: BTreeMap<u32, usize>,
}

#[derive(Debug)]
struct CanonicalShapeLayout {
    section_size: usize,
    padding: Vec<J3dPaddingSpan>,
    packet_count: u32,
}

fn checked_geometry_u32(value: usize, allocation: &str) -> Result<u32> {
    u32::try_from(value).map_err(|_| {
        unsupported(format!(
            "canonical {allocation} allocation is too large: {value:#x}"
        ))
    })
}

fn checked_geometry_u16(value: usize, allocation: &str) -> Result<u16> {
    u16::try_from(value).map_err(|_| {
        unsupported(format!(
            "canonical {allocation} allocation is too large: {value:#x}"
        ))
    })
}

fn checked_geometry_add(left: usize, right: usize, allocation: &str) -> Result<usize> {
    left.checked_add(right)
        .ok_or_else(|| unsupported(format!("canonical {allocation} allocation size overflow")))
}

fn align_geometry(value: usize, alignment: usize, allocation: &str) -> Result<usize> {
    let mask = alignment - 1;
    checked_geometry_add(value, mask, allocation).map(|value| value & !mask)
}

fn geometry_padding_span(
    start: usize,
    end: usize,
    kind: J3dPaddingKind,
) -> Result<Option<J3dPaddingSpan>> {
    if start >= end {
        return Ok(None);
    }
    Ok(Some(J3dPaddingSpan {
        offset: checked_geometry_u32(start, "padding offset")?,
        length: checked_geometry_u32(end - start, "padding length")?,
        kind,
    }))
}

fn vertex_array_slot(attribute: J3dVertexArrayAttribute) -> Option<(usize, u32)> {
    match attribute {
        J3dVertexArrayAttribute::Position => Some((0, 9)),
        J3dVertexArrayAttribute::Normal => Some((1, 10)),
        J3dVertexArrayAttribute::NormalBinormalTangent => Some((2, 25)),
        J3dVertexArrayAttribute::Color0 => Some((3, 11)),
        J3dVertexArrayAttribute::Color1 => Some((4, 12)),
        J3dVertexArrayAttribute::TexCoord(index @ 0..=7) => {
            Some((index as usize + 5, index as u32 + 13))
        }
        J3dVertexArrayAttribute::TexCoord(_) => None,
    }
}

fn scalar_array_element_count(values: &J3dScalarArray) -> usize {
    match values {
        J3dScalarArray::Unsigned8(values) | J3dScalarArray::PackedColor(values) => values.len(),
        J3dScalarArray::Signed8(values) => values.len(),
        J3dScalarArray::Unsigned16(values) => values.len(),
        J3dScalarArray::Signed16(values) => values.len(),
        J3dScalarArray::Unsigned32(values) | J3dScalarArray::Float32Bits(values) => values.len(),
    }
}

fn scalar_array_matches_component_type(values: &J3dScalarArray, component_type: u32) -> bool {
    matches!(
        (values, component_type),
        (J3dScalarArray::Unsigned8(_), 0)
            | (J3dScalarArray::Signed8(_), 1)
            | (J3dScalarArray::Unsigned16(_), 2)
            | (J3dScalarArray::Signed16(_), 3)
            | (J3dScalarArray::Float32Bits(_), 4)
    )
}

fn vertex_array_cardinality(
    array: &J3dVertexArray,
    gx_attribute: u32,
    format: &J3dVertexAttributeFormat,
) -> Result<usize> {
    let element_count = scalar_array_element_count(&array.values);
    let stride = match array.attribute {
        J3dVertexArrayAttribute::Position => match format.component_count {
            0 => 2,
            1 => 3,
            value => {
                return Err(unsupported(format!(
                    "VTX1 position component count {value} is unsupported"
                )));
            }
        },
        J3dVertexArrayAttribute::Normal | J3dVertexArrayAttribute::NormalBinormalTangent => {
            match format.component_count {
                0 => 3,
                1 => 9,
                2 => {
                    return Err(unsupported(format!(
                        "VTX1 attribute {gx_attribute} uses GX_NRM_NBT3, whose three-index operands are not modeled"
                    )));
                }
                value => {
                    return Err(unsupported(format!(
                        "VTX1 normal component count {value} is unsupported"
                    )));
                }
            }
        }
        J3dVertexArrayAttribute::Color0 | J3dVertexArrayAttribute::Color1 => {
            match format.component_type {
                0 | 3 => 2,
                1 | 4 => 3,
                2 | 5 => 4,
                value => {
                    return Err(unsupported(format!(
                        "VTX1 color component type {value} is unsupported"
                    )));
                }
            }
        }
        J3dVertexArrayAttribute::TexCoord(_) => match format.component_count {
            0 => 1,
            1 => 2,
            value => {
                return Err(unsupported(format!(
                    "VTX1 texture-coordinate component count {value} is unsupported"
                )));
            }
        },
    };
    if !element_count.is_multiple_of(stride) {
        return Err(unsupported(format!(
            "VTX1 attribute {gx_attribute} has {element_count} encoded components, not a multiple of its {stride}-component stride"
        )));
    }
    Ok(element_count / stride)
}

fn canonicalize_vtx1_layout(data: &mut J3dVertexSection) -> Result<CanonicalGeometryLayout> {
    let terminator_index = data
        .formats
        .iter()
        .position(|format| format.attribute == 0xff)
        .ok_or_else(|| unsupported("VTX1 format list is missing its terminator".to_string()))?;
    if terminator_index + 1 != data.formats.len() {
        return Err(unsupported(
            "VTX1 format records follow the terminator".to_string(),
        ));
    }

    let mut slots: [Option<J3dVertexArray>; 13] = std::array::from_fn(|_| None);
    for array in std::mem::take(&mut data.arrays) {
        let (slot, gx_attribute) = vertex_array_slot(array.attribute).ok_or_else(|| {
            unsupported(format!(
                "VTX1 has unsupported array attribute {:?}",
                array.attribute
            ))
        })?;
        if slots[slot].is_some() {
            return Err(unsupported(format!(
                "VTX1 has duplicate array attribute {:?}",
                array.attribute
            )));
        }
        if scalar_array_len(&array.values) == 0 {
            return Err(unsupported(format!(
                "VTX1 array attribute {:?} is empty; remove the array instead",
                array.attribute
            )));
        }
        let format = data
            .formats
            .iter()
            .find(|format| format.attribute == gx_attribute)
            .ok_or_else(|| {
                unsupported(format!(
                    "VTX1 array attribute {gx_attribute} has no format record"
                ))
            })?;
        let is_color = matches!(
            array.attribute,
            J3dVertexArrayAttribute::Color0 | J3dVertexArrayAttribute::Color1
        );
        if is_color {
            if !matches!(array.values, J3dScalarArray::PackedColor(_)) {
                return Err(unsupported(format!(
                    "VTX1 color attribute {gx_attribute} is not packed color data"
                )));
            }
        } else if !scalar_array_matches_component_type(&array.values, format.component_type) {
            return Err(unsupported(format!(
                "VTX1 attribute {gx_attribute} values do not match component type {}",
                format.component_type
            )));
        }
        slots[slot] = Some(array);
    }

    let format_offset = 0x40usize;
    let format_bytes = data
        .formats
        .len()
        .checked_mul(0x10)
        .ok_or_else(|| unsupported("VTX1 format-list size overflow".to_string()))?;
    let format_end = checked_geometry_add(format_offset, format_bytes, "VTX1 formats")?;
    let mut cursor = align_geometry(format_end, 0x20, "VTX1 formats")?;
    let mut padding = Vec::new();
    if let Some(span) = geometry_padding_span(format_end, cursor, J3dPaddingKind::Zero)? {
        padding.push(span);
    }

    data.offsets = [0; 14];
    data.offsets[0] = format_offset as u32;
    data.arrays = Vec::new();
    let mut previous_array_end = None;
    for (slot, array) in slots.into_iter().enumerate() {
        let Some(mut array) = array else {
            continue;
        };
        let array_start = align_geometry(cursor, 0x20, "VTX1 vertex array")?;
        if let Some(span) = geometry_padding_span(
            cursor,
            array_start,
            if previous_array_end.is_some() {
                J3dPaddingKind::RetailMessage { phase: 0 }
            } else {
                J3dPaddingKind::Zero
            },
        )? {
            padding.push(span);
        }
        array.offset = checked_geometry_u32(array_start, "VTX1 vertex-array offset")?;
        data.offsets[slot + 1] = array.offset;
        cursor = checked_geometry_add(
            array_start,
            scalar_array_len(&array.values),
            "VTX1 vertex array",
        )?;
        previous_array_end = Some(cursor);
        data.arrays.push(array);
    }

    let section_size = align_geometry(cursor, 0x20, "VTX1 section")?;
    if let Some(span) = geometry_padding_span(
        cursor,
        section_size,
        if previous_array_end.is_some() {
            J3dPaddingKind::RetailMessage { phase: 0 }
        } else {
            J3dPaddingKind::Zero
        },
    )? {
        padding.push(span);
    }

    let mut vertex_cardinalities = BTreeMap::new();
    for array in &data.arrays {
        let (_, gx_attribute) = vertex_array_slot(array.attribute)
            .expect("canonical VTX1 contains only validated array attributes");
        let format = data
            .formats
            .iter()
            .find(|format| format.attribute == gx_attribute)
            .expect("canonical VTX1 array formats were validated above");
        let cardinality = vertex_array_cardinality(array, gx_attribute, format)?;
        vertex_cardinalities.insert(gx_attribute, cardinality);
    }
    let vertex_count = checked_geometry_u32(
        vertex_cardinalities.get(&9).copied().unwrap_or(0),
        "VTX1 vertex count",
    )?;

    Ok(CanonicalGeometryLayout {
        section_size,
        padding,
        vertex_count,
        vertex_cardinalities,
    })
}

fn j3d_name_hash(encoded_name: &[u8]) -> u16 {
    encoded_name.iter().fold(0u16, |hash, byte| {
        hash.wrapping_mul(3).wrapping_add(*byte as u16)
    })
}

fn canonicalize_name_table_layout(table: &mut J3dNameTable) -> Result<usize> {
    let entry_bytes = table
        .entries
        .len()
        .checked_mul(4)
        .ok_or_else(|| unsupported("J3D name-table size overflow".to_string()))?;
    let mut cursor = checked_geometry_add(4, entry_bytes, "J3D name table")?;
    for entry in &mut table.entries {
        let (encoded, _, had_errors) = SHIFT_JIS.encode(&entry.name);
        if had_errors {
            return Err(unsupported(format!(
                "name {:?} cannot be encoded as Shift-JIS",
                entry.name
            )));
        }
        entry.hash = j3d_name_hash(&encoded);
        entry.string_offset = checked_geometry_u16(cursor, "J3D name string offset")?;
        cursor = checked_geometry_add(cursor, encoded.len() + 1, "J3D name strings")?;
    }
    Ok(cursor)
}

#[derive(Debug, Clone, Copy)]
struct ExpectedVertexOperand {
    attribute: u32,
    input_type: u32,
    cardinality: Option<usize>,
}

fn expected_vertex_operands(
    descriptors: &[J3dVertexDescriptor],
    vertex_cardinalities: &BTreeMap<u32, usize>,
) -> Result<Vec<ExpectedVertexOperand>> {
    let mut expected = Vec::new();
    for descriptor in descriptors {
        if descriptor.attribute == 0xff {
            break;
        }
        match descriptor.input_type {
            0 => {}
            1 if descriptor.attribute <= 8 => expected.push(ExpectedVertexOperand {
                attribute: descriptor.attribute,
                input_type: 1,
                cardinality: None,
            }),
            1 => {
                return Err(unsupported(format!(
                    "direct SHP1 attribute {} needs its VAT encoding modeled",
                    descriptor.attribute
                )));
            }
            2 | 3 if descriptor.attribute <= 8 => {
                return Err(unsupported(format!(
                    "matrix-index attribute {} cannot use indexed GX input type {}",
                    descriptor.attribute, descriptor.input_type
                )));
            }
            2 | 3 => {
                let cardinality = vertex_cardinalities
                    .get(&descriptor.attribute)
                    .copied()
                    .ok_or_else(|| {
                        unsupported(format!(
                            "GX attribute {} has no corresponding VTX1 array",
                            descriptor.attribute
                        ))
                    })?;
                expected.push(ExpectedVertexOperand {
                    attribute: descriptor.attribute,
                    input_type: descriptor.input_type,
                    cardinality: Some(cardinality),
                });
            }
            input_type => {
                return Err(unsupported(format!(
                    "unsupported GX vertex input type {input_type}"
                )));
            }
        }
    }
    Ok(expected)
}

fn canonicalize_draw_commands(
    draw: &mut J3dShapeDraw,
    descriptors: &[J3dVertexDescriptor],
    vertex_cardinalities: &BTreeMap<u32, usize>,
    matrix_palette_capacity: usize,
) -> Result<()> {
    // A GX no-op has no observable effect. Strip imported cache-line padding
    // before regenerating a single deterministic padding suffix.
    draw.commands
        .retain(|command| !matches!(command, J3dGxCommand::Nop));
    let expected_operands = expected_vertex_operands(descriptors, vertex_cardinalities)?;
    for command in &mut draw.commands {
        let J3dGxCommand::Primitive {
            opcode,
            vertex_count,
            vertices,
        } = command
        else {
            continue;
        };
        if !matches!(
            *opcode & 0xf8,
            0x80 | 0x88 | 0x90 | 0x98 | 0xa0 | 0xa8 | 0xb0 | 0xb8
        ) {
            return Err(unsupported(format!(
                "unsupported GX primitive opcode {opcode:#04x}"
            )));
        }
        *vertex_count = checked_geometry_u16(vertices.len(), "GX primitive vertex count")?;
        for (vertex_index, vertex) in vertices.iter().enumerate() {
            if vertex.len() != expected_operands.len() {
                return Err(unsupported(format!(
                    "GX primitive vertex {vertex_index} has {} operands, expected {}",
                    vertex.len(),
                    expected_operands.len()
                )));
            }
            for (operand_index, (operand, expected)) in
                vertex.iter().zip(&expected_operands).enumerate()
            {
                let matches = matches!(
                    (operand, expected.input_type),
                    (J3dGxVertexOperand::DirectU8(_), 1)
                        | (J3dGxVertexOperand::Index8(_), 2)
                        | (J3dGxVertexOperand::Index16(_), 3)
                );
                if !matches {
                    return Err(unsupported(format!(
                        "GX primitive vertex {vertex_index} operand {operand_index} does not match its descriptor"
                    )));
                }
                if let Some(cardinality) = expected.cardinality {
                    let index = match operand {
                        J3dGxVertexOperand::Index8(value) => *value as usize,
                        J3dGxVertexOperand::Index16(value) => *value as usize,
                        J3dGxVertexOperand::DirectU8(_) => unreachable!("validated above"),
                    };
                    if index >= cardinality {
                        return Err(unsupported(format!(
                            "GX primitive vertex {vertex_index} attribute {} index {index} is outside its VTX1 cardinality {cardinality}",
                            expected.attribute
                        )));
                    }
                }
                if expected.attribute == 0 {
                    let J3dGxVertexOperand::DirectU8(value) = operand else {
                        unreachable!("matrix descriptor type was validated above");
                    };
                    if value % 3 != 0 || *value as usize / 3 >= matrix_palette_capacity {
                        return Err(unsupported(format!(
                            "GX primitive vertex {vertex_index} position-matrix index {value} is outside the draw palette capacity {matrix_palette_capacity}"
                        )));
                    }
                }
            }
        }
    }

    let encoded_size = encode_display_list(&draw.commands)?.len();
    let display_list_size = align_geometry(encoded_size, 0x20, "SHP1 display list")?;
    draw.commands.extend(std::iter::repeat_n(
        J3dGxCommand::Nop,
        display_list_size - encoded_size,
    ));
    draw.display_list_size = checked_geometry_u32(display_list_size, "SHP1 display-list size")?;
    Ok(())
}

fn canonicalize_shp1_layout(
    data: &mut J3dShapeSection,
    vertex_cardinalities: &BTreeMap<u32, usize>,
    draw_matrix_count: Option<usize>,
) -> Result<CanonicalShapeLayout> {
    data.shape_count = checked_geometry_u16(data.remap.len(), "SHP1 shape count")?;
    let init_count = data
        .remap
        .iter()
        .copied()
        .max()
        .map_or(0usize, |index| index as usize + 1);
    if data.shapes.len() != init_count {
        return Err(unsupported(format!(
            "SHP1 has {} shape records, but its remap references {init_count}",
            data.shapes.len()
        )));
    }

    let mut descriptor_indices = BTreeMap::new();
    for (index, set) in data.vertex_descriptor_sets.iter().enumerate() {
        if descriptor_indices
            .insert(set.relative_offset, index)
            .is_some()
        {
            return Err(unsupported(format!(
                "SHP1 has duplicate descriptor offset {:#x}",
                set.relative_offset
            )));
        }
        let terminator_index = set
            .descriptors
            .iter()
            .position(|descriptor| descriptor.attribute == 0xff)
            .ok_or_else(|| {
                unsupported(format!(
                    "SHP1 descriptor set {:#x} has no terminator",
                    set.relative_offset
                ))
            })?;
        if terminator_index + 1 != set.descriptors.len() {
            return Err(unsupported(format!(
                "SHP1 descriptor set {:#x} has records after its terminator",
                set.relative_offset
            )));
        }
    }

    let matrix_group_count = data
        .shapes
        .iter()
        .map(|shape| shape.matrix_group_start as usize + shape.matrix_group_count as usize)
        .max()
        .unwrap_or(0);
    if matrix_group_count != data.matrix_groups.len() {
        return Err(unsupported(format!(
            "SHP1 shape ranges reference {matrix_group_count} matrix groups, but {} are present",
            data.matrix_groups.len()
        )));
    }
    let draw_count = data
        .shapes
        .iter()
        .map(|shape| shape.draw_start as usize + shape.matrix_group_count as usize)
        .max()
        .unwrap_or(0);
    if draw_count != data.draws.len() {
        return Err(unsupported(format!(
            "SHP1 shape ranges reference {draw_count} draws, but {} are present",
            data.draws.len()
        )));
    }

    for (shape_index, shape) in data.shapes.iter().enumerate() {
        if shape.matrix_type > 3 {
            return Err(unsupported(format!(
                "SHP1 shape {shape_index} uses unsupported matrix type {}",
                shape.matrix_type
            )));
        }
        for local_group in 0..shape.matrix_group_count as usize {
            let group_index = shape.matrix_group_start as usize + local_group;
            let group = &data.matrix_groups[group_index];
            if shape.matrix_type == 3 {
                let first_matrix = group.first_matrix as usize;
                let matrix_end = first_matrix
                    .checked_add(group.matrix_count as usize)
                    .ok_or_else(|| {
                        unsupported(format!(
                            "SHP1 matrix group {group_index} table range overflows"
                        ))
                    })?;
                if matrix_end > data.matrix_table.len() {
                    return Err(unsupported(format!(
                        "SHP1 matrix group {group_index} uses matrix-table range {first_matrix}..{matrix_end}, but the table has {} entries",
                        data.matrix_table.len()
                    )));
                }
                if let Some(draw_matrix_count) = draw_matrix_count {
                    for (palette_index, matrix_index) in data.matrix_table[first_matrix..matrix_end]
                        .iter()
                        .copied()
                        .enumerate()
                    {
                        if matrix_index != u16::MAX && matrix_index as usize >= draw_matrix_count {
                            return Err(unsupported(format!(
                                "SHP1 matrix group {group_index} palette entry {palette_index} references DRW1 matrix {matrix_index}, but only {draw_matrix_count} exist"
                            )));
                        }
                    }
                }
            } else if let Some(draw_matrix_count) = draw_matrix_count {
                if group.matrix_index as usize >= draw_matrix_count {
                    return Err(unsupported(format!(
                        "SHP1 matrix group {group_index} references DRW1 matrix {}, but only {draw_matrix_count} exist",
                        group.matrix_index
                    )));
                }
            }
        }
    }

    let mut descriptor_used_by_shape = vec![false; data.vertex_descriptor_sets.len()];
    for shape in &data.shapes {
        let descriptor_index = descriptor_indices
            .get(&shape.vertex_descriptor_offset)
            .copied()
            .ok_or_else(|| {
                unsupported(format!(
                    "SHP1 shape references missing descriptor set {:#x}",
                    shape.vertex_descriptor_offset
                ))
            })?;
        descriptor_used_by_shape[descriptor_index] = true;
    }
    if let Some(index) = descriptor_used_by_shape.iter().position(|used| !*used) {
        return Err(unsupported(format!(
            "SHP1 descriptor set {:#x} is not referenced by a shape",
            data.vertex_descriptor_sets[index].relative_offset
        )));
    }

    let mut draw_descriptor_indices = vec![None; draw_count];
    let mut draw_matrix_palette_capacities = vec![None; draw_count];
    for init_index in data.remap.iter().take(data.shape_count as usize).copied() {
        let shape = &data.shapes[init_index as usize];
        let descriptor_index = descriptor_indices[&shape.vertex_descriptor_offset];
        for group in 0..shape.matrix_group_count as usize {
            let draw_index = shape.draw_start as usize + group;
            let slot = &mut draw_descriptor_indices[draw_index];
            if slot.is_some_and(|existing| existing != descriptor_index) {
                return Err(unsupported(format!(
                    "SHP1 draw {draw_index} is shared by incompatible descriptor sets"
                )));
            }
            *slot = Some(descriptor_index);
            let matrix_group = &data.matrix_groups[shape.matrix_group_start as usize + group];
            let palette_capacity = if shape.matrix_type == 3 {
                matrix_group.matrix_count as usize
            } else {
                1
            };
            let capacity_slot = &mut draw_matrix_palette_capacities[draw_index];
            *capacity_slot = Some(capacity_slot.map_or(palette_capacity, |existing: usize| {
                existing.min(palette_capacity)
            }));
        }
    }
    if let Some(index) = draw_descriptor_indices
        .iter()
        .position(|descriptor| descriptor.is_none())
    {
        return Err(unsupported(format!(
            "SHP1 draw {index} is not referenced by a remapped shape"
        )));
    }

    for ((draw, descriptor_index), matrix_palette_capacity) in data
        .draws
        .iter_mut()
        .zip(draw_descriptor_indices)
        .zip(draw_matrix_palette_capacities)
    {
        canonicalize_draw_commands(
            draw,
            &data.vertex_descriptor_sets[descriptor_index.expect("validated above")].descriptors,
            vertex_cardinalities,
            matrix_palette_capacity.expect("referenced draw capacity was populated above"),
        )?;
    }

    let shape_init_offset = 0x2cusize;
    let shape_bytes = data
        .shapes
        .len()
        .checked_mul(0x28)
        .ok_or_else(|| unsupported("SHP1 shape-record size overflow".to_string()))?;
    let mut cursor = checked_geometry_add(shape_init_offset, shape_bytes, "SHP1 shapes")?;
    let remap_offset = cursor;
    cursor = checked_geometry_add(cursor, data.remap.len() * 2, "SHP1 remap")?;
    let name_table_offset = if let Some(names) = &mut data.names {
        let offset = cursor;
        cursor =
            checked_geometry_add(cursor, canonicalize_name_table_layout(names)?, "SHP1 names")?;
        offset
    } else {
        0
    };

    let mut padding = Vec::new();
    let descriptor_offset = align_geometry(cursor, 0x20, "SHP1 descriptors")?;
    if let Some(span) = geometry_padding_span(cursor, descriptor_offset, J3dPaddingKind::Zero)? {
        padding.push(span);
    }
    cursor = descriptor_offset;
    let mut canonical_descriptor_offsets = Vec::with_capacity(data.vertex_descriptor_sets.len());
    for set in &mut data.vertex_descriptor_sets {
        let relative_offset = cursor - descriptor_offset;
        set.relative_offset = checked_geometry_u16(relative_offset, "SHP1 descriptor offset")?;
        canonical_descriptor_offsets.push(set.relative_offset);
        cursor = checked_geometry_add(cursor, set.descriptors.len() * 8, "SHP1 descriptors")?;
    }
    for shape in &mut data.shapes {
        let old_index = descriptor_indices[&shape.vertex_descriptor_offset];
        shape.vertex_descriptor_offset = canonical_descriptor_offsets[old_index];
    }

    let matrix_table_offset = if data.matrix_table.is_empty() {
        0
    } else {
        let offset = cursor;
        cursor = checked_geometry_add(cursor, data.matrix_table.len() * 2, "SHP1 matrix table")?;
        offset
    };
    let display_list_offset = align_geometry(cursor, 0x20, "SHP1 display lists")?;
    if let Some(span) = geometry_padding_span(
        cursor,
        display_list_offset,
        if data.matrix_table.is_empty() {
            J3dPaddingKind::Zero
        } else {
            J3dPaddingKind::RetailMessage { phase: 0 }
        },
    )? {
        padding.push(span);
    }
    cursor = display_list_offset;
    for draw in &mut data.draws {
        draw.display_list_offset =
            checked_geometry_u32(cursor - display_list_offset, "SHP1 display-list offset")?;
        cursor = checked_geometry_add(
            cursor,
            draw.display_list_size as usize,
            "SHP1 display lists",
        )?;
    }
    let matrix_group_offset = cursor;
    cursor = checked_geometry_add(cursor, data.matrix_groups.len() * 8, "SHP1 matrix groups")?;
    let draw_header_offset = cursor;
    cursor = checked_geometry_add(cursor, data.draws.len() * 8, "SHP1 draw headers")?;
    let section_size = align_geometry(cursor, 0x20, "SHP1 section")?;
    if let Some(span) = geometry_padding_span(cursor, section_size, J3dPaddingKind::Zero)? {
        padding.push(span);
    }

    data.offsets = [
        checked_geometry_u32(shape_init_offset, "SHP1 shape offset")?,
        checked_geometry_u32(remap_offset, "SHP1 remap offset")?,
        checked_geometry_u32(name_table_offset, "SHP1 name-table offset")?,
        checked_geometry_u32(descriptor_offset, "SHP1 descriptor offset")?,
        checked_geometry_u32(matrix_table_offset, "SHP1 matrix-table offset")?,
        checked_geometry_u32(display_list_offset, "SHP1 display-list offset")?,
        checked_geometry_u32(matrix_group_offset, "SHP1 matrix-group offset")?,
        checked_geometry_u32(draw_header_offset, "SHP1 draw-header offset")?,
    ];

    Ok(CanonicalShapeLayout {
        section_size,
        padding,
        packet_count: checked_geometry_u32(data.draws.len(), "SHP1 packet count")?,
    })
}

fn parse_section(bytes: &[u8]) -> Result<J3dRebuildSection> {
    let mut tag = [0; 4];
    tag.copy_from_slice(checked_slice(FORMAT, bytes, 0, 4)?);
    let declared_size = be_u32(bytes, 4, FORMAT)?;
    if declared_size as usize != bytes.len() {
        return Err(unsupported(format!(
            "section {:?} size mismatch",
            String::from_utf8_lossy(&tag)
        )));
    }
    let mut coverage = Coverage::new(bytes.len());
    coverage.mark(0, 8)?;
    let data = match &tag {
        b"INF1" => J3dRebuildSectionData::Information(parse_inf1(bytes, &mut coverage)?),
        b"VTX1" => J3dRebuildSectionData::Vertices(parse_vtx1(bytes, &mut coverage)?),
        b"EVP1" => J3dRebuildSectionData::Envelopes(parse_evp1(bytes, &mut coverage)?),
        b"DRW1" => J3dRebuildSectionData::DrawMatrices(parse_drw1(bytes, &mut coverage)?),
        b"JNT1" => J3dRebuildSectionData::Joints(parse_jnt1(bytes, &mut coverage)?),
        b"SHP1" => J3dRebuildSectionData::Shapes(parse_shp1(bytes, &mut coverage)?),
        b"MAT3" => J3dRebuildSectionData::Materials(parse_mat3(bytes, &mut coverage)?),
        b"TEX1" => J3dRebuildSectionData::Textures(parse_tex1(bytes, &mut coverage)?),
        b"MDL3" => J3dRebuildSectionData::MaterialDisplayLists(parse_mdl3(bytes, &mut coverage)?),
        _ => {
            return Err(unsupported(format!(
                "unsupported source-free section {}",
                String::from_utf8_lossy(&tag)
            )));
        }
    };
    let padding = parse_padding(bytes, &coverage).map_err(|error| {
        unsupported(format!(
            "{} padding/layout: {error}",
            String::from_utf8_lossy(&tag)
        ))
    })?;
    Ok(J3dRebuildSection {
        declared_size,
        data,
        padding,
    })
}

#[derive(Debug)]
struct Coverage {
    claimed: Vec<bool>,
}

impl Coverage {
    fn new(length: usize) -> Self {
        Self {
            claimed: vec![false; length],
        }
    }

    fn mark(&mut self, offset: usize, length: usize) -> Result<()> {
        let end = offset
            .checked_add(length)
            .ok_or_else(|| invalid_offset(offset, self.claimed.len()))?;
        if end > self.claimed.len() {
            return Err(invalid_offset(offset, self.claimed.len()));
        }
        self.claimed[offset..end].fill(true);
        Ok(())
    }
}

fn parse_padding(bytes: &[u8], coverage: &Coverage) -> Result<Vec<J3dPaddingSpan>> {
    let mut spans = Vec::new();
    let mut cursor = 0usize;
    while cursor < bytes.len() {
        if coverage.claimed[cursor] {
            cursor += 1;
            continue;
        }
        let start = cursor;
        while cursor < bytes.len() && !coverage.claimed[cursor] {
            cursor += 1;
        }
        let mut run = start;
        while run < cursor {
            let (kind, length) = classify_padding_run(&bytes[run..cursor]).ok_or_else(|| {
                unsupported(format!(
                    "unmodeled non-padding bytes at section offset {run:#x}: {:02x?}",
                    &bytes[run..cursor.min(run + 16)]
                ))
            })?;
            spans.push(J3dPaddingSpan {
                offset: run as u32,
                length: length as u32,
                kind,
            });
            run += length;
        }
    }
    Ok(spans)
}

fn classify_padding_run(bytes: &[u8]) -> Option<(J3dPaddingKind, usize)> {
    if bytes.is_empty() {
        return None;
    }
    if bytes[0] == 0 {
        let length = bytes.iter().take_while(|byte| **byte == 0).count();
        return Some((J3dPaddingKind::Zero, length));
    }
    if bytes[0] == 0xff {
        let length = bytes.iter().take_while(|byte| **byte == 0xff).count();
        return Some((J3dPaddingKind::Ones, length));
    }
    for phase in 0..RETAIL_PADDING.len() {
        if bytes[0] != RETAIL_PADDING[phase] {
            continue;
        }
        let length = bytes
            .iter()
            .enumerate()
            .take_while(|(index, byte)| {
                **byte == RETAIL_PADDING[(phase + index) % RETAIL_PADDING.len()]
            })
            .count();
        if length != 0 {
            return Some((J3dPaddingKind::RetailMessage { phase: phase as u8 }, length));
        }
    }
    None
}

fn write_padding(out: &mut [u8], spans: &[J3dPaddingSpan]) -> Result<()> {
    for span in spans {
        let offset = span.offset as usize;
        let length = span.length as usize;
        let out_len = out.len();
        let target = out
            .get_mut(offset..offset.saturating_add(length))
            .ok_or_else(|| invalid_offset(offset, out_len))?;
        match span.kind {
            J3dPaddingKind::Zero => target.fill(0),
            J3dPaddingKind::Ones => target.fill(0xff),
            J3dPaddingKind::RetailMessage { phase } => {
                for (index, byte) in target.iter_mut().enumerate() {
                    *byte = RETAIL_PADDING[(phase as usize + index) % RETAIL_PADDING.len()];
                }
            }
        }
    }
    Ok(())
}

fn sorted_nonzero_offsets(offsets: &[u32], section_len: usize) -> Result<Vec<usize>> {
    let mut sorted = offsets
        .iter()
        .copied()
        .filter(|offset| *offset != 0)
        .map(|offset| offset as usize)
        .collect::<Vec<_>>();
    sorted.sort_unstable();
    sorted.dedup();
    if sorted.iter().any(|offset| *offset >= section_len) {
        return Err(unsupported(
            "section table offset lies outside section".to_string(),
        ));
    }
    sorted.push(section_len);
    Ok(sorted)
}

fn allocation_end(offset: usize, sorted_offsets: &[usize]) -> Result<usize> {
    sorted_offsets
        .iter()
        .copied()
        .find(|candidate| *candidate > offset)
        .ok_or_else(|| invalid_offset(offset, *sorted_offsets.last().unwrap_or(&0)))
}

fn trim_retail_padding_suffix(
    bytes: &[u8],
    start: usize,
    end: usize,
    element_alignment: usize,
) -> usize {
    let search_start = start.max(end.saturating_sub(0x1f));
    (search_start..end)
        .find(|candidate| {
            (*candidate - start).is_multiple_of(element_alignment)
                && bytes[*candidate..end]
                    .iter()
                    .enumerate()
                    .all(|(index, byte)| *byte == RETAIL_PADDING[index % RETAIL_PADDING.len()])
        })
        .unwrap_or(end)
}

fn read_u8_array(bytes: &[u8], offset: usize, length: usize) -> Result<Vec<u8>> {
    Ok(checked_slice(FORMAT, bytes, offset, length)?.to_vec())
}

fn read_i8_array(bytes: &[u8], offset: usize, length: usize) -> Result<Vec<i8>> {
    Ok(checked_slice(FORMAT, bytes, offset, length)?
        .iter()
        .map(|value| *value as i8)
        .collect())
}

fn read_u16_array(bytes: &[u8], offset: usize, length: usize) -> Result<Vec<u16>> {
    if !length.is_multiple_of(2) {
        return Err(unsupported(format!(
            "unaligned u16 allocation at {offset:#x}"
        )));
    }
    (0..length / 2)
        .map(|index| be_u16(bytes, offset + index * 2, FORMAT))
        .collect()
}

fn read_i16_array(bytes: &[u8], offset: usize, length: usize) -> Result<Vec<i16>> {
    if !length.is_multiple_of(2) {
        return Err(unsupported(format!(
            "unaligned i16 allocation at {offset:#x}"
        )));
    }
    (0..length / 2)
        .map(|index| be_i16(bytes, offset + index * 2, FORMAT))
        .collect()
}

fn read_u32_array(bytes: &[u8], offset: usize, length: usize) -> Result<Vec<u32>> {
    if !length.is_multiple_of(4) {
        return Err(unsupported(format!(
            "unaligned u32 allocation at {offset:#x}"
        )));
    }
    (0..length / 4)
        .map(|index| be_u32(bytes, offset + index * 4, FORMAT))
        .collect()
}

fn scalar_array_len(values: &J3dScalarArray) -> usize {
    match values {
        J3dScalarArray::Unsigned8(values) => values.len(),
        J3dScalarArray::Signed8(values) => values.len(),
        J3dScalarArray::Unsigned16(values) => values.len() * 2,
        J3dScalarArray::Signed16(values) => values.len() * 2,
        J3dScalarArray::Unsigned32(values) | J3dScalarArray::Float32Bits(values) => {
            values.len() * 4
        }
        J3dScalarArray::PackedColor(values) => values.len(),
    }
}

fn write_scalar_array(out: &mut [u8], offset: usize, values: &J3dScalarArray) -> Result<()> {
    let length = scalar_array_len(values);
    let out_len = out.len();
    let target = out
        .get_mut(offset..offset.saturating_add(length))
        .ok_or_else(|| invalid_offset(offset, out_len))?;
    match values {
        J3dScalarArray::Unsigned8(values) => target.copy_from_slice(values),
        J3dScalarArray::Signed8(values) => {
            for (target, value) in target.iter_mut().zip(values) {
                *target = *value as u8;
            }
        }
        J3dScalarArray::Unsigned16(values) => {
            for (target, value) in target.chunks_exact_mut(2).zip(values) {
                target.copy_from_slice(&value.to_be_bytes());
            }
        }
        J3dScalarArray::Signed16(values) => {
            for (target, value) in target.chunks_exact_mut(2).zip(values) {
                target.copy_from_slice(&value.to_be_bytes());
            }
        }
        J3dScalarArray::Unsigned32(values) | J3dScalarArray::Float32Bits(values) => {
            for (target, value) in target.chunks_exact_mut(4).zip(values) {
                target.copy_from_slice(&value.to_be_bytes());
            }
        }
        J3dScalarArray::PackedColor(values) => target.copy_from_slice(values),
    }
    Ok(())
}

fn put_u16(out: &mut [u8], offset: usize, value: u16) -> Result<()> {
    let out_len = out.len();
    out.get_mut(offset..offset.saturating_add(2))
        .ok_or_else(|| invalid_offset(offset, out_len))?
        .copy_from_slice(&value.to_be_bytes());
    Ok(())
}

fn put_i16(out: &mut [u8], offset: usize, value: i16) -> Result<()> {
    let out_len = out.len();
    out.get_mut(offset..offset.saturating_add(2))
        .ok_or_else(|| invalid_offset(offset, out_len))?
        .copy_from_slice(&value.to_be_bytes());
    Ok(())
}

fn put_u32(out: &mut [u8], offset: usize, value: u32) -> Result<()> {
    let out_len = out.len();
    out.get_mut(offset..offset.saturating_add(4))
        .ok_or_else(|| invalid_offset(offset, out_len))?
        .copy_from_slice(&value.to_be_bytes());
    Ok(())
}

fn put_f32(out: &mut [u8], offset: usize, value: f32) -> Result<()> {
    put_u32(out, offset, value.to_bits())
}

fn unsupported(message: String) -> FormatError {
    FormatError::Unsupported {
        format: FORMAT,
        message,
    }
}

fn invalid_offset(offset: usize, len: usize) -> FormatError {
    FormatError::InvalidOffset {
        format: FORMAT,
        offset,
        len,
    }
}

fn parse_inf1(bytes: &[u8], coverage: &mut Coverage) -> Result<J3dInformationSection> {
    require_len(FORMAT, bytes, 0x18)?;
    coverage.mark(8, 0x10)?;
    let hierarchy_offset = be_u32(bytes, 0x14, FORMAT)?;
    let mut hierarchy = Vec::new();
    let mut cursor = hierarchy_offset as usize;
    for _ in 0..0x10000 {
        let node_type = be_u16(bytes, cursor, FORMAT)?;
        let index = be_u16(bytes, cursor + 2, FORMAT)?;
        hierarchy.push(J3dHierarchyCommand { node_type, index });
        coverage.mark(cursor, 4)?;
        cursor += 4;
        if node_type == 0 {
            break;
        }
    }
    if hierarchy
        .last()
        .is_none_or(|command| command.node_type != 0)
    {
        return Err(unsupported(
            "INF1 hierarchy is missing its end command".to_string(),
        ));
    }
    Ok(J3dInformationSection {
        flags: be_u16(bytes, 8, FORMAT)?,
        reserved: be_u16(bytes, 0x0a, FORMAT)?,
        packet_count: be_u32(bytes, 0x0c, FORMAT)?,
        vertex_count: be_u32(bytes, 0x10, FORMAT)?,
        hierarchy_offset,
        hierarchy,
    })
}

fn parse_vtx1(bytes: &[u8], coverage: &mut Coverage) -> Result<J3dVertexSection> {
    require_len(FORMAT, bytes, 0x40)?;
    coverage.mark(8, 0x38)?;
    let mut offsets = [0u32; 14];
    for (index, offset) in offsets.iter_mut().enumerate() {
        *offset = be_u32(bytes, 8 + index * 4, FORMAT)?;
    }
    if offsets[0] == 0 {
        return Err(unsupported("VTX1 has no attribute-format list".to_string()));
    }
    let mut formats = Vec::new();
    let mut cursor = offsets[0] as usize;
    for _ in 0..64 {
        checked_slice(FORMAT, bytes, cursor, 0x10)?;
        let attribute = be_u32(bytes, cursor, FORMAT)?;
        let mut reserved = [0; 3];
        reserved.copy_from_slice(&bytes[cursor + 0x0d..cursor + 0x10]);
        formats.push(J3dVertexAttributeFormat {
            attribute,
            component_count: be_u32(bytes, cursor + 4, FORMAT)?,
            component_type: be_u32(bytes, cursor + 8, FORMAT)?,
            fractional_bits: bytes[cursor + 0x0c],
            reserved,
        });
        coverage.mark(cursor, 0x10)?;
        cursor += 0x10;
        if attribute == 0xff {
            break;
        }
    }
    if formats.last().is_none_or(|format| format.attribute != 0xff) {
        return Err(unsupported(
            "VTX1 format list is missing its terminator".to_string(),
        ));
    }

    let sorted = sorted_nonzero_offsets(&offsets, bytes.len())?;
    let attributes = [
        J3dVertexArrayAttribute::Position,
        J3dVertexArrayAttribute::Normal,
        J3dVertexArrayAttribute::NormalBinormalTangent,
        J3dVertexArrayAttribute::Color0,
        J3dVertexArrayAttribute::Color1,
        J3dVertexArrayAttribute::TexCoord(0),
        J3dVertexArrayAttribute::TexCoord(1),
        J3dVertexArrayAttribute::TexCoord(2),
        J3dVertexArrayAttribute::TexCoord(3),
        J3dVertexArrayAttribute::TexCoord(4),
        J3dVertexArrayAttribute::TexCoord(5),
        J3dVertexArrayAttribute::TexCoord(6),
        J3dVertexArrayAttribute::TexCoord(7),
    ];
    let gx_attributes = [9u32, 10, 25, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20];
    let mut arrays = Vec::new();
    for index in 0..13 {
        let offset = offsets[index + 1] as usize;
        if offset == 0 {
            continue;
        }
        let format = formats
            .iter()
            .find(|format| format.attribute == gx_attributes[index]);
        let allocation_end = allocation_end(offset, &sorted)?;
        let alignment = if matches!(
            attributes[index],
            J3dVertexArrayAttribute::Color0 | J3dVertexArrayAttribute::Color1
        ) {
            1
        } else {
            match format.map(|format| format.component_type).unwrap_or(0) {
                2 | 3 => 2,
                4 => 4,
                _ => 1,
            }
        };
        let end = trim_retail_padding_suffix(bytes, offset, allocation_end, alignment);
        let length = end - offset;
        let values = if matches!(
            attributes[index],
            J3dVertexArrayAttribute::Color0 | J3dVertexArrayAttribute::Color1
        ) {
            J3dScalarArray::PackedColor(read_u8_array(bytes, offset, length)?)
        } else {
            match format.map(|format| format.component_type).unwrap_or(0) {
                0 => J3dScalarArray::Unsigned8(read_u8_array(bytes, offset, length)?),
                1 => J3dScalarArray::Signed8(read_i8_array(bytes, offset, length)?),
                2 => J3dScalarArray::Unsigned16(read_u16_array(bytes, offset, length)?),
                3 => J3dScalarArray::Signed16(read_i16_array(bytes, offset, length)?),
                4 => J3dScalarArray::Float32Bits(read_u32_array(bytes, offset, length)?),
                component_type => {
                    return Err(unsupported(format!(
                        "VTX1 attribute {} uses component type {component_type}",
                        gx_attributes[index]
                    )));
                }
            }
        };
        coverage.mark(offset, length)?;
        arrays.push(J3dVertexArray {
            attribute: attributes[index],
            offset: offset as u32,
            values,
        });
    }
    Ok(J3dVertexSection {
        offsets,
        formats,
        arrays,
    })
}

fn parse_evp1(bytes: &[u8], coverage: &mut Coverage) -> Result<J3dEnvelopeSection> {
    require_len(FORMAT, bytes, 0x1c)?;
    coverage.mark(8, 0x14)?;
    let envelope_count = be_u16(bytes, 8, FORMAT)?;
    let offsets = [
        be_u32(bytes, 0x0c, FORMAT)?,
        be_u32(bytes, 0x10, FORMAT)?,
        be_u32(bytes, 0x14, FORMAT)?,
        be_u32(bytes, 0x18, FORMAT)?,
    ];
    if envelope_count == 0 {
        return Ok(J3dEnvelopeSection {
            envelope_count,
            reserved: be_u16(bytes, 0x0a, FORMAT)?,
            offsets,
            mix_counts: Vec::new(),
            joint_indices: Vec::new(),
            weights: Vec::new(),
            inverse_bind_matrices: Vec::new(),
        });
    }
    if offsets.contains(&0) {
        return Err(unsupported(
            "non-empty EVP1 has a null table offset".to_string(),
        ));
    }
    let mix_counts = read_u8_array(bytes, offsets[0] as usize, envelope_count as usize)?;
    coverage.mark(offsets[0] as usize, mix_counts.len())?;
    let mix_count = mix_counts
        .iter()
        .map(|count| *count as usize)
        .sum::<usize>();
    let joint_indices = read_u16_array(bytes, offsets[1] as usize, mix_count * 2)?;
    coverage.mark(offsets[1] as usize, mix_count * 2)?;
    let weights = (0..mix_count)
        .map(|index| be_f32(bytes, offsets[2] as usize + index * 4, FORMAT))
        .collect::<Result<Vec<_>>>()?;
    coverage.mark(offsets[2] as usize, mix_count * 4)?;
    // EVP1 stores one inverse bind matrix for every joint, including joints
    // not referenced by any weighted envelope.  No joint count is repeated in
    // this section, so its final allocation boundary is authoritative.
    let inverse_offset = offsets[3] as usize;
    let inverse_count = (bytes.len() - inverse_offset) / 0x30;
    let mut inverse_bind_matrices = Vec::with_capacity(inverse_count);
    for matrix_index in 0..inverse_count {
        let base = inverse_offset + matrix_index * 0x30;
        let mut rows = [[0.0; 4]; 3];
        for (row, values) in rows.iter_mut().enumerate() {
            for (column, value) in values.iter_mut().enumerate() {
                *value = be_f32(bytes, base + (row * 4 + column) * 4, FORMAT)?;
            }
        }
        inverse_bind_matrices.push(J3dMatrix34Record { rows });
    }
    coverage.mark(inverse_offset, inverse_count * 0x30)?;
    Ok(J3dEnvelopeSection {
        envelope_count,
        reserved: be_u16(bytes, 0x0a, FORMAT)?,
        offsets,
        mix_counts,
        joint_indices,
        weights,
        inverse_bind_matrices,
    })
}

fn parse_drw1(bytes: &[u8], coverage: &mut Coverage) -> Result<J3dDrawMatrixSection> {
    require_len(FORMAT, bytes, 0x14)?;
    coverage.mark(8, 0x0c)?;
    let matrix_count = be_u16(bytes, 8, FORMAT)?;
    let flag_offset = be_u32(bytes, 0x0c, FORMAT)?;
    let index_offset = be_u32(bytes, 0x10, FORMAT)?;
    let weighted_flags = read_u8_array(bytes, flag_offset as usize, matrix_count as usize)?;
    let indices = read_u16_array(bytes, index_offset as usize, matrix_count as usize * 2)?;
    coverage.mark(flag_offset as usize, weighted_flags.len())?;
    coverage.mark(index_offset as usize, indices.len() * 2)?;
    Ok(J3dDrawMatrixSection {
        matrix_count,
        reserved: be_u16(bytes, 0x0a, FORMAT)?,
        flag_offset,
        index_offset,
        weighted_flags,
        indices,
    })
}

fn parse_jnt1(bytes: &[u8], coverage: &mut Coverage) -> Result<J3dJointSection> {
    require_len(FORMAT, bytes, 0x18)?;
    coverage.mark(8, 0x10)?;
    let joint_count = be_u16(bytes, 8, FORMAT)?;
    let init_offset = be_u32(bytes, 0x0c, FORMAT)?;
    let remap_offset = be_u32(bytes, 0x10, FORMAT)?;
    let name_table_offset = be_u32(bytes, 0x14, FORMAT)?;
    let remap = read_u16_array(bytes, remap_offset as usize, joint_count as usize * 2)?;
    coverage.mark(remap_offset as usize, remap.len() * 2)?;
    let init_count = remap
        .iter()
        .copied()
        .max()
        .map_or(0usize, |index| index as usize + 1);
    let mut joints = Vec::with_capacity(init_count);
    for index in 0..init_count {
        let base = init_offset as usize + index * 0x40;
        checked_slice(FORMAT, bytes, base, 0x40)?;
        joints.push(J3dJointRecord {
            matrix_type: be_u16(bytes, base, FORMAT)?,
            scale_compensate: bytes[base + 2],
            reserved: bytes[base + 3],
            scale: [
                be_f32(bytes, base + 4, FORMAT)?,
                be_f32(bytes, base + 8, FORMAT)?,
                be_f32(bytes, base + 0x0c, FORMAT)?,
            ],
            rotation: [
                be_i16(bytes, base + 0x10, FORMAT)?,
                be_i16(bytes, base + 0x12, FORMAT)?,
                be_i16(bytes, base + 0x14, FORMAT)?,
            ],
            rotation_padding: be_i16(bytes, base + 0x16, FORMAT)?,
            translation: [
                be_f32(bytes, base + 0x18, FORMAT)?,
                be_f32(bytes, base + 0x1c, FORMAT)?,
                be_f32(bytes, base + 0x20, FORMAT)?,
            ],
            radius: be_f32(bytes, base + 0x24, FORMAT)?,
            bounds_min: [
                be_f32(bytes, base + 0x28, FORMAT)?,
                be_f32(bytes, base + 0x2c, FORMAT)?,
                be_f32(bytes, base + 0x30, FORMAT)?,
            ],
            bounds_max: [
                be_f32(bytes, base + 0x34, FORMAT)?,
                be_f32(bytes, base + 0x38, FORMAT)?,
                be_f32(bytes, base + 0x3c, FORMAT)?,
            ],
        });
    }
    coverage.mark(init_offset as usize, init_count * 0x40)?;
    let names = parse_name_table(bytes, name_table_offset as usize, coverage)?;
    if names.entries.len() != joint_count as usize {
        return Err(unsupported(format!(
            "JNT1 has {joint_count} joints but {} names",
            names.entries.len()
        )));
    }
    Ok(J3dJointSection {
        joint_count,
        reserved: be_u16(bytes, 0x0a, FORMAT)?,
        init_offset,
        remap_offset,
        name_table_offset,
        joints,
        remap,
        names,
    })
}

fn parse_name_table(bytes: &[u8], offset: usize, coverage: &mut Coverage) -> Result<J3dNameTable> {
    checked_slice(FORMAT, bytes, offset, 4)?;
    let count = be_u16(bytes, offset, FORMAT)? as usize;
    let reserved = be_u16(bytes, offset + 2, FORMAT)?;
    coverage.mark(offset, 4 + count * 4)?;
    let mut entries = Vec::with_capacity(count);
    for index in 0..count {
        let entry = offset + 4 + index * 4;
        let hash = be_u16(bytes, entry, FORMAT)?;
        let string_offset = be_u16(bytes, entry + 2, FORMAT)?;
        let start = offset + string_offset as usize;
        let end = bytes[start..]
            .iter()
            .position(|byte| *byte == 0)
            .map(|length| start + length)
            .ok_or_else(|| invalid_offset(start, bytes.len()))?;
        let (name, _, had_errors) = SHIFT_JIS.decode(&bytes[start..end]);
        if had_errors {
            return Err(unsupported(format!(
                "name table entry {index} is not valid Shift-JIS"
            )));
        }
        coverage.mark(start, end - start + 1)?;
        entries.push(J3dNameEntry {
            hash,
            string_offset,
            name: name.into_owned(),
        });
    }
    Ok(J3dNameTable { reserved, entries })
}

fn parse_shp1(bytes: &[u8], coverage: &mut Coverage) -> Result<J3dShapeSection> {
    require_len(FORMAT, bytes, 0x2c)?;
    coverage.mark(8, 0x24)?;
    let shape_count = be_u16(bytes, 8, FORMAT)?;
    let mut offsets = [0u32; 8];
    for (index, offset) in offsets.iter_mut().enumerate() {
        *offset = be_u32(bytes, 0x0c + index * 4, FORMAT)?;
    }
    let remap = read_u16_array(bytes, offsets[1] as usize, shape_count as usize * 2)?;
    coverage.mark(offsets[1] as usize, remap.len() * 2)?;
    let init_count = remap
        .iter()
        .copied()
        .max()
        .map_or(0usize, |index| index as usize + 1);
    let mut shapes = Vec::with_capacity(init_count);
    for index in 0..init_count {
        let base = offsets[0] as usize + index * 0x28;
        checked_slice(FORMAT, bytes, base, 0x28)?;
        shapes.push(J3dShapeRecord {
            matrix_type: bytes[base],
            reserved_01: bytes[base + 1],
            matrix_group_count: be_u16(bytes, base + 2, FORMAT)?,
            vertex_descriptor_offset: be_u16(bytes, base + 4, FORMAT)?,
            matrix_group_start: be_u16(bytes, base + 6, FORMAT)?,
            draw_start: be_u16(bytes, base + 8, FORMAT)?,
            reserved_0a: be_u16(bytes, base + 0x0a, FORMAT)?,
            radius: be_f32(bytes, base + 0x0c, FORMAT)?,
            bounds_min: [
                be_f32(bytes, base + 0x10, FORMAT)?,
                be_f32(bytes, base + 0x14, FORMAT)?,
                be_f32(bytes, base + 0x18, FORMAT)?,
            ],
            bounds_max: [
                be_f32(bytes, base + 0x1c, FORMAT)?,
                be_f32(bytes, base + 0x20, FORMAT)?,
                be_f32(bytes, base + 0x24, FORMAT)?,
            ],
        });
    }
    coverage.mark(offsets[0] as usize, init_count * 0x28)?;
    let names = if offsets[2] == 0 {
        None
    } else {
        Some(parse_name_table(bytes, offsets[2] as usize, coverage)?)
    };

    let descriptor_offsets = shapes
        .iter()
        .map(|shape| shape.vertex_descriptor_offset)
        .collect::<BTreeSet<_>>();
    let mut vertex_descriptor_sets = Vec::with_capacity(descriptor_offsets.len());
    for relative_offset in descriptor_offsets {
        let mut descriptors = Vec::new();
        let mut cursor = offsets[3] as usize + relative_offset as usize;
        for _ in 0..64 {
            let attribute = be_u32(bytes, cursor, FORMAT)?;
            let input_type = be_u32(bytes, cursor + 4, FORMAT)?;
            descriptors.push(J3dVertexDescriptor {
                attribute,
                input_type,
            });
            coverage.mark(cursor, 8)?;
            cursor += 8;
            if attribute == 0xff {
                break;
            }
        }
        if descriptors
            .last()
            .is_none_or(|descriptor| descriptor.attribute != 0xff)
        {
            return Err(unsupported(format!(
                "SHP1 vertex descriptor set {relative_offset:#x} has no terminator"
            )));
        }
        vertex_descriptor_sets.push(J3dVertexDescriptorSet {
            relative_offset,
            descriptors,
        });
    }

    let matrix_table_offset = offsets[4] as usize;
    let display_list_offset = offsets[5] as usize;
    let matrix_table = if matrix_table_offset == 0 {
        Vec::new()
    } else {
        let allocation_end = [
            display_list_offset,
            offsets[6] as usize,
            offsets[7] as usize,
            bytes.len(),
        ]
        .into_iter()
        .filter(|end| *end > matrix_table_offset)
        .min()
        .ok_or_else(|| invalid_offset(matrix_table_offset, bytes.len()))?;
        let end = trim_retail_padding_suffix(bytes, matrix_table_offset, allocation_end, 2);
        let values = read_u16_array(bytes, matrix_table_offset, end - matrix_table_offset)?;
        coverage.mark(matrix_table_offset, values.len() * 2)?;
        values
    };

    let matrix_group_count = shapes
        .iter()
        .map(|shape| shape.matrix_group_start as usize + shape.matrix_group_count as usize)
        .max()
        .unwrap_or(0);
    let mut matrix_groups = Vec::with_capacity(matrix_group_count);
    for index in 0..matrix_group_count {
        let base = offsets[6] as usize + index * 8;
        matrix_groups.push(J3dShapeMatrixGroup {
            matrix_index: be_u16(bytes, base, FORMAT)?,
            matrix_count: be_u16(bytes, base + 2, FORMAT)?,
            first_matrix: be_u32(bytes, base + 4, FORMAT)?,
        });
    }
    coverage.mark(offsets[6] as usize, matrix_group_count * 8)?;

    let draw_count = shapes
        .iter()
        .map(|shape| shape.draw_start as usize + shape.matrix_group_count as usize)
        .max()
        .unwrap_or(0);
    let mut draw_headers = Vec::with_capacity(draw_count);
    for index in 0..draw_count {
        let base = offsets[7] as usize + index * 8;
        draw_headers.push((
            be_u32(bytes, base, FORMAT)?,
            be_u32(bytes, base + 4, FORMAT)?,
        ));
    }
    coverage.mark(offsets[7] as usize, draw_count * 8)?;

    let descriptors_by_offset = vertex_descriptor_sets
        .iter()
        .map(|set| (set.relative_offset, &set.descriptors))
        .collect::<BTreeMap<_, _>>();
    let mut draw_descriptor_offsets = vec![None; draw_count];
    for init_index in remap.iter().take(shape_count as usize).copied() {
        let init_index = init_index as usize;
        let shape = shapes
            .get(init_index)
            .ok_or_else(|| invalid_offset(init_index, shapes.len()))?;
        for group in 0..shape.matrix_group_count as usize {
            let draw_index = shape.draw_start as usize + group;
            let slot = draw_descriptor_offsets
                .get_mut(draw_index)
                .ok_or_else(|| invalid_offset(draw_index, draw_count))?;
            if slot.is_some_and(|existing| existing != shape.vertex_descriptor_offset) {
                return Err(unsupported(format!(
                    "SHP1 draw {draw_index} is shared by incompatible descriptor sets"
                )));
            }
            *slot = Some(shape.vertex_descriptor_offset);
        }
    }
    let mut draws = Vec::with_capacity(draw_count);
    for (index, (size, relative_offset)) in draw_headers.into_iter().enumerate() {
        let descriptor_offset = draw_descriptor_offsets[index].ok_or_else(|| {
            unsupported(format!("SHP1 draw {index} is not referenced by a shape"))
        })?;
        let descriptors = descriptors_by_offset
            .get(&descriptor_offset)
            .ok_or_else(|| {
                unsupported(format!(
                    "missing SHP1 descriptor set {descriptor_offset:#x}"
                ))
            })?;
        let start = display_list_offset
            .checked_add(relative_offset as usize)
            .ok_or_else(|| invalid_offset(display_list_offset, bytes.len()))?;
        let display_bytes = checked_slice(FORMAT, bytes, start, size as usize)?;
        let commands = parse_display_list(display_bytes, descriptors)?;
        coverage.mark(start, size as usize)?;
        draws.push(J3dShapeDraw {
            display_list_size: size,
            display_list_offset: relative_offset,
            commands,
        });
    }

    Ok(J3dShapeSection {
        shape_count,
        reserved: be_u16(bytes, 0x0a, FORMAT)?,
        offsets,
        shapes,
        remap,
        names,
        vertex_descriptor_sets,
        matrix_table,
        matrix_groups,
        draws,
    })
}

fn parse_display_list(
    bytes: &[u8],
    descriptors: &[J3dVertexDescriptor],
) -> Result<Vec<J3dGxCommand>> {
    let mut commands = Vec::new();
    let mut cursor = 0usize;
    while cursor < bytes.len() {
        let opcode = bytes[cursor];
        cursor += 1;
        match opcode {
            0 => commands.push(J3dGxCommand::Nop),
            0x08 => {
                checked_slice(FORMAT, bytes, cursor, 5)?;
                commands.push(J3dGxCommand::LoadCp {
                    register: bytes[cursor],
                    value: be_u32(bytes, cursor + 1, FORMAT)?,
                });
                cursor += 5;
            }
            0x10 => {
                checked_slice(FORMAT, bytes, cursor, 4)?;
                let count = be_u16(bytes, cursor, FORMAT)? as usize + 1;
                let address = be_u16(bytes, cursor + 2, FORMAT)?;
                cursor += 4;
                let values = read_u32_array(bytes, cursor, count * 4)?;
                cursor += count * 4;
                commands.push(J3dGxCommand::LoadXf { address, values });
            }
            0x61 => {
                let value = be_u32(bytes, cursor, FORMAT)?;
                cursor += 4;
                commands.push(J3dGxCommand::LoadBp { value });
            }
            _ if matches!(
                opcode & 0xf8,
                0x80 | 0x88 | 0x90 | 0x98 | 0xa0 | 0xa8 | 0xb0 | 0xb8
            ) =>
            {
                let vertex_count = be_u16(bytes, cursor, FORMAT)?;
                cursor += 2;
                let mut vertices = Vec::with_capacity(vertex_count as usize);
                for _ in 0..vertex_count {
                    let mut vertex = Vec::new();
                    for descriptor in descriptors {
                        if descriptor.attribute == 0xff {
                            break;
                        }
                        match descriptor.input_type {
                            0 => {}
                            1 if descriptor.attribute <= 8 => {
                                let value = *checked_slice(FORMAT, bytes, cursor, 1)?
                                    .first()
                                    .unwrap_or(&0);
                                cursor += 1;
                                vertex.push(J3dGxVertexOperand::DirectU8(value));
                            }
                            1 => {
                                return Err(unsupported(format!(
                                    "direct SHP1 attribute {} needs its VAT encoding modeled",
                                    descriptor.attribute
                                )));
                            }
                            2 => {
                                let value = *checked_slice(FORMAT, bytes, cursor, 1)?
                                    .first()
                                    .unwrap_or(&0);
                                cursor += 1;
                                vertex.push(J3dGxVertexOperand::Index8(value));
                            }
                            3 => {
                                let value = be_u16(bytes, cursor, FORMAT)?;
                                cursor += 2;
                                vertex.push(J3dGxVertexOperand::Index16(value));
                            }
                            input_type => {
                                return Err(unsupported(format!(
                                    "unsupported GX vertex input type {input_type}"
                                )));
                            }
                        }
                    }
                    vertices.push(vertex);
                }
                commands.push(J3dGxCommand::Primitive {
                    opcode,
                    vertex_count,
                    vertices,
                });
            }
            _ => {
                return Err(unsupported(format!(
                    "unsupported GX display-list opcode {opcode:#04x} at {:#x}",
                    cursor - 1
                )));
            }
        }
    }
    Ok(commands)
}

fn parse_mat3(bytes: &[u8], coverage: &mut Coverage) -> Result<J3dMaterialSection> {
    require_len(FORMAT, bytes, 0x84)?;
    coverage.mark(8, 0x7c)?;
    let material_count = be_u16(bytes, 8, FORMAT)?;
    let mut offsets = [0u32; 30];
    for (index, offset) in offsets.iter_mut().enumerate() {
        *offset = be_u32(bytes, 0x0c + index * 4, FORMAT)?;
    }
    if offsets[0] == 0 || offsets[1] == 0 {
        return Err(unsupported(
            "MAT3 is missing init or remap data".to_string(),
        ));
    }
    let remap = read_u16_array(bytes, offsets[1] as usize, material_count as usize * 2)?;
    let init_count = remap
        .iter()
        .copied()
        .max()
        .map_or(0usize, |index| index as usize + 1);
    let mut material_init_records = Vec::with_capacity(init_count);
    for index in 0..init_count {
        material_init_records.push(parse_material_init_record(
            bytes,
            offsets[0] as usize + index * 0x14c,
        )?);
    }
    coverage.mark(offsets[0] as usize, init_count * 0x14c)?;
    let names = if offsets[2] == 0 {
        None
    } else {
        Some(parse_name_table(bytes, offsets[2] as usize, coverage)?)
    };

    let kinds = [
        J3dMaterialTableKind::MaterialInit,
        J3dMaterialTableKind::MaterialRemap,
        J3dMaterialTableKind::Names,
        J3dMaterialTableKind::IndirectInit,
        J3dMaterialTableKind::CullMode,
        J3dMaterialTableKind::MaterialColor,
        J3dMaterialTableKind::ColorChannelCount,
        J3dMaterialTableKind::ColorChannel,
        J3dMaterialTableKind::AmbientColor,
        J3dMaterialTableKind::Light,
        J3dMaterialTableKind::TexGenCount,
        J3dMaterialTableKind::TexCoord,
        J3dMaterialTableKind::TexCoord2,
        J3dMaterialTableKind::TexMatrix,
        J3dMaterialTableKind::PostTexMatrix,
        J3dMaterialTableKind::TextureNumber,
        J3dMaterialTableKind::TevOrder,
        J3dMaterialTableKind::TevColor,
        J3dMaterialTableKind::TevKonstColor,
        J3dMaterialTableKind::TevStageCount,
        J3dMaterialTableKind::TevStage,
        J3dMaterialTableKind::TevSwapMode,
        J3dMaterialTableKind::TevSwapTable,
        J3dMaterialTableKind::Fog,
        J3dMaterialTableKind::AlphaCompare,
        J3dMaterialTableKind::Blend,
        J3dMaterialTableKind::ZMode,
        J3dMaterialTableKind::ZCompareLocation,
        J3dMaterialTableKind::Dither,
        J3dMaterialTableKind::NbtScale,
    ];
    let sorted = sorted_nonzero_offsets(&offsets, bytes.len())?;
    let mut tables = Vec::new();
    for (index, (&kind, &offset)) in kinds.iter().zip(&offsets).enumerate() {
        if offset == 0 || matches!(index, 0 | 2) {
            continue;
        }
        if index == 1 {
            coverage.mark(offset as usize, remap.len() * 2)?;
            tables.push(J3dMaterialTable {
                kind,
                offset,
                allocation: J3dScalarArray::Unsigned16(remap.clone()),
            });
            continue;
        }
        let start = offset as usize;
        let allocation_end = allocation_end(start, &sorted)?;
        let element_alignment = match kind {
            J3dMaterialTableKind::CullMode => 4,
            J3dMaterialTableKind::TextureNumber | J3dMaterialTableKind::TevColor => 2,
            _ => 1,
        };
        let end = trim_retail_padding_suffix(bytes, start, allocation_end, element_alignment);
        let length = end - start;
        let allocation = match kind {
            J3dMaterialTableKind::CullMode if length.is_multiple_of(4) => {
                J3dScalarArray::Unsigned32(read_u32_array(bytes, start, length)?)
            }
            J3dMaterialTableKind::TextureNumber if length.is_multiple_of(2) => {
                J3dScalarArray::Unsigned16(read_u16_array(bytes, start, length)?)
            }
            J3dMaterialTableKind::TevColor if length.is_multiple_of(2) => {
                J3dScalarArray::Signed16(read_i16_array(bytes, start, length)?)
            }
            _ => J3dScalarArray::Unsigned8(read_u8_array(bytes, start, length)?),
        };
        coverage.mark(start, length)?;
        tables.push(J3dMaterialTable {
            kind,
            offset,
            allocation,
        });
    }
    Ok(J3dMaterialSection {
        material_count,
        reserved: be_u16(bytes, 0x0a, FORMAT)?,
        offsets,
        material_init_records,
        names,
        tables,
    })
}

fn parse_material_init_record(bytes: &[u8], base: usize) -> Result<J3dMaterialInitRecord> {
    checked_slice(FORMAT, bytes, base, 0x14c)?;
    Ok(J3dMaterialInitRecord {
        material_mode: bytes[base],
        cull_mode_index: bytes[base + 1],
        color_channel_count_index: bytes[base + 2],
        tex_gen_count_index: bytes[base + 3],
        tev_stage_count_index: bytes[base + 4],
        z_compare_location_index: bytes[base + 5],
        z_mode_index: bytes[base + 6],
        dither_index: bytes[base + 7],
        material_color_indices: read_fixed_u16(bytes, base + 8)?,
        color_channel_indices: read_fixed_u16(bytes, base + 0x0c)?,
        ambient_color_indices: read_fixed_u16(bytes, base + 0x14)?,
        light_indices: read_fixed_u16(bytes, base + 0x18)?,
        tex_coord_indices: read_fixed_u16(bytes, base + 0x28)?,
        post_tex_coord_indices: read_fixed_u16(bytes, base + 0x38)?,
        tex_matrix_indices: read_fixed_u16(bytes, base + 0x48)?,
        post_tex_matrix_indices: read_fixed_u16(bytes, base + 0x5c)?,
        texture_number_indices: read_fixed_u16(bytes, base + 0x84)?,
        tev_konst_color_indices: read_fixed_u16(bytes, base + 0x94)?,
        tev_konst_color_selectors: bytes[base + 0x9c..base + 0xac]
            .try_into()
            .expect("fixed checked MAT3 record slice"),
        tev_konst_alpha_selectors: bytes[base + 0xac..base + 0xbc]
            .try_into()
            .expect("fixed checked MAT3 record slice"),
        tev_order_indices: read_fixed_u16(bytes, base + 0xbc)?,
        tev_color_indices: read_fixed_u16(bytes, base + 0xdc)?,
        tev_stage_indices: read_fixed_u16(bytes, base + 0xe4)?,
        tev_swap_mode_indices: read_fixed_u16(bytes, base + 0x104)?,
        tev_swap_table_indices: read_fixed_u16(bytes, base + 0x124)?,
        unused_tail_words: J3dMaterialUnusedTailWords::from_be_bytes(bytes, base + 0x12c)?,
        fog_index: be_u16(bytes, base + 0x144, FORMAT)?,
        alpha_compare_index: be_u16(bytes, base + 0x146, FORMAT)?,
        blend_index: be_u16(bytes, base + 0x148, FORMAT)?,
        nbt_scale_index: be_u16(bytes, base + 0x14a, FORMAT)?,
    })
}

fn read_fixed_u16<const N: usize>(bytes: &[u8], offset: usize) -> Result<[u16; N]> {
    let mut values = [0; N];
    for (index, value) in values.iter_mut().enumerate() {
        *value = be_u16(bytes, offset + index * 2, FORMAT)?;
    }
    Ok(values)
}

fn parse_tex1(bytes: &[u8], coverage: &mut Coverage) -> Result<J3dTextureSection> {
    require_len(FORMAT, bytes, 0x14)?;
    coverage.mark(8, 0x0c)?;
    let texture_count = be_u16(bytes, 8, FORMAT)?;
    let header_offset = be_u32(bytes, 0x0c, FORMAT)?;
    let name_table_offset = be_u32(bytes, 0x10, FORMAT)?;
    let mut textures = Vec::with_capacity(texture_count as usize);
    for index in 0..texture_count as usize {
        let base = header_offset as usize + index * 0x20;
        checked_slice(FORMAT, bytes, base, 0x20)?;
        coverage.mark(base, 0x20)?;
        let format = bytes[base];
        let width = be_u16(bytes, base + 2, FORMAT)?;
        let height = be_u16(bytes, base + 4, FORMAT)?;
        let palette_enabled = bytes[base + 8];
        let palette_entry_count = be_u16(bytes, base + 0x0a, FORMAT)?;
        let palette_offset = be_u32(bytes, base + 0x0c, FORMAT)?;
        let mipmap_count = bytes[base + 0x18];
        let image_offset = be_u32(bytes, base + 0x1c, FORMAT)?;
        let mut encoded_mip_levels = Vec::new();
        let relative_image_offset = if image_offset == 0 {
            0x20
        } else {
            image_offset as usize
        };
        let mut level_offset = base
            .checked_add(relative_image_offset)
            .ok_or_else(|| invalid_offset(base, bytes.len()))?;
        let mut level_width = width;
        let mut level_height = height;
        for _ in 0..mipmap_count.max(1) {
            let length = encoded_texture_size(format, level_width, level_height)?;
            let encoded = checked_slice(FORMAT, bytes, level_offset, length)?.to_vec();
            coverage.mark(level_offset, length)?;
            encoded_mip_levels.push(J3dTextureBlock {
                absolute_section_offset: level_offset as u32,
                bytes: encoded,
            });
            level_offset = level_offset
                .checked_add(length)
                .ok_or_else(|| invalid_offset(level_offset, bytes.len()))?;
            if level_width == 1 && level_height == 1 {
                break;
            }
            level_width = (level_width / 2).max(1);
            level_height = (level_height / 2).max(1);
        }
        let encoded_palette = if palette_enabled != 0 && palette_entry_count != 0 {
            let absolute_offset = base
                .checked_add(palette_offset as usize)
                .ok_or_else(|| invalid_offset(base, bytes.len()))?;
            let length = palette_entry_count as usize * 2;
            let encoded = checked_slice(FORMAT, bytes, absolute_offset, length)?.to_vec();
            coverage.mark(absolute_offset, length)?;
            Some(J3dTextureBlock {
                absolute_section_offset: absolute_offset as u32,
                bytes: encoded,
            })
        } else {
            None
        };
        textures.push(J3dTextureRecord {
            header_relative_offset: (base - header_offset as usize) as u32,
            format,
            transparency: bytes[base + 1],
            width,
            height,
            wrap_s: bytes[base + 6],
            wrap_t: bytes[base + 7],
            palette_enabled,
            palette_format: bytes[base + 9],
            palette_entry_count,
            palette_offset,
            mipmap_enabled: bytes[base + 0x10],
            edge_lod: bytes[base + 0x11],
            bias_clamp: bytes[base + 0x12],
            max_anisotropy: bytes[base + 0x13],
            min_filter: bytes[base + 0x14],
            mag_filter: bytes[base + 0x15],
            min_lod: bytes[base + 0x16] as i8,
            max_lod: bytes[base + 0x17] as i8,
            mipmap_count,
            reserved_19: bytes[base + 0x19],
            lod_bias: be_i16(bytes, base + 0x1a, FORMAT)?,
            image_offset,
            encoded_mip_levels,
            encoded_palette,
        });
    }
    let names = parse_name_table(bytes, name_table_offset as usize, coverage)?;
    if names.entries.len() != texture_count as usize {
        return Err(unsupported(format!(
            "TEX1 has {texture_count} textures but {} names",
            names.entries.len()
        )));
    }
    Ok(J3dTextureSection {
        texture_count,
        reserved: be_u16(bytes, 0x0a, FORMAT)?,
        header_offset,
        name_table_offset,
        textures,
        names,
    })
}

fn encoded_texture_size(format: u8, width: u16, height: u16) -> Result<usize> {
    let (tile_width, tile_height, tile_bytes) = match format {
        0 | 8 | 0x0e => (8usize, 8usize, 32usize),
        1 | 2 | 9 => (8, 4, 32),
        3 | 4 | 5 | 0x0a => (4, 4, 32),
        6 => (4, 4, 64),
        _ => {
            return Err(unsupported(format!(
                "unsupported GX texture format {format}"
            )));
        }
    };
    let blocks_x = (width as usize).div_ceil(tile_width);
    let blocks_y = (height as usize).div_ceil(tile_height);
    blocks_x
        .checked_mul(blocks_y)
        .and_then(|blocks| blocks.checked_mul(tile_bytes))
        .ok_or_else(|| unsupported("GX texture allocation overflow".to_string()))
}

fn parse_mdl3(bytes: &[u8], coverage: &mut Coverage) -> Result<J3dMaterialDisplayListSection> {
    require_len(FORMAT, bytes, 0x24)?;
    let material_count = be_u16(bytes, 8, FORMAT)?;
    let reserved = be_u16(bytes, 0x0a, FORMAT)?;
    // MDL3 revisions have between five and seven table offsets.  The offsets
    // occupy the fixed 0x0c..0x24 header used by SMS's bdl4 files.
    let offsets = (0..6)
        .map(|index| be_u32(bytes, 0x0c + index * 4, FORMAT))
        .collect::<Result<Vec<_>>>()?;
    coverage.mark(8, 0x1c)?;
    let sorted = sorted_nonzero_offsets(&offsets, bytes.len())?;
    let mut allocations = Vec::new();
    for offset in offsets.iter().copied().filter(|offset| *offset != 0) {
        let start = offset as usize;
        let allocation_end = allocation_end(start, &sorted)?;
        let end = trim_retail_padding_suffix(bytes, start, allocation_end, 4);
        let words = read_u32_array(bytes, start, end - start)?;
        coverage.mark(start, words.len() * 4)?;
        allocations.push(J3dMaterialDisplayListAllocation { offset, words });
    }
    Ok(J3dMaterialDisplayListSection {
        material_count,
        reserved,
        offsets,
        allocations,
    })
}

fn encode_section(section: &J3dRebuildSection) -> Result<Vec<u8>> {
    let size = section.declared_size as usize;
    if size < 8 {
        return Err(unsupported(
            "encoded section is smaller than its header".to_string(),
        ));
    }
    let mut out = vec![0; size];
    write_padding(&mut out, &section.padding)?;
    out[..4].copy_from_slice(&section.tag());
    put_u32(&mut out, 4, section.declared_size)?;
    match &section.data {
        J3dRebuildSectionData::Information(data) => encode_inf1(&mut out, data)?,
        J3dRebuildSectionData::Vertices(data) => encode_vtx1(&mut out, data)?,
        J3dRebuildSectionData::Envelopes(data) => encode_evp1(&mut out, data)?,
        J3dRebuildSectionData::DrawMatrices(data) => encode_drw1(&mut out, data)?,
        J3dRebuildSectionData::Joints(data) => encode_jnt1(&mut out, data)?,
        J3dRebuildSectionData::Shapes(data) => encode_shp1(&mut out, data)?,
        J3dRebuildSectionData::Materials(data) => encode_mat3(&mut out, data)?,
        J3dRebuildSectionData::Textures(data) => encode_tex1(&mut out, data)?,
        J3dRebuildSectionData::MaterialDisplayLists(data) => encode_mdl3(&mut out, data)?,
    }
    Ok(out)
}

fn encode_inf1(out: &mut [u8], data: &J3dInformationSection) -> Result<()> {
    put_u16(out, 8, data.flags)?;
    put_u16(out, 0x0a, data.reserved)?;
    put_u32(out, 0x0c, data.packet_count)?;
    put_u32(out, 0x10, data.vertex_count)?;
    put_u32(out, 0x14, data.hierarchy_offset)?;
    let mut cursor = data.hierarchy_offset as usize;
    for command in &data.hierarchy {
        put_u16(out, cursor, command.node_type)?;
        put_u16(out, cursor + 2, command.index)?;
        cursor += 4;
    }
    Ok(())
}

fn encode_vtx1(out: &mut [u8], data: &J3dVertexSection) -> Result<()> {
    for (index, offset) in data.offsets.iter().enumerate() {
        put_u32(out, 8 + index * 4, *offset)?;
    }
    let mut cursor = data.offsets[0] as usize;
    for format in &data.formats {
        put_u32(out, cursor, format.attribute)?;
        put_u32(out, cursor + 4, format.component_count)?;
        put_u32(out, cursor + 8, format.component_type)?;
        let out_len = out.len();
        let record = out
            .get_mut(cursor + 0x0c..cursor + 0x10)
            .ok_or_else(|| invalid_offset(cursor, out_len))?;
        record[0] = format.fractional_bits;
        record[1..].copy_from_slice(&format.reserved);
        cursor += 0x10;
    }
    for array in &data.arrays {
        write_scalar_array(out, array.offset as usize, &array.values)?;
    }
    Ok(())
}

fn encode_evp1(out: &mut [u8], data: &J3dEnvelopeSection) -> Result<()> {
    put_u16(out, 8, data.envelope_count)?;
    put_u16(out, 0x0a, data.reserved)?;
    for (index, offset) in data.offsets.iter().enumerate() {
        put_u32(out, 0x0c + index * 4, *offset)?;
    }
    if data.envelope_count == 0 {
        return Ok(());
    }
    write_scalar_array(
        out,
        data.offsets[0] as usize,
        &J3dScalarArray::Unsigned8(data.mix_counts.clone()),
    )?;
    write_scalar_array(
        out,
        data.offsets[1] as usize,
        &J3dScalarArray::Unsigned16(data.joint_indices.clone()),
    )?;
    for (index, value) in data.weights.iter().enumerate() {
        put_f32(out, data.offsets[2] as usize + index * 4, *value)?;
    }
    for (matrix_index, matrix) in data.inverse_bind_matrices.iter().enumerate() {
        let base = data.offsets[3] as usize + matrix_index * 0x30;
        for (row, values) in matrix.rows.iter().enumerate() {
            for (column, value) in values.iter().enumerate() {
                put_f32(out, base + (row * 4 + column) * 4, *value)?;
            }
        }
    }
    Ok(())
}

fn encode_drw1(out: &mut [u8], data: &J3dDrawMatrixSection) -> Result<()> {
    put_u16(out, 8, data.matrix_count)?;
    put_u16(out, 0x0a, data.reserved)?;
    put_u32(out, 0x0c, data.flag_offset)?;
    put_u32(out, 0x10, data.index_offset)?;
    write_scalar_array(
        out,
        data.flag_offset as usize,
        &J3dScalarArray::Unsigned8(data.weighted_flags.clone()),
    )?;
    write_scalar_array(
        out,
        data.index_offset as usize,
        &J3dScalarArray::Unsigned16(data.indices.clone()),
    )
}

fn encode_jnt1(out: &mut [u8], data: &J3dJointSection) -> Result<()> {
    put_u16(out, 8, data.joint_count)?;
    put_u16(out, 0x0a, data.reserved)?;
    put_u32(out, 0x0c, data.init_offset)?;
    put_u32(out, 0x10, data.remap_offset)?;
    put_u32(out, 0x14, data.name_table_offset)?;
    for (index, joint) in data.joints.iter().enumerate() {
        let base = data.init_offset as usize + index * 0x40;
        put_u16(out, base, joint.matrix_type)?;
        let out_len = out.len();
        let flags = out
            .get_mut(base + 2..base + 4)
            .ok_or_else(|| invalid_offset(base, out_len))?;
        flags.copy_from_slice(&[joint.scale_compensate, joint.reserved]);
        for (component, value) in joint.scale.iter().enumerate() {
            put_f32(out, base + 4 + component * 4, *value)?;
        }
        for (component, value) in joint.rotation.iter().enumerate() {
            put_i16(out, base + 0x10 + component * 2, *value)?;
        }
        put_i16(out, base + 0x16, joint.rotation_padding)?;
        for (component, value) in joint.translation.iter().enumerate() {
            put_f32(out, base + 0x18 + component * 4, *value)?;
        }
        put_f32(out, base + 0x24, joint.radius)?;
        for (component, value) in joint.bounds_min.iter().enumerate() {
            put_f32(out, base + 0x28 + component * 4, *value)?;
        }
        for (component, value) in joint.bounds_max.iter().enumerate() {
            put_f32(out, base + 0x34 + component * 4, *value)?;
        }
    }
    write_scalar_array(
        out,
        data.remap_offset as usize,
        &J3dScalarArray::Unsigned16(data.remap.clone()),
    )?;
    encode_name_table(out, data.name_table_offset as usize, &data.names)
}

fn encode_name_table(out: &mut [u8], offset: usize, table: &J3dNameTable) -> Result<()> {
    let count = u16::try_from(table.entries.len())
        .map_err(|_| unsupported("J3D name table has more than 65535 entries".to_string()))?;
    put_u16(out, offset, count)?;
    put_u16(out, offset + 2, table.reserved)?;
    for (index, entry) in table.entries.iter().enumerate() {
        let entry_offset = offset + 4 + index * 4;
        put_u16(out, entry_offset, entry.hash)?;
        put_u16(out, entry_offset + 2, entry.string_offset)?;
        let (encoded, _, had_errors) = SHIFT_JIS.encode(&entry.name);
        if had_errors {
            return Err(unsupported(format!(
                "name {:?} cannot be encoded as Shift-JIS",
                entry.name
            )));
        }
        let start = offset + entry.string_offset as usize;
        let out_len = out.len();
        let target = out
            .get_mut(start..start.saturating_add(encoded.len() + 1))
            .ok_or_else(|| invalid_offset(start, out_len))?;
        target[..encoded.len()].copy_from_slice(&encoded);
        target[encoded.len()] = 0;
    }
    Ok(())
}

fn encode_shp1(out: &mut [u8], data: &J3dShapeSection) -> Result<()> {
    put_u16(out, 8, data.shape_count)?;
    put_u16(out, 0x0a, data.reserved)?;
    for (index, offset) in data.offsets.iter().enumerate() {
        put_u32(out, 0x0c + index * 4, *offset)?;
    }
    for (index, shape) in data.shapes.iter().enumerate() {
        let base = data.offsets[0] as usize + index * 0x28;
        let out_len = out.len();
        let flags = out
            .get_mut(base..base + 2)
            .ok_or_else(|| invalid_offset(base, out_len))?;
        flags.copy_from_slice(&[shape.matrix_type, shape.reserved_01]);
        put_u16(out, base + 2, shape.matrix_group_count)?;
        put_u16(out, base + 4, shape.vertex_descriptor_offset)?;
        put_u16(out, base + 6, shape.matrix_group_start)?;
        put_u16(out, base + 8, shape.draw_start)?;
        put_u16(out, base + 0x0a, shape.reserved_0a)?;
        put_f32(out, base + 0x0c, shape.radius)?;
        for (component, value) in shape.bounds_min.iter().enumerate() {
            put_f32(out, base + 0x10 + component * 4, *value)?;
        }
        for (component, value) in shape.bounds_max.iter().enumerate() {
            put_f32(out, base + 0x1c + component * 4, *value)?;
        }
    }
    write_scalar_array(
        out,
        data.offsets[1] as usize,
        &J3dScalarArray::Unsigned16(data.remap.clone()),
    )?;
    if let Some(names) = &data.names {
        encode_name_table(out, data.offsets[2] as usize, names)?;
    }
    for set in &data.vertex_descriptor_sets {
        let mut cursor = data.offsets[3] as usize + set.relative_offset as usize;
        for descriptor in &set.descriptors {
            put_u32(out, cursor, descriptor.attribute)?;
            put_u32(out, cursor + 4, descriptor.input_type)?;
            cursor += 8;
        }
    }
    if data.offsets[4] != 0 {
        write_scalar_array(
            out,
            data.offsets[4] as usize,
            &J3dScalarArray::Unsigned16(data.matrix_table.clone()),
        )?;
    }
    for (index, group) in data.matrix_groups.iter().enumerate() {
        let base = data.offsets[6] as usize + index * 8;
        put_u16(out, base, group.matrix_index)?;
        put_u16(out, base + 2, group.matrix_count)?;
        put_u32(out, base + 4, group.first_matrix)?;
    }
    for (index, draw) in data.draws.iter().enumerate() {
        let base = data.offsets[7] as usize + index * 8;
        put_u32(out, base, draw.display_list_size)?;
        put_u32(out, base + 4, draw.display_list_offset)?;
        let encoded = encode_display_list(&draw.commands)?;
        if encoded.len() != draw.display_list_size as usize {
            return Err(unsupported(format!(
                "SHP1 draw {index} encodes to {:#x} bytes, expected {:#x}",
                encoded.len(),
                draw.display_list_size
            )));
        }
        let start = data.offsets[5] as usize + draw.display_list_offset as usize;
        let out_len = out.len();
        out.get_mut(start..start + encoded.len())
            .ok_or_else(|| invalid_offset(start, out_len))?
            .copy_from_slice(&encoded);
    }
    Ok(())
}

fn encode_display_list(commands: &[J3dGxCommand]) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    for command in commands {
        match command {
            J3dGxCommand::Nop => out.push(0),
            J3dGxCommand::LoadCp { register, value } => {
                out.extend_from_slice(&[0x08, *register]);
                out.extend_from_slice(&value.to_be_bytes());
            }
            J3dGxCommand::LoadBp { value } => {
                out.push(0x61);
                out.extend_from_slice(&value.to_be_bytes());
            }
            J3dGxCommand::LoadXf { address, values } => {
                if values.is_empty() || values.len() > u16::MAX as usize + 1 {
                    return Err(unsupported("invalid GX XF write length".to_string()));
                }
                out.push(0x10);
                out.extend_from_slice(&((values.len() - 1) as u16).to_be_bytes());
                out.extend_from_slice(&address.to_be_bytes());
                for value in values {
                    out.extend_from_slice(&value.to_be_bytes());
                }
            }
            J3dGxCommand::Primitive {
                opcode,
                vertex_count,
                vertices,
            } => {
                if vertices.len() != *vertex_count as usize {
                    return Err(unsupported(
                        "GX primitive vertex count mismatch".to_string(),
                    ));
                }
                out.push(*opcode);
                out.extend_from_slice(&vertex_count.to_be_bytes());
                for vertex in vertices {
                    for operand in vertex {
                        match operand {
                            J3dGxVertexOperand::DirectU8(value)
                            | J3dGxVertexOperand::Index8(value) => out.push(*value),
                            J3dGxVertexOperand::Index16(value) => {
                                out.extend_from_slice(&value.to_be_bytes())
                            }
                        }
                    }
                }
            }
        }
    }
    Ok(out)
}

fn encode_mat3(out: &mut [u8], data: &J3dMaterialSection) -> Result<()> {
    put_u16(out, 8, data.material_count)?;
    put_u16(out, 0x0a, data.reserved)?;
    for (index, offset) in data.offsets.iter().enumerate() {
        put_u32(out, 0x0c + index * 4, *offset)?;
    }
    for (index, record) in data.material_init_records.iter().enumerate() {
        encode_material_init_record(out, data.offsets[0] as usize + index * 0x14c, record)?;
    }
    if let Some(names) = &data.names {
        encode_name_table(out, data.offsets[2] as usize, names)?;
    }
    for table in &data.tables {
        write_scalar_array(out, table.offset as usize, &table.allocation)?;
    }
    Ok(())
}

fn encode_material_init_record(
    out: &mut [u8],
    base: usize,
    record: &J3dMaterialInitRecord,
) -> Result<()> {
    let out_len = out.len();
    out.get_mut(base..base + 8)
        .ok_or_else(|| invalid_offset(base, out_len))?
        .copy_from_slice(&[
            record.material_mode,
            record.cull_mode_index,
            record.color_channel_count_index,
            record.tex_gen_count_index,
            record.tev_stage_count_index,
            record.z_compare_location_index,
            record.z_mode_index,
            record.dither_index,
        ]);
    write_fixed_u16(out, base + 8, &record.material_color_indices)?;
    write_fixed_u16(out, base + 0x0c, &record.color_channel_indices)?;
    write_fixed_u16(out, base + 0x14, &record.ambient_color_indices)?;
    write_fixed_u16(out, base + 0x18, &record.light_indices)?;
    write_fixed_u16(out, base + 0x28, &record.tex_coord_indices)?;
    write_fixed_u16(out, base + 0x38, &record.post_tex_coord_indices)?;
    write_fixed_u16(out, base + 0x48, &record.tex_matrix_indices)?;
    write_fixed_u16(out, base + 0x5c, &record.post_tex_matrix_indices)?;
    write_fixed_u16(out, base + 0x84, &record.texture_number_indices)?;
    write_fixed_u16(out, base + 0x94, &record.tev_konst_color_indices)?;
    out.get_mut(base + 0x9c..base + 0xac)
        .ok_or_else(|| invalid_offset(base + 0x9c, out_len))?
        .copy_from_slice(&record.tev_konst_color_selectors);
    out.get_mut(base + 0xac..base + 0xbc)
        .ok_or_else(|| invalid_offset(base + 0xac, out_len))?
        .copy_from_slice(&record.tev_konst_alpha_selectors);
    write_fixed_u16(out, base + 0xbc, &record.tev_order_indices)?;
    write_fixed_u16(out, base + 0xdc, &record.tev_color_indices)?;
    write_fixed_u16(out, base + 0xe4, &record.tev_stage_indices)?;
    write_fixed_u16(out, base + 0x104, &record.tev_swap_mode_indices)?;
    write_fixed_u16(out, base + 0x124, &record.tev_swap_table_indices)?;
    record.unused_tail_words.write_be(out, base + 0x12c)?;
    put_u16(out, base + 0x144, record.fog_index)?;
    put_u16(out, base + 0x146, record.alpha_compare_index)?;
    put_u16(out, base + 0x148, record.blend_index)?;
    put_u16(out, base + 0x14a, record.nbt_scale_index)
}

fn write_fixed_u16(out: &mut [u8], offset: usize, values: &[u16]) -> Result<()> {
    for (index, value) in values.iter().enumerate() {
        put_u16(out, offset + index * 2, *value)?;
    }
    Ok(())
}

fn encode_tex1(out: &mut [u8], data: &J3dTextureSection) -> Result<()> {
    put_u16(out, 8, data.texture_count)?;
    put_u16(out, 0x0a, data.reserved)?;
    put_u32(out, 0x0c, data.header_offset)?;
    put_u32(out, 0x10, data.name_table_offset)?;
    let mut allocations = BTreeMap::<(u32, usize), &[u8]>::new();
    for texture in &data.textures {
        for block in texture
            .encoded_mip_levels
            .iter()
            .chain(texture.encoded_palette.iter())
        {
            register_texture_block(&mut allocations, block)?;
        }
    }
    for texture in &data.textures {
        let base = data.header_offset as usize + texture.header_relative_offset as usize;
        let out_len = out.len();
        let header = out
            .get_mut(base..base + 0x20)
            .ok_or_else(|| invalid_offset(base, out_len))?;
        header[0] = texture.format;
        header[1] = texture.transparency;
        header[2..4].copy_from_slice(&texture.width.to_be_bytes());
        header[4..6].copy_from_slice(&texture.height.to_be_bytes());
        header[6] = texture.wrap_s;
        header[7] = texture.wrap_t;
        header[8] = texture.palette_enabled;
        header[9] = texture.palette_format;
        header[0x0a..0x0c].copy_from_slice(&texture.palette_entry_count.to_be_bytes());
        header[0x0c..0x10].copy_from_slice(&texture.palette_offset.to_be_bytes());
        header[0x10] = texture.mipmap_enabled;
        header[0x11] = texture.edge_lod;
        header[0x12] = texture.bias_clamp;
        header[0x13] = texture.max_anisotropy;
        header[0x14] = texture.min_filter;
        header[0x15] = texture.mag_filter;
        header[0x16] = texture.min_lod as u8;
        header[0x17] = texture.max_lod as u8;
        header[0x18] = texture.mipmap_count;
        header[0x19] = texture.reserved_19;
        header[0x1a..0x1c].copy_from_slice(&texture.lod_bias.to_be_bytes());
        header[0x1c..0x20].copy_from_slice(&texture.image_offset.to_be_bytes());
    }
    for ((offset, _), bytes) in allocations {
        write_texture_block(
            out,
            &J3dTextureBlock {
                absolute_section_offset: offset,
                bytes: bytes.to_vec(),
            },
        )?;
    }
    encode_name_table(out, data.name_table_offset as usize, &data.names)
}

fn register_texture_block<'a>(
    allocations: &mut BTreeMap<(u32, usize), &'a [u8]>,
    block: &'a J3dTextureBlock,
) -> Result<()> {
    let start = block.absolute_section_offset as usize;
    let end = start
        .checked_add(block.bytes.len())
        .ok_or_else(|| invalid_offset(start, usize::MAX))?;
    for ((existing_offset, existing_length), existing_bytes) in allocations.iter() {
        let existing_start = *existing_offset as usize;
        let existing_end = existing_start
            .checked_add(*existing_length)
            .ok_or_else(|| invalid_offset(existing_start, usize::MAX))?;
        if start < existing_end && existing_start < end {
            if start == existing_start
                && end == existing_end
                && *existing_bytes == block.bytes.as_slice()
            {
                return Ok(());
            }
            return Err(unsupported(format!(
                "TEX1 allocations overlap incompatibly at {start:#x}..{end:#x} and {existing_start:#x}..{existing_end:#x}"
            )));
        }
    }
    allocations.insert(
        (block.absolute_section_offset, block.bytes.len()),
        &block.bytes,
    );
    Ok(())
}

fn write_texture_block(out: &mut [u8], block: &J3dTextureBlock) -> Result<()> {
    let offset = block.absolute_section_offset as usize;
    let out_len = out.len();
    out.get_mut(offset..offset.saturating_add(block.bytes.len()))
        .ok_or_else(|| invalid_offset(offset, out_len))?
        .copy_from_slice(&block.bytes);
    Ok(())
}

fn encode_mdl3(out: &mut [u8], data: &J3dMaterialDisplayListSection) -> Result<()> {
    put_u16(out, 8, data.material_count)?;
    put_u16(out, 0x0a, data.reserved)?;
    for (index, offset) in data.offsets.iter().enumerate() {
        put_u32(out, 0x0c + index * 4, *offset)?;
    }
    for allocation in &data.allocations {
        write_scalar_array(
            out,
            allocation.offset as usize,
            &J3dScalarArray::Unsigned32(allocation.words.clone()),
        )?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn synthetic_geometry_document(vertex_count: usize) -> J3dRebuildDocument {
        let positions = (0..vertex_count)
            .flat_map(|index| {
                [
                    (index as f32).to_bits(),
                    (index as f32 + 0.25).to_bits(),
                    (index as f32 + 0.5).to_bits(),
                ]
            })
            .collect();
        let vertices = (0..vertex_count)
            .map(|index| vec![J3dGxVertexOperand::Index16(index as u16)])
            .collect::<Vec<_>>();
        J3dRebuildDocument {
            file_type: *b"bmd3",
            version_tag: *b"SVR3",
            reserved_words: [u32::MAX; 3],
            declared_section_count: 3,
            sections: vec![
                J3dRebuildSection {
                    declared_size: 0x20,
                    data: J3dRebuildSectionData::Information(J3dInformationSection {
                        flags: 0,
                        reserved: u16::MAX,
                        packet_count: 0,
                        vertex_count: 0,
                        hierarchy_offset: 0x18,
                        hierarchy: vec![J3dHierarchyCommand {
                            node_type: 0,
                            index: 0,
                        }],
                    }),
                    padding: vec![J3dPaddingSpan {
                        offset: 0x1c,
                        length: 4,
                        kind: J3dPaddingKind::Zero,
                    }],
                },
                J3dRebuildSection {
                    declared_size: 0,
                    data: J3dRebuildSectionData::Vertices(J3dVertexSection {
                        offsets: [0; 14],
                        formats: vec![
                            J3dVertexAttributeFormat {
                                attribute: 9,
                                component_count: 1,
                                component_type: 4,
                                fractional_bits: 0,
                                reserved: [0; 3],
                            },
                            J3dVertexAttributeFormat {
                                attribute: 0xff,
                                component_count: 0,
                                component_type: 0,
                                fractional_bits: 0,
                                reserved: [0; 3],
                            },
                        ],
                        arrays: vec![J3dVertexArray {
                            attribute: J3dVertexArrayAttribute::Position,
                            offset: 0,
                            values: J3dScalarArray::Float32Bits(positions),
                        }],
                    }),
                    padding: Vec::new(),
                },
                J3dRebuildSection {
                    declared_size: 0,
                    data: J3dRebuildSectionData::Shapes(J3dShapeSection {
                        shape_count: 0,
                        reserved: u16::MAX,
                        offsets: [0; 8],
                        shapes: vec![J3dShapeRecord {
                            matrix_type: 0,
                            reserved_01: 0xff,
                            matrix_group_count: 1,
                            vertex_descriptor_offset: 7,
                            matrix_group_start: 0,
                            draw_start: 0,
                            reserved_0a: u16::MAX,
                            radius: 1.0,
                            bounds_min: [-1.0; 3],
                            bounds_max: [1.0; 3],
                        }],
                        remap: vec![0],
                        names: None,
                        vertex_descriptor_sets: vec![J3dVertexDescriptorSet {
                            relative_offset: 7,
                            descriptors: vec![
                                J3dVertexDescriptor {
                                    attribute: 9,
                                    input_type: 3,
                                },
                                J3dVertexDescriptor {
                                    attribute: 0xff,
                                    input_type: 0,
                                },
                            ],
                        }],
                        matrix_table: vec![0],
                        matrix_groups: vec![J3dShapeMatrixGroup {
                            matrix_index: 0,
                            matrix_count: 1,
                            first_matrix: 0,
                        }],
                        draws: vec![J3dShapeDraw {
                            display_list_size: 0,
                            display_list_offset: 0,
                            commands: vec![J3dGxCommand::Primitive {
                                opcode: 0x90,
                                vertex_count: 0,
                                vertices,
                            }],
                        }],
                    }),
                    padding: Vec::new(),
                },
            ],
        }
    }

    fn position_values(document: &mut J3dRebuildDocument) -> &mut Vec<u32> {
        document
            .sections
            .iter_mut()
            .find_map(|section| match &mut section.data {
                J3dRebuildSectionData::Vertices(vertices) => {
                    vertices.arrays.iter_mut().find_map(|array| {
                        if array.attribute == J3dVertexArrayAttribute::Position {
                            match &mut array.values {
                                J3dScalarArray::Float32Bits(values) => Some(values),
                                _ => None,
                            }
                        } else {
                            None
                        }
                    })
                }
                _ => None,
            })
            .expect("synthetic document has float positions")
    }

    fn primitive_vertices(document: &mut J3dRebuildDocument) -> &mut Vec<Vec<J3dGxVertexOperand>> {
        document
            .sections
            .iter_mut()
            .find_map(|section| match &mut section.data {
                J3dRebuildSectionData::Shapes(shapes) => shapes.draws[0]
                    .commands
                    .iter_mut()
                    .find_map(|command| match command {
                        J3dGxCommand::Primitive { vertices, .. } => Some(vertices),
                        _ => None,
                    }),
                _ => None,
            })
            .expect("synthetic document has a primitive")
    }

    fn geometry_counts(document: &J3dRebuildDocument) -> (u32, u32) {
        document
            .sections
            .iter()
            .find_map(|section| match &section.data {
                J3dRebuildSectionData::Information(information) => {
                    Some((information.vertex_count, information.packet_count))
                }
                _ => None,
            })
            .expect("synthetic document has INF1")
    }

    #[test]
    fn canonical_geometry_layout_grows_vertex_and_primitive_allocations() {
        let mut document = synthetic_geometry_document(3);
        document
            .canonicalize_geometry_layout()
            .expect("canonicalize initial geometry");
        let initial = document.to_bytes().expect("encode initial geometry");

        position_values(&mut document).extend((3..43).flat_map(|index| {
            [
                (index as f32).to_bits(),
                (index as f32 + 0.25).to_bits(),
                (index as f32 + 0.5).to_bits(),
            ]
        }));
        primitive_vertices(&mut document)
            .extend((3..43).map(|index| vec![J3dGxVertexOperand::Index16(index as u16)]));
        document
            .canonicalize_geometry_layout()
            .expect("canonicalize grown geometry");
        let grown = document.to_bytes().expect("encode grown geometry");
        assert!(grown.len() > initial.len());

        let reopened = J3dRebuildDocument::parse(&grown).expect("reparse grown geometry");
        assert_eq!(geometry_counts(&reopened), (43, 1));
        assert_eq!(
            reopened.to_bytes().expect("re-encode grown geometry"),
            grown
        );
    }

    #[test]
    fn canonical_geometry_layout_shrinks_vertex_and_primitive_allocations() {
        let mut document = synthetic_geometry_document(43);
        document
            .canonicalize_geometry_layout()
            .expect("canonicalize initial geometry");
        let initial = document.to_bytes().expect("encode initial geometry");

        position_values(&mut document).truncate(9);
        primitive_vertices(&mut document).truncate(3);
        document
            .canonicalize_geometry_layout()
            .expect("canonicalize shrunken geometry");
        let shrunken = document.to_bytes().expect("encode shrunken geometry");
        assert!(shrunken.len() < initial.len());

        let reopened = J3dRebuildDocument::parse(&shrunken).expect("reparse shrunken geometry");
        assert_eq!(geometry_counts(&reopened), (3, 1));
        assert_eq!(
            reopened.to_bytes().expect("re-encode shrunken geometry"),
            shrunken
        );
    }

    #[test]
    fn failed_geometry_canonicalization_is_transactional() {
        let mut document = synthetic_geometry_document(3);
        let J3dRebuildSectionData::Vertices(vertices) = &mut document.sections[1].data else {
            panic!("synthetic section 1 is VTX1");
        };
        vertices.formats.pop();
        let before = document.clone();

        assert!(document.canonicalize_geometry_layout().is_err());
        assert_eq!(document, before);
    }

    #[test]
    fn canonical_name_layout_rehashes_shift_jis_renames() {
        let mut table = J3dNameTable {
            reserved: u16::MAX,
            entries: vec![J3dNameEntry {
                hash: 0,
                string_offset: 0,
                name: "old".to_string(),
            }],
        };
        canonicalize_name_table_layout(&mut table).expect("canonicalize original name");
        let original_hash = table.entries[0].hash;

        table.entries[0].name = "マリオ".to_string();
        canonicalize_name_table_layout(&mut table).expect("canonicalize renamed Shift-JIS name");

        assert_ne!(table.entries[0].hash, original_hash);
        assert_eq!(table.entries[0].hash, 0xb863);
        assert_eq!(table.entries[0].string_offset, 8);
    }

    #[test]
    fn geometry_canonicalization_rejects_indices_after_vertex_shrink() {
        let mut document = synthetic_geometry_document(3);
        position_values(&mut document).truncate(6);
        let before = document.clone();

        let error = document
            .canonicalize_geometry_layout()
            .expect_err("vertex 2 still references the removed position");
        assert!(format!("{error}").contains("outside its VTX1 cardinality 2"));
        assert_eq!(document, before);
    }

    #[test]
    fn geometry_canonicalization_rejects_explicit_out_of_range_index() {
        let mut document = synthetic_geometry_document(3);
        let vertices = primitive_vertices(&mut document);
        vertices[1][0] = J3dGxVertexOperand::Index16(7);

        let error = document
            .canonicalize_geometry_layout()
            .expect_err("position index 7 exceeds the three-entry VTX1 array");
        assert!(format!("{error}").contains("index 7 is outside its VTX1 cardinality 3"));
    }

    #[test]
    fn geometry_canonicalization_rejects_matrix_table_overrun() {
        let mut document = synthetic_geometry_document(3);
        let shapes = document
            .sections
            .iter_mut()
            .find_map(|section| match &mut section.data {
                J3dRebuildSectionData::Shapes(shapes) => Some(shapes),
                _ => None,
            })
            .expect("synthetic document has SHP1");
        shapes.shapes[0].matrix_type = 3;
        shapes.matrix_groups[0].matrix_count = 2;

        let error = document
            .canonicalize_geometry_layout()
            .expect_err("two-entry matrix palette exceeds one-entry table");
        assert!(format!("{error}").contains("matrix-table range 0..2"));
    }

    #[test]
    fn geometry_canonicalization_rejects_position_matrix_outside_palette() {
        let mut document = synthetic_geometry_document(3);
        for vertex in primitive_vertices(&mut document) {
            vertex.insert(0, J3dGxVertexOperand::DirectU8(3));
        }
        let shapes = document
            .sections
            .iter_mut()
            .find_map(|section| match &mut section.data {
                J3dRebuildSectionData::Shapes(shapes) => Some(shapes),
                _ => None,
            })
            .expect("synthetic document has SHP1");
        shapes.vertex_descriptor_sets[0].descriptors.insert(
            0,
            J3dVertexDescriptor {
                attribute: 0,
                input_type: 1,
            },
        );

        let error = document
            .canonicalize_geometry_layout()
            .expect_err("single-matrix shape loads only palette slot zero");
        assert!(format!("{error}").contains("outside the draw palette capacity 1"));
    }

    #[test]
    fn mat3_unused_tail_words_reconstruct_noncanonical_retail_values() {
        let mut bytes = vec![0; 0x14c];
        let zero = parse_material_init_record(&bytes, 0).expect("parse zero-filled MAT3 tail");
        assert_eq!(
            zero.unused_tail_words,
            J3dMaterialUnusedTailWords::default()
        );
        assert_eq!(zero.unused_tail_words, J3dMaterialUnusedTailWords::ZERO);

        bytes[0x12c..0x144].fill(0xff);
        let ones = parse_material_init_record(&bytes, 0).expect("parse ones-filled MAT3 tail");
        assert_eq!(ones.unused_tail_words, J3dMaterialUnusedTailWords::ONES);
        let mut encoded = vec![0; 0x14c];
        encode_material_init_record(&mut encoded, 0, &ones).expect("encode canonical MAT3 tail");
        assert_eq!(&encoded[0x12c..0x144], &[0xff; 24]);

        let custom_bytes = [
            0x12, 0x34, 0x56, 0x78, 0x9a, 0xbc, 0xde, 0xf0, 0x48, 0x4d, 0x00, 0xff, 0x00, 0x00,
            0x01, 0x50, 0x82, 0x55, 0x10, 0xc8, 0x01, 0x02, 0x00, 0x01,
        ];
        bytes[0x12c..0x144].copy_from_slice(&custom_bytes);
        let custom = parse_material_init_record(&bytes, 0).expect("parse six typed MAT3 words");
        assert_eq!(custom.unused_tail_words.word_00, 0x1234_5678);
        assert_eq!(custom.unused_tail_words.word_04, 0x9abc_def0);
        assert_eq!(custom.unused_tail_words.word_14, 0x0102_0001);
        encoded.fill(0);
        encode_material_init_record(&mut encoded, 0, &custom).expect("encode six typed MAT3 words");
        assert_eq!(&encoded[0x12c..0x144], &custom_bytes);
    }

    #[test]
    fn shared_texture_allocations_reject_conflicting_aliases() {
        let first = J3dTextureBlock {
            absolute_section_offset: 0x80,
            bytes: vec![1, 2, 3, 4],
        };
        let identical_alias = first.clone();
        let conflicting_alias = J3dTextureBlock {
            absolute_section_offset: 0x80,
            bytes: vec![4, 3, 2, 1],
        };
        let mut allocations = BTreeMap::new();
        register_texture_block(&mut allocations, &first).unwrap();
        register_texture_block(&mut allocations, &identical_alias).unwrap();
        assert_eq!(allocations.len(), 1);
        assert!(register_texture_block(&mut allocations, &conflicting_alias).is_err());
    }

    #[test]
    fn semantic_document_does_not_retain_the_source_buffer() {
        let mut source = vec![0u8; 0x40];
        let source_len = source.len() as u32;
        source[..4].copy_from_slice(b"J3D2");
        source[4..8].copy_from_slice(b"bmd3");
        source[8..0x0c].copy_from_slice(&source_len.to_be_bytes());
        source[0x0c..0x10].copy_from_slice(&1u32.to_be_bytes());
        source[0x10..0x14].copy_from_slice(b"SVR3");
        source[0x14..0x20].fill(0xff);
        source[0x20..0x24].copy_from_slice(b"INF1");
        source[0x24..0x28].copy_from_slice(&0x20u32.to_be_bytes());
        source[0x28..0x2a].copy_from_slice(&0u16.to_be_bytes());
        source[0x2a..0x2c].copy_from_slice(&0xffffu16.to_be_bytes());
        source[0x2c..0x30].copy_from_slice(&0u32.to_be_bytes());
        source[0x30..0x34].copy_from_slice(&0u32.to_be_bytes());
        source[0x34..0x38].copy_from_slice(&0x18u32.to_be_bytes());
        source[0x38..0x3c].copy_from_slice(&0u32.to_be_bytes());
        source[0x3c..0x40].copy_from_slice(b"This");
        let expected = source.clone();

        let document = J3dRebuildDocument::parse(&source).expect("parse semantic J3D");
        source.fill(0);

        assert_eq!(document.to_bytes().expect("encode semantic J3D"), expected);
    }

    #[test]
    fn source_free_dolpic_map_model_rebuild_is_identical_when_fixture_exists() {
        let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .join("target/rebuild-audit-dolpic-map.bmd");
        if !fixture.is_file() {
            return;
        }
        let source = std::fs::read(&fixture).expect("read extracted retail test fixture");
        let document = J3dRebuildDocument::parse(&source).expect("parse source-free J3D AST");
        let rebuilt = document.to_bytes().expect("encode source-free J3D AST");
        assert_eq!(rebuilt, source);
    }

    #[test]
    fn canonicalized_dolpic_geometry_reparses_when_fixture_exists() {
        let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .join("target/rebuild-audit-dolpic-map.bmd");
        if !fixture.is_file() {
            return;
        }
        let source = std::fs::read(&fixture).expect("read extracted retail test fixture");
        let mut document = J3dRebuildDocument::parse(&source).expect("parse source-free J3D AST");
        document
            .canonicalize_geometry_layout()
            .expect("canonicalize retail geometry layout");
        let canonical = document.to_bytes().expect("encode canonical J3D AST");
        let reopened = J3dRebuildDocument::parse(&canonical).expect("reparse canonical J3D AST");
        assert_eq!(
            reopened.to_bytes().expect("re-encode canonical J3D AST"),
            canonical
        );
    }

    #[test]
    fn edited_geometry_survives_a_fresh_semantic_reimport_when_fixture_exists() {
        let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .join("target/rebuild-audit-dolpic-map.bmd");
        if !fixture.is_file() {
            return;
        }
        let source = std::fs::read(&fixture).expect("read extracted retail test fixture");
        let mut document = J3dRebuildDocument::parse(&source).expect("parse source-free J3D AST");
        let position_values = document
            .sections
            .iter_mut()
            .find_map(|section| match &mut section.data {
                J3dRebuildSectionData::Vertices(vertices) => vertices
                    .arrays
                    .iter_mut()
                    .find(|array| array.attribute == J3dVertexArrayAttribute::Position)
                    .map(|array| &mut array.values),
                _ => None,
            })
            .expect("Dolpic map has a position array");
        match position_values {
            J3dScalarArray::Unsigned8(values) => values[0] = values[0].wrapping_add(1),
            J3dScalarArray::Signed8(values) => values[0] = values[0].wrapping_add(1),
            J3dScalarArray::Unsigned16(values) => values[0] = values[0].wrapping_add(1),
            J3dScalarArray::Signed16(values) => values[0] = values[0].wrapping_add(1),
            J3dScalarArray::Unsigned32(values) => values[0] = values[0].wrapping_add(1),
            J3dScalarArray::Float32Bits(values) => {
                values[0] = (f32::from_bits(values[0]) + 1.0).to_bits();
            }
            J3dScalarArray::PackedColor(_) => panic!("positions cannot use a packed-color VAT"),
        }
        let expected_positions = position_values.clone();

        let rebuilt = document.to_bytes().expect("encode edited semantic J3D");
        assert_ne!(rebuilt, source);
        let reopened = J3dRebuildDocument::parse(&rebuilt).expect("reimport edited semantic J3D");
        let reopened_positions = reopened
            .sections
            .iter()
            .find_map(|section| match &section.data {
                J3dRebuildSectionData::Vertices(vertices) => vertices
                    .arrays
                    .iter()
                    .find(|array| array.attribute == J3dVertexArrayAttribute::Position)
                    .map(|array| &array.values),
                _ => None,
            })
            .expect("reopened model has a position array");
        assert_eq!(reopened_positions, &expected_positions);
    }

    #[test]
    #[ignore = "requires SMS_BASE_ROOT with extracted retail stage archives"]
    fn source_free_rebuilds_retail_stage_models() {
        let root = std::env::var_os("SMS_BASE_ROOT")
            .map(std::path::PathBuf::from)
            .expect("set SMS_BASE_ROOT to an extracted retail game root");
        let max_files = std::env::var("SMS_J3D_CENSUS_MAX_FILES")
            .ok()
            .and_then(|value| value.parse::<usize>().ok())
            .unwrap_or(usize::MAX);
        let archives = crate::discover_scene_archives(root).expect("discover stage archives");
        let mut rebuilt_count = 0usize;
        let mut bmd_count = 0usize;
        let mut bdl_count = 0usize;
        let mut bmt_count = 0usize;
        let mut overreported_bmt_count = 0usize;
        'archives: for archive_info in archives {
            let source = std::fs::read(&archive_info.path).expect("read stage archive");
            let decoded = if source.starts_with(b"Yaz0") {
                crate::decode_yaz0(&source).expect("decode stage archive")
            } else {
                source
            };
            let archive = crate::RarcArchive::parse(decoded).expect("parse stage archive");
            for entry in archive.file_entries() {
                let path = entry.path.to_ascii_lowercase();
                if !(path.ends_with(".bmd") || path.ends_with(".bdl") || path.ends_with(".bmt")) {
                    continue;
                }
                let original = archive
                    .file_bytes_raw(&entry.raw_path)
                    .expect("read model entry");
                let document = J3dRebuildDocument::parse(&original).unwrap_or_else(|error| {
                    let diagnostic = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                        .join("../..")
                        .join("target/rebuild-audit-j3d-failure.bin");
                    let _ = std::fs::write(diagnostic, &original);
                    panic!(
                        "parse {} in {}: {error}",
                        entry.path,
                        archive_info.path.display()
                    )
                });
                if path.ends_with(".bmt")
                    && document.declared_section_count as usize != document.sections.len()
                {
                    overreported_bmt_count += 1;
                }
                let rebuilt = document.to_bytes().expect("encode source-free J3D");
                assert_eq!(
                    rebuilt,
                    original,
                    "source-free model rebuild differs for {} in {}",
                    entry.path,
                    archive_info.path.display()
                );
                rebuilt_count += 1;
                if path.ends_with(".bdl") {
                    bdl_count += 1;
                } else if path.ends_with(".bmt") {
                    bmt_count += 1;
                } else {
                    bmd_count += 1;
                }
                if rebuilt_count >= max_files {
                    break 'archives;
                }
            }
        }
        if max_files == usize::MAX {
            assert_eq!(bmd_count, 8_664, "retail BMD count drifted");
            assert_eq!(bdl_count, 1, "retail BDL count drifted");
            assert_eq!(bmt_count, 507, "retail BMT count drifted");
            assert_eq!(rebuilt_count, 9_172, "retail J3D resource count drifted");
        } else {
            assert!(rebuilt_count > 0, "retail census found no BMD/BDL models");
        }
        eprintln!(
            "source-free J3D census rebuilt {rebuilt_count} stage resources ({bmd_count} BMD, {bdl_count} BDL, {bmt_count} BMT; {overreported_bmt_count} BMT with a retail-overreported section count)"
        );
    }

    #[test]
    #[ignore = "requires SMS_BASE_ROOT with extracted retail stage archives"]
    fn canonical_geometry_layout_reimports_retail_stage_models() {
        let root = std::env::var_os("SMS_BASE_ROOT")
            .map(std::path::PathBuf::from)
            .expect("set SMS_BASE_ROOT to an extracted retail game root");
        let archives = crate::discover_scene_archives(root).expect("discover stage archives");
        let mut rebuilt_count = 0usize;
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
                if !(path.ends_with(".bmd") || path.ends_with(".bdl")) {
                    continue;
                }
                let original = archive
                    .file_bytes_raw(&entry.raw_path)
                    .expect("read model entry");
                let mut document = J3dRebuildDocument::parse(&original).unwrap_or_else(|error| {
                    panic!(
                        "parse {} in {}: {error}",
                        entry.path,
                        archive_info.path.display()
                    )
                });
                let imported_counts = geometry_counts(&document);
                document
                    .canonicalize_geometry_layout()
                    .unwrap_or_else(|error| {
                        panic!(
                            "canonicalize {} in {}: {error}",
                            entry.path,
                            archive_info.path.display()
                        )
                    });
                assert_eq!(
                    geometry_counts(&document),
                    imported_counts,
                    "canonical counts changed for untouched {} in {}",
                    entry.path,
                    archive_info.path.display()
                );
                let canonical = document.to_bytes().expect("encode canonical model");
                let reopened = J3dRebuildDocument::parse(&canonical).unwrap_or_else(|error| {
                    panic!(
                        "reparse canonical {} in {}: {error}",
                        entry.path,
                        archive_info.path.display()
                    )
                });
                assert_eq!(
                    reopened.to_bytes().expect("re-encode canonical model"),
                    canonical,
                    "canonical model is not stable for {} in {}",
                    entry.path,
                    archive_info.path.display()
                );
                rebuilt_count += 1;
            }
        }
        assert_eq!(
            rebuilt_count, 8_665,
            "retail geometry resource count drifted"
        );
        eprintln!("canonical J3D geometry census rebuilt {rebuilt_count} BMD/BDL resources");
    }
}
