use std::path::{Path, PathBuf};

use kithara::{self, platform::CancelToken};
use kithara_test_fixtures::SignalAsset;
use url::Url;

use crate::{TestServerHelper, mixed_codec_ladder_url};

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

#[must_use]
#[kithara::fixture]
pub fn cancel_token() -> CancelToken {
    CancelToken::never()
}

#[must_use]
#[kithara::fixture]
pub fn rt_cancel() -> CancelToken {
    CancelToken::never()
}

#[must_use]
#[kithara::fixture]
pub fn cancel_token_cancelled() -> CancelToken {
    let token = CancelToken::never();
    token.cancel();
    token
}

#[kithara::fixture]
pub async fn served_mp3() -> (TestServerHelper, Url) {
    let server = TestServerHelper::new().await;
    let url = server.signal(SignalAsset::MP3_SINE880_48K_162S);
    (server, url)
}

#[kithara::fixture]
pub async fn served_short_mp3() -> (TestServerHelper, Url) {
    let server = TestServerHelper::new().await;
    let url = server.signal(SignalAsset::MP3_SINE880_30S);
    (server, url)
}

#[kithara::fixture]
pub async fn served_silence() -> (TestServerHelper, Url) {
    let server = TestServerHelper::new().await;
    let url = server.signal(SignalAsset::WAV_SILENCE_1S);
    (server, url)
}

#[kithara::fixture]
pub async fn served_aac() -> (TestServerHelper, Url) {
    let server = TestServerHelper::new().await;
    let url = server.signal(SignalAsset::AAC_SINE440_60S_320K);
    (server, url)
}

#[kithara::fixture]
pub async fn mixed_plain() -> (TestServerHelper, Url) {
    let server = TestServerHelper::new().await;
    let url = mixed_codec_ladder_url(&server, false).await;
    (server, url)
}

#[kithara::fixture]
pub async fn mixed_encrypted() -> (TestServerHelper, Url) {
    let server = TestServerHelper::new().await;
    let url = mixed_codec_ladder_url(&server, true).await;
    (server, url)
}
