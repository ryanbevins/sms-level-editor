use serde::{Deserialize, Serialize};
use std::sync::Arc;

use crate::binary::{
    be_f32, be_i16, be_u16, be_u32, checked_slice, read_jut_name_table, require_len, require_magic,
};
use crate::j3d_anim::J3dJointAnimation;
use crate::{FormatError, PreserveBytes, Result};

mod arc_bytes {
    use serde::{Deserialize, Deserializer, Serialize, Serializer};
    use std::sync::Arc;

    pub fn serialize<S>(bytes: &Arc<[u8]>, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        bytes.as_ref().serialize(serializer)
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<Arc<[u8]>, D::Error>
    where
        D: Deserializer<'de>,
    {
        Vec::<u8>::deserialize(deserializer).map(Arc::from)
    }
}

mod texture;
use texture::*;
const FORMAT: &str = "J3D";
const MAX_TEX1_TEXTURE_COUNT: usize = 4096;
const MAX_TEX1_RETAINED_BYTES: usize = 512 * 1024 * 1024;
pub const SMS_MAP_MODEL_LOAD_FLAGS: u32 = 0x1002_0000;
pub const SMS_POLLUTION_MODEL_LOAD_FLAGS: u32 = 0x1122_0000;
pub const SMS_DEFAULT_OBJECT_MODEL_LOAD_FLAGS: u32 = 0x1022_0000;
/// Loader flags hardcoded by Sunshine's `SmJ3DAct` constructor.
///
/// Source: `JSystem/JDrama/JDRSmJ3DAct.cpp` in the decompilation.
pub const SMS_SM_J3D_ACT_MODEL_LOAD_FLAGS: u32 = 0x0024_0000;
const GX_VA_PNMTXIDX: u32 = 0;
const GX_VA_POS: u32 = 9;
const GX_VA_NRM: u32 = 10;
const GX_VA_CLR0: u32 = 11;
const GX_VA_CLR1: u32 = 12;
const GX_VA_TEX0: u32 = 13;
const GX_VA_TEX7: u32 = GX_VA_TEX0 + 7;
const GX_VA_NULL: u32 = 0xFF;
const GX_DIRECT: u32 = 1;
const GX_INDEX8: u32 = 2;
const GX_INDEX16: u32 = 3;
const GX_POS_XYZ: u32 = 1;
const GX_NRM_XYZ: u32 = 0;
const GX_TEX_ST: u32 = 1;
const GX_TG_POS: u8 = 0;
const GX_TG_NRM: u8 = 1;
const GX_TG_TEX0: u8 = 4;
const GX_TG_TEX7: u8 = GX_TG_TEX0 + 7;
const GX_TEXMTX0: u8 = 30;
const GX_IDENTITY: u8 = 60;
const GX_U8: u32 = 0;
const GX_S8: u32 = 1;
const GX_U16: u32 = 2;
const GX_S16: u32 = 3;
const GX_F32: u32 = 4;
const GX_RGB565: u32 = 0;
const GX_RGB8: u32 = 1;
const GX_RGBX8: u32 = 2;
const GX_RGBA4: u32 = 3;
const GX_RGBA6: u32 = 4;
const GX_RGBA8: u32 = 5;
const GX_DRAW_QUADS: u8 = 0x80;
const GX_DRAW_TRIANGLES: u8 = 0x90;
const GX_DRAW_TRIANGLE_STRIP: u8 = 0x98;
const GX_DRAW_TRIANGLE_FAN: u8 = 0xA0;
const GX_OPCODE_MASK: u8 = 0xF8;
const GX_VAT_MASK: u8 = 0x07;
const GX_TF_I4: u8 = 0x0;
const GX_TF_I8: u8 = 0x1;
const GX_TF_IA4: u8 = 0x2;
const GX_TF_IA8: u8 = 0x3;
const GX_TF_RGB565: u8 = 0x4;
const GX_TF_RGB5A3: u8 = 0x5;
const GX_TF_RGBA8: u8 = 0x6;
const GX_TF_C4: u8 = 0x8;
const GX_TF_C8: u8 = 0x9;
const GX_TF_C14X2: u8 = 0xA;
const GX_TF_CMPR: u8 = 0xE;
const J3D_HIERARCHY_END: u16 = 0x00;
const J3D_HIERARCHY_BEGIN_CHILD: u16 = 0x01;
const J3D_HIERARCHY_END_CHILD: u16 = 0x02;
const J3D_HIERARCHY_JOINT: u16 = 0x10;
const J3D_HIERARCHY_MATERIAL: u16 = 0x11;
const J3D_HIERARCHY_SHAPE: u16 = 0x12;
const J3D_JOINT_INIT_DATA_SIZE: usize = 0x40;
const J3D_MATERIAL_INIT_DATA_SIZE: usize = 0x14C;
const J3D_TEX_COORD_INFO_SIZE: usize = 4;
const J3D_TEX_MTX_INFO_SIZE: usize = 0x64;
const J3D_TEV_ORDER_INFO_SIZE: usize = 4;
const J3D_TEV_STAGE_INFO_SIZE: usize = 0x14;
const J3D_IND_INIT_DATA_SIZE: usize = 0x138;
const TEX_COORD_COUNT: usize = 8;
const GX_CC_CPREV: u8 = 0;
const GX_CC_APREV: u8 = 1;
const GX_CC_C0: u8 = 2;
const GX_CC_A0: u8 = 3;
const GX_CC_C1: u8 = 4;
const GX_CC_A1: u8 = 5;
const GX_CC_C2: u8 = 6;
const GX_CC_A2: u8 = 7;
const GX_CC_TEXC: u8 = 8;
const GX_CC_TEXA: u8 = 9;
const GX_CC_RASC: u8 = 10;
const GX_CC_RASA: u8 = 11;
const GX_CC_ONE: u8 = 12;
const GX_CC_HALF: u8 = 13;
const GX_CC_KONST: u8 = 14;
const GX_CC_ZERO: u8 = 15;
const GX_CC_TEXRRR: u8 = 16;
const GX_CC_TEXBBB: u8 = 18;
const GX_CA_APREV: u8 = 0;
const GX_CA_A0: u8 = 1;
const GX_CA_A1: u8 = 2;
const GX_CA_A2: u8 = 3;
const GX_CA_TEXA: u8 = 4;
const GX_CA_RASA: u8 = 5;
const GX_CA_KONST: u8 = 6;
const GX_CA_ZERO: u8 = 7;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct J3dHeader {
    pub file_type: String,
    pub file_size: u32,
    pub section_count: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct J3dSection {
    pub tag: String,
    pub offset: u32,
    pub size: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct J3dFile {
    header: J3dHeader,
    sections: Vec<J3dSection>,
    bytes: J3dSource,
}

#[derive(Clone, PartialEq, Eq)]
struct J3dSource {
    source: Arc<[u8]>,
    parsed_len: usize,
}

impl J3dSource {
    fn new(source: Arc<[u8]>, parsed_len: usize) -> Self {
        debug_assert!(parsed_len <= source.len());
        Self { source, parsed_len }
    }

    #[cfg(test)]
    fn full(source: Arc<[u8]>) -> Self {
        let parsed_len = source.len();
        Self { source, parsed_len }
    }

    fn source_bytes(&self) -> &[u8] {
        &self.source
    }
}

impl std::fmt::Debug for J3dSource {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("J3dSource")
            .field("source_len", &self.source.len())
            .field("parsed_len", &self.parsed_len)
            .finish()
    }
}

impl std::ops::Deref for J3dSource {
    type Target = [u8];

    fn deref(&self) -> &Self::Target {
        &self.source[..self.parsed_len]
    }
}

#[derive(Serialize, Deserialize)]
struct J3dFileSerde {
    header: J3dHeader,
    sections: Vec<J3dSection>,
    #[serde(with = "arc_bytes")]
    bytes: Arc<[u8]>,
}

impl Serialize for J3dFile {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        J3dFileSerde {
            header: self.header.clone(),
            sections: self.sections.clone(),
            bytes: Arc::clone(&self.bytes.source),
        }
        .serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for J3dFile {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let decoded = J3dFileSerde::deserialize(deserializer)?;
        let parsed_len = decoded.header.file_size as usize;
        if parsed_len > decoded.bytes.len() {
            return Err(serde::de::Error::custom(format!(
                "J3D declared size {parsed_len} exceeds serialized source length {}",
                decoded.bytes.len()
            )));
        }
        Ok(Self {
            header: decoded.header,
            sections: decoded.sections,
            bytes: J3dSource::new(decoded.bytes, parsed_len),
        })
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct J3dVertexPreview {
    pub positions: Vec<[f32; 3]>,
    pub bounds_min: [f32; 3],
    pub bounds_max: [f32; 3],
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct J3dGeometryPreview {
    pub positions: Vec<[f32; 3]>,
    pub triangles: Vec<J3dTriangle>,
    pub textures: Vec<J3dTexturePreview>,
    pub materials: Vec<J3dMaterial>,
    pub bounds_min: [f32; 3],
    pub bounds_max: [f32; 3],
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct J3dTriangle {
    pub vertices: [[f32; 3]; 3],
    pub normals: Option<[[f32; 3]; 3]>,
    pub color_channels: [Option<[[u8; 4]; 3]>; 2],
    pub tex_coord_sets: [Option<[[f32; 2]; 3]>; 8],
    pub material_index: Option<usize>,
    pub shape_index: usize,
    pub packet_index: usize,
    pub color: Option<[u8; 4]>,
    pub vertex_colors: Option<[[u8; 4]; 3]>,
    pub combine_mode: J3dPreviewCombineMode,
    pub tex_coords: Option<[[f32; 2]; 3]>,
    pub texture_index: Option<usize>,
    pub mask_tex_coords: Option<[[f32; 2]; 3]>,
    pub mask_texture_index: Option<usize>,
    pub cull_mode: Option<u8>,
    pub alpha_compare: Option<J3dAlphaCompare>,
    pub blend_mode: Option<J3dBlendMode>,
    pub z_mode: Option<J3dZMode>,
    pub z_comp_loc: Option<u8>,
    pub billboard: Option<J3dBillboard>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum J3dBillboardMode {
    Full,
    YAxis,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct J3dBillboard {
    pub mode: J3dBillboardMode,
    pub center: [f32; 3],
    pub axes: [[f32; 3]; 3],
    pub offsets: [[f32; 3]; 3],
    pub normals: Option<[[f32; 3]; 3]>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct J3dMaterialRenderState {
    pub cull_mode: Option<u8>,
    pub alpha_compare: Option<J3dAlphaCompare>,
    pub blend_mode: Option<J3dBlendMode>,
    pub z_mode: Option<J3dZMode>,
    pub z_comp_loc: Option<u8>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct J3dAlphaCompare {
    pub comp0: u8,
    pub ref0: u8,
    pub op: u8,
    pub comp1: u8,
    pub ref1: u8,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct J3dBlendMode {
    pub mode: u8,
    pub src_factor: u8,
    pub dst_factor: u8,
    pub logic_op: u8,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct J3dZMode {
    pub compare_enable: u8,
    pub func: u8,
    pub update_enable: u8,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct J3dColorChannel {
    pub enable: u8,
    pub mat_src: u8,
    pub light_mask: u8,
    pub diffuse_fn: u8,
    pub attenuation_fn: u8,
    pub amb_src: u8,
}

impl Default for J3dColorChannel {
    fn default() -> Self {
        Self {
            enable: 0,
            mat_src: 0,
            light_mask: 0,
            diffuse_fn: 2,
            attenuation_fn: 2,
            amb_src: 0,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct J3dTexGen {
    pub gen_type: u8,
    pub source: u8,
    pub matrix: u8,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct J3dTexMatrix {
    pub projection: u8,
    pub mode: u8,
    pub maya: bool,
    pub center: [f32; 3],
    pub scale: [f32; 2],
    pub rotation: i16,
    pub translation: [f32; 2],
    pub effect_matrix: [[f32; 4]; 4],
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct J3dTevOrder {
    pub tex_coord: Option<u8>,
    pub tex_map: Option<u8>,
    pub color_channel: u8,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct J3dIndirectTevStage {
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct J3dTevStage {
    pub order: J3dTevOrder,
    pub color_args: [u8; 4],
    pub color_op: u8,
    pub color_bias: u8,
    pub color_scale: u8,
    pub color_clamp: u8,
    pub color_register: u8,
    pub alpha_args: [u8; 4],
    pub alpha_op: u8,
    pub alpha_bias: u8,
    pub alpha_scale: u8,
    pub alpha_clamp: u8,
    pub alpha_register: u8,
    pub konst_color: u8,
    pub konst_alpha: u8,
    pub raster_swap: u8,
    pub texture_swap: u8,
    pub indirect: J3dIndirectTevStage,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct J3dIndirectOrder {
    pub tex_coord: Option<u8>,
    pub tex_map: Option<u8>,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct J3dIndirectMatrix {
    pub rows: [[f32; 3]; 2],
    pub scale_exponent: i8,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct J3dIndirectScale {
    pub scale_s: u8,
    pub scale_t: u8,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct J3dIndirectMaterial {
    pub enabled: bool,
    pub stage_count: u8,
    pub orders: [Option<J3dIndirectOrder>; 4],
    pub matrices: [Option<J3dIndirectMatrix>; 3],
    pub scales: [Option<J3dIndirectScale>; 4],
    pub tev_stages: [J3dIndirectTevStage; 16],
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct J3dFog {
    pub fog_type: u8,
    pub adjustment_enabled: u8,
    pub center: u16,
    pub start_z: f32,
    pub end_z: f32,
    pub near_z: f32,
    pub far_z: f32,
    pub color: [u8; 4],
    pub adjustment_table: [u16; 10],
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct J3dMaterial {
    pub name: String,
    pub material_index: usize,
    pub material_id: usize,
    pub loader_flags: u32,
    pub lighting_enabled: bool,
    pub mode: u8,
    pub cull_mode: u8,
    pub color_channel_count: u8,
    pub material_colors: [[u8; 4]; 2],
    pub ambient_colors: [[u8; 4]; 2],
    pub color_channels: [J3dColorChannel; 4],
    pub tex_gen_count: u8,
    pub tex_gens: [J3dTexGen; 8],
    pub tex_matrices: [Option<J3dTexMatrix>; 8],
    pub texture_indices: [Option<usize>; 8],
    pub tev_colors: [[i16; 4]; 4],
    pub tev_k_colors: [[u8; 4]; 4],
    pub tev_stages: Vec<J3dTevStage>,
    pub swap_tables: [[u8; 4]; 4],
    pub indirect: J3dIndirectMaterial,
    pub fog: Option<J3dFog>,
    pub alpha_compare: J3dAlphaCompare,
    pub blend_mode: J3dBlendMode,
    pub z_mode: J3dZMode,
    pub z_comp_loc: u8,
    pub dither: u8,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum J3dPreviewCombineMode {
    TextureOnly,
    TextureModulateMaterial,
    TextureModulateVertex,
    MaterialOnly,
    VertexOnly,
}

impl J3dPreviewCombineMode {
    fn needs_vertex_color(self) -> bool {
        matches!(self, Self::TextureModulateVertex | Self::VertexOnly)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct J3dTexturePreview {
    #[serde(default)]
    pub name: String,
    pub width: u16,
    pub height: u16,
    pub format: u8,
    pub wrap_s: u8,
    pub wrap_t: u8,
    pub min_filter: u8,
    pub mag_filter: u8,
    #[serde(default = "default_texture_mipmap_enabled")]
    pub mipmap_enabled: bool,
    #[serde(default)]
    pub do_edge_lod: bool,
    #[serde(default)]
    pub bias_clamp: bool,
    #[serde(default)]
    pub max_anisotropy: u8,
    #[serde(default)]
    pub min_lod: f32,
    #[serde(default = "default_texture_max_lod")]
    pub max_lod: f32,
    #[serde(default)]
    pub lod_bias: f32,
    pub mipmap_count: u8,
    pub rgba: Vec<u8>,
    pub mips: Vec<J3dTextureMipPreview>,
}

fn default_texture_mipmap_enabled() -> bool {
    true
}

fn default_texture_max_lod() -> f32 {
    10.0
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct J3dTextureMipPreview {
    pub width: u16,
    pub height: u16,
    pub rgba: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct J3dMaterialDiagnostic {
    pub material_index: usize,
    pub material_id: usize,
    pub name: String,
    pub mode: u8,
    pub color_channel_count: u8,
    pub color: [u8; 4],
    pub material_colors: [[u8; 4]; 2],
    pub ambient_colors: [[u8; 4]; 2],
    pub color_channels: [J3dColorChannel; 4],
    pub cull_mode: Option<u8>,
    pub alpha_compare: Option<J3dAlphaCompare>,
    pub blend_mode: Option<J3dBlendMode>,
    pub z_mode: Option<J3dZMode>,
    pub z_comp_loc: Option<u8>,
    pub tev_colors: [[i16; 4]; 4],
    pub tev_k_colors: [[u8; 4]; 4],
    pub stages: Vec<J3dTevStageDiagnostic>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct J3dTevStageDiagnostic {
    pub stage: usize,
    pub tex_coord_index: Option<usize>,
    pub tex_map_slot: Option<usize>,
    pub tex_gen_src: Option<u8>,
    pub tex_gen_mtx: Option<u8>,
    pub texture_index: Option<usize>,
    pub texture_format: Option<u8>,
    pub color_chan: u8,
    pub color_args: [u8; 4],
    pub alpha_args: [u8; 4],
    pub k_color_sel: u8,
    pub k_alpha_sel: u8,
    pub konst_color: Option<[u8; 4]>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct PositionFormat {
    component_type: u32,
    frac: u8,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct TexCoordFormat {
    component_type: u32,
    frac: u8,
    components: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ColorFormat {
    component_type: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct NormalFormat {
    component_type: u32,
    frac: u8,
    components: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct AttributeFormat {
    attr: u32,
    cnt: u32,
    component_type: u32,
    frac: u8,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct VertexDesc {
    attr: u32,
    attr_type: u32,
}

#[derive(Debug, Clone, Copy)]
struct VertexArrays {
    normal_offset: Option<usize>,
    normal_format: Option<NormalFormat>,
    color_offsets: [Option<usize>; 2],
    color_formats: [Option<ColorFormat>; 2],
    tex_offsets: [Option<usize>; TEX_COORD_COUNT],
    tex_formats: [Option<TexCoordFormat>; TEX_COORD_COUNT],
}

#[derive(Debug, Clone, Copy)]
struct MaterialPreviewBinding {
    texture_index: Option<usize>,
    tex_coord_source: TexCoordPreviewSource,
    tex_mtx: Option<TexMtx2d>,
    mask_texture_index: Option<usize>,
    mask_tex_coord_source: TexCoordPreviewSource,
    mask_tex_mtx: Option<TexMtx2d>,
    combine_mode: J3dPreviewCombineMode,
    tint_color: Option<[u8; 4]>,
}

#[derive(Debug, Clone, Copy)]
struct TexMtx2d {
    rows: [[f32; 3]; 2],
}

impl TexMtx2d {
    fn apply(self, coord: [f32; 2]) -> [f32; 2] {
        [
            self.rows[0][0] * coord[0] + self.rows[0][1] * coord[1] + self.rows[0][2],
            self.rows[1][0] * coord[0] + self.rows[1][1] * coord[1] + self.rows[1][2],
        ]
    }
}

#[derive(Debug, Clone, Copy)]
struct TexCoordPreviewBinding {
    source: TexCoordPreviewSource,
    tex_mtx: Option<TexMtx2d>,
}

#[derive(Debug, Clone, Copy)]
enum TexCoordPreviewSource {
    Vertex(usize),
    Position,
    Normal,
}

#[derive(Debug, Clone, Copy)]
struct MaterialTexCoordInfo {
    gen_src: u8,
    gen_mtx: u8,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct MaterialTevOrder {
    tex_coord_index: Option<usize>,
    tex_map_index: Option<usize>,
    color_chan: u8,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RasterColorSource {
    Material,
    Vertex,
    Disabled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct TevColorArgs {
    a: u8,
    b: u8,
    c: u8,
    d: u8,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct TevAlphaArgs {
    a: u8,
    b: u8,
    c: u8,
    d: u8,
}

pub type J3dMatrix34 = [[f32; 4]; 3];
type Mtx34 = J3dMatrix34;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct J3dJointTransformOverride {
    pub joint_index: usize,
    pub translation_offset: [f32; 3],
    pub scale_multiplier: [f32; 3],
}

#[derive(Debug, Clone, Copy)]
struct JointPreviewTransform {
    scale_compensate: bool,
    scale: [f32; 3],
    rotation: [i16; 3],
    translation: [f32; 3],
}

#[derive(Debug, Clone, Copy)]
struct PrimitiveVertex {
    position: [f32; 3],
    normal: Option<[f32; 3]>,
    colors: [Option<[u8; 4]>; 2],
    tex_coords: [Option<[f32; 2]>; TEX_COORD_COUNT],
}

#[derive(Debug, Clone)]
pub struct J3dPreparedAnimatedTriangles {
    source: PreparedModelSource,
    packets: Vec<PreparedAnimatedPacket>,
    source_triangle_count: usize,
    max_packet_vertex_count: usize,
}

impl J3dPreparedAnimatedTriangles {
    pub fn source_triangle_count(&self) -> usize {
        self.source_triangle_count
    }

    pub fn is_empty(&self) -> bool {
        self.source_triangle_count == 0
    }

    fn pose(&self, draw_matrices: &[Option<Mtx34>]) -> Vec<J3dTriangle> {
        let mut triangles = Vec::with_capacity(self.source_triangle_count);
        let mut vertices = Vec::with_capacity(self.max_packet_vertex_count);
        for packet in &self.packets {
            vertices.clear();
            vertices.extend(
                packet
                    .vertices
                    .iter()
                    .copied()
                    .map(|vertex| vertex.pose(draw_matrices)),
            );
            for &[a, b, c] in &packet.triangle_indices {
                let triangle_count = triangles.len();
                push_triangle(
                    &mut triangles,
                    vertices[a],
                    vertices[b],
                    vertices[c],
                    None,
                    J3dMaterialRenderState::default(),
                    None,
                );
                if triangles.len() == triangle_count {
                    continue;
                }

                let triangle = triangles
                    .last_mut()
                    .expect("push_triangle appended one triangle");
                triangle.shape_index = packet.shape_index;
                triangle.packet_index = packet.packet_index;
                if let Some(billboard) = packet.billboard {
                    let draw_matrix = draw_matrices
                        .get(billboard.draw_matrix_index as usize)
                        .copied()
                        .flatten();
                    if let Some(draw_matrix) = draw_matrix {
                        triangle.billboard = billboard_for_triangle(
                            triangle.vertices,
                            triangle.normals,
                            draw_matrix,
                            billboard.shape_matrix_type,
                        );
                    }
                }
            }
        }
        triangles
    }
}

#[derive(Clone)]
struct PreparedModelSource(J3dSource);

impl std::fmt::Debug for PreparedModelSource {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PreparedModelSource")
            .field("bytes_len", &self.0.len())
            .finish()
    }
}

#[derive(Debug, Clone)]
struct PreparedAnimatedPacket {
    vertices: Vec<PreparedPrimitiveVertex>,
    triangle_indices: Vec<[usize; 3]>,
    shape_index: usize,
    packet_index: usize,
    billboard: Option<PreparedBillboard>,
}

#[derive(Debug, Clone)]
struct PreparedDisplayList {
    vertices: Vec<PreparedPrimitiveVertex>,
    triangle_indices: Vec<[usize; 3]>,
}

#[derive(Debug, Clone, Copy)]
struct PreparedBillboard {
    draw_matrix_index: u16,
    shape_matrix_type: u8,
}

#[derive(Debug, Clone, Copy)]
struct PreparedPrimitiveVertex {
    vertex: PrimitiveVertex,
    draw_matrix_index: Option<u16>,
}

impl PreparedPrimitiveVertex {
    fn pose(self, draw_matrices: &[Option<Mtx34>]) -> PrimitiveVertex {
        PrimitiveVertex {
            position: transform_position_for_draw_matrix(
                self.vertex.position,
                self.draw_matrix_index,
                draw_matrices,
            ),
            normal: self.vertex.normal.map(|normal| {
                transform_normal_for_draw_matrix(normal, self.draw_matrix_index, draw_matrices)
            }),
            colors: self.vertex.colors,
            tex_coords: self.vertex.tex_coords,
        }
    }
}

impl J3dFile {
    pub fn parse(bytes: impl AsRef<[u8]>) -> Result<Self> {
        let bytes = bytes.as_ref();
        require_len(FORMAT, bytes, 0x20)?;
        require_magic(FORMAT, bytes, b"J3D2")?;

        let file_type = String::from_utf8_lossy(checked_slice(FORMAT, bytes, 0x04, 4)?)
            .trim_end_matches('\0')
            .to_string();
        let file_size = be_u32(bytes, 0x08, FORMAT)?;
        let section_count = be_u32(bytes, 0x0C, FORMAT)?;
        let declared_size = file_size as usize;
        if declared_size < 0x20 || declared_size > bytes.len() {
            return Err(FormatError::InvalidOffset {
                format: FORMAT,
                offset: declared_size,
                len: bytes.len(),
            });
        }
        let file = &bytes[..declared_size];

        let mut sections = Vec::<J3dSection>::new();
        let mut offset = 0x20usize;
        for section_index in 0..section_count {
            if offset.checked_add(8).is_none_or(|end| end > file.len()) {
                // Retail texture-only BMTs such as Mamma's sandbombbase.bmt
                // declare a second block even though their sole TEX1 block
                // ends exactly at the declared file size. J3D's material-table
                // loader still uses that TEX1 table. Keep this exception as
                // narrow as possible so truncated BMD/BDL files remain errors.
                let retail_texture_only_bmt = file_type.starts_with("bmt")
                    && section_index + 1 == section_count
                    && offset == file.len()
                    && sections.len() == 1
                    && sections[0].tag == "TEX1";
                if retail_texture_only_bmt {
                    break;
                }
                return Err(FormatError::InvalidOffset {
                    format: FORMAT,
                    offset,
                    len: file.len(),
                });
            }

            let tag = String::from_utf8_lossy(checked_slice(FORMAT, file, offset, 4)?)
                .trim_end_matches('\0')
                .to_string();
            let size = be_u32(file, offset + 4, FORMAT)?;
            if size < 8 {
                return Err(FormatError::Unsupported {
                    format: FORMAT,
                    message: format!("section {tag} has invalid size {size}"),
                });
            }

            checked_slice(FORMAT, file, offset, size as usize)?;
            sections.push(J3dSection {
                tag,
                offset: offset as u32,
                size,
            });
            offset = offset
                .checked_add(size as usize)
                .ok_or_else(|| invalid_offset(offset, file.len()))?;
        }

        Ok(Self {
            header: J3dHeader {
                file_type,
                file_size,
                section_count,
            },
            sections,
            bytes: J3dSource::new(Arc::from(bytes), declared_size),
        })
    }

    pub fn header(&self) -> &J3dHeader {
        &self.header
    }

    pub fn sections(&self) -> &[J3dSection] {
        &self.sections
    }

    pub fn vertex_preview(&self) -> Result<J3dVertexPreview> {
        let vertex_count = self.vertex_count()?;
        let vtx1 = self
            .section("VTX1")
            .ok_or_else(|| FormatError::Unsupported {
                format: FORMAT,
                message: "missing VTX1 section".to_string(),
            })?;
        let section_offset = vtx1.offset as usize;
        let attr_list_offset = section_offset
            .checked_add(be_u32(&self.bytes, section_offset + 0x08, FORMAT)? as usize)
            .ok_or_else(|| invalid_offset(section_offset, self.bytes.len()))?;
        let pos_array_offset = section_offset
            .checked_add(be_u32(&self.bytes, section_offset + 0x0C, FORMAT)? as usize)
            .ok_or_else(|| invalid_offset(section_offset, self.bytes.len()))?;

        let attr_formats = self.attribute_formats(attr_list_offset)?;
        let format = position_format_from(&attr_formats)?;
        let positions = self.read_positions(pos_array_offset, vertex_count, format)?;
        if positions.is_empty() {
            return Err(FormatError::Unsupported {
                format: FORMAT,
                message: "model has no previewable positions".to_string(),
            });
        }

        let (bounds_min, bounds_max) = bounds_for(&positions);
        Ok(J3dVertexPreview {
            positions,
            bounds_min,
            bounds_max,
        })
    }

    pub fn geometry_preview(&self) -> Result<J3dGeometryPreview> {
        self.geometry_preview_with_loader_flags(SMS_MAP_MODEL_LOAD_FLAGS)
    }

    pub fn geometry_preview_with_loader_flags(
        &self,
        loader_flags: u32,
    ) -> Result<J3dGeometryPreview> {
        self.geometry_preview_with_pose(loader_flags, None, &[])
    }

    pub fn geometry_preview_with_joint_animation(
        &self,
        loader_flags: u32,
        animation: &J3dJointAnimation,
        elapsed_seconds: f32,
    ) -> Result<J3dGeometryPreview> {
        let frame = animation.playback_frame(elapsed_seconds);
        self.geometry_preview_with_pose(loader_flags, Some((animation, frame)), &[])
    }

    pub fn geometry_preview_with_joint_overrides(
        &self,
        loader_flags: u32,
        overrides: &[J3dJointTransformOverride],
    ) -> Result<J3dGeometryPreview> {
        self.geometry_preview_with_pose(loader_flags, None, overrides)
    }

    pub fn animated_triangles_with_joint_animation(
        &self,
        loader_flags: u32,
        animation: &J3dJointAnimation,
        elapsed_seconds: f32,
    ) -> Result<Vec<J3dTriangle>> {
        self.animated_triangles_and_joint_matrices_with_joint_animation(
            loader_flags,
            animation,
            elapsed_seconds,
        )
        .map(|(triangles, _)| triangles)
    }

    /// Samples one animation frame and returns both its posed triangles and the
    /// joint matrices used to build the corresponding draw matrices.
    ///
    /// Callers that also pose joint-attached geometry can use this method to
    /// avoid evaluating the same joint animation a second time.
    pub fn animated_triangles_and_joint_matrices_with_joint_animation(
        &self,
        loader_flags: u32,
        animation: &J3dJointAnimation,
        elapsed_seconds: f32,
    ) -> Result<(Vec<J3dTriangle>, Vec<J3dMatrix34>)> {
        let frame = animation.playback_frame(elapsed_seconds);
        let joint_matrices =
            self.preview_joint_matrices(loader_flags, Some((animation, frame)), &[])?;
        let draw_matrices = self.preview_draw_matrices_from_joint_matrices(&joint_matrices)?;
        let triangles = self.triangles_with_draw_matrices(&draw_matrices)?;
        Ok((triangles, joint_matrices))
    }

    /// Decodes the immutable vertex attributes and primitive topology needed
    /// for repeated joint-animation samples.
    ///
    /// The returned data is tied to this parsed model instance and can be
    /// reused for every animation frame without reparsing SHP1 display lists.
    pub fn prepare_animated_triangles(&self) -> Result<J3dPreparedAnimatedTriangles> {
        let vertex_preview = self.vertex_preview()?;
        let vtx1 = self
            .section("VTX1")
            .ok_or_else(|| FormatError::Unsupported {
                format: FORMAT,
                message: "missing VTX1 section".to_string(),
            })?;
        let vtx_offset = vtx1.offset as usize;
        let attr_formats = self.attribute_formats(
            vtx_offset + be_u32(&self.bytes, vtx_offset + 0x08, FORMAT)? as usize,
        )?;
        let position_format = position_format_from(&attr_formats)?;
        let vertex_arrays = vertex_arrays(&self.bytes, vtx_offset, &attr_formats);
        let shp1 = self
            .section("SHP1")
            .ok_or_else(|| FormatError::Unsupported {
                format: FORMAT,
                message: "missing SHP1 section".to_string(),
            })?;
        let packets = self.read_prepared_shape_packets(
            shp1.offset as usize,
            &vertex_preview.positions,
            &attr_formats,
            position_format,
            vertex_arrays,
        )?;
        let source_triangle_count = packets
            .iter()
            .map(|packet| packet.triangle_indices.len())
            .sum();
        let max_packet_vertex_count = packets
            .iter()
            .map(|packet| packet.vertices.len())
            .max()
            .unwrap_or(0);
        Ok(J3dPreparedAnimatedTriangles {
            source: PreparedModelSource(self.bytes.clone()),
            packets,
            source_triangle_count,
            max_packet_vertex_count,
        })
    }

    /// Samples one animation frame using topology prepared by
    /// [`Self::prepare_animated_triangles`].
    pub fn animate_prepared_triangles_with_joint_animation(
        &self,
        prepared: &J3dPreparedAnimatedTriangles,
        loader_flags: u32,
        animation: &J3dJointAnimation,
        elapsed_seconds: f32,
    ) -> Result<(Vec<J3dTriangle>, Vec<J3dMatrix34>)> {
        if !self.prepared_animation_source_matches(prepared) {
            return Err(FormatError::Unsupported {
                format: FORMAT,
                message: "prepared animated triangles belong to a different parsed model"
                    .to_string(),
            });
        }
        let frame = animation.playback_frame(elapsed_seconds);
        let joint_matrices =
            self.preview_joint_matrices(loader_flags, Some((animation, frame)), &[])?;
        let draw_matrices = self.preview_draw_matrices_from_joint_matrices(&joint_matrices)?;
        Ok((prepared.pose(&draw_matrices), joint_matrices))
    }

    fn prepared_animation_source_matches(&self, prepared: &J3dPreparedAnimatedTriangles) -> bool {
        prepared.source.0.parsed_len == self.bytes.parsed_len
            && Arc::ptr_eq(&prepared.source.0.source, &self.bytes.source)
    }

    fn triangles_with_draw_matrices(
        &self,
        draw_matrices: &[Option<Mtx34>],
    ) -> Result<Vec<J3dTriangle>> {
        let vertex_preview = self.vertex_preview()?;
        let vtx1 = self
            .section("VTX1")
            .ok_or_else(|| FormatError::Unsupported {
                format: FORMAT,
                message: "missing VTX1 section".to_string(),
            })?;
        let vtx_offset = vtx1.offset as usize;
        let attr_formats = self.attribute_formats(
            vtx_offset + be_u32(&self.bytes, vtx_offset + 0x08, FORMAT)? as usize,
        )?;
        let position_format = position_format_from(&attr_formats)?;
        let vertex_arrays = vertex_arrays(&self.bytes, vtx_offset, &attr_formats);
        let shp1 = self
            .section("SHP1")
            .ok_or_else(|| FormatError::Unsupported {
                format: FORMAT,
                message: "missing SHP1 section".to_string(),
            })?;
        self.read_shape_triangles(
            shp1.offset as usize,
            &vertex_preview.positions,
            &attr_formats,
            position_format,
            vertex_arrays,
            &[],
            &[],
            &[],
            &[],
            draw_matrices,
        )
    }

    pub fn triangles_with_joint_overrides(
        &self,
        loader_flags: u32,
        overrides: &[J3dJointTransformOverride],
    ) -> Result<Vec<J3dTriangle>> {
        let draw_matrices = self.preview_draw_matrices(loader_flags, None, overrides)?;
        self.triangles_with_draw_matrices(&draw_matrices)
    }

    pub fn joint_names(&self) -> Result<Vec<String>> {
        let Some(jnt1) = self.section("JNT1") else {
            return Ok(Vec::new());
        };
        let base = jnt1.offset as usize;
        let name_table_offset = relative_offset(&self.bytes, base, 0x14)?;
        let section_end = base
            .checked_add(jnt1.size as usize)
            .ok_or_else(|| invalid_offset(base, self.bytes.len()))?
            .min(self.bytes.len());
        read_jut_name_table(&self.bytes, name_table_offset, section_end)
    }

    pub fn joint_matrices_with_joint_animation(
        &self,
        loader_flags: u32,
        animation: &J3dJointAnimation,
        elapsed_seconds: f32,
    ) -> Result<Vec<J3dMatrix34>> {
        let frame = animation.playback_frame(elapsed_seconds);
        self.preview_joint_matrices(loader_flags, Some((animation, frame)), &[])
    }

    pub fn joint_matrices(&self, loader_flags: u32) -> Result<Vec<J3dMatrix34>> {
        self.preview_joint_matrices(loader_flags, None, &[])
    }

    pub fn map_building_joint_index(&self, building_index: usize) -> Result<Option<usize>> {
        let Some(jnt1) = self.section("JNT1") else {
            return Ok(None);
        };
        let joint_count = be_u16(&self.bytes, jnt1.offset as usize + 0x08, FORMAT)? as usize;
        let parents = self.joint_parent_indices(joint_count)?;
        Ok(map_building_joint_from_parents(&parents, building_index))
    }

    pub fn runtime_joint_child_index(
        &self,
        parent_joint_index: usize,
        child_index: usize,
    ) -> Result<Option<usize>> {
        let Some(jnt1) = self.section("JNT1") else {
            return Ok(None);
        };
        let joint_count = be_u16(&self.bytes, jnt1.offset as usize + 0x08, FORMAT)? as usize;
        let parents = self.joint_parent_indices(joint_count)?;
        Ok(runtime_child_joint_index(
            &parents,
            parent_joint_index,
            child_index,
        ))
    }

    pub fn joint_subtree_shape_indices(&self, joint_index: usize) -> Result<Vec<usize>> {
        let Some(jnt1) = self.section("JNT1") else {
            return Ok(Vec::new());
        };
        let joint_count = be_u16(&self.bytes, jnt1.offset as usize + 0x08, FORMAT)? as usize;
        let parents = self.joint_parent_indices(joint_count)?;
        let owners = self.shape_joint_indices()?;
        Ok(owners
            .into_iter()
            .enumerate()
            .filter_map(|(shape, owner)| {
                owner
                    .is_some_and(|owner| joint_is_in_subtree(owner, joint_index, &parents))
                    .then_some(shape)
            })
            .collect())
    }

    pub fn joint_bounds(&self, joint_index: usize) -> Result<([f32; 3], [f32; 3])> {
        let jnt1 = self
            .section("JNT1")
            .ok_or_else(|| FormatError::Unsupported {
                format: FORMAT,
                message: "missing JNT1 section".to_string(),
            })?;
        let base = jnt1.offset as usize;
        let joint_count = be_u16(&self.bytes, base + 0x08, FORMAT)? as usize;
        if joint_index >= joint_count {
            return Err(invalid_offset(joint_index, joint_count));
        }
        let init_offset = relative_offset(&self.bytes, base, 0x0C)?;
        let index_table_offset = relative_offset(&self.bytes, base, 0x10)?;
        let init_index =
            be_u16(&self.bytes, index_table_offset + joint_index * 2, FORMAT)? as usize;
        let offset = init_offset + init_index * J3D_JOINT_INIT_DATA_SIZE;
        let min = [
            be_f32(&self.bytes, offset + 0x28, FORMAT)?,
            be_f32(&self.bytes, offset + 0x2C, FORMAT)?,
            be_f32(&self.bytes, offset + 0x30, FORMAT)?,
        ];
        let max = [
            be_f32(&self.bytes, offset + 0x34, FORMAT)?,
            be_f32(&self.bytes, offset + 0x38, FORMAT)?,
            be_f32(&self.bytes, offset + 0x3C, FORMAT)?,
        ];
        Ok((min, max))
    }

    fn geometry_preview_with_pose(
        &self,
        loader_flags: u32,
        animation: Option<(&J3dJointAnimation, f32)>,
        overrides: &[J3dJointTransformOverride],
    ) -> Result<J3dGeometryPreview> {
        let vertex_preview = self.vertex_preview()?;
        let vtx1 = self
            .section("VTX1")
            .ok_or_else(|| FormatError::Unsupported {
                format: FORMAT,
                message: "missing VTX1 section".to_string(),
            })?;
        let vtx_offset = vtx1.offset as usize;
        let attr_formats = self.attribute_formats(
            vtx_offset + be_u32(&self.bytes, vtx_offset + 0x08, FORMAT)? as usize,
        )?;
        let position_format = position_format_from(&attr_formats)?;
        let vertex_arrays = vertex_arrays(&self.bytes, vtx_offset, &attr_formats);

        let shp1 = self
            .section("SHP1")
            .ok_or_else(|| FormatError::Unsupported {
                format: FORMAT,
                message: "missing SHP1 section".to_string(),
            })?;
        let shape_materials = self.shape_material_indices()?;
        let textures = self.texture_previews()?;
        let material_colors = self.material_preview_colors()?;
        let materials = self.material_programs_with_loader_flags(loader_flags)?;
        let material_render_states = materials
            .iter()
            .map(|material| J3dMaterialRenderState {
                cull_mode: Some(material.cull_mode),
                alpha_compare: Some(material.alpha_compare),
                blend_mode: Some(material.blend_mode),
                z_mode: Some(material.z_mode),
                z_comp_loc: Some(material.z_comp_loc),
            })
            .collect::<Vec<_>>();
        let material_textures = self.material_texture_bindings(&textures, &material_colors)?;
        let draw_matrices = self.preview_draw_matrices(loader_flags, animation, overrides)?;
        let triangles = self.read_shape_triangles(
            shp1.offset as usize,
            &vertex_preview.positions,
            &attr_formats,
            position_format,
            vertex_arrays,
            &shape_materials,
            &material_colors,
            &material_render_states,
            &material_textures,
            &draw_matrices,
        )?;
        if triangles.is_empty() {
            return Err(FormatError::Unsupported {
                format: FORMAT,
                message: "SHP1 produced no preview triangles".to_string(),
            });
        }
        let mut positions = Vec::with_capacity(triangles.len() * 3);
        for triangle in &triangles {
            positions.extend_from_slice(&triangle.vertices);
        }
        let (bounds_min, bounds_max) = bounds_for(&positions);

        Ok(J3dGeometryPreview {
            positions,
            triangles,
            textures,
            materials,
            bounds_min,
            bounds_max,
        })
    }

    fn section(&self, tag: &str) -> Option<&J3dSection> {
        self.sections.iter().find(|section| section.tag == tag)
    }

    fn vertex_count(&self) -> Result<usize> {
        let info = self
            .section("INF1")
            .ok_or_else(|| FormatError::Unsupported {
                format: FORMAT,
                message: "missing INF1 section".to_string(),
            })?;
        Ok(be_u32(&self.bytes, info.offset as usize + 0x10, FORMAT)? as usize)
    }

    fn attribute_formats(&self, attr_list_offset: usize) -> Result<Vec<AttributeFormat>> {
        let mut formats = Vec::new();
        for index in 0..64 {
            let offset = attr_list_offset
                .checked_add(index * 0x10)
                .ok_or_else(|| invalid_offset(attr_list_offset, self.bytes.len()))?;
            let attr = be_u32(&self.bytes, offset, FORMAT)?;
            if attr == GX_VA_NULL {
                break;
            }

            formats.push(AttributeFormat {
                attr,
                cnt: be_u32(&self.bytes, offset + 0x04, FORMAT)?,
                component_type: be_u32(&self.bytes, offset + 0x08, FORMAT)?,
                frac: *checked_slice(FORMAT, &self.bytes, offset + 0x0C, 1)?
                    .first()
                    .unwrap_or(&0),
            });
        }

        Ok(formats)
    }

    fn read_positions(
        &self,
        offset: usize,
        vertex_count: usize,
        format: PositionFormat,
    ) -> Result<Vec<[f32; 3]>> {
        match format.component_type {
            GX_F32 => {
                checked_slice(FORMAT, &self.bytes, offset, vertex_count.saturating_mul(12))?;
                let mut positions = Vec::with_capacity(vertex_count);
                for index in 0..vertex_count {
                    let base = offset + index * 12;
                    let point = [
                        be_f32(&self.bytes, base, FORMAT)?,
                        be_f32(&self.bytes, base + 4, FORMAT)?,
                        be_f32(&self.bytes, base + 8, FORMAT)?,
                    ];
                    positions.push(point);
                }
                Ok(positions)
            }
            GX_S16 => {
                checked_slice(FORMAT, &self.bytes, offset, vertex_count.saturating_mul(6))?;
                let scale = 1.0 / (1u32 << format.frac.min(30)) as f32;
                let mut positions = Vec::with_capacity(vertex_count);
                for index in 0..vertex_count {
                    let base = offset + index * 6;
                    positions.push([
                        be_i16(&self.bytes, base, FORMAT)? as f32 * scale,
                        be_i16(&self.bytes, base + 2, FORMAT)? as f32 * scale,
                        be_i16(&self.bytes, base + 4, FORMAT)? as f32 * scale,
                    ]);
                }
                Ok(positions)
            }
            _ => Err(FormatError::Unsupported {
                format: FORMAT,
                message: format!(
                    "unsupported position component type {}",
                    format.component_type
                ),
            }),
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn read_shape_triangles(
        &self,
        shape_offset: usize,
        positions: &[[f32; 3]],
        attr_formats: &[AttributeFormat],
        position_format: PositionFormat,
        vertex_arrays: VertexArrays,
        shape_materials: &[Option<usize>],
        material_colors: &[[u8; 4]],
        material_render_states: &[J3dMaterialRenderState],
        material_textures: &[Option<MaterialPreviewBinding>],
        draw_matrices: &[Option<Mtx34>],
    ) -> Result<Vec<J3dTriangle>> {
        let shape_count = be_u16(&self.bytes, shape_offset + 0x08, FORMAT)? as usize;
        let shape_init_offset = relative_offset(&self.bytes, shape_offset, 0x0C)?;
        let index_table_offset = relative_offset(&self.bytes, shape_offset, 0x10)?;
        let vtx_desc_offset = relative_offset(&self.bytes, shape_offset, 0x18)?;
        let mtx_table_offset = relative_offset(&self.bytes, shape_offset, 0x1C).ok();
        let display_list_offset = relative_offset(&self.bytes, shape_offset, 0x20)?;
        let mtx_init_offset = relative_offset(&self.bytes, shape_offset, 0x24)?;
        let draw_init_offset = relative_offset(&self.bytes, shape_offset, 0x28)?;

        let mut triangles = Vec::new();
        let mut packet_index = 0usize;
        for shape_no in 0..shape_count {
            let index = be_u16(&self.bytes, index_table_offset + shape_no * 2, FORMAT)? as usize;
            let init_offset = shape_init_offset + index * 0x28;
            checked_slice(FORMAT, &self.bytes, init_offset, 0x28)?;
            let shape_mtx_type = checked_slice(FORMAT, &self.bytes, init_offset, 1)?[0];
            let mtx_group_count = be_u16(&self.bytes, init_offset + 0x02, FORMAT)? as usize;
            let vtx_desc_index = be_u16(&self.bytes, init_offset + 0x04, FORMAT)? as usize;
            let mtx_init_index = be_u16(&self.bytes, init_offset + 0x06, FORMAT)? as usize;
            let draw_init_index = be_u16(&self.bytes, init_offset + 0x08, FORMAT)? as usize;
            let vtx_descs = self.read_vertex_descs(vtx_desc_offset + vtx_desc_index)?;
            let material_index = shape_materials.get(shape_no).copied().flatten();
            let color = shape_materials.get(shape_no).and_then(|material| {
                material.and_then(|index| material_colors.get(index).copied())
            });
            let render_state = shape_materials
                .get(shape_no)
                .and_then(|material| {
                    material.and_then(|index| material_render_states.get(index).copied())
                })
                .unwrap_or_default();
            let texture_binding = shape_materials.get(shape_no).and_then(|material| {
                material.and_then(|index| material_textures.get(index).copied().flatten())
            });
            let mut matrix_palette = Vec::new();

            for group in 0..mtx_group_count {
                let raw_group_matrices = self.shape_group_draw_matrices(
                    shape_mtx_type,
                    mtx_init_offset + (mtx_init_index + group) * 0x08,
                    mtx_table_offset,
                )?;
                let group_matrices =
                    resolve_shape_matrix_palette(&raw_group_matrices, &mut matrix_palette);
                let draw_offset = draw_init_offset + (draw_init_index + group) * 0x08;
                checked_slice(FORMAT, &self.bytes, draw_offset, 0x08)?;
                let display_list_size = be_u32(&self.bytes, draw_offset, FORMAT)? as usize;
                let display_list_index = be_u32(&self.bytes, draw_offset + 0x04, FORMAT)? as usize;
                let display_list = checked_slice(
                    FORMAT,
                    &self.bytes,
                    display_list_offset + display_list_index,
                    display_list_size,
                )?;
                let mut shape_triangles = decode_display_list(
                    display_list,
                    &self.bytes,
                    positions,
                    &vtx_descs,
                    attr_formats,
                    position_format,
                    vertex_arrays,
                    &group_matrices,
                    draw_matrices,
                    color,
                    render_state,
                    texture_binding,
                )?;
                for triangle in &mut shape_triangles {
                    triangle.material_index = material_index;
                    triangle.shape_index = shape_no;
                    triangle.packet_index = packet_index;
                }
                if matches!(shape_mtx_type, 0x01 | 0x02) {
                    let draw_matrix = group_matrices
                        .first()
                        .copied()
                        .filter(|index| *index != 0xFFFF)
                        .and_then(|index| draw_matrices.get(index as usize))
                        .copied()
                        .flatten();
                    if let Some(draw_matrix) = draw_matrix {
                        for triangle in &mut shape_triangles {
                            triangle.billboard = billboard_for_triangle(
                                triangle.vertices,
                                triangle.normals,
                                draw_matrix,
                                shape_mtx_type,
                            );
                        }
                    }
                }
                triangles.append(&mut shape_triangles);
                packet_index += 1;
            }
        }

        Ok(triangles)
    }

    #[allow(clippy::too_many_arguments)]
    fn read_prepared_shape_packets(
        &self,
        shape_offset: usize,
        positions: &[[f32; 3]],
        attr_formats: &[AttributeFormat],
        position_format: PositionFormat,
        vertex_arrays: VertexArrays,
    ) -> Result<Vec<PreparedAnimatedPacket>> {
        let shape_count = be_u16(&self.bytes, shape_offset + 0x08, FORMAT)? as usize;
        let shape_init_offset = relative_offset(&self.bytes, shape_offset, 0x0C)?;
        let index_table_offset = relative_offset(&self.bytes, shape_offset, 0x10)?;
        let vtx_desc_offset = relative_offset(&self.bytes, shape_offset, 0x18)?;
        let mtx_table_offset = relative_offset(&self.bytes, shape_offset, 0x1C).ok();
        let display_list_offset = relative_offset(&self.bytes, shape_offset, 0x20)?;
        let mtx_init_offset = relative_offset(&self.bytes, shape_offset, 0x24)?;
        let draw_init_offset = relative_offset(&self.bytes, shape_offset, 0x28)?;

        let mut packets = Vec::new();
        let mut packet_index = 0usize;
        for shape_no in 0..shape_count {
            let index = be_u16(&self.bytes, index_table_offset + shape_no * 2, FORMAT)? as usize;
            let init_offset = shape_init_offset + index * 0x28;
            checked_slice(FORMAT, &self.bytes, init_offset, 0x28)?;
            let shape_mtx_type = checked_slice(FORMAT, &self.bytes, init_offset, 1)?[0];
            let mtx_group_count = be_u16(&self.bytes, init_offset + 0x02, FORMAT)? as usize;
            let vtx_desc_index = be_u16(&self.bytes, init_offset + 0x04, FORMAT)? as usize;
            let mtx_init_index = be_u16(&self.bytes, init_offset + 0x06, FORMAT)? as usize;
            let draw_init_index = be_u16(&self.bytes, init_offset + 0x08, FORMAT)? as usize;
            let vtx_descs = self.read_vertex_descs(vtx_desc_offset + vtx_desc_index)?;
            let mut matrix_palette = Vec::new();

            for group in 0..mtx_group_count {
                let raw_group_matrices = self.shape_group_draw_matrices(
                    shape_mtx_type,
                    mtx_init_offset + (mtx_init_index + group) * 0x08,
                    mtx_table_offset,
                )?;
                let group_matrices =
                    resolve_shape_matrix_palette(&raw_group_matrices, &mut matrix_palette);
                let draw_offset = draw_init_offset + (draw_init_index + group) * 0x08;
                checked_slice(FORMAT, &self.bytes, draw_offset, 0x08)?;
                let display_list_size = be_u32(&self.bytes, draw_offset, FORMAT)? as usize;
                let display_list_index = be_u32(&self.bytes, draw_offset + 0x04, FORMAT)? as usize;
                let display_list = checked_slice(
                    FORMAT,
                    &self.bytes,
                    display_list_offset + display_list_index,
                    display_list_size,
                )?;
                let prepared = decode_prepared_display_list(
                    display_list,
                    &self.bytes,
                    positions,
                    &vtx_descs,
                    attr_formats,
                    position_format,
                    vertex_arrays,
                    &group_matrices,
                )?;
                let billboard = if matches!(shape_mtx_type, 0x01 | 0x02) {
                    group_matrices
                        .first()
                        .copied()
                        .filter(|index| *index != 0xFFFF)
                        .map(|draw_matrix_index| PreparedBillboard {
                            draw_matrix_index,
                            shape_matrix_type: shape_mtx_type,
                        })
                } else {
                    None
                };
                packets.push(PreparedAnimatedPacket {
                    vertices: prepared.vertices,
                    triangle_indices: prepared.triangle_indices,
                    shape_index: shape_no,
                    packet_index,
                    billboard,
                });
                packet_index += 1;
            }
        }

        Ok(packets)
    }

    fn shape_group_draw_matrices(
        &self,
        shape_mtx_type: u8,
        init_offset: usize,
        mtx_table_offset: Option<usize>,
    ) -> Result<Vec<u16>> {
        checked_slice(FORMAT, &self.bytes, init_offset, 0x08)?;
        let use_mtx_index = be_u16(&self.bytes, init_offset, FORMAT)?;
        let use_mtx_count = be_u16(&self.bytes, init_offset + 0x02, FORMAT)? as usize;
        let first_use_mtx_index = be_u32(&self.bytes, init_offset + 0x04, FORMAT)? as usize;

        if shape_mtx_type == 0x03 {
            let Some(table_offset) = mtx_table_offset else {
                return Ok(vec![use_mtx_index]);
            };
            let mut matrices = Vec::with_capacity(use_mtx_count.max(1));
            for index in 0..use_mtx_count {
                matrices.push(be_u16(
                    &self.bytes,
                    table_offset + (first_use_mtx_index + index) * 2,
                    FORMAT,
                )?);
            }
            if matrices.is_empty() {
                matrices.push(use_mtx_index);
            }
            Ok(matrices)
        } else {
            Ok(vec![use_mtx_index])
        }
    }

    fn preview_draw_matrices(
        &self,
        loader_flags: u32,
        animation: Option<(&J3dJointAnimation, f32)>,
        overrides: &[J3dJointTransformOverride],
    ) -> Result<Vec<Option<Mtx34>>> {
        let joint_matrices = self.preview_joint_matrices(loader_flags, animation, overrides)?;
        self.preview_draw_matrices_from_joint_matrices(&joint_matrices)
    }

    fn preview_draw_matrices_from_joint_matrices(
        &self,
        joint_matrices: &[Mtx34],
    ) -> Result<Vec<Option<Mtx34>>> {
        let envelope_matrices = self.preview_envelope_matrices(joint_matrices)?;
        let Some(drw1) = self.section("DRW1") else {
            return Ok(joint_matrices.iter().copied().map(Some).collect());
        };
        let base = drw1.offset as usize;
        let matrix_count = be_u16(&self.bytes, base + 0x08, FORMAT)? as usize;
        let flag_offset = relative_offset(&self.bytes, base, 0x0C)?;
        let index_offset = relative_offset(&self.bytes, base, 0x10)?;
        let mut matrices = Vec::with_capacity(matrix_count);
        for index in 0..matrix_count {
            let flag = checked_slice(FORMAT, &self.bytes, flag_offset + index, 1)?[0];
            let matrix_index = be_u16(&self.bytes, index_offset + index * 2, FORMAT)? as usize;
            let matrix = if flag == 0 {
                joint_matrices.get(matrix_index).copied()
            } else {
                envelope_matrices.get(matrix_index).copied()
            };
            matrices.push(matrix);
        }
        Ok(matrices)
    }

    fn preview_envelope_matrices(&self, joint_matrices: &[Mtx34]) -> Result<Vec<Mtx34>> {
        let Some(evp1) = self.section("EVP1") else {
            return Ok(Vec::new());
        };
        let base = evp1.offset as usize;
        let envelope_count = be_u16(&self.bytes, base + 0x08, FORMAT)? as usize;
        if envelope_count == 0 {
            return Ok(Vec::new());
        }
        let mix_count_offset = relative_offset(&self.bytes, base, 0x0C)?;
        let mix_index_offset = relative_offset(&self.bytes, base, 0x10)?;
        let mix_weight_offset = relative_offset(&self.bytes, base, 0x14)?;
        let inverse_matrix_offset = relative_offset(&self.bytes, base, 0x18)?;
        let mut mix_entry = 0usize;
        let mut matrices = Vec::with_capacity(envelope_count);

        for envelope in 0..envelope_count {
            let mix_count =
                checked_slice(FORMAT, &self.bytes, mix_count_offset + envelope, 1)?[0] as usize;
            let mut weighted = [[0.0; 4]; 3];
            for _ in 0..mix_count {
                let joint_index =
                    be_u16(&self.bytes, mix_index_offset + mix_entry * 2, FORMAT)? as usize;
                let weight = be_f32(&self.bytes, mix_weight_offset + mix_entry * 4, FORMAT)?;
                mix_entry += 1;
                let Some(animation) = joint_matrices.get(joint_index).copied() else {
                    continue;
                };
                let inverse = read_mtx34(&self.bytes, inverse_matrix_offset + joint_index * 0x30)?;
                add_weighted_mtx34(&mut weighted, concat_mtx34(animation, inverse), weight);
            }
            matrices.push(weighted);
        }

        Ok(matrices)
    }

    fn preview_joint_matrices(
        &self,
        loader_flags: u32,
        animation: Option<(&J3dJointAnimation, f32)>,
        overrides: &[J3dJointTransformOverride],
    ) -> Result<Vec<Mtx34>> {
        let Some(jnt1) = self.section("JNT1") else {
            return Ok(vec![identity_mtx34()]);
        };
        let base = jnt1.offset as usize;
        let joint_count = be_u16(&self.bytes, base + 0x08, FORMAT)? as usize;
        if joint_count == 0 {
            return Ok(vec![identity_mtx34()]);
        }
        let init_offset = relative_offset(&self.bytes, base, 0x0C)?;
        let index_table_offset = relative_offset(&self.bytes, base, 0x10)?;
        let parents = self.joint_parent_indices(joint_count)?;
        let mut local = Vec::with_capacity(joint_count);
        for joint in 0..joint_count {
            let init_index = be_u16(&self.bytes, index_table_offset + joint * 2, FORMAT)? as usize;
            let offset = init_offset + init_index * J3D_JOINT_INIT_DATA_SIZE;
            checked_slice(FORMAT, &self.bytes, offset, J3D_JOINT_INIT_DATA_SIZE)?;
            local.push(read_joint_preview_transform(&self.bytes, offset)?);
        }

        if let Some((animation, frame)) = animation {
            for (joint, transform) in local.iter_mut().enumerate() {
                let Some(sample) = animation.sample_joint(joint, frame) else {
                    continue;
                };
                transform.scale = sample.scale;
                transform.rotation = sample.rotation_degrees.map(degrees_to_j3d_angle);
                transform.translation = sample.translation;
            }
        }

        for joint_override in overrides {
            let Some(transform) = local.get_mut(joint_override.joint_index) else {
                continue;
            };
            for (translation, offset) in transform
                .translation
                .iter_mut()
                .zip(joint_override.translation_offset)
            {
                *translation += offset;
            }
            for (scale, multiplier) in transform
                .scale
                .iter_mut()
                .zip(joint_override.scale_multiplier)
            {
                *scale *= multiplier;
            }
        }

        let info = self
            .section("INF1")
            .ok_or_else(|| FormatError::Unsupported {
                format: FORMAT,
                message: "missing INF1 section".to_string(),
            })?;
        let file_flags = be_u16(&self.bytes, info.offset as usize + 0x08, FORMAT)? as u32;
        let matrix_mode = (loader_flags | file_flags) & 0x0F;

        match matrix_mode {
            1 => Ok(softimage_joint_matrices(&local, &parents)),
            2 => Ok(maya_joint_matrices(&local, &parents)),
            _ => Ok(basic_joint_matrices(&local, &parents)),
        }
    }

    fn joint_parent_indices(&self, joint_count: usize) -> Result<Vec<Option<usize>>> {
        let mut parents = vec![None; joint_count];
        let info = self
            .section("INF1")
            .ok_or_else(|| FormatError::Unsupported {
                format: FORMAT,
                message: "missing INF1 section".to_string(),
            })?;
        let info_offset = info.offset as usize;
        let hierarchy_offset = relative_offset(&self.bytes, info_offset, 0x14)?;
        let mut current_parent = None;
        let mut current_node = None;
        let mut stack = Vec::<(Option<usize>, Option<usize>)>::new();

        for index in 0..8192 {
            let offset = hierarchy_offset
                .checked_add(index * 4)
                .ok_or_else(|| invalid_offset(hierarchy_offset, self.bytes.len()))?;
            checked_slice(FORMAT, &self.bytes, offset, 4)?;
            let node_type = be_u16(&self.bytes, offset, FORMAT)?;
            let value = be_u16(&self.bytes, offset + 0x02, FORMAT)? as usize;
            match node_type {
                J3D_HIERARCHY_END => break,
                J3D_HIERARCHY_BEGIN_CHILD => {
                    stack.push((current_parent, current_node));
                    current_parent = current_node;
                }
                J3D_HIERARCHY_END_CHILD => {
                    if let Some((parent, node)) = stack.pop() {
                        current_parent = parent;
                        current_node = node;
                    }
                }
                J3D_HIERARCHY_JOINT => {
                    if let Some(parent) = parents.get_mut(value) {
                        *parent = current_parent;
                    }
                    current_node = Some(value);
                }
                _ => {}
            }
        }

        Ok(parents)
    }

    fn read_vertex_descs(&self, offset: usize) -> Result<Vec<VertexDesc>> {
        let mut descs = Vec::new();
        for index in 0..64 {
            let entry_offset = offset
                .checked_add(index * 0x08)
                .ok_or_else(|| invalid_offset(offset, self.bytes.len()))?;
            let attr = be_u32(&self.bytes, entry_offset, FORMAT)?;
            let attr_type = be_u32(&self.bytes, entry_offset + 0x04, FORMAT)?;
            if attr == GX_VA_NULL {
                break;
            }
            descs.push(VertexDesc { attr, attr_type });
        }

        Ok(descs)
    }

    fn shape_material_indices(&self) -> Result<Vec<Option<usize>>> {
        let shape_count = self
            .section("SHP1")
            .map(|section| be_u16(&self.bytes, section.offset as usize + 0x08, FORMAT))
            .transpose()?
            .unwrap_or(0) as usize;
        let mut materials = vec![None; shape_count];
        let info = self
            .section("INF1")
            .ok_or_else(|| FormatError::Unsupported {
                format: FORMAT,
                message: "missing INF1 section".to_string(),
            })?;
        let info_offset = info.offset as usize;
        let hierarchy_offset = relative_offset(&self.bytes, info_offset, 0x14)?;
        let mut current_material = None;

        for index in 0..8192 {
            let offset = hierarchy_offset
                .checked_add(index * 4)
                .ok_or_else(|| invalid_offset(hierarchy_offset, self.bytes.len()))?;
            checked_slice(FORMAT, &self.bytes, offset, 4)?;
            let node_type = be_u16(&self.bytes, offset, FORMAT)?;
            let value = be_u16(&self.bytes, offset + 0x02, FORMAT)? as usize;
            match node_type {
                J3D_HIERARCHY_END => break,
                J3D_HIERARCHY_MATERIAL => current_material = Some(value),
                J3D_HIERARCHY_SHAPE => {
                    if let Some(material) = materials.get_mut(value) {
                        *material = current_material;
                    }
                }
                _ => {}
            }
        }

        Ok(materials)
    }

    fn shape_joint_indices(&self) -> Result<Vec<Option<usize>>> {
        let shape_count = self
            .section("SHP1")
            .map(|section| be_u16(&self.bytes, section.offset as usize + 0x08, FORMAT))
            .transpose()?
            .unwrap_or(0) as usize;
        let mut owners = vec![None; shape_count];
        let info = self
            .section("INF1")
            .ok_or_else(|| FormatError::Unsupported {
                format: FORMAT,
                message: "missing INF1 section".to_string(),
            })?;
        let hierarchy_offset = relative_offset(&self.bytes, info.offset as usize, 0x14)?;
        let mut current_joint = None;
        let mut stack = Vec::new();

        for index in 0..8192 {
            let offset = hierarchy_offset
                .checked_add(index * 4)
                .ok_or_else(|| invalid_offset(hierarchy_offset, self.bytes.len()))?;
            checked_slice(FORMAT, &self.bytes, offset, 4)?;
            let node_type = be_u16(&self.bytes, offset, FORMAT)?;
            let value = be_u16(&self.bytes, offset + 0x02, FORMAT)? as usize;
            match node_type {
                J3D_HIERARCHY_END => break,
                J3D_HIERARCHY_BEGIN_CHILD => stack.push(current_joint),
                J3D_HIERARCHY_END_CHILD => current_joint = stack.pop().flatten(),
                J3D_HIERARCHY_JOINT => current_joint = Some(value),
                J3D_HIERARCHY_SHAPE => {
                    if let Some(owner) = owners.get_mut(value) {
                        *owner = current_joint;
                    }
                }
                _ => {}
            }
        }
        Ok(owners)
    }

    fn material_preview_colors(&self) -> Result<Vec<[u8; 4]>> {
        let Some(mat3) = self.section("MAT3") else {
            return Ok(Vec::new());
        };
        let base = mat3.offset as usize;
        let material_count = be_u16(&self.bytes, base + 0x08, FORMAT)? as usize;
        let init_offset = relative_offset(&self.bytes, base, 0x0C)?;
        let material_id_offset = relative_offset(&self.bytes, base, 0x10)?;
        let mat_color_offset = relative_offset(&self.bytes, base, 0x20).ok();
        let amb_color_offset = relative_offset(&self.bytes, base, 0x2C).ok();
        let mut colors = Vec::with_capacity(material_count);

        for index in 0..material_count {
            let material_id = be_u16(&self.bytes, material_id_offset + index * 2, FORMAT)? as usize;
            let init = init_offset + material_id * J3D_MATERIAL_INIT_DATA_SIZE;
            checked_slice(FORMAT, &self.bytes, init, J3D_MATERIAL_INIT_DATA_SIZE)?;

            let mat_color = read_indexed_color(
                &self.bytes,
                mat_color_offset,
                be_u16(&self.bytes, init + 0x08, FORMAT)?,
            );
            let amb_color = read_indexed_color(
                &self.bytes,
                amb_color_offset,
                be_u16(&self.bytes, init + 0x14, FORMAT)
                    .ok()
                    .unwrap_or(0xFFFF),
            );
            let color = mat_color
                .filter(|color| preview_color_is_useful(*color))
                .or_else(|| amb_color.filter(|color| preview_color_is_useful(*color)))
                .or(mat_color)
                .or(amb_color)
                .unwrap_or([255, 255, 255, 255]);
            colors.push(color);
        }

        Ok(colors)
    }

    fn material_render_states(&self) -> Result<Vec<J3dMaterialRenderState>> {
        let Some(mat3) = self.section("MAT3") else {
            return Ok(Vec::new());
        };
        let base = mat3.offset as usize;
        let material_count = be_u16(&self.bytes, base + 0x08, FORMAT)? as usize;
        let init_offset = relative_offset(&self.bytes, base, 0x0C)?;
        let material_id_offset = relative_offset(&self.bytes, base, 0x10)?;
        let cull_mode_offset = relative_offset(&self.bytes, base, 0x1C).ok();
        let alpha_comp_offset = relative_offset(&self.bytes, base, 0x6C).ok();
        let blend_offset = relative_offset(&self.bytes, base, 0x70).ok();
        let z_mode_offset = relative_offset(&self.bytes, base, 0x74).ok();
        let z_comp_loc_offset = relative_offset(&self.bytes, base, 0x78).ok();
        let mut states = Vec::with_capacity(material_count);

        for index in 0..material_count {
            let material_id = be_u16(&self.bytes, material_id_offset + index * 2, FORMAT)? as usize;
            let init = init_offset + material_id * J3D_MATERIAL_INIT_DATA_SIZE;
            checked_slice(FORMAT, &self.bytes, init, J3D_MATERIAL_INIT_DATA_SIZE)?;

            let cull_index = checked_slice(FORMAT, &self.bytes, init + 0x01, 1)?[0];
            let cull_mode = read_indexed_cull_mode(&self.bytes, cull_mode_offset, cull_index);
            let z_comp_loc_index = checked_slice(FORMAT, &self.bytes, init + 0x05, 1)?[0];
            let z_comp_loc = read_indexed_u8(&self.bytes, z_comp_loc_offset, z_comp_loc_index);
            let z_mode_index = checked_slice(FORMAT, &self.bytes, init + 0x06, 1)?[0];
            let z_mode = read_indexed_z_mode(&self.bytes, z_mode_offset, z_mode_index);
            let alpha_comp_index = be_u16(&self.bytes, init + 0x146, FORMAT)?;
            let alpha_compare =
                read_indexed_alpha_compare(&self.bytes, alpha_comp_offset, alpha_comp_index);
            let blend_index = be_u16(&self.bytes, init + 0x148, FORMAT)?;
            let blend_mode = read_indexed_blend_mode(&self.bytes, blend_offset, blend_index);

            states.push(J3dMaterialRenderState {
                cull_mode,
                alpha_compare,
                blend_mode,
                z_mode,
                z_comp_loc,
            });
        }

        Ok(states)
    }

    fn material_texture_bindings(
        &self,
        textures: &[J3dTexturePreview],
        material_colors: &[[u8; 4]],
    ) -> Result<Vec<Option<MaterialPreviewBinding>>> {
        let Some(mat3) = self.section("MAT3") else {
            return Ok(Vec::new());
        };
        let base = mat3.offset as usize;
        let material_count = be_u16(&self.bytes, base + 0x08, FORMAT)? as usize;
        let init_offset = relative_offset(&self.bytes, base, 0x0C)?;
        let material_id_offset = relative_offset(&self.bytes, base, 0x10)?;
        let tex_coord_offset = relative_offset(&self.bytes, base, 0x38).ok();
        let tex_mtx_offset = relative_offset(&self.bytes, base, 0x40).ok();
        let tex_no_offset = relative_offset(&self.bytes, base, 0x48).ok();
        let tev_order_offset = relative_offset(&self.bytes, base, 0x4C).ok();
        let tev_color_offset = relative_offset(&self.bytes, base, 0x50).ok();
        let tev_k_color_offset = relative_offset(&self.bytes, base, 0x54).ok();
        let tev_stage_num_offset = relative_offset(&self.bytes, base, 0x58).ok();
        let tev_stage_info_offset = relative_offset(&self.bytes, base, 0x5C).ok();
        let color_chan_info_offset = relative_offset(&self.bytes, base, 0x28).ok();
        let mut texture_bindings = Vec::with_capacity(material_count);

        for index in 0..material_count {
            let material_id = be_u16(&self.bytes, material_id_offset + index * 2, FORMAT)? as usize;
            let init = init_offset + material_id * J3D_MATERIAL_INIT_DATA_SIZE;
            checked_slice(FORMAT, &self.bytes, init, J3D_MATERIAL_INIT_DATA_SIZE)?;
            let tev_colors = material_tev_colors(&self.bytes, init, tev_color_offset);
            let tev_k_colors = material_tev_k_colors(&self.bytes, init, tev_k_color_offset);
            let material_color = material_colors.get(index).copied();

            let mut first_valid = None;
            let mut first_color = None;
            let mut first_intensity_mask = None;
            let mut last_textureless = None;
            let mut previous_stage_color = None;
            let stage_count =
                material_tev_stage_count(&self.bytes, init, tev_stage_num_offset).unwrap_or(16);
            for stage in 0..stage_count.min(16) {
                let Some(order) =
                    read_material_tev_order(&self.bytes, init, tev_order_offset, stage)
                else {
                    continue;
                };
                let raster_source = material_color_channel_source(
                    &self.bytes,
                    init,
                    color_chan_info_offset,
                    order.color_chan,
                );
                let stage_preview_color = material_tev_stage_preview_color(
                    &self.bytes,
                    init,
                    tev_stage_info_offset,
                    stage,
                    tev_colors,
                    tev_k_colors,
                    previous_stage_color,
                    material_color,
                    raster_source,
                );
                if let Some(color) = stage_preview_color {
                    previous_stage_color = Some(color);
                }
                let tint_color = material_tev_stage_modulate_tint_color(
                    &self.bytes,
                    init,
                    tev_stage_info_offset,
                    stage,
                    tev_k_colors,
                )
                .or_else(|| {
                    material_tev_stage_blend_tint_color(
                        &self.bytes,
                        init,
                        tev_stage_info_offset,
                        stage,
                        tev_k_colors,
                        material_color,
                        raster_source,
                    )
                })
                .or(stage_preview_color);
                let uses_previous_texture_mask = material_tev_stage_uses_previous_texture_mask(
                    &self.bytes,
                    init,
                    tev_stage_info_offset,
                    stage,
                );
                let mut binding = if let Some(tex_map_index) = order.tex_map_index {
                    let Some(texture_index) = material_texture_index_for_slot(
                        &self.bytes,
                        init,
                        tex_no_offset,
                        tex_map_index,
                    ) else {
                        continue;
                    };
                    let Some(combine_mode) = material_tev_stage_texture_combine(
                        &self.bytes,
                        init,
                        tev_stage_info_offset,
                        stage,
                        raster_source,
                    ) else {
                        continue;
                    };
                    let texture_is_intensity = textures
                        .get(texture_index)
                        .is_some_and(|texture| preview_texture_is_intensity(texture.format));
                    let combine_mode = if combine_mode == J3dPreviewCombineMode::TextureOnly
                        && texture_is_intensity
                        && material_color.is_some_and(preview_color_is_useful)
                    {
                        J3dPreviewCombineMode::TextureModulateMaterial
                    } else {
                        combine_mode
                    };
                    let tex_coord = material_generated_tex_coord_binding(
                        &self.bytes,
                        init,
                        tex_coord_offset,
                        tex_mtx_offset,
                        order.tex_coord_index.unwrap_or(tex_map_index),
                    );
                    MaterialPreviewBinding {
                        texture_index: Some(texture_index),
                        tex_coord_source: tex_coord
                            .map(|tex_coord| tex_coord.source)
                            .unwrap_or_else(|| {
                                TexCoordPreviewSource::Vertex(
                                    order.tex_coord_index.unwrap_or(tex_map_index),
                                )
                            }),
                        tex_mtx: tex_coord.and_then(|tex_coord| tex_coord.tex_mtx),
                        mask_texture_index: None,
                        mask_tex_coord_source: TexCoordPreviewSource::Vertex(0),
                        mask_tex_mtx: None,
                        combine_mode,
                        tint_color,
                    }
                } else {
                    let Some(combine_mode) = material_tev_stage_raster_combine(
                        &self.bytes,
                        init,
                        tev_stage_info_offset,
                        stage,
                        raster_source,
                    )
                    .or_else(|| stage_preview_color.map(|_| J3dPreviewCombineMode::MaterialOnly)) else {
                        continue;
                    };
                    MaterialPreviewBinding {
                        texture_index: None,
                        tex_coord_source: TexCoordPreviewSource::Vertex(0),
                        tex_mtx: None,
                        mask_texture_index: None,
                        mask_tex_coord_source: TexCoordPreviewSource::Vertex(0),
                        mask_tex_mtx: None,
                        combine_mode,
                        tint_color,
                    }
                };
                if first_valid.is_none() {
                    first_valid = Some(binding);
                }
                if binding.texture_index.is_none() {
                    last_textureless = Some(binding);
                }
                if binding.texture_index.is_some_and(|texture_index| {
                    textures
                        .get(texture_index)
                        .is_some_and(|texture| preview_texture_is_intensity(texture.format))
                }) && first_intensity_mask.is_none()
                {
                    first_intensity_mask = Some(binding);
                }
                if binding.texture_index.is_some_and(|texture_index| {
                    textures
                        .get(texture_index)
                        .is_some_and(|texture| preview_texture_is_base_color(texture.format))
                }) {
                    if uses_previous_texture_mask {
                        if let Some(mask) = first_intensity_mask {
                            binding.mask_texture_index = mask.texture_index;
                            binding.mask_tex_coord_source = mask.tex_coord_source;
                            binding.mask_tex_mtx = mask.tex_mtx;
                        }
                    }
                    first_color = Some(binding);
                    break;
                }
            }

            if first_valid.is_none() {
                for slot in 0..TEX_COORD_COUNT {
                    let Some(texture_index) =
                        material_texture_index_for_slot(&self.bytes, init, tex_no_offset, slot)
                    else {
                        continue;
                    };
                    let tex_coord = material_generated_tex_coord_binding(
                        &self.bytes,
                        init,
                        tex_coord_offset,
                        tex_mtx_offset,
                        slot,
                    );
                    let binding = MaterialPreviewBinding {
                        texture_index: Some(texture_index),
                        tex_coord_source: tex_coord
                            .map(|tex_coord| tex_coord.source)
                            .unwrap_or(TexCoordPreviewSource::Vertex(slot)),
                        tex_mtx: tex_coord.and_then(|tex_coord| tex_coord.tex_mtx),
                        mask_texture_index: None,
                        mask_tex_coord_source: TexCoordPreviewSource::Vertex(0),
                        mask_tex_mtx: None,
                        combine_mode: if textures
                            .get(texture_index)
                            .is_some_and(|texture| preview_texture_is_intensity(texture.format))
                            && material_color.is_some_and(preview_color_is_useful)
                        {
                            J3dPreviewCombineMode::TextureModulateMaterial
                        } else {
                            J3dPreviewCombineMode::TextureOnly
                        },
                        tint_color: None,
                    };
                    if first_valid.is_none() {
                        first_valid = Some(binding);
                    }
                    if textures
                        .get(texture_index)
                        .is_some_and(|texture| preview_texture_is_base_color(texture.format))
                    {
                        first_color = Some(binding);
                        break;
                    }
                }
            }

            texture_bindings.push(first_color.or(last_textureless).or(first_valid));
        }

        Ok(texture_bindings)
    }

    pub fn texture_previews(&self) -> Result<Vec<J3dTexturePreview>> {
        self.texture_previews_with_limits(MAX_TEX1_TEXTURE_COUNT, MAX_TEX1_RETAINED_BYTES)
    }

    fn texture_previews_with_limits(
        &self,
        max_texture_count: usize,
        max_retained_bytes: usize,
    ) -> Result<Vec<J3dTexturePreview>> {
        let Some(tex1) = self.section("TEX1") else {
            return Ok(Vec::new());
        };
        let base = tex1.offset as usize;
        let texture_count = be_u16(&self.bytes, base + 0x08, FORMAT)? as usize;
        if texture_count > max_texture_count {
            return Err(FormatError::ResourceLimit {
                format: FORMAT,
                resource: "TEX1 texture count",
                requested: texture_count,
                limit: max_texture_count,
            });
        }
        let texture_offset = relative_offset(&self.bytes, base, 0x0C)?;
        let texture_names = optional_relative_offset(&self.bytes, base, 0x10)
            .map(|offset| read_jut_name_table(&self.bytes, offset, self.bytes.len()))
            .transpose()?
            .unwrap_or_default();
        let mut textures = Vec::with_capacity(texture_count);
        let mut retained_bytes = 0usize;

        for index in 0..texture_count {
            let header_offset = texture_offset + index * 0x20;
            checked_slice(FORMAT, &self.bytes, header_offset, 0x20)?;
            let texture_bytes = decoded_timg_retained_bytes(&self.bytes, header_offset)?;
            let requested =
                retained_bytes
                    .checked_add(texture_bytes)
                    .ok_or(FormatError::ResourceLimit {
                        format: FORMAT,
                        resource: "TEX1 retained decoded texture bytes",
                        requested: usize::MAX,
                        limit: max_retained_bytes,
                    })?;
            if requested > max_retained_bytes {
                return Err(FormatError::ResourceLimit {
                    format: FORMAT,
                    resource: "TEX1 retained decoded texture bytes",
                    requested,
                    limit: max_retained_bytes,
                });
            }
            let mut texture = decode_timg(&self.bytes, header_offset)?;
            texture.name = texture_names.get(index).cloned().unwrap_or_default();
            textures.push(texture);
            retained_bytes = requested;
        }

        Ok(textures)
    }

    pub fn material_programs(&self) -> Result<Vec<J3dMaterial>> {
        self.material_programs_with_loader_flags(SMS_MAP_MODEL_LOAD_FLAGS)
    }

    pub fn material_programs_with_loader_flags(
        &self,
        loader_flags: u32,
    ) -> Result<Vec<J3dMaterial>> {
        let Some(mat3) = self.section("MAT3") else {
            return Ok(Vec::new());
        };
        let base = mat3.offset as usize;
        let material_count = be_u16(&self.bytes, base + 0x08, FORMAT)? as usize;
        let init_offset = relative_offset(&self.bytes, base, 0x0C)?;
        let material_id_offset = relative_offset(&self.bytes, base, 0x10)?;
        let material_names = optional_relative_offset(&self.bytes, base, 0x14)
            .map(|offset| {
                read_jut_name_table(
                    &self.bytes,
                    offset,
                    base.saturating_add(mat3.size as usize)
                        .min(self.bytes.len()),
                )
            })
            .transpose()?
            .unwrap_or_default();
        let ind_init_offset = optional_relative_offset(&self.bytes, base, 0x18);
        let cull_mode_offset = optional_relative_offset(&self.bytes, base, 0x1C);
        let mat_color_offset = optional_relative_offset(&self.bytes, base, 0x20);
        let color_chan_num_offset = optional_relative_offset(&self.bytes, base, 0x24);
        let color_chan_info_offset = optional_relative_offset(&self.bytes, base, 0x28);
        let amb_color_offset = optional_relative_offset(&self.bytes, base, 0x2C);
        let tex_gen_num_offset = optional_relative_offset(&self.bytes, base, 0x34);
        let tex_coord_offset = optional_relative_offset(&self.bytes, base, 0x38);
        let tex_mtx_offset = optional_relative_offset(&self.bytes, base, 0x40);
        let tex_no_offset = optional_relative_offset(&self.bytes, base, 0x48);
        let tev_order_offset = optional_relative_offset(&self.bytes, base, 0x4C);
        let tev_color_offset = optional_relative_offset(&self.bytes, base, 0x50);
        let tev_k_color_offset = optional_relative_offset(&self.bytes, base, 0x54);
        let tev_stage_num_offset = optional_relative_offset(&self.bytes, base, 0x58);
        let tev_stage_info_offset = optional_relative_offset(&self.bytes, base, 0x5C);
        let tev_swap_mode_offset = optional_relative_offset(&self.bytes, base, 0x60);
        let tev_swap_table_offset = optional_relative_offset(&self.bytes, base, 0x64);
        let fog_offset = optional_relative_offset(&self.bytes, base, 0x68);
        let alpha_comp_offset = optional_relative_offset(&self.bytes, base, 0x6C);
        let blend_offset = optional_relative_offset(&self.bytes, base, 0x70);
        let z_mode_offset = optional_relative_offset(&self.bytes, base, 0x74);
        let z_comp_loc_offset = optional_relative_offset(&self.bytes, base, 0x78);
        let dither_offset = optional_relative_offset(&self.bytes, base, 0x7C);

        let mut materials = Vec::with_capacity(material_count);
        for material_index in 0..material_count {
            let material_id =
                be_u16(&self.bytes, material_id_offset + material_index * 2, FORMAT)? as usize;
            let init = init_offset + material_id * J3D_MATERIAL_INIT_DATA_SIZE;
            let init_data = checked_slice(FORMAT, &self.bytes, init, J3D_MATERIAL_INIT_DATA_SIZE)?;
            let mode = init_data[0];

            let cull_mode =
                read_indexed_cull_mode(&self.bytes, cull_mode_offset, init_data[1]).unwrap_or(2);
            let color_channel_count =
                read_indexed_u8(&self.bytes, color_chan_num_offset, init_data[2]).unwrap_or(0);
            let tex_gen_count =
                read_indexed_u8(&self.bytes, tex_gen_num_offset, init_data[3]).unwrap_or(0);

            let material_colors = std::array::from_fn(|slot| {
                read_indexed_color(
                    &self.bytes,
                    mat_color_offset,
                    be_u16(&self.bytes, init + 0x08 + slot * 2, FORMAT).unwrap_or(0xFFFF),
                )
                .unwrap_or([255, 255, 255, 255])
            });
            let ambient_colors = std::array::from_fn(|slot| {
                read_indexed_color(
                    &self.bytes,
                    amb_color_offset,
                    be_u16(&self.bytes, init + 0x14 + slot * 2, FORMAT).unwrap_or(0xFFFF),
                )
                .unwrap_or([50, 50, 50, 50])
            });
            let color_channels = std::array::from_fn(|slot| {
                read_material_color_channel(&self.bytes, init, color_chan_info_offset, slot)
                    .unwrap_or_default()
            });
            let tex_gens = std::array::from_fn(|slot| {
                read_material_tex_gen(&self.bytes, init, tex_coord_offset, slot)
            });
            let tex_matrices = std::array::from_fn(|slot| {
                read_material_tex_matrix(&self.bytes, init, tex_mtx_offset, slot)
            });
            let texture_indices = std::array::from_fn(|slot| {
                material_texture_index_for_slot(&self.bytes, init, tex_no_offset, slot)
            });
            let tev_colors = material_tev_colors(&self.bytes, init, tev_color_offset);
            let tev_k_colors = material_tev_k_colors(&self.bytes, init, tev_k_color_offset);
            let mut indirect = read_material_indirect(&self.bytes, ind_init_offset, material_index);
            if loader_flags & 0x0100_0000 == 0 {
                indirect.enabled = false;
                indirect.stage_count = 0;
            }
            let stage_count = material_tev_stage_count(&self.bytes, init, tev_stage_num_offset)
                .unwrap_or_else(|| {
                    texture_indices
                        .iter()
                        .filter(|index| index.is_some())
                        .count()
                })
                .min(16);
            let tev_stages = (0..stage_count)
                .map(|stage| {
                    read_material_tev_stage(
                        &self.bytes,
                        init,
                        tev_order_offset,
                        tev_stage_info_offset,
                        tev_swap_mode_offset,
                        &indirect,
                        stage,
                    )
                })
                .collect();
            let swap_tables = std::array::from_fn(|slot| {
                read_material_swap_table(&self.bytes, init, tev_swap_table_offset, slot)
            });

            let explicit_alpha = read_indexed_alpha_compare(
                &self.bytes,
                alpha_comp_offset,
                be_u16(&self.bytes, init + 0x146, FORMAT).unwrap_or(0xFFFF),
            );
            let explicit_blend = read_indexed_blend_mode(
                &self.bytes,
                blend_offset,
                be_u16(&self.bytes, init + 0x148, FORMAT).unwrap_or(0xFFFF),
            );
            let explicit_z = read_indexed_z_mode(&self.bytes, z_mode_offset, init_data[6]);
            let explicit_z_comp = read_indexed_u8(&self.bytes, z_comp_loc_offset, init_data[5]);
            let (alpha_compare, blend_mode, z_mode, z_comp_loc) = resolve_pe_state(
                mode,
                loader_flags & 0x1000_0000 != 0,
                explicit_alpha,
                explicit_blend,
                explicit_z,
                explicit_z_comp,
            );
            let fog = be_u16(&self.bytes, init + 0x144, FORMAT)
                .ok()
                .and_then(|index| read_indexed_fog(&self.bytes, fog_offset, index));
            let dither = read_indexed_u8(&self.bytes, dither_offset, init_data[7]).unwrap_or(0);

            materials.push(J3dMaterial {
                name: material_names
                    .get(material_index)
                    .cloned()
                    .unwrap_or_default(),
                material_index,
                material_id,
                loader_flags,
                lighting_enabled: loader_flags & 0x4000_0000 != 0,
                mode,
                cull_mode,
                color_channel_count,
                material_colors,
                ambient_colors,
                color_channels,
                tex_gen_count,
                tex_gens,
                tex_matrices,
                texture_indices,
                tev_colors,
                tev_k_colors,
                tev_stages,
                swap_tables,
                indirect,
                fog,
                alpha_compare,
                blend_mode,
                z_mode,
                z_comp_loc,
                dither,
            });
        }

        Ok(materials)
    }

    pub fn material_diagnostics(&self) -> Result<Vec<J3dMaterialDiagnostic>> {
        let Some(mat3) = self.section("MAT3") else {
            return Ok(Vec::new());
        };
        let textures = self.texture_previews()?;
        let material_colors = self.material_preview_colors()?;
        let material_render_states = self.material_render_states()?;
        let base = mat3.offset as usize;
        let material_count = be_u16(&self.bytes, base + 0x08, FORMAT)? as usize;
        let material_programs = self.material_programs()?;
        let init_offset = relative_offset(&self.bytes, base, 0x0C)?;
        let material_id_offset = relative_offset(&self.bytes, base, 0x10)?;
        let tex_no_offset = relative_offset(&self.bytes, base, 0x48).ok();
        let tev_order_offset = relative_offset(&self.bytes, base, 0x4C).ok();
        let tev_color_offset = relative_offset(&self.bytes, base, 0x50).ok();
        let tev_k_color_offset = relative_offset(&self.bytes, base, 0x54).ok();
        let tev_stage_num_offset = relative_offset(&self.bytes, base, 0x58).ok();
        let tev_stage_info_offset = relative_offset(&self.bytes, base, 0x5C).ok();
        let mut materials = Vec::with_capacity(material_count);

        for index in 0..material_count {
            let material_id = be_u16(&self.bytes, material_id_offset + index * 2, FORMAT)? as usize;
            let init = init_offset + material_id * J3D_MATERIAL_INIT_DATA_SIZE;
            checked_slice(FORMAT, &self.bytes, init, J3D_MATERIAL_INIT_DATA_SIZE)?;
            let render_state = material_render_states
                .get(index)
                .copied()
                .unwrap_or_default();
            let tev_colors = material_tev_colors(&self.bytes, init, tev_color_offset);
            let tev_k_colors = material_tev_k_colors(&self.bytes, init, tev_k_color_offset);
            let stage_count =
                material_tev_stage_count(&self.bytes, init, tev_stage_num_offset).unwrap_or(16);
            let mut stages = Vec::new();

            for stage in 0..stage_count.min(16) {
                let Some(order) =
                    read_material_tev_order(&self.bytes, init, tev_order_offset, stage)
                else {
                    continue;
                };
                let texture_index = order.tex_map_index.and_then(|slot| {
                    material_texture_index_for_slot(&self.bytes, init, tex_no_offset, slot)
                });
                let texture_format = texture_index
                    .and_then(|texture_index| textures.get(texture_index).map(|t| t.format));
                let tex_coord_offset = relative_offset(&self.bytes, base, 0x38).ok();
                let tex_gen = order.tex_coord_index.map(|index| {
                    material_tex_coord_info(&self.bytes, init, tex_coord_offset, index)
                });
                let color_args =
                    material_tev_stage_color_args(&self.bytes, init, tev_stage_info_offset, stage)
                        .map(|args| [args.a, args.b, args.c, args.d])
                        .unwrap_or([0xFF; 4]);
                let alpha_args =
                    material_tev_stage_alpha_args(&self.bytes, init, tev_stage_info_offset, stage)
                        .map(|args| [args.a, args.b, args.c, args.d])
                        .unwrap_or([0xFF; 4]);
                let k_color_sel = checked_slice(FORMAT, &self.bytes, init + 0x9C + stage, 1)
                    .ok()
                    .and_then(|bytes| bytes.first().copied())
                    .unwrap_or(0xFF);
                let k_alpha_sel = checked_slice(FORMAT, &self.bytes, init + 0xAC + stage, 1)
                    .ok()
                    .and_then(|bytes| bytes.first().copied())
                    .unwrap_or(0xFF);
                stages.push(J3dTevStageDiagnostic {
                    stage,
                    tex_coord_index: order.tex_coord_index,
                    tex_map_slot: order.tex_map_index,
                    tex_gen_src: tex_gen.map(|info| info.gen_src),
                    tex_gen_mtx: tex_gen.map(|info| info.gen_mtx),
                    texture_index,
                    texture_format,
                    color_chan: order.color_chan,
                    color_args,
                    alpha_args,
                    k_color_sel,
                    k_alpha_sel,
                    konst_color: material_tev_stage_konst_color(
                        &self.bytes,
                        init,
                        tev_stage_info_offset,
                        stage,
                        tev_k_colors,
                    ),
                });
            }

            materials.push(J3dMaterialDiagnostic {
                material_index: index,
                material_id,
                name: material_programs
                    .get(index)
                    .map_or_else(String::new, |material| material.name.clone()),
                mode: material_programs
                    .get(index)
                    .map_or(0, |material| material.mode),
                color_channel_count: material_programs
                    .get(index)
                    .map_or(0, |material| material.color_channel_count),
                color: material_colors
                    .get(index)
                    .copied()
                    .unwrap_or([255, 255, 255, 255]),
                material_colors: material_programs
                    .get(index)
                    .map_or([[255; 4]; 2], |material| material.material_colors),
                ambient_colors: material_programs
                    .get(index)
                    .map_or([[50; 4]; 2], |material| material.ambient_colors),
                color_channels: material_programs
                    .get(index)
                    .map_or([J3dColorChannel::default(); 4], |material| {
                        material.color_channels
                    }),
                cull_mode: render_state.cull_mode,
                alpha_compare: render_state.alpha_compare,
                blend_mode: render_state.blend_mode,
                z_mode: render_state.z_mode,
                z_comp_loc: render_state.z_comp_loc,
                tev_colors,
                tev_k_colors,
                stages,
            });
        }

        Ok(materials)
    }
}

fn runtime_child_joint_index(
    parents: &[Option<usize>],
    parent: usize,
    child_index: usize,
) -> Option<usize> {
    // TJointObj::initChildren walks younger siblings forward while filling its
    // child array from the end, so runtime indices reverse INF1 sibling order.
    parents
        .iter()
        .enumerate()
        .filter_map(|(joint, joint_parent)| (*joint_parent == Some(parent)).then_some(joint))
        .rev()
        .nth(child_index)
}

fn map_building_joint_from_parents(
    parents: &[Option<usize>],
    building_index: usize,
) -> Option<usize> {
    let root = parents.iter().position(Option::is_none)?;
    let building_parent = runtime_child_joint_index(parents, root, 0)?;
    runtime_child_joint_index(parents, building_parent, building_index)
}

fn joint_is_in_subtree(mut joint: usize, subtree_root: usize, parents: &[Option<usize>]) -> bool {
    loop {
        if joint == subtree_root {
            return true;
        }
        let Some(parent) = parents.get(joint).copied().flatten() else {
            return false;
        };
        joint = parent;
    }
}

pub fn decode_bti_texture(bytes: impl AsRef<[u8]>) -> Result<J3dTexturePreview> {
    decode_timg(bytes.as_ref(), 0)
}

fn read_material_color_channel(
    bytes: &[u8],
    init_offset: usize,
    table_offset: Option<usize>,
    slot: usize,
) -> Option<J3dColorChannel> {
    let index = be_u16(bytes, init_offset + 0x0C + slot * 2, FORMAT).ok()?;
    if index == 0xFFFF {
        return None;
    }
    let info = checked_slice(
        FORMAT,
        bytes,
        table_offset?.checked_add(index as usize * 8)?,
        8,
    )
    .ok()?;
    Some(J3dColorChannel {
        enable: info[0],
        mat_src: info[1],
        light_mask: info[2],
        diffuse_fn: info[3],
        attenuation_fn: info[4],
        amb_src: if info[5] == 0xFF { 0 } else { info[5] },
    })
}

fn read_material_tex_gen(
    bytes: &[u8],
    init_offset: usize,
    table_offset: Option<usize>,
    slot: usize,
) -> J3dTexGen {
    let default = J3dTexGen {
        gen_type: 1,
        source: GX_TG_TEX0 + slot as u8,
        matrix: GX_IDENTITY,
    };
    let Ok(index) = be_u16(bytes, init_offset + 0x28 + slot * 2, FORMAT) else {
        return default;
    };
    if index == 0xFFFF {
        return default;
    }
    let Some(offset) = table_offset.and_then(|offset| offset.checked_add(index as usize * 4))
    else {
        return default;
    };
    let Ok(info) = checked_slice(FORMAT, bytes, offset, 4) else {
        return default;
    };
    J3dTexGen {
        gen_type: info[0],
        source: info[1],
        matrix: info[2],
    }
}

fn read_material_tex_matrix(
    bytes: &[u8],
    init_offset: usize,
    table_offset: Option<usize>,
    slot: usize,
) -> Option<J3dTexMatrix> {
    let index = be_u16(bytes, init_offset + 0x48 + slot * 2, FORMAT).ok()?;
    if index == 0xFFFF {
        return None;
    }
    let offset = table_offset?.checked_add(index as usize * J3D_TEX_MTX_INFO_SIZE)?;
    checked_slice(FORMAT, bytes, offset, J3D_TEX_MTX_INFO_SIZE).ok()?;
    let info = checked_slice(FORMAT, bytes, offset + 1, 1).ok()?[0];
    let mut effect_matrix = [[0.0; 4]; 4];
    for (row, values) in effect_matrix.iter_mut().enumerate() {
        for (column, value) in values.iter_mut().enumerate() {
            *value = be_f32(bytes, offset + 0x24 + (row * 4 + column) * 4, FORMAT).ok()?;
        }
    }
    Some(J3dTexMatrix {
        projection: checked_slice(FORMAT, bytes, offset, 1).ok()?[0],
        mode: info & 0x7F,
        maya: info & 0x80 != 0,
        center: [
            be_f32(bytes, offset + 0x04, FORMAT).ok()?,
            be_f32(bytes, offset + 0x08, FORMAT).ok()?,
            be_f32(bytes, offset + 0x0C, FORMAT).ok()?,
        ],
        scale: [
            be_f32(bytes, offset + 0x10, FORMAT).ok()?,
            be_f32(bytes, offset + 0x14, FORMAT).ok()?,
        ],
        rotation: be_i16(bytes, offset + 0x18, FORMAT).ok()?,
        translation: [
            be_f32(bytes, offset + 0x1C, FORMAT).ok()?,
            be_f32(bytes, offset + 0x20, FORMAT).ok()?,
        ],
        effect_matrix,
    })
}

fn read_material_indirect(
    bytes: &[u8],
    table_offset: Option<usize>,
    material_index: usize,
) -> J3dIndirectMaterial {
    let Some(offset) =
        table_offset.and_then(|offset| offset.checked_add(material_index * J3D_IND_INIT_DATA_SIZE))
    else {
        return J3dIndirectMaterial::default();
    };
    let Ok(info) = checked_slice(FORMAT, bytes, offset, J3D_IND_INIT_DATA_SIZE) else {
        return J3dIndirectMaterial::default();
    };
    if info[0] == 0 {
        return J3dIndirectMaterial::default();
    }

    let orders = std::array::from_fn(|slot| {
        let base = 0x04 + slot * 4;
        let tex_coord = info[base];
        let tex_map = info[base + 1];
        Some(J3dIndirectOrder {
            tex_coord: (tex_coord < 8).then_some(tex_coord),
            tex_map: (tex_map < 8).then_some(tex_map),
        })
    });
    let matrices = std::array::from_fn(|slot| {
        let base = offset + 0x14 + slot * 0x1C;
        let mut rows = [[0.0; 3]; 2];
        for (row, values) in rows.iter_mut().enumerate() {
            for (column, value) in values.iter_mut().enumerate() {
                *value = be_f32(bytes, base + (row * 3 + column) * 4, FORMAT).ok()?;
            }
        }
        Some(J3dIndirectMatrix {
            rows,
            scale_exponent: checked_slice(FORMAT, bytes, base + 0x18, 1).ok()?[0] as i8,
        })
    });
    let scales = std::array::from_fn(|slot| {
        let base = 0x68 + slot * 4;
        Some(J3dIndirectScale {
            scale_s: info[base],
            scale_t: info[base + 1],
        })
    });
    let tev_stages = std::array::from_fn(|stage| {
        let base = 0x78 + stage * 0x0C;
        J3dIndirectTevStage {
            stage: info[base],
            format: info[base + 1],
            bias: info[base + 2],
            matrix: info[base + 3],
            wrap_s: info[base + 4],
            wrap_t: info[base + 5],
            add_previous: info[base + 6],
            use_original_lod: info[base + 7],
            alpha: info[base + 8],
        }
    });

    J3dIndirectMaterial {
        enabled: true,
        stage_count: info[1].min(4),
        orders,
        matrices,
        scales,
        tev_stages,
    }
}

fn read_material_tev_stage(
    bytes: &[u8],
    init_offset: usize,
    order_table_offset: Option<usize>,
    stage_table_offset: Option<usize>,
    swap_mode_table_offset: Option<usize>,
    indirect: &J3dIndirectMaterial,
    stage: usize,
) -> J3dTevStage {
    const DEFAULT_INFO: [u8; J3D_TEV_STAGE_INFO_SIZE] = [
        0x04, 0x0A, 0x0F, 0x0F, 0x00, 0x00, 0x00, 0x00, 0x01, 0x00, 0x05, 0x07, 0x07, 0x00, 0x00,
        0x00, 0x00, 0x01, 0x00, 0x00,
    ];
    let stage_info = material_tev_stage_info(bytes, init_offset, stage_table_offset, stage)
        .unwrap_or(&DEFAULT_INFO);
    let order = read_material_tev_order(bytes, init_offset, order_table_offset, stage)
        .map(|order| J3dTevOrder {
            tex_coord: order.tex_coord_index.map(|value| value as u8),
            tex_map: order.tex_map_index.map(|value| value as u8),
            color_channel: order.color_chan,
        })
        .unwrap_or(J3dTevOrder {
            tex_coord: None,
            tex_map: None,
            color_channel: 0xFF,
        });
    let (raster_swap, texture_swap) = be_u16(bytes, init_offset + 0x104 + stage * 2, FORMAT)
        .ok()
        .filter(|index| *index != 0xFFFF)
        .and_then(|index| {
            let offset = swap_mode_table_offset?.checked_add(index as usize * 4)?;
            let info = checked_slice(FORMAT, bytes, offset, 4).ok()?;
            Some((info[0], info[1]))
        })
        .unwrap_or((0, 0));

    J3dTevStage {
        order,
        color_args: [stage_info[1], stage_info[2], stage_info[3], stage_info[4]],
        color_op: stage_info[5],
        color_bias: stage_info[6],
        color_scale: stage_info[7],
        color_clamp: stage_info[8],
        color_register: stage_info[9],
        alpha_args: [
            stage_info[0x0A],
            stage_info[0x0B],
            stage_info[0x0C],
            stage_info[0x0D],
        ],
        alpha_op: stage_info[0x0E],
        alpha_bias: stage_info[0x0F],
        alpha_scale: stage_info[0x10],
        alpha_clamp: stage_info[0x11],
        alpha_register: stage_info[0x12],
        konst_color: checked_slice(FORMAT, bytes, init_offset + 0x9C + stage, 1)
            .ok()
            .map(|value| value[0])
            .unwrap_or(0x0C),
        konst_alpha: checked_slice(FORMAT, bytes, init_offset + 0xAC + stage, 1)
            .ok()
            .map(|value| value[0])
            .unwrap_or(0x1C),
        raster_swap,
        texture_swap,
        indirect: indirect.tev_stages.get(stage).copied().unwrap_or_default(),
    }
}

fn read_material_swap_table(
    bytes: &[u8],
    init_offset: usize,
    table_offset: Option<usize>,
    slot: usize,
) -> [u8; 4] {
    let Ok(index) = be_u16(bytes, init_offset + 0x124 + slot * 2, FORMAT) else {
        return [0, 1, 2, 3];
    };
    if index == 0xFFFF {
        return [0, 1, 2, 3];
    }
    let Some(offset) = table_offset.and_then(|offset| offset.checked_add(index as usize * 4))
    else {
        return [0, 1, 2, 3];
    };
    let Ok(info) = checked_slice(FORMAT, bytes, offset, 4) else {
        return [0, 1, 2, 3];
    };
    [info[0], info[1], info[2], info[3]]
}

fn read_indexed_fog(bytes: &[u8], table_offset: Option<usize>, index: u16) -> Option<J3dFog> {
    if index == 0xFFFF {
        return None;
    }
    let offset = table_offset?.checked_add(index as usize * 0x2C)?;
    let info = checked_slice(FORMAT, bytes, offset, 0x2C).ok()?;
    let mut adjustment_table = [0; 10];
    for (slot, value) in adjustment_table.iter_mut().enumerate() {
        *value = be_u16(bytes, offset + 0x18 + slot * 2, FORMAT).ok()?;
    }
    Some(J3dFog {
        fog_type: info[0],
        adjustment_enabled: info[1],
        center: be_u16(bytes, offset + 0x02, FORMAT).ok()?,
        start_z: be_f32(bytes, offset + 0x04, FORMAT).ok()?,
        end_z: be_f32(bytes, offset + 0x08, FORMAT).ok()?,
        near_z: be_f32(bytes, offset + 0x0C, FORMAT).ok()?,
        far_z: be_f32(bytes, offset + 0x10, FORMAT).ok()?,
        color: [info[0x14], info[0x15], info[0x16], info[0x17]],
        adjustment_table,
    })
}

fn resolve_pe_state(
    mode: u8,
    full_pe_block: bool,
    alpha_compare: Option<J3dAlphaCompare>,
    blend_mode: Option<J3dBlendMode>,
    z_mode: Option<J3dZMode>,
    z_comp_loc: Option<u8>,
) -> (J3dAlphaCompare, J3dBlendMode, J3dZMode, u8) {
    let always = J3dAlphaCompare {
        comp0: 7,
        ref0: 0,
        op: 0,
        comp1: 7,
        ref1: 0,
    };
    let opaque_blend = J3dBlendMode {
        mode: 0,
        src_factor: 1,
        dst_factor: 0,
        logic_op: 3,
    };
    let alpha_blend = J3dBlendMode {
        mode: 1,
        src_factor: 4,
        dst_factor: 5,
        logic_op: 3,
    };
    let depth_write = J3dZMode {
        compare_enable: 1,
        func: 3,
        update_enable: 1,
    };

    let mode_state = if mode & 1 != 0 {
        (always, opaque_blend, depth_write, 1)
    } else if mode & 2 != 0 {
        (
            J3dAlphaCompare {
                comp0: 6,
                ref0: 0x80,
                op: 0,
                comp1: 3,
                ref1: 0xFF,
            },
            opaque_blend,
            depth_write,
            0,
        )
    } else if mode & 4 != 0 {
        (
            always,
            alpha_blend,
            J3dZMode {
                compare_enable: 1,
                func: 3,
                update_enable: 0,
            },
            1,
        )
    } else {
        (always, alpha_blend, depth_write, 1)
    };

    if full_pe_block {
        (
            alpha_compare.unwrap_or(mode_state.0),
            blend_mode.unwrap_or(mode_state.1),
            z_mode.unwrap_or(mode_state.2),
            z_comp_loc.unwrap_or(mode_state.3),
        )
    } else {
        mode_state
    }
}

fn preview_texture_is_base_color(format: u8) -> bool {
    matches!(
        format,
        GX_TF_RGB565 | GX_TF_RGB5A3 | GX_TF_RGBA8 | GX_TF_C4 | GX_TF_C8 | GX_TF_C14X2 | GX_TF_CMPR
    )
}

fn preview_texture_is_intensity(format: u8) -> bool {
    matches!(format, GX_TF_I4 | GX_TF_I8 | GX_TF_IA4 | GX_TF_IA8)
}

fn preview_color_is_useful(color: [u8; 4]) -> bool {
    color[3] > 12
        && !(color[0] > 242 && color[1] > 242 && color[2] > 242)
        && !(color[0] < 8 && color[1] < 8 && color[2] < 8)
}

fn preview_tint_color_is_useful(color: [u8; 4]) -> bool {
    color != [255, 255, 255, 255]
}

fn material_texture_index_for_slot(
    bytes: &[u8],
    init_offset: usize,
    tex_no_offset: Option<usize>,
    slot: usize,
) -> Option<usize> {
    if slot >= TEX_COORD_COUNT {
        return None;
    }
    let tex_no_index = be_u16(bytes, init_offset + 0x84 + slot * 2, FORMAT).ok()?;
    if tex_no_index == 0xFFFF {
        return None;
    }
    be_u16(bytes, tex_no_offset? + tex_no_index as usize * 2, FORMAT)
        .ok()
        .map(|value| value as usize)
}

fn material_generated_tex_coord_binding(
    bytes: &[u8],
    init_offset: usize,
    tex_coord_offset: Option<usize>,
    tex_mtx_offset: Option<usize>,
    generated_coord_index: usize,
) -> Option<TexCoordPreviewBinding> {
    if generated_coord_index >= TEX_COORD_COUNT {
        return None;
    }
    let info = material_tex_coord_info(bytes, init_offset, tex_coord_offset, generated_coord_index);
    let source = tex_gen_source_to_preview_source(info.gen_src)?;
    let tex_mtx = tex_mtx_slot_for_id(info.gen_mtx)
        .and_then(|slot| material_tex_mtx_for_slot(bytes, init_offset, tex_mtx_offset, slot));
    Some(TexCoordPreviewBinding { source, tex_mtx })
}

fn material_tex_coord_info(
    bytes: &[u8],
    init_offset: usize,
    tex_coord_offset: Option<usize>,
    generated_coord_index: usize,
) -> MaterialTexCoordInfo {
    let default = MaterialTexCoordInfo {
        gen_src: GX_TG_TEX0 + generated_coord_index as u8,
        gen_mtx: GX_IDENTITY,
    };
    let Ok(index) = be_u16(
        bytes,
        init_offset + 0x28 + generated_coord_index * 2,
        FORMAT,
    ) else {
        return default;
    };
    if index == 0xFFFF {
        return default;
    }
    let Some(offset) = tex_coord_offset
        .and_then(|offset| offset.checked_add(index as usize * J3D_TEX_COORD_INFO_SIZE))
    else {
        return default;
    };
    let Ok(info) = checked_slice(FORMAT, bytes, offset, J3D_TEX_COORD_INFO_SIZE) else {
        return default;
    };
    MaterialTexCoordInfo {
        gen_src: info[1],
        gen_mtx: info[2],
    }
}

fn tex_gen_source_to_preview_source(gen_src: u8) -> Option<TexCoordPreviewSource> {
    match gen_src {
        GX_TG_POS => Some(TexCoordPreviewSource::Position),
        GX_TG_NRM => Some(TexCoordPreviewSource::Normal),
        GX_TG_TEX0..=GX_TG_TEX7 => Some(TexCoordPreviewSource::Vertex(
            (gen_src - GX_TG_TEX0) as usize,
        )),
        _ => None,
    }
}

fn tex_mtx_slot_for_id(gen_mtx: u8) -> Option<usize> {
    if gen_mtx == GX_IDENTITY || gen_mtx < GX_TEXMTX0 {
        return None;
    }
    let offset = gen_mtx - GX_TEXMTX0;
    if !offset.is_multiple_of(3) {
        return None;
    }
    let slot = (offset / 3) as usize;
    (slot < TEX_COORD_COUNT).then_some(slot)
}

fn material_tex_mtx_for_slot(
    bytes: &[u8],
    init_offset: usize,
    tex_mtx_offset: Option<usize>,
    slot: usize,
) -> Option<TexMtx2d> {
    if slot >= TEX_COORD_COUNT {
        return None;
    }
    let index = be_u16(bytes, init_offset + 0x48 + slot * 2, FORMAT).ok()?;
    if index == 0xFFFF {
        return None;
    }
    let offset = tex_mtx_offset?.checked_add(index as usize * J3D_TEX_MTX_INFO_SIZE)?;
    read_tex_mtx_2d(bytes, offset).ok()
}

fn read_tex_mtx_2d(bytes: &[u8], offset: usize) -> Result<TexMtx2d> {
    checked_slice(FORMAT, bytes, offset, J3D_TEX_MTX_INFO_SIZE)?;
    let info = checked_slice(FORMAT, bytes, offset + 0x01, 1)?[0];
    let use_maya_format = (info & 0x80) != 0;
    let center = [
        be_f32(bytes, offset + 0x04, FORMAT)?,
        be_f32(bytes, offset + 0x08, FORMAT)?,
    ];
    let scale_x = be_f32(bytes, offset + 0x10, FORMAT)?;
    let scale_y = be_f32(bytes, offset + 0x14, FORMAT)?;
    let rotation = be_i16(bytes, offset + 0x18, FORMAT)?;
    let translate_x = be_f32(bytes, offset + 0x1C, FORMAT)?;
    let translate_y = be_f32(bytes, offset + 0x20, FORMAT)?;
    let radians = rotation as f32 * std::f32::consts::TAU / 65536.0;
    let (sin, cos) = radians.sin_cos();

    let rows = if use_maya_format {
        [
            [
                scale_x * cos,
                scale_y * sin,
                (translate_x - 0.5) * cos - sin * ((translate_y - 0.5) + scale_y) + 0.5,
            ],
            [
                -scale_x * sin,
                scale_y * cos,
                -(translate_x - 0.5) * sin - cos * ((translate_y - 0.5) + scale_y) + 0.5,
            ],
        ]
    } else {
        [
            [
                scale_x * cos,
                -scale_x * sin,
                (-scale_x * cos * center[0] + scale_x * sin * center[1]) + center[0] + translate_x,
            ],
            [
                scale_y * sin,
                scale_y * cos,
                (-scale_y * sin * center[0] - scale_y * cos * center[1]) + center[1] + translate_y,
            ],
        ]
    };

    Ok(TexMtx2d { rows })
}

fn material_tev_colors(
    bytes: &[u8],
    init_offset: usize,
    tev_color_offset: Option<usize>,
) -> [[i16; 4]; 4] {
    let mut colors = [[0, 0, 0, 0]; 4];
    let Some(table_offset) = tev_color_offset else {
        return colors;
    };

    for (slot, color) in colors.iter_mut().enumerate() {
        let Ok(index) = be_u16(bytes, init_offset + 0xDC + slot * 2, FORMAT) else {
            continue;
        };
        if index == 0xFFFF {
            continue;
        }
        let Some(offset) = table_offset.checked_add(index as usize * 8) else {
            continue;
        };
        if checked_slice(FORMAT, bytes, offset, 8).is_err() {
            continue;
        }
        *color = [
            be_i16(bytes, offset, FORMAT).unwrap_or(0),
            be_i16(bytes, offset + 2, FORMAT).unwrap_or(0),
            be_i16(bytes, offset + 4, FORMAT).unwrap_or(0),
            be_i16(bytes, offset + 6, FORMAT).unwrap_or(0),
        ];
    }

    colors
}

fn material_tev_k_colors(
    bytes: &[u8],
    init_offset: usize,
    tev_k_color_offset: Option<usize>,
) -> [[u8; 4]; 4] {
    let mut colors = [[255, 255, 255, 255]; 4];
    let Some(table_offset) = tev_k_color_offset else {
        return colors;
    };

    for (slot, color) in colors.iter_mut().enumerate() {
        let Ok(index) = be_u16(bytes, init_offset + 0x94 + slot * 2, FORMAT) else {
            continue;
        };
        if index == 0xFFFF {
            continue;
        }
        let Some(offset) = table_offset.checked_add(index as usize * 4) else {
            continue;
        };
        let Ok(raw) = checked_slice(FORMAT, bytes, offset, 4) else {
            continue;
        };
        *color = [raw[0], raw[1], raw[2], raw[3]];
    }

    colors
}

fn material_tev_stage_konst_color(
    bytes: &[u8],
    init_offset: usize,
    tev_stage_info_offset: Option<usize>,
    stage: usize,
    tev_k_colors: [[u8; 4]; 4],
) -> Option<[u8; 4]> {
    let color_args =
        material_tev_stage_color_args(bytes, init_offset, tev_stage_info_offset, stage)?;
    let alpha_args =
        material_tev_stage_alpha_args(bytes, init_offset, tev_stage_info_offset, stage);
    let mut tint = [255, 255, 255, 255];
    let mut uses_tint = false;

    if tev_args_use_konst_color(color_args) {
        let selector = *checked_slice(FORMAT, bytes, init_offset + 0x9C + stage, 1)
            .ok()?
            .first()?;
        let color = tev_konst_color_for_selector(selector, tev_k_colors)?;
        tint[0] = color[0];
        tint[1] = color[1];
        tint[2] = color[2];
        uses_tint = true;
    }
    if alpha_args.is_some_and(tev_alpha_args_use_konst_alpha) {
        let selector = *checked_slice(FORMAT, bytes, init_offset + 0xAC + stage, 1)
            .ok()?
            .first()?;
        tint[3] = tev_konst_alpha_for_selector(selector, tev_k_colors)?;
        uses_tint = true;
    }

    uses_tint.then_some(tint)
}

fn material_tev_stage_modulate_tint_color(
    bytes: &[u8],
    init_offset: usize,
    tev_stage_info_offset: Option<usize>,
    stage: usize,
    tev_k_colors: [[u8; 4]; 4],
) -> Option<[u8; 4]> {
    let color_args =
        material_tev_stage_color_args(bytes, init_offset, tev_stage_info_offset, stage)?;
    let alpha_args =
        material_tev_stage_alpha_args(bytes, init_offset, tev_stage_info_offset, stage);
    let mut tint = [255, 255, 255, 255];
    let mut uses_tint = false;

    if tev_args_are_texture_konst_modulate(color_args) {
        let selector = *checked_slice(FORMAT, bytes, init_offset + 0x9C + stage, 1)
            .ok()?
            .first()?;
        let color = tev_konst_color_for_selector(selector, tev_k_colors)?;
        tint[0] = color[0];
        tint[1] = color[1];
        tint[2] = color[2];
        uses_tint = true;
    }
    if alpha_args.is_some_and(tev_alpha_args_are_texture_konst_modulate) {
        let selector = *checked_slice(FORMAT, bytes, init_offset + 0xAC + stage, 1)
            .ok()?
            .first()?;
        tint[3] = tev_konst_alpha_for_selector(selector, tev_k_colors)?;
        uses_tint = true;
    }

    uses_tint.then_some(tint)
}

fn material_tev_stage_blend_tint_color(
    bytes: &[u8],
    init_offset: usize,
    tev_stage_info_offset: Option<usize>,
    stage: usize,
    tev_k_colors: [[u8; 4]; 4],
    material_color: Option<[u8; 4]>,
    raster_source: RasterColorSource,
) -> Option<[u8; 4]> {
    if raster_source != RasterColorSource::Material {
        return None;
    }
    let material_color = material_color?;
    let color_args =
        material_tev_stage_color_args(bytes, init_offset, tev_stage_info_offset, stage)?;
    if !tev_args_are_texture_raster_konst_blend(color_args) {
        return None;
    }

    let selector = *checked_slice(FORMAT, bytes, init_offset + 0x9C + stage, 1)
        .ok()?
        .first()?;
    let konst_color = tev_konst_color_for_selector(selector, tev_k_colors)?;
    Some([
        texture_raster_konst_blend_preview_channel(material_color[0], konst_color[0]),
        texture_raster_konst_blend_preview_channel(material_color[1], konst_color[1]),
        texture_raster_konst_blend_preview_channel(material_color[2], konst_color[2]),
        255,
    ])
}

fn texture_raster_konst_blend_preview_channel(material: u8, konst: u8) -> u8 {
    let material = material as u16;
    let konst = konst as u16;
    ((255 * (255 - konst) + material * konst) / 255) as u8
}

#[allow(clippy::too_many_arguments)]
fn material_tev_stage_preview_color(
    bytes: &[u8],
    init_offset: usize,
    tev_stage_info_offset: Option<usize>,
    stage: usize,
    tev_colors: [[i16; 4]; 4],
    tev_k_colors: [[u8; 4]; 4],
    previous_color: Option<[u8; 4]>,
    material_color: Option<[u8; 4]>,
    raster_source: RasterColorSource,
) -> Option<[u8; 4]> {
    let color_args =
        material_tev_stage_color_args(bytes, init_offset, tev_stage_info_offset, stage)?;
    if tev_color_args_are_texture_dependent(color_args) {
        return None;
    }

    let color_selector = checked_slice(FORMAT, bytes, init_offset + 0x9C + stage, 1)
        .ok()
        .and_then(|bytes| bytes.first().copied());
    let alpha_selector = checked_slice(FORMAT, bytes, init_offset + 0xAC + stage, 1)
        .ok()
        .and_then(|bytes| bytes.first().copied());
    let red = tev_blend_component(
        tev_color_arg_component(
            color_args.a,
            0,
            previous_color,
            tev_colors,
            tev_k_colors,
            color_selector,
            material_color,
            raster_source,
        )?,
        tev_color_arg_component(
            color_args.b,
            0,
            previous_color,
            tev_colors,
            tev_k_colors,
            color_selector,
            material_color,
            raster_source,
        )?,
        tev_color_arg_component(
            color_args.c,
            0,
            previous_color,
            tev_colors,
            tev_k_colors,
            color_selector,
            material_color,
            raster_source,
        )?,
        tev_color_arg_component(
            color_args.d,
            0,
            previous_color,
            tev_colors,
            tev_k_colors,
            color_selector,
            material_color,
            raster_source,
        )?,
    );
    let green = tev_blend_component(
        tev_color_arg_component(
            color_args.a,
            1,
            previous_color,
            tev_colors,
            tev_k_colors,
            color_selector,
            material_color,
            raster_source,
        )?,
        tev_color_arg_component(
            color_args.b,
            1,
            previous_color,
            tev_colors,
            tev_k_colors,
            color_selector,
            material_color,
            raster_source,
        )?,
        tev_color_arg_component(
            color_args.c,
            1,
            previous_color,
            tev_colors,
            tev_k_colors,
            color_selector,
            material_color,
            raster_source,
        )?,
        tev_color_arg_component(
            color_args.d,
            1,
            previous_color,
            tev_colors,
            tev_k_colors,
            color_selector,
            material_color,
            raster_source,
        )?,
    );
    let blue = tev_blend_component(
        tev_color_arg_component(
            color_args.a,
            2,
            previous_color,
            tev_colors,
            tev_k_colors,
            color_selector,
            material_color,
            raster_source,
        )?,
        tev_color_arg_component(
            color_args.b,
            2,
            previous_color,
            tev_colors,
            tev_k_colors,
            color_selector,
            material_color,
            raster_source,
        )?,
        tev_color_arg_component(
            color_args.c,
            2,
            previous_color,
            tev_colors,
            tev_k_colors,
            color_selector,
            material_color,
            raster_source,
        )?,
        tev_color_arg_component(
            color_args.d,
            2,
            previous_color,
            tev_colors,
            tev_k_colors,
            color_selector,
            material_color,
            raster_source,
        )?,
    );

    let alpha = material_tev_stage_alpha_args(bytes, init_offset, tev_stage_info_offset, stage)
        .and_then(|alpha_args| {
            if tev_alpha_args_are_texture_dependent(alpha_args) {
                return None;
            }
            Some(tev_blend_component(
                tev_alpha_arg_component(
                    alpha_args.a,
                    previous_color,
                    tev_colors,
                    tev_k_colors,
                    alpha_selector,
                    material_color,
                    raster_source,
                )?,
                tev_alpha_arg_component(
                    alpha_args.b,
                    previous_color,
                    tev_colors,
                    tev_k_colors,
                    alpha_selector,
                    material_color,
                    raster_source,
                )?,
                tev_alpha_arg_component(
                    alpha_args.c,
                    previous_color,
                    tev_colors,
                    tev_k_colors,
                    alpha_selector,
                    material_color,
                    raster_source,
                )?,
                tev_alpha_arg_component(
                    alpha_args.d,
                    previous_color,
                    tev_colors,
                    tev_k_colors,
                    alpha_selector,
                    material_color,
                    raster_source,
                )?,
            ))
        })
        .unwrap_or_else(|| material_color.map(|color| color[3]).unwrap_or(255));

    Some([red, green, blue, alpha])
}

#[allow(clippy::too_many_arguments)]
fn tev_color_arg_component(
    arg: u8,
    channel: usize,
    previous_color: Option<[u8; 4]>,
    tev_colors: [[i16; 4]; 4],
    tev_k_colors: [[u8; 4]; 4],
    selector: Option<u8>,
    material_color: Option<[u8; 4]>,
    raster_source: RasterColorSource,
) -> Option<i32> {
    match arg {
        GX_CC_CPREV => Some(previous_color.unwrap_or([0, 0, 0, 0])[channel] as i32),
        GX_CC_APREV => Some(previous_color.unwrap_or([0, 0, 0, 0])[3] as i32),
        GX_CC_C0 => Some(tev_s10_to_preview_component(tev_colors[0][channel])),
        GX_CC_A0 => Some(tev_s10_to_preview_component(tev_colors[0][3])),
        GX_CC_C1 => Some(tev_s10_to_preview_component(tev_colors[1][channel])),
        GX_CC_A1 => Some(tev_s10_to_preview_component(tev_colors[1][3])),
        GX_CC_C2 => Some(tev_s10_to_preview_component(tev_colors[2][channel])),
        GX_CC_A2 => Some(tev_s10_to_preview_component(tev_colors[2][3])),
        GX_CC_RASC => tev_raster_component(material_color, raster_source, channel),
        GX_CC_RASA => tev_raster_component(material_color, raster_source, 3),
        GX_CC_ONE => Some(255),
        GX_CC_HALF => Some(128),
        GX_CC_KONST => {
            let color = tev_konst_color_for_selector(selector?, tev_k_colors)?;
            Some(color[channel] as i32)
        }
        GX_CC_ZERO => Some(0),
        _ => None,
    }
}

fn tev_alpha_arg_component(
    arg: u8,
    previous_color: Option<[u8; 4]>,
    tev_colors: [[i16; 4]; 4],
    tev_k_colors: [[u8; 4]; 4],
    selector: Option<u8>,
    material_color: Option<[u8; 4]>,
    raster_source: RasterColorSource,
) -> Option<i32> {
    match arg {
        GX_CA_APREV => Some(previous_color.unwrap_or([0, 0, 0, 0])[3] as i32),
        GX_CA_A0 => Some(tev_s10_to_preview_component(tev_colors[0][3])),
        GX_CA_A1 => Some(tev_s10_to_preview_component(tev_colors[1][3])),
        GX_CA_A2 => Some(tev_s10_to_preview_component(tev_colors[2][3])),
        GX_CA_RASA => tev_raster_component(material_color, raster_source, 3),
        GX_CA_KONST => Some(tev_konst_alpha_for_selector(selector?, tev_k_colors)? as i32),
        GX_CA_ZERO => Some(0),
        _ => None,
    }
}

fn tev_raster_component(
    material_color: Option<[u8; 4]>,
    raster_source: RasterColorSource,
    channel: usize,
) -> Option<i32> {
    match raster_source {
        RasterColorSource::Material => material_color.map(|color| color[channel] as i32),
        RasterColorSource::Vertex | RasterColorSource::Disabled => None,
    }
}

fn tev_s10_to_preview_component(value: i16) -> i32 {
    value.clamp(0, 255) as i32
}

fn tev_blend_component(a: i32, b: i32, c: i32, d: i32) -> u8 {
    let c = c.clamp(0, 255);
    (d + ((a * (255 - c) + b * c + 127) / 255)).clamp(0, 255) as u8
}

fn tev_konst_color_for_selector(selector: u8, tev_k_colors: [[u8; 4]; 4]) -> Option<[u8; 4]> {
    match selector {
        0x00 => Some([255, 255, 255, 255]),
        0x01 => Some([224, 224, 224, 255]),
        0x02 => Some([192, 192, 192, 255]),
        0x03 => Some([160, 160, 160, 255]),
        0x04 => Some([128, 128, 128, 255]),
        0x05 => Some([96, 96, 96, 255]),
        0x06 => Some([64, 64, 64, 255]),
        0x07 => Some([32, 32, 32, 255]),
        0x0C..=0x0F => Some(tev_k_colors[(selector - 0x0C) as usize]),
        0x10..=0x13 => Some(konst_channel_color(
            tev_k_colors[(selector - 0x10) as usize],
            0,
        )),
        0x14..=0x17 => Some(konst_channel_color(
            tev_k_colors[(selector - 0x14) as usize],
            1,
        )),
        0x18..=0x1B => Some(konst_channel_color(
            tev_k_colors[(selector - 0x18) as usize],
            2,
        )),
        0x1C..=0x1F => Some(konst_channel_color(
            tev_k_colors[(selector - 0x1C) as usize],
            3,
        )),
        _ => None,
    }
}

fn konst_channel_color(color: [u8; 4], channel: usize) -> [u8; 4] {
    let value = color[channel];
    [value, value, value, color[3]]
}

fn tev_konst_alpha_for_selector(selector: u8, tev_k_colors: [[u8; 4]; 4]) -> Option<u8> {
    match selector {
        0x00 => Some(255),
        0x01 => Some(224),
        0x02 => Some(192),
        0x03 => Some(160),
        0x04 => Some(128),
        0x05 => Some(96),
        0x06 => Some(64),
        0x07 => Some(32),
        0x10..=0x13 => Some(tev_k_colors[(selector - 0x10) as usize][0]),
        0x14..=0x17 => Some(tev_k_colors[(selector - 0x14) as usize][1]),
        0x18..=0x1B => Some(tev_k_colors[(selector - 0x18) as usize][2]),
        0x1C..=0x1F => Some(tev_k_colors[(selector - 0x1C) as usize][3]),
        _ => None,
    }
}

fn read_material_tev_order(
    bytes: &[u8],
    init_offset: usize,
    tev_order_offset: Option<usize>,
    stage: usize,
) -> Option<MaterialTevOrder> {
    let order_index = be_u16(bytes, init_offset + 0xBC + stage * 2, FORMAT).ok()?;
    if order_index == 0xFFFF {
        return None;
    }
    let offset = tev_order_offset?.checked_add(order_index as usize * J3D_TEV_ORDER_INFO_SIZE)?;
    let order = checked_slice(FORMAT, bytes, offset, J3D_TEV_ORDER_INFO_SIZE).ok()?;
    let tex_coord = order[0];
    let tex_map = order[1];
    let color_chan = order[2];
    let tex_coord_index = (tex_coord < TEX_COORD_COUNT as u8).then_some(tex_coord as usize);
    let tex_map_index = (tex_map < TEX_COORD_COUNT as u8).then_some(tex_map as usize);
    Some(MaterialTevOrder {
        tex_coord_index,
        tex_map_index,
        color_chan,
    })
}

fn material_tev_stage_count(
    bytes: &[u8],
    init_offset: usize,
    tev_stage_num_offset: Option<usize>,
) -> Option<usize> {
    let stage_num_index = *checked_slice(FORMAT, bytes, init_offset + 0x04, 1)
        .ok()?
        .first()?;
    if stage_num_index == 0xFF {
        return None;
    }
    checked_slice(
        FORMAT,
        bytes,
        tev_stage_num_offset? + stage_num_index as usize,
        1,
    )
    .ok()?
    .first()
    .map(|value| *value as usize)
}

fn material_color_channel_source(
    bytes: &[u8],
    init_offset: usize,
    color_chan_info_offset: Option<usize>,
    color_chan: u8,
) -> RasterColorSource {
    if color_chan == 0xFF {
        return RasterColorSource::Disabled;
    }
    let channel_slot = match color_chan {
        0 | 4 => 0,
        1 | 5 => 2,
        2 => 1,
        3 => 3,
        _ => return RasterColorSource::Disabled,
    };
    let Ok(color_chan_index) = be_u16(bytes, init_offset + 0x0C + channel_slot * 2, FORMAT) else {
        return RasterColorSource::Disabled;
    };
    if color_chan_index == 0xFFFF {
        return RasterColorSource::Disabled;
    }
    let Some(offset) =
        color_chan_info_offset.and_then(|offset| offset.checked_add(color_chan_index as usize * 8))
    else {
        return RasterColorSource::Disabled;
    };
    let Ok(info) = checked_slice(FORMAT, bytes, offset, 8) else {
        return RasterColorSource::Disabled;
    };

    if info[1] == 1 {
        RasterColorSource::Vertex
    } else {
        RasterColorSource::Material
    }
}

fn material_tev_stage_texture_combine(
    bytes: &[u8],
    init_offset: usize,
    tev_stage_info_offset: Option<usize>,
    stage: usize,
    raster_source: RasterColorSource,
) -> Option<J3dPreviewCombineMode> {
    let args = material_tev_stage_color_args(bytes, init_offset, tev_stage_info_offset, stage)?;
    if !tev_args_use_texture_color(args) {
        return None;
    }
    if tev_args_are_texture_raster_modulate(args) {
        return Some(match raster_source {
            RasterColorSource::Vertex => J3dPreviewCombineMode::TextureModulateVertex,
            RasterColorSource::Material => J3dPreviewCombineMode::TextureModulateMaterial,
            RasterColorSource::Disabled => J3dPreviewCombineMode::TextureOnly,
        });
    }
    if tev_args_are_texture_konst_modulate(args) {
        return Some(J3dPreviewCombineMode::TextureModulateMaterial);
    }
    if tev_args_are_texture_raster_konst_blend(args) {
        return Some(match raster_source {
            RasterColorSource::Vertex => J3dPreviewCombineMode::TextureModulateVertex,
            RasterColorSource::Material => J3dPreviewCombineMode::TextureModulateMaterial,
            RasterColorSource::Disabled => J3dPreviewCombineMode::TextureOnly,
        });
    }

    Some(J3dPreviewCombineMode::TextureOnly)
}

fn material_tev_stage_uses_previous_texture_mask(
    bytes: &[u8],
    init_offset: usize,
    tev_stage_info_offset: Option<usize>,
    stage: usize,
) -> bool {
    material_tev_stage_color_args(bytes, init_offset, tev_stage_info_offset, stage)
        .is_some_and(tev_args_are_previous_texture_modulate)
}

fn material_tev_stage_raster_combine(
    bytes: &[u8],
    init_offset: usize,
    tev_stage_info_offset: Option<usize>,
    stage: usize,
    raster_source: RasterColorSource,
) -> Option<J3dPreviewCombineMode> {
    let args = material_tev_stage_color_args(bytes, init_offset, tev_stage_info_offset, stage)?;
    if !tev_args_are_raster_pass(args) {
        return None;
    }

    Some(match raster_source {
        RasterColorSource::Vertex => J3dPreviewCombineMode::VertexOnly,
        RasterColorSource::Material | RasterColorSource::Disabled => {
            J3dPreviewCombineMode::MaterialOnly
        }
    })
}

fn material_tev_stage_color_args(
    bytes: &[u8],
    init_offset: usize,
    tev_stage_info_offset: Option<usize>,
    stage: usize,
) -> Option<TevColorArgs> {
    let info = material_tev_stage_info(bytes, init_offset, tev_stage_info_offset, stage)?;

    Some(TevColorArgs {
        a: info[1],
        b: info[2],
        c: info[3],
        d: info[4],
    })
}

fn material_tev_stage_alpha_args(
    bytes: &[u8],
    init_offset: usize,
    tev_stage_info_offset: Option<usize>,
    stage: usize,
) -> Option<TevAlphaArgs> {
    let info = material_tev_stage_info(bytes, init_offset, tev_stage_info_offset, stage)?;
    Some(TevAlphaArgs {
        a: info[0x0A],
        b: info[0x0B],
        c: info[0x0C],
        d: info[0x0D],
    })
}

fn tev_args_use_texture_color(args: TevColorArgs) -> bool {
    [args.a, args.b, args.c, args.d]
        .iter()
        .any(|arg| tev_arg_is_texture_color(*arg))
}

fn tev_args_use_konst_color(args: TevColorArgs) -> bool {
    [args.a, args.b, args.c, args.d].contains(&GX_CC_KONST)
}

fn tev_color_args_are_texture_dependent(args: TevColorArgs) -> bool {
    [args.a, args.b, args.c, args.d]
        .iter()
        .any(|arg| tev_arg_needs_texture_sample(*arg))
}

fn tev_alpha_args_use_konst_alpha(args: TevAlphaArgs) -> bool {
    [args.a, args.b, args.c, args.d].contains(&GX_CA_KONST)
}

fn tev_alpha_args_are_texture_dependent(args: TevAlphaArgs) -> bool {
    [args.a, args.b, args.c, args.d].contains(&GX_CA_TEXA)
}

fn tev_alpha_args_are_texture_konst_modulate(args: TevAlphaArgs) -> bool {
    args.a == GX_CA_ZERO
        && args.d == GX_CA_ZERO
        && ((args.b == GX_CA_TEXA && args.c == GX_CA_KONST)
            || (args.b == GX_CA_KONST && args.c == GX_CA_TEXA))
}

fn tev_args_are_texture_raster_modulate(args: TevColorArgs) -> bool {
    args.a == GX_CC_ZERO
        && args.d == GX_CC_ZERO
        && ((tev_arg_is_texture_color(args.b) && args.c == GX_CC_RASC)
            || (args.b == GX_CC_RASC && tev_arg_is_texture_color(args.c)))
}

fn tev_args_are_texture_konst_modulate(args: TevColorArgs) -> bool {
    args.a == GX_CC_ZERO
        && args.d == GX_CC_ZERO
        && ((tev_arg_is_texture_color(args.b) && args.c == GX_CC_KONST)
            || (args.b == GX_CC_KONST && tev_arg_is_texture_color(args.c)))
}

fn tev_args_are_texture_raster_konst_blend(args: TevColorArgs) -> bool {
    tev_arg_is_texture_color(args.a)
        && args.b == GX_CC_RASC
        && args.c == GX_CC_KONST
        && args.d == GX_CC_C0
}

fn tev_args_are_previous_texture_modulate(args: TevColorArgs) -> bool {
    args.a == GX_CC_ZERO
        && args.d == GX_CC_ZERO
        && ((args.b == GX_CC_CPREV && tev_arg_is_texture_color(args.c))
            || (tev_arg_is_texture_color(args.b) && args.c == GX_CC_CPREV))
}

fn tev_args_are_raster_pass(args: TevColorArgs) -> bool {
    (args.a == GX_CC_RASC && args.b == GX_CC_ZERO && args.c == GX_CC_ZERO && args.d == GX_CC_ZERO)
        || (args.a == GX_CC_ZERO
            && args.b == GX_CC_ZERO
            && args.c == GX_CC_ZERO
            && args.d == GX_CC_RASC)
}

fn tev_arg_is_texture_color(arg: u8) -> bool {
    arg == GX_CC_TEXC || (GX_CC_TEXRRR..=GX_CC_TEXBBB).contains(&arg)
}

fn tev_arg_needs_texture_sample(arg: u8) -> bool {
    tev_arg_is_texture_color(arg) || arg == GX_CC_TEXA
}

fn material_tev_stage_info(
    bytes: &[u8],
    init_offset: usize,
    tev_stage_info_offset: Option<usize>,
    stage: usize,
) -> Option<&[u8]> {
    let stage_index = be_u16(bytes, init_offset + 0xE4 + stage * 2, FORMAT).ok()?;
    if stage_index == 0xFFFF {
        return None;
    }
    let offset =
        tev_stage_info_offset?.checked_add(stage_index as usize * J3D_TEV_STAGE_INFO_SIZE)?;
    checked_slice(FORMAT, bytes, offset, J3D_TEV_STAGE_INFO_SIZE).ok()
}

impl PreserveBytes for J3dFile {
    fn source_bytes(&self) -> &[u8] {
        self.bytes.source_bytes()
    }
}

fn identity_mtx34() -> Mtx34 {
    [
        [1.0, 0.0, 0.0, 0.0],
        [0.0, 1.0, 0.0, 0.0],
        [0.0, 0.0, 1.0, 0.0],
    ]
}

fn read_mtx34(bytes: &[u8], offset: usize) -> Result<Mtx34> {
    checked_slice(FORMAT, bytes, offset, 0x30)?;
    let mut matrix = [[0.0; 4]; 3];
    for (row, values) in matrix.iter_mut().enumerate() {
        for (column, value) in values.iter_mut().enumerate() {
            *value = be_f32(bytes, offset + (row * 4 + column) * 4, FORMAT)?;
        }
    }
    Ok(matrix)
}

fn add_weighted_mtx34(target: &mut Mtx34, matrix: Mtx34, weight: f32) {
    for row in 0..3 {
        for column in 0..4 {
            target[row][column] += matrix[row][column] * weight;
        }
    }
}

fn read_joint_preview_transform(bytes: &[u8], offset: usize) -> Result<JointPreviewTransform> {
    Ok(JointPreviewTransform {
        scale_compensate: checked_slice(FORMAT, bytes, offset + 0x02, 1)?[0] != 0,
        scale: [
            be_f32(bytes, offset + 0x04, FORMAT)?,
            be_f32(bytes, offset + 0x08, FORMAT)?,
            be_f32(bytes, offset + 0x0C, FORMAT)?,
        ],
        rotation: [
            be_i16(bytes, offset + 0x10, FORMAT)?,
            be_i16(bytes, offset + 0x12, FORMAT)?,
            be_i16(bytes, offset + 0x14, FORMAT)?,
        ],
        translation: [
            be_f32(bytes, offset + 0x18, FORMAT)?,
            be_f32(bytes, offset + 0x1C, FORMAT)?,
            be_f32(bytes, offset + 0x20, FORMAT)?,
        ],
    })
}

fn joint_transform_mtx(transform: JointPreviewTransform) -> Mtx34 {
    let mut mtx = translate_rotate_mtx(
        transform.rotation,
        transform.translation[0],
        transform.translation[1],
        transform.translation[2],
    );
    for row in &mut mtx {
        row[0] *= transform.scale[0];
        row[1] *= transform.scale[1];
        row[2] *= transform.scale[2];
    }
    mtx
}

fn degrees_to_j3d_angle(degrees: f32) -> i16 {
    if !degrees.is_finite() {
        return 0;
    }
    (degrees * (32768.0 / 180.0)).round() as i32 as i16
}

fn basic_joint_matrices(
    transforms: &[JointPreviewTransform],
    parents: &[Option<usize>],
) -> Vec<Mtx34> {
    let mut world = vec![identity_mtx34(); transforms.len()];
    for (joint, transform) in transforms.iter().copied().enumerate() {
        let local = joint_transform_mtx(transform);
        world[joint] = parent_matrix(&world, parents, joint)
            .map(|parent| concat_mtx34(parent, local))
            .unwrap_or(local);
    }
    world
}

fn softimage_joint_matrices(
    transforms: &[JointPreviewTransform],
    parents: &[Option<usize>],
) -> Vec<Mtx34> {
    let mut animation = vec![identity_mtx34(); transforms.len()];
    let mut current = vec![identity_mtx34(); transforms.len()];
    let mut cumulative_scale = vec![[1.0; 3]; transforms.len()];

    for (joint, transform) in transforms.iter().copied().enumerate() {
        let parent = parents.get(joint).copied().flatten();
        let parent_mtx = parent
            .and_then(|index| current.get(index).copied())
            .unwrap_or_else(identity_mtx34);
        let parent_scale = parent
            .and_then(|index| cumulative_scale.get(index).copied())
            .unwrap_or([1.0; 3]);
        let local = translate_rotate_mtx(
            transform.rotation,
            transform.translation[0] * parent_scale[0],
            transform.translation[1] * parent_scale[1],
            transform.translation[2] * parent_scale[2],
        );
        current[joint] = concat_mtx34(parent_mtx, local);
        cumulative_scale[joint] = [
            parent_scale[0] * transform.scale[0],
            parent_scale[1] * transform.scale[1],
            parent_scale[2] * transform.scale[2],
        ];
        animation[joint] = scale_mtx34_columns(current[joint], cumulative_scale[joint]);
    }

    animation
}

fn maya_joint_matrices(
    transforms: &[JointPreviewTransform],
    parents: &[Option<usize>],
) -> Vec<Mtx34> {
    let mut world = vec![identity_mtx34(); transforms.len()];
    for (joint, transform) in transforms.iter().copied().enumerate() {
        let parent = parents.get(joint).copied().flatten();
        let mut local = joint_transform_mtx(transform);
        if transform.scale_compensate {
            let parent_scale = parent
                .and_then(|index| transforms.get(index).map(|value| value.scale))
                .unwrap_or([1.0; 3]);
            for row in 0..3 {
                let inverse = if parent_scale[row].abs() > f32::EPSILON {
                    parent_scale[row].recip()
                } else {
                    1.0
                };
                for value in local[row].iter_mut().take(3) {
                    *value *= inverse;
                }
            }
        }
        world[joint] = parent
            .and_then(|index| world.get(index).copied())
            .map(|parent_mtx| concat_mtx34(parent_mtx, local))
            .unwrap_or(local);
    }
    world
}

fn parent_matrix(matrices: &[Mtx34], parents: &[Option<usize>], joint: usize) -> Option<Mtx34> {
    parents
        .get(joint)
        .copied()
        .flatten()
        .and_then(|parent| (parent < joint).then(|| matrices[parent]))
}

fn scale_mtx34_columns(mut matrix: Mtx34, scale: [f32; 3]) -> Mtx34 {
    for row in &mut matrix {
        row[0] *= scale[0];
        row[1] *= scale[1];
        row[2] *= scale[2];
    }
    matrix
}

fn translate_rotate_mtx(rotation: [i16; 3], tx: f32, ty: f32, tz: f32) -> Mtx34 {
    let (sx, cx) = jma_sin_cos(rotation[0]);
    let (sy, cy) = jma_sin_cos(rotation[1]);
    let (sz, cz) = jma_sin_cos(rotation[2]);

    let cxsz = cx * sz;
    let sxcz = sx * cz;
    let sxsz = sx * sz;
    let cxcz = cx * cz;

    [
        [cz * cy, sxcz * sy - cxsz, cxcz * sy + sxsz, tx],
        [sz * cy, sxsz * sy + cxcz, cxsz * sy - sxcz, ty],
        [-sy, cy * sx, cy * cx, tz],
    ]
}

fn jma_sin_cos(angle: i16) -> (f32, f32) {
    let radians = angle as f32 * std::f32::consts::PI / 32768.0;
    radians.sin_cos()
}

fn concat_mtx34(a: Mtx34, b: Mtx34) -> Mtx34 {
    let mut out = [[0.0; 4]; 3];
    for row in 0..3 {
        for col in 0..3 {
            out[row][col] = a[row][0] * b[0][col] + a[row][1] * b[1][col] + a[row][2] * b[2][col];
        }
        out[row][3] = a[row][0] * b[0][3] + a[row][1] * b[1][3] + a[row][2] * b[2][3] + a[row][3];
    }
    out
}

fn transform_mtx34_point(mtx: Mtx34, point: [f32; 3]) -> [f32; 3] {
    [
        mtx[0][0] * point[0] + mtx[0][1] * point[1] + mtx[0][2] * point[2] + mtx[0][3],
        mtx[1][0] * point[0] + mtx[1][1] * point[1] + mtx[1][2] * point[2] + mtx[1][3],
        mtx[2][0] * point[0] + mtx[2][1] * point[1] + mtx[2][2] * point[2] + mtx[2][3],
    ]
}

fn normalize_vec3(vector: [f32; 3]) -> Option<[f32; 3]> {
    let len_sq = vector[0] * vector[0] + vector[1] * vector[1] + vector[2] * vector[2];
    if !len_sq.is_finite() || len_sq <= f32::EPSILON {
        return None;
    }
    let inv_len = 1.0 / len_sq.sqrt();
    Some([
        vector[0] * inv_len,
        vector[1] * inv_len,
        vector[2] * inv_len,
    ])
}

fn billboard_for_triangle(
    vertices: [[f32; 3]; 3],
    normals: Option<[[f32; 3]; 3]>,
    matrix: Mtx34,
    shape_mtx_type: u8,
) -> Option<J3dBillboard> {
    let center = [matrix[0][3], matrix[1][3], matrix[2][3]];
    let axes = [
        normalize_vec3([matrix[0][0], matrix[1][0], matrix[2][0]])?,
        normalize_vec3([matrix[0][1], matrix[1][1], matrix[2][1]])?,
        normalize_vec3([matrix[0][2], matrix[1][2], matrix[2][2]])?,
    ];
    let offsets = vertices.map(|vertex| {
        let relative = [
            vertex[0] - center[0],
            vertex[1] - center[1],
            vertex[2] - center[2],
        ];
        axes.map(|axis| dot_vec3(relative, axis))
    });
    let normals =
        normals.map(|normals| normals.map(|normal| axes.map(|axis| dot_vec3(normal, axis))));
    Some(J3dBillboard {
        mode: if shape_mtx_type == 0x02 {
            J3dBillboardMode::YAxis
        } else {
            J3dBillboardMode::Full
        },
        center,
        axes,
        offsets,
        normals,
    })
}

fn dot_vec3(a: [f32; 3], b: [f32; 3]) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

fn resolve_shape_matrix_palette(raw: &[u16], palette: &mut Vec<u16>) -> Vec<u16> {
    if palette.len() < raw.len() {
        palette.resize(raw.len(), 0xFFFF);
    }
    for (slot, matrix) in raw.iter().copied().enumerate() {
        if matrix != 0xFFFF {
            palette[slot] = matrix;
        }
    }
    palette[..raw.len()].to_vec()
}

fn shape_draw_matrix_index(matrix_slot: Option<u16>, group_matrices: &[u16]) -> Option<u16> {
    let raw_slot = matrix_slot.unwrap_or(0) as usize;
    let slot = if raw_slot.is_multiple_of(3) {
        raw_slot / 3
    } else {
        raw_slot
    };
    group_matrices
        .get(slot)
        .copied()
        .or_else(|| group_matrices.first().copied())
        .filter(|draw_index| *draw_index != 0xFFFF)
}

fn transform_position_for_draw_matrix(
    position: [f32; 3],
    draw_index: Option<u16>,
    draw_matrices: &[Option<Mtx34>],
) -> [f32; 3] {
    draw_index
        .and_then(|draw_index| draw_matrices.get(draw_index as usize))
        .copied()
        .flatten()
        .map(|matrix| transform_mtx34_point(matrix, position))
        .unwrap_or(position)
}

fn transform_normal_for_draw_matrix(
    normal: [f32; 3],
    draw_index: Option<u16>,
    draw_matrices: &[Option<Mtx34>],
) -> [f32; 3] {
    draw_index
        .and_then(|draw_index| draw_matrices.get(draw_index as usize))
        .copied()
        .flatten()
        .and_then(|matrix| transform_mtx34_normal(matrix, normal))
        .unwrap_or_else(|| normalize_vec3(normal).unwrap_or(normal))
}

fn transform_mtx34_normal(matrix: Mtx34, normal: [f32; 3]) -> Option<[f32; 3]> {
    let [a, b, c] = [matrix[0][0], matrix[0][1], matrix[0][2]];
    let [d, e, f] = [matrix[1][0], matrix[1][1], matrix[1][2]];
    let [g, h, i] = [matrix[2][0], matrix[2][1], matrix[2][2]];
    let cofactor = [
        [e * i - f * h, f * g - d * i, d * h - e * g],
        [c * h - b * i, a * i - c * g, b * g - a * h],
        [b * f - c * e, c * d - a * f, a * e - b * d],
    ];
    let transformed = [
        cofactor[0][0] * normal[0] + cofactor[0][1] * normal[1] + cofactor[0][2] * normal[2],
        cofactor[1][0] * normal[0] + cofactor[1][1] * normal[1] + cofactor[1][2] * normal[2],
        cofactor[2][0] * normal[0] + cofactor[2][1] * normal[1] + cofactor[2][2] * normal[2],
    ];
    normalize_vec3(transformed)
}

fn bounds_for(points: &[[f32; 3]]) -> ([f32; 3], [f32; 3]) {
    let mut min = points[0];
    let mut max = points[0];
    for point in points.iter().skip(1) {
        for axis in 0..3 {
            min[axis] = min[axis].min(point[axis]);
            max[axis] = max[axis].max(point[axis]);
        }
    }

    (min, max)
}

fn position_format_from(formats: &[AttributeFormat]) -> Result<PositionFormat> {
    let format = formats
        .iter()
        .find(|format| format.attr == GX_VA_POS)
        .ok_or_else(|| FormatError::Unsupported {
            format: FORMAT,
            message: "missing GX_VA_POS format entry".to_string(),
        })?;

    if format.component_type == GX_F32 || format.component_type == GX_S16 {
        Ok(PositionFormat {
            component_type: format.component_type,
            frac: format.frac,
        })
    } else {
        Err(FormatError::Unsupported {
            format: FORMAT,
            message: format!(
                "unsupported position component type {}",
                format.component_type
            ),
        })
    }
}

fn relative_offset(bytes: &[u8], base: usize, field_offset: usize) -> Result<usize> {
    base.checked_add(be_u32(bytes, base + field_offset, FORMAT)? as usize)
        .ok_or_else(|| invalid_offset(base, bytes.len()))
}

fn optional_relative_offset(bytes: &[u8], base: usize, field_offset: usize) -> Option<usize> {
    let relative = be_u32(bytes, base + field_offset, FORMAT).ok()? as usize;
    if relative == 0 {
        return None;
    }
    base.checked_add(relative)
        .filter(|offset| *offset < bytes.len())
}

fn section_relative_offset(bytes: &[u8], base: usize, field_offset: usize) -> Result<usize> {
    let relative = be_u32(bytes, base + field_offset, FORMAT)? as usize;
    if relative == 0 {
        return Err(FormatError::Unsupported {
            format: FORMAT,
            message: format!("section relative offset at 0x{field_offset:X} is null"),
        });
    }
    base.checked_add(relative)
        .ok_or_else(|| invalid_offset(base, bytes.len()))
}

fn vertex_arrays(
    bytes: &[u8],
    vtx_offset: usize,
    attr_formats: &[AttributeFormat],
) -> VertexArrays {
    let normal_offset = section_relative_offset(bytes, vtx_offset, 0x10).ok();
    let normal_format = normal_format_from(attr_formats).ok();
    let mut color_offsets = [None; 2];
    let mut color_formats = [None; 2];
    for index in 0..2 {
        color_offsets[index] = section_relative_offset(bytes, vtx_offset, 0x18 + index * 4).ok();
        color_formats[index] = color_format_from(attr_formats, GX_VA_CLR0 + index as u32).ok();
    }

    let mut tex_offsets = [None; TEX_COORD_COUNT];
    let mut tex_formats = [None; TEX_COORD_COUNT];
    for index in 0..TEX_COORD_COUNT {
        tex_offsets[index] = section_relative_offset(bytes, vtx_offset, 0x20 + index * 4).ok();
        tex_formats[index] = tex_coord_format_from(attr_formats, GX_VA_TEX0 + index as u32).ok();
    }

    VertexArrays {
        normal_offset,
        normal_format,
        color_offsets,
        color_formats,
        tex_offsets,
        tex_formats,
    }
}

fn normal_format_from(formats: &[AttributeFormat]) -> Result<NormalFormat> {
    let format = formats
        .iter()
        .find(|format| format.attr == GX_VA_NRM)
        .ok_or_else(|| FormatError::Unsupported {
            format: FORMAT,
            message: "missing normal format".to_string(),
        })?;
    match format.component_type {
        GX_S8 | GX_S16 | GX_F32 => Ok(NormalFormat {
            component_type: format.component_type,
            frac: format.frac,
            components: normal_component_count(format.cnt),
        }),
        _ => Err(FormatError::Unsupported {
            format: FORMAT,
            message: format!(
                "unsupported normal component type {}",
                format.component_type
            ),
        }),
    }
}

fn color_format_from(formats: &[AttributeFormat], attr: u32) -> Result<ColorFormat> {
    let format = formats
        .iter()
        .find(|format| format.attr == attr)
        .ok_or_else(|| FormatError::Unsupported {
            format: FORMAT,
            message: format!("missing color format for attr {attr}"),
        })?;
    match format.component_type {
        GX_RGB565 | GX_RGB8 | GX_RGBX8 | GX_RGBA4 | GX_RGBA6 | GX_RGBA8 => Ok(ColorFormat {
            component_type: format.component_type,
        }),
        _ => Err(FormatError::Unsupported {
            format: FORMAT,
            message: format!("unsupported color component type {}", format.component_type),
        }),
    }
}

fn tex_coord_format_from(formats: &[AttributeFormat], attr: u32) -> Result<TexCoordFormat> {
    let format = formats
        .iter()
        .find(|format| format.attr == attr)
        .ok_or_else(|| FormatError::Unsupported {
            format: FORMAT,
            message: format!("missing texture coordinate format for attr {attr}"),
        })?;
    let components = if format.cnt == GX_TEX_ST { 2 } else { 1 };
    match format.component_type {
        GX_U8 | GX_S8 | GX_U16 | GX_S16 | GX_F32 => Ok(TexCoordFormat {
            component_type: format.component_type,
            frac: format.frac,
            components,
        }),
        _ => Err(FormatError::Unsupported {
            format: FORMAT,
            message: format!(
                "unsupported texture coordinate component type {}",
                format.component_type
            ),
        }),
    }
}

#[allow(clippy::too_many_arguments)]
fn decode_display_list(
    bytes: &[u8],
    source_bytes: &[u8],
    positions: &[[f32; 3]],
    descs: &[VertexDesc],
    attr_formats: &[AttributeFormat],
    position_format: PositionFormat,
    vertex_arrays: VertexArrays,
    group_matrices: &[u16],
    draw_matrices: &[Option<Mtx34>],
    color: Option<[u8; 4]>,
    render_state: J3dMaterialRenderState,
    texture_binding: Option<MaterialPreviewBinding>,
) -> Result<Vec<J3dTriangle>> {
    let mut offset = 0usize;
    let mut triangles = Vec::new();

    while offset < bytes.len() {
        let command = bytes[offset];
        offset += 1;
        if command == 0 {
            break;
        }
        let opcode = gx_primitive_opcode(command)?;
        if offset + 2 > bytes.len() {
            return Err(FormatError::InvalidOffset {
                format: FORMAT,
                offset,
                len: bytes.len(),
            });
        }

        let vertex_count = u16::from_be_bytes([bytes[offset], bytes[offset + 1]]) as usize;
        offset += 2;
        let mut primitive = Vec::with_capacity(vertex_count);
        for _ in 0..vertex_count {
            primitive.push(read_primitive_vertex(
                bytes,
                source_bytes,
                &mut offset,
                positions,
                descs,
                attr_formats,
                position_format,
                vertex_arrays,
                group_matrices,
                draw_matrices,
            )?);
        }

        match opcode {
            GX_DRAW_TRIANGLES => {
                for chunk in primitive.chunks_exact(3) {
                    push_triangle(
                        &mut triangles,
                        chunk[0],
                        chunk[1],
                        chunk[2],
                        color,
                        render_state,
                        texture_binding,
                    );
                }
            }
            GX_DRAW_TRIANGLE_STRIP => {
                for i in 2..primitive.len() {
                    if i % 2 == 0 {
                        push_triangle(
                            &mut triangles,
                            primitive[i - 2],
                            primitive[i - 1],
                            primitive[i],
                            color,
                            render_state,
                            texture_binding,
                        );
                    } else {
                        push_triangle(
                            &mut triangles,
                            primitive[i - 1],
                            primitive[i - 2],
                            primitive[i],
                            color,
                            render_state,
                            texture_binding,
                        );
                    }
                }
            }
            GX_DRAW_TRIANGLE_FAN => {
                for i in 2..primitive.len() {
                    push_triangle(
                        &mut triangles,
                        primitive[0],
                        primitive[i - 1],
                        primitive[i],
                        color,
                        render_state,
                        texture_binding,
                    );
                }
            }
            GX_DRAW_QUADS => {
                for quad in primitive.chunks_exact(4) {
                    push_triangle(
                        &mut triangles,
                        quad[0],
                        quad[1],
                        quad[2],
                        color,
                        render_state,
                        texture_binding,
                    );
                    push_triangle(
                        &mut triangles,
                        quad[0],
                        quad[2],
                        quad[3],
                        color,
                        render_state,
                        texture_binding,
                    );
                }
            }
            _ => {}
        }
    }

    Ok(triangles)
}

#[allow(clippy::too_many_arguments)]
fn decode_prepared_display_list(
    bytes: &[u8],
    source_bytes: &[u8],
    positions: &[[f32; 3]],
    descs: &[VertexDesc],
    attr_formats: &[AttributeFormat],
    position_format: PositionFormat,
    vertex_arrays: VertexArrays,
    group_matrices: &[u16],
) -> Result<PreparedDisplayList> {
    let mut offset = 0usize;
    let mut vertices = Vec::new();
    let mut triangle_indices = Vec::new();

    while offset < bytes.len() {
        let command = bytes[offset];
        offset += 1;
        if command == 0 {
            break;
        }
        let opcode = gx_primitive_opcode(command)?;
        if offset + 2 > bytes.len() {
            return Err(FormatError::InvalidOffset {
                format: FORMAT,
                offset,
                len: bytes.len(),
            });
        }

        let vertex_count = u16::from_be_bytes([bytes[offset], bytes[offset + 1]]) as usize;
        offset += 2;
        let mut primitive = Vec::with_capacity(vertex_count);
        for _ in 0..vertex_count {
            primitive.push(read_prepared_primitive_vertex(
                bytes,
                source_bytes,
                &mut offset,
                positions,
                descs,
                attr_formats,
                position_format,
                vertex_arrays,
                group_matrices,
            )?);
        }

        let base = vertices.len();
        match opcode {
            GX_DRAW_TRIANGLES => {
                for start in (0..primitive.len()).step_by(3) {
                    if start + 2 < primitive.len() {
                        triangle_indices.push([base + start, base + start + 1, base + start + 2]);
                    }
                }
            }
            GX_DRAW_TRIANGLE_STRIP => {
                for i in 2..primitive.len() {
                    if i % 2 == 0 {
                        triangle_indices.push([base + i - 2, base + i - 1, base + i]);
                    } else {
                        triangle_indices.push([base + i - 1, base + i - 2, base + i]);
                    }
                }
            }
            GX_DRAW_TRIANGLE_FAN => {
                for i in 2..primitive.len() {
                    triangle_indices.push([base, base + i - 1, base + i]);
                }
            }
            GX_DRAW_QUADS => {
                for start in (0..primitive.len()).step_by(4) {
                    if start + 3 < primitive.len() {
                        triangle_indices.push([base + start, base + start + 1, base + start + 2]);
                        triangle_indices.push([base + start, base + start + 2, base + start + 3]);
                    }
                }
            }
            _ => continue,
        }
        vertices.append(&mut primitive);
    }

    Ok(PreparedDisplayList {
        vertices,
        triangle_indices,
    })
}

fn gx_primitive_opcode(command: u8) -> Result<u8> {
    let vat = command & GX_VAT_MASK;
    if vat != 0 {
        return Err(FormatError::Unsupported {
            format: FORMAT,
            message: format!("GX primitive command 0x{command:02X} uses unsupported VAT {vat}"),
        });
    }

    let opcode = command & GX_OPCODE_MASK;
    if matches!(
        opcode,
        GX_DRAW_QUADS | GX_DRAW_TRIANGLES | GX_DRAW_TRIANGLE_STRIP | GX_DRAW_TRIANGLE_FAN
    ) {
        Ok(opcode)
    } else {
        Err(FormatError::Unsupported {
            format: FORMAT,
            message: format!("unsupported GX primitive opcode 0x{opcode:02X}"),
        })
    }
}

#[allow(clippy::too_many_arguments)]
fn read_primitive_vertex(
    bytes: &[u8],
    source_bytes: &[u8],
    offset: &mut usize,
    positions: &[[f32; 3]],
    descs: &[VertexDesc],
    attr_formats: &[AttributeFormat],
    position_format: PositionFormat,
    vertex_arrays: VertexArrays,
    group_matrices: &[u16],
    draw_matrices: &[Option<Mtx34>],
) -> Result<PrimitiveVertex> {
    read_prepared_primitive_vertex(
        bytes,
        source_bytes,
        offset,
        positions,
        descs,
        attr_formats,
        position_format,
        vertex_arrays,
        group_matrices,
    )
    .map(|vertex| vertex.pose(draw_matrices))
}

#[allow(clippy::too_many_arguments)]
fn read_prepared_primitive_vertex(
    bytes: &[u8],
    source_bytes: &[u8],
    offset: &mut usize,
    positions: &[[f32; 3]],
    descs: &[VertexDesc],
    attr_formats: &[AttributeFormat],
    position_format: PositionFormat,
    vertex_arrays: VertexArrays,
    group_matrices: &[u16],
) -> Result<PreparedPrimitiveVertex> {
    let mut position = None;
    let mut matrix_slot = None;
    let mut normal = None;
    let mut colors = [None; 2];
    let mut tex_coords = [None; TEX_COORD_COUNT];
    for desc in descs {
        match desc.attr_type {
            GX_DIRECT => {
                if desc.attr == GX_VA_PNMTXIDX {
                    checked_slice(FORMAT, bytes, *offset, 1)?;
                    matrix_slot = Some(bytes[*offset] as u16);
                    *offset += 1;
                } else if desc.attr == GX_VA_POS {
                    position = Some(read_direct_position(bytes, offset, position_format)?);
                } else if desc.attr == GX_VA_NRM {
                    if let Some(format) = vertex_arrays.normal_format {
                        normal = Some(read_direct_normal(bytes, offset, format)?);
                    } else {
                        let size = direct_attribute_size(desc.attr, attr_formats)?;
                        checked_slice(FORMAT, bytes, *offset, size)?;
                        *offset += size;
                    }
                } else if desc.attr == GX_VA_CLR0 || desc.attr == GX_VA_CLR1 {
                    let color_index = (desc.attr - GX_VA_CLR0) as usize;
                    if let Some(format) = vertex_arrays.color_formats[color_index] {
                        colors[color_index] = Some(read_direct_color(bytes, offset, format)?);
                    } else {
                        let size = direct_attribute_size(desc.attr, attr_formats)?;
                        checked_slice(FORMAT, bytes, *offset, size)?;
                        *offset += size;
                    }
                } else if (GX_VA_TEX0..=GX_VA_TEX7).contains(&desc.attr) {
                    let tex_index = (desc.attr - GX_VA_TEX0) as usize;
                    if let Some(format) = vertex_arrays.tex_formats[tex_index] {
                        tex_coords[tex_index] = Some(read_direct_tex_coord(bytes, offset, format)?);
                    } else {
                        let size = direct_attribute_size(desc.attr, attr_formats)?;
                        checked_slice(FORMAT, bytes, *offset, size)?;
                        *offset += size;
                    }
                } else {
                    let size = direct_attribute_size(desc.attr, attr_formats)?;
                    checked_slice(FORMAT, bytes, *offset, size)?;
                    *offset += size;
                }
            }
            GX_INDEX8 => {
                checked_slice(FORMAT, bytes, *offset, 1)?;
                let index = bytes[*offset] as usize;
                *offset += 1;
                if desc.attr == GX_VA_PNMTXIDX {
                    matrix_slot = Some(index as u16);
                } else if desc.attr == GX_VA_POS {
                    position = positions.get(index).copied();
                } else if desc.attr == GX_VA_NRM {
                    normal = Some(read_indexed_normal(source_bytes, index, vertex_arrays)?);
                } else if desc.attr == GX_VA_CLR0 || desc.attr == GX_VA_CLR1 {
                    let color_index = (desc.attr - GX_VA_CLR0) as usize;
                    colors[color_index] = Some(read_indexed_vertex_color(
                        source_bytes,
                        index,
                        vertex_arrays,
                        color_index,
                    )?);
                } else if (GX_VA_TEX0..=GX_VA_TEX7).contains(&desc.attr) {
                    let tex_index = (desc.attr - GX_VA_TEX0) as usize;
                    tex_coords[tex_index] = Some(read_indexed_tex_coord(
                        source_bytes,
                        index,
                        vertex_arrays,
                        tex_index,
                    )?);
                }
            }
            GX_INDEX16 => {
                checked_slice(FORMAT, bytes, *offset, 2)?;
                let index = u16::from_be_bytes([bytes[*offset], bytes[*offset + 1]]) as usize;
                *offset += 2;
                if desc.attr == GX_VA_PNMTXIDX {
                    matrix_slot = Some(index as u16);
                } else if desc.attr == GX_VA_POS {
                    position = positions.get(index).copied();
                } else if desc.attr == GX_VA_NRM {
                    normal = Some(read_indexed_normal(source_bytes, index, vertex_arrays)?);
                } else if desc.attr == GX_VA_CLR0 || desc.attr == GX_VA_CLR1 {
                    let color_index = (desc.attr - GX_VA_CLR0) as usize;
                    colors[color_index] = Some(read_indexed_vertex_color(
                        source_bytes,
                        index,
                        vertex_arrays,
                        color_index,
                    )?);
                } else if (GX_VA_TEX0..=GX_VA_TEX7).contains(&desc.attr) {
                    let tex_index = (desc.attr - GX_VA_TEX0) as usize;
                    tex_coords[tex_index] = Some(read_indexed_tex_coord(
                        source_bytes,
                        index,
                        vertex_arrays,
                        tex_index,
                    )?);
                }
            }
            _ => {}
        }
    }

    let position = position.ok_or_else(|| FormatError::Unsupported {
        format: FORMAT,
        message: "primitive vertex did not include a valid position".to_string(),
    })?;
    Ok(PreparedPrimitiveVertex {
        vertex: PrimitiveVertex {
            position,
            normal,
            colors,
            tex_coords,
        },
        draw_matrix_index: shape_draw_matrix_index(matrix_slot, group_matrices),
    })
}

fn read_direct_position(
    bytes: &[u8],
    offset: &mut usize,
    format: PositionFormat,
) -> Result<[f32; 3]> {
    match format.component_type {
        GX_F32 => {
            checked_slice(FORMAT, bytes, *offset, 12)?;
            let point = [
                f32::from_bits(u32::from_be_bytes([
                    bytes[*offset],
                    bytes[*offset + 1],
                    bytes[*offset + 2],
                    bytes[*offset + 3],
                ])),
                f32::from_bits(u32::from_be_bytes([
                    bytes[*offset + 4],
                    bytes[*offset + 5],
                    bytes[*offset + 6],
                    bytes[*offset + 7],
                ])),
                f32::from_bits(u32::from_be_bytes([
                    bytes[*offset + 8],
                    bytes[*offset + 9],
                    bytes[*offset + 10],
                    bytes[*offset + 11],
                ])),
            ];
            *offset += 12;
            Ok(point)
        }
        GX_S16 => {
            checked_slice(FORMAT, bytes, *offset, 6)?;
            let scale = 1.0 / (1u32 << format.frac.min(30)) as f32;
            let point = [
                i16::from_be_bytes([bytes[*offset], bytes[*offset + 1]]) as f32 * scale,
                i16::from_be_bytes([bytes[*offset + 2], bytes[*offset + 3]]) as f32 * scale,
                i16::from_be_bytes([bytes[*offset + 4], bytes[*offset + 5]]) as f32 * scale,
            ];
            *offset += 6;
            Ok(point)
        }
        _ => Err(FormatError::Unsupported {
            format: FORMAT,
            message: format!("unsupported direct position type {}", format.component_type),
        }),
    }
}

fn read_direct_tex_coord(
    bytes: &[u8],
    offset: &mut usize,
    format: TexCoordFormat,
) -> Result<[f32; 2]> {
    let mut tex = [0.0, 0.0];
    for component in tex.iter_mut().take(format.components) {
        *component = read_tex_component(bytes, offset, format)?;
    }
    Ok(tex)
}

fn read_direct_normal(bytes: &[u8], offset: &mut usize, format: NormalFormat) -> Result<[f32; 3]> {
    let mut components = Vec::with_capacity(format.components.max(3));
    for _ in 0..format.components {
        components.push(read_normal_component(bytes, offset, format)?);
    }
    let normal = [
        *components.first().unwrap_or(&0.0),
        *components.get(1).unwrap_or(&0.0),
        *components.get(2).unwrap_or(&1.0),
    ];
    normalize_vec3(normal).ok_or_else(|| FormatError::Unsupported {
        format: FORMAT,
        message: "normal vector has zero length".to_string(),
    })
}

fn read_normal_component(bytes: &[u8], offset: &mut usize, format: NormalFormat) -> Result<f32> {
    let scale = 1.0 / (1u32 << format.frac.min(30)) as f32;
    let value = match format.component_type {
        GX_S8 => {
            checked_slice(FORMAT, bytes, *offset, 1)?;
            let value = bytes[*offset] as i8 as f32 * scale;
            *offset += 1;
            value
        }
        GX_S16 => {
            checked_slice(FORMAT, bytes, *offset, 2)?;
            let value = i16::from_be_bytes([bytes[*offset], bytes[*offset + 1]]) as f32 * scale;
            *offset += 2;
            value
        }
        GX_F32 => {
            checked_slice(FORMAT, bytes, *offset, 4)?;
            let value = f32::from_bits(u32::from_be_bytes([
                bytes[*offset],
                bytes[*offset + 1],
                bytes[*offset + 2],
                bytes[*offset + 3],
            ]));
            *offset += 4;
            value
        }
        _ => {
            return Err(FormatError::Unsupported {
                format: FORMAT,
                message: format!(
                    "unsupported normal component type {}",
                    format.component_type
                ),
            });
        }
    };
    Ok(value)
}

fn read_direct_color(bytes: &[u8], offset: &mut usize, format: ColorFormat) -> Result<[u8; 4]> {
    let color = match format.component_type {
        GX_RGB565 => {
            checked_slice(FORMAT, bytes, *offset, 2)?;
            let value = u16::from_be_bytes([bytes[*offset], bytes[*offset + 1]]);
            *offset += 2;
            rgb565_to_rgba(value)
        }
        GX_RGB8 => {
            checked_slice(FORMAT, bytes, *offset, 3)?;
            let color = [bytes[*offset], bytes[*offset + 1], bytes[*offset + 2], 255];
            *offset += 3;
            color
        }
        GX_RGBX8 => {
            checked_slice(FORMAT, bytes, *offset, 4)?;
            let color = [bytes[*offset], bytes[*offset + 1], bytes[*offset + 2], 255];
            *offset += 4;
            color
        }
        GX_RGBA4 => {
            checked_slice(FORMAT, bytes, *offset, 2)?;
            let value = u16::from_be_bytes([bytes[*offset], bytes[*offset + 1]]);
            *offset += 2;
            [
                expand_bits((value >> 12) & 0x0F, 4),
                expand_bits((value >> 8) & 0x0F, 4),
                expand_bits((value >> 4) & 0x0F, 4),
                expand_bits(value & 0x0F, 4),
            ]
        }
        GX_RGBA6 => {
            checked_slice(FORMAT, bytes, *offset, 3)?;
            let value = ((bytes[*offset] as u32) << 16)
                | ((bytes[*offset + 1] as u32) << 8)
                | bytes[*offset + 2] as u32;
            *offset += 3;
            [
                expand_bits(((value >> 18) & 0x3F) as u16, 6),
                expand_bits(((value >> 12) & 0x3F) as u16, 6),
                expand_bits(((value >> 6) & 0x3F) as u16, 6),
                expand_bits((value & 0x3F) as u16, 6),
            ]
        }
        GX_RGBA8 => {
            checked_slice(FORMAT, bytes, *offset, 4)?;
            let color = [
                bytes[*offset],
                bytes[*offset + 1],
                bytes[*offset + 2],
                bytes[*offset + 3],
            ];
            *offset += 4;
            color
        }
        _ => {
            return Err(FormatError::Unsupported {
                format: FORMAT,
                message: format!("unsupported direct color type {}", format.component_type),
            });
        }
    };

    Ok(color)
}

fn read_indexed_normal(
    bytes: &[u8],
    index: usize,
    vertex_arrays: VertexArrays,
) -> Result<[f32; 3]> {
    let Some(array_offset) = vertex_arrays.normal_offset else {
        return Err(FormatError::Unsupported {
            format: FORMAT,
            message: "missing normal array".to_string(),
        });
    };
    let Some(format) = vertex_arrays.normal_format else {
        return Err(FormatError::Unsupported {
            format: FORMAT,
            message: "missing normal format".to_string(),
        });
    };
    let stride = normal_stride(format);
    let mut offset = array_offset + index * stride;
    read_direct_normal(bytes, &mut offset, format)
}

fn read_indexed_vertex_color(
    bytes: &[u8],
    index: usize,
    vertex_arrays: VertexArrays,
    color_index: usize,
) -> Result<[u8; 4]> {
    let Some(array_offset) = vertex_arrays
        .color_offsets
        .get(color_index)
        .copied()
        .flatten()
    else {
        return Err(FormatError::Unsupported {
            format: FORMAT,
            message: format!("missing CLR{color_index} array"),
        });
    };
    let Some(format) = vertex_arrays
        .color_formats
        .get(color_index)
        .copied()
        .flatten()
    else {
        return Err(FormatError::Unsupported {
            format: FORMAT,
            message: format!("missing CLR{color_index} format"),
        });
    };
    let stride = packed_color_size(format.component_type)?;
    let mut offset = array_offset + index * stride;
    read_direct_color(bytes, &mut offset, format)
}

fn read_indexed_tex_coord(
    bytes: &[u8],
    index: usize,
    vertex_arrays: VertexArrays,
    tex_index: usize,
) -> Result<[f32; 2]> {
    let Some(array_offset) = vertex_arrays.tex_offsets.get(tex_index).copied().flatten() else {
        return Err(FormatError::Unsupported {
            format: FORMAT,
            message: format!("missing TEX{tex_index} coordinate array"),
        });
    };
    let Some(format) = vertex_arrays.tex_formats.get(tex_index).copied().flatten() else {
        return Err(FormatError::Unsupported {
            format: FORMAT,
            message: format!("missing TEX{tex_index} coordinate format"),
        });
    };
    let stride = tex_coord_stride(format);
    let mut offset = array_offset + index * stride;
    read_direct_tex_coord(bytes, &mut offset, format)
}

fn read_tex_component(bytes: &[u8], offset: &mut usize, format: TexCoordFormat) -> Result<f32> {
    let scale = 1.0 / (1u32 << format.frac.min(30)) as f32;
    let value = match format.component_type {
        GX_U8 => {
            checked_slice(FORMAT, bytes, *offset, 1)?;
            let value = bytes[*offset] as f32 * scale;
            *offset += 1;
            value
        }
        GX_S8 => {
            checked_slice(FORMAT, bytes, *offset, 1)?;
            let value = bytes[*offset] as i8 as f32 * scale;
            *offset += 1;
            value
        }
        GX_U16 => {
            checked_slice(FORMAT, bytes, *offset, 2)?;
            let value = u16::from_be_bytes([bytes[*offset], bytes[*offset + 1]]) as f32 * scale;
            *offset += 2;
            value
        }
        GX_S16 => {
            checked_slice(FORMAT, bytes, *offset, 2)?;
            let value = i16::from_be_bytes([bytes[*offset], bytes[*offset + 1]]) as f32 * scale;
            *offset += 2;
            value
        }
        GX_F32 => {
            checked_slice(FORMAT, bytes, *offset, 4)?;
            let value = f32::from_bits(u32::from_be_bytes([
                bytes[*offset],
                bytes[*offset + 1],
                bytes[*offset + 2],
                bytes[*offset + 3],
            ]));
            *offset += 4;
            value
        }
        _ => {
            return Err(FormatError::Unsupported {
                format: FORMAT,
                message: format!(
                    "unsupported texture coordinate component type {}",
                    format.component_type
                ),
            });
        }
    };

    Ok(value)
}

fn tex_coord_stride(format: TexCoordFormat) -> usize {
    format.components * scalar_component_size(format.component_type).unwrap_or(0)
}

fn normal_stride(format: NormalFormat) -> usize {
    format.components * scalar_component_size(format.component_type).unwrap_or(0)
}

fn normal_component_count(cnt: u32) -> usize {
    if cnt == GX_NRM_XYZ {
        3
    } else {
        9
    }
}

fn direct_attribute_size(attr: u32, attr_formats: &[AttributeFormat]) -> Result<usize> {
    if (GX_VA_PNMTXIDX..GX_VA_POS).contains(&attr) {
        return Ok(1);
    }

    let Some(format) = attr_formats.iter().find(|format| format.attr == attr) else {
        return Err(FormatError::Unsupported {
            format: FORMAT,
            message: format!("missing direct attribute format for attr {attr}"),
        });
    };

    if attr == GX_VA_CLR0 || attr == GX_VA_CLR1 {
        return packed_color_size(format.component_type);
    }

    let component_count = if attr == GX_VA_POS {
        if format.cnt == GX_POS_XYZ {
            3
        } else {
            2
        }
    } else if attr == GX_VA_NRM {
        normal_component_count(format.cnt)
    } else if attr >= GX_VA_TEX0 {
        if format.cnt == GX_TEX_ST {
            2
        } else {
            1
        }
    } else {
        3
    };

    Ok(component_count * scalar_component_size(format.component_type)?)
}

fn scalar_component_size(component_type: u32) -> Result<usize> {
    match component_type {
        GX_U8 | GX_S8 => Ok(1),
        GX_U16 | GX_S16 => Ok(2),
        GX_F32 => Ok(4),
        _ => Err(FormatError::Unsupported {
            format: FORMAT,
            message: format!("unsupported scalar component type {component_type}"),
        }),
    }
}

fn packed_color_size(component_type: u32) -> Result<usize> {
    match component_type {
        GX_RGB565 | GX_RGBA4 => Ok(2),
        GX_RGB8 | GX_RGBA6 => Ok(3),
        GX_RGBX8 | GX_RGBA8 => Ok(4),
        _ => Err(FormatError::Unsupported {
            format: FORMAT,
            message: format!("unsupported color component type {component_type}"),
        }),
    }
}

fn read_indexed_color(bytes: &[u8], table_offset: Option<usize>, index: u16) -> Option<[u8; 4]> {
    if index == 0xFFFF {
        return None;
    }
    let offset = table_offset?.checked_add(index as usize * 4)?;
    let color = checked_slice(FORMAT, bytes, offset, 4).ok()?;
    Some([color[0], color[1], color[2], color[3]])
}

fn read_indexed_u8(bytes: &[u8], table_offset: Option<usize>, index: u8) -> Option<u8> {
    if index == 0xFF {
        return None;
    }
    checked_slice(FORMAT, bytes, table_offset?.checked_add(index as usize)?, 1)
        .ok()
        .and_then(|bytes| bytes.first().copied())
}

fn read_indexed_cull_mode(bytes: &[u8], table_offset: Option<usize>, index: u8) -> Option<u8> {
    if index == 0xFF {
        return None;
    }
    let offset = table_offset?.checked_add(index as usize * std::mem::size_of::<u32>())?;
    u8::try_from(be_u32(bytes, offset, FORMAT).ok()?).ok()
}

fn read_indexed_alpha_compare(
    bytes: &[u8],
    table_offset: Option<usize>,
    index: u16,
) -> Option<J3dAlphaCompare> {
    if index == 0xFFFF {
        return None;
    }
    let offset = table_offset?.checked_add(index as usize * 8)?;
    let info = checked_slice(FORMAT, bytes, offset, 8).ok()?;
    Some(J3dAlphaCompare {
        comp0: info[0],
        ref0: info[1],
        op: info[2],
        comp1: info[3],
        ref1: info[4],
    })
}

fn read_indexed_blend_mode(
    bytes: &[u8],
    table_offset: Option<usize>,
    index: u16,
) -> Option<J3dBlendMode> {
    if index == 0xFFFF {
        return None;
    }
    let offset = table_offset?.checked_add(index as usize * 4)?;
    let info = checked_slice(FORMAT, bytes, offset, 4).ok()?;
    Some(J3dBlendMode {
        mode: info[0],
        src_factor: info[1],
        dst_factor: info[2],
        logic_op: info[3],
    })
}

fn read_indexed_z_mode(bytes: &[u8], table_offset: Option<usize>, index: u8) -> Option<J3dZMode> {
    if index == 0xFF {
        return None;
    }
    let offset = table_offset?.checked_add(index as usize * 4)?;
    let info = checked_slice(FORMAT, bytes, offset, 4).ok()?;
    Some(J3dZMode {
        compare_enable: info[0],
        func: info[1],
        update_enable: info[2],
    })
}

// Texture decoding lives in j3d/texture.rs.

fn push_triangle(
    triangles: &mut Vec<J3dTriangle>,
    a: PrimitiveVertex,
    b: PrimitiveVertex,
    c: PrimitiveVertex,
    color: Option<[u8; 4]>,
    render_state: J3dMaterialRenderState,
    texture_binding: Option<MaterialPreviewBinding>,
) {
    if a.position == b.position || b.position == c.position || a.position == c.position {
        return;
    }
    if !a
        .position
        .iter()
        .chain(b.position.iter())
        .chain(c.position.iter())
        .all(|value| value.is_finite())
    {
        return;
    }
    let tex_coords = texture_binding.and_then(|binding| {
        binding.texture_index?;
        let coords = preview_tex_coords_for_vertices(binding.tex_coord_source, a, b, c)?;
        let coords = binding
            .tex_mtx
            .map(|tex_mtx| coords.map(|coord| tex_mtx.apply(coord)))
            .unwrap_or(coords);
        tex_coords_are_finite(coords).then_some(coords)
    });
    let texture_index = match (
        tex_coords,
        texture_binding.and_then(|binding| binding.texture_index),
    ) {
        (Some(_), Some(texture_index)) => Some(texture_index),
        _ => None,
    };
    let mask_tex_coords = texture_binding.and_then(|binding| {
        binding.mask_texture_index?;
        let coords = preview_tex_coords_for_vertices(binding.mask_tex_coord_source, a, b, c)?;
        let coords = binding
            .mask_tex_mtx
            .map(|tex_mtx| coords.map(|coord| tex_mtx.apply(coord)))
            .unwrap_or(coords);
        tex_coords_are_finite(coords).then_some(coords)
    });
    let mask_texture_index = match (
        mask_tex_coords,
        texture_binding.and_then(|binding| binding.mask_texture_index),
    ) {
        (Some(_), Some(texture_index)) => Some(texture_index),
        _ => None,
    };
    let texture_tint = texture_binding
        .and_then(|binding| binding.tint_color)
        .filter(|color| preview_tint_color_is_useful(*color));
    let mut combine_mode = texture_binding
        .map(|binding| binding.combine_mode)
        .unwrap_or(J3dPreviewCombineMode::MaterialOnly);
    if texture_index.is_some()
        && texture_tint.is_some()
        && combine_mode == J3dPreviewCombineMode::TextureOnly
    {
        combine_mode = J3dPreviewCombineMode::TextureModulateMaterial;
    }
    let color = texture_tint.or(color);
    let vertex_colors = if combine_mode.needs_vertex_color() {
        match (a.colors[0], b.colors[0], c.colors[0]) {
            (Some(a), Some(b), Some(c)) => Some([a, b, c]),
            _ => None,
        }
    } else {
        None
    };
    let color_channels =
        std::array::from_fn(
            |slot| match (a.colors[slot], b.colors[slot], c.colors[slot]) {
                (Some(a), Some(b), Some(c)) => Some([a, b, c]),
                _ => None,
            },
        );
    let tex_coord_sets = std::array::from_fn(|slot| {
        match (a.tex_coords[slot], b.tex_coords[slot], c.tex_coords[slot]) {
            (Some(a), Some(b), Some(c)) => Some([a, b, c]),
            _ => None,
        }
    });
    triangles.push(J3dTriangle {
        vertices: [a.position, b.position, c.position],
        normals: match (a.normal, b.normal, c.normal) {
            (Some(a), Some(b), Some(c)) => Some([a, b, c]),
            _ => None,
        },
        color_channels,
        tex_coord_sets,
        material_index: None,
        shape_index: 0,
        packet_index: 0,
        color,
        vertex_colors,
        combine_mode,
        tex_coords,
        texture_index,
        mask_tex_coords,
        mask_texture_index,
        cull_mode: render_state.cull_mode,
        alpha_compare: render_state.alpha_compare,
        blend_mode: render_state.blend_mode,
        z_mode: render_state.z_mode,
        z_comp_loc: render_state.z_comp_loc,
        billboard: None,
    });
}

fn preview_tex_coords_for_vertices(
    source: TexCoordPreviewSource,
    a: PrimitiveVertex,
    b: PrimitiveVertex,
    c: PrimitiveVertex,
) -> Option<[[f32; 2]; 3]> {
    match source {
        TexCoordPreviewSource::Vertex(tex_index) => match (
            a.tex_coords.get(tex_index).copied().flatten(),
            b.tex_coords.get(tex_index).copied().flatten(),
            c.tex_coords.get(tex_index).copied().flatten(),
        ) {
            (Some(a), Some(b), Some(c)) => Some([a, b, c]),
            _ => None,
        },
        TexCoordPreviewSource::Position => Some([
            position_planar_tex_coord(a.position),
            position_planar_tex_coord(b.position),
            position_planar_tex_coord(c.position),
        ]),
        TexCoordPreviewSource::Normal => {
            if let (Some(a), Some(b), Some(c)) = (a.normal, b.normal, c.normal) {
                return Some([
                    normal_preview_tex_coord(a),
                    normal_preview_tex_coord(b),
                    normal_preview_tex_coord(c),
                ]);
            }
            let normal = triangle_preview_normal(a.position, b.position, c.position)?;
            Some([
                normal_preview_tex_coord(normal),
                normal_preview_tex_coord(normal),
                normal_preview_tex_coord(normal),
            ])
        }
    }
}

fn position_planar_tex_coord(position: [f32; 3]) -> [f32; 2] {
    [position[0], position[2]]
}

fn normal_preview_tex_coord(normal: [f32; 3]) -> [f32; 2] {
    [normal[0] * 0.5 + 0.5, normal[1] * 0.5 + 0.5]
}

fn triangle_preview_normal(a: [f32; 3], b: [f32; 3], c: [f32; 3]) -> Option<[f32; 3]> {
    let ab = [b[0] - a[0], b[1] - a[1], b[2] - a[2]];
    let ac = [c[0] - a[0], c[1] - a[1], c[2] - a[2]];
    let normal = [
        ab[1] * ac[2] - ab[2] * ac[1],
        ab[2] * ac[0] - ab[0] * ac[2],
        ab[0] * ac[1] - ab[1] * ac[0],
    ];
    let len_sq = normal[0] * normal[0] + normal[1] * normal[1] + normal[2] * normal[2];
    if !len_sq.is_finite() || len_sq <= f32::EPSILON {
        return None;
    }
    let inv_len = 1.0 / len_sq.sqrt();
    Some([
        normal[0] * inv_len,
        normal[1] * inv_len,
        normal[2] * inv_len,
    ])
}

fn tex_coords_are_finite(tex_coords: [[f32; 2]; 3]) -> bool {
    tex_coords
        .iter()
        .flatten()
        .all(|value| value.is_finite() && value.abs() < 1_000_000.0)
}

fn invalid_offset(offset: usize, len: usize) -> FormatError {
    FormatError::InvalidOffset {
        format: FORMAT,
        offset,
        len,
    }
}

#[cfg(test)]
#[path = "j3d_tests.rs"]
mod tests;
