#![forbid(unsafe_code)]

//! Detected source type from URL analysis.

use url::Url;

/// Detected source type from URL.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SourceType {
    /// Remote progressive file (MP3, AAC, FLAC, WAV, etc.)
    #[cfg(feature = "file")]
    RemoteFile(Url),
    /// HLS stream (URL ending with `.m3u8`).
    #[cfg(feature = "hls")]
    HlsStream(Url),
}

impl SourceType {
    /// Detect source type from a URL string.
    ///
    /// - URLs ending with `.m3u8` -> HLS
    /// - All other URLs -> progressive file download
    pub fn detect(url: &str) -> Result<Self, kithara_decode::DecodeError> {
        let parsed = Url::parse(url.trim())
            .map_err(|e| kithara_decode::DecodeError::DecodeError(format!("invalid URL: {e}")))?;

        #[cfg(feature = "hls")]
        if parsed.path().ends_with(".m3u8") {
            return Ok(Self::HlsStream(parsed));
        }

        #[cfg(feature = "file")]
        return Ok(Self::RemoteFile(parsed));

        #[cfg(not(feature = "file"))]
        Err(kithara_decode::DecodeError::DecodeError(
            "no suitable feature enabled for this URL (enable `file` or `hls`)".to_string(),
        ))
    }
}
