use serde::{Deserialize, Serialize};
use sms_formats::{
    compile_static_bmd3, compile_texture_section, BmpFile, GxEncodedTexture, GxMaterial,
    GxPaletteFormat, GxTextureEncodeOptions, GxTextureEncoding, GxTextureFormat, J3dFile,
    J3dMaterialTableKind, J3dPaddingKind, J3dPaddingSpan, J3dRebuildDocument,
    J3dRebuildSectionData, J3dScalarArray, RgbaImage, StaticModel, StaticModelMesh,
    StaticModelVertex, YmpDocument, YmpLayer,
};

use crate::{Result, SceneError, StageDocument, StageResourceDocument};

pub const GOOP_RESOURCE_PATH: &[u8] = b"map/ymap.ymp";
pub const GOOP_CELL_SIZE: f32 = 40.0;
/// Inverse of `TPollutionPos::worldToDepth`'s hard-coded `0.025` conversion.
/// This is independent of a retail layer's horizontal `mVerticalScale`.
pub const GOOP_DEPTH_WORLD_UNITS_PER_CODE: f32 = 40.0;
/// Depth zero cannot be modified by Sunshine's texture-stamp pass. The pass
/// uses strict `depth > lower && upper > depth` TEV comparisons and clamps a
/// negative lower bound to zero, making a zero-valued YMP cell permanently
/// fail the first comparison even when the water stamp is at the same height.
pub const GOOP_MIN_MUTABLE_DEPTH: u8 = 1;
pub const GOOP_MAX_LAYERS: usize = 20;
pub const GOOP_MAX_DIMENSION: usize = 1024;
pub const GOOP_AUTHORING_FORMAT_VERSION: u32 = 9;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GoopPlane {
    Floor,
    WallPlusX,
    WallMinusX,
    WallPlusZ,
    WallMinusZ,
    Wave,
    Retail(u16),
}

impl GoopPlane {
    pub fn runtime_code(self) -> u16 {
        match self {
            Self::Floor => 0,
            Self::WallPlusX => 2,
            Self::WallMinusX => 3,
            Self::WallPlusZ => 4,
            Self::WallMinusZ => 5,
            Self::Wave => 6,
            Self::Retail(code) => code,
        }
    }

