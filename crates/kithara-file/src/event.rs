#![forbid(unsafe_code)]

use kithara_events::Event;
use kithara_stream::{AudioCodec, ContainerFormat};

/// Errors specific to the file stream layer (non-network, non-downloader).
///
/// Network errors are reported by `DownloaderEvent::RequestFailed`
/// with a typed `NetError`.
#[derive(Debug, Clone, derive_more::Display, PartialEq, Eq)]
pub enum FileError {
    /// Local I/O / storage failure (mmap, write, eviction).
    #[display("io: {_0}")]
    Io(String),
    /// Decoder-side complaint surfaced through the file stream.
    #[display("decode: {_0}")]
    Decode(String),
    /// Anything else not covered above.
    #[display("other: {_0}")]
    Other(String),
}

/// Events emitted by file streams.
///
/// All variants describe **reader-side** facts. For HTTP request
/// lifecycle (enqueue → started → completed/failed/cancelled),
/// subscribe to `DownloaderEvent` on the same bus scope.
#[derive(Debug, Clone, PartialEq, Eq, Event)]
pub enum FileEvent {
    Opened {
        codec: Option<AudioCodec>,
        container: Option<ContainerFormat>,
        total_bytes: Option<u64>,
        cached: bool,
    },
    TotalBytesResolved {
        total_bytes: u64,
        source: TotalBytesSource,
    },
    CacheComplete {
        total_bytes: u64,
    },
    /// Reader progressed through the stream — bytes consumed by the
    /// reader, not bytes written to disk and not bytes played by the
    /// sink. Sink-truth lives in `AudioEvent::PlaybackProgress`.
    ReadProgress {
        position: u64,
        total: Option<u64>,
    },
    /// Reader byte cursor jumped (driven by the decoder calling
    /// `Seek::seek` after a user-facing seek).
    ReaderSeek {
        from_offset: u64,
        to_offset: u64,
    },
    /// Non-network error specific to the file stream.
    Error {
        error: FileError,
    },
    /// Reader reached EOF.
    EndOfStream,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TotalBytesSource {
    CommittedLen,
    ContentLength,
}
