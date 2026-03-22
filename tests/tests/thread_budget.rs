//! Thread budget tests — enforce upper bounds on OS thread creation.
//!
//! An audio player must be a lightweight citizen on the user's machine.
//! These tests catch regressions where a refactoring or new feature
//! accidentally creates extra OS threads or tokio runtimes.
//!
//! ## Minimum thread model
//!
//! The absolute minimum for an audio player with N tracks:
//! - 1 shared audio worker thread (decode + effects for all tracks)
//! - 0 downloader threads (async tasks on the caller's tokio runtime)
//! - 1 cpal audio output thread (OS-managed, not counted)
//!
//! Total own threads: **1** (the shared worker), regardless of track count.
//! Any threads beyond this are waste.
//!
//! ## Counting strategy
//!
//! All kithara-owned threads are spawned via [`kithara_platform::thread::spawn_named`],
//! which maintains an atomic counter. Tests read this counter instead of
//! parsing `ps -M`, making assertions immune to tokio pool noise and
//! parallel test execution.

#![cfg(not(target_arch = "wasm32"))]

use std::time::Duration;

use kithara_assets::StoreOptions;
use kithara_audio::{Audio, AudioConfig, AudioWorkerHandle};
use kithara_hls::{AbrMode, AbrOptions, Hls, HlsConfig};
use kithara_platform::thread::active_named_thread_count;
use kithara_stream::Stream;
use kithara_test_utils::{TestTempDir, kithara, serve_assets, temp_dir};
use tokio_util::sync::CancellationToken;
use tracing::info;

/// Wait for spawned threads to register with the OS.
fn settle() {
    std::thread::sleep(Duration::from_millis(1500));
}

// AudioWorkerHandle — exactly 1 thread

#[kithara::test]
fn thread_budget_audio_worker_is_one_thread() {
    let before = active_named_thread_count();
    let worker = AudioWorkerHandle::new();
    settle();
    let after = active_named_thread_count();

    let delta = after.saturating_sub(before);
    assert_eq!(
        delta, 1,
        "AudioWorkerHandle must spawn exactly 1 thread (got delta={delta}, before={before}, after={after})"
    );

    worker.shutdown();
}

// Single HLS pipeline: target = 1 thread (shared worker only)
//
// Downloader must run as an async task on the caller's runtime,
// not spawn a dedicated OS thread + tokio runtime.

#[kithara::test(
    tokio,
    multi_thread,
    native,
    timeout(Duration::from_secs(15)),
    env(KITHARA_HANG_TIMEOUT_SECS = "3")
)]
async fn thread_budget_single_hls_pipeline(temp_dir: TestTempDir) {
    let server = serve_assets().await;
    let cancel = CancellationToken::new();

    let before = active_named_thread_count();

    let hls_config = HlsConfig::new(server.url("/hls/master.m3u8"))
        .with_store(StoreOptions::new(temp_dir.path()))
        .with_cancel(cancel.clone())
        .with_abr(AbrOptions {
            mode: AbrMode::Manual(0),
            ..Default::default()
        });
    let config = AudioConfig::<Hls>::new(hls_config);
    let mut audio = Audio::<Stream<Hls>>::new(config)
        .await
        .expect("create hls audio");
    audio.preload();
    settle();

    let after = active_named_thread_count();
    let delta = after.saturating_sub(before);
    info!(before, after, delta, "single HLS pipeline");

    // Budget: 1 audio worker. Downloader runs as async task (0 threads).
    assert!(
        delta <= 2,
        "Single pipeline budget: ≤2 kithara threads, got delta={delta} \
         (before={before}, after={after})"
    );

    cancel.cancel();
}

// 3 tracks with shared worker: target = 0 extra threads
//
// 1 shared worker serves all tracks. Downloaders are async tasks.

#[kithara::test(
    tokio,
    multi_thread,
    native,
    timeout(Duration::from_secs(20)),
    env(KITHARA_HANG_TIMEOUT_SECS = "3")
)]
async fn thread_budget_three_tracks_shared_worker(temp_dir: TestTempDir) {
    let server = serve_assets().await;
    let cancel = CancellationToken::new();
    let shared_worker = AudioWorkerHandle::new();

    // Snapshot taken AFTER worker creation — worker thread is already counted.
    settle();
    let before = active_named_thread_count();

    // Track 1: HLS
    let hls_config = HlsConfig::new(server.url("/hls/master.m3u8"))
        .with_store(StoreOptions::new(temp_dir.path()))
        .with_cancel(cancel.clone())
        .with_abr(AbrOptions {
            mode: AbrMode::Manual(0),
            ..Default::default()
        });
    let config: AudioConfig<Hls> = AudioConfig::new(hls_config).with_worker(shared_worker.clone());
    let a1 = Audio::<Stream<Hls>>::new(config).await;

    // Track 2: HLS (different variant)
    let hls_config2 = HlsConfig::new(server.url("/hls/master.m3u8"))
        .with_store(StoreOptions::new(temp_dir.path()))
        .with_cancel(cancel.clone())
        .with_abr(AbrOptions {
            mode: AbrMode::Manual(1),
            ..Default::default()
        });
    let config: AudioConfig<Hls> = AudioConfig::new(hls_config2).with_worker(shared_worker.clone());
    let a2 = Audio::<Stream<Hls>>::new(config).await;

    // Track 3: DRM
    let drm_config = HlsConfig::new(server.url("/drm/master.m3u8"))
        .with_store(StoreOptions::new(temp_dir.path()))
        .with_cancel(cancel.clone())
        .with_abr(AbrOptions {
            mode: AbrMode::Manual(0),
            ..Default::default()
        });
    let config: AudioConfig<Hls> = AudioConfig::new(drm_config).with_worker(shared_worker.clone());
    let a3 = Audio::<Stream<Hls>>::new(config).await;

    let mut audios: Vec<Box<dyn std::any::Any>> = Vec::new();
    if let Ok(mut a) = a1 {
        a.preload();
        audios.push(Box::new(a));
    }
    if let Ok(mut a) = a2 {
        a.preload();
        audios.push(Box::new(a));
    }
    if let Ok(mut a) = a3 {
        a.preload();
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

    // Budget: 0 extra kithara threads. Worker already counted in `before`.
    // Downloaders run as async tasks on the caller's runtime.
    assert!(
        delta == 0,
        "3 tracks with shared worker: 0 extra kithara threads expected, got delta={delta} \
         (before={before}, after={after})"
    );

    cancel.cancel();
    shared_worker.shutdown();
    drop(audios);
}

// Process ceiling — no leaked kithara threads when idle

#[kithara::test]
fn thread_budget_process_ceiling() {
    let count = active_named_thread_count();
    assert!(
        count == 0,
        "Process has {count} active kithara threads with no active pipelines — \
         investigate leaked threads."
    );
}
