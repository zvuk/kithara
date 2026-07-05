use kithara_decode::PcmChunk;

use super::track_analysis::Analyzer;
use crate::waveform::Waveform;

pub(crate) struct WaveformPass;

impl WaveformPass {
    pub(crate) fn new(_sample_rate: u32, _buckets: usize) -> Self {
        Self
    }
}

impl Analyzer for WaveformPass {
    type Output = Waveform;

    fn finish(self) -> Waveform {
        Waveform::from(Vec::new())
    }

    fn push(&mut self, _chunk: &PcmChunk) {}
}
