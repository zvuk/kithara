#![cfg(not(target_arch = "wasm32"))]

//! Deterministic repro for the production "immediate-seek → silent
//! auto-advance" bug: start an HLS track and seek into a non-final segment
//! that is not yet fully known/delivered (the genuinely-immediate-seek
//! condition), before the next playlist track is ever requested. The queue
//! must NOT advance to the next track, and no terminal
//! (`ItemDidPlayToEnd` / `ItemDidFail`) may fire for the seeked track while it
//! is mid-seek into an in-range region.
//!
//! Determinism: no `sleep`, no real-time pacing. The
//! [`PackagedTestServer`] withhold gate controls the seek-target segment's
//! **body** (GET parked); segment-aware fMP4 deliberately does not use startup
//! HEAD size probes. The audio graph is pulled one block at a time via the
//! manual [`OfflineSession`] and the queue is ticked synchronously between
//! blocks.
//! Auto-advance is observed as a `Queue::current_index()` change against a
//! multi-track queue.

use kithara::{
    assets::StoreOptions,
    decode::DecoderBackend,
    events::{AbrMode, PlayerEvent},
    net::{HttpClient, NetOptions},
    platform::{CancelToken, sync::Arc, time::Duration},
    play::{PlayerConfig, PlayerImpl, Resource, ResourceConfig, SessionDispatcher},
    queue::{Queue, QueueConfig, Transition},
    stream::dl::{Downloader, DownloaderConfig},
};
use kithara_integration_tests::{
    PackagedTestServer, SegmentGateHandle, TestTempDir, kithara, offline::OfflineSession,
};

const SAMPLE_RATE: u32 = 44_100;
const BLOCK_FRAMES: usize = 512;

/// Packaged plain fixture: 3 variants × 3 segments × 4s = 12s.
const GATED_VARIANT: usize = 0;
/// Middle (non-final) segment, time region [4s, 8s).
const GATED_SEGMENT: usize = 1;
/// Seek target inside the gated segment's time region.
const SEEK_TARGET_SECS: f64 = 5.5;

/// Blocks rendered before the seek so segment 0 starts producing PCM and the
/// track is genuinely "playing" when the seek arrives (~0.46s at 512 frames).
const WARMUP_BLOCKS: usize = 40;
/// Blocks rendered while observing the post-seek outcome. Each block is pulled
/// synchronously (no sleep). A buggy run flips `current_index` within a few
/// blocks; a healthy run holds on track 0 (or stalls — the 1s hang budget
/// catches a stall first).
///
/// MUST stay strictly below the genuine post-seek remainder. The packaged
/// track is 537600 frames (12.19s); seeking to 5.5s leaves ~576 blocks of
/// real audio. In the body-open mode the gated segment is still delivered and
/// a producer that keeps pace can legitimately reach the track's genuine end.
/// That correct `ItemDidPlayToEnd` + advance must not be misread as the
/// premature-terminal cascade. 500 blocks (5.8s, max position 11.3s) keeps the
/// genuine end unreachable in-window under any scheduling while still being
/// orders of magnitude above the few blocks a premature byte-EOF needs to
/// surface.
const OBSERVE_BLOCKS: usize = 500;

#[derive(Clone, Copy, Debug)]
struct GateMode {
    withhold_head: bool,
    withhold_body: bool,
}

struct Harness {
    player: Arc<PlayerImpl>,
    session: Arc<OfflineSession>,
}

impl Harness {
    fn new() -> Self {
        let session = Arc::new(OfflineSession::new_manual());
        let config = PlayerConfig::builder()
            .crossfade_duration(0.0)
            .sample_rate(SAMPLE_RATE)
            .session(Arc::clone(&session) as Arc<dyn SessionDispatcher>)
            .build();
        let player = Arc::new(PlayerImpl::new(config));
        Self { player, session }
    }

    fn render(&self, frames: usize) -> Vec<f32> {
        self.session.render(frames)
    }
}

async fn build_hls_resource(
    master: &url::Url,
    downloader: &Downloader,
    store: &StoreOptions,
) -> Resource {
    let cfg = ResourceConfig::for_src(master.as_str())
        .expect("valid master URL")
        .downloader(downloader.clone())
        .store(store.clone())
        .decoder(
            kithara::audio::AudioDecoderConfig::builder()
                .backend(DecoderBackend::Symphonia)
                .build(),
        )
        .initial_abr_mode(AbrMode::manual(GATED_VARIANT))
        .build();
    Resource::new(cfg).await.expect("create HLS resource")
}

/// What the queue did during the post-seek observation window.
#[derive(Debug)]
enum Outcome {
    HeldOnTrack,
    AutoAdvanced { new_index: usize, trigger: Trigger },
}

#[derive(Debug, Clone, Copy)]
enum Trigger {
    DidPlayToEnd,
    DidFail,
    NoTerminal,
}

// NOTE: the body-withheld / size-known case (seek into a sized-but-undelivered
// segment) is intentionally NOT covered here. It exercises a *separate*,
// load-sensitive audio-worker seek-anchor stall (`apply_seek_from_timeline ->
// seek_time_anchor`) — a distinct root from the byte-EOF under-count this file
// pins — and is owned by the seek-stall workstream. Mixing it in would make
// this otherwise-deterministic guard flaky.

