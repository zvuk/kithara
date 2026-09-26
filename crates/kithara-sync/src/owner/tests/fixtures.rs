use std::num::NonZeroU32;

use kithara_signal::{SessionEpoch, SessionFrame};
use kithara_warp::{
    BeatGrid, BeatGridId, BeatGridRevision, BeatGridSnapshot, SessionAnchor, SessionBeat,
};

use crate::{
    GroupState, ParentFact, ParentGridUpdate, SessionAxisUpdate, SyncAdmission, SyncError,
    SyncGroup, SyncGroupSnapshot, SyncMemberKind, SyncMode, SyncOperation, SyncReceipt,
    SyncRejected, SyncStaged, SyncStatusSnapshot, SyncTransition,
};

/// Test-only recursive group that delegates to the real owner state.
///
/// The production nested-group representation lives in `kithara-play`, which
/// this crate must not depend on.
pub(super) struct TestGroup(pub(super) GroupState<Self>);

impl TestGroup {
    fn unavailable(
        id: BeatGridId,
        sample_rate: NonZeroU32,
        epoch: SessionEpoch,
        member_kind: SyncMemberKind,
    ) -> Self {
        Self(GroupState::unavailable(
            id,
            sample_rate,
            epoch,
            member_kind,
            SyncMode::Off,
        ))
    }

    delegate::delegate! {
        to self.0 {
            pub(super) fn publish_grid(
                &mut self,
                candidate: BeatGridSnapshot,
            ) -> Result<SyncTransition, SyncError>;
        }
    }
}

/// Stages one parent fact on a group and commits it, as its parent does.
pub(super) trait Accept: SyncGroup {
    fn accept(&mut self, fact: ParentFact) -> Result<SyncTransition, SyncError> {
        let staged = self.stage_fact(fact)?;
        Ok(self.apply_staged(staged))
    }

    fn accept_parent(&mut self, update: ParentGridUpdate) -> Result<SyncTransition, SyncError> {
        self.accept(ParentFact::Segment(update))
    }

    fn accept_axis(&mut self, update: SessionAxisUpdate) -> Result<SyncTransition, SyncError> {
        self.accept(ParentFact::Axis(update))
    }
}

impl<T: SyncGroup> Accept for T {}

/// A plain live grid owned by a group as its direct member.
pub(super) struct TestGrid(pub(super) BeatGridSnapshot);

impl BeatGrid for TestGrid {
    delegate::delegate! {
        to self.0 {
            fn id(&self) -> BeatGridId;
            #[call(clone)]
            fn snapshot(&self) -> BeatGridSnapshot;
        }
    }
}

impl BeatGrid for TestGroup {
    delegate::delegate! {
        to self.0 {
            fn id(&self) -> BeatGridId;
            fn snapshot(&self) -> BeatGridSnapshot;
        }
    }
}

impl SyncGroup for TestGroup {
    type NestedGroup = Self;

    delegate::delegate! {
        to self.0 {
            fn stage_fact(&self, fact: ParentFact) -> Result<SyncStaged, SyncError>;
            fn apply_staged(&mut self, staged: SyncStaged) -> SyncTransition;
            fn acknowledge(&mut self, receipt: SyncReceipt) -> Result<SyncStatusSnapshot, SyncError>;
            fn status(&self) -> SyncStatusSnapshot;
            fn mode(&self) -> SyncMode;
            fn topology(&self) -> Result<SyncGroupSnapshot, SyncError>;
            fn transact(
                &mut self,
                operation: SyncOperation<Self>,
            ) -> Result<SyncAdmission, SyncRejected<Self>>;
        }
    }
}

pub(super) fn session_grid(
    id: BeatGridId,
    revision: BeatGridRevision,
    epoch: SessionEpoch,
    beats_per_second: f64,
) -> BeatGridSnapshot {
    session_grid_at_rate(id, revision, epoch, beats_per_second, 48_000)
}

pub(super) fn session_grid_at_rate(
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

pub(super) fn fixture_group() -> TestGroup {
    TestGroup::unavailable(
        BeatGridId::allocate().expect("invariant: fixture group id is available"),
        NonZeroU32::new(48_000).expect("invariant: fixture sample rate is non-zero"),
        SessionEpoch::new(0),
        SyncMemberKind::Grid,
    )
}
