//! Regression test: ABR variant switch during normal playback must not hang.
//!
//! Reproduces the production crash where HLS ABR switch causes
//! `[HangDetector] run_shared_worker_loop no progress for 10s`.
//!
//! Uses real fMP4/AAC assets (same as production app) with disk cache.

use kithara::{
    assets::StoreOptions,
    audio::{Audio, AudioConfig},
    hls::{AbrMode, AbrOptions, Hls, HlsConfig},
    stream::Stream,
};
use kithara_platform::time::{Duration, Instant, sleep};
use kithara_test_utils::{TestTempDir, serve_assets, temp_dir, tracing_setup};
use tokio_util::sync::CancellationToken;
use tracing::info;

/// Real fMP4/AAC HLS stream with ABR auto-switch must play without hanging.
///
/// This is the exact scenario from the production crash:
/// `kithara-app` plays track.mp3 + hls/master.m3u8 + drm/master.m3u8.
/// ABR switches variant on HLS track → worker hangs → all tracks die.
#[kithara::test(
    tokio,
    native,
    serial,
    timeout(Duration::from_secs(30)),
    env(KITHARA_HANG_TIMEOUT_SECS = "3")
)]
async fn abr_switch_real_assets_does_not_hang(_tracing_setup: (), temp_dir: TestTempDir) {
    let server = serve_assets().await;
    let url = server.url("/hls/master.m3u8");

    let cancel = CancellationToken::new();
    let hls_config = HlsConfig::new(url)
        .with_store(StoreOptions::new(temp_dir.path()))
        .with_cancel(cancel)
        .with_abr(AbrOptions {
            mode: AbrMode::Auto(Some(0)),
            ..Default::default()
        });

    let config = AudioConfig::<Hls>::new(hls_config);
    let mut audio = Audio::<Stream<Hls>>::new(config)
        .await
        .expect("create audio");
    audio.preload();

    // Read for 15 seconds. If ABR switch hangs the worker,
    // HangDetector (3s) will panic before the 30s test timeout.
    let deadline = Instant::now() + Duration::from_secs(15);
    let mut buf = vec![0f32; 4096];
    let mut total_samples = 0u64;

    while Instant::now() < deadline {
        let n = audio.read(&mut buf);
        total_samples += n as u64;
        if n == 0 {
            if audio.is_eof() {
                break;
            }
            sleep(Duration::from_millis(10)).await;
        }
    }

    info!(total_samples, "playback completed without hang");
    assert!(
        total_samples > 1000,
        "expected sustained playback, got only {total_samples} samples"
    );
}

/// DRM stream must continue producing chunks after seek.
///
/// Regression for app3.log: DRM track plays to 23.97s with seeks,
/// then stops producing chunks → recv_outcome_blocking hang.
#[kithara::test(
    tokio,
    native,
    serial,
    timeout(Duration::from_secs(30)),
    env(KITHARA_HANG_TIMEOUT_SECS = "5")
)]
async fn drm_stream_continues_after_seek(_tracing_setup: (), temp_dir: TestTempDir) {
    let server = serve_assets().await;
    let url = server.url("/drm/master.m3u8");

    let cancel = CancellationToken::new();
    let hls_config = HlsConfig::new(url)
        .with_store(StoreOptions::new(temp_dir.path()))
        .with_cancel(cancel)
        .with_abr(AbrOptions {
            mode: AbrMode::Auto(Some(0)),
            ..Default::default()
        });

    let config = AudioConfig::<Hls>::new(hls_config);
    let mut audio = Audio::<Stream<Hls>>::new(config)
        .await
        .expect("create audio");
    audio.preload();

    let mut buf = vec![0f32; 4096];

    // Phase 1: Read for 3s (warmup)
    let warmup = Instant::now() + Duration::from_secs(3);
    while Instant::now() < warmup {
        let _ = audio.read(&mut buf);
        sleep(Duration::from_millis(5)).await;
    }

    // Phase 2: Seek to ~7s, ~13s, ~18s, ~24s (like app3.log)
    for &target_secs in &[7.0, 13.0, 18.0, 24.0] {
        audio
            .seek(Duration::from_secs_f64(target_secs))
            .expect("seek");
        let read_deadline = Instant::now() + Duration::from_secs(3);
        let mut samples = 0u64;
        while Instant::now() < read_deadline {
            let n = audio.read(&mut buf);
            samples += n as u64;
            if n == 0 {
                sleep(Duration::from_millis(10)).await;
            }
        }
        assert!(
            samples > 0,
            "seek to {target_secs}s must produce samples, got 0"
        );
    }

    // Phase 3: Continue reading after last seek — must not hang
    let continue_deadline = Instant::now() + Duration::from_secs(5);
    let mut post_seek_samples = 0u64;
    while Instant::now() < continue_deadline {
        let n = audio.read(&mut buf);
        post_seek_samples += n as u64;
        if n == 0 {
            if audio.is_eof() {
                break;
            }
            sleep(Duration::from_millis(10)).await;
        }
    }
    assert!(
        post_seek_samples > 0,
        "playback after seeks must continue, got 0 samples"
    );
}

/// Same test but without ABR (fixed variant 0) — baseline.
#[kithara::test(
    tokio,
    native,
    serial,
    timeout(Duration::from_secs(20)),
    env(KITHARA_HANG_TIMEOUT_SECS = "3")
)]
async fn fixed_variant_real_assets_plays_without_hang(_tracing_setup: (), temp_dir: TestTempDir) {
    let server = serve_assets().await;
    let url = server.url("/hls/master.m3u8");

    let cancel = CancellationToken::new();
    let hls_config = HlsConfig::new(url)
        .with_store(StoreOptions::new(temp_dir.path()))
        .with_cancel(cancel)
        .with_abr(AbrOptions {
            mode: AbrMode::Manual(0),
            ..Default::default()
        });

    let config = AudioConfig::<Hls>::new(hls_config);
    let mut audio = Audio::<Stream<Hls>>::new(config)
        .await
        .expect("create audio");
    audio.preload();

    let deadline = Instant::now() + Duration::from_secs(10);
    let mut buf = vec![0f32; 4096];
    let mut total_samples = 0u64;

    while Instant::now() < deadline {
        let n = audio.read(&mut buf);
        total_samples += n as u64;
        if n == 0 {
            if audio.is_eof() {
                break;
            }
            sleep(Duration::from_millis(10)).await;
        }
    }

    assert!(
        total_samples > 1000,
        "baseline: expected sustained playback, got only {total_samples} samples"
    );
}
