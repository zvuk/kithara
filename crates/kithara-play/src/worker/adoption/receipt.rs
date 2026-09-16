use kithara_events::TrackId;
use kithara_warp::{
    LoadGeneration, SessionBeat, SessionFrame, SyncOperationId, TransportRevision, WarpMapRevision,
};

/// A worker outcome for one request identity.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum FreeAdoptionReceipt {
    Installed(FreeAdoptionInstalled),
    Rejected(FreeAdoptionRejected),
}

impl FreeAdoptionReceipt {
    pub(crate) const fn operation(self) -> SyncOperationId {
        match self {
            Self::Installed(value) => value.operation,
            Self::Rejected(value) => value.operation,
        }
    }
    pub(crate) const fn warp_map(self) -> WarpMapRevision {
        match self {
            Self::Installed(value) => value.warp_map,
            Self::Rejected(value) => value.warp_map,
        }
    }
    pub(crate) const fn item(self) -> TrackId {
        match self {
            Self::Installed(value) => value.item,
            Self::Rejected(value) => value.item,
        }
    }
    pub(crate) const fn load(self) -> LoadGeneration {
        match self {
            Self::Installed(value) => value.load,
            Self::Rejected(value) => value.load,
        }
    }
    pub(crate) const fn transport(self) -> TransportRevision {
        match self {
            Self::Installed(value) => value.transport,
            Self::Rejected(value) => value.transport,
        }
    }
}

/// Evidence of one installed worker boundary.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct FreeAdoptionInstalled {
    pub(crate) operation: SyncOperationId,
    pub(crate) warp_map: WarpMapRevision,
    pub(crate) item: TrackId,
    pub(crate) load: LoadGeneration,
    pub(crate) transport: TransportRevision,
    pub(crate) decode_epoch: u64,
    pub(crate) source: u64,
    pub(crate) output: SessionFrame,
    pub(crate) activation_beat: SessionBeat,
}

/// A request did not reach an installable worker boundary.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct FreeAdoptionRejected {
    pub(crate) operation: SyncOperationId,
    pub(crate) warp_map: WarpMapRevision,
    pub(crate) item: TrackId,
    pub(crate) load: LoadGeneration,
    pub(crate) transport: TransportRevision,
    pub(crate) decode_epoch: u64,
    pub(crate) reason: FreeAdoptionRejectReason,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum FreeAdoptionRejectReason {
    Closed,
    Superseded,
    DecodeEpoch,
    SeekEpoch,
    Geometry,
}
