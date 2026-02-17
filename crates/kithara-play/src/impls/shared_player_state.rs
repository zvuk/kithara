//! Shared atomic state between the main thread and the audio-thread processor.
//!
//! [`SharedPlayerState`] is wrapped in `Arc` and shared between
//! `PlayerNode` (main thread) and `PlayerNodeProcessor` (audio thread).
//! All fields use atomic operations or lock-free channels for safe
//! concurrent access.

use std::{
    num::NonZeroU32,
    sync::atomic::{AtomicBool, Ordering},
};

use portable_atomic::{AtomicF64, AtomicU32};

use super::player_notification::PlayerNotification;

/// Shared state that bridges the main thread and the audio processor.
///
/// Position and duration are updated by the processor every render cycle.
/// Notifications flow from the processor to the main thread via kanal.
#[derive(Debug)]
pub(crate) struct SharedPlayerState {
    /// Whether playback is active.
    pub(crate) playing: AtomicBool,
    /// Current playback position in seconds.
    pub(crate) position: AtomicF64,
    /// Total duration in seconds.
    pub(crate) duration: AtomicF64,
    /// Current sample rate from the audio stream.
    pub(crate) sample_rate: AtomicU32,
    /// Sender for processor-to-main-thread notifications.
    pub(crate) notification_tx: kanal::Sender<PlayerNotification>,
    /// Receiver for processor-to-main-thread notifications.
    #[cfg_attr(not(test), expect(dead_code, reason = "used by Task 9 wiring"))]
    pub(crate) notification_rx: kanal::Receiver<PlayerNotification>,
}

impl SharedPlayerState {
    /// Create a new shared state with default values.
    ///
    /// The notification channel is bounded to 32 to avoid dropping
    /// notifications during high activity while keeping memory bounded.
    pub(crate) fn new() -> Self {
        let (tx, rx) = kanal::bounded(32);
        Self {
            playing: AtomicBool::new(false),
            position: AtomicF64::new(0.0),
            duration: AtomicF64::new(0.0),
            sample_rate: AtomicU32::new(0),
            notification_tx: tx,
            notification_rx: rx,
        }
    }

    /// Get the current sample rate, if set.
    #[cfg_attr(not(test), expect(dead_code, reason = "used by Task 9 wiring"))]
    pub(crate) fn sample_rate(&self) -> Option<NonZeroU32> {
        NonZeroU32::new(self.sample_rate.load(Ordering::Relaxed))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shared_state_defaults() {
        let state = SharedPlayerState::new();
        assert!(!state.playing.load(Ordering::Relaxed));
        assert_eq!(state.position.load(Ordering::Relaxed), 0.0);
        assert_eq!(state.duration.load(Ordering::Relaxed), 0.0);
        assert_eq!(state.sample_rate.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn shared_state_sample_rate_none_when_zero() {
        let state = SharedPlayerState::new();
        assert!(state.sample_rate().is_none());
    }

    #[test]
    fn shared_state_sample_rate_some_when_set() {
        let state = SharedPlayerState::new();
        state.sample_rate.store(44100, Ordering::Relaxed);
        let sr = state.sample_rate();
        assert_eq!(sr, NonZeroU32::new(44100));
    }

    #[test]
    fn shared_state_notification_channel_works() {
        use std::sync::Arc;

        let state = SharedPlayerState::new();
        let sent = state
            .notification_tx
            .try_send(PlayerNotification::TrackLoaded(Arc::from("test.mp3")));
        assert!(sent.is_ok());

        let received = state.notification_rx.try_recv();
        assert!(received.is_ok());
        assert!(matches!(
            received.unwrap().unwrap(),
            PlayerNotification::TrackLoaded(src) if &*src == "test.mp3"
        ));
    }

    #[test]
    fn shared_state_position_and_duration_update() {
        let state = SharedPlayerState::new();
        state.position.store(42.5, Ordering::Relaxed);
        state.duration.store(180.0, Ordering::Relaxed);
        assert!((state.position.load(Ordering::Relaxed) - 42.5).abs() < f64::EPSILON);
        assert!((state.duration.load(Ordering::Relaxed) - 180.0).abs() < f64::EPSILON);
    }
}
