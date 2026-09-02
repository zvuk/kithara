use std::num::{NonZeroU32, NonZeroUsize};

use bon::Builder;
use kithara_bufpool::PoolRegion;
use kithara_platform::{CancelToken, time::Duration};
use kithara_worker::Worker;

struct Consts;

impl Consts {
    const ACTIVE_WAIT_TIMEOUT: Duration = Duration::from_millis(1);
    const BACKPRESSURE_POLL_INTERVAL: Duration = Duration::from_micros(250);
    const CAPACITY: NonZeroUsize = match NonZeroUsize::new(16) {
        Some(value) => value,
        None => unreachable!(),
    };
    const FAIRNESS_YIELD_INTERVAL: NonZeroU32 = match NonZeroU32::new(16) {
        Some(value) => value,
        None => unreachable!(),
    };
    const TASK_BURST: NonZeroU32 = match NonZeroU32::new(32) {
        Some(value) => value,
        None => unreachable!(),
    };
}

/// Configuration for one shared playback worker.
#[derive(Builder, fieldwork::Fieldwork)]
#[fieldwork(opt_in, get)]
#[non_exhaustive]
pub struct PlayWorkerConfig<S> {
    /// Typed pool facade shared by every Player and resource registered with the worker.
    #[builder(start_fn)]
    #[field(get)]
    pub(crate) pools: PoolRegion<S>,
    /// Poll interval for the RT-safe deferred wake while the final ring is full.
    #[builder(default = Consts::BACKPRESSURE_POLL_INTERVAL)]
    #[field(get, copy)]
    pub(crate) backpressure_poll_interval: Duration,
    /// Parent cancellation token for this playback dispatcher lifetime.
    pub(crate) cancel: Option<CancelToken>,
    /// Optional base worker shared with other domain workers.
    pub(crate) worker: Option<Worker>,
    /// Maximum number of simultaneously registered track render chains.
    #[builder(default = Consts::CAPACITY)]
    #[field(get, copy)]
    pub(crate) capacity: NonZeroUsize,
    /// Consecutive progress passes between cooperative thread yields.
    #[builder(default = Consts::FAIRNESS_YIELD_INTERVAL)]
    #[field(get, copy)]
    pub(crate) fairness_yield_interval: NonZeroU32,
    /// Park duration when no playback task expects progress.
    #[builder(default = Duration::from_millis(100))]
    #[field(get, copy)]
    pub(crate) idle_timeout: Duration,
    /// Threshold for reporting a slow playback tick.
    #[builder(default = Duration::from_millis(10))]
    #[field(get, copy)]
    pub(crate) slow_tick_threshold: Duration,
    /// Maximum consecutive ticks for one track visit.
    #[builder(default = Consts::TASK_BURST)]
    #[field(get, copy)]
    pub(crate) task_burst: NonZeroU32,
    /// Park duration while live playback tasks are waiting.
    #[builder(default = Consts::ACTIVE_WAIT_TIMEOUT)]
    #[field(get, copy)]
    pub(crate) wait_timeout: Duration,
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::*;
    use crate::test_pools::pools;

    #[kithara::test]
    fn playback_worker_uses_live_audio_wait_budgets() {
        let config = PlayWorkerConfig::builder(pools()).build();

        assert_eq!(
            config.backpressure_poll_interval,
            Duration::from_micros(250)
        );
        assert_eq!(config.wait_timeout, Duration::from_millis(1));
    }
}
