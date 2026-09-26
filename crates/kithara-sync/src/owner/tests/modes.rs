use std::num::NonZeroU32;

use kithara_platform::sync::{Arc, Mutex};
use kithara_signal::{SessionEpoch, SessionFrame, TransportRevision};
use kithara_test_utils::kithara;
use kithara_warp::{
    AssetAxis, AssetExtent, AssetFrame, BeatGrid, BeatGridId, BeatGridQuery, BeatGridRevision,
    BeatGridSnapshot, BeatGridStamp, BeatGridState, BeatsPerMinute, MapAxis, MapPoint, MapPosition,
    PresentationFrontier, SessionAnchor, SessionAxis, SessionBeat, WarpPlan,
};
use num_traits::ToPrimitive;

use super::{Accept, TestGrid, TestGroup, preparation::asset_grid, session_grid};
use crate::{
    AlignmentSource, GroupState, LoadGeneration, ParentGridUpdate, SessionAxisUpdate,
    SyncAdmission, SyncApplied, SyncCapability, SyncEffect, SyncError, SyncExecutionReject,
    SyncGroup, SyncIntent, SyncMember, SyncMemberKind, SyncMode, SyncOperation, SyncPreparation,
    SyncReceipt, SyncStatusSnapshot, TopologyOperation, owner::descent::Parent,
};

/// Time constant the tempo fixtures approach a new target with.
const SMOOTHING_SECONDS: f64 = 0.005;

pub(super) type Group = GroupState<TestGroup>;

struct PublishedGrid {
    id: BeatGridId,
    current: Arc<Mutex<BeatGridSnapshot>>,
}

impl BeatGrid for PublishedGrid {
    fn id(&self) -> BeatGridId {
        self.id
    }

    fn snapshot(&self) -> BeatGridSnapshot {
        self.current.lock().clone()
    }
}

pub(super) fn rate(value: u32) -> NonZeroU32 {
    NonZeroU32::new(value).expect("invariant: fixture sample rate is non-zero")
}

pub(super) fn group_in(mode: SyncMode, member_kind: SyncMemberKind) -> Group {
    GroupState::unavailable(
        BeatGridId::allocate().expect("invariant: fixture group id is available"),
        rate(48_000),
        SessionEpoch::new(0),
        member_kind,
        mode,
    )
}

fn fixture_group() -> Group {
    group_in(SyncMode::Off, SyncMemberKind::Grid)
}

fn live_deck() -> Group {
    live_deck_at(120.0)
}

fn live_deck_at(bpm: f64) -> Group {
    GroupState::new(
        session_grid(
            BeatGridId::allocate().expect("grid id"),
            BeatGridRevision::first(),
            SessionEpoch::new(0),
            bpm / 60.0,
        ),
        SyncMemberKind::Grid,
    )
}

/// An Off deck with one gridded track and a live Host parent.
pub(super) fn owning_deck_with_parent() -> (Group, BeatGridId, BeatGridId) {
    let deck = BeatGridId::allocate().expect("deck id");
    let track = BeatGridId::allocate().expect("track id");
    let parent = parent_id();
    let mut group: Group = GroupState::owning(
        deck,
        rate(48_000),
        SessionEpoch::new(0),
        SyncMember::Grid {
            alignment: None,
            grid: Box::new(TestGrid(asset_grid(track, 480_000, 24_000))),
        },
    );
    group
        .accept_parent(parent_update(
            parent_stamp(parent, 1),
            anchor_at_rate(2.0, 48_000),
        ))
        .expect("host beat grid");
    (group, track, parent)
}

/// A deck owning a local 120 BPM timeline latched from its live grid.
pub(super) fn synced_deck() -> Group {
    synced_deck_at(120.0)
}

/// A deck owning a local `bpm` timeline latched from its live grid.
pub(super) fn synced_deck_at(bpm: f64) -> Group {
    let mut deck = live_deck_at(bpm);
    let _ = deck
        .transact(sync(deck.id(), SyncIntent::Disable))
        .expect("disable latches the live grid");
    let _ = deck
        .transact(tempo(deck.id(), bpm))
        .expect("a local deck accepts its own tempo");
    deck
}

pub(super) fn sync(target: BeatGridId, intent: SyncIntent) -> SyncOperation<TestGroup> {
    sync_at(target, intent, SessionFrame::new(0))
}

pub(super) fn sync_at(
    target: BeatGridId,
    intent: SyncIntent,
    activation: SessionFrame,
) -> SyncOperation<TestGroup> {
    SyncOperation::Sync {
        target,
        load: LoadGeneration::first(),
        transport: TransportRevision::first(),
        source: AlignmentSource::Prepared(AssetFrame::default()),
        activation,
        intent,
    }
}

fn tempo(target: BeatGridId, value: f64) -> SyncOperation<TestGroup> {
    tempo_at(target, value, SessionFrame::new(0))
}

pub(super) fn tempo_at(
    target: BeatGridId,
    value: f64,
    commit: SessionFrame,
) -> SyncOperation<TestGroup> {
    SyncOperation::Tempo {
        target,
        tempo: BeatsPerMinute::try_from(value).expect("finite positive bpm"),
        commit,
        smoothing: SMOOTHING_SECONDS,
    }
}

pub(super) fn transport_unavailable() -> SyncError {
    SyncError::CapabilityUnavailable {
        capability: SyncCapability::Transport,
    }
}

pub(super) fn anchor_at_rate(beats_per_second: f64, sample_rate: u32) -> SessionAnchor {
    SessionAnchor::new(
        SessionFrame::new(0),
        SessionBeat::new(0.0).expect("beat"),
        beats_per_second,
        rate(sample_rate),
    )
    .expect("session anchor")
}

pub(super) fn parent_update(parent: BeatGridStamp, anchor: SessionAnchor) -> ParentGridUpdate {
    ParentGridUpdate::new(parent, SessionEpoch::new(0), anchor, None)
}

pub(super) fn parent_stamp(parent: BeatGridId, revision: u32) -> BeatGridStamp {
    let mut value = BeatGridRevision::first();
    for _ in 1..revision {
        value = value
            .checked_next()
            .expect("invariant: fixture parent revision can advance");
    }
    BeatGridStamp::new(parent, value)
}

pub(super) fn parent_id() -> BeatGridId {
    BeatGridId::allocate().expect("parent identity")
}

fn grid_tempo_at(group: &Group, frame: i64) -> f64 {
    let grid = group.snapshot();
    let BeatGridQuery::Resolved(estimate) = grid.tempo_at(MapPoint::new(
        grid.stamp(),
        MapPosition::Session(SessionFrame::new(frame)),
    )) else {
        panic!("a live local grid resolves its tempo");
    };
    f64::from(*estimate.value())
}

fn projected_plan(preparation: &SyncPreparation) -> &WarpPlan {
    let SyncEffect::Projection { plan, .. } = preparation.effect() else {
        panic!("a sounding Host entry has a projected plan");
    };
    plan
}

