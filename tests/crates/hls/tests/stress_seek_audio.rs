use std::num::NonZeroUsize;

use kithara::{
    assets::{AssetStore, StorageBackend},
    audio::{AudioConfig, AudioControl, AudioRead, AudioSession, ReadOutcome},
    decode::DecoderBackend,
    hls::{AbrMode, Hls, HlsConfig},
    platform::{CancelToken, sync::Arc, time::Duration, tokio::task::spawn_blocking},
    play::{PlayWorker, PlayWorkerConfig},
    stream::{AudioCodec, ContainerFormat, MediaInfo},
};
use kithara_integration_tests::{
    HlsFixtureBuilder, TestServerHelper, TestTempDir, Xorshift64,
    bufpool_ext::{TestPools, pools},
    fixture_protocol::PcmPattern,
    hls_server::{HlsTestServer, HlsTestServerConfig},
};
#[cfg(not(target_arch = "wasm32"))]
use kithara_test_fixtures::hls_fixtures::{hls_sized_wav_forty_eight, hls_sized_wav_hundred};
use kithara_test_fixtures::signal;
use tracing::info;
use url::Url;

use crate::common::test_defaults::SawWav;

struct Consts;
impl Consts {
    const D: SawWav = SawWav::DEFAULT;
    const VARIANT_COUNT: usize = 1;
    const SEEK_ITERATIONS: usize = 1000;

    /// Total fixture byte size for a given segment count.
    const fn total_bytes(segment_count: usize) -> usize {
        segment_count * Self::D.segment_size
    }

    /// Compute the expected duration in seconds for the generated WAV.
    fn expected_duration_secs(segment_count: usize) -> f64 {
        let header_size = 44usize;
        let bytes_per_frame = Self::D.channels as usize * 2;
        let frame_count = (Self::total_bytes(segment_count) - header_size) / bytes_per_frame;
        frame_count as f64 / f64::from(Self::D.sample_rate)
    }
}

/// Headroom over the segment count for full-cache open assertions: covers
/// the fMP4 init segment plus a few acquire-then-open double-touches before
/// the cache settles (observed overhead is ~+4..+5).
const FULL_CACHE_OPEN_SLACK: usize = 10;
#[derive(Clone, Copy, Debug)]
enum SeekAudioFixture {
    WavFileLike,
    FlacFmp4,
}

