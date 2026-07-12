use crate::TrackId;

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
    /// In-flight load was overridden by a later [`Queue::select`] of a
    /// different track. The slot is intentionally left unpopulated so
    /// auto-advance does not flip onto a track the user explicitly
    /// walked away from. An explicit `select(id)` from the user
    /// re-engages this state and triggers a fresh load.
    Cancelled,
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