fn plan_source_at(plan: &WarpPlan, frame: i64) -> u64 {
    let BeatGridQuery::Resolved(source) = plan.source_at(SessionFrame::new(frame)) else {
        panic!("the sounding plan covers output frame {frame}");
    };
    f64::from(source)
        .round()
        .to_u64()
        .expect("finite source frame")
}

fn presented_host_entry(group: &mut Group) -> SyncPreparation {
    let admission = group
        .transact(sync_at(
            group.id(),
            SyncIntent::Enable,
            SessionFrame::new(2_048),
        ))
        .expect("Host entry is accepted");
    let SyncAdmission::StateChanged { transition, .. } = admission else {
        panic!("Host entry changes the timeline: {admission:?}");
    };
    let [preparation] = transition.issued() else {
        panic!("one track enters the Host timeline");
    };
    let preparation = preparation.clone();
    present_preparation(group, &preparation);
    preparation
}

fn present_preparation(group: &mut Group, preparation: &SyncPreparation) {
    let plan = projected_plan(preparation);
    let _ = group
        .acknowledge(SyncReceipt::Installed(preparation.stamp()))
        .expect("the entry is installed");
    let _ = group
        .acknowledge(SyncReceipt::Armed(preparation.stamp()))
        .expect("the entry is armed");
    let _ = group
        .acknowledge(SyncReceipt::Presented(
            SyncApplied::builder()
                .stamp(preparation.stamp())
                .frontier(
                    PresentationFrontier::builder()
                        .warp_map(plan.activation().revision())
                        .source(plan.activation().source())
                        .output(plan.activation().output())
                        .build(),
                )
                .build(),
        ))
        .expect("the Host map is presented");
}

fn disable_from_presented(
    group: &Group,
    preparation: &SyncPreparation,
    observed: i64,
    activation: i64,
) -> SyncOperation<TestGroup> {
    let plan = projected_plan(preparation);
    SyncOperation::Sync {
        target: group.id(),
        load: preparation.stamp().load(),
        transport: preparation.stamp().transport(),
        source: AlignmentSource::Audible {
            frontier: PresentationFrontier::builder()
                .warp_map(plan.activation().revision())
                .source(plan_source_at(plan, observed))
                .output(SessionFrame::new(observed))
                .build(),
            speed: 1.0,
        },
        activation: SessionFrame::new(activation),
        intent: SyncIntent::Disable,
    }
}

pub(super) fn grid_beat_at(group: &Group, frame: i64) -> f64 {
    let grid = group.snapshot();
    let BeatGridQuery::Resolved(estimate) = grid.beat_at(MapPoint::new(
        grid.stamp(),
        MapPosition::Session(SessionFrame::new(frame)),
    )) else {
        panic!("a live local grid resolves its beat");
    };
    f64::from(*estimate.value().value())
}

#[kithara::test]
#[case::tempo(None)]
#[case::enable(Some(SyncIntent::Enable))]
#[case::free(Some(SyncIntent::Free))]
fn rejected_state_change_preserves_mode_and_tempo(#[case] intent: Option<SyncIntent>) {
    let mut group = group_in(SyncMode::LocalSync, SyncMemberKind::Grid);
    let _ = group
        .transact(tempo(group.id(), 90.0))
        .expect("local tempo");
    let tempo_before = group.tempo();
    group.next_operation = None;
    let operation = match intent {
        Some(intent) => sync(group.id(), intent),
        None => tempo(group.id(), 120.0),
    };
    let (error, _): (SyncError, SyncOperation<TestGroup>) = group
        .transact(operation)
        .expect_err("operation identities are exhausted")
        .into();
    assert!(matches!(error, SyncError::OperationIdExhausted { .. }));
    assert_eq!(group.mode(), SyncMode::LocalSync, "rejected mode changed");
    assert_eq!(group.tempo(), tempo_before, "rejected tempo changed");
}

#[kithara::test]
fn rejected_parent_anchor_preserves_the_committed_grid_and_anchor() {
    let mut group = group_in(SyncMode::HostSync, SyncMemberKind::Grid);
    let parent = parent_id();
    let committed = anchor_at_rate(2.0, 48_000);
    group
        .accept_parent(parent_update(parent_stamp(parent, 1), committed))
        .expect("first anchor");
    let stamp = group.snapshot().stamp();
    assert!(matches!(
        group.accept_parent(parent_update(
            parent_stamp(parent, 2),
            anchor_at_rate(2.0, 44_100)
        )),
        Err(SyncError::GridAxisChanged { .. })
    ));
    assert_eq!(group.snapshot().stamp(), stamp);
    assert_eq!(
        group
            .parent
            .and_then(Parent::segment)
            .map(|parent| parent.anchor()),
        Some(committed)
    );
}

#[kithara::test]
fn rejected_grid_change_preserves_the_whole_deck_transaction() {
    let mut group = group_in(SyncMode::HostSync, SyncMemberKind::Grid);
    let parent = parent_id();
    group
        .accept_parent(parent_update(
            parent_stamp(parent, 1),
            anchor_at_rate(2.0, 48_000),
        ))
        .expect("initial anchor");
    let _ = group
        .transact(sync(group.id(), SyncIntent::Disable))
        .expect("latch local sync");
    let _ = group
        .transact(tempo(group.id(), 90.0))
        .expect("local tempo");
    group
        .accept_parent(parent_update(
            parent_stamp(parent, 2),
            anchor_at_rate(2.0, 44_100),
        ))
        .expect("local mode records parent intent");
    let stamp = group.snapshot().stamp();
    let next_operation = group.next_operation;
    let status = group.status();
    let tempo = group.tempo();
    let operation = sync(group.id(), SyncIntent::Enable);
    let (error, _): (SyncError, SyncOperation<TestGroup>) = group
        .transact(operation)
        .expect_err("incompatible parent axis must reject the whole operation")
        .into();
    assert!(matches!(error, SyncError::GridAxisChanged { .. }));
    assert_eq!(group.mode(), SyncMode::LocalSync);
    assert_eq!(group.tempo(), tempo);
    assert_eq!(group.snapshot().stamp(), stamp);
    assert_eq!(group.next_operation, next_operation);
    assert_eq!(group.status(), status);
}

#[kithara::test]
fn local_tempo_transaction_preserves_the_beat_at_its_commit_frame() {
    let mut group = group_in(SyncMode::HostSync, SyncMemberKind::Grid);
    group
        .accept_parent(parent_update(
            parent_stamp(parent_id(), 1),
            anchor_at_rate(2.0, 48_000),
        ))
        .expect("initial anchor");
    let now = SessionFrame::new(48_000);
    let admission = group
        .transact(sync_at(group.id(), SyncIntent::Disable, now))
        .expect("latch local tempo");
    assert!(matches!(admission, SyncAdmission::StateChanged { .. }));
    let admission = group
        .transact(tempo_at(group.id(), 90.0, now))
        .expect("commit local tempo");
    assert!(matches!(admission, SyncAdmission::StateChanged { .. }));
    assert_eq!(group.mode(), SyncMode::LocalSync);
    assert_eq!(grid_beat_at(&group, 48_000), 2.0);
    let approach_surplus = (2.0 - 1.5) * SMOOTHING_SECONDS;
    assert!(
        (grid_beat_at(&group, 96_000) - (3.5 + approach_surplus)).abs() < 1e-9,
        "a second after the commit the group has played its target tempo plus \
         the beats the approach carried over, got {}",
        grid_beat_at(&group, 96_000)
    );
}

