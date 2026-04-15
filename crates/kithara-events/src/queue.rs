//! Queue-level events emitted by `kithara-queue::Queue`.
//!
//! Types live here (rather than in `kithara-queue`) so the root
//! [`Event`](crate::Event) enum can carry a `Queue(QueueEvent)` variant
//! without a circular dependency.

/// Monotonic identifier for a track inside a single `Queue` instance.
///
/// Stable across removals — when a track is removed and a new one
/// appended, the new one gets a fresh id.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TrackId(pub u64);

impl TrackId {
    /// Raw id value.
    #[must_use]
    pub fn as_u64(self) -> u64 {
        self.0
    }
}

/// Loading lifecycle of a track in the queue.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum TrackStatus {
    /// Queued but loading has not started.
    Pending,
    /// Currently loading.
    Loading,
    /// Loading longer than the soft timeout.
    Slow,
    /// Loaded and ready for playback.
    Loaded,
    /// Loading failed. The string renders the underlying error.
    Failed(String),
    /// Consumed by the engine after playback — needs a fresh load before
    /// it can be selected again.
    Consumed,
}

/// Queue-level events emitted by `kithara-queue::Queue`.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub enum QueueEvent {
    /// A new track was appended / inserted at `index`.
    TrackAdded { id: TrackId, index: usize },
    /// A track was removed from the queue.
    TrackRemoved { id: TrackId },
    /// A track's loading status changed.
    TrackStatusChanged { id: TrackId, status: TrackStatus },
    /// The currently playing track changed.
    CurrentTrackChanged { id: Option<TrackId> },
    /// Queue reached the end and is not repeating.
    QueueEnded,
    /// The crossfade duration was updated at runtime.
    CrossfadeDurationChanged { seconds: f32 },
    /// A crossfade between tracks just started. Emitted when
    /// [`Queue::select`](https://docs.rs/kithara-queue) triggers the engine
    /// to fade from a currently-playing track to the newly selected one.
    /// UIs can use `duration_seconds` to drive a progress indicator.
    CrossfadeStarted { duration_seconds: f32 },
}
