use std::num::NonZeroU32;

use kithara_bufpool::{HasPool, PoolError, PoolRegion};
use tracing::warn;

use crate::{
    BlobError, analyzer::WaveformPass, progress::WaveformResume, waveform::bucket::Waveform,
};

pub(crate) type Config = Option<usize>;
#[derive(Default)]
pub(crate) struct Slot(Option<WaveformPass>);

impl<S> TryFrom<(&Config, NonZeroU32, &PoolRegion<S>)> for Slot
where
    S: HasPool<f32>,
{
    type Error = PoolError;

    fn try_from(
        (config, rate, pools): (&Config, NonZeroU32, &PoolRegion<S>),
    ) -> Result<Self, Self::Error> {
        config
            .as_ref()
            .map(|buckets| WaveformPass::new(rate.get(), *buckets, pools))
            .transpose()
            .map(Self)
    }
}

pub(crate) fn cache_tag(config: Config) -> Option<String> {
    config.map(|buckets| format!("wave:native:max{buckets}:v1"))
}

pub(crate) const fn config_is_empty(config: Config) -> bool {
    config.is_none()
}

pub(crate) fn push<S>(slot: &mut Slot, pools: &PoolRegion<S>, pcm: &[f32], channels: usize, at: u64)
where
    S: HasPool<f32>,
{
    let failure = slot
        .0
        .as_mut()
        .and_then(|analyzer| analyzer.push(pools, pcm, channels, at).err());
    if let Some(error) = failure {
        warn!(
            ?error,
            "waveform analysis buffer allocation failed; waveform disabled"
        );
        slot.0 = None;
    }
}

pub(crate) fn snapshot(slot: &mut Slot, extent: Option<u64>) -> Option<Waveform> {
    slot.0.as_mut().map(|analyzer| analyzer.snapshot(extent))
}

pub(crate) fn write_resume(slot: &Slot) -> Option<Vec<u8>> {
    slot.0.as_ref().map(|analyzer| {
        let mut out = Vec::new();
        analyzer.write_resume(&mut out);
        out
    })
}

pub(crate) fn restore<S>(
    slot: &mut Slot,
    pools: &PoolRegion<S>,
    resume: Option<WaveformResume>,
) -> Result<(), BlobError>
where
    S: HasPool<f32>,
{
    match (slot.0.as_mut(), resume) {
        (Some(analyzer), Some(resume)) => analyzer.restore(pools, resume),
        (None, None) => Ok(()),
        (Some(_), None) | (None, Some(_)) => Err(BlobError::Corrupt),
    }
}

pub(crate) const fn with_buckets(config: &mut Config, buckets: usize) {
    *config = Some(buckets);
}
