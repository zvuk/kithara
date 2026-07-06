use kithara_decode::{PcmChunk, PcmSpec};
use num_traits::cast::AsPrimitive;

use super::{
    WaveformPass,
    track_analysis::{Analyzer, TrackAnalysis},
};

#[must_use]
pub fn beat_cache_tag() -> Option<String> {
    super::nn::tag()
}

#[derive(Default)]
pub(crate) struct TrackAnalyzers {
    waveform: Option<WaveformPass>,
    source_frames: u64,
}

impl TrackAnalyzers {
    pub(crate) fn finish_staged<F: FnMut(TrackAnalysis)>(mut self, mut emit: F) {
        emit(TrackAnalysis {
            waveform: self.waveform.take().map(Analyzer::finish),
            source_frames: self.source_frames,
            beat: None,
        });
    }

    pub(crate) fn push(&mut self, chunk: &PcmChunk) {
        let frames: u64 = chunk.frames().as_();
        self.source_frames = self.source_frames.saturating_add(frames);

        if let Some(a) = &mut self.waveform {
            a.push(chunk);
        }
    }
}

#[derive(Default)]
pub struct AnalyzerBuilder {
    waveform_buckets: Option<usize>,
}

impl AnalyzerBuilder {
    pub(crate) fn build(&self, spec: PcmSpec) -> TrackAnalyzers {
        TrackAnalyzers {
            waveform: self
                .waveform_buckets
                .map(|buckets| WaveformPass::new(spec.sample_rate.get(), buckets)),
            source_frames: 0,
        }
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.waveform_buckets.is_none()
    }

    #[must_use]
    pub fn with_beat(self) -> Self {
        self
    }

    #[must_use]
    #[cfg(feature = "analysis-waveform")]
    pub fn with_waveform(self, buckets: usize) -> Self {
        Self {
            waveform_buckets: Some(buckets),
        }
    }

    #[must_use]
    #[cfg(not(feature = "analysis-waveform"))]
    pub fn with_waveform(self, _buckets: usize) -> Self {
        self
    }
}
