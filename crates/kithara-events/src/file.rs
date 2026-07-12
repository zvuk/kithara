#![forbid(unsafe_code)]

/// Errors specific to the file stream layer (non-network, non-downloader).
///
/// Network errors are reported by [`crate::DownloaderEvent::RequestFailed`]
/// with a typed [`kithara_net::NetError`].
#[derive(Debug, Clone, derive_more::Display, PartialEq, Eq)]
#[non_exhaustive]
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
