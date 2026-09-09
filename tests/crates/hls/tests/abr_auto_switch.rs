use std::sync::atomic::{AtomicUsize, Ordering};

use kithara::{
    assets::{AssetStore, StorageBackend},
    audio::{AudioConfig, AudioSession},
    events::EventBus,
    hls::{Hls, HlsConfig},
    platform::{
        CancelToken,
        sync::Arc,
        time::Duration,
        tokio::task::{spawn, spawn_blocking},
    },
    play::{PlayWorker, PlayWorkerConfig},
    stream::{AudioCodec, ContainerFormat, MediaInfo},
};
use kithara_integration_tests::{
    TestTempDir, abr_fast, auto,
    bufpool_ext::{TestPools, pools},
    fixture_protocol::DelayRule,
    hls_server::{HlsTestServer, HlsTestServerConfig},
    reads::read_to_eof,
    temp_dir,
};
#[cfg(not(target_arch = "wasm32"))]
use kithara_test_fixtures::hls_fixtures::{hls_pcm_thirty, hls_stream_header};
use tracing::info;

use crate::common::test_defaults::SawWav;

struct Consts;
impl Consts {
    const D: SawWav = SawWav::DEFAULT;
    const SEGMENT_COUNT: usize = 30;
}

/// ABR must switch variant at least once during HLS playback.
///
/// V0 segments are delayed after segment 3, making V0 throughput low relative
/// to its declared bandwidth (5 Mbps). ABR should down-switch to V1 (1 Mbps).
///
/// Also verifies that `content_duration` from fast initial segments (< 10ms)
/// is accumulated for buffer level tracking, which is required for up-switch
/// decisions (`min_buffer_for_up_switch_secs` check).
#[kithara::fixture]
async fn audio_server(hls_stream_header: Vec<u8>, hls_pcm_thirty: Vec<u8>) -> HlsTestServer {
    let init_segment = Arc::new(hls_stream_header);
    let pcm_data = Arc::new(hls_pcm_thirty);

    let segment_duration = Consts::D.segment_size as f64
        / (f64::from(Consts::D.sample_rate) * f64::from(Consts::D.channels) * 2.0);

    let server = HlsTestServer::new(HlsTestServerConfig {
        variant_count: 2,
        segments_per_variant: Consts::SEGMENT_COUNT,
        segment_size: Consts::D.segment_size,
        segment_duration_secs: segment_duration,
        custom_data_per_variant: Some(vec![Arc::clone(&pcm_data), Arc::clone(&pcm_data)]),
        init_data_per_variant: Some(vec![Arc::clone(&init_segment), Arc::clone(&init_segment)]),
        variant_bandwidths: Some(vec![5_000_000, 1_000_000]),
        codecs: Some("wav".to_string()),
        delay_rules: vec![DelayRule {
            variant: Some(0),
            segment_gte: Some(3),
            delay_ms: 500,
            ..Default::default()
        }],
        ..Default::default()
    })
    .await;

    server
}

#[kithara::test(
    native,
    tokio,
    serial,
    timeout(Duration::from_secs(30)),
    hang_timeout_secs(3),
    tracing("kithara_abr=debug,kithara_audio=debug,kithara_hls=debug,kithara_stream=debug")
)]
async fn abr_auto_switch_during_playback(
    #[future(awt)] audio_server: HlsTestServer,
    temp_dir: TestTempDir,
    _abr_fast: kithara::abr::AbrSettings,
) {
    let server = audio_server;
    let url = server.url("/master.m3u8");
    info!(%url, "HLS server ready with 2 variants");

    let cancel = CancelToken::never();
    let pools = pools();
    let worker = PlayWorker::new(
        PlayWorkerConfig::builder(pools.clone())
            .cancel(cancel.clone())
            .build(),
    );

    let bus = EventBus::new(32);
    let switches = Arc::new(AtomicUsize::new(0));
    let switches_bg = switches.clone();
    let mut events_rx = bus.subscribe();
    spawn(async move {
        use kithara::platform::tokio::sync::broadcast::error::RecvError;
        loop {
            match events_rx.recv().await {
                Ok(env) => {
                    let ev = env.event;
                    let ev_str = format!("{ev:?}");
                    if ev_str.contains("VariantApplied") {
                        switches_bg.fetch_add(1, Ordering::Relaxed);
                        info!("ABR switch: {ev_str}");
                    }
                }
                Err(RecvError::Lagged(_)) => continue,
                Err(RecvError::Closed) => break,
            }
        }
    });

    let hls_config = HlsConfig::for_url(url)
        .store(
            AssetStore::builder(pools.clone())
                .backend(StorageBackend::Disk {
                    root: temp_dir.path().to_path_buf(),
                })
                .build(),
        )
        .pools(pools)
        .cancel(cancel)
        .events(bus.clone())
        .initial_abr_mode(auto(0))
        .build();

    let wav_info = MediaInfo::builder()
        .maybe_codec(Some(AudioCodec::Pcm))
        .maybe_container(Some(ContainerFormat::Wav))
        .build();
    let config = AudioConfig::<Hls<TestPools>>::for_stream(hls_config)
        .events(bus)
        .media_info(wav_info)
        .build();
    let mut audio = worker
        .open(config)
        .await
        .expect("create Audio<Stream<Hls>>");

    let abr = audio.abr_handle();

    let result = spawn_blocking(move || {
        let total_samples = read_to_eof(&mut audio);
        info!(total_samples, "playback finished");
        total_samples
    })
    .await
    .expect("spawn_blocking");

    let switch_count = switches.load(Ordering::Relaxed);
    let final_variant = abr.and_then(|h| h.current_variant_index());
    info!(
        switch_count,
        ?final_variant,
        total_samples = result,
        "test complete"
    );

    assert!(result > 0, "expected audio output, got 0 samples");
    // Assert on the authoritative ABR state, not the VariantApplied event
    // count: the event rides a bounded broadcast and can be dropped under
    // full-suite load, but the committed variant index is lossless.
    assert!(
        final_variant.is_some_and(|v| v != 0),
        "ABR must down-switch away from the slow variant 0 during playback; \
         final variant index = {final_variant:?} (observed {switch_count} VariantApplied events)"
    );
}
