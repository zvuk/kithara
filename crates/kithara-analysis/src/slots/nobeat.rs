use std::{convert::Infallible, marker::PhantomData, num::NonZeroU32, ops::Range};

use kithara_bufpool::{HasPool, PoolRegion};
use kithara_resampler::ResamplerBackend;
use rangemap::RangeSet;

use super::{Intake, Opens};
use crate::{BeatAnalysisConfig, BeatArtifact, BlobError, progress::BeatResume};

pub(crate) type Detector = ();

pub(crate) struct DetectRequest;
pub(crate) struct DetectOutput;

pub(crate) const fn detect(_request: DetectRequest, _detector: &Detector) -> DetectOutput {
    DetectOutput
}

#[derive(Clone)]
#[derive_where::derive_where(Default)]
pub(crate) struct Config<B>(PhantomData<B>)
where
    B: ResamplerBackend;

impl<B> Config<B>
where
    B: ResamplerBackend,
{
    pub(crate) fn build<S>(&self, _rate: NonZeroU32, _pools: &PoolRegion<S>) -> Slot<B>
    where
        S: HasPool<f32>,
    {
        Slot(PhantomData)
    }

    pub(crate) const fn is_empty(&self) -> bool {
        true
    }

    pub(crate) fn set_resampler(&mut self, _resampler: BeatAnalysisConfig<B>) {}

    pub(crate) fn take_detector<S>(&mut self, _pools: &PoolRegion<S>) -> Option<Detector>
    where
        S: HasPool<f32> + Send + Sync + 'static,
    {
        None
    }

    pub(crate) fn with_default(&mut self, _resampler: BeatAnalysisConfig<B>) {}
}

#[derive_where::derive_where(Default)]
pub(crate) struct Slot<B>(PhantomData<B>)
where
    B: ResamplerBackend;

impl<B> Slot<B>
where
    B: ResamplerBackend,
{
    pub(crate) fn apply_detection(&mut self, _output: DetectOutput) {}

    pub(crate) const fn coverage<'a>(&'a self, seen: &'a RangeSet<u64>) -> &'a RangeSet<u64> {
        seen
    }

    pub(crate) const fn intake(&self) -> Intake {
        Intake::Anywhere
    }

    pub(crate) fn prepare_detection<S>(
        &mut self,
        _pools: &PoolRegion<S>,
        _trailing: bool,
    ) -> Option<DetectRequest>
    where
        S: HasPool<f32>,
    {
        None
    }

    pub(crate) fn push<S>(
        &mut self,
        _pools: &PoolRegion<S>,
        _pcm: &[f32],
        _channels: usize,
        _at: u64,
        _opens: Opens,
        _detector: Option<&mut Detector>,
    ) -> bool
    where
        S: HasPool<f32>,
    {
        false
    }

    pub(crate) fn restore<S>(
        &mut self,
        _pools: &PoolRegion<S>,
        resume: Option<BeatResume>,
    ) -> Result<(), BlobError>
    where
        S: HasPool<f32>,
    {
        super::restore(None::<&mut Infallible>, resume, |pass, _| match *pass {})
    }

    pub(crate) fn snapshot<S>(
        &mut self,
        _pools: &PoolRegion<S>,
        _detector: Option<&mut Detector>,
        _ending: bool,
        _extent: Option<u64>,
    ) -> Option<(BeatArtifact, Vec<Range<u64>>)>
    where
        S: HasPool<f32>,
    {
        None
    }

    pub(crate) const fn write_resume(&mut self) -> Option<Vec<u8>> {
        None
    }
}
