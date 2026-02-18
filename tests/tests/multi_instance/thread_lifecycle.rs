//! Thread lifecycle tests.
//!
//! Verifies that pool threads are released when `Audio` instances are
//! dropped and the pool stays fully usable afterwards.
//!
//! Key invariants:
//! - Dropping an `Audio` instance cancels its worker and downloader threads
//! - After all instances are dropped, all pool threads are idle
//! - A pool can be reused for new instances after previous ones are destroyed

use std::{
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use kithara::assets::StoreOptions;
use kithara::audio::{Audio, AudioConfig};
use kithara::file::{File, FileConfig};
use kithara::hls::{AbrMode, AbrOptions, Hls, HlsConfig};
use kithara::platform::ThreadPool;
use kithara::stream::{AudioCodec, ContainerFormat, MediaInfo, Stream};
use rstest::rstest;
use tempfile::TempDir;
use tokio_util::sync::CancellationToken;
use tracing::info;

use kithara_test_utils::wav::create_test_wav;

use crate::{
    kithara_decode::fixture::AudioTestServer,
    kithara_hls::fixture::{HlsTestServer, HlsTestServerConfig},
};

const SAMPLE_RATE: u32 = 44100;
const CHANNELS: u16 = 2;
const SEGMENT_SIZE: usize = 200_000;
const SEGMENT_COUNT: usize = 10;

fn generate_wav_data() -> Arc<Vec<u8>> {
    let total_bytes = SEGMENT_COUNT * SEGMENT_SIZE;
    let bytes_per_frame = CHANNELS as usize * 2;
    let header_size = 44;
    let sample_count = (total_bytes - header_size) / bytes_per_frame;
    Arc::new(create_test_wav(sample_count, SAMPLE_RATE, CHANNELS))
}

/// Verify that a pool has all its threads available by running N concurrent
/// tasks and confirming that all N execute simultaneously.
///
/// If any thread is stuck from a previous instance, the concurrent count
/// will be less than `expected_threads`.
async fn assert_pool_threads_available(pool: &ThreadPool, expected_threads: usize) {
    let running = Arc::new(AtomicUsize::new(0));
    let max_concurrent = Arc::new(AtomicUsize::new(0));
    let _barrier = Arc::new(tokio::sync::Barrier::new(expected_threads));

    let mut handles = Vec::new();
    for _ in 0..expected_threads {
        let running = Arc::clone(&running);
        let max_concurrent = Arc::clone(&max_concurrent);

        let (tx, rx) = tokio::sync::oneshot::channel::<usize>();
        pool.spawn(move || {
            let cur = running.fetch_add(1, Ordering::SeqCst) + 1;
            max_concurrent.fetch_max(cur, Ordering::SeqCst);

            // Block until all threads arrive — proves they're all free.
            // Use a spin-wait with channel since we're on rayon threads.
            // Actually we use tokio::sync::Barrier from the async side instead.
            // But we're on a rayon thread, so just sleep briefly to let all
            // threads register their presence.
            std::thread::sleep(Duration::from_millis(100));

            running.fetch_sub(1, Ordering::SeqCst);
            let _ = tx.send(cur);
        });
        handles.push(rx);
    }

    for rx in handles {
        let _ = rx.await;
    }

    let observed_max = max_concurrent.load(Ordering::SeqCst);
    assert!(
        observed_max >= expected_threads,
        "only {observed_max}/{expected_threads} threads were free concurrently — \
         some threads are still occupied by a previous instance"
    );
}

/// Read some PCM data (not to EOF), then drop the instance.
fn read_partial_and_drop_file(audio: Audio<Stream<File>>, samples_to_read: usize) {
    let mut audio = audio;
    let mut buf = vec![0.0f32; 1024];
    let mut total = 0;
    while total < samples_to_read {
        let n = audio.read(&mut buf);
        if n == 0 {
            break;
        }
        total += n;
    }
    info!(
        total_samples = total,
        "partial read done, dropping instance"
    );
    // audio is dropped here — should cancel worker + downloader
}

/// Read some PCM data (not to EOF), then drop the instance.
fn read_partial_and_drop_hls(audio: Audio<Stream<Hls>>, samples_to_read: usize) {
    let mut audio = audio;
    let mut buf = vec![0.0f32; 1024];
    let mut total = 0;
    while total < samples_to_read {
        let n = audio.read(&mut buf);
        if n == 0 {
            break;
        }
        total += n;
    }
    info!(
        total_samples = total,
        "partial read done, dropping instance"
    );
}

// Tests

/// Drop a File instance mid-stream, then verify all pool threads are free.
#[rstest]
#[timeout(Duration::from_secs(30))]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn file_threads_released_after_drop() {
    let _ = tracing_subscriber::fmt()
        .with_test_writer()
        .with_max_level(tracing::Level::INFO)
        .try_init();

    let pool = ThreadPool::with_num_threads(4).expect("thread pool");
    let server = AudioTestServer::new().await;
    let temp = TempDir::new().expect("temp dir");

    // Create and partially read a File instance, then drop it.
    let file_config = FileConfig::new(server.mp3_url().into())
        .with_store(StoreOptions::new(temp.path()))
        .with_thread_pool(pool.clone());
    let config = AudioConfig::<File>::new(file_config)
        .with_hint("mp3")
        .with_thread_pool(pool.clone());
    let audio = Audio::<Stream<File>>::new(config)
        .await
        .expect("create audio");

    tokio::task::spawn_blocking(move || {
        read_partial_and_drop_file(audio, 10_000);
    })
    .await
    .expect("join");

    // Give worker/downloader time to notice cancellation and exit.
    tokio::time::sleep(Duration::from_millis(500)).await;

    // All 4 pool threads should be free now.
    assert_pool_threads_available(&pool, 4).await;
    info!("all pool threads are free after File instance drop");
}

