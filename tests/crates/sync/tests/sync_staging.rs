#![cfg(not(target_os = "android"))]
#![cfg(not(target_arch = "wasm32"))]

use kithara::{
    platform::time::{Duration, Instant},
    signal::SessionFrame,
    sync::{AlignmentSource, LoadGeneration, SyncAdmission, SyncGroup, SyncIntent, SyncOperation},
    warp::AssetFrame,
};
use kithara_integration_tests::{grid::Start, kithara, usdt_trace};

use super::{
    sync_listening::render_frames,
    sync_product_matrix::{
        Audible, BLOCK_FRAMES, CHANNELS, NEWTECHNO_PHRASE, PreparedSources, ProductHarness,
        STAGED_BESIDE_PLAYBACK, STAGED_BESIDE_PLAYBACK_CONTROL, STAGED_CUE,
        STAGED_CUE_BESIDE_A_DECK, STAGED_UNDER_LOOSE_DEADLINE, STAGED_WITHOUT_CAPACITY, SyncCase,
        TUNNEL_CUE, newtechno_sources, tunnel_sources,
    },
};

/// Probe the executor fires once the group owner answered a receipt.
const RECEIPT: &str = "sync_receipt_delivered";
/// Probe codes of the receipts these scenarios expect.
const INSTALLED: u64 = 0;
const CAPACITY: u64 = 3;
const CANCELLED: u64 = 4;
const RECEIPT_TIMEOUT: Duration = Duration::from_secs(30);
/// Longer than a lane's ring holds, so the sounding lane has to keep decoding.
const LISTEN_FRAMES: usize = 48_000 * 6;
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

/// One receipt the owner answered: the operation it answers for, its
/// rejection code and whether the owner recorded it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct Delivered {
    operation: u64,
    rejected: u64,
    accepted: bool,
}

fn delivered() -> Vec<Delivered> {
    usdt_trace::events()
        .iter()
        .filter(|event| event.probe == RECEIPT)
        .map(|event| Delivered {
            operation: event.field("operation").expect("receipt operation"),
            rejected: event.field("rejected").expect("receipt rejection"),
            accepted: event.field("accepted").expect("receipt answer") == 1,
        })
        .collect()
}

/// Renders until the owner has answered a receipt with `code` for
/// `operation`.
async fn render_until(
    harness: &mut ProductHarness,
    case: SyncCase,
    operation: u64,
    code: u64,
) -> Vec<Delivered> {
    let deadline = Instant::now() + RECEIPT_TIMEOUT;
    loop {
        let receipts = delivered();
        if receipts
            .iter()
            .any(|receipt| receipt.operation == operation && receipt.rejected == code)
        {
            return receipts;
        }
        assert!(
            Instant::now() < deadline,
            "{}: no receipt {code} for operation {operation} reached the owner; delivered {receipts:?}",
            case.id()
        );
        let _ = harness.render(case, BLOCK_FRAMES).await;
    }
}

/// Syncs the deck, then asks its group to prepare the track from the exact
/// recording frame `seconds` in: a launch the executor stages beside
/// whatever the deck plays. Returns the operation the preparation carries.
async fn prepare_cue(harness: &mut ProductHarness, case: SyncCase, rate: f64, cue: Start) -> u64 {
    let seconds = harness.start_seconds(0, cue);
    harness.request_sync_intent(case, SyncIntent::Enable).await;
    let deck = harness.decks[0].id();
    let topology = harness
        .host
        .with(|host| host.topology())
        .await
        .unwrap_or_else(|error| panic!("{}: read the session topology: {error}", case.id()));
    let target = topology
        .members()
        .iter()
        .filter(|member| member.grid().id() == deck)
        .find_map(|member| {
            member
                .group_topology()?
                .members()
                .first()
                .map(|track| track.grid().id())
        })
        .unwrap_or_else(|| panic!("{}: the deck holds no track grid", case.id()));
    let transport = harness
        .host
        .transport_revision()
        .await
        .unwrap_or_else(|error| panic!("{}: query Host transport: {error}", case.id()));
    let now = SessionFrame::new(i64::try_from(harness.host.position()).unwrap_or(i64::MAX));
    let cue = AssetFrame::new(seconds * rate).expect("fixture cue is finite");
    let admission = harness
        .host
        .with(move |host| {
            host.transact(SyncOperation::Prepare {
                target,
                load: LoadGeneration::first(),
                transport,
                source: AlignmentSource::Prepared(cue),
                window: now..SessionFrame::new(i64::MAX),
            })
        })
        .await
        .unwrap_or_else(|rejected| panic!("{}: prepare the cue: {rejected}", case.id()));
    let SyncAdmission::Prepared(preparation) = admission else {
        panic!("{}: the cue was not prepared: {admission:?}", case.id());
    };
    u64::from(preparation.stamp().operation())
}