#[kithara::test]
fn enabling_without_a_parent_withdraws_local_geometry() {
    let mut group = group_in(SyncMode::LocalSync, SyncMemberKind::Grid);
    let _ = group
        .transact(tempo(group.id(), 90.0))
        .expect("local geometry");
    assert_eq!(group.snapshot().state(), BeatGridState::Live);
    let admission = group
        .transact(sync(group.id(), SyncIntent::Enable))
        .expect("enable waiting for parent geometry");
    assert!(matches!(admission, SyncAdmission::StateChanged { .. }));
    assert_eq!(group.mode(), SyncMode::HostSync);
    assert!(matches!(
        group.snapshot().state(),
        BeatGridState::Unavailable(_)
    ));
    group
        .accept_parent(parent_update(
            parent_stamp(parent_id(), 1),
            anchor_at_rate(1.5, 48_000),
        ))
        .expect("parent geometry becomes available");
    assert_eq!(group.snapshot().state(), BeatGridState::Live);
}

#[kithara::test]
fn public_enable_prepares_its_owned_track_without_a_second_command() {
    let (mut group, track, _) = owning_deck_with_parent();
    let deck = group.id();

    let admission = group
        .transact(sync_at(deck, SyncIntent::Enable, SessionFrame::new(2_048)))
        .expect("public Enable is accepted");
    let SyncAdmission::StateChanged { transition, .. } = admission else {
        panic!("Enable must change the group mode: {admission:?}");
    };
    assert_eq!(
        transition.issued().len(),
        1,
        "Enable must issue its own preparation"
    );
    assert_eq!(transition.issued()[0].stamp().member().grid_id(), track);
}

#[kithara::test]
fn audible_public_enable_survives_parent_publication_before_installation() {
    let (mut group, track, parent) = owning_deck_with_parent();
    let deck = group.id();
    let admission = group
        .transact(SyncOperation::Sync {
            target: deck,
            load: LoadGeneration::first(),
            transport: TransportRevision::first(),
            source: AlignmentSource::Audible {
                frontier: PresentationFrontier::builder()
                    .source(5_000)
                    .output(SessionFrame::new(0))
                    .build(),
                speed: 1.0,
            },
            activation: SessionFrame::new(2_048),
            intent: SyncIntent::Enable,
        })
        .expect("public ON while manual PCM plays");
    let SyncAdmission::StateChanged { transition, .. } = admission else {
        panic!("Enable must change the group mode: {admission:?}");
    };
    let [first] = transition.issued() else {
        panic!("one audible deck needs one preparation");
    };
    assert_eq!(first.stamp().member().grid_id(), track);
    assert_eq!(first.activation().1, SessionFrame::new(24_000));

    let same_tempo = group
        .accept_parent(parent_update(
            parent_stamp(parent, 2),
            anchor_at_rate(2.0, 48_000),
        ))
        .expect("a later parent revision preserves an unarmed entry");
    let [carried] = same_tempo.issued() else {
        panic!("same-tempo parent revision replans the audible entry");
    };
    assert_eq!(carried.activation().1, first.activation().1);
    let slower = group
        .accept_parent(parent_update(
            parent_stamp(parent, 3),
            anchor_at_rate(1.0, 48_000),
        ))
        .expect("a slower parent revision replans the audible source");
    let [replanned] = slower.issued() else {
        panic!("the audible entry stays pending on the slower Host grid");
    };
    let SyncEffect::Projection {
        plan, alignment, ..
    } = replanned.effect()
    else {
        panic!("the audible entry remains a projection");
    };
    let live_source = 5_000 + u64::try_from(i64::from(plan.activation().output())).expect("frame");
    assert!(plan.activation().source() >= live_source);
    assert_eq!(f64::from(*alignment.source().value()).fract(), 0.0);
    assert_eq!(f64::from(*alignment.target().value()).fract(), 0.0);
    assert!(
        matches!(group.status(), SyncStatusSnapshot::Prepared { .. }),
        "the accepted audible entry must remain prepared after a parent revision"
    );
}

#[kithara::test]
fn enable_without_track_geometry_rejects_before_changing_mode_and_can_retry() {
    let deck = BeatGridId::allocate().expect("deck id");
    let track = BeatGridId::allocate().expect("track id");
    let published = Arc::new(Mutex::new(BeatGridSnapshot::unavailable(
        track,
        BeatGridRevision::first(),
        MapAxis::Asset(AssetAxis::new(rate(48_000), AssetExtent::Bounded(480_000))),
    )));
    let mut group: Group = GroupState::owning(
        deck,
        rate(48_000),
        SessionEpoch::new(0),
        SyncMember::Grid {
            alignment: None,
            grid: Box::new(PublishedGrid {
                id: track,
                current: Arc::clone(&published),
            }),
        },
    );
    group
        .accept_parent(parent_update(
            parent_stamp(parent_id(), 1),
            anchor_at_rate(2.0, 48_000),
        ))
        .expect("host beat grid");
    let before_operation = group.next_operation;

    let rejected = group
        .transact(sync_at(deck, SyncIntent::Enable, SessionFrame::new(2_048)))
        .expect_err("ON cannot retain a request with missing geometry");
    assert!(matches!(
        rejected.error(),
        SyncError::GridCoverageUnavailable { .. }
    ));
    assert_eq!(group.mode(), SyncMode::Off);
    assert_eq!(group.next_operation, before_operation);

    *published.lock() = asset_grid(track, 480_000, 24_000);
    let admission = group
        .transact(sync_at(deck, SyncIntent::Enable, SessionFrame::new(2_048)))
        .expect("retry after geometry publication");
    assert!(matches!(admission, SyncAdmission::StateChanged { .. }));
}

#[kithara::test]
fn disable_before_enable_claim_restores_the_unmapped_mode() {
    let (mut group, _, _) = owning_deck_with_parent();
    let deck = group.id();
    let enabled = group
        .transact(sync_at(deck, SyncIntent::Enable, SessionFrame::new(2_048)))
        .expect("initial preparation");
    let SyncAdmission::StateChanged { transition, .. } = enabled else {
        panic!("ON changes the timeline: {enabled:?}");
    };
    let [pending] = transition.issued() else {
        panic!("ON issues one entry");
    };
    let pending = pending.stamp();

    let disabled = group
        .transact(sync_at(deck, SyncIntent::Disable, SessionFrame::new(4_096)))
        .expect("OFF before claim cancels ON");
    let SyncAdmission::StateChanged { transition, .. } = disabled else {
        panic!("OFF restores the prior timeline: {disabled:?}");
    };
    assert_eq!(transition.withdrawn(), [pending]);
    assert_eq!(group.mode(), SyncMode::Off);
    assert_eq!(group.tempo(), None);
    assert!(matches!(group.status(), SyncStatusSnapshot::Off { .. }));
}

