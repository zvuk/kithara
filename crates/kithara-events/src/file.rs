#![forbid(unsafe_code)]

//! Events emitted by file streams.
//!
//! Reader-side: factual download lifecycle (HTTP request, body, errors)
//! lives in [`crate::DownloaderEvent`]. This module owns reader/source-
//! level facts: byte-progress through the stream, terminal EOF, and
//! non-network errors specific to file processing.

/// Errors specific to the file stream layer (non-network, non-downloader).
///
/// Network errors are reported by [`crate::DownloaderEvent::RequestFailed`]
/// with a typed [`kithara_net::NetError`].
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum FileError {
    /// Local I/O / storage failure (mmap, write, eviction).
    Io(String),
    /// Decoder-side complaint surfaced through the file stream.
    Decode(String),
    /// Anything else not covered above.
    Other(String),
}

impl FileError {
    /// Whether the consumer can reasonably retry the request.
    #[must_use]
    pub const fn is_recoverable(&self) -> bool {
        matches!(self, Self::Io(_))
    }
}

impl std::fmt::Display for FileError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(msg) => write!(f, "io: {msg}"),
            Self::Decode(msg) => write!(f, "decode: {msg}"),
            Self::Other(msg) => write!(f, "other: {msg}"),
        }
    }
}

/// Events emitted by file streams.
///
/// All variants describe **reader-side** facts. For HTTP request
/// lifecycle (enqueue → started → completed/failed/cancelled),
/// subscribe to [`crate::DownloaderEvent`] on the same bus scope.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum FileEvent {
    /// Reader progressed through the stream — bytes consumed by the
    /// reader, not bytes written to disk and not bytes played by the
    /// sink. Sink-truth lives in `AudioEvent::PlaybackProgress`.
    ReadProgress { position: u64, total: Option<u64> },
    /// Reader byte cursor jumped (driven by the decoder calling
    /// `Seek::seek` after a user-facing seek).
    ReaderSeek {
        from_offset: u64,
        to_offset: u64,
        seek_epoch: crate::SeekEpoch,
    },
    /// Non-network error specific to the file stream.
    Error { error: FileError },
    /// Reader reached EOF.
    EndOfStream,
}
