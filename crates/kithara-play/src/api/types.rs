pub use kithara_events::{
    DjEvent, EngineEvent, InterruptionKind, ItemEvent, ItemStatus, PlayerEvent, PlayerStatus,
    RouteChangeReason, SessionEvent, SlotId, TimeControlStatus, TimeRange, WaitingReason,
};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum SessionDuckingMode {
    #[default]
    Off,
    Soft,
    Hard,
}
