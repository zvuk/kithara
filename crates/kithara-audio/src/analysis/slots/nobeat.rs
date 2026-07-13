use std::marker::PhantomData;

use kithara_bufpool::PcmPool;
use kithara_decode::{PcmChunk, PcmSpec};
use kithara_resampler::ResamplerBackend;

use crate::{analysis::BeatAnalysisConfig, waveform::BeatGrid};

pub(crate) struct Config<B>(PhantomData<B>);

impl<B> Config<B>
where
    B: ResamplerBackend,
{
    pub(crate) fn build(&self, _spec: PcmSpec, _pcm_pool: &PcmPool) -> Slot<B> {
        Slot(PhantomData)
    }

    pub(crate) fn is_empty(&self) -> bool {
        true
    }

    pub(crate) fn set_resampler(&mut self, _resampler: BeatAnalysisConfig<B>) {}

    pub(crate) fn with_default(&mut self, _resampler: BeatAnalysisConfig<B>) {}
}

impl<B> Default for Config<B> {
    fn default() -> Self {
        Self(PhantomData)
    }
}

pub(crate) struct Slot<B>(PhantomData<B>);

impl<B> Default for Slot<B> {
    fn default() -> Self {
        Self(PhantomData)
    }
}

impl<B> Slot<B>
where
    B: ResamplerBackend,
{
    pub(crate) fn finish(self) -> Option<BeatGrid> {
        None
    }

    pub(crate) fn is_empty(&self) -> bool {
        true
    }

    pub(crate) fn push(&mut self, _chunk: &PcmChunk) {}
}
