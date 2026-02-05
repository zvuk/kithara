//! New error types for the generic decoder architecture.
//!
//! This module will replace the existing `DecodeError` in `types.rs` once the
//! refactor is complete.

use std::io;

use kithara_stream::{AudioCodec, ContainerFormat};
use thiserror::Error;

/// Errors that can occur during audio decoding.
///
/// This error type is backend-agnostic, wrapping decoder-specific errors
/// in the `Backend` variant.
#[derive(Debug, Error)]
#[allow(dead_code)] // Will be used once integrated with new decoder traits
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

    #[error("Probe failed: could not detect codec")]
    ProbeFailed,

    #[error("Decoder error: {0}")]
    Backend(#[source] Box<dyn std::error::Error + Send + Sync>),
}

/// Result type for decode operations.
#[allow(dead_code)] // Will be used once integrated with new decoder traits
pub type DecodeResult<T> = Result<T, DecodeError>;

#[cfg(test)]
mod tests {
    use std::io;

    use super::*;

    #[test]
    fn test_decode_error_from_io() {
        let io_err = io::Error::new(io::ErrorKind::NotFound, "file not found");
        let decode_err: DecodeError = io_err.into();
        assert!(matches!(decode_err, DecodeError::Io(_)));
    }

    #[test]
    fn test_decode_error_display() {
        let err = DecodeError::InvalidData("bad frame".into());
        assert_eq!(err.to_string(), "Invalid data: bad frame");
    }

    #[test]
    fn test_decode_error_backend_wraps_any_error() {
        let inner = io::Error::new(io::ErrorKind::Other, "symphonia error");
        let err = DecodeError::Backend(Box::new(inner));
        assert!(err.to_string().contains("Decoder error"));
    }
}
