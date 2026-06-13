#![cfg(not(target_arch = "wasm32"))]

//! Deterministic repro for flake F5: the audio worker BUSY-SPINS when
//! steady-state forward playback reaches a not-ready (withheld) segment.
//!
//! Root: `Track<Decoding>::step` enters `decode_one_step` whenever
//! `source_is_ready()` is true. That gate checks only `[pos, next-segment-
//! boundary)` — the CURRENT segment — so it reports `Ready` even when the
//! decoder's container parser reads *across* the boundary into the withheld
//! next segment and returns `Pending`. Before the fix, the `Pending` mapped
//! to a bare `TrackStep::Blocked(Waiting)` with NO FSM transition: the slot
//! stayed `Decoding`, so the very next scheduler tick re-ran the full
//! `decode_one_step` (decode-into-buffer + `wait_range` budget) again. The
//! source phase at `pos` never changes (the current segment stays ready), so
//! the loop never parks. The blocking consumer's `recv_outcome_blocking`
//! wake loop unparks the worker on a tight loop while it waits for a chunk,
//! and each unpark re-runs the full decode — thousands of times per second
//! (`step_track took too long — starving other tracks`), burning CPU and
//! never making progress.
//!
//! Contract: while the next segment's body is withheld, the worker must PARK
//! (transition to `WaitingForSource(Playback)` and re-check the source's
//! forward read-ahead window cheaply on each wake) — it must NOT re-run
//! `decode_one_step` on a hot loop. Counted directly off the
//! `kithara_audio::decode_one_step` probe: a parked worker fires it only on
//! the occasional wake; the busy-spin fires it thousands of times over the
//! same window. Once the body is released, decoding must resume.
//!
//! Determinism: the `HlsTestServer` segment gate withholds segment
//! `GATED_SEGMENT`'s BODY while its size (HEAD) stays known, so the layout is
//! learned up front and the worker reaches the boundary deterministically.
//! The blocking decode runs on a `spawn_blocking` thread (the documented
//! offline-pull contract) while a releaser thread observes the parked GET on
//! the gate's in-process counter, measures the spin RATE off the probe (a
//! real metric, not a sleep that masks a hang), then releases. No real-time
//! pacing, no codec timers.

