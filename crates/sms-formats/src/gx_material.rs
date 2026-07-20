//! Semantic GX/MAT3 authoring and deterministic bank compilation.
//!
//! Values intentionally use GX's native numeric enums. This permits complete
//! authoring without forcing the editor to discard SDK values that its current
//! viewport cannot emulate.

use std::collections::BTreeMap;

use encoding_rs::SHIFT_JIS;
use serde::{Deserialize, Serialize};

use crate::{
    FormatError, J3dMaterialInitRecord, J3dMaterialSection, J3dMaterialTable, J3dMaterialTableKind,
    J3dMaterialUnusedTailWords, J3dNameEntry, J3dNameTable, J3dRebuildSection,
    J3dRebuildSectionData, J3dScalarArray, Result, SMS_DEFAULT_OBJECT_MODEL_LOAD_FLAGS,
    SMS_MAP_MODEL_LOAD_FLAGS, SMS_POLLUTION_MODEL_LOAD_FLAGS,
};

const FORMAT: &str = "GX/MAT3 authoring";
const ABSENT_U8: u8 = 0xff;
const ABSENT_U16: u16 = 0xffff;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct GxColorChannel {
    pub enable: u8,
    pub material_source: u8,
    pub light_mask: u8,
    pub diffuse_function: u8,
    pub attenuation_function: u8,
    pub ambient_source: u8,
}

