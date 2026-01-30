#![forbid(unsafe_code)]

//! Audio pipeline events for monitoring.

use std::time::Duration;

use kithara_decode::PcmSpec;

/// Events from the audio pipeline.
#[derive(Debug, Clone)]
pub enum AudioEvent {
    /// Audio format detected.
    FormatDetected { spec: PcmSpec },
    /// Audio format changed (ABR switch).
    FormatChanged { old: PcmSpec, new: PcmSpec },
    /// Seek completed.
    SeekComplete { position: Duration },
    /// Decoding finished (EOF).
    EndOfStream,
}

/// Unified audio pipeline event stream.
///
/// `E` is the stream event type (`HlsEvent`, `FileEvent`).
#[derive(Debug, Clone)]
pub enum AudioPipelineEvent<E> {
    /// Event from the underlying stream.
    Stream(E),
    /// Event from the audio pipeline.
    Audio(AudioEvent),
}
