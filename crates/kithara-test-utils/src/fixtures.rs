//! Common fixtures for integration tests.

use std::{
    path::{Path, PathBuf},
    time::Duration,
};

use kithara_abr::{AbrMode, AbrOptions};
use tokio_util::sync::CancellationToken;
use tracing_subscriber::EnvFilter;
#[cfg(target_arch = "wasm32")]
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};
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

#[must_use]
#[kithara::fixture]
pub fn abr_switch_trigger() -> AbrOptions {
    AbrOptions {
        down_hysteresis_ratio: 1.0,
        down_switch_buffer_secs: 0.0,
        max_bandwidth_bps: None,
        min_buffer_for_up_switch_secs: 0.0,
        min_switch_interval: Duration::ZERO,
        min_throughput_record_ms: 0,
        mode: AbrMode::Auto(None),
        sample_window: Duration::from_millis(100),
        throughput_safety_factor: 1.0,
        up_hysteresis_ratio: 1.0,
        variants: Vec::new(),
    }
}

#[must_use]
#[kithara::fixture]
pub fn abr_fast() -> AbrOptions {
    AbrOptions {
        down_hysteresis_ratio: 0.9,
        down_switch_buffer_secs: 0.0,
        max_bandwidth_bps: None,
        min_buffer_for_up_switch_secs: 0.0,
        min_switch_interval: Duration::from_secs(1),
        min_throughput_record_ms: 0,
        mode: AbrMode::Auto(None),
        sample_window: Duration::from_millis(200),
        throughput_safety_factor: 1.0,
        up_hysteresis_ratio: 2.0,
        variants: Vec::new(),
    }
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
        let _ = tracing_subscriber::fmt()
            .with_env_filter(filter)
            .with_test_writer()
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