    pub fn from_runtime_code(code: u16) -> Option<Self> {
        match code {
            0 | 1 => Some(Self::Floor),
            2 => Some(Self::WallPlusX),
            3 => Some(Self::WallMinusX),
            4 => Some(Self::WallPlusZ),
            5 => Some(Self::WallMinusZ),
            6 => Some(Self::Wave),
            code => Some(Self::Retail(code)),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GoopBehavior {
    Normal,
    Fire,
    Slippery,
    Barrier,
    Electric,
    Retail(u16),
}

impl GoopBehavior {
    pub fn runtime_code(self) -> u16 {
        match self {
            Self::Normal => 0,
            Self::Fire => 1,
            Self::Slippery => 2,
            Self::Barrier => 3,
            Self::Electric => 4,
            Self::Retail(code) => code,
        }
    }

    pub fn from_runtime_code(code: u16) -> Self {
        match code {
            0 => Self::Normal,
            1 => Self::Fire,
            2 => Self::Slippery,
            3 => Self::Barrier,
            4 => Self::Electric,
            code => Self::Retail(code),
        }
    }

    pub fn label(self) -> String {
        match self {
            Self::Normal => "Normal".to_string(),
            Self::Fire => "Fire".to_string(),
            Self::Slippery => "Slippery".to_string(),
            Self::Barrier => "Barrier".to_string(),
            Self::Electric => "Electric".to_string(),
            Self::Retail(code) => format!("Retail type {code}"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GoopStyleSource {
    pub stage_id: String,
    pub layer_index: usize,
    pub display_name: String,
    #[serde(default)]
    pub behavior_code: u16,
    #[serde(default)]
    pub forced_incompatible: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct GoopRegion {
    pub min_x: f32,
    pub min_z: f32,
    pub max_x: f32,
    pub max_z: f32,
}

impl GoopRegion {
    pub fn contains(self, x: f32, z: f32) -> bool {
        x >= self.min_x && x < self.max_x && z >= self.min_z && z < self.max_z
    }

    pub fn overlaps(self, other: Self) -> bool {
        self.min_x < other.max_x
            && other.min_x < self.max_x
            && self.min_z < other.max_z
            && other.min_z < self.max_z
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GoopLayerOrigin {
    Imported,
    Generated,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GoopLayerAuthoring {
    pub id: String,
    pub runtime_index: usize,
    pub origin: GoopLayerOrigin,
    pub plane: GoopPlane,
    pub behavior: GoopBehavior,
    #[serde(default = "default_true")]
    pub visible: bool,
    pub region: GoopRegion,
    pub runtime: YmpLayer,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bitmap: Option<BmpFile>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub generated_model: Option<J3dRebuildDocument>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub style_source: Option<GoopStyleSource>,
    #[serde(default)]
    pub resource_stem: String,
    #[serde(default)]
    pub metadata_dirty: bool,
}

fn default_true() -> bool {
    true
}

impl GoopLayerAuthoring {
    pub fn editable(&self) -> bool {
        self.plane == GoopPlane::Floor && self.bitmap.is_some()
    }

    pub fn dimensions(&self) -> Result<(usize, usize)> {
        Ok(self.runtime.dimensions()?)
    }

    pub fn valid_cell(&self, x: usize, y: usize) -> bool {
        self.runtime.depth_at(x, y).is_ok_and(|depth| depth != 0xff)
    }

    pub fn mask(&self) -> Result<Vec<u8>> {
        self.bitmap
            .as_ref()
            .ok_or_else(|| {
                SceneError::StageExport(format!("goop layer {} has no bitmap", self.id))
            })?
            .top_down_indices()
            .map_err(Into::into)
    }

    pub fn set_mask(&mut self, mask: &[u8]) -> Result<()> {
        self.bitmap
            .as_mut()
            .ok_or_else(|| {
                SceneError::StageExport(format!("goop layer {} has no bitmap", self.id))
            })?
            .set_top_down_indices(mask)?;
        Ok(())
    }

    pub fn world_to_cell(&self, x: f32, z: f32) -> Option<(usize, usize)> {
        if !self.region.contains(x, z) {
            return None;
        }
        let (width, height) = self.runtime.dimensions().ok()?;
        let cell_size = self.runtime.vertical_scale;
        if !cell_size.is_finite() || cell_size <= 0.0 {
            return None;
        }
        let cell_x = ((x - self.region.min_x) / cell_size) as usize;
        let cell_y = ((z - self.region.min_z) / cell_size) as usize;
        (cell_x < width && cell_y < height).then_some((cell_x, cell_y))
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GoopAuthoringDocument {
    pub format_version: u32,
    pub layers: Vec<GoopLayerAuthoring>,
    #[serde(default)]
    pub terrain_fingerprint: u64,
    #[serde(default)]
    pub stale: bool,
}

impl Default for GoopAuthoringDocument {
    fn default() -> Self {
        Self {
            format_version: GOOP_AUTHORING_FORMAT_VERSION,
            layers: Vec::new(),
            terrain_fingerprint: 0,
            stale: false,
        }
    }
}

impl GoopAuthoringDocument {
    pub fn requires_generator_upgrade(&self) -> bool {
        self.format_version < GOOP_AUTHORING_FORMAT_VERSION
            && self
                .layers
                .iter()
                .any(|layer| layer.origin == GoopLayerOrigin::Generated)
    }

    pub fn validate(&self) -> Result<()> {
        if self.requires_generator_upgrade() {
            return Err(SceneError::StageExport(format!(
                "generated goop uses authoring format version {}, but this editor requires version {GOOP_AUTHORING_FORMAT_VERSION}; open the Goop tool and rebuild the generated layers",
                self.format_version
            )));
        }
        if self.layers.len() > GOOP_MAX_LAYERS {
            return Err(SceneError::StageExport(format!(
                "Sunshine supports at most {GOOP_MAX_LAYERS} goop layers"
            )));
        }
        for (index, layer) in self.layers.iter().enumerate() {
            if layer.runtime_index != index {
                return Err(SceneError::StageExport(format!(
                    "goop layer {} has runtime index {}, expected {index}",
                    layer.id, layer.runtime_index
                )));
            }
            let (width, height) = layer.dimensions()?;
            if width > GOOP_MAX_DIMENSION || height > GOOP_MAX_DIMENSION {
                return Err(SceneError::StageExport(format!(
                    "goop layer {} is {width}x{height}, exceeding 1024x1024",
                    layer.id
                )));
            }
            if layer.plane == GoopPlane::Floor {
                layer.runtime.validate_floor_runtime()?;
            }
            if let Some(bitmap) = &layer.bitmap {
                if bitmap.width.unsigned_abs() as usize != width
                    || bitmap.height.unsigned_abs() as usize != height
                {
                    return Err(SceneError::StageExport(format!(
                        "goop layer {} bitmap dimensions do not match YMP",
                        layer.id
                    )));
                }
            }
            if layer.origin == GoopLayerOrigin::Generated {
                if (layer.runtime.vertical_scale - GOOP_CELL_SIZE).abs() > f32::EPSILON {
                    return Err(SceneError::StageExport(format!(
                        "generated goop layer {} must use the canonical {GOOP_CELL_SIZE}-unit scale, got {}",
                        layer.id, layer.runtime.vertical_scale
                    )));
                }
                if layer.bitmap.is_none() || layer.generated_model.is_none() {
                    return Err(SceneError::StageExport(format!(
                        "generated goop layer {} is missing its bitmap or pollution model",
                        layer.id
                    )));
                }
                if layer.style_source.is_none() {
                    return Err(SceneError::StageExport(format!(
                        "generated goop layer {} has no retail style provenance",
                        layer.id
                    )));
                }
            }
            if let Some(model) = &layer.generated_model {
                let first_texture = model
                    .sections
                    .iter()
                    .find_map(|section| match &section.data {
                        J3dRebuildSectionData::Textures(textures) => textures.textures.first(),
                        _ => None,
                    });
                let Some(texture) = first_texture else {
                    return Err(SceneError::StageExport(format!(
                        "goop layer {} model has no first texture",
                        layer.id
                    )));
                };
                if texture.format != GxTextureFormat::I8 as u8
                    || usize::from(texture.width) != width
                    || usize::from(texture.height) != height
                {
                    return Err(SceneError::StageExport(format!(
                        "goop layer {} model texture zero is not a matching I8 mask",
                        layer.id
                    )));
                }
            }
        }
        for left in 0..self.layers.len() {
            for right in left + 1..self.layers.len() {
                if (self.layers[left].origin == GoopLayerOrigin::Generated
                    || self.layers[right].origin == GoopLayerOrigin::Generated)
                    && self.layers[left].plane == GoopPlane::Floor
                    && self.layers[right].plane == GoopPlane::Floor
                    && self.layers[left].region.overlaps(self.layers[right].region)
                {
                    return Err(SceneError::StageExport(format!(
                        "goop regions {} and {} overlap",
                        self.layers[left].id, self.layers[right].id
                    )));
                }
            }
        }
        if self.stale {
            return Err(SceneError::StageExport(
                "generated goopmaps are stale; rebuild them before export".to_string(),
            ));
        }
        Ok(())
    }

    /// Returns the same authored resources with only release-readiness gates
    /// cleared. Editor mutations must remain serializable while a generated
    /// layer is stale or waiting for a generator upgrade; release validation
    /// still runs against the original document and continues to block it.
    fn for_resource_compilation(&self) -> Self {
        let mut compilable = self.clone();
        compilable.format_version = GOOP_AUTHORING_FORMAT_VERSION;
        compilable.stale = false;
        compilable
    }

    pub fn compiled_ymp(&self) -> Result<YmpDocument> {
        self.validate()?;
        Ok(YmpDocument::canonical(
            self.layers
                .iter()
                .map(|layer| layer.runtime.clone())
                .collect(),
        )?)
    }

    pub fn compiled_ymp_preserving(&self, base: &YmpDocument) -> Result<YmpDocument> {
        self.validate()?;
        let allocation_compatible = base.layers.len() == self.layers.len()
            && base
                .layers
                .iter()
                .zip(&self.layers)
                .all(|(old, authored)| old.depth_map.len() == authored.runtime.depth_map.len());
        if !allocation_compatible {
            return self.compiled_ymp();
        }
        let mut document = base.clone();
        for (target, authored) in document.layers.iter_mut().zip(&self.layers) {
            let map_offset = target.map_offset;
            *target = authored.runtime.clone();
            target.map_offset = map_offset;
        }
        // Encoding performs checked bounds validation while retaining the
        // imported allocation, padding styles, and unrelated layer bytes.
        document.encode()?;
        Ok(document)
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GoopTerrainTriangle {
    pub vertices: [[f32; 3]; 3],
}

/// One triangle decoded from the finalized map-render BMD. Unlike collision
/// triangles, its GX winding and optional vertex normals retain which side is
/// the visible upward surface.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GoopRenderTriangle {
    pub vertices: [[f32; 3]; 3],
    pub normals: Option<[[f32; 3]; 3]>,
}

pub fn whole_terrain_region(triangles: &[GoopTerrainTriangle]) -> Result<(GoopRegion, u16, u16)> {
    let mut min_x = f32::INFINITY;
    let mut min_z = f32::INFINITY;
    let mut max_x = f32::NEG_INFINITY;
    let mut max_z = f32::NEG_INFINITY;
    for vertex in triangles.iter().flat_map(|triangle| triangle.vertices) {
        if !vertex.iter().all(|value| value.is_finite()) {
            continue;
        }
        min_x = min_x.min(vertex[0]);
        min_z = min_z.min(vertex[2]);
        max_x = max_x.max(vertex[0]);
        max_z = max_z.max(vertex[2]);
    }
    if !min_x.is_finite() || !min_z.is_finite() {
        return Err(SceneError::StageExport(
            "terrain has no finite collision triangles".to_string(),
        ));
    }
    min_x = (min_x / GOOP_CELL_SIZE).floor() * GOOP_CELL_SIZE;
    min_z = (min_z / GOOP_CELL_SIZE).floor() * GOOP_CELL_SIZE;
    let cells_x = (((max_x - min_x) / GOOP_CELL_SIZE).ceil() as usize)
        .max(8)
        .next_power_of_two();
    let cells_z = (((max_z - min_z) / GOOP_CELL_SIZE).ceil() as usize)
        .max(4)
        .next_power_of_two();
    if cells_x > GOOP_MAX_DIMENSION || cells_z > GOOP_MAX_DIMENSION {
        return Err(SceneError::StageExport(format!(
            "terrain requires a {cells_x}x{cells_z} goopmap; create smaller regions"
        )));
    }
    Ok((
        GoopRegion {
            min_x,
            min_z,
            max_x: min_x + cells_x as f32 * GOOP_CELL_SIZE,
            max_z: min_z + cells_z as f32 * GOOP_CELL_SIZE,
        },
        cells_x.trailing_zeros() as u16,
        cells_z.trailing_zeros() as u16,
    ))
}

pub fn generate_floor_depth_map(
    triangles: &[GoopTerrainTriangle],
    region: GoopRegion,
    width_log2: u16,
    height_log2: u16,
) -> Result<(f32, Vec<u8>)> {
    let width = 1usize << width_log2;
    let height = 1usize << height_log2;
    if width < 8 || height < 4 || width > GOOP_MAX_DIMENSION || height > GOOP_MAX_DIMENSION {
        return Err(SceneError::StageExport(format!(
            "invalid goopmap dimensions {width}x{height}"
        )));
    }
    let mut samples = vec![None; width * height];
    let mut minimum = f32::INFINITY;
    // Prefer the center height used by runtime stamps. Near a terrain edge,
    // fall back to the four inner quarter points instead of prohibiting the
    // entire 40-unit cell because an expanded outer corner crosses a wall or
    // ledge. Generated pollution geometry is clipped back to fragments which
    // match the chosen encoded depth, so every visible fragment remains
    // cleanable by the runtime layer. Sunshine does not ordinarily call
    // TPollutionObj::updateDepthMap: its sole retail caller is the Bianco
    // pollution-reset event when it swaps a buried-building pollution object.
    // Static authored layers therefore retain this generated YMP at runtime.
    let offsets = [
        [GOOP_CELL_SIZE * 0.5, GOOP_CELL_SIZE * 0.5],
        [GOOP_CELL_SIZE * 0.25, GOOP_CELL_SIZE * 0.25],
        [GOOP_CELL_SIZE * 0.75, GOOP_CELL_SIZE * 0.25],
        [GOOP_CELL_SIZE * 0.25, GOOP_CELL_SIZE * 0.75],
        [GOOP_CELL_SIZE * 0.75, GOOP_CELL_SIZE * 0.75],
    ];
    for y in 0..height {
        for x in 0..width {
            let world_x = region.min_x + x as f32 * GOOP_CELL_SIZE;
            let world_z = region.min_z + y as f32 * GOOP_CELL_SIZE;
            let Some(sample) = offsets.iter().find_map(|offset| {
                topmost_height(triangles, world_x + offset[0], world_z + offset[1])
            }) else {
                continue;
            };
            samples[y * width + x] = Some(sample);
            minimum = minimum.min(sample);
        }
    }
    if !minimum.is_finite() {
        return Err(SceneError::StageExport(
            "the goop region contains no representable floor cells".to_string(),
        ));
    }
    // Keep one encoded step below the lowest terrain. Depth 0 is visible but
    // cannot be changed by TPollutionCounterLayer::drawTexStamp because its
    // strict lower-bound comparison clamps the bound to 0. Reserving this
    // guard band makes every generated floor cell paintable and cleanable.
    let vertical_offset = (minimum / GOOP_DEPTH_WORLD_UNITS_PER_CODE).floor()
        * GOOP_DEPTH_WORLD_UNITS_PER_CODE
        - f32::from(GOOP_MIN_MUTABLE_DEPTH) * GOOP_DEPTH_WORLD_UNITS_PER_CODE;
    let mut layer = YmpLayer {
        layer_type: 0,
        subtype: 0,
        flags: 0,
        reserved: 0,
        vertical_offset,
        vertical_scale: GOOP_CELL_SIZE,
        min_x: region.min_x,
        min_z: region.min_z,
        max_x: region.max_x,
        max_z: region.max_z,
        width_log2,
        height_log2,
        user_value: 0,
        map_offset: 0,
        depth_map: vec![0xff; width * height],
    };
    for y in 0..height {
        for x in 0..width {
            let Some(value) = samples[y * width + x] else {
                continue;
            };
            let depth = ((value - vertical_offset) * 0.025).trunc();
            if !(f32::from(GOOP_MIN_MUTABLE_DEPTH)..=254.0).contains(&depth) {
                return Err(SceneError::StageExport(format!(
                    "terrain height {value} in cell ({x}, {y}) exceeds the YMP 8-bit vertical span from offset {vertical_offset}"
                )));
            }
            layer.set_depth(x, y, depth as u8)?;
        }
    }
    Ok((vertical_offset, layer.depth_map))
}

fn topmost_height(triangles: &[GoopTerrainTriangle], x: f32, z: f32) -> Option<f32> {
    triangles
        .iter()
        .filter_map(|triangle| triangle_height_at(triangle.vertices, x, z))
        .max_by(f32::total_cmp)
}

fn triangle_height_at(vertices: [[f32; 3]; 3], x: f32, z: f32) -> Option<f32> {
    let [a, b, c] = vertices;
    let denominator = (b[2] - c[2]) * (a[0] - c[0]) + (c[0] - b[0]) * (a[2] - c[2]);
    if denominator.abs() <= f32::EPSILON {
        return None;
    }
    let wa = ((b[2] - c[2]) * (x - c[0]) + (c[0] - b[0]) * (z - c[2])) / denominator;
    let wb = ((c[2] - a[2]) * (x - c[0]) + (a[0] - c[0]) * (z - c[2])) / denominator;
    let wc = 1.0 - wa - wb;
    (wa >= -0.0001 && wb >= -0.0001 && wc >= -0.0001).then_some(wa * a[1] + wb * b[1] + wc * c[1])
}

pub fn terrain_fingerprint(model: &[u8], collision: &[u8]) -> u64 {
    model
        .iter()
        .chain(collision)
        .fold(0xcbf2_9ce4_8422_2325, |hash, byte| {
            (hash ^ u64::from(*byte)).wrapping_mul(0x0000_0100_0000_01b3)
        })
}

/// Builds a source-free floor pollution model while retaining the template's
/// active material-zero MAT3 state and every TEX1 texture after texture zero.
pub fn generate_floor_pollution_model(
    template: &J3dRebuildDocument,
    triangles: &[GoopRenderTriangle],
    runtime: &YmpLayer,
    allow_incompatible_template: bool,
) -> Result<J3dRebuildDocument> {
    runtime.validate_floor_runtime()?;
    let (width, height) = runtime.dimensions()?;
    let texture_width = u16::try_from(width).map_err(|_| {
        SceneError::StageExport(format!(
            "goop texture width {width} cannot be represented by J3D"
        ))
    })?;
    let texture_height = u16::try_from(height).map_err(|_| {
        SceneError::StageExport(format!(
            "goop texture height {height} cannot be represented by J3D"
        ))
    })?;
    let region = GoopRegion {
        min_x: runtime.min_x,
        min_z: runtime.min_z,
        max_x: runtime.max_x,
        max_z: runtime.max_z,
    };
    let expected_max_x = region.min_x + width as f32 * runtime.vertical_scale;
    let expected_max_z = region.min_z + height as f32 * runtime.vertical_scale;
    if (region.max_x - expected_max_x).abs() > 0.01 || (region.max_z - expected_max_z).abs() > 0.01
    {
        return Err(SceneError::StageExport(format!(
            "goop model region does not match its {width}x{height} YMP grid at scale {}",
            runtime.vertical_scale
        )));
    }
    let mut material_section = template
        .sections
        .iter()
        .find(|section| matches!(section.data, J3dRebuildSectionData::Materials(_)))
        .cloned()
        .ok_or_else(|| SceneError::StageExport("goop template has no MAT3 section".to_string()))?;
    let texture_section = template
        .sections
        .iter()
        .find_map(|section| match &section.data {
            J3dRebuildSectionData::Textures(textures) => Some(textures),
            _ => None,
        })
        .ok_or_else(|| SceneError::StageExport("goop template has no TEX1 section".to_string()))?;
    let first = texture_section.textures.first().ok_or_else(|| {
        SceneError::StageExport("goop template TEX1 has no first texture".to_string())
    })?;
    if !allow_incompatible_template && first.format != GxTextureFormat::I8 as u8 {
        return Err(SceneError::StageExport(format!(
            "goop template texture zero is GX format {}, expected mutable I8",
            first.format
        )));
    }

    let mut textures = texture_section
        .textures
        .iter()
        .enumerate()
        .map(|(index, record)| {
            let name = texture_section
                .names
                .entries
                .get(index)
                .map_or_else(|| format!("texture{index}"), |entry| entry.name.clone());
            GxEncodedTexture::from_j3d_record(name, record).map_err(SceneError::from)
        })
        .collect::<Result<Vec<_>>>()?;
    let mask_name = textures
        .first()
        .map_or_else(|| "pollution".to_string(), |texture| texture.name.clone());
    let mask = RgbaImage {
        width: texture_width,
        height: texture_height,
        pixels: vec![0; width * height * 4],
    };
    textures[0] = GxEncodedTexture::encode_rgba(
        mask_name,
        &mask,
        GxTextureEncodeOptions {
            encoding: GxTextureEncoding::Exact(GxTextureFormat::I8),
            palette_format: GxPaletteFormat::Ia8,
            mip_count: 1,
            sampler: textures[0].sampler,
        },
    )?;
    let detail_uv_gradients = template_detail_uv_gradients(template)?;
    retain_material_zero_only(&mut material_section)?;
    redirect_detail_tex_gens(&mut material_section, &detail_uv_gradients)?;

    let mut vertices = Vec::new();
    let mut indices = Vec::new();
    for triangle in triangles {
        let Some(surface_vertices) = upward_render_vertices(*triangle) else {
            continue;
        };
        for mut polygon in runtime_safe_surface_polygons(surface_vertices, runtime)? {
            // `compile_static_bmd3` accepts conventional geometric winding and
            // reverses it when emitting GX's clockwise display-list winding.
            // J3D preview exposes GX's clockwise runtime winding. Convert it
            // back before the static compiler emits the final display list.
            if triangle_normal_y(surface_vertices) < 0.0 {
                polygon.reverse();
            }
            for position in &mut polygon {
                position[1] += SURFACE_Y_OFFSET;
            }
            let start = u32::try_from(vertices.len()).map_err(|_| {
                SceneError::StageExport("generated goop mesh has too many vertices".to_string())
            })?;
            for position in polygon.iter().copied() {
                let mut vertex = StaticModelVertex::new(position, [0.0, 1.0, 0.0]);
                vertex.tex_coords[0] = Some([
                    (position[0] - region.min_x) / (region.max_x - region.min_x),
                    (position[2] - region.min_z) / (region.max_z - region.min_z),
                ]);
                for (slot, gradient) in detail_uv_gradients.iter().enumerate().skip(1) {
                    let Some(gradient) = gradient else {
                        continue;
                    };
                    let x = position[0] - region.min_x;
                    let z = position[2] - region.min_z;
                    vertex.tex_coords[slot] = Some([
                        gradient.du_dx * x + gradient.du_dz * z,
                        gradient.dv_dx * x + gradient.dv_dz * z,
                    ]);
                }
                vertices.push(vertex);
            }
            for index in 1..polygon.len() - 1 {
                let triangle = [polygon[0], polygon[index], polygon[index + 1]];
                // Cell/triangle clipping can retain collinear boundary points.
                // A non-zero polygon may therefore contain one zero-area fan
                // triangle, which the static BMD compiler correctly rejects.
                // Drop only that degenerate fan member and retain the rest of
                // the clipped surface.
                if triangle_has_nonzero_area(triangle) {
                    indices.push([start, start + index as u32, start + index as u32 + 1]);
                }
            }
        }
    }
    if indices.is_empty() {
        return Err(SceneError::StageExport(
            "goop region has no upward-facing terrain mesh".to_string(),
        ));
    }
    let mut generated = compile_static_bmd3(&StaticModel {
        root_joint_name: "pollution".to_string(),
        meshes: vec![StaticModelMesh {
            name: "pollution".to_string(),
            material_index: 0,
            vertices,
            triangles: indices,
        }],
        materials: vec![GxMaterial::default()],
        textures: vec![textures[0].clone()],
    })?;
    let textures = compile_texture_section(&textures)?;
    for section in &mut generated.sections {
        match section.data {
            J3dRebuildSectionData::Materials(_) => *section = material_section.clone(),
            J3dRebuildSectionData::Textures(_) => *section = textures.clone(),
            _ => {}
        }
    }
    // Reparse the encoded model so offset, count, and section agreement is
    // checked before it enters the semantic archive.
    let bytes = generated.to_bytes()?;
    J3dRebuildDocument::parse(&bytes)?;
    Ok(generated)
}

fn retain_material_zero_only(material_section: &mut sms_formats::J3dRebuildSection) -> Result<()> {
    const INDIRECT_MATERIAL_RECORD_SIZE: usize = 0x138;
    let J3dRebuildSectionData::Materials(materials) = &mut material_section.data else {
        return Err(SceneError::StageExport(
            "goop template material section is not MAT3".to_string(),
        ));
    };
    let material_zero_init = materials
        .tables
        .iter()
        .find(|table| table.kind == J3dMaterialTableKind::MaterialRemap)
        .and_then(|table| match &table.allocation {
            J3dScalarArray::Unsigned16(remap) => remap.first().copied(),
            _ => None,
        })
        .map(usize::from)
        .ok_or_else(|| {
            SceneError::StageExport("goop template MAT3 has no material-zero remap".to_string())
        })?;
    let active_record = materials
        .material_init_records
        .get(material_zero_init)
        .cloned()
        .ok_or_else(|| {
            SceneError::StageExport(format!(
                "goop template material-zero remap {material_zero_init} is out of bounds"
            ))
        })?;

    materials.material_count = 1;
    materials.material_init_records = vec![active_record];
    let remap = materials
        .tables
        .iter_mut()
        .find(|table| table.kind == J3dMaterialTableKind::MaterialRemap)
        .expect("material-zero remap was found above");
    remap.allocation = J3dScalarArray::Unsigned16(vec![0]);
    if let Some(names) = &mut materials.names {
        names.entries.truncate(1);
    }

    if let Some(indirect) = materials
        .tables
        .iter_mut()
        .find(|table| table.kind == J3dMaterialTableKind::IndirectInit)
    {
        let J3dScalarArray::Unsigned8(bytes) = &mut indirect.allocation else {
            return Err(SceneError::StageExport(
                "goop template MAT3 indirect-material bank is malformed".to_string(),
            ));
        };
        if bytes.len() < INDIRECT_MATERIAL_RECORD_SIZE {
            return Err(SceneError::StageExport(format!(
                "goop template MAT3 indirect-material bank has {} bytes, expected at least {INDIRECT_MATERIAL_RECORD_SIZE}",
                bytes.len()
            )));
        }
        bytes.truncate(INDIRECT_MATERIAL_RECORD_SIZE);
    }
    Ok(())
}

fn triangle_normal_y(vertices: [[f32; 3]; 3]) -> f32 {
    let ab = [
        vertices[1][0] - vertices[0][0],
        vertices[1][1] - vertices[0][1],
        vertices[1][2] - vertices[0][2],
    ];
    let ac = [
        vertices[2][0] - vertices[0][0],
        vertices[2][1] - vertices[0][1],
        vertices[2][2] - vertices[0][2],
    ];
    ab[2] * ac[0] - ab[0] * ac[2]
}

fn triangle_has_nonzero_area([a, b, c]: [[f32; 3]; 3]) -> bool {
    let ab = [b[0] - a[0], b[1] - a[1], b[2] - a[2]];
    let ac = [c[0] - a[0], c[1] - a[1], c[2] - a[2]];
    let cross = [
        ab[1] * ac[2] - ab[2] * ac[1],
        ab[2] * ac[0] - ab[0] * ac[2],
        ab[0] * ac[1] - ab[1] * ac[0],
    ];
    cross.iter().map(|value| value * value).sum::<f32>() > f32::EPSILON
}

const MIN_UPWARD_COMPONENT: f32 = 0.1;
const FLOOR_RUNTIME_DEPTH_TOLERANCE: f32 = 2.0;
// A retail census found a modal +2 world-Y separation between map floors and
// their pollution meshes (Bianco is about +1..2, Delfino/Monte/Ricco are about
// +2, and Airport is higher). The runtime adds no transform, so reproduce the
// conservative retail offset in generated geometry after depth matching.
const SURFACE_Y_OFFSET: f32 = 2.0;

fn upward_render_vertices(triangle: GoopRenderTriangle) -> Option<[[f32; 3]; 3]> {
    let upward_component = if let Some(normals) = triangle.normals {
        let normalized = normals.map(normalize_vector);
        let average = normalized.iter().fold([0.0; 3], |mut average, normal| {
            for axis in 0..3 {
                average[axis] += normal[axis];
            }
            average
        });
        normalize_vector(average)[1]
    } else {
        let [a, b, c] = triangle.vertices;
        let ab = [b[0] - a[0], b[1] - a[1], b[2] - a[2]];
        let ac = [c[0] - a[0], c[1] - a[1], c[2] - a[2]];
        let gx_normal = normalize_vector([
            ab[1] * ac[2] - ab[2] * ac[1],
            ab[2] * ac[0] - ab[0] * ac[2],
            ab[0] * ac[1] - ab[1] * ac[0],
        ]);
        // Effective BMD previews retain GX's clockwise winding, so the
        // geometric normal points opposite the visible vertex normal.
        -gx_normal[1]
    };
    if upward_component <= MIN_UPWARD_COMPONENT {
        return None;
    }
    Some(triangle.vertices)
}

fn normalize_vector(vector: [f32; 3]) -> [f32; 3] {
    let length = vector_length(vector);
    if !length.is_finite() || length <= f32::EPSILON {
        [0.0; 3]
    } else {
        vector.map(|component| component / length)
    }
}

fn vector_length(vector: [f32; 3]) -> f32 {
    (vector[0] * vector[0] + vector[1] * vector[1] + vector[2] * vector[2]).sqrt()
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct PlanarUvGradient {
    du_dx: f32,
    du_dz: f32,
    dv_dx: f32,
    dv_dz: f32,
}

fn template_detail_uv_gradients(
    template: &J3dRebuildDocument,
) -> Result<[Option<PlanarUvGradient>; 8]> {
    let preview = J3dFile::parse(template.to_bytes()?)?.geometry_preview()?;
    let material = preview
        .materials
        .iter()
        .find(|material| material.material_index == 0)
        .ok_or_else(|| SceneError::StageExport("goop template has no material zero".to_string()))?;
    let mut samples: [Vec<PlanarUvGradient>; 8] = std::array::from_fn(|_| Vec::new());
    for triangle in preview
        .triangles
        .iter()
        .filter(|triangle| triangle.material_index == Some(0))
    {
        for (slot, slot_samples) in samples
            .iter_mut()
            .enumerate()
            .take(usize::from(material.tex_gen_count.min(8)))
            .skip(1)
        {
            let source = material.tex_gens[slot].source;
            let Some(source_slot) = source
                .checked_sub(4)
                .map(usize::from)
                .filter(|slot| *slot < 8)
            else {
                continue;
            };
            let Some(tex_coords) = triangle.tex_coord_sets[source_slot] else {
                continue;
            };
            if let Some(gradient) = triangle_planar_uv_gradient(triangle.vertices, tex_coords) {
                slot_samples.push(gradient);
            }
        }
    }

    Ok(std::array::from_fn(|slot| {
        (!samples[slot].is_empty()).then(|| median_uv_gradient(&mut samples[slot]))
    }))
}

fn redirect_detail_tex_gens(
    material_section: &mut sms_formats::J3dRebuildSection,
    gradients: &[Option<PlanarUvGradient>; 8],
) -> Result<()> {
    const J3D_SECTION_ALIGNMENT: u32 = 0x20;
    let original_declared_size = material_section.declared_size;
    if !material_section
        .declared_size
        .is_multiple_of(J3D_SECTION_ALIGNMENT)
    {
        return Err(SceneError::StageExport(format!(
            "goop template MAT3 size {:#x} is not 32-byte aligned",
            material_section.declared_size
        )));
    }
    let (insertion_offset, allocation_growth, layout_growth) = {
        let J3dRebuildSectionData::Materials(materials) = &mut material_section.data else {
            return Err(SceneError::StageExport(
                "goop template material section is not MAT3".to_string(),
            ));
        };
        let material_zero_init = materials
            .tables
            .iter()
            .find(|table| table.kind == J3dMaterialTableKind::MaterialRemap)
            .and_then(|table| match &table.allocation {
                J3dScalarArray::Unsigned16(remap) => remap.first().copied(),
                _ => None,
            })
            .map(usize::from)
            .ok_or_else(|| {
                SceneError::StageExport("goop template MAT3 has no material-zero remap".to_string())
            })?;
        let table_index = materials
            .tables
            .iter()
            .position(|table| table.kind == J3dMaterialTableKind::TexCoord)
            .ok_or_else(|| {
                SceneError::StageExport("goop template MAT3 has no texcoord bank".to_string())
            })?;
        let (table_offset, mut allocation) = match &materials.tables[table_index].allocation {
            J3dScalarArray::Unsigned8(bytes) if bytes.len().is_multiple_of(4) => {
                (materials.tables[table_index].offset, bytes.clone())
            }
            _ => {
                return Err(SceneError::StageExport(
                    "goop template MAT3 texcoord bank is malformed".to_string(),
                ));
            }
        };
        let original_len = allocation.len();
        let record = materials
            .material_init_records
            .get_mut(material_zero_init)
            .ok_or_else(|| {
                SceneError::StageExport(format!(
                    "goop template material-zero remap {material_zero_init} is out of bounds"
                ))
            })?;
        let mask_source_index = record.tex_coord_indices[0];
        if mask_source_index == u16::MAX {
            return Err(SceneError::StageExport(
                "goop template material zero has no mask texcoord generator".to_string(),
            ));
        }
        let mask_source_offset =
            usize::from(mask_source_index)
                .checked_mul(4)
                .ok_or_else(|| {
                    SceneError::StageExport(
                        "goop template MAT3 mask texcoord index overflow".to_string(),
                    )
                })?;
        let mask_source: [u8; 4] = allocation
            .get(mask_source_offset..mask_source_offset + 4)
            .ok_or_else(|| {
                SceneError::StageExport(format!(
                    "goop template MAT3 mask texcoord index {mask_source_index} is out of bounds"
                ))
            })?
            .try_into()
            .expect("checked four-byte texcoord record");
        // Generated texture zero is the mutable pollution mask. Its UVs are
        // already normalized from the layer's world-space X/Z region, so any
        // retail texture matrix would move the visible mask away from the YMP
        // cell that Sunshine stamps. Force TEX0 and GX_IDENTITY while leaving
        // the template's detail channels and visual material state intact.
        let redirected_mask = [mask_source[0], 4, 60, mask_source[3]];
        let redirected_mask_index = allocation
            .chunks_exact(4)
            .position(|entry| entry == redirected_mask)
            .unwrap_or_else(|| {
                let index = allocation.len() / 4;
                allocation.extend_from_slice(&redirected_mask);
                index
            });
        record.tex_coord_indices[0] = u16::try_from(redirected_mask_index).map_err(|_| {
            SceneError::StageExport("goop template MAT3 has too many texcoord entries".to_string())
        })?;
        for (slot, gradient) in gradients.iter().enumerate().skip(1) {
            if gradient.is_none() || record.tex_coord_indices[slot] == u16::MAX {
                continue;
            }
            let source_index = usize::from(record.tex_coord_indices[slot]);
            let source_offset = source_index.checked_mul(4).ok_or_else(|| {
                SceneError::StageExport("goop template MAT3 texcoord index overflow".to_string())
            })?;
            let source = allocation
                .get(source_offset..source_offset + 4)
                .ok_or_else(|| {
                    SceneError::StageExport(format!(
                        "goop template MAT3 texcoord index {source_index} is out of bounds"
                    ))
                })?;
            if !(4..=11).contains(&source[1]) {
                continue;
            }
            let redirected = [source[0], 4 + slot as u8, source[2], source[3]];
            let redirected_index = allocation
                .chunks_exact(4)
                .position(|entry| entry == redirected)
                .unwrap_or_else(|| {
                    let index = allocation.len() / 4;
                    allocation.extend_from_slice(&redirected);
                    index
                });
            record.tex_coord_indices[slot] = u16::try_from(redirected_index).map_err(|_| {
                SceneError::StageExport(
                    "goop template MAT3 has too many texcoord entries".to_string(),
                )
            })?;
        }

        let allocation_growth = allocation.len() - original_len;
        if allocation_growth != 0 {
            let insertion_offset = table_offset
                .checked_add(u32::try_from(original_len).map_err(|_| {
                    SceneError::StageExport(
                        "goop template MAT3 texcoord bank is too large".to_string(),
                    )
                })?)
                .ok_or_else(|| {
                    SceneError::StageExport(
                        "goop template MAT3 texcoord offset overflow".to_string(),
                    )
                })?;
            let allocation_growth = u32::try_from(allocation_growth).map_err(|_| {
                SceneError::StageExport("goop template MAT3 expansion is too large".to_string())
            })?;
            let layout_growth = allocation_growth
                .checked_add(J3D_SECTION_ALIGNMENT - 1)
                .map(|value| value / J3D_SECTION_ALIGNMENT * J3D_SECTION_ALIGNMENT)
                .ok_or_else(|| {
                    SceneError::StageExport(
                        "goop template MAT3 aligned expansion overflow".to_string(),
                    )
                })?;
            for offset in &mut materials.offsets {
                if *offset >= insertion_offset {
                    *offset = offset.checked_add(allocation_growth).ok_or_else(|| {
                        SceneError::StageExport(
                            "goop template MAT3 table offset overflow".to_string(),
                        )
                    })?;
                }
            }
            for (index, table) in materials.tables.iter_mut().enumerate() {
                if index != table_index && table.offset >= insertion_offset {
                    table.offset =
                        table.offset.checked_add(allocation_growth).ok_or_else(|| {
                            SceneError::StageExport(
                                "goop template MAT3 allocation offset overflow".to_string(),
                            )
                        })?;
                }
            }
            materials.tables[table_index].allocation = J3dScalarArray::Unsigned8(allocation);
            (insertion_offset, allocation_growth, layout_growth)
        } else {
            (0, 0, 0)
        }
    };

    if layout_growth != 0 {
        material_section.declared_size = material_section
            .declared_size
            .checked_add(layout_growth)
            .ok_or_else(|| {
                SceneError::StageExport("goop template MAT3 size overflow".to_string())
            })?;
        for span in &mut material_section.padding {
            let span_end = span.offset.saturating_add(span.length);
            if span.offset >= insertion_offset {
                span.offset = span.offset.checked_add(allocation_growth).ok_or_else(|| {
                    SceneError::StageExport(
                        "goop template MAT3 padding offset overflow".to_string(),
                    )
                })?;
            } else if span_end > insertion_offset {
                return Err(SceneError::StageExport(
                    "goop template MAT3 texcoord bank overlaps an imported padding span"
                        .to_string(),
                ));
            }
        }
        if allocation_growth < layout_growth {
            material_section.padding.push(J3dPaddingSpan {
                offset: original_declared_size + allocation_growth,
                length: layout_growth - allocation_growth,
                kind: J3dPaddingKind::Zero,
            });
            material_section
                .padding
                .sort_unstable_by_key(|span| span.offset);
        }
        if !material_section
            .declared_size
            .is_multiple_of(J3D_SECTION_ALIGNMENT)
        {
            return Err(SceneError::StageExport(format!(
                "generated goop MAT3 size {:#x} lost 32-byte alignment",
                material_section.declared_size
            )));
        }
    }
    Ok(())
}

fn triangle_planar_uv_gradient(
    positions: [[f32; 3]; 3],
    tex_coords: [[f32; 2]; 3],
) -> Option<PlanarUvGradient> {
    let dx1 = positions[1][0] - positions[0][0];
    let dz1 = positions[1][2] - positions[0][2];
    let dx2 = positions[2][0] - positions[0][0];
    let dz2 = positions[2][2] - positions[0][2];
    let determinant = dx1 * dz2 - dx2 * dz1;
    if !determinant.is_finite() || determinant.abs() <= 0.0001 {
        return None;
    }
    let du1 = tex_coords[1][0] - tex_coords[0][0];
    let du2 = tex_coords[2][0] - tex_coords[0][0];
    let dv1 = tex_coords[1][1] - tex_coords[0][1];
    let dv2 = tex_coords[2][1] - tex_coords[0][1];
    let gradient = PlanarUvGradient {
        du_dx: (du1 * dz2 - du2 * dz1) / determinant,
        du_dz: (dx1 * du2 - dx2 * du1) / determinant,
        dv_dx: (dv1 * dz2 - dv2 * dz1) / determinant,
        dv_dz: (dx1 * dv2 - dx2 * dv1) / determinant,
    };
    [
        gradient.du_dx,
        gradient.du_dz,
        gradient.dv_dx,
        gradient.dv_dz,
    ]
    .into_iter()
    .all(f32::is_finite)
    .then_some(gradient)
}

fn median_uv_gradient(samples: &mut [PlanarUvGradient]) -> PlanarUvGradient {
    fn median(samples: &[PlanarUvGradient], value: impl Fn(PlanarUvGradient) -> f32) -> f32 {
        let mut values = samples.iter().copied().map(value).collect::<Vec<_>>();
        values.sort_unstable_by(f32::total_cmp);
        values[values.len() / 2]
    }

    PlanarUvGradient {
        du_dx: median(samples, |sample| sample.du_dx),
        du_dz: median(samples, |sample| sample.du_dz),
        dv_dx: median(samples, |sample| sample.dv_dx),
        dv_dz: median(samples, |sample| sample.dv_dz),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct GoopCellRect {
    min_x: usize,
    min_y: usize,
    max_x: usize,
    max_y: usize,
}

fn runtime_safe_surface_polygons(
    vertices: [[f32; 3]; 3],
    runtime: &YmpLayer,
) -> Result<Vec<Vec<[f32; 3]>>> {
    const CELL_OUTSIDE: u8 = 0;
    const CELL_ACCEPTED: u8 = 1;
    const CELL_REJECTED: u8 = 2;

    let (width, height) = runtime.dimensions()?;
    let Some(bounds) = triangle_cell_bounds(vertices, runtime, width, height) else {
        return Ok(Vec::new());
    };
    let cell_width = bounds.max_x - bounds.min_x;
    let cell_height = bounds.max_y - bounds.min_y;
    let mut cells = vec![CELL_OUTSIDE; cell_width * cell_height];
    for y in bounds.min_y..bounds.max_y {
        for x in bounds.min_x..bounds.max_x {
            let polygon = clip_triangle_to_region(vertices, runtime_cell_region(runtime, x, y));
            if polygon_projected_area_twice(&polygon).abs() <= 0.0001 {
                continue;
            }
            let accepted = runtime.depth_at(x, y)? != 0xff
                && polygon
                    .iter()
                    .all(|vertex| runtime_depth_matches_surface(runtime, x, y, vertex[1]));
            cells[(y - bounds.min_y) * cell_width + (x - bounds.min_x)] = if accepted {
                CELL_ACCEPTED
            } else {
                CELL_REJECTED
            };
        }
    }

    let accepted_prefix = cell_status_prefix(&cells, cell_width, cell_height, CELL_ACCEPTED);
    let rejected_prefix = cell_status_prefix(&cells, cell_width, cell_height, CELL_REJECTED);
    let mut accepted_rectangles = Vec::new();
    collect_accepted_cell_rectangles(
        GoopCellRect {
            min_x: 0,
            min_y: 0,
            max_x: cell_width,
            max_y: cell_height,
        },
        cell_width + 1,
        &accepted_prefix,
        &rejected_prefix,
        &mut accepted_rectangles,
    );

    Ok(accepted_rectangles
        .into_iter()
        .filter_map(|rectangle| {
            let global = GoopCellRect {
                min_x: rectangle.min_x + bounds.min_x,
                min_y: rectangle.min_y + bounds.min_y,
                max_x: rectangle.max_x + bounds.min_x,
                max_y: rectangle.max_y + bounds.min_y,
            };
            let polygon =
                clip_triangle_to_region(vertices, runtime_cell_rectangle_region(runtime, global));
            (polygon_projected_area_twice(&polygon).abs() > 0.0001).then_some(polygon)
        })
        .collect())
}

fn triangle_cell_bounds(
    vertices: [[f32; 3]; 3],
    runtime: &YmpLayer,
    width: usize,
    height: usize,
) -> Option<GoopCellRect> {
    let min_x = vertices
        .iter()
        .map(|vertex| vertex[0])
        .min_by(f32::total_cmp)?;
    let max_x = vertices
        .iter()
        .map(|vertex| vertex[0])
        .max_by(f32::total_cmp)?;
    let min_z = vertices
        .iter()
        .map(|vertex| vertex[2])
        .min_by(f32::total_cmp)?;
    let max_z = vertices
        .iter()
        .map(|vertex| vertex[2])
        .max_by(f32::total_cmp)?;
    let scale = runtime.vertical_scale;
    let min_cell_x =
        (((min_x - runtime.min_x) / scale).floor() as isize).clamp(0, width as isize) as usize;
    let max_cell_x =
        (((max_x - runtime.min_x) / scale).ceil() as isize).clamp(0, width as isize) as usize;
    let min_cell_y =
        (((min_z - runtime.min_z) / scale).floor() as isize).clamp(0, height as isize) as usize;
    let max_cell_y =
        (((max_z - runtime.min_z) / scale).ceil() as isize).clamp(0, height as isize) as usize;
    (min_cell_x < max_cell_x && min_cell_y < max_cell_y).then_some(GoopCellRect {
        min_x: min_cell_x,
        min_y: min_cell_y,
        max_x: max_cell_x,
        max_y: max_cell_y,
    })
}

fn runtime_cell_region(runtime: &YmpLayer, x: usize, y: usize) -> GoopRegion {
    runtime_cell_rectangle_region(
        runtime,
        GoopCellRect {
            min_x: x,
            min_y: y,
            max_x: x + 1,
            max_y: y + 1,
        },
    )
}

fn runtime_cell_rectangle_region(runtime: &YmpLayer, cells: GoopCellRect) -> GoopRegion {
    GoopRegion {
        min_x: runtime.min_x + cells.min_x as f32 * runtime.vertical_scale,
        min_z: runtime.min_z + cells.min_y as f32 * runtime.vertical_scale,
        max_x: runtime.min_x + cells.max_x as f32 * runtime.vertical_scale,
        max_z: runtime.min_z + cells.max_y as f32 * runtime.vertical_scale,
    }
}

fn runtime_depth_matches_surface(runtime: &YmpLayer, x: usize, y: usize, surface_y: f32) -> bool {
    let Ok(depth) = runtime.depth_at(x, y) else {
        return false;
    };
    if depth == 0xff {
        return false;
    }
    // TPollutionPos::worldToDepth always uses 0.025, independently of the
    // horizontal grid scale, and returns int with C++ truncation toward zero.
    let encoded = ((surface_y - runtime.vertical_offset) * 0.025).trunc();
    encoded.is_finite()
        && encoded >= f32::from(depth) - FLOOR_RUNTIME_DEPTH_TOLERANCE
        && encoded <= f32::from(depth) + FLOOR_RUNTIME_DEPTH_TOLERANCE
}

fn polygon_projected_area_twice(polygon: &[[f32; 3]]) -> f32 {
    if polygon.len() < 3 {
        return 0.0;
    }
    polygon
        .iter()
        .zip(polygon.iter().cycle().skip(1))
        .take(polygon.len())
        .map(|(left, right)| left[0] * right[2] - right[0] * left[2])
        .sum()
}

fn cell_status_prefix(cells: &[u8], width: usize, height: usize, status: u8) -> Vec<usize> {
    let stride = width + 1;
    let mut prefix = vec![0; stride * (height + 1)];
    for y in 0..height {
        for x in 0..width {
            prefix[(y + 1) * stride + x + 1] =
                prefix[y * stride + x + 1] + prefix[(y + 1) * stride + x] - prefix[y * stride + x]
                    + usize::from(cells[y * width + x] == status);
        }
    }
    prefix
}

fn prefix_cell_count(prefix: &[usize], stride: usize, rectangle: GoopCellRect) -> usize {
    prefix[rectangle.max_y * stride + rectangle.max_x]
        + prefix[rectangle.min_y * stride + rectangle.min_x]
        - prefix[rectangle.min_y * stride + rectangle.max_x]
        - prefix[rectangle.max_y * stride + rectangle.min_x]
}

fn collect_accepted_cell_rectangles(
    rectangle: GoopCellRect,
    prefix_stride: usize,
    accepted_prefix: &[usize],
    rejected_prefix: &[usize],
    output: &mut Vec<GoopCellRect>,
) {
    let accepted = prefix_cell_count(accepted_prefix, prefix_stride, rectangle);
    if accepted == 0 {
        return;
    }
    if prefix_cell_count(rejected_prefix, prefix_stride, rectangle) == 0 {
        output.push(rectangle);
        return;
    }

    let width = rectangle.max_x - rectangle.min_x;
    let height = rectangle.max_y - rectangle.min_y;
    if width >= height && width > 1 {
        let middle = rectangle.min_x + width / 2;
        collect_accepted_cell_rectangles(
            GoopCellRect {
                max_x: middle,
                ..rectangle
            },
            prefix_stride,
            accepted_prefix,
            rejected_prefix,
            output,
        );
        collect_accepted_cell_rectangles(
            GoopCellRect {
                min_x: middle,
                ..rectangle
            },
            prefix_stride,
            accepted_prefix,
            rejected_prefix,
            output,
        );
    } else if height > 1 {
        let middle = rectangle.min_y + height / 2;
        collect_accepted_cell_rectangles(
            GoopCellRect {
                max_y: middle,
                ..rectangle
            },
            prefix_stride,
            accepted_prefix,
            rejected_prefix,
            output,
        );
        collect_accepted_cell_rectangles(
            GoopCellRect {
                min_y: middle,
                ..rectangle
            },
            prefix_stride,
            accepted_prefix,
            rejected_prefix,
            output,
        );
    } else {
        debug_assert_eq!(accepted, 1, "a mixed status cannot fit in one cell");
        output.push(rectangle);
    }
}

fn clip_triangle_to_region(vertices: [[f32; 3]; 3], region: GoopRegion) -> Vec<[f32; 3]> {
    let mut polygon = vertices.to_vec();
    for (axis, boundary, keep_greater) in [
        (0, region.min_x, true),
        (0, region.max_x, false),
        (2, region.min_z, true),
        (2, region.max_z, false),
    ] {
        if polygon.is_empty() {
            break;
        }
        let input = std::mem::take(&mut polygon);
        let mut previous = *input.last().expect("non-empty clipped polygon");
        let mut previous_inside = if keep_greater {
            previous[axis] >= boundary
        } else {
            previous[axis] <= boundary
        };
        for current in input {
            let current_inside = if keep_greater {
                current[axis] >= boundary
            } else {
                current[axis] <= boundary
            };
            if current_inside != previous_inside {
                let denominator = current[axis] - previous[axis];
                if denominator.abs() > f32::EPSILON {
                    let t = (boundary - previous[axis]) / denominator;
                    polygon.push([
                        previous[0] + (current[0] - previous[0]) * t,
                        previous[1] + (current[1] - previous[1]) * t,
                        previous[2] + (current[2] - previous[2]) * t,
                    ]);
                }
            }
            if current_inside {
                polygon.push(current);
            }
            previous = current;
            previous_inside = current_inside;
        }
    }
    polygon
}

impl StageDocument {
    pub fn ensure_goop_authoring(&mut self) -> Result<&mut GoopAuthoringDocument> {
        if self.goop_authoring.is_none() {
            let Some(StageResourceDocument::PollutionMap(ymp)) =
                self.effective_resource_clone(GOOP_RESOURCE_PATH)?
            else {
                self.goop_authoring = Some(GoopAuthoringDocument::default());
                return Ok(self
                    .goop_authoring
                    .as_mut()
                    .expect("goop authoring inserted"));
            };
            let mut layers = Vec::with_capacity(ymp.layers.len());
            for (index, runtime) in ymp.layers.into_iter().enumerate() {
                let stem = pollution_stem(index, &self.stage_id);
                let bitmap_path = format!("map/pollution/{stem}.bmp");
                let bitmap = match self.effective_resource_clone(bitmap_path.as_bytes())? {
                    Some(StageResourceDocument::Bitmap(bitmap)) => Some(bitmap),
                    _ => None,
                };
                let plane = GoopPlane::from_runtime_code(runtime.flags)
                    .expect("all runtime plane codes have a lossless representation");
                layers.push(GoopLayerAuthoring {
                    id: format!("goop-layer-{index:02}"),
                    runtime_index: index,
                    origin: GoopLayerOrigin::Imported,
                    plane,
                    behavior: GoopBehavior::from_runtime_code(runtime.layer_type),
                    visible: true,
                    region: GoopRegion {
                        min_x: runtime.min_x,
                        min_z: runtime.min_z,
                        max_x: runtime.max_x,
                        max_z: runtime.max_z,
                    },
                    runtime,
                    bitmap,
                    generated_model: None,
                    style_source: None,
                    resource_stem: stem,
                    metadata_dirty: false,
                });
            }
            self.goop_authoring = Some(GoopAuthoringDocument {
                format_version: GOOP_AUTHORING_FORMAT_VERSION,
                layers,
                terrain_fingerprint: 0,
                stale: false,
            });
        }
        let authoring = self
            .goop_authoring
            .as_mut()
            .expect("goop authoring inserted");
        if authoring.format_version < GOOP_AUTHORING_FORMAT_VERSION {
            // Earlier generators used collision geometry for the visible BMD;
            // those meshes can be culled, displaced, or projected onto walls.
            // Mark every older generated layer for the normal template-backed
            // rebuild, which preserves/reprojects its authored mask.
            authoring.stale |= authoring
                .layers
                .iter()
                .any(|layer| layer.origin == GoopLayerOrigin::Generated);
            if !authoring
                .layers
                .iter()
                .any(|layer| layer.origin == GoopLayerOrigin::Generated)
            {
                authoring.format_version = GOOP_AUTHORING_FORMAT_VERSION;
            }
        }
        Ok(authoring)
    }

    pub fn compile_goop_authoring(&mut self) -> Result<()> {
        let Some(authoring) = self.goop_authoring.clone() else {
            return Ok(());
        };
        let compilable = authoring.for_resource_compilation();
        compilable.validate()?;
        if compilable
            .layers
            .iter()
            .any(|layer| layer.origin == GoopLayerOrigin::Generated || layer.metadata_dirty)
        {
            let compiled = match self.effective_resource_clone(GOOP_RESOURCE_PATH)? {
                Some(StageResourceDocument::PollutionMap(base)) => {
                    compilable.compiled_ymp_preserving(&base)?
                }
                _ => compilable.compiled_ymp()?,
            };
            self.upsert_authored_resource(
                GOOP_RESOURCE_PATH.to_vec(),
                StageResourceDocument::PollutionMap(compiled),
            );
        }
        for layer in compilable.layers {
            if let Some(bitmap) = layer.bitmap {
                self.upsert_authored_resource(
                    format!("map/pollution/{}.bmp", layer.resource_stem).into_bytes(),
                    StageResourceDocument::Bitmap(bitmap),
                );
            }
            if let Some(model) = layer.generated_model {
                self.upsert_authored_resource(
                    format!("map/pollution/{}.bmd", layer.resource_stem).into_bytes(),
                    StageResourceDocument::Model(model),
                );
            }
        }
        Ok(())
    }

    /// Updates only the bitmap resource changed by an interactive paint stroke.
    ///
    /// The layer layout, YMP metadata, and generated model are unchanged while
    /// painting, so recompiling every authored goop resource here would clone
    /// and re-index the complete stage overlay on mouse release.
    pub fn compile_goop_layer_mask(&mut self, layer_index: usize) -> Result<()> {
        let (resource_stem, bitmap) = self
            .goop_authoring
            .as_ref()
            .and_then(|authoring| authoring.layers.get(layer_index))
            .and_then(|layer| {
                layer
                    .bitmap
                    .as_ref()
                    .map(|bitmap| (layer.resource_stem.clone(), bitmap.clone()))
            })
            .ok_or_else(|| {
                SceneError::StageExport(format!("goop layer {layer_index} has no editable bitmap"))
            })?;
        self.archive_edits.upsert_resource(
            format!("map/pollution/{resource_stem}.bmp").into_bytes(),
            StageResourceDocument::Bitmap(bitmap),
        );
        Ok(())
    }

    pub fn effective_terrain_fingerprint(&self) -> Result<u64> {
        let model = self
            .effective_resource_clone(b"map/map/map.bmd")?
            .map(|resource| resource.to_bytes())
            .transpose()?
            .unwrap_or_default();
        let collision = self
            .effective_resource_clone(b"map/map.col")?
            .or(self.effective_resource_clone(b"map/map/map.col")?)
            .map(|resource| resource.to_bytes())
            .transpose()?
            .unwrap_or_default();
        Ok(terrain_fingerprint(&model, &collision))
    }

    pub fn refresh_goop_stale_status(&mut self) -> Result<()> {
        let fingerprint = self.effective_terrain_fingerprint()?;
        if let Some(authoring) = &mut self.goop_authoring {
            if authoring
                .layers
                .iter()
                .any(|layer| layer.origin == GoopLayerOrigin::Generated)
                && authoring.terrain_fingerprint != 0
                && authoring.terrain_fingerprint != fingerprint
            {
                authoring.stale = true;
            }
        }
        Ok(())
    }
}

fn pollution_stem(index: usize, stage_id: &str) -> String {
    if stage_id.to_ascii_lowercase().starts_with("mare") {
        match index {
            7 => return "pollutionA".to_string(),
            8 => return "pollutionB".to_string(),
            _ => {}
        }
    }
    format!("pollution{index:02}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn flat(size: f32, height: f32) -> Vec<GoopTerrainTriangle> {
        vec![
            GoopTerrainTriangle {
                vertices: [[0.0, height, 0.0], [size, height, 0.0], [0.0, height, size]],
            },
            GoopTerrainTriangle {
                vertices: [
                    [size, height, 0.0],
                    [size, height, size],
                    [0.0, height, size],
                ],
            },
        ]
    }

    fn flat_rectangle(
        min_x: f32,
        min_z: f32,
        max_x: f32,
        max_z: f32,
        height: f32,
    ) -> Vec<GoopTerrainTriangle> {
        vec![
            GoopTerrainTriangle {
                vertices: [
                    [min_x, height, min_z],
                    [max_x, height, min_z],
                    [min_x, height, max_z],
                ],
            },
            GoopTerrainTriangle {
                vertices: [
                    [max_x, height, min_z],
                    [max_x, height, max_z],
                    [min_x, height, max_z],
                ],
            },
        ]
    }

    fn render_flat(size: f32, height: f32) -> Vec<GoopRenderTriangle> {
        flat(size, height)
            .into_iter()
            .map(|triangle| GoopRenderTriangle {
                vertices: triangle.vertices,
                normals: Some([[0.0, 1.0, 0.0]; 3]),
            })
            .collect()
    }

    fn mask_material() -> GxMaterial {
        let mut material = GxMaterial {
            tex_gen_count: 1,
            tev_stage_count: 1,
            ..GxMaterial::default()
        };
        material.tex_gens[0] = Some(sms_formats::GxTexCoordGen {
            function: 1,
            source: 4,
            matrix: 60,
        });
        material.texture_numbers[0] = Some(0);
        material.tev_orders[0] = Some(sms_formats::GxTevOrder {
            tex_coord: Some(0),
            tex_map: Some(0),
            color_channel: 0xff,
        });
        material
    }

    fn floor_runtime(
        region: GoopRegion,
        width_log2: u16,
        height_log2: u16,
        vertical_offset: f32,
        depth: u8,
    ) -> YmpLayer {
        let width = 1usize << width_log2;
        let height = 1usize << height_log2;
        YmpLayer {
            layer_type: 0,
            subtype: 0,
            flags: GoopPlane::Floor.runtime_code(),
            reserved: 0,
            vertical_offset,
            vertical_scale: GOOP_CELL_SIZE,
            min_x: region.min_x,
            min_z: region.min_z,
            max_x: region.max_x,
            max_z: region.max_z,
            width_log2,
            height_log2,
            user_value: 0,
            map_offset: 0,
            depth_map: vec![depth; width * height],
        }
    }

    #[test]
    fn paint_stroke_compiles_only_its_bitmap_without_serializing_the_overlay() {
        let region = GoopRegion {
            min_x: 0.0,
            min_z: 0.0,
            max_x: 320.0,
            max_z: 160.0,
        };
        let mut layer = GoopLayerAuthoring {
            id: "painted".to_string(),
            runtime_index: 0,
            origin: GoopLayerOrigin::Generated,
            plane: GoopPlane::Floor,
            behavior: GoopBehavior::Normal,
            visible: true,
            region,
            runtime: floor_runtime(region, 3, 2, 0.0, 1),
            bitmap: Some(BmpFile::new_pollution_mask(8, 4, vec![0; 32]).unwrap()),
            generated_model: None,
            style_source: None,
            resource_stem: "pollution00".to_string(),
            metadata_dirty: false,
        };
        let mut painted = vec![0; 32];
        painted[11] = 192;
        layer.set_mask(&painted).unwrap();
        let mut document = StageDocument {
            stage_id: "test".to_string(),
            base_root: std::path::PathBuf::new(),
            assets: Vec::new(),
            objects: Vec::new(),
            changed_files: std::collections::BTreeMap::new(),
            stage_archive: None,
            stage_archive_source_path: None,
            archive_edits: Default::default(),
            registry: None,
            route_authoring: None,
            goop_authoring: Some(GoopAuthoringDocument {
                layers: vec![layer],
                ..Default::default()
            }),
            load_issues: Vec::new(),
            lighting: Default::default(),
            actor_previews: std::collections::BTreeMap::new(),
            loaded_project: None,
        };

        document.compile_goop_layer_mask(0).unwrap();

        assert!(document.changed_files.is_empty());
        assert_eq!(document.archive_edits.resources.len(), 1);
        let edit = &document.archive_edits.resources[0];
        assert_eq!(edit.raw_resource_path, b"map/pollution/pollution00.bmp");
        let StageResourceDocument::Bitmap(bitmap) = &edit.document else {
            panic!("paint stroke compiled a non-bitmap resource");
        };
        assert_eq!(bitmap.top_down_indices().unwrap(), painted);
    }

    #[test]
    fn stale_release_gate_does_not_block_resource_compilation() {
        let authoring = GoopAuthoringDocument {
            format_version: GOOP_AUTHORING_FORMAT_VERSION - 1,
            layers: Vec::new(),
            terrain_fingerprint: 123,
            stale: true,
        };
        assert!(authoring.validate().is_err());

        let compilable = authoring.for_resource_compilation();
        assert!(compilable.validate().is_ok());
        assert_eq!(compilable.format_version, GOOP_AUTHORING_FORMAT_VERSION);
        assert!(!compilable.stale);
        assert!(authoring.stale);
    }

    #[test]
    fn clipped_polygon_fans_drop_collinear_triangles() {
        assert!(!triangle_has_nonzero_area([
            [0.0, 0.0, 0.0],
            [20.0, 0.0, 0.0],
            [40.0, 0.0, 0.0],
        ]));
        assert!(triangle_has_nonzero_area([
            [0.0, 0.0, 0.0],
            [40.0, 0.0, 0.0],
            [40.0, 0.0, 40.0],
        ]));
    }

    #[test]
    fn whole_region_is_cell_aligned_and_power_of_two() {
        let (region, width, height) = whole_terrain_region(&flat(300.0, 80.0)).unwrap();
        assert_eq!((width, height), (3, 3));
        assert_eq!(region.min_x, 0.0);
        assert_eq!(region.max_x, 320.0);
    }

    #[test]
    fn floor_generator_uses_tiled_depth_and_reserves_mutable_zero_guard_band() {
        let triangles = flat(400.0, 125.0);
        let region = GoopRegion {
            min_x: 0.0,
            min_z: 0.0,
            max_x: 320.0,
            max_z: 160.0,
        };
        let (offset, depth_map) = generate_floor_depth_map(&triangles, region, 3, 2).unwrap();
        assert_eq!(offset, 80.0);
        let layer = YmpLayer {
            layer_type: 0,
            subtype: 0,
            flags: 0,
            reserved: 0,
            vertical_offset: offset,
            vertical_scale: 40.0,
            min_x: 0.0,
            min_z: 0.0,
            max_x: 320.0,
            max_z: 160.0,
            width_log2: 3,
            height_log2: 2,
            user_value: 0,
            map_offset: 0,
            depth_map,
        };
        assert_eq!(layer.depth_at(1, 1).unwrap(), GOOP_MIN_MUTABLE_DEPTH);
    }

    #[test]
    fn floor_generator_keeps_partial_cells_at_terrain_edges() {
        let region = GoopRegion {
            min_x: 0.0,
            min_z: 0.0,
            max_x: 320.0,
            max_z: 160.0,
        };

        // The center is outside this small surface, but the first inner
        // quarter point (10, 10) is supported. Expanded outer-corner sampling
        // used to reject the entire cell and leave a square hole near walls.
        let triangles = flat_rectangle(5.0, 5.0, 15.0, 15.0, 125.0);
        let (offset, depth_map) = generate_floor_depth_map(&triangles, region, 3, 2).unwrap();
        let layer = YmpLayer {
            layer_type: 0,
            subtype: 0,
            flags: 0,
            reserved: 0,
            vertical_offset: offset,
            vertical_scale: GOOP_CELL_SIZE,
            min_x: region.min_x,
            min_z: region.min_z,
            max_x: region.max_x,
            max_z: region.max_z,
            width_log2: 3,
            height_log2: 2,
            user_value: 0,
            map_offset: 0,
            depth_map,
        };
        assert_eq!(offset, 80.0);
        assert_eq!(layer.depth_at(0, 0).unwrap(), GOOP_MIN_MUTABLE_DEPTH);
        assert_eq!(layer.depth_at(1, 0).unwrap(), 0xff);
    }

    #[test]
    fn floor_generator_prefers_the_center_topmost_floor() {
        let region = GoopRegion {
            min_x: 0.0,
            min_z: 0.0,
            max_x: 320.0,
            max_z: 160.0,
        };
        let mut triangles = flat(400.0, 0.0);
        triangles.extend(flat_rectangle(15.0, 15.0, 25.0, 25.0, 200.0));
        let (offset, depth_map) = generate_floor_depth_map(&triangles, region, 3, 2).unwrap();
        let layer = YmpLayer {
            layer_type: 0,
            subtype: 0,
            flags: 0,
            reserved: 0,
            vertical_offset: offset,
            vertical_scale: GOOP_CELL_SIZE,
            min_x: region.min_x,
            min_z: region.min_z,
            max_x: region.max_x,
            max_z: region.max_z,
            width_log2: 3,
            height_log2: 2,
            user_value: 0,
            map_offset: 0,
            depth_map,
        };
        assert_eq!(offset, -40.0);
        assert_eq!(layer.depth_at(0, 0).unwrap(), 6);
        assert_eq!(layer.depth_at(1, 0).unwrap(), GOOP_MIN_MUTABLE_DEPTH);
    }

    #[test]
    fn floor_generator_selects_the_topmost_stacked_floor() {
        let mut triangles = flat(400.0, 80.0);
        triangles.extend(flat(400.0, 205.0));
        let region = GoopRegion {
            min_x: 0.0,
            min_z: 0.0,
            max_x: 320.0,
            max_z: 160.0,
        };
        let (offset, depth_map) = generate_floor_depth_map(&triangles, region, 3, 2).unwrap();
        assert_eq!(offset, 160.0);
        let mut layer = YmpLayer {
            layer_type: 0,
            subtype: 0,
            flags: 0,
            reserved: 0,
            vertical_offset: offset,
            vertical_scale: 40.0,
            min_x: region.min_x,
            min_z: region.min_z,
            max_x: region.max_x,
            max_z: region.max_z,
            width_log2: 3,
            height_log2: 2,
            user_value: 0,
            map_offset: 0,
            depth_map,
        };
        assert_eq!(layer.depth_at(1, 1).unwrap(), GOOP_MIN_MUTABLE_DEPTH);
        layer.set_depth(1, 1, 12).unwrap();
        assert_eq!(layer.depth_at(1, 1).unwrap(), 12);
    }

    #[test]
    fn floor_generator_rejects_unrepresentable_vertical_span() {
        let mut triangles = vec![
            GoopTerrainTriangle {
                vertices: [
                    [-20.0, 0.0, -20.0],
                    [140.0, 0.0, -20.0],
                    [-20.0, 0.0, 200.0],
                ],
            },
            GoopTerrainTriangle {
                vertices: [
                    [140.0, 0.0, -20.0],
                    [140.0, 0.0, 200.0],
                    [-20.0, 0.0, 200.0],
                ],
            },
        ];
        triangles.extend([
            GoopTerrainTriangle {
                vertices: [
                    [160.0, 11_000.0, -20.0],
                    [400.0, 11_000.0, -20.0],
                    [160.0, 11_000.0, 200.0],
                ],
            },
            GoopTerrainTriangle {
                vertices: [
                    [400.0, 11_000.0, -20.0],
                    [400.0, 11_000.0, 200.0],
                    [160.0, 11_000.0, 200.0],
                ],
            },
        ]);
        let region = GoopRegion {
            min_x: 0.0,
            min_z: 0.0,
            max_x: 320.0,
            max_z: 160.0,
        };
        assert!(generate_floor_depth_map(&triangles, region, 3, 2).is_err());
    }

    #[test]
    fn pollution_model_keeps_template_material_and_replaces_texture_zero_with_i8() {
        let texture = GxEncodedTexture::encode_rgba(
            "retail_goop",
            &RgbaImage {
                width: 8,
                height: 4,
                pixels: vec![0; 8 * 4 * 4],
            },
            GxTextureEncodeOptions {
                encoding: GxTextureEncoding::Exact(GxTextureFormat::I8),
                ..GxTextureEncodeOptions::default()
            },
        )
        .unwrap();
        let template = compile_static_bmd3(&StaticModel {
            root_joint_name: "template".to_string(),
            meshes: vec![StaticModelMesh {
                name: "template".to_string(),
                material_index: 0,
                vertices: vec![
                    StaticModelVertex::new([0.0, 0.0, 0.0], [0.0, 1.0, 0.0]),
                    StaticModelVertex::new([320.0, 0.0, 0.0], [0.0, 1.0, 0.0]),
                    StaticModelVertex::new([0.0, 0.0, 160.0], [0.0, 1.0, 0.0]),
                ],
                triangles: vec![[0, 1, 2]],
            }],
            materials: vec![mask_material()],
            textures: vec![texture],
        })
        .unwrap();
        let region = GoopRegion {
            min_x: 0.0,
            min_z: 0.0,
            max_x: 640.0,
            max_z: 320.0,
        };
        let runtime = floor_runtime(region, 4, 3, 0.0, 0);
        let generated =
            generate_floor_pollution_model(&template, &render_flat(700.0, 0.0), &runtime, false)
                .unwrap();
        let template_material = template
            .sections
            .iter()
            .find(|section| matches!(section.data, J3dRebuildSectionData::Materials(_)))
            .unwrap();
        let generated_material = generated
            .sections
            .iter()
            .find(|section| matches!(section.data, J3dRebuildSectionData::Materials(_)))
            .unwrap();
        assert_eq!(generated_material, template_material);
        let first_texture = generated
            .sections
            .iter()
            .find_map(|section| match &section.data {
                J3dRebuildSectionData::Textures(textures) => textures.textures.first(),
                _ => None,
            })
            .unwrap();
        assert_eq!(first_texture.format, GxTextureFormat::I8 as u8);
        assert_eq!((first_texture.width, first_texture.height), (16, 8));
    }

    #[test]
    fn pollution_model_keeps_only_upward_render_faces_and_separates_them_from_the_map() {
        let texture = GxEncodedTexture::encode_rgba(
            "retail_goop",
            &RgbaImage {
                width: 8,
                height: 4,
                pixels: vec![0; 8 * 4 * 4],
            },
            GxTextureEncodeOptions {
                encoding: GxTextureEncoding::Exact(GxTextureFormat::I8),
                ..GxTextureEncodeOptions::default()
            },
        )
        .unwrap();
        let template = compile_static_bmd3(&StaticModel {
            root_joint_name: "template".to_string(),
            meshes: vec![StaticModelMesh {
                name: "template".to_string(),
                material_index: 0,
                vertices: vec![
                    StaticModelVertex::new([0.0, 0.0, 0.0], [0.0, 1.0, 0.0]),
                    StaticModelVertex::new([320.0, 0.0, 0.0], [0.0, 1.0, 0.0]),
                    StaticModelVertex::new([0.0, 0.0, 160.0], [0.0, 1.0, 0.0]),
                ],
                triangles: vec![[0, 2, 1]],
            }],
            materials: vec![mask_material()],
            textures: vec![texture],
        })
        .unwrap();
        let region = GoopRegion {
            min_x: 0.0,
            min_z: 0.0,
            max_x: 320.0,
            max_z: 160.0,
        };

        let upward = GoopRenderTriangle {
            vertices: [[0.0, 0.0, 0.0], [320.0, 0.0, 0.0], [0.0, 0.0, 160.0]],
            normals: Some([[0.0, 1.0, 0.0]; 3]),
        };
        let wall = GoopRenderTriangle {
            vertices: [[0.0, 0.0, 0.0], [0.0, 100.0, 0.0], [0.0, 0.0, 160.0]],
            normals: Some([[1.0, 0.0, 0.0]; 3]),
        };
        let runtime = floor_runtime(region, 3, 2, 0.0, 0);
        let generated =
            generate_floor_pollution_model(&template, &[upward, wall], &runtime, false).unwrap();
        let preview = sms_formats::J3dFile::parse(generated.to_bytes().unwrap())
            .unwrap()
            .geometry_preview()
            .unwrap();
        assert_eq!(preview.triangles.len(), 1);
        assert!(preview.triangles.iter().all(|triangle| {
            triangle_normal_y(triangle.vertices) < 0.0
                && triangle.cull_mode == Some(2)
                && triangle
                    .vertices
                    .iter()
                    .all(|vertex| (vertex[1] - 2.0).abs() <= f32::EPSILON)
        }));

        let underside = GoopRenderTriangle {
            vertices: [upward.vertices[0], upward.vertices[2], upward.vertices[1]],
            normals: Some([[0.0, -1.0, 0.0]; 3]),
        };
        assert!(generate_floor_pollution_model(&template, &[underside], &runtime, false).is_err());
    }

    #[test]
    fn pollution_model_prunes_orphan_materials_and_separates_detail_uvs() {
        let encode_texture = |name: &str| {
            GxEncodedTexture::encode_rgba(
                name,
                &RgbaImage {
                    width: 8,
                    height: 4,
                    pixels: vec![0; 8 * 4 * 4],
                },
                GxTextureEncodeOptions {
                    encoding: GxTextureEncoding::Exact(GxTextureFormat::I8),
                    ..GxTextureEncodeOptions::default()
                },
            )
            .unwrap()
        };
        let mut material = GxMaterial {
            name: "goop-material-zero".to_string(),
            tex_gen_count: 3,
            tev_stage_count: 3,
            ..GxMaterial::default()
        };
        material.tex_gens[0] = Some(sms_formats::GxTexCoordGen {
            function: 1,
            source: 4,
            matrix: 30,
        });
        material.tex_matrices[0] = Some(sms_formats::GxTexMatrix {
            scale: [2.0, 3.0],
            translation: [0.25, -0.125],
            ..sms_formats::GxTexMatrix::default()
        });
        material.tex_gens[1] = Some(sms_formats::GxTexCoordGen {
            function: 1,
            source: 4,
            matrix: 60,
        });
        material.tex_gens[2] = Some(sms_formats::GxTexCoordGen {
            function: 1,
            source: 4,
            matrix: 60,
        });
        material.texture_numbers[0] = Some(0);
        material.texture_numbers[1] = Some(1);
        material.texture_numbers[2] = Some(2);
        material.tev_orders[0] = Some(sms_formats::GxTevOrder {
            tex_coord: Some(0),
            tex_map: Some(0),
            color_channel: 0xff,
        });
        material.tev_orders[1] = Some(sms_formats::GxTevOrder {
            tex_coord: Some(1),
            tex_map: Some(1),
            color_channel: 0xff,
        });
        material.tev_orders[2] = Some(sms_formats::GxTevOrder {
            tex_coord: Some(2),
            tex_map: Some(2),
            color_channel: 0xff,
        });
        material.tev_stages[1] = Some(sms_formats::GxTevStage::default());
        material.tev_stages[2] = Some(sms_formats::GxTevStage::default());

        let mut template_vertices = vec![
            StaticModelVertex::new([0.0, 0.0, 0.0], [0.0, 1.0, 0.0]),
            StaticModelVertex::new([320.0, 0.0, 0.0], [0.0, 1.0, 0.0]),
            StaticModelVertex::new([0.0, 0.0, 160.0], [0.0, 1.0, 0.0]),
        ];
        template_vertices[0].tex_coords[0] = Some([0.0, 0.0]);
        template_vertices[1].tex_coords[0] = Some([4.0, 0.0]);
        template_vertices[2].tex_coords[0] = Some([0.0, 2.0]);
        let mut unrelated_vertices = template_vertices.clone();
        unrelated_vertices[0].tex_coords[0] = Some([0.0, 0.0]);
        unrelated_vertices[1].tex_coords[0] = Some([400.0, 0.0]);
        unrelated_vertices[2].tex_coords[0] = Some([0.0, 200.0]);
        let mut unrelated_material = material.clone();
        unrelated_material.name = "unrelated-material-one".to_string();
        let template = compile_static_bmd3(&StaticModel {
            root_joint_name: "template".to_string(),
            meshes: vec![
                StaticModelMesh {
                    name: "template".to_string(),
                    material_index: 0,
                    vertices: template_vertices,
                    triangles: vec![[0, 1, 2]],
                },
                StaticModelMesh {
                    name: "unrelated".to_string(),
                    material_index: 1,
                    vertices: unrelated_vertices,
                    triangles: vec![[0, 1, 2]],
                },
            ],
            materials: vec![material, unrelated_material],
            textures: vec![
                encode_texture("mask"),
                encode_texture("detail"),
                encode_texture("detail-two"),
            ],
        })
        .unwrap();
        let region = GoopRegion {
            min_x: 0.0,
            min_z: 0.0,
            max_x: 320.0,
            max_z: 160.0,
        };
        let runtime = floor_runtime(region, 3, 2, 0.0, 0);
        let generated =
            generate_floor_pollution_model(&template, &render_flat(320.0, 0.0), &runtime, false)
                .unwrap();
        assert!(generated
            .sections
            .iter()
            .all(|section| section.declared_size.is_multiple_of(0x20)));
        let generated_material_section = generated
            .sections
            .iter()
            .find(|section| matches!(section.data, J3dRebuildSectionData::Materials(_)))
            .unwrap();
        assert!(generated_material_section
            .padding
            .iter()
            .any(|span| { span.offset + span.length == generated_material_section.declared_size }));
        let preview = J3dFile::parse(generated.to_bytes().unwrap())
            .unwrap()
            .geometry_preview()
            .unwrap();
        let material = preview
            .materials
            .iter()
            .find(|material| material.material_index == 0)
            .unwrap();
        assert_eq!(preview.materials.len(), 1);
        assert_eq!(material.name, "goop-material-zero");
        assert!(preview
            .triangles
            .iter()
            .all(|triangle| triangle.material_index == Some(0)));
        assert_eq!(material.tex_gens[0].source, 4);
        assert_eq!(material.tex_gens[0].matrix, 60);
        assert_eq!(material.texture_indices[1], Some(1));
        assert_eq!(material.texture_indices[2], Some(2));
        assert_eq!(material.tex_gens[1].source, 5);
        assert_eq!(material.tex_gens[2].source, 6);
        for slot in [1, 2] {
            let detail_span = preview
                .triangles
                .iter()
                .filter_map(|triangle| triangle.tex_coord_sets[slot])
                .flatten()
                .map(|uv| uv[0])
                .fold((f32::INFINITY, f32::NEG_INFINITY), |(min, max), value| {
                    (min.min(value), max.max(value))
                });
            assert!(detail_span.1 - detail_span.0 >= 3.9);
            assert!(detail_span.1 - detail_span.0 < 5.0);
        }
        assert!(preview.triangles.iter().all(|triangle| {
            triangle.tex_coord_sets[0].is_some_and(|coords| {
                coords
                    .into_iter()
                    .all(|uv| (-0.001..=1.001).contains(&uv[0]))
            })
        }));
        for triangle in &preview.triangles {
            let mask_coords = triangle.tex_coord_sets[0]
                .expect("generated geometry keeps normalized mask coordinates");
            for (position, uv) in triangle.vertices.into_iter().zip(mask_coords) {
                let expected = [position[0] / 320.0, position[2] / 160.0];
                assert!((uv[0] - expected[0]).abs() < 0.0001);
                assert!((uv[1] - expected[1]).abs() < 0.0001);
            }
        }
    }

    #[test]
    fn runtime_surface_matching_uses_inclusive_depth_window_and_cpp_truncation() {
        let region = GoopRegion {
            min_x: 0.0,
            min_z: 0.0,
            max_x: 320.0,
            max_z: 160.0,
        };
        let runtime = floor_runtime(region, 3, 2, 100.0, 5);
        assert!(runtime_depth_matches_surface(&runtime, 0, 0, 220.0));
        assert!(runtime_depth_matches_surface(&runtime, 0, 0, 380.0));
        assert!(!runtime_depth_matches_surface(&runtime, 0, 0, 219.0));
        assert!(!runtime_depth_matches_surface(&runtime, 0, 0, 420.0));

        let zero = floor_runtime(region, 3, 2, 100.0, 0);
        assert!(runtime_depth_matches_surface(&zero, 0, 0, 80.0));
        assert!(!runtime_depth_matches_surface(&zero, 0, 0, -20.0));
    }

    #[test]
    fn runtime_surface_clipping_removes_lower_geometry_beneath_a_topmost_cell() {
        let region = GoopRegion {
            min_x: 0.0,
            min_z: 0.0,
            max_x: 320.0,
            max_z: 160.0,
        };
        let mut runtime = floor_runtime(region, 3, 2, 0.0, 0);
        runtime.set_depth(2, 1, 4).unwrap();
        let lower = [[0.0, 0.0, 0.0], [320.0, 0.0, 0.0], [0.0, 0.0, 160.0]];
        let polygons = runtime_safe_surface_polygons(lower, &runtime).unwrap();
        assert!(!polygons.is_empty());
        assert!(polygons.len() < 20);
        for polygon in polygons {
            let center_x =
                polygon.iter().map(|vertex| vertex[0]).sum::<f32>() / polygon.len() as f32;
            let center_z =
                polygon.iter().map(|vertex| vertex[2]).sum::<f32>() / polygon.len() as f32;
            assert!(!(80.0 < center_x && center_x < 120.0 && 40.0 < center_z && center_z < 80.0));
        }

        let upper = [
            [80.0, 160.0, 40.0],
            [120.0, 160.0, 40.0],
            [80.0, 160.0, 80.0],
        ];
        assert!(!runtime_safe_surface_polygons(upper, &runtime)
            .unwrap()
            .is_empty());
    }
}
