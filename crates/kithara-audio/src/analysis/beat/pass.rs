use kithara_decode::PcmChunk;
use kithara_platform::sync::{Arc, Mutex};
use kithara_resampler::ResamplerBackend;
use tracing::warn;

use super::{
    analyzer::{BeatAnalyzer, BeatPassConfig},
    detector::BeatDetector,
};
use crate::{analysis::analyzer::Analyzer, waveform::BeatGrid};

/// Detector shared across sequential per-track analyzers; model load is
/// paid once at registration, the worker runs one analysis at a time.
pub(crate) type SharedBeatDetector = Arc<Mutex<Box<dyn BeatDetector>>>;

pub(crate) struct BeatPass<B>
where
    B: ResamplerBackend,
{
    analyzer: BeatAnalyzer<B>,
    detector: SharedBeatDetector,
}

impl<B> BeatPass<B>
where
    B: ResamplerBackend,
{
    pub(crate) fn new(config: BeatPassConfig<B>, detector: SharedBeatDetector) -> Self {
        Self {
            detector,
            analyzer: BeatAnalyzer::new(config),
        }
    }
}

impl<B> Analyzer for BeatPass<B>
where
    B: ResamplerBackend,
{
    type Output = Option<BeatGrid>;

    fn finish(self) -> Option<BeatGrid> {
        let mut detector = self.detector.lock();
        match self.analyzer.finalize(detector.as_mut()) {
            Ok(grid) => Some(grid),
            Err(e) => {
                warn!(?e, "beat analysis failed; leaving the beat slot empty");
                None
            }
        }
    }

    fn push(&mut self, chunk: &PcmChunk) {
        let channels = usize::from(chunk.spec().channels.max(1));
        let mut detector = self.detector.lock();
        self.analyzer
            .push_interleaved(&chunk.samples[..], channels, detector.as_mut());
    }
}
