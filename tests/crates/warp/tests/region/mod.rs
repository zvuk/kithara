use std::num::{NonZero, NonZeroU32};

use kithara::{
    platform::sync::Arc,
    signal::{
        AudioChunk, AudioChunkInfo, AudioSpec, FrameCount, InterleavedView, OutputContext,
        SessionEpoch, SessionFrame, TransportRevision,
    },
    stretch::StretchKind,
    warp::{
        AssetAxis, AssetExtent, Beat, BeatAlignment, BeatGridId, BeatGridQuery, BeatGridRevision,
        BeatGridSnapshot, GridSegment, MapPoint, PresentationFrontier, RegionPlan, RegionPlanError,
        RenderContext, SessionAnchor, SessionBeat, SpeedCurve, Warp, WarpConfig, WarpMap,
        WarpMapRevision, mock::asset_grid,
    },
};
use kithara_test_fixtures::unit_fixtures::{warp_clicks, warp_nominal_clicks, warp_sine};
use kithara_test_utils::kithara::hang_watchdog;
use num_traits::ToPrimitive;

use crate::test_pools::{Pools, pools, sample_buffer};

mod core;
mod projected;
use core::*;
pub(crate) use core::{CH, Presented, Projection, Timeline, render_configured_grid_with_updates};
