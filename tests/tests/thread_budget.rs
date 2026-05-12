#![cfg(not(target_arch = "wasm32"))]

use std::time::{Duration, Instant};

use kithara_assets::StoreOptions;
use kithara_audio::{Audio, AudioConfig, AudioWorkerHandle};
use kithara_hls::{AbrMode, Hls, HlsConfig};
use kithara_platform::thread::active_named_thread_count;
use kithara_stream::Stream;
use kithara_test_utils::{TestServerHelper, TestTempDir, kithara, temp_dir};
use tokio_util::sync::CancellationToken;
use tracing::info;

/// Wait for spawned threads to register with the OS.
fn settle() {
    std::thread::sleep(Duration::from_millis(1500));
}

fn wait_for_named_threads(target: usize, timeout: Duration) -> usize {
    let deadline = Instant::now() + timeout;

    loop {
        let last_count = active_named_thread_count();
        if last_count == target {
            std::thread::sleep(Duration::from_millis(200));
            let stable_count = active_named_thread_count();
            if stable_count == target {
                return stable_count;
            }
        }

        if Instant::now() >= deadline {
            return last_count;
        }

        std::thread::sleep(Duration::from_millis(100));
    }
}

#[kithara::test(serial)]
fn thread_budget_audio_worker_is_one_thread() {
    let before = active_named_thread_count();
    let worker = AudioWorkerHandle::new();
    settle();
    let after = active_named_thread_count();

    let delta = after.saturating_sub(before);
    worker.shutdown();
    settle();
    assert_eq!(
        delta, 1,
        "AudioWorkerHandle must spawn exactly 1 thread (got delta={delta}, before={before}, after={after})"
    );
}

#[kithara::test(
    tokio,
    multi_thread,
    native,
    serial,
    timeout(Duration::from_secs(15)),
    env(KITHARA_HANG_TIMEOUT_SECS = "3")
)]
async fn thread_budget_single_hls_pipeline(temp_dir: TestTempDir) {
    let server = TestServerHelper::new().await;
    let cancel = CancellationToken::new();

    let before = active_named_thread_count();

    let hls_config = HlsConfig::new(server.asset("hls/master.m3u8"))
        .with_store(StoreOptions::new(temp_dir.path()))
        .with_cancel(cancel.clone())
        .with_initial_abr_mode(AbrMode::Manual(0));
    let config = AudioConfig::<Hls>::new(hls_config);
    let mut audio = Audio::<Stream<Hls>>::new(config)
        .await
        .expect("create hls audio");
    audio.preload().expect("preload must succeed");
    settle();

    let after = active_named_thread_count();
    let delta = after.saturating_sub(before);
    info!(before, after, delta, "single HLS pipeline");

    drop(audio);
    cancel.cancel();
    settle();

    assert!(
        delta <= 2,
        "Single pipeline budget: ≤2 kithara threads, got delta={delta} \
         (before={before}, after={after})"
    );
}

#[kithara::test(
    tokio,
    multi_thread,
    native,
    serial,
    timeout(Duration::from_secs(20)),
    env(KITHARA_HANG_TIMEOUT_SECS = "3")
)]
async fn thread_budget_three_tracks_shared_worker(temp_dir: TestTempDir) {
    let server = TestServerHelper::new().await;
    let cancel = CancellationToken::new();
    let shared_worker = AudioWorkerHandle::new();

    settle();
    let before = active_named_thread_count();

    let hls_config = HlsConfig::new(server.asset("hls/master.m3u8"))
        .with_store(StoreOptions::new(temp_dir.path()))
        .with_cancel(cancel.clone())
        .with_initial_abr_mode(AbrMode::Manual(0));
    let config: AudioConfig<Hls> = AudioConfig::new(hls_config).with_worker(shared_worker.clone());
    let a1 = Audio::<Stream<Hls>>::new(config).await;

    let hls_config2 = HlsConfig::new(server.asset("hls/master.m3u8"))
        .with_store(StoreOptions::new(temp_dir.path()))
        .with_cancel(cancel.clone())
        .with_initial_abr_mode(AbrMode::Manual(1));
    let config: AudioConfig<Hls> = AudioConfig::new(hls_config2).with_worker(shared_worker.clone());
    let a2 = Audio::<Stream<Hls>>::new(config).await;

    let drm_config = HlsConfig::new(server.asset("drm/master.m3u8"))
        .with_store(StoreOptions::new(temp_dir.path()))
        .with_cancel(cancel.clone())
        .with_initial_abr_mode(AbrMode::Manual(0));
    let config: AudioConfig<Hls> = AudioConfig::new(drm_config).with_worker(shared_worker.clone());
    let a3 = Audio::<Stream<Hls>>::new(config).await;

    let mut audios: Vec<Box<dyn std::any::Any>> = Vec::new();
    if let Ok(mut a) = a1 {
        a.preload().expect("preload must succeed");
        audios.push(Box::new(a));
    }
    if let Ok(mut a) = a2 {
        a.preload().expect("preload must succeed");
        audios.push(Box::new(a));
    }
    if let Ok(mut a) = a3 {
        a.preload().expect("preload must succeed");
        audios.push(Box::new(a));
    }
    settle();

    let after = active_named_thread_count();
    let delta = after.saturating_sub(before);
    info!(
        before,
        after,
        delta,
        tracks = audios.len(),
        "3 tracks shared worker"
    );

    drop(audios);
    cancel.cancel();
    shared_worker.shutdown();
    settle();

    assert_eq!(
        delta, 0,
        "3 tracks with shared worker: 0 extra kithara threads expected, got delta={delta} \
         (before={before}, after={after})"
    );
}

#[ignore = "requires isolated process-wide quiescence"]
#[kithara::test(serial)]
fn thread_budget_process_ceiling() {
    let count = wait_for_named_threads(0, Duration::from_secs(30));
    assert_eq!(
        count, 0,
        "Process has {count} active kithara threads with no active pipelines — \
         investigate leaked threads."
    );
}
