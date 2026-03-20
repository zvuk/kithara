//! Common fixtures for integration tests.

use std::path::{Path, PathBuf};

use tokio_util::sync::CancellationToken;
use tracing_subscriber::EnvFilter;
#[cfg(target_arch = "wasm32")]
use tracing_subscriber::{layer, util};

use crate::kithara;

/// Cross-platform temporary directory.
///
/// On native: wraps `tempfile::TempDir` (real filesystem).
/// On WASM: provides a dummy path — callers that need real FS should
/// use `AssetStoreBuilder::ephemeral(true)` instead.
pub struct TestTempDir {
    #[cfg(not(target_arch = "wasm32"))]
    inner: tempfile::TempDir,
}

impl TestTempDir {
    /// Create a new temporary directory.
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

/// Common fixture for temporary directory
#[must_use]
#[kithara::fixture]
pub fn temp_dir() -> TestTempDir {
    TestTempDir::new()
}

/// Fixture returning both `TestTempDir` and `PathBuf`
#[must_use]
#[kithara::fixture]
pub fn temp_path() -> (TestTempDir, PathBuf) {
    let dir = TestTempDir::new();
    let path = dir.path().to_path_buf();
    (dir, path)
}

#[must_use]
#[kithara::fixture]
pub fn cancel_token() -> CancellationToken {
    CancellationToken::new()
}

#[must_use]
#[kithara::fixture]
pub fn cancel_token_cancelled() -> CancellationToken {
    let token = CancellationToken::new();
    token.cancel();
    token
}

#[kithara::fixture]
pub fn tracing_setup() {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("warn"));
    init_tracing(filter);
}

#[kithara::fixture]
pub fn debug_tracing_setup() {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| {
        EnvFilter::new("warn")
            .add_directive("kithara_hls=debug".parse().expect("valid directive"))
            .add_directive("kithara_stream=debug".parse().expect("valid directive"))
            .add_directive("kithara_decode=debug".parse().expect("valid directive"))
    });
    init_tracing(filter);
}

pub fn init_tracing(filter: EnvFilter) {
    #[cfg(not(target_arch = "wasm32"))]
    {
        let _ = tracing_subscriber::fmt()
            .with_env_filter(filter)
            .with_test_writer()
            .try_init();
    }

    #[cfg(target_arch = "wasm32")]
    {
        let mut config = tracing_wasm::WASMLayerConfigBuilder::new();
        config.set_report_logs_in_timings(false);
        let subscriber = layer::SubscriberExt::with(tracing_subscriber::registry(), filter);
        let subscriber =
            layer::SubscriberExt::with(subscriber, tracing_wasm::WASMLayer::new(config.build()));
        let _ = util::SubscriberInitExt::try_init(subscriber);
    }
}
