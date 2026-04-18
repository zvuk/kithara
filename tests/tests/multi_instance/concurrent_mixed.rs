//! Mixed concurrent File + HLS instance tests.
//!
//! Verifies that File and HLS `Audio` instances can coexist and all
//! read PCM data to EOF.

use std::sync::Arc;

use kithara::{
    assets::StoreOptions,
    audio::{Audio, AudioConfig},
    file::{File, FileConfig},
    hls::{AbrMode, AbrOptions, Hls, HlsConfig},
    stream::{AudioCodec, ContainerFormat, MediaInfo, Stream},
};
use kithara_integration_tests::hls_fixture::{HlsTestServer, HlsTestServerConfig};
#[cfg(target_arch = "wasm32")]
use kithara_platform::thread;
use kithara_platform::{
    time::Duration,
    tokio::task::{JoinHandle, spawn_blocking},
};
use kithara_test_utils::{TestServerHelper, TestTempDir, wav::create_test_wav};
use tokio_util::sync::CancellationToken;
use tracing::info;

use crate::common::test_defaults::SawWav;

struct Consts;
impl Consts {
    const D: SawWav = SawWav::DEFAULT;
    #[cfg(not(target_arch = "wasm32"))]
    const SEGMENT_COUNT: usize = 10;
    #[cfg(target_arch = "wasm32")]
    const SEGMENT_COUNT: usize = 4;
    #[cfg(target_arch = "wasm32")]
    const MAX_ZERO_READS: usize = 200;
    #[cfg(target_arch = "wasm32")]
    const MIN_SAMPLES_PER_INSTANCE: u64 = 8192;
}

/// Result of one instance completing.
#[derive(Debug)]
struct InstanceResult {
    id: usize,
    kind: &'static str,
    total_samples: u64,
}

/// Read File audio to EOF in blocking context.
fn read_file_to_eof(audio: &mut Audio<Stream<File>>) -> u64 {
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
    assert!(audio.is_eof(), "expected EOF");
    total
}

#[cfg(target_arch = "wasm32")]
fn read_file_for_concurrency_check(audio: &mut Audio<Stream<File>>) -> u64 {
    let mut buf = vec![0.0f32; 4096];
    let mut total = 0u64;
    let mut zero_reads = 0usize;

    while total < Consts::MIN_SAMPLES_PER_INSTANCE && zero_reads < Consts::MAX_ZERO_READS {
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
        total >= Consts::MIN_SAMPLES_PER_INSTANCE,
        "expected at least {Consts::MIN_SAMPLES_PER_INSTANCE} samples, got {total}",
    );
    total
}

#[cfg(not(target_arch = "wasm32"))]
fn read_file_for_concurrency_check(audio: &mut Audio<Stream<File>>) -> u64 {
    read_file_to_eof(audio)
}

/// Read HLS audio to EOF in blocking context.
fn read_hls_to_eof(audio: &mut Audio<Stream<Hls>>) -> u64 {
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
    assert!(audio.is_eof(), "expected EOF");
    total
}

#[cfg(target_arch = "wasm32")]
fn read_hls_for_concurrency_check(audio: &mut Audio<Stream<Hls>>) -> u64 {
    let mut buf = vec![0.0f32; 4096];
    let mut total = 0u64;
    let mut zero_reads = 0usize;

    while total < Consts::MIN_SAMPLES_PER_INSTANCE && zero_reads < Consts::MAX_ZERO_READS {
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
        total >= Consts::MIN_SAMPLES_PER_INSTANCE,
        "expected at least {Consts::MIN_SAMPLES_PER_INSTANCE} samples, got {total}",
    );
    total
}

#[cfg(not(target_arch = "wasm32"))]
fn read_hls_for_concurrency_check(audio: &mut Audio<Stream<Hls>>) -> u64 {
    read_hls_to_eof(audio)
}

fn generate_wav_data() -> Arc<Vec<u8>> {
    let total_bytes = Consts::SEGMENT_COUNT * Consts::D.segment_size;
    let bytes_per_frame = Consts::D.channels as usize * 2;
    let header_size = 44;
    let sample_count = (total_bytes - header_size) / bytes_per_frame;
    Arc::new(create_test_wav(
        sample_count,
        Consts::D.sample_rate,
        Consts::D.channels,
    ))
}

