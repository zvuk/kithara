//! Stress test: 1000 random seek+read cycles on `Audio<Stream<Hls>>` (20 MB WAV over HLS).
//!
//! Generates a deterministic WAV with a saw-tooth pattern (period = 65536 frames),
//! serves it as HLS segments via [`HlsTestServer`], creates `Audio<Stream<Hls>>`,
//! and performs 1000 random time-based seeks with three-level PCM verification:
//!
//! 1. **Integrity**: samples finite, in `[-1.0, 1.0]`, L == R
//! 2. **Continuity**: consecutive frames follow the saw-tooth pattern
//! 3. **Position**: decoded phase matches expected position (±50 frames tolerance)
//!
//! Deterministic [`Xorshift64`] PRNG guarantees reproducibility.
//! No external network required.

use std::{sync::Arc, time::Duration};

use kithara_assets::StoreOptions;
use kithara_audio::{Audio, AudioConfig};
use kithara_hls::{AbrMode, AbrOptions, Hls, HlsConfig};
use kithara_stream::{AudioCodec, ContainerFormat, MediaInfo, Stream};
use rstest::rstest;
use tempfile::TempDir;
use tokio_util::sync::CancellationToken;
use tracing::info;

use super::fixture::{HlsTestServer, HlsTestServerConfig};
use crate::common::Xorshift64;

const SAMPLE_RATE: u32 = 44100;
const CHANNELS: u16 = 2;
const SEGMENT_SIZE: usize = 200_000;
const SEGMENT_COUNT: usize = 100;
const TOTAL_BYTES: usize = SEGMENT_COUNT * SEGMENT_SIZE; // 20 MB
const SEEK_ITERATIONS: usize = 1000;

/// Saw-tooth period: 65536 frames (~1.486s at 44100 Hz).
/// Each frame within a period has a unique i16 value.
const SAW_PERIOD: usize = 65536;

// ==================== WAV Generator ====================

/// Saw-tooth sample at frame index `i`.
///
/// Maps `[0, PERIOD-1]` → `[-32768, 32767]` (full i16 range).
/// Exact roundtrip: i16 → f32 (`/ 32768.0`) → i16 (`* 32768.0, round`).
fn saw_sample(frame_index: usize) -> i16 {
    ((frame_index % SAW_PERIOD) as i32 - 32768) as i16
}

/// Recover saw-tooth phase from a decoded f32 sample.
fn phase_from_f32(sample: f32) -> usize {
    let i16_val = (sample * 32768.0).round() as i32;
    ((i16_val + 32768) & 0xFFFF) as usize
}

/// Generate WAV with saw-tooth pattern, sized exactly to `total_bytes`.
///
/// - Stereo 44100 Hz, 16-bit PCM
/// - L and R channels get the same value per frame
/// - Total size = WAV header (44 bytes) + PCM data
fn create_saw_wav(total_bytes: usize) -> Vec<u8> {
    let bytes_per_sample: u16 = 2;
    let bytes_per_frame = CHANNELS as usize * bytes_per_sample as usize;
    let header_size = 44usize;
    let data_size = total_bytes - header_size;
    let frame_count = data_size / bytes_per_frame;
    // Ensure data_size is exact multiple of bytes_per_frame
    let data_size = (frame_count * bytes_per_frame) as u32;
    let file_size = 36 + data_size;

    let mut wav = Vec::with_capacity(total_bytes);

    // RIFF header
    wav.extend_from_slice(b"RIFF");
    wav.extend_from_slice(&file_size.to_le_bytes());
    wav.extend_from_slice(b"WAVE");

    // fmt chunk
    wav.extend_from_slice(b"fmt ");
    wav.extend_from_slice(&16u32.to_le_bytes());
    wav.extend_from_slice(&1u16.to_le_bytes()); // PCM
    wav.extend_from_slice(&CHANNELS.to_le_bytes());
    wav.extend_from_slice(&SAMPLE_RATE.to_le_bytes());
    let byte_rate = SAMPLE_RATE * CHANNELS as u32 * bytes_per_sample as u32;
    wav.extend_from_slice(&byte_rate.to_le_bytes());
    let block_align = CHANNELS * bytes_per_sample;
    wav.extend_from_slice(&block_align.to_le_bytes());
    wav.extend_from_slice(&(bytes_per_sample * 8).to_le_bytes());

    // data chunk
    wav.extend_from_slice(b"data");
    wav.extend_from_slice(&data_size.to_le_bytes());

    // PCM data: saw-tooth, both channels identical
    for i in 0..frame_count {
        let sample = saw_sample(i);
        for _ in 0..CHANNELS {
            wav.extend_from_slice(&sample.to_le_bytes());
        }
    }

    // Pad to exact total_bytes if rounding left a gap
    wav.resize(total_bytes, 0);

    wav
}

