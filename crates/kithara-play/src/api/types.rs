pub use kithara_events::{
    DjEvent, EngineEvent, InterruptionKind, ItemEvent, ItemStatus, PlaybackDirection, PlayerEvent,
    PlayerStatus, RouteChangeReason, SessionEvent, SlotId, SyncEvent, TimeControlStatus, TimeRange,
    TransportEvent, WaitingReason,
};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum SessionDuckingMode {
    #[default]
    Off,
    Soft,
    Hard,
}
