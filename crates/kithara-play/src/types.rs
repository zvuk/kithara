use std::time::Duration;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum PlayerStatus {
    #[default]
    Unknown,
    ReadyToPlay,
    Failed,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum TimeControlStatus {
    #[default]
    Paused,
    WaitingToPlay,
    Playing,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum WaitingReason {
    ToMinimizeStalls,
    EvaluatingBufferingRate,
    NoItemToPlay,
    InterruptedBySession,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum ActionAtItemEnd {
    #[default]
    Advance,
    Pause,
    None,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum ItemStatus {
    #[default]
    Unknown,
    ReadyToPlay,
    Failed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct SlotId(pub(crate) u64);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ObserverId(pub(crate) u64);

#[derive(Clone, Copy, Debug, PartialEq)]
#[non_exhaustive]
pub struct TimeRange {
    pub start: Duration,
    pub duration: Duration,
}

impl TimeRange {
    #[must_use]
    pub fn new(start: Duration, duration: Duration) -> Self {
        Self { start, duration }
    }

    #[must_use]
    pub fn end(&self) -> Duration {
        self.start + self.duration
    }

    #[must_use]
    pub fn contains(&self, time: Duration) -> bool {
        time >= self.start && time < self.end()
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum EqBand {
    #[default]
    Low,
    Mid,
    High,
}
