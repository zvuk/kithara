//! Stress test: ABR variant switch and seek on `Audio<Stream<Hls>>`.
//!
//! Two HLS variants with different saw-tooth directions (ascending vs. descending).
//! V0 segments are delayed after segment 3 to trigger ABR downgrade to V1.
//! WAV header as HLS init segment (`#EXT-X-MAP`).
//!
//! Four-phase verification:
//! 1. **Warmup**: read until ABR switches from V0 (ascending) to V1 (descending)
//! 2. **Post-switch**: 10 chunks of descending data confirm stable V1
//! 3. **Random seeks**: 200 seek+read cycles with integrity/continuity/direction checks
//! 4. **EOF**: seek near the end, read to EOF

use std::{sync::Arc, time::Duration};

use kithara::{
    assets::StoreOptions,
    audio::{Audio, AudioConfig},
    hls::{AbrMode, AbrOptions, Hls, HlsConfig},
    stream::{AudioCodec, ContainerFormat, MediaInfo, Stream},
};
use kithara_integration_tests::hls_fixture::{HlsTestServer, HlsTestServerConfig};
use kithara_platform::{time::Instant, tokio::task::spawn_blocking};
use kithara_test_utils::{
    SignalDirection as Direction, TestTempDir, Xorshift64, detect_direction,
    fixture_protocol::DelayRule,
    phase_from_f32,
    signal_pcm::{Finite, SignalPcm, signal},
    wav::create_wav_header,
};
use tokio_util::sync::CancellationToken;
use tracing::info;

use crate::common::test_defaults::SawWav;

struct Consts;
impl Consts {
    const D: SawWav = SawWav::DEFAULT;
    const SEGMENT_COUNT: usize = 50;
    const SEEK_ITERATIONS: usize = 200;
    const WARMUP_TIMEOUT_SECS: u64 = 30;
}

// Stress Test

