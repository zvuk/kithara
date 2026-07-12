//! Integration coverage for `kithara_app::waveform`: decode a fixture-server
//! WAV end to end through the production `TrackAnalysisRunner` (resource
//! open + shared analysis-worker thread) and assert the source-analysis
//! contract.

#![cfg(not(target_arch = "wasm32"))]

use kithara::{
    audio::{Bucket, analysis::BeatAnalysisConfig},
    platform::{CancelToken, time::Duration},
    prelude::ResourceConfig,
};
use kithara_app::waveform::{TrackAnalysis, TrackAnalysisRunner};
use kithara_integration_tests::{SignalFormat, SignalSpec, SignalSpecLength, TestServerHelper};

fn silence_wav_spec() -> SignalSpec {
    SignalSpec {
        format: SignalFormat::Wav,
        length: SignalSpecLength::Seconds(1.0),
        channels: 2,
        sample_rate: 44_100,
        bit_rate: None,
    }
}

/// Run one analysis through the production runner and await its result.
async fn run_analysis(
    master: &CancelToken,
    config: ResourceConfig,
    buckets: usize,
) -> Option<TrackAnalysis> {
    let mut runner = TrackAnalysisRunner::new(master, buckets, BeatAnalysisConfig::default());
    let mut rx = runner.analyze(config);

    // Staged analysis can emit twice (waveform, then waveform+beat).
    let mut last = None;
    while rx.changed().await.is_ok() {
        last = rx.borrow().clone();
    }
    last
}

#[kithara::test(
    tokio,
    timeout(Duration::from_secs(2)),
    env(KITHARA_HANG_TIMEOUT_SECS = "2")
)]
async fn runner_silent_wav_yields_all_zero_envelope() {
    let server = TestServerHelper::new().await;
    let url = server.silence(&silence_wav_spec()).await;
    let config =
        ResourceConfig::new(url.as_str()).expect("silence URL must build a ResourceConfig");

    // A silent 1s WAV must decode end to end and finalise to a native-resolution
    // envelope capped by the requested maximum. No frames are loud, so nothing
    // normalises up to 1.0.
    let analysis = run_analysis(&CancelToken::never(), config, 100)
        .await
        .expect("silent WAV must decode to a finalised analysis");
    let waveform = analysis
        .waveform
        .expect("the registered waveform analyzer must fill its slot");

    assert!(
        (1..=100).contains(&waveform.len()),
        "requested buckets are an upper bound, got {}",
        waveform.len()
    );
    assert!(
        waveform.buckets().iter().all(|b| *b == Bucket::default()),
        "a silent source must yield all-zero buckets: {:?}",
        waveform.buckets()
    );
}

#[kithara::test(
    tokio,
    timeout(Duration::from_secs(2)),
    env(KITHARA_HANG_TIMEOUT_SECS = "2")
)]
async fn runner_returns_nothing_when_cancelled_upfront() {
    let server = TestServerHelper::new().await;
    let url = server.silence(&silence_wav_spec()).await;
    let config =
        ResourceConfig::new(url.as_str()).expect("silence URL must build a ResourceConfig");

    let master = CancelToken::never();
    master.cancel();
    assert!(
        run_analysis(&master, config, 100).await.is_none(),
        "a pre-cancelled analysis must not return an envelope"
    );
}
