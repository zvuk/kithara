use std::num::NonZeroU32;

use kithara_test_utils::kithara;
use kithara_warp::{
    AlignmentSource, AssetAxis, AssetFrame, Beat, BeatEvidence, BeatGrid, BeatGridId,
    BeatGridQuery, BeatGridRevision, BeatGridSnapshot, BeatGridState, BeatGridUnavailable,
    BeatMarker, BeatOrdinal, BeatsPerMinute, FrameUncertainty, LoadGeneration, MapAxis, MapPoint,
    MapPosition, MapSegment, MemberArm, Meter, MeterFacts, PresentationFrontier, RateTarget,
    ReconcileCause, SegmentFacts, SegmentSet, SessionAnchor, SessionAxis, SessionBeat,
    SessionEpoch, SessionFrame, SyncAdmission, SyncApplied, SyncCapability, SyncError, SyncGroup,
    SyncIntent, SyncMember, SyncMemberKind, SyncMode, SyncOperation, SyncStatusSnapshot,
    TopologyOperation, TransportRevision,
};

use super::{GroupState, host_seek, prepare::EntryWindow};
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
        SessionAxis::new(sample_rate, SessionEpoch::new(0)),
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
    delegate::delegate! {
        to self.0 {
            fn id(&self) -> BeatGridId;
            #[call(clone)]
            fn snapshot(&self) -> BeatGridSnapshot;
        }
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
        source: AlignmentSource::Prepared(frontier_at_zero()),
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
    let _ = group
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
    let track = BeatGridId::allocate().expect("grid id");
    attach_grid(&mut group, asset_grid(track, 480_000, 24_000));
    let _ = reconcile_at(
        &mut group,
        track,
        ReconcileCause::GridAvailable,
        PresentationFrontier::builder()
            .source(24_000)
            .output(SessionFrame::new(24_000))
            .warp_map(kithara_warp::WarpMapRevision::first())
            .build(),
    );
    let previous = group
        .prepared()
        .latest()
        .expect("reconcile leaves its map prepared");
    let previous_identity = (previous.operation, previous.warp_map);
    let admission = group
        .transact(SyncOperation::Sync {
            target: group.id(),
            load: LoadGeneration::first(),
            transport: TransportRevision::first(),
            source: AlignmentSource::Audible {
                presentation: PresentationFrontier::builder()
                    .source(48_000)
                    .output(SessionFrame::new(48_000))
                    .warp_map(kithara_warp::WarpMapRevision::first())
                    .build(),
                preparation_source: 48_448,
                playback_rate: RateTarget::default(),
            },
            activation: SessionFrame::new(0),
            intent: SyncIntent::Free,
        })
        .expect("free");
    let SyncAdmission::Preparing {
        operation,
        warp_map,
        ..
    } = admission
    else {
        panic!("Free must reserve identity before worker adoption: {admission:?}");
    };
    assert_ne!((operation, warp_map), previous_identity);
    assert!(group.prepared().is_empty());
    assert!(matches!(
        group.status(),
        SyncStatusSnapshot::Preparing {
            operation: current_operation,
            warp_map: current_map,
            ..
        } if (current_operation, current_map) == (operation, warp_map)
    ));
    let (load, transport) = group.generations();
    let preparing = group.preparing().cloned().expect("free preparation");
    assert!(
        group.adopt_free(kithara_sync::SyncExecutionReceipt::Installed {
            stamp: preparing.stamp,
            alignment: preparing.alignment,
        })
    );
    let prepared = group
        .prepared()
        .latest()
        .expect("worker-adopted Free handoff");
    assert_eq!(prepared.source, 48_448);
    assert_eq!(prepared.activation, SessionFrame::new(48_448));
    let applied = SyncApplied::builder()
        .group(group.snapshot().stamp())
        .load(load)
        .frontier(
            PresentationFrontier::builder()
                .source(0)
                .output(SessionFrame::new(0))
                .warp_map(warp_map)
                .build(),
        )
        .operation(operation)
        .topology(group.topology().expect("topology").stamp())
        .transport(transport)
        .warp_map(warp_map)
        .build();
    assert!(matches!(
        group.acknowledge(applied).expect("Free applies"),
        SyncStatusSnapshot::Off { .. }
    ));
    let rejected = group
        .transact(tempo(group.id(), 126.0))
        .expect_err("a free group has no tempo owner");
    assert_eq!(*rejected.error(), transport_unavailable());
}

#[kithara::test]
fn rejected_free_geometry_receipt_clears_the_exact_preparing_state() {
    let mut group = live_deck();
    let _ = group
        .transact(sync(group.id(), SyncIntent::Disable))
        .expect("disable seeds local sync");
    let track = BeatGridId::allocate().expect("grid id");
    attach_grid(&mut group, asset_grid(track, 480_000, 24_000));
    let _ = reconcile_at(
        &mut group,
        track,
        ReconcileCause::GridAvailable,
        PresentationFrontier::builder()
            .source(24_000)
            .output(SessionFrame::new(24_000))
            .warp_map(kithara_warp::WarpMapRevision::first())
            .build(),
    );
    let SyncAdmission::Preparing { .. } = group
        .transact(SyncOperation::Sync {
            target: group.id(),
            load: LoadGeneration::first(),
            transport: TransportRevision::first(),
            source: AlignmentSource::Audible {
                presentation: PresentationFrontier::builder()
                    .source(48_000)
                    .output(SessionFrame::new(48_000))
                    .warp_map(kithara_warp::WarpMapRevision::first())
                    .build(),
                preparation_source: 48_448,
                playback_rate: RateTarget::default(),
            },
            activation: SessionFrame::new(0),
            intent: SyncIntent::Free,
        })
        .expect("free")
    else {
        panic!("Free must prepare")
    };
    let (_load, _transport) = group.generations();
    let stamp = group.preparing().expect("free preparation").stamp;

    assert!(
        group.adopt_free(kithara_sync::SyncExecutionReceipt::Rejected {
            stamp,
            reason: kithara_sync::SyncExecutionReject::Geometry,
        })
    );
    assert!(group.preparing().is_none());
    assert!(group.prepared().is_empty());
}

