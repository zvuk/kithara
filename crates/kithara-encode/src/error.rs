use std::{error::Error as StdError, io};

use kithara_stream::{AudioCodec, ContainerFormat};
use thiserror::Error;

#[derive(Debug, Error)]
#[error("{0}")]
pub(crate) struct BackendMessage(pub(crate) String);

/// Errors that can occur during audio encoding.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum EncodeError {
    #[error("IO error: {0}")]
    Io(#[from] io::Error),

    #[error("Unsupported codec: {0:?}")]
    UnsupportedCodec(AudioCodec),

    #[error("Unsupported container: {0:?}")]
    UnsupportedContainer(ContainerFormat),

    #[error(
        "{container:?} container limit exceeded: attempted {attempted_bytes} bytes, maximum {max_bytes}"
    )]
    ContainerLimitExceeded {
        /// Container whose hard byte limit was exceeded.
        container: ContainerFormat,
        /// Byte length the operation attempted to represent.
        attempted_bytes: u64,
        /// Largest byte length representable by `container`.
        max_bytes: u64,
    },

    #[error("Invalid input: {0}")]
    InvalidInput(String),

    #[error("Invalid media info: missing `{0}`")]
    InvalidMediaInfo(&'static str),

    #[error("Encoder error: {0}")]
    Backend(#[source] Box<dyn StdError + Send + Sync>),
}

impl EncodeError {
    #[must_use]
    pub fn backend_message(message: String) -> Self {
        Self::Backend(Box::new(BackendMessage(message)))
    }
}

#[cfg(all(not(target_arch = "wasm32"), feature = "ffmpeg"))]
impl From<ffmpeg_next::Error> for EncodeError {
    fn from(error: ffmpeg_next::Error) -> Self {
        Self::Backend(Box::new(error))
    }
}

/// Result type for encode operations.
pub type EncodeResult<T> = Result<T, EncodeError>;

#[cfg(test)]
mod tests {
    use kithara_stream::{AudioCodec, ContainerFormat};
    use kithara_test_utils::kithara;

    use super::EncodeError;

    #[kithara::test]
    #[case::codec(
        EncodeError::UnsupportedCodec(AudioCodec::AacLc),
        "Unsupported codec: AacLc"
    )]
    #[case::container(
        EncodeError::UnsupportedContainer(ContainerFormat::Fmp4),
        "Unsupported container: Fmp4"
    )]
    fn error_display_mentions(#[case] error: EncodeError, #[case] message: &str) {
        assert_eq!(error.to_string(), message);
    }

    #[kithara::test]
    fn backend_message_wraps_any_string() {
        let error = EncodeError::backend_message("ffmpeg init failed".to_owned());
        assert!(error.to_string().contains("Encoder error"));
    }
}
