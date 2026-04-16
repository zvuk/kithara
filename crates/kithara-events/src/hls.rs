#![forbid(unsafe_code)]

use kithara_abr::{AbrMode, AbrReason, VariantInfo};
use kithara_platform::time::Duration;

use crate::SeekEpoch;

/// Events emitted during HLS playback.
#[derive(Clone, Debug)]
pub enum HlsEvent {
    /// Master playlist loaded and variants discovered.
    VariantsDiscovered {
        variants: Vec<VariantInfo>,
        initial_variant: usize,
    },
    /// ABR mode changed at runtime.
    AbrModeChanged { mode: AbrMode },
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
        cached: bool,
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
    /// Byte-level read progress from HLS source (`read_at`), not sink playback truth.
    ByteProgress { position: u64, total: Option<u64> },
    /// Deprecated alias retained for compatibility with old consumers.
    PlaybackProgress { position: u64, total: Option<u64> },
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
    /// Error occurred.
    Error { error: String, recoverable: bool },
    /// Stream ended.
    EndOfStream,
}