#[kithara::test]
fn a_route_boundary_drops_the_free_handoff_planned_on_the_previous_axis() {
    let anchor = |rate, epoch| {
        SessionAnchor::new(
            SessionFrame::new(0),
            SessionBeat::new(0.0).expect("beat"),
            2.0,
            SessionAxis::new(
                NonZeroU32::new(rate).expect("sample rate"),
                SessionEpoch::new(epoch),
            ),
        )
        .expect("session anchor")
    };
    let mut group = GroupState::<PlayerMember>::unavailable(
        BeatGridId::allocate().expect("grid id"),
        NonZeroU32::new(44_100).expect("sample rate"),
        SessionEpoch::new(0),
        SyncMemberKind::Grid,
        SyncMode::HostSync,
    );
    group
        .publish_session_anchor(anchor(44_100, 0))
        .expect("the first anchor makes the deck grid live");
    let track = BeatGridId::allocate().expect("grid id");
    attach_grid(&mut group, asset_grid(track, 480_000, 24_000));
    let _ = reconcile(&mut group, track, ReconcileCause::GridAvailable);
    let admission = group
        .transact(SyncOperation::Sync {
            target: group.id(),
            load: LoadGeneration::first(),
            transport: TransportRevision::first(),
            source: AlignmentSource::Audible {
                presentation: PresentationFrontier::builder()
                    .source(48_000)
                    .output(SessionFrame::new(48_000))
                    .warp_map(kithara_warp::WarpMapRevision::first())
                    .build(),
                preparation_source: 48_448,
                playback_rate: RateTarget::default(),
            },
            activation: SessionFrame::new(0),
            intent: SyncIntent::Free,
        })
        .expect("free");
    assert!(matches!(admission, SyncAdmission::Preparing { .. }));

    group
        .publish_session_anchor(anchor(48_000, 1))
        .expect("the successor epoch steps the deck through an unavailable grid");

    assert!(group.preparing().is_none());
}

#[kithara::test]
fn installed_free_receipt_is_consumed_once() {
    let mut group = live_deck();
    let _ = group
        .transact(sync(group.id(), SyncIntent::Disable))
        .expect("disable seeds local sync");
    let track = BeatGridId::allocate().expect("grid id");
    attach_grid(&mut group, asset_grid(track, 480_000, 24_000));
    let _ = reconcile_at(
        &mut group,
        track,
        ReconcileCause::GridAvailable,
        PresentationFrontier::builder()
            .source(24_000)
            .output(SessionFrame::new(24_000))
            .warp_map(kithara_warp::WarpMapRevision::first())
            .build(),
    );
    let SyncAdmission::Preparing { .. } = group
        .transact(SyncOperation::Sync {
            target: group.id(),
            load: LoadGeneration::first(),
            transport: TransportRevision::first(),
            source: AlignmentSource::Audible {
                presentation: PresentationFrontier::builder()
                    .source(48_000)
                    .output(SessionFrame::new(48_000))
                    .warp_map(kithara_warp::WarpMapRevision::first())
                    .build(),
                preparation_source: 48_448,
                playback_rate: RateTarget::default(),
            },
            activation: SessionFrame::new(0),
            intent: SyncIntent::Free,
        })
        .expect("free")
    else {
        panic!("Free must prepare")
    };
    let preparing = group.preparing().cloned().expect("free preparation");
    let mut target_mismatch = preparing.stamp;
    target_mismatch.target = BeatGridId::allocate().expect("mismatched target");
    let mut grid_mismatch = preparing.stamp;
    grid_mismatch.target_grid = asset_grid(
        BeatGridId::allocate().expect("mismatched grid"),
        480_000,
        24_000,
    )
    .stamp();
    let mut topology_mismatch = preparing.stamp;
    topology_mismatch.topology = kithara_warp::TopologyStamp::new(
        BeatGridId::allocate().expect("mismatched topology"),
        kithara_warp::TopologyRevision::first(),
    );
    let mut axis_mismatch = preparing.stamp;
    axis_mismatch.target_axis = MapAxis::Asset(AssetAxis::new(
        NonZeroU32::new(44_100).expect("sample rate"),
        480_000,
    ));
    let mut successor_mismatch = preparing.stamp;
    successor_mismatch.successor = successor_mismatch
        .successor
        .checked_next()
        .expect("mismatched map revision");
    for stamp in [
        target_mismatch,
        grid_mismatch,
        topology_mismatch,
        axis_mismatch,
        successor_mismatch,
    ] {
        assert!(
            !group.adopt_free(kithara_sync::SyncExecutionReceipt::Rejected {
                stamp,
                reason: kithara_sync::SyncExecutionReject::Superseded,
            })
        );
        assert_eq!(
            group.preparing().map(|value| value.stamp),
            Some(preparing.stamp)
        );
    }
    let stale_receipt = kithara_sync::SyncExecutionReceipt::Installed {
        stamp: preparing.stamp,
        alignment: preparing.alignment,
    };
    let SyncAdmission::Preparing { .. } = group
        .transact(SyncOperation::Sync {
            target: group.id(),
            load: LoadGeneration::first(),
            transport: TransportRevision::first(),
            source: AlignmentSource::Audible {
                presentation: PresentationFrontier::builder()
                    .source(48_000)
                    .output(SessionFrame::new(48_000))
                    .warp_map(kithara_warp::WarpMapRevision::first())
                    .build(),
                preparation_source: 48_448,
                playback_rate: RateTarget::default(),
            },
            activation: SessionFrame::new(0),
            intent: SyncIntent::Free,
        })
        .expect("replacement free")
    else {
        panic!("replacement Free must prepare")
    };
    let preparing = group.preparing().cloned().expect("replacement preparation");
    assert_ne!(preparing.stamp, stale_receipt.stamp());
    assert_eq!(
        preparing.stamp.predecessor,
        stale_receipt.stamp().predecessor
    );
    assert!(!group.adopt_free(stale_receipt));
    assert_eq!(
        group.preparing().map(|value| value.stamp),
        Some(preparing.stamp)
    );
    let receipt = kithara_sync::SyncExecutionReceipt::Installed {
        stamp: preparing.stamp,
        alignment: preparing.alignment,
    };

    assert!(group.adopt_free(receipt));
    assert!(matches!(
        group.status(),
        SyncStatusSnapshot::Prepared { .. }
    ));
    assert!(!group.adopt_free(receipt));
}

#[kithara::test]
fn worker_handoff_geometry_derives_source_output_and_beat_together() {
    let owner = live_deck().snapshot();
    let member = asset_grid(BeatGridId::allocate().expect("grid id"), 480_000, 24_000);
    let alignment = |source, output, preparation_source| {
        crate::sync::prepare::handoff_member(
            &owner,
            &member,
            None,
            AlignmentSource::Audible {
                presentation: PresentationFrontier::builder()
                    .source(source)
                    .output(SessionFrame::new(output))
                    .build(),
                preparation_source,
                playback_rate: RateTarget::default(),
            },
        )
        .expect("fixture geometry covers worker boundary")
    };
    let early = alignment(48_000, 48_000, 48_448);
    let late = alignment(96_000, 96_000, 96_448);

    assert_ne!(
        (early.source, early.activation, early.activation_beat),
        (late.source, late.activation, late.activation_beat),
        "a later worker boundary must not reuse a scalar activation beat"
    );
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
                    arm: MemberArm::Waiting,
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
    asset_grid_with_meter(id, frames, beat_frames, None)
}

