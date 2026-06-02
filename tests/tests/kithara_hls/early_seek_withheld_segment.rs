#![cfg(not(target_arch = "wasm32"))]

//! EndOfStream structural-guard test: when the reader reaches a non-final HLS
//! segment whose body has not yet arrived, it must surface as `Pending`
//! (need-data) and **never** as `Eof` (nor a failed/closed producer). A
//! premature `Eof` is the root of the production "silent auto-advance / track
//! skip" cascade — the audio source FSM reaches `at_eof()` for an in-range,
//! un-downloaded segment, the worker emits `NaturalEof` into the PCM channel,
//! the consumer goes `AtEof`, and the queue (one layer up) reads that terminal
//! signal and advances to the next track without the user asking.
//!
//! On current code this scenario already behaves correctly (segment sizes are
//! learned up-front via HEAD / `#EXT-X-BYTERANGE`, so the `offset >=
//! total_bytes` EOF gate never fires for an in-range segment). This test is
//! therefore a permanent **regression guard** for the EOF contract: it was
//! validated by fault-injection — temporarily forcing `HlsVariant::phase_at` /
//! `wait_range` to mint `Eof` for an in-range, not-ready range makes it go RED
//! (the producer fails / reader EOFs), and it returns GREEN once the fault is
//! removed. It pins the contract the EndOfStream proof-token preserves: EOF is
//! only ever surfaced when the end is genuinely reached, never inferred for a
//! withheld in-range segment.
//!
//! Determinism: no network, no `sleep`, no real-time pacing, **no seek** (the
//! seek-anchor path into a withheld segment has a separate, load-sensitive
//! worker-stall race that is out of scope here — see the Phase 1 handoff). The
//! reader forward-plays the un-gated leading segment and then reaches the gated
//! one naturally; the worker parks cleanly on need-data, which is the
//! deterministic seam. [`PackagedTestServer::with_segment_gate`] parks the GET
//! body of one `(variant, segment)` until release (HEAD stays unblocked, so the
//! size is known). [`Audio::preload`] puts the reader in non-blocking mode, so
//! an empty PCM ringbuf surfaces as `ReadOutcome::Pending` rather than parking.

use std::time::{Duration, Instant};

use kithara::{
    abr::AbrMode,
    assets::StoreOptions,
    audio::{Audio, AudioConfig, ReadOutcome},
    hls::{Hls, HlsConfig},
    stream::Stream,
};
use kithara_decode::DecoderBackend;
use kithara_integration_tests::{PackagedTestServer, SegmentGateHandle, TestTempDir};
use kithara_platform::{CancellationToken, tokio::task::spawn_blocking};
use url::Url;

/// Packaged plain fixture geometry: 3 variants x 3 segments x 4s = 12s.
const SEGMENT_SECS: f64 = 4.0;
const NUM_SEGMENTS: usize = 3;
/// The gated, non-final, in-range middle segment (`k = 1 < last = 2`).
const GATED_VARIANT: usize = 0;
const GATED_SEGMENT: usize = 1;

/// Wall-clock window (well under the 1s `KITHARA_HANG_TIMEOUT_SECS`) during
/// which the reader must hold `Pending` after reaching the withheld segment.
/// This is an upper bound on a yielding spin (no `sleep`); a premature `Eof`
/// or producer-fail on ANY pull fails immediately, long before the window
/// elapses. Kept short so the worker's clean need-data park is never mistaken
/// for a hang under load, yet long enough that a buggy one-shot `NaturalEof`
/// (which the consumer latches into `AtEof`) is observed.
const HOLD_WINDOW: Duration = Duration::from_millis(300);

/// Upper bound on pulls used to drive the reader to the withheld segment and
/// to confirm resume after release. Generous: each non-blocking pull returns
/// instantly; the bound only guards a true stall (which the 1s hang budget
/// catches first).
const PULL_BUDGET: usize = 5_000_000;

const SAMPLE_RATE: u32 = 44_100;
const CHANNELS: usize = 2;
const READ_FRAMES: usize = 1024;
/// Frames that prove the reader has drained essentially all of un-gated
/// segment 0 (~4s @ 44.1k) and is now blocked on the withheld segment 1. Set
/// to 90% of the nominal segment so the AAC priming / algorithmic-delay strip
/// (the decoder emits a few hundred fewer frames than the container's nominal
/// 4s) cannot keep the threshold permanently out of reach.
const SEGMENT_0_FRAMES: u64 = (SAMPLE_RATE as u64) * 4 * 9 / 10;

