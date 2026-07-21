pub mod channels;
pub mod eq;
pub mod playback;
pub mod protocol;

pub use channels::{NodeInputs, SlotControl, slot_channels};
pub use eq::SharedEq;
pub(crate) use eq::{EQ_MAX_GAIN_DB, EQ_MIN_GAIN_DB};
pub use playback::{PlaybackShared, PlaybackSnapshot, SessionSeekAttempt};
pub use protocol::{
    PlayerCmd, PlayerNotification, TrackPlaybackStopReason, TrackStart, TrackState, TrackTransition,
};

pub use crate::session::{
    AllocatedSlot, Cmd, CmdMsg, PlayerId, Reply, SessionDispatcher, SessionError, SessionHandle,
    SessionState, StartStreamFn, run_cmd,
};
