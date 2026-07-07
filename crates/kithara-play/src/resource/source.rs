use std::{fmt, path::PathBuf};

use kithara_decode::DecodeError;
use url::Url;

use super::config::ResourceConfig;

/// Source of an audio resource: either a URL or a local file path.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ResourceSrc {
    /// Remote resource accessed via URL (HTTP/HTTPS, or other schemes).
    Url(Url),
    /// Local file accessed directly from disk.
    Path(PathBuf),
}

impl From<Url> for ResourceSrc {
    fn from(url: Url) -> Self {
        Self::Url(url)
    }
}

impl From<PathBuf> for ResourceSrc {
    fn from(path: PathBuf) -> Self {
        Self::Path(path)
    }
}

impl fmt::Display for ResourceSrc {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Url(url) => write!(f, "{url}"),
            Self::Path(path) => write!(f, "{}", path.display()),
        }
    }
}

pub(crate) fn parse_src<S: AsRef<str>>(input: S) -> Result<ResourceSrc, DecodeError> {
    let trimmed = input.as_ref().trim();

    match Url::parse(trimmed) {
        #[cfg(not(target_arch = "wasm32"))]
        Ok(url) if url.scheme() == "file" => {
            let path = url.to_file_path().map_err(|()| DecodeError::InvalidData {
                detail: "invalid file URL",
            })?;
            Ok(ResourceSrc::Path(path))
        }
        #[cfg(target_arch = "wasm32")]
        Ok(url) if url.scheme() == "file" => Err(DecodeError::InvalidData {
            detail: "file:// URL is not supported on wasm",
        }),
        Ok(url) => Ok(ResourceSrc::Url(url)),
        Err(_) => {
            let path = PathBuf::from(trimmed);
            if !path.is_absolute() {
                return Err(DecodeError::InvalidData {
                    detail: "invalid URL or file path (must be absolute)",
                });
            }
            Ok(ResourceSrc::Path(path))
        }
    }
}

impl ResourceConfig {
    /// Parse a URL string or local file path into a [`ResourceSrc`].
    ///
    /// Tries URL parsing first. On failure, falls back to absolute file path.
    /// A `file://` URL is normalized to a `Path`.
    ///
    /// # Errors
    ///
    /// Returns `DecodeError::InvalidData` if the input is an invalid `file://`
    /// URL or a non-absolute file path.
    pub fn parse_src<S: AsRef<str>>(input: S) -> Result<ResourceSrc, DecodeError> {
        parse_src(input)
    }
}

/// Detected source type from input.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SourceType {
    /// Remote progressive file (MP3, AAC, FLAC, WAV, etc.)
    RemoteFile(Url),
    /// Local file on disk.
    LocalFile(PathBuf),
    /// HLS stream (URL ending with `.m3u8`).
    HlsStream(Url),
}

impl SourceType {
    /// Detect source type from a `ResourceSrc`.
    ///
    /// - `ResourceSrc::Path` -> local file
    /// - URL ending with `.m3u8` -> HLS
    /// - Other URLs -> progressive file download
    ///
    /// # Errors
    ///
    /// Returns `DecodeError` if detection fails (currently never — kept for
    /// API symmetry with future format probes).
    pub fn detect(src: &ResourceSrc) -> Result<Self, DecodeError> {
        match src {
            ResourceSrc::Path(path) => Ok(Self::LocalFile(path.clone())),
            ResourceSrc::Url(url) => {
                if url.path().ends_with(".m3u8") {
                    Ok(Self::HlsStream(url.clone()))
                } else {
                    Ok(Self::RemoteFile(url.clone()))
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::*;

    fn source_tag(source: &SourceType) -> &'static str {
        match source {
            SourceType::LocalFile(_) => "local",
            SourceType::RemoteFile(_) => "remote",
            SourceType::HlsStream(_) => "hls",
        }
    }

    #[kithara::test]
    #[case(ResourceSrc::Path(PathBuf::from("/tmp/song.mp3")), "local")]
    #[case(
        ResourceSrc::Url(Url::parse("https://example.com/song.mp3").expect("BUG: valid URL")),
        "remote"
    )]
    fn detect_file_sources(#[case] src: ResourceSrc, #[case] expected: &str) {
        let detected = SourceType::detect(&src).expect("BUG: source must be detected");
        assert_eq!(source_tag(&detected), expected);
    }

    #[kithara::test]
    #[case("https://example.com/playlist.m3u8")]
    #[case("https://example.com/live/index.m3u8")]
    fn detect_hls_url(#[case] url: &str) {
        let src = ResourceSrc::Url(Url::parse(url).expect("BUG: valid URL"));
        let detected = SourceType::detect(&src).expect("BUG: source must be detected");
        assert_eq!(source_tag(&detected), "hls");
    }

    #[kithara::test]
    fn detect_invalid_relative_path() {
        let src = ResourceSrc::Path(PathBuf::from("relative/path.mp3"));
        let result = SourceType::detect(&src);
        assert_eq!(
            source_tag(&result.expect("BUG: relative path must map to local")),
            "local"
        );
    }
}
