#![forbid(unsafe_code)]

/// Events emitted by file streams.
#[derive(Debug, Clone, PartialEq)]
pub enum FileEvent {
    /// Bytes written to local storage by the downloader.
    DownloadProgress { offset: u64, total: Option<u64> },
    /// Download completed successfully.
    DownloadComplete { total_bytes: u64 },
    /// Download failed.
    DownloadError { error: String },
    /// Bytes consumed by the playback reader.
    PlaybackProgress { position: u64, total: Option<u64> },
    /// General error.
    Error { error: String, recoverable: bool },
    /// Stream ended.
    EndOfStream,
}
