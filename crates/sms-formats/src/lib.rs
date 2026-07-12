//! Exact-preserving format readers for Super Mario Sunshine editor workflows.
//!
//! The first editor milestone treats unsupported data conservatively: parse
//! enough structure to identify files and render summaries, but keep the source
//! bytes so open/save roundtrips remain byte-identical unless a later editing
//! layer explicitly rewrites a known format.

mod binary;
mod col;
mod j3d;
mod j3d_anim;
mod jdrama;
mod rarc;
mod raw;
mod stage_assets;
mod yaz0;

pub use col::{ColFile, ColHeader};
pub use j3d::{
    decode_bti_texture, J3dAlphaCompare, J3dBlendMode, J3dColorChannel, J3dFile, J3dFog,
    J3dGeometryPreview, J3dHeader, J3dIndirectMaterial, J3dIndirectMatrix, J3dIndirectOrder,
    J3dIndirectScale, J3dIndirectTevStage, J3dJointTransformOverride, J3dMaterial,
    J3dMaterialRenderState, J3dMatrix34, J3dPreviewCombineMode, J3dSection, J3dTevOrder,
    J3dTevStage, J3dTexGen, J3dTexMatrix, J3dTextureMipPreview, J3dTexturePreview, J3dTriangle,
    J3dZMode, SMS_DEFAULT_OBJECT_MODEL_LOAD_FLAGS, SMS_MAP_MODEL_LOAD_FLAGS,
    SMS_POLLUTION_MODEL_LOAD_FLAGS,
};
pub use j3d_anim::{
    J3dJointAnimation, J3dJointTransform, J3dTexturePatternAnimation, J3dTexturePatternBinding,
    J3dTextureSrt, J3dTextureSrtAnimation, J3dTextureSrtBinding,
};
pub use jdrama::{
    parse_jdrama_object_records, JDramaMapEventBuilding, JDramaMapEventSinkParams, JDramaNpcParams,
    JDramaObjectRecord, JDramaTransform,
};
pub use rarc::{RarcArchive, RarcFileEntry, RarcHeader};
pub use raw::{RawFile, RawFormat};
pub use stage_assets::{
    discover_scene_archives, extract_archive_file, mount_scene_archive, read_stage_asset_bytes,
    scan_stage_assets, SceneArchiveInfo, StageAsset, StageAssetKind,
};
pub use yaz0::{decode_yaz0, Yaz0File};

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum FormatError {
    #[error("file is too small for {format}: expected at least {expected} bytes, got {actual}")]
    TooSmall {
        format: &'static str,
        expected: usize,
        actual: usize,
    },
    #[error("bad magic for {format}: expected {expected:?}, got {actual:?}")]
    BadMagic {
        format: &'static str,
        expected: &'static [u8],
        actual: Vec<u8>,
    },
    #[error("invalid offset in {format}: {offset:#x} with length {len:#x}")]
    InvalidOffset {
        format: &'static str,
        offset: usize,
        len: usize,
    },
    #[error("unsupported {format} feature: {message}")]
    Unsupported {
        format: &'static str,
        message: String,
    },
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

pub type Result<T> = std::result::Result<T, FormatError>;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceLocation {
    pub path: PathBuf,
    pub offset: Option<u64>,
    pub length: Option<u64>,
}

pub trait PreserveBytes {
    fn source_bytes(&self) -> &[u8];

    fn to_bytes(&self) -> Vec<u8> {
        self.source_bytes().to_vec()
    }
}

pub fn detect_raw_format(bytes: &[u8]) -> RawFormat {
    if bytes.starts_with(b"Yaz0") {
        RawFormat::Yaz0
    } else if bytes.starts_with(b"RARC") {
        RawFormat::Rarc
    } else if bytes.starts_with(b"J3D2") {
        RawFormat::J3d
    } else if bytes.starts_with(b"MESGbmg1") {
        RawFormat::Bmg
    } else {
        RawFormat::Unknown
    }
}
