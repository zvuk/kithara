use std::{
    collections::VecDeque,
    num::{NonZeroU32, NonZeroUsize},
};

use kithara_audio::{
    AudioReadError, AudioSource, Fetch, SeekOutcome, SourceDiscontinuity, SourceEnd,
    TrackFailureKind, TrackStep,
};
#[cfg(feature = "stretch-identity")]
use kithara_command::Batch;
use kithara_command::{ChannelConfig, Outcome, Rejection, When, channel};
use kithara_effects::{AudioEffect, EffectDrain, held_source_frames};
use kithara_platform::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};
use kithara_signal::{AudioChunk, AudioSpec, SegmentId, SourceSpan};
#[cfg(any(feature = "stretch-signalsmith", feature = "stretch-bungee"))]
use kithara_test_fixtures::play_fixtures::half;
#[cfg(any(
    feature = "stretch-signalsmith",
    feature = "stretch-bungee",
    feature = "stretch-glide"
))]
use kithara_test_fixtures::play_fixtures::{negative_half, three_quarter};
use kithara_test_fixtures::play_fixtures::{negative_quarter, quarter};
#[cfg(any(
    feature = "stretch-signalsmith",
    feature = "stretch-bungee",
    feature = "stretch-glide"
))]
use kithara_test_utils::bufpool::pools_with_budget;
use kithara_test_utils::{
    bufpool::{TestPools, pools},
    kithara,
};
#[cfg(any(
    feature = "stretch-signalsmith",
    feature = "stretch-bungee",
    feature = "stretch-glide"
))]
use kithara_warp::WarpCapabilities;
use kithara_warp::{SpeedCurve, StretchKind};
#[cfg(any(
    feature = "stretch-signalsmith",
    feature = "stretch-bungee",
    feature = "stretch-glide"
))]
use num_traits::AsPrimitive;

use super::core::*;
use crate::{LaneCommand, LaneFrame, LaneProtocol, LaneSetup};

mod consts;
mod doubles;
mod flow;
mod lane;
use doubles::*;
