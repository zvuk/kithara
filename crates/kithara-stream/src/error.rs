#![forbid(unsafe_code)]

use std::{error::Error as StdError, io};

use thiserror::Error;

/// Unified source error, surfaced by every `Source` impl.
///
/// Concrete adapters (file, HLS, mocks) flatten their crate-local error
/// types into one of these variants at the `Source` impl boundary so that
/// `Source` itself stays object-safe (no associated `Error` type).
///
/// Adapter-specific failures that don't map to a generic variant flow
/// through [`SourceError::Other`], which boxes the original typed error
/// for `downcast` access.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum SourceError {
    #[error("network: {0}")]
    Net(#[from] kithara_net::NetError),

    #[error("storage: {0}")]
    Storage(#[from] kithara_storage::StorageError),

    #[error("invalid path: {0}")]
    InvalidPath(String),

    #[error("playlist parse: {0}")]
    PlaylistParse(String),

    #[error("variant not found: {0}")]
    VariantNotFound(String),

    #[error("segment not found: {0}")]
    SegmentNotFound(String),

    #[error("key processing: {0}")]
    KeyProcessing(String),

    #[error("invalid url: {0}")]
    InvalidUrl(String),

    #[error("cancelled")]
    Cancelled,

    #[error("timeout: {0}")]
    Timeout(String),

    #[error("io: {0}")]
    Io(#[from] io::Error),

    #[error("{0}")]
    Other(Box<dyn StdError + Send + Sync>),
}

impl SourceError {
    /// Wrap an arbitrary error in `Other`.
    pub fn other<E>(err: E) -> Self
    where
        E: StdError + Send + Sync + 'static,
    {
        Self::Other(Box::new(err))
    }
}

/// Errors produced by `kithara-stream`.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum StreamError {
    #[error("source error: {0}")]
    Source(#[source] SourceError),
}

impl<E> From<E> for StreamError
where
    E: Into<SourceError>,
{
    fn from(err: E) -> Self {
        Self::Source(err.into())
    }
}

/// Result type for `kithara-stream`.
pub type StreamResult<T> = Result<T, StreamError>;

#[cfg(test)]
mod tests {
    mod kithara {
        pub(crate) use kithara_test_macros::test;
    }

    use std::io::{Error as IoError, ErrorKind};

    use super::*;

    #[kithara::test]
    fn test_source_error_display() {
        let io_err = IoError::new(ErrorKind::NotFound, "file missing");
        let err = StreamError::Source(SourceError::Io(io_err));
        assert!(err.to_string().contains("file missing"));
    }

    #[kithara::test]
    fn test_stream_error_is_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<StreamError>();
        assert_send_sync::<SourceError>();
    }
}
