#[cfg(any(feature = "analysis-beat", feature = "analysis-waveform"))]
use kithara_decode::PcmChunk;

use crate::waveform::{BeatGrid, Waveform};

/// Streaming per-track analyzer contract: fed every decoded chunk once, then
/// consumed to yield its own artifact.
#[cfg(any(feature = "analysis-beat", feature = "analysis-waveform"))]
pub(crate) trait Analyzer: Send {
    /// The artifact this analyzer produces.
    type Output;

    /// End of stream: produce the artifact.
    fn finish(self) -> Self::Output;
    /// Consume one decoded chunk (interleaved PCM with meta).
    fn push(&mut self, chunk: &PcmChunk);
}

/// The output of one analysis pass
#[derive(Clone, Debug, Default)]
#[non_exhaustive]
pub struct TrackAnalysis {
    /// Cleaned beat grid (source frames).
    pub beat: Option<BeatGrid>,
    /// Track waveform.
    pub waveform: Option<Waveform>,
    /// Total decoded source frames: the denominator for `BeatGrid` frame to fraction.
    pub source_frames: u64,
}
