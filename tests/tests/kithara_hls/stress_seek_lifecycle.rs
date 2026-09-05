use std::num::NonZeroUsize;

use kithara::{
    abr::AbrHandle,
    assets::{AssetStore, StorageBackend},
    audio::{AudioConfig, AudioControl, AudioRead, AudioSession, ReadOutcome},
    hls::{Hls, HlsConfig},
    platform::{CancelToken, sync::Arc, time::Duration, tokio::task::spawn_blocking},
    play::{PlayWorker, PlayWorkerConfig},
    stream::{AudioCodec, ContainerFormat, MediaInfo},
};
use kithara_integration_tests::{
    TestTempDir, Xorshift64, abr_fast, auto,
    bufpool_ext::{TestPools, pools},
    fixture_protocol::DelayRule,
    hls_server::{HlsTestServer, HlsTestServerConfig},
    hls_test_helpers::pin_abr_variant,
};
use kithara_test_fixtures::signal::{
    self, Pcm, SignalDirection as Direction, Wave, detect_direction,
};
use tracing::{info, warn};

use crate::common::test_defaults::{SawWav, frames_in_segments};

struct Consts;
impl Consts {
    const D: SawWav = SawWav::DEFAULT;
    const SEGMENT_COUNT: usize = 40;
    const VARIANT_COUNT: usize = 3;
    const STRESS_SEEK_ITERATIONS: usize = 2000;
    const MAX_ZERO_READS: usize = 50;
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
    for retry in 0..Consts::MAX_ZERO_READS {
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
    (0, Consts::MAX_ZERO_READS, false)
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

/// Aggressive lifecycle stress test with 3 ABR variants, 2000 seeks,
/// and full-track integrity verification after seek-to-zero.
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
    #[case] ephemeral: bool,
    abr_fast: kithara::abr::AbrSettings,
) {
    let init_segment = Arc::new(signal::header(
        Consts::D.sample_rate,
        Consts::D.channels,
        None,
    ));
    let v0_pcm = Arc::new(Vec::from(Pcm::new(
        Consts::D.sample_rate,
        Consts::D.channels,
        frames_in_segments(
            Consts::SEGMENT_COUNT,
            Consts::D.segment_size,
            Consts::D.channels,
        ),
        Wave::Sawtooth,
    )));
    let v1_pcm = Arc::new(Vec::from(Pcm::new(
        Consts::D.sample_rate,
        Consts::D.channels,
        frames_in_segments(
            Consts::SEGMENT_COUNT,
            Consts::D.segment_size,
            Consts::D.channels,
        ),
        Wave::SawtoothDescending,
    )));
    let v2_pcm = Arc::new(Vec::from(Pcm::new(
        Consts::D.sample_rate,
        Consts::D.channels,
        frames_in_segments(
            Consts::SEGMENT_COUNT,
            Consts::D.segment_size,
            Consts::D.channels,
        ),
        Wave::SawtoothShifted,
    )));

    let segment_duration = Consts::D.segment_size as f64
        / (f64::from(Consts::D.sample_rate) * f64::from(Consts::D.channels) * 2.0);
    let total_secs = segment_duration * Consts::SEGMENT_COUNT as f64;

    info!(
        segments = Consts::SEGMENT_COUNT,
        variants = Consts::VARIANT_COUNT,
        segment_duration,
        total_secs = format!("{total_secs:.2}"),
        "Test data generated"
    );

    let server = HlsTestServer::new(HlsTestServerConfig {
        variant_count: Consts::VARIANT_COUNT,
        segments_per_variant: Consts::SEGMENT_COUNT,
        segment_size: Consts::D.segment_size,
        segment_duration_secs: segment_duration,
        custom_data_per_variant: Some(vec![
            Arc::clone(&v0_pcm),
            Arc::clone(&v1_pcm),
            Arc::clone(&v2_pcm),
        ]),
        init_data_per_variant: Some(vec![
            Arc::clone(&init_segment),
            Arc::clone(&init_segment),
            Arc::clone(&init_segment),
        ]),
        variant_bandwidths: Some(vec![5_000_000, 1_000_000, 500_000]),
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
            NonZeroUsize::new(Consts::SEGMENT_COUNT * Consts::VARIANT_COUNT + 20).expect("nz");
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
    let config = AudioConfig::<Hls<TestPools>>::for_stream(hls_config)
        .media_info(wav_info)
        .block_on_underrun(true)
        .build();
    let mut audio = worker.open(config).await.expect("create Audio pipeline");

    let spec = audio.spec();
    info!(
        sample_rate = spec.sample_rate,
        channels = spec.channels,
        "Audio pipeline created"
    );

    let result = spawn_blocking(move || {
        let channels = spec.channels as usize;
        let chunk_samples = num_traits::cast::<f64, usize>(
            0.05 * f64::from(spec.sample_rate.get()) * channels as f64,
        )
        .unwrap_or(usize::MAX);
        let mut buf = vec![0.0f32; chunk_samples];
        let mut rng = Xorshift64::new(0xCAFE_BABE_DEAD_BEEF);

        info!("Phase 1: warmup - reading until ABR switch");
        let mut initial_direction = Direction::Unknown;
        let mut switch_detected = false;

        loop {
            let (n, _, _) = read_with_retry(&mut audio, &mut buf);
            if n == 0 {
                break;
            }
            let dir = detect_direction(&buf[..n], channels);
            if initial_direction == Direction::Unknown && dir != Direction::Unknown {
                initial_direction = dir;
                info!(?dir, "Initial direction detected");
            }
            if initial_direction != Direction::Unknown
                && dir != Direction::Unknown
                && dir != initial_direction
            {
                info!(
                    from = ?initial_direction,
                    to = ?dir,
                    "ABR switch detected"
                );
                switch_detected = true;
                break;
            }
        }

        if !switch_detected {
            warn!("ABR switch not detected during warmup - continuing anyway");
        }

        info!("Phase 2: {} rapid random seeks", Consts::STRESS_SEEK_ITERATIONS);
        let max_seek_secs = total_secs - 0.1;
        let mut dead_seeks = 0u64;
        let mut total_retries = 0u64;
        let mut max_retries_single = 0usize;
        let mut integrity_errors = 0u64;
        let mut channel_mismatches = 0u64;

        for i in 0..Consts::STRESS_SEEK_ITERATIONS {
            let r = rng.next_f64();
            let pos_secs = if r < 0.1 {
                rng.range_f64(0.0, 1.0)
            } else if r < 0.2 {
                rng.range_f64(max_seek_secs - 2.0, max_seek_secs)
            } else {
                rng.range_f64(0.001, max_seek_secs)
            };

            let position = Duration::from_secs_f64(pos_secs);

            if let Err(e) = audio.seek(position) {
                warn!(iteration = i, pos_secs, ?e, "seek failed");
                dead_seeks += 1;
                continue;
            }

            let (n, retries, saw_eof) = read_with_retry(&mut audio, &mut buf);
            total_retries += retries as u64;
            if retries > max_retries_single {
                max_retries_single = retries;
            }

            if n == 0 {
                dead_seeks += 1;
                if dead_seeks <= 5 {
                    warn!(
                        iteration = i,
                        pos_secs,
                        is_eof = saw_eof,
                        retries,
                        "STUCK: read returned 0 after {} retries", Consts::MAX_ZERO_READS
                    );
                }
                continue;
            }

            for (j, &sample) in buf[..n].iter().enumerate() {
                if !sample.is_finite() || !(-1.0..=1.0).contains(&sample) {
                    integrity_errors += 1;
                    if integrity_errors <= 3 {
                        warn!(iteration = i, offset = j, sample, pos_secs, "bad sample");
                    }
                    break;
                }
            }

            if channels == 2 {
                let frames = n / channels;
                for f in 0..frames {
                    let l = buf[f * 2];
                    let r_val = buf[f * 2 + 1];
                    if (l - r_val).abs() > f32::EPSILON {
                        channel_mismatches += 1;
                        break;
                    }
                }
            }

            if (i + 1) % 500 == 0 {
                info!(
                    iteration = i + 1,
                    dead_seeks, total_retries, max_retries_single, integrity_errors, "Progress"
                );
            }
        }

        info!(
            dead_seeks,
            total_retries,
            max_retries_single,
            integrity_errors,
            channel_mismatches,
            "Phase 2 complete"
        );

        let max_dead = (Consts::STRESS_SEEK_ITERATIONS as u64) / 100;
        assert!(
            dead_seeks <= max_dead,
            "too many dead seeks: {}/{} (>{max_dead} = 1% threshold) - pipeline stalls after seek",
            dead_seeks, Consts::STRESS_SEEK_ITERATIONS
        );
        assert_eq!(
            integrity_errors, 0,
            "integrity errors: samples outside [-1,1] or not finite"
        );
        assert_eq!(
            channel_mismatches, 0,
            "L/R channel mismatches - data corruption"
        );

        info!("Phase 3: seek to 0 - full track integrity verification");

        let abr = audio
            .abr_handle()
            .expect("an HLS ladder must expose its ABR handle");
        let pinned = freeze_active_variant(&abr);

        audio.seek(Duration::ZERO).expect("seek to 0 must succeed");

        let mut total_frames_read = 0u64;
        let mut continuity_breaks = 0u64;
        let mut first_breaks: Vec<String> = Vec::new();
        let mut prev_phase: Option<usize> = None;
        let mut read_attempts = 0u64;
        let max_read_attempts = 100_000u64;

        #[expect(unused_assignments)]
        let mut final_saw_eof = false;
        loop {
            let (n, retries, saw_eof) = read_with_retry(&mut audio, &mut buf);
            read_attempts += 1;

            if n == 0 {
                if saw_eof {
                    final_saw_eof = true;
                    break;
                }
                if retries >= Consts::MAX_ZERO_READS {
                    panic!(
                        "STUCK at position {:.3}s after seek to 0: \
                         read returned 0 after {} retries, \
                         total_frames_read={}",
                        audio.position().as_secs_f64(),
                        Consts::MAX_ZERO_READS,
                        total_frames_read,
                    );
                }
                continue;
            }

            let frames = n / channels;

            for (j, &sample) in buf[..n].iter().enumerate() {
                assert!(
                    sample.is_finite() && (-1.0..=1.0).contains(&sample),
                    "invalid sample at frame {} (total_frames_read={}): {}",
                    total_frames_read + (j / channels) as u64,
                    total_frames_read,
                    sample
                );
            }

            if channels == 2 {
                for f in 0..frames {
                    let l = buf[f * 2];
                    let r_val = buf[f * 2 + 1];
                    assert!(
                        (l - r_val).abs() <= f32::EPSILON,
                        "L/R mismatch at frame {}: L={}, R={}",
                        total_frames_read + f as u64,
                        l, r_val
                    );
                }
            }

            let first_phase = signal::phase::units(buf[0]);
            if let Some(pp) = prev_phase {
                let next_asc = (pp + 1) % signal::SAW_PERIOD;
                let next_desc = (pp + signal::SAW_PERIOD - 1) % signal::SAW_PERIOD;
                if first_phase != next_asc && first_phase != next_desc {
                    continuity_breaks += 1;
                    if first_breaks.len() < MAX_REPORTED_BREAKS {
                        first_breaks.push(format!(
                            "inter-chunk@{total_frames_read}: {pp}->{first_phase} (expected {next_asc} or {next_desc})"
                        ));
                    }
                }
            }

            for f in 1..frames {
                let p0 = signal::phase::units(buf[(f - 1) * channels]);
                let p1 = signal::phase::units(buf[f * channels]);
                let next_asc = (p0 + 1) % signal::SAW_PERIOD;
                let next_desc = (p0 + signal::SAW_PERIOD - 1) % signal::SAW_PERIOD;
                if p1 != next_asc && p1 != next_desc {
                    continuity_breaks += 1;
                    if first_breaks.len() < MAX_REPORTED_BREAKS {
                        let frame = total_frames_read + f as u64;
                        first_breaks.push(format!(
                            "intra-chunk@{frame}: {p0}->{p1} (expected {next_asc} or {next_desc})"
                        ));
                    }
                }
            }

            let last_frame_phase = signal::phase::units(buf[(frames - 1) * channels]);
            prev_phase = Some(last_frame_phase);

            total_frames_read += frames as u64;

            if read_attempts > max_read_attempts {
                panic!(
                    "exceeded {} read attempts in phase 3, \
                     total_frames_read={} - possible infinite loop",
                    max_read_attempts, total_frames_read
                );
            }
        }

        assert!(final_saw_eof, "expected EOF after full track read");

        let expected_frames = (Consts::SEGMENT_COUNT * Consts::D.segment_size) / (Consts::D.channels as usize * 2);
        let frame_diff = total_frames_read.abs_diff(expected_frames as u64);
        let tolerance = (expected_frames as u64) / 50;

        info!(
            total_frames_read,
            expected_frames, frame_diff, tolerance, continuity_breaks, "Phase 3 complete"
        );

        assert!(
            frame_diff <= tolerance,
            "frame count mismatch after seek-to-0: got {}, expected ~{} (+-{})",
            total_frames_read, expected_frames, tolerance
        );

        assert_eq!(
            abr.current_variant_index(),
            Some(pinned),
            "the ladder moved during the integrity read, so the phase check below \
             is reading a crossfade rather than one variant's waveform"
        );

        let max_breaks = 10u64;
        assert!(
            continuity_breaks <= max_breaks,
            "too many continuity breaks after seek-to-0: {} (>{} tolerance) \
             - data corruption or segment gap; total_frames_read={}, \
             expected_frames={}, first breaks: {:?}",
            continuity_breaks, max_breaks, total_frames_read, expected_frames, first_breaks
        );

        info!("All phases passed");
    })
    .await;

    match result {
        Ok(()) => info!("Lifecycle stress test passed"),
        Err(e) => panic!("spawn_blocking failed: {e}"),
    }
}
