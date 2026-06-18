use kithara_decode::PcmChunk;

use crate::waveform::{BeatGrid, Waveform};

/// Streaming per-track analyzer contract: fed every decoded chunk once, then
/// consumed to yield its own artifact.
pub(crate) trait Analyzer: Send {
    /// The artifact this analyzer produces.
    type Output;

    /// Consume one decoded chunk (interleaved PCM with meta).
    fn push(&mut self, chunk: &PcmChunk);
    /// End of stream: produce the artifact.
    fn finish(self) -> Self::Output;
}

/// The output of one analysis pass
#[derive(Clone, Debug, Default)]
#[non_exhaustive]
pub struct TrackAnalysis {
    /// Track waveform.
    pub waveform: Option<Waveform>,
    /// Cleaned beat grid (source frames).
    pub beat: Option<BeatGrid>,
    /// Total decoded source frames: the denominator for `BeatGrid` frame → fraction.
    pub source_frames: u64,
}
