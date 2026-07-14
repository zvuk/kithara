use kithara_bufpool::PcmPool;
use kithara_decode::{PcmChunk, PcmSpec};
use kithara_resampler::ResamplerBackend;

use crate::{
    analysis::{
        analyzer::{BeatAnalysisConfig, default_beat_detector},
        beat::{BeatDetector, BeatPass, BeatPassConfig, GridParams},
    },
    waveform::BeatGrid,
};

pub(crate) type Detector = Box<dyn BeatDetector>;

struct BeatConfig<B>
where
    B: ResamplerBackend,
{
    detector: Option<Detector>,
    params: GridParams,
    resampler: BeatAnalysisConfig<B>,
}

pub(crate) struct Config<B>(Option<BeatConfig<B>>)
where
    B: ResamplerBackend;

impl<B> Config<B>
where
    B: ResamplerBackend,
{
    pub(crate) fn build(&self, spec: PcmSpec, pcm_pool: &PcmPool) -> Slot<B> {
        Slot(self.0.as_ref().map(|config| {
            let pass = BeatPassConfig::new(
                spec.sample_rate.get(),
                config.params.clone(),
                config.resampler.clone(),
                pcm_pool.clone(),
            );
            BeatPass::new(pass)
        }))
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.0.is_none()
    }

    pub(crate) fn set_resampler(&mut self, resampler: BeatAnalysisConfig<B>) {
        if let Some(config) = &mut self.0 {
            config.resampler = resampler;
        }
    }

    pub(crate) fn with_default(&mut self, resampler: BeatAnalysisConfig<B>) {
        self.0 = default_beat_detector().map(|detector| BeatConfig {
            detector: Some(detector),
            params: GridParams::default(),
            resampler,
        });
    }

    #[cfg(test)]
    pub(crate) fn with_detector(
        &mut self,
        detector: Detector,
        params: GridParams,
        resampler: BeatAnalysisConfig<B>,
    ) {
        self.0 = Some(BeatConfig {
            detector: Some(detector),
            params,
            resampler,
        });
    }

    pub(crate) fn take_detector(&mut self) -> Option<Detector> {
        self.0.as_mut().and_then(|config| config.detector.take())
    }
}

impl<B> Default for Config<B>
where
    B: ResamplerBackend,
{
    fn default() -> Self {
        Self(None)
    }
}

pub(crate) struct Slot<B>(Option<BeatPass<B>>)
where
    B: ResamplerBackend;

impl<B> Default for Slot<B>
where
    B: ResamplerBackend,
{
    fn default() -> Self {
        Self(None)
    }
}

impl<B> Slot<B>
where
    B: ResamplerBackend,
{
    pub(crate) fn finish(self, detector: Option<&mut Detector>) -> Option<BeatGrid> {
        self.0
            .zip(detector)
            .and_then(|(analyzer, detector)| analyzer.finish(detector.as_mut()))
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.0.is_none()
    }

    pub(crate) fn push(&mut self, chunk: &PcmChunk, detector: Option<&mut Detector>) {
        if let (Some(analyzer), Some(detector)) = (&mut self.0, detector) {
            analyzer.push(chunk, detector.as_mut());
        }
    }
}