async fn build_audio(master: Url, temp: &TestTempDir) -> Audio<Stream<Hls>> {
    let hls_config = HlsConfig::for_url(master)
        .store(StoreOptions::new(temp.path()))
        .cancel(CancellationToken::default())
        .initial_abr_mode(AbrMode::manual(GATED_VARIANT))
        .build();
    let audio_config = AudioConfig::<Hls>::for_stream(hls_config)
        .decoder_backend(DecoderBackend::Symphonia)
        .build();
    Audio::<Stream<Hls>>::new(audio_config)
        .await
        .expect("create Audio<Stream<Hls>>")
}

/// Number of consecutive `Pending` pulls (with the gated GET already at the
/// server and segment 0 substantially drained) that proves the reader is
/// genuinely **blocked** on the withheld segment — not merely between
/// segment-0 chunks. Each pull is non-blocking + yields, so this is a short
/// wall-clock window well under the 1s hang budget.
const BLOCKED_PENDING_STREAK: usize = 4096;

/// Forward-play until the reader is blocked on the withheld segment: it has
/// drained essentially all of un-gated segment 0, the gated GET has reached the
/// server (`requested >= 1`), and it then holds a sustained `Pending` streak
/// (no more segment-0 PCM to serve). A premature `Eof`/producer-fail before
/// that point is itself the bug. Returns the frames consumed from segment 0.
fn play_until_blocked_on_withheld(
    audio: &mut Audio<Stream<Hls>>,
    handle: &SegmentGateHandle,
) -> u64 {
    let mut buf = vec![0.0_f32; READ_FRAMES * CHANNELS];
    let mut frames: u64 = 0;
    let mut pending_streak = 0usize;
    for _ in 0..PULL_BUDGET {
        let outcome = audio
            .read(&mut buf)
            .unwrap_or_else(|e| panic!("PRODUCER FAILED while draining segment 0: Err({e})"));
        match outcome {
            ReadOutcome::Frames { count, .. } => {
                frames += count.get() as u64 / CHANNELS as u64;
                pending_streak = 0; // still serving segment-0 PCM
            }
            ReadOutcome::Pending { .. } => {
                // Only count toward "blocked" once segment 0 is essentially
                // drained and the gated GET is in flight — otherwise this is a
                // normal mid-segment-0 refill pause.
                if frames >= SEGMENT_0_FRAMES && handle.requested() >= 1 {
                    pending_streak += 1;
                    if pending_streak >= BLOCKED_PENDING_STREAK {
                        return frames;
                    }
                }
            }
            ReadOutcome::Eof { position } => panic!(
                "PREMATURE EOF at {position:?} after only {frames} frames — segment 0 is \
                 not gated and the stream is ~{}s; no terminal may fire before the end.",
                SEGMENT_SECS * NUM_SEGMENTS as f64,
            ),
        }
        std::thread::yield_now();
    }
    panic!(
        "reader never blocked on the withheld segment {GATED_SEGMENT} (frames={frames}, \
         requested={}, pending_streak={pending_streak}) within {PULL_BUDGET} pulls",
        handle.requested(),
    );
}

/// Hold for `HOLD_WINDOW`, asserting every pull stays `Pending` (never `Eof`,
/// never producer-fail) while the segment body is withheld.
fn assert_holds_pending(audio: &mut Audio<Stream<Hls>>, handle: &SegmentGateHandle) {
    let mut buf = vec![0.0_f32; READ_FRAMES * CHANNELS];
    let deadline = Instant::now() + HOLD_WINDOW;
    while Instant::now() < deadline {
        let outcome = audio.read(&mut buf).unwrap_or_else(|e| {
            panic!(
                "PRODUCER FAILED: Err({e}) while non-final segment {GATED_SEGMENT} \
                 (variant {GATED_VARIANT}) was withheld — a withheld (not failed) segment \
                 must surface as Pending(need-data), never a closed/failed producer."
            )
        });
        match outcome {
            ReadOutcome::Pending { .. } => {}
            ReadOutcome::Frames { count, position } => panic!(
                "UNEXPECTED FRAMES: {count} frames at {position:?} from the withheld segment \
                 {GATED_SEGMENT} — no PCM for it can exist while its GET body is parked. \
                 requested={}",
                handle.requested(),
            ),
            ReadOutcome::Eof { position } => panic!(
                "PREMATURE EOF: reader minted Eof at {position:?} while non-final segment \
                 {GATED_SEGMENT} (variant {GATED_VARIANT}) was withheld. It is in-range and \
                 not yet on disk — the only correct outcome is Pending(need-data). This Eof \
                 is the root of the silent auto-advance cascade. requested={}",
                handle.requested(),
            ),
        }
        std::thread::yield_now();
    }
}

