/// Why a read position or a span was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum DelayError {
    #[error("delay is not finite, shorter than one frame, or longer than the prepared capacity")]
    OutOfRange,
    #[error("channel, plane count, plane coverage or span length does not match the delay line")]
    Shape,
}

/// Why a [`FeedbackComb`](super::FeedbackComb) could not be prepared or run.
#[derive(Debug, thiserror::Error)]
pub enum CombError {
    #[error("the history refused the comb: {0}")]
    Delay(#[from] DelayError),
    #[error("the history does not fit the pools: {0}")]
    Pool(#[from] kithara_bufpool::PoolError),
    #[error("the loop section could not be built: {0}")]
    Filter(#[from] crate::filter::FilterError),
}