impl SeekAudioFixture {
    const fn media_info(self) -> MediaInfo {
        match self {
            Self::WavFileLike => MediaInfo::builder()
                .maybe_codec(Some(AudioCodec::Pcm))
                .maybe_container(Some(ContainerFormat::Wav))
                .build(),
            Self::FlacFmp4 => MediaInfo::builder()
                .maybe_codec(Some(AudioCodec::Flac))
                .maybe_container(Some(ContainerFormat::Fmp4))
                .build(),
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

    const fn variant_count(&self) -> usize {
        match self {
            Self::HlsServer(server) => server.config().variant_count,
            Self::Helper { variants, .. } => *variants,
        }
    }

    const fn segment_count(&self) -> usize {
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

#[cfg(not(target_arch = "wasm32"))]
#[kithara::fixture]
async fn wav_hundred(hls_sized_wav_hundred: Vec<u8>) -> (Url, SizeProbeCounter) {
    wav_seek(hls_sized_wav_hundred, 100).await
}

#[cfg(not(target_arch = "wasm32"))]
#[kithara::fixture]
async fn wav_forty_eight(hls_sized_wav_forty_eight: Vec<u8>) -> (Url, SizeProbeCounter) {
    wav_seek(hls_sized_wav_forty_eight, 48).await
}

#[cfg(not(target_arch = "wasm32"))]
async fn wav_seek(wav_data: Vec<u8>, segment_count: usize) -> (Url, SizeProbeCounter) {
    let server = HlsTestServer::new(HlsTestServerConfig {
        segments_per_variant: segment_count,
        segment_size: Consts::D.segment_size,
        segment_duration_secs: Consts::D.segment_duration_secs(),
        custom_data: Some(Arc::new(wav_data)),
        ..Default::default()
    })
    .await;
    (
        server.url("/master.m3u8"),
        SizeProbeCounter::HlsServer(server),
    )
}

#[cfg(not(target_arch = "wasm32"))]
#[kithara::fixture]
async fn flac_hundred() -> (Url, SizeProbeCounter) {
    let helper = TestServerHelper::new().await;
    let created = helper
        .create_hls(
            HlsFixtureBuilder::new()
                .variant_count(Consts::VARIANT_COUNT)
                .segments_per_variant(100)
                .segment_duration_secs(Consts::D.segment_duration_secs())
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
            segments: 100,
            token,
            variants: Consts::VARIANT_COUNT,
        },
    )
}

/// Random seek+read cycles with PCM verification on `Audio<Stream<Hls>>`.
///
/// Scenario:
/// 1. Receive a prepared file-like WAV or packaged FLAC/fMP4 saw-tooth fixture
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
    case::wav_apple_ephemeral(
        true,
        DecoderBackend::Apple,
        SeekAudioFixture::WavFileLike,
        100,
        Some(110),
        wav_hundred().await
    )
)]
#[cfg(not(target_arch = "wasm32"))]
#[case::wav_symphonia_ephemeral(
    true,
    DecoderBackend::Symphonia,
    SeekAudioFixture::WavFileLike,
    100,
    Some(110),
    wav_hundred().await
)]
#[case::wav_symphonia_mmap(
    false,
    DecoderBackend::Symphonia,
    SeekAudioFixture::WavFileLike,
    100,
    None,
    wav_hundred().await
)]
#[case::wav_symphonia_full_cache(
    false,
    DecoderBackend::Symphonia,
    SeekAudioFixture::WavFileLike,
    48,
    Some(56),
    wav_forty_eight().await
)]
#[case::flac_fmp4_symphonia_ephemeral(
    true,
    DecoderBackend::Symphonia,
    SeekAudioFixture::FlacFmp4,
    100,
    Some(110),
    flac_hundred().await
)]
#[case::flac_fmp4_symphonia_mmap(
    false,
    DecoderBackend::Symphonia,
    SeekAudioFixture::FlacFmp4,
    100,
    None,
    flac_hundred().await
)]
#[cfg_attr(
    any(target_os = "macos", target_os = "ios"),
    case::wav_apple_mmap(
        false,
        DecoderBackend::Apple,
        SeekAudioFixture::WavFileLike,
        100,
        None,
        wav_hundred().await
    )
)]
async fn stress_seek_audio_hls(
    #[case] ephemeral: bool,
    #[case] backend: DecoderBackend,
    #[case] fixture: SeekAudioFixture,
    #[case] segment_count: usize,
    #[case] cache_capacity_override: Option<usize>,
    #[case] prepared: (Url, SizeProbeCounter),
) {
    #[cfg(any(target_os = "macos", target_os = "ios"))]
    kithara_integration_tests::apple_warmup::warm_if_apple(backend);
    let expected_dur = matches!(fixture, SeekAudioFixture::WavFileLike)
        .then(|| Consts::expected_duration_secs(segment_count));
    let (url, counter) = prepared;

    info!(?fixture, %url, segments = segment_count, "HLS server ready");

    let temp_dir = TestTempDir::new();
    let cancel = CancelToken::never();
    let pools = pools();
    let worker = PlayWorker::new(
        PlayWorkerConfig::builder(pools.clone())
            .cancel(cancel.clone())
            .build(),
    );

    let storage_backend = if ephemeral {
        StorageBackend::Memory
    } else {
        StorageBackend::Disk {
            root: temp_dir.path().into(),
        }
    };
    let cache_capacity =
        cache_capacity_override.map(|cap| NonZeroUsize::new(cap).expect("nonzero cache capacity"));
    let store = AssetStore::builder(pools.clone())
        .backend(storage_backend)
        .maybe_cache_capacity(cache_capacity)
        .build();

    let hls_config = HlsConfig::for_url(url)
        .store(store)
        .pools(pools)
        .cancel(cancel)
        .initial_abr_mode(AbrMode::manual(0))
        .build();

    let config = AudioConfig::<Hls<TestPools>>::for_stream(hls_config)
        .media_info(fixture.media_info())
        .decoder(
            kithara::audio::AudioDecoderConfig::builder()
                .backend(backend)
                .build(),
        )
        .block_on_underrun(true)
        .build();

    let mut audio = worker
        .open(config)
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
        let chunk_samples = num_traits::cast::<f64, usize>(
            chunk_duration_secs * f64::from(spec.sample_rate.get()) * f64::from(spec.channels),
        )
        .unwrap_or(usize::MAX);
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
        let mut position_error_details: Vec<String> = Vec::new();

        let channels = spec.channels as usize;
        let bytes_per_frame = usize::from(Consts::D.channels) * 2;

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
                    let prev_phase = signal::phase::units(buf[(f - 1) * channels]);
                    let curr_phase = signal::phase::units(buf[f * channels]);
                    let expected_next = (prev_phase + 1) % signal::SAW_PERIOD;
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

            let expected_frame_idx = num_traits::cast::<f64, usize>(
                (pos_secs * f64::from(spec.sample_rate.get())).round(),
            )
            .unwrap_or(usize::MAX);
            let expected_phase = expected_frame_idx % signal::SAW_PERIOD;
            let actual_phase = signal::phase::units(buf[0]);
            let dist = signal::phase::distance(actual_phase, expected_phase);
            if dist > 1200 {
                position_errors += 1;
                if position_error_details.len() < 10 {
                    let requested_byte = 44 + expected_frame_idx * bytes_per_frame;
                    let segment_index =
                        (requested_byte / Consts::D.segment_size).min(segment_count - 1);
                    position_error_details.push(format!(
                        "#{i}: requested={pos_secs:.6}s expected_frame={expected_frame_idx} \
                         expected_phase={expected_phase} actual_phase={actual_phase} \
                         delta={dist} segment={segment_index}"
                    ));
                }
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
            "{position_errors} position mismatches (>3 tolerance) - seek landed in wrong place. \
             First mismatches (capped at 10):\n{}",
            position_error_details.join("\n")
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