#[kithara::test]
fn disable_before_enable_claim_restores_the_previous_local_timeline() {
    let mut group = synced_deck_at(108.0);
    let track = BeatGridId::allocate().expect("track id");
    let _ = super::preparation::attach_grid(&mut group, asset_grid(track, 480_000, 24_000));
    let parent = parent_id();
    group
        .accept_parent(parent_update(
            parent_stamp(parent, 1),
            anchor_at_rate(2.0, 48_000),
        ))
        .expect("Host geometry");
    let deck = group.id();
    let before = (group.tempo(), grid_beat_at(&group, 48_000));
    let admission = group
        .transact(sync_at(deck, SyncIntent::Enable, SessionFrame::new(2_048)))
        .expect("ON prepares one Host entry");
    let SyncAdmission::StateChanged { transition, .. } = admission else {
        panic!("ON must prepare the track: {admission:?}");
    };
    let [entry] = transition.issued() else {
        panic!("one Host entry");
    };

    let disabled = group
        .transact(sync_at(deck, SyncIntent::Disable, SessionFrame::new(4_096)))
        .expect("OFF cancels an unclaimed Host entry");
    let SyncAdmission::StateChanged { transition, .. } = disabled else {
        panic!("OFF restores LocalSync: {disabled:?}");
    };
    assert_eq!(transition.withdrawn(), [entry.stamp()]);
    assert!(
        transition.issued().is_empty(),
        "the Host entry must not carry"
    );
    assert_eq!(group.mode(), SyncMode::LocalSync);
    assert_eq!((group.tempo(), grid_beat_at(&group, 48_000)), before);
    assert!(matches!(group.status(), SyncStatusSnapshot::Off { .. }));
}

#[kithara::test]
fn disable_after_pending_host_retarget_latches_the_presented_map() {
    let (mut group, track, parent) = owning_deck_with_parent();
    let old = presented_host_entry(&mut group);
    let SyncEffect::Projection { alignment, .. } = old.effect() else {
        panic!("the presented entry has an alignment");
    };
    assert_ne!(
        alignment.source().value(),
        alignment.target().value(),
        "the source and Host beats must differ so phase cannot come from the projection's source beat"
    );
    let retarget = group
        .accept_parent(parent_update(
            parent_stamp(parent, 2),
            anchor_at_rate(132.0 / 60.0, 48_000),
        ))
        .expect("the newer Host tempo is accepted");
    let [pending] = retarget.issued() else {
        panic!("the 132 BPM replacement remains pending");
    };
    assert_eq!(group.tempo().map(f64::from), Some(132.0));
    assert_eq!(
        group.applied_of(track).map(|lane| lane.map()),
        Some(projected_plan(&old).activation().revision())
    );

    let request = disable_from_presented(&group, &old, 48_000, 96_000);
    let admission = group
        .transact(request)
        .expect("Disable latches the map still sounding");
    let SyncAdmission::StateChanged { transition, .. } = admission else {
        panic!("Disable takes local ownership: {admission:?}");
    };
    assert!(
        transition.withdrawn().is_empty(),
        "the existing operation is carried onto its Local successor"
    );
    let [local] = transition.issued() else {
        panic!("the old sounding map gets one local successor");
    };
    assert_eq!(local.stamp().operation(), pending.stamp().operation());
    assert_ne!(local.stamp(), pending.stamp());
    assert_eq!(group.pending.len(), 1);
    assert_eq!(
        group.pending[0].preparation().map(SyncPreparation::stamp),
        Some(local.stamp()),
        "the old Host preparation is no longer the owner's pending map"
    );
    let cut = SessionFrame::new(96_000);
    assert_eq!(projected_plan(local).activation().output(), cut);
    assert_eq!(
        projected_plan(local).target_tempo_at(cut),
        projected_plan(&old).target_tempo_at(cut),
        "the Local map starts at the tempo that sounded at its physical cut"
    );
    assert_eq!(group.mode(), SyncMode::LocalSync);
    assert_eq!(group.tempo().map(f64::from), Some(120.0));
    assert!((grid_beat_at(&group, 96_000) - 4.0).abs() < 1e-9);
    for frame in [96_000, 120_000] {
        assert_eq!(
            projected_plan(&old).source_at(SessionFrame::new(frame)),
            projected_plan(local).source_at(SessionFrame::new(frame)),
            "the recording stays continuous at and after the latch"
        );
    }
}

#[kithara::test]
fn disable_waits_for_an_armed_retarget_to_present() {
    let (mut group, _, parent) = owning_deck_with_parent();
    let old = presented_host_entry(&mut group);
    let retarget = group
        .accept_parent(parent_update(
            parent_stamp(parent, 2),
            anchor_at_rate(132.0 / 60.0, 48_000),
        ))
        .expect("the newer Host tempo is accepted");
    let [pending] = retarget.issued() else {
        panic!("one replacement is pending");
    };
    let _ = group
        .acknowledge(SyncReceipt::Installed(pending.stamp()))
        .expect("replacement installed");
    let _ = group
        .acknowledge(SyncReceipt::Armed(pending.stamp()))
        .expect("replacement crossed the claim gate");
    let before = group.snapshot().stamp();

    let request = disable_from_presented(&group, &old, 48_000, 96_000);
    let rejected = group
        .transact(request)
        .expect_err("Disable cannot withdraw an armed replacement");
    assert_eq!(
        *rejected.error(),
        SyncError::ArmedOperation {
            member_id: pending.stamp().member().grid_id(),
            operation: pending.stamp().operation(),
        }
    );
    assert_eq!(group.mode(), SyncMode::HostSync);
    assert_eq!(group.snapshot().stamp(), before);
    assert_eq!(group.tempo().map(f64::from), Some(132.0));
}

