#![cfg(not(target_os = "android"))]
#![cfg(not(target_arch = "wasm32"))]

use std::fmt;

use kithara::{
    events::TrackId,
    hls::AbrMode,
    link::{GridAnswer, LinkedHostCommand, SyncStatus},
    platform::time::{Duration, Instant},
    play::{ArtifactSource, Position, ResourceConfig, ResourceSrc, TrackCommand, TrackStatus},
    queue::TrackSource,
    signal::SessionFrame,
};
use kithara_command::{Seq, When};
use kithara_integration_tests::{
    grid::Start,
    hls_fixture::create_test_downloader,
    kithara, memory_asset_store,
    offline::linked::{LinkProbeCommand, LinkVerdict},
};
use num_traits::{AsPrimitive, ToPrimitive};

use super::{
    sync_listening::render_frames,
    sync_product_matrix::{
        Audible, BLOCK_FRAMES, CHANNELS, NEWTECHNO_PHRASE, PreparedSources, ProductHarness,
        STAGED_BESIDE_PLAYBACK, STAGED_BESIDE_PLAYBACK_CONTROL, STAGED_CUE,
        STAGED_CUE_BESIDE_A_DECK, STAGED_UNDER_LOOSE_DEADLINE, STAGED_WITHOUT_CAPACITY, SyncCase,
        SyncRequest, TUNNEL_CUE, newtechno_sources, tunnel_sources,
    },
};

