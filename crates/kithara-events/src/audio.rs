#![forbid(unsafe_code)]

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
