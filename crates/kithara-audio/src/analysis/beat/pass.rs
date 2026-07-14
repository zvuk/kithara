use kithara_decode::PcmChunk;
use kithara_resampler::ResamplerBackend;
use tracing::warn;

use super::{
    analyzer::{BeatAnalyzer, BeatPassConfig},
    detector::BeatDetector,
};
use crate::waveform::BeatGrid;

pub(crate) struct BeatPass<B>
where
    B: ResamplerBackend,
{
    analyzer: BeatAnalyzer<B>,
}

impl<B> BeatPass<B>
where
    B: ResamplerBackend,
{
    pub(crate) fn new(config: BeatPassConfig<B>) -> Self {
        Self {
            analyzer: BeatAnalyzer::new(config),
        }
    }

    pub(crate) fn finish(self, detector: &mut dyn BeatDetector) -> Option<BeatGrid> {
        match self.analyzer.finalize(detector) {
            Ok(grid) => Some(grid),
            Err(e) => {
                warn!(?e, "beat analysis failed; leaving the beat slot empty");
                None
            }
        }
    }

    pub(crate) fn push(&mut self, chunk: &PcmChunk, detector: &mut dyn BeatDetector) {
        let channels = usize::from(chunk.spec().channels.max(1));
        self.analyzer
            .push_interleaved(&chunk.samples[..], channels, detector);
    }
}
