#![forbid(unsafe_code)]

use kithara_decode::PcmSpec;
use kithara_platform::time::Duration;

use crate::{SeekEpoch, SeekTaskId};

/// Seek lifecycle stage used for end-to-end diagnostics.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum SeekLifecycleStage {
    SeekRequest,
    SeekApplied,
    DecodeStarted,
    OutputCommitted,
}

/// Events from the audio pipeline.
#[derive(Debug, Clone)]
pub enum AudioEvent {
    /// Audio format detected.
    FormatDetected { spec: PcmSpec },
    /// Audio format changed (ABR switch).
    FormatChanged { old: PcmSpec, new: PcmSpec },
    /// PCM output progress committed by playback sink.
    PlaybackProgress {
        position_ms: u64,
        total_ms: Option<u64>,
        seek_epoch: SeekEpoch,
    },
    /// Seek lifecycle diagnostics.
    SeekLifecycle {
        stage: SeekLifecycleStage,
        seek_epoch: SeekEpoch,
        task_id: SeekTaskId,
        variant: Option<usize>,
        segment_index: Option<u32>,
        byte_range_start: Option<u64>,
        byte_range_end: Option<u64>,
    },
    /// Seek completed.
    SeekComplete {
        position: Duration,
        seek_epoch: SeekEpoch,
    },
    /// Seek abandoned after exhausting retry budget.
    SeekRejected {
        epoch: SeekEpoch,
        target: Duration,
        attempts: u8,
    },
    /// Decoding finished (EOF).
    EndOfStream,
}
