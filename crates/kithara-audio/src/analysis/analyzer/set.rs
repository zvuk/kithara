use std::sync::Arc;

use kithara_decode::{PcmChunk, PcmSpec};

use super::{
    track_analysis::{Analyzer, TrackAnalysis},
    waveform_pass::WaveformPass,
};
use crate::analysis::beat::{BeatPass, GridParams, SharedBeatDetector};

/// Cache fingerprint of the active beat-analysis configuration (detector
/// kind, model, grid tuning); folding it into the analysis cache key makes a
/// config change a cache miss. `None` when no beat detector is compiled in.
#[must_use]
pub fn beat_cache_tag() -> Option<String> {
    super::nn::tag()
}

#[derive(Default)]
pub(crate) struct TrackAnalyzers {
    beat: Option<BeatPass>,
    waveform: Option<WaveformPass>,
}

impl TrackAnalyzers {
    pub(crate) fn finish(self) -> TrackAnalysis {
        TrackAnalysis {
            waveform: self.waveform.map(Analyzer::finish),
            beat: self.beat.and_then(Analyzer::finish),
        }
    }

    pub(crate) fn push(&mut self, chunk: &PcmChunk) {
        if let Some(a) = &mut self.waveform {
            a.push(chunk);
        }

        if let Some(a) = &mut self.beat {
            a.push(chunk);
        }
    }
}

/// Configures which analyzers run, then mints a fresh [`TrackAnalyzers`]
/// per track at the source spec. Cfg-free for callers: a beat backend
/// absent at compile time is silently ignored.
#[derive(Default)]
pub struct AnalyzerBuilder {
    beat: Option<(SharedBeatDetector, GridParams)>,
    waveform_buckets: Option<usize>,
}

impl AnalyzerBuilder {
    pub(crate) fn build(&self, spec: PcmSpec) -> TrackAnalyzers {
        TrackAnalyzers {
            waveform: self
                .waveform_buckets
                .map(|buckets| WaveformPass::new(spec.sample_rate.get(), buckets)),
            beat: self.beat.as_ref().map(|(detector, params)| {
                BeatPass::new(spec.sample_rate.get(), params.clone(), Arc::clone(detector))
            }),
        }
    }

    /// `true` when no analyzer would run — the runtime signal to skip the
    /// decode pass entirely.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.waveform_buckets.is_none() && self.beat.is_none()
    }

    /// Run the NN beat analyzer. No-op when no detector is compiled in or
    /// its model fails to load (`beat-nn`).
    #[must_use]
    pub fn with_beat(self) -> Self {
        #[cfg(feature = "beat-nn")]
        {
            Self {
                beat: super::nn::detector().map(|d| (d, GridParams::default())),
                ..self
            }
        }
        #[cfg(not(feature = "beat-nn"))]
        {
            self
        }
    }

    /// Test seam: install a mock detector without loading the NN model.
    #[cfg(test)]
    pub(crate) fn with_beat_detector(
        mut self,
        detector: SharedBeatDetector,
        params: GridParams,
    ) -> Self {
        self.beat = Some((detector, params));
        self
    }

    /// Run a waveform analyzer at `buckets` resolution.
    #[must_use]
    pub fn with_waveform(self, buckets: usize) -> Self {
        Self {
            waveform_buckets: Some(buckets),
            ..self
        }
    }
}
