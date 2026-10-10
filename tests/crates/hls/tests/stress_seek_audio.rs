use std::num::NonZeroUsize;

use kithara::{
    assets::{AssetStore, StorageBackend},
    audio::{AudioConfig, AudioControl, AudioRead, AudioSession, ReadOutcome},
    decode::DecoderBackend,
    hls::{AbrMode, Hls, HlsConfig},
    platform::{CancelToken, sync::Arc, time::Duration, tokio::task::spawn_blocking},
    play::{PlayWorker, PlayWorkerConfig},
    stream::{AudioCodec, ContainerFormat, MediaInfo, Stream},
};
use kithara_integration_tests::{
    CreatedHls, HlsFixtureBuilder, TestServerHelper,
    bufpool_ext::{TestPools, pools},
    fixture_protocol::PcmPattern,
    mock::LaneAudio,
    usdt_trace::{self, ProbeEvent},
};
#[cfg(not(target_arch = "wasm32"))]
use kithara_test_fixtures::hls_fixtures::{hls_sized_wav_forty_eight, hls_sized_wav_hundred};
use kithara_test_fixtures::signal;
use kithara_test_utils::{TestTempDir, Xorshift64};
use num_traits::AsPrimitive;
use tracing::info;

use crate::{
    common::test_defaults::SawWav,
    saw_chunk::{channel_mismatches, first_invalid_sample, phase_steps},
};

mod consts {
    use super::SawWav;

    pub(super) const D: SawWav = SawWav::DEFAULT;
    pub(super) const VARIANT_COUNT: usize = 1;
    pub(super) const SEEK_ITERATIONS: usize = 1000;
}

/// Total fixture byte size for a given segment count.
const fn total_bytes(segment_count: usize) -> usize {
    segment_count * consts::D.segment_size
}

