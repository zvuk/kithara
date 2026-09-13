use std::num::NonZeroU32;

use kithara_bufpool::{HasPool, PoolError, PoolRegion};

use crate::{BlobError, progress::WaveformResume, waveform::bucket::Waveform};

#[derive(Clone, Copy, Default)]
pub(crate) struct Config;

#[derive(Default)]
pub(crate) struct Slot;

impl<S> TryFrom<(&Config, NonZeroU32, &PoolRegion<S>)> for Slot
where
    S: HasPool<f32>,
{
    type Error = PoolError;

    fn try_from(_: (&Config, NonZeroU32, &PoolRegion<S>)) -> Result<Self, Self::Error> {
        Ok(Self)
    }
}

pub(crate) const fn cache_tag(_config: Config) -> Option<String> {
    None
}

pub(crate) const fn config_is_empty(_config: Config) -> bool {
    true
}

pub(crate) fn push<S>(
    _slot: &mut Slot,
    _pools: &PoolRegion<S>,
    _pcm: &[f32],
    _channels: usize,
    _at: u64,
) where
    S: HasPool<f32>,
{
}

pub(crate) fn snapshot(_slot: &mut Slot, _extent: Option<u64>) -> Option<Waveform> {
    None
}

pub(crate) const fn write_resume(_slot: &Slot) -> Option<Vec<u8>> {
    None
}

pub(crate) fn restore<S>(
    _slot: &mut Slot,
    _pools: &PoolRegion<S>,
    resume: Option<&WaveformResume>,
) -> Result<(), BlobError>
where
    S: HasPool<f32>,
{
    if resume.is_none() {
        Ok(())
    } else {
        Err(BlobError::Corrupt)
    }
}
