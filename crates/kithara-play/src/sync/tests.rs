use std::num::NonZeroU32;

use kithara_test_utils::kithara;
use kithara_warp::{
    AlignmentSource, AssetAxis, AssetFrame, BeatEvidence, BeatGrid, BeatGridId, BeatGridRevision,
    BeatGridSnapshot, BeatGridState, BeatGridUnavailable, BeatMarker, BeatOrdinal, BeatsPerMinute,
    FrameUncertainty, LoadGeneration, MapAxis, MapSegment, PresentationFrontier, ReconcileCause,
    SegmentFacts, SegmentSet, SessionAnchor, SessionAxis, SessionBeat, SessionEpoch, SessionFrame,
    SyncAdmission, SyncApplied, SyncCapability, SyncError, SyncGroup, SyncIntent, SyncMember,
    SyncMemberKind, SyncMode, SyncOperation, SyncStatusSnapshot, TopologyOperation,
    TransportRevision,
};

use super::GroupState;
use crate::player::PlayerMember;

fn session_grid(
    id: BeatGridId,
    revision: BeatGridRevision,
    epoch: SessionEpoch,
    beats_per_second: f64,
) -> BeatGridSnapshot {
    session_grid_at_rate(id, revision, epoch, beats_per_second, 48_000)
}

fn session_grid_at_rate(
    id: BeatGridId,
    revision: BeatGridRevision,
    epoch: SessionEpoch,
    beats_per_second: f64,
    sample_rate: u32,
) -> BeatGridSnapshot {
    let sample_rate =
        NonZeroU32::new(sample_rate).expect("invariant: fixture sample rate is non-zero");
    let anchor = SessionAnchor::new(
        SessionFrame::new(0),
        SessionBeat::new(0.0).expect("invariant: fixture beat is finite"),
        beats_per_second,
        sample_rate,
    )
    .expect("invariant: fixture session anchor is valid");
    BeatGridSnapshot::session(id, revision, epoch, anchor, None)
}

fn fixture_group() -> GroupState<PlayerMember> {
    GroupState::unavailable(
        BeatGridId::allocate().expect("invariant: fixture group id is available"),
        NonZeroU32::new(48_000).expect("invariant: fixture sample rate is non-zero"),
        SessionEpoch::new(0),
        SyncMemberKind::Grid,
        SyncMode::Off,
    )
}

struct TestGrid(BeatGridSnapshot);

impl BeatGrid for TestGrid {
    fn id(&self) -> BeatGridId {
        self.0.id()
    }

    fn snapshot(&self) -> BeatGridSnapshot {
        self.0.clone()
    }
}

fn live_deck() -> GroupState<PlayerMember> {
    GroupState::new(
        session_grid(
            BeatGridId::allocate().expect("grid id"),
            BeatGridRevision::first(),
            SessionEpoch::new(0),
            2.0,
        ),
        SyncMemberKind::Grid,
        SyncMode::Off,
    )
}

fn sync(target: BeatGridId, intent: SyncIntent) -> SyncOperation<PlayerMember> {
    SyncOperation::Sync {
        target,
        load: LoadGeneration::first(),
        transport: TransportRevision::first(),
        source: AlignmentSource::Prepared,
        activation: SessionFrame::new(0),
        intent,
    }
}

fn tempo(target: BeatGridId, value: f64) -> SyncOperation<PlayerMember> {
    SyncOperation::Tempo {
        target,
        tempo: BeatsPerMinute::try_from(value).expect("finite positive bpm"),
    }
}

fn transport_unavailable() -> SyncError {
    SyncError::CapabilityUnavailable {
        capability: SyncCapability::Transport,
    }
}

#[kithara::test]
fn group_rejects_foreign_grid_identity() {
    let mut group = fixture_group();
    let before = group.snapshot();
    let foreign = session_grid(
        BeatGridId::allocate().expect("invariant: foreign grid id is available"),
        before
            .revision()
            .checked_next()
            .expect("invariant: fixture grid revision can advance"),
        SessionEpoch::new(0),
        2.0,
    );

    assert_eq!(
        group.publish_grid(foreign.clone()),
        Err(SyncError::GridIdentityMismatch {
            expected: before.id(),
            given: foreign.id(),
        })
    );
    assert_eq!(group.snapshot(), before);
}