const INSTALLED: LinkVerdict = LinkVerdict::Applied;
const CAPACITY: LinkVerdict = LinkVerdict::Capacity;
const CANCELLED: LinkVerdict = LinkVerdict::Stale;
const RECEIPT_TIMEOUT: Duration = Duration::from_secs(30);
/// Longer than a lane's ring holds, so the sounding lane has to keep decoding.
const LISTEN_FRAMES: usize = 48_000 * 6;
/// How far into the deck's own timeline a measured window opens.
///
/// A harness leaves its deck wherever its build happened to land, and that
/// landing is not fixed: the transport warm-up renders until the renderer
/// publishes a revision, which off the virtual clock costs one render more on
/// a loaded host. Two harnesses compared sample by sample then open one block
/// apart, and a whole window of the same audio read one block late is every
/// sample differing at a matching level. Both sides render up to this lead
/// instead, so a window opens at a point of the deck's timeline rather than at
/// whatever its build cost. The lead only has to clear what a build leaves
/// behind, which is a handful of blocks.
const WINDOW_LEAD_FRAMES: u64 = (BLOCK_FRAMES * 16) as u64;
/// How many blocks a wait for that lead may render before it reports that the
/// deck never reached it.
///
/// Rendering is what moves the deck, so rendered blocks are what bound the
/// wait. A clock cannot: under flash `Instant::now()` reads the virtual clock,
/// and the engine fast-forwards it to the next pending deadline whenever every
/// task is parked, so a deadline there measures the jump rather than the deck.
/// A deck that advances reaches the lead in the blocks the lead is made of;
/// this clears that many times over.
const WINDOW_BLOCK_BUDGET: usize = 1024;
/// A cue on the second beat of the Tunnel's fifth bar.
const TUNNEL_WEAK_CUE: Start = Start::Bar { bar: 4, beat: 1 };
/// A second Tunnel cue that supersedes the first.
const TUNNEL_SUPERSEDING_CUE: Start = Start::bar(8);
/// Newtechno's third phrase, a second cue that supersedes its second.
const NEWTECHNO_SUPERSEDING_CUE: Start = Start::Beat(128);
/// Recording rate of The Tunnel, the frame rate its cues index.
const TUNNEL_RATE: f64 = 44_100.0;
/// Recording rate of newtechno.
const NEWTECHNO_RATE: f64 = 48_000.0;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Operation {
    Cue { item: TrackId, seq: Seq },
    Load { item: TrackId, seq: Seq },
    Start { item: TrackId, seq: Seq },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct Delivered {
    operation: Operation,
    rejected: LinkVerdict,
}

fn delivered(harness: &ProductHarness, operations: &[Operation]) -> Vec<Delivered> {
    // Ruling: sync_receipt_delivered codes and accepted bit → executor-qualified Seq: owner-visible Ready state for Cue, native returned Outcome for Load/Start — spec §11.2–§11.3.
    harness
        .decks
        .iter()
        .flat_map(|deck| {
            deck.observe(|observation| {
                operations
                    .iter()
                    .flat_map(|operation| {
                        let verdicts = match operation {
                            Operation::Cue { item, seq } => observation
                                .ready_cues
                                .iter()
                                .filter_map(|&(owner, applied)| {
                                    (owner == *item && applied == *seq).then_some(INSTALLED)
                                })
                                .collect::<Vec<_>>(),
                            Operation::Load { seq, .. } => observation
                                .dispatcher
                                .iter()
                                .filter_map(|&(answered, verdict)| {
                                    (answered == *seq).then_some(verdict)
                                })
                                .collect(),
                            Operation::Start { seq, .. } => observation
                                .deck
                                .iter()
                                .filter_map(|&(answered, verdict)| {
                                    (answered == *seq).then_some(verdict)
                                })
                                .collect(),
                        };
                        verdicts.into_iter().map(|rejected| Delivered {
                            operation: *operation,
                            rejected,
                        })
                    })
                    .collect::<Vec<_>>()
            })
        })
        .collect()
}

#[kithara_test_utils::kithara::hang_watchdog(timeout = Duration::from_secs(5), ctx = String)]
async fn render_until(
    harness: &mut ProductHarness,
    case: SyncCase,
    operation: Operation,
    code: LinkVerdict,
) -> Vec<Delivered> {
    let deadline = Instant::now() + RECEIPT_TIMEOUT;
    let mut received = 0;
    loop {
        let receipts = delivered(harness, &[operation]);
        hang_tick!(format!(
            "render_until: awaiting {code:?} for {operation:?}; received {receipts:?}"
        ));
        if receipts.len() > received {
            received = receipts.len();
            hang_reset!();
        }
        if receipts
            .iter()
            .any(|receipt| receipt.operation == operation && receipt.rejected == code)
        {
            return receipts;
        }
        assert!(
            Instant::now() < deadline,
            "{}: no {code:?} for {operation:?}; received {receipts:?}",
            case.id()
        );
        let _ = harness.render(case, BLOCK_FRAMES).await;
    }
}

/// The source second Prepare maps deck 0's cue to: the next downbeat of a
/// metered grid, the next beat of an unmetered one, which has no downbeats.
fn prepared_mark(harness: &ProductHarness, cue: Start) -> f64 {
    let ArtifactSource::Value(grid) = harness.provider_grid(0) else {
        panic!("fixture grid is materialized");
    };
    let seconds = harness.start_seconds(0, cue);
    let raw = grid.as_raw();
    raw.downbeats
        .iter()
        .filter(|_| raw.meter.is_some())
        .map(|beat| beat.at)
        .chain(
            raw.beats
                .iter()
                .filter(|_| raw.meter.is_none())
                .map(|beat| beat.at),
        )
        .find(|at| *at >= seconds)
        .expect("fixture contains the prepared cue mark")
}

#[kithara_test_utils::kithara::hang_watchdog(timeout = Duration::from_secs(5), ctx = String)]
async fn prepare_cue(
    harness: &mut ProductHarness,
    case: SyncCase,
    rate: f64,
    cue: Start,
) -> Operation {
    // Paused Sync preserves its cue. Prepare on a supplied downbeat when metered,
    // or a supplied beat otherwise, before enabling Sync.
    let seconds = harness.start_seconds(0, cue);
    assert!((seconds * rate).is_finite(), "fixture cue is finite");
    let prepared = prepared_mark(harness, cue);
    let control = harness.decks[0].control().clone();
    harness
        .host
        .run(move || control.seek(prepared))
        .await
        .expect("paused cue seek");
    let item = harness.decks[0].current().expect("paused current track").id;
    while harness.decks[0]
        .track_snapshot(item)
        .is_some_and(|snapshot| snapshot.track.pending_lane)
    {
        hang_tick!("prepare_cue: waiting for progress".to_owned());
        let _ = harness.render(case, BLOCK_FRAMES).await;
    }
    hang_reset!();
    harness.request_sync_intent(case, SyncRequest::On).await;
    let deadline = Instant::now() + RECEIPT_TIMEOUT;
    loop {
        hang_tick!("prepare_cue: waiting for progress".to_owned());
        if let Some(seq) = harness.decks[0].observe(|observation| {
            observation
                .cues
                .iter()
                .rev()
                .find_map(|&(owner, seq)| (owner == item).then_some(seq))
        }) {
            hang_reset!();
            return Operation::Cue { item, seq };
        }
        assert!(
            Instant::now() < deadline,
            "{}: SYNC did not send the cue segment",
            case.id()
        );
        let _ = harness.render(case, BLOCK_FRAMES).await;
    }
}

#[kithara_test_utils::kithara::hang_watchdog(timeout = Duration::from_secs(5), ctx = String)]
async fn prepare_incoming(
    harness: &mut ProductHarness,
    case: SyncCase,
    rate: f64,
    cue: Start,
    deck_index: usize,
    prepare_cue: bool,
) -> Operation {
    let seconds = harness.start_seconds(0, cue);
    assert!((seconds * rate).is_finite(), "fixture cue is finite");
    let source = harness.decks[0]
        .current()
        .expect("sounding current track")
        .url
        .expect("fixture URL");
    let source = ResourceConfig::for_src(ResourceSrc::parse(&source).expect("fixture source"))
        .downloader(create_test_downloader())
        .store(memory_asset_store())
        .initial_abr_mode(AbrMode::manual(0))
        .build();
    let item = TrackId::allocate();
    harness.decks[deck_index].probe(LinkProbeCommand::Incoming {
        item,
        source: TrackSource::Config(Box::new(source)),
        cue: prepare_cue
            .then_some(Position::try_from_secs_f64(seconds).expect("finite incoming cue")),
    });
    let control = harness.decks[deck_index].control().clone();
    harness
        .host
        .run(move || control.tick())
        .await
        .expect("drain incoming request without rendering");
    let deadline = Instant::now() + RECEIPT_TIMEOUT;
    loop {
        hang_tick!("prepare_incoming: waiting for progress".to_owned());
        if let Some(seq) = harness.decks[deck_index].observe(|observation| {
            observation
                .loads
                .iter()
                .find_map(|&(owner, seq)| (owner == item).then_some(seq))
        }) {
            hang_reset!();
            return Operation::Load { item, seq };
        }
        assert!(
            Instant::now() < deadline,
            "{}: incoming Load not dispatched",
            case.id()
        );
        let _real_io = kithara::platform::flash::real_io();
        kithara::platform::time::sleep(Duration::from_millis(1)).await;
    }
}

async fn release_incoming_grid(harness: &ProductHarness, operation: Operation) {
    let Operation::Load { item, seq } = operation else {
        panic!("incoming load sequence")
    };
    let ArtifactSource::Value(model) = harness.provider_grid(0) else {
        panic!("materialized fixture grid")
    };
    let deck = harness.decks[0].id();
    harness
        .host
        .send(LinkedHostCommand::Grid {
            deck,
            answer: GridAnswer {
                item,
                load: seq,
                model: Ok((*model).clone()),
            },
        })
        .await
        .expect("incoming grid delivery");
}

#[kithara_test_utils::kithara::hang_watchdog(timeout = Duration::from_secs(5), ctx = String)]
async fn schedule_start(harness: &mut ProductHarness, case: SyncCase) -> Operation {
    let item = harness.decks[0].current().expect("loaded current track").id;
    let at = SessionFrame::new(i64::try_from(harness.host.position()).expect("Host cursor"))
        + kithara::signal::FrameCount::new(usize::try_from(case.sample_rate).expect("sample rate"));
    harness.decks[0].probe(LinkProbeCommand::Track {
        item,
        command: TrackCommand::Play { at: When::At(at) },
    });
    let control = harness.decks[0].control().clone();
    harness
        .host
        .run(move || control.tick())
        .await
        .expect("schedule exact-frame start");
    let deadline = Instant::now() + RECEIPT_TIMEOUT;
    loop {
        hang_tick!(format!(
            "schedule_start: item {item:?}, requested {at:?}, snapshot {:?}",
            harness.decks[0].track_snapshot(item)
        ));
        if let Some(seq) = harness.decks[0].observe(|observation| {
            observation
                .commands
                .iter()
                .rev()
                .find_map(|&(owner, seq)| (owner == item).then_some(seq))
        }) {
            hang_reset!();
            return Operation::Start { item, seq };
        }
        assert!(
            Instant::now() < deadline,
            "{}: no scheduled start",
            case.id()
        );
        let _real_io = kithara::platform::flash::real_io();
        kithara::platform::time::sleep(Duration::from_millis(1)).await;
    }
}

#[kithara::test(native, tokio, multi_thread, serial, timeout(Duration::from_secs(60)))]
#[case::tunnel_unbounded_deadline(tunnel_sources().await, TUNNEL_RATE, TUNNEL_CUE, STAGED_CUE)]
#[case::tunnel_deadline_looser_than_the_ring(
    tunnel_sources().await,
    TUNNEL_RATE,
    TUNNEL_CUE,
    STAGED_UNDER_LOOSE_DEADLINE
)]
#[case::tunnel_weak_beat(tunnel_sources().await, TUNNEL_RATE, TUNNEL_WEAK_CUE, STAGED_CUE)]
#[case::newtechno_unbounded_deadline(
    newtechno_sources().await,
    NEWTECHNO_RATE,
    NEWTECHNO_PHRASE,
    STAGED_CUE
)]
#[case::newtechno_deadline_looser_than_the_ring(
    newtechno_sources().await,
    NEWTECHNO_RATE,
    NEWTECHNO_PHRASE,
    STAGED_UNDER_LOOSE_DEADLINE
)]
#[kithara_test_utils::kithara::hang_watchdog(timeout = Duration::from_secs(5), ctx = String)]
async fn a_cued_sync_installs_mapped_pcm_before_anything_sounds(
    #[case] sources: PreparedSources,
    #[case] rate: f64,
    #[case] cue: Start,
    #[case] case: SyncCase,
) {
    let mut harness = ProductHarness::new(case, &sources, cue, Audible::Deck(0)).await;
    let operation = prepare_cue(&mut harness, case, rate, cue).await;

    let receipts = render_until(&mut harness, case, operation, INSTALLED).await;
    assert_eq!(
        receipts,
        [Delivered {
            operation,
            rejected: INSTALLED,
        }],
        "{}: the owner records exactly one proven lane",
        case.id()
    );
    let item = harness.decks[0].current().expect("paused current").id;
    harness.decks[0].observe(|observation| {
        let cues: Vec<_> = observation
            .cues
            .iter()
            .filter(|(owner, _)| *owner == item)
            .collect();
        assert_eq!(
            cues.len(),
            1,
            "the paused SYNC submits exactly one cue segment"
        );
        assert!(matches!(operation, Operation::Cue { seq, .. } if seq == cues[0].1));
    });
    let snapshot = harness.decks[0]
        .read_snapshot()
        .await
        .into_iter()
        .map(|(snapshot, _)| snapshot)
        .find(|snapshot| snapshot.track.item == item)
        .expect("owner reports the paused linked track");
    assert_eq!(snapshot.sync, SyncStatus::On);
    assert!(
        !snapshot.track.pending_lane,
        "paused SYNC segment is Ready before Play"
    );
    let prepared = prepared_mark(&harness, cue);
    assert!(
        (snapshot.track.position.as_secs_f64() - prepared).abs() <= 1.0 / rate,
        "the Ready segment begins at the exact prepared source frame"
    );
    // Ruling: installed mapped lane before sound → one cue Seq reaches Ready, then one native Applied Start and silence before its aligned first PCM — spec §4.5, §11.3.
    let opened = harness.host.position();
    harness.play_all().await;
    let mut pcm = Vec::new();
    let deadline = Instant::now() + RECEIPT_TIMEOUT;
    let since = loop {
        hang_tick!(
            "a_cued_sync_installs_mapped_pcm_before_anything_sounds: waiting for progress"
                .to_owned()
        );
        pcm.extend(harness.render(case, BLOCK_FRAMES).await);
        let snapshot = harness.decks[0]
            .track_snapshot(item)
            .expect("playing linked track");
        if let TrackStatus::Playing { since } = snapshot.track.status
            && pcm.iter().any(|sample: &f32| sample.abs() > f32::EPSILON)
        {
            assert_eq!(snapshot.sync, SyncStatus::On);
            hang_reset!();
            break since;
        }
        assert!(
            Instant::now() < deadline,
            "{}: aligned Start produced no PCM",
            case.id()
        );
    };
    harness.decks[0].observe(|observation| {
        let starts: Vec<_> = observation.starts.iter().filter(|(owner, _)| *owner == item).collect();
        assert_eq!(starts.len(), 1, "the Ready cue starts exactly once");
        let seq = starts[0].1;
        let returned: Vec<_> = observation.deck.iter().filter(|(answered, _)| *answered == seq).collect();
        assert_eq!(returned, [&(seq, INSTALLED)], "one native Applied receipt for Start");
        assert_eq!(observation.settled.iter().filter(|(owner, receipt)| {
            *owner == item && matches!(receipt, kithara::play::Settled::Applied { seq: answered, at } if *answered == seq && *at == since)
        }).count(), 1, "the owner settles that Start on its aligned frame exactly once");
    });
    let beat_frames = u64::from(case.sample_rate) / 2;
    let start = since
        .frames_since(SessionFrame::new(0))
        .expect("nonnegative Start frame");
    assert_eq!(
        start % beat_frames,
        0,
        "the first audible segment starts on the Host beat"
    );
    let silence_samples = usize::try_from(start.checked_sub(opened).expect("Start follows Play"))
        .expect("start offset fits usize")
        * usize::from(CHANNELS);
    assert!(
        silence_samples < pcm.len(),
        "the capture includes the aligned Start"
    );
    assert!(
        pcm[..silence_samples].iter().all(|sample| *sample == 0.0),
        "no mapped PCM sounds before the aligned Start"
    );
    drop(harness);
}

