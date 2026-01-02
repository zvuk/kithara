use crate::{CachePath, CacheResult, PutResult};

/// Atomic write utilities for cache operations.
/// Provides atomic file writing with temp+rename pattern.
pub struct AtomicWrite;

impl AtomicWrite {
    /// Atomically write data to a file with temp+rename pattern
    pub fn write_atomic<P: AsRef<std::path::Path>>(
        dir: P,
        rel_path: &CachePath,
        bytes: &[u8],
    ) -> CacheResult<PutResult> {
        let path = dir.as_ref().join(rel_path.as_path_buf());
        let temp_path = path.with_extension("tmp");

        // Write to temp file first
        std::fs::write(&temp_path, bytes)?;

        // Atomic rename
        std::fs::rename(&temp_path, &path)?;

        Ok(PutResult {
            bytes_written: bytes.len() as u64,
        })
    }
}
