use std::num::NonZeroU32;

use kithara_bufpool::{HasPool, PoolRegion};
use kithara_platform::sync::Arc;
use kithara_resampler::ResamplerBackend;
use tracing::warn;

use crate::{
    BeatArtifact, BlobError,
    analyzer::{BeatAnalysisConfig, default_beat_detector},
    beat::{
        BeatDetector, BeatPass, BeatPassConfig, DetectOutput, DetectRequest, GridParams, Intake,
    },
    coverage::{Coverage, FrameRange},
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
            BeatPass::new(pass)
        }))
    }

    pub(crate) const fn is_empty(&self) -> bool {
        self.0.is_none()
    }

    pub(crate) fn take_detector<S>(&mut self, pools: &PoolRegion<S>) -> Option<Detector>
    where
        S: HasPool<f32> + Send + Sync + 'static,
    {
        let source = self.0.as_mut()?.detector.clone()?;
        let detector = match source {
            DetectorConfig::Default => default_beat_detector(pools),
            DetectorConfig::Ready(detector) => Some(detector.clone()),
        };
        if let Some(detector) = &detector {
            self.0.as_mut()?.detector = Some(DetectorConfig::Ready(detector.clone()));
        } else {
            self.0 = None;
        }
        detector
    }

    pub(crate) fn set_resampler(&mut self, resampler: BeatAnalysisConfig<B>) {
        if let Some(config) = &mut self.0 {
            config.resampler = resampler;
        }
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
    pub(crate) fn snapshot<S>(
        &mut self,
        pools: &PoolRegion<S>,
        detector: Option<&mut Detector>,
        ending: bool,
        extent: Option<u64>,
    ) -> Option<(BeatArtifact, Vec<FrameRange>)>
    where
        S: HasPool<f32>,
    {
        let analyzer = self.0.as_mut()?;
        match detector {
            Some(detector) => analyzer.snapshot(pools, detector.as_ref(), ending, extent),
            None => analyzer.snapshot_deferred(ending, extent),
        }
    }

    /// Whether the pass took audio it did not have. `opens` says whether this
    /// audio may start a run of its own. A build with no beat pass takes
    /// nothing.
    pub(crate) fn push<S>(
        &mut self,
        pools: &PoolRegion<S>,
        pcm: &[f32],
        channels: usize,
        at: u64,
        opens: bool,
        detector: Option<&mut Detector>,
    ) -> bool
    where
        S: HasPool<f32>,
    {
        let Some(analyzer) = &mut self.0 else {
            return false;
        };
        let took = match detector {
            Some(detector) => analyzer.push(pools, pcm, channels, at, opens, detector.as_ref()),
            None => analyzer.push_deferred(pools, pcm, channels, at, opens),
        };
        self.close_if_failed();
        took
    }

    /// A pass that failed holds audio the pass would wait on and turns down
    /// the rest. Closing the slot releases what it holds, so the pass reads on
    /// without a beat grid rather than waiting on a detector it cannot feed.
    fn close_if_failed(&mut self) {
        if let Some(error) = self.0.as_ref().and_then(BeatPass::failure) {
            warn!(?error, "beat analysis failed; the pass reads on without it");
            self.0 = None;
        }
    }

    /// What the beat pass has taken, which trails what the pass has seen while
    /// the detector is behind. A build with no beat pass is governed by `seen`,
    /// and takes anything.
    pub(crate) fn coverage<'a>(&'a self, seen: &'a Coverage) -> &'a Coverage {
        self.0.as_ref().map_or(seen, BeatPass::coverage)
    }

    pub(crate) fn intake(&self) -> Intake {
        self.0.as_ref().map_or(Intake::Anywhere, BeatPass::intake)
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

    pub(crate) fn apply_detection(&mut self, output: DetectOutput) {
        if let Some(analyzer) = &mut self.0 {
            analyzer.apply_detection(output);
        }
        self.close_if_failed();
    }

    pub(crate) fn write_resume(&mut self) -> Option<Vec<u8>> {
        self.0.as_mut().map(|analyzer| {
            let mut out = Vec::new();
            analyzer.write_resume(&mut out);
            out
        })
    }

    pub(crate) fn restore<S>(
        &mut self,
        pools: &PoolRegion<S>,
        resume: Option<BeatResume>,
    ) -> Result<(), BlobError>
    where
        S: HasPool<f32>,
    {
        match (self.0.as_mut(), resume) {
            (Some(analyzer), Some(resume)) => analyzer.restore(pools, resume),
            (None, None) => Ok(()),
            (Some(_), None) | (None, Some(_)) => Err(BlobError::Corrupt),
        }
    }
}

pub(crate) use crate::beat::{DetectOutput as DetectionOutput, DetectRequest as DetectionRequest};

pub(crate) fn detect(request: DetectionRequest, detector: &Detector) -> DetectionOutput {
    request.detect(detector.as_ref())
}
