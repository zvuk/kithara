mod arena_contracts;
mod core;
mod legacy_fixture;
mod no_sync_deadline;
mod player_processor_internal;
mod rt_click;
mod rt_metrics;
mod snapshot_contracts;
mod stop;
mod transport;
mod zero_fade;

use std::num::{NonZeroU32, NonZeroUsize};

use firewheel::node::{ProcBuffers, ProcInfo};
use kithara_command::Seq;
use kithara_dsp::param::SmootherConfig;
use kithara_signal::SegmentId;
use kithara_test_utils::kithara;

use self::core::*;
use super::{
    super::{context::read_render_context, track::PlayerTrack},
    shape::{BufferGeometryError, StreamShape},
};
use crate::bridge::{DeckEvent, SessionInbox, Slot, SlotState};
