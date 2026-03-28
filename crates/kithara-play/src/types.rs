pub use kithara_events::{
    ItemStatus, PlayerStatus, SlotId, TimeControlStatus, TimeRange, WaitingReason,
};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum ActionAtItemEnd {
    #[default]
    Advance,
    Pause,
    None,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ObserverId(pub(crate) u64);

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum SessionDuckingMode {
    #[default]
    Off,
    Soft,
    Hard,
}