#[kithara::test(native, tokio, multi_thread, serial, timeout(Duration::from_secs(60)))]
#[case::tunnel(tunnel_sources().await, TUNNEL_RATE, TUNNEL_CUE)]
#[case::tunnel_weak_beat(tunnel_sources().await, TUNNEL_RATE, TUNNEL_WEAK_CUE)]
#[case::newtechno(newtechno_sources().await, NEWTECHNO_RATE, NEWTECHNO_PHRASE)]
async fn unloading_the_track_reports_its_installed_lane_cancelled(
    #[case] sources: PreparedSources,
    #[case] rate: f64,
    #[case] cue: Start,
) {
    let case = STAGED_CUE_BESIDE_A_DECK;
    let mut harness = ProductHarness::new(case, &sources, cue, Audible::Deck(0)).await;
    let operation = prepare_cue(&mut harness, case, rate, cue).await;
    let _ = render_until(&mut harness, case, operation, INSTALLED).await;

    // Ruling: unloading cancels an installed prepared lane → unload supersedes a scheduled Start's slot basis, with Stale and no Applied — spec §11.3.
    let operation = schedule_start(&mut harness, case).await;
    let control = harness.decks[0].control().clone();
    harness
        .host
        .run(move || control.clear())
        .await
        .expect("clear the loaded queue");
    let receipts = render_until(&mut harness, case, operation, CANCELLED).await;
    harness.settle(case, 8).await;

    assert_eq!(
        delivered(&harness, &[operation]),
        receipts,
        "{}: a dropped lane reports once and nothing follows it",
        case.id()
    );
    assert_eq!(
        receipts.last(),
        Some(&Delivered {
            operation,
            rejected: CANCELLED,
        }),
        "{}: the owner takes the cancellation of its installed preparation",
        case.id()
    );
    drop(harness);
}

