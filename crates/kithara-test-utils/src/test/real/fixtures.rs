use std::path::{Path, PathBuf};

use tokio_util::sync::CancellationToken;
use tracing_subscriber::{EnvFilter, layer::SubscriberExt, util::SubscriberInitExt};
#[cfg(target_arch = "wasm32")]
use tracing_wasm::{WASMLayer, WASMLayerConfigBuilder};

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

pub fn setup_tracing() {
    setup_tracing_with_filter("warn");
}

pub fn setup_tracing_with_filter(directives: &str) {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(directives));
    init_tracing(filter);
}

pub fn init_tracing(filter: EnvFilter) {
    #[cfg(not(target_arch = "wasm32"))]
    {
        use tracing_subscriber::Layer as _;

        let fmt_layer = tracing_subscriber::fmt::layer()
            .with_test_writer()
            .with_filter(filter);
        let probe_layer = crate::probe_capture::probe_layer();
        let _ = tracing_subscriber::registry()
            .with(fmt_layer)
            .with(probe_layer)
            .try_init();
    }

    #[cfg(target_arch = "wasm32")]
    {
        let mut config = WASMLayerConfigBuilder::new();
        config.set_report_logs_in_timings(false);
        let subscriber = tracing_subscriber::registry().with(filter);
        let subscriber = subscriber.with(WASMLayer::new(config.build()));
        let _ = subscriber.try_init();
    }
}
