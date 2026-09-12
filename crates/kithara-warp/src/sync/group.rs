use super::{
    SyncAdmission, SyncApplied, SyncCapability, SyncGroupSnapshot, SyncGroupTopologyError,
    SyncMemberKind, SyncOperation, SyncOperationId, SyncRejected, TopologyStamp, WarpMapRevision,
};
use crate::{
    BeatGrid, BeatGridId, BeatGridIdAllocationError, BeatGridSnapshotError, BeatGridStamp,
    BeatGridState, MapAxis, MapRegion, SessionFrame,
};

/// Canonical synchronization state observed from one live group.
#[derive(Clone, Copy, Debug, PartialEq)]
#[must_use]
#[non_exhaustive]
pub enum SyncStatusSnapshot {
    /// Parent-group correction is disabled inside the resident signal path.
    Off { topology: TopologyStamp },
    /// The requested alignment needs grid coverage not yet published.
    WaitingForGrid {
        operation: SyncOperationId,
        topology: TopologyStamp,
        required: MapRegion,
    },
    /// A warp map is admitted but its activation has not been acknowledged.
    Prepared {
        operation: SyncOperationId,
        topology: TopologyStamp,
        warp_map: WarpMapRevision,
        activation: SessionFrame,
    },
    /// The requested behavior is not implemented by the current group.
    Unavailable {
        operation: SyncOperationId,
        topology: TopologyStamp,
        capability: SyncCapability,
    },
    /// The renderer has applied a continuity-preserving correction.
    Converging {
        applied: SyncApplied,
        phase_error_frames: f64,
    },
    /// The renderer is holding the target tempo and phase.
    Locked {
        applied: SyncApplied,
        phase_error_frames: f64,
    },
}

