use std::num::NonZeroU32;

#[cfg(any(feature = "analysis-beat", feature = "analysis-waveform"))]
use kithara_decode::PcmChunk;

use crate::waveform::{BeatGrid, bucket::Waveform};

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

/// The output of one analysis pass.
#[derive(Clone, Debug, Default)]
#[non_exhaustive]
pub struct TrackAnalysis {
    /// Cleaned beat grid (source frames).
    beat: Option<BeatGrid>,
    /// Track waveform.
    waveform: Option<Waveform>,
    /// Total decoded source frames: the denominator for `BeatGrid` frame to fraction.
    source_frames: u64,
    /// Sample-rate axis of `source_frames` and the beat-grid markers.
    source_sample_rate: Option<NonZeroU32>,
}

impl TrackAnalysis {
    /// Creates a snapshot without a decoded-source sample-rate axis.
    ///
    /// Use [`Self::with_source_rate`] when beat markers must form a
    /// [`crate::TrackBeatMap`].
    #[must_use]
    pub fn new(beat: Option<BeatGrid>, waveform: Option<Waveform>, source_frames: u64) -> Self {
        Self {
            beat,
            waveform,
            source_frames,
            source_sample_rate: None,
        }
    }

    /// Creates an analysis snapshot with its decoded-source sample-rate axis.
    #[must_use]
    pub fn with_source_rate(
        beat: Option<BeatGrid>,
        waveform: Option<Waveform>,
        source_frames: u64,
        source_sample_rate: NonZeroU32,
    ) -> Self {
        Self {
            beat,
            waveform,
            source_frames,
            source_sample_rate: Some(source_sample_rate),
        }
    }

    #[must_use]
    pub fn beat(&self) -> Option<&BeatGrid> {
        self.beat.as_ref()
    }

    #[must_use]
    pub fn source_frames(&self) -> u64 {
        self.source_frames
    }

    /// Returns the sample-rate axis shared by source frames and beat markers.
    #[must_use]
    pub fn source_sample_rate(&self) -> Option<NonZeroU32> {
        self.source_sample_rate
    }

    #[must_use]
    pub fn waveform(&self) -> Option<&Waveform> {
        self.waveform.as_ref()
    }
}
