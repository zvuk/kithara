use std::num::{NonZeroU32, NonZeroUsize};

use bon::Builder;
use kithara_macros::Patch;
use kithara_platform::{CancelGroup, time::Duration};

use crate::{Observer, observer::Event};

/// Scheduler thread budgets and observer.
#[non_exhaustive]
#[derive(Builder, Patch)]
#[builder(state_mod(vis = "pub"))]
pub struct DispatcherConfig {
    /// Where this dispatcher reports its passes. Not a document key: an
    /// observer is a live object only code can hand over.
    #[builder(
        default = Box::new(NoopObserver),
        with = |observer: impl Observer| Box::new(observer)
    )]
    #[patch(skip)]
    pub(crate) observer: Box<dyn Observer>,
    /// Poll interval for deferred wakes while a task's sink is full.
    #[builder(default = Duration::from_millis(10))]
    #[patch(attribute(serde(with = "humantime_serde::option")))]
    pub(crate) backpressure_poll_interval: Duration,
    /// Park duration when no task expects progress.
    #[builder(default = Duration::from_millis(100))]
    #[patch(attribute(serde(with = "humantime_serde::option")))]
    pub(crate) idle_timeout: Duration,
    /// Threshold for reporting a slow tick.
    #[builder(default = Duration::from_millis(10))]
    #[patch(attribute(serde(with = "humantime_serde::option")))]
    pub(crate) slow_tick_threshold: Duration,
    /// Park duration while tasks are waiting.
    #[builder(default = Duration::from_millis(10))]
    #[patch(attribute(serde(with = "humantime_serde::option")))]
    pub(crate) wait_timeout: Duration,
    /// Consecutive progress passes between cooperative thread yields.
    #[builder(default = NonZeroU32::new(16).unwrap_or(NonZeroU32::MIN))]
    pub(crate) fairness_yield_interval: NonZeroU32,
    /// Maximum consecutive ticks for one task visit.
    #[builder(default = NonZeroU32::new(32).unwrap_or(NonZeroU32::MIN))]
    pub(crate) task_burst: NonZeroU32,
    /// Maximum number of simultaneously registered tasks.
    #[builder(default = NonZeroUsize::new(64).unwrap_or(NonZeroUsize::MIN))]
    pub(crate) capacity: NonZeroUsize,
    /// Parent cancellation group for this dispatcher's lifetime. Not a
    /// document key: the caller owns the token tree.
    #[patch(skip)]
    pub(crate) cancel: Option<CancelGroup>,
    /// Thread name for this dispatcher. Not a document key: each dispatcher
    /// names itself where it is built, and one document key would rename
    /// every one of them at once.
    #[builder(into)]
    #[patch(skip)]
    pub(crate) name: String,
}

struct NoopObserver;

impl Observer for NoopObserver {
    fn on_event(&mut self, _event: Event) {}
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use kithara_test_utils::kithara;

    use super::{DispatcherConfig, DispatcherConfigPatch, Duration};

    #[kithara::test(native, flash(false))]
    fn a_document_budget_reaches_the_built_dispatcher_config() {
        let patch: DispatcherConfigPatch = serde_yaml_ng::from_str(
            "wait_timeout: 3ms\nbackpressure_poll_interval: 250us\ncapacity: 8\n",
        )
        .expect("a valid dispatcher document");
        let mut config = DispatcherConfig::builder().name("document-test").build();

        config.apply(patch);

        assert_eq!(config.wait_timeout, Duration::from_millis(3));
        assert_eq!(
            config.backpressure_poll_interval,
            Duration::from_micros(250)
        );
        assert_eq!(config.capacity.get(), 8);
    }

    #[kithara::test(native, flash(false))]
    fn a_key_the_document_did_not_name_keeps_the_crate_default() {
        let patch: DispatcherConfigPatch =
            serde_yaml_ng::from_str("capacity: 8\n").expect("a valid dispatcher document");
        let mut config = DispatcherConfig::builder().name("document-test").build();

        config.apply(patch);

        assert_eq!(config.idle_timeout, Duration::from_millis(100));
    }

    #[kithara::test(native, flash(false))]
    fn the_thread_name_is_not_a_document_key() {
        let error = serde_yaml_ng::from_str::<DispatcherConfigPatch>("name: renamed\n")
            .expect_err("one document key must not rename every dispatcher");

        assert!(error.to_string().contains("name"), "{error}");
    }
}
