//! Stress test: rapid seeking during/after ABR switch with real Symphonia decoder.
//!
//! Uses production HLS stream (requires network).
//! Expected: FAILS — seek after ABR switch causes deadlock or audio death.

use std::time::{Duration, Instant};

use kithara::assets::StoreOptions;
use kithara::audio::{Audio, AudioConfig};
use kithara::hls::{AbrMode, AbrOptions, Hls, HlsConfig};
use kithara::stream::Stream;
use rstest::rstest;
use tempfile::TempDir;
use tracing::info;

use kithara_test_utils::temp_dir;

const HLS_URL: &str = "https://stream.silvercomet.top/hls/master.m3u8";

/// Stress test: 20 seconds of rapid seeking after ABR switch.
///
/// Reproduces production bug: after ABR switch (V0 AAC → V3 FLAC),
/// seek causes deadlock because `detect_format_change` picks wrong
/// segment offset → decoder created at wrong position → "missing ftyp atom".
#[rstest]
#[timeout(Duration::from_secs(60))]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires network — production HLS stream"]
async fn stress_seek_during_abr_switch_real_decoder(temp_dir: TempDir) {
    let _ = tracing_subscriber::fmt()
        .with_test_writer()
        .with_max_level(tracing::Level::DEBUG)
        .with_env_filter(std::env::var("RUST_LOG").unwrap_or_else(|_| {
            "kithara_audio=debug,kithara_hls=debug,kithara_decode=debug".to_string()
        }))
        .try_init();

    let url: url::Url = HLS_URL.parse().expect("valid URL");
    info!("Opening HLS stream: {}", url);

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
    let switches = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let switches_bg = switches.clone();
    tokio::spawn(async move {
        while let Ok(ev) = events_rx.recv().await {
            let ev_str = format!("{:?}", ev);
            if ev_str.contains("VariantApplied") {
                switches_bg.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                info!("ABR switch detected: {}", ev_str);
            }
        }
    });

    // Phase 1: Warmup — read PCM for ~10s to let ABR switch happen
    // Phase 2: 20s rapid seeking
    let result = tokio::task::spawn_blocking(move || {
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
            std::thread::sleep(Duration::from_millis(50));
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

        // Assert: not too many dead seeks (some failures OK, but not all)
        let dead_ratio = if seek_count > 0 {
            dead_seeks as f64 / seek_count as f64
        } else {
            1.0
        };
        assert!(
            dead_ratio < 0.9,
            "Too many dead seeks: {}/{} ({:.0}%). Audio is mostly dead after ABR switch.",
            dead_seeks,
            seek_count,
            dead_ratio * 100.0,
        );
    })
    .await;

    match result {
        Ok(()) => info!("Test passed"),
        Err(e) if e.is_panic() => std::panic::resume_unwind(e.into_panic()),
        Err(e) => panic!("spawn_blocking failed: {}", e),
    }
}