/// 2 File + 2 HLS instances running concurrently.
#[kithara::test(
    tokio,
    browser,
    serial,
    timeout(Duration::from_secs(30)),
    env(KITHARA_HANG_TIMEOUT_SECS = "2")
)]
async fn mixed_two_file_two_hls() {
    let wav_data = generate_wav_data();
    let file_server = TestServerHelper::new().await;

    let segment_duration = Consts::D.segment_size as f64
        / (f64::from(Consts::D.sample_rate) * f64::from(Consts::D.channels) * 2.0);

    let mut handles: Vec<JoinHandle<InstanceResult>> = Vec::new();
    let mut temps = Vec::new();
    let mut servers = Vec::new();

    // Spawn 2 File instances
    for i in 0..2 {
        let url = file_server.asset("test.mp3");
        let temp = TestTempDir::new();

        let file_config = FileConfig::new(url.into()).with_store(StoreOptions::new(temp.path()));
        let config = AudioConfig::<File>::new(file_config).with_hint("mp3");

        let mut audio = Audio::<Stream<File>>::new(config)
            .await
            .expect("create File audio");

        temps.push(temp);
        handles.push(spawn_blocking(move || {
            let total = read_file_for_concurrency_check(&mut audio);
            info!(instance = i, kind = "file", total_samples = total, "done");
            InstanceResult {
                id: i,
                kind: "file",
                total_samples: total,
            }
        }));
    }

    // Spawn 2 HLS instances
    for i in 2..4 {
        let server = HlsTestServer::new(HlsTestServerConfig {
            segments_per_variant: Consts::SEGMENT_COUNT,
            segment_size: Consts::D.segment_size,
            segment_duration_secs: segment_duration,
            custom_data: Some(Arc::clone(&wav_data)),
            ..Default::default()
        })
        .await;

        let url = server.url("/master.m3u8");
        let temp = TestTempDir::new();
        let cancel = CancellationToken::new();

        let hls_config = HlsConfig::new(url)
            .with_store(StoreOptions::new(temp.path()))
            .with_cancel(cancel)
            .with_abr_options(AbrOptions {
                mode: AbrMode::Manual(0),
                ..AbrOptions::default()
            });

        let wav_info = MediaInfo::new(Some(AudioCodec::Pcm), Some(ContainerFormat::Wav));
        let config = AudioConfig::<Hls>::new(hls_config).with_media_info(wav_info);

        let mut audio = Audio::<Stream<Hls>>::new(config)
            .await
            .expect("create HLS audio");

        temps.push(temp);
        servers.push(server);
        handles.push(spawn_blocking(move || {
            let total = read_hls_for_concurrency_check(&mut audio);
            info!(instance = i, kind = "hls", total_samples = total, "done");
            InstanceResult {
                id: i,
                kind: "hls",
                total_samples: total,
            }
        }));
    }

    let mut results = Vec::new();
    for h in handles {
        results.push(h.await.expect("join"));
    }
    drop(temps);
    drop(servers);

    info!(?results, "all mixed instances done");
    for r in &results {
        assert!(
            r.total_samples > 0,
            "instance {} ({}) read 0 samples",
            r.id,
            r.kind
        );
    }
}

/// 4 File + 4 HLS instances (8 total) running concurrently.
#[kithara::test(
    tokio,
    browser,
    serial,
    timeout(Duration::from_secs(30)),
    env(KITHARA_HANG_TIMEOUT_SECS = "2")
)]
async fn mixed_four_file_four_hls() {
    let wav_data = generate_wav_data();
    let file_server = TestServerHelper::new().await;

    let segment_duration = Consts::D.segment_size as f64
        / (f64::from(Consts::D.sample_rate) * f64::from(Consts::D.channels) * 2.0);

    let mut handles: Vec<JoinHandle<InstanceResult>> = Vec::new();
    let mut temps = Vec::new();
    let mut servers = Vec::new();

    // Spawn 4 File instances
    for i in 0..4 {
        let url = file_server.asset("test.mp3");
        let temp = TestTempDir::new();

        let file_config = FileConfig::new(url.into()).with_store(StoreOptions::new(temp.path()));
        let config = AudioConfig::<File>::new(file_config).with_hint("mp3");

        let mut audio = Audio::<Stream<File>>::new(config)
            .await
            .expect("create File audio");

        temps.push(temp);
        handles.push(spawn_blocking(move || {
            let total = read_file_for_concurrency_check(&mut audio);
            info!(instance = i, kind = "file", total_samples = total, "done");
            InstanceResult {
                id: i,
                kind: "file",
                total_samples: total,
            }
        }));
    }

    // Spawn 4 HLS instances
    for i in 4..8 {
        let server = HlsTestServer::new(HlsTestServerConfig {
            segments_per_variant: Consts::SEGMENT_COUNT,
            segment_size: Consts::D.segment_size,
            segment_duration_secs: segment_duration,
            custom_data: Some(Arc::clone(&wav_data)),
            ..Default::default()
        })
        .await;

        let url = server.url("/master.m3u8");
        let temp = TestTempDir::new();
        let cancel = CancellationToken::new();

        let hls_config = HlsConfig::new(url)
            .with_store(StoreOptions::new(temp.path()))
            .with_cancel(cancel)
            .with_abr_options(AbrOptions {
                mode: AbrMode::Manual(0),
                ..AbrOptions::default()
            });

        let wav_info = MediaInfo::new(Some(AudioCodec::Pcm), Some(ContainerFormat::Wav));
        let config = AudioConfig::<Hls>::new(hls_config).with_media_info(wav_info);

        let mut audio = Audio::<Stream<Hls>>::new(config)
            .await
            .expect("create HLS audio");

        temps.push(temp);
        servers.push(server);
        handles.push(spawn_blocking(move || {
            let total = read_hls_for_concurrency_check(&mut audio);
            info!(instance = i, kind = "hls", total_samples = total, "done");
            InstanceResult {
                id: i,
                kind: "hls",
                total_samples: total,
            }
        }));
    }

    let mut results = Vec::new();
    for h in handles {
        results.push(h.await.expect("join"));
    }
    drop(temps);
    drop(servers);

    info!(?results, "all mixed instances done");
    for r in &results {
        assert!(
            r.total_samples > 0,
            "instance {} ({}) read 0 samples",
            r.id,
            r.kind
        );
    }
}