#[kithara::test(native, tokio, multi_thread, serial, timeout(Duration::from_secs(60)))]
#[case::tunnel(tunnel_sources().await, TUNNEL_RATE, TUNNEL_CUE)]
#[case::tunnel_weak_beat(tunnel_sources().await, TUNNEL_RATE, TUNNEL_WEAK_CUE)]
#[case::newtechno(newtechno_sources().await, NEWTECHNO_RATE, NEWTECHNO_PHRASE)]
async fn a_lane_the_worker_cannot_hold_is_refused_for_capacity(
    #[case] sources: PreparedSources,
    #[case] rate: f64,
    #[case] cue: Start,
) {
    let case = STAGED_WITHOUT_CAPACITY;
    let mut harness = ProductHarness::new(case, &sources, cue, Audible::Deck(0)).await;
    // Ruling: spec §11.3 Capacity needs a seated Warm load; an unseated Idle load does not consume the worker's foreground capacity.
    let contender = harness.add_seated_deck(case).await;
    let operation = prepare_incoming(&mut harness, case, rate, cue, contender, false).await;

    let receipts = render_until(&mut harness, case, operation, CAPACITY).await;
    assert!(
        receipts.iter().all(|receipt| *receipt
            == Delivered {
                operation,
                rejected: CAPACITY,
            }),
        "{}: nothing is installed without a slot: {receipts:?}",
        case.id()
    );
    let sounding = render_frames(&mut harness, case, LISTEN_FRAMES).await;
    assert!(
        sounding.iter().any(|sample| sample.abs() > f32::EPSILON),
        "{}: the sounding deck keeps its slot and plays on",
        case.id()
    );
    drop(harness);
}

