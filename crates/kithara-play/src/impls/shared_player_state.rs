//! Shared atomic state between the main thread and the audio-thread processor.
//!
//! [`SharedPlayerState`] is wrapped in `Arc` and shared between
//! `PlayerNode` (main thread) and `PlayerNodeProcessor` (audio thread).
//! All fields use atomic operations or lock-free channels for safe
//! concurrent access.

use std::{
    num::NonZeroU32,
    sync::atomic::{AtomicBool, AtomicU64, Ordering},
};

use kithara_platform::Mutex;
use portable_atomic::{AtomicF64, AtomicU32};
use ringbuf::{HeapCons, HeapProd, HeapRb, traits::Split};

use super::player_notification::PlayerNotification;

/// Shared state that bridges the main thread and the audio processor.
///
/// Position and duration are updated by the processor every render cycle.
/// Notifications flow from the processor to the main thread via a bounded channel.
pub(crate) struct SharedPlayerState {
    /// Whether playback is active.
    pub(crate) playing: AtomicBool,
    /// Current seek epoch used to invalidate stale seek requests.
    pub(crate) seek_epoch: AtomicU64,
    /// Last observed playback position snapshot in seconds.
    ///
    /// Source of truth is the per-track `Timeline` in the audio pipeline.
    pub(crate) position: AtomicF64,
    /// Last observed total duration snapshot in seconds.
    ///
    /// Source of truth is the per-track `Timeline` in the audio pipeline.
    pub(crate) duration: AtomicF64,
    /// Current sample rate from the audio stream.
    pub(crate) sample_rate: AtomicU32,
    /// Diagnostic: how many times `process()` has been called on the audio thread.
    pub(crate) process_count: AtomicU64,
    /// Sender for processor-to-main-thread notifications.
    pub(crate) notification_tx: Mutex<HeapProd<PlayerNotification>>,
    /// Receiver for processor-to-main-thread notifications.
    pub(crate) notification_rx: Mutex<HeapCons<PlayerNotification>>,
}

impl SharedPlayerState {
    /// Create a new shared state with default values.
    ///
    /// The notification channel is bounded to 32 to avoid dropping
    /// notifications during high activity while keeping memory bounded.
    pub(crate) fn new() -> Self {
        let (tx, rx) = HeapRb::<PlayerNotification>::new(32).split();
        Self {
            playing: AtomicBool::new(false),
            seek_epoch: AtomicU64::new(0),
            position: AtomicF64::new(0.0),
            duration: AtomicF64::new(0.0),
            sample_rate: AtomicU32::new(0),
            process_count: AtomicU64::new(0),
            notification_tx: Mutex::new(tx),
            notification_rx: Mutex::new(rx),
        }
    }

    /// Get the current sample rate, if set.
    #[cfg_attr(not(test), expect(dead_code, reason = "used by Task 9 wiring"))]
    pub(crate) fn sample_rate(&self) -> Option<NonZeroU32> {
        NonZeroU32::new(self.sample_rate.load(Ordering::Relaxed))
    }

    pub(crate) fn next_seek_epoch(&self) -> u64 {
        self.seek_epoch
            .fetch_add(1, Ordering::AcqRel)
            .wrapping_add(1)
    }
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;
    use ringbuf::traits::{Consumer, Producer};

    use super::*;

    #[kithara::test]
    fn shared_state_defaults() {
        let state = SharedPlayerState::new();
        assert!(!state.playing.load(Ordering::Relaxed));
        assert_eq!(state.seek_epoch.load(Ordering::Relaxed), 0);
        assert_eq!(state.position.load(Ordering::Relaxed), 0.0);
        assert_eq!(state.duration.load(Ordering::Relaxed), 0.0);
        assert_eq!(state.sample_rate.load(Ordering::Relaxed), 0);
    }

    #[kithara::test]
    fn shared_state_seek_epoch_increments() {
        let state = SharedPlayerState::new();
        assert_eq!(state.next_seek_epoch(), 1);
        assert_eq!(state.next_seek_epoch(), 2);
        assert_eq!(state.next_seek_epoch(), 3);
    }

    #[kithara::test]
    #[case(0, None)]
    #[case(44_100, Some(44_100))]
    #[case(48_000, Some(48_000))]
    fn shared_state_sample_rate_view(#[case] raw_sample_rate: u32, #[case] expected: Option<u32>) {
        let state = SharedPlayerState::new();
        state.sample_rate.store(raw_sample_rate, Ordering::Relaxed);
        assert_eq!(state.sample_rate().map(NonZeroU32::get), expected);
    }

    #[kithara::test]
    fn shared_state_notification_channel_works() {
        use std::sync::Arc;

        let state = SharedPlayerState::new();
        let sent = state
            .notification_tx
            .lock_sync()
            .try_push(PlayerNotification::TrackLoaded(Arc::from("test.mp3")));
        assert!(sent.is_ok());

        let received = state.notification_rx.lock_sync().try_pop();
        assert!(received.is_some());
        assert!(matches!(
            received.unwrap(),
            PlayerNotification::TrackLoaded(src) if &*src == "test.mp3"
        ));
    }

    #[kithara::test]
    fn shared_state_position_and_duration_update() {
        let state = SharedPlayerState::new();
        state.position.store(42.5, Ordering::Relaxed);
        state.duration.store(180.0, Ordering::Relaxed);
        assert!((state.position.load(Ordering::Relaxed) - 42.5).abs() < f64::EPSILON);
        assert!((state.duration.load(Ordering::Relaxed) - 180.0).abs() < f64::EPSILON);
    }
}