#[kithara::test]
fn group_enforces_grid_successors() {
    let mut group = fixture_group();
    let initial = group.snapshot();
    let published_revision = initial
        .revision()
        .checked_next()
        .expect("invariant: fixture grid revision can advance");
    let published = session_grid(initial.id(), published_revision, SessionEpoch::new(0), 2.0);
    group
        .publish_grid(published.clone())
        .expect("invariant: newer fixture publication is valid");
    let stale = session_grid(initial.id(), initial.revision(), SessionEpoch::new(0), 1.5);

    assert_eq!(
        group.publish_grid(stale.clone()),
        Err(SyncError::StaleGridRevision {
            current: published.stamp(),
            given: stale.stamp(),
        })
    );
    assert_eq!(group.snapshot(), published);

    let withdrawn_revision = published_revision
        .checked_next()
        .expect("invariant: fixture grid revision can advance twice");
    let withdrawn =
        BeatGridSnapshot::unavailable(initial.id(), withdrawn_revision, published.axis());
    assert_eq!(
        group.publish_grid(withdrawn.clone()),
        Err(SyncError::InvalidGroupGridTransition {
            from: BeatGridState::Live,
            to: BeatGridState::Unavailable(BeatGridUnavailable::NoGeometry),
        })
    );
    assert_eq!(group.snapshot(), published);

    let wrong_axis_revision = published_revision
        .checked_next()
        .expect("invariant: fixture grid revision can advance twice");
    let sample_rate = NonZeroU32::new(48_000).expect("invariant: fixture sample rate is non-zero");
    let wrong_axis = BeatGridSnapshot::unavailable(
        initial.id(),
        wrong_axis_revision,
        MapAxis::Asset(AssetAxis::new(sample_rate, 0)),
    );
    assert_eq!(
        group.publish_grid(wrong_axis.clone()),
        Err(SyncError::GridAxisChanged {
            expected: published.axis(),
            given: wrong_axis.axis(),
        })
    );
    assert_eq!(group.snapshot(), published);

    let mut negotiated = fixture_group();
    let initial = negotiated.snapshot();
    let live_revision = initial
        .revision()
        .checked_next()
        .expect("invariant: fixture grid revision can advance");
    let observed = session_grid_at_rate(
        initial.id(),
        live_revision,
        SessionEpoch::new(0),
        2.0,
        44_100,
    );
    negotiated
        .publish_grid(observed.clone())
        .expect("an unavailable axis admits the negotiated live sample rate");
    assert_eq!(negotiated.snapshot(), observed);

    let changed_rate = session_grid_at_rate(
        initial.id(),
        live_revision
            .checked_next()
            .expect("invariant: fixture grid revision can advance twice"),
        SessionEpoch::new(0),
        2.0,
        32_000,
    );
    assert_eq!(
        negotiated.publish_grid(changed_rate.clone()),
        Err(SyncError::GridAxisChanged {
            expected: observed.axis(),
            given: changed_rate.axis(),
        })
    );
    assert_eq!(negotiated.snapshot(), observed);
}

#[kithara::test]
fn group_treats_same_grid_stamp_as_idempotent_publication() {
    let mut group = fixture_group();
    let current = group.snapshot();
    let revision = current
        .revision()
        .checked_next()
        .expect("invariant: fixture grid revision can advance");
    let published = session_grid(current.id(), revision, SessionEpoch::new(0), 2.0);
    group
        .publish_grid(published.clone())
        .expect("invariant: newer fixture publication is valid");

    group
        .publish_grid(published.clone())
        .expect("publishing the same immutable grid revision is idempotent");

    assert_eq!(group.snapshot(), published);
}

#[kithara::test]
fn group_accepts_latest_grid_after_unpublished_revisions() {
    let mut group = fixture_group();
    let current = group.snapshot();
    let skipped = current
        .revision()
        .checked_next()
        .expect("invariant: fixture grid revision can advance");
    let published_revision = skipped
        .checked_next()
        .expect("invariant: fixture grid revision can advance twice");
    let published = session_grid(current.id(), published_revision, SessionEpoch::new(0), 2.0);

    group
        .publish_grid(published.clone())
        .expect("a newer published observation may skip invisible revisions");

    assert_eq!(group.snapshot(), published);
    assert_eq!(group.snapshot().revision(), published_revision);
}

