use std::{num::NonZeroU32, ops::Range};

use kithara_bufpool::{HasPool, PoolRegion};
use kithara_platform::sync::Arc;
use kithara_resampler::ResamplerBackend;
use rangemap::RangeSet;
use tracing::warn;

use super::{Intake, Opens};
pub(crate) use crate::beat::{DetectOutput, DetectRequest};
use crate::{
    BeatArtifact, BlobError,
    analyzer::{BeatAnalysisConfig, default_beat_detector},
    beat::{BeatAnalyzer, BeatDetector, BeatPassConfig, GridParams, extend_over},
    progress::BeatResume,
};

pub(crate) type Detector = Arc<dyn BeatDetector>;

#[derive(Clone)]
struct BeatConfig<B>
where
    B: ResamplerBackend,
{
    resampler: BeatAnalysisConfig<B>,
    params: GridParams,
    detector: Option<DetectorConfig>,
}

#[derive(Clone)]
enum DetectorConfig {
    Default,
    Ready(Detector),
}

#[derive(Clone)]
#[derive_where::derive_where(Default)]
pub(crate) struct Config<B>(Option<BeatConfig<B>>)
where
    B: ResamplerBackend;

impl<B> Config<B>
where
    B: ResamplerBackend,
{
    pub(crate) fn build<S>(&self, rate: NonZeroU32, pools: &PoolRegion<S>) -> Slot<B>
    where
        S: HasPool<f32>,
    {
        Slot(self.0.as_ref().map(|config| {
            let pass = BeatPassConfig::builder()
                .source_rate(rate.get())
                .params(config.params.clone())
                .resampler(config.resampler.clone())
                .pools(pools.clone())
                .build();
            BeatAnalyzer::new(pass)
        }))
    }

    pub(crate) const fn is_empty(&self) -> bool {
        self.0.is_none()
    }

    pub(crate) fn set_resampler(&mut self, resampler: BeatAnalysisConfig<B>) {
        if let Some(config) = &mut self.0 {
            config.resampler = resampler;
        }
    }

    pub(crate) fn take_detector<S>(&mut self, pools: &PoolRegion<S>) -> Option<Detector>
    where
        S: HasPool<f32> + Send + Sync + 'static,
    {
        let config = self.0.as_ref()?;
        let detector = match config.detector.clone()? {
            DetectorConfig::Default => default_beat_detector(&config.resampler, pools),
            DetectorConfig::Ready(detector) => Some(detector),
        };
        if let Some(detector) = &detector {
            self.0.as_mut()?.detector = Some(DetectorConfig::Ready(detector.clone()));
        } else {
            self.0 = None;
        }
        detector
    }

    pub(crate) fn with_default(&mut self, resampler: BeatAnalysisConfig<B>) {
        self.0 = Some(BeatConfig {
            resampler,
            detector: Some(DetectorConfig::Default),
            params: GridParams::default(),
        });
    }

    #[cfg(test)]
    pub(crate) fn with_detector(
        &mut self,
        detector: Box<dyn BeatDetector>,
        params: GridParams,
        resampler: BeatAnalysisConfig<B>,
    ) {
        self.0 = Some(BeatConfig {
            params,
            resampler,
            detector: Some(DetectorConfig::Ready(Arc::from(detector))),
        });
    }
}

#[derive_where::derive_where(Default)]
pub(crate) struct Slot<B>(Option<BeatAnalyzer<B>>)
where
    B: ResamplerBackend;

impl<B> Slot<B>
where
    B: ResamplerBackend,
{
    pub(crate) fn observe_origin(&mut self, origin: u64) -> bool {
        self.0
            .as_mut()
            .is_none_or(|analyzer| analyzer.observe_origin(origin))
    }

    pub(crate) fn apply_detection(&mut self, output: DetectOutput) {
        if let Some(analyzer) = &mut self.0 {
            analyzer.apply_detection(output);
        }
        self.close_if_failed();
    }

    fn close_if_failed(&mut self) {
        if let Some(error) = self.0.as_ref().and_then(BeatAnalyzer::failure) {
            warn!(?error, "beat analysis failed; the pass reads on without it");
            self.0 = None;
        }
    }

    pub(crate) fn coverage<'a>(&'a self, seen: &'a RangeSet<u64>) -> &'a RangeSet<u64> {
        self.0.as_ref().map_or(seen, BeatAnalyzer::coverage)
    }

    pub(crate) fn intake(&self) -> Intake {
        self.0
            .as_ref()
            .map_or(Intake::Anywhere, BeatAnalyzer::intake)
    }

    pub(crate) fn prepare_detection<S>(
        &mut self,
        pools: &PoolRegion<S>,
        trailing: bool,
    ) -> Option<DetectRequest>
    where
        S: HasPool<f32>,
    {
        let request = self.0.as_mut()?.prepare_detection(pools, trailing);
        self.close_if_failed();
        request
    }

    pub(crate) fn push<S>(
        &mut self,
        pools: &PoolRegion<S>,
        pcm: &[f32],
        channels: usize,
        at: u64,
        opens: Opens,
        detector: Option<&mut Detector>,
    ) -> bool
    where
        S: HasPool<f32>,
    {
        let Some(analyzer) = &mut self.0 else {
            return false;
        };
        let took = match detector {
            Some(detector) => {
                analyzer.push_interleaved(pools, pcm, channels, at, opens, detector.as_ref())
            }
            None => analyzer.push_interleaved_deferred(pools, pcm, channels, at, opens),
        };
        self.close_if_failed();
        took
    }

    pub(crate) fn restore<S>(
        &mut self,
        pools: &PoolRegion<S>,
        resume: Option<BeatResume>,
    ) -> Result<(), BlobError>
    where
        S: HasPool<f32>,
    {
        super::restore(self.0.as_mut(), resume, |analyzer, resume| {
            analyzer.restore(pools, resume)
        })
    }

    pub(crate) fn snapshot<S>(
        &mut self,
        pools: &PoolRegion<S>,
        detector: Option<&mut Detector>,
        ending: bool,
        extent: Option<u64>,
    ) -> Option<(BeatArtifact, Vec<Range<u64>>)>
    where
        S: HasPool<f32>,
    {
        let analyzer = self.0.as_mut()?;
        let grid = match detector {
            Some(detector) => analyzer.snapshot(pools, detector.as_ref(), ending),
            None => analyzer.snapshot_deferred(ending),
        };
        match grid {
            Ok(grid) => {
                let grid = match extent {
                    Some(extent) => extend_over(grid, extent, analyzer.source_rate()),
                    None => grid,
                };
                Some((grid, analyzer.unanalysed(extent)))
            }
            Err(error) => {
                warn!(?error, "beat analysis failed; leaving the beat slot empty");
                None
            }
        }
    }

    pub(crate) fn write_resume(&mut self) -> Option<Vec<u8>> {
        self.0.as_mut().map(|analyzer| {
            let mut out = Vec::new();
            analyzer.write_resume(&mut out);
            out
        })
    }
}

pub(crate) fn detect(request: DetectRequest, detector: &Detector) -> DetectOutput {
    request.detect(detector.as_ref())
}
