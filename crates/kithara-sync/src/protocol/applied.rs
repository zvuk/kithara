use kithara_warp::PresentationFrontier;

use crate::SyncExecutionStamp;

/// What an executor reports about one preparation it was handed.
///
/// Receipts follow one preparation through its execution: installed while
/// the executor can still drop it, armed once its activation is committed to
/// the output, presented once it sounds. Only a preparation that is not armed
/// yet can be rejected.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum SyncReceipt {
    /// The executor holds the preparation and has not armed its activation.
    Installed(SyncExecutionStamp),
    /// The executor dropped the preparation; nothing of it sounds.
    Rejected {
        /// Facts of the dropped preparation.
        stamp: SyncExecutionStamp,
        /// Why the executor dropped it.
        reason: SyncExecutionReject,
    },
    /// The executor committed the activation to its output; only a successor
    /// operation or a new session epoch changes it now.
    Armed(SyncExecutionStamp),
    /// The activation became audible.
    Presented(SyncApplied),
}

impl SyncReceipt {
    /// Facts of the preparation this receipt reports on.
    pub(crate) const fn stamp(&self) -> SyncExecutionStamp {
        match self {
            Self::Installed(stamp) | Self::Rejected { stamp, .. } | Self::Armed(stamp) => *stamp,
            Self::Presented(applied) => applied.stamp,
        }
    }
}

/// Why an executor dropped a preparation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum SyncExecutionReject {
    /// The renderer cannot carry out the preparation's geometry.
    Geometry,
    /// The activation passed before the executor could arm it.
    Late,
    /// The executor holds no free worker, pool, or output capacity for
    /// another prepared lane.
    Capacity,
    /// The Host could not complete the bounded owner acknowledgement.
    ControlBusy,
    /// The executor dropped the preparation before it was ready, such as when
    /// its track was unloaded or its session closed.
    Cancelled,
    /// The recording could not be opened, positioned, or decoded for the
    /// preparation.
    Media,
}

/// The fact that one preparation's activation became audible.
#[derive(Clone, Copy, Debug, Eq, PartialEq, bon::Builder, fieldwork::Fieldwork)]
#[fieldwork(opt_in, get)]
#[non_exhaustive]
pub struct SyncApplied {
    /// Facts of the preparation that sounded.
    #[field(get, copy)]
    stamp: SyncExecutionStamp,
    /// Source/output boundary the callback consumed and the map it rendered;
    /// a free handoff renders no map.
    #[field(get, copy)]
    frontier: PresentationFrontier,
}
