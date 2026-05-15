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

    /// Cooperative `wait_range` budget exceeded — caller passed
    /// `Some(timeout)` and the source did not become ready within it.
    /// Hot-path classifier for the audio worker's retry loop; diagnostic
    /// detail is intentionally absent so emission allocates nothing.
    #[error("wait_range budget exceeded")]
    WaitBudgetExceeded,

    /// `format_change_segment_range` not applicable in the current
    /// state. Reasons (all expected steady states, not bugs):
    /// - source has no init-bearing format-change concept (file
    ///   source — default `Source` trait impl returns this);
    /// - active HLS variant was activated by same-codec ABR with
    ///   `served_from > 0`: init bytes live at natural `[0..init_size)`
    ///   while virtual space starts at `byte_shift`, so init is
    ///   unreachable via Stream reads. Same-codec post-switch
    ///   playback continues through `byte_shift`; recovery via
    ///   init probe is by design not applicable.
    ///
    /// Callers must treat this as "no recovery possible at this
    /// site" (steady-state) — not an error to surface to the user.
    #[error("format change not applicable to this source kind/state")]
    FormatChangeNotApplicable,

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
    use std::io::{Error as IoError, ErrorKind};

    use kithara_test_utils::kithara;

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