#[kithara::test(
    native,
    tokio,
    multi_thread,
    serial,
    flash(false),
    timeout(Duration::from_secs(60))
)]
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
            accepted: true,
        }],
        "{}: the owner records exactly one proven lane",
        case.id()
    );
}

#[kithara::test(
    native,
    tokio,
    multi_thread,
    serial,
    flash(false),
    timeout(Duration::from_secs(60))
)]
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

    let control = harness.decks[0].control().clone();
    harness.host.run(move || control.clear()).await;
    let receipts = render_until(&mut harness, case, operation, CANCELLED).await;
    harness.settle(case, 8).await;

    assert_eq!(
        delivered(),
        receipts,
        "{}: a dropped lane reports once and nothing follows it",
        case.id()
    );
    assert_eq!(
        receipts.last(),
        Some(&Delivered {
            operation,
            rejected: CANCELLED,
            accepted: true,
        }),
        "{}: the owner takes the cancellation of its installed preparation",
        case.id()
    );
}

#[kithara::test(
    native,
    tokio,
    multi_thread,
    serial,
    flash(false),
    timeout(Duration::from_secs(60))
)]
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
    let operation = prepare_cue(&mut harness, case, rate, cue).await;

    let receipts = render_until(&mut harness, case, operation, CAPACITY).await;
    assert!(
        receipts.iter().all(|receipt| *receipt
            == Delivered {
                operation,
                rejected: CAPACITY,
                accepted: true,
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
}

#[kithara::test(
    native,
    tokio,
    multi_thread,
    serial,
    flash(false),
    timeout(Duration::from_secs(120))
)]
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
async fn the_sounding_lane_plays_on_while_its_staged_lane_is_superseded(
    #[case] sources: PreparedSources,
    #[case] rate: f64,
    #[case] cue: Start,
    #[case] superseding: Start,
) {
    let case = STAGED_BESIDE_PLAYBACK;
    let (control_opened, control) = {
        let control = STAGED_BESIDE_PLAYBACK_CONTROL;
        let mut harness =
            ProductHarness::new_for_block(control, &sources, cue, Audible::Deck(0), BLOCK_FRAMES)
                .await;
        let opened = window_opened(&harness);
        (
            opened,
            render_frames(&mut harness, control, LISTEN_FRAMES).await,
        )
    };
    let mut harness =
        ProductHarness::new_for_block(case, &sources, cue, Audible::Deck(0), BLOCK_FRAMES).await;
    harness.mark("staged cue, then a superseding cue");
    let superseded = prepare_cue(&mut harness, case, rate, cue).await;
    let successor = prepare_cue(&mut harness, case, rate, superseding).await;
    let candidate_opened = window_opened(&harness);
    let candidate = render_frames(&mut harness, case, LISTEN_FRAMES).await;
    let receipts = render_until(&mut harness, case, successor, INSTALLED).await;
    assert!(
        receipts.iter().all(|receipt| receipt.rejected == INSTALLED
            && (receipt.operation == superseded
                || receipt
                    == &Delivered {
                        operation: successor,
                        rejected: INSTALLED,
                        accepted: true,
                    })),
        "{}: the successor installs once; a superseded lane is dropped without a \
         rejection, installed at most before it was replaced: {receipts:?}",
        case.id()
    );

    assert_eq!(
        candidate.len(),
        control.len(),
        "{}: the two renders must cover the same window",
        case.id(),
    );
    assert_eq!(
        divergence(&candidate, &control),
        None,
        "{}: staging beside the sounding lane changed what it plays; \
         the window opened on {candidate_opened}, the control on {control_opened}",
        case.id(),
    );
}

/// Where the sounding deck stood when a measured window opened.
///
/// Two harnesses compared sample by sample have to open their window at the
/// same point in the deck's own timeline. A divergence that starts at frame 0
/// while both levels agree reads as that timeline being shifted, and nothing
/// in the PCM says which side moved - the deck's own position when the window
/// opened does.
fn window_opened(harness: &ProductHarness) -> String {
    let playback = harness.decks[0].playback_view();
    format!(
        "deck 0 at {:.6}s, playing {}",
        playback.position.unwrap_or(f64::NAN),
        playback.playing
    )
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
    #[expect(
        clippy::cast_possible_truncation,
        reason = "two ratios printed into a panic message, not signal values"
    )]
    let fitted = (gain as f32, (left / energy).sqrt() as f32);
    fitted
}

/// Root-mean-square of `pcm`, the one summary of a render that survives a
/// shift along its own timeline.
fn level(pcm: &[f32]) -> f32 {
    if pcm.is_empty() {
        return 0.0;
    }
    let sum: f64 = pcm.iter().map(|s| f64::from(*s) * f64::from(*s)).sum();
    #[expect(
        clippy::cast_possible_truncation,
        reason = "a level printed into a panic message, not a signal value"
    )]
    let rms = (sum / pcm.len() as f64).sqrt() as f32;
    rms
}