#[kithara::test]
fn group_requires_each_unavailable_route_boundary() {
    let mut group = fixture_group();
    let initial = group.snapshot();
    let live_revision = initial
        .revision()
        .checked_next()
        .expect("invariant: fixture grid revision can advance");
    let live = session_grid(initial.id(), live_revision, SessionEpoch::new(0), 2.0);
    group
        .publish_grid(live.clone())
        .expect("the initial session grid becomes live in its current epoch");

    let boundary_revision = live_revision
        .checked_next()
        .expect("invariant: fixture grid revision can advance twice");
    let sample_rate = NonZeroU32::new(48_000).expect("invariant: fixture sample rate is non-zero");
    let skipped_axis = MapAxis::Session(SessionAxis::new(sample_rate, SessionEpoch::new(2)));
    let skipped = BeatGridSnapshot::unavailable(live.id(), boundary_revision, skipped_axis);
    assert_eq!(
        group.publish_grid(skipped),
        Err(SyncError::GridAxisChanged {
            expected: live.axis(),
            given: skipped_axis,
        })
    );

    let successor_live = session_grid(live.id(), boundary_revision, SessionEpoch::new(1), 2.0);
    assert_eq!(
        group.publish_grid(successor_live.clone()),
        Err(SyncError::GridAxisChanged {
            expected: live.axis(),
            given: successor_live.axis(),
        })
    );

    let boundary_axis = MapAxis::Session(SessionAxis::new(sample_rate, SessionEpoch::new(1)));
    let boundary = BeatGridSnapshot::unavailable(live.id(), boundary_revision, boundary_axis);
    group
        .publish_grid(boundary.clone())
        .expect("the exact successor epoch is admitted through an unavailable boundary");

    let next_live = session_grid_at_rate(
        live.id(),
        boundary_revision
            .checked_next()
            .expect("invariant: fixture grid revision can advance three times"),
        SessionEpoch::new(1),
        2.0,
        44_100,
    );
    group
        .publish_grid(next_live.clone())
        .expect("the unavailable boundary admits the negotiated live axis");
    assert_eq!(group.snapshot(), next_live);
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
fn free_leaves_the_beat_timeline() {
    let mut group = live_deck();
    let _ = group
        .transact(sync(group.id(), SyncIntent::Disable))
        .expect("disable seeds local sync");
    let admission = group
        .transact(sync(group.id(), SyncIntent::Free))
        .expect("free");
    assert!(
        matches!(admission, SyncAdmission::StateChanged { .. }),
        "{admission:?}"
    );
    let rejected = group
        .transact(tempo(group.id(), 126.0))
        .expect_err("a free group has no tempo owner");
    assert_eq!(*rejected.error(), transport_unavailable());
}

#[kithara::test]
fn a_root_built_local_sync_takes_its_first_tempo_from_the_host() {
    let mut root = GroupState::<PlayerMember>::unavailable(
        BeatGridId::allocate().expect("grid id"),
        NonZeroU32::new(48_000).expect("sample rate"),
        SessionEpoch::new(0),
        SyncMemberKind::Group,
        SyncMode::LocalSync,
    );
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
                        MapAxis::Asset(AssetAxis::new(
                            NonZeroU32::new(48_000).expect("sample rate"),
                            480_000,
                        )),
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

fn asset_grid(id: BeatGridId, frames: u64, beat_frames: u64) -> BeatGridSnapshot {
    let sample_rate = NonZeroU32::new(48_000).expect("sample rate");
    let exact = FrameUncertainty::new(0.0).expect("zero uncertainty is finite");
    let marker = |ordinal: u64, frame: u64| {
        BeatMarker::new(
            AssetFrame::new(frame as f64)
                .expect("fixture frame is finite")
                .into(),
            Some(BeatOrdinal::new(ordinal as i64)),
            BeatEvidence::Observed,
            exact,
        )
    };
    let segments = (0..frames / beat_frames)
        .map(|beat| {
            MapSegment::new(
                marker(beat, beat * beat_frames),
                marker(beat + 1, (beat + 1) * beat_frames),
                SegmentFacts::new(BeatEvidence::Observed, exact, None),
            )
            .expect("fixture segment advances on both axes")
        })
        .collect();
    BeatGridSnapshot::segments(
        id,
        BeatGridRevision::first(),
        BeatGridState::Complete,
        SegmentSet::new(
            MapAxis::Asset(AssetAxis::new(sample_rate, frames)),
            segments,
        )
        .expect("fixture segment set is contiguous"),
    )
    .expect("fixture asset grid is valid")
}

fn synced_deck() -> GroupState<PlayerMember> {
    let mut deck = GroupState::new(
        session_grid(
            BeatGridId::allocate().expect("grid id"),
            BeatGridRevision::first(),
            SessionEpoch::new(0),
            2.0,
        ),
        SyncMemberKind::Grid,
        SyncMode::LocalSync,
    );
    let _ = deck
        .transact(tempo(deck.id(), 120.0))
        .expect("a local deck accepts its own tempo");
    deck
}

fn attach_grid(group: &mut GroupState<PlayerMember>, grid: BeatGridSnapshot) {
    let base = group.topology().expect("topology").stamp();
    let _ = group
        .transact(SyncOperation::Topology {
            base,
            operations: Box::new([TopologyOperation::Attach {
                member: SyncMember::Grid {
                    alignment: None,
                    grid: Box::new(TestGrid(grid)),
                },
            }]),
        })
        .expect("attach");
}

fn frontier_at_zero() -> PresentationFrontier {
    PresentationFrontier::builder()
        .source(0)
        .output(SessionFrame::new(0))
        .build()
}

fn reconcile(
    group: &mut GroupState<PlayerMember>,
    target: BeatGridId,
    cause: ReconcileCause,
) -> SyncAdmission {
    let (load, transport) = group.generations();
    group
        .transact(SyncOperation::Reconcile {
            target,
            load,
            transport,
            cause,
            frontier: frontier_at_zero(),
        })
        .expect("reconcile is admitted")
}

#[kithara::test]
fn a_track_without_geometry_waits_for_its_grid() {
    let mut group = synced_deck();
    let track = BeatGridId::allocate().expect("grid id");
    attach_grid(
        &mut group,
        BeatGridSnapshot::unavailable(
            track,
            BeatGridRevision::first(),
            MapAxis::Asset(AssetAxis::new(
                NonZeroU32::new(48_000).expect("sample rate"),
                480_000,
            )),
        ),
    );

    let admission = reconcile(&mut group, track, ReconcileCause::GridAvailable);

    assert!(
        matches!(admission, SyncAdmission::Deferred { .. }),
        "{admission:?}"
    );
    assert!(
        matches!(group.status(), SyncStatusSnapshot::WaitingForGrid { .. }),
        "{:?}",
        group.status()
    );
}

#[kithara::test]
fn a_complete_grid_is_prepared_on_the_next_deck_beat_and_locks_on_acknowledge() {
    let mut group = synced_deck();
    let track = BeatGridId::allocate().expect("grid id");
    attach_grid(&mut group, asset_grid(track, 480_000, 24_000));

    let SyncAdmission::Prepared {
        operation,
        warp_map,
        activation,
        ..
    } = reconcile(&mut group, track, ReconcileCause::GridAvailable)
    else {
        panic!("complete grid must be prepared");
    };

    assert_eq!(
        activation,
        SessionFrame::new(0),
        "a frontier on beat 0 activates on beat 0"
    );
    assert!(
        matches!(group.status(), SyncStatusSnapshot::Prepared { .. }),
        "{:?}",
        group.status()
    );
    let (load, transport) = group.generations();
    let applied = || {
        SyncApplied::builder()
            .group(group.snapshot().stamp())
            .load(load)
            .frontier(frontier_at_zero())
            .operation(operation)
            .topology(group.topology().expect("topology").stamp())
            .transport(transport)
            .warp_map(warp_map)
            .build()
    };
    let first = applied();
    let again = applied();

    let status = group.acknowledge(first).expect("acknowledged");

    assert!(
        matches!(status, SyncStatusSnapshot::Locked { .. }),
        "{status:?}"
    );
    assert_eq!(
        group.acknowledge(again).expect_err("second acknowledge"),
        SyncError::DuplicateAcknowledgement { operation }
    );
}

#[kithara::test]
fn a_deck_owning_its_tempo_reports_it() {
    let deck = synced_deck();
    assert_eq!(
        deck.deck_tempo(),
        Some(BeatsPerMinute::try_from(120.0).expect("fixture tempo"))
    );
}

#[kithara::test]
fn a_deck_inheriting_its_tempo_reads_it_from_the_live_session_grid() {
    let deck = live_deck();
    assert_eq!(
        deck.deck_tempo(),
        Some(BeatsPerMinute::try_from(120.0).expect("two beats per second"))
    );
}

#[kithara::test]
fn a_deck_without_a_grid_has_no_tempo() {
    assert_eq!(fixture_group().deck_tempo(), None);
}
