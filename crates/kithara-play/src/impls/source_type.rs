//! Detected source type from input analysis.

use std::path::PathBuf;

use url::Url;

use crate::impls::config::ResourceSrc;

/// Detected source type from input.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SourceType {
    /// Remote progressive file (MP3, AAC, FLAC, WAV, etc.)
    #[cfg(feature = "file")]
    RemoteFile(Url),
    /// Local file on disk.
    #[cfg(feature = "file")]
    LocalFile(PathBuf),
    /// HLS stream (URL ending with `.m3u8`).
    #[cfg(feature = "hls")]
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
    /// Returns `DecodeError` if no suitable feature is enabled for the given source.
    pub fn detect(src: &ResourceSrc) -> Result<Self, kithara_decode::DecodeError> {
        match src {
            #[cfg(feature = "file")]
            ResourceSrc::Path(path) => Ok(Self::LocalFile(path.clone())),

            ResourceSrc::Url(url) => {
                #[cfg(feature = "hls")]
                if url.path().ends_with(".m3u8") {
                    return Ok(Self::HlsStream(url.clone()));
                }

                #[cfg(feature = "file")]
                return Ok(Self::RemoteFile(url.clone()));

                #[cfg(not(feature = "file"))]
                Err(kithara_decode::DecodeError::DecodeError(
                    "no suitable feature enabled for this URL (enable `file` or `hls`)".to_string(),
                ))
            }

            #[cfg(not(feature = "file"))]
            ResourceSrc::Path(_) => Err(kithara_decode::DecodeError::DecodeError(
                "local file support requires the `file` feature".to_string(),
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[cfg(feature = "file")]
    fn detect_local_file_path() {
        let src = ResourceSrc::Path(PathBuf::from("/tmp/song.mp3"));
        let result = SourceType::detect(&src);
        assert!(result.is_ok());
        assert!(matches!(result.unwrap(), SourceType::LocalFile(_)));
    }

    #[test]
    #[cfg(feature = "file")]
    fn detect_remote_file_url() {
        let src = ResourceSrc::Url(Url::parse("https://example.com/song.mp3").unwrap());
        let result = SourceType::detect(&src);
        assert!(result.is_ok());
        assert!(matches!(result.unwrap(), SourceType::RemoteFile(_)));
    }

    #[test]
    #[cfg(feature = "hls")]
    fn detect_hls_url() {
        let src = ResourceSrc::Url(Url::parse("https://example.com/playlist.m3u8").unwrap());
        let result = SourceType::detect(&src);
        assert!(result.is_ok());
        assert!(matches!(result.unwrap(), SourceType::HlsStream(_)));
    }

    #[test]
    fn detect_invalid_relative_path() {
        // Relative paths should not reach here (caught by ResourceConfig::new),
        // but verify graceful handling via the Path variant
        let src = ResourceSrc::Path(PathBuf::from("relative/path.mp3"));
        let result = SourceType::detect(&src);
        // With file feature enabled, it's accepted as LocalFile
        #[cfg(feature = "file")]
        assert!(matches!(result.unwrap(), SourceType::LocalFile(_)));
    }
}
