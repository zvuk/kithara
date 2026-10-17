mod batch;
mod core;
mod fade_events;
mod legacy_lifecycle;
mod legacy_metrics;
mod legacy_rate;
mod media_time;

use std::num::NonZeroU32;

use kithara_command::{Batch, Outcome, Receipt, Rejection, SendError, Sender, Seq, When};
use kithara_events::TrackId;
use kithara_platform::time::Duration;
use kithara_render::{
    CrossfadeSettings, Dispatched, DispatcherProtocol, LaneCommand, LaneProtocol,
    bridge::{
        DeckPart, DeckProtocol, DeckRefusal, Fade, FadeDir, PlaybackFault, Returned, Slot,
        SlotMark, SlotState,
    },
};
use kithara_signal::{FrameCount, SegmentId, SessionFrame};
use num_traits::ToPrimitive;

use self::core::*;
use super::{
    super::{
        outbox::{Outbox, Settled, TrackReceipt},
        settings::{PlayerConfig, TrackSettings, TrackSettingsChange},
    },
    geometry::*,
    state::*,
    *,
};
use crate::{OpenedTrack, PlayError, ResourceLoad};
