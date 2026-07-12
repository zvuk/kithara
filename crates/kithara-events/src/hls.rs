#![forbid(unsafe_code)]

use crate::SeekEpoch;

/// Errors specific to the HLS stream layer (non-network, non-downloader).
#[derive(Debug, Clone, derive_more::Display, PartialEq, Eq)]
#[non_exhaustive]
pub enum HlsError {
    /// Playlist parse / structure error.
    #[display("playlist: {_0}")]
    Playlist(String),
    /// AES-128 / `FairPlay` decryption failure.
    #[display("decryption: {_0}")]
    Decryption(String),
    /// Codec / container probing or validation failure.
    #[display("codec: {_0}")]
    Codec(String),
    /// Anything else not covered above.
    #[display("other: {_0}")]
    Other(String),
}

/// Events emitted during HLS playback.
///
/// All variants describe **reader-side** facts. For HTTP request
/// lifecycle (enqueue → started → completed/failed/cancelled),
/// subscribe to [`crate::DownloaderEvent`] on the same bus scope.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub enum HlsEvent {
    /// Reader entered a new segment (or first segment after open / seek).
    SegmentReadStart {
        variant: usize,
        segment_index: usize,
        byte_offset: u64,
    },
    /// Reader finished consuming the segment.
    SegmentReadComplete {
        variant: usize,
        segment_index: usize,
        bytes_read: u64,
    },
    /// Reader byte-level progress through the virtual byte stream.
    ReadProgress { position: u64, total: Option<u64> },
    /// Reader byte cursor jumped (driven by the decoder calling
    /// `Seek::seek` after a user-facing seek). Captures both endpoints
    /// of the jump plus the resolved segment-side coordinates so a
    /// subscriber can verify the reader actually moved into the target
    /// region of the stream (and not, e.g., toward the prefix while
    /// it waits for in-flight prefix fetches to drain).
    ReaderSeek {
        from_offset: u64,
        to_offset: u64,
        seek_epoch: SeekEpoch,
        variant: Option<usize>,
        segment_index: Option<usize>,
        byte_in_segment: Option<u64>,
    },
    /// Stale seek request dropped before planning.
    StaleRequestDropped {
        seek_epoch: SeekEpoch,
        current_epoch: SeekEpoch,
        variant: usize,
        segment_index: usize,
    },
    /// Stale fetch result dropped before commit.
    StaleFetchDropped {
        seek_epoch: SeekEpoch,
        current_epoch: SeekEpoch,
        variant: usize,
        segment_index: usize,
    },
    /// Targeted seek diagnostics for debugging index drift.
    Seek {
        stage: &'static str,
        seek_epoch: SeekEpoch,
        variant: usize,
        offset: u64,
        from_segment_index: usize,
        to_segment_index: usize,
    },
    /// HLS-specific error (non-network).
    Error { error: HlsError },
    /// Stream ended (reader hit EOF).
    EndOfStream,
}