#[kithara::test]
fn disable_after_pending_retarget_latches_the_presented_ramp_instant() {
    let (mut group, _, parent) = owning_deck_with_parent();
    let _ = presented_host_entry(&mut group);
    let ramp = anchor_at_rate(2.0, 48_000)
        .retarget(SessionFrame::new(0), 3.0, 2.0)
        .expect("two-second approach to 180 BPM");
    let ramp_change = group
        .accept_parent(parent_update(parent_stamp(parent, 2), ramp))
        .expect("the Host ramp is accepted");
    let [old] = ramp_change.issued() else {
        panic!("one ramp replacement is prepared");
    };
    let old = old.clone();
    present_preparation(&mut group, &old);
    let retarget = group
        .accept_parent(parent_update(
            parent_stamp(parent, 3),
            anchor_at_rate(132.0 / 60.0, 48_000),
        ))
        .expect("the 132 BPM target is accepted but not presented");
    assert_eq!(retarget.issued().len(), 1);
    let observed = projected_plan(&old).activation().output();
    let at = SessionFrame::new(i64::from(observed) + 2_048);
    let BeatGridQuery::Resolved(beat) = projected_plan(&old).target_beat_at(at) else {
        panic!("the presented ramp covers the latch beat");
    };
    let expected_beat = f64::from(*beat.value().value());
    let BeatGridQuery::Resolved(tempo) = projected_plan(&old).target_tempo_at(at) else {
        panic!("the presented ramp covers the latch tempo");
    };
    let expected_tempo = f64::from(tempo);
    assert!(expected_tempo > 120.0 && expected_tempo < 180.0);

    let request = disable_from_presented(&group, &old, i64::from(observed), i64::from(at));
    let admission = group
        .transact(request)
        .expect("Disable latches the sounding ramp");
    let SyncAdmission::StateChanged { transition, .. } = admission else {
        panic!("Disable owns the sounding ramp instant: {admission:?}");
    };
    let [local] = transition.issued() else {
        panic!("the ramp gets one local successor");
    };
    assert_eq!(
        projected_plan(local).activation().output(),
        at,
        "latch and Local activation must use the same physical cut"
    );
    assert_eq!(
        projected_plan(&old).source_at(at),
        projected_plan(local).source_at(at),
        "the source is continuous at the mid-ramp latch"
    );
    assert_eq!(
        projected_plan(&old).target_tempo_at(at),
        projected_plan(local).target_tempo_at(at),
        "the applied target tempo at the cut must become the Local tempo"
    );
    assert_eq!(group.mode(), SyncMode::LocalSync);
    assert!((grid_beat_at(&group, i64::from(at)) - expected_beat).abs() < 1e-9);
    for frame in [i64::from(at), i64::from(at) + 48_000] {
        assert!((grid_tempo_at(&group, frame) - expected_tempo).abs() < 1e-9);
    }
}

#[kithara::test]
fn a_parent_shift_beyond_the_first_entry_window_restores_the_sounding_mode() {
    let (mut group, _, parent) = owning_deck_with_parent();
    let deck = group.id();
    let admission = group
        .transact(sync_at(deck, SyncIntent::Enable, SessionFrame::new(2_048)))
        .expect("ON prepares the first entry");
    let SyncAdmission::StateChanged { transition, .. } = admission else {
        panic!("ON changes the timeline: {admission:?}");
    };
    let [entry] = transition.issued() else {
        panic!("one first entry");
    };

    let shifted = group
        .accept_parent(parent_update(
            parent_stamp(parent, 2),
            anchor_at_rate(0.2, 48_000),
        ))
        .expect("later parent moves the boundary beyond its window");
    assert_eq!(shifted.withdrawn(), [entry.stamp()]);
    assert_eq!(group.mode(), SyncMode::Off);
    assert_eq!(group.tempo(), None);
    assert!(matches!(group.status(), SyncStatusSnapshot::Off { .. }));
    assert!(
        group.before_entry.is_none(),
        "terminal withdrawal clears custody"
    );
}

#[kithara::test]
fn a_new_axis_clears_unpresented_entry_custody() {
    let (mut group, _, _) = owning_deck_with_parent();
    let deck = group.id();
    let admission = group
        .transact(sync_at(deck, SyncIntent::Enable, SessionFrame::new(2_048)))
        .expect("ON prepares the first entry");
    let SyncAdmission::StateChanged { transition, .. } = admission else {
        panic!("ON changes the timeline: {admission:?}");
    };
    let [entry] = transition.issued() else {
        panic!("one first entry");
    };
    let axis = SessionAxis::new(rate(44_100), SessionEpoch::new(1));
    let changed = group
        .accept_axis(SessionAxisUpdate::new(axis))
        .expect("a new epoch replaces the old one");
    assert_eq!(changed.withdrawn(), [entry.stamp()]);
    assert!(group.before_entry.is_none());
    assert!(matches!(
        group.transact(sync_at(deck, SyncIntent::Disable, SessionFrame::new(4_096))),
        Ok(SyncAdmission::Deferred { .. })
    ));
    assert_eq!(group.mode(), SyncMode::HostSync);
}

#[kithara::test]
fn topology_replacement_withdraws_an_unclaimed_entry_and_restores_manual_mode() {
    let (mut group, track, _) = owning_deck_with_parent();
    let deck = group.id();
    let admission = group
        .transact(sync_at(deck, SyncIntent::Enable, SessionFrame::new(2_048)))
        .expect("ON prepares the first entry");
    let SyncAdmission::StateChanged { transition, .. } = admission else {
        panic!("ON changes the timeline: {admission:?}");
    };
    let [entry] = transition.issued() else {
        panic!("one first entry");
    };
    let base = group.topology().expect("topology").stamp();
    let changed = group
        .transact(SyncOperation::Topology {
            base,
            operations: Box::new([TopologyOperation::Replace {
                member: track,
                replacement: SyncMember::Grid {
                    alignment: None,
                    grid: Box::new(TestGrid(asset_grid(track, 480_000, 24_000))),
                },
            }]),
        })
        .expect("topology replacement fences the old entry");
    let SyncAdmission::TopologyChanged { transition, .. } = changed else {
        panic!("topology changed: {changed:?}");
    };
    assert_eq!(transition.withdrawn(), [entry.stamp()]);
    assert_eq!(group.mode(), SyncMode::Off);
    assert_eq!(group.tempo(), None);
    assert!(matches!(group.status(), SyncStatusSnapshot::Off { .. }));
    assert!(group.before_entry.is_none());
}

#[kithara::test]
fn align_now_replaces_an_unarmed_public_enable_with_a_fresh_target() {
    let (mut group, _, _) = owning_deck_with_parent();
    let deck = group.id();
    let enabled = group
        .transact(sync_at(deck, SyncIntent::Enable, SessionFrame::new(2_048)))
        .expect("first public ON");
    let SyncAdmission::StateChanged { transition, .. } = enabled else {
        panic!("ON must issue a preparation: {enabled:?}");
    };
    let [first] = transition.issued() else {
        panic!("one initial preparation");
    };
    let first = first.stamp();

    let aligned = group
        .transact(SyncOperation::Sync {
            target: deck,
            load: LoadGeneration::first(),
            transport: TransportRevision::first(),
            source: AlignmentSource::Prepared(AssetFrame::new(24_000.0).expect("cue frame")),
            activation: SessionFrame::new(4_096),
            intent: SyncIntent::AlignNow,
        })
        .expect("AlignNow replaces the pending target");
    let SyncAdmission::StateChanged { transition, .. } = aligned else {
        panic!("same-mode AlignNow must issue a new target: {aligned:?}");
    };
    assert_eq!(transition.withdrawn(), [first]);
    let [latest] = transition.issued() else {
        panic!("one latest preparation");
    };
    assert_ne!(latest.stamp().operation(), first.operation());
    assert_eq!(group.mode(), SyncMode::HostSync);
}

