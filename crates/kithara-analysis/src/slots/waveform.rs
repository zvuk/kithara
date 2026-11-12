use std::num::NonZeroU32;

use kithara_bufpool::{HasPool, PoolRegion};
use kithara_waveform::{AnalyzerError, WaveformResume};
use tracing::warn;

use crate::{AnalysisParams, BlobError, Waveform, WaveformAnalyzer};

/// The analyzer and the bucket ceiling its snapshots are built to.
#[derive(Default)]
pub(crate) struct Slot(Option<(WaveformAnalyzer, usize)>);

/// Builds the pass that fills `buckets` at most. The caller decides whether a
/// waveform is wanted at all; reaching here means it is.
impl<S> TryFrom<(usize, NonZeroU32, &PoolRegion<S>)> for Slot
where
    S: HasPool<f32>,
{
    type Error = AnalyzerError;

    fn try_from(
        (buckets, rate, pools): (usize, NonZeroU32, &PoolRegion<S>),
    ) -> Result<Self, Self::Error> {
        WaveformAnalyzer::new(rate.get(), AnalysisParams::default(), pools)
            .map(|analyzer| Self(Some((analyzer, buckets))))
    }
}

/// The tag of a waveform built to `buckets`. No bucket ceiling is no waveform,
/// and so no tag.
pub(crate) fn cache_tag(buckets: Option<usize>) -> Option<String> {
    buckets.map(|buckets| format!("wave:native:max{buckets}:v1"))
}

pub(crate) fn push<S>(slot: &mut Slot, pools: &PoolRegion<S>, pcm: &[f32], channels: usize, at: u64)
where
    S: HasPool<f32>,
{
    let failure = slot
        .0
        .as_mut()
        .and_then(|(analyzer, _)| analyzer.push(pools, pcm, channels, at).err());
    if let Some(error) = failure {
        warn!(
            ?error,
            "waveform analysis buffer allocation failed; waveform disabled"
        );
        slot.0 = None;
    }
}

pub(crate) fn snapshot(slot: &mut Slot, extent: Option<u64>) -> Option<Waveform> {
    slot.0
        .as_mut()
        .map(|(analyzer, buckets)| analyzer.snapshot(*buckets, extent))
}

pub(crate) fn write_resume(slot: &Slot) -> Option<Vec<u8>> {
    slot.0.as_ref().map(|(analyzer, _)| {
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
    super::restore(slot.0.as_mut(), resume, |(analyzer, _), resume| {
        analyzer.restore(pools, resume)
    })
}
