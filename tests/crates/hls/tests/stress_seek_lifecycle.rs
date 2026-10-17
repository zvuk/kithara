use std::num::NonZeroUsize;

use kithara::{
    abr::AbrHandle,
    assets::{AssetStore, StorageBackend},
    audio::{AudioConfig, AudioControl, AudioRead, AudioSession, ReadOutcome},
    hls::{Hls, HlsConfig},
    platform::{CancelToken, sync::Arc, time::Duration, tokio::task::spawn_blocking},
    play::{PlayWorker, PlayWorkerConfig},
    stream::{AudioCodec, ContainerFormat, MediaInfo, Stream},
};
use kithara_integration_tests::{
    CreatedHls, HlsFixtureBuilder, TestServerHelper, abr_fast, auto,
    bufpool_ext::{TestPools, pools},
    fixture_protocol::DelayRule,
    hls_test_helpers::pin_abr_variant,
    mock::LaneAudio,
};
#[cfg(not(target_arch = "wasm32"))]
use kithara_test_fixtures::hls_fixtures::{
    hls_header_forty, hls_pcm_forty, hls_pcm_forty_descending, hls_pcm_forty_shifted,
};
use kithara_test_fixtures::signal::{self, SignalDirection as Direction, detect_direction};
use kithara_test_utils::{TestTempDir, Xorshift64};
use num_traits::AsPrimitive;
use tracing::{info, warn};

use crate::{
    common::test_defaults::SawWav,
    saw_chunk::{channel_mismatches, first_invalid_sample, phase_steps},
};

mod consts {
    use super::SawWav;

    pub(super) const D: SawWav = SawWav::DEFAULT;
    pub(super) const SEGMENT_COUNT: usize = 40;
    pub(super) const VARIANT_COUNT: usize = 3;
    pub(super) const STRESS_SEEK_ITERATIONS: usize = 2000;
    pub(super) const MAX_ZERO_READS: usize = 50;
}

/// Read with retry: keeps trying until data arrives, a terminal signal is
/// reached (natural EOF or unrecoverable producer failure), or the retry
/// budget is exhausted. Returns (`samples_read`, `retries_needed`,
/// `saw_terminal`).
///
/// `saw_terminal = true` combines natural EOF (`Ok(ReadOutcome::Eof)`) and
/// terminal producer failure (`Err(DecodeError)`, i.e. `ConsumerPhase::Failed`).
/// Both are permanent for this `Audio` instance, so the stress test treats
/// them identically: end of this read loop. A terminal Err counts against
/// the `dead_seeks` tolerance budget (1% of `STRESS_SEEK_ITERATIONS`).
fn read_with_retry<R: AudioRead>(audio: &mut R, buf: &mut [f32]) -> (usize, usize, bool) {
    for retry in 0..consts::MAX_ZERO_READS {
        match audio.read(buf) {
            Ok(ReadOutcome::Frames { count, .. }) => return (count.get(), retry, false),
            Ok(ReadOutcome::Pending { .. }) => {}
            Ok(ReadOutcome::Eof { .. }) => return (0, retry, true),
            Err(e) => {
                warn!(
                    ?e,
                    "terminal producer failure under stress; counted as dead seek"
                );
                return (0, retry, true);
            }
        }
    }
    (0, consts::MAX_ZERO_READS, false)
}

/// Break sites carried into the continuity panic message.
///
/// A stress artifact keeps only the panic, and `info!` from a test binary never
/// reaches it, so a bare count cannot say whether the phase jumped once over a
/// segment gap or drifted everywhere.
const MAX_REPORTED_BREAKS: usize = 5;

