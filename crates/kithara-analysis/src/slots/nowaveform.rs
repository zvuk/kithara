use std::{convert::Infallible, num::NonZeroU32};

use kithara_bufpool::{HasPool, PoolError, PoolRegion};
use kithara_waveform::WaveformResume;

use crate::{BlobError, Waveform};

/// A slot that never holds a waveform pass: this build has no waveform
/// analysis.
#[derive(Default)]
pub(crate) struct Slot(Option<Infallible>);

impl<S> TryFrom<(usize, NonZeroU32, &PoolRegion<S>)> for Slot
where
    S: HasPool<f32>,
{
    type Error = PoolError;

    fn try_from(_: (usize, NonZeroU32, &PoolRegion<S>)) -> Result<Self, Self::Error> {
        Ok(Self(None))
    }
}

pub(crate) const fn cache_tag(_buckets: Option<usize>) -> Option<String> {
    None
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
