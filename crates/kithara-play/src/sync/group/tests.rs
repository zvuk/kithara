use std::num::NonZeroU32;

use kithara_test_utils::kithara;
use kithara_warp::{
    AlignmentSource, BeatGrid, BeatGridId, BeatGridQuery, BeatGridRevision, BeatGridSnapshot,
    BeatGridState, BeatsPerMinute, MapPoint, MapPosition, SessionAnchor, SessionBeat, SessionEpoch,
    SessionFrame, SyncAdmission, SyncError, SyncGroup, SyncIntent, SyncMemberKind, SyncMode,
    SyncOperation,
};

use super::GroupState;
use crate::{player::PlayerMember, sync::TempoSource};

#[kithara::test]
#[case::tempo(None)]
#[case::enable(Some(SyncIntent::Enable))]
#[case::free(Some(SyncIntent::Free))]
fn rejected_state_change_preserves_mode_and_tempo(#[case] intent: Option<SyncIntent>) {
    let mut group = GroupState::<PlayerMember>::unavailable(
        BeatGridId::allocate().expect("group identity"),
        NonZeroU32::new(48_000).expect("sample rate"),
        SessionEpoch::new(0),
        SyncMemberKind::Grid,
        SyncMode::LocalSync,
    );
    let tempo = TempoSource::Local(BeatsPerMinute::try_from(90.0).expect("local tempo"));
    group.tempo = tempo;
    group.next_operation = None;
    let operation = match intent {
        Some(intent) => SyncOperation::Sync {
            target: group.id(),
            load: group.generations.0,
            transport: group.generations.1,
            source: AlignmentSource::Prepared,
            activation: SessionFrame::new(0),
            intent,
        },
        None => SyncOperation::Tempo {
            target: group.id(),
            tempo: BeatsPerMinute::try_from(120.0).expect("requested tempo"),
        },
    };
    let (error, _): (SyncError, SyncOperation<PlayerMember>) = group
        .transact(operation)
        .expect_err("operation identities are exhausted")
        .into();
    assert!(matches!(error, SyncError::OperationIdExhausted { .. }));
    assert_eq!(group.mode, SyncMode::LocalSync, "rejected mode changed");
    assert_eq!(group.tempo, tempo, "rejected tempo changed");
}

#[kithara::test]
fn rejected_parent_anchor_preserves_the_committed_grid_and_anchor() {
    let mut group = GroupState::<PlayerMember>::unavailable(
        BeatGridId::allocate().expect("group identity"),
        NonZeroU32::new(48_000).expect("sample rate"),
        SessionEpoch::new(0),
        SyncMemberKind::Grid,
        SyncMode::HostSync,
    );
    let anchor = |rate| {
        SessionAnchor::new(
            SessionFrame::new(0),
            SessionBeat::new(0.0).expect("beat"),
            2.0,
            NonZeroU32::new(rate).expect("sample rate"),
        )
        .expect("session anchor")
    };
    let committed = anchor(48_000);
    group
        .publish_session_anchor(committed)
        .expect("first anchor");
    let stamp = group.snapshot().stamp();
    assert!(matches!(
        group.publish_session_anchor(anchor(44_100)),
        Err(SyncError::GridAxisChanged { .. })
    ));
    assert_eq!(group.snapshot().stamp(), stamp);
    assert_eq!(group.parent_anchor, Some(committed));
}

#[kithara::test]
fn rejected_grid_change_preserves_the_whole_deck_transaction() {
    let mut group = GroupState::<PlayerMember>::unavailable(
        BeatGridId::allocate().expect("group identity"),
        NonZeroU32::new(48_000).expect("sample rate"),
        SessionEpoch::new(0),
        SyncMemberKind::Grid,
        SyncMode::HostSync,
    );
    let anchor = |rate| {
        SessionAnchor::new(
            SessionFrame::new(0),
            SessionBeat::new(0.0).expect("beat"),
            2.0,
            NonZeroU32::new(rate).expect("sample rate"),
        )
        .expect("anchor")
    };
    group
        .publish_session_anchor(anchor(48_000))
        .expect("initial anchor");
    group.mode = SyncMode::LocalSync;
    group.tempo = TempoSource::Local(BeatsPerMinute::try_from(90.0).expect("tempo"));
    group
        .publish_session_anchor(anchor(44_100))
        .expect("local mode records parent intent");
    let stamp = group.snapshot().stamp();
    let next_operation = group.next_operation;
    let status = group.status();
    let tempo = group.tempo;
    let operation = SyncOperation::Sync {
        target: group.id(),
        load: group.generations.0,
        transport: group.generations.1,
        source: AlignmentSource::Prepared,
        activation: SessionFrame::new(0),
        intent: SyncIntent::Enable,
    };
    let (error, _): (SyncError, SyncOperation<PlayerMember>) = group
        .transact_at(operation, SessionFrame::new(0))
        .expect_err("incompatible parent axis must reject the whole operation")
        .into();
    assert!(matches!(error, SyncError::GridAxisChanged { .. }));
    assert_eq!(group.mode, SyncMode::LocalSync);
    assert_eq!(group.tempo, tempo);
    assert_eq!(group.snapshot().stamp(), stamp);
    assert_eq!(group.next_operation, next_operation);
    assert_eq!(group.status(), status);
}

