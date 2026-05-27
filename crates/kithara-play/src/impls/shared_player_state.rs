use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use kithara_platform::Mutex;
use portable_atomic::{AtomicF64, AtomicU32};
use ringbuf::{
    HeapCons, HeapProd, HeapRb,
    traits::{Consumer, Producer, Split},
};

use super::{player_notification::PlayerNotification, player_track::PlayerTrack};

/// Shared state that bridges the main thread and the audio processor.
///
/// Position and duration are updated by the processor every render cycle.
/// Notifications flow from the processor to the main thread via a bounded channel.
pub struct SharedPlayerState {
    /// Whether playback is active.
    pub playing: AtomicBool,
    /// Last observed total duration snapshot in seconds.
    ///
    /// Source of truth is the per-track `Timeline` in the audio pipeline.
    pub duration: AtomicF64,
    /// Last observed playback position snapshot in seconds.
    ///
    /// Source of truth is the per-track `Timeline` in the audio pipeline.
    pub position: AtomicF64,
    /// Current sample rate from the audio stream.
    pub sample_rate: AtomicU32,
    /// Diagnostic: how many times `process()` has been called on the audio thread.
    pub process_count: AtomicU64,
    /// Current seek epoch used to invalidate stale seek requests.
    pub seek_epoch: AtomicU64,
    /// Receiver for processor-to-main-thread notifications.
    pub notification_rx: Mutex<HeapCons<PlayerNotification>>,
    /// Sender for processor-to-main-thread notifications.
    pub notification_tx: Mutex<HeapProd<PlayerNotification>>,
    /// Main-thread side of the deferred-drop channel: drained and dropped
    /// off the audio thread.
    pub trash_rx: Mutex<HeapCons<PlayerTrack>>,
    /// Audio-thread side of the deferred-drop channel: evicted tracks are
    /// pushed here instead of being dropped inline.
    pub trash_tx: Mutex<HeapProd<PlayerTrack>>,
}

impl Default for SharedPlayerState {
    fn default() -> Self {
        Self::new()
    }
}

impl SharedPlayerState {
    /// Capacity of the notification ring buffer.
    const NOTIFICATION_RINGBUF_CAPACITY: usize = 32;

    /// Capacity of the deferred-drop ("trash") ring buffer. Evicted tracks own
    /// heap (PCM reader, metadata) that must not be freed on the audio thread;
    /// they are pushed here and dropped by the main thread. Sized well above
    /// the live track count so the non-RT drain keeps it from ever filling.
    const TRASH_RINGBUF_CAPACITY: usize = 64;

    /// Create a new shared state with default values.
    ///
    /// The notification channel is bounded to 32 to avoid dropping
    /// notifications during high activity while keeping memory bounded.
    #[must_use]
    // ast-grep-ignore: style.prefer-default-derive
    pub fn new() -> Self {
        let (tx, rx) =
            HeapRb::<PlayerNotification>::new(Self::NOTIFICATION_RINGBUF_CAPACITY).split();
        let (trash_tx, trash_rx) = HeapRb::<PlayerTrack>::new(Self::TRASH_RINGBUF_CAPACITY).split();
        Self {
            playing: AtomicBool::new(false),
            seek_epoch: AtomicU64::new(0),
            position: AtomicF64::new(0.0),
            duration: AtomicF64::new(0.0),
            sample_rate: AtomicU32::new(0),
            process_count: AtomicU64::new(0),
            notification_tx: Mutex::new(tx),
            notification_rx: Mutex::new(rx),
            trash_tx: Mutex::new(trash_tx),
            trash_rx: Mutex::new(trash_rx),
        }
    }

    /// Hand an evicted track to the deferred-drop channel so its heap is
    /// freed by the main thread, never on the audio thread. If the channel is
    /// full the track drops here as a bounded degraded path.
    pub(crate) fn discard_track(&self, track: PlayerTrack) {
        let _ = self.trash_tx.lock_sync().try_push(track);
    }

    /// Drop every track queued for deferred destruction. Called from the
    /// main-thread notification drain, never from the audio thread.
    pub(crate) fn drain_trash(&self) {
        let mut rx = self.trash_rx.lock_sync();
        while rx.try_pop().is_some() {}
    }

    pub(crate) fn next_seek_epoch(&self) -> u64 {
        self.seek_epoch
            .fetch_add(1, Ordering::AcqRel)
            .wrapping_add(1)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use kithara_test_utils::kithara;

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
    fn shared_state_notification_channel_works() {
        let state = SharedPlayerState::new();
        let sent = state
            .notification_tx
            .lock_sync()
            .try_push(PlayerNotification::Loaded {
                src: Arc::from("a.mp3"),
            });
        assert!(sent.is_ok());

        let received = state.notification_rx.lock_sync().try_pop();
        let Some(PlayerNotification::Loaded { src }) = received else {
            panic!("expected Loaded notification, got {received:?}");
        };
        assert_eq!(&*src, "a.mp3");
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
