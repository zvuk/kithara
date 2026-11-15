use kithara_bufpool::PoolError;
use kithara_dsp::filter::FilterError;

/// Why a waveform analyzer could not be built or a window could not be reduced.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum AnalyzerError {
    #[error("waveform filter failed: {0}")]
    Filter(#[from] FilterError),
    #[error("waveform buffer allocation failed: {0}")]
    Pool(#[from] PoolError),
}