/// Freeze the ladder on the variant phase 2 left active.
///
/// Every variant carries its own waveform so phase 1 can see a switch by
/// direction, and a promotion crossfades 20 ms across the join - 882 frames
/// at 44100 Hz that advance by neither `+1` nor `-1`. Phase 3 asks whether
/// the track reads back intact, not whether ABR still switches, so a
/// promotion inside it prints a correct blend as corruption: run
/// 33910610734 blended variant 1 into 2 at frame 1270656 in one attempt of
/// fifty, and every one of the 882 counted as a break. Pinning to the index
/// already active drops the pending decision and refuses every later target.
fn freeze_active_variant(abr: &AbrHandle) -> usize {
    let active = abr
        .current_variant_index()
        .expect("a registered ABR peer names its active variant");
    pin_abr_variant(abr, active);
    info!(variant = active, "ladder pinned for the integrity read");
    active
}

type HlsAudio = LaneAudio<Stream<Hls<TestPools>>, TestPools>;

/// Phase 1: read until the saw changes direction, the ABR switch.
fn warmup_until_switch(audio: &mut HlsAudio, buf: &mut [f32], channels: usize) {
    info!("Phase 1: warmup - reading until ABR switch");
    let mut initial_direction = Direction::Unknown;

    loop {
        let (n, _, _) = read_with_retry(audio, buf);
        if n == 0 {
            break;
        }
        let dir = detect_direction(&buf[..n], channels);
        if dir == Direction::Unknown {
            continue;
        }
        if initial_direction == Direction::Unknown {
            initial_direction = dir;
            info!(?dir, "Initial direction detected");
        } else if dir != initial_direction {
            info!(
                from = ?initial_direction,
                to = ?dir,
                "ABR switch detected"
            );
            return;
        }
    }

    warn!("ABR switch not detected during warmup - continuing anyway");
}

/// Where a rapid seek lands: a tenth near the start, a tenth near the end,
/// the rest anywhere.
fn seek_target(rng: &mut Xorshift64, max_seek_secs: f64) -> f64 {
    let r = rng.next_f64();
    if r < 0.1 {
        rng.range_f64(0.0, 1.0)
    } else if r < 0.2 {
        rng.range_f64(max_seek_secs - 2.0, max_seek_secs)
    } else {
        rng.range_f64(0.001, max_seek_secs)
    }
}

/// What the rapid seeks ran into.
#[derive(Debug, Default)]
struct SeekStress {
    dead_seeks: u64,
    total_retries: u64,
    max_retries_single: usize,
    integrity_errors: u64,
    channel_mismatches: u64,
}

impl SeekStress {
    /// Seek to `pos_secs`, read one chunk, and count what the pair ran into.
    fn seek_and_read(
        &mut self,
        audio: &mut HlsAudio,
        buf: &mut [f32],
        iteration: usize,
        pos_secs: f64,
        channels: usize,
    ) {
        if let Err(e) = audio.seek(Duration::from_secs_f64(pos_secs)) {
            warn!(iteration, pos_secs, ?e, "seek failed");
            self.dead_seeks += 1;
            return;
        }

        let (n, retries, saw_eof) = read_with_retry(audio, buf);
        self.total_retries += retries as u64;
        self.max_retries_single = self.max_retries_single.max(retries);

        if n == 0 {
            self.dead_seeks += 1;
            if self.dead_seeks <= 5 {
                warn!(
                    iteration,
                    pos_secs,
                    is_eof = saw_eof,
                    retries,
                    "STUCK: read returned 0 after {} retries",
                    consts::MAX_ZERO_READS
                );
            }
            return;
        }

        self.check_chunk(iteration, pos_secs, &buf[..n], channels);
    }

    /// Count a chunk with a sample outside `[-1, 1]`, or with stereo channels
    /// that differ, once each.
    fn check_chunk(&mut self, iteration: usize, pos_secs: f64, chunk: &[f32], channels: usize) {
        if let Some((offset, sample)) = first_invalid_sample(chunk) {
            self.integrity_errors += 1;
            if self.integrity_errors <= 3 {
                warn!(iteration, offset, sample, pos_secs, "bad sample");
            }
        }
        if channels == 2 && channel_mismatches(chunk).next().is_some() {
            self.channel_mismatches += 1;
        }
    }
}

