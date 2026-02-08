//! Stress test: 1000 random seek+read cycles on synthetic WAV.
//!
//! Generates a deterministic WAV (10s, ~1.7 MB stereo 44.1 kHz, 16-bit),
//! creates `Audio<Stream<File>>`, then performs 1000 random seeks
//! each followed by a read, verifying data integrity at every step.
//!
//! Deterministic xorshift64 PRNG guarantees reproducibility.
//! No network required.

use std::time::Duration;

use kithara_audio::{Audio, AudioConfig};
use kithara_file::{File, FileConfig, FileSrc};
use kithara_stream::Stream;
use rstest::rstest;
use tracing::info;

use crate::common::Xorshift64;

const SAMPLE_RATE: u32 = 44100;
const CHANNELS: u16 = 2;
const DURATION_SECS: f64 = 10.0;
const SAMPLE_COUNT: usize = (SAMPLE_RATE as f64 * DURATION_SECS) as usize;
const SEEK_ITERATIONS: usize = 1000;

// ==================== WAV Generator ====================

/// Generate WAV with deterministic sine pattern.
///
/// Sample at index `i` = `sin(i * 0.1) * 32767` truncated to `i16`.
/// Both L and R channels get the same value → we verify this in the test.
fn create_stress_wav(sample_count: usize) -> Vec<u8> {
    let bytes_per_sample: u16 = 2;
    let data_size = (sample_count * CHANNELS as usize * bytes_per_sample as usize) as u32;
    let file_size = 36 + data_size;

    let mut wav = Vec::with_capacity(file_size as usize + 8);

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

    // Deterministic samples
    for i in 0..sample_count {
        let sample = ((i as f32 * 0.1).sin() * 32767.0) as i16;
        for _ in 0..CHANNELS {
            wav.extend_from_slice(&sample.to_le_bytes());
        }
    }

    wav
}

// ==================== Stress Test ====================

/// 1000 random seek+read cycles with data verification.
///
/// Scenario:
/// 1. Generate synthetic WAV (N MB, X seconds)
/// 2. Create `Audio<Stream<File>>` (local decoder pipeline)
/// 3. Query duration in seconds
/// 4. Compute optimal random chunk size (proportional to stream, capped)
/// 5. Sample 1000 random seek positions in `(0, duration - chunk_duration)`
/// 6. For each: seek → read → verify data (valid range, L==R channels)
/// 7. Final: seek to `duration - chunk_duration`, read all → verify EOF
#[rstest]
#[timeout(Duration::from_secs(120))]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn stress_random_seek_read_synthetic_wav() {
    let _ = tracing_subscriber::fmt()
        .with_test_writer()
        .with_max_level(tracing::Level::DEBUG)
        .with_env_filter(std::env::var("RUST_LOG").unwrap_or_else(|_| {
            "kithara_audio=debug,kithara_decode=debug,kithara_stream=debug".to_string()
        }))
        .try_init();

    // --- Step 1: Create synthetic WAV ---
    let wav_data = create_stress_wav(SAMPLE_COUNT);
    let wav_size_mb = wav_data.len() as f64 / 1_000_000.0;
    info!(
        samples = SAMPLE_COUNT,
        duration_secs = DURATION_SECS,
        size_mb = format!("{wav_size_mb:.2}"),
        "Generated test WAV"
    );

    let tmp = tempfile::NamedTempFile::new().expect("create temp file");
    std::io::Write::write_all(
        &mut std::fs::File::create(tmp.path()).expect("open temp file"),
        &wav_data,
    )
    .expect("write WAV data");

    // --- Step 2: Create Audio pipeline (mock decoder = real decoder on synthetic data) ---
    let file_config = FileConfig::new(FileSrc::Local(tmp.path().to_path_buf()));
    let config = AudioConfig::<File>::new(file_config).with_hint("wav");
    let mut audio = Audio::<Stream<File>>::new(config)
        .await
        .expect("create audio pipeline");

    // --- Step 3: Query duration ---
    let total_duration = audio.duration().expect("WAV should report known duration");
    let total_secs = total_duration.as_secs_f64();
    info!(total_secs, "Stream duration");

    assert!(
        (total_secs - DURATION_SECS).abs() < 0.1,
        "duration mismatch: expected ~{DURATION_SECS}, got {total_secs}",
    );

    let spec = audio.spec();
    info!(
        sample_rate = spec.sample_rate,
        channels = spec.channels,
        "Audio spec"
    );

    // --- Step 4: Compute optimal chunk size ---
    // ~0.5% of total duration, clamped to [0.05s, 0.5s].
    let chunk_duration_secs = (total_secs * 0.005).clamp(0.05, 0.5);
    let chunk_samples =
        (chunk_duration_secs * spec.sample_rate as f64 * spec.channels as f64) as usize;
    info!(chunk_duration_secs, chunk_samples, "Read chunk size");

    // --- Steps 5-7: Run seek+read loop in blocking thread ---
    let result = tokio::task::spawn_blocking(move || {
        let mut rng = Xorshift64::new(0xDEAD_BEEF_CAFE_1337);
        let mut buf = vec![0.0f32; chunk_samples];

        // Step 5: Generate 1000 random seek positions > 0, < duration - chunk
        let max_seek_secs = total_secs - chunk_duration_secs;
        assert!(max_seek_secs > 0.0, "stream too short for chunk size");

        let seek_positions: Vec<f64> = (0..SEEK_ITERATIONS)
            .map(|_| rng.range_f64(0.001, max_seek_secs))
            .collect();

        info!(
            count = seek_positions.len(),
            max_seek_secs, "Generated seek positions"
        );

        // Step 6: Iterate seek + read + verify
        let mut successful_reads = 0u64;
        let mut total_samples_read = 0u64;
        let mut channel_mismatches = 0u64;

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

            // Verify: all samples finite and in [-1.0, 1.0]
            for (j, &sample) in buf[..n].iter().enumerate() {
                assert!(
                    sample.is_finite() && (-1.0..=1.0).contains(&sample),
                    "invalid sample at seek #{i} offset {j}: {sample} (pos {pos_secs:.4}s)",
                );
            }

            // Verify: L and R channels match (both generated from same sine value)
            let channels = spec.channels as usize;
            if channels == 2 {
                let frames = n / channels;
                for f in 0..frames {
                    let l = buf[f * 2];
                    let r = buf[f * 2 + 1];
                    if (l - r).abs() > f32::EPSILON {
                        channel_mismatches += 1;
                    }
                }
            }

            successful_reads += 1;
            total_samples_read += n as u64;

            if (i + 1) % 200 == 0 {
                info!(
                    iteration = i + 1,
                    successful_reads, total_samples_read, channel_mismatches, "Progress"
                );
            }
        }

        info!(
            successful_reads,
            total_samples_read,
            channel_mismatches,
            "All {SEEK_ITERATIONS} seek+read iterations done"
        );

        assert_eq!(successful_reads, SEEK_ITERATIONS as u64);
        assert_eq!(
            channel_mismatches, 0,
            "L/R channel data diverged {channel_mismatches} times — data corruption"
        );

        // Step 7: Final seek near end → read all → verify EOF
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
        Ok(()) => info!("Stress test passed"),
        Err(e) if e.is_panic() => std::panic::resume_unwind(e.into_panic()),
        Err(e) => panic!("spawn_blocking failed: {e}"),
    }
}
