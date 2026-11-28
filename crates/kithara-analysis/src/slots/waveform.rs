use std::num::NonZeroU32;

use kithara_bufpool::{HasPool, PoolRegion};
use kithara_waveform::{AnalyzerError, WaveformResume};
use tracing::warn;

use crate::{AnalysisParams, BlobError, Waveform, WaveformAnalyzer};

/// The analyzer and the fold factor its snapshots use.
#[derive(Default)]
pub(crate) struct Slot(Option<(WaveformAnalyzer, NonZeroU32)>);

/// Builds the pass that folds columns by `fold`. The caller decides whether a
/// waveform is wanted at all; reaching here means it is.
impl<S> TryFrom<(NonZeroU32, NonZeroU32, &PoolRegion<S>)> for Slot
where
    S: HasPool<f32>,
{
    type Error = AnalyzerError;

    fn try_from(
        (fold, rate, pools): (NonZeroU32, NonZeroU32, &PoolRegion<S>),
    ) -> Result<Self, Self::Error> {
        WaveformAnalyzer::new(rate.get(), AnalysisParams::default(), pools)
            .map(|analyzer| Self(Some((analyzer, fold))))
    }
}

/// The tag names the requested fold factor; an absent factor disables the waveform.
pub(crate) fn cache_tag(fold: Option<NonZeroU32>) -> Option<String> {
    fold.map(|fold| format!("wave:fold{fold}:v3"))
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
        warn!(?error, "waveform input failed; waveform disabled");
        slot.0 = None;
    }
}

pub(crate) fn snapshot(slot: &mut Slot, extent: Option<u64>) -> Option<Waveform> {
    let result = slot
        .0
        .as_mut()
        .map(|(analyzer, fold)| analyzer.snapshot(*fold, extent));
    match result {
        Some(Ok(waveform)) => Some(waveform),
        Some(Err(error)) => {
            warn!(?error, "waveform window failed; waveform disabled");
            slot.0 = None;
            None
        }
        None => None,
    }
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
