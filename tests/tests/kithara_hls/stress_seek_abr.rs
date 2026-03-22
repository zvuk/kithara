//! Stress test: rapid seeking during/after ABR switch with real Symphonia decoder.
//!
//! Uses production HLS stream (requires network).
//! Expected: FAILS — seek after ABR switch causes deadlock or audio death.

use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

use kithara::{
    assets::StoreOptions,
    audio::{Audio, AudioConfig},
    hls::{AbrMode, AbrOptions, Hls, HlsConfig},
    stream::Stream,
};
use kithara_platform::{
    thread,
    time::{Duration, Instant},
    tokio::task::{spawn, spawn_blocking},
};
use kithara_test_utils::{TestTempDir, serve_assets, temp_dir, tracing_setup};
use tracing::info;

/// Stress test: 20 seconds of rapid seeking after ABR switch.
///
/// Reproduces production bug: after ABR switch (V0 AAC → V3 FLAC),
/// seek causes deadlock because `detect_format_change` picks wrong
/// segment offset → decoder created at wrong position → "missing ftyp atom".
#[kithara::test(
    tokio,
    native,
    serial,
    timeout(Duration::from_secs(120)),
    env(KITHARA_HANG_TIMEOUT_SECS = "3")
)]
#[case::hls("/hls/master.m3u8", "HLS")]
#[case::drm("/drm/master.m3u8", "DRM")]
async fn stress_seek_during_abr_switch_real_decoder(
    temp_dir: TestTempDir,
    #[case] path: &str,
    #[case] label: &str,
) {
    let server = serve_assets().await;
    let url = server.url(path);
    info!(label, path, "Opening real stream");

    // Create audio pipeline with ABR auto (start from cheapest variant)
    let hls_config = HlsConfig::new(url)
        .with_store(StoreOptions::new(temp_dir.path()))
        .with_abr(AbrOptions {
            mode: AbrMode::Auto(Some(0)),
            ..Default::default()
        });
    let config = AudioConfig::<Hls>::new(hls_config);

    let mut audio = Audio::<Stream<Hls>>::new(config)
        .await
        .expect("audio creation");

    // Subscribe to unified events for ABR switch tracking
    let mut events_rx = audio.events();

    // Track ABR switches in background
    let switches = Arc::new(AtomicUsize::new(0));
    let switches_bg = switches.clone();
    spawn(async move {
        while let Ok(ev) = events_rx.recv().await {
            let ev_str = format!("{:?}", ev);
            if ev_str.contains("VariantApplied") {
                switches_bg.fetch_add(1, Ordering::Relaxed);
                info!("ABR switch detected: {}", ev_str);
            }
        }
    });

    // Phase 1: Warmup — read PCM for ~10s to let ABR switch happen
    // Phase 2: 20s rapid seeking
    let result = spawn_blocking(move || {
        let mut buf = vec![0f32; 4096];
        let start = Instant::now();

        info!("Phase 1: warmup — reading PCM samples");
        let mut warmup_samples = 0u64;
        while start.elapsed() < Duration::from_secs(10) {
            let n = audio.read(&mut buf);
            if n == 0 {
                break;
            }
            warmup_samples += n as u64;
        }
        info!(
            warmup_samples,
            elapsed_ms = start.elapsed().as_millis(),
            "Warmup done"
        );

        // Phase 2: 20 seconds of rapid seeking
        info!("Phase 2: stress seeking for 20 seconds");
        let seek_start = Instant::now();
        let mut seek_count = 0u64;
        let mut samples_after_seek = 0u64;
        let mut seek_errors = 0u64;
        let mut dead_seeks = 0u64;

        // Seek positions spanning the full track (0s to ~220s)
        let positions_secs: Vec<f64> = vec![
            147.0, 30.0, 200.0, 5.0, 180.0, 60.0, 210.0, 15.0, 100.0, 0.0, 170.0, 45.0, 195.0,
            80.0, 220.0, 10.0, 130.0, 25.0, 160.0, 90.0, 50.0, 110.0, 175.0, 35.0, 140.0, 70.0,
            205.0, 55.0, 120.0, 185.0, 20.0, 150.0,
        ];

        let mut pos_idx = 0;
        while seek_start.elapsed() < Duration::from_secs(20) {
            let pos = positions_secs[pos_idx % positions_secs.len()];
            pos_idx += 1;

            let position = Duration::from_secs_f64(pos);
            match audio.seek(position) {
                Ok(()) => {
                    seek_count += 1;
                    // Try to read some samples after seek
                    let n = audio.read(&mut buf);
                    if n > 0 {
                        samples_after_seek += n as u64;
                    } else {
                        dead_seeks += 1;
                    }
                }
                Err(e) => {
                    seek_errors += 1;
                    info!(?e, pos, "seek error");
                }
            }

            // Small pause between seeks (simulate real user interaction)
            thread::sleep(Duration::from_millis(50));
        }

        info!(
            seek_count,
            samples_after_seek, seek_errors, dead_seeks, "Stress test complete"
        );

        // Assert: at least some seeks produced audio
        // If ALL seeks produce 0 samples → audio is dead → bug confirmed
        assert!(
            samples_after_seek > 0,
            "Audio died after ABR switch: {} seeks, {} errors, {} dead (0 samples), \
             0 total samples produced. Bug: seek after ABR switch kills audio.",
            seek_count,
            seek_errors,
            dead_seeks,
        );

        // Assert: zero dead seeks — with seek_pending retry, all seeks must produce audio.
        assert_eq!(
            dead_seeks, 0,
            "Dead seeks: {dead_seeks}/{seek_count}. \
             All seeks must produce audio with seek_pending retry.",
        );
    })
    .await;

    match result {
        Ok(()) => info!(label, path, "Stress test passed"),
        Err(e) => panic!("spawn_blocking failed: {e}"),
    }
}

