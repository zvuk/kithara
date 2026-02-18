//! WASM stress tests for the HLS audio pipeline.
//!
//! Runs in a dedicated Web Worker via `wasm-bindgen-test-runner` + headless Chrome.
//! Requires the HLS fixture server to be running (see `scripts/ci/wasm-test.sh`).

use std::{
    sync::atomic::{AtomicBool, Ordering},
    time::Duration,
};

use kithara::{
    assets::StoreOptions,
    audio::{Audio, AudioConfig},
    events::EventBus,
    hls::{AbrMode, AbrOptions, Hls, HlsConfig},
    platform::ThreadPool,
    stream::{AudioCodec, ContainerFormat, MediaInfo, Stream},
};
use tracing::{info, warn};
use url::Url;
use wasm_bindgen_futures::JsFuture;
use wasm_bindgen_test::*;

wasm_bindgen_test_configure!(run_in_dedicated_worker);

/// Number of rayon worker threads for the thread pool.
const THREAD_COUNT: usize = 2;

/// Guard: `init_thread_pool` panics if called twice in the same page.
static INITIALIZED: AtomicBool = AtomicBool::new(false);

/// Get HLS test URL from compile-time env or fall back to default.
fn fixture_url() -> Url {
    let url_str = option_env!("HLS_TEST_URL").unwrap_or("http://127.0.0.1:3333/master.m3u8");
    url_str.parse().unwrap()
}

/// Minimal xorshift64 PRNG for deterministic seek positions.
struct Xorshift64(u64);

impl Xorshift64 {
    fn new(seed: u64) -> Self {
        Self(seed)
    }

