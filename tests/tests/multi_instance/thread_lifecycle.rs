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

use kithara::{
    assets::StoreOptions,
    audio::{Audio, AudioConfig},
    file::{File, FileConfig},
    hls::{AbrMode, AbrOptions, Hls, HlsConfig},
    platform::ThreadPool,
    stream::{AudioCodec, ContainerFormat, MediaInfo, Stream},
};
use kithara_test_utils::{TestTempDir, wav::create_test_wav};
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
const POOL_AVAILABILITY_PROBE_HOLD: Duration = Duration::from_millis(120);
const POOL_AVAILABILITY_RETRY: Duration = Duration::from_millis(100);
const POOL_AVAILABILITY_TIMEOUT: Duration = Duration::from_secs(5);

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
async fn pool_max_concurrency(pool: &ThreadPool, expected_threads: usize) -> usize {
    let running = Arc::new(AtomicUsize::new(0));
    let max_concurrent = Arc::new(AtomicUsize::new(0));

    let mut handles = Vec::with_capacity(expected_threads);
    for _ in 0..expected_threads {
        let running = Arc::clone(&running);
        let max_concurrent = Arc::clone(&max_concurrent);

        let (tx, rx) = tokio::sync::oneshot::channel::<usize>();
        pool.spawn(move || {
            let cur = running.fetch_add(1, Ordering::SeqCst) + 1;
            max_concurrent.fetch_max(cur, Ordering::SeqCst);

            kithara_platform::thread::sleep(POOL_AVAILABILITY_PROBE_HOLD);

            running.fetch_sub(1, Ordering::SeqCst);
            let _ = tx.send(cur);
        });
        handles.push(rx);
    }

    for rx in handles {
        let _ = rx.await;
    }

    max_concurrent.load(Ordering::SeqCst)
}

async fn assert_pool_threads_available(pool: &ThreadPool, expected_threads: usize) {
    let deadline = tokio::time::Instant::now() + POOL_AVAILABILITY_TIMEOUT;
    let mut best_observed = 0usize;

    loop {
        let observed_max = pool_max_concurrency(pool, expected_threads).await;
        best_observed = best_observed.max(observed_max);

        if observed_max >= expected_threads {
            return;
        }

        if tokio::time::Instant::now() >= deadline {
            assert!(
                best_observed >= expected_threads,
                "only {best_observed}/{expected_threads} threads were free concurrently \
                 within {:?} — some threads are still occupied by a previous instance",
                POOL_AVAILABILITY_TIMEOUT
            );
        }

        tokio::time::sleep(POOL_AVAILABILITY_RETRY).await;
    }
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
#[kithara::test(tokio, timeout(Duration::from_secs(30)))]
async fn file_threads_released_after_drop() {
    let _ = tracing_subscriber::fmt()
        .with_test_writer()
        .with_max_level(tracing::Level::INFO)
        .try_init();

    let pool = ThreadPool::with_num_threads(4).expect("thread pool");
    let server = AudioTestServer::new().await;
    let temp = TestTempDir::new();

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

    kithara_platform::spawn_blocking(move || {
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
#[kithara::test(tokio, timeout(Duration::from_secs(30)))]
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
    let temp = TestTempDir::new();
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

    kithara_platform::spawn_blocking(move || {
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
#[kithara::test(tokio, timeout(Duration::from_secs(60)))]
async fn sequential_file_create_destroy_no_leak() {
    let _ = tracing_subscriber::fmt()
        .with_test_writer()
        .with_max_level(tracing::Level::INFO)
        .try_init();

    let pool = ThreadPool::with_num_threads(4).expect("thread pool");
    let server = AudioTestServer::new().await;

    for round in 0..10 {
        let temp = TestTempDir::new();
        let file_config = FileConfig::new(server.mp3_url().into())
            .with_store(StoreOptions::new(temp.path()))
            .with_thread_pool(pool.clone());
        let config = AudioConfig::<File>::new(file_config)
            .with_hint("mp3")
            .with_thread_pool(pool.clone());
        let audio = Audio::<Stream<File>>::new(config)
            .await
            .expect("create audio");

        kithara_platform::spawn_blocking(move || {
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
#[kithara::test(tokio, timeout(Duration::from_secs(60)))]
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
        let temp = TestTempDir::new();
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

        kithara_platform::spawn_blocking(move || {
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
#[kithara::test(tokio, timeout(Duration::from_secs(120)))]
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

    let temp1 = TestTempDir::new();
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
    let temp2 = TestTempDir::new();
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
    let h1 = kithara_platform::spawn_blocking(move || {
        let _temp = temp1;
        read_partial_and_drop_file(audio_file, 10_000);
    });
    let h2 = kithara_platform::spawn_blocking(move || {
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

    let temp3 = TestTempDir::new();
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
    let total = kithara_platform::spawn_blocking(move || {
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

/// Probe helper should tolerate delayed thread release and not fail
/// on a single transient snapshot.
#[kithara::test(tokio, timeout(Duration::from_secs(20)))]
async fn pool_availability_waits_for_delayed_release() {
    let pool = ThreadPool::with_num_threads(4).expect("thread pool");
    let (ready_tx, ready_rx) = std::sync::mpsc::channel();

    for _ in 0..2 {
        let tx = ready_tx.clone();
        pool.spawn(move || {
            let _ = tx.send(());
            kithara_platform::thread::sleep(Duration::from_millis(700));
        });
    }
    drop(ready_tx);

    for _ in 0..2 {
        ready_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("busy task should start");
    }

    let started = tokio::time::Instant::now();
    assert_pool_threads_available(&pool, 4).await;
    assert!(
        started.elapsed() >= Duration::from_millis(400),
        "availability check should wait for delayed release"
    );
}