#[kithara::test]
fn rejected_public_enable_restores_the_still_sounding_manual_mode() {
    let (mut group, _, _) = owning_deck_with_parent();
    let deck = group.id();
    let admission = group
        .transact(sync_at(deck, SyncIntent::Enable, SessionFrame::new(2_048)))
        .expect("public ON");
    let SyncAdmission::StateChanged { transition, .. } = admission else {
        panic!("ON must issue a preparation: {admission:?}");
    };
    let [preparation] = transition.issued() else {
        panic!("one preparation");
    };
    let _ = group
        .acknowledge(SyncReceipt::Rejected {
            stamp: preparation.stamp(),
            reason: SyncExecutionReject::Capacity,
        })
        .expect("owner accepts a pre-claim rejection");
    assert_eq!(group.mode(), SyncMode::Off);
    assert_eq!(group.tempo(), None);
    assert!(matches!(group.status(), SyncStatusSnapshot::Off { .. }));
    let retry = group
        .transact(sync_at(deck, SyncIntent::Enable, SessionFrame::new(4_096)))
        .expect("ON can retry from the actual manual mode");
    assert!(matches!(retry, SyncAdmission::StateChanged { .. }));
}

#[kithara::test]
fn local_tempo_on_a_group_without_geometry_starts_at_its_target() {
    let mut group = group_in(SyncMode::LocalSync, SyncMemberKind::Grid);
    let now = SessionFrame::new(48_000);
    let admission = group
        .transact(tempo_at(group.id(), 120.0, now))
        .expect("a group without geometry commits its local tempo");
    assert!(matches!(admission, SyncAdmission::StateChanged { .. }));
    assert_eq!(grid_beat_at(&group, 48_000), 0.0);
    assert_eq!(grid_beat_at(&group, 96_000), 2.0);
}

#[kithara::test]
fn a_new_deck_is_off_so_a_tempo_has_no_owner() {
    let mut group = fixture_group();
    let rejected = group
        .transact(tempo(group.id(), 126.0))
        .expect_err("tempo under off is rejected");
    assert_eq!(*rejected.error(), transport_unavailable());
}

#[kithara::test]
fn enable_enters_host_sync_where_tempo_is_inherited() {
    let mut group = fixture_group();
    let admission = group
        .transact(sync(group.id(), SyncIntent::Enable))
        .expect("enable is admitted");
    assert!(
        matches!(admission, SyncAdmission::StateChanged { .. }),
        "{admission:?}"
    );
    let rejected = group
        .transact(tempo(group.id(), 126.0))
        .expect_err("tempo under host sync is rejected");
    assert_eq!(
        *rejected.error(),
        SyncError::TempoInherited { owner: group.id() }
    );
}

#[kithara::test]
fn disable_from_host_sync_latches_a_local_tempo() {
    let mut group = live_deck();
    group
        .accept_parent(parent_update(
            parent_stamp(parent_id(), 1),
            anchor_at_rate(2.0, 48_000),
        ))
        .expect("the parent segment the deck will follow");
    let _ = group
        .transact(sync(group.id(), SyncIntent::Enable))
        .expect("enable");
    let admission = group
        .transact(sync(group.id(), SyncIntent::Disable))
        .expect("disable seeds the local tempo from the live grid");
    assert!(
        matches!(admission, SyncAdmission::StateChanged { .. }),
        "{admission:?}"
    );
    let admission = group
        .transact(tempo(group.id(), 126.0))
        .expect("a group that owns its tempo accepts a new one");
    assert!(
        matches!(admission, SyncAdmission::StateChanged { .. }),
        "{admission:?}"
    );
}

#[kithara::test]
fn disable_without_any_grid_waits_for_one() {
    let mut group = fixture_group();
    let admission = group
        .transact(sync(group.id(), SyncIntent::Disable))
        .expect("disable is admitted as deferred");
    assert!(
        matches!(admission, SyncAdmission::Deferred { .. }),
        "{admission:?}"
    );
    assert!(matches!(
        group.status(),
        SyncStatusSnapshot::WaitingForGrid { .. }
    ));
    let rejected = group
        .transact(tempo(group.id(), 126.0))
        .expect_err("the mode changes only once a tempo exists");
    assert_eq!(*rejected.error(), transport_unavailable());
}

#[kithara::test]
fn align_now_enters_host_sync() {
    let mut group = synced_deck();

    let admission = group
        .transact(sync(group.id(), SyncIntent::AlignNow))
        .expect("one-shot alignment is admitted");

    assert!(matches!(admission, SyncAdmission::StateChanged { .. }));
    let rejected = group
        .transact(tempo(group.id(), 126.0))
        .expect_err("aligned deck inherits the Host tempo");
    assert_eq!(
        *rejected.error(),
        SyncError::TempoInherited { owner: group.id() }
    );
}

#[kithara::test]
fn a_root_built_local_sync_takes_its_first_tempo_from_the_host() {
    let mut root = group_in(SyncMode::LocalSync, SyncMemberKind::Group);
    let admission = root
        .transact(tempo(root.id(), 128.0))
        .expect("root tempo is admitted without any grid");
    assert!(
        matches!(admission, SyncAdmission::StateChanged { .. }),
        "{admission:?}"
    );
}

#[kithara::test]
fn a_sync_intent_addressed_to_a_track_grid_is_rejected() {
    let mut group = live_deck();
    let track = BeatGridId::allocate().expect("grid id");
    let base = group.topology().expect("topology").stamp();
    let _ = group
        .transact(SyncOperation::Topology {
            base,
            operations: Box::new([TopologyOperation::Attach {
                member: SyncMember::Grid {
                    alignment: None,
                    grid: Box::new(TestGrid(BeatGridSnapshot::unavailable(
                        track,
                        BeatGridRevision::first(),
                        MapAxis::Asset(AssetAxis::new(rate(48_000), AssetExtent::Bounded(480_000))),
                    ))),
                },
            }]),
        })
        .expect("attach");
    let rejected = group
        .transact(sync(track, SyncIntent::Enable))
        .expect_err("a track has no mode");
    assert_eq!(
        *rejected.error(),
        SyncError::CapabilityUnavailable {
            capability: SyncCapability::Alignment,
        }
    );
}

#[kithara::test]
fn a_deck_owning_its_tempo_reports_it() {
    let deck = synced_deck();
    assert_eq!(
        deck.tempo(),
        Some(BeatsPerMinute::try_from(120.0).expect("fixture tempo"))
    );
}

#[kithara::test]
fn a_deck_inheriting_its_tempo_reads_it_from_the_live_session_grid() {
    let mut deck = group_in(SyncMode::HostSync, SyncMemberKind::Grid);
    deck.accept_parent(parent_update(
        parent_stamp(parent_id(), 1),
        anchor_at_rate(2.0, 48_000),
    ))
    .expect("the parent session grid");
    assert_eq!(
        deck.tempo(),
        Some(BeatsPerMinute::try_from(120.0).expect("two beats per second"))
    );
}