/// A synchronization operation violates the live group contract.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
#[non_exhaustive]
pub enum SyncError {
    /// The real-time command queue cannot admit this transaction yet; retry it unchanged.
    #[error("real-time command queue is full")]
    SlotChannelFull,
    /// The canonical group owner stopped before accepting an operation.
    #[error("canonical synchronization-group owner is unavailable")]
    OwnerUnavailable,
    /// This group does not implement the requested operation yet.
    #[error("synchronization capability {capability:?} is unavailable")]
    CapabilityUnavailable { capability: SyncCapability },
    /// The group inherits its tempo from its parent; set it there.
    #[error("tempo of group {owner:?} is inherited from its parent")]
    TempoInherited { owner: BeatGridId },
    /// A topology transaction was based on another published revision.
    #[error("topology base is {given:?}, expected {expected:?}")]
    StaleTopology {
        expected: TopologyStamp,
        given: TopologyStamp,
    },
    /// No live group with the requested identity exists in this tree.
    #[error("synchronization group {group_id} was not found")]
    GroupNotFound { group_id: BeatGridId },
    /// A live grid snapshot or publication belongs to another stable owner.
    #[error("grid identity is {given}, expected {expected}")]
    GridIdentityMismatch {
        expected: BeatGridId,
        given: BeatGridId,
    },
    /// A group-grid publication did not advance the owner's current revision.
    #[error("group grid publication {given:?} does not advance {current:?}")]
    StaleGridRevision {
        current: BeatGridStamp,
        given: BeatGridStamp,
    },
    /// A group-grid publication changed its native axis outside a session restart.
    #[error("group grid publication changed axis from {expected:?} to {given:?}")]
    GridAxisChanged { expected: MapAxis, given: MapAxis },
    /// A group grid used a bounded-analysis lifecycle state.
    #[error("state {state:?} is invalid for a synchronization-group grid")]
    InvalidGroupGridState { state: BeatGridState },
    /// A group-grid lifecycle change requires a new session epoch.
    #[error("group grid cannot transition from {from:?} to {to:?} in one session epoch")]
    InvalidGroupGridTransition {
        from: BeatGridState,
        to: BeatGridState,
    },
    /// A grid owner attempted an invalid immutable snapshot transition.
    #[error(transparent)]
    BeatGridSnapshot(#[from] BeatGridSnapshotError),
    /// A grid owner cannot allocate another grid identity.
    #[error(transparent)]
    BeatGridIdAllocation(#[from] BeatGridIdAllocationError),
    /// A grid owner cannot mint another revision of one grid.
    #[error("beat grid revision space is exhausted for grid {grid_id}")]
    BeatGridRevisionExhausted { grid_id: BeatGridId },
    /// No direct member with the requested identity exists in this group.
    #[error("member {member_id} was not found in group {group_id}")]
    MemberNotFound {
        group_id: BeatGridId,
        member_id: BeatGridId,
    },
    /// A group policy does not admit this category of direct member.
    #[error("member {member_id} in group {group_id} has kind {given:?}, expected {expected:?}")]
    InvalidMemberKind {
        group_id: BeatGridId,
        member_id: BeatGridId,
        expected: SyncMemberKind,
        given: SyncMemberKind,
    },
    /// A topology owner cannot mint another revision.
    #[error("topology revision space is exhausted for group {group_id}")]
    TopologyRevisionExhausted { group_id: BeatGridId },
    /// A group owner cannot mint another operation identity.
    #[error("synchronization operation identity space is exhausted for group {group_id}")]
    OperationIdExhausted { group_id: BeatGridId },
    /// A group owner cannot mint another warp-map revision.
    #[error("warp map revision space is exhausted for group {group_id}")]
    WarpMapRevisionExhausted { group_id: BeatGridId },
    /// No prepared renderer operation can accept an acknowledgement.
    #[error("synchronization group has no prepared operation")]
    NoPreparedOperation,
    /// The renderer repeated an acknowledgement that was already committed.
    #[error("synchronization operation {operation} was already acknowledged")]
    DuplicateAcknowledgement { operation: SyncOperationId },
    /// The renderer acknowledged another operation than the prepared one.
    #[error("renderer acknowledged operation {given}, expected {expected}")]
    StaleAcknowledgement {
        expected: SyncOperationId,
        given: SyncOperationId,
    },
    /// One or more renderer acknowledgement stamps do not match the prepared warp map.
    #[error("renderer acknowledgement {given:?} does not match {expected:?}")]
    AppliedMismatch {
        expected: Box<SyncApplied>,
        given: Box<SyncApplied>,
    },
    /// The candidate ownership tree violates a topology invariant.
    #[error(transparent)]
    Topology(#[from] SyncGroupTopologyError),
}

/// Live owner protocol for a recursive group of beat grids.
///
/// The topology's group-grid stamp must equal `snapshot().stamp()`, and its
/// group identity must equal `id()`.
pub trait SyncGroup: BeatGrid {
    /// Concrete synchronization-group type accepted as a direct child.
    type NestedGroup: SyncGroup;

    /// Commits an operation as audibly applied and returns the resulting sync state.
    ///
    /// # Errors
    ///
    /// Returns [`SyncError`] when the acknowledgement is stale, duplicate, or
    /// does not match the currently prepared operation.
    fn acknowledge(&mut self, applied: SyncApplied) -> Result<SyncStatusSnapshot, SyncError>;

    /// Returns the canonical control-plane view of this group's sync state.
    fn status(&self) -> SyncStatusSnapshot;

    /// Returns one immutable topology snapshot for a complete calculation.
    ///
    /// # Errors
    ///
    /// Returns [`SyncError`] when a live child violates the recursive topology
    /// contract while the observation is being materialized.
    fn topology(&self) -> Result<SyncGroupSnapshot, SyncError>;

    /// Validates and admits one operation without claiming audible application.
    ///
    /// # Errors
    ///
    /// Returns [`SyncRejected`] when the operation is invalid or unsupported;
    /// the rejected value retains ownership of the complete operation.
    fn transact(
        &mut self,
        operation: SyncOperation<Self::NestedGroup>,
    ) -> Result<SyncAdmission, SyncRejected<Self::NestedGroup>>;
}
