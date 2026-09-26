//! A real Sync owner decision for RT unit tests that need an owner-minted permit.

use std::num::NonZeroU32;

use kithara_signal::{SessionEpoch, SessionFrame, TransportRevision};
use kithara_sync::{
    AlignmentSource, GroupState, LoadGeneration, ParentFact, ParentGridUpdate, SyncAdmission,
    SyncEffect, SyncError, SyncExecutionStamp, SyncGroup, SyncGroupSnapshot, SyncIntent,
    SyncMember, SyncMode, SyncOperation, SyncReceipt, SyncRejected, SyncStaged, SyncStatusSnapshot,
    SyncTransition,
};
use kithara_warp::{
    AssetAxis, AssetExtent, AssetFrame, BeatEvidence, BeatGrid, BeatGridId, BeatGridRevision,
    BeatGridSnapshot, BeatGridStamp, BeatGridState, BeatMarker, BeatOrdinal, FrameUncertainty,
    MapAxis, MapPosition, MapSegment, SegmentFacts, SegmentSet, SessionAnchor, SessionBeat,
    WarpMapRevision,
};

struct FixtureGroup(GroupState<Self>);

impl BeatGrid for FixtureGroup {
    delegate::delegate! {
        to self.0 {
            fn id(&self) -> BeatGridId;
            fn snapshot(&self) -> BeatGridSnapshot;
        }
    }
}

impl SyncGroup for FixtureGroup {
    type NestedGroup = Self;

    delegate::delegate! {
        to self.0 {
            fn stage_fact(&self, fact: ParentFact) -> Result<SyncStaged, SyncError>;
            fn apply_staged(&mut self, staged: SyncStaged) -> SyncTransition;
            fn acknowledge(&mut self, receipt: SyncReceipt) -> Result<SyncStatusSnapshot, SyncError>;
            fn status(&self) -> SyncStatusSnapshot;
            fn mode(&self) -> SyncMode;
            fn topology(&self) -> Result<SyncGroupSnapshot, SyncError>;
            fn transact(&mut self, operation: SyncOperation<Self>) -> Result<SyncAdmission, SyncRejected<Self>>;
        }
    }
}

struct FixtureGrid(BeatGridSnapshot);

impl BeatGrid for FixtureGrid {
    delegate::delegate! {
        to self.0 {
            fn id(&self) -> BeatGridId;
            #[call(clone)]
            fn snapshot(&self) -> BeatGridSnapshot;
        }
    }
}

fn source_grid(member: BeatGridId, rate: NonZeroU32) -> BeatGridSnapshot {
    let exact = FrameUncertainty::new(0.0).expect("fixture uncertainty");
    let marker = |frame: u64, ordinal: i64| {
        BeatMarker::new(
            MapPosition::Asset(AssetFrame::new(frame as f64).expect("fixture source frame")),
            Some(BeatOrdinal::new(ordinal)),
            BeatEvidence::Observed,
            exact,
        )
    };
    let segment = MapSegment::new(
        marker(0, 0),
        marker(24_000, 1),
        SegmentFacts::new(BeatEvidence::Observed, exact, None),
    )
    .expect("fixture beat interval");
    let segments = SegmentSet::new(
        MapAxis::Asset(AssetAxis::new(rate, AssetExtent::Bounded(24_000))),
        vec![segment],
    )
    .expect("fixture segment set");
    BeatGridSnapshot::segments(
        member,
        BeatGridRevision::first(),
        BeatGridState::Complete,
        segments,
    )
    .expect("fixture source grid")
}

/// Return the exact stamp and map revision from one public owner Enable at
/// output frame 32; neither RT test manufactures a private execution stamp.
pub(super) fn prepared_entry(
    member: BeatGridId,
    group: BeatGridId,
    load: LoadGeneration,
    transport: TransportRevision,
    output_transport: Option<TransportRevision>,
    rate: NonZeroU32,
) -> (SyncExecutionStamp, WarpMapRevision) {
    let mut owner: GroupState<FixtureGroup> = GroupState::owning(
        group,
        rate,
        SessionEpoch::new(1),
        SyncMember::Grid {
            alignment: None,
            grid: Box::new(FixtureGrid(source_grid(member, rate))),
        },
    );
    let parent = BeatGridId::allocate().expect("fixture parent identity");
    let anchor = SessionAnchor::new(SessionFrame::new(32), SessionBeat::default(), 2.0, rate)
        .expect("fixture parent anchor");
    let mut update = ParentGridUpdate::new(
        BeatGridStamp::new(parent, BeatGridRevision::first()),
        SessionEpoch::new(1),
        anchor,
        None,
    );
    if let Some(revision) = output_transport {
        update = update.with_output_transport(revision);
    }
    let fact = ParentFact::Segment(update);
    let staged = owner.stage_fact(fact).expect("fixture parent publication");
    owner.apply_staged(staged);
    let admission = owner
        .transact(SyncOperation::Sync {
            target: group,
            load,
            transport,
            source: AlignmentSource::Prepared(AssetFrame::new(0.0).expect("fixture cue")),
            activation: SessionFrame::new(32),
            intent: SyncIntent::Enable,
        })
        .expect("public owner Enable");
    let SyncAdmission::StateChanged { transition, .. } = admission else {
        panic!("public Enable must issue a preparation");
    };
    let [preparation] = transition.issued() else {
        panic!("one direct member must receive one preparation");
    };
    let SyncEffect::Projection { plan, .. } = preparation.effect() else {
        panic!("public Enable must project the member");
    };
    assert_eq!(plan.activation().output(), SessionFrame::new(32));
    (preparation.stamp(), plan.activation().revision())
}
