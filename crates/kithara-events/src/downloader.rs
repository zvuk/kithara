#![forbid(unsafe_code)]

/// Events emitted by the unified downloader layer.
///
/// Downloader-owned signals (as opposed to protocol-specific events like
/// `HlsEvent` / `FileEvent`). Published to the
/// [`EventBus`](crate::EventBus) that the protocol peer handed to the
/// downloader at registration time, so each track's subscribers only see
/// events for fetches belonging to that track.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum DownloaderEvent {
    /// Soft timeout fired — the current HTTP request is taking longer
    /// than `DownloaderConfig::soft_timeout`. The request is still
    /// running; this is informational so UIs can flag the track as
    /// "slow".
    LoadSlow,
}
