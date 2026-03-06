//! Concurrent HLS instance tests.
//!
//! Verifies that 2, 4, and 8 `Audio<Stream<Hls>>` instances can run
//! concurrently and each reads PCM data to EOF.
//! Tests both manual variant (no ABR) and auto ABR modes.

use std::{path::Path, sync::Arc};

use kithara::{
    assets::StoreOptions,
    audio::{Audio, AudioConfig},
    hls::{AbrMode, AbrOptions, Hls, HlsConfig},
    stream::{AudioCodec, ContainerFormat, MediaInfo, Stream},
};
use kithara_integration_tests::hls_fixture::{HlsTestServer, HlsTestServerConfig};
#[cfg(target_arch = "wasm32")]
use kithara_platform::thread;
use kithara_platform::{time::Duration, tokio::task::spawn_blocking};
use kithara_test_utils::{TestTempDir, tracing_setup, wav::create_test_wav};
use tokio_util::sync::CancellationToken;
use tracing::info;

use crate::common::test_defaults::SawWav;

const D: SawWav = SawWav::DEFAULT;
#[cfg(not(target_arch = "wasm32"))]
const SEGMENT_COUNT: usize = 10; // Smaller than stress test — enough for concurrency check.
#[cfg(target_arch = "wasm32")]
const SEGMENT_COUNT: usize = 4; // Keep fixture session payload small in browser-runner tests.
#[cfg(target_arch = "wasm32")]
const MAX_ZERO_READS: usize = 200;
#[cfg(target_arch = "wasm32")]
const MIN_SAMPLES_PER_INSTANCE: u64 = 8192;

/// Read all PCM data from an `Audio<Stream<Hls>>` instance to EOF.
///
/// Returns the total number of samples read.
fn read_to_eof(audio: &mut Audio<Stream<Hls>>) -> u64 {
    let mut buf = vec![0.0f32; 4096];
    let mut total = 0u64;
    loop {
        let n = audio.read(&mut buf);
        if n == 0 {
            break;
        }
        for &s in &buf[..n] {
            assert!(s.is_finite(), "non-finite sample at offset {total}");
        }
        total += n as u64;
    }
    assert!(audio.is_eof(), "expected EOF after reading all data");
    total
}

#[cfg(target_arch = "wasm32")]
fn read_for_concurrency_check(audio: &mut Audio<Stream<Hls>>) -> u64 {
    let mut buf = vec![0.0f32; 4096];
    let mut total = 0u64;
    let mut zero_reads = 0usize;

    while total < MIN_SAMPLES_PER_INSTANCE && zero_reads < MAX_ZERO_READS {
        let n = audio.read(&mut buf);
        if n == 0 {
            if audio.is_eof() {
                break;
            }
            zero_reads += 1;
            thread::sleep(Duration::from_millis(10));
            continue;
        }

        zero_reads = 0;
        for &sample in &buf[..n] {
            assert!(sample.is_finite(), "non-finite sample at offset {total}");
        }
        total += n as u64;
    }

    assert!(
        total >= MIN_SAMPLES_PER_INSTANCE,
        "expected at least {MIN_SAMPLES_PER_INSTANCE} samples, got {total}",
    );
    total
}

#[cfg(not(target_arch = "wasm32"))]
fn read_for_concurrency_check(audio: &mut Audio<Stream<Hls>>) -> u64 {
    read_to_eof(audio)
}

/// Create an HLS server with WAV segments.
async fn create_hls_server(wav_data: Arc<Vec<u8>>) -> HlsTestServer {
    let segment_duration = D.segment_size as f64 / (D.sample_rate as f64 * D.channels as f64 * 2.0);

    HlsTestServer::new(HlsTestServerConfig {
        segments_per_variant: SEGMENT_COUNT,
        segment_size: D.segment_size,
        segment_duration_secs: segment_duration,
        custom_data: Some(wav_data),
        ..Default::default()
    })
    .await
}

/// Create an HLS server with 2 ABR variants of different bandwidth.
async fn create_hls_server_abr(wav_data: Arc<Vec<u8>>) -> HlsTestServer {
    let segment_duration = D.segment_size as f64 / (D.sample_rate as f64 * D.channels as f64 * 2.0);

    HlsTestServer::new(HlsTestServerConfig {
        variant_count: 2,
        segments_per_variant: SEGMENT_COUNT,
        segment_size: D.segment_size,
        segment_duration_secs: segment_duration,
        custom_data: Some(wav_data),
        variant_bandwidths: Some(vec![5_000_000, 1_000_000]),
        ..Default::default()
    })
    .await
}

/// Create an `Audio<Stream<Hls>>` for a single-variant (manual ABR) stream.
async fn create_hls_audio(server: &HlsTestServer, cache_dir: &Path) -> Audio<Stream<Hls>> {
    let url = server.url("/master.m3u8").expect("url");
    let cancel = CancellationToken::new();

    let hls_config = HlsConfig::new(url)
        .with_store(StoreOptions::new(cache_dir))
        .with_cancel(cancel)
        .with_abr(AbrOptions {
            mode: AbrMode::Manual(0),
            ..AbrOptions::default()
        });

    let wav_info = MediaInfo::new(Some(AudioCodec::Pcm), Some(ContainerFormat::Wav));

    let config = AudioConfig::<Hls>::new(hls_config).with_media_info(wav_info);

    Audio::<Stream<Hls>>::new(config)
        .await
        .expect("create Audio<Stream<Hls>>")
}

