#![cfg(not(target_arch = "wasm32"))]

//! Decoder construction must remain bounded when media data is withheld.
//! Init-only backends can open from the header; media-probing backends report
//! the stream's typed readiness error. Releasing the body restores playback.
use std::num::NonZeroUsize;

use kithara::{
    assets::{AssetStore, StorageBackend},
    audio::AudioConfig,
    hls::{Hls, HlsConfig},
    net::{NetOptions, RetryPolicy},
    platform::{
        CancelToken,
        sync::Arc,
        // The alias keeps the construction deadline on the caller's wall clock.
        time::{Duration, Instant, Instant as RealInstant},
        tokio,
    },
    play::{PlayWorker, PlayWorkerConfig},
    stream::{AudioCodec, ContainerFormat, MediaInfo},
};
use kithara_integration_tests::{
    auto,
    bufpool_ext::{Pools, TestPools, pools},
    hls_server::{HlsTestServer, HlsTestServerConfig},
};
use kithara_test_fixtures::hls_fixtures::{hls_header_boundary, hls_pcm_boundary};
#[cfg(not(target_os = "android"))]
use tracing::info;

const SAMPLE_RATE: u32 = 44_100;
const CHANNELS: u16 = 2;
const SEGMENT_SIZE: usize = 32_768;
const SEGMENT_COUNT: usize = 8;

#[kithara::fixture]
fn fixture_config(hls_header_boundary: Vec<u8>, hls_pcm_boundary: Vec<u8>) -> HlsTestServerConfig {
    let init_segment = Arc::new(hls_header_boundary);
    let pcm = Arc::new(hls_pcm_boundary);
    let segment_duration = SEGMENT_SIZE as f64
        / (f64::from(SAMPLE_RATE) * f64::from(CHANNELS) * size_of::<i16>() as f64);
    HlsTestServerConfig {
        variant_count: 1,
        segments_per_variant: SEGMENT_COUNT,
        segment_size: SEGMENT_SIZE,
        segment_duration_secs: segment_duration,
        custom_data_per_variant: Some(vec![pcm]),
        init_data_per_variant: Some(vec![init_segment]),
        variant_bandwidths: Some(vec![1_000_000]),
        ..Default::default()
    }
}

fn audio_config(
    server: &HlsTestServer,
    pools: &Pools,
    cancel: &CancelToken,
) -> AudioConfig<Hls<TestPools>> {
    let store = AssetStore::builder(pools.clone())
        .backend(StorageBackend::Memory)
        .cache_capacity(NonZeroUsize::new(8).expect("nonzero"))
        .build();
    // Exhaust a withheld body's retries within the five-second construction bound.
    let net = NetOptions::builder()
        .inactivity_timeout(Duration::from_millis(400))
        .retry_policy(
            RetryPolicy::builder()
                .max_retries(2)
                .base_delay(Duration::from_millis(20))
                .max_delay(Duration::from_millis(100))
                .build(),
        )
        .build();
    let hls_config = HlsConfig::for_url(server.url("/master.m3u8"))
        .store(store)
        .pools(pools.clone())
        .cancel(cancel.clone())
        .initial_abr_mode(auto(0))
        .net_options(net)
        .build();
    let wav_info = MediaInfo::builder()
        .maybe_codec(Some(AudioCodec::Pcm))
        .maybe_container(Some(ContainerFormat::Wav))
        .build();
    AudioConfig::<Hls<TestPools>>::for_stream(hls_config)
        .media_info(wav_info)
        .build()
}

#[kithara::test(
    tokio,
    native,
    serial,
    timeout(Duration::from_secs(30)),
    tracing("kithara_audio=info,kithara_hls=info,kithara_stream=info")
)]
async fn audio_new_is_bounded_when_first_segment_withheld(fixture_config: HlsTestServerConfig) {
    let (server, gate) = HlsTestServer::with_segment_gate(fixture_config, 0, 0).await;
    let cancel = CancelToken::never();
    let pools = pools();
    let worker = PlayWorker::new(
        PlayWorkerConfig::builder(pools.clone())
            .cancel(cancel.clone())
            .build(),
    );
    let started = RealInstant::now();
    let result = worker.open(audio_config(&server, &pools, &cancel)).await;
    let elapsed = started.elapsed();

    assert!(
        elapsed < Duration::from_secs(5),
        "opening must be bounded while media is withheld: {elapsed:?}"
    );
    gate.release();

    #[cfg(target_os = "android")]
    {
        let mut audio = result.expect("the WAV init is sufficient for native construction");
        let samples = tokio::task::spawn_blocking(move || {
            kithara_integration_tests::reads::read_to_eof(&mut audio)
        })
        .await
        .expect("read joins");
        let expected = SEGMENT_COUNT * SEGMENT_SIZE / size_of::<i16>();
        assert_eq!(
            samples, expected as u64,
            "released media must decode completely"
        );
    }
    #[cfg(not(target_os = "android"))]
    {
        let err = result
            .err()
            .expect("the container probe requires media bytes");
        let message = err.to_string();
        info!(?elapsed, %message, is_interrupted = err.is_interrupted(), "PlayWorker::open failed");
        let lower = message.to_ascii_lowercase();
        assert!(
            lower.contains("not ready") || lower.contains("wait budget"),
            "opening must preserve the stream readiness error: {message}"
        );
        assert!(
            !lower.contains("timed out") && !lower.contains("timeout"),
            "a source wait must not become a synthetic timeout: {message}"
        );
    }
}

#[kithara::test(
    tokio,
    native,
    serial,
    timeout(Duration::from_secs(30)),
    tracing("kithara_audio=info,kithara_hls=info,kithara_stream=info")
)]
async fn audio_new_succeeds_when_first_segment_released_during_probe(
    fixture_config: HlsTestServerConfig,
) {
    let (server, gate) = HlsTestServer::with_segment_gate(fixture_config, 0, 0).await;
    let cancel = CancelToken::never();
    let pools = pools();
    let worker = PlayWorker::new(
        PlayWorkerConfig::builder(pools.clone())
            .cancel(cancel.clone())
            .build(),
    );
    // Release the body when its GET reaches the gate.
    let release_gate = gate.clone();
    let releaser = tokio::task::spawn(async move {
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            if release_gate.requested() > 0 {
                release_gate.release();
                return;
            }
            assert!(
                Instant::now() < deadline,
                "withheld GET never reached the gate"
            );
            tokio::task::yield_now().await;
        }
    });

    let result = worker.open(audio_config(&server, &pools, &cancel)).await;
    releaser.await.expect("releaser joins");

    assert!(
        result.is_ok(),
        "PlayWorker::open must succeed once the slow first segment arrives, got {:?}",
        result.err().map(|e| e.to_string())
    );
}