impl Default for GxColorChannel {
    fn default() -> Self {
        Self {
            enable: 0,
            material_source: 0,
            light_mask: 0,
            diffuse_function: 2,
            attenuation_function: 2,
            ambient_source: 0,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct GxLight {
    pub color: [u8; 4],
    pub position: [f32; 3],
    pub direction: [f32; 3],
    pub distance_attenuation: [f32; 3],
    pub angle_attenuation: [f32; 3],
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct GxTexCoordGen {
    pub function: u8,
    pub source: u8,
    pub matrix: u8,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct GxTexMatrix {
    pub projection: u8,
    /// Low seven bits are the mapping mode; bit seven is the Maya convention.
    pub mapping_mode: u8,
    pub center: [f32; 3],
    pub scale: [f32; 2],
    pub rotation: i16,
    pub translation: [f32; 2],
    pub effect_matrix: [[f32; 4]; 4],
}

impl Default for GxTexMatrix {
    fn default() -> Self {
        let mut effect_matrix = [[0.0; 4]; 4];
        for (index, row) in effect_matrix.iter_mut().enumerate() {
            row[index] = 1.0;
        }
        Self {
            projection: 0,
            mapping_mode: 0,
            center: [0.5, 0.5, 0.5],
            scale: [1.0, 1.0],
            rotation: 0,
            translation: [0.0, 0.0],
            effect_matrix,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct GxTevOrder {
    pub tex_coord: Option<u8>,
    pub tex_map: Option<u8>,
    pub color_channel: u8,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct GxTevStage {
    /// J3D's leading TEV-stage byte, retained explicitly even though GX does
    /// not consume it as a combine argument.
    pub reserved: u8,
    pub color_inputs: [u8; 4],
    pub color_operation: u8,
    pub color_bias: u8,
    pub color_scale: u8,
    pub color_clamp: u8,
    pub color_register: u8,
    pub alpha_inputs: [u8; 4],
    pub alpha_operation: u8,
    pub alpha_bias: u8,
    pub alpha_scale: u8,
    pub alpha_clamp: u8,
    pub alpha_register: u8,
}

impl Default for GxTevStage {
    fn default() -> Self {
        Self {
            reserved: 0xff,
            color_inputs: [4, 10, 15, 15],
            color_operation: 0,
            color_bias: 0,
            color_scale: 0,
            color_clamp: 1,
            color_register: 0,
            alpha_inputs: [5, 7, 7, 0],
            alpha_operation: 0,
            alpha_bias: 0,
            alpha_scale: 0,
            alpha_clamp: 1,
            alpha_register: 0,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct GxTevSwapMode {
    pub raster_swap_table: u8,
    pub texture_swap_table: u8,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct GxTevSwapTable {
    pub red: u8,
    pub green: u8,
    pub blue: u8,
    pub alpha: u8,
}

impl Default for GxTevSwapTable {
    fn default() -> Self {
        Self {
            red: 0,
            green: 1,
            blue: 2,
            alpha: 3,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct GxIndirectOrder {
    pub tex_coord: Option<u8>,
    pub tex_map: Option<u8>,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct GxIndirectMatrix {
    pub rows: [[f32; 3]; 2],
    pub scale_exponent: i8,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct GxIndirectScale {
    pub scale_s: u8,
    pub scale_t: u8,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GxIndirectTevStage {
    pub stage: u8,
    pub format: u8,
    pub bias: u8,
    pub matrix: u8,
    pub wrap_s: u8,
    pub wrap_t: u8,
    pub add_previous: u8,
    pub use_original_lod: u8,
    pub alpha: u8,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct GxIndirectMaterial {
    pub enabled: bool,
    pub stage_count: u8,
    pub orders: [Option<GxIndirectOrder>; 4],
    pub matrices: [Option<GxIndirectMatrix>; 3],
    pub scales: [Option<GxIndirectScale>; 4],
    pub tev_stages: [GxIndirectTevStage; 16],
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct GxFog {
    pub function: u8,
    pub range_adjustment_enabled: u8,
    pub center: u16,
    pub start_z: f32,
    pub end_z: f32,
    pub near_z: f32,
    pub far_z: f32,
    pub color: [u8; 4],
    pub range_adjustment_table: [u16; 10],
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct GxAlphaCompare {
    pub comparison_0: u8,
    pub reference_0: u8,
    pub operation: u8,
    pub comparison_1: u8,
    pub reference_1: u8,
}

impl Default for GxAlphaCompare {
    fn default() -> Self {
        Self {
            comparison_0: 7,
            reference_0: 0,
            operation: 0,
            comparison_1: 7,
            reference_1: 0,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct GxBlendMode {
    pub mode: u8,
    pub source_factor: u8,
    pub destination_factor: u8,
    pub logic_operation: u8,
}

impl Default for GxBlendMode {
    fn default() -> Self {
        Self {
            mode: 0,
            source_factor: 1,
            destination_factor: 0,
            logic_operation: 0xf,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct GxDepthMode {
    pub comparison_enabled: u8,
    pub function: u8,
    pub update_enabled: u8,
}

impl Default for GxDepthMode {
    fn default() -> Self {
        Self {
            comparison_enabled: 1,
            function: 3,
            update_enabled: 1,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct GxNbtScale {
    pub enabled: u8,
    pub scale: [f32; 3],
}

impl Default for GxNbtScale {
    fn default() -> Self {
        Self {
            enabled: 0,
            scale: [1.0; 3],
        }
    }
}

/// Complete semantic material consumed by the MAT3 compiler.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GxMaterial {
    pub name: String,
    pub material_mode: u8,
    pub cull_mode: u32,
    pub color_channel_count: u8,
    pub tex_gen_count: u8,
    pub tev_stage_count: u8,
    pub material_colors: [Option<[u8; 4]>; 2],
    pub color_channels: [Option<GxColorChannel>; 4],
    pub ambient_colors: [Option<[u8; 4]>; 2],
    pub lights: [Option<GxLight>; 8],
    pub tex_gens: [Option<GxTexCoordGen>; 8],
    pub post_tex_gens: [Option<GxTexCoordGen>; 8],
    pub tex_matrices: [Option<GxTexMatrix>; 10],
    pub post_tex_matrices: [Option<GxTexMatrix>; 20],
    pub texture_numbers: [Option<u16>; 8],
    pub tev_konst_colors: [Option<[u8; 4]>; 4],
    pub tev_konst_color_selectors: [u8; 16],
    pub tev_konst_alpha_selectors: [u8; 16],
    pub tev_orders: [Option<GxTevOrder>; 16],
    pub tev_colors: [Option<[i16; 4]>; 4],
    pub tev_stages: [Option<GxTevStage>; 16],
    pub tev_swap_modes: [Option<GxTevSwapMode>; 16],
    pub tev_swap_tables: [Option<GxTevSwapTable>; 4],
    pub indirect: GxIndirectMaterial,
    pub fog: Option<GxFog>,
    pub alpha_compare: GxAlphaCompare,
    pub blend_mode: GxBlendMode,
    pub depth_mode: GxDepthMode,
    pub z_compare_location: u8,
    pub dither: u8,
    pub nbt_scale: GxNbtScale,
}

impl Default for GxMaterial {
    fn default() -> Self {
        let mut tev_orders = [None; 16];
        tev_orders[0] = Some(GxTevOrder {
            tex_coord: None,
            tex_map: None,
            color_channel: 0xff,
        });
        let mut tev_stages = [None; 16];
        tev_stages[0] = Some(GxTevStage::default());
        let mut tev_swap_modes = [None; 16];
        tev_swap_modes[0] = Some(GxTevSwapMode {
            raster_swap_table: 0,
            texture_swap_table: 0,
        });
        Self {
            name: "material".to_string(),
            material_mode: 1,
            cull_mode: 2,
            color_channel_count: 0,
            tex_gen_count: 0,
            tev_stage_count: 1,
            material_colors: [Some([255; 4]), None],
            color_channels: [None; 4],
            ambient_colors: [Some([50; 4]), None],
            lights: [None; 8],
            tex_gens: [None; 8],
            post_tex_gens: [None; 8],
            tex_matrices: [None; 10],
            post_tex_matrices: [None; 20],
            texture_numbers: [None; 8],
            tev_konst_colors: [None; 4],
            tev_konst_color_selectors: [0x0c; 16],
            tev_konst_alpha_selectors: [0x1c; 16],
            tev_orders,
            tev_colors: [None; 4],
            tev_stages,
            tev_swap_modes,
            tev_swap_tables: [Some(GxTevSwapTable::default()); 4],
            indirect: GxIndirectMaterial::default(),
            fog: None,
            alpha_compare: GxAlphaCompare::default(),
            blend_mode: GxBlendMode::default(),
            depth_mode: GxDepthMode::default(),
            z_compare_location: 1,
            dither: 0,
            nbt_scale: GxNbtScale::default(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TargetLoaderProfile {
    Full,
    SunshineMap,
    SunshineObject,
    SunshinePollution,
    Custom(u32),
}

impl TargetLoaderProfile {
    pub const fn flags(self) -> u32 {
        match self {
            Self::Full => u32::MAX,
            Self::SunshineMap => SMS_MAP_MODEL_LOAD_FLAGS,
            Self::SunshineObject => SMS_DEFAULT_OBJECT_MODEL_LOAD_FLAGS,
            Self::SunshinePollution => SMS_POLLUTION_MODEL_LOAD_FLAGS,
            Self::Custom(flags) => flags,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum GxDiagnosticSeverity {
    Warning,
    Error,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GxMaterialDiagnostic {
    pub severity: GxDiagnosticSeverity,
    pub code: String,
    pub material_index: usize,
    pub message: String,
}

const J3DMLF_MATERIAL_COLOR_LIGHT_ON: u32 = 0x4000_0000;
const J3D_DEFAULT_AMBIENT_COLOR: [u8; 4] = [0x32; 4];

pub fn validate_materials_for_loader(
    materials: &[GxMaterial],
    profile: TargetLoaderProfile,
) -> Vec<GxMaterialDiagnostic> {
    let flags = profile.flags();
    let mut diagnostics = Vec::new();
    for (material_index, material) in materials.iter().enumerate() {
        if material.indirect.enabled && flags & 0x0100_0000 == 0 {
            diagnostics.push(GxMaterialDiagnostic {
                severity: GxDiagnosticSeverity::Warning,
                code: "loader-ignores-indirect".to_string(),
                material_index,
                message: format!(
                    "target loader flags {flags:#010x} do not instantiate MAT3 indirect state"
                ),
            });
        }
        if flags & J3DMLF_MATERIAL_COLOR_LIGHT_ON == 0 {
            let non_default_ambient_slots = material
                .ambient_colors
                .iter()
                .enumerate()
                .filter_map(|(slot, color)| match color {
                    Some(color) if *color != J3D_DEFAULT_AMBIENT_COLOR => Some(slot),
                    _ => None,
                })
                .collect::<Vec<_>>();
            if !non_default_ambient_slots.is_empty() {
                diagnostics.push(GxMaterialDiagnostic {
                    severity: GxDiagnosticSeverity::Warning,
                    code: "loader-ignores-material-ambient-colors".to_string(),
                    material_index,
                    message: format!(
                        "target loader flags {flags:#010x} instantiate J3DColorBlockLightOff; MAT3 ambient color slot(s) {non_default_ambient_slots:?} differ from J3D's default [50, 50, 50, 50] and are not loaded, so GX ambient color must be supplied externally"
                    ),
                });
            }

            let embedded_light_slots = material
                .lights
                .iter()
                .enumerate()
                .filter_map(|(slot, light)| light.is_some().then_some(slot))
                .collect::<Vec<_>>();
            if !embedded_light_slots.is_empty() {
                diagnostics.push(GxMaterialDiagnostic {
                    severity: GxDiagnosticSeverity::Warning,
                    code: "loader-ignores-embedded-lights".to_string(),
                    material_index,
                    message: format!(
                        "target loader flags {flags:#010x} instantiate J3DColorBlockLightOff; embedded MAT3 light slot(s) {embedded_light_slots:?} are not allocated or loaded, while material colors and channel controls remain active and use externally configured GX lights"
                    ),
                });
            }
        }
        if flags & 0x1000_0000 == 0
            && (material.alpha_compare != GxAlphaCompare::default()
                || material.blend_mode != GxBlendMode::default()
                || material.depth_mode != GxDepthMode::default()
                || material.z_compare_location != 1)
        {
            diagnostics.push(GxMaterialDiagnostic {
                severity: GxDiagnosticSeverity::Warning,
                code: "loader-derives-pixel-engine-state".to_string(),
                material_index,
                message: format!(
                    "target loader flags {flags:#010x} derive pixel-engine state from material mode"
                ),
            });
        }
    }
    diagnostics
}

/// Compiles semantic materials into canonical MAT3 init records and deduplicated
/// allocation banks. Identical values always receive the first stable index.
pub fn compile_material_section(materials: &[GxMaterial]) -> Result<J3dRebuildSection> {
    if materials.is_empty() {
        return Err(unsupported("BMD3 requires at least one material"));
    }
    if materials.len() > u16::MAX as usize {
        return Err(limit("materials", materials.len(), u16::MAX as usize));
    }

    let mut banks = MaterialBanks::default();
    let mut records = Vec::with_capacity(materials.len());
    let mut indirect_records = Vec::with_capacity(materials.len());
    for material in materials {
        validate_material(material)?;
        records.push(compile_material(material, &mut banks)?);
        indirect_records.push(encode_indirect(&material.indirect)?);
    }
    banks
        .values
        .insert(J3dMaterialTableKind::IndirectInit, indirect_records);

    let remap = (0..materials.len())
        .map(|index| index as u16)
        .collect::<Vec<_>>();
    let mut names = build_name_table(materials.iter().map(|material| material.name.as_str()))?;
    let name_size = canonicalize_name_table(&mut names)?;

    let mut offsets = [0u32; 30];
    let mut cursor = 0x84usize;
    offsets[0] = cursor as u32;
    cursor = checked_add(cursor, records.len() * 0x14c, "MAT3 init records")?;
    cursor = align(cursor, 4)?;
    offsets[1] = cursor as u32;
    cursor = checked_add(cursor, remap.len() * 2, "MAT3 remap")?;
    cursor = align(cursor, 4)?;
    offsets[2] = cursor as u32;
    cursor = checked_add(cursor, name_size, "MAT3 names")?;

    let mut tables = vec![J3dMaterialTable {
        kind: J3dMaterialTableKind::MaterialRemap,
        offset: offsets[1],
        allocation: J3dScalarArray::Unsigned16(remap),
    }];
    for (offset_index, kind) in MAT3_KINDS.iter().copied().enumerate().skip(3) {
        let Some(entries) = banks.values.get(&kind) else {
            continue;
        };
        if entries.is_empty() {
            continue;
        }
        let alignment = table_alignment(kind);
        cursor = align(cursor, alignment)?;
        offsets[offset_index] = cursor as u32;
        let bytes = entries.concat();
        cursor = checked_add(cursor, bytes.len(), "MAT3 bank")?;
        tables.push(J3dMaterialTable {
            kind,
            offset: offsets[offset_index],
            allocation: table_scalar_array(kind, bytes)?,
        });
    }
    let declared_size = align(cursor, 0x20)?;
    Ok(J3dRebuildSection {
        declared_size: declared_size as u32,
        data: J3dRebuildSectionData::Materials(J3dMaterialSection {
            material_count: materials.len() as u16,
            reserved: u16::MAX,
            offsets,
            material_init_records: records,
            names: Some(names),
            tables,
        }),
        padding: Vec::new(),
    })
}

const MAT3_KINDS: [J3dMaterialTableKind; 30] = [
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

#[derive(Default)]
struct MaterialBanks {
    values: BTreeMap<J3dMaterialTableKind, Vec<Vec<u8>>>,
}

impl MaterialBanks {
    fn intern_u8(&mut self, kind: J3dMaterialTableKind, bytes: Vec<u8>) -> Result<u8> {
        let index = self.intern(kind, bytes)?;
        u8::try_from(index).map_err(|_| limit("MAT3 byte-indexed bank", index + 1, 256))
    }

    fn intern_u16(&mut self, kind: J3dMaterialTableKind, bytes: Vec<u8>) -> Result<u16> {
        let index = self.intern(kind, bytes)?;
        u16::try_from(index).map_err(|_| limit("MAT3 word-indexed bank", index + 1, 65_536))
    }

    fn intern(&mut self, kind: J3dMaterialTableKind, bytes: Vec<u8>) -> Result<usize> {
        let values = self.values.entry(kind).or_default();
        if let Some(index) = values.iter().position(|candidate| *candidate == bytes) {
            return Ok(index);
        }
        let index = values.len();
        values.push(bytes);
        Ok(index)
    }
}

fn compile_material(
    material: &GxMaterial,
    banks: &mut MaterialBanks,
) -> Result<J3dMaterialInitRecord> {
    let mut record = J3dMaterialInitRecord {
        material_mode: material.material_mode,
        cull_mode_index: banks.intern_u8(
            J3dMaterialTableKind::CullMode,
            material.cull_mode.to_be_bytes().to_vec(),
        )?,
        color_channel_count_index: banks.intern_u8(
            J3dMaterialTableKind::ColorChannelCount,
            vec![material.color_channel_count],
        )?,
        tex_gen_count_index: banks.intern_u8(
            J3dMaterialTableKind::TexGenCount,
            vec![material.tex_gen_count],
        )?,
        tev_stage_count_index: banks.intern_u8(
            J3dMaterialTableKind::TevStageCount,
            vec![material.tev_stage_count],
        )?,
        z_compare_location_index: banks.intern_u8(
            J3dMaterialTableKind::ZCompareLocation,
            vec![material.z_compare_location],
        )?,
        z_mode_index: banks.intern_u8(
            J3dMaterialTableKind::ZMode,
            encode_depth(material.depth_mode),
        )?,
        dither_index: banks.intern_u8(J3dMaterialTableKind::Dither, vec![material.dither])?,
        material_color_indices: [ABSENT_U16; 2],
        color_channel_indices: [ABSENT_U16; 4],
        ambient_color_indices: [ABSENT_U16; 2],
        light_indices: [ABSENT_U16; 8],
        tex_coord_indices: [ABSENT_U16; 8],
        post_tex_coord_indices: [ABSENT_U16; 8],
        tex_matrix_indices: [ABSENT_U16; 10],
        post_tex_matrix_indices: [ABSENT_U16; 20],
        texture_number_indices: [ABSENT_U16; 8],
        tev_konst_color_indices: [ABSENT_U16; 4],
        tev_konst_color_selectors: material.tev_konst_color_selectors,
        tev_konst_alpha_selectors: material.tev_konst_alpha_selectors,
        tev_order_indices: [ABSENT_U16; 16],
        tev_color_indices: [ABSENT_U16; 4],
        tev_stage_indices: [ABSENT_U16; 16],
        tev_swap_mode_indices: [ABSENT_U16; 16],
        tev_swap_table_indices: [ABSENT_U16; 4],
        unused_tail_words: J3dMaterialUnusedTailWords::ZERO,
        fog_index: ABSENT_U16,
        alpha_compare_index: banks.intern_u16(
            J3dMaterialTableKind::AlphaCompare,
            encode_alpha(material.alpha_compare),
        )?,
        blend_index: banks.intern_u16(
            J3dMaterialTableKind::Blend,
            encode_blend(material.blend_mode),
        )?,
        nbt_scale_index: banks.intern_u16(
            J3dMaterialTableKind::NbtScale,
            encode_nbt(material.nbt_scale),
        )?,
    };
    intern_options(
        banks,
        J3dMaterialTableKind::MaterialColor,
        &material.material_colors,
        &mut record.material_color_indices,
        |value| value.to_vec(),
    )?;
    intern_options(
        banks,
        J3dMaterialTableKind::ColorChannel,
        &material.color_channels,
        &mut record.color_channel_indices,
        |value| encode_color_channel(*value),
    )?;
    intern_options(
        banks,
        J3dMaterialTableKind::AmbientColor,
        &material.ambient_colors,
        &mut record.ambient_color_indices,
        |value| value.to_vec(),
    )?;
    intern_options(
        banks,
        J3dMaterialTableKind::Light,
        &material.lights,
        &mut record.light_indices,
        |value| encode_light(*value),
    )?;
    intern_options(
        banks,
        J3dMaterialTableKind::TexCoord,
        &material.tex_gens,
        &mut record.tex_coord_indices,
        |value| encode_tex_gen(*value),
    )?;
    intern_options(
        banks,
        J3dMaterialTableKind::TexCoord2,
        &material.post_tex_gens,
        &mut record.post_tex_coord_indices,
        |value| encode_tex_gen(*value),
    )?;
    intern_options(
        banks,
        J3dMaterialTableKind::TexMatrix,
        &material.tex_matrices,
        &mut record.tex_matrix_indices,
        |value| encode_tex_matrix(*value),
    )?;
    intern_options(
        banks,
        J3dMaterialTableKind::PostTexMatrix,
        &material.post_tex_matrices,
        &mut record.post_tex_matrix_indices,
        |value| encode_tex_matrix(*value),
    )?;
    intern_options(
        banks,
        J3dMaterialTableKind::TextureNumber,
        &material.texture_numbers,
        &mut record.texture_number_indices,
        |value| value.to_be_bytes().to_vec(),
    )?;
    intern_options(
        banks,
        J3dMaterialTableKind::TevKonstColor,
        &material.tev_konst_colors,
        &mut record.tev_konst_color_indices,
        |value| value.to_vec(),
    )?;
    intern_options(
        banks,
        J3dMaterialTableKind::TevOrder,
        &material.tev_orders,
        &mut record.tev_order_indices,
        |value| encode_tev_order(*value),
    )?;
    intern_options(
        banks,
        J3dMaterialTableKind::TevColor,
        &material.tev_colors,
        &mut record.tev_color_indices,
        |value| value.iter().flat_map(|v| v.to_be_bytes()).collect(),
    )?;
    intern_options(
        banks,
        J3dMaterialTableKind::TevStage,
        &material.tev_stages,
        &mut record.tev_stage_indices,
        |value| encode_tev_stage(*value),
    )?;
    intern_options(
        banks,
        J3dMaterialTableKind::TevSwapMode,
        &material.tev_swap_modes,
        &mut record.tev_swap_mode_indices,
        |value| encode_swap_mode(*value),
    )?;
    intern_options(
        banks,
        J3dMaterialTableKind::TevSwapTable,
        &material.tev_swap_tables,
        &mut record.tev_swap_table_indices,
        |value| encode_swap_table(*value),
    )?;
    if let Some(fog) = material.fog {
        record.fog_index = banks.intern_u16(J3dMaterialTableKind::Fog, encode_fog(fog))?;
    }
    Ok(record)
}

fn intern_options<T, const N: usize>(
    banks: &mut MaterialBanks,
    kind: J3dMaterialTableKind,
    source: &[Option<T>; N],
    target: &mut [u16; N],
    encode: impl Fn(&T) -> Vec<u8>,
) -> Result<()> {
    for (index, value) in source.iter().enumerate() {
        if let Some(value) = value {
            target[index] = banks.intern_u16(kind, encode(value))?;
        }
    }
    Ok(())
}

fn validate_material(material: &GxMaterial) -> Result<()> {
    validate_name(&material.name)?;
    if material.color_channel_count > 4 {
        return Err(unsupported(format!(
            "material {:?} has {} color channels, limit is 4",
            material.name, material.color_channel_count
        )));
    }
    if material.tex_gen_count > 8 {
        return Err(unsupported(format!(
            "material {:?} has {} texgens, limit is 8",
            material.name, material.tex_gen_count
        )));
    }
    if material.tev_stage_count == 0 || material.tev_stage_count > 16 {
        return Err(unsupported(format!(
            "material {:?} has {} TEV stages, expected 1..=16",
            material.name, material.tev_stage_count
        )));
    }
    if material.indirect.stage_count > 4 {
        return Err(unsupported(format!(
            "material {:?} has {} indirect stages, limit is 4",
            material.name, material.indirect.stage_count
        )));
    }
    Ok(())
}

fn encode_color_channel(value: GxColorChannel) -> Vec<u8> {
    vec![
        value.enable,
        value.material_source,
        value.light_mask,
        value.diffuse_function,
        value.attenuation_function,
        value.ambient_source,
        0xff,
        0xff,
    ]
}

fn encode_light(value: GxLight) -> Vec<u8> {
    let mut out = vec![0; 0x34];
    out[..4].copy_from_slice(&value.color);
    put_f32_array(&mut out, 4, &value.position);
    put_f32_array(&mut out, 0x10, &value.direction);
    put_f32_array(&mut out, 0x1c, &value.distance_attenuation);
    put_f32_array(&mut out, 0x28, &value.angle_attenuation);
    out
}

fn encode_tex_gen(value: GxTexCoordGen) -> Vec<u8> {
    vec![value.function, value.source, value.matrix, 0xff]
}

fn encode_tex_matrix(value: GxTexMatrix) -> Vec<u8> {
    let mut out = vec![0; 0x64];
    out[0] = value.projection;
    out[1] = value.mapping_mode;
    out[2..4].fill(0xff);
    put_f32_array(&mut out, 4, &value.center);
    put_f32_array(&mut out, 0x10, &value.scale);
    out[0x18..0x1a].copy_from_slice(&value.rotation.to_be_bytes());
    out[0x1a..0x1c].fill(0xff);
    put_f32_array(&mut out, 0x1c, &value.translation);
    let effect = value
        .effect_matrix
        .into_iter()
        .flatten()
        .collect::<Vec<_>>();
    put_f32_array(&mut out, 0x24, &effect);
    out
}

fn encode_tev_order(value: GxTevOrder) -> Vec<u8> {
    vec![
        value.tex_coord.unwrap_or(ABSENT_U8),
        value.tex_map.unwrap_or(ABSENT_U8),
        value.color_channel,
        0xff,
    ]
}

fn encode_tev_stage(value: GxTevStage) -> Vec<u8> {
    let mut out = Vec::with_capacity(0x14);
    out.push(value.reserved);
    out.extend_from_slice(&value.color_inputs);
    out.extend_from_slice(&[
        value.color_operation,
        value.color_bias,
        value.color_scale,
        value.color_clamp,
        value.color_register,
    ]);
    out.extend_from_slice(&value.alpha_inputs);
    out.extend_from_slice(&[
        value.alpha_operation,
        value.alpha_bias,
        value.alpha_scale,
        value.alpha_clamp,
        value.alpha_register,
        0xff,
    ]);
    debug_assert_eq!(out.len(), 0x14);
    out
}

fn encode_swap_mode(value: GxTevSwapMode) -> Vec<u8> {
    vec![
        value.raster_swap_table,
        value.texture_swap_table,
        0xff,
        0xff,
    ]
}

fn encode_swap_table(value: GxTevSwapTable) -> Vec<u8> {
    vec![value.red, value.green, value.blue, value.alpha]
}

fn encode_indirect(value: &GxIndirectMaterial) -> Result<Vec<u8>> {
    if value.stage_count > 4 {
        return Err(unsupported("indirect stage count exceeds four"));
    }
    let mut out = vec![0; 0x138];
    out[0] = value.enabled.into();
    out[1] = value.stage_count;
    out[2..4].fill(0xff);
    for (index, order) in value.orders.iter().enumerate() {
        let base = 4 + index * 4;
        if let Some(order) = order {
            out[base] = order.tex_coord.unwrap_or(ABSENT_U8);
            out[base + 1] = order.tex_map.unwrap_or(ABSENT_U8);
        } else {
            out[base..base + 2].fill(ABSENT_U8);
        }
        out[base + 2..base + 4].fill(0xff);
    }
    for (index, matrix) in value.matrices.iter().enumerate() {
        let base = 0x14 + index * 0x1c;
        if let Some(matrix) = matrix {
            let values = matrix.rows.into_iter().flatten().collect::<Vec<_>>();
            put_f32_array(&mut out, base, &values);
            out[base + 0x18] = matrix.scale_exponent as u8;
        }
        out[base + 0x19..base + 0x1c].fill(0xff);
    }
    for (index, scale) in value.scales.iter().enumerate() {
        let base = 0x68 + index * 4;
        if let Some(scale) = scale {
            out[base] = scale.scale_s;
            out[base + 1] = scale.scale_t;
        }
        out[base + 2..base + 4].fill(0xff);
    }
    for (index, stage) in value.tev_stages.iter().enumerate() {
        let base = 0x78 + index * 0x0c;
        out[base..base + 9].copy_from_slice(&[
            stage.stage,
            stage.format,
            stage.bias,
            stage.matrix,
            stage.wrap_s,
            stage.wrap_t,
            stage.add_previous,
            stage.use_original_lod,
            stage.alpha,
        ]);
        out[base + 9..base + 0x0c].fill(0xff);
    }
    Ok(out)
}

fn encode_fog(value: GxFog) -> Vec<u8> {
    let mut out = vec![0; 0x2c];
    out[0] = value.function;
    out[1] = value.range_adjustment_enabled;
    out[2..4].copy_from_slice(&value.center.to_be_bytes());
    put_f32_array(
        &mut out,
        4,
        &[value.start_z, value.end_z, value.near_z, value.far_z],
    );
    out[0x14..0x18].copy_from_slice(&value.color);
    for (index, entry) in value.range_adjustment_table.iter().enumerate() {
        out[0x18 + index * 2..0x1a + index * 2].copy_from_slice(&entry.to_be_bytes());
    }
    out
}

fn encode_alpha(value: GxAlphaCompare) -> Vec<u8> {
    vec![
        value.comparison_0,
        value.reference_0,
        value.operation,
        value.comparison_1,
        value.reference_1,
        0xff,
        0xff,
        0xff,
    ]
}

fn encode_blend(value: GxBlendMode) -> Vec<u8> {
    vec![
        value.mode,
        value.source_factor,
        value.destination_factor,
        value.logic_operation,
    ]
}

fn encode_depth(value: GxDepthMode) -> Vec<u8> {
    vec![
        value.comparison_enabled,
        value.function,
        value.update_enabled,
        0xff,
    ]
}

fn encode_nbt(value: GxNbtScale) -> Vec<u8> {
    let mut out = vec![0xff; 0x10];
    out[0] = value.enabled;
    put_f32_array(&mut out, 4, &value.scale);
    out
}

fn put_f32_array(out: &mut [u8], offset: usize, values: &[f32]) {
    for (index, value) in values.iter().enumerate() {
        out[offset + index * 4..offset + index * 4 + 4]
            .copy_from_slice(&value.to_bits().to_be_bytes());
    }
}

fn table_alignment(kind: J3dMaterialTableKind) -> usize {
    match kind {
        J3dMaterialTableKind::CullMode
        | J3dMaterialTableKind::Light
        | J3dMaterialTableKind::TexMatrix
        | J3dMaterialTableKind::PostTexMatrix
        | J3dMaterialTableKind::Fog
        | J3dMaterialTableKind::NbtScale
        | J3dMaterialTableKind::IndirectInit => 4,
        J3dMaterialTableKind::TextureNumber | J3dMaterialTableKind::TevColor => 2,
        _ => 1,
    }
}

fn table_scalar_array(kind: J3dMaterialTableKind, bytes: Vec<u8>) -> Result<J3dScalarArray> {
    match kind {
        J3dMaterialTableKind::CullMode => Ok(J3dScalarArray::Unsigned32(
            bytes
                .chunks_exact(4)
                .map(|chunk| u32::from_be_bytes(chunk.try_into().expect("four-byte chunk")))
                .collect(),
        )),
        J3dMaterialTableKind::TextureNumber => Ok(J3dScalarArray::Unsigned16(
            bytes
                .chunks_exact(2)
                .map(|chunk| u16::from_be_bytes(chunk.try_into().expect("two-byte chunk")))
                .collect(),
        )),
        J3dMaterialTableKind::TevColor => Ok(J3dScalarArray::Signed16(
            bytes
                .chunks_exact(2)
                .map(|chunk| i16::from_be_bytes(chunk.try_into().expect("two-byte chunk")))
                .collect(),
        )),
        _ => Ok(J3dScalarArray::Unsigned8(bytes)),
    }
}

fn build_name_table<'a>(names: impl IntoIterator<Item = &'a str>) -> Result<J3dNameTable> {
    Ok(J3dNameTable {
        reserved: u16::MAX,
        entries: names
            .into_iter()
            .map(|name| {
                validate_name(name)?;
                Ok(J3dNameEntry {
                    hash: 0,
                    string_offset: 0,
                    name: name.to_string(),
                })
            })
            .collect::<Result<Vec<_>>>()?,
    })
}

fn validate_name(name: &str) -> Result<()> {
    let (_, _, errors) = SHIFT_JIS.encode(name);
    if errors {
        Err(unsupported(format!(
            "material name {name:?} cannot be encoded as Shift-JIS"
        )))
    } else {
        Ok(())
    }
}

fn canonicalize_name_table(table: &mut J3dNameTable) -> Result<usize> {
    let mut cursor = checked_add(4, table.entries.len() * 4, "MAT3 name entries")?;
    for entry in &mut table.entries {
        let (encoded, _, errors) = SHIFT_JIS.encode(&entry.name);
        if errors {
            return Err(unsupported("invalid Shift-JIS material name"));
        }
        entry.hash = encoded.iter().fold(0u16, |hash, byte| {
            hash.wrapping_mul(3).wrapping_add(*byte as u16)
        });
        entry.string_offset = u16::try_from(cursor)
            .map_err(|_| limit("MAT3 name table bytes", cursor, u16::MAX as usize))?;
        cursor = checked_add(cursor, encoded.len() + 1, "MAT3 name strings")?;
    }
    Ok(cursor)
}

fn align(value: usize, alignment: usize) -> Result<usize> {
    value
        .checked_add(alignment - 1)
        .map(|value| value & !(alignment - 1))
        .ok_or_else(|| unsupported("MAT3 alignment overflow"))
}

fn checked_add(left: usize, right: usize, resource: &'static str) -> Result<usize> {
    left.checked_add(right)
        .ok_or_else(|| limit(resource, usize::MAX, u32::MAX as usize))
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
    use crate::{J3dFile, J3dRebuildDocument};

    fn exhaustive_material() -> GxMaterial {
        let tex_matrix = GxTexMatrix::default();
        GxMaterial {
            name: "完全材質".to_string(),
            material_mode: 4,
            cull_mode: 1,
            color_channel_count: 4,
            tex_gen_count: 8,
            tev_stage_count: 16,
            material_colors: [Some([1, 2, 3, 4]), Some([5, 6, 7, 8])],
            color_channels: [Some(GxColorChannel::default()); 4],
            ambient_colors: [Some([9, 10, 11, 12]), Some([13, 14, 15, 16])],
            lights: [Some(GxLight {
                color: [17, 18, 19, 20],
                position: [1.0, 2.0, 3.0],
                direction: [4.0, 5.0, 6.0],
                distance_attenuation: [7.0, 8.0, 9.0],
                angle_attenuation: [10.0, 11.0, 12.0],
            }); 8],
            tex_gens: [Some(GxTexCoordGen {
                function: 1,
                source: 4,
                matrix: 60,
            }); 8],
            post_tex_gens: [Some(GxTexCoordGen {
                function: 2,
                source: 5,
                matrix: 64,
            }); 8],
            tex_matrices: [Some(tex_matrix); 10],
            post_tex_matrices: [Some(tex_matrix); 20],
            texture_numbers: std::array::from_fn(|index| Some(index as u16)),
            tev_konst_colors: [Some([21, 22, 23, 24]); 4],
            tev_konst_color_selectors: std::array::from_fn(|index| index as u8),
            tev_konst_alpha_selectors: std::array::from_fn(|index| 31 - index as u8),
            tev_orders: [Some(GxTevOrder {
                tex_coord: Some(0),
                tex_map: Some(0),
                color_channel: 0,
            }); 16],
            tev_colors: [Some([-1, 2, -3, 4]); 4],
            tev_stages: [Some(GxTevStage::default()); 16],
            tev_swap_modes: [Some(GxTevSwapMode {
                raster_swap_table: 1,
                texture_swap_table: 2,
            }); 16],
            tev_swap_tables: [Some(GxTevSwapTable::default()); 4],
            indirect: GxIndirectMaterial {
                enabled: true,
                stage_count: 4,
                orders: [Some(GxIndirectOrder {
                    tex_coord: Some(0),
                    tex_map: Some(0),
                }); 4],
                matrices: [Some(GxIndirectMatrix {
                    rows: [[1.0, 2.0, 3.0], [4.0, 5.0, 6.0]],
                    scale_exponent: -2,
                }); 3],
                scales: [Some(GxIndirectScale {
                    scale_s: 1,
                    scale_t: 2,
                }); 4],
                tev_stages: [GxIndirectTevStage {
                    stage: 1,
                    format: 2,
                    bias: 3,
                    matrix: 4,
                    wrap_s: 5,
                    wrap_t: 6,
                    add_previous: 1,
                    use_original_lod: 1,
                    alpha: 2,
                }; 16],
            },
            fog: Some(GxFog {
                function: 2,
                range_adjustment_enabled: 1,
                center: 320,
                start_z: 1.0,
                end_z: 1000.0,
                near_z: 0.1,
                far_z: 10_000.0,
                color: [25, 26, 27, 28],
                range_adjustment_table: std::array::from_fn(|index| index as u16),
            }),
            alpha_compare: GxAlphaCompare {
                comparison_0: 1,
                reference_0: 2,
                operation: 3,
                comparison_1: 4,
                reference_1: 5,
            },
            blend_mode: GxBlendMode {
                mode: 1,
                source_factor: 4,
                destination_factor: 5,
                logic_operation: 6,
            },
            depth_mode: GxDepthMode {
                comparison_enabled: 1,
                function: 6,
                update_enabled: 0,
            },
            z_compare_location: 0,
            dither: 1,
            nbt_scale: GxNbtScale {
                enabled: 1,
                scale: [2.0, 3.0, 4.0],
            },
        }
    }

    #[test]
    fn exhaustive_mat3_compiles_reopens_and_is_byte_stable() {
        let section = compile_material_section(&[exhaustive_material()]).unwrap();
        let document = J3dRebuildDocument {
            file_type: *b"bmd3",
            version_tag: *b"SVR3",
            reserved_words: [u32::MAX; 3],
            declared_section_count: 1,
            sections: vec![section],
        };
        let first = document.to_bytes().unwrap();
        let reopened = J3dRebuildDocument::parse(&first).unwrap();
        let second = reopened.to_bytes().unwrap();
        assert_eq!(first, second);
        let J3dRebuildSectionData::Materials(reopened_mat3) = &reopened.sections[0].data else {
            panic!("MAT3 section")
        };
        assert_eq!(reopened_mat3.material_init_records.len(), 1);
        for kind in MAT3_KINDS.into_iter().filter(|kind| {
            !matches!(
                kind,
                J3dMaterialTableKind::MaterialInit | J3dMaterialTableKind::Names
            )
        }) {
            assert!(
                reopened_mat3.tables.iter().any(|table| table.kind == kind),
                "missing {kind:?} bank"
            );
        }
        let renderer = J3dFile::parse(first).unwrap();
        let material = renderer
            .material_programs_with_loader_flags(u32::MAX)
            .unwrap()
            .remove(0);
        assert_eq!(material.name, "完全材質");
        assert_eq!(material.tev_stages.len(), 16);
        assert!(material.indirect.enabled);
        assert!(material.fog.is_some());
    }

    #[test]
    fn identical_values_are_deduplicated() {
        let second = GxMaterial {
            name: "second".to_string(),
            ..GxMaterial::default()
        };
        let section = compile_material_section(&[GxMaterial::default(), second]).unwrap();
        let J3dRebuildSectionData::Materials(mat3) = section.data else {
            panic!("MAT3 section")
        };
        assert_eq!(mat3.material_init_records[0], mat3.material_init_records[1]);
        let cull = mat3
            .tables
            .iter()
            .find(|table| table.kind == J3dMaterialTableKind::CullMode)
            .unwrap();
        assert_eq!(cull.allocation, J3dScalarArray::Unsigned32(vec![2]));
    }

    #[test]
    fn loader_diagnostics_report_runtime_ignored_indirect_state() {
        let mut material = GxMaterial::default();
        material.indirect.enabled = true;
        material.indirect.stage_count = 1;
        let diagnostics =
            validate_materials_for_loader(&[material], TargetLoaderProfile::SunshineMap);
        assert!(diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == "loader-ignores-indirect"));
    }

    #[test]
    fn light_off_loader_preserves_diffuse_and_specular_channel_controls() {
        let mut material = GxMaterial {
            color_channel_count: 2,
            ..Default::default()
        };
        material.color_channels[0] = Some(GxColorChannel {
            enable: 1,
            light_mask: 1,
            diffuse_function: 2,
            attenuation_function: 2,
            ..Default::default()
        });
        material.color_channels[2] = Some(GxColorChannel {
            enable: 1,
            light_mask: 4,
            diffuse_function: 1,
            attenuation_function: 0,
            ..Default::default()
        });

        let diagnostics =
            validate_materials_for_loader(&[material], TargetLoaderProfile::Custom(0x0024_0000));
        assert!(diagnostics.is_empty(), "{diagnostics:#?}");
    }

    #[test]
    fn light_off_loader_reports_non_default_material_ambient_colors() {
        let mut material = GxMaterial::default();
        material.ambient_colors[1] = Some([1, 2, 3, 4]);

        let diagnostics =
            validate_materials_for_loader(&[material], TargetLoaderProfile::Custom(0x0024_0000));
        let diagnostic = diagnostics
            .iter()
            .find(|diagnostic| diagnostic.code == "loader-ignores-material-ambient-colors")
            .expect("non-default MAT3 ambient color warning");
        assert!(diagnostic.message.contains("slot(s) [1]"));
        assert!(diagnostic.message.contains("J3DColorBlockLightOff"));
    }

    #[test]
    fn light_off_loader_reports_embedded_lights() {
        let mut material = GxMaterial::default();
        material.lights[4] = Some(GxLight {
            color: [255; 4],
            position: [1.0, 2.0, 3.0],
            direction: [0.0, -1.0, 0.0],
            distance_attenuation: [1.0, 0.0, 0.0],
            angle_attenuation: [1.0, 0.0, 0.0],
        });

        let diagnostics =
            validate_materials_for_loader(&[material], TargetLoaderProfile::Custom(0x0024_0000));
        let diagnostic = diagnostics
            .iter()
            .find(|diagnostic| diagnostic.code == "loader-ignores-embedded-lights")
            .expect("embedded MAT3 light warning");
        assert!(diagnostic.message.contains("slot(s) [4]"));
        assert!(diagnostic
            .message
            .contains("channel controls remain active"));
    }
}
