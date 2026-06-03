use std::{num::NonZeroUsize, sync::Arc};

use kithara::{
    assets::StoreOptions,
    audio::{Audio, AudioConfig, ReadOutcome},
    decode::DecoderBackend,
    hls::{AbrMode, Hls, HlsConfig},
    stream::{AudioCodec, ContainerFormat, MediaInfo, Stream},
};
use kithara_integration_tests::{
    TestTempDir, Xorshift64, create_wav_exact_bytes,
    hls_server::{HlsTestServer, HlsTestServerConfig},
    phase_distance, phase_from_f32,
    signal_pcm::signal,
};
use kithara_platform::{CancellationToken, time::Duration, tokio::task::spawn_blocking};
use tracing::info;

use crate::common::test_defaults::SawWav;

struct Consts;
impl Consts {
    const D: SawWav = SawWav::DEFAULT;
    const SEGMENT_COUNT: usize = 100;
    const TOTAL_BYTES: usize = Self::SEGMENT_COUNT * Self::D.segment_size;
    const SEEK_ITERATIONS: usize = 1000;

    /// Compute the expected duration in seconds for the generated WAV.
    fn expected_duration_secs() -> f64 {
        let header_size = 44usize;
        let bytes_per_frame = Self::D.channels as usize * 2;
        let frame_count = (Self::TOTAL_BYTES - header_size) / bytes_per_frame;
        frame_count as f64 / f64::from(Self::D.sample_rate)
    }
}

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
#[case::symphonia_ephemeral(true, DecoderBackend::Symphonia)]
#[cfg_attr(
    any(target_os = "macos", target_os = "ios"),
    case::apple_ephemeral(true, DecoderBackend::Apple)
)]
#[cfg(not(target_arch = "wasm32"))]
#[case::symphonia_mmap(false, DecoderBackend::Symphonia)]
#[cfg_attr(
    any(target_os = "macos", target_os = "ios"),
    case::apple_mmap(false, DecoderBackend::Apple)
)]
async fn stress_seek_audio_hls_wav(#[case] ephemeral: bool, #[case] backend: DecoderBackend) {
    #[cfg(any(target_os = "macos", target_os = "ios"))]
    kithara_integration_tests::apple_warmup::warm_if_apple(backend);
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

    let temp_dir = TestTempDir::new();
    let cancel = CancellationToken::default();

    let mut store = StoreOptions::new(temp_dir.path());
    if ephemeral {
        store.cache_capacity =
            Some(NonZeroUsize::new(Consts::SEGMENT_COUNT + 10).expect("nonzero"));
        store.is_ephemeral = true;
    }

    let hls_config = HlsConfig::for_url(url)
        .store(store)
        .cancel(cancel)
        .initial_abr_mode(AbrMode::manual(0))
        .build();

    let wav_info = MediaInfo::new(Some(AudioCodec::Pcm), Some(ContainerFormat::Wav));
    let config = AudioConfig::<Hls>::for_stream(hls_config)
        .media_info(wav_info)
        .decoder_backend(backend)
        .build();
    let mut audio = Audio::<Stream<Hls>>::new(config)
        .await
        .expect("create Audio<Stream<Hls>> pipeline");

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

    let result = spawn_blocking(move || {
        let chunk_duration_secs = 0.05;
        let chunk_samples =
            (chunk_duration_secs * f64::from(spec.sample_rate.get()) * f64::from(spec.channels)) as usize;
        info!(chunk_duration_secs, chunk_samples, "Read chunk size");

        let mut rng = Xorshift64::new(0xDEAD_BEEF_CAFE_1337);
        let mut buf = vec![0.0f32; chunk_samples];

        let max_seek_secs = total_secs - chunk_duration_secs;
        assert!(max_seek_secs > 0.0, "stream too short for chunk size");

        let seek_positions: Vec<f64> = (0..Consts::SEEK_ITERATIONS)
            .map(|_| rng.range_f64(0.001, max_seek_secs))
            .collect();

        info!(
            count = seek_positions.len(),
            max_seek_secs, "Generated seek positions"
        );

        let mut successful_reads = 0u64;
        let mut total_samples_read = 0u64;
        let mut channel_mismatches = 0u64;
        let mut continuity_errors = 0u64;
        let mut position_errors = 0u64;

        let channels = spec.channels as usize;

        for (i, &pos_secs) in seek_positions.iter().enumerate() {
            let position = Duration::from_secs_f64(pos_secs);

            audio.seek(position).unwrap_or_else(|e| {
                panic!("seek #{i} to {pos_secs:.4}s failed: {e}");
            });

            let n = match audio.read(&mut buf) {
                Ok(ReadOutcome::Frames { count, .. }) => count.get(),
                Ok(ReadOutcome::Pending { .. }) => {
                    panic!(
                        "read returned 0 after seek #{i} to {pos_secs:.4}s",
                    );
                }
                Ok(ReadOutcome::Eof { .. }) => {
                    panic!(
                        "read returned Eof after seek #{i} to {pos_secs:.4}s",
                    );
                }
                Err(e) => panic!("read error after seek #{i}: {e}"),
            };

            let frames = n / channels;

            for (j, &sample) in buf[..n].iter().enumerate() {
                assert!(
                    sample.is_finite() && (-1.0..=1.0).contains(&sample),
                    "invalid sample at seek #{i} offset {j}: {sample} (pos {pos_secs:.4}s)",
                );
            }

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

            let expected_frame_idx = (pos_secs * f64::from(spec.sample_rate.get())).round() as usize;
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

        let final_seek_secs = total_secs - chunk_duration_secs;
        info!(final_seek_secs, "Final seek near end");

        audio
            .seek(Duration::from_secs_f64(final_seek_secs))
            .unwrap_or_else(|e| {
                panic!("final seek to {final_seek_secs:.4}s failed: {e}");
            });

        let mut remaining_samples = 0u64;
        let mut saw_eof = false;
        loop {
            match audio.read(&mut buf) {
                Ok(ReadOutcome::Pending { .. }) => break,
                Ok(ReadOutcome::Frames { count, .. }) => {
                    remaining_samples += count.get() as u64;
                    for &sample in &buf[..count.get()] {
                        assert!(
                            sample.is_finite() && (-1.0..=1.0).contains(&sample),
                            "invalid sample in final tail read",
                        );
                    }
                }
                Ok(ReadOutcome::Eof { .. }) => {
                    saw_eof = true;
                    break;
                }
                Err(e) => panic!("final tail read error: {e}"),
            }
        }

        assert!(
            saw_eof,
            "expected EOF after reading all remaining data from {final_seek_secs:.4}s"
        );

        info!(remaining_samples, "Final read done - EOF confirmed");

        let resume_positions = [0.5_f64, total_secs * 0.25, total_secs * 0.75];
        for (i, pos_secs) in resume_positions.iter().copied().enumerate() {
            audio
                .seek(Duration::from_secs_f64(pos_secs))
                .unwrap_or_else(|e| panic!("seek-after-eof #{i} to {pos_secs:.4}s failed: {e}"));

            match audio.read(&mut buf) {
                Ok(ReadOutcome::Frames { .. }) => {}
                Ok(other) => {
                    panic!(
                        "seek-after-eof #{i} at {pos_secs:.4}s produced no samples: {other:?}"
                    );
                }
                Err(e) => panic!("seek-after-eof #{i} read error: {e}"),
            }
        }
    })
    .await;

    match result {
        Ok(()) => info!("Audio+HLS stress test passed"),
        Err(e) => panic!("spawn_blocking failed: {e}"),
    }
}
