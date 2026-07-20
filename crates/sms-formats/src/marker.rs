use serde::{Deserialize, Serialize};

use crate::{FormatError, Result};

const FORMAT: &str = "stage marker text";

/// Line-ending convention of a tiny stage marker file such as `delete.me`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum MarkerLineEnding {
    None,
    LineFeed,
    CarriageReturnLineFeed,
}

/// Source-free semantic representation of the ASCII marker files shipped in
/// stage archives. These are authoring/build markers, not arbitrary blobs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MarkerTextFile {
    pub text: String,
    pub line_ending: MarkerLineEnding,
}

impl MarkerTextFile {
    pub fn parse(bytes: impl AsRef<[u8]>) -> Result<Self> {
        let bytes = bytes.as_ref();
        let (body, line_ending) = if let Some(body) = bytes.strip_suffix(b"\r\n") {
            (body, MarkerLineEnding::CarriageReturnLineFeed)
        } else if let Some(body) = bytes.strip_suffix(b"\n") {
            (body, MarkerLineEnding::LineFeed)
        } else {
            (bytes, MarkerLineEnding::None)
        };
        if body.is_empty()
            || !body
                .iter()
                .all(|byte| byte.is_ascii_graphic() || *byte == b' ')
        {
            return Err(FormatError::Unsupported {
                format: FORMAT,
                message: "marker must contain non-empty printable ASCII text".to_string(),
            });
        }
        Ok(Self {
            text: String::from_utf8(body.to_vec()).expect("validated ASCII marker"),
            line_ending,
        })
    }

    pub fn encode(&self) -> Result<Vec<u8>> {
        if self.text.is_empty()
            || !self
                .text
                .as_bytes()
                .iter()
                .all(|byte| byte.is_ascii_graphic() || *byte == b' ')
        {
            return Err(FormatError::Unsupported {
                format: FORMAT,
                message: "marker must contain non-empty printable ASCII text".to_string(),
            });
        }
        let mut bytes = self.text.as_bytes().to_vec();
        match self.line_ending {
            MarkerLineEnding::None => {}
            MarkerLineEnding::LineFeed => bytes.push(b'\n'),
            MarkerLineEnding::CarriageReturnLineFeed => bytes.extend_from_slice(b"\r\n"),
        }
        Ok(bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn marker_rebuilds_semantically() {
        let mut source = b"dummy\r\n".to_vec();
        let marker = MarkerTextFile::parse(&source).unwrap();
        source.fill(0xA5);
        assert_eq!(marker.text, "dummy");
        assert_eq!(marker.encode().unwrap(), b"dummy\r\n");
    }

    #[test]
    #[ignore = "requires SMS_BASE_ROOT with extracted retail stage archives"]
    fn source_free_rebuilds_every_retail_stage_marker() {
        let root = std::env::var_os("SMS_BASE_ROOT")
            .map(std::path::PathBuf::from)
            .expect("set SMS_BASE_ROOT to an extracted retail game root");
        let archives = crate::discover_scene_archives(root).expect("discover stage archives");
        let mut rebuilt = 0usize;
        for archive in archives {
            for asset in crate::mount_scene_archive(&archive.path)
                .unwrap_or_else(|error| panic!("mount {}: {error}", archive.path.display()))
            {
                if !asset
                    .path
                    .to_string_lossy()
                    .to_ascii_lowercase()
                    .ends_with(".me")
                {
                    continue;
                }
                let source = crate::read_stage_asset_bytes(&asset.path)
                    .unwrap_or_else(|error| panic!("read {}: {error}", asset.path.display()));
                let document = MarkerTextFile::parse(&source)
                    .unwrap_or_else(|error| panic!("parse {}: {error}", asset.path.display()));
                assert_eq!(
                    document.encode().expect("encode semantic marker"),
                    source,
                    "source-free marker rebuild differs for {}",
                    asset.path.display()
                );
                rebuilt += 1;
            }
        }
        assert_eq!(rebuilt, 492, "retail marker count drifted");
        eprintln!("source-free marker census rebuilt {rebuilt} files");
    }
}
