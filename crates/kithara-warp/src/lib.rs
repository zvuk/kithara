#![forbid(unsafe_code)]

//! Beat-grid geometry, projection, and warp rendering.

mod beat_grid;
mod coordinate;
#[cfg(feature = "render")]
mod render;
mod segment;
#[cfg(all(test, feature = "render"))]
pub(crate) use kithara_test_utils::bufpool as test_pools;
#[cfg(any(test, feature = "mock"))]
pub mod mock;
mod warp;

pub use beat_grid::{
    BeatEstimate, BeatGrid, BeatGridId, BeatGridIdAllocationError, BeatGridModelError,
    BeatGridQuery, BeatGridRegion, BeatGridRevision, BeatGridSnapshot, BeatGridSnapshotError,
    BeatGridStamp, BeatGridState, BeatGridUnavailable, BeatGridView, GridProjectionError,
};
pub(crate) use coordinate::AxisKind;
pub use coordinate::{
    AssetAxis, AssetExtent, AssetFrame, Beat, BeatAlignment, BeatOrdinal, FrameUncertainty,
    MapAxis, MapCoordinateError, MapPoint, MapPosition, SessionAxis,
};
pub(crate) use kithara_signal::{SessionEpoch, SessionFrame};
#[cfg(feature = "render")]
pub use kithara_stretch::{BackendCapabilities as WarpCapabilities, StretchKind};
#[cfg(feature = "render")]
pub use render::{SourceTrajectory, WarpRenderError, WarpRenderer};
pub use segment::{
    BeatEvidence, BeatMarker, BeatsPerMinute, BeatsPerMinuteError, MapRegion, MapRegionError,
    MapSegment, Meter, MeterError, MeterFacts, SegmentEndpoint, SegmentError, SegmentFacts,
    SegmentSet,
};
#[cfg(feature = "render")]
pub use warp::{Warp, WarpConfig, WarpConfigPatch, WarpConfigPatchError};
pub use warp::{WarpCursor, WarpMap, WarpMapRevision};
mod consts;