/// Create an `Audio<Stream<Hls>>` with auto ABR.
async fn create_hls_audio_abr(server: &HlsTestServer, cache_dir: &Path) -> Audio<Stream<Hls>> {
    let url = server.url("/master.m3u8").expect("url");
    let cancel = CancellationToken::new();

    let hls_config = HlsConfig::new(url)
        .with_store(StoreOptions::new(cache_dir))
        .with_cancel(cancel)
        .with_abr(AbrOptions {
            mode: AbrMode::Auto(Some(0)),
            ..AbrOptions::default()
        });

    let wav_info = MediaInfo::new(Some(AudioCodec::Pcm), Some(ContainerFormat::Wav));

    let config = AudioConfig::<Hls>::new(hls_config).with_media_info(wav_info);

    Audio::<Stream<Hls>>::new(config)
        .await
        .expect("create Audio<Stream<Hls>> with ABR")
}

/// Generate WAV data for the test (total size = segments * `segment_size`).
fn generate_wav_data() -> Arc<Vec<u8>> {
    let total_bytes = SEGMENT_COUNT * D.segment_size;
    let bytes_per_frame = D.channels as usize * 2;
    let header_size = 44;
    let sample_count = (total_bytes - header_size) / bytes_per_frame;
    Arc::new(create_test_wav(sample_count, D.sample_rate, D.channels))
}

// Tests

/// 2 concurrent HLS instances (manual variant, no ABR).
#[kithara::test(tokio, browser, serial, timeout(Duration::from_secs(60)))]
async fn two_hls_instances(_tracing_setup: ()) {
    let wav_data = generate_wav_data();

    // Each instance needs its own server (binds a random port) and cache dir.
    let mut handles = Vec::new();
    for i in 0..2 {
        let server = create_hls_server(Arc::clone(&wav_data)).await;
        let temp = TestTempDir::new();
        let audio = create_hls_audio(&server, temp.path()).await;
        // Keep server and temp alive until reader finishes.
        handles.push(spawn_blocking(move || {
            let _server = server;
            let _temp = temp;
            let mut audio = audio;
            let total = read_for_concurrency_check(&mut audio);
            info!(instance = i, total_samples = total, "instance finished");
            (i, total)
        }));
    }

    let mut results = Vec::new();
    for h in handles {
        results.push(h.await.expect("join"));
    }

    info!(?results, "all HLS instances done");
    for (id, total) in &results {
        assert!(*total > 0, "HLS instance {id} read 0 samples");
    }
}

/// 4 concurrent HLS instances (manual variant, no ABR).
#[kithara::test(tokio, browser, serial, timeout(Duration::from_secs(60)))]
async fn four_hls_instances(_tracing_setup: ()) {
    let wav_data = generate_wav_data();

    let mut handles = Vec::new();
    for i in 0..4 {
        let server = create_hls_server(Arc::clone(&wav_data)).await;
        let temp = TestTempDir::new();
        let audio = create_hls_audio(&server, temp.path()).await;
        handles.push(spawn_blocking(move || {
            let _server = server;
            let _temp = temp;
            let mut audio = audio;
            let total = read_for_concurrency_check(&mut audio);
            info!(instance = i, total_samples = total, "instance finished");
            (i, total)
        }));
    }

    let mut results = Vec::new();
    for h in handles {
        results.push(h.await.expect("join"));
    }

    info!(?results, "all HLS instances done");
    for (id, total) in &results {
        assert!(*total > 0, "HLS instance {id} read 0 samples");
    }
}

/// 8 concurrent HLS instances (manual variant, no ABR).
#[kithara::test(tokio, browser, serial, timeout(Duration::from_secs(120)))]
async fn eight_hls_instances(_tracing_setup: ()) {
    let wav_data = generate_wav_data();

    let mut handles = Vec::new();
    for i in 0..8 {
        let server = create_hls_server(Arc::clone(&wav_data)).await;
        let temp = TestTempDir::new();
        let audio = create_hls_audio(&server, temp.path()).await;
        handles.push(spawn_blocking(move || {
            let _server = server;
            let _temp = temp;
            let mut audio = audio;
            let total = read_for_concurrency_check(&mut audio);
            info!(instance = i, total_samples = total, "instance finished");
            (i, total)
        }));
    }

    let mut results = Vec::new();
    for h in handles {
        results.push(h.await.expect("join"));
    }

    info!(?results, "all HLS instances done");
    for (id, total) in &results {
        assert!(*total > 0, "HLS instance {id} read 0 samples");
    }
}

/// 4 concurrent HLS instances with auto ABR (2 variants).
#[kithara::test(tokio, browser, serial, timeout(Duration::from_secs(60)))]
async fn four_hls_instances_with_abr(_tracing_setup: ()) {
    let wav_data = generate_wav_data();

    let mut handles = Vec::new();
    for i in 0..4 {
        let server = create_hls_server_abr(Arc::clone(&wav_data)).await;
        let temp = TestTempDir::new();
        let audio = create_hls_audio_abr(&server, temp.path()).await;
        handles.push(spawn_blocking(move || {
            let _server = server;
            let _temp = temp;
            let mut audio = audio;
            let total = read_for_concurrency_check(&mut audio);
            info!(instance = i, total_samples = total, "instance finished");
            (i, total)
        }));
    }

    let mut results = Vec::new();
    for h in handles {
        results.push(h.await.expect("join"));
    }

    info!(?results, "all HLS+ABR instances done");
    for (id, total) in &results {
        assert!(*total > 0, "HLS+ABR instance {id} read 0 samples");
    }
}
