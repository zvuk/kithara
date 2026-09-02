use std::num::{NonZeroU32, NonZeroUsize};

use bon::Builder;
use kithara_platform::{CancelGroup, time::Duration};

use crate::{Observer, observer::Event};

/// Scheduler thread budgets and observer.
#[non_exhaustive]
#[derive(Builder)]
#[builder(state_mod(vis = "pub"))]
pub struct DispatcherConfig {
    #[builder(
        default = Box::new(NoopObserver),
        with = |observer: impl Observer| Box::new(observer)
    )]
    pub(crate) observer: Box<dyn Observer>,
    #[builder(default = Duration::from_millis(10))]
    pub(crate) backpressure_poll_interval: Duration,
    #[builder(default = Duration::from_millis(100))]
    pub(crate) idle_timeout: Duration,
    #[builder(default = Duration::from_millis(10))]
    pub(crate) slow_tick_threshold: Duration,
    #[builder(default = Duration::from_millis(10))]
    pub(crate) wait_timeout: Duration,
    #[builder(default = NonZeroU32::new(16).unwrap_or(NonZeroU32::MIN))]
    pub(crate) fairness_yield_interval: NonZeroU32,
    #[builder(default = NonZeroU32::new(32).unwrap_or(NonZeroU32::MIN))]
    pub(crate) task_burst: NonZeroU32,
    #[builder(default = NonZeroUsize::new(64).unwrap_or(NonZeroUsize::MIN))]
    pub(crate) capacity: NonZeroUsize,
    pub(crate) cancel: Option<CancelGroup>,
    #[builder(into)]
    pub(crate) name: String,
}

struct NoopObserver;

impl Observer for NoopObserver {
    fn on_event(&mut self, _event: Event) {}
}
