use kithara_bufpool::PcmPool;
use kithara_decode::{PcmChunk, PcmSpec};
use kithara_resampler::ResamplerBackend;

use crate::{
    analysis::{
        analyzer::{BeatAnalysisConfig, default_beat_detector},
        beat::{BeatDetector, BeatPass, BeatPassConfig, GridParams, GridPool},
    },
    waveform::BeatGrid,
};

pub(crate) type Detector = Box<dyn BeatDetector>;

struct BeatConfig<B>
where
    B: ResamplerBackend,
{
    resampler: BeatAnalysisConfig<B>,
    params: GridParams,
    detector: Option<Detector>,
}

pub(crate) struct Config<B>
where
    B: ResamplerBackend,
{
    beat: Option<BeatConfig<B>>,
    grid_pool: GridPool,
}

impl<B> Config<B>
where
    B: ResamplerBackend,
{
    pub(crate) fn build(&self, spec: PcmSpec, pcm_pool: &PcmPool) -> Slot<B> {
        Slot(self.beat.as_ref().map(|config| {
            let pass = BeatPassConfig::builder()
                .source_rate(spec.sample_rate.get())
                .params(config.params.clone())
                .resampler(config.resampler.clone())
                .grid_pool(self.grid_pool.clone())
                .pcm_pool(pcm_pool.clone())
                .build();
            BeatPass::new(pass)
        }))
    }

    delegate::delegate! {
        to self.beat {
            #[call(is_none)]
            pub(crate) const fn is_empty(&self) -> bool;
            #[expr($.and_then(|config| config.detector.take()))]
            #[call(as_mut)]
            pub(crate) fn take_detector(&mut self) -> Option<Detector>;
        }
    }

    pub(crate) fn set_resampler(&mut self, resampler: BeatAnalysisConfig<B>) {
        if let Some(config) = &mut self.beat {
            config.resampler = resampler;
        }
    }

    pub(crate) fn with_default(&mut self, resampler: BeatAnalysisConfig<B>) {
        self.beat = default_beat_detector().map(|detector| BeatConfig {
            resampler,
            detector: Some(detector),
            params: GridParams::default(),
        });
    }

    #[cfg(test)]
    pub(crate) fn with_detector(
        &mut self,
        detector: Detector,
        params: GridParams,
        resampler: BeatAnalysisConfig<B>,
    ) {
        self.beat = Some(BeatConfig {
            params,
            resampler,
            detector: Some(detector),
        });
    }
}

impl<B> Default for Config<B>
where
    B: ResamplerBackend,
{
    fn default() -> Self {
        Self {
            beat: None,
            grid_pool: GridPool::default(),
        }
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

    pub(crate) const fn is_empty(&self) -> bool {
        self.0.is_none()
    }

    pub(crate) fn push(&mut self, chunk: &PcmChunk, detector: Option<&mut Detector>) {
        if let (Some(analyzer), Some(detector)) = (&mut self.0, detector) {
            analyzer.push(chunk, detector.as_mut());
        }
    }
}
