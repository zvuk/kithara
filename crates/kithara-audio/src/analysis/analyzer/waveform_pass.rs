use kithara_decode::PcmChunk;

use super::track_analysis::Analyzer;
#[cfg(not(feature = "analysis-waveform"))]
use crate::waveform::Waveform;
#[cfg(feature = "analysis-waveform")]
use crate::waveform::{AnalysisParams, Waveform, WaveformAnalyzer};

#[cfg(feature = "analysis-waveform")]
pub(crate) struct WaveformPass {
    inner: WaveformAnalyzer,
    buckets: usize,
}

#[cfg(not(feature = "analysis-waveform"))]
pub(crate) struct WaveformPass;

#[cfg(feature = "analysis-waveform")]
impl WaveformPass {
    pub(crate) fn new(sample_rate: u32, buckets: usize) -> Self {
        Self {
            buckets,
            inner: WaveformAnalyzer::new(sample_rate, AnalysisParams::default()),
        }
    }
}

#[cfg(not(feature = "analysis-waveform"))]
impl WaveformPass {
    pub(crate) fn new(_sample_rate: u32, _buckets: usize) -> Self {
        Self
    }
}

#[cfg(feature = "analysis-waveform")]
impl Analyzer for WaveformPass {
    type Output = Waveform;

    fn finish(self) -> Waveform {
        self.inner.finalize(self.buckets)
    }

    fn push(&mut self, chunk: &PcmChunk) {
        let channels = usize::from(chunk.spec().channels.max(1));
        self.inner.push_interleaved(&chunk.samples[..], channels);
    }
}

#[cfg(not(feature = "analysis-waveform"))]
impl Analyzer for WaveformPass {
    type Output = Waveform;

    fn finish(self) -> Waveform {
        Waveform::from(Vec::new())
    }

    fn push(&mut self, _chunk: &PcmChunk) {}
}
