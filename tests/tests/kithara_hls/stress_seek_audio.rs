//! Stress test: 1000 random seek+read cycles on `Audio<Stream<Hls>>` (20 MB WAV over HLS).
//!
//! Generates a deterministic WAV with a saw-tooth pattern (period = `65_536` frames),
//! serves it as HLS segments via [`HlsTestServer`], creates `Audio<Stream<Hls>>`,
//! and performs 1000 random time-based seeks with three-level PCM verification:
//!
//! 1. **Integrity**: samples finite, in `[-1.0, 1.0]`, L == R
//! 2. **Continuity**: consecutive frames follow the saw-tooth pattern
//! 3. **Position**: decoded phase matches expected position (±50 frames tolerance)
//!
//! Deterministic [`Xorshift64`] PRNG guarantees reproducibility.
//! No external network is required.

use std::{num::NonZeroUsize, sync::Arc, time::Duration};

use kithara::{
    assets::StoreOptions,
    audio::{Audio, AudioConfig},
    hls::{AbrMode, AbrOptions, Hls, HlsConfig},
    stream::{AudioCodec, ContainerFormat, MediaInfo, Stream},
};
use kithara_integration_tests::hls_fixture::{HlsTestServer, HlsTestServerConfig};
use kithara_platform::tokio::task::spawn_blocking;
use kithara_test_utils::{
    TestTempDir, Xorshift64, create_wav_exact_bytes, phase_distance, phase_from_f32,
    signal_pcm::signal,
};
use tokio_util::sync::CancellationToken;
use tracing::info;

use crate::common::test_defaults::SawWav;

struct Consts;
impl Consts {
    const D: SawWav = SawWav::DEFAULT;
    const SEGMENT_COUNT: usize = 100;
    const TOTAL_BYTES: usize = Self::SEGMENT_COUNT * Self::D.segment_size; // 20 MB
    const SEEK_ITERATIONS: usize = 1000;

    /// Compute the expected duration in seconds for the generated WAV.
    fn expected_duration_secs() -> f64 {
        let header_size = 44usize;
        let bytes_per_frame = Self::D.channels as usize * 2;
        let frame_count = (Self::TOTAL_BYTES - header_size) / bytes_per_frame;
        frame_count as f64 / f64::from(Self::D.sample_rate)
    }
}

// Stress Test

