//! Integration coverage for `kithara_app::waveform::analyze`: decode a
//! fixture-server WAV end to end and assert the envelope contract.

#![cfg(not(target_arch = "wasm32"))]

use std::time::Duration;

use kithara::prelude::ResourceConfig;
use kithara_app::waveform::analyze;
use kithara_integration_tests::{SignalFormat, SignalSpec, SignalSpecLength, TestServerHelper};
use kithara_platform::CancellationToken;

fn silence_wav_spec() -> SignalSpec {
    SignalSpec {
        format: SignalFormat::Wav,
        length: SignalSpecLength::Seconds(1.0),
        channels: 2,
        sample_rate: 44_100,
        bit_rate: None,
    }
}

#[kithara::test(
    tokio,
    timeout(Duration::from_secs(2)),
    env(KITHARA_HANG_TIMEOUT_SECS = "2")
)]
async fn analyze_silent_wav_yields_all_zero_envelope() {
    let server = TestServerHelper::new().await;
    let url = server.silence(&silence_wav_spec()).await;
    let config =
        ResourceConfig::new(url.as_str()).expect("silence URL must build a ResourceConfig");

    // A silent 1s WAV must decode end to end and finalise to exactly
    // `buckets` all-zero values (no frames are loud, so nothing normalises
    // up to 1.0).
    let env = analyze(config, 100, CancellationToken::default())
        .await
        .expect("silent WAV must decode to a finalised envelope");

    assert_eq!(env.len(), 100, "one value per requested bucket");
    assert!(
        env.iter().all(|&v| v == 0.0),
        "a silent source must yield all-zero buckets: {env:?}"
    );
}

#[kithara::test(
    tokio,
    timeout(Duration::from_secs(2)),
    env(KITHARA_HANG_TIMEOUT_SECS = "2")
)]
async fn analyze_returns_none_when_cancelled_upfront() {
    let server = TestServerHelper::new().await;
    let url = server.silence(&silence_wav_spec()).await;
    let config =
        ResourceConfig::new(url.as_str()).expect("silence URL must build a ResourceConfig");

    let cancel = CancellationToken::default();
    cancel.cancel();
    assert!(
        analyze(config, 100, cancel).await.is_none(),
        "a pre-cancelled analysis must not return an envelope"
    );
}
