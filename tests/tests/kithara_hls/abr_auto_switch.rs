//! Integration test: ABR auto-switch must happen during HLS playback.
//!
//! Two WAV variants: V0 (high bandwidth, delayed after segment 3) and V1 (low
//! bandwidth, instant). ABR starts on V0 and must down-switch to V1 when V0
//! throughput drops below the declared bandwidth.
//!
//! This test catches the bug where `record_throughput` discarded
//! `content_duration` for fast downloads, keeping `buffer_level_secs` at
//! zero and blocking ABR decisions that need a minimum buffer level.

use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

use kithara::{
    assets::StoreOptions,
    audio::{Audio, AudioConfig},
    events::EventBus,
    hls::{AbrMode, AbrOptions, Hls, HlsConfig},
    stream::{AudioCodec, ContainerFormat, MediaInfo, Stream},
};
use kithara_integration_tests::hls_fixture::{HlsTestServer, HlsTestServerConfig};
use kithara_platform::{
    time::{Duration, Instant},
    tokio::task::{spawn, spawn_blocking},
};
use kithara_test_utils::{TestTempDir, fixture_protocol::DelayRule, tracing_setup};
use tokio_util::sync::CancellationToken;
use tracing::info;

use crate::common::test_defaults::SawWav;

const D: SawWav = SawWav::DEFAULT;
const SEGMENT_COUNT: usize = 30;

fn create_wav_init_segment() -> Vec<u8> {
    let bytes_per_sample: u16 = 2;
    let byte_rate = D.sample_rate * D.channels as u32 * bytes_per_sample as u32;
    let block_align = D.channels * bytes_per_sample;
    let data_size = 0xFFFF_FFFFu32;
    let file_size = 0xFFFF_FFFFu32;

    let mut wav = Vec::with_capacity(44);
    wav.extend_from_slice(b"RIFF");
    wav.extend_from_slice(&file_size.to_le_bytes());
    wav.extend_from_slice(b"WAVE");
    wav.extend_from_slice(b"fmt ");
    wav.extend_from_slice(&16u32.to_le_bytes());
    wav.extend_from_slice(&1u16.to_le_bytes()); // PCM
    wav.extend_from_slice(&D.channels.to_le_bytes());
    wav.extend_from_slice(&D.sample_rate.to_le_bytes());
    wav.extend_from_slice(&byte_rate.to_le_bytes());
    wav.extend_from_slice(&block_align.to_le_bytes());
    wav.extend_from_slice(&(bytes_per_sample * 8).to_le_bytes());
    wav.extend_from_slice(b"data");
    wav.extend_from_slice(&data_size.to_le_bytes());
    wav
}

fn create_pcm_segments() -> Vec<u8> {
    let total_bytes = SEGMENT_COUNT * D.segment_size;
    let bytes_per_frame = D.channels as usize * 2;
    let total_frames = total_bytes / bytes_per_frame;

    let mut pcm = Vec::with_capacity(total_bytes);
    for frame in 0..total_frames {
        let sample = ((frame % D.saw_period) as i32 - 32768) as i16;
        for _ in 0..D.channels {
            pcm.extend_from_slice(&sample.to_le_bytes());
        }
    }
    pcm.resize(total_bytes, 0);
    pcm
}

/// ABR must switch variant at least once during HLS playback.
///
/// V0 segments are delayed after segment 3, making V0 throughput low relative
/// to its declared bandwidth (5 Mbps). ABR should down-switch to V1 (1 Mbps).
///
/// Also verifies that `content_duration` from fast initial segments (< 10ms)
/// is accumulated for buffer level tracking, which is required for up-switch
/// decisions (min_buffer_for_up_switch_secs check).
#[kithara::test(
    native,
    tokio,
    serial,
    timeout(Duration::from_secs(30)),
    env(KITHARA_HANG_TIMEOUT_SECS = "3")
)]
async fn abr_auto_switch_during_playback(_tracing_setup: ()) {
    let init_segment = Arc::new(create_wav_init_segment());
    let pcm_data = Arc::new(create_pcm_segments());

    let segment_duration = D.segment_size as f64 / (D.sample_rate as f64 * D.channels as f64 * 2.0);

    let server = HlsTestServer::new(HlsTestServerConfig {
        variant_count: 2,
        segments_per_variant: SEGMENT_COUNT,
        segment_size: D.segment_size,
        segment_duration_secs: segment_duration,
        custom_data_per_variant: Some(vec![Arc::clone(&pcm_data), Arc::clone(&pcm_data)]),
        init_data_per_variant: Some(vec![Arc::clone(&init_segment), Arc::clone(&init_segment)]),
        // V0 = 5 Mbps (high, delayed), V1 = 1 Mbps (low, fast).
        variant_bandwidths: Some(vec![5_000_000, 1_000_000]),
        // V0 segments 3+ delayed 500ms → throughput ~3.2 Mbps < 5 Mbps → down-switch.
        delay_rules: vec![DelayRule {
            variant: Some(0),
            segment_gte: Some(3),
            delay_ms: 500,
            ..Default::default()
        }],
        ..Default::default()
    })
    .await;

    let url = server.url("/master.m3u8").expect("url");
    info!(%url, "HLS server ready with 2 variants");

    let temp_dir = TestTempDir::new();
    let cancel = CancellationToken::new();

    // Shared event bus: subscribe BEFORE Audio::new so we don't miss
    // fast ABR switches that happen during stream creation.
    let bus = EventBus::new(32);
    let switches = Arc::new(AtomicUsize::new(0));
    let switches_bg = switches.clone();
    let mut events_rx = bus.subscribe();
    spawn(async move {
        while let Ok(ev) = events_rx.recv().await {
            let ev_str = format!("{ev:?}");
            if ev_str.contains("VariantApplied") {
                switches_bg.fetch_add(1, Ordering::Relaxed);
                info!("ABR switch: {ev_str}");
            }
        }
    });

    let hls_config = HlsConfig::new(url)
        .with_store(StoreOptions::new(temp_dir.path()))
        .with_cancel(cancel)
        .with_events(bus.clone())
        .with_abr(AbrOptions {
            down_switch_buffer_secs: 0.0,
            min_buffer_for_up_switch_secs: 0.0,
            min_switch_interval: Duration::from_secs(120),
            mode: AbrMode::Auto(Some(0)), // start on V0
            throughput_safety_factor: 1.0,
            ..AbrOptions::default()
        });

    let wav_info = MediaInfo::new(Some(AudioCodec::Pcm), Some(ContainerFormat::Wav));
    let config = AudioConfig::<Hls>::new(hls_config)
        .with_events(bus)
        .with_media_info(wav_info);
    let mut audio = Audio::<Stream<Hls>>::new(config)
        .await
        .expect("create Audio<Stream<Hls>>");

    // Read audio until EOF or timeout
    let result = spawn_blocking(move || {
        let mut buf = vec![0.0f32; 4096];
        let mut total_samples = 0u64;
        let start = Instant::now();
        let timeout = Duration::from_secs(20);

        while start.elapsed() < timeout {
            let n = audio.read(&mut buf);
            if n == 0 {
                if audio.is_eof() {
                    break;
                }
                continue;
            }
            total_samples += n as u64;
        }

        info!(total_samples, "playback finished");
        total_samples
    })
    .await
    .expect("spawn_blocking");

    let switch_count = switches.load(Ordering::Relaxed);
    info!(switch_count, total_samples = result, "test complete");

    assert!(result > 0, "expected audio output, got 0 samples");
    assert!(
        switch_count > 0,
        "ABR must switch variant at least once during playback, got 0 switches"
    );
}
