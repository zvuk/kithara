use std::ops::Range;

use kithara_signal::{SessionFrame, TransportRevision};
use kithara_test_utils::kithara;
use kithara_warp::{
    AssetFrame, BeatGrid, BeatGridId, BeatGridQuery, PresentationFrontier, WarpMapRevision,
    WarpPlan,
};
use num_traits::ToPrimitive;

use super::{
    modes::{Group, synced_deck, tempo_at},
    preparation::{
        asset_grid, asset_segments_from, attach_grid, building, cue, four_four, observed, prepare,
        window,
    },
    refresh::prepared,
};
use crate::{
    LoadGeneration, SyncAdmission, SyncApplied, SyncEffect, SyncError, SyncExecutionReject,
    SyncGroup, SyncOperation, SyncPreparation, SyncReceipt, TransportOperation,
};

/// The first session frame no caller can use.
const OPEN_END: i64 = i64::MAX;

/// A deck at 120 BPM whose track sounds on its beats from frame 0.
fn sounding_deck() -> (Group, BeatGridId, SyncPreparation) {
    let mut group = synced_deck();
    let track = BeatGridId::allocate().expect("grid id");
    attach_grid(&mut group, asset_grid(track, 960_000, 24_000));
    let launch = launched(&mut group, track);
    sound(&mut group, &launch);
    (group, track, launch)
}

fn launched(group: &mut Group, track: BeatGridId) -> SyncPreparation {
    match prepare(group, track, cue(0), 0) {
        SyncAdmission::Prepared(preparation) => preparation,
        admission => panic!("expected a prepared member, got {admission:?}"),
    }
}

fn plan(preparation: &SyncPreparation) -> &WarpPlan {
    let SyncEffect::Projection { plan, .. } = preparation.effect() else {
        panic!("expected a projection, got {preparation:?}");
    };
    plan
}

fn map(preparation: &SyncPreparation) -> WarpMapRevision {
    plan(preparation).activation().revision()
}

fn acknowledge(group: &mut Group, receipt: SyncReceipt) {
    let _ = group.acknowledge(receipt).expect("the receipt is recorded");
}

/// Installs, arms and presents `preparation` exactly at its activation.
fn sound(group: &mut Group, preparation: &SyncPreparation) {
    acknowledge(group, SyncReceipt::Installed(preparation.stamp()));
    acknowledge(group, SyncReceipt::Armed(preparation.stamp()));
    let (warp_map, activation) = preparation.activation();
    acknowledge(
        group,
        SyncReceipt::Presented(
            SyncApplied::builder()
                .stamp(preparation.stamp())
                .frontier(
                    PresentationFrontier::builder()
                        .maybe_warp_map(warp_map)
                        .source(plan(preparation).activation().source())
                        .output(activation)
                        .build(),
                )
                .build(),
        ),
    );
}

/// The frontier the applied `preparation` reaches at session frame `output`.
fn heard_at(preparation: &SyncPreparation, output: i64) -> PresentationFrontier {
    let BeatGridQuery::Resolved(source) = plan(preparation).source_at(SessionFrame::new(output))
    else {
        panic!("the applied plan covers frame {output}");
    };
    PresentationFrontier::builder()
        .warp_map(map(preparation))
        .source(f64::from(source).round().to_u64().expect("source frame"))
        .output(SessionFrame::new(output))
        .build()
}

fn relocate(
    track: BeatGridId,
    load: LoadGeneration,
    cue: u64,
    frontier: PresentationFrontier,
    window: Range<SessionFrame>,
) -> SyncOperation<super::TestGroup> {
    SyncOperation::Relocate {
        target: track,
        load,
        transport: TransportRevision::first(),
        cue: AssetFrame::new(cue as f64).expect("fixture cue is finite"),
        frontier,
        window,
    }
}

fn relocate_in(
    group: &mut Group,
    operation: SyncOperation<super::TestGroup>,
) -> Result<SyncPreparation, SyncError> {
    match group.transact(operation) {
        Ok(SyncAdmission::Prepared(preparation)) => Ok(preparation),
        Ok(admission) => panic!("a relocation is prepared or refused, got {admission:?}"),
        Err(rejected) => Err(rejected.error().clone()),
    }
}

fn transport(track: BeatGridId, operation: TransportOperation) -> SyncOperation<super::TestGroup> {
    SyncOperation::Transport {
        target: track,
        load: LoadGeneration::first(),
        transport: TransportRevision::first(),
        operation,
    }
}