#[kithara::test(native, tokio, multi_thread, serial, timeout(Duration::from_secs(120)))]
#[case::tunnel(tunnel_sources().await, TUNNEL_RATE, TUNNEL_CUE, TUNNEL_SUPERSEDING_CUE)]
#[case::tunnel_weak_beat(
    tunnel_sources().await,
    TUNNEL_RATE,
    TUNNEL_WEAK_CUE,
    TUNNEL_SUPERSEDING_CUE
)]
#[case::newtechno(
    newtechno_sources().await,
    NEWTECHNO_RATE,
    NEWTECHNO_PHRASE,
    NEWTECHNO_SUPERSEDING_CUE
)]
#[kithara_test_utils::kithara::hang_watchdog(timeout = Duration::from_secs(5), ctx = String)]
async fn the_sounding_lane_plays_on_while_its_staged_lane_is_superseded(
    #[case] sources: PreparedSources,
    #[case] rate: f64,
    #[case] cue: Start,
    #[case] superseding: Start,
) {
    let case = STAGED_BESIDE_PLAYBACK;
    let (control_built, control_opened, control, control_trace, control_closed) = {
        let control = STAGED_BESIDE_PLAYBACK_CONTROL;
        let mut harness =
            ProductHarness::new_for_block(control, &sources, cue, Audible::Deck(0), BLOCK_FRAMES)
                .await;
        let built = Window::read(&harness, control);
        let opened = open_window(&mut harness, control).await;
        let (pcm, trace) = render_window(&mut harness, control, None).await;
        let closed = Window::read(&harness, control);
        drop(harness);
        (built, opened, pcm, trace, closed)
    };
    let mut harness =
        ProductHarness::new_for_block(case, &sources, cue, Audible::Deck(0), BLOCK_FRAMES).await;
    let candidate_built = Window::read(&harness, case);
    assert_eq!(
        candidate_built,
        control_built,
        "{}: the two sides of this comparison differ only in the lane staged \
         beside the sounding deck, and nothing has been staged yet, so the two \
         builds must leave their decks on the same frame. They did not, which \
         makes every later sample-by-sample comparison a comparison of two \
         different points of the same timeline",
        case.id(),
    );
    harness.mark("staged cue, then a superseding cue");
    // Ruling: superseded prepared lanes → two rapid selections of incoming linked tracks waiting for Grid, without touching the sounding track — spec §6 scenario 11, §11.3.
    let superseded = prepare_incoming(&mut harness, case, rate, cue, 0, true).await;
    let candidate_staged = Window::read(&harness, case);
    let successor = prepare_incoming(&mut harness, case, rate, superseding, 0, true).await;
    let candidate_superseded = Window::read(&harness, case);
    assert_eq!(
        (candidate_staged, candidate_superseded),
        (candidate_built, candidate_built),
        "{}: staging a lane beside the sounding deck moved that deck. Only a \
         render advances a deck and neither staging call renders one, so the \
         deck should have held the {candidate_built} its build left it on; it \
         stood on {candidate_staged} once a lane was staged beside it and on \
         {candidate_superseded} once that lane was superseded. This is the \
         displacement the comparison below would otherwise report second-hand, \
         as two windows opening apart or as audio read from the wrong place, \
         depending only on whether it happens to cross a block boundary",
        case.id(),
    );
    let candidate_opened = open_window(&mut harness, case).await;
    let (candidate, _) = render_window(&mut harness, case, Some(&control_trace)).await;
    let candidate_closed = Window::read(&harness, case);
    release_incoming_grid(&harness, successor).await;
    let _ = render_until(&mut harness, case, successor, INSTALLED).await;
    let Operation::Load { item, .. } = successor else {
        panic!("successor load")
    };
    let deadline = Instant::now() + RECEIPT_TIMEOUT;
    loop {
        hang_tick!(
            "the_sounding_lane_plays_on_while_its_staged_lane_is_superseded: waiting for progress"
                .to_owned()
        );
        if let Some(seq) = harness.decks[0].observe(|observation| {
            observation
                .cues
                .iter()
                .rev()
                .find_map(|&(owner, seq)| (owner == item).then_some(seq))
        }) {
            let ready = Operation::Cue { item, seq };
            assert_eq!(
                render_until(&mut harness, case, ready, INSTALLED).await,
                [Delivered {
                    operation: ready,
                    rejected: INSTALLED
                }]
            );
            harness.decks[0].observe(|observation| {
                assert_eq!(
                    observation
                        .cues
                        .iter()
                        .filter(|(owner, _)| *owner == item)
                        .count(),
                    1,
                    "only one cue segment is submitted for the successor"
                );
            });
            let snapshot = harness.decks[0]
                .read_snapshot()
                .await
                .into_iter()
                .map(|(snapshot, _)| snapshot)
                .find(|snapshot| snapshot.track.item == item)
                .expect("owner reports the successor linked track");
            assert_eq!(snapshot.sync, SyncStatus::On);
            assert!(!snapshot.track.pending_lane, "successor segment is Ready");
            hang_reset!();
            break;
        }
        assert!(
            Instant::now() < deadline,
            "{}: successor did not prepare a segment",
            case.id()
        );
        let _ = harness.render(case, BLOCK_FRAMES).await;
    }
    let receipts = delivered(&harness, &[superseded, successor]);
    // Ruling: superseded preparation has no rejection → replaced Load is cancelled or already applied; only the successor reaches Ready — spec §6 scenario 11, §11.3.
    assert!(
        receipts.iter().all(|receipt| {
            (receipt.operation == superseded
                && matches!(receipt.rejected, INSTALLED | LinkVerdict::Cancelled))
                || *receipt
                    == Delivered {
                        operation: successor,
                        rejected: INSTALLED,
                    }
        }),
        "{}: the successor installs once; a replaced Load is cancelled or was \
         already applied before replacement: {receipts:?}",
        case.id()
    );
    assert_eq!(
        receipts
            .iter()
            .filter(|receipt| receipt.operation == successor)
            .count(),
        1
    );
    assert!(
        receipts
            .iter()
            .filter(|receipt| receipt.operation == superseded)
            .count()
            <= 1
    );
    assert!(
        harness.decks[0]
            .track_snapshot(match superseded {
                Operation::Load { item, .. } => item,
                _ => panic!("superseded load"),
            })
            .is_none(),
        "the replaced incoming track cannot sound"
    );

    assert_eq!(
        candidate.len(),
        control.len(),
        "{}: the two renders must cover the same window",
        case.id(),
    );
    assert_eq!(
        candidate_opened,
        control_opened,
        "{}: the two windows opened at different points of the deck's own \
         timeline, so one covers audio the other has already played. The two \
         checks above already put both decks on {candidate_built} after their \
         builds and held them there through the staging, so what moved this \
         one moved it inside the blocks rendered while waiting for the lead. \
         That loop steps whole blocks, so a deck displaced within a block \
         carries the displacement into its opening rather than losing it",
        case.id(),
    );
    assert_eq!(
        divergence(&candidate, &control),
        None,
        "{}: staging beside the sounding lane changed what it plays; both \
         windows opened on {candidate_opened} and took {LISTEN_FRAMES} frames, \
         closing with the candidate on {candidate_closed} and the control on \
         {control_closed}; closings that disagree put the two decks on \
         different timelines, closings that agree put different audio on one",
        case.id(),
    );
    drop(harness);
}

