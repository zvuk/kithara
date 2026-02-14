#![forbid(unsafe_code)]

use std::time::Duration;

use kithara_abr::{AbrReason, VariantInfo};

/// Events emitted during HLS playback.
#[derive(Clone, Debug)]
pub enum HlsEvent {
    /// Master playlist loaded and variants discovered.
    VariantsDiscovered {
        variants: Vec<VariantInfo>,
        initial_variant: usize,
    },
    /// Variant (quality level) changed.
    VariantApplied {
        from_variant: usize,
        to_variant: usize,
        reason: AbrReason,
    },
    /// Segment download started.
    SegmentStart {
        variant: usize,
        segment_index: usize,
        byte_offset: u64,
    },
    /// Segment download completed.
    SegmentComplete {
        variant: usize,
        segment_index: usize,
        bytes_transferred: u64,
        duration: Duration,
    },
    /// Throughput measurement.
    ThroughputSample { bytes_per_second: f64 },
    /// Cumulative download progress.
    DownloadProgress { offset: u64, total: Option<u64> },
    /// Download completed successfully.
    DownloadComplete { total_bytes: u64 },
    /// Download failed.
    DownloadError { error: String },
    /// Playback progress update.
    PlaybackProgress { position: u64, total: Option<u64> },
    /// Error occurred.
    Error { error: String, recoverable: bool },
    /// Stream ended.
    EndOfStream,
}