fn asset_segments(frames: u64, beat_frames: u64, meter: Option<MeterFacts>) -> SegmentSet {
    asset_segments_from(frames, beat_frames, 0, meter)
}

fn asset_segments_from(
    frames: u64,
    beat_frames: u64,
    first_beat_frame: u64,
    meter: Option<MeterFacts>,
) -> SegmentSet {
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
    let segments = (0..(frames - first_beat_frame) / beat_frames)
        .map(|beat| {
            MapSegment::new(
                marker(beat, first_beat_frame + beat * beat_frames),
                marker(beat + 1, first_beat_frame + (beat + 1) * beat_frames),
                SegmentFacts::new(BeatEvidence::Observed, exact, meter),
            )
            .expect("fixture segment advances on both axes")
        })
        .collect();
    SegmentSet::new(
        MapAxis::Asset(AssetAxis::new(sample_rate, frames)),
        segments,
    )
    .expect("fixture segment set is contiguous")
}

fn asset_grid_with_meter(
    id: BeatGridId,
    frames: u64,
    beat_frames: u64,
    meter: Option<MeterFacts>,
) -> BeatGridSnapshot {
    BeatGridSnapshot::segments(
        id,
        BeatGridRevision::first(),
        BeatGridState::Complete,
        asset_segments(frames, beat_frames, meter),
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

#[kithara::test]
fn an_attached_member_waits_until_the_group_arms_it() {
    let mut group = fixture_group();
    let track = BeatGridId::allocate().expect("invariant: track grid id is available");
    attach_waiting_grid(&mut group, asset_grid(track, 96_000, 24_000));
    assert_eq!(member_arm(&group, track), MemberArm::Waiting);

    transact_topology(&mut group, TopologyOperation::Arm { member: track });
    assert_eq!(member_arm(&group, track), MemberArm::Armed);

    transact_topology(&mut group, TopologyOperation::Disarm { member: track });
    assert_eq!(member_arm(&group, track), MemberArm::Waiting);
}

#[kithara::test]
fn an_armed_member_keeps_its_arm_across_a_grid_replacement() {
    let mut group = fixture_group();
    let track = BeatGridId::allocate().expect("invariant: track grid id is available");
    attach_grid(&mut group, asset_grid(track, 96_000, 24_000));
    transact_topology(&mut group, TopologyOperation::Arm { member: track });

    let refined = BeatGridSnapshot::segments(
        track,
        BeatGridRevision::first()
            .checked_next()
            .expect("invariant: fixture grid revision can advance"),
        BeatGridState::Complete,
        asset_segments(96_000, 24_000, None),
    )
    .expect("fixture asset grid is valid");
    transact_topology(
        &mut group,
        TopologyOperation::Replace {
            member: track,
            replacement: SyncMember::Grid {
                alignment: None,
                arm: MemberArm::Waiting,
                grid: Box::new(TestGrid(refined)),
            },
        },
    );

    assert_eq!(member_arm(&group, track), MemberArm::Armed);
}

#[kithara::test]
fn arming_an_absent_member_is_rejected() {
    let mut group = fixture_group();
    let absent = BeatGridId::allocate().expect("invariant: absent grid id is available");
    let base = group.topology().expect("topology").stamp();

    let rejected = group
        .transact(SyncOperation::Topology {
            base,
            operations: Box::new([TopologyOperation::Arm { member: absent }]),
        })
        .expect_err("a group cannot arm a member it does not hold");

    let (error, _) = rejected.into();
    assert_eq!(
        error,
        SyncError::MemberNotFound {
            group_id: group.id(),
            member_id: absent,
        }
    );
}

fn transact_topology(
    group: &mut GroupState<PlayerMember>,
    operation: TopologyOperation<PlayerMember>,
) {
    let base = group.topology().expect("topology").stamp();
    let admission = group
        .transact(SyncOperation::Topology {
            base,
            operations: Box::new([operation]),
        })
        .expect("the group admits its own topology change");
    assert!(
        matches!(admission, SyncAdmission::TopologyChanged { .. }),
        "topology change is applied, got {admission:?}"
    );
}

fn member_arm(group: &GroupState<PlayerMember>, target: BeatGridId) -> MemberArm {
    group
        .topology()
        .expect("topology")
        .members()
        .iter()
        .find(|member| member.grid().id() == target)
        .expect("attached member is present")
        .arm()
}

/// Attaches a grid as the member the deck plays, which the deck arms at once.
fn attach_grid(group: &mut GroupState<PlayerMember>, grid: BeatGridSnapshot) {
    let member = grid.id();
    attach_waiting_grid(group, grid);
    group.arm_deck_track(member);
}

/// Attaches a grid the group has not armed, as a queued track enters waiting.
fn attach_waiting_grid(group: &mut GroupState<PlayerMember>, grid: BeatGridSnapshot) {
    let base = group.topology().expect("topology").stamp();
    let _ = group
        .transact(SyncOperation::Topology {
            base,
            operations: Box::new([TopologyOperation::Attach {
                member: SyncMember::Grid {
                    alignment: None,
                    arm: MemberArm::Waiting,
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
    reconcile_at(group, target, cause, frontier_at_zero())
}

fn reconcile_at(
    group: &mut GroupState<PlayerMember>,
    target: BeatGridId,
    cause: ReconcileCause,
    frontier: PresentationFrontier,
) -> SyncAdmission {
    let (load, transport) = group.generations();
    group
        .transact(SyncOperation::Reconcile {
            target,
            load,
            transport,
            cause,
            source: AlignmentSource::Prepared(frontier),
            source_cue: None,
        })
        .expect("reconcile is admitted")
}

#[kithara::test]
fn preparation_carries_the_next_source_beat_to_the_next_deck_beat() {
    let mut group = synced_deck();
    let track = BeatGridId::allocate().expect("grid id");
    attach_grid(&mut group, asset_grid(track, 480_000, 24_000));
    let frontier = PresentationFrontier::builder()
        .source(10_000)
        .output(SessionFrame::new(5_000))
        .build();

    let admission = reconcile_at(&mut group, track, ReconcileCause::GridAvailable, frontier);
    let prepared = group.prepared().latest().expect("prepared relation");

    assert!(matches!(admission, SyncAdmission::Prepared { .. }));
    assert_eq!(prepared.source, 24_000);
    assert_eq!(prepared.activation, SessionFrame::new(24_000));
}

/// A four-four track grid, so bar phase is observable in the entry window.
fn four_four_grid(id: BeatGridId, frames: u64, beat_frames: u64) -> BeatGridSnapshot {
    let exact = FrameUncertainty::new(0.0).expect("zero uncertainty is finite");
    let meter = Meter::new(4).expect("fixture meter is valid");
    asset_grid_with_meter(
        id,
        frames,
        beat_frames,
        Some(MeterFacts::new(meter, BeatEvidence::Observed, exact)),
    )
}

#[kithara::test]
fn a_waiting_member_enters_on_the_last_owner_downbeat_the_deadline_allows() {
    let mut group = synced_deck();
    let track = BeatGridId::allocate().expect("grid id");
    attach_waiting_grid(&mut group, four_four_grid(track, 480_000, 24_000));

    let admission = group
        .prepare_entry(
            track,
            EntryWindow {
                earliest: SessionFrame::new(0),
                deadline: SessionFrame::new(250_000),
            },
            None,
        )
        .expect("a waiting member enters inside its window");
    let prepared = group.prepared().get(track).expect("entry map");

    assert!(matches!(admission, SyncAdmission::Prepared { .. }));
    assert_eq!(prepared.activation, SessionFrame::new(192_000));
    assert_eq!(prepared.source, 0);
}

#[kithara::test]
fn a_window_too_short_for_a_downbeat_enters_on_the_first_one_after_it() {
    let mut group = synced_deck();
    let track = BeatGridId::allocate().expect("grid id");
    attach_waiting_grid(&mut group, four_four_grid(track, 480_000, 24_000));

    let _ = group
        .prepare_entry(
            track,
            EntryWindow {
                earliest: SessionFrame::new(194_000),
                deadline: SessionFrame::new(200_000),
            },
            None,
        )
        .expect("a short window still enters on a downbeat");
    let prepared = group.prepared().get(track).expect("entry map");

    assert_eq!(prepared.activation, SessionFrame::new(288_000));
}

#[kithara::test]
fn a_waiting_member_has_nothing_to_reconcile() {
    let mut group = synced_deck();
    let track = BeatGridId::allocate().expect("grid id");
    attach_waiting_grid(&mut group, four_four_grid(track, 480_000, 24_000));

    let (load, transport) = group.generations();
    let refusal = group
        .transact(SyncOperation::Reconcile {
            target: track,
            load,
            transport,
            cause: ReconcileCause::GridAvailable,
            source: AlignmentSource::Prepared(frontier_at_zero()),
            source_cue: None,
        })
        .expect_err("a waiting member is not reconciled");

    let (error, _): (SyncError, SyncOperation<PlayerMember>) = refusal.into();
    assert!(
        matches!(error, SyncError::MemberNotArmed { member_id, .. } if member_id == track),
        "got {error:?}"
    );
}

#[kithara::test]
fn the_track_the_deck_plays_without_an_entry_is_armed_at_once() {
    let mut group = synced_deck();
    let track = BeatGridId::allocate().expect("grid id");
    attach_waiting_grid(&mut group, four_four_grid(track, 480_000, 24_000));

    group.arm_deck_track(track);

    assert_eq!(member_arm(&group, track), MemberArm::Armed);
    let _ = reconcile(&mut group, track, ReconcileCause::GridAvailable);
}

#[kithara::test]
fn a_track_the_handover_makes_current_keeps_its_prepared_entry() {
    let mut group = synced_deck();
    let track = BeatGridId::allocate().expect("grid id");
    attach_waiting_grid(&mut group, four_four_grid(track, 480_000, 24_000));
    let _ = group
        .prepare_entry(
            track,
            EntryWindow {
                earliest: SessionFrame::new(0),
                deadline: SessionFrame::new(250_000),
            },
            None,
        )
        .expect("a waiting member enters inside its window");
    let entry = group.prepared().get(track);

    group.arm_deck_track(track);

    assert_eq!(group.prepared().get(track), entry);
    assert_eq!(member_arm(&group, track), MemberArm::Waiting);
}

#[kithara::test]
fn an_entry_without_reachable_geometry_spends_no_operation_identity() {
    let mut group = synced_deck();
    let track = BeatGridId::allocate().expect("grid id");
    let partial = BeatGridSnapshot::segments(
        track,
        BeatGridRevision::first(),
        BeatGridState::Building,
        asset_segments(480_000, 24_000, None),
    )
    .expect("a building grid is valid");
    attach_waiting_grid(&mut group, partial);
    let before = group.status();

    let refusal = group
        .prepare_entry(
            track,
            EntryWindow {
                earliest: SessionFrame::new(0),
                deadline: SessionFrame::new(250_000),
            },
            None,
        )
        .expect_err("a building grid carries no entry geometry");

    assert!(
        matches!(refusal, super::EntryRefusal::Geometry(_)),
        "got {refusal:?}"
    );
    assert_eq!(group.status(), before);
    assert!(group.prepared().is_empty());
}

#[kithara::test]
fn a_waiting_member_with_a_cue_enters_on_the_cue_bar_phase() {
    let mut group = synced_deck();
    let track = BeatGridId::allocate().expect("grid id");
    attach_waiting_grid(&mut group, four_four_grid(track, 480_000, 24_000));

    let _ = group
        .prepare_entry(
            track,
            EntryWindow {
                earliest: SessionFrame::new(0),
                deadline: SessionFrame::new(250_000),
            },
            Some(Beat::new(2.0).expect("cue beat")),
        )
        .expect("a cued member enters inside its window");
    let prepared = group.prepared().get(track).expect("entry map");

    assert_eq!(prepared.activation, SessionFrame::new(240_000));
    assert_eq!(prepared.source, 48_000);
}

#[kithara::test]
fn an_entry_placed_past_a_moved_deadline_is_discarded() {
    let mut group = synced_deck();
    let track = BeatGridId::allocate().expect("grid id");
    attach_waiting_grid(&mut group, four_four_grid(track, 480_000, 24_000));
    let _ = group
        .prepare_entry(
            track,
            EntryWindow {
                earliest: SessionFrame::new(0),
                deadline: SessionFrame::new(250_000),
            },
            None,
        )
        .expect("a waiting member enters inside its window");

    group.discard_stale_entry(
        track,
        EntryWindow {
            earliest: SessionFrame::new(0),
            deadline: SessionFrame::new(100_000),
        },
    );

    assert!(group.prepared().get(track).is_none());
}

#[kithara::test]
fn entering_arms_the_member_that_reached_presentation() {
    let mut group = synced_deck();
    let track = BeatGridId::allocate().expect("grid id");
    attach_waiting_grid(&mut group, four_four_grid(track, 480_000, 24_000));
    let SyncAdmission::Prepared {
        operation,
        warp_map,
        ..
    } = group
        .prepare_entry(
            track,
            EntryWindow {
                earliest: SessionFrame::new(0),
                deadline: SessionFrame::new(250_000),
            },
            None,
        )
        .expect("a waiting member enters inside its window")
    else {
        panic!("an entry prepares a map");
    };
    assert_eq!(member_arm(&group, track), MemberArm::Waiting);
    let (load, transport) = group.generations();
    let applied = SyncApplied::builder()
        .group(group.snapshot().stamp())
        .load(load)
        .frontier(frontier_at_zero())
        .operation(operation)
        .topology(group.topology().expect("topology").stamp())
        .transport(transport)
        .warp_map(warp_map)
        .build();

    let status = group.acknowledge(applied).expect("acknowledged");

    assert!(matches!(status, SyncStatusSnapshot::Locked { .. }));
    assert_eq!(member_arm(&group, track), MemberArm::Armed);
}

#[kithara::test]
fn an_armed_member_refuses_a_second_entry() {
    let mut group = synced_deck();
    let track = BeatGridId::allocate().expect("grid id");
    attach_grid(&mut group, four_four_grid(track, 480_000, 24_000));

    let refusal = group.prepare_entry(
        track,
        EntryWindow {
            earliest: SessionFrame::new(0),
            deadline: SessionFrame::new(250_000),
        },
        None,
    );

    assert!(matches!(
        refusal,
        Err(super::EntryRefusal::Group(
            SyncError::MemberAlreadyArmed { member_id, .. }
        )) if member_id == track
    ));
}

#[kithara::test]
fn each_member_keeps_its_own_prepared_map() {
    let mut group = synced_deck();
    let first = BeatGridId::allocate().expect("grid id");
    let second = BeatGridId::allocate().expect("grid id");
    attach_grid(&mut group, asset_grid(first, 480_000, 24_000));
    attach_grid(&mut group, asset_grid(second, 480_000, 36_000));

    let _ = reconcile(&mut group, first, ReconcileCause::GridAvailable);
    let _ = reconcile(&mut group, second, ReconcileCause::GridAvailable);

    let prepared_first = group.prepared().get(first).expect("first member's map");
    let prepared_second = group.prepared().get(second).expect("second member's map");
    assert_eq!(prepared_first.target, first);
    assert_eq!(prepared_second.target, second);
    assert_ne!(prepared_first.operation, prepared_second.operation);
}

#[kithara::test]
fn a_tempo_commit_retargets_only_the_member_holding_no_prepared_map() {
    let axis = SessionAxis::new(
        NonZeroU32::new(44_100).expect("sample rate"),
        SessionEpoch::new(0),
    );
    let anchor = |beats_per_second| {
        SessionAnchor::new(
            SessionFrame::new(0),
            SessionBeat::new(0.0).expect("beat"),
            beats_per_second,
            axis,
        )
        .expect("session anchor")
    };
    let mut group = GroupState::<PlayerMember>::unavailable(
        BeatGridId::allocate().expect("grid id"),
        NonZeroU32::new(44_100).expect("sample rate"),
        SessionEpoch::new(0),
        SyncMemberKind::Grid,
        SyncMode::HostSync,
    );
    group
        .publish_session_anchor(anchor(2.0))
        .expect("the first anchor makes the deck grid live");
    let prepared_member = BeatGridId::allocate().expect("grid id");
    let settled_member = BeatGridId::allocate().expect("grid id");
    attach_grid(&mut group, asset_grid(prepared_member, 480_000, 24_000));
    attach_grid(&mut group, asset_grid(settled_member, 480_000, 36_000));
    let _ = reconcile(&mut group, prepared_member, ReconcileCause::GridAvailable);

    let faster = anchor(2.5);

    assert!(!group.retargets_tempo(prepared_member, faster));
    assert!(group.retargets_tempo(settled_member, faster));
}

#[kithara::test]
fn a_route_boundary_drops_the_preparation_planned_on_the_previous_axis() {
    let anchor = |rate, epoch| {
        SessionAnchor::new(
            SessionFrame::new(0),
            SessionBeat::new(0.0).expect("beat"),
            2.0,
            SessionAxis::new(
                NonZeroU32::new(rate).expect("sample rate"),
                SessionEpoch::new(epoch),
            ),
        )
        .expect("session anchor")
    };
    let mut group = GroupState::<PlayerMember>::unavailable(
        BeatGridId::allocate().expect("grid id"),
        NonZeroU32::new(44_100).expect("sample rate"),
        SessionEpoch::new(0),
        SyncMemberKind::Grid,
        SyncMode::HostSync,
    );
    group
        .publish_session_anchor(anchor(44_100, 0))
        .expect("the first anchor makes the deck grid live");
    let track = BeatGridId::allocate().expect("grid id");
    attach_grid(&mut group, asset_grid(track, 480_000, 24_000));
    let admission = reconcile(&mut group, track, ReconcileCause::GridAvailable);
    assert!(matches!(admission, SyncAdmission::Prepared { .. }));

    group
        .publish_session_anchor(anchor(48_000, 1))
        .expect("the successor epoch steps the deck through an unavailable grid");

    assert!(group.prepared().is_empty());
}

#[kithara::test]
fn host_seek_quantizes_between_beats_and_keeps_an_exact_beat() {
    let mut group = synced_deck();
    let track = BeatGridId::allocate().expect("grid id");
    let grid = asset_grid(track, 480_000, 24_000);
    let stamp = grid.stamp();
    attach_grid(&mut group, grid);
    let transport = group.generations().1;
    let prepare = |source| {
        host_seek::prepare(
            &group,
            stamp,
            AlignmentSource::Prepared(
                PresentationFrontier::builder()
                    .source(source)
                    .output(SessionFrame::new(10_000))
                    .build(),
            ),
            transport,
        )
        .expect("host seek preparation")
    };

    let between = prepare(10_000);
    assert_eq!(between.prepared.source, 24_000);
    assert!(between.prepared.activation >= SessionFrame::new(10_000));
    let on_beat = prepare(24_000);
    assert_eq!(on_beat.prepared.source, 24_000);
}

#[kithara::test]
fn stale_host_seek_topology_commits_nothing() {
    let mut group = synced_deck();
    let track = BeatGridId::allocate().expect("grid id");
    let grid = asset_grid(track, 480_000, 24_000);
    let stamp = grid.stamp();
    attach_grid(&mut group, grid);
    let candidate = host_seek::prepare(
        &group,
        stamp,
        AlignmentSource::Prepared(frontier_at_zero()),
        group.generations().1,
    )
    .expect("candidate");
    let before_generations = group.generations();
    let before_prepared = group.prepared().clone();
    attach_grid(
        &mut group,
        asset_grid(
            BeatGridId::allocate().expect("replacement grid id"),
            480_000,
            24_000,
        ),
    );

    assert!(matches!(
        host_seek::commit(&mut group, candidate),
        Err(SyncError::OwnerUnavailable)
    ));
    assert_eq!(group.generations(), before_generations);
    assert_eq!(*group.prepared(), before_prepared);
}

#[kithara::test]
fn stale_host_seek_member_grid_stamp_commits_nothing() {
    let mut group = synced_deck();
    let track = BeatGridId::allocate().expect("grid id");
    let first = asset_grid(track, 480_000, 24_000);
    let stamp = first.stamp();
    attach_grid(&mut group, first);
    let candidate = host_seek::prepare(
        &group,
        stamp,
        AlignmentSource::Prepared(frontier_at_zero()),
        group.generations().1,
    )
    .expect("candidate");
    let base = group.topology().expect("topology").stamp();
    let replacement = BeatGridSnapshot::segments(
        track,
        BeatGridRevision::first()
            .checked_next()
            .expect("fixture grid revision advances"),
        BeatGridState::Complete,
        asset_segments(480_000, 24_000, None),
    )
    .expect("fixture replacement grid is valid");
    let _ = group
        .transact(SyncOperation::Topology {
            base,
            operations: Box::new([TopologyOperation::Replace {
                member: track,
                replacement: SyncMember::Grid {
                    alignment: None,
                    arm: MemberArm::Waiting,
                    grid: Box::new(TestGrid(replacement)),
                },
            }]),
        })
        .expect("replace member grid");
    let before_generations = group.generations();
    let before_prepared = group.prepared().clone();
    let before_status = group.status();

    assert!(matches!(
        host_seek::commit(&mut group, candidate),
        Err(SyncError::OwnerUnavailable)
    ));
    assert_eq!(group.generations(), before_generations);
    assert_eq!(*group.prepared(), before_prepared);
    assert_eq!(group.status(), before_status);
}

#[kithara::test]
fn stale_host_seek_owner_grid_stamp_commits_nothing_without_topology_change() {
    let mut group = synced_deck();
    let track = BeatGridId::allocate().expect("grid id");
    let grid = asset_grid(track, 480_000, 24_000);
    let stamp = grid.stamp();
    attach_grid(&mut group, grid);
    let candidate = host_seek::prepare(
        &group,
        stamp,
        AlignmentSource::Prepared(frontier_at_zero()),
        group.generations().1,
    )
    .expect("candidate");
    let owner = group.snapshot();
    let _ = group
        .publish_grid(session_grid(
            owner.id(),
            owner
                .revision()
                .checked_next()
                .expect("fixture owner revision advances"),
            SessionEpoch::new(0),
            2.0,
        ))
        .expect("publish newer owner grid");
    let before_generations = group.generations();
    let before_prepared = group.prepared().clone();
    let before_status = group.status();
    let before_topology = group.topology().expect("topology").stamp();

    assert!(matches!(
        host_seek::commit(&mut group, candidate),
        Err(SyncError::OwnerUnavailable)
    ));
    assert_eq!(group.generations(), before_generations);
    assert_eq!(*group.prepared(), before_prepared);
    assert_eq!(group.status(), before_status);
    assert_eq!(group.topology().expect("topology").stamp(), before_topology);
}

#[kithara::test]
fn preparation_before_the_first_grid_beat_cues_that_first_beat() {
    let mut group = synced_deck();
    let track = BeatGridId::allocate().expect("grid id");
    attach_grid(
        &mut group,
        BeatGridSnapshot::segments(
            track,
            BeatGridRevision::first(),
            BeatGridState::Complete,
            asset_segments_from(480_000, 30_000, 30_000, None),
        )
        .expect("fixture asset grid is valid"),
    );
    let frontier = PresentationFrontier::builder()
        .source(6_000)
        .output(SessionFrame::new(6_000))
        .build();

    let admission = reconcile_at(
        &mut group,
        track,
        ReconcileCause::AlignmentRequested,
        frontier,
    );

    assert!(
        matches!(admission, SyncAdmission::Prepared { .. }),
        "{admission:?}"
    );
    let prepared = group.prepared().latest().expect("prepared relation");
    assert_eq!(prepared.source, 30_000);
}

#[kithara::test]
fn audible_exact_beat_selects_a_reachable_future_cue() {
    let mut group = synced_deck();
    let track = BeatGridId::allocate().expect("grid id");
    attach_grid(&mut group, asset_grid(track, 480_000, 24_000));
    let frontier = frontier_at_zero();
    let (load, transport) = group.generations();

    let _ = group
        .transact(SyncOperation::Reconcile {
            target: track,
            load,
            transport,
            cause: ReconcileCause::AlignmentRequested,
            source: AlignmentSource::Audible {
                presentation: frontier,
                preparation_source: frontier.source(),
                playback_rate: RateTarget::default(),
            },
            source_cue: None,
        })
        .expect("audible alignment is admitted");
    let prepared = group.prepared().latest().expect("prepared relation");

    assert_eq!(prepared.source, 24_000);
    assert_eq!(prepared.activation, SessionFrame::new(24_000));
}

#[kithara::test]
fn audible_alignment_seeks_ahead_of_the_live_source_at_the_next_host_downbeat() {
    let mut group = synced_deck();
    let track = BeatGridId::allocate().expect("grid id");
    let exact = FrameUncertainty::new(0.0).expect("zero uncertainty is finite");
    let meter = Meter::new(4).expect("fixture meter is valid");
    attach_grid(
        &mut group,
        asset_grid_with_meter(
            track,
            960_000,
            24_000,
            Some(MeterFacts::new(meter, BeatEvidence::Observed, exact)),
        ),
    );
    let frontier = PresentationFrontier::builder()
        .source(383_872)
        .output(SessionFrame::new(60_768))
        .build();
    let (load, transport) = group.generations();

    let _ = group
        .transact(SyncOperation::Reconcile {
            target: track,
            load,
            transport,
            cause: ReconcileCause::GridAvailable,
            source: AlignmentSource::Audible {
                presentation: frontier,
                preparation_source: 384_320,
                playback_rate: RateTarget::default(),
            },
            source_cue: None,
        })
        .expect("audible alignment is admitted");
    let prepared = group.prepared().latest().expect("prepared relation");

    // Preparation reaches output 61_216 (beat 2.55), so the next Host downbeat
    // is beat 4 at 96_000. The live mapping stands at source 419_104 there
    // (beat 17.46); the next source downbeat is beat 20 at 480_000, which the
    // old stream has not reached when the seek activates.
    assert_eq!(prepared.activation, SessionFrame::new(96_000));
    assert_eq!(prepared.source, 480_000);
}

#[kithara::test]
#[case::source_has_farther_to_travel(6_000, 18_000, 18_000, 6_000)]
#[case::host_has_farther_to_travel(18_000, 6_000, 6_000, 18_000)]
fn preparation_preserves_both_phase_error_directions(
    #[case] source_frontier: u64,
    #[case] output_frontier: i64,
    #[case] expected_source_distance: u64,
    #[case] expected_output_distance: i64,
) {
    let mut group = synced_deck();
    let track = BeatGridId::allocate().expect("grid id");
    attach_grid(&mut group, asset_grid(track, 480_000, 24_000));
    let frontier = PresentationFrontier::builder()
        .source(source_frontier)
        .output(SessionFrame::new(output_frontier))
        .build();

    let _ = reconcile_at(&mut group, track, ReconcileCause::GridAvailable, frontier);
    let prepared = group.prepared().latest().expect("prepared relation");

    assert_eq!(prepared.source, 24_000);
    assert_eq!(prepared.activation, SessionFrame::new(24_000));
    assert_eq!(prepared.source - source_frontier, expected_source_distance);
    assert_eq!(
        i64::from(prepared.activation) - output_frontier,
        expected_output_distance
    );
}

#[kithara::test]
fn different_bpm_grids_produce_one_coherent_phase_and_rate_decision() {
    let mut group = synced_deck();
    let track = BeatGridId::allocate().expect("grid id");
    let segments = asset_segments(480_000, 30_000, None);
    attach_grid(
        &mut group,
        BeatGridSnapshot::segments(
            track,
            BeatGridRevision::first(),
            BeatGridState::Complete,
            segments.clone(),
        )
        .expect("fixture asset grid is valid"),
    );
    let frontier = PresentationFrontier::builder()
        .source(6_000)
        .output(SessionFrame::new(6_000))
        .build();

    let _ = reconcile_at(&mut group, track, ReconcileCause::GridAvailable, frontier);
    let prepared = group.prepared().latest().expect("prepared relation");
    assert_eq!(prepared.source, 30_000);
    assert_eq!(prepared.activation, SessionFrame::new(24_000));
    assert_eq!(
        prepared.activation_beat,
        SessionBeat::new(1.0).expect("beat")
    );

    let plan = kithara_warp::WarpPlan::new(prepared.projection.clone());
    let BeatGridQuery::Resolved(rate) = plan.rate_at(prepared.activation) else {
        panic!("the prepared projection answers a rate at the activation it was built for");
    };
    assert!((rate - 1.25).abs() < 1e-9, "{rate}");
}

#[kithara::test]
fn preparation_aligns_a_known_track_downbeat_to_the_session_origin_phase() {
    let mut group = synced_deck();
    let track = BeatGridId::allocate().expect("grid id");
    let exact = FrameUncertainty::new(0.0).expect("zero uncertainty is finite");
    let meter = Meter::new(4).expect("fixture meter is valid");
    attach_grid(
        &mut group,
        asset_grid_with_meter(
            track,
            480_000,
            24_000,
            Some(MeterFacts::new(meter, BeatEvidence::Observed, exact)),
        ),
    );
    let frontier = PresentationFrontier::builder()
        .source(100_000)
        .output(SessionFrame::new(100_000))
        .build();

    let _ = reconcile_at(&mut group, track, ReconcileCause::GridAvailable, frontier);
    let prepared = group.prepared().latest().expect("prepared relation");

    assert_eq!(prepared.source, 192_000);
    assert_eq!(prepared.activation, SessionFrame::new(192_000));
}

#[kithara::test]
fn preparation_preserves_non_four_four_downbeat_phase() {
    let mut group = synced_deck();
    let track = BeatGridId::allocate().expect("grid id");
    let exact = FrameUncertainty::new(0.0).expect("zero uncertainty is finite");
    let meter = Meter::new(3)
        .expect("fixture meter is valid")
        .with_downbeat(BeatOrdinal::new(1));
    attach_grid(
        &mut group,
        asset_grid_with_meter(
            track,
            480_000,
            24_000,
            Some(MeterFacts::new(meter, BeatEvidence::Observed, exact)),
        ),
    );
    let frontier = PresentationFrontier::builder()
        .source(50_000)
        .output(SessionFrame::new(50_000))
        .build();

    let _ = reconcile_at(&mut group, track, ReconcileCause::GridAvailable, frontier);
    let prepared = group.prepared().latest().expect("prepared relation");

    assert_eq!(
        prepared.source, 96_000,
        "source beat 4 is the next 3/4 downbeat"
    );
    assert_eq!(
        prepared.activation,
        SessionFrame::new(72_000),
        "Host beat 3 is the nearest 3/4 downbeat"
    );
}

#[kithara::test]
fn preparation_keeps_the_nearest_host_downbeat_for_a_large_source_jump() {
    let mut group = synced_deck();
    let track = BeatGridId::allocate().expect("grid id");
    let exact = FrameUncertainty::new(0.0).expect("zero uncertainty is finite");
    let meter = Meter::new(4).expect("fixture meter is valid");
    attach_grid(
        &mut group,
        asset_grid_with_meter(
            track,
            480_000,
            24_000,
            Some(MeterFacts::new(meter, BeatEvidence::Observed, exact)),
        ),
    );
    let frontier = PresentationFrontier::builder()
        .source(252_000)
        .output(SessionFrame::new(191_999))
        .build();

    let _ = reconcile_at(
        &mut group,
        track,
        ReconcileCause::TransportChanged,
        frontier,
    );
    let prepared = group.prepared().latest().expect("prepared relation");

    assert_eq!(prepared.source, 288_000);
    assert_eq!(prepared.activation, SessionFrame::new(192_000));
}

#[kithara::test]
fn explicit_alignment_uses_the_next_beat_instead_of_waiting_for_a_bar() {
    let mut group = synced_deck();
    let track = BeatGridId::allocate().expect("grid id");
    let exact = FrameUncertainty::new(0.0).expect("zero uncertainty is finite");
    let meter = Meter::new(4).expect("fixture meter is valid");
    attach_grid(
        &mut group,
        asset_grid_with_meter(
            track,
            480_000,
            24_000,
            Some(MeterFacts::new(meter, BeatEvidence::Observed, exact)),
        ),
    );
    let frontier = PresentationFrontier::builder()
        .source(252_000)
        .output(SessionFrame::new(191_999))
        .build();

    let _ = reconcile_at(
        &mut group,
        track,
        ReconcileCause::AlignmentRequested,
        frontier,
    );
    let prepared = group.prepared().latest().expect("prepared relation");

    assert_eq!(prepared.source, 264_000);
    assert_eq!(prepared.activation, SessionFrame::new(192_000));
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
    let prepared = group.prepared().latest().expect("prepared relation");
    assert_eq!(prepared.source, 0);
    assert_eq!(prepared.target, track);
    assert_eq!(prepared.warp_map, warp_map);
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

fn grid_tempo_at(group: &GroupState<PlayerMember>, frame: i64) -> f64 {
    let grid = group.snapshot();
    let BeatGridQuery::Resolved(estimate) = grid.tempo_at(MapPoint::new(
        grid.stamp(),
        MapPosition::Session(SessionFrame::new(frame)),
    )) else {
        panic!("a live local grid resolves its tempo");
    };
    f64::from(*estimate.value())
}

fn grid_beat_at(group: &GroupState<PlayerMember>, frame: i64) -> f64 {
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
fn a_local_tempo_is_approached_from_the_tempo_already_playing() {
    let mut deck = synced_deck();
    let now = SessionFrame::new(48_000);
    let before = grid_beat_at(&deck, 48_000);

    let _ = deck
        .transact_at(tempo(deck.id(), 180.0), now)
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
            .transact_at(tempo(deck.id(), target), SessionFrame::new(step * 128))
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
    assert_eq!(fixture_group().deck_tempo(), None);
}

#[kithara::test]
fn a_tempo_retarget_continues_the_audible_source_without_a_new_beat() {
    let axis = SessionAxis::new(
        NonZeroU32::new(48_000).expect("sample rate"),
        SessionEpoch::new(0),
    );
    let mut group = GroupState::<PlayerMember>::unavailable(
        BeatGridId::allocate().expect("grid id"),
        axis.sample_rate(),
        axis.epoch(),
        SyncMemberKind::Grid,
        SyncMode::HostSync,
    );
    let initial = SessionAnchor::new(
        SessionFrame::new(0),
        SessionBeat::new(0.0).expect("beat"),
        2.0,
        axis,
    )
    .expect("initial anchor");
    group
        .publish_session_anchor(initial)
        .expect("the first anchor makes the deck grid live");
    let track = BeatGridId::allocate().expect("grid id");
    attach_grid(&mut group, asset_grid(track, 480_000, 24_000));
    let _ = reconcile_at(
        &mut group,
        track,
        ReconcileCause::GridAvailable,
        PresentationFrontier::builder()
            .source(30_000)
            .output(SessionFrame::new(30_000))
            .build(),
    );
    let planned = group.prepared().latest().expect("prepared relation");
    let faster = SessionAnchor::new(
        SessionFrame::new(60_000),
        initial.beat_at(SessionFrame::new(60_000)).expect("beat"),
        2.5,
        axis,
    )
    .expect("tempo anchor");
    group
        .publish_session_anchor(faster)
        .expect("a tempo commit keeps the session axis");
    let presentation = PresentationFrontier::builder()
        .source(60_000)
        .output(SessionFrame::new(60_000))
        .warp_map(planned.warp_map)
        .build();
    let (load, transport) = group.generations();

    let admission = group
        .transact(SyncOperation::Reconcile {
            target: track,
            load,
            transport,
            cause: ReconcileCause::TempoRetargeted,
            source: AlignmentSource::Audible {
                presentation,
                preparation_source: 60_448,
                playback_rate: RateTarget::default(),
            },
            source_cue: None,
        })
        .expect("a tempo retarget is admitted");
    let retarget = group.prepared().latest().expect("retarget relation");

    assert!(matches!(admission, SyncAdmission::Prepared { .. }));
    assert!(retarget.warp_map > planned.warp_map);
    assert_eq!(
        retarget.source, 60_448,
        "the decoder continues through its source"
    );
    assert!(
        (60_000..=60_448).contains(&i64::from(retarget.activation)),
        "activation {:?} waited for a later beat",
        retarget.activation
    );
}

#[kithara::test]
fn a_tempo_commit_before_the_activation_moves_it_onto_the_live_beat() {
    let axis = SessionAxis::new(
        NonZeroU32::new(48_000).expect("sample rate"),
        SessionEpoch::new(0),
    );
    let mut group = GroupState::<PlayerMember>::unavailable(
        BeatGridId::allocate().expect("grid id"),
        axis.sample_rate(),
        axis.epoch(),
        SyncMemberKind::Grid,
        SyncMode::HostSync,
    );
    let initial = SessionAnchor::new(
        SessionFrame::new(0),
        SessionBeat::new(0.0).expect("beat"),
        2.0,
        axis,
    )
    .expect("initial anchor");
    group
        .publish_session_anchor(initial)
        .expect("the first anchor makes the deck grid live");
    let track = BeatGridId::allocate().expect("grid id");
    attach_grid(&mut group, asset_grid(track, 480_000, 24_000));
    let frontier = PresentationFrontier::builder()
        .source(30_000)
        .output(SessionFrame::new(30_000))
        .build();
    let admission = reconcile_at(&mut group, track, ReconcileCause::GridAvailable, frontier);
    assert!(matches!(admission, SyncAdmission::Prepared { .. }));
    let planned = group.prepared().latest().expect("prepared relation");
    assert_eq!(
        planned.activation,
        initial.frame_at(planned.activation_beat).expect("frame")
    );

    let faster = SessionAnchor::new(
        SessionFrame::new(36_000),
        initial.beat_at(SessionFrame::new(36_000)).expect("beat"),
        2.5,
        axis,
    )
    .expect("tempo anchor");
    group
        .publish_session_anchor(faster)
        .expect("a tempo commit keeps the session axis");
    let successor = group
        .reanchored_prepared(planned.target, faster)
        .expect("the activation beat maps onto the live tempo")
        .expect("the unreached activation moves");
    assert_eq!(group.prepared().get(planned.target), Some(planned.clone()));
    group.adopt_reanchored(successor);

    let prepared = group
        .prepared()
        .latest()
        .expect("the activation survives a tempo commit");
    assert_eq!(prepared.operation, planned.operation);
    assert_eq!(prepared.source, planned.source);
    assert_eq!(prepared.activation_beat, planned.activation_beat);
    assert!(prepared.warp_map > planned.warp_map);
    assert_eq!(
        prepared.activation,
        faster.frame_at(planned.activation_beat).expect("frame")
    );
}
