use crate::track::{TrackId, TrackStatus};

/// Queue-level events emitted by [`Queue`](crate::Queue).
///
/// Published on the shared event bus in addition to the player / audio / hls
/// events from `kithara-play`. Wired to `kithara-events::Event::Queue` in
/// C.2b (see plan `.docs/plans/2026-04-15-kithara-queue-crate.md`); until
/// then [`QueueEvent`] is emitted via a Queue-owned channel and exposed
/// through [`Queue::subscribe`](crate::Queue::subscribe).
#[derive(Clone, Debug)]
#[non_exhaustive]
pub enum QueueEvent {
    /// A new track was appended / inserted.
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
}