/// Where the sounding deck stood when a measured window opened or closed.
///
/// Two harnesses compared sample by sample have to open their window at the
/// same point in the deck's own timeline. A divergence that starts at frame 0
/// while both levels agree reads as that timeline being shifted, and nothing
/// in the PCM says which side moved - the deck's own position does.
///
/// Read at both ends, it also separates the two ways a divergence that starts
/// mid-window can happen. The same count of frames went into each render, so
/// two decks that close on different positions were running their own
/// timelines at different speeds, and two that close together were handed
/// different audio to play on one.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct Window {
    /// The deck's own position, in frames of the session it renders into.
    /// Absent until the deck reports one.
    frames: Option<u64>,
    playing: bool,
}

impl Window {
    /// Reads where the deck stands now, in the session's own frames so that
    /// two readings compare exactly.
    fn read(harness: &ProductHarness, case: SyncCase) -> Self {
        let playback = harness.decks[0].playback_view();
        let frames = playback
            .position
            .map(|seconds| seconds * f64::from(case.sample_rate))
            .filter(|frames| frames.is_finite() && *frames >= 0.0)
            .map(|frames| frames.round().as_());
        Self {
            frames,
            playing: playback.playing,
        }
    }
}

impl fmt::Display for Window {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.frames {
            Some(frames) => write!(formatter, "deck 0 at frame {frames}"),
            None => formatter.write_str("deck 0 reporting no position"),
        }?;
        write!(formatter, ", playing {}", self.playing)
    }
}

