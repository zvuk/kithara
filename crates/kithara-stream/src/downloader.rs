#![forbid(unsafe_code)]

//! Downloader trait for background data fetching.

use async_trait::async_trait;

/// Async download feed driven by Backend's select! loop.
///
/// Implementations write data to storage and emit events internally.
/// - `FileDownloader` (kithara-file): wraps Writer, progressive HTTP download
/// - `HlsDownloader` (kithara-hls): segment-by-segment download
#[async_trait]
pub trait Downloader: Send + 'static {
    /// Drive next download step.
    ///
    /// Returns `true` if more data may be available.
    /// Returns `false` when download is complete (EOF/error).
    async fn step(&mut self) -> bool;
}

/// No-op downloader for sources that manage downloads internally.
pub struct NoDownload;

#[async_trait]
impl Downloader for NoDownload {
    async fn step(&mut self) -> bool {
        std::future::pending().await
    }
}
