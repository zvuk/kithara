use super::{
    LoadGeneration, PresentationFrontier, SyncGroup, SyncMember, SyncOperationId, TopologyStamp,
    TransportRevision, WarpMapRevision,
};
use crate::{Beat, BeatGridId, BeatsPerMinute, MapPoint, MapRegion, SessionFrame};

/// A beat on a source grid aligned with a beat on a target grid.
#[derive(Clone, Copy, Debug, PartialEq, fieldwork::Fieldwork)]
#[fieldwork(opt_in, get)]
#[non_exhaustive]
pub struct BeatAlignment {
    /// Returns the point on the grid being aligned.
    #[field(get, copy)]
    source: MapPoint<Beat>,
    /// Returns the corresponding point on the target grid.
    #[field(get, copy)]
    target: MapPoint<Beat>,
}

impl BeatAlignment {
    /// Creates one directionally explicit alignment edge.
    #[must_use]
    pub const fn new(source: MapPoint<Beat>, target: MapPoint<Beat>) -> Self {
        Self { source, target }
    }
}

/// Playback state from which synchronization is requested.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum AlignmentSource {
    /// Decoded audio has not become audible and may be positioned before playback.
    Prepared,
    /// Decoded audio is already audible at the stated exact presentation frontier.
    Audible(PresentationFrontier),
}

/// One operation routed through the live synchronization-group owner.
#[derive(Debug)]
pub enum SyncOperation<G: SyncGroup> {
    /// Applies one ordered ownership-tree transaction against an exact base.
    Topology {
        /// Topology identity and revision the caller observed.
        base: TopologyStamp,
        /// Ordered operations committed together or not at all.
        operations: Box<[TopologyOperation<G>]>,
    },
    /// Routes one playback transport operation through the resident Deck envelope.
    Transport {
        /// Stable Deck or Track grid receiving the operation.
        target: BeatGridId,
        /// Exact Track load receiving the operation.
        load: LoadGeneration,
        /// Exact committed session transport state.
        transport: TransportRevision,
        /// Playback operation being routed.
        operation: TransportOperation,
    },
    /// Changes the synchronization intent of one Deck.
    Sync {
        /// Stable Deck grid receiving the intent.
        target: BeatGridId,
        /// Exact Track load receiving the intent.
        load: LoadGeneration,
        /// Exact committed session transport state.
        transport: TransportRevision,
        /// Whether the affected audio is prepared or already audible.
        source: AlignmentSource,
        /// Exact output frame at which the intent may take effect.
        activation: SessionFrame,
        /// Requested synchronization state transition.
        intent: SyncIntent,
    },
    /// Sets the local tempo of one group that owns its tempo.
    Tempo {
        /// Group whose local tempo changes.
        target: BeatGridId,
        /// New local tempo.
        tempo: BeatsPerMinute,
    },
    /// Re-evaluates an active warp map after one material control-plane change.
    Reconcile {
        /// Stable Deck grid whose active warp map is being re-evaluated.
        target: BeatGridId,
        /// Exact Track load whose active warp map is being re-evaluated.
        load: LoadGeneration,
        /// Exact committed session transport state.
        transport: TransportRevision,
        /// Change that requires reconciliation.
        cause: ReconcileCause,
        /// Last source/output boundary consumed by the callback.
        frontier: PresentationFrontier,
    },
}

impl<G: SyncGroup> SyncOperation<G> {
    /// Returns the unique group or grid targeted by this operation.
    #[must_use]
    pub const fn target(&self) -> BeatGridId {
        match self {
            Self::Topology { base, .. } => base.group_id,
            Self::Transport { target, .. }
            | Self::Sync { target, .. }
            | Self::Tempo { target, .. }
            | Self::Reconcile { target, .. } => *target,
        }
    }
}

/// One atomic ownership-tree operation.
#[derive(Debug)]
pub enum TopologyOperation<G: SyncGroup> {
    /// Attaches one exclusively owned member to a direct parent group.
    Attach {
        /// Live member transferred to the receiving group.
        member: SyncMember<G>,
    },
    /// Detaches one direct member from a parent group.
    Detach {
        /// Identity of the direct member being detached.
        member: BeatGridId,
    },
    /// Replaces one direct member atomically.
    Replace {
        /// Identity of the direct member being replaced.
        member: BeatGridId,
        /// New live member transferred to the parent group.
        replacement: SyncMember<G>,
    },
}