/// Phase 2: rapid random seeks each produce sound data, and at most one in a
/// hundred produces none.
fn rapid_random_seeks(audio: &mut HlsAudio, buf: &mut [f32], channels: usize, max_seek_secs: f64) {
    info!(
        "Phase 2: {} rapid random seeks",
        consts::STRESS_SEEK_ITERATIONS
    );
    let mut rng = Xorshift64::new(0xCAFE_BABE_DEAD_BEEF);
    let mut stress = SeekStress::default();

    for i in 0..consts::STRESS_SEEK_ITERATIONS {
        let pos_secs = seek_target(&mut rng, max_seek_secs);
        stress.seek_and_read(audio, buf, i, pos_secs, channels);

        if (i + 1) % 500 == 0 {
            info!(iteration = i + 1, ?stress, "Progress");
        }
    }

    info!(?stress, "Phase 2 complete");

    let max_dead = (consts::STRESS_SEEK_ITERATIONS as u64) / 100;
    assert!(
        stress.dead_seeks <= max_dead,
        "too many dead seeks: {}/{} (>{max_dead} = 1% threshold) - pipeline stalls after seek",
        stress.dead_seeks,
        consts::STRESS_SEEK_ITERATIONS
    );
    assert_eq!(
        stress.integrity_errors, 0,
        "integrity errors: samples outside [-1,1] or not finite"
    );
    assert_eq!(
        stress.channel_mismatches, 0,
        "L/R channel mismatches - data corruption"
    );
}

/// Panic on the first sample outside `[-1, 1]`, or the first stereo frame
/// whose channels differ, naming its frame in the whole read.
fn assert_chunk_sound(chunk: &[f32], channels: usize, start_frame: u64) {
    if let Some((j, sample)) = first_invalid_sample(chunk) {
        panic!(
            "invalid sample at frame {} (total_frames_read={start_frame}): {sample}",
            start_frame + (j / channels) as u64
        );
    }
    if channels == 2
        && let Some((f, l, r)) = channel_mismatches(chunk).next()
    {
        panic!(
            "L/R mismatch at frame {}: L={l}, R={r}",
            start_frame + f as u64
        );
    }
}

/// Saw continuity across a whole read, chunk after chunk.
#[derive(Debug, Default)]
struct SawContinuity {
    breaks: u64,
    first_breaks: Vec<String>,
    last_phase: Option<usize>,
}

impl SawContinuity {
    /// Check a chunk that starts at `start_frame`: its first frame against the
    /// previous chunk's last, and every frame against the one before it.
    fn record(&mut self, chunk: &[f32], channels: usize, start_frame: u64) {
        let first_phase = signal::phase::units(chunk[0]);
        if let Some(prev) = self.last_phase {
            self.check("inter-chunk", start_frame, prev, first_phase);
        }
        for (f, prev, curr) in phase_steps(chunk, channels) {
            self.check("intra-chunk", start_frame + f as u64, prev, curr);
        }
        if let Some(frame) = chunk.chunks_exact(channels).last() {
            self.last_phase = Some(signal::phase::units(frame[0]));
        }
    }

    /// Count a step that moves the saw by other than one frame, keeping the
    /// first few sites for the panic message.
    fn check(&mut self, kind: &str, frame: u64, prev: usize, curr: usize) {
        if signal::phase::distance(prev, curr) == 1 {
            return;
        }
        self.breaks += 1;
        if self.first_breaks.len() < MAX_REPORTED_BREAKS {
            let next_asc = (prev + 1) % signal::SAW_PERIOD;
            let next_desc = (prev + signal::SAW_PERIOD - 1) % signal::SAW_PERIOD;
            self.first_breaks.push(format!(
                "{kind}@{frame}: {prev}->{curr} (expected {next_asc} or {next_desc})"
            ));
        }
    }
}

