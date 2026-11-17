use std::num::{NonZeroU32, NonZeroUsize};

use kithara_bufpool::{HasPool, PoolRegion};
use kithara_key::{KeyError, KeyEstimate, KeyEstimator, KeyResume};

use crate::BlobError;

/// The key estimator of a pass opened with key demand.
#[derive(Default)]
pub(crate) struct Slot(Option<KeyEstimator>);

/// The tag of a key estimate, when one is wanted at all: the estimator and
/// its key profile. The resume state carries its own layout version.
pub(crate) fn cache_tag(enabled: bool) -> Option<String> {
    enabled.then(|| "key:keyextractor:bgate:v1".to_owned())
}

impl Slot {
    pub(crate) fn new<S>(rate: NonZeroU32, pools: &PoolRegion<S>) -> Result<Self, KeyError>
    where
        S: HasPool<f32>,
    {
        KeyEstimator::new(rate.get(), pools).map(|estimator| Self(Some(estimator)))
    }

    pub(crate) fn push<S>(
        &mut self,
        pools: &PoolRegion<S>,
        pcm: &[f32],
        channels: NonZeroUsize,
        at: u64,
    ) -> Result<(), KeyError>
    where
        S: HasPool<f32>,
    {
        self.0
            .as_mut()
            .map_or(Ok(()), |estimator| estimator.push(pools, pcm, channels, at))
    }

    /// The key so far. A settled pass states its extent first, so the tail
    /// frames count; an earlier extent can still grow.
    pub(crate) fn snapshot(
        &mut self,
        settled: bool,
        extent: Option<u64>,
    ) -> Result<Option<KeyEstimate>, KeyError> {
        let Some(estimator) = self.0.as_mut() else {
            return Ok(None);
        };
        if settled && let Some(extent) = extent {
            estimator.set_end(extent)?;
        }
        Ok(estimator.estimate())
    }

    pub(crate) fn write_resume(&self) -> Option<Vec<u8>> {
        self.0.as_ref().map(|estimator| {
            let mut out = Vec::new();
            estimator.write_state(&mut out);
            out
        })
    }

    pub(crate) fn restore<S>(
        &mut self,
        pools: &PoolRegion<S>,
        resume: Option<KeyResume>,
    ) -> Result<(), BlobError>
    where
        S: HasPool<f32>,
    {
        super::restore(self.0.as_mut(), resume, |estimator, resume| {
            estimator.restore_state(pools, resume)
        })
    }
}
