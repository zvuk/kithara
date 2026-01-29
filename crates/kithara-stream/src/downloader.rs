#![forbid(unsafe_code)]

//! Downloader trait for background data fetching.

use std::future::Future;

/// Async download feed driven by Backend's task loop.
///
/// Implementations write data to storage and emit events internally.
/// - `FileDownloader` (kithara-file): wraps Writer, progressive HTTP download
/// - `HlsDownloader` (kithara-hls): segment-by-segment download
pub trait Downloader: Send + 'static {
    /// Drive next download step.
    ///
    /// Returns `true` if more data may be available.
    /// Returns `false` when download is complete (EOF/error).
    fn step(&mut self) -> impl Future<Output = bool> + Send;
}

/// No-op downloader for sources that manage downloads internally.
pub struct NoDownload;

impl Downloader for NoDownload {
    async fn step(&mut self) -> bool {
        std::future::pending().await
    }
}
