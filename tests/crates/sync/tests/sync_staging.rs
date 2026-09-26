#![cfg(not(target_os = "android"))]
#![cfg(not(target_arch = "wasm32"))]

use kithara::{
    platform::{
        sync::Arc,
        time::{Duration, Instant},
    },
    signal::SessionFrame,
    sync::{
        AlignmentSource, LoadGeneration, SyncAdmission, SyncGroup, SyncIntent, SyncOperation,
        SyncStatusSnapshot,
    },
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
async fn prepare_cue(
    harness: &mut ProductHarness,
    case: SyncCase,
    rate: f64,
    cue: Start,
    defer_frames: usize,
) -> u64 {
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
    let now = i64::try_from(harness.host.position()).unwrap_or(i64::MAX);
    let window_start =
        SessionFrame::new(now.saturating_add(i64::try_from(defer_frames).unwrap_or(i64::MAX)));
    let cue = AssetFrame::new(seconds * rate).expect("fixture cue is finite");
    let admission = harness
        .host
        .with(move |host| {
            host.transact(SyncOperation::Prepare {
                target,
                load: LoadGeneration::first(),
                transport,
                source: AlignmentSource::Prepared(cue),
                window: window_start..SessionFrame::new(i64::MAX),
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
    let operation = prepare_cue(&mut harness, case, rate, cue, 0).await;

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
async fn unloading_the_track_retires_its_installed_lane_without_a_stale_receipt(
    #[case] sources: PreparedSources,
    #[case] rate: f64,
    #[case] cue: Start,
) {
    let case = STAGED_CUE_BESIDE_A_DECK;
    let mut harness = ProductHarness::new(case, &sources, cue, Audible::Deck(0)).await;
    let operation = prepare_cue(&mut harness, case, rate, cue, LISTEN_FRAMES + BLOCK_FRAMES).await;
    let installed = render_until(&mut harness, case, operation, INSTALLED).await;
    let deck = Arc::clone(&harness.decks[0]);
    let before = harness
        .host
        .with(move |host| host.deck_sync_state(&deck))
        .await
        .expect("read installed deck state");
    assert!(
        matches!(before.status, SyncStatusSnapshot::Prepared { operation: pending, .. } if u64::from(pending) == operation),
        "the installed lane is pending until its first PCM"
    );
    let SyncStatusSnapshot::Prepared { activation, .. } = before.status else {
        panic!("the prepared lane has a future activation");
    };
    assert!(
        i64::from(activation)
            > i64::try_from(harness.host.position()).expect("fixture output fits")
                + i64::try_from(BLOCK_FRAMES * 8).expect("fixture span fits"),
        "the unplayed ticket stays ahead of the clear and follow-up render"
    );

    let control = harness.decks[0].control().clone();
    harness.host.run(move || control.clear()).await;
    let after_withdrawal = delivered();
    assert!(
        after_withdrawal.starts_with(&installed),
        "the Installed receipt remains recorded through clear"
    );
    harness.settle(case, 8).await;
    let deck = Arc::clone(&harness.decks[0]);
    let after = harness
        .host
        .with(move |host| host.deck_sync_state(&deck))
        .await
        .expect("read cleared deck state");

    assert!(
        !matches!(
            after.status,
            SyncStatusSnapshot::Prepared { .. } | SyncStatusSnapshot::WaitingForGrid { .. }
        ),
        "{}: cleared track cannot retain any pending preparation",
        case.id()
    );
    assert_eq!(harness.decks[0].len(), 0, "cleared deck has no resident");
    assert!(harness.decks[0].playback_view().buffered.is_none());
    assert_eq!(
        delivered(),
        after_withdrawal,
        "no receipt after clear returns"
    );
    let after_clear = render_frames(&mut harness, case, BLOCK_FRAMES * 8).await;
    assert!(
        after_clear
            .iter()
            .all(|sample| sample.abs() <= f32::EPSILON),
        "{}: cleared deck produces no PCM after custody returns",
        case.id()
    );
    assert_eq!(delivered(), after_withdrawal, "no late receipt after clear");
}

#[kithara::test(
    native,
    tokio,
    multi_thread,
    serial,
    flash(false),
    timeout(Duration::from_secs(60))
)]
#[case::tunnel(tunnel_sources().await, TUNNEL_CUE)]
#[case::tunnel_weak_beat(tunnel_sources().await, TUNNEL_WEAK_CUE)]
#[case::newtechno(newtechno_sources().await, NEWTECHNO_PHRASE)]
async fn a_lane_the_worker_cannot_hold_is_refused_for_capacity(
    #[case] sources: PreparedSources,
    #[case] cue: Start,
) {
    let case = STAGED_WITHOUT_CAPACITY;
    let mut harness = ProductHarness::new(case, &sources, cue, Audible::Deck(0)).await;
    let deck = Arc::clone(&harness.decks[0]);
    harness
        .host
        .with(move |host| host.request_deck_sync(&deck, SyncIntent::Enable))
        .await
        .expect("public Enable accepts the sounding deck");
    let deadline = Instant::now() + RECEIPT_TIMEOUT;
    let receipts = loop {
        let receipts = delivered();
        if receipts.iter().any(|receipt| receipt.rejected == CAPACITY) {
            break receipts;
        }
        assert!(
            Instant::now() < deadline,
            "{}: no capacity refusal reached Host",
            case.id()
        );
        let _ = harness.render(case, BLOCK_FRAMES).await;
    };
    assert_eq!(
        receipts.len(),
        1,
        "{}: one public request issues one lane",
        case.id()
    );
    assert_eq!(receipts[0].rejected, CAPACITY);
    assert!(receipts[0].accepted, "Host records the capacity refusal");
    let sounding = render_frames(&mut harness, case, LISTEN_FRAMES).await;
    assert!(
        sounding.iter().any(|sample| sample.abs() > f32::EPSILON),
        "{}: the sounding deck keeps its slot and plays on",
        case.id()
    );
    assert_eq!(
        delivered(),
        receipts,
        "the refused lane emits no later receipt"
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
    let control = {
        let control = STAGED_BESIDE_PLAYBACK_CONTROL;
        let mut harness =
            ProductHarness::new_for_block(control, &sources, cue, Audible::Deck(0), BLOCK_FRAMES)
                .await;
        render_frames(&mut harness, control, LISTEN_FRAMES).await
    };
    let mut harness =
        ProductHarness::new_for_block(case, &sources, cue, Audible::Deck(0), BLOCK_FRAMES).await;
    harness.mark("staged cue, then a superseding cue");
    let after_listening = LISTEN_FRAMES + BLOCK_FRAMES;
    let superseded = prepare_cue(&mut harness, case, rate, cue, after_listening).await;
    let successor = prepare_cue(&mut harness, case, rate, superseding, after_listening).await;
    let deck = Arc::clone(&harness.decks[0]);
    let state = harness
        .host
        .with(move |host| host.deck_sync_state(&deck))
        .await
        .expect("read the superseding preparation");
    let SyncStatusSnapshot::Prepared {
        operation,
        activation,
        ..
    } = state.status
    else {
        panic!("the successor is prepared before the listening capture");
    };
    assert_eq!(u64::from(operation), successor);
    assert!(
        i64::from(activation)
            > i64::try_from(harness.host.position()).expect("fixture output fits")
                + i64::try_from(LISTEN_FRAMES).expect("fixture capture fits"),
        "the successor cannot begin sounding during the bitwise comparison"
    );
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

    assert_eq!(candidate.len(), control.len());
    let diverged = candidate
        .iter()
        .zip(&control)
        .position(|(heard, expected)| heard.to_bits() != expected.to_bits());
    assert_eq!(
        diverged.map(|sample| sample / usize::from(CHANNELS)),
        None,
        "{}: staging beside the sounding lane changed what it plays from this frame on",
        case.id(),
    );
}