/// Phase 3: from zero, the whole track reads back intact on one pinned
/// variant.
fn full_track_integrity(audio: &mut HlsAudio, buf: &mut [f32], channels: usize) {
    info!("Phase 3: seek to 0 - full track integrity verification");

    let abr = audio
        .abr_handle()
        .expect("an HLS ladder must expose its ABR handle");
    let pinned = freeze_active_variant(&abr);

    audio.seek(Duration::ZERO).expect("seek to 0 must succeed");

    let mut total_frames_read = 0u64;
    let mut continuity = SawContinuity::default();
    let mut read_attempts = 0u64;
    let max_read_attempts = 100_000u64;

    loop {
        let (n, retries, saw_eof) = read_with_retry(audio, buf);
        read_attempts += 1;

        if n == 0 {
            if saw_eof {
                break;
            }
            assert!(
                retries < consts::MAX_ZERO_READS,
                "STUCK at position {:.3}s after seek to 0: \
                 read returned 0 after {} retries, \
                 total_frames_read={}",
                audio.position().as_secs_f64(),
                consts::MAX_ZERO_READS,
                total_frames_read,
            );
            continue;
        }

        let chunk = &buf[..n];
        assert_chunk_sound(chunk, channels, total_frames_read);
        continuity.record(chunk, channels, total_frames_read);
        total_frames_read += (n / channels) as u64;

        assert!(
            read_attempts <= max_read_attempts,
            "exceeded {max_read_attempts} read attempts in phase 3, \
             total_frames_read={total_frames_read} - possible infinite loop"
        );
    }

    let expected_frames =
        (consts::SEGMENT_COUNT * consts::D.segment_size) / (usize::from(consts::D.channels) * 2);
    let frame_diff = total_frames_read.abs_diff(expected_frames as u64);
    let tolerance = (expected_frames as u64) / 50;

    info!(
        total_frames_read,
        expected_frames,
        frame_diff,
        tolerance,
        continuity_breaks = continuity.breaks,
        "Phase 3 complete"
    );

    assert!(
        frame_diff <= tolerance,
        "frame count mismatch after seek-to-0: got {total_frames_read}, expected \
         ~{expected_frames} (+-{tolerance})"
    );

    assert_eq!(
        abr.current_variant_index(),
        Some(pinned),
        "the ladder moved during the integrity read, so the phase check below \
         is reading a crossfade rather than one variant's waveform"
    );

    let max_breaks = 10u64;
    assert!(
        continuity.breaks <= max_breaks,
        "too many continuity breaks after seek-to-0: {} (>{} tolerance) \
         - data corruption or segment gap; total_frames_read={}, \
         expected_frames={}, first breaks: {:?}",
        continuity.breaks,
        max_breaks,
        total_frames_read,
        expected_frames,
        continuity.first_breaks
    );
}

/// Aggressive lifecycle stress test with 3 ABR variants, 2000 seeks,
/// and full-track integrity verification after seek-to-zero.
#[kithara::fixture]
async fn audio_server(
    hls_header_forty: Vec<u8>,
    hls_pcm_forty: Vec<u8>,
    hls_pcm_forty_descending: Vec<u8>,
    hls_pcm_forty_shifted: Vec<u8>,
) -> CreatedHls {
    let init_segment = Arc::new(hls_header_forty);
    let v0_pcm = Arc::new(hls_pcm_forty);
    let v1_pcm = Arc::new(hls_pcm_forty_descending);
    let v2_pcm = Arc::new(hls_pcm_forty_shifted);

    let segment_duration = consts::D.segment_duration_secs();
    let segments: f64 = consts::SEGMENT_COUNT.as_();
    let total_secs = segment_duration * segments;

    info!(
        segments = consts::SEGMENT_COUNT,
        variants = consts::VARIANT_COUNT,
        segment_duration,
        total_secs = format!("{total_secs:.2}"),
        "Test data generated"
    );

    TestServerHelper::new()
        .await
        .create_hls(
            HlsFixtureBuilder::new()
                .variant_count(consts::VARIANT_COUNT)
                .segments_per_variant(consts::SEGMENT_COUNT)
                .segment_size(consts::D.segment_size)
                .segment_duration_secs(segment_duration)
                .custom_data_per_variant(vec![
                    Arc::clone(&v0_pcm),
                    Arc::clone(&v1_pcm),
                    Arc::clone(&v2_pcm),
                ])
                .init_data_per_variant(vec![
                    Arc::clone(&init_segment),
                    Arc::clone(&init_segment),
                    Arc::clone(&init_segment),
                ])
                .variant_bandwidths(vec![5_000_000, 1_000_000, 500_000])
                .delay_rules(vec![DelayRule {
                    variant: Some(0),
                    segment_gte: Some(3),
                    delay_ms: 500,
                    ..Default::default()
                }]),
        )
        .await
        .expect("create HLS fixture")
}

