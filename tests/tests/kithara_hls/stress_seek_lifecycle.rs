//! Aggressive lifecycle stress test: rapid seeks + seek-to-zero integrity.
//!
//! Designed to catch bugs where:
//! - `read()` returns 0 after seek (player stalls, never resumes)
//! - Seek to position 0 after heavy seek activity yields no data
//!   (first segments not loaded / evicted)
//!
//! Setup: 40 segments × 3 ABR variants (ascending, descending, phase-shifted).
//! V0 segments delayed after segment 3 to trigger ABR downgrade.
//!
//! Phases:
//! 1. **Warmup**: read until ABR switch from V0→V1
//! 2. **Stress**: 2000 random seeks — verify read() always produces data
//! 3. **Reset**: seek to 0 → read entire track beginning to end,
//!    verify saw-tooth continuity on every frame

use std::{sync::Arc, time::Duration};

use kithara::assets::StoreOptions;
use kithara::audio::{Audio, AudioConfig};
use kithara::hls::{AbrMode, AbrOptions, Hls, HlsConfig};
use kithara::stream::{AudioCodec, ContainerFormat, MediaInfo, Stream};
use rstest::rstest;
use tempfile::TempDir;
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

use super::fixture::{HlsTestServer, HlsTestServerConfig};
use kithara_test_utils::Xorshift64;

const SAMPLE_RATE: u32 = 44100;
const CHANNELS: u16 = 2;
const SEGMENT_SIZE: usize = 200_000;
const SEGMENT_COUNT: usize = 40;
const VARIANT_COUNT: usize = 3;
const SAW_PERIOD: usize = 65536;
const STRESS_SEEK_ITERATIONS: usize = 2000;

/// Maximum consecutive zero-reads before declaring the pipeline stuck.
const MAX_ZERO_READS: usize = 50;

/// Ascending saw-tooth: frame 0 → -32768, frame 65535 → 32767.
fn ascending_sample(frame: usize) -> i16 {
    ((frame % SAW_PERIOD) as i32 - 32768) as i16
}

/// Descending saw-tooth: frame 0 → 32767, frame 65535 → -32768.
fn descending_sample(frame: usize) -> i16 {
    (32767 - (frame % SAW_PERIOD) as i32) as i16
}

/// Ascending saw-tooth with phase offset (distinguishes V2 from V0).
fn shifted_ascending_sample(frame: usize) -> i16 {
    (((frame + SAW_PERIOD / 2) % SAW_PERIOD) as i32 - 32768) as i16
}

/// Build WAV init segment (44-byte streaming header).
fn create_wav_init_segment() -> Vec<u8> {
    let bytes_per_sample: u16 = 2;
    let byte_rate = SAMPLE_RATE * CHANNELS as u32 * bytes_per_sample as u32;
    let block_align = CHANNELS * bytes_per_sample;
    let data_size = 0xFFFF_FFFFu32;
    let file_size = 0xFFFF_FFFFu32;

    let mut wav = Vec::with_capacity(44);
    wav.extend_from_slice(b"RIFF");
    wav.extend_from_slice(&file_size.to_le_bytes());
    wav.extend_from_slice(b"WAVE");
    wav.extend_from_slice(b"fmt ");
    wav.extend_from_slice(&16u32.to_le_bytes());
    wav.extend_from_slice(&1u16.to_le_bytes());
    wav.extend_from_slice(&CHANNELS.to_le_bytes());
    wav.extend_from_slice(&SAMPLE_RATE.to_le_bytes());
    wav.extend_from_slice(&byte_rate.to_le_bytes());
    wav.extend_from_slice(&block_align.to_le_bytes());
    wav.extend_from_slice(&(bytes_per_sample * 8).to_le_bytes());
    wav.extend_from_slice(b"data");
    wav.extend_from_slice(&data_size.to_le_bytes());
    wav
}