#[kithara::test]
fn a_local_tempo_is_approached_from_the_tempo_already_playing() {
    let mut deck = synced_deck();
    let now = SessionFrame::new(48_000);
    let before = grid_beat_at(&deck, 48_000);

    let _ = deck
        .transact(tempo_at(deck.id(), 180.0, now))
        .expect("a local deck accepts a new tempo");

    assert!(
        (grid_beat_at(&deck, 48_000) - before).abs() < 1e-9,
        "the beat at the command frame does not step"
    );
    assert!(
        (grid_tempo_at(&deck, 48_000) - 120.0).abs() < 1e-9,
        "the approach starts at the tempo already playing, got {}",
        grid_tempo_at(&deck, 48_000)
    );
    assert!(
        (grid_tempo_at(&deck, 96_000) - 180.0).abs() < 1e-9,
        "a second later the approach has reached its target, got {}",
        grid_tempo_at(&deck, 96_000)
    );
}

#[kithara::test]
fn a_knob_turned_every_block_lands_every_tempo_it_passes() {
    let mut deck = synced_deck();

    for step in 0..16_i64 {
        let target = if step % 2 == 0 { 124.0 } else { 116.0 };
        let admission = deck
            .transact(tempo_at(deck.id(), target, SessionFrame::new(step * 128)))
            .expect("every knob position is admitted");
        assert!(
            matches!(admission, SyncAdmission::StateChanged { .. }),
            "step {step}: {admission:?}"
        );
        let played = grid_tempo_at(&deck, step * 128);
        // The bounds carry one ulp of slack: the knob range is exact, the
        // tempo reaching it is a float.
        assert!(
            (115.999..=124.001).contains(&played),
            "step {step}: tempo {played} left the knob range"
        );
    }
}

#[kithara::test]
fn a_deck_without_a_grid_has_no_tempo() {
    assert_eq!(fixture_group().tempo(), None);
}

// New S2 coverage below: not ported from #321.

#[kithara::test]
fn disable_mid_ramp_latches_the_tempo_actually_playing() {
    let mut deck = group_in(SyncMode::HostSync, SyncMemberKind::Grid);
    let ramp = anchor_at_rate(2.0, 48_000)
        .retarget(SessionFrame::new(0), 3.0, 1.0)
        .expect("a one-second approach to 180 BPM");
    deck.accept_parent(parent_update(parent_stamp(parent_id(), 1), ramp))
        .expect("the parent ramps");
    let latch = SessionFrame::new(48_000);
    let playing_beat = f64::from(ramp.beat_at(latch).expect("beat at the latch"));
    let playing_tempo = ramp.tempo_at(latch);
    assert!(playing_tempo < 2.9, "the latch lands mid-ramp");

    let _ = deck
        .transact(sync_at(deck.id(), SyncIntent::Disable, latch))
        .expect("disable latches mid-ramp");

    assert_eq!(deck.mode(), SyncMode::LocalSync);
    assert!((grid_beat_at(&deck, 48_000) - playing_beat).abs() < 1e-9);
    for frame in [48_000, 96_000, 480_000] {
        assert!(
            (grid_tempo_at(&deck, frame) - playing_tempo * 60.0).abs() < 1e-9,
            "the latched tempo holds at frame {frame}, not the parent's target"
        );
    }
    let owned = deck.tempo().map(f64::from).expect("a local tempo");
    assert!((owned - playing_tempo * 60.0).abs() < 1e-9);
}

#[kithara::test]
fn parent_tempo_reaches_only_a_host_synced_group() {
    let parent = parent_id();
    let mut off = live_deck();
    let mut local = synced_deck();
    let mut host = group_in(SyncMode::HostSync, SyncMemberKind::Grid);
    let before = (off.snapshot().stamp(), local.snapshot().stamp());

    for group in [&mut off, &mut local, &mut host] {
        group
            .accept_parent(parent_update(
                parent_stamp(parent, 1),
                anchor_at_rate(3.0, 48_000),
            ))
            .expect("every mode accepts the parent segment");
    }

    assert_eq!(
        (off.snapshot().stamp(), local.snapshot().stamp()),
        before,
        "the parent's tempo does not reach Off or LocalSync"
    );
    assert_eq!(local.tempo().map(f64::from), Some(120.0));
    assert_eq!(off.tempo(), None);
    assert!((grid_tempo_at(&host, 0) - 180.0).abs() < 1e-9);
    assert_eq!(
        host.accept_parent(parent_update(
            parent_stamp(parent, 1),
            anchor_at_rate(2.0, 48_000)
        )),
        Err(SyncError::StaleGridRevision {
            current: parent_stamp(parent, 1),
            given: parent_stamp(parent, 1),
        }),
        "a second segment under the same parent revision is stale"
    );
}

#[kithara::test]
fn a_physical_epoch_invalidates_every_mode() {
    let mut off = live_deck();
    let mut local = synced_deck();
    let mut host = group_in(SyncMode::HostSync, SyncMemberKind::Grid);
    host.accept_parent(parent_update(
        parent_stamp(parent_id(), 1),
        anchor_at_rate(2.0, 48_000),
    ))
    .expect("host geometry");
    let next = SessionAxis::new(rate(44_100), SessionEpoch::new(1));
    let skipped = SessionAxisUpdate::new(SessionAxis::new(rate(44_100), SessionEpoch::new(2)));

    for (group, mode) in [
        (&mut off, SyncMode::Off),
        (&mut local, SyncMode::LocalSync),
        (&mut host, SyncMode::HostSync),
    ] {
        let before = group.snapshot().stamp();
        assert!(
            matches!(
                group.accept_axis(skipped),
                Err(SyncError::GridAxisChanged { .. })
            ),
            "{mode:?} refuses an axis that skips an epoch"
        );
        assert_eq!(group.snapshot().stamp(), before);

        group
            .accept_axis(SessionAxisUpdate::new(next))
            .expect("the successor epoch reaches every mode");
        let grid = group.snapshot();
        assert_eq!(grid.axis(), MapAxis::Session(next));
        assert!(matches!(grid.state(), BeatGridState::Unavailable(_)));
        assert_eq!(group.mode(), mode, "the axis keeps the mode");
        assert_eq!(group.tempo(), None, "{mode:?} lost every frame it had");
        group
            .accept_axis(SessionAxisUpdate::new(next))
            .expect("the same axis again is no change");
        assert_eq!(group.snapshot().stamp(), grid.stamp());
    }
}

#[kithara::test]
fn free_withdraws_the_timeline_without_a_new_epoch() {
    let mut deck = synced_deck();
    let axis = deck.snapshot().axis();
    assert_eq!(
        deck.publish_grid(session_grid(
            deck.id(),
            deck.snapshot()
                .revision()
                .checked_next()
                .expect("next revision"),
            SessionEpoch::new(0),
            2.0,
        )),
        Err(SyncError::GridOwnedByMode {
            mode: SyncMode::LocalSync,
        }),
        "a local deck derives its grid; no external owner may publish it"
    );

    let admission = deck
        .transact(sync(deck.id(), SyncIntent::Free))
        .expect("free");

    let SyncAdmission::StateChanged { mode, grid, .. } = admission else {
        panic!("free changes the mode: {admission:?}");
    };
    assert_eq!(mode, SyncMode::Off);
    assert_eq!(grid, deck.snapshot().stamp());
    assert_eq!(deck.snapshot().axis(), axis, "no fictitious epoch");
    assert!(matches!(
        deck.snapshot().state(),
        BeatGridState::Unavailable(_)
    ));
    let rejected = deck
        .transact(tempo(deck.id(), 126.0))
        .expect_err("a free group has no tempo owner");
    assert_eq!(*rejected.error(), transport_unavailable());
}

