use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use thiserror::Error;

pub type AuthoringResult<T> = Result<T, AuthoringError>;

#[derive(Debug, Error)]
pub enum AuthoringError {
    #[error("could not read {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("invalid glTF: {0}")]
    Gltf(String),
    #[error("invalid model asset: {0}")]
    Invalid(String),
    #[error("unsupported glTF feature ({code}): {message}")]
    Unsupported { code: String, message: String },
    #[error("unsafe glTF resource ({code}): {message}")]
    Security { code: String, message: String },
    #[error("native model serialization failed: {0}")]
    Serialization(#[from] serde_json::Error),
    #[error("collision cannot be represented as Sunshine COL: {0}")]
    Collision(String),
    #[error("model compilation failed: {0}")]
    Compile(String),
}

impl AuthoringError {
    pub(crate) fn io(path: impl Into<PathBuf>, source: std::io::Error) -> Self {
        Self::Io {
            path: path.into(),
            source,
        }
    }

    pub(crate) fn invalid(message: impl Into<String>) -> Self {
        Self::Invalid(message.into())
    }

    pub(crate) fn unsupported(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self::Unsupported {
            code: code.into(),
            message: message.into(),
        }
    }

    pub(crate) fn security(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self::Security {
            code: code.into(),
            message: message.into(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    Info,
    Warning,
    Error,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiagnosticCode {
    MultipleScenes,
    GeneratedNormals,
    EmptyPrimitive,
    UnmappedMetallicRoughness,
    UnmappedNormalTexture,
    UnmappedOcclusionTexture,
    UnmappedEmissive,
    TargetIgnoresFeature,
    CollisionDegenerateRemoved,
    CollisionDuplicateRemoved,
    CollisionVertexWelded,
    CollisionUnusedVertexRemoved,
    CoordinateSpaceMigrated,
    CollisionWindingNormalized,
    CollisionSimplified,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Diagnostic {
    pub severity: Severity,
    pub code: DiagnosticCode,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context: Option<String>,
    #[serde(default)]
    pub acknowledgement_required: bool,
}

impl Diagnostic {
    pub(crate) fn info(
        code: DiagnosticCode,
        message: impl Into<String>,
        context: Option<String>,
    ) -> Self {
        Self {
            severity: Severity::Info,
            code,
            message: message.into(),
            context,
            acknowledgement_required: false,
        }
    }

    pub(crate) fn warning(
        code: DiagnosticCode,
        message: impl Into<String>,
        context: Option<String>,
        acknowledgement_required: bool,
    ) -> Self {
        Self {
            severity: Severity::Warning,
            code,
            message: message.into(),
            context,
            acknowledgement_required,
        }
    }
}