#[kithara::test]
fn a_relocation_starts_on_the_exact_cue_and_keeps_the_applied_map() {
    let (mut group, track, launch) = sounding_deck();
    let applied = group.applied.clone();
    let pickup = 36_000;

    let relocation = relocate_in(
        &mut group,
        relocate(
            track,
            LoadGeneration::first(),
            pickup,
            heard_at(&launch, 48_000),
            window(96_000, OPEN_END),
        ),
    )
    .expect("a sounding member relocates");

    let SyncEffect::Projection { replaces, plan, .. } = relocation.effect() else {
        panic!("a relocation is a projection");
    };
    assert_eq!(*replaces, Some(map(&launch)));
    assert!(plan.activation().revision() > map(&launch));
    assert_eq!(plan.activation().source(), pickup);
    assert!(plan.activation().output() >= SessionFrame::new(96_000));
    assert_ne!(
        relocation.stamp().operation(),
        launch.stamp().operation(),
        "a relocation is its own operation"
    );
    assert_eq!(group.applied, applied);
    assert_eq!(prepared(&group, track), relocation);
}

#[kithara::test]
fn a_relocation_starts_no_earlier_than_the_frame_after_its_frontier() {
    let (mut group, track, launch) = sounding_deck();

    let relocation = relocate_in(
        &mut group,
        relocate(
            track,
            LoadGeneration::first(),
            0,
            heard_at(&launch, 96_000),
            window(0, OPEN_END),
        ),
    )
    .expect("a sounding member relocates");

    assert!(plan(&relocation).activation().output() > SessionFrame::new(96_000));
    assert_eq!(
        relocate_in(
            &mut group,
            relocate(
                track,
                LoadGeneration::first(),
                0,
                heard_at(&launch, 96_000),
                window(0, 96_001),
            ),
        ),
        Err(SyncError::NoAdmissibleBoundary {
            member_id: track,
            first: SessionFrame::new(96_001),
            end: SessionFrame::new(96_001),
        })
    );
    assert_eq!(
        prepared(&group, track),
        relocation,
        "a refused relocation keeps the pending one"
    );
}

#[kithara::test]
fn a_relocation_is_refused_off_the_applied_lane() {
    let mut group = synced_deck();
    let track = BeatGridId::allocate().expect("grid id");
    attach_grid(&mut group, asset_grid(track, 960_000, 24_000));
    let silent = PresentationFrontier::builder()
        .source(0)
        .output(SessionFrame::new(0))
        .build();
    assert_eq!(
        relocate_in(
            &mut group,
            relocate(
                track,
                LoadGeneration::first(),
                0,
                silent,
                window(0, OPEN_END)
            ),
        ),
        Err(SyncError::MemberSilent { member_id: track })
    );

    let launch = launched(&mut group, track);
    sound(&mut group, &launch);
    let applied = group.applied.clone();
    assert_eq!(
        relocate_in(
            &mut group,
            relocate(
                track,
                LoadGeneration::first(),
                0,
                silent,
                window(0, OPEN_END)
            ),
        ),
        Err(SyncError::AudibleMapMismatch {
            member_id: track,
            expected: Some(map(&launch)),
            given: None,
        })
    );
    let other = LoadGeneration::first().checked_next().expect("load");
    assert_eq!(
        relocate_in(
            &mut group,
            relocate(
                track,
                other,
                0,
                heard_at(&launch, 48_000),
                window(0, OPEN_END)
            ),
        ),
        Err(SyncError::LoadMismatch {
            member_id: track,
            expected: LoadGeneration::first(),
            given: other,
        })
    );
    assert_eq!(group.applied, applied);
    assert!(group.pending.is_empty());
}

#[kithara::test]
fn an_uncovered_cue_refuses_the_relocation_without_waiting() {
    let mut group = synced_deck();
    let track = BeatGridId::allocate().expect("grid id");
    attach_grid(
        &mut group,
        building(
            track,
            asset_segments_from(480_000, 192_000, 24_000, 0, observed(four_four())),
        ),
    );
    let launch = launched(&mut group, track);
    sound(&mut group, &launch);

    let refused = relocate_in(
        &mut group,
        relocate(
            track,
            LoadGeneration::first(),
            300_000,
            heard_at(&launch, 48_000),
            window(96_000, OPEN_END),
        ),
    );

    assert!(
        matches!(refused, Err(SyncError::RelocationUncovered { member_id, .. }) if member_id == track),
        "{refused:?}"
    );
    assert!(group.pending.is_empty());
}

