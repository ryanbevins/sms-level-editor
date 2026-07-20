//! Source-free model and collision authoring for Super Mario Sunshine.
//!
//! The crate deliberately stops at a normalized, editable representation. The
//! low-level BMD/MAT3/TEX1 compiler lives at the format boundary, while this
//! crate owns secure glTF ingestion and semantic Sunshine collision output.

mod bootstrap;
mod catalog;
mod collision;
mod compile;
mod error;
mod import;
mod math;
mod merge;
mod model;
mod native;

pub use bootstrap::built_in_blank_stage_proxy;
pub use catalog::*;
pub use collision::{CollisionCleanupReport, CollisionImportResult, CollisionSimplificationReport};
pub use compile::{
    decode_bmd3, decode_canonical_bmd3, BmdCompileReport, BmdSectionSize, CompiledBmd,
};
pub use error::{AuthoringError, AuthoringResult, Diagnostic, DiagnosticCode, Severity};
pub use import::{import_collision, import_model};
pub use merge::{
    merge_model_instances, ModelInstanceExportMode, ModelInstancePlacement, ResolvedModelInstance,
};
pub use model::*;
pub use sms_formats::{
    GxAlphaCompare, GxBlendMode, GxColorChannel, GxDepthMode, GxDiagnosticSeverity,
    GxEncodedTexture, GxFog, GxIndirectMaterial, GxIndirectMatrix, GxIndirectOrder,
    GxIndirectScale, GxIndirectTevStage, GxLight, GxMaterial, GxMaterialDiagnostic, GxNbtScale,
    GxPaletteFormat, GxSampler, GxTevOrder, GxTevStage, GxTevSwapMode, GxTevSwapTable,
    GxTexCoordGen, GxTexMatrix, GxTextureEncodeOptions, GxTextureEncoding, GxTextureFormat,
    RgbaImage, TargetLoaderProfile,
};
