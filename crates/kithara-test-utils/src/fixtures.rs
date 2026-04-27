//! Common fixtures for integration tests.

use std::{
    path::{Path, PathBuf},
    time::Duration,
};

use kithara_abr::{AbrMode, AbrSettings};
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

/// ABR settings tuned for tests that want variant switches to fire on
/// every sample without hysteresis or interval gates.
#[must_use]
#[kithara::fixture]
pub fn abr_switch_trigger() -> AbrSettings {
    AbrSettings {
        warmup_min_bytes: 0,
        min_buffer_for_up_switch: Duration::ZERO,
        urgent_downswitch_buffer: Duration::ZERO,
        min_switch_interval: Duration::ZERO,
        throughput_safety_factor: 1.0,
        up_hysteresis_ratio: 1.0,
        down_hysteresis_ratio: 1.0,
        min_throughput_record_ms: 0,
        ..AbrSettings::default()
    }
}

/// ABR settings for fast-reacting tests (sub-second switch interval).
#[must_use]
#[kithara::fixture]
pub fn abr_fast() -> AbrSettings {
    AbrSettings {
        warmup_min_bytes: 0,
        min_buffer_for_up_switch: Duration::ZERO,
        urgent_downswitch_buffer: Duration::ZERO,
        min_switch_interval: Duration::from_secs(1),
        throughput_safety_factor: 1.0,
        up_hysteresis_ratio: 2.0,
        down_hysteresis_ratio: 0.9,
        min_throughput_record_ms: 0,
        ..AbrSettings::default()
    }
}

/// Default initial ABR mode for test fixtures — Auto starting at variant 0.
#[must_use]
#[kithara::fixture]
pub fn abr_initial_mode() -> AbrMode {
    AbrMode::Auto(None)
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
