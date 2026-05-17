use std::path::PathBuf;

use kithara_decode::DecodeError;
use url::Url;

use crate::impls::config::ResourceSrc;

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
