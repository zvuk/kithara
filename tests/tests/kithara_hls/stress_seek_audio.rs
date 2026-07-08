use std::{num::NonZeroUsize, sync::Arc};

use kithara::{
    assets::{StorageBackend, StoreOptions},
    audio::{Audio, AudioConfig, ReadOutcome},
    decode::DecoderBackend,
    hls::{AbrMode, Hls, HlsConfig},
    platform::{CancelToken, time::Duration, tokio::task::spawn_blocking},
    stream::{AudioCodec, ContainerFormat, MediaInfo, Stream},
};
use kithara_integration_tests::{
    HlsFixtureBuilder, TestServerHelper, TestTempDir, Xorshift64, create_wav_exact_bytes,
    fixture_protocol::PcmPattern,
    hls_server::{HlsTestServer, HlsTestServerConfig},
    phase_distance, phase_from_f32,
    signal_pcm::signal,
};
use kithara_test_utils::probe::capture::{Recorder, install as install_recorder};
use tracing::info;

use crate::common::test_defaults::SawWav;

struct Consts;
impl Consts {
    const D: SawWav = SawWav::DEFAULT;
    const VARIANT_COUNT: usize = 1;
    const SEEK_ITERATIONS: usize = 1000;

    /// Total fixture byte size for a given segment count.
    fn total_bytes(segment_count: usize) -> usize {
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

#[derive(Debug, Default)]
struct AssetOpenStats {
    total: usize,
    acquire: usize,
    open: usize,
}

impl AssetOpenStats {
    fn record_acquire(&mut self) {
        self.total += 1;
        self.acquire += 1;
    }

    fn record_open(&mut self) {
        self.total += 1;
        self.open += 1;
    }
}

/// Real asset-store backend opens during the run: each cache miss reaches
/// `EvictAssets::{acquire,open}_resource_with_ctx`, which fire the
/// `kithara_assets_probe` probe. Cache hits short-circuit above this layer.
fn asset_open_stats(recorder: &Recorder) -> AssetOpenStats {
    let mut stats = AssetOpenStats::default();
    for event in recorder.snapshot() {
        if event.target != "kithara_assets_probe" {
            continue;
        }
        match event.probe_name() {
            Some("acquire_resource_with_ctx") => stats.record_acquire(),
            Some("open_resource_with_ctx") => stats.record_open(),
            _ => {}
        }
    }
    stats
}

/// Open-count contract. With a cache sized to hold every segment, each
/// backend resource is opened ~once for the whole run regardless of the
/// 1000 seek iterations (no re-open thrash). With a capped cache over a
/// larger track, aggregate opens are schedule-sensitive because background
/// prefetch and frequency eviction can legitimately displace mmap handles
/// before later random seeks reuse them. The deterministic capped-cache
/// contract is therefore write-side: segment acquisition must stay near one
/// backend open per resource, proving capped read-handle churn does not turn
/// into re-fetch/re-acquire thrash.
fn assert_backend_open_count(
    fixture: SeekAudioFixture,
    segment_count: usize,
    cache_capacity_override: Option<usize>,
    recorder: &Recorder,
) {
    let stats = asset_open_stats(recorder);
    let opens = stats.total;
    let full_cache = cache_capacity_override.is_some_and(|cap| cap >= segment_count);
    info!(
        ?fixture,
        opens,
        acquire_opens = stats.acquire,
        read_opens = stats.open,
        segment_count,
        ?cache_capacity_override,
        full_cache,
        "asset-store backend opens during seek stress"
    );
    if full_cache {
        let bound = segment_count + FULL_CACHE_OPEN_SLACK;
        assert!(
            opens <= bound,
            "full cache must open each segment ~once (no thrash): opens={opens}, \
             segment_count={segment_count}, bound={bound}"
        );
    } else {
        let bound = segment_count + FULL_CACHE_OPEN_SLACK;
        assert!(
            stats.acquire <= bound,
            "capped cache must not re-acquire segment resources repeatedly: \
             acquire_opens={}, read_opens={}, total_opens={}, bound={} \
             (segment_count={segment_count})",
            stats.acquire,
            stats.open,
            stats.total,
            bound
        );
    }
}

/// Snapshot of the three HLS layout/queue-reset probe counts at one instant
/// in the run. The per-seek-churn contract compares two of these to isolate
/// steady-state churn from one-time startup size resolution. The marker
/// probes sit on the three HLS layout/queue reset sites (`Frame::recompute`,
/// `rebuild_queue`, `reset_for_seek`) that should stay invariant across
/// same-variant seeks on a fully-cached single-variant in-memory track.
#[derive(Clone, Copy, Debug)]
struct ChurnSnapshot {
    recompute: usize,
    rebuild_queue: usize,
    reset_for_seek: usize,
}

/// Tally all three `kithara_hls_probe` reset-site counts from a SINGLE
/// recorder snapshot. The recorder buffer holds 100k+ probe events by the
/// end of a 1000-seek run and `Recorder::snapshot` clones it whole, so this
/// counts the three probe names in one pass rather than re-cloning the buffer
/// once per name. Counts are identical to per-name filtering.
fn snapshot_hls_churn(recorder: &Recorder) -> ChurnSnapshot {
    let mut snap = ChurnSnapshot {
        recompute: 0,
        rebuild_queue: 0,
        reset_for_seek: 0,
    };
    for event in recorder.snapshot() {
        if event.target != "kithara_hls_probe" {
            continue;
        }
        match event.probe_name() {
            Some("recompute") => snap.recompute += 1,
            Some("rebuild_queue") => snap.rebuild_queue += 1,
            Some("reset_for_seek") => snap.reset_for_seek += 1,
            _ => {}
        }
    }
    snap
}

/// Warmup seek index for the steady-state churn contract. The residual
/// `recompute`/`rebuild_queue` counts are one-time STARTUP size resolutions:
/// each of the ~100 placeholder segment estimates is replaced by its exact
/// byte size the first time a seek touches it, which genuinely shifts the
/// offset table once. Coupon-collector over 100 segments needs ~460 random
/// seeks to touch them all, so a warmup in the final third (index 800) lands
/// well after the cache has fully resolved. The delta over the remaining
/// `SEEK_ITERATIONS - WARMUP_K` seeks is the true per-seek invariant.
const WARMUP_K: usize = 800;

/// Slack on the steady-state delta. After warmup every segment size is exact,
/// the layout is canonical, and the fetch plan is satisfied, so all three
/// reset sites are gated off — the delta is ~0. Kept small enough that a
/// regression to even ~2/seek churn (which would add `2 * (SEEK_ITERATIONS -
/// WARMUP_K)` ≈ 400) fails loudly.
const STEADY_STATE_SLACK: usize = 8;

/// Steady-state per-seek-churn contract. On a fully-cached single-variant
/// in-memory track the HLS byte-offset table, the fetch queue, and the seek
/// reset are all INVARIANT between seeks once the cache has fully resolved:
/// nothing is downloaded, no variant flips, every segment size is known.
/// The residual counts captured at `warmup` are legitimate one-time startup
/// size resolutions; only the DELTA over the final seeks must stay ~0.
fn assert_seek_churn_steady_state(warmup: ChurnSnapshot, end: ChurnSnapshot) {
    let d_recompute = end.recompute - warmup.recompute;
    let d_rebuild = end.rebuild_queue - warmup.rebuild_queue;
    let d_reset = end.reset_for_seek - warmup.reset_for_seek;
    info!(
        startup_recompute = warmup.recompute,
        startup_rebuild_queue = warmup.rebuild_queue,
        startup_reset_for_seek = warmup.reset_for_seek,
        delta_recompute = d_recompute,
        delta_rebuild_queue = d_rebuild,
        delta_reset_for_seek = d_reset,
        warmup_k = WARMUP_K,
        steady_seeks = Consts::SEEK_ITERATIONS - WARMUP_K,
        "HLS startup vs steady-state layout/queue churn"
    );
    let worst = d_recompute.max(d_rebuild).max(d_reset);
    assert!(
        worst <= STEADY_STATE_SLACK,
        "fully-cached single-variant in-memory seeks must not rebuild HLS \
         layout/queue per seek once the cache has resolved: \
         delta_recompute={d_recompute}, delta_rebuild_queue={d_rebuild}, \
         delta_reset_for_seek={d_reset} over the final {} seeks \
         (warmup_k={WARMUP_K}, steady slack={STEADY_STATE_SLACK})",
        Consts::SEEK_ITERATIONS - WARMUP_K
    );
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
    case::wav_apple_ephemeral(
        true,
        DecoderBackend::Apple,
        SeekAudioFixture::WavFileLike,
        100,
        Some(110)
    )
)]
#[cfg(not(target_arch = "wasm32"))]
#[case::wav_symphonia_ephemeral(
    true,
    DecoderBackend::Symphonia,
    SeekAudioFixture::WavFileLike,
    100,
    Some(110)
)]
#[case::wav_symphonia_mmap(
    false,
    DecoderBackend::Symphonia,
    SeekAudioFixture::WavFileLike,
    100,
    None
)]
#[case::wav_symphonia_full_cache(
    false,
    DecoderBackend::Symphonia,
    SeekAudioFixture::WavFileLike,
    48,
    Some(56)
)]
#[case::flac_fmp4_symphonia_ephemeral(
    true,
    DecoderBackend::Symphonia,
    SeekAudioFixture::FlacFmp4,
    100,
    Some(110)
)]
#[case::flac_fmp4_symphonia_mmap(
    false,
    DecoderBackend::Symphonia,
    SeekAudioFixture::FlacFmp4,
    100,
    None
)]
#[cfg_attr(
    any(target_os = "macos", target_os = "ios"),
    case::wav_apple_mmap(false, DecoderBackend::Apple, SeekAudioFixture::WavFileLike, 100, None)
)]
async fn stress_seek_audio_hls(
    #[case] ephemeral: bool,
    #[case] backend: DecoderBackend,
    #[case] fixture: SeekAudioFixture,
    #[case] segment_count: usize,
    #[case] cache_capacity_override: Option<usize>,
) {
    #[cfg(any(target_os = "macos", target_os = "ios"))]
    kithara_integration_tests::apple_warmup::warm_if_apple(backend);
    let segment_duration = Consts::D.segment_size as f64
        / (f64::from(Consts::D.sample_rate) * f64::from(Consts::D.channels) * 2.0);
    let expected_dur = matches!(fixture, SeekAudioFixture::WavFileLike)
        .then(|| Consts::expected_duration_secs(segment_count));
    let (url, counter) = match fixture {
        SeekAudioFixture::WavFileLike => {
            let wav_data = create_wav_exact_bytes(
                signal::Sawtooth,
                Consts::D.sample_rate,
                Consts::D.channels,
                Consts::total_bytes(segment_count),
            );
            if let Some(expected_dur) = expected_dur {
                info!(
                    total_bytes = Consts::total_bytes(segment_count),
                    duration_secs = format!("{expected_dur:.2}"),
                    "Generated saw-tooth WAV"
                );
            }
            let server = HlsTestServer::new(HlsTestServerConfig {
                segments_per_variant: segment_count,
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
                        .segments_per_variant(segment_count)
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
                    segments: segment_count,
                    token,
                    variants: Consts::VARIANT_COUNT,
                },
            )
        }
    };

    info!(?fixture, %url, segments = segment_count, "HLS server ready");

    let temp_dir = TestTempDir::new();
    let cancel = CancelToken::never();

    let mut store = StoreOptions::new(temp_dir.path());
    if ephemeral {
        store.backend = StorageBackend::Memory;
    }
    if let Some(cap) = cache_capacity_override {
        store.cache_capacity = Some(NonZeroUsize::new(cap).expect("nonzero cache capacity"));
    }

    let hls_config = HlsConfig::for_url(url)
        .store(store)
        .cancel(cancel)
        .initial_abr_mode(AbrMode::manual(0))
        .build();

    let config = AudioConfig::<Hls>::for_stream(hls_config)
        .media_info(fixture.media_info())
        .decoder(
            kithara::audio::AudioDecoderConfig::builder()
                .backend(backend)
                .build(),
        )
        .block_on_underrun(true)
        .build();
    let recorder = install_recorder();

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

    let churn_recorder = recorder.clone();
    let result = spawn_blocking(move || {
        let mut warmup_churn: Option<ChurnSnapshot> = None;
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

            if i + 1 == WARMUP_K {
                warmup_churn = Some(snapshot_hls_churn(&churn_recorder));
            }

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

        let end_churn = snapshot_hls_churn(&churn_recorder);

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

        let warmup_churn =
            warmup_churn.expect("warmup churn snapshot captured (WARMUP_K < SEEK_ITERATIONS)");
        (warmup_churn, end_churn)
    })
    .await;

    match result {
        Ok((warmup_churn, end_churn)) => {
            assert_seek_size_probes(fixture, &counter);
            assert_backend_open_count(fixture, segment_count, cache_capacity_override, &recorder);
            let full_cache = cache_capacity_override.is_some_and(|cap| cap >= segment_count);
            if ephemeral && matches!(fixture, SeekAudioFixture::WavFileLike) && full_cache {
                assert_seek_churn_steady_state(warmup_churn, end_churn);
            }
            info!(?fixture, "Audio+HLS stress test passed");
        }
        Err(e) => panic!("spawn_blocking failed: {e}"),
    }
}