    fn next_u64(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn next_f64(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64
    }

    fn range_f64(&mut self, min: f64, max: f64) -> f64 {
        min + (max - min) * self.next_f64()
    }
}

/// One-time initialization: panic hook, tracing, rayon thread pool.
///
/// Idempotent — safe to call from every test. All tests share one page
/// in `wasm_bindgen_test`, so `init_thread_pool` must only run once.
async fn init() {
    if INITIALIZED.swap(true, Ordering::SeqCst) {
        return;
    }
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
        .with_store(StoreOptions::default().with_ephemeral(true))
        .with_abr(AbrOptions {
            mode: AbrMode::Auto(Some(0)),
            ..Default::default()
        });

    let wav_info = MediaInfo::new(Some(AudioCodec::Pcm), Some(ContainerFormat::Wav));
    let config = AudioConfig::<Hls>::new(hls_config).with_media_info(wav_info);
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
    read_with_yield_limit(audio, buf, 500).await
}

/// Read with configurable retry limit.
async fn read_with_yield_limit(
    audio: &mut Audio<Stream<Hls>>,
    buf: &mut [f32],
    max_yields: usize,
) -> usize {
    const YIELD_MS: u32 = 10;

    for _ in 0..max_yields {
        let n = audio.read(buf);
        if n > 0 {
            return n;
        }
        if audio.is_eof() {
            return 0;
        }
        gloo_timers::future::TimeoutFuture::new(YIELD_MS).await;
    }
    0
}

/// Yield to event loop so async I/O and Web Workers can progress.
async fn yield_ms(ms: u32) {
    gloo_timers::future::TimeoutFuture::new(ms).await;
}

// ── Saw-tooth verification helpers ──
//
// The HLS fixture server (`hls_fixture_server`) serves a deterministic saw-tooth
// WAV signal with period 65536 frames. Each frame encodes its position as an i16
// value, allowing content-based seek verification: after seek(T), the decoded
// phase must match the expected phase for time T.

/// Saw-tooth period: 65536 frames (~1.486s at 44100 Hz).
const SAW_PERIOD: usize = 65536;

/// Recover saw-tooth phase from a decoded f32 sample.
///
/// Inverse of the generator: `sample = ((i % SAW_PERIOD) as i32 - 32768) as i16`.
fn phase_from_f32(sample: f32) -> usize {
    let i16_val = (sample * 32768.0).round() as i32;
    ((i16_val + 32768) & 0xFFFF) as usize
}

/// Circular distance between two phases (mod `SAW_PERIOD`).
fn phase_distance(a: usize, b: usize) -> usize {
    let d = a.abs_diff(b);
    d.min(SAW_PERIOD - d)
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

    let channels = spec.channels as usize;
    let mut buf = vec![0.0f32; 4096];
    let mut total_samples = 0usize;
    let target_chunks = 200;
    let mut chunks_read = 0;
    let mut channel_mismatches = 0u64;
    let mut continuity_errors = 0u64;
    let mut prev_last_phase: Option<usize> = None;

    for _ in 0..target_chunks {
        let n = read_with_yield(&mut audio, &mut buf).await;
        if n == 0 {
            break;
        }
        chunks_read += 1;
        total_samples += n;
        let frames = n / channels;

        // Level 1: Integrity — samples finite, in [-1.0, 1.0]
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

        // Level 1b: L == R (stereo saw-tooth has identical channels)
        if channels == 2 {
            for f in 0..frames {
                let l = buf[f * 2];
                let r = buf[f * 2 + 1];
                if (l - r).abs() > f32::EPSILON {
                    channel_mismatches += 1;
                    if channel_mismatches <= 3 {
                        warn!(chunk = chunks_read, frame = f, l, r, "L/R mismatch");
                    }
                }
            }
        }

        // Level 2: Continuity — consecutive frames follow saw-tooth pattern
        // Inter-chunk: last frame of prev chunk → first frame of this chunk
        if let Some(prev_phase) = prev_last_phase {
            let first_phase = phase_from_f32(buf[0]);
            let expected = (prev_phase + 1) % SAW_PERIOD;
            if first_phase != expected {
                continuity_errors += 1;
                if continuity_errors <= 3 {
                    warn!(
                        chunk = chunks_read,
                        prev_phase, first_phase, expected, "inter-chunk continuity break"
                    );
                }
            }
        }

        // Intra-chunk: every pair of adjacent frames
        if frames >= 2 {
            for f in 1..frames {
                let prev = phase_from_f32(buf[(f - 1) * channels]);
                let curr = phase_from_f32(buf[f * channels]);
                let expected = (prev + 1) % SAW_PERIOD;
                if curr != expected {
                    continuity_errors += 1;
                    if continuity_errors <= 3 {
                        warn!(
                            chunk = chunks_read,
                            frame = f,
                            prev,
                            curr,
                            expected,
                            "intra-chunk continuity break"
                        );
                    }
                }
            }
        }

        // Track last phase for inter-chunk continuity
        if frames > 0 {
            prev_last_phase = Some(phase_from_f32(buf[(frames - 1) * channels]));
        }
    }

    info!(
        total_samples,
        chunks_read, channel_mismatches, continuity_errors, "Read complete"
    );
    assert!(total_samples > 0, "must read at least some audio samples");
    assert!(
        chunks_read >= 5,
        "expected at least 5 chunks, got {chunks_read}"
    );
    assert_eq!(
        channel_mismatches, 0,
        "L/R channels diverged {channel_mismatches} times — data corruption"
    );
    assert!(
        continuity_errors <= 5,
        "{continuity_errors} continuity breaks (>5 tolerance) — non-contiguous decoded data"
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
    let spec = audio.spec();
    let channels = spec.channels as usize;
    let mut position_errors = 0u64;
    let mut channel_mismatches = 0u64;
    let mut continuity_errors = 0u64;

    for (i, &frac) in seek_positions.iter().enumerate() {
        let pos_secs = duration_secs * frac;
        let seek_to = Duration::from_secs_f64(pos_secs);
        info!(seek_idx = i, seek_ms = seek_to.as_millis(), "Seeking");

        if let Err(e) = audio.seek(seek_to) {
            info!(?e, "Seek failed (may be near boundary), skipping");
            continue;
        }

        // Read after seek and verify integrity.
        let mut post_seek_samples = 0;
        let mut position_checked = false;
        for _ in 0..10 {
            let n = read_with_yield(&mut audio, &mut buf).await;
            if n == 0 {
                break;
            }
            let frames = n / channels;
            post_seek_samples += n;

            // Level 1: Integrity
            for &sample in &buf[..n] {
                assert!(
                    sample.is_finite() && (-1.0..=1.0).contains(&sample),
                    "corrupt sample after seek {i}: {sample}"
                );
            }

            // Level 1b: L == R
            if channels == 2 {
                for f in 0..frames {
                    let l = buf[f * 2];
                    let r = buf[f * 2 + 1];
                    if (l - r).abs() > f32::EPSILON {
                        channel_mismatches += 1;
                    }
                }
            }

            // Level 2: Intra-chunk continuity
            if frames >= 2 {
                for f in 1..frames {
                    let prev = phase_from_f32(buf[(f - 1) * channels]);
                    let curr = phase_from_f32(buf[f * channels]);
                    if curr != (prev + 1) % SAW_PERIOD {
                        continuity_errors += 1;
                    }
                }
            }

            // Level 3: Position — first read after seek must match expected phase.
            // Tolerance: 1200 frames (~27ms) for decoder packet boundary alignment.
            if !position_checked && frames > 0 {
                let expected_frame = (pos_secs * spec.sample_rate as f64).round() as usize;
                let expected_phase = expected_frame % SAW_PERIOD;
                let actual_phase = phase_from_f32(buf[0]);
                let dist = phase_distance(actual_phase, expected_phase);
                if dist > 1200 {
                    position_errors += 1;
                    warn!(
                        seek_idx = i,
                        pos_secs,
                        expected_phase,
                        actual_phase,
                        dist,
                        "position mismatch after seek"
                    );
                }
                position_checked = true;
            }
        }
        info!(post_seek_samples, "Read after seek {i}");
    }

    info!(
        position_errors,
        channel_mismatches, continuity_errors, "Seek test done"
    );
    assert_eq!(
        channel_mismatches, 0,
        "L/R channels diverged {channel_mismatches} times"
    );
    assert!(
        continuity_errors <= 5,
        "{continuity_errors} continuity breaks (>5 tolerance)"
    );
    assert!(
        position_errors <= 1,
        "{position_errors} position mismatches — seek landed in wrong place"
    );
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
    let wav_info = MediaInfo::new(Some(AudioCodec::Pcm), Some(ContainerFormat::Wav));
    let info = kithara_wasm::load_hls_with_media_info(fixture_url().to_string(), wav_info)
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

/// Aggressive seek stress test: 1000 rapid random seeks.
///
/// Catches the bug where `read()` returns 0 after seek and the pipeline
/// stalls permanently (player stops, never resumes without stop+restart).
///
/// Strategy:
/// - Warmup: read 20 chunks to prime the pipeline
/// - 1000 random seeks: 10% near start (<1s), 10% near end, 80% random
/// - After each seek: read_with_yield must produce >0 samples (not stuck)
/// - All samples must be finite and in [-1.0, 1.0]
/// - Tolerate at most 1% dead seeks (pipeline restart race)
#[wasm_bindgen_test]
async fn stress_rapid_seeks_must_not_stall() {
    init().await;
    info!("Starting stress_rapid_seeks_must_not_stall");

    let mut audio = create_pipeline().await;
    let spec = audio.spec();
    info!(
        channels = spec.channels,
        sample_rate = spec.sample_rate,
        "Pipeline created"
    );

    let mut buf = vec![0.0f32; 4096];

    // Phase 1: Warmup
    let mut warmup_samples = 0usize;
    for _ in 0..20 {
        let n = read_with_yield(&mut audio, &mut buf).await;
        if n == 0 {
            break;
        }
        warmup_samples += n;
    }
    assert!(warmup_samples > 0, "warmup must produce data");
    info!(warmup_samples, "Warmup complete");

    let duration = audio.duration().unwrap_or(Duration::from_secs(60));
    let duration_secs = duration.as_secs_f64();
    let max_seek = duration_secs - 0.5;
    info!(duration_secs, max_seek, "Duration known");

    // Phase 2: 1000 rapid random seeks
    const SEEK_COUNT: usize = 1000;
    let sample_rate = audio.spec().sample_rate;
    let channels = audio.spec().channels as usize;
    let mut rng = Xorshift64::new(0xDEAD_BEEF_CAFE_1337);
    let mut dead_seeks = 0u64;
    let mut seek_errors = 0u64;
    let mut integrity_errors = 0u64;
    let mut position_mismatches = 0u64;
    let mut total_samples = 0u64;

    for i in 0..SEEK_COUNT {
        // Mix: 10% near start, 10% near end, 80% random
        let r = rng.next_f64();
        let pos_secs = if r < 0.1 {
            rng.range_f64(0.0, 1.0_f64.min(max_seek))
        } else if r < 0.2 {
            rng.range_f64((max_seek - 2.0).max(0.0), max_seek)
        } else {
            rng.range_f64(0.001, max_seek)
        };

        let position = Duration::from_secs_f64(pos_secs);
        if let Err(e) = audio.seek(position) {
            seek_errors += 1;
            if seek_errors <= 3 {
                warn!(iteration = i, pos_secs, ?e, "seek error");
            }
            continue;
        }

        // Must produce data within 200 yields (2s at 10ms each)
        let n = read_with_yield_limit(&mut audio, &mut buf, 200).await;
        if n == 0 && !audio.is_eof() {
            dead_seeks += 1;
            if dead_seeks <= 5 {
                warn!(
                    iteration = i,
                    pos_secs,
                    is_eof = audio.is_eof(),
                    "STALL: read returned 0 after seek"
                );
            }
            continue;
        }

        // Integrity check
        for &sample in &buf[..n] {
            if !sample.is_finite() || !(-1.0..=1.0).contains(&sample) {
                integrity_errors += 1;
                break;
            }
        }

        // Position check: decoded phase must match expected phase for seek target.
        // Tolerance: 1200 frames (~27ms) for packet boundary alignment.
        let frames = n / channels;
        if frames > 0 {
            let expected_frame = (pos_secs * sample_rate as f64).round() as usize;
            let expected_phase = expected_frame % SAW_PERIOD;
            let actual_phase = phase_from_f32(buf[0]);
            let dist = phase_distance(actual_phase, expected_phase);
            if dist > 1200 {
                position_mismatches += 1;
                if position_mismatches <= 5 {
                    warn!(
                        iteration = i,
                        pos_secs, expected_phase, actual_phase, dist, "position mismatch"
                    );
                }
            }
        }

        total_samples += n as u64;

        if (i + 1) % 200 == 0 {
            info!(
                iteration = i + 1,
                dead_seeks, seek_errors, position_mismatches, total_samples, "Progress"
            );
        }

        // Yield every 50 iterations to keep event loop responsive
        if i % 50 == 49 {
            yield_ms(1).await;
        }
    }

    info!(
        dead_seeks,
        seek_errors,
        integrity_errors,
        position_mismatches,
        total_samples,
        "Phase 2 complete: {SEEK_COUNT} seeks"
    );

    let max_dead = (SEEK_COUNT as u64) / 100; // 1% threshold
    assert!(
        dead_seeks <= max_dead,
        "pipeline stalled {dead_seeks}/{SEEK_COUNT} times \
         (>{max_dead} = 1% threshold) — read() returns 0 after seek"
    );
    assert_eq!(
        integrity_errors, 0,
        "samples outside [-1,1] or not finite — data corruption"
    );

    // Soft threshold: at most 5% position mismatches
    let max_mismatches = (SEEK_COUNT as u64) / 20;
    assert!(
        position_mismatches <= max_mismatches,
        "position mismatches {position_mismatches}/{SEEK_COUNT} \
         (>{max_mismatches} = 5% threshold) — seek landed in wrong place"
    );
}

/// Seek-to-zero after heavy pressure: ensures the beginning of the track
/// is still accessible after thousands of operations.
///
/// Catches the bug where seeking back to position 0 produces no data
/// because first segments were evicted or the pipeline entered a bad state.
///
/// Strategy:
/// - Warmup: read 20 chunks
/// - Stress: 500 random seeks (prime the pipeline, exercise eviction)
/// - Reset: seek to 0
/// - Verify: read at least 50 chunks from the beginning, all valid
/// - Position must be near 0 after seeking, then advance monotonically
#[wasm_bindgen_test]
async fn stress_seek_to_zero_after_pressure() {
    init().await;
    info!("Starting stress_seek_to_zero_after_pressure");

    let mut audio = create_pipeline().await;
    let mut buf = vec![0.0f32; 4096];

    // Phase 1: Warmup
    let mut warmup = 0usize;
    for _ in 0..20 {
        let n = read_with_yield(&mut audio, &mut buf).await;
        if n == 0 {
            break;
        }
        warmup += n;
    }
    assert!(warmup > 0, "warmup must produce data");

    let duration = audio.duration().unwrap_or(Duration::from_secs(60));
    let duration_secs = duration.as_secs_f64();
    let max_seek = duration_secs - 0.5;
    info!(warmup, duration_secs, "Warmup done");

    // Phase 2: Stress — 500 random seeks
    let mut rng = Xorshift64::new(0xABCD_EF01_2345_6789);
    for i in 0..500 {
        let pos = rng.range_f64(0.001, max_seek);
        let _ = audio.seek(Duration::from_secs_f64(pos));
        // Read a small amount — don't care about result, just exercise the pipeline
        let _ = read_with_yield_limit(&mut audio, &mut buf, 50).await;

        if i % 100 == 99 {
            yield_ms(1).await;
        }
    }
    info!("Stress phase done: 500 seeks");

    // Phase 3: Seek to 0
    let channels = audio.spec().channels as usize;
    let sample_rate = audio.spec().sample_rate;
    audio
        .seek(Duration::ZERO)
        .expect("seek to 0 must succeed after stress");

    let pos_after_seek = audio.position();
    info!(
        pos_ms = pos_after_seek.as_millis(),
        "Seeked to 0, position reported"
    );

    // Phase 4: Read from beginning and verify
    let mut total_from_zero = 0usize;
    let mut chunks_from_zero = 0usize;
    let target_chunks = 50;
    let mut position_checked = false;
    let mut continuity_errors = 0u64;
    let mut prev_last_phase: Option<usize> = None;

    for attempt in 0..target_chunks * 20 {
        let n = read_with_yield_limit(&mut audio, &mut buf, 100).await;
        if n == 0 {
            if audio.is_eof() {
                info!(chunks_from_zero, total_from_zero, "EOF reached");
                break;
            }
            if attempt > target_chunks * 10 {
                panic!(
                    "STUCK after seek-to-0: read returned 0 for {attempt} attempts, \
                     chunks_from_zero={chunks_from_zero}, \
                     position={:.3}s",
                    audio.position().as_secs_f64()
                );
            }
            continue;
        }

        chunks_from_zero += 1;
        total_from_zero += n;
        let frames = n / channels;

        // Every sample must be valid
        for (j, &sample) in buf[..n].iter().enumerate() {
            assert!(
                sample.is_finite() && (-1.0..=1.0).contains(&sample),
                "corrupt sample at offset {j} in chunk {chunks_from_zero} \
                 after seek-to-0: {sample}"
            );
        }

        // Position check: first decoded sample after seek(0) must have phase near 0.
        // Phase 0 = frame 0 of the saw-tooth.  Tolerance: 1200 frames.
        if !position_checked && frames > 0 {
            let actual_phase = phase_from_f32(buf[0]);
            // Expected phase for position 0 = 0
            let dist = phase_distance(actual_phase, 0);
            info!(
                actual_phase,
                dist, sample_rate, "Phase after seek-to-0 (expected ≈ 0)"
            );
            assert!(
                dist <= 1200,
                "after seek(0), decoded phase is {actual_phase} \
                 (distance {dist} > 1200 from expected 0) — seek landed in wrong place"
            );
            position_checked = true;
        }

        // Continuity: inter-chunk
        if let Some(prev_phase) = prev_last_phase {
            let first_phase = phase_from_f32(buf[0]);
            if first_phase != (prev_phase + 1) % SAW_PERIOD {
                continuity_errors += 1;
            }
        }

        // Continuity: intra-chunk
        if frames >= 2 {
            for f in 1..frames {
                let prev = phase_from_f32(buf[(f - 1) * channels]);
                let curr = phase_from_f32(buf[f * channels]);
                if curr != (prev + 1) % SAW_PERIOD {
                    continuity_errors += 1;
                }
            }
        }

        if frames > 0 {
            prev_last_phase = Some(phase_from_f32(buf[(frames - 1) * channels]));
        }

        if chunks_from_zero >= target_chunks {
            break;
        }
    }

    info!(
        chunks_from_zero,
        total_from_zero,
        continuity_errors,
        position_ms = audio.position().as_millis(),
        "Read after seek-to-0 complete"
    );

    assert!(
        chunks_from_zero >= target_chunks,
        "expected at least {target_chunks} chunks after seek-to-0, got {chunks_from_zero} \
         — first segments may be missing or pipeline stalled"
    );
    assert!(total_from_zero > 0, "must read samples after seek-to-0");
    assert!(
        continuity_errors <= 5,
        "{continuity_errors} continuity breaks after seek-to-0 — non-contiguous data"
    );
}

/// Full WasmPlayer lifecycle under pressure: play → rapid seeks → pause →
/// seek to 0 → play → verify data flows.
///
/// Tests the same code path the browser uses (WasmPlayer + fill_buffer).
/// Catches the bug where the browser player stops after seeking and
/// requires stop+restart to resume.
#[wasm_bindgen_test]
async fn stress_player_lifecycle_seek_pressure() {
    init().await;
    info!("Starting stress_player_lifecycle_seek_pressure");

    let mut player = kithara_wasm::WasmPlayer::new();
    let wav_info = MediaInfo::new(Some(AudioCodec::Pcm), Some(ContainerFormat::Wav));
    let load_info = kithara_wasm::load_hls_with_media_info(fixture_url().to_string(), wav_info)
        .await
        .unwrap();
    let channels = load_info[0] as u32;
    let sample_rate = load_info[1] as u32;
    let duration_ms = load_info[2];
    info!(channels, sample_rate, duration_ms, "HLS loaded");

    player.update_ring(channels, sample_rate);

    // Phase 1: Play and fill some data
    player.play();
    assert!(player.is_playing(), "should be playing after play()");

    let mut total_filled = 0u64;
    for i in 0..100 {
        let written = player.fill_buffer();
        total_filled += u64::from(written);
        if written == 0 {
            yield_ms(20).await;
        } else if i % 20 == 19 {
            yield_ms(5).await;
        }
    }
    assert!(total_filled > 0, "must fill some data during warmup");
    info!(total_filled, "Warmup fill done");

    let max_seek_ms = if duration_ms > 1000.0 {
        duration_ms - 500.0
    } else {
        duration_ms * 0.9
    };

    // Phase 2: Rapid seek+fill_buffer cycles (500 iterations)
    let mut rng = Xorshift64::new(0x1234_5678_9ABC_DEF0);
    let mut dead_fills = 0u64;

    for i in 0..500 {
        // Random seek position: 10% near start, 10% near end, 80% random
        let r = rng.next_f64();
        let seek_ms = if r < 0.1 {
            rng.range_f64(0.0, 1000.0f64.min(max_seek_ms))
        } else if r < 0.2 {
            rng.range_f64((max_seek_ms - 2000.0).max(0.0), max_seek_ms)
        } else {
            rng.range_f64(0.0, max_seek_ms)
        };

        player.seek(seek_ms);

        // Try to fill after seek — must eventually produce data
        let mut got_data = false;
        for _ in 0..50 {
            let written = player.fill_buffer();
            if written > 0 {
                got_data = true;
                total_filled += u64::from(written);
                break;
            }
            if player.is_eof() {
                got_data = true;
                break;
            }
            yield_ms(10).await;
        }

        if !got_data {
            dead_fills += 1;
            if dead_fills <= 3 {
                warn!(
                    iteration = i,
                    seek_ms,
                    is_playing = player.is_playing(),
                    is_eof = player.is_eof(),
                    "fill_buffer stuck after seek"
                );
            }
        }

        if i % 100 == 99 {
            info!(
                iteration = i + 1,
                dead_fills,
                total_filled,
                is_playing = player.is_playing(),
                "Seek stress progress"
            );
            yield_ms(1).await;
        }
    }

    info!(dead_fills, "Seek stress done: 500 seeks");

    // Phase 3: Pause → seek to 0 → play → verify fill_buffer works
    player.pause();
    assert!(!player.is_playing(), "should be paused after pause()");

    player.seek(0.0);
    player.play();
    assert!(player.is_playing(), "should be playing after play()");

    let mut post_reset_filled = 0u64;
    for i in 0..200 {
        let written = player.fill_buffer();
        post_reset_filled += u64::from(written);
        if written == 0 {
            if player.is_eof() {
                break;
            }
            yield_ms(20).await;
        } else if i % 20 == 19 {
            yield_ms(5).await;
        }
    }

    info!(
        post_reset_filled,
        position_ms = player.get_position_ms(),
        is_playing = player.is_playing(),
        "Post-reset fill complete"
    );

    assert!(
        post_reset_filled > 0,
        "fill_buffer must produce data after pause → seek(0) → play \
         (got 0 samples — pipeline stalled)"
    );

    // Position should be reasonable (not stuck at 0, not wildly ahead)
    let final_pos = player.get_position_ms();
    assert!(
        final_pos < duration_ms + 1000.0,
        "position {final_pos:.0}ms exceeds duration {duration_ms:.0}ms — position drift"
    );

    let max_dead = 500u64 / 100; // 1%
    assert!(
        dead_fills <= max_dead,
        "fill_buffer stalled {dead_fills}/500 times \
         (>{max_dead} = 1% threshold) — pipeline stuck after seek"
    );
}
