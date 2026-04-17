#![forbid(unsafe_code)]

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

/// Static PCM format descriptor carried by audio events.
///
/// Duplicates the shape of `kithara_decode::PcmSpec` intentionally:
/// keeping the event crate free of a `kithara-decode` dependency
/// breaks what would otherwise be a
/// `kithara-events → kithara-decode → kithara-stream → kithara-events`
/// cycle once `kithara-stream` starts publishing downloader events via
/// this bus.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct AudioFormat {
    pub channels: u16,
    pub sample_rate: u32,
}

impl AudioFormat {
    #[must_use]
    pub const fn new(channels: u16, sample_rate: u32) -> Self {
        Self {
            channels,
            sample_rate,
        }
    }
}

impl std::fmt::Display for AudioFormat {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} Hz, {} channels", self.sample_rate, self.channels)
    }
}

/// Events from the audio pipeline.
#[derive(Debug, Clone)]
pub enum AudioEvent {
    /// Audio format detected.
    FormatDetected { spec: AudioFormat },
    /// Audio format changed (ABR switch).
    FormatChanged { old: AudioFormat, new: AudioFormat },
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
    /// Decoder initialized or recreated (ABR switch, format boundary, recovery).
    DecoderReady {
        base_offset: u64,
        variant: Option<u32>,
    },
    /// Decoding finished (EOF).
    EndOfStream,
}
