//! WASM stress tests for the HLS audio pipeline.
//!
//! Runs in headless Chrome via `wasm-pack test --headless --chrome`.
//! Requires the HLS fixture server to be running (see `scripts/ci/wasm-test.sh`).

use std::time::Duration;

use kithara_audio::{Audio, AudioConfig, EventBus};
use kithara_hls::{AbrMode, AbrOptions, Hls, HlsConfig};
use kithara_stream::{Stream, ThreadPool};
use tracing::info;
use url::Url;
use wasm_bindgen_futures::JsFuture;
use wasm_bindgen_test::*;

wasm_bindgen_test_configure!(run_in_browser);

/// Number of rayon worker threads for the thread pool.
const THREAD_COUNT: usize = 2;

/// Get HLS test URL from compile-time env or fall back to default.
fn fixture_url() -> Url {
    let url_str = option_env!("HLS_TEST_URL").unwrap_or("http://127.0.0.1:3333/master.m3u8");
    url_str.parse().unwrap()
}

/// One-time initialization: panic hook, tracing, rayon thread pool.
async fn init() {
    console_error_panic_hook::set_once();
    tracing_wasm::set_as_global_default();

    // Initialize rayon thread pool (Web Workers).
    JsFuture::from(kithara_wasm::init_thread_pool(THREAD_COUNT))
        .await
        .unwrap();
    info!("Rayon thread pool initialized with {THREAD_COUNT} workers");
}

/// Create an `Audio<Stream<Hls>>` pipeline in ephemeral mode.
async fn create_pipeline() -> Audio<Stream<Hls>> {
    let pool = ThreadPool::global();
    let bus = EventBus::new(128);

    let hls_config = HlsConfig::new(fixture_url())
        .with_thread_pool(pool)
        .with_events(bus)
        .with_ephemeral(true)
        .with_abr(AbrOptions {
            mode: AbrMode::Auto(Some(0)),
            ..Default::default()
        });

    let config = AudioConfig::<Hls>::new(hls_config);
    let mut audio = Audio::<Stream<Hls>>::new(config).await.unwrap();
    audio.preload();
    audio
}

/// Non-blocking read that yields to the event loop between attempts.
///
/// On wasm32 main thread, `Atomics.wait` is forbidden. The audio pipeline
/// uses `preload()` mode where `read()` returns 0 when data isn't ready yet.
/// We yield via `gloo_timers` to let async downloads and Web Workers proceed.
async fn read_with_yield(audio: &mut Audio<Stream<Hls>>, buf: &mut [f32]) -> usize {
    const MAX_YIELDS: usize = 500;
    const YIELD_MS: u32 = 20;

    for _ in 0..MAX_YIELDS {
        let n = audio.read(buf);
        if n > 0 {
            return n;
        }
        if audio.is_eof() {
            return 0;
        }
        // Yield to event loop so downloads and decoder can progress.
        gloo_timers::future::TimeoutFuture::new(YIELD_MS).await;
    }
    0
}

/// Yield to event loop so async I/O and Web Workers can progress.
async fn yield_ms(ms: u32) {
    gloo_timers::future::TimeoutFuture::new(ms).await;
}

#[wasm_bindgen_test]
async fn stress_read_samples_integrity() {
    init().await;
    info!("Starting stress_read_samples_integrity");

    let mut audio = create_pipeline().await;
    let spec = audio.spec();

    assert!(spec.channels > 0, "channels must be > 0");
    assert!(spec.sample_rate > 0, "sample_rate must be > 0");
    info!(
        channels = spec.channels,
        sample_rate = spec.sample_rate,
        "Audio spec"
    );

    let mut buf = vec![0.0f32; 4096];
    let mut total_samples = 0usize;
    let target_chunks = 200;
    let mut chunks_read = 0;

    for _ in 0..target_chunks {
        let n = read_with_yield(&mut audio, &mut buf).await;
        if n == 0 {
            break;
        }
        chunks_read += 1;
        total_samples += n;

        // Verify sample integrity: all finite, within [-1.0, 1.0]
        for (i, &sample) in buf[..n].iter().enumerate() {
            assert!(
                sample.is_finite(),
                "sample {i} in chunk {chunks_read} is not finite: {sample}"
            );
            assert!(
                (-1.0..=1.0).contains(&sample),
                "sample {i} in chunk {chunks_read} out of range: {sample}"
            );
        }
    }

    info!(total_samples, chunks_read, "Read complete");
    assert!(total_samples > 0, "must read at least some audio samples");
    assert!(
        chunks_read >= 5,
        "expected at least 5 chunks, got {chunks_read}"
    );
}

