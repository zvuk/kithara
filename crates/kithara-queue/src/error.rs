use kithara_events::TrackId;
use kithara_play::PlayError;

/// Errors from the [`Queue`](crate::Queue) orchestration layer.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum QueueError {
    /// Source URL failed to parse as a URL or absolute file path.
    #[error("invalid URL or path: {0}")]
    InvalidUrl(String),

    /// The given [`TrackId`] is not present in the queue.
    #[error("unknown track id: {0:?}")]
    UnknownTrackId(TrackId),

    /// Operation attempted on a track that has not finished loading.
    #[error("track not ready: {0:?}")]
    NotReady(TrackId),

    /// Error bubbled up from `kithara-play`.
    #[error(transparent)]
    Play(#[from] PlayError),

    /// Resource construction failed (decoding, config, or I/O).
    #[error("resource error: {0}")]
    Resource(String),
}
