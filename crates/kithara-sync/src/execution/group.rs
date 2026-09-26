use std::num::NonZeroU32;

use kithara_signal::SessionEpoch;
use kithara_warp::{BeatGrid, BeatGridId, BeatGridSnapshot};

use super::SyncExecution;
use crate::{
    GroupState, ParentFact, SyncAdmission, SyncError, SyncGroup, SyncGroupSnapshot, SyncMember,
    SyncMode, SyncOperation, SyncReceipt, SyncRejected, SyncStaged, SyncStatusSnapshot,
    SyncTransition,
};

/// What a player hands the owner of its synchronization group: the group's
/// identity and rate, the track geometry the group owns from birth, and the
/// executor that stages the group's preparations.
pub struct SyncAttachment {
    id: BeatGridId,
    sample_rate: NonZeroU32,
    track: Box<dyn BeatGrid + Send + Sync>,
    execution: SyncExecution,
}

impl SyncAttachment {
    /// The attachment of group `id` running at `sample_rate`, owning
    /// `track` and staged by `execution`.
    #[must_use]
    pub fn new(
        id: BeatGridId,
        sample_rate: NonZeroU32,
        track: Box<dyn BeatGrid + Send + Sync>,
        execution: SyncExecution,
    ) -> Self {
        Self {
            id,
            sample_rate,
            track,
            execution,
        }
    }

    /// Identity of the group this attachment builds.
    #[must_use]
    pub const fn id(&self) -> BeatGridId {
        self.id
    }

    /// The group that owns the track geometry as its only member, its
    /// preparations carried out by the attached executor.
    #[must_use]
    pub fn into_group<G: SyncGroup<NestedGroup = G>>(self) -> ExecutedGroup<GroupState<G>> {
        let state = GroupState::owning(
            self.id,
            self.sample_rate,
            SessionEpoch::new(0),
            SyncMember::Grid {
                alignment: None,
                grid: self.track,
            },
        );
        ExecutedGroup::new(state, self.execution)
    }
}

/// A synchronization group whose staged preparations an executor carries
/// out: it refuses what the executor cannot stage before the group admits
/// it, and hands the executor every preparation the group issues or
/// withdraws.
pub struct ExecutedGroup<G> {
    group: G,
    execution: SyncExecution,
}

impl<G: SyncGroup> ExecutedGroup<G> {
    /// `group`, its staged preparations carried out through `execution`.
    #[must_use]
    pub const fn new(group: G, execution: SyncExecution) -> Self {
        Self { group, execution }
    }
}

impl<G: SyncGroup> BeatGrid for ExecutedGroup<G> {
    delegate::delegate! {
        to self.group {
            fn id(&self) -> BeatGridId;
            fn snapshot(&self) -> BeatGridSnapshot;
        }
    }
}

impl<G: SyncGroup> SyncGroup for ExecutedGroup<G> {
    type NestedGroup = G::NestedGroup;

    fn apply_staged(&mut self, staged: SyncStaged) -> SyncTransition {
        let transition = self.group.apply_staged(staged);
        self.execution.follow_transition(&transition);
        transition
    }

    fn transact(
        &mut self,
        operation: SyncOperation<Self::NestedGroup>,
    ) -> Result<SyncAdmission, SyncRejected<Self::NestedGroup>> {
        if let Err(error) = self.execution.admit(&operation) {
            return Err(SyncRejected::new(error, operation));
        }
        let admission = self.group.transact(operation)?;
        self.execution.follow_admission(&admission);
        Ok(admission)
    }

    delegate::delegate! {
        to self.group {
            fn stage_fact(&self, fact: ParentFact) -> Result<SyncStaged, SyncError>;
            fn status(&self) -> SyncStatusSnapshot;
            fn mode(&self) -> SyncMode;
            fn topology(&self) -> Result<SyncGroupSnapshot, SyncError>;
            fn acknowledge(&mut self, receipt: SyncReceipt) -> Result<SyncStatusSnapshot, SyncError>;
        }
    }
}
