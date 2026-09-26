use std::ops::Range;

use kithara_signal::{SessionFrame, TransportRevision};
use kithara_warp::{
    AssetFrame, BeatGridId, BeatGridStamp, BeatsPerMinute, MapRegion, PresentationFrontier,
};

use crate::{
    LoadGeneration, SyncGroup, SyncMember, SyncOperationId, SyncPreparation, SyncTransition,
    TopologyStamp,
};

/// Playback state from which synchronization is requested.
#[derive(Clone, Copy, Debug, PartialEq)]
#[non_exhaustive]
pub enum AlignmentSource {
    /// Decoded audio has not become audible and waits at this recording
    /// frame, from which it may be positioned before playback.
    Prepared(AssetFrame),
    /// Decoded audio has not become audible and must start exactly at this
    /// recording frame, such as a pickup before the first downbeat, so its
    /// beat and bar phase are kept.
    Cued(AssetFrame),
    /// Decoded audio is already audible at the stated exact presentation frontier.
    Audible {
        /// Source/output boundary the audio callback consumed.
        frontier: PresentationFrontier,
        /// Recording seconds the unsynchronized stream advances per session
        /// second, such as a manual playback speed.
        speed: f64,
    },
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
    /// Prepares one direct grid member to enter its group's beat timeline
    /// inside an exact launch window.
    Prepare {
        /// Stable member grid being prepared.
        target: BeatGridId,
        /// Exact Track load being prepared.
        load: LoadGeneration,
        /// Exact committed session transport state.
        transport: TransportRevision,
        /// Where the member's recording stands when the preparation is asked
        /// for.
        source: AlignmentSource,
        /// Session frames the activation may land on: from the first one the
        /// caller can still reach up to the first one it can no longer use.
        window: Range<SessionFrame>,
    },
    /// Moves a member that already sounds through an applied map to an exact
    /// recording cue, entering the group's beats inside a window after its
    /// presented frontier while the applied map keeps sounding.
    Relocate {
        /// Stable member grid being relocated.
        target: BeatGridId,
        /// Exact Track load being relocated.
        load: LoadGeneration,
        /// Exact committed session transport state.
        transport: TransportRevision,
        /// Exact recording frame the member continues from, pickup and
        /// fractional beat phase included.
        cue: AssetFrame,
        /// Boundary the member's applied map presented last.
        frontier: PresentationFrontier,
        /// Session frames the activation may land on: from the first one the
        /// caller can still reach up to the first one it can no longer use.
        window: Range<SessionFrame>,
    },
    /// Commits a new tempo on a group that owns its own beat timeline.
    Tempo {
        /// Stable group grid receiving the tempo.
        target: BeatGridId,
        /// Tempo the group approaches from the tempo already playing.
        tempo: BeatsPerMinute,
        /// Session frame at which the approach starts; the beat playing there
        /// does not move.
        commit: SessionFrame,
        /// Time constant of the approach, in seconds; zero steps at `commit`.
        smoothing: f64,
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
            | Self::Prepare { target, .. }
            | Self::Relocate { target, .. }
            | Self::Tempo { target, .. } => *target,
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
    /// Snap immediately to the parent group's tempo and phase.
    AlignNow,
    /// Leave the beat timeline: continue the audible source unsynchronized.
    Free,
}

/// Relation between a group's beat timeline and its parent.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum SyncMode {
    /// The group claims no musical timeline of its own or of its parent.
    Off,
    /// The group owns its tempo and phase; parent tempo does not reach it.
    LocalSync,
    /// The group follows the parent's accepted tempo and phase.
    HostSync,
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
    /// Leaving a mapped lane for an unsynchronized manual source.
    Free,
}

/// Result of validating and admitting one operation on the control plane.
#[derive(Clone, Debug, PartialEq)]
#[must_use]
#[non_exhaustive]
pub enum SyncAdmission {
    /// A control-plane-only topology transaction committed atomically.
    TopologyChanged {
        /// Identity assigned to the committed transaction.
        operation: SyncOperationId,
        /// Exact topology published by the transaction.
        topology: TopologyStamp,
        /// Preparations the new topology fence withdrew.
        transition: SyncTransition,
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
    /// One member's decision is prepared for one exact render boundary.
    Prepared(SyncPreparation),
    /// The group's mode, beat timeline, or direct member preparation changed.
    StateChanged {
        /// Identity of the admitted operation.
        operation: SyncOperationId,
        /// Topology against which the operation was admitted.
        topology: TopologyStamp,
        /// Mode the group holds after the operation.
        mode: SyncMode,
        /// Group grid published by the operation.
        grid: BeatGridStamp,
        /// Preparations the change issued and withdrew across the subtree.
        transition: SyncTransition,
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
}