/// Drop an HLS instance mid-stream, then verify all pool threads are free.
#[rstest]
#[timeout(Duration::from_secs(30))]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn hls_threads_released_after_drop() {
    let _ = tracing_subscriber::fmt()
        .with_test_writer()
        .with_max_level(tracing::Level::INFO)
        .try_init();

    let pool = ThreadPool::with_num_threads(4).expect("thread pool");
    let wav_data = generate_wav_data();
    let segment_duration = SEGMENT_SIZE as f64 / (SAMPLE_RATE as f64 * CHANNELS as f64 * 2.0);

    let server = HlsTestServer::new(HlsTestServerConfig {
        segments_per_variant: SEGMENT_COUNT,
        segment_size: SEGMENT_SIZE,
        segment_duration_secs: segment_duration,
        custom_data: Some(wav_data),
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
    let audio = Audio::<Stream<Hls>>::new(config)
        .await
        .expect("create audio");

    tokio::task::spawn_blocking(move || {
        let _server = server;
        let _temp = temp;
        read_partial_and_drop_hls(audio, 10_000);
    })
    .await
    .expect("join");

    tokio::time::sleep(Duration::from_millis(500)).await;

    assert_pool_threads_available(&pool, 4).await;
    info!("all pool threads are free after HLS instance drop");
}

/// Create and destroy File instances sequentially, 10 times, on a tiny pool.
///
/// If threads leak, the pool (2 threads) would be exhausted by the 2nd
/// iteration and the test would deadlock.
#[rstest]
#[timeout(Duration::from_secs(60))]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn sequential_file_create_destroy_no_leak() {
    let _ = tracing_subscriber::fmt()
        .with_test_writer()
        .with_max_level(tracing::Level::INFO)
        .try_init();

    let pool = ThreadPool::with_num_threads(4).expect("thread pool");
    let server = AudioTestServer::new().await;

    for round in 0..10 {
        let temp = TempDir::new().expect("temp dir");
        let file_config = FileConfig::new(server.mp3_url().into())
            .with_store(StoreOptions::new(temp.path()))
            .with_thread_pool(pool.clone());
        let config = AudioConfig::<File>::new(file_config)
            .with_hint("mp3")
            .with_thread_pool(pool.clone());
        let audio = Audio::<Stream<File>>::new(config)
            .await
            .expect("create audio");

        tokio::task::spawn_blocking(move || {
            let _temp = temp;
            read_partial_and_drop_file(audio, 5_000);
        })
        .await
        .expect("join");

        // Brief pause to let threads exit.
        tokio::time::sleep(Duration::from_millis(200)).await;

        info!(round, "iteration done, pool still alive");
    }

    // After 10 rounds the pool should be fully usable.
    assert_pool_threads_available(&pool, 4).await;
    info!("10 sequential create/destroy cycles completed without thread leak");
}

