use std::num::{NonZeroU32, NonZeroUsize};

use bon::Builder;
use kithara_platform::{CancelToken, time::Duration};
use kithara_resampler::ResamplerBackend;
use kithara_worker::{Priority, Worker};

use crate::analyzer::AnalyzerBuilder;

struct Consts;

impl Consts {
    const CAPACITY: NonZeroUsize = match NonZeroUsize::new(64) {
        Some(value) => value,
        None => unreachable!(),
    };
    const CHUNK_SECONDS: NonZeroU32 = match NonZeroU32::new(16) {
        Some(value) => value,
        None => unreachable!(),
    };
    const FAIRNESS_YIELD_INTERVAL: NonZeroU32 = match NonZeroU32::new(16) {
        Some(value) => value,
        None => unreachable!(),
    };
    const PRODUCER_DRAIN_LIMIT: NonZeroUsize = match NonZeroUsize::new(8) {
        Some(value) => value,
        None => unreachable!(),
    };
    const PUBLISH_SECONDS: NonZeroU32 = match NonZeroU32::new(5) {
        Some(value) => value,
        None => unreachable!(),
    };
}

/// Configuration for one analysis dispatcher and its per-pass tasks.
#[kithara_config::config(construction, builder = false)]
#[derive(Builder)]
#[builder(start_fn = for_builder)]
#[non_exhaustive]
pub struct AnalysisWorkerConfig<B, S>
where
    B: ResamplerBackend,
{
    /// Analyzer selection and DSP configuration.
    #[builder(start_fn)]
    #[config(skip = "analyzer builder moves into per-pass task factory")]
    pub(crate) builder: AnalyzerBuilder<B, S>,
    /// Park duration when no analysis task expects progress.
    #[builder(default = Duration::from_millis(10))]
    #[config(value)]
    pub(crate) idle_timeout: Duration,
    /// Threshold for reporting a slow analysis tick.
    #[builder(default = Duration::from_millis(10))]
    #[config(value)]
    pub(crate) slow_tick_threshold: Duration,
    /// Park duration while analysis is waiting on decoded input.
    #[builder(default = Duration::from_millis(10))]
    #[config(value)]
    pub(crate) wait_timeout: Duration,
    /// Fixed source duration covered by one progressive schedule chunk.
    #[builder(default = Consts::CHUNK_SECONDS)]
    #[config(value)]
    pub(crate) chunk_seconds: NonZeroU32,
    /// Consecutive progress passes between cooperative thread yields.
    #[builder(default = Consts::FAIRNESS_YIELD_INTERVAL)]
    #[config(value)]
    pub(crate) fairness_yield_interval: NonZeroU32,
    /// Newly covered source duration between progressive publications.
    #[builder(default = Consts::PUBLISH_SECONDS)]
    #[config(value)]
    pub(crate) publish_seconds: NonZeroU32,
    /// Maximum consecutive ticks for one analysis task visit.
    #[builder(default = NonZeroU32::MIN)]
    #[config(value)]
    pub(crate) task_burst: NonZeroU32,
    /// Maximum number of tasks admitted to the analysis dispatcher.
    #[builder(default = Consts::CAPACITY)]
    #[config(value)]
    pub(crate) capacity: NonZeroUsize,
    /// Maximum number of in-flight compute jobs owned by one analysis pass.
    #[builder(default = NonZeroUsize::MIN)]
    #[config(value)]
    pub(crate) max_compute_tasks: NonZeroUsize,
    /// Maximum playback-ring descriptors folded during one dispatcher tick.
    #[builder(default = Consts::PRODUCER_DRAIN_LIMIT)]
    #[config(value)]
    pub(crate) producer_drain_limit: NonZeroUsize,
    /// Parent cancellation token for the analysis worker lifetime.
    #[config(skip = "injected cancellation resource")]
    pub(crate) cancel: Option<CancelToken>,
    /// Optional base worker shared with other domain workers.
    #[config(skip = "injected base worker")]
    pub(crate) worker: Option<Worker>,
    /// Numeric priority of every analysis pass task.
    #[builder(default = Priority::new(0))]
    #[config(value)]
    pub(crate) priority: Priority,
}