use std::{
    num::NonZeroUsize,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

use kithara::{
    assets::StoreOptions,
    audio::{Audio, AudioConfig, ReadOutcome},
    hls::{AbrMode, Hls, HlsConfig},
    stream::{AudioCodec, ContainerFormat, MediaInfo, Stream},
};
use kithara_integration_tests::{
    TestTempDir,
    hls_server::{HlsTestServer, HlsTestServerConfig},
    signal_pcm::{Finite, SignalPcm, signal},
    wav::create_wav_header,
};
use kithara_platform::{
    CancelToken,
    time::{Duration, Instant, sleep},
    tokio::task::spawn_blocking,
};
use kithara_test_utils::probe::capture::{Recorder, install as install_recorder};
use tracing::info;

const SAMPLE_RATE: u32 = 44_100;
const CHANNELS: u16 = 2;
/// Small segments so the boundary is reached after only a few reads.
const SEGMENT_SIZE: usize = 32_768;
const SEGMENT_COUNT: usize = 8;
/// The segment whose body is withheld; forward playback parks at its start.
const GATED_SEGMENT: usize = 4;

/// Window over which the parked worker's `decode_one_step` invocations are
/// counted while the consumer is blocked at the withheld boundary. A real
/// budget — long enough that a busy-spin (thousands/sec) blows the bound by
/// orders of magnitude.
const OBSERVE_WINDOW: Duration = Duration::from_millis(700);
/// Upper bound on `decode_one_step` invocations over the observation window
/// while parked. The pre-fix spin produced ~7 500/sec (~5 000 in this
/// window). A correctly-parked worker re-checks the cheap source phase per
/// wake, not the decode loop, so it fires `decode_one_step` only a few
/// hundred times at most — well under this ceiling, still an order of
/// magnitude below the spin floor.
const MAX_PARKED_DECODE_STEPS: usize = 500;

fn count_decode_steps(recorder: &Recorder) -> usize {
    recorder
        .snapshot()
        .into_iter()
        .filter(|e| e.target == "kithara_audio_probe" && e.probe_name() == Some("decode_one_step"))
        .count()
}

#[kithara::test(
    flash(false),
    tokio,
    native,
    serial,
    timeout(Duration::from_secs(30)),
    tracing("kithara_audio=info,kithara_hls=info")
)]
async fn forward_into_withheld_segment_parks_without_busy_spin() {
    let init_segment = Arc::new(create_wav_header(SAMPLE_RATE, CHANNELS, None));
    let pcm = Arc::new(
        SignalPcm::new(
            signal::Sawtooth,
            SAMPLE_RATE,
            CHANNELS,
            Finite::from_segments(SEGMENT_COUNT, SEGMENT_SIZE, CHANNELS),
        )
        .into_vec(),
    );
    let segment_duration = SEGMENT_SIZE as f64
        / (f64::from(SAMPLE_RATE) * f64::from(CHANNELS) * size_of::<i16>() as f64);
    let config = HlsTestServerConfig {
        variant_count: 1,
        segments_per_variant: SEGMENT_COUNT,
        segment_size: SEGMENT_SIZE,
        segment_duration_secs: segment_duration,
        custom_data_per_variant: Some(vec![Arc::clone(&pcm)]),
        init_data_per_variant: Some(vec![Arc::clone(&init_segment)]),
        variant_bandwidths: Some(vec![1_000_000]),
        ..Default::default()
    };

    // Withhold the BODY of GATED_SEGMENT; its HEAD (size) stays open so the
    // up-front layout is complete and the worker reaches the boundary.
    let (server, gate) = HlsTestServer::with_segment_gate(config, 0, GATED_SEGMENT).await;
    let temp_dir = TestTempDir::new();

    let store = StoreOptions::builder()
        .cache_dir(temp_dir.path().into())
        .is_ephemeral(true)
        .cache_capacity(NonZeroUsize::new(SEGMENT_COUNT + 10).expect("nonzero"))
        .build();
    let hls_config = HlsConfig::for_url(server.url("/master.m3u8"))
        .store(store)
        .cancel(CancelToken::never())
        .initial_abr_mode(AbrMode::manual(0))
        .build();
    let wav_info = MediaInfo::new(Some(AudioCodec::Pcm), Some(ContainerFormat::Wav));
    let audio_config = AudioConfig::<Hls>::for_stream(hls_config)
        .media_info(wav_info)
        .build();

    let recorder = install_recorder();

    let mut audio = Audio::<Stream<Hls>>::new(audio_config)
        .await
        .expect("audio creation (segment 0 not withheld)");

    let spec = audio.spec();
    let read_samples = spec.channels as usize * 512;

    // Set once the blocking decode has consumed enough to be parked at the
    // withheld boundary, so the releaser measures the spin during the park.
    let at_boundary = Arc::new(AtomicBool::new(false));
    let released = Arc::new(AtomicBool::new(false));

    // Releaser: wait for the withheld GET to park AND the decode thread to
    // signal it is at the boundary, then measure `decode_one_step` over a
    // fixed window (the worker is blocked-consumer-driven during this window),
    // assert no busy-spin, and release the body so playback resumes.
    let release_gate = gate.clone();
    let release_recorder = recorder.clone();
    let release_at_boundary = Arc::clone(&at_boundary);
    let release_flag = Arc::clone(&released);
    let releaser = tokio::spawn(async move {
        let deadline = Instant::now() + Duration::from_secs(8);
        loop {
            if release_gate.requested() > 0 && release_at_boundary.load(Ordering::Acquire) {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "decode never parked at the withheld boundary within budget"
            );
            sleep(Duration::from_millis(5)).await;
        }

        let before = count_decode_steps(&release_recorder);
        sleep(OBSERVE_WINDOW).await;
        let after = count_decode_steps(&release_recorder);
        let delta = after.saturating_sub(before);
        info!(
            before,
            after,
            delta,
            bound = MAX_PARKED_DECODE_STEPS,
            observe_ms = OBSERVE_WINDOW.as_millis(),
            "F5 parked-window decode_one_step observation"
        );

        release_flag.store(true, Ordering::Release);
        release_gate.release();
        delta
    });

    let decode_at_boundary = Arc::clone(&at_boundary);
    let decode_released = Arc::clone(&released);
    let decode = spawn_blocking(move || -> (u64, u64) {
        let mut buf = vec![0.0f32; read_samples];
        let mut frames_before = 0u64;
        let mut frames_after = 0u64;

        // Phase 1: drive forward (blocking reads — the offline-pull contract)
        // until the gate has been released. Before release, reads consume the
        // ready segments 0..GATED_SEGMENT, then block at the withheld
        // boundary. The moment a read blocks (consumes all ready data and the
        // worker can't fill the ring), signal the boundary so the releaser
        // starts its spin measurement; the blocking `read` itself then parks
        // this thread until the body arrives.
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            if decode_released.load(Ordering::Acquire) {
                break;
            }
            // Signal "at boundary" once we've decoded a full segment's worth —
            // the worker is now past the ready segments and about to (or
            // already) block on the withheld one.
            if frames_before >= (GATED_SEGMENT as u64) * (SEGMENT_SIZE as u64) / 4 {
                decode_at_boundary.store(true, Ordering::Release);
            }
            match audio.read(&mut buf) {
                Ok(ReadOutcome::Frames { count, .. }) => frames_before += count.get() as u64,
                Ok(ReadOutcome::Pending { .. }) => {
                    decode_at_boundary.store(true, Ordering::Release);
                }
                Ok(ReadOutcome::Eof { .. }) => break,
                Err(e) => panic!("read error before release: {e}"),
            }
            assert!(
                Instant::now() < deadline,
                "decode stalled before the gate released within budget"
            );
        }

        // Phase 2: gate released — playback must resume past the boundary.
        let resume_deadline = Instant::now() + Duration::from_secs(15);
        while frames_after < 4_096 {
            match audio.read(&mut buf) {
                Ok(ReadOutcome::Frames { count, .. }) => frames_after += count.get() as u64,
                Ok(ReadOutcome::Pending { .. }) => std::thread::yield_now(),
                Ok(ReadOutcome::Eof { .. }) => break,
                Err(e) => panic!("read error after release: {e}"),
            }
            assert!(
                Instant::now() < resume_deadline,
                "playback did not resume within budget after releasing the body"
            );
        }

        (frames_before, frames_after)
    });

    let parked_delta = releaser.await.expect("releaser joins");
    let (frames_before, frames_after) = decode.await.expect("decode task joins");

    info!(
        frames_before,
        frames_after,
        parked_delta,
        bound = MAX_PARKED_DECODE_STEPS,
        "F5 forward-withhold result"
    );

    assert!(
        frames_before > 0,
        "expected some frames decoded before the withheld boundary"
    );
    assert!(
        parked_delta <= MAX_PARKED_DECODE_STEPS,
        "audio worker BUSY-SPUN at the withheld segment boundary: {parked_delta} \
         `decode_one_step` invocations over {OBSERVE_WINDOW:?} (bound \
         {MAX_PARKED_DECODE_STEPS}). A parked worker re-checks the cheap source \
         phase per wake; this rate means it re-ran the full decode loop on a hot \
         tick (flake F5)."
    );
    assert!(
        frames_after > 0,
        "playback did not resume after releasing the withheld body"
    );

    drop(server);
}