/// ABR variant switch stress test with ascending/descending saw-tooth verification.
///
/// Scenario:
/// 1. Two variants: V0 (ascending, high bandwidth, delayed) and V1 (descending, low bandwidth)
/// 2. ABR starts on V0, switches to V1 when V0 segments become slow
/// 3. Verify the switch happened via PCM direction change
/// 4. 200 random seeks with direction + integrity checks
#[kithara::test(
    native,
    tokio,
    serial,
    timeout(Duration::from_secs(30)),
    env(KITHARA_HANG_TIMEOUT_SECS = "3")
)]
async fn stress_seek_abr_audio() {
    // Generate WAV data for two variants
    let init_segment = Arc::new(create_wav_header(
        Consts::D.sample_rate,
        Consts::D.channels,
        None,
    ));
    let v0_pcm = Arc::new(
        SignalPcm::new(
            signal::Sawtooth,
            Consts::D.sample_rate,
            Consts::D.channels,
            Finite::from_segments(
                Consts::SEGMENT_COUNT,
                Consts::D.segment_size,
                Consts::D.channels,
            ),
        )
        .into_vec(),
    );
    let v1_pcm = Arc::new(
        SignalPcm::new(
            signal::SawtoothDescending,
            Consts::D.sample_rate,
            Consts::D.channels,
            Finite::from_segments(
                Consts::SEGMENT_COUNT,
                Consts::D.segment_size,
                Consts::D.channels,
            ),
        )
        .into_vec(),
    );

    info!(
        init_size = init_segment.len(),
        v0_size = v0_pcm.len(),
        v1_size = v1_pcm.len(),
        segments = Consts::SEGMENT_COUNT,
        "Generated WAV data for two variants"
    );

    // Spawn HLS server
    let segment_duration = Consts::D.segment_size as f64
        / (f64::from(Consts::D.sample_rate) * f64::from(Consts::D.channels) * 2.0);

    let server = HlsTestServer::new(HlsTestServerConfig {
        variant_count: 2,
        segments_per_variant: Consts::SEGMENT_COUNT,
        segment_size: Consts::D.segment_size,
        segment_duration_secs: segment_duration,
        custom_data_per_variant: Some(vec![Arc::clone(&v0_pcm), Arc::clone(&v1_pcm)]),
        init_data_per_variant: Some(vec![Arc::clone(&init_segment), Arc::clone(&init_segment)]),
        variant_bandwidths: Some(vec![5_000_000, 1_000_000]),
        delay_rules: vec![DelayRule {
            variant: Some(0),
            segment_gte: Some(3),
            delay_ms: 500,
            ..Default::default()
        }],
        ..Default::default()
    })
    .await;

    let url = server.url("/master.m3u8");
    info!(%url, "HLS server ready with 2 variants");

    // Create Audio<Stream<Hls>> with Auto ABR starting on V0
    let temp_dir = TestTempDir::new();
    let cancel = CancellationToken::new();

    let hls_config = HlsConfig::new(url)
        .with_store(StoreOptions::new(temp_dir.path()))
        .with_cancel(cancel)
        .with_abr_options(AbrOptions {
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
        .expect("create Audio<Stream<Hls>> pipeline");

    let spec = audio.spec();
    info!(
        sample_rate = spec.sample_rate,
        channels = spec.channels,
        "Audio pipeline created"
    );

    // Run test phases in a blocking thread
    let result = spawn_blocking(move || {
        let channels = spec.channels as usize;
        let chunk_duration_secs = 0.05;
        let chunk_samples =
            (chunk_duration_secs * f64::from(spec.sample_rate) * channels as f64) as usize;
        let mut buf = vec![0.0f32; chunk_samples];

        // Phase 1: Warmup + ABR switch detection
        info!("Phase 1: waiting for ABR switch (ascending -> descending)...");

        let warmup_start = Instant::now();
        let warmup_timeout = Duration::from_secs(Consts::WARMUP_TIMEOUT_SECS);
        let mut warmup_ascending_chunks = 0u64;
        let mut warmup_unknown_chunks = 0u64;

        loop {
            if warmup_start.elapsed() > warmup_timeout {
                panic!(
                    "ABR switch not detected within {}s (ascending={}, unknown={})",
                    Consts::WARMUP_TIMEOUT_SECS,
                    warmup_ascending_chunks,
                    warmup_unknown_chunks
                );
            }

            let n = audio.read(&mut buf);
            if n == 0 {
                if audio.is_eof() {
                    panic!(
                        "Hit EOF before ABR switch (ascending={}, unknown={})",
                        warmup_ascending_chunks, warmup_unknown_chunks
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
                        "ABR switch detected: ascending -> descending"
                    );
                    break;
                }
                Direction::Unknown => {
                    warmup_unknown_chunks += 1;
                }
            }

            if warmup_ascending_chunks.is_multiple_of(100) && warmup_ascending_chunks > 0 {
                info!(
                    warmup_ascending_chunks,
                    warmup_unknown_chunks,
                    elapsed_ms = warmup_start.elapsed().as_millis(),
                    "Still waiting for ABR switch..."
                );
            }
        }

        // Phase 2: Post-switch verification
        info!("Phase 2: verifying 10 post-switch chunks are descending...");

        let mut post_switch_ok = 0u64;
        for chunk_idx in 0..10 {
            let n = audio.read(&mut buf);
            assert!(
                n > 0,
                "read returned 0 in post-switch chunk {} (is_eof={})",
                chunk_idx,
                audio.is_eof()
            );

            let frames = n / channels;

            // Integrity
            for (j, &sample) in buf[..n].iter().enumerate() {
                assert!(
                    sample.is_finite() && (-1.0..=1.0).contains(&sample),
                    "invalid sample in post-switch chunk {} offset {}: {}",
                    chunk_idx,
                    j,
                    sample
                );
            }

            // L == R
            if channels == 2 {
                for f in 0..frames {
                    let l = buf[f * 2];
                    let r = buf[f * 2 + 1];
                    assert!(
                        (l - r).abs() <= f32::EPSILON,
                        "L/R mismatch in post-switch chunk {} frame {}: L={} R={}",
                        chunk_idx,
                        f,
                        l,
                        r
                    );
                }
            }

            // Continuity (descending pattern).
            // Allow up to 1 break per chunk: the old decoder may have already decoded
            // some V1 data before the format change was detected, so when the new decoder
            // starts from the seg4 beginning, there's a position overlap.
            if frames >= 2 {
                let mut break_count = 0;
                for f in 1..frames {
                    let prev_phase = phase_from_f32(buf[(f - 1) * channels]);
                    let curr_phase = phase_from_f32(buf[f * channels]);
                    let expected = (prev_phase + SawWav::SAW_PERIOD - 1) % SawWav::SAW_PERIOD;
                    if curr_phase != expected {
                        break_count += 1;
                    }
                }
                assert!(
                    break_count <= 1,
                    "too many continuity breaks in post-switch chunk {}: {}",
                    chunk_idx,
                    break_count
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
                "post-switch chunk {} direction is {:?}, expected Descending",
                chunk_idx,
                dir
            );

            post_switch_ok += 1;
        }

        info!(
            post_switch_ok,
            "Phase 2 complete: all post-switch chunks descending"
        );

        // Phase 3: Random seeks
        info!(
            "Phase 3: {} random seek+read cycles...",
            Consts::SEEK_ITERATIONS
        );

        let total_duration = audio.duration();
        let total_secs = total_duration.map_or(
            Consts::SEGMENT_COUNT as f64 * chunk_duration_secs * 20.0,
            |d| d.as_secs_f64(),
        );
        let max_seek_secs = (total_secs - chunk_duration_secs).max(0.1);

        let mut rng = Xorshift64::new(0xAB25_5017_C400_0000);
        let mut successful_reads = 0u64;
        let mut channel_mismatches = 0u64;
        let mut continuity_errors = 0u64;
        let mut direction_errors = 0u64;

        for i in 0..Consts::SEEK_ITERATIONS {
            let pos_secs = rng.range_f64(0.001, max_seek_secs);
            let position = Duration::from_secs_f64(pos_secs);

            audio.seek(position).unwrap_or_else(|e| {
                panic!("seek #{} to {:.4}s failed: {}", i, pos_secs, e);
            });

            let n = audio.read(&mut buf);
            if n == 0 {
                // After seek, a zero-read can happen if we're at the exact EOF boundary
                continue;
            }

            let frames = n / channels;

            // Level 1: Integrity
            for (j, &sample) in buf[..n].iter().enumerate() {
                assert!(
                    sample.is_finite() && (-1.0..=1.0).contains(&sample),
                    "invalid sample at seek #{} offset {}: {} (pos {:.4}s)",
                    i,
                    j,
                    sample,
                    pos_secs
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
                    let expected_asc = (prev_phase + 1) % SawWav::SAW_PERIOD;
                    let expected_desc = (prev_phase + SawWav::SAW_PERIOD - 1) % SawWav::SAW_PERIOD;
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
                        "Unexpected direction (expected SawtoothDescending)"
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
            "Phase 3 complete: {} seek+read cycles",
            Consts::SEEK_ITERATIONS
        );

        assert_eq!(
            channel_mismatches, 0,
            "L/R channel data diverged {} times",
            channel_mismatches
        );
        assert_eq!(
            continuity_errors, 0,
            "{} continuity breaks in decoded data",
            continuity_errors
        );
        assert_eq!(
            direction_errors, 0,
            "{} direction errors (expected descending after ABR switch)",
            direction_errors
        );

        // Phase 4: Final seek near end -> EOF
        info!("Phase 4: seek near end + read to EOF...");

        let final_seek_secs = (total_secs - chunk_duration_secs).max(0.0);
        audio
            .seek(Duration::from_secs_f64(final_seek_secs))
            .unwrap_or_else(|e| {
                panic!("final seek to {:.4}s failed: {}", final_seek_secs, e);
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
        Err(e) => panic!("spawn_blocking failed: {e}"),
    }
}