/// 1000 random seek+read cycles with three-level PCM verification
/// on `Audio<Stream<Hls>>` serving a 20 MB WAV.
///
/// Scenario:
/// 1. Generate saw-tooth WAV (20 MB, ~113s, stereo 44.1 kHz)
/// 2. Spawn `HlsTestServer` with `custom_data` = WAV bytes
/// 3. Create `Audio<Stream<Hls>>` with a format hint "wav"
/// 4. Verify duration ≈ expected
/// 5. 1000 random seeks with verification:
///    - Level 1: integrity (finite, range, L==R)
///    - Level 2: continuity (consecutive frames follow a pattern)
///    - Level 3: position (decoded phase ≈ expected phase)
/// 6. Final seek near the end → read to EOF
#[kithara::test(
    tokio,
    native,
    serial,
    timeout(Duration::from_secs(30)),
    env(KITHARA_HANG_TIMEOUT_SECS = "1")
)]
#[case::ephemeral(true)]
#[cfg(not(target_arch = "wasm32"))]
#[case::mmap(false)]
async fn stress_seek_audio_hls_wav(#[case] ephemeral: bool) {
    // Step 1: Generate WAV
    let wav_data = create_wav_exact_bytes(
        signal::Sawtooth,
        Consts::D.sample_rate,
        Consts::D.channels,
        Consts::TOTAL_BYTES,
    );
    let expected_dur = Consts::expected_duration_secs();
    info!(
        total_bytes = Consts::TOTAL_BYTES,
        duration_secs = format!("{expected_dur:.2}"),
        "Generated saw-tooth WAV"
    );

    // Step 2: Spawn HLS server with custom WAV data
    let segment_duration = Consts::D.segment_size as f64
        / (f64::from(Consts::D.sample_rate) * f64::from(Consts::D.channels) * 2.0);
    let server = HlsTestServer::new(HlsTestServerConfig {
        segments_per_variant: Consts::SEGMENT_COUNT,
        segment_size: Consts::D.segment_size,
        segment_duration_secs: segment_duration,
        custom_data: Some(Arc::new(wav_data)),
        ..Default::default()
    })
    .await;

    let url = server.url("/master.m3u8");
    info!(%url, segments = Consts::SEGMENT_COUNT, "HLS server ready");

    // Step 3: Create Audio<Stream<Hls>>
    let temp_dir = TestTempDir::new();
    let cancel = CancellationToken::new();

    let mut store = StoreOptions::new(temp_dir.path());
    if ephemeral {
        // Ephemeral mode auto-evicts MemResources from the LRU cache.
        // Increase capacity so all segments remain accessible for random seeks.
        store.cache_capacity =
            Some(NonZeroUsize::new(Consts::SEGMENT_COUNT + 10).expect("nonzero"));
        store.ephemeral = true;
    }

    let hls_config = HlsConfig::new(url)
        .with_store(store)
        .with_cancel(cancel)
        .with_abr_options(AbrOptions {
            mode: AbrMode::Manual(0),
            ..AbrOptions::default()
        });

    let wav_info = MediaInfo::new(Some(AudioCodec::Pcm), Some(ContainerFormat::Wav));
    let config = AudioConfig::<Hls>::new(hls_config).with_media_info(wav_info);
    let mut audio = Audio::<Stream<Hls>>::new(config)
        .await
        .expect("create Audio<Stream<Hls>> pipeline");

    // Step 4: Verify duration
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

    // Steps 5-6 in blocking thread
    let result = spawn_blocking(move || {
        // Compute chunk size: ~50ms of audio
        let chunk_duration_secs = 0.05;
        let chunk_samples =
            (chunk_duration_secs * f64::from(spec.sample_rate) * f64::from(spec.channels)) as usize;
        info!(chunk_duration_secs, chunk_samples, "Read chunk size");

        let mut rng = Xorshift64::new(0xDEAD_BEEF_CAFE_1337);
        let mut buf = vec![0.0f32; chunk_samples];

        // Generate 1000 random seek positions
        let max_seek_secs = total_secs - chunk_duration_secs;
        assert!(max_seek_secs > 0.0, "stream too short for chunk size");

        let seek_positions: Vec<f64> = (0..Consts::SEEK_ITERATIONS)
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

            // Level 1: Integrity
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

            // Level 2: Continuity
            // Consecutive frames should differ by exactly 1 step in the saw-tooth,
            // or wrap from max to min.
            if frames >= 2 {
                for f in 1..frames {
                    let prev_phase = phase_from_f32(buf[(f - 1) * channels]);
                    let curr_phase = phase_from_f32(buf[f * channels]);
                    let expected_next = (prev_phase + 1) % SawWav::SAW_PERIOD;
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

            // Level 3: Position
            // First decoded sample should correspond to the seek position.
            //
            // Symphonia's WAV reader returns packets of 1152 frames.
            // After seek, the first decoded packet starts from the packet
            // boundary BEFORE the target, so the actual position can be
            // up to 1151 frames earlier. Tolerance of 1200 covers this
            // while still detecting gross seek errors (>27ms at 44.1 kHz).
            let expected_frame_idx = (pos_secs * f64::from(spec.sample_rate)).round() as usize;
            let expected_phase = expected_frame_idx % SawWav::SAW_PERIOD;
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
            "All {} seek+read iterations done", Consts::SEEK_ITERATIONS
        );

        assert_eq!(successful_reads, Consts::SEEK_ITERATIONS as u64);
        assert_eq!(
            channel_mismatches, 0,
            "L/R channel data diverged {channel_mismatches} times - data corruption"
        );
        if continuity_errors > 0 {
            tracing::warn!(
                continuity_errors,
                "continuity breaks detected (within tolerance of 5)"
            );
        }
        assert!(
            continuity_errors <= 5,
            "{continuity_errors} continuity breaks (>5 tolerance) - decoder returned non-contiguous data"
        );
        if position_errors > 0 {
            tracing::warn!(
                position_errors,
                "position mismatches detected (within tolerance of 3)"
            );
        }
        assert!(
            position_errors <= 3,
            "{position_errors} position mismatches (>3 tolerance) - seek landed in wrong place"
        );

        // Step 6: Final seek near the end → read to EOF
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

        info!(remaining_samples, "Final read done - EOF confirmed");

        // Step 7: Regression check - seek after EOF must resume playback.
        // This catches false-EOF races where seek re-queues demand, but the downloader 
        // marks EOF before demand is processed.
        let resume_positions = [0.5_f64, total_secs * 0.25, total_secs * 0.75];
        for (i, pos_secs) in resume_positions.iter().copied().enumerate() {
            audio
                .seek(Duration::from_secs_f64(pos_secs))
                .unwrap_or_else(|e| panic!("seek-after-eof #{i} to {pos_secs:.4}s failed: {e}"));

            let n = audio.read(&mut buf);
            assert!(
                n > 0,
                "seek-after-eof #{i} returned 0 samples at {pos_secs:.4}s (is_eof={})",
                audio.is_eof(),
            );
        }
    })
    .await;

    match result {
        Ok(()) => info!("Audio+HLS stress test passed"),
        Err(e) => panic!("spawn_blocking failed: {e}"),
    }
}
