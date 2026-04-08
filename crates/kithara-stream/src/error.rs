#![forbid(unsafe_code)]

use std::error::Error as StdError;

use thiserror::Error;

/// Errors produced by `kithara-stream` (generic over source error).
///
/// Currently the single variant `Source(E)` wraps a domain-specific
/// failure surfaced by a concrete `Source` implementation (network,
/// storage, parsing, DRM, etc.). Additional framework-level variants
/// were removed in Phase 4b-redo after workspace audit showed zero
/// production constructors — callers can reintroduce them honestly
/// if a future flow needs them.
#[derive(Debug, Error)]
pub enum StreamError<E>
where
    E: StdError + Send + Sync + 'static,
{
    #[error("source error: {0}")]
    Source(#[source] E),
}

/// Result type for `kithara-stream` (generic over source error).
pub type StreamResult<T, E> = Result<T, StreamError<E>>;

#[cfg(test)]
mod tests {
    mod kithara {
        pub(crate) use kithara_test_macros::test;
    }

    use std::io::{self, Error as IoError, ErrorKind};

    use super::*;

    #[kithara::test]
    fn test_source_error_display() {
        let io_err = IoError::new(ErrorKind::NotFound, "file missing");
        let err: StreamError<io::Error> = StreamError::Source(io_err);
        assert_eq!(err.to_string(), "source error: file missing");
    }

    #[kithara::test]
    fn test_stream_error_is_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<StreamError<io::Error>>();
    }
}
