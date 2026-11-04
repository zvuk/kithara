use std::num::{NonZeroU32, NonZeroUsize};

use kithara_bufpool::PoolError;
use kithara_config::Config;
use kithara_dsp::{delay::DelayError, filter::FilterError};

use super::{DelayTime, Feedback, FeedbackHighPass};

/// The history a [`FeedbackDelay`](super::FeedbackDelay) is prepared with and
/// the parameters it starts at.
#[derive(Clone, Copy, Debug, PartialEq, Config)]
#[config(construction, fields(get(copy)))]
#[non_exhaustive]
pub struct FeedbackDelayConfig {
    /// Frames per second the delay runs at.
    sample_rate: NonZeroU32,
    /// Frames of history each channel holds: the longest delay.
    capacity: NonZeroUsize,
    time: DelayTime,
    feedback: Feedback,
    high_pass: FeedbackHighPass,
}

/// Why a [`FeedbackDelay`](super::FeedbackDelay) could not be prepared.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum FeedbackDelayError {
    #[error("the history refused the delay: {0}")]
    Delay(#[from] DelayError),
    #[error("the history does not fit the pools: {0}")]
    Pool(#[from] PoolError),
    #[error("the feedback high-pass could not be built: {0}")]
    Filter(#[from] FilterError),
}
