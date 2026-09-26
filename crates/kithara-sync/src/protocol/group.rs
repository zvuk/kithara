use kithara_signal::{SessionFrame, TransportRevision};
use kithara_warp::{
    BeatGrid, BeatGridId, BeatGridSnapshotError, BeatGridStamp, BeatGridState, CoordinateError,
    GridProjectionError, MapAxis, MapRegion, PresentationFrontier, WarpMapRevision,
};

use crate::{
    LoadGeneration, ParentFact, SyncAdmission, SyncApplied, SyncCapability, SyncExecutionStamp,
    SyncGroupSnapshot, SyncGroupTopologyError, SyncMemberKind, SyncMode, SyncOperation,
    SyncOperationId, SyncReceipt, SyncRejected, SyncStaged, SyncTransition, TopologyStamp,
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
    /// A preparation is issued but its activation has not been presented;
    /// a free handoff carries no map.
    Prepared {
        operation: SyncOperationId,
        topology: TopologyStamp,
        warp_map: Option<WarpMapRevision>,
        activation: SessionFrame,
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
    /// The canonical group owner stopped before accepting an operation.
    #[error("canonical synchronization-group owner is unavailable")]
    OwnerUnavailable,
    /// This group does not implement the requested operation yet.
    #[error("synchronization capability {capability:?} is unavailable")]
    CapabilityUnavailable { capability: SyncCapability },
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
    /// A group owner cannot mint another grid revision.
    #[error("grid revision space is exhausted for group {group_id}")]
    GridRevisionExhausted { group_id: BeatGridId },
    /// An external grid publication reached a group that derives its own grid.
    #[error("group grid is derived by {mode:?} and cannot be published externally")]
    GridOwnedByMode { mode: SyncMode },
    /// A parent fact reached a session root, which owns its axis and tempo.
    #[error("group {group_id} is a session root and follows no parent")]
    SessionRoot { group_id: BeatGridId },
    /// A tempo was addressed to a group that follows its parent's tempo.
    #[error("group {owner} inherits its tempo from its parent")]
    TempoInherited { owner: BeatGridId },
    /// A tempo or phase relation cannot be represented on the session axis.
    #[error(transparent)]
    Coordinate(#[from] CoordinateError),
    /// A grid owner attempted an invalid immutable snapshot transition.
    #[error(transparent)]
    BeatGridSnapshot(#[from] BeatGridSnapshotError),
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
    /// The member a receipt names holds no preparation.
    #[error("synchronization group has no prepared operation")]
    NoPreparedOperation,
    /// The executor repeated a receipt the group already recorded.
    #[error("synchronization operation {operation} was already acknowledged")]
    DuplicateAcknowledgement { operation: SyncOperationId },
    /// The executor reported another operation than the one the member holds.
    #[error("renderer acknowledged operation {given}, expected {expected}")]
    StaleAcknowledgement {
        expected: SyncOperationId,
        given: SyncOperationId,
    },
    /// A receipt names the member's operation under other facts than the
    /// preparation the member holds.
    #[error("receipt {given:?} does not match the held preparation {expected:?}")]
    ReceiptMismatch {
        expected: Box<SyncExecutionStamp>,
        given: Box<SyncExecutionStamp>,
    },
    /// A receipt skips or reverses a phase: armed before installed, presented
    /// before armed, or rejected once armed.
    #[error("receipt for operation {operation} does not follow its current phase")]
    ReceiptOutOfOrder { operation: SyncOperationId },
    /// A presented frontier does not lie on the preparation's map.
    #[error(
        "presented frontier {given:?} does not lie on map {expected:?} of operation {operation}"
    )]
    PresentationMismatch {
        operation: SyncOperationId,
        expected: Option<WarpMapRevision>,
        given: PresentationFrontier,
    },
    /// The candidate ownership tree violates a topology invariant.
    #[error(transparent)]
    Topology(#[from] SyncGroupTopologyError),
    /// No admissible beat boundary lies inside the launch window.
    #[error("member {member_id} can first enter at {first:?}, not before the window end {end:?}")]
    NoAdmissibleBoundary {
        member_id: BeatGridId,
        first: SessionFrame,
        end: SessionFrame,
    },
    /// Public deck sync cannot preserve an entry request whose required beat
    /// geometry has not arrived; the caller may retry after publication.
    #[error("beat geometry for member {member_id} is not available yet")]
    GridCoverageUnavailable { member_id: BeatGridId },
    /// A finished grid proves the requested position or beat lies outside it.
    #[error("grid {grid_id} places nothing at the requested coordinate")]
    OutsideGrid { grid_id: BeatGridId },
    /// A member grid cannot be projected onto its group grid.
    #[error(transparent)]
    Projection(Box<GridProjectionError>),
    /// An audible member names another warp map than the one this group
    /// applied to it.
    #[error("member {member_id} sounds through map {given:?}, not the applied {expected:?}")]
    AudibleMapMismatch {
        member_id: BeatGridId,
        expected: Option<WarpMapRevision>,
        given: Option<WarpMapRevision>,
    },
    /// A member that already sounds through an applied map was asked to
    /// start again; only an audible retarget can move it.
    #[error("member {member_id} already sounds")]
    MemberAudible { member_id: BeatGridId },
    /// A relocation reached a member that sounds through no applied map;
    /// only a launch can start it.
    #[error("member {member_id} sounds through no applied map")]
    MemberSilent { member_id: BeatGridId },
    /// A destructive transport reached a member that sounds through an
    /// applied map; only a relocation moves it without leaving the beats.
    #[error("member {member_id} sounds through an applied map and must be relocated")]
    RelocationRequired { member_id: BeatGridId },
    /// A relocation named another Track load than the one sounding through
    /// the member's applied map.
    #[error("member {member_id} sounds load {expected:?}, not {given:?}")]
    LoadMismatch {
        member_id: BeatGridId,
        expected: LoadGeneration,
        given: LoadGeneration,
    },
    /// A replacement of an unclaimed public entry named a different media
    /// load or transport from the one whose prior timeline is still sounding.
    #[error("member {member_id} changed load or transport before its first sync entry")]
    EntryIdentityMismatch {
        member_id: BeatGridId,
        expected_load: LoadGeneration,
        given_load: LoadGeneration,
        expected_transport: TransportRevision,
        given_transport: TransportRevision,
    },
    /// The member grid does not cover a relocation cue yet; a relocation is
    /// refused rather than left waiting, since its cue ages with the frontier.
    #[error("member {member_id} grid does not cover the relocation cue yet")]
    RelocationUncovered { member_id: BeatGridId },
    /// A member's armed preparation is committed to the output until it is
    /// presented.
    #[error("member {member_id} is committed to operation {operation}")]
    ArmedOperation {
        member_id: BeatGridId,
        operation: SyncOperationId,
    },
    /// A group owner cannot mint another warp-map revision.
    #[error("warp-map revision space is exhausted for group {group_id}")]
    WarpMapRevisionExhausted { group_id: BeatGridId },
}

/// Live owner protocol for a recursive group of beat grids.
///
/// Every group is both a parent and a member of its own parent: operations
/// are routed down through `transact`, the parent's timeline facts arrive
/// through `stage_fact` and `apply_staged`, and `status` and `topology`
/// report upwards. The topology's group-grid stamp must equal
/// `snapshot().stamp()`, and its group identity must equal `id()`.
pub trait SyncGroup: BeatGrid {
    /// Concrete synchronization-group type accepted as a direct child.
    type NestedGroup: SyncGroup;

    /// Computes, without mutation, everything a parent fact changes across
    /// this subtree, so a parent can refuse the fact before any member
    /// changes.
    ///
    /// # Errors
    ///
    /// Returns [`SyncError`] when this group or any nested group refuses the
    /// fact.
    fn stage_fact(&self, fact: ParentFact) -> Result<SyncStaged, SyncError>;

    /// Commits a change [`Self::stage_fact`] computed on this unchanged
    /// subtree, and returns every preparation it issued and withdrew.
    fn apply_staged(&mut self, staged: SyncStaged) -> SyncTransition;

    /// Records an executor's receipt for one preparation in this subtree and
    /// returns the resulting state of the group that issued it.
    ///
    /// # Errors
    ///
    /// Returns [`SyncError`] when the receipt is stale, duplicate, out of
    /// order, or does not match the preparation its member holds; nothing
    /// changes then.
    fn acknowledge(&mut self, receipt: SyncReceipt) -> Result<SyncStatusSnapshot, SyncError>;

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
