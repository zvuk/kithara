#![forbid(unsafe_code)]

use std::io;

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
    Io(#[from] io::Error),

    #[cfg(not(target_arch = "wasm32"))]
    #[error("mmap error: {0}")]
    Mmap(#[from] mmap_io::MmapIoError),

    #[error("invalid range: start {start} >= end {end}")]
    InvalidRange { start: u64, end: u64 },

    #[error("resource failed: {0}")]
    Failed(String),

    /// The atomic-chunked temp path is already claimed by another writer
    /// (different `AssetStore` instance, possibly cross-process). The
    /// caller should either wait for the holder to release (commit or
    /// drop) and retry, or fall through to a read-only / passthrough
    /// view of the canonical path once committed.
    ///
    /// Reported by [`crate::AtomicChunked::open`] when the sibling temp
    /// file already exists on disk. Filesystem-level signal — no
    /// in-process registry involved.
    #[error("atomic-chunked tmp claimed by another writer: {0}")]
    TmpClaimed(std::path::PathBuf),

    /// A read targeted a resource whose processing pipeline has not yet
    /// committed (e.g. AES-128 segment reactivated for re-fetch). The
    /// underlying bytes either still hold ciphertext or are stale; the
    /// caller should treat this as transient and retry once the writer
    /// completes its commit.
    #[error("processed resource is not readable before commit")]
    NotReadable,

    #[error("operation cancelled")]
    Cancelled,
}