/// Renders the measured window a block at a time, recording where the deck
/// stood after each, and stops on the first block whose reading parts from a
/// reference trace.
///
/// A playing deck advances by the frames rendered into it, and the control's
/// close measures exactly that: it ends on its opening plus the whole window.
/// Its own trace therefore says the same thing block by block, which is what
/// makes it worth comparing against - a measurement, not a tolerance someone
/// picked.
///
/// Stopping on the block that parts, rather than at the close, is the point of
/// keeping the trace at all. Only a failing attempt writes a dump, and the
/// flight ring inside one holds a fraction of a second of probes; a loss first
/// named six seconds after it happened has already fallen out of the ring that
/// would have shown it happen.
#[kithara_test_utils::kithara::hang_watchdog(timeout = Duration::from_secs(5), ctx = String)]
async fn render_window(
    harness: &mut ProductHarness,
    case: SyncCase,
    reference: Option<&[Window]>,
) -> (Vec<f32>, Vec<Window>) {
    let blocks = LISTEN_FRAMES.div_ceil(BLOCK_FRAMES);
    let mut pcm = Vec::with_capacity(LISTEN_FRAMES * usize::from(CHANNELS));
    let mut trace = Vec::with_capacity(blocks);
    while trace.len() < blocks {
        hang_tick!("render_window: waiting for progress".to_owned());
        let rendered = pcm.len() / usize::from(CHANNELS);
        let step = (LISTEN_FRAMES - rendered).min(BLOCK_FRAMES);
        pcm.extend_from_slice(&render_frames(harness, case, step).await);
        let here = Window::read(harness, case);
        if let Some(&expected) = reference.and_then(|side| side.get(trace.len())) {
            assert_eq!(
                here,
                expected,
                "{}: the deck stopped keeping up with the window on block {} of \
                 {blocks}, {} frames in. Both sides render the same blocks into \
                 a playing deck, so both advance by what was rendered, and the \
                 control's own trace is that statement measured rather than \
                 assumed. A deck that parts from it here is the displacement \
                 the comparison at the close can only report second-hand, as \
                 two windows opening apart or as audio read from the wrong \
                 place",
                case.id(),
                trace.len() + 1,
                rendered + step,
            );
        }
        trace.push(here);
        hang_reset!();
    }
    (pcm, trace)
}

