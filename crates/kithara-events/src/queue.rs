/// Monotonic identifier for a track across the entire process.
///
/// Allocated from a single global counter so [`Queue`](crate::queue) and
/// FFI items share one address space — the value `audioId` reports
/// over the FFI boundary is exactly the value the queue uses
/// internally. Stable across removals: removing a track and adding a
/// new one yields a fresh id.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TrackId(pub u64);

impl core::fmt::Display for TrackId {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        self.0.fmt(f)
    }
}

impl From<u64> for TrackId {
    fn from(value: u64) -> Self {
        Self(value)
    }
}

impl From<TrackId> for u64 {
    fn from(id: TrackId) -> Self {
        id.0
    }
}

impl TrackId {
    /// Allocate the next monotonic id from the process-wide counter.
    ///
    /// This is the single allocation site: the FFI item layer reserves
    /// an id at construction so caller-visible `audioId` is stable from
    /// day one, and `Queue::insert` consumes that same id without
    /// re-allocating. The counter starts at `0` and is never reset.
    #[must_use]
    pub fn allocate() -> Self {
        use core::sync::atomic::{AtomicU64, Ordering};
        static NEXT: AtomicU64 = AtomicU64::new(0);
        Self(NEXT.fetch_add(1, Ordering::Relaxed))
    }

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
