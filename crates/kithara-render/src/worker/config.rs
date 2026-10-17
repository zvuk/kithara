use std::num::{NonZeroU32, NonZeroUsize};

use kithara_bufpool::PoolRegion;
use kithara_config::Config;
use kithara_derive::Patch;
use kithara_platform::{CancelToken, time::Duration, tokio::runtime::Handle};
use kithara_worker::Worker;

use crate::consts;

/// Configuration for one shared playback worker.
#[derive(Config, fieldwork::Fieldwork, Patch)]
#[config(construction)]
#[fieldwork(opt_in, get)]
#[non_exhaustive]
pub struct PlayWorkerConfig<S> {
    /// Typed pool facade shared by every Player and resource registered with the worker.
    #[config(
        skip = "transferred to the playback worker",
        builder(start_fn),
        patch(skip)
    )]
    #[field(get)]
    pub(crate) pools: PoolRegion<S>,
    /// Poll interval for RT-safe deferred wakes while the final ring is full.
    #[config(value, builder(default = consts::BACKPRESSURE_POLL_INTERVAL), patch(humantime))]
    #[field(get, copy)]
    pub(crate) backpressure_poll_interval: Duration,
    /// Park duration when no playback task expects progress.
    #[config(value, builder(default = Duration::from_millis(100)), patch(humantime))]
    #[field(get, copy)]
    pub(crate) idle_timeout: Duration,
    /// Threshold for reporting a slow playback tick.
    #[config(value, builder(default = Duration::from_millis(10)), patch(humantime))]
    #[field(get, copy)]
    pub(crate) slow_tick_threshold: Duration,
    /// Park duration while live playback tasks are waiting.
    #[config(value, builder(default = consts::ACTIVE_WAIT_TIMEOUT), patch(humantime))]
    #[field(get, copy)]
    pub(crate) wait_timeout: Duration,
    /// Consecutive progress passes between cooperative thread yields.
    #[config(value, builder(default = consts::FAIRNESS_YIELD_INTERVAL))]
    #[field(get, copy)]
    pub(crate) fairness_yield_interval: NonZeroU32,
    /// Maximum consecutive ticks for one track visit.
    #[config(value, builder(default = consts::TASK_BURST))]
    #[field(get, copy)]
    pub(crate) task_burst: NonZeroU32,
    /// Maximum number of simultaneously registered track render chains.
    #[config(value, builder(default = consts::CAPACITY))]
    #[field(get, copy)]
    pub(crate) capacity: NonZeroUsize,
    /// Batches one track's render lane holds in flight: speed changes the
    /// player sent and the lane has not yet executed.
    #[config(value, builder(default = consts::LANE_CAPACITY))]
    pub(crate) lane_capacity: NonZeroUsize,
    /// Parent cancellation token for this playback dispatcher lifetime. Not a
    /// document key: the caller owns the token tree.
    #[config(skip = "composed into the playback cancel scope", patch(skip))]
    pub(crate) cancel: Option<CancelToken>,
    /// Optional base worker shared with other domain workers. Not a document
    /// key: a live worker is an object only code can hand over.
    #[config(skip = "transferred to the playback worker", patch(skip))]
    pub(crate) worker: Option<Worker>,
    /// Runtime for source opens and lane commands; defaults to the ambient runtime.
    /// When sharing a base worker, that worker's runtime is used instead.
    #[config(
        skip = "transferred to the base worker",
        builder(required, default = Handle::try_current().ok()),
        patch(skip)
    )]
    pub(crate) runtime: Option<Handle>,
}

#[cfg(all(test, not(target_arch = "wasm32")))]
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

    #[kithara::test(native, flash(false))]
    fn a_document_budget_reaches_the_built_playback_worker_config() {
        let patch: PlayWorkerConfigPatch =
            serde_yaml_ng::from_str("wait_timeout: 4ms\ncapacity: 3\n")
                .expect("a valid playback-worker document");
        let mut config = PlayWorkerConfig::builder(pools()).build();

        config.apply(patch);

        assert_eq!(config.wait_timeout, Duration::from_millis(4));
        assert_eq!(config.capacity.get(), 3);
    }

    #[kithara::test(native)]
    fn a_document_lane_capacity_sizes_every_render_lane() {
        let patch: PlayWorkerConfigPatch = serde_yaml_ng::from_str("lane_capacity: 256\n")
            .expect("a valid playback-worker document");
        let mut config = PlayWorkerConfig::builder(pools()).build();

        config.apply(patch);

        assert_eq!(config.lane_capacity.get(), 256);
    }

    #[kithara::test(native, flash(false))]
    fn a_key_the_document_did_not_name_keeps_the_crate_default() {
        let patch: PlayWorkerConfigPatch =
            serde_yaml_ng::from_str("capacity: 3\n").expect("a valid playback-worker document");
        let mut config = PlayWorkerConfig::builder(pools()).build();

        config.apply(patch);

        assert_eq!(
            config.backpressure_poll_interval,
            Duration::from_micros(250)
        );
    }

    #[kithara::test(native, flash(false))]
    fn the_shared_pools_are_not_a_document_key() {
        let error = serde_yaml_ng::from_str::<PlayWorkerConfigPatch>("pools: shared\n")
            .expect_err("a document cannot name a live pool facade");

        assert!(error.to_string().contains("pools"), "{error}");
    }
}
