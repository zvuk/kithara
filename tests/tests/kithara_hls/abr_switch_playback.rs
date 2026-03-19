//! Regression test: ABR variant switch during normal playback must not hang
//! the shared worker loop.
//!
//! Reproduces the production crash where playing MP3 + HLS in the same
//! shared worker caused a deadlock when ABR switched on the HLS track.
//! The `detect_format_change` → `format_change_segment_range` → recreation
//! path must complete without blocking the worker for other tracks.

use std::sync::Arc;

use kithara::{
    assets::StoreOptions,
    audio::{Audio, AudioConfig, PcmReader},
    hls::{AbrMode, AbrOptions, Hls, HlsConfig},
    stream::{AudioCodec, ContainerFormat, MediaInfo, Stream},
};
use kithara_integration_tests::hls_fixture::{HlsTestServer, HlsTestServerConfig};
use kithara_platform::time::{Duration, Instant, sleep};
use kithara_test_utils::{TestTempDir, fixture_protocol::DelayRule};
use tokio_util::sync::CancellationToken;
use tracing::info;

use crate::common::test_defaults::SawWav;

const D: SawWav = SawWav::DEFAULT;
const SEGMENT_COUNT: usize = 20;

fn create_wav_init_segment() -> Vec<u8> {
    let bytes_per_sample: u16 = 2;
    let byte_rate = D.sample_rate * D.channels as u32 * bytes_per_sample as u32;
    let block_align = D.channels * bytes_per_sample;

    let mut wav = Vec::with_capacity(44);
    wav.extend_from_slice(b"RIFF");
    wav.extend_from_slice(&0xFFFF_FFFFu32.to_le_bytes());
    wav.extend_from_slice(b"WAVE");
    wav.extend_from_slice(b"fmt ");
    wav.extend_from_slice(&16u32.to_le_bytes());
    wav.extend_from_slice(&1u16.to_le_bytes());
    wav.extend_from_slice(&D.channels.to_le_bytes());
    wav.extend_from_slice(&D.sample_rate.to_le_bytes());
    wav.extend_from_slice(&byte_rate.to_le_bytes());
    wav.extend_from_slice(&block_align.to_le_bytes());
    wav.extend_from_slice(&(bytes_per_sample * 8).to_le_bytes());
    wav.extend_from_slice(b"data");
    wav.extend_from_slice(&0xFFFF_FFFFu32.to_le_bytes());
    wav
}

fn create_pcm_data(sample_fn: fn(usize) -> i16) -> Vec<u8> {
    let total_bytes = SEGMENT_COUNT * D.segment_size;
    let bytes_per_frame = D.channels as usize * 2;
    let total_frames = total_bytes / bytes_per_frame;

    let mut pcm = Vec::with_capacity(total_bytes);
    for frame in 0..total_frames {
        let sample = sample_fn(frame);
        for _ in 0..D.channels {
            pcm.extend_from_slice(&sample.to_le_bytes());
        }
    }
    pcm.resize(total_bytes, 0);
    pcm
}

fn ascending(frame: usize) -> i16 {
    ((frame % D.saw_period) as i32 - 32768) as i16
}

fn descending(frame: usize) -> i16 {
    (32767 - (frame % D.saw_period) as i32) as i16
}

/// ABR switch during normal playback must not hang the worker.
///
/// Regression for: `[HangDetector] run_shared_worker_loop no progress for 10s`
/// when playing track.mp3 alongside HLS with ABR auto-switch.
#[kithara::test(
    tokio,
    native,
    serial,
    timeout(Duration::from_secs(30)),
    env(KITHARA_HANG_TIMEOUT_SECS = "3")
)]
async fn abr_switch_during_playback_does_not_hang_worker() {
    let init = Arc::new(create_wav_init_segment());
    let v0 = Arc::new(create_pcm_data(ascending));
    let v1 = Arc::new(create_pcm_data(descending));

    let seg_dur = D.segment_size as f64 / (D.sample_rate as f64 * D.channels as f64 * 2.0);

    let server = HlsTestServer::new(HlsTestServerConfig {
        variant_count: 2,
        segments_per_variant: SEGMENT_COUNT,
        segment_size: D.segment_size,
        segment_duration_secs: seg_dur,
        custom_data_per_variant: Some(vec![Arc::clone(&v0), Arc::clone(&v1)]),
        init_data_per_variant: Some(vec![Arc::clone(&init), Arc::clone(&init)]),
        variant_bandwidths: Some(vec![5_000_000, 1_000_000]),
        // Delay V0 segments after segment 2 to force ABR downgrade to V1
        delay_rules: vec![DelayRule {
            variant: Some(0),
            segment_gte: Some(2),
            delay_ms: 800,
            ..Default::default()
        }],
        ..Default::default()
    })
    .await;

    let url = server.url("/master.m3u8").expect("url");
    let temp_dir = TestTempDir::new();
    let cancel = CancellationToken::new();

    let hls_config = HlsConfig::new(url)
        .with_store(StoreOptions::new(temp_dir.path()))
        .with_cancel(cancel)
        .with_abr(AbrOptions {
            down_switch_buffer_secs: 0.0,
            min_buffer_for_up_switch_secs: 0.0,
            min_switch_interval: Duration::from_secs(120),
            mode: AbrMode::Auto(Some(0)),
            throughput_safety_factor: 1.0,
            ..AbrOptions::default()
        });

    let wav_info = MediaInfo::new(Some(AudioCodec::Pcm), Some(ContainerFormat::Wav));
    let config = AudioConfig::<Hls>::new(hls_config).with_media_info(wav_info);
    let mut audio = Audio::<Stream<Hls>>::new(config)
        .await
        .expect("create audio");

    audio.preload();

    // Read chunks for up to 20 seconds. If ABR switch hangs the worker,
    // HangDetector (3s) will panic before the 30s test timeout.
    let deadline = Instant::now() + Duration::from_secs(20);
    let mut chunks_read = 0u64;

    while Instant::now() < deadline {
        if let Some(_chunk) = PcmReader::next_chunk(&mut audio) {
            chunks_read += 1;
        } else if audio.is_eof() {
            break;
        }
        sleep(Duration::from_millis(5)).await;
    }

    info!(chunks_read, "test completed");
    assert!(
        chunks_read > 10,
        "expected at least 10 chunks during playback, got {chunks_read}"
    );
}