/// Compute the expected duration in seconds for the generated WAV.
fn expected_duration_secs(segment_count: usize) -> f64 {
    let header_size = 44usize;
    let bytes_per_frame = consts::D.channels as usize * 2;
    let frame_count = (total_bytes(segment_count) - header_size) / bytes_per_frame;
    let frame_count: f64 = frame_count.as_();
    frame_count / f64::from(consts::D.sample_rate)
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

fn assert_seek_size_probes(fixture: SeekAudioFixture, helper: &TestServerHelper, hls: &CreatedHls) {
    let total = helper.total_size_probe_count(hls);
    let per_variant: Vec<u64> = (0..hls.spec().variant_count)
        .map(|variant| helper.variant_size_probe_count(hls, variant))
        .collect();
    info!(
        ?fixture,
        total,
        ?per_variant,
        "size-probe counts after seek stress"
    );
    match fixture {
        SeekAudioFixture::WavFileLike => {
            let bound =
                u64::try_from(hls.spec().segments_per_variant).expect("segment count fits u64");
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
    const fn record_acquire(&mut self) {
        self.total += 1;
        self.acquire += 1;
    }

    const fn record_open(&mut self) {
        self.total += 1;
        self.open += 1;
    }
}

/// Real asset-store backend opens during the run: each cache miss reaches
/// `EvictAssets::{acquire,open}_resource_with_ctx`, which fire the
/// `kithara_assets_probe` probe. Cache hits short-circuit above this layer.
fn asset_open_stats(events: &[ProbeEvent]) -> AssetOpenStats {
    let mut stats = AssetOpenStats::default();
    for event in events {
        if event.target != "kithara_assets_probe" {
            continue;
        }
        match event.probe {
            "acquire_resource_with_ctx" => stats.record_acquire(),
            "open_resource_with_ctx" => stats.record_open(),
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
    events: &[ProbeEvent],
) {
    let stats = asset_open_stats(events);
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
    let bound = segment_count + FULL_CACHE_OPEN_SLACK;
    if full_cache {
        assert!(
            opens <= bound,
            "full cache must open each segment ~once (no thrash): opens={opens}, \
             segment_count={segment_count}, bound={bound}, stats={stats:?}"
        );
    } else {
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

/// The three HLS layout/queue-reset probe counts at one instant in the run.
/// The per-seek-churn contract compares two of these to isolate steady-state
/// churn from one-time startup size resolution. The marker probes sit on the
/// three HLS layout/queue reset sites (`Frame::recompute`, `rebuild_queue`,
/// `reset_for_seek`) that should stay invariant across same-variant seeks on
/// a fully-cached single-variant in-memory track.
#[derive(Clone, Copy, Debug)]
struct ChurnSnapshot {
    recompute: usize,
    rebuild_queue: usize,
    reset_for_seek: usize,
}

/// Tallies the three `kithara_hls_probe` reset-site counts in one pass.
fn snapshot_hls_churn(events: &[ProbeEvent]) -> ChurnSnapshot {
    let mut snap = ChurnSnapshot {
        recompute: 0,
        rebuild_queue: 0,
        reset_for_seek: 0,
    };
    for event in events {
        if event.target != "kithara_hls_probe" {
            continue;
        }
        match event.probe {
            "recompute" => snap.recompute += 1,
            "rebuild_queue" => snap.rebuild_queue += 1,
            "reset_for_seek" => snap.reset_for_seek += 1,
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
        steady_seeks = consts::SEEK_ITERATIONS - WARMUP_K,
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
        consts::SEEK_ITERATIONS - WARMUP_K
    );
}

#[cfg(not(target_arch = "wasm32"))]
#[kithara::fixture]
async fn wav_hundred(hls_sized_wav_hundred: Vec<u8>) -> (TestServerHelper, CreatedHls) {
    wav_seek(hls_sized_wav_hundred, 100).await
}

#[cfg(not(target_arch = "wasm32"))]
#[kithara::fixture]
async fn wav_forty_eight(hls_sized_wav_forty_eight: Vec<u8>) -> (TestServerHelper, CreatedHls) {
    wav_seek(hls_sized_wav_forty_eight, 48).await
}

#[cfg(not(target_arch = "wasm32"))]
async fn wav_seek(wav_data: Vec<u8>, segment_count: usize) -> (TestServerHelper, CreatedHls) {
    let helper = TestServerHelper::new().await;
    let server = helper
        .create_hls(
            HlsFixtureBuilder::new()
                .segments_per_variant(segment_count)
                .segment_size(consts::D.segment_size)
                .segment_duration_secs(consts::D.segment_duration_secs())
                .codecs("wav".to_string())
                .custom_data(Arc::new(wav_data)),
        )
        .await
        .expect("create HLS fixture");
    (helper, server)
}

#[cfg(not(target_arch = "wasm32"))]
#[kithara::fixture]
async fn flac_hundred() -> (TestServerHelper, CreatedHls) {
    let helper = TestServerHelper::new().await;
    let created = helper
        .create_hls(
            HlsFixtureBuilder::new()
                .variant_count(consts::VARIANT_COUNT)
                .segments_per_variant(100)
                .segment_duration_secs(consts::D.segment_duration_secs())
                .packaged_audio_per_variant_pcm_flac(
                    consts::D.sample_rate,
                    consts::D.channels,
                    vec![PcmPattern::Ascending],
                ),
        )
        .await
        .expect("create FLAC/fMP4 HLS fixture");
    (helper, created)
}

type HlsAudio = LaneAudio<Stream<Hls<TestPools>>, TestPools>;

/// Per-read checks of the random seek loop, and what they found.
struct SeekCheck {
    channels: usize,
    sample_rate: u32,
    segment_count: usize,
    successful_reads: u64,
    total_samples_read: u64,
    channel_mismatches: u64,
    continuity_errors: u64,
    position_errors: u64,
    position_error_details: Vec<String>,
}

impl SeekCheck {
    const fn new(channels: usize, sample_rate: u32, segment_count: usize) -> Self {
        Self {
            channels,
            sample_rate,
            segment_count,
            successful_reads: 0,
            total_samples_read: 0,
            channel_mismatches: 0,
            continuity_errors: 0,
            position_errors: 0,
            position_error_details: Vec::new(),
        }
    }

    /// Check the chunk read right after seeking to `pos_secs`: valid samples,
    /// channels that agree, an ascending saw, and a first frame where the
    /// seek asked for.
    fn record(&mut self, iteration: usize, pos_secs: f64, chunk: &[f32]) {
        if let Some((j, sample)) = first_invalid_sample(chunk) {
            panic!("invalid sample at seek #{iteration} offset {j}: {sample} (pos {pos_secs:.4}s)");
        }

        if self.channels == 2 {
            for (frame, l, r) in channel_mismatches(chunk) {
                self.channel_mismatches += 1;
                if self.channel_mismatches <= 3 {
                    info!(iteration, frame, l, r, pos_secs, "Channel mismatch");
                }
            }
        }

        for (frame, prev_phase, curr_phase) in phase_steps(chunk, self.channels) {
            if signal::phase::delta(prev_phase, curr_phase) != 1 {
                self.continuity_errors += 1;
                if self.continuity_errors <= 3 {
                    info!(
                        iteration,
                        frame, prev_phase, curr_phase, pos_secs, "Continuity break"
                    );
                }
            }
        }

        self.check_landing(iteration, pos_secs, chunk[0]);

        self.successful_reads += 1;
        self.total_samples_read += chunk.len() as u64;
    }

    /// Count a seek whose first frame sits more than 1200 frames of saw away
    /// from the frame `pos_secs` names.
    fn check_landing(&mut self, iteration: usize, pos_secs: f64, first_sample: f32) {
        let expected_frame_idx =
            num_traits::cast::<f64, usize>((pos_secs * f64::from(self.sample_rate)).round())
                .unwrap_or(usize::MAX);
        let expected_phase = expected_frame_idx % signal::SAW_PERIOD;
        let actual_phase = signal::phase::units(first_sample);
        let dist = signal::phase::distance(actual_phase, expected_phase);
        if dist <= 1200 {
            return;
        }

        self.position_errors += 1;
        if self.position_error_details.len() < 10 {
            let bytes_per_frame = usize::from(consts::D.channels) * 2;
            let requested_byte = 44 + expected_frame_idx * bytes_per_frame;
            let segment_index =
                (requested_byte / consts::D.segment_size).min(self.segment_count - 1);
            self.position_error_details.push(format!(
                "#{iteration}: requested={pos_secs:.6}s expected_frame={expected_frame_idx} \
                 expected_phase={expected_phase} actual_phase={actual_phase} \
                 delta={dist} segment={segment_index}"
            ));
        }
        if self.position_errors <= 3 {
            info!(
                iteration,
                pos_secs,
                expected_frame_idx,
                expected_phase,
                actual_phase,
                dist,
                "Position mismatch"
            );
        }
    }

    fn log(&self, message: &str) {
        info!(
            successful_reads = self.successful_reads,
            total_samples_read = self.total_samples_read,
            channel_mismatches = self.channel_mismatches,
            continuity_errors = self.continuity_errors,
            position_errors = self.position_errors,
            "{message}"
        );
    }

    /// Every seek read back, with channels that never diverge and only a few
    /// breaks or misplaced landings.
    fn assert_within_tolerance(&self) {
        assert_eq!(self.successful_reads, consts::SEEK_ITERATIONS as u64);
        assert_eq!(
            self.channel_mismatches, 0,
            "L/R channel data diverged {} times - data corruption",
            self.channel_mismatches
        );
        if self.continuity_errors > 0 {
            tracing::warn!(
                continuity_errors = self.continuity_errors,
                "continuity breaks detected (within tolerance of 5)"
            );
        }
        assert!(
            self.continuity_errors <= 5,
            "{} continuity breaks (>5 tolerance) - decoder returned non-contiguous data",
            self.continuity_errors
        );
        if self.position_errors > 0 {
            tracing::warn!(
                position_errors = self.position_errors,
                "position mismatches detected (within tolerance of 3)"
            );
        }
        assert!(
            self.position_errors <= 3,
            "{} position mismatches (>3 tolerance) - seek landed in wrong place. \
             First mismatches (capped at 10):\n{}",
            self.position_errors,
            self.position_error_details.join("\n")
        );
    }
}

/// Seek to `SEEK_ITERATIONS` random positions and check the chunk each one
/// reads back. Returns the churn snapshot taken after `WARMUP_K` seeks.
fn random_seek_reads(
    audio: &mut HlsAudio,
    buf: &mut [f32],
    check: &mut SeekCheck,
    max_seek_secs: f64,
) -> ChurnSnapshot {
    let mut rng = Xorshift64::new(0xDEAD_BEEF_CAFE_1337);
    let seek_positions: Vec<f64> = (0..consts::SEEK_ITERATIONS)
        .map(|_| rng.range_f64(0.001, max_seek_secs))
        .collect();

    info!(
        count = seek_positions.len(),
        max_seek_secs, "Generated seek positions"
    );

    let mut warmup_churn = None;
    for (i, &pos_secs) in seek_positions.iter().enumerate() {
        audio
            .seek(Duration::from_secs_f64(pos_secs))
            .unwrap_or_else(|e| panic!("seek #{i} to {pos_secs:.4}s failed: {e}"));

        let n = match audio.read(buf) {
            Ok(ReadOutcome::Frames { count, .. }) => count.get(),
            Ok(ReadOutcome::Pending { .. }) => {
                panic!("read returned 0 after seek #{i} to {pos_secs:.4}s");
            }
            Ok(ReadOutcome::Eof { .. }) => {
                panic!("read returned Eof after seek #{i} to {pos_secs:.4}s");
            }
            Err(e) => panic!("read error after seek #{i}: {e}"),
        };
        check.record(i, pos_secs, &buf[..n]);

        if i + 1 == WARMUP_K {
            warmup_churn = Some(snapshot_hls_churn(&usdt_trace::events()));
        }
        if (i + 1) % 200 == 0 {
            check.log(&format!("Progress: iteration {}", i + 1));
        }
    }

    check.log(&format!(
        "All {} seek+read iterations done",
        consts::SEEK_ITERATIONS
    ));
    check.assert_within_tolerance();

    warmup_churn.expect("warmup churn snapshot captured (WARMUP_K < SEEK_ITERATIONS)")
}

/// A seek near the end reads valid samples through to EOF without ever
/// reporting `Pending`.
fn drain_tail_to_eof(audio: &mut HlsAudio, buf: &mut [f32], final_seek_secs: f64) {
    info!(final_seek_secs, "Final seek near end");

    audio
        .seek(Duration::from_secs_f64(final_seek_secs))
        .unwrap_or_else(|e| panic!("final seek to {final_seek_secs:.4}s failed: {e}"));

    let mut remaining_samples = 0u64;
    loop {
        match audio.read(buf) {
            Ok(ReadOutcome::Pending { .. }) => {
                panic!("final tail read returned Pending with block_on_underrun");
            }
            Ok(ReadOutcome::Frames { count, .. }) => {
                remaining_samples += count.get() as u64;
                assert!(
                    first_invalid_sample(&buf[..count.get()]).is_none(),
                    "invalid sample in final tail read",
                );
            }
            Ok(ReadOutcome::Eof { .. }) => break,
            Err(e) => panic!("final tail read error: {e}"),
        }
    }

    info!(remaining_samples, "Final read done - EOF confirmed");
}

/// After EOF, a seek back into the track reads samples again.
fn resume_after_eof(audio: &mut HlsAudio, buf: &mut [f32], total_secs: f64) {
    let resume_positions = [0.5_f64, total_secs * 0.25, total_secs * 0.75];
    for (i, pos_secs) in resume_positions.iter().copied().enumerate() {
        audio
            .seek(Duration::from_secs_f64(pos_secs))
            .unwrap_or_else(|e| panic!("seek-after-eof #{i} to {pos_secs:.4}s failed: {e}"));

        match audio.read(buf) {
            Ok(ReadOutcome::Frames { .. }) => {}
            Ok(other) => {
                panic!("seek-after-eof #{i} at {pos_secs:.4}s produced no samples: {other:?}");
            }
            Err(e) => panic!("seek-after-eof #{i} read error: {e}"),
        }
    }
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
#[cfg_attr(not(target_os = "android"), case::wav_symphonia_ephemeral(
    true,
    DecoderBackend::Symphonia,
    SeekAudioFixture::WavFileLike,
    100,
    Some(110),
    wav_hundred().await
))]
#[cfg_attr(target_os = "android", case::wav_android_ephemeral(
    true,
    DecoderBackend::default(),
    SeekAudioFixture::WavFileLike,
    100,
    Some(110),
    wav_hundred().await
))]
#[cfg_attr(not(target_os = "android"), case::wav_symphonia_mmap(
    false,
    DecoderBackend::Symphonia,
    SeekAudioFixture::WavFileLike,
    100,
    None,
    wav_hundred().await
))]
#[cfg_attr(target_os = "android", case::wav_android_mmap(
    false,
    DecoderBackend::default(),
    SeekAudioFixture::WavFileLike,
    100,
    None,
    wav_hundred().await
))]
#[cfg_attr(not(target_os = "android"), case::wav_symphonia_full_cache(
    false,
    DecoderBackend::Symphonia,
    SeekAudioFixture::WavFileLike,
    48,
    Some(56),
    wav_forty_eight().await
))]
#[cfg_attr(target_os = "android", case::wav_android_full_cache(
    false,
    DecoderBackend::default(),
    SeekAudioFixture::WavFileLike,
    48,
    Some(56),
    wav_forty_eight().await
))]
#[cfg_attr(not(target_os = "android"), case::flac_fmp4_symphonia_ephemeral(
    true,
    DecoderBackend::Symphonia,
    SeekAudioFixture::FlacFmp4,
    100,
    Some(110),
    flac_hundred().await
))]
#[cfg_attr(target_os = "android", case::flac_fmp4_android_ephemeral(
    true,
    DecoderBackend::default(),
    SeekAudioFixture::FlacFmp4,
    100,
    Some(110),
    flac_hundred().await
))]
#[cfg_attr(not(target_os = "android"), case::flac_fmp4_symphonia_mmap(
    false,
    DecoderBackend::Symphonia,
    SeekAudioFixture::FlacFmp4,
    100,
    None,
    flac_hundred().await
))]
#[cfg_attr(target_os = "android", case::flac_fmp4_android_mmap(
    false,
    DecoderBackend::default(),
    SeekAudioFixture::FlacFmp4,
    100,
    None,
    flac_hundred().await
))]
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
    #[case] prepared: (TestServerHelper, CreatedHls),
) {
    #[cfg(any(target_os = "macos", target_os = "ios"))]
    kithara_integration_tests::apple_warmup::warm_if_apple(backend);
    let expected_dur = matches!(fixture, SeekAudioFixture::WavFileLike)
        .then(|| expected_duration_secs(segment_count));
    let (helper, hls) = prepared;
    let url = hls.master_url();

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

    let config = kithara::play::TrackConfig::for_audio(
        AudioConfig::<Hls<TestPools>>::for_stream(hls_config)
            .media_info(fixture.media_info())
            .decoder(
                kithara::audio::AudioDecoderConfig::builder()
                    .backend(backend)
                    .build(),
            )
            .build(),
    )
    .block_on_underrun(true)
    .build();
    let trace = usdt_trace::scope();

    let mut audio = kithara_integration_tests::mock::load_audio(&worker, config)
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

        let mut buf = vec![0.0f32; chunk_samples];

        let max_seek_secs = total_secs - chunk_duration_secs;
        assert!(max_seek_secs > 0.0, "stream too short for chunk size");

        let mut check = SeekCheck::new(
            usize::from(spec.channels),
            spec.sample_rate.get(),
            segment_count,
        );
        let warmup_churn = random_seek_reads(&mut audio, &mut buf, &mut check, max_seek_secs);
        drain_tail_to_eof(&mut audio, &mut buf, max_seek_secs);
        resume_after_eof(&mut audio, &mut buf, total_secs);

        let end_churn = snapshot_hls_churn(&usdt_trace::events());
        (warmup_churn, end_churn)
    })
    .await;

    match result {
        Ok((warmup_churn, end_churn)) => {
            assert_seek_size_probes(fixture, &helper, &hls);
            assert_backend_open_count(
                fixture,
                segment_count,
                cache_capacity_override,
                &trace.events(),
            );
            let full_cache = cache_capacity_override.is_some_and(|cap| cap >= segment_count);
            if ephemeral && matches!(fixture, SeekAudioFixture::WavFileLike) && full_cache {
                assert_seek_churn_steady_state(warmup_churn, end_churn);
            }
            info!(?fixture, "Audio+HLS stress test passed");
        }
        Err(e) => panic!("spawn_blocking failed: {e}"),
    }
    drop(trace);
}
