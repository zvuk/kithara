#![forbid(unsafe_code)]

//! Beat-grid warping and synchronization contracts.

mod anchor;
mod beat_grid;
mod coordinate;
mod segment;
mod sync;
mod temporal;
#[cfg(all(test, feature = "render"))]
pub(crate) use kithara_bufpool::testing as test_pools;
mod warp;

pub use anchor::{CoordinateError, SessionAnchor, SessionBeat, SessionFrame};
pub use beat_grid::{
    BeatEstimate, BeatGrid, BeatGridId, BeatGridIdAllocationError, BeatGridQuery, BeatGridRegion,
    BeatGridRevision, BeatGridSnapshot, BeatGridSnapshotError, BeatGridStamp, BeatGridState,
    BeatGridUnavailable, BeatGridView,
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
    AlignmentSource, BeatAlignment, LoadGeneration, PresentationFrontier, ReconcileCause,
    SyncAdmission, SyncApplied, SyncCapability, SyncError, SyncGroup, SyncGroupSnapshot,
    SyncGroupTopologyError, SyncIntent, SyncMember, SyncMemberKind, SyncMemberSnapshot,
    SyncOperation, SyncOperationId, SyncRejected, SyncStatusSnapshot, TopologyOperation,
    TopologyRevision, TopologyStamp, TransportOperation, TransportRevision, WarpMapRevision,
};
#[cfg(all(
    not(target_arch = "wasm32"),
    any(feature = "stretch-signalsmith", feature = "stretch-bungee")
))]
pub use temporal::StretchKind;
pub use temporal::{
    ActiveRegion, GridSegment, RegionPlan, RegionPlanError, RenderContext, RenderPublisher,
    RenderReader, RenderSnapshot, StretchControls,
};
#[cfg(feature = "render")]
pub use warp::WarpRenderer;
pub use warp::{Warp, WarpConfig, WarpCursor, WarpMap};
