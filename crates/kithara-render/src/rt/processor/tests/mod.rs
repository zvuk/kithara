mod core;
mod legacy_fixture;
mod no_sync_deadline;
mod player_processor_internal;
mod rt_click;
mod rt_metrics;
mod transport;

use std::num::{NonZeroU32, NonZeroUsize};

use firewheel::node::{ProcBuffers, ProcInfo};
use kithara_command::{LevelInbox, ScopeId, Seq};
use kithara_dsp::param::SmootherConfig;
use kithara_signal::SegmentId;
use kithara_test_utils::kithara;

use self::core::*;
use super::{
    super::{context::read_render_context, track::PlayerTrack},
    shape::{BufferGeometryError, StreamShape},
    *,
};
use crate::bridge::{DeckEvent, DeckProtocol, SessionInbox, Slot, SlotState};
