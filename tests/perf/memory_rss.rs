#![expect(
    clippy::cast_precision_loss,
    reason = "RSS values in MB, f64 precision is sufficient"
)]
//! RSS memory profiling tests for HLS playback pipeline.
//!
//! Measures resident set size (RSS) while running the full pipeline:
//! HTTP → HLS parsing → segment download → asset store → stream → decode →
//! resampler → PCM buffers.
//!
//! Run with: `cargo test --test memory_rss -- --test-threads=1 --nocapture`

#![cfg(not(target_arch = "wasm32"))]

use std::time::Duration;

use hotpath::FunctionsGuardBuilder;
use kithara::{
    assets::StoreOptions,
    audio::{Audio, AudioConfig},
    hls::{AbrMode, AbrOptions, Hls, HlsConfig},
    stream::Stream,
};
use kithara_platform::{time::Instant, tokio::task::spawn_blocking};
use kithara_test_utils::{TestTempDir, serve_assets, temp_dir};
use memory_stats::memory_stats;
use tracing::info;

const MB: usize = 1024 * 1024;

// Test 1: RSS budget

const BUDGET_RUNS: usize = 3;
const BUDGET_PLAYBACK_SECS: u64 = 10;
const BUDGET_SAMPLE_INTERVAL_MS: u64 = 500;
const RSS_BUDGET_MB: usize = 30;

/// Multi-run RSS measurement: peak RSS delta must stay within budget.
#[kithara::test(
    native,
    tokio,
    serial,
    timeout(Duration::from_secs(10)),
    env(KITHARA_HANG_TIMEOUT_SECS = "1")
)]
async fn test_hls_playback_rss_within_budget(temp_dir: TestTempDir) {
    let _guard = FunctionsGuardBuilder::new("rss_budget").build();
    let mut run_deltas = Vec::with_capacity(BUDGET_RUNS);

    for run in 0..BUDGET_RUNS {
        let baseline_rss = memory_stats()
            .expect("memory_stats unsupported")
            .physical_mem;

        let server = serve_assets().await;
        let url = server.url("/hls/master.m3u8");

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

        let samples = spawn_blocking(move || {
            let mut buf = vec![0f32; 4096];
            let mut rss_samples = Vec::new();
            let start = Instant::now();
            let mut last_sample = start;

            while start.elapsed() < Duration::from_secs(BUDGET_PLAYBACK_SECS) {
                let n = audio.read(&mut buf);
                if n == 0 {
                    break;
                }

                if last_sample.elapsed() >= Duration::from_millis(BUDGET_SAMPLE_INTERVAL_MS) {
                    if let Some(stats) = memory_stats() {
                        rss_samples.push(stats.physical_mem);
                    }
                    last_sample = Instant::now();
                }
            }
            rss_samples
        })
        .await
        .expect("spawn_blocking");

        let peak_rss = samples.iter().copied().max().unwrap_or(baseline_rss);
        let delta = peak_rss.saturating_sub(baseline_rss);
        run_deltas.push(delta);

        info!(
            "Run {run}: baseline={:.1}MB peak={:.1}MB delta={:.1}MB samples={}",
            baseline_rss as f64 / MB as f64,
            peak_rss as f64 / MB as f64,
            delta as f64 / MB as f64,
            samples.len(),
        );

        // Cleanup between runs
        drop(server);
    }

    let min_delta = run_deltas.iter().copied().min().unwrap_or(0);
    let max_delta = run_deltas.iter().copied().max().unwrap_or(0);
    let mean_delta = run_deltas.iter().sum::<usize>() / run_deltas.len();

    info!(
        "RSS deltas: min={:.1}MB mean={:.1}MB max={:.1}MB budget={RSS_BUDGET_MB}MB",
        min_delta as f64 / MB as f64,
        mean_delta as f64 / MB as f64,
        max_delta as f64 / MB as f64,
    );

    assert!(
        max_delta < RSS_BUDGET_MB * MB,
        "RSS exceeded budget: max delta {:.1}MB > {RSS_BUDGET_MB}MB",
        max_delta as f64 / MB as f64,
    );
}

// Test 2: No RSS leak

const LEAK_PLAYBACK_SECS: u64 = 15;
const LEAK_WARMUP_SECS: u64 = 5;
const LEAK_TOLERANCE_MB: usize = 5;

/// RSS should stabilize after warmup — no sustained growth.
#[kithara::test(
    native,
    tokio,
    serial,
    timeout(Duration::from_secs(10)),
    env(KITHARA_HANG_TIMEOUT_SECS = "1")
)]
async fn test_hls_playback_no_rss_leak(temp_dir: TestTempDir) {
    let _guard = FunctionsGuardBuilder::new("rss_leak").build();
    let server = serve_assets().await;
    let url = server.url("/hls/master.m3u8");

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

    let (warmup_rss, final_rss) = spawn_blocking(move || {
        let mut buf = vec![0f32; 4096];
        let start = Instant::now();
        let mut warmup_rss = None;
        let mut final_rss = 0usize;

        while start.elapsed() < Duration::from_secs(LEAK_PLAYBACK_SECS) {
            let n = audio.read(&mut buf);
            if n == 0 {
                break;
            }

            let elapsed = start.elapsed();

            // Capture RSS at warmup boundary
            if warmup_rss.is_none()
                && elapsed >= Duration::from_secs(LEAK_WARMUP_SECS)
                && let Some(stats) = memory_stats()
            {
                warmup_rss = Some(stats.physical_mem);
            }

            // Continuously update final RSS
            if let Some(stats) = memory_stats() {
                final_rss = stats.physical_mem;
            }
        }

        let warmup = warmup_rss.unwrap_or(final_rss);
        (warmup, final_rss)
    })
    .await
    .expect("spawn_blocking");

    let growth = final_rss.saturating_sub(warmup_rss);

    info!(
        "Leak test: warmup={:.1}MB final={:.1}MB growth={:.1}MB tolerance={LEAK_TOLERANCE_MB}MB",
        warmup_rss as f64 / MB as f64,
        final_rss as f64 / MB as f64,
        growth as f64 / MB as f64,
    );

    assert!(
        growth < LEAK_TOLERANCE_MB * MB,
        "RSS grew after warmup: {:.1}MB > {LEAK_TOLERANCE_MB}MB (warmup={:.1}MB final={:.1}MB)",
        growth as f64 / MB as f64,
        warmup_rss as f64 / MB as f64,
        final_rss as f64 / MB as f64,
    );
}