/// Renders whole blocks until the deck has reached [`WINDOW_LEAD_FRAMES`], and
/// reports where the window opens.
///
/// Both sides of a comparison call this, and they open on the same frame only
/// while both step the same grid: a render advances a deck by one block, so a
/// side its build left further back catches up to the same first position past
/// the lead. A side that is off that grid instead stops at the first of its own
/// steps past the lead, which is a different frame, and the caller's assertion
/// on the two openings is what reports it. Measured: a deck 359 frames off the
/// block grid opened on 8551 against the other's 8192.
#[kithara_test_utils::kithara::hang_watchdog(timeout = Duration::from_secs(5), ctx = String)]
async fn open_window(harness: &mut ProductHarness, case: SyncCase) -> Window {
    let mut previous = Window::read(harness, case).frames;
    for _ in 0..WINDOW_BLOCK_BUDGET {
        hang_tick!("open_window: waiting for progress".to_owned());
        let window = Window::read(harness, case);
        if window.frames > previous {
            previous = window.frames;
            hang_reset!();
        }
        if window
            .frames
            .is_some_and(|frames| frames >= WINDOW_LEAD_FRAMES)
        {
            hang_reset!();
            return window;
        }
        let _ = harness.render(case, BLOCK_FRAMES).await;
    }
    panic!(
        "{}: the deck never reached frame {WINDOW_LEAD_FRAMES} in \
         {WINDOW_BLOCK_BUDGET} rendered blocks, the lead a measured window \
         opens after; it stopped at {}",
        case.id(),
        Window::read(harness, case),
    );
}

/// What separates two renders of the same lane, beyond where it starts.
///
/// One sample off by a rounding step and a different signal from the first
/// frame both report as a diverging position, and the buffers do not survive
/// into a stress report: only the count and the widest gap tell them apart.
///
/// Each side's level comes too, because once every sample differs the count
/// has nothing left to say. Two levels that agree while no sample does put
/// the same audio at a different place in its own timeline; two that disagree
/// put different audio there.
///
/// Levels alone still cannot name which: a render turned up and a render with
/// a second lane summed into it both read as louder. The best-fit gain and
/// what it leaves behind separate them. A residual near zero says the
/// candidate IS the control at another gain, and the accusation is the
/// mixer's; a residual near the control's own level says the candidate
/// carries audio the control never had, and the accusation is that a staged
/// lane reached the output. These land on opposite halves of the product, so
/// the report must not have to guess between them.
fn divergence(candidate: &[f32], control: &[f32]) -> Option<String> {
    let mut first = None;
    let mut differing = 0usize;
    let mut widest = 0.0f32;
    for (sample, (heard, expected)) in candidate.iter().zip(control).enumerate() {
        if heard.to_bits() == expected.to_bits() {
            continue;
        }
        first.get_or_insert((sample, *heard, *expected));
        differing = differing.saturating_add(1);
        widest = widest.max((heard - expected).abs());
    }
    let (sample, heard, expected) = first?;
    let (gain, residual) = fit(candidate, control);
    Some(format!(
        "from frame {} ({heard} against {expected}); {differing} of {} samples differ, \
         widest {widest}; level {} against {}; best-fit gain {gain} leaves residual {residual}",
        sample / usize::from(CHANNELS),
        candidate.len(),
        level(candidate),
        level(control),
    ))
}

/// The gain that best explains `candidate` as `control`, and the level of what
/// that gain cannot explain.
///
/// The gain is the least-squares fit, and the residual is the level of
/// `candidate - gain * control` measured against the control's own level, so
/// it reads as a fraction rather than an absolute the reader has to scale by
/// hand. A silent control leaves nothing to fit against and reports no gain.
fn fit(candidate: &[f32], control: &[f32]) -> (f32, f32) {
    let mut energy = 0.0f64;
    let mut cross = 0.0f64;
    for (heard, expected) in candidate.iter().zip(control) {
        energy += f64::from(*expected) * f64::from(*expected);
        cross += f64::from(*heard) * f64::from(*expected);
    }
    if energy == 0.0 {
        return (f32::NAN, level(candidate));
    }
    let gain = cross / energy;
    let mut left = 0.0f64;
    for (heard, expected) in candidate.iter().zip(control) {
        let unexplained = f64::from(*heard) - gain * f64::from(*expected);
        left += unexplained * unexplained;
    }
    (
        gain.to_f32().expect("finite fitted gain"),
        (left / energy).sqrt().to_f32().expect("finite residual"),
    )
}

/// Root-mean-square of `pcm`, the one summary of a render that survives a
/// shift along its own timeline.
fn level(pcm: &[f32]) -> f32 {
    if pcm.is_empty() {
        return 0.0;
    }
    let sum: f64 = pcm
        .iter()
        .map(|sample| f64::from(*sample) * f64::from(*sample))
        .sum();
    let len: f64 = pcm.len().as_();
    (sum / len).sqrt().as_()
}
