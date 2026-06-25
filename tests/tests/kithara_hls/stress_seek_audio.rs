use std::{num::NonZeroUsize, sync::Arc};

use kithara::{
    assets::StoreOptions,
    audio::{Audio, AudioConfig, ReadOutcome},
    decode::DecoderBackend,
    hls::{AbrMode, Hls, HlsConfig},
    stream::{AudioCodec, ContainerFormat, MediaInfo, Stream},
};
use kithara_integration_tests::{
    HlsFixtureBuilder, TestServerHelper, TestTempDir, Xorshift64, create_wav_exact_bytes,
    fixture_protocol::PcmPattern,
    hls_server::{HlsTestServer, HlsTestServerConfig},
    phase_distance, phase_from_f32,
    signal_pcm::signal,
};
use kithara_platform::{CancelToken, time::Duration, tokio::task::spawn_blocking};
use tracing::info;

use crate::common::test_defaults::SawWav;

struct Consts;
impl Consts {
    const D: SawWav = SawWav::DEFAULT;
    const SEGMENT_COUNT: usize = 100;
    const VARIANT_COUNT: usize = 1;
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

#[derive(Clone, Copy, Debug)]
enum SeekAudioFixture {
    WavFileLike,
    FlacFmp4,
}

impl SeekAudioFixture {
    fn media_info(self) -> MediaInfo {
        match self {
            Self::WavFileLike => MediaInfo::new(Some(AudioCodec::Pcm), Some(ContainerFormat::Wav)),
            Self::FlacFmp4 => MediaInfo::new(Some(AudioCodec::Flac), Some(ContainerFormat::Fmp4)),
        }
    }
}

enum SizeProbeCounter {
    HlsServer(HlsTestServer),
    Helper {
        helper: TestServerHelper,
        segments: usize,
        token: String,
        variants: usize,
    },
}

impl SizeProbeCounter {
    fn size_probe_count(&self, variant: usize, segment: usize) -> u64 {
        match self {
            Self::HlsServer(server) => server.size_probe_count(variant, segment),
            Self::Helper { helper, token, .. } => helper.size_probe_count(token, variant, segment),
        }
    }

    fn variant_count(&self) -> usize {
        match self {
            Self::HlsServer(server) => server.config().variant_count,
            Self::Helper { variants, .. } => *variants,
        }
    }

    fn segment_count(&self) -> usize {
        match self {
            Self::HlsServer(server) => server.config().segments_per_variant,
            Self::Helper { segments, .. } => *segments,
        }
    }

    fn variant_size_probe_count(&self, variant: usize) -> u64 {
        (0..self.segment_count())
            .map(|segment| self.size_probe_count(variant, segment))
            .sum()
    }

