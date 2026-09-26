//! Temporary directories for tests, one type on every target.

use std::path::{Path, PathBuf};

use crate::kithara;

/// Cross-platform temporary directory.
///
/// On native: wraps `tempfile::TempDir` (real filesystem).
/// On WASM: provides a dummy path — callers that need real FS should
/// use `AssetStore::builder(pools()).backend(StorageBackend::Memory)` instead.
pub struct TestTempDir {
    #[cfg(not(target_arch = "wasm32"))]
    inner: tempfile::TempDir,
}

impl TestTempDir {
    /// Create a new temporary directory.
    ///
    /// # Panics
    ///
    /// Panics when the native temporary directory cannot be created.
    #[must_use]
    pub fn new() -> Self {
        #[cfg(not(target_arch = "wasm32"))]
        {
            Self {
                inner: tempfile::tempdir().expect("Failed to create temp dir"),
            }
        }
        #[cfg(target_arch = "wasm32")]
        {
            Self {}
        }
    }

    /// Write `bytes` into this directory under `name` and return the path.
    ///
    /// Generated bodies live in the fixture store or inside the binary; a test
    /// that opens a file by path needs them on a real filesystem first.
    ///
    /// # Panics
    ///
    /// Panics when the file cannot be written.
    #[cfg(not(target_arch = "wasm32"))]
    #[must_use]
    pub fn write(&self, name: &str, bytes: &[u8]) -> PathBuf {
        let path = self.path().join(name);
        std::fs::write(&path, bytes)
            .unwrap_or_else(|error| panic!("write {}: {error}", path.display()));
        path
    }

    /// Get the path of the temporary directory.
    #[must_use]
    pub fn path(&self) -> &Path {
        #[cfg(not(target_arch = "wasm32"))]
        {
            self.inner.path()
        }
        #[cfg(target_arch = "wasm32")]
        {
            Path::new("/kithara-test")
        }
    }
}

impl Default for TestTempDir {
    fn default() -> Self {
        Self::new()
    }
}

/// Common fixture for a temporary directory.
///
/// # Panics
///
/// Panics when the native temporary directory cannot be created.
#[must_use]
#[kithara::fixture]
pub fn temp_dir() -> TestTempDir {
    TestTempDir::new()
}

/// Fixture returning both `TestTempDir` and `PathBuf`.
///
/// # Panics
///
/// Panics when the native temporary directory cannot be created.
#[must_use]
#[kithara::fixture]
pub fn temp_path() -> (TestTempDir, PathBuf) {
    let dir = TestTempDir::new();
    let path = dir.path().to_path_buf();
    (dir, path)
}
