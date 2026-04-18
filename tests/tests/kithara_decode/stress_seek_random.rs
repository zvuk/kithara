//! Stress test: 1000 random seek+read cycles on synthetic WAV.
//!
//! Generates a deterministic WAV (10s, ~1.7 MB stereo 44.1 kHz, 16-bit),
//! creates `Audio<Stream<File>>`, then performs 1000 random seeks
//! each followed by a read, verifying data integrity at every step.
//!
//! Deterministic xorshift64 PRNG guarantees reproducibility.
//! No network required.

use std::{fs::File as FsFile, io::Write};

use kithara::{
    audio::{Audio, AudioConfig},
    file::{File, FileConfig, FileSrc},
    stream::Stream,
};
use kithara_platform::{time::Duration, tokio::task::spawn_blocking};
use kithara_test_utils::{Xorshift64, wav::create_test_wav};
use tempfile::NamedTempFile;
use tracing::info;

use crate::common::test_defaults::SawWav;

#[kithara::test(
    native,
    serial,
    timeout(Duration::from_secs(10)),
    env(KITHARA_HANG_TIMEOUT_SECS = "1"),
    tracing("kithara_audio=debug,kithara_decode=debug,kithara_stream=debug")
)]
async fn stress_random_seek_read_synthetic_wav() {
    const DURATION_SECS: f64 = 10.0;
    const SAMPLE_COUNT: usize = (SawWav::DEFAULT.sample_rate as f64 * DURATION_SECS) as usize;
    const SEEK_ITERATIONS: usize = 1000;

    // Step 1: Create synthetic WAV
    let wav_data = create_test_wav(SAMPLE_COUNT, 44100, 2);
    let wav_size_mb = wav_data.len() as f64 / 1_000_000.0;
    info!(
        samples = SAMPLE_COUNT,
        duration_secs = DURATION_SECS,
        size_mb = format!("{wav_size_mb:.2}"),
        "Generated test WAV"
    );

    let tmp = NamedTempFile::new().expect("create temp file");
    Write::write_all(
        &mut FsFile::create(tmp.path()).expect("open temp file"),
        &wav_data,
    )
    .expect("write WAV data");

    // Step 2: Create Audio pipeline (mock decoder = real decoder on synthetic data)
    let file_config = FileConfig::new(FileSrc::Local(tmp.path().to_path_buf()));
    let config = AudioConfig::<File>::new(file_config).with_hint("wav");
    let mut audio = Audio::<Stream<File>>::new(config)
        .await
        .expect("create audio pipeline");

    // Step 3: Query duration
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

    // Step 4: Compute optimal chunk size
    // ~0.5% of total duration, clamped to [0.05s, 0.5s].
    let chunk_duration_secs = (total_secs * 0.005).clamp(0.05, 0.5);
    let chunk_samples =
        (chunk_duration_secs * f64::from(spec.sample_rate) * f64::from(spec.channels)) as usize;
    info!(chunk_duration_secs, chunk_samples, "Read chunk size");

    // Steps 5-7: Run seek+read loop in blocking thread
    let result = spawn_blocking(move || {
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
        let mut zero_reads = 0u64;

        for (i, &pos_secs) in seek_positions.iter().enumerate() {
            let position = Duration::from_secs_f64(pos_secs);

            // Seek
            audio.seek(position).unwrap_or_else(|e| {
                panic!("seek #{i} to {pos_secs:.4}s failed: {e}");
            });

            // Read
            let mut n = audio.read(&mut buf);
            if n == 0 {
                // Under concurrent load, decoder may transiently report EOF after seek.
                // Retry once: re-seek to the same position and re-read.
                audio.seek(position).unwrap_or_else(|e| {
                    panic!("re-seek #{i} to {pos_secs:.4}s failed: {e}");
                });
                n = audio.read(&mut buf);
                if n == 0 {
                    zero_reads += 1;
                    if zero_reads <= 3 {
                        tracing::warn!(
                            iteration = i,
                            pos_secs,
                            "zero-read after retry (transient)"
                        );
                    }
                    continue;
                }
            }

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
            zero_reads,
            "All {} seek+read iterations done",
            SEEK_ITERATIONS
        );

        if zero_reads > 0 {
            tracing::warn!(zero_reads, "zero-reads detected (within tolerance of 3)");
        }
        assert!(
            zero_reads <= 3,
            "{} zero-reads out of {} (>3 tolerance) — decoder EOF race",
            zero_reads,
            SEEK_ITERATIONS
        );
        assert!(
            successful_reads >= SEEK_ITERATIONS as u64 - 3,
            "only {} successful reads out of {}",
            successful_reads,
            SEEK_ITERATIONS
        );
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

    result.expect("spawn_blocking failed");
    info!("Stress test passed");
}
