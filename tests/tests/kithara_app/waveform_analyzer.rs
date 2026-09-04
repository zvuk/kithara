//! Integration coverage for `kithara_app::waveform`: decode a fixture-server
//! WAV end to end through the production `TrackAnalysisRunner` (resource
//! open + shared analysis-worker thread) and assert the source-analysis
//! contract.
#![cfg(not(target_arch = "wasm32"))]

use std::num::NonZeroU32;

use kithara::{
    analysis::{BeatAnalysisConfig, Bucket},
    assets::StorageBackend,
    platform::{CancelToken, time::Duration},
    play::{PlayWorker, PlayWorkerConfig, ResourceConfig, ResourceSrc},
};
use kithara_app::{
    pools::{AppPools, AppResourceConfig, AppStore, AppWorker, Pools, build},
    waveform::{TrackAnalysis, TrackAnalysisRunner},
};
use kithara_integration_tests::TestServerHelper;
use kithara_test_fixtures::SignalAsset;

/// The fixtures decode at 44.1 kHz; the pass is opened on the same axis so
/// nothing is resampled on the way in.
const RATE: NonZeroU32 = NonZeroU32::new(44_100).expect("fixture rate is non-zero");
const CHUNK_SECONDS: NonZeroU32 = NonZeroU32::new(16).expect("fixture chunk duration is non-zero");

fn worker(pools: Pools) -> AppWorker {
    PlayWorker::new(PlayWorkerConfig::builder(pools).build())
}

fn memory_store(pools: Pools) -> AppStore {
    AppStore::builder(pools)
        .backend(StorageBackend::Memory)
        .build()
}

/// Run one analysis through the production runner and await its result.
async fn run_analysis(
    master: &CancelToken,
    config: AppResourceConfig,
    pools: Pools,
    buckets: usize,
) -> Option<TrackAnalysis> {
    let mut runner = TrackAnalysisRunner::new(
        master,
        None,
        CHUNK_SECONDS,
        buckets,
        BeatAnalysisConfig::default(),
        pools,
    );
    let mut rx = runner.analyze(config, "waveform-track".into(), RATE, 0, drop);

    // Staged analysis can emit twice (waveform, then waveform+beat).
    let mut last = None;
    while rx.changed().await.is_ok() {
        last = rx.borrow().clone();
    }
    last.map(Into::into)
}

#[kithara::test(tokio, timeout(Duration::from_secs(2)), hang_timeout_secs(2))]
async fn runner_silent_wav_yields_all_zero_envelope() {
    let server = TestServerHelper::new().await;
    let url = server.signal(SignalAsset::WAV_SILENCE_1S);
    let pools = build().expect("app pools");
    let config = ResourceConfig::<AppPools>::for_src(
        ResourceSrc::parse(url.as_str()).expect("silence URL must build a ResourceConfig"),
    )
    .store(memory_store(pools.clone()))
    .worker(worker(pools.clone()))
    .build();

    // A silent 1s WAV must decode end to end and finalise to a native-resolution
    // envelope capped by the requested maximum. No frames are loud, so nothing
    // normalises up to 1.0.
    let analysis = run_analysis(&CancelToken::never(), config, pools, 100)
        .await
        .expect("silent WAV must decode to a finalised analysis");
    let waveform = analysis
        .waveform()
        .cloned()
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

#[kithara::test(tokio, timeout(Duration::from_secs(2)), hang_timeout_secs(2))]
async fn runner_returns_nothing_when_cancelled_upfront() {
    let server = TestServerHelper::new().await;
    let url = server.signal(SignalAsset::WAV_SILENCE_1S);
    let pools = build().expect("app pools");
    let config = ResourceConfig::<AppPools>::for_src(
        ResourceSrc::parse(url.as_str()).expect("silence URL must build a ResourceConfig"),
    )
    .store(memory_store(pools.clone()))
    .worker(worker(pools.clone()))
    .build();

    let master = CancelToken::never();
    master.cancel();
    assert!(
        run_analysis(&master, config, pools, 100).await.is_none(),
        "a pre-cancelled analysis must not return an envelope"
    );
}
