mod backend;
mod entry;
mod fixtures;
mod playback;
mod projection;
mod target;
mod timeline;

use std::num::NonZero;

use fixtures::{
    WarpRenderer, chunk, dominant_bin, expected_bin, f64_of, flush_serviced, render_serviced,
    renderer, spec,
};
use kithara_platform::sync::Arc;
use kithara_signal::{AudioChunkInfo, OutputContext, SessionEpoch, SessionFrame};
use kithara_test_utils::kithara;

use super::StretchControls;
use crate::{PresentationFrontier, RenderContext, Warp, WarpConfig, test_pools::pools};
