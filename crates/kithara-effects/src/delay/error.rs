use kithara_dsp::delay::DelayError;

/// Why a feedback delay could not be prepared or run.
#[derive(Debug, thiserror::Error)]
pub enum CombError {
    #[error("the history refused the comb: {0}")]
    Delay(#[from] DelayError),
    #[error("the history does not fit the pools: {0}")]
    Pool(#[from] kithara_bufpool::PoolError),
    #[error("the loop section could not be built: {0}")]
    Filter(#[from] kithara_dsp::filter::FilterError),
}
