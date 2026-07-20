//! Checked readers and semantic writers for Super Mario Sunshine editor flows.
//!
//! Renderer-oriented legacy readers may retain source bytes for inexpensive
//! previews. The authoring documents exported by the `*_rebuild` modules and
//! strict stage pipeline do not: they either reconstruct a resource from typed
//! data or reject it, so exactness can never come from an opaque fallback.

mod bas;
mod binary;
mod bmg;
mod bmp;
mod bti;
mod col;
mod gx_material;
mod gx_texture;
mod j3d;
mod j3d_anim;
mod j3d_anim_rebuild;
mod j3d_rebuild;
mod j3d_static;
mod jdrama;
mod jpa;
mod jpa_rebuild;
mod mario_record;
mod marker;
mod prm;
mod rarc;
mod raw;
mod stage_assets;
mod stage_misc;
mod yaz0;

pub use bas::{BasFile, BasSoundCue};
pub use bmg::{BmgEntry, BmgFile, BmgMessage, BmgMessageToken};
pub use bmp::BmpFile;
pub use bti::BtiFile;
pub use col::{ColFile, ColGroup, ColHeader, ColTriangle, ColVertex};
pub use gx_material::*;
pub use gx_texture::*;
pub use j3d::{
    decode_bti_texture, J3dAlphaCompare, J3dBillboard, J3dBillboardMode, J3dBlendMode,
    J3dColorChannel, J3dFile, J3dFog, J3dGeometryPreview, J3dHeader, J3dIndirectMaterial,
    J3dIndirectMatrix, J3dIndirectOrder, J3dIndirectScale, J3dIndirectTevStage,
    J3dJointTransformOverride, J3dMaterial, J3dMaterialRenderState, J3dMatrix34,
    J3dPreparedAnimatedTriangles, J3dPreviewCombineMode, J3dSection, J3dTevOrder, J3dTevStage,
    J3dTexGen, J3dTexMatrix, J3dTextureMipPreview, J3dTexturePreview, J3dTriangle, J3dZMode,
    SMS_DEFAULT_OBJECT_MODEL_LOAD_FLAGS, SMS_MAP_MODEL_LOAD_FLAGS, SMS_POLLUTION_MODEL_LOAD_FLAGS,
    SMS_SM_J3D_ACT_MODEL_LOAD_FLAGS,
};
pub use j3d_anim::{
    J3dJointAnimation, J3dJointTransform, J3dTexturePatternAnimation, J3dTexturePatternBinding,
    J3dTextureSrt, J3dTextureSrtAnimation, J3dTextureSrtBinding, SMS_ANIMATION_FRAMES_PER_SECOND,
};
pub use j3d_anim_rebuild::*;
pub use j3d_rebuild::*;
pub use j3d_static::*;
pub use jdrama::{
    append_jdrama_scenario_archive_slot, effective_npc_parts_mask, encode_jdrama_document,
    ensure_jdrama_scenario_archive_slot, jdrama_key_code, parse_jdrama_document,
    parse_jdrama_object_records, parse_jdrama_scenario_archive_areas,
    parse_jdrama_scenario_archive_entries, JDramaAmbient, JDramaCubeGeneralInfo, JDramaDocument,
    JDramaField, JDramaFieldValue, JDramaLight, JDramaLightMap, JDramaLightMapEntry,
    JDramaMapEventBuilding, JDramaMapEventSinkParams, JDramaNpcParams, JDramaObjectRecord,
    JDramaRecord, JDramaRecordPayload, JDramaScenarioArchiveArea, JDramaScenarioArchiveEntry,
    JDramaScenarioArchiveWriteOutcome, JDramaTransform, SMS_AUTHORED_RUNTIME_CARRIER_AREAS,
};
pub use jpa::{
    JpaBaseShape, JpaChildShape, JpaColorAnimation, JpaColorKey, JpaEffect, JpaEmitter,
    JpaExtraShape, JpaField, JpaKeyframeCurve, JpaRawBlock,
};
pub use jpa_rebuild::*;
pub use mario_record::{MarioRecordBundle, MarioRecordChannel, MarioRecordFile, MarioRecordRun};
pub use marker::{MarkerLineEnding, MarkerTextFile};
pub use prm::{PrmEntry, PrmFile, PrmValue};
pub use rarc::{
    rarc_name_hash, RarcArchive, RarcBuilder, RarcDocument, RarcEntryRecord, RarcFileEntry,
    RarcHeader, RarcLayout, RarcNodeRecord,
};
pub use raw::{RawFile, RawFormat};
pub use stage_assets::{
    discover_scene_archives, extract_archive_file, mount_scene_archive, read_stage_asset_bytes,
    scan_common_stage_assets, scan_stage_assets, SceneArchiveInfo, StageAsset, StageAssetKind,
};
pub use stage_misc::*;
pub use yaz0::{decode_yaz0, encode_yaz0, encode_yaz0_with_reserved, Yaz0Document, Yaz0File};

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
    #[error(
        "resource limit exceeded while parsing {format}: {resource} requested {requested} bytes/items, limit is {limit}"
    )]
    ResourceLimit {
        format: &'static str,
        resource: &'static str,
        requested: usize,
        limit: usize,
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