/// After release, pull until `Frames` proves playback resumed. A lingering
/// `Eof` here would mean the released segment still mints a false terminal.
/// Yields each pull so the worker + runtime drive the unblocked download
/// forward (no `sleep`; a yielding spin capped by `PULL_BUDGET`).
fn assert_resumes_with_frames(audio: &mut Audio<Stream<Hls>>) {
    let mut buf = vec![0.0_f32; READ_FRAMES * CHANNELS];
    for pull in 0..PULL_BUDGET {
        match audio.read(&mut buf).expect("read after release") {
            ReadOutcome::Frames { count, .. } => {
                assert!(count.get() > 0, "Frames must carry > 0 frames");
                return;
            }
            ReadOutcome::Eof { position } => panic!(
                "EOF AFTER RELEASE: reader minted Eof at {position:?} on pull {pull} after the \
                 gated segment {GATED_SEGMENT} body was released — the segment is now fully \
                 available, playback must resume with Frames, not terminate."
            ),
            ReadOutcome::Pending { .. } => {}
        }
        std::thread::yield_now();
    }
    panic!(
        "reader never produced Frames within {PULL_BUDGET} pulls after release — playback \
         failed to resume on the now-available segment {GATED_SEGMENT}"
    );
}

/// Forward-play into a withheld non-final HLS segment must hold `Pending`
/// (never `Eof`); after release, playback resumes. Offline pull, no sleep.
#[kithara::test(
    tokio,
    native,
    serial,
    timeout(Duration::from_secs(25)),
    env(KITHARA_HANG_TIMEOUT_SECS = "1"),
    tracing("kithara_hls=debug,kithara_stream=debug,kithara_audio=debug")
)]
async fn forward_play_into_withheld_segment_never_eofs() {
    let (server, gate) = PackagedTestServer::with_segment_gate(GATED_VARIANT, GATED_SEGMENT).await;
    let master = server.url("/master.m3u8");
    let temp = TestTempDir::new();

    let mut audio = build_audio(master, &temp).await;

    let total_secs = audio
        .duration()
        .expect("packaged fixture reports duration")
        .as_secs_f64();
    assert!(
        (total_secs - SEGMENT_SECS * NUM_SEGMENTS as f64).abs() < 1.0,
        "fixture geometry changed: expected ~{}s, got {total_secs:.2}s",
        SEGMENT_SECS * NUM_SEGMENTS as f64,
    );
    let aspec = audio.spec();
    assert_eq!(aspec.sample_rate, SAMPLE_RATE);
    assert_eq!(usize::from(aspec.channels), CHANNELS);

    // Non-blocking reader mode: an empty PCM ringbuf surfaces as `Pending`
    // instead of parking. This is the offline `pull_expect_*` seam.
    audio.preload().expect("preload (non-blocking reader)");

    let outcome = spawn_blocking(move || {
        let drained = play_until_blocked_on_withheld(&mut audio, &gate);
        assert!(
            drained >= SEGMENT_0_FRAMES,
            "expected to drain >= one segment before blocking, got {drained} frames"
        );
        assert!(
            gate.requested() >= 1,
            "gated segment {GATED_SEGMENT} GET never reached the server"
        );

        // CONTRACT: while withheld, the reader holds Pending — never Eof.
        assert_holds_pending(&mut audio, &gate);

        // Release the body; playback must resume on now-available data.
        gate.release();
        assert_resumes_with_frames(&mut audio);
        gate.requested()
    })
    .await
    .expect("pull thread joined");

    assert!(outcome >= 1, "gated GET request count should be observed");
    drop(server);
}
