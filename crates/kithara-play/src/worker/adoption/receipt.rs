use kithara_events::TrackId;
use kithara_sync::{MemberAlignment, SyncExecutionStamp};

/// A worker outcome for one request identity.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum FreeAdoptionReceipt {
    Installed(FreeAdoptionInstalled),
    Rejected(FreeAdoptionRejected),
}

impl FreeAdoptionReceipt {
    pub(crate) const fn item(self) -> TrackId {
        match self {
            Self::Installed(value) => value.item,
            Self::Rejected(value) => value.item,
        }
    }
}

/// Evidence of one installed worker boundary.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct FreeAdoptionInstalled {
    pub(crate) stamp: SyncExecutionStamp,
    pub(crate) item: TrackId,
    pub(crate) decode_epoch: u64,
    pub(crate) alignment: MemberAlignment,
}

/// A request did not reach an installable worker boundary.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct FreeAdoptionRejected {
    pub(crate) stamp: SyncExecutionStamp,
    pub(crate) item: TrackId,
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
