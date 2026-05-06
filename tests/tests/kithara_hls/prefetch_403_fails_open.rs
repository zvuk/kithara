#![forbid(unsafe_code)]

//! Regression test for the silent-hang chain when DRM-key prefetch fails.
//!
//! When the key server returns HTTP 403 during `Hls::create`, the prefetch
//! must propagate the error to the caller instead of swallowing it as a
//! warning. The previous code resolved `Stream::<Hls>::new` with `Ok`,
//! pushing the symptom into a downstream `wait_range` hang that
//! `HangDetector` panicked on after 5 seconds — see the original
//! reproduction in `app.log` (track ids 95038745 / 176000094).
//!
//! Acceptance:
//! - `Stream::<Hls>::new` returns `Err` (any source error variant) when the
//!   encrypted variant's key endpoint serves 403.
//! - The whole open completes in well under 1 second; we wrap the call in
//!   a 1-second `tokio::time::timeout` so a regression manifests as a
//!   timeout panic rather than a 5-second hang.

use std::{error::Error, time::Duration};

use kithara::{
    assets::StoreOptions,
    hls::{Hls, HlsConfig},
    stream::Stream,
};
use kithara_test_utils::{
    PackagedTestServer, TestTempDir,
    fixture_protocol::{HlsRouteKind, HttpErrorRule},
    temp_dir,
};
use tokio_util::sync::CancellationToken;

#[kithara::test(
    tokio,
    timeout(Duration::from_secs(2)),
    env(KITHARA_HANG_TIMEOUT_SECS = "1"),
    tracing("kithara_hls=debug,kithara_stream=info,warn")
)]
async fn prefetch_403_returns_err_quickly(
    temp_dir: TestTempDir,
) -> Result<(), Box<dyn Error + Send + Sync>> {
    let server = PackagedTestServer::with_error_rules(vec![HttpErrorRule {
        kind: HlsRouteKind::Key,
        status: 403,
        body: Some("{\"detail\":\"User not registered\"}".to_string()),
        ..Default::default()
    }])
    .await;

    let url = server.url("/master-encrypted.m3u8");
    let config = HlsConfig::new(url)
        .with_store(StoreOptions::new(temp_dir.path()))
        .with_cancel(CancellationToken::new());

    let started = std::time::Instant::now();
    let result = tokio::time::timeout(Duration::from_secs(1), Stream::<Hls>::new(config))
        .await
        .map_err(|_| "Stream::<Hls>::new did not return within 1s — silent hang regression")?;
    let elapsed = started.elapsed();

    let err = match result {
        Ok(_) => panic!(
            "Stream::<Hls>::new must fail when key server returns 403; got Ok in {elapsed:?}"
        ),
        Err(e) => e,
    };
    let msg = format!("{err:?}");
    assert!(
        msg.contains("Key") || msg.contains("403") || msg.contains("registered"),
        "expected key-related error message, got {msg}"
    );

    Ok(())
}
