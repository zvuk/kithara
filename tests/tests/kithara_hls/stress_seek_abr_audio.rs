//! Stress test: ABR variant switch + seek on `Audio<Stream<Hls>>`.
//!
//! Two HLS variants with different saw-tooth directions (ascending vs descending).
//! V0 segments are delayed after segment 3 to trigger ABR downgrade to V1.
//! WAV header as HLS init segment (`#EXT-X-MAP`).
//!
//! Four-phase verification:
//! 1. **Warmup**: read until ABR switches from V0 (ascending) to V1 (descending)
//! 2. **Post-switch**: 10 chunks of descending data confirm stable V1
//! 3. **Random seeks**: 200 seek+read cycles with integrity/continuity/direction checks
//! 4. **EOF**: seek near end, read to EOF

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
const SEGMENT_COUNT: usize = 50;
const SEEK_ITERATIONS: usize = 200;
const SAW_PERIOD: usize = 65536;
const WARMUP_TIMEOUT_SECS: u64 = 30;

// WAV Generators

/// Ascending saw-tooth: frame 0 → -32768, frame 65535 → 32767.
fn ascending_sample(frame: usize) -> i16 {
    ((frame % SAW_PERIOD) as i32 - 32768) as i16
}

/// Descending saw-tooth: frame 0 → 32767, frame 65535 → -32768.
fn descending_sample(frame: usize) -> i16 {
    (32767 - (frame % SAW_PERIOD) as i32) as i16
}

/// Build WAV init segment (44-byte header) with known PCM data size.
fn create_wav_init_segment() -> Vec<u8> {
    let bytes_per_sample: u16 = 2;
    let byte_rate = SAMPLE_RATE * CHANNELS as u32 * bytes_per_sample as u32;
    let block_align = CHANNELS * bytes_per_sample;
    // Use 0xFFFFFFFF for streaming mode: the decoder will read until EOF
    // rather than expecting an exact data size. This avoids size mismatch
    // when ABR switch starts from a mid-stream segment.
    let data_size = 0xFFFF_FFFFu32;
    let file_size = 0xFFFF_FFFFu32;

    let mut wav = Vec::with_capacity(44);

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
    wav.extend_from_slice(&byte_rate.to_le_bytes());
    wav.extend_from_slice(&block_align.to_le_bytes());
    wav.extend_from_slice(&(bytes_per_sample * 8).to_le_bytes());

    // data chunk
    wav.extend_from_slice(b"data");
    wav.extend_from_slice(&data_size.to_le_bytes());

    wav
}

/// Generate PCM data (no WAV header) for media segments.
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
    // Pad to exact size if rounding leaves a gap
    pcm.resize(total_bytes, 0);
    pcm
}

// Direction Detection

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Direction {
    Ascending,
    Descending,
    Unknown,
}

/// Recover saw-tooth phase from a decoded f32 sample.
fn phase_from_f32(sample: f32) -> usize {
    let i16_val = (sample * 32768.0).round() as i32;
    ((i16_val + 32768) & 0xFFFF) as usize
}

/// Detect saw-tooth direction from a buffer of interleaved f32 samples.
///
/// Checks up to 10 pairs of consecutive frames.
/// ascending: phase[f+1] == (phase[f] + 1) % SAW_PERIOD
/// descending: phase[f+1] == (phase[f] + SAW_PERIOD - 1) % SAW_PERIOD
fn detect_direction(buf: &[f32], channels: usize) -> Direction {
    let frames = buf.len() / channels;
    if frames < 2 {
        return Direction::Unknown;
    }

    let check_count = 10.min(frames - 1);
    let mut ascending_votes = 0u32;
    let mut descending_votes = 0u32;

    for f in 0..check_count {
        let p0 = phase_from_f32(buf[f * channels]);
        let p1 = phase_from_f32(buf[(f + 1) * channels]);

        let expected_asc = (p0 + 1) % SAW_PERIOD;
        let expected_desc = (p0 + SAW_PERIOD - 1) % SAW_PERIOD;

        if p1 == expected_asc {
            ascending_votes += 1;
        } else if p1 == expected_desc {
            descending_votes += 1;
        }
    }

    if ascending_votes > descending_votes && ascending_votes > 0 {
        Direction::Ascending
    } else if descending_votes > ascending_votes && descending_votes > 0 {
        Direction::Descending
    } else {
        Direction::Unknown
    }
}