/// No eager exact size, body open: seek lands while only placeholder geometry
/// is available.
#[kithara::test(
    tokio,
    multi_thread,
    serial,
    timeout(Duration::from_secs(60)),
    env(KITHARA_HANG_TIMEOUT_SECS = "1"),
    tracing("kithara_hls=debug,kithara_stream=debug,kithara_audio=debug,kithara_queue=debug")
)]
async fn immediate_seek_size_withheld() {
    run_case(GateMode {
        withhold_head: true,
        withhold_body: false,
    })
    .await;
}

/// Body withheld: seek lands on an undelivered segment — the closest model of
/// the genuinely-immediate user seek.
#[kithara::test(
    tokio,
    multi_thread,
    serial,
    timeout(Duration::from_secs(60)),
    env(KITHARA_HANG_TIMEOUT_SECS = "1"),
    tracing("kithara_hls=debug,kithara_stream=debug,kithara_audio=debug,kithara_queue=debug")
)]
async fn immediate_seek_size_and_body_withheld() {
    run_case(GateMode {
        withhold_head: true,
        withhold_body: true,
    })
    .await;
}

async fn run_case(mode: GateMode) {
    let (server, gate): (PackagedTestServer, SegmentGateHandle) =
        PackagedTestServer::with_segment_gate(GATED_VARIANT, GATED_SEGMENT).await;
    // `with_segment_gate` parks the body by default. Apply the requested mode.
    if mode.withhold_head {
        gate.withhold_head();
    }
    if !mode.withhold_body {
        gate.release();
    }

    let master = server.url("/master.m3u8");
    let temp = TestTempDir::new();
    let store = StoreOptions::new(temp.path());
    let downloader = Downloader::new(
        DownloaderConfig::for_client(HttpClient::new(NetOptions::default(), CancelToken::never()))
            .build(),
    );

    let harness = Harness::new();
    let queue = Queue::new(
        QueueConfig::builder()
            .should_autoplay(false)
            .build()
            .with_player(Arc::clone(&harness.player)),
    );
    let mut rx = harness.player.subscribe();

    // Track 0 = the gated HLS track. Track 1 = a second HLS track so a forward
    // auto-advance has somewhere to land (observable as current_index 0 -> 1).
    let target = build_hls_resource(&master, &downloader, &store).await;
    let target_src = target.src().clone();
    let id0 = queue.insert_loaded_for_test(target);
    let next = build_hls_resource(&master, &downloader, &store).await;
    let _id1 = queue.insert_loaded_for_test(next);

    queue.select(id0, Transition::None).expect("select track 0");
    queue.play();
    assert_eq!(queue.current_index(), Some(0), "starts on track 0");

    // Warm up: render some blocks so segment 0 decodes and the track is
    // genuinely playing before the seek arrives.
    for _ in 0..WARMUP_BLOCKS {
        let _ = queue.tick();
        let _ = harness.render(BLOCK_FRAMES);
    }
    assert_eq!(
        gate.head_requested(),
        0,
        "segment-aware fMP4 must not HEAD-probe the gated segment at startup \
         (mode={mode:?})"
    );

    // The user taps the slider immediately: seek into the gated segment's time
    // region while exact size is unavailable and the body may still be withheld.
    queue
        .seek(SEEK_TARGET_SECS)
        .expect("seek accepted by player");

    let mut trigger = Trigger::NoTerminal;
    let mut outcome = Outcome::HeldOnTrack;
    for _ in 0..OBSERVE_BLOCKS {
        let _ = queue.tick();
        let _ = harness.render(BLOCK_FRAMES);
        while let Ok(ev) = rx.try_recv() {
            if let kithara::events::Event::Player(pe) = ev {
                match pe {
                    PlayerEvent::ItemDidFail { ref src, .. } if *src == target_src => {
                        trigger = Trigger::DidFail;
                    }
                    PlayerEvent::ItemDidPlayToEnd { ref src, .. } if *src == target_src => {
                        trigger = Trigger::DidPlayToEnd;
                    }
                    _ => {}
                }
            }
        }
        let idx = queue.current_index();
        if idx != Some(0) {
            outcome = Outcome::AutoAdvanced {
                new_index: idx.unwrap_or(usize::MAX),
                trigger,
            };
            break;
        }
    }

    // Release gates so a healthy pipeline can resume (after observation, so a
    // bug — if present — is already captured).
    gate.release_head();
    gate.release();

    match outcome {
        Outcome::HeldOnTrack => {
            assert_eq!(
                queue.current_index(),
                Some(0),
                "queue must stay on track 0 while the seek target segment is \
                 placeholder-sized/undelivered (mode={mode:?})"
            );
            // (Post-release behaviour — eventually advancing at the track's
            // genuine end once needed segments are delivered — is correct and
            // intentionally NOT asserted here: this guard pins the
            // *in-withheld-window* contract only.)
        }
        Outcome::AutoAdvanced { new_index, trigger } => {
            queue.clear();
            drop(queue);
            drop(server);
            panic!(
                "PRODUCTION CASCADE REPRODUCED (mode={mode:?}): an immediate seek to \
                 {SEEK_TARGET_SECS}s into a non-final segment that was not yet \
                 ready/delivered caused the queue to advance 0 -> {new_index} (triggered by \
                 {trigger:?}). The user never asked for a track switch. The audio source \
                 minted a premature terminal for an in-range segment instead of holding \
                 Pending."
            );
        }
    }

    queue.clear();
    drop(queue);
    drop(server);
}