#[kithara::test(
    tokio,
    native,
    serial,
    timeout(Duration::from_secs(60)),
    hang_timeout_secs(5),
    tracing("kithara_audio=debug,kithara_decode=debug,kithara_hls=debug,kithara_stream=debug")
)]
#[case::ephemeral(true)]
#[cfg(not(target_arch = "wasm32"))]
#[case::mmap(false)]
async fn stress_seek_lifecycle_with_zero_reset(
    #[future(awt)] audio_server: CreatedHls,
    #[case] ephemeral: bool,
    abr_fast: kithara::abr::AbrSettings,
) {
    let server = audio_server;
    let segment_duration = server.spec().segment_duration_secs;
    let segments: f64 = consts::SEGMENT_COUNT.as_();
    let total_secs = segment_duration * segments;
    let url = server.master_url();
    info!(%url, "HLS server ready");

    let temp_dir = TestTempDir::new();
    let cancel = CancelToken::never();
    let pools = pools();
    let worker = PlayWorker::new(
        PlayWorkerConfig::builder(pools.clone())
            .cancel(cancel.clone())
            .build(),
    );

    let store = if ephemeral {
        let cap =
            NonZeroUsize::new(consts::SEGMENT_COUNT * consts::VARIANT_COUNT + 20).expect("nz");
        AssetStore::builder(pools.clone())
            .backend(StorageBackend::Memory)
            .cache_capacity(cap)
            .build()
    } else {
        AssetStore::builder(pools.clone())
            .backend(StorageBackend::Disk {
                root: temp_dir.path().to_path_buf(),
            })
            .build()
    };

    let hls_config = HlsConfig::for_url(url)
        .store(store)
        .pools(pools)
        .cancel(cancel)
        .initial_abr_mode(auto(0))
        .build();
    let _ = &abr_fast;

    let wav_info = MediaInfo::builder()
        .maybe_codec(Some(AudioCodec::Pcm))
        .maybe_container(Some(ContainerFormat::Wav))
        .build();
    let config = kithara::play::TrackConfig::for_audio(
        AudioConfig::<Hls<TestPools>>::for_stream(hls_config)
            .media_info(wav_info)
            .build(),
    )
    .block_on_underrun(true)
    .build();
    let mut audio = kithara_integration_tests::mock::load_audio(&worker, config)
        .await
        .expect("create Audio pipeline");

    let spec = audio.spec();
    info!(
        sample_rate = spec.sample_rate,
        channels = spec.channels,
        "Audio pipeline created"
    );

    let result = spawn_blocking(move || {
        let channels = usize::from(spec.channels);
        let chunk_samples = num_traits::cast::<f64, usize>(
            0.05 * f64::from(spec.sample_rate.get()) * f64::from(spec.channels),
        )
        .unwrap_or(usize::MAX);
        let mut buf = vec![0.0f32; chunk_samples];

        warmup_until_switch(&mut audio, &mut buf, channels);
        rapid_random_seeks(&mut audio, &mut buf, channels, total_secs - 0.1);
        full_track_integrity(&mut audio, &mut buf, channels);

        info!("All phases passed");
    })
    .await;

    match result {
        Ok(()) => info!("Lifecycle stress test passed"),
        Err(e) => panic!("spawn_blocking failed: {e}"),
    }
}