/// Generate PCM data for media segments (no WAV header).
fn create_pcm_segments(sample_fn: fn(usize) -> i16) -> Vec<u8> {
    let total_bytes = SEGMENT_COUNT * SEGMENT_SIZE;
    let bytes_per_frame = CHANNELS as usize * 2;
    let total_frames = total_bytes / bytes_per_frame;

    let mut pcm = Vec::with_capacity(total_bytes);
    for frame in 0..total_frames {
        let sample = sample_fn(frame);
        for _ in 0..CHANNELS {
            pcm.extend_from_slice(&sample.to_le_bytes());
        }
    }
    pcm.resize(total_bytes, 0);
    pcm
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Direction {
    Ascending,
    Descending,
    Unknown,
}

fn phase_from_f32(sample: f32) -> usize {
    let i16_val = (sample * 32768.0).round() as i32;
    ((i16_val + 32768) & 0xFFFF) as usize
}

fn detect_direction(buf: &[f32], channels: usize) -> Direction {
    let frames = buf.len() / channels;
    if frames < 2 {
        return Direction::Unknown;
    }

    let check = 10.min(frames - 1);
    let mut asc = 0u32;
    let mut desc = 0u32;

    for f in 0..check {
        let p0 = phase_from_f32(buf[f * channels]);
        let p1 = phase_from_f32(buf[(f + 1) * channels]);
        if p1 == (p0 + 1) % SAW_PERIOD {
            asc += 1;
        } else if p1 == (p0 + SAW_PERIOD - 1) % SAW_PERIOD {
            desc += 1;
        }
    }

    if asc > desc && asc > 0 {
        Direction::Ascending
    } else if desc > asc && desc > 0 {
        Direction::Descending
    } else {
        Direction::Unknown
    }
}

/// Read with retry: keeps trying until data arrives or stuck.
/// Returns (samples_read, retries_needed).
fn read_with_retry(audio: &mut Audio<Stream<Hls>>, buf: &mut [f32]) -> (usize, usize) {
    for retry in 0..MAX_ZERO_READS {
        let n = audio.read(buf);
        if n > 0 {
            return (n, retry);
        }
        if audio.is_eof() {
            return (0, retry);
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    (0, MAX_ZERO_READS)
}

/// Aggressive lifecycle stress test with 3 ABR variants, 2000 seeks,
/// and full-track integrity verification after seek-to-zero.
#[rstest]
#[case::ephemeral(true)]
#[case::mmap(false)]
#[timeout(Duration::from_secs(300))]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn stress_seek_lifecycle_with_zero_reset(#[case] ephemeral: bool) {
    let _ = tracing_subscriber::fmt()
        .with_test_writer()
        .with_max_level(tracing::Level::DEBUG)
        .with_env_filter(std::env::var("RUST_LOG").unwrap_or_else(|_| {
            "kithara_audio=debug,kithara_decode=debug,kithara_hls=debug,kithara_stream=debug"
                .to_string()
        }))
        .try_init();

    let init_segment = Arc::new(create_wav_init_segment());
    let v0_pcm = Arc::new(create_pcm_segments(ascending_sample));
    let v1_pcm = Arc::new(create_pcm_segments(descending_sample));
    let v2_pcm = Arc::new(create_pcm_segments(shifted_ascending_sample));

    let segment_duration = SEGMENT_SIZE as f64 / (SAMPLE_RATE as f64 * CHANNELS as f64 * 2.0);
    let total_secs = segment_duration * SEGMENT_COUNT as f64;

    info!(
        segments = SEGMENT_COUNT,
        variants = VARIANT_COUNT,
        segment_duration,
        total_secs = format!("{total_secs:.2}"),
        "Test data generated"
    );

    let server = HlsTestServer::new(HlsTestServerConfig {
        variant_count: VARIANT_COUNT,
        segments_per_variant: SEGMENT_COUNT,
        segment_size: SEGMENT_SIZE,
        segment_duration_secs: segment_duration,
        custom_data_per_variant: Some(vec![
            Arc::clone(&v0_pcm),
            Arc::clone(&v1_pcm),
            Arc::clone(&v2_pcm),
        ]),
        init_data_per_variant: Some(vec![
            Arc::clone(&init_segment),
            Arc::clone(&init_segment),
            Arc::clone(&init_segment),
        ]),
        variant_bandwidths: Some(vec![5_000_000, 1_000_000, 500_000]),
        segment_delay: Some(Arc::new(|variant, segment| {
            if variant == 0 && segment >= 3 {
                Duration::from_millis(500)
            } else {
                Duration::ZERO
            }
        })),
        ..Default::default()
    })
    .await;

    let url = server.url("/master.m3u8").expect("url");
    info!(%url, "HLS server ready");

    let temp_dir = TempDir::new().expect("temp dir");
    let cancel = CancellationToken::new();

    let mut store = StoreOptions::new(temp_dir.path());
    if ephemeral {
        let cap = std::num::NonZeroUsize::new(SEGMENT_COUNT * VARIANT_COUNT + 20).expect("nz");
        store.cache_capacity = Some(cap);
        store.ephemeral = true;
    }

    let hls_config = HlsConfig::new(url)
        .with_store(store)
        .with_cancel(cancel)
        .with_abr(AbrOptions {
            down_switch_buffer_secs: 0.0,
            min_buffer_for_up_switch_secs: 0.0,
            min_switch_interval: Duration::from_secs(120),
            mode: AbrMode::Auto(Some(0)),
            throughput_safety_factor: 1.0,
            ..AbrOptions::default()
        });

    let wav_info = MediaInfo::new(Some(AudioCodec::Pcm), Some(ContainerFormat::Wav));
    let config = AudioConfig::<Hls>::new(hls_config).with_media_info(wav_info);
    let mut audio = Audio::<Stream<Hls>>::new(config)
        .await
        .expect("create Audio pipeline");

    let spec = audio.spec();
    info!(
        sample_rate = spec.sample_rate,
        channels = spec.channels,
        "Audio pipeline created"
    );

    let result = tokio::task::spawn_blocking(move || {
        let channels = spec.channels as usize;
        let chunk_samples = (0.05 * spec.sample_rate as f64 * channels as f64) as usize;
        let mut buf = vec![0.0f32; chunk_samples];
        let mut rng = Xorshift64::new(0xCAFE_BABE_DEAD_BEEF);

        // ── Phase 1: Warmup until ABR switch ─────────────────────────
        info!("Phase 1: warmup — reading until ABR switch");
        let mut initial_direction = Direction::Unknown;
        let mut switch_detected = false;
        let warmup_deadline = std::time::Instant::now() + Duration::from_secs(60);

        while std::time::Instant::now() < warmup_deadline {
            let (n, _) = read_with_retry(&mut audio, &mut buf);
            if n == 0 {
                break;
            }
            let dir = detect_direction(&buf[..n], channels);
            if initial_direction == Direction::Unknown && dir != Direction::Unknown {
                initial_direction = dir;
                info!(?dir, "Initial direction detected");
            }
            if initial_direction != Direction::Unknown
                && dir != Direction::Unknown
                && dir != initial_direction
            {
                info!(
                    from = ?initial_direction,
                    to = ?dir,
                    "ABR switch detected"
                );
                switch_detected = true;
                break;
            }
        }

        if !switch_detected {
            warn!("ABR switch not detected during warmup — continuing anyway");
        }

        // ── Phase 2: 2000 rapid random seeks ─────────────────────────
        info!("Phase 2: {STRESS_SEEK_ITERATIONS} rapid random seeks");
        let max_seek_secs = total_secs - 0.1;
        let mut dead_seeks = 0u64;
        let mut total_retries = 0u64;
        let mut max_retries_single = 0usize;
        let mut integrity_errors = 0u64;
        let mut channel_mismatches = 0u64;

        for i in 0..STRESS_SEEK_ITERATIONS {
            // Mix of random positions: 10% chance to seek near start (< 1s),
            // 10% chance to seek near end (last 2s), 80% random.
            let r = rng.next_f64();
            let pos_secs = if r < 0.1 {
                rng.range_f64(0.0, 1.0)
            } else if r < 0.2 {
                rng.range_f64(max_seek_secs - 2.0, max_seek_secs)
            } else {
                rng.range_f64(0.001, max_seek_secs)
            };

            let position = Duration::from_secs_f64(pos_secs);

            if let Err(e) = audio.seek(position) {
                warn!(iteration = i, pos_secs, ?e, "seek failed");
                dead_seeks += 1;
                continue;
            }

            let (n, retries) = read_with_retry(&mut audio, &mut buf);
            total_retries += retries as u64;
            if retries > max_retries_single {
                max_retries_single = retries;
            }

            if n == 0 {
                dead_seeks += 1;
                if dead_seeks <= 5 {
                    warn!(
                        iteration = i,
                        pos_secs,
                        is_eof = audio.is_eof(),
                        retries,
                        "STUCK: read returned 0 after {MAX_ZERO_READS} retries"
                    );
                }
                continue;
            }

            // Integrity check: finite, in range
            for (j, &sample) in buf[..n].iter().enumerate() {
                if !sample.is_finite() || !(-1.0..=1.0).contains(&sample) {
                    integrity_errors += 1;
                    if integrity_errors <= 3 {
                        warn!(iteration = i, offset = j, sample, pos_secs, "bad sample");
                    }
                    break;
                }
            }

            // L == R check
            if channels == 2 {
                let frames = n / channels;
                for f in 0..frames {
                    let l = buf[f * 2];
                    let r_val = buf[f * 2 + 1];
                    if (l - r_val).abs() > f32::EPSILON {
                        channel_mismatches += 1;
                        break;
                    }
                }
            }

            if (i + 1) % 500 == 0 {
                info!(
                    iteration = i + 1,
                    dead_seeks, total_retries, max_retries_single, integrity_errors, "Progress"
                );
            }
        }

        info!(
            dead_seeks,
            total_retries,
            max_retries_single,
            integrity_errors,
            channel_mismatches,
            "Phase 2 complete"
        );

        // Tolerate a small number of dead seeks (decoder restart race)
        // but not more than 1% of total iterations.
        let max_dead = (STRESS_SEEK_ITERATIONS as u64) / 100;
        assert!(
            dead_seeks <= max_dead,
            "too many dead seeks: {dead_seeks}/{STRESS_SEEK_ITERATIONS} \
             (>{max_dead} = 1% threshold) — pipeline stalls after seek"
        );
        assert_eq!(
            integrity_errors, 0,
            "integrity errors: samples outside [-1,1] or not finite"
        );
        assert_eq!(
            channel_mismatches, 0,
            "L/R channel mismatches — data corruption"
        );

        // ── Phase 3: Seek to 0 → full track read with continuity check ──
        info!("Phase 3: seek to 0 — full track integrity verification");

        audio.seek(Duration::ZERO).expect("seek to 0 must succeed");

        let mut total_frames_read = 0u64;
        let mut continuity_breaks = 0u64;
        let mut prev_phase: Option<usize> = None;
        let mut read_attempts = 0u64;
        let max_read_attempts = 100_000u64;

        loop {
            let (n, retries) = read_with_retry(&mut audio, &mut buf);
            read_attempts += 1;

            if n == 0 {
                if audio.is_eof() {
                    break;
                }
                if retries >= MAX_ZERO_READS {
                    panic!(
                        "STUCK at position {:.3}s after seek to 0: \
                         read returned 0 after {MAX_ZERO_READS} retries, \
                         total_frames_read={total_frames_read}, is_eof={}",
                        audio.position().as_secs_f64(),
                        audio.is_eof()
                    );
                }
                continue;
            }

            let frames = n / channels;

            // Integrity: every sample must be finite and in range
            for (j, &sample) in buf[..n].iter().enumerate() {
                assert!(
                    sample.is_finite() && (-1.0..=1.0).contains(&sample),
                    "invalid sample at frame {} (total_frames_read={total_frames_read}): {sample}",
                    total_frames_read + (j / channels) as u64,
                );
            }

            // L == R
            if channels == 2 {
                for f in 0..frames {
                    let l = buf[f * 2];
                    let r_val = buf[f * 2 + 1];
                    assert!(
                        (l - r_val).abs() <= f32::EPSILON,
                        "L/R mismatch at frame {}: L={l}, R={r_val}",
                        total_frames_read + f as u64,
                    );
                }
            }

            // Continuity: check inter-chunk boundary (prev_phase → first frame)
            let first_phase = phase_from_f32(buf[0]);
            if let Some(pp) = prev_phase {
                // After seek to 0, we're reading the post-ABR-switch variant
                // (ascending or descending). Check both directions.
                let next_asc = (pp + 1) % SAW_PERIOD;
                let next_desc = (pp + SAW_PERIOD - 1) % SAW_PERIOD;
                if first_phase != next_asc && first_phase != next_desc {
                    continuity_breaks += 1;
                    if continuity_breaks <= 5 {
                        info!(
                            frame = total_frames_read,
                            prev_phase = pp,
                            first_phase,
                            expected_asc = next_asc,
                            expected_desc = next_desc,
                            "inter-chunk continuity break"
                        );
                    }
                }
            }

            // Intra-chunk continuity
            for f in 1..frames {
                let p0 = phase_from_f32(buf[(f - 1) * channels]);
                let p1 = phase_from_f32(buf[f * channels]);
                let next_asc = (p0 + 1) % SAW_PERIOD;
                let next_desc = (p0 + SAW_PERIOD - 1) % SAW_PERIOD;
                if p1 != next_asc && p1 != next_desc {
                    continuity_breaks += 1;
                    if continuity_breaks <= 5 {
                        info!(
                            frame = total_frames_read + f as u64,
                            p0, p1, "intra-chunk continuity break"
                        );
                    }
                }
            }

            // Track last phase for inter-chunk check
            let last_frame_phase = phase_from_f32(buf[(frames - 1) * channels]);
            prev_phase = Some(last_frame_phase);

            total_frames_read += frames as u64;

            if read_attempts > max_read_attempts {
                panic!(
                    "exceeded {max_read_attempts} read attempts in phase 3, \
                     total_frames_read={total_frames_read} — possible infinite loop"
                );
            }
        }

        assert!(audio.is_eof(), "expected EOF after full track read");

        let expected_frames = (SEGMENT_COUNT * SEGMENT_SIZE) / (CHANNELS as usize * 2);
        let frame_diff = total_frames_read.abs_diff(expected_frames as u64);
        let tolerance = (expected_frames as u64) / 50; // 2%

        info!(
            total_frames_read,
            expected_frames, frame_diff, tolerance, continuity_breaks, "Phase 3 complete"
        );

        assert!(
            frame_diff <= tolerance,
            "frame count mismatch after seek-to-0: \
             got {total_frames_read}, expected ~{expected_frames} (±{tolerance})"
        );

        // Allow a few continuity breaks at segment/decoder boundaries,
        // but not proportional to track length.
        let max_breaks = 10u64;
        assert!(
            continuity_breaks <= max_breaks,
            "too many continuity breaks after seek-to-0: \
             {continuity_breaks} (>{max_breaks} tolerance) — data corruption or segment gap"
        );

        info!("All phases passed");
    })
    .await;

    match result {
        Ok(()) => info!("Lifecycle stress test passed"),
        Err(e) if e.is_panic() => std::panic::resume_unwind(e.into_panic()),
        Err(e) => panic!("spawn_blocking failed: {e}"),
    }
}
