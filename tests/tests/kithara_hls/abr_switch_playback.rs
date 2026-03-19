//! Regression test: ABR variant switch during normal playback must not hang.
//!
//! Reproduces the production crash where HLS ABR switch causes
//! `[HangDetector] run_shared_worker_loop no progress for 10s`.
//!
//! Uses real fMP4/AAC assets (same as production app) with disk cache.

use kithara::{
    assets::StoreOptions,
    audio::{Audio, AudioConfig},
    file::{File, FileConfig},
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

/// Stream must continue producing chunks after seek sequence.
///
/// Regression for app3.log: DRM track plays to 23.97s with seeks,
/// then stops producing chunks → recv_outcome_blocking hang.
///
/// Parameterized: path × ABR mode to isolate DRM vs HLS vs no-ABR.
#[kithara::test(
    tokio,
    native,
    serial,
    timeout(Duration::from_secs(30)),
    env(KITHARA_HANG_TIMEOUT_SECS = "5")
)]
#[case::drm_abr_auto("/drm/master.m3u8", true)]
#[case::hls_abr_auto("/hls/master.m3u8", true)]
#[case::drm_manual_v0("/drm/master.m3u8", false)]
#[case::hls_manual_v0("/hls/master.m3u8", false)]
async fn stream_continues_after_seek(
    _tracing_setup: (),
    temp_dir: TestTempDir,
    #[case] path: &str,
    #[case] abr_auto: bool,
) {
    let server = serve_assets().await;
    let url = server.url(path);

    let cancel = CancellationToken::new();
    let abr_mode = if abr_auto {
        AbrMode::Auto(Some(0))
    } else {
        AbrMode::Manual(0)
    };
    let hls_config = HlsConfig::new(url)
        .with_store(StoreOptions::new(temp_dir.path()))
        .with_cancel(cancel)
        .with_abr(AbrOptions {
            mode: abr_mode,
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
    //
    // Read a limited number of samples after each seek — NOT a wall-time
    // deadline — because the decoder decodes faster than real-time and
    // would otherwise reach EOF within each 3-second window.
    let samples_per_seek: u64 = 48000 * 2; // ~1 second of stereo 48 kHz
    for &target_secs in &[7.0, 13.0, 18.0, 24.0] {
        audio
            .seek(Duration::from_secs_f64(target_secs))
            .expect("seek");

        let mut samples = 0u64;
        let deadline = Instant::now() + Duration::from_secs(5);
        while samples < samples_per_seek && Instant::now() < deadline {
            let n = audio.read(&mut buf);
            samples += n as u64;
            if n == 0 {
                sleep(Duration::from_millis(10)).await;
            }
        }
        assert!(
            samples > 0,
            "[{path}] seek to {target_secs}s must produce samples, got 0"
        );
    }

    // Phase 3: Continue reading after last seek — must not hang.
    // Read another ~1s of audio to confirm playback continues.
    let mut post_seek_samples = 0u64;
    let deadline = Instant::now() + Duration::from_secs(5);
    while post_seek_samples < samples_per_seek && Instant::now() < deadline {
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
        "[{path}] playback after seeks must continue, got 0 samples"
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

/// MP3 progressive file must continue producing chunks after seek sequence.
///
/// Same seek pattern as HLS/DRM tests but with `Audio<Stream<File>>`.
/// Baseline: no ABR, no segments, no variant switching.
#[kithara::test(
    tokio,
    native,
    serial,
    timeout(Duration::from_secs(30)),
    env(KITHARA_HANG_TIMEOUT_SECS = "5")
)]
async fn mp3_stream_continues_after_seek(_tracing_setup: (), temp_dir: TestTempDir) {
    let server = serve_assets().await;
    let url = server.url("/track.mp3");

    let file_config = FileConfig::new(url.into()).with_store(StoreOptions::new(temp_dir.path()));
    let config = AudioConfig::<File>::new(file_config).with_hint("mp3");
    let mut audio = Audio::<Stream<File>>::new(config)
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

    // Phase 2: Seek to ~7s, ~13s, ~18s, ~24s (same as HLS/DRM tests)
    let samples_per_seek: u64 = 48000 * 2;
    for &target_secs in &[7.0, 13.0, 18.0, 24.0] {
        audio
            .seek(Duration::from_secs_f64(target_secs))
            .expect("seek");

        let mut samples = 0u64;
        let deadline = Instant::now() + Duration::from_secs(5);
        while samples < samples_per_seek && Instant::now() < deadline {
            let n = audio.read(&mut buf);
            samples += n as u64;
            if n == 0 {
                sleep(Duration::from_millis(10)).await;
            }
        }
        assert!(
            samples > 0,
            "[mp3] seek to {target_secs}s must produce samples, got 0"
        );
    }

    // Phase 3: Continue reading after last seek — must not hang
    let mut post_seek_samples = 0u64;
    let deadline = Instant::now() + Duration::from_secs(5);
    while post_seek_samples < samples_per_seek && Instant::now() < deadline {
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
        "[mp3] playback after seeks must continue, got 0 samples"
    );
}
