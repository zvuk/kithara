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

#![cfg(not(target_arch = "wasm32"))]

use std::time::Duration;

use kithara_assets::StoreOptions;
use kithara_audio::{Audio, AudioConfig, AudioWorkerHandle};
use kithara_hls::{AbrMode, AbrOptions, Hls, HlsConfig};
use kithara_stream::Stream;
use kithara_test_utils::{TestTempDir, kithara, serve_assets, temp_dir};
use tokio_util::sync::CancellationToken;
use tracing::info;

/// Count OS threads for the current process via `ps -M`.
fn thread_count() -> usize {
    let output = std::process::Command::new("ps")
        .args(["-M", "-p", &std::process::id().to_string()])
        .output()
        .expect("ps -M failed");
    let stdout = String::from_utf8_lossy(&output.stdout);
    stdout.lines().count().saturating_sub(1)
}

/// Wait for spawned threads to register with the OS.
fn settle() {
    std::thread::sleep(Duration::from_millis(1500));
}

// ---------------------------------------------------------------------------
// AudioWorkerHandle — exactly 1 thread
// ---------------------------------------------------------------------------

#[kithara::test]
fn thread_budget_audio_worker_is_one_thread() {
    let before = thread_count();
    let worker = AudioWorkerHandle::new();
    settle();
    let after = thread_count();
    worker.shutdown();

    let delta = after.saturating_sub(before);
    assert_eq!(delta, 1, "AudioWorkerHandle must spawn exactly 1 thread");
}

// ---------------------------------------------------------------------------
// Single HLS pipeline: target = 1 thread (shared worker only)
//
// Downloader must run as an async task on the caller's runtime,
// not spawn a dedicated OS thread + tokio runtime.
// ---------------------------------------------------------------------------

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

    let before = thread_count();

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

    let after = thread_count();
    let delta = after.saturating_sub(before);
    info!(before, after, delta, "single HLS pipeline");

    // Budget breakdown:
    //   1 audio worker (standalone, no shared worker in this test)
    //   1 downloader helper thread (block_on shared runtime, no extra runtime)
    //   2-4 transient spawn_blocking threads (probe + decoder, tokio pool)
    // Total: ≤7. Previous baseline was ~14 (with per-stream runtimes).
    assert!(
        delta <= 10,
        "Single pipeline budget: ≤10 threads, got delta={delta} \
         (before={before}, after={after})"
    );

    cancel.cancel();
}

// ---------------------------------------------------------------------------
// 3 tracks with shared worker: target = 1 thread total
//
// 1 shared worker serves all tracks. Downloaders are async tasks.
// ---------------------------------------------------------------------------

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

    let before = thread_count();

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

    let after = thread_count();
    let delta = after.saturating_sub(before);
    info!(
        before,
        after,
        delta,
        tracks = audios.len(),
        "3 tracks shared worker"
    );

    // Budget breakdown:
    //   1 shared audio worker
    //   3 downloader helper threads (block_on shared runtime, no extra runtimes)
    //   2-6 transient spawn_blocking threads (probe + decoder per track)
    // Total: ≤16. Previous baseline was ~33 (with per-stream runtimes + per-track workers).
    assert!(
        delta <= 16,
        "3 tracks with shared worker budget: ≤16 threads, got delta={delta} \
         (before={before}, after={after})"
    );

    cancel.cancel();
    shared_worker.shutdown();
    drop(audios);
}

// ---------------------------------------------------------------------------
// Process ceiling
// ---------------------------------------------------------------------------

#[kithara::test]
fn thread_budget_process_ceiling() {
    let count = thread_count();
    assert!(
        count <= 15,
        "Process has {count} threads with no active pipelines — \
         investigate leaked runtimes or eager thread creation."
    );
}
