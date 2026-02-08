//! Error types for audio decoding.

use std::io;

use kithara_stream::{AudioCodec, ContainerFormat};
use thiserror::Error;

/// Errors that can occur during audio decoding.
///
/// This error type is backend-agnostic, wrapping decoder-specific errors
/// in the `Backend` variant.
#[derive(Debug, Error)]
pub enum DecodeError {
    #[error("IO error: {0}")]
    Io(#[from] io::Error),

    #[error("Unsupported codec: {0:?}")]
    UnsupportedCodec(AudioCodec),

    #[error("Unsupported container: {0:?}")]
    UnsupportedContainer(ContainerFormat),

    #[error("Invalid data: {0}")]
    InvalidData(String),

    #[error("Seek failed: {0}")]
    SeekFailed(String),

    /// Alias for SeekFailed (backward compatibility).
    #[error("Seek error: {0}")]
    SeekError(String),

    #[error("Probe failed: could not detect codec")]
    ProbeFailed,

    #[error("Decoder error: {0}")]
    Backend(#[source] Box<dyn std::error::Error + Send + Sync>),
}

/// Result type for decode operations.
pub type DecodeResult<T> = Result<T, DecodeError>;

#[cfg(test)]
mod tests {
    use std::io;

    use rstest::rstest;

    use super::*;

    #[rstest]
    #[case::invalid_data(DecodeError::InvalidData("bad frame".into()), "Invalid data: bad frame")]
    #[case::seek_failed(DecodeError::SeekFailed("timestamp out of range".into()), "Seek failed: timestamp out of range")]
    #[case::seek_error(DecodeError::SeekError("invalid position".into()), "Seek error: invalid position")]
    #[case::probe_failed(DecodeError::ProbeFailed, "Probe failed: could not detect codec")]
    #[case::unsupported_codec(
        DecodeError::UnsupportedCodec(AudioCodec::AacLc),
        "Unsupported codec: AacLc"
    )]
    #[case::unsupported_container(
        DecodeError::UnsupportedContainer(ContainerFormat::Fmp4),
        "Unsupported container: Fmp4"
    )]
    #[test]
    fn test_error_display(#[case] error: DecodeError, #[case] expected: &str) {
        assert_eq!(error.to_string(), expected);
    }

    #[test]
    fn test_decode_error_from_io() {
        let io_err = io::Error::new(io::ErrorKind::NotFound, "file not found");
        let decode_err: DecodeError = io_err.into();
        assert!(matches!(decode_err, DecodeError::Io(_)));
    }

    #[test]
    fn test_decode_error_backend_wraps_any_error() {
        let inner = io::Error::new(io::ErrorKind::Other, "symphonia error");
        let err = DecodeError::Backend(Box::new(inner));
        assert!(err.to_string().contains("Decoder error"));
    }

    #[test]
    fn test_decode_error_is_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<DecodeError>();
    }
}