#[kithara::test]
fn local_tempo_transaction_preserves_the_beat_at_its_commit_frame() {
    let mut group = GroupState::<PlayerMember>::unavailable(
        BeatGridId::allocate().expect("group identity"),
        NonZeroU32::new(48_000).expect("sample rate"),
        SessionEpoch::new(0),
        SyncMemberKind::Grid,
        SyncMode::HostSync,
    );
    group
        .publish_session_anchor(
            SessionAnchor::new(
                SessionFrame::new(0),
                SessionBeat::new(0.0).expect("beat"),
                2.0,
                NonZeroU32::new(48_000).expect("sample rate"),
            )
            .expect("anchor"),
        )
        .expect("initial anchor");
    let now = SessionFrame::new(48_000);
    let admission = group
        .transact_at(
            SyncOperation::Sync {
                target: group.id(),
                load: group.generations.0,
                transport: group.generations.1,
                source: AlignmentSource::Prepared,
                activation: now,
                intent: SyncIntent::Disable,
            },
            now,
        )
        .expect("latch local tempo");
    assert!(matches!(admission.0, SyncAdmission::StateChanged { .. }));
    let admission = group
        .transact_at(
            SyncOperation::Tempo {
                target: group.id(),
                tempo: BeatsPerMinute::try_from(90.0).expect("tempo"),
            },
            now,
        )
        .expect("commit local tempo");
    assert!(matches!(admission.0, SyncAdmission::StateChanged { .. }));
    let grid = group.snapshot();
    let beat = |frame| match grid.beat_at(MapPoint::new(
        grid.stamp(),
        MapPosition::Session(SessionFrame::new(frame)),
    )) {
        BeatGridQuery::Resolved(value) => f64::from(*value.value().value()),
        other => panic!("committed local grid must resolve: {other:?}"),
    };
    assert_eq!(group.mode, SyncMode::LocalSync);
    assert_eq!(beat(48_000), 2.0);
    assert_eq!(beat(96_000), 3.5);
}

#[kithara::test]
fn enabling_without_a_parent_withdraws_local_geometry() {
    let id = BeatGridId::allocate().expect("group identity");
    let rate = NonZeroU32::new(48_000).expect("sample rate");
    let anchor = SessionAnchor::new(SessionFrame::new(0), SessionBeat::default(), 1.5, rate)
        .expect("anchor");
    let grid = BeatGridSnapshot::session(
        id,
        BeatGridRevision::first(),
        SessionEpoch::new(0),
        anchor,
        None,
    );
    let mut group =
        GroupState::<PlayerMember>::new(grid, SyncMemberKind::Grid, SyncMode::LocalSync);
    group.tempo = TempoSource::Local(BeatsPerMinute::try_from(90.0).expect("tempo"));
    let admission = group
        .transact_at(
            SyncOperation::Sync {
                target: id,
                load: group.generations.0,
                transport: group.generations.1,
                source: AlignmentSource::Prepared,
                activation: SessionFrame::new(0),
                intent: SyncIntent::Enable,
            },
            SessionFrame::new(0),
        )
        .expect("enable waiting for parent geometry");
    assert!(matches!(admission.0, SyncAdmission::StateChanged { .. }));
    assert_eq!(group.mode, SyncMode::HostSync);
    assert!(matches!(
        group.snapshot().state(),
        BeatGridState::Unavailable(_)
    ));
    group
        .publish_session_anchor(anchor)
        .expect("parent geometry becomes available");
    assert_eq!(group.snapshot().state(), BeatGridState::Live);
}
