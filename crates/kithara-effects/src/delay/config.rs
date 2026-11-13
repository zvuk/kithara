use std::num::{NonZeroU32, NonZeroUsize};

use kithara_config::Config;

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
