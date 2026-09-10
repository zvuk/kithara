use std::num::NonZeroU32;

use kithara_warp::{
    BeatGrid, BeatGridId, BeatGridSnapshot, BeatGridState, SegmentSet, SessionAnchor, SessionEpoch,
    SessionFrame, SyncAdmission, SyncApplied, SyncError, SyncGroup, SyncGroupSnapshot,
    SyncMemberKind, SyncMode, SyncOperation, SyncRejected, SyncStatusSnapshot,
};
use portable_atomic::{AtomicF32, Ordering};

use crate::{
    api::TrackId,
    sync::{DeckGrid, GroupState},
};

pub(crate) struct PlayerSync {
    grid: BeatGridSnapshot,
    owned: Option<GroupState<PlayerMember>>,
    topology: Result<SyncGroupSnapshot, SyncError>,
    status: SyncStatusSnapshot,
}

impl PlayerSync {
    pub(crate) fn owned(&self) -> Result<&GroupState<PlayerMember>, SyncError> {
        self.owned.as_ref().ok_or(SyncError::OwnerUnavailable)
    }

    delegate::delegate! {
        to self.owned.as_mut().ok_or(SyncError::OwnerUnavailable)? {
            pub(crate) fn publish_session_anchor(&mut self, anchor: SessionAnchor) -> Result<(), SyncError>;
        }
    }

    pub(crate) fn transact_at(
        &mut self,
        operation: SyncOperation<PlayerMember>,
        now: SessionFrame,
    ) -> Result<(SyncAdmission, Option<DeckGrid>), SyncRejected<PlayerMember>> {
        match self.owned.as_mut() {
            Some(owned) => owned.transact_at(operation, now),
            None => Err(SyncRejected::new(SyncError::OwnerUnavailable, operation)),
        }
    }

    pub(crate) fn take(&mut self) -> Option<GroupState<PlayerMember>> {
        let owned = self.owned.take()?;
        self.grid = owned.snapshot();
        self.topology = owned.topology();
        self.status = owned.status();
        Some(owned)
    }
    pub(crate) fn unavailable(
        id: BeatGridId,
        sample_rate: NonZeroU32,
        epoch: SessionEpoch,
        member_kind: SyncMemberKind,
        mode: SyncMode,
    ) -> Self {
        let owned = GroupState::unavailable(id, sample_rate, epoch, member_kind, mode);
        Self {
            grid: owned.snapshot(),
            topology: owned.topology(),
            status: owned.status(),
            owned: Some(owned),
        }
    }
}

impl BeatGrid for PlayerSync {
    fn id(&self) -> BeatGridId {
        self.owned.as_ref().map_or(self.grid.id(), BeatGrid::id)
    }

    fn snapshot(&self) -> BeatGridSnapshot {
        self.owned
            .as_ref()
            .map_or_else(|| self.grid.clone(), BeatGrid::snapshot)
    }
}

impl SyncGroup for PlayerSync {
    type NestedGroup = PlayerMember;

    fn acknowledge(&mut self, applied: SyncApplied) -> Result<SyncStatusSnapshot, SyncError> {
        self.owned
            .as_mut()
            .map_or(Err(SyncError::OwnerUnavailable), |owned| {
                owned.acknowledge(applied)
            })
    }

    fn status(&self) -> SyncStatusSnapshot {
        self.owned.as_ref().map_or(self.status, SyncGroup::status)
    }

    fn topology(&self) -> Result<SyncGroupSnapshot, SyncError> {
        self.owned
            .as_ref()
            .map_or_else(|| self.topology.clone(), SyncGroup::topology)
    }

    fn transact(
        &mut self,
        operation: SyncOperation<PlayerMember>,
    ) -> Result<SyncAdmission, SyncRejected<PlayerMember>> {
        match self.owned.as_mut() {
            Some(owned) => owned.transact(operation),
            None => Err(SyncRejected::new(SyncError::OwnerUnavailable, operation)),
        }
    }
}

/// Host-owned sendable synchronization state and desired level for a wasm player.
pub struct PlayerMember {
    level: AtomicF32,
    sync: GroupState<PlayerMember>,
}

impl PlayerMember {
    pub(crate) fn new(sync: GroupState<Self>, level: f32) -> Self {
        Self {
            sync,
            level: AtomicF32::new(level),
        }
    }

    /// Commits the Host-applied level after its graph batch succeeds.
    pub fn commit_host_level(&self, level: f32) {
        self.level.store(level, Ordering::Relaxed);
    }

    /// Reads the desired Host level used for later graph registration.
    #[must_use]
    pub fn host_level(&self) -> f32 {
        self.level.load(Ordering::Relaxed)
    }

    /// Pushes the Host's committed session anchor into the member's group.
    ///
    /// # Errors
    ///
    /// Returns the group's grid publication error.
    pub fn commit_session_anchor(&mut self, anchor: SessionAnchor) -> Result<(), SyncError> {
        self.sync.publish_session_anchor(anchor)
    }

    /// Track grids need the player runtime, which the Host-owned wasm member
    /// does not reach.
    ///
    /// # Errors
    ///
    /// Always returns [`SyncError::OwnerUnavailable`].
    pub fn publish_item_grid(
        &mut self,
        _item: TrackId,
        _segments: SegmentSet,
        _state: BeatGridState,
    ) -> Result<SyncAdmission, SyncError> {
        Err(SyncError::OwnerUnavailable)
    }

    /// Acknowledgement needs the player runtime, which the Host-owned wasm
    /// member does not reach.
    ///
    /// # Errors
    ///
    /// Always returns [`SyncError::OwnerUnavailable`].
    pub fn acknowledge_prepared(&mut self) -> Result<Option<SyncStatusSnapshot>, SyncError> {
        Err(SyncError::OwnerUnavailable)
    }
}

impl BeatGrid for PlayerMember {
    delegate::delegate! {
        to self.sync {
            fn id(&self) -> BeatGridId;
            fn snapshot(&self) -> BeatGridSnapshot;
        }
    }
}

impl SyncGroup for PlayerMember {
    type NestedGroup = Self;

    delegate::delegate! {
        to self.sync {
            fn topology(&self) -> Result<SyncGroupSnapshot, SyncError>;
            fn transact(
                &mut self,
                operation: SyncOperation<Self>,
            ) -> Result<SyncAdmission, SyncRejected<Self>>;
            fn status(&self) -> SyncStatusSnapshot;
            fn acknowledge(&mut self, applied: SyncApplied) -> Result<SyncStatusSnapshot, SyncError>;
        }
    }
}