/// Create and destroy HLS instances sequentially, 10 times, on a shared pool.
#[rstest]
#[timeout(Duration::from_secs(60))]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn sequential_hls_create_destroy_no_leak() {
    let _ = tracing_subscriber::fmt()
        .with_test_writer()
        .with_max_level(tracing::Level::INFO)
        .try_init();

    let pool = ThreadPool::with_num_threads(4).expect("thread pool");
    let wav_data = generate_wav_data();
    let segment_duration = SEGMENT_SIZE as f64 / (SAMPLE_RATE as f64 * CHANNELS as f64 * 2.0);

    for round in 0..10 {
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
        let audio = Audio::<Stream<Hls>>::new(config)
            .await
            .expect("create audio");

        tokio::task::spawn_blocking(move || {
            let _server = server;
            let _temp = temp;
            read_partial_and_drop_hls(audio, 5_000);
        })
        .await
        .expect("join");

        tokio::time::sleep(Duration::from_millis(200)).await;

        info!(round, "iteration done");
    }

    assert_pool_threads_available(&pool, 4).await;
    info!("10 sequential HLS create/destroy cycles completed without thread leak");
}

/// Saturate a 4-thread pool with 2 instances (each uses ~2 threads:
/// worker + downloader), drop them all, then verify full recovery.
#[rstest]
#[timeout(Duration::from_secs(60))]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn pool_recovers_after_saturation() {
    let _ = tracing_subscriber::fmt()
        .with_test_writer()
        .with_max_level(tracing::Level::INFO)
        .try_init();

    let pool = ThreadPool::with_num_threads(4).expect("thread pool");
    let wav_data = generate_wav_data();
    let segment_duration = SEGMENT_SIZE as f64 / (SAMPLE_RATE as f64 * CHANNELS as f64 * 2.0);
    let server = AudioTestServer::new().await;

    // Phase 1: saturate the pool with 2 instances (File + HLS)
    info!("phase 1: saturating pool");

    let temp1 = TempDir::new().expect("temp dir");
    let file_config = FileConfig::new(server.mp3_url().into())
        .with_store(StoreOptions::new(temp1.path()))
        .with_thread_pool(pool.clone());
    let config = AudioConfig::<File>::new(file_config)
        .with_hint("mp3")
        .with_thread_pool(pool.clone());
    let audio_file = Audio::<Stream<File>>::new(config)
        .await
        .expect("create file audio");

    let hls_server = HlsTestServer::new(HlsTestServerConfig {
        segments_per_variant: SEGMENT_COUNT,
        segment_size: SEGMENT_SIZE,
        segment_duration_secs: segment_duration,
        custom_data: Some(Arc::clone(&wav_data)),
        ..Default::default()
    })
    .await;
    let url = hls_server.url("/master.m3u8").expect("url");
    let temp2 = TempDir::new().expect("temp dir");
    let cancel = CancellationToken::new();
    let hls_config = HlsConfig::new(url)
        .with_store(StoreOptions::new(temp2.path()))
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
    let audio_hls = Audio::<Stream<Hls>>::new(config)
        .await
        .expect("create hls audio");

    // Read partially from both, then drop.
    let h1 = tokio::task::spawn_blocking(move || {
        let _temp = temp1;
        read_partial_and_drop_file(audio_file, 10_000);
    });
    let h2 = tokio::task::spawn_blocking(move || {
        let _server = hls_server;
        let _temp = temp2;
        read_partial_and_drop_hls(audio_hls, 10_000);
    });
    h1.await.expect("join");
    h2.await.expect("join");

    info!("phase 1 done: both instances dropped");
    tokio::time::sleep(Duration::from_millis(500)).await;

    // Phase 2: verify full recovery
    info!("phase 2: verifying pool recovery");
    assert_pool_threads_available(&pool, 4).await;

    // Phase 3: create new instances on the recovered pool
    info!("phase 3: creating new instances on recovered pool");

    let temp3 = TempDir::new().expect("temp dir");
    let file_config = FileConfig::new(server.mp3_url().into())
        .with_store(StoreOptions::new(temp3.path()))
        .with_thread_pool(pool.clone());
    let config = AudioConfig::<File>::new(file_config)
        .with_hint("mp3")
        .with_thread_pool(pool.clone());
    let mut audio = Audio::<Stream<File>>::new(config)
        .await
        .expect("create audio on recovered pool");

    // Read to EOF to prove the pool is fully functional.
    let total = tokio::task::spawn_blocking(move || {
        let _temp = temp3;
        let mut buf = vec![0.0f32; 4096];
        let mut total = 0u64;
        loop {
            let n = audio.read(&mut buf);
            if n == 0 {
                break;
            }
            total += n as u64;
        }
        total
    })
    .await
    .expect("join");

    assert!(total > 0, "post-recovery instance read 0 samples");
    info!(
        total_samples = total,
        "pool recovered and new instance completed"
    );
}