#[wasm_bindgen_test]
async fn stress_seek_and_read() {
    init().await;
    info!("Starting stress_seek_and_read");

    let mut audio = create_pipeline().await;

    // Read some data first to ensure pipeline is warm.
    let mut buf = vec![0.0f32; 4096];
    let mut warmup = 0;
    for _ in 0..20 {
        let n = read_with_yield(&mut audio, &mut buf).await;
        if n == 0 {
            break;
        }
        warmup += n;
    }
    assert!(warmup > 0, "warmup must read some data");
    info!(warmup, "Pipeline warmed up");

    let duration = audio.duration().unwrap_or(Duration::from_secs(30));
    let duration_secs = duration.as_secs_f64();

    // Perform seeks to various positions and verify data integrity after each.
    let seek_positions = [0.1, 0.5, 0.25, 0.75, 0.0, 0.9, 0.3, 0.6, 0.15, 0.85];

    for (i, &frac) in seek_positions.iter().enumerate() {
        let seek_to = Duration::from_secs_f64(duration_secs * frac);
        info!(seek_idx = i, seek_ms = seek_to.as_millis(), "Seeking");

        if let Err(e) = audio.seek(seek_to) {
            info!(?e, "Seek failed (may be near boundary), skipping");
            continue;
        }

        // Read after seek and verify integrity.
        let mut post_seek_samples = 0;
        for _ in 0..10 {
            let n = read_with_yield(&mut audio, &mut buf).await;
            if n == 0 {
                break;
            }
            post_seek_samples += n;
            for &sample in &buf[..n] {
                assert!(
                    sample.is_finite() && (-1.0..=1.0).contains(&sample),
                    "corrupt sample after seek {i}: {sample}"
                );
            }
        }
        info!(post_seek_samples, "Read after seek {i}");
    }
}

/// The actual WASM player uses `WasmPlayer::fill_buffer()` which reads decoded
/// PCM via `Audio::read()` and writes into `PcmRingBuffer`.  The JS AudioWorklet
/// (`pcm-processor.js`) consumes from the ring buffer at real-time rate.
///
/// Bug: `fill_buffer()` reads from the decoder (advancing `position()`) without
/// checking ring buffer capacity.  When the ring is full, overflow is silently
/// dropped.  Position races ahead, audio skips.
///
/// This test exercises the real `WasmPlayer::fill_buffer()` path and verifies
/// that every sample read from the decoder makes it to the ring buffer.
#[wasm_bindgen_test]
async fn fill_buffer_position_must_not_drift() {
    init().await;
    info!("Starting fill_buffer_position_must_not_drift");

    // Load HLS via the real WasmPlayer path (stores Audio in thread_local).
    let mut player = kithara_wasm::WasmPlayer::new();
    let info = kithara_wasm::load_hls(fixture_url().to_string())
        .await
        .unwrap();
    let channels = info[0];
    let sample_rate = info[1];
    info!(channels, sample_rate, "Loaded HLS");

    player.update_ring(channels as u32, sample_rate as u32);
    player.play();

    let rate = sample_rate * channels;
    let mut total_to_ring = 0u64;

    // Call fill_buffer in a loop, yielding to let async I/O progress.
    // No AudioWorklet is consuming — ring will fill up quickly.
    for i in 0..200 {
        let written = player.fill_buffer();
        total_to_ring += u64::from(written);

        if written == 0 {
            if player.is_eof() {
                info!(iteration = i, "EOF reached");
                break;
            }
            // No data yet or ring full — yield and retry.
            yield_ms(20).await;
            continue;
        }

        // Yield every 10 iterations to let downloads progress.
        if i % 10 == 9 {
            yield_ms(5).await;
        }
    }

    assert!(total_to_ring > 0, "must write some samples to ring");

    let position_ms = player.get_position_ms();
    let ring_ms = (total_to_ring as f64 / rate) * 1000.0;

    info!(
        position_ms,
        ring_ms, total_to_ring, "Position vs ring content"
    );

    // Position (from Audio::samples_read) must match what actually reached
    // the ring buffer.  With backpressure they should be equal.
    // Without it, position >> ring_ms (the bug).
    let drift = position_ms / ring_ms.max(0.001);
    assert!(
        drift < 1.05,
        "position drifted {drift:.1}x ahead of ring buffer: \
         position={position_ms:.0}ms but ring received only {ring_ms:.0}ms of audio"
    );
}
