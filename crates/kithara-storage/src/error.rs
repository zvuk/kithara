#![forbid(unsafe_code)]

use random_access_storage::RandomAccessError;
use thiserror::Error;

/// Result type used by `kithara-storage`.
pub type StorageResult<T> = Result<T, StorageError>;

/// Errors produced by storage primitives.
///
/// Notes:
/// - We intentionally keep this error type fairly small at this stage.
/// - Higher-level crates may wrap this error to add domain context (resource key, URL, etc.).
#[derive(Debug, Error)]
pub enum StorageError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    #[error("random access error: {0}")]
    RandomAccess(#[from] RandomAccessError),

    #[error("invalid range: start {start} >= end {end}")]
    InvalidRange { start: u64, end: u64 },

    #[error("resource is sealed (no longer writable)")]
    Sealed,

    #[error("resource failed: {0}")]
    Failed(String),

    #[error("operation cancelled")]
    Cancelled,
}
