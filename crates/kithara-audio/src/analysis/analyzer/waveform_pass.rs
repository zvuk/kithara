use kithara_decode::PcmChunk;

use super::track_analysis::Analyzer;
use crate::waveform::{AnalysisParams, Waveform, WaveformAnalyzer};

pub(crate) struct WaveformPass {
    inner: WaveformAnalyzer,
    buckets: usize,
}

impl WaveformPass {
    pub(crate) fn new(sample_rate: u32, buckets: usize) -> Self {
        Self {
            buckets,
            inner: WaveformAnalyzer::new(sample_rate, AnalysisParams::default()),
        }
    }
}

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
