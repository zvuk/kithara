use kithara_bufpool::{HasPool, PoolRegion};
use kithara_resampler::ResamplerBackend;
use tracing::warn;

use super::{
    analyzer::{BeatAnalyzer, BeatPassConfig, DetectOutput, DetectRequest},
    detector::{BeatDetectError, BeatDetector},
    grid::extend_over,
    runs::Intake,
};
use crate::{
    BeatArtifact, BlobError,
    coverage::{Coverage, FrameRange},
    progress::BeatResume,
};

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
    pub(crate) fn new<S>(config: BeatPassConfig<B, S>) -> Self
    where
        S: HasPool<f32>,
    {
        Self {
            analyzer: BeatAnalyzer::new(config),
        }
    }

    delegate::delegate! {
        to self.analyzer {
            pub(crate) fn coverage(&self) -> &Coverage;
            pub(crate) fn intake(&self) -> Intake;
            pub(crate) fn failure(&self) -> Option<&BeatDetectError>;
            pub(crate) fn apply_detection(&mut self, output: DetectOutput);
            pub(crate) fn write_resume(&mut self, out: &mut Vec<u8>);
        }
    }

    pub(crate) fn push<S>(
        &mut self,
        pools: &PoolRegion<S>,
        pcm: &[f32],
        channels: usize,
        at: u64,
        opens: bool,
        detector: &dyn BeatDetector,
    ) -> bool
    where
        S: HasPool<f32>,
    {
        self.analyzer
            .push_interleaved(pools, pcm, channels, at, opens, detector)
    }

    pub(crate) fn push_deferred<S>(
        &mut self,
        pools: &PoolRegion<S>,
        pcm: &[f32],
        channels: usize,
        at: u64,
        opens: bool,
    ) -> bool
    where
        S: HasPool<f32>,
    {
        self.analyzer
            .push_interleaved_deferred(pools, pcm, channels, at, opens)
    }

    pub(crate) fn prepare_detection<S>(
        &mut self,
        pools: &PoolRegion<S>,
        trailing: bool,
    ) -> Option<DetectRequest>
    where
        S: HasPool<f32>,
    {
        self.analyzer.prepare_detection(pools, trailing)
    }

    pub(crate) fn restore<S>(
        &mut self,
        pools: &PoolRegion<S>,
        resume: BeatResume,
    ) -> Result<(), BlobError>
    where
        S: HasPool<f32>,
    {
        self.analyzer.restore(pools, resume)
    }

    pub(crate) fn snapshot<S>(
        &mut self,
        pools: &PoolRegion<S>,
        detector: &dyn BeatDetector,
        ending: bool,
        extent: Option<u64>,
    ) -> Option<(BeatArtifact, Vec<FrameRange>)>
    where
        S: HasPool<f32>,
    {
        match self.analyzer.snapshot(pools, detector, ending) {
            Ok(grid) => {
                let rate = self.analyzer.source_rate();
                let grid = match extent {
                    Some(extent) => extend_over(grid, extent, rate),
                    None => grid,
                };
                Some((grid, self.analyzer.unanalysed(extent)))
            }
            Err(e) => {
                warn!(?e, "beat analysis failed; leaving the beat slot empty");
                None
            }
        }
    }

    pub(crate) fn snapshot_deferred(
        &mut self,
        ending: bool,
        extent: Option<u64>,
    ) -> Option<(BeatArtifact, Vec<FrameRange>)> {
        match self.analyzer.snapshot_deferred(ending) {
            Ok(grid) => {
                let rate = self.analyzer.source_rate();
                let grid = match extent {
                    Some(extent) => extend_over(grid, extent, rate),
                    None => grid,
                };
                Some((grid, self.analyzer.unanalysed(extent)))
            }
            Err(error) => {
                warn!(?error, "beat analysis failed; leaving the beat slot empty");
                None
            }
        }
    }
}
