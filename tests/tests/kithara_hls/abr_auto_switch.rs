use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

use kithara::{
    assets::StoreOptions,
    audio::{Audio, AudioConfig, ReadOutcome},
    events::EventBus,
    hls::{Hls, HlsConfig},
    stream::{AudioCodec, ContainerFormat, MediaInfo, Stream},
};
use kithara_integration_tests::{
    TestTempDir, abr_fast, auto,
    fixture_protocol::DelayRule,
    hls_server::{HlsTestServer, HlsTestServerConfig},
    signal_pcm::{Finite, SignalPcm, signal},
    temp_dir,
    wav::create_wav_header,
};
use kithara_platform::{
    CancellationToken,
    time::{Duration, Instant},
    tokio::task::{spawn, spawn_blocking},
};
use tracing::info;

use crate::common::test_defaults::SawWav;

struct Consts;
impl Consts {
    const D: SawWav = SawWav::DEFAULT;
    const SEGMENT_COUNT: usize = 30;
}

fn create_wav_init_segment() -> Vec<u8> {
    create_wav_header(Consts::D.sample_rate, Consts::D.channels, None)
}

fn create_pcm_segments() -> Vec<u8> {
    SignalPcm::new(
        signal::Sawtooth,
        Consts::D.sample_rate,
        Consts::D.channels,
        Finite::from_segments(
            Consts::SEGMENT_COUNT,
            Consts::D.segment_size,
            Consts::D.channels,
        ),
    )
    .into_vec()
}

/// ABR must switch variant at least once during HLS playback.
///
/// V0 segments are delayed after segment 3, making V0 throughput low relative
/// to its declared bandwidth (5 Mbps). ABR should down-switch to V1 (1 Mbps).
///
/// Also verifies that `content_duration` from fast initial segments (< 10ms)
/// is accumulated for buffer level tracking, which is required for up-switch
/// decisions (`min_buffer_for_up_switch_secs` check).
#[kithara::test(
    flash(false),
    native,
    tokio,
    serial,
    timeout(Duration::from_secs(30)),
    env(KITHARA_HANG_TIMEOUT_SECS = "3")
)]
async fn abr_auto_switch_during_playback(
    temp_dir: TestTempDir,
    _abr_fast: kithara_abr::AbrSettings,
) {
    let init_segment = Arc::new(create_wav_init_segment());
    let pcm_data = Arc::new(create_pcm_segments());

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

    let url = server.url("/master.m3u8");
    info!(%url, "HLS server ready with 2 variants");

    let cancel = CancellationToken::default();

    let bus = EventBus::new(32);
    let switches = Arc::new(AtomicUsize::new(0));
    let switches_bg = switches.clone();
    let mut events_rx = bus.subscribe();
    spawn(async move {
        use kithara_platform::tokio::sync::broadcast::error::RecvError;
        loop {
            match events_rx.recv().await {
                Ok(ev) => {
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
        .store(StoreOptions::new(temp_dir.path()))
        .cancel(cancel)
        .events(bus.clone())
        .initial_abr_mode(auto(0))
        .build();

    let wav_info = MediaInfo::new(Some(AudioCodec::Pcm), Some(ContainerFormat::Wav));
    let config = AudioConfig::<Hls>::for_stream(hls_config)
        .events(bus)
        .media_info(wav_info)
        .build();
    let mut audio = Audio::<Stream<Hls>>::new(config)
        .await
        .expect("create Audio<Stream<Hls>>");

    let abr = audio.abr_handle();

    let result = spawn_blocking(move || {
        let mut buf = vec![0.0f32; 4096];
        let mut total_samples = 0u64;
        let start = Instant::now();
        let timeout = Duration::from_secs(5);

        while start.elapsed() < timeout {
            match audio.read(&mut buf) {
                Ok(ReadOutcome::Pending { .. }) => continue,
                Ok(ReadOutcome::Frames { count, .. }) => {
                    total_samples += count.get() as u64;
                }
                Ok(ReadOutcome::Eof { .. }) => break,
                Err(e) => panic!("decode error: {e}"),
            }
        }

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
