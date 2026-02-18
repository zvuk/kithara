//! Mixed concurrent File + HLS instance tests.
//!
//! Verifies that File and HLS `Audio` instances can coexist on the
//! same shared `ThreadPool` and all read PCM data to EOF.

use std::{sync::Arc, time::Duration};

use kithara::{
    assets::StoreOptions,
    audio::{Audio, AudioConfig},
    file::{File, FileConfig},
    hls::{AbrMode, AbrOptions, Hls, HlsConfig},
    platform::ThreadPool,
    stream::{AudioCodec, ContainerFormat, MediaInfo, Stream},
};
use kithara_test_utils::wav::create_test_wav;
use rstest::rstest;
use tempfile::TempDir;
use tokio_util::sync::CancellationToken;
use tracing::info;

use crate::{
    kithara_decode::fixture::AudioTestServer,
    kithara_hls::fixture::{HlsTestServer, HlsTestServerConfig},
};

const SAMPLE_RATE: u32 = 44100;
const CHANNELS: u16 = 2;
const SEGMENT_SIZE: usize = 200_000;
const SEGMENT_COUNT: usize = 10;

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

fn generate_wav_data() -> Arc<Vec<u8>> {
    let total_bytes = SEGMENT_COUNT * SEGMENT_SIZE;
    let bytes_per_frame = CHANNELS as usize * 2;
    let header_size = 44;
    let sample_count = (total_bytes - header_size) / bytes_per_frame;
    Arc::new(create_test_wav(sample_count, SAMPLE_RATE, CHANNELS))
}

/// 2 File + 2 HLS instances on a shared pool.
#[rstest]
#[timeout(Duration::from_secs(120))]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn mixed_two_file_two_hls() {
    let _ = tracing_subscriber::fmt()
        .with_test_writer()
        .with_max_level(tracing::Level::INFO)
        .try_init();

    // Each Audio instance uses 2 pool threads (downloader + audio_loop).
    let pool = ThreadPool::with_num_threads(10).expect("thread pool");
    let wav_data = generate_wav_data();
    let file_server = AudioTestServer::new().await;

    let segment_duration = SEGMENT_SIZE as f64 / (SAMPLE_RATE as f64 * CHANNELS as f64 * 2.0);

    let mut handles: Vec<tokio::task::JoinHandle<InstanceResult>> = Vec::new();
    let mut temps = Vec::new();
    let mut servers = Vec::new();

    // Spawn 2 File instances
    for i in 0..2 {
        let url = file_server.mp3_url();
        let temp = TempDir::new().expect("temp dir");
        let pool = pool.clone();

        let file_config = FileConfig::new(url.into())
            .with_store(StoreOptions::new(temp.path()))
            .with_thread_pool(pool.clone());
        let config = AudioConfig::<File>::new(file_config)
            .with_hint("mp3")
            .with_thread_pool(pool);

        let mut audio = Audio::<Stream<File>>::new(config)
            .await
            .expect("create File audio");

        temps.push(temp);
        handles.push(tokio::task::spawn_blocking(move || {
            let total = read_file_to_eof(&mut audio);
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
            segments_per_variant: SEGMENT_COUNT,
            segment_size: SEGMENT_SIZE,
            segment_duration_secs: segment_duration,
            custom_data: Some(Arc::clone(&wav_data)),
            ..Default::default()
        })
        .await;

        let url = server.url("/master.m3u8").expect("url");
        let temp = TempDir::new().expect("temp dir");
        let cancel = CancellationToken::new();

        let hls_config = HlsConfig::new(url)
            .with_store(StoreOptions::new(temp.path()))
            .with_cancel(cancel)
            .with_thread_pool(pool.clone())
            .with_abr(AbrOptions {
                mode: AbrMode::Manual(0),
                ..AbrOptions::default()
            });

        let wav_info = MediaInfo::new(Some(AudioCodec::Pcm), Some(ContainerFormat::Wav));
        let config = AudioConfig::<Hls>::new(hls_config)
            .with_media_info(wav_info)
            .with_thread_pool(pool.clone());

        let mut audio = Audio::<Stream<Hls>>::new(config)
            .await
            .expect("create HLS audio");

        temps.push(temp);
        servers.push(server);
        handles.push(tokio::task::spawn_blocking(move || {
            let total = read_hls_to_eof(&mut audio);
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

/// 4 File + 4 HLS instances (8 total) on a shared pool.
#[rstest]
#[timeout(Duration::from_secs(180))]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn mixed_four_file_four_hls() {
    let _ = tracing_subscriber::fmt()
        .with_test_writer()
        .with_max_level(tracing::Level::INFO)
        .try_init();

    // Each Audio instance uses 2 pool threads (downloader + audio_loop).
    let pool = ThreadPool::with_num_threads(18).expect("thread pool");
    let wav_data = generate_wav_data();
    let file_server = AudioTestServer::new().await;

    let segment_duration = SEGMENT_SIZE as f64 / (SAMPLE_RATE as f64 * CHANNELS as f64 * 2.0);

    let mut handles: Vec<tokio::task::JoinHandle<InstanceResult>> = Vec::new();
    let mut temps = Vec::new();
    let mut servers = Vec::new();

    // Spawn 4 File instances
    for i in 0..4 {
        let url = file_server.mp3_url();
        let temp = TempDir::new().expect("temp dir");
        let pool = pool.clone();

        let file_config = FileConfig::new(url.into())
            .with_store(StoreOptions::new(temp.path()))
            .with_thread_pool(pool.clone());
        let config = AudioConfig::<File>::new(file_config)
            .with_hint("mp3")
            .with_thread_pool(pool);

        let mut audio = Audio::<Stream<File>>::new(config)
            .await
            .expect("create File audio");

        temps.push(temp);
        handles.push(tokio::task::spawn_blocking(move || {
            let total = read_file_to_eof(&mut audio);
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
            segments_per_variant: SEGMENT_COUNT,
            segment_size: SEGMENT_SIZE,
            segment_duration_secs: segment_duration,
            custom_data: Some(Arc::clone(&wav_data)),
            ..Default::default()
        })
        .await;

        let url = server.url("/master.m3u8").expect("url");
        let temp = TempDir::new().expect("temp dir");
        let cancel = CancellationToken::new();

        let hls_config = HlsConfig::new(url)
            .with_store(StoreOptions::new(temp.path()))
            .with_cancel(cancel)
            .with_thread_pool(pool.clone())
            .with_abr(AbrOptions {
                mode: AbrMode::Manual(0),
                ..AbrOptions::default()
            });

        let wav_info = MediaInfo::new(Some(AudioCodec::Pcm), Some(ContainerFormat::Wav));
        let config = AudioConfig::<Hls>::new(hls_config)
            .with_media_info(wav_info)
            .with_thread_pool(pool.clone());

        let mut audio = Audio::<Stream<Hls>>::new(config)
            .await
            .expect("create HLS audio");

        temps.push(temp);
        servers.push(server);
        handles.push(tokio::task::spawn_blocking(move || {
            let total = read_hls_to_eof(&mut audio);
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
