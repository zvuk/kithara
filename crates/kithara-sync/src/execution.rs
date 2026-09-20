use kithara_warp::{
    BeatAlignment, BeatGridId, BeatGridStamp, LoadGeneration, MapAxis, SessionBeat, SessionFrame,
    SyncOperationId, TopologyStamp, TransportRevision, WarpMapRevision,
};

/// One member's installed musical alignment on its owner's output timeline.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MemberAlignment {
    pub alignment: BeatAlignment,
    pub activation: SessionFrame,
    pub activation_beat: SessionBeat,
    pub source: u64,
}

/// Immutable identity of one prepared synchronization execution.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SyncExecutionStamp {
    pub operation: SyncOperationId,
    pub predecessor: WarpMapRevision,
    pub successor: WarpMapRevision,
    pub target: BeatGridId,
    pub load: LoadGeneration,
    pub transport: TransportRevision,
    pub topology: TopologyStamp,
    pub owner_grid: BeatGridStamp,
    pub target_grid: BeatGridStamp,
    pub owner_axis: MapAxis,
    pub target_axis: MapAxis,
}

/// Domain outcome of attempting to install one prepared synchronization.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum SyncExecutionReceipt {
    Installed {
        stamp: SyncExecutionStamp,
        alignment: MemberAlignment,
    },
    Rejected {
        stamp: SyncExecutionStamp,
        reason: SyncExecutionReject,
    },
}

impl SyncExecutionReceipt {
    #[must_use]
    pub const fn stamp(self) -> SyncExecutionStamp {
        match self {
            Self::Installed { stamp, .. } | Self::Rejected { stamp, .. } => stamp,
        }
    }
}

/// A domain-level reason that an exact prepared execution was not installed.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SyncExecutionReject {
    Superseded,
    Unavailable,
    Geometry,
}
