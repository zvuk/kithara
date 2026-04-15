/// Behavior when the queue reaches the last track.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub enum RepeatMode {
    /// Stop after the last track.
    #[default]
    Off,
    /// Repeat the currently selected track.
    One,
    /// Loop back to the first track.
    All,
}

/// Queue navigation state (shuffle, repeat, history). Filled in C.3.
#[derive(Debug, Default)]
#[non_exhaustive]
pub struct NavigationState;
