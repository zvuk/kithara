use std::num::{NonZeroU32, NonZeroUsize};

use bon::Builder;
use kithara_bufpool::PoolRegion;
use kithara_derive::Patch;
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
#[kithara_config::config(construction, builder = false)]
#[derive(Builder, fieldwork::Fieldwork, Patch)]
#[fieldwork(opt_in, get)]
#[non_exhaustive]
pub struct PlayWorkerConfig<S> {
    /// Typed pool facade shared by every Player and resource registered with the worker.
    #[config(
        skip = "injected pool facade",
        builder(start_fn),
        field(get),
        patch(skip)
    )]
    pub(crate) pools: PoolRegion<S>,
    /// Poll interval for RT-safe deferred wakes while the final ring is full.
    #[config(value, builder(default = Consts::BACKPRESSURE_POLL_INTERVAL), field(get, copy), patch(humantime))]
    pub(crate) backpressure_poll_interval: Duration,
    /// Park duration when no playback task expects progress.
    #[config(value, builder(default = Duration::from_millis(100)), field(get, copy), patch(humantime))]
    pub(crate) idle_timeout: Duration,
    /// Threshold for reporting a slow playback tick.
    #[config(value, builder(default = Duration::from_millis(10)), field(get, copy), patch(humantime))]
    pub(crate) slow_tick_threshold: Duration,
    /// Park duration while live playback tasks are waiting.
    #[config(value, builder(default = Consts::ACTIVE_WAIT_TIMEOUT), field(get, copy), patch(humantime))]
    pub(crate) wait_timeout: Duration,
    /// Consecutive progress passes between cooperative thread yields.
    #[config(value, builder(default = Consts::FAIRNESS_YIELD_INTERVAL), field(get, copy))]
    pub(crate) fairness_yield_interval: NonZeroU32,
    /// Maximum consecutive ticks for one track visit.
    #[config(value, builder(default = Consts::TASK_BURST), field(get, copy))]
    pub(crate) task_burst: NonZeroU32,
    /// Maximum number of simultaneously registered track render chains.
    #[config(value, builder(default = Consts::CAPACITY), field(get, copy))]
    pub(crate) capacity: NonZeroUsize,
    /// Parent cancellation token for this playback dispatcher lifetime. Not a
    /// document key: the caller owns the token tree.
    #[config(skip = "injected cancellation resource", patch(skip))]
    pub(crate) cancel: Option<CancelToken>,
    /// Optional base worker shared with other domain workers. Not a document
    /// key: a live worker is an object only code can hand over.
    #[config(skip = "injected base worker", patch(skip))]
    pub(crate) worker: Option<Worker>,
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