#[kithara::test]
fn an_applied_member_refuses_a_destructive_transport() {
    let (mut group, track, _) = sounding_deck();

    for operation in [
        TransportOperation::Seek { source_frame: 0 },
        TransportOperation::PrepareStart { source_frame: 0 },
    ] {
        assert_eq!(
            group
                .transact(transport(track, operation))
                .map_err(|rejected| rejected.error().clone()),
            Err(SyncError::RelocationRequired { member_id: track })
        );
    }
    assert!(matches!(
        group.transact(transport(track, TransportOperation::Pause)),
        Ok(SyncAdmission::Accepted { .. })
    ));
}

#[kithara::test]
fn an_unmapped_member_keeps_the_ordinary_seek() {
    let mut group = synced_deck();
    let track = BeatGridId::allocate().expect("grid id");
    attach_grid(&mut group, asset_grid(track, 960_000, 24_000));

    assert!(matches!(
        group.transact(transport(
            track,
            TransportOperation::Seek { source_frame: 0 }
        )),
        Ok(SyncAdmission::Accepted { .. })
    ));
}

#[kithara::test]
fn a_tempo_commit_withdraws_the_relocation_and_retargets_under_a_new_operation() {
    let (mut group, track, launch) = sounding_deck();
    let relocation = relocate_in(
        &mut group,
        relocate(
            track,
            LoadGeneration::first(),
            240_000,
            heard_at(&launch, 48_000),
            window(400_000, OPEN_END),
        ),
    )
    .expect("a sounding member relocates");
    acknowledge(&mut group, SyncReceipt::Installed(relocation.stamp()));

    let id = group.id();
    let admission = group
        .transact(tempo_at(id, 130.0, SessionFrame::new(96_000)))
        .expect("the tempo commits");

    let SyncAdmission::StateChanged { transition, .. } = admission else {
        panic!("a tempo commit changes state, got {admission:?}");
    };
    assert_eq!(transition.withdrawn(), [relocation.stamp()]);
    let retarget = prepared(&group, track);
    assert_eq!(transition.issued(), [retarget.clone()]);
    assert_ne!(retarget.stamp().operation(), relocation.stamp().operation());
    let activation = plan(&retarget).activation().output();
    assert!(activation >= SessionFrame::new(98_048));
    assert_eq!(
        plan(&retarget).activation().source(),
        heard_at(&launch, i64::from(activation)).source(),
        "the replacement continues the sounding source at its chosen beat"
    );

    let held = group.pending.clone();
    for late in [
        SyncReceipt::Installed(relocation.stamp()),
        SyncReceipt::Rejected {
            stamp: relocation.stamp(),
            reason: SyncExecutionReject::Cancelled,
        },
    ] {
        assert_eq!(
            group.acknowledge(late),
            Err(SyncError::StaleAcknowledgement {
                expected: retarget.stamp().operation(),
                given: relocation.stamp().operation(),
            })
        );
    }
    assert_eq!(group.pending, held);
}

#[kithara::test]
fn a_capacity_rejection_drops_the_relocation_and_keeps_the_applied_map() {
    let (mut group, track, launch) = sounding_deck();
    let applied = group.applied.clone();
    let relocation = relocate_in(
        &mut group,
        relocate(
            track,
            LoadGeneration::first(),
            0,
            heard_at(&launch, 48_000),
            window(96_000, OPEN_END),
        ),
    )
    .expect("a sounding member relocates");

    acknowledge(
        &mut group,
        SyncReceipt::Rejected {
            stamp: relocation.stamp(),
            reason: SyncExecutionReject::Capacity,
        },
    );

    assert!(group.pending.is_empty());
    assert_eq!(group.applied, applied);
}

#[kithara::test]
fn an_armed_preparation_refuses_a_relocation() {
    let (mut group, track, launch) = sounding_deck();
    let id = group.id();
    let _ = group
        .transact(tempo_at(id, 130.0, SessionFrame::new(96_000)))
        .expect("the tempo commits");
    let retarget = prepared(&group, track);
    acknowledge(&mut group, SyncReceipt::Installed(retarget.stamp()));
    acknowledge(&mut group, SyncReceipt::Armed(retarget.stamp()));

    assert_eq!(
        relocate_in(
            &mut group,
            relocate(
                track,
                LoadGeneration::first(),
                0,
                heard_at(&launch, 48_000),
                window(96_000, OPEN_END),
            ),
        ),
        Err(SyncError::ArmedOperation {
            member_id: track,
            operation: retarget.stamp().operation(),
        })
    );
}