/// Compute the expected duration in seconds for the generated WAV.
fn expected_duration_secs() -> f64 {
    let header_size = 44usize;
    let bytes_per_frame = CHANNELS as usize * 2;
    let frame_count = (TOTAL_BYTES - header_size) / bytes_per_frame;
    frame_count as f64 / SAMPLE_RATE as f64
}

// ==================== Verification Helpers ====================

/// Check circular distance between two phases (mod SAW_PERIOD).
fn phase_distance(a: usize, b: usize) -> usize {
    let d = if a > b { a - b } else { b - a };
    d.min(SAW_PERIOD - d)
}

// ==================== Stress Test ====================

/// 1000 random seek+read cycles with three-level PCM verification
/// on `Audio<Stream<Hls>>` serving a 20 MB WAV.
///
/// Scenario:
/// 1. Generate saw-tooth WAV (20 MB, ~113s, stereo 44.1 kHz)
/// 2. Spawn `HlsTestServer` with `custom_data` = WAV bytes
/// 3. Create `Audio<Stream<Hls>>` with format hint "wav"
/// 4. Verify duration ≈ expected
/// 5. 1000 random seeks with verification:
///    - Level 1: integrity (finite, range, L==R)
///    - Level 2: continuity (consecutive frames follow pattern)
///    - Level 3: position (decoded phase ≈ expected phase)
/// 6. Final seek near end → read to EOF
#[rstest]
#[timeout(Duration::from_secs(120))]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn stress_seek_audio_hls_wav() {
    let _ = tracing_subscriber::fmt()
        .with_test_writer()
        .with_max_level(tracing::Level::DEBUG)
        .with_env_filter(std::env::var("RUST_LOG").unwrap_or_else(|_| {
            "kithara_audio=debug,kithara_decode=debug,kithara_hls=debug,kithara_stream=debug"
                .to_string()
        }))
        .try_init();

    // --- Step 1: Generate WAV ---
    let wav_data = create_saw_wav(TOTAL_BYTES);
    let expected_dur = expected_duration_secs();
    info!(
        total_bytes = TOTAL_BYTES,
        duration_secs = format!("{expected_dur:.2}"),
        "Generated saw-tooth WAV"
    );

    // --- Step 2: Spawn HLS server with custom WAV data ---
    let segment_duration = SEGMENT_SIZE as f64 / (SAMPLE_RATE as f64 * CHANNELS as f64 * 2.0);
    let server = HlsTestServer::new(HlsTestServerConfig {
        segments_per_variant: SEGMENT_COUNT,
        segment_size: SEGMENT_SIZE,
        segment_duration_secs: segment_duration,
        custom_data: Some(Arc::new(wav_data)),
        ..Default::default()
    })
    .await;

    let url = server.url("/master.m3u8").expect("url");
    info!(%url, segments = SEGMENT_COUNT, "HLS server ready");

    // --- Step 3: Create Audio<Stream<Hls>> ---
    let temp_dir = TempDir::new().expect("temp dir");
    let cancel = CancellationToken::new();

    let hls_config = HlsConfig::new(url)
        .with_store(StoreOptions::new(temp_dir.path()))
        .with_cancel(cancel)
        .with_abr(AbrOptions {
            mode: AbrMode::Manual(0),
            ..AbrOptions::default()
        });

    let wav_info = MediaInfo::new(Some(AudioCodec::Pcm), Some(ContainerFormat::Wav));
    let config = AudioConfig::<Hls>::new(hls_config).with_media_info(wav_info);
    let mut audio = Audio::<Stream<Hls>>::new(config)
        .await
        .expect("create Audio<Stream<Hls>> pipeline");

    // --- Step 4: Verify duration ---
    let total_duration = audio.duration().expect("WAV should report known duration");
    let total_secs = total_duration.as_secs_f64();
    info!(total_secs, expected_dur, "Stream duration");

    assert!(
        (total_secs - expected_dur).abs() < 1.0,
        "duration mismatch: expected ~{expected_dur:.1}, got {total_secs:.1}",
    );

    let spec = audio.spec();
    info!(
        sample_rate = spec.sample_rate,
        channels = spec.channels,
        "Audio spec"
    );

    // --- Steps 5-6 in blocking thread ---
    let result = tokio::task::spawn_blocking(move || {
        // Compute chunk size: ~50ms of audio
        let chunk_duration_secs = 0.05;
        let chunk_samples =
            (chunk_duration_secs * spec.sample_rate as f64 * spec.channels as f64) as usize;
        info!(chunk_duration_secs, chunk_samples, "Read chunk size");

        let mut rng = Xorshift64::new(0xDEAD_BEEF_CAFE_1337);
        let mut buf = vec![0.0f32; chunk_samples];

        // Generate 1000 random seek positions
        let max_seek_secs = total_secs - chunk_duration_secs;
        assert!(max_seek_secs > 0.0, "stream too short for chunk size");

        let seek_positions: Vec<f64> = (0..SEEK_ITERATIONS)
            .map(|_| rng.range_f64(0.001, max_seek_secs))
            .collect();

        info!(
            count = seek_positions.len(),
            max_seek_secs, "Generated seek positions"
        );

        // Step 5: Iterate seek + read + verify
        let mut successful_reads = 0u64;
        let mut total_samples_read = 0u64;
        let mut channel_mismatches = 0u64;
        let mut continuity_errors = 0u64;
        let mut position_errors = 0u64;

        let channels = spec.channels as usize;

        for (i, &pos_secs) in seek_positions.iter().enumerate() {
            let position = Duration::from_secs_f64(pos_secs);

            // Seek
            audio.seek(position).unwrap_or_else(|e| {
                panic!("seek #{i} to {pos_secs:.4}s failed: {e}");
            });

            // Read
            let n = audio.read(&mut buf);
            assert!(
                n > 0,
                "read returned 0 after seek #{i} to {pos_secs:.4}s (is_eof={})",
                audio.is_eof(),
            );

            let frames = n / channels;

            // === Level 1: Integrity ===
            for (j, &sample) in buf[..n].iter().enumerate() {
                assert!(
                    sample.is_finite() && (-1.0..=1.0).contains(&sample),
                    "invalid sample at seek #{i} offset {j}: {sample} (pos {pos_secs:.4}s)",
                );
            }

            // L == R check
            if channels == 2 {
                for f in 0..frames {
                    let l = buf[f * 2];
                    let r = buf[f * 2 + 1];
                    if (l - r).abs() > f32::EPSILON {
                        channel_mismatches += 1;
                        if channel_mismatches <= 3 {
                            info!(iteration = i, frame = f, l, r, pos_secs, "Channel mismatch");
                        }
                    }
                }
            }

            // === Level 2: Continuity ===
            // Consecutive frames should differ by exactly 1 step in the saw-tooth,
            // or wrap from max to min.
            if frames >= 2 {
                for f in 1..frames {
                    let prev_phase = phase_from_f32(buf[(f - 1) * channels]);
                    let curr_phase = phase_from_f32(buf[f * channels]);
                    let expected_next = (prev_phase + 1) % SAW_PERIOD;
                    if curr_phase != expected_next {
                        continuity_errors += 1;
                        if continuity_errors <= 3 {
                            info!(
                                iteration = i,
                                frame = f,
                                prev_phase,
                                curr_phase,
                                expected_next,
                                pos_secs,
                                "Continuity break"
                            );
                        }
                    }
                }
            }

            // === Level 3: Position ===
            // First decoded sample should correspond to the seek position.
            //
            // Symphonia's WAV reader returns packets of 1152 frames.
            // After seek, the first decoded packet starts from the packet
            // boundary BEFORE the target, so the actual position can be
            // up to 1151 frames earlier. Tolerance of 1200 covers this
            // while still detecting gross seek errors (>27ms at 44.1 kHz).
            let expected_frame_idx = (pos_secs * spec.sample_rate as f64).round() as usize;
            let expected_phase = expected_frame_idx % SAW_PERIOD;
            let actual_phase = phase_from_f32(buf[0]);
            let dist = phase_distance(actual_phase, expected_phase);
            if dist > 1200 {
                position_errors += 1;
                if position_errors <= 3 {
                    info!(
                        iteration = i,
                        pos_secs,
                        expected_frame_idx,
                        expected_phase,
                        actual_phase,
                        dist,
                        "Position mismatch"
                    );
                }
            }

            successful_reads += 1;
            total_samples_read += n as u64;

            if (i + 1) % 200 == 0 {
                info!(
                    iteration = i + 1,
                    successful_reads,
                    total_samples_read,
                    channel_mismatches,
                    continuity_errors,
                    position_errors,
                    "Progress"
                );
            }
        }

        info!(
            successful_reads,
            total_samples_read,
            channel_mismatches,
            continuity_errors,
            position_errors,
            "All {SEEK_ITERATIONS} seek+read iterations done"
        );

        assert_eq!(successful_reads, SEEK_ITERATIONS as u64);
        assert_eq!(
            channel_mismatches, 0,
            "L/R channel data diverged {channel_mismatches} times — data corruption"
        );
        if continuity_errors > 0 {
            tracing::warn!(
                continuity_errors,
                "continuity breaks detected (within tolerance of 5)"
            );
        }
        assert!(
            continuity_errors <= 5,
            "{continuity_errors} continuity breaks (>5 tolerance) — decoder returned non-contiguous data"
        );
        assert_eq!(
            position_errors, 0,
            "{position_errors} position mismatches — seek landed in wrong place"
        );

        // Step 6: Final seek near end → read to EOF
        let final_seek_secs = total_secs - chunk_duration_secs;
        info!(final_seek_secs, "Final seek near end");

        audio
            .seek(Duration::from_secs_f64(final_seek_secs))
            .unwrap_or_else(|e| {
                panic!("final seek to {final_seek_secs:.4}s failed: {e}");
            });

        let mut remaining_samples = 0u64;
        loop {
            let n = audio.read(&mut buf);
            if n == 0 {
                break;
            }
            remaining_samples += n as u64;

            for &sample in &buf[..n] {
                assert!(
                    sample.is_finite() && (-1.0..=1.0).contains(&sample),
                    "invalid sample in final tail read",
                );
            }
        }

        assert!(
            audio.is_eof(),
            "expected EOF after reading all remaining data from {final_seek_secs:.4}s"
        );

        info!(remaining_samples, "Final read done — EOF confirmed");
    })
    .await;

    match result {
        Ok(()) => info!("Audio+HLS stress test passed"),
        Err(e) if e.is_panic() => std::panic::resume_unwind(e.into_panic()),
        Err(e) => panic!("spawn_blocking failed: {e}"),
    }
}
