use kithara::play::PlayerEvent;

/// Measurements collected from one offline render window.
#[non_exhaustive]
pub struct WindowStats {
    /// Silent blocks in the window.
    pub silent_blocks: u32,
    /// Total blocks in the window.
    pub total_blocks: u32,
    /// First sample of the window in the output buffer.
    pub window_start_sample: usize,
}

impl WindowStats {
    #[must_use]
    pub const fn new(silent_blocks: u32, total_blocks: u32, window_start_sample: usize) -> Self {
        Self {
            silent_blocks,
            total_blocks,
            window_start_sample,
        }
    }
}

#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct TimedPlayerEvent {
    pub frame_end: usize,
    pub event: PlayerEvent,
}

impl TimedPlayerEvent {
    #[must_use]
    pub const fn new(frame_end: usize, event: PlayerEvent) -> Self {
        Self { frame_end, event }
    }
}