/// Runtime member category used by group-specific topology policy.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum SyncMemberKind {
    /// An ordinary beat grid, such as a loaded Track.
    Grid,
    /// A nested synchronization group owned by another group.
    Group,
}

/// One playback operation that must cross the resident Deck envelope.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum TransportOperation {
    /// Prepare an exact source position before any affected audio is audible.
    PrepareStart {
        /// Exact decoded source-frame destination.
        source_frame: u64,
    },
    /// Begin or resume playback.
    Play,
    /// Hold playback at the current frontier.
    Pause,
    /// Relocate playback to an exact decoded source-frame destination.
    Seek {
        /// Exact decoded source-frame destination.
        source_frame: u64,
    },
    /// End playback and retire the current render state.
    Stop,
}

/// Requested synchronization transition for one stable Deck.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum SyncIntent {
    /// Start following the parent group's tempo and phase.
    Enable,
    /// Stop future parent-group correction and latch the current settings.
    Disable,
    /// Leave the beat timeline: the group plays at its plain rate multiplier.
    Free,
    /// Snap immediately to the parent group's tempo and phase.
    AlignNow,
}

/// Material change that requires an active warp map to be reconsidered.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum ReconcileCause {
    /// A previously unavailable grid became usable.
    GridAvailable,
    /// A newer grid revision materially changed the active relation.
    GridRefined,
    /// The authoritative session transport changed.
    TransportChanged,
    /// The recursive ownership tree changed.
    TopologyChanged,
}

/// A synchronization capability that may be unavailable in one implementation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum SyncCapability {
    /// Ownership-tree mutation.
    Topology,
    /// Play, pause, seek, start preparation, and stop routing.
    Transport,
    /// Grid-to-grid tempo and phase alignment.
    Alignment,
    /// Continuity-preserving replacement of an active warp map.
    Reconciliation,
}

/// Result of validating and admitting one operation on the control plane.
#[derive(Clone, Copy, Debug, PartialEq)]
#[must_use]
#[non_exhaustive]
pub enum SyncAdmission {
    /// A control-plane-only topology transaction committed atomically.
    TopologyChanged {
        /// Identity assigned to the committed transaction.
        operation: SyncOperationId,
        /// Exact topology published by the transaction.
        topology: TopologyStamp,
    },
    /// A mode or tempo transition was applied; nothing waits on the render
    /// boundary.
    StateChanged {
        /// Operation that carried the transition.
        operation: SyncOperationId,
        /// Topology the transition was applied under.
        topology: TopologyStamp,
    },
    /// A validated SYNC-off transport command may enter the existing sample path.
    Accepted {
        /// Identity of the admitted operation.
        operation: SyncOperationId,
        /// Topology against which the operation was admitted.
        topology: TopologyStamp,
        /// Exact Track load authorized for dispatch.
        load: LoadGeneration,
        /// Exact committed session transport state authorized for dispatch.
        transport: TransportRevision,
    },
    /// A stamped warp map is prepared for one exact render boundary.
    Prepared {
        /// Identity of the admitted operation.
        operation: SyncOperationId,
        /// Topology against which the operation was admitted.
        topology: TopologyStamp,
        /// Immutable warp map prepared by the group.
        warp_map: WarpMapRevision,
        /// Exact output boundary at which the warp map takes effect.
        activation: SessionFrame,
    },
    /// The requested operation already matches committed state.
    Unchanged {
        /// Identity of the admitted operation.
        operation: SyncOperationId,
        /// Topology against which the operation was admitted.
        topology: TopologyStamp,
    },
    /// A later grid revision may make the operation admissible.
    Deferred {
        /// Identity of the deferred operation.
        operation: SyncOperationId,
        /// Topology against which the operation was evaluated.
        topology: TopologyStamp,
        /// Grid coverage required before the operation can be prepared.
        required: MapRegion,
    },
    /// The current implementation cannot perform this operation.
    Unavailable {
        /// Identity of the rejected operation.
        operation: SyncOperationId,
        /// Topology against which the operation was evaluated.
        topology: TopologyStamp,
        /// Missing synchronization capability.
        capability: SyncCapability,
    },
}
