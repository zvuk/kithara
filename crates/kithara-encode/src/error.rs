use std::{error::Error as StdError, io};

use kithara_stream::{AudioCodec, ContainerFormat};
use thiserror::Error;

#[derive(Debug, Error)]
#[error("{0}")]
pub(crate) struct BackendMessage(pub(crate) String);

/// Errors that can occur during audio encoding.
#[derive(Debug, Error)]
pub enum EncodeError {
    #[error("IO error: {0}")]
    Io(#[from] io::Error),

    #[error("Unsupported codec: {0:?}")]
    UnsupportedCodec(AudioCodec),

    #[error("Unsupported container: {0:?}")]
    UnsupportedContainer(ContainerFormat),

    #[error("Invalid input: {0}")]
    InvalidInput(String),

    #[error("Invalid media info: missing `{0}`")]
    InvalidMediaInfo(&'static str),

    #[error("Encoder error: {0}")]
    Backend(#[source] Box<dyn StdError + Send + Sync>),
}

impl EncodeError {
    pub(crate) fn backend_message(message: String) -> Self {
        Self::Backend(Box::new(BackendMessage(message)))
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl From<ffmpeg_next::Error> for EncodeError {
    fn from(error: ffmpeg_next::Error) -> Self {
        Self::Backend(Box::new(error))
    }
}

/// Result type for encode operations.
pub type EncodeResult<T> = Result<T, EncodeError>;

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::*;

    #[kithara::test]
    fn error_display_mentions_codec() {
        let error = EncodeError::UnsupportedCodec(AudioCodec::AacLc);
        assert_eq!(error.to_string(), "Unsupported codec: AacLc");
    }

    #[kithara::test]
    fn error_display_mentions_container() {
        let error = EncodeError::UnsupportedContainer(ContainerFormat::Fmp4);
        assert_eq!(error.to_string(), "Unsupported container: Fmp4");
    }

    #[kithara::test]
    fn backend_message_wraps_any_string() {
        let error = EncodeError::backend_message("ffmpeg init failed".to_owned());
        assert!(error.to_string().contains("Encoder error"));
    }
}
