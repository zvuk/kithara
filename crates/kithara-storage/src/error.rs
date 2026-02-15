#![forbid(unsafe_code)]

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

    #[error("mmap error: {0}")]
    Mmap(#[from] mmap_io::MmapIoError),

    #[error("invalid range: start {start} >= end {end}")]
    InvalidRange { start: u64, end: u64 },

    #[error("resource failed: {0}")]
    Failed(String),

    #[error("operation cancelled")]
    Cancelled,
}