/// Repro test for production issue: repeated seeks on the exact stream.
///
/// Uses seek positions observed in logs and asserts that each seek
/// still yields PCM samples (audio must stay alive).
#[kithara::test(
    tokio,
    native,
    serial,
    timeout(Duration::from_secs(120)),
    env(KITHARA_HANG_TIMEOUT_SECS = "5")
)]
#[case::hls("/hls/master.m3u8", "HLS")]
#[case::drm("/drm/master.m3u8", "DRM")]
async fn seek_sequence_from_log_real_stream(
    _tracing_setup: (),
    temp_dir: TestTempDir,
    #[case] path: &str,
    #[case] label: &str,
) {
    let server = serve_assets().await;
    let url = server.url(path);
    let hls_config = HlsConfig::new(url)
        .with_store(StoreOptions::new(temp_dir.path()))
        .with_abr(AbrOptions {
            mode: AbrMode::Auto(Some(0)),
            ..Default::default()
        });
    let config = AudioConfig::<Hls>::new(hls_config);
    let mut audio = Audio::<Stream<Hls>>::new(config)
        .await
        .expect("audio creation");

    let result = spawn_blocking(move || {
        let mut buf = vec![0f32; 4096];
        // Warm up pipeline before seek sequence.
        let warmup_deadline = Instant::now() + Duration::from_secs(4);
        while Instant::now() < warmup_deadline {
            let _ = audio.read(&mut buf);
        }

        let seeks = [7.135_147_392, 12.279_818_594, 17.778_684_807];
        for (idx, seconds) in seeks.into_iter().enumerate() {
            let pos = Duration::from_secs_f64(seconds);
            audio.seek(pos).expect("seek must not fail");

            let mut samples_after_seek = 0usize;
            let read_deadline = Instant::now() + Duration::from_secs(8);
            while Instant::now() < read_deadline && samples_after_seek < 16_384 {
                let n = audio.read(&mut buf);
                samples_after_seek += n;
                if n == 0 {
                    thread::sleep(Duration::from_millis(15));
                }
            }

            assert!(
                samples_after_seek > 0,
                "seek #{idx} at {seconds:.3}s produced no PCM samples"
            );
        }
    })
    .await;

    match result {
        Ok(()) => info!(label, path, "seek_sequence_from_log_real_stream passed"),
        Err(e) => panic!("spawn_blocking failed: {e}"),
    }
}
