#![forbid(unsafe_code)]

/// Events emitted by file streams.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileEvent {
    /// Bytes written to local storage by the downloader.
    DownloadProgress { offset: u64, total: Option<u64> },
    /// Download completed successfully.
    DownloadComplete { total_bytes: u64 },
    /// Download failed.
    DownloadError { error: String },
    /// Bytes consumed by the file reader (not sink playback truth).
    ByteProgress { position: u64, total: Option<u64> },
    /// Deprecated alias retained for compatibility with old consumers.
    PlaybackProgress { position: u64, total: Option<u64> },
    /// General error.
    Error { error: String, recoverable: bool },
    /// Stream ended.
    EndOfStream,
}
