use kithara_resampler::ResamplerBackend;

use crate::analysis::BeatAnalysisConfig;

#[cfg(feature = "analysis-beat")]
pub(crate) fn detector() -> Option<crate::analysis::beat::SharedBeatDetector> {
    None
}

pub(crate) fn tag<B>(_config: &BeatAnalysisConfig<B>) -> Option<String>
where
    B: ResamplerBackend,
{
    None
}