pub(super) fn attach_group(parent: &mut Group, child: Group) {
    let base = parent.topology().expect("topology").stamp();
    let _ = parent
        .transact(SyncOperation::Topology {
            base,
            operations: Box::new([TopologyOperation::Attach {
                member: SyncMember::Group {
                    alignment: None,
                    group: Box::new(TestGroup(child)),
                },
            }]),
        })
        .expect("a group admits a nested group");
}

pub(super) fn nested<R: 'static>(root: &Group, path: &[BeatGridId], read: fn(&Group) -> R) -> R {
    match path {
        [] => read(root),
        [child, rest @ ..] => root
            .with_group(*child, |group| nested(&group.0, rest, read))
            .expect("the nested group is a direct member"),
    }
}

fn root_segment(root: &Group, revision: u32, beats_per_second: f64) -> ParentGridUpdate {
    parent_update(
        parent_stamp(root.id(), revision),
        anchor_at_rate(beats_per_second, 48_000),
    )
}

#[kithara::test]
fn a_session_publication_reaches_host_synced_descendants_on_every_level() {
    let mut root = group_in(SyncMode::Off, SyncMemberKind::Group);
    let mut middle = group_in(SyncMode::HostSync, SyncMemberKind::Group);
    let leaf = group_in(SyncMode::HostSync, SyncMemberKind::Grid);
    let local = synced_deck();
    let (middle_id, leaf_id, local_id) = (middle.id(), leaf.id(), local.id());
    let local_before = local.snapshot().stamp();
    attach_group(&mut middle, leaf);
    attach_group(&mut root, middle);
    attach_group(&mut root, local);

    root.publish_session(root_segment(&root, 2, 3.0))
        .expect("every descendant accepts the session segment");

    for path in [&[middle_id][..], &[middle_id, leaf_id][..]] {
        assert_eq!(
            nested(&root, path, Group::tempo).map(f64::from),
            Some(180.0),
            "a host-synced group on level {} follows the session",
            path.len()
        );
    }
    assert_eq!(
        nested(&root, &[local_id], |group| (
            group.snapshot().stamp(),
            group.tempo().map(f64::from)
        )),
        (local_before, Some(120.0)),
        "a local sibling keeps its own timeline"
    );
    let middle_stamp = nested(&root, &[middle_id], |group| group.snapshot().stamp());
    let leaf_parent = nested(&root, &[middle_id, leaf_id], |group| {
        group
            .parent
            .and_then(Parent::segment)
            .map(|parent| parent.parent())
    });
    assert_eq!(
        leaf_parent,
        Some(middle_stamp),
        "the leaf follows its direct parent's grid, not the root's"
    );
}

#[kithara::test]
fn a_child_refusing_the_segment_leaves_the_whole_tree_unchanged() {
    let mut root = group_in(SyncMode::Off, SyncMemberKind::Group);
    let accepting = group_in(SyncMode::HostSync, SyncMemberKind::Grid);
    let mut refusing = group_in(SyncMode::HostSync, SyncMemberKind::Grid);
    refusing
        .accept_parent(root_segment(&root, 5, 2.0))
        .expect("a later root segment reached this child first");
    let (accepting_id, refusing_id) = (accepting.id(), refusing.id());
    attach_group(&mut root, accepting);
    attach_group(&mut root, refusing);
    let read = |root: &Group| {
        (
            root.snapshot().stamp(),
            nested(root, &[accepting_id], |group| group.snapshot().stamp()),
            nested(root, &[refusing_id], |group| group.snapshot().stamp()),
        )
    };
    let before = read(&root);

    assert_eq!(
        root.publish_session(root_segment(&root, 2, 3.0)),
        Err(SyncError::StaleGridRevision {
            current: parent_stamp(root.id(), 5),
            given: parent_stamp(root.id(), 2),
        })
    );
    assert_eq!(
        read(&root),
        before,
        "neither the root nor the accepting sibling moved"
    );
}

#[kithara::test]
fn a_new_session_axis_reaches_every_descendant_in_every_mode() {
    let mut root = group_in(SyncMode::Off, SyncMemberKind::Group);
    let mut middle = group_in(SyncMode::HostSync, SyncMemberKind::Group);
    let leaf = synced_deck();
    let (middle_id, leaf_id) = (middle.id(), leaf.id());
    attach_group(&mut middle, leaf);
    attach_group(&mut root, middle);
    root.publish_session(root_segment(&root, 2, 3.0))
        .expect("session segment");
    let next = SessionAxis::new(rate(44_100), SessionEpoch::new(1));

    root.publish_unavailable_grid(parent_stamp(root.id(), 3), next.sample_rate(), next.epoch())
        .expect("the successor epoch reaches every descendant");

    for (path, mode) in [
        (&[middle_id][..], SyncMode::HostSync),
        (&[middle_id, leaf_id][..], SyncMode::LocalSync),
    ] {
        let (axis, state, kept, tempo) = nested(&root, path, |group| {
            let grid = group.snapshot();
            (grid.axis(), grid.state(), group.mode(), group.tempo())
        });
        assert_eq!(axis, MapAxis::Session(next));
        assert!(matches!(state, BeatGridState::Unavailable(_)));
        assert_eq!(kept, mode, "the axis keeps the mode");
        assert_eq!(tempo, None, "{mode:?} lost every frame it had");
    }
}

#[kithara::test]
fn a_group_joins_its_parent_on_the_parent_axis_after_epochs_it_never_saw() {
    let mut root = group_in(SyncMode::Off, SyncMemberKind::Group);
    for (revision, epoch) in [(2, 1), (3, 2)] {
        root.publish_unavailable_grid(
            parent_stamp(root.id(), revision),
            rate(48_000),
            SessionEpoch::new(epoch),
        )
        .expect("a route restart before the child joins");
    }
    let mut middle = group_in(SyncMode::HostSync, SyncMemberKind::Group);
    let leaf = synced_deck();
    let (middle_id, leaf_id) = (middle.id(), leaf.id());
    attach_group(&mut middle, leaf);
    attach_group(&mut root, middle);

    let paths = [&[middle_id][..], &[middle_id, leaf_id][..]];
    for path in paths {
        assert_eq!(
            nested(&root, path, |group| group.snapshot().axis()),
            root.snapshot().axis(),
            "level {} joins on the parent's axis",
            path.len()
        );
    }

    let next = SessionAxis::new(rate(48_000), SessionEpoch::new(3));
    root.publish_unavailable_grid(parent_stamp(root.id(), 4), next.sample_rate(), next.epoch())
        .expect("the next route restart reaches the joined subtree");
    for path in paths {
        assert_eq!(
            nested(&root, path, |group| group.snapshot().axis()),
            MapAxis::Session(next)
        );
    }
}