// Stress Test

/// ABR variant switch stress test with ascending/descending saw-tooth verification.
///
/// Scenario:
/// 1. Two variants: V0 (ascending, high bandwidth, delayed) and V1 (descending, low bandwidth)
/// 2. ABR starts on V0, switches to V1 when V0 segments become slow
/// 3. Verify switch happened via PCM direction change
/// 4. 200 random seeks with direction + integrity checks
#[rstest]
#[timeout(Duration::from_secs(120))]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn stress_seek_abr_audio() {
    let _ = tracing_subscriber::fmt()
        .with_test_writer()
        .with_max_level(tracing::Level::DEBUG)
        .with_env_filter(std::env::var("RUST_LOG").unwrap_or_else(|_| {
            "kithara_audio=debug,kithara_decode=debug,kithara_hls=debug,kithara_stream=debug"
                .to_string()
        }))
        .try_init();

    // --- Generate WAV data for two variants ---
    let init_segment = Arc::new(create_wav_init_segment());
    let v0_pcm = Arc::new(create_pcm_segments(ascending_sample));
    let v1_pcm = Arc::new(create_pcm_segments(descending_sample));

    info!(
        init_size = init_segment.len(),
        v0_size = v0_pcm.len(),
        v1_size = v1_pcm.len(),
        segments = SEGMENT_COUNT,
        "Generated WAV data for two variants"
    );

    // --- Spawn HLS server ---
    let segment_duration = SEGMENT_SIZE as f64 / (SAMPLE_RATE as f64 * CHANNELS as f64 * 2.0);

    let server = HlsTestServer::new(HlsTestServerConfig {
        variant_count: 2,
        segments_per_variant: SEGMENT_COUNT,
        segment_size: SEGMENT_SIZE,
        segment_duration_secs: segment_duration,
        custom_data_per_variant: Some(vec![Arc::clone(&v0_pcm), Arc::clone(&v1_pcm)]),
        init_data_per_variant: Some(vec![Arc::clone(&init_segment), Arc::clone(&init_segment)]),
        variant_bandwidths: Some(vec![5_000_000, 1_000_000]),
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
    info!(%url, "HLS server ready with 2 variants");

    // --- Create Audio<Stream<Hls>> with Auto ABR starting on V0 ---
    let temp_dir = TempDir::new().expect("temp dir");
    let cancel = CancellationToken::new();

    let hls_config = HlsConfig::new(url)
        .with_store(StoreOptions::new(temp_dir.path()))
        .with_cancel(cancel)
        .with_abr(AbrOptions {
            down_switch_buffer_secs: 0.0,
            min_buffer_for_up_switch_secs: 0.0,
            min_switch_interval: Duration::ZERO,
            mode: AbrMode::Auto(Some(0)),
            throughput_safety_factor: 1.0,
            ..AbrOptions::default()
        });

    let wav_info = MediaInfo::new(Some(AudioCodec::Pcm), Some(ContainerFormat::Wav));
    let config = AudioConfig::<Hls>::new(hls_config).with_media_info(wav_info);
    let mut audio = Audio::<Stream<Hls>>::new(config)
        .await
        .expect("create Audio<Stream<Hls>> pipeline");

    let spec = audio.spec();
    info!(
        sample_rate = spec.sample_rate,
        channels = spec.channels,
        "Audio pipeline created"
    );

    // --- Run test phases in blocking thread ---
    let result = tokio::task::spawn_blocking(move || {
        let channels = spec.channels as usize;
        let chunk_duration_secs = 0.05;
        let chunk_samples =
            (chunk_duration_secs * spec.sample_rate as f64 * channels as f64) as usize;
        let mut buf = vec![0.0f32; chunk_samples];

        // Phase 1: Warmup + ABR switch detection
        info!("Phase 1: waiting for ABR switch (ascending → descending)...");

        let warmup_start = std::time::Instant::now();
        let warmup_timeout = Duration::from_secs(WARMUP_TIMEOUT_SECS);
        let mut warmup_ascending_chunks = 0u64;
        let mut warmup_unknown_chunks = 0u64;
        let mut switch_detected = false;

        loop {
            if warmup_start.elapsed() > warmup_timeout {
                panic!(
                    "ABR switch not detected within {WARMUP_TIMEOUT_SECS}s \
                     (ascending={warmup_ascending_chunks}, unknown={warmup_unknown_chunks})"
                );
            }

            let n = audio.read(&mut buf);
            if n == 0 {
                if audio.is_eof() {
                    panic!(
                        "Hit EOF before ABR switch \
                         (ascending={warmup_ascending_chunks}, unknown={warmup_unknown_chunks})"
                    );
                }
                continue;
            }

            let dir = detect_direction(&buf[..n], channels);
            match dir {
                Direction::Ascending => {
                    warmup_ascending_chunks += 1;
                }
                Direction::Descending => {
                    info!(
                        warmup_ascending_chunks,
                        warmup_unknown_chunks,
                        elapsed_ms = warmup_start.elapsed().as_millis(),
                        "ABR switch detected: ascending → descending"
                    );
                    switch_detected = true;
                    break;
                }
                Direction::Unknown => {
                    warmup_unknown_chunks += 1;
                }
            }

            if warmup_ascending_chunks % 100 == 0 && warmup_ascending_chunks > 0 {
                info!(
                    warmup_ascending_chunks,
                    warmup_unknown_chunks,
                    elapsed_ms = warmup_start.elapsed().as_millis(),
                    "Still waiting for ABR switch..."
                );
            }
        }

        assert!(switch_detected, "ABR switch was not detected");

        // Phase 2: Post-switch verification
        info!("Phase 2: verifying 10 post-switch chunks are descending...");

        let mut post_switch_ok = 0u64;
        for chunk_idx in 0..10 {
            let n = audio.read(&mut buf);
            assert!(
                n > 0,
                "read returned 0 in post-switch chunk {chunk_idx} (is_eof={})",
                audio.is_eof()
            );

            let frames = n / channels;

            // Integrity
            for (j, &sample) in buf[..n].iter().enumerate() {
                assert!(
                    sample.is_finite() && (-1.0..=1.0).contains(&sample),
                    "invalid sample in post-switch chunk {chunk_idx} offset {j}: {sample}",
                );
            }

            // L == R
            if channels == 2 {
                for f in 0..frames {
                    let l = buf[f * 2];
                    let r = buf[f * 2 + 1];
                    assert!(
                        (l - r).abs() <= f32::EPSILON,
                        "L/R mismatch in post-switch chunk {chunk_idx} frame {f}: L={l} R={r}",
                    );
                }
            }

            // Continuity (descending pattern).
            // Allow up to 1 break per chunk: the old decoder may have already decoded
            // some V1 data before format change was detected, so when the new decoder
            // starts from seg4 beginning, there's a position overlap.
            if frames >= 2 {
                let mut break_count = 0;
                for f in 1..frames {
                    let prev_phase = phase_from_f32(buf[(f - 1) * channels]);
                    let curr_phase = phase_from_f32(buf[f * channels]);
                    let expected = (prev_phase + SAW_PERIOD - 1) % SAW_PERIOD;
                    if curr_phase != expected {
                        break_count += 1;
                    }
                }
                assert!(
                    break_count <= 1,
                    "too many continuity breaks in post-switch chunk {chunk_idx}: {break_count}",
                );
                if break_count == 1 {
                    info!(
                        chunk_idx,
                        "post-switch chunk has 1 expected decoder handoff break"
                    );
                }
            }

            // Direction
            let dir = detect_direction(&buf[..n], channels);
            assert_eq!(
                dir,
                Direction::Descending,
                "post-switch chunk {chunk_idx} direction is {dir:?}, expected Descending"
            );

            post_switch_ok += 1;
        }

        info!(
            post_switch_ok,
            "Phase 2 complete: all post-switch chunks descending"
        );

        // Phase 3: Random seeks
        info!("Phase 3: {SEEK_ITERATIONS} random seek+read cycles...");

        let total_duration = audio.duration();
        let total_secs = total_duration
            .map(|d| d.as_secs_f64())
            .unwrap_or(SEGMENT_COUNT as f64 * chunk_duration_secs * 20.0);
        let max_seek_secs = (total_secs - chunk_duration_secs).max(0.1);

        let mut rng = Xorshift64::new(0xAB25_5017_C400_0000);
        let mut successful_reads = 0u64;
        let mut channel_mismatches = 0u64;
        let mut continuity_errors = 0u64;
        let mut direction_errors = 0u64;

        for i in 0..SEEK_ITERATIONS {
            let pos_secs = rng.range_f64(0.001, max_seek_secs);
            let position = Duration::from_secs_f64(pos_secs);

            audio.seek(position).unwrap_or_else(|e| {
                panic!("seek #{i} to {pos_secs:.4}s failed: {e}");
            });

            let n = audio.read(&mut buf);
            if n == 0 {
                // After seek, a zero-read can happen if we're at exact EOF boundary
                continue;
            }

            let frames = n / channels;

            // Level 1: Integrity
            for (j, &sample) in buf[..n].iter().enumerate() {
                assert!(
                    sample.is_finite() && (-1.0..=1.0).contains(&sample),
                    "invalid sample at seek #{i} offset {j}: {sample} (pos {pos_secs:.4}s)",
                );
            }

            // L == R
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

            // Level 2: Continuity (check for either ascending or descending pattern)
            if frames >= 2 {
                for f in 1..frames {
                    let prev_phase = phase_from_f32(buf[(f - 1) * channels]);
                    let curr_phase = phase_from_f32(buf[f * channels]);
                    let expected_asc = (prev_phase + 1) % SAW_PERIOD;
                    let expected_desc = (prev_phase + SAW_PERIOD - 1) % SAW_PERIOD;
                    if curr_phase != expected_asc && curr_phase != expected_desc {
                        continuity_errors += 1;
                        if continuity_errors <= 3 {
                            info!(
                                iteration = i,
                                frame = f,
                                prev_phase,
                                curr_phase,
                                expected_asc,
                                expected_desc,
                                pos_secs,
                                "Continuity break"
                            );
                        }
                    }
                }
            }

            // Level 3: Direction (after ABR switch, must be descending = V1)
            let dir = detect_direction(&buf[..n], channels);
            if dir != Direction::Descending && dir != Direction::Unknown {
                direction_errors += 1;
                if direction_errors <= 3 {
                    info!(
                        iteration = i,
                        direction = ?dir,
                        pos_secs,
                        "Unexpected direction (expected Descending)"
                    );
                }
            }

            successful_reads += 1;

            if (i + 1) % 50 == 0 {
                info!(
                    iteration = i + 1,
                    successful_reads,
                    channel_mismatches,
                    continuity_errors,
                    direction_errors,
                    "Progress"
                );
            }
        }

        info!(
            successful_reads,
            channel_mismatches,
            continuity_errors,
            direction_errors,
            "Phase 3 complete: {SEEK_ITERATIONS} seek+read cycles"
        );

        assert_eq!(
            channel_mismatches, 0,
            "L/R channel data diverged {channel_mismatches} times"
        );
        assert_eq!(
            continuity_errors, 0,
            "{continuity_errors} continuity breaks in decoded data"
        );
        assert_eq!(
            direction_errors, 0,
            "{direction_errors} direction errors (expected descending after ABR switch)"
        );

        // Phase 4: Final seek near end -> EOF
        info!("Phase 4: seek near end + read to EOF...");

        let final_seek_secs = (total_secs - chunk_duration_secs).max(0.0);
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
            "expected EOF after reading all remaining data"
        );

        info!(remaining_samples, "Phase 4 complete: EOF confirmed");
    })
    .await;

    match result {
        Ok(()) => info!("ABR stress test passed"),
        Err(e) if e.is_panic() => std::panic::resume_unwind(e.into_panic()),
        Err(e) => panic!("spawn_blocking failed: {e}"),
    }
}