    fn total_size_probe_count(&self) -> u64 {
        (0..self.variant_count())
            .map(|variant| self.variant_size_probe_count(variant))
            .sum()
    }
}

fn assert_seek_size_probes(fixture: SeekAudioFixture, counter: &SizeProbeCounter) {
    let total = counter.total_size_probe_count();
    let per_variant: Vec<u64> = (0..counter.variant_count())
        .map(|variant| counter.variant_size_probe_count(variant))
        .collect();
    info!(
        ?fixture,
        total,
        ?per_variant,
        "size-probe counts after seek stress"
    );
    match fixture {
        SeekAudioFixture::WavFileLike => {
            let bound = u64::try_from(counter.segment_count()).expect("segment count fits u64");
            assert!(
                total > 0,
                "WAV cold seeks must resolve exact byte sizes on demand; saw zero size-probes"
            );
            assert!(
                total <= bound,
                "WAV lazy size probes must stay bounded to the touched single-variant prefix: \
                 total={total}, bound={bound}, per_variant={per_variant:?}"
            );
        }
        SeekAudioFixture::FlacFmp4 => {
            assert_eq!(
                total, 0,
                "FLAC/fMP4 segment-aware seeks must not issue size-probes; per_variant={per_variant:?}"
            );
        }
    }
}

/// Random seek+read cycles with PCM verification on `Audio<Stream<Hls>>`.
///
/// Scenario:
/// 1. Generate a file-like WAV or packaged FLAC/fMP4 saw-tooth fixture
/// 2. Create `Audio<Stream<Hls>>` with a matching media hint
/// 3. Verify duration
/// 4. 1000 random seeks with verification:
///    - Level 1: integrity (finite, range, L==R)
///    - Level 2: continuity (consecutive frames follow a pattern)
///    - Level 3: position (decoded phase ≈ expected phase)
/// 5. Final seek near the end → read to EOF
#[kithara::test(tokio, native, serial, timeout(Duration::from_secs(30)))]
#[cfg_attr(
    any(target_os = "macos", target_os = "ios"),
    case::wav_apple_ephemeral(true, DecoderBackend::Apple, SeekAudioFixture::WavFileLike)
)]
#[cfg(not(target_arch = "wasm32"))]
#[case::wav_symphonia_ephemeral(true, DecoderBackend::Symphonia, SeekAudioFixture::WavFileLike)]
#[case::wav_symphonia_mmap(false, DecoderBackend::Symphonia, SeekAudioFixture::WavFileLike)]
#[case::flac_fmp4_symphonia_ephemeral(true, DecoderBackend::Symphonia, SeekAudioFixture::FlacFmp4)]
#[case::flac_fmp4_symphonia_mmap(false, DecoderBackend::Symphonia, SeekAudioFixture::FlacFmp4)]
#[cfg_attr(
    any(target_os = "macos", target_os = "ios"),
    case::wav_apple_mmap(false, DecoderBackend::Apple, SeekAudioFixture::WavFileLike)
)]
async fn stress_seek_audio_hls(
    #[case] ephemeral: bool,
    #[case] backend: DecoderBackend,
    #[case] fixture: SeekAudioFixture,
) {
    #[cfg(any(target_os = "macos", target_os = "ios"))]
    kithara_integration_tests::apple_warmup::warm_if_apple(backend);
    let segment_duration = Consts::D.segment_size as f64
        / (f64::from(Consts::D.sample_rate) * f64::from(Consts::D.channels) * 2.0);
    let expected_dur =
        matches!(fixture, SeekAudioFixture::WavFileLike).then(Consts::expected_duration_secs);
    let (url, counter) = match fixture {
        SeekAudioFixture::WavFileLike => {
            let wav_data = create_wav_exact_bytes(
                signal::Sawtooth,
                Consts::D.sample_rate,
                Consts::D.channels,
                Consts::TOTAL_BYTES,
            );
            if let Some(expected_dur) = expected_dur {
                info!(
                    total_bytes = Consts::TOTAL_BYTES,
                    duration_secs = format!("{expected_dur:.2}"),
                    "Generated saw-tooth WAV"
                );
            }
            let server = HlsTestServer::new(HlsTestServerConfig {
                segments_per_variant: Consts::SEGMENT_COUNT,
                segment_size: Consts::D.segment_size,
                segment_duration_secs: segment_duration,
                custom_data: Some(Arc::new(wav_data)),
                ..Default::default()
            })
            .await;
            (
                server.url("/master.m3u8"),
                SizeProbeCounter::HlsServer(server),
            )
        }
        SeekAudioFixture::FlacFmp4 => {
            let helper = TestServerHelper::new().await;
            let created = helper
                .create_hls(
                    HlsFixtureBuilder::new()
                        .variant_count(Consts::VARIANT_COUNT)
                        .segments_per_variant(Consts::SEGMENT_COUNT)
                        .segment_duration_secs(segment_duration)
                        .packaged_audio_per_variant_pcm_flac(
                            Consts::D.sample_rate,
                            Consts::D.channels,
                            vec![PcmPattern::Ascending],
                        ),
                )
                .await
                .expect("create FLAC/fMP4 HLS fixture");
            let url = created.master_url();
            let token = created.token().to_owned();
            (
                url,
                SizeProbeCounter::Helper {
                    helper,
                    segments: Consts::SEGMENT_COUNT,
                    token,
                    variants: Consts::VARIANT_COUNT,
                },
            )
        }
    };

    info!(?fixture, %url, segments = Consts::SEGMENT_COUNT, "HLS server ready");

    let temp_dir = TestTempDir::new();
    let cancel = CancelToken::never();

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

    let config = AudioConfig::<Hls>::for_stream(hls_config)
        .media_info(fixture.media_info())
        .decoder_backend(backend)
        .block_on_underrun(true)
        .build();
    let mut audio = Audio::<Stream<Hls>>::new(config)
        .await
        .expect("create Audio<Stream<Hls>> pipeline");

    let total_duration = audio.duration().expect("WAV should report known duration");
    let total_secs = total_duration.as_secs_f64();
    info!(total_secs, expected_dur, "Stream duration");

    if let Some(expected_dur) = expected_dur {
        assert!(
            (total_secs - expected_dur).abs() < 1.0,
            "duration mismatch: expected ~{expected_dur:.1}, got {total_secs:.1}",
        );
    } else {
        assert!(
            total_secs > 1.0,
            "FLAC/fMP4 stream duration too short: {total_secs:.3}s"
        );
    }

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
        loop {
            match audio.read(&mut buf) {
                Ok(ReadOutcome::Pending { .. }) => {
                    panic!("final tail read returned Pending with block_on_underrun");
                }
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
                    break;
                }
                Err(e) => panic!("final tail read error: {e}"),
            }
        }

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
        Ok(()) => {
            assert_seek_size_probes(fixture, &counter);
            info!(?fixture, "Audio+HLS stress test passed");
        }
        Err(e) => panic!("spawn_blocking failed: {e}"),
    }
}
