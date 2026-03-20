//! Error types for audio decoding.

use std::{error::Error as StdError, io};

use kithara_stream::{AudioCodec, ContainerFormat, VariantChangeError};
use symphonia::core::errors::Error as SymphoniaError;
use thiserror::Error;

/// Errors that can occur during audio decoding.
///
/// This error type is backend-agnostic, wrapping decoder-specific errors
/// in the `Backend` variant.
#[derive(Debug, Error)]
pub enum DecodeError {
    #[error("IO error: {0}")]
    Io(io::Error),

    #[error("Unsupported codec: {0:?}")]
    UnsupportedCodec(AudioCodec),

    #[error("Unsupported container: {0:?}")]
    UnsupportedContainer(ContainerFormat),

    #[error("Invalid data: {0}")]
    InvalidData(String),

    #[error("Seek failed: {0}")]
    SeekFailed(String),

    /// Alias for `SeekFailed` (backward compatibility).
    #[error("Seek error: {0}")]
    SeekError(String),

    #[error("Probe failed: could not detect codec")]
    ProbeFailed,

    /// A seek interrupted the decode operation. Not a real error —
    /// the caller should check for pending seeks and retry.
    #[error("Interrupted by seek")]
    Interrupted,

    #[error("Decoder error: {0}")]
    Backend(#[source] Box<dyn StdError + Send + Sync>),
}

fn is_seek_pending_io(err: &io::Error) -> bool {
    err.kind() == io::ErrorKind::Interrupted || err.to_string() == "seek pending"
}

fn is_variant_change_io(err: &io::Error) -> bool {
    err.get_ref()
        .and_then(|source| source.downcast_ref::<VariantChangeError>())
        .is_some()
}

fn error_chain_is_interrupted(err: &(dyn StdError + 'static)) -> bool {
    if let Some(io_err) = err.downcast_ref::<io::Error>() {
        return is_seek_pending_io(io_err);
    }

    if let Some(symphonia_err) = err.downcast_ref::<SymphoniaError>() {
        return match symphonia_err {
            SymphoniaError::IoError(io_err) => is_seek_pending_io(io_err),
            _ => symphonia_err.to_string().contains("seek pending"),
        };
    }

    if err.to_string().contains("seek pending") {
        return true;
    }

    err.source().is_some_and(error_chain_is_interrupted)
}

fn error_chain_is_variant_change(err: &(dyn StdError + 'static)) -> bool {
    if let Some(io_err) = err.downcast_ref::<io::Error>() {
        return is_variant_change_io(io_err);
    }

    if let Some(symphonia_err) = err.downcast_ref::<SymphoniaError>() {
        return match symphonia_err {
            SymphoniaError::IoError(io_err) => is_variant_change_io(io_err),
            _ => false,
        };
    }

    if err.downcast_ref::<VariantChangeError>().is_some() {
        return true;
    }

    err.source().is_some_and(error_chain_is_variant_change)
}

impl DecodeError {
    /// Returns `true` if the error is an [`Interrupted`](Self::Interrupted) variant.
    #[must_use]
    pub fn is_interrupted(&self) -> bool {
        match self {
            Self::Interrupted => true,
            Self::Io(err) => is_seek_pending_io(err),
            Self::Backend(err) => error_chain_is_interrupted(err.as_ref()),
            _ => false,
        }
    }

    /// Returns `true` if the error signals a non-retriable cross-variant boundary.
    #[must_use]
    pub fn is_variant_change(&self) -> bool {
        match self {
            Self::Io(err) => is_variant_change_io(err),
            Self::Backend(err) => error_chain_is_variant_change(err.as_ref()),
            _ => false,
        }
    }
}

impl From<io::Error> for DecodeError {
    fn from(err: io::Error) -> Self {
        if err.kind() == io::ErrorKind::Interrupted {
            Self::Interrupted
        } else {
            Self::Io(err)
        }
    }
}

/// Result type for decode operations.
pub type DecodeResult<T> = Result<T, DecodeError>;

#[cfg(test)]
mod tests {
    use std::io;

    use kithara_test_utils::kithara;

    use super::*;

    #[kithara::test]
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
    fn test_error_display(#[case] error: DecodeError, #[case] expected: &str) {
        assert_eq!(error.to_string(), expected);
    }

    #[kithara::test]
    fn test_decode_error_from_io() {
        let io_err = io::Error::new(io::ErrorKind::NotFound, "file not found");
        let decode_err: DecodeError = io_err.into();
        assert!(matches!(decode_err, DecodeError::Io(_)));
    }

    #[kithara::test]
    fn test_decode_error_backend_wraps_any_error() {
        let inner = io::Error::other("symphonia error");
        let err = DecodeError::Backend(Box::new(inner));
        assert!(err.to_string().contains("Decoder error"));
    }

    #[kithara::test]
    fn test_decode_error_is_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<DecodeError>();
    }

    #[kithara::test]
    fn test_io_interrupted_becomes_decode_interrupted() {
        let io_err = io::Error::new(io::ErrorKind::Interrupted, "seek pending");
        let decode_err: DecodeError = io_err.into();
        assert!(matches!(decode_err, DecodeError::Interrupted));
    }

    #[kithara::test]
    fn test_io_other_stays_io_variant() {
        let io_err = io::Error::new(io::ErrorKind::NotFound, "missing");
        let decode_err: DecodeError = io_err.into();
        assert!(matches!(decode_err, DecodeError::Io(_)));
    }

    #[kithara::test]
    fn test_backend_seek_pending_counts_as_interrupted() {
        let decode_err = DecodeError::Backend(Box::new(io::Error::other("seek pending")));
        assert!(decode_err.is_interrupted());
    }

    #[kithara::test]
    fn test_backend_other_io_is_not_interrupted() {
        let decode_err = DecodeError::Backend(Box::new(io::Error::other("other backend error")));
        assert!(!decode_err.is_interrupted());
    }

    #[kithara::test]
    fn test_backend_symphonia_seek_pending_counts_as_interrupted() {
        let decode_err = DecodeError::Backend(Box::new(SymphoniaError::IoError(io::Error::other(
            "seek pending",
        ))));
        assert!(decode_err.is_interrupted());
    }

    #[kithara::test]
    fn test_io_variant_change_is_detected() {
        let decode_err = DecodeError::Io(io::Error::other(VariantChangeError));
        assert!(decode_err.is_variant_change());
        assert!(!decode_err.is_interrupted());
    }
}
