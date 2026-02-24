//! Common fixtures for integration tests.

use std::path::PathBuf;

use tempfile::TempDir;
use tokio_util::sync::CancellationToken;

use crate::kithara;

/// Common fixture for temporary directory
#[must_use]
#[kithara::fixture]
pub fn temp_dir() -> TempDir {
    tempfile::tempdir().expect("Failed to create temp dir")
}

/// Fixture returning both `TempDir` and `PathBuf`
#[must_use]
#[kithara::fixture]
pub fn temp_path() -> (TempDir, PathBuf) {
    let dir = tempfile::tempdir().expect("Failed to create temp dir");
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
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::default()
                .add_directive("warn".parse().expect("valid directive")),
        )
        .with_test_writer()
        .try_init();
}

#[kithara::fixture]
pub fn debug_tracing_setup() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::default()
                .add_directive("kithara_hls=debug".parse().expect("valid directive"))
                .add_directive("kithara_stream=debug".parse().expect("valid directive"))
                .add_directive("kithara_decode=debug".parse().expect("valid directive")),
        )
        .with_test_writer()
        .try_init();
}
