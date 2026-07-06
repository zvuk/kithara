pub use kithara_events::{
    ItemStatus, PlayerStatus, SlotId, TimeControlStatus, TimeRange, WaitingReason,
};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum SessionDuckingMode {
    #[default]
    Off,
    Soft,
    Hard,
}
