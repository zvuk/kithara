#![forbid(unsafe_code)]

//! Beat-grid warping and synchronization contracts.

mod anchor;
mod beat_grid;
mod coordinate;
mod segment;
mod sync;
mod temporal;
#[cfg(all(test, feature = "render"))]
pub(crate) use kithara_test_utils::bufpool as test_pools;
#[cfg(test)]
mod test_grids;
mod warp;

pub use anchor::{CoordinateError, SessionAnchor, SessionBeat, SessionFrame};
pub use beat_grid::{
    BeatEstimate, BeatGrid, BeatGridId, BeatGridIdAllocationError, BeatGridQuery, BeatGridRegion,
    BeatGridRevision, BeatGridSnapshot, BeatGridSnapshotError, BeatGridStamp, BeatGridState,
    BeatGridUnavailable, BeatGridView, GridProjectionError,
};
pub(crate) use coordinate::AxisKind;
pub use coordinate::{
    AssetAxis, AssetFrame, Beat, BeatOrdinal, FrameUncertainty, MapAxis, MapCoordinateError,
    MapPoint, MapPosition, SessionAxis, SessionEpoch,
};
pub use segment::{
    BeatEvidence, BeatMarker, BeatsPerMinute, BeatsPerMinuteError, MapRegion, MapRegionError,
    MapSegment, Meter, MeterError, MeterFacts, SegmentEndpoint, SegmentError, SegmentFacts,
    SegmentSet,
};
pub use sync::{
    AlignmentSource, BeatAlignment, LoadGeneration, MemberArm, PresentationFrontier,
    ReconcileCause, SyncAdmission, SyncApplied, SyncCapability, SyncError, SyncGroup,
    SyncGroupSnapshot, SyncGroupTopologyError, SyncIntent, SyncMember, SyncMemberKind,
    SyncMemberSnapshot, SyncMode, SyncOperation, SyncOperationId, SyncRejected, SyncStatusSnapshot,
    TopologyOperation, TopologyRevision, TopologyStamp, TransportOperation, TransportRevision,
    WarpMapRevision,
};
#[cfg(feature = "render")]
pub(crate) use temporal::RenderState;
#[cfg(all(
    not(target_arch = "wasm32"),
    any(feature = "stretch-signalsmith", feature = "stretch-bungee")
))]
pub use temporal::StretchKind;
pub use temporal::{
    RateTarget, RenderContext, RenderPublisher, RenderReader, RenderSnapshot, StretchControls,
};
pub use warp::{
    DEFAULT_RATE_SMOOTHING, DEFAULT_TEMPO_SMOOTHING_SECONDS, FreeActivation, Warp, WarpConfig,
    WarpConfigPatch, WarpCursor, WarpMap, WarpPlan, WarpPlanSlot, supports_playback_rate,
};
#[cfg(feature = "render")]
pub use warp::{ScheduledActivationProgress, WarpRenderer};
