use std::num::{NonZero, NonZeroU32};

use kithara::{
    platform::sync::Arc,
    signal::{
        AudioChunk, AudioChunkInfo, AudioSpec, FrameCount, InterleavedView, OutputContext,
        SessionEpoch, SessionFrame, TransportRevision,
    },
    stretch::StretchKind,
    sync::{SessionAnchor, SessionBeat, SpeedCurve},
    warp::{
        AssetAxis, AssetExtent, Beat, BeatAlignment, BeatGridId, BeatGridQuery, BeatGridRevision,
        BeatGridSnapshot, MapPoint, Warp, WarpConfig, WarpMap, WarpMapRevision, mock::asset_grid,
    },
};
use kithara_test_fixtures::unit_fixtures::warp_nominal_clicks;
use kithara_test_utils::kithara::hang_watchdog;
use num_traits::ToPrimitive;

use crate::test_pools::{Pools, pools, sample_buffer};

mod core;
mod projected;
mod tempo_perf;
use core::*;
pub(crate) use core::{CH, Presented, Projection, render_configured_grid_with_updates};
