use std::num::NonZeroUsize;

use kithara_macros::Patch;
#[cfg(not(target_arch = "wasm32"))]
use kithara_platform::sync::Arc;
use kithara_platform::{CancelToken, tokio::runtime::Handle};
use serde::Deserialize;

/// Shared resources and cancellation parent for a [`Worker`](crate::Worker).
#[non_exhaustive]
#[derive(Clone, fieldwork::Fieldwork, Patch)]
#[fieldwork(opt_in, with)]
pub struct WorkerConfig {
    #[field(with)]
    pub(crate) max_compute_tasks: NonZeroUsize,
    #[field(with, option_set_some)]
    #[patch(skip)]
    pub(crate) cancel: Option<CancelToken>,
    #[field(with, option_set_some)]
    #[patch(skip)]
    pub(crate) runtime: Option<Handle>,
    #[patch(skip)]
    pub(crate) pool: PoolConfig,
}

impl WorkerConfig {
    /// Create a standalone worker with no Tokio handle or Rayon pool.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            cancel: None,
            max_compute_tasks: NonZeroUsize::MIN,
            pool: PoolConfig::Disabled,
            runtime: None,
        }
    }

    /// Install the compute pool a configuration document named.
    #[must_use]
    pub fn with_compute_pool(mut self, pool: ComputePool) -> Self {
        self.pool = match pool {
            ComputePool::Disabled {} => PoolConfig::Disabled,
            #[cfg(not(target_arch = "wasm32"))]
            ComputePool::Owned { name, threads } => {
                PoolConfig::OwnedLazy(RayonConfig::new(threads, name))
            }
        };
        self
    }

    /// Lazily create an owned Rayon pool on the first admitted compute job.
    #[cfg(not(target_arch = "wasm32"))]
    #[must_use]
    pub fn with_owned_pool(mut self, config: RayonConfig) -> Self {
        self.pool = PoolConfig::OwnedLazy(config);
        self
    }

    /// Share an existing Rayon pool without creating another pool.
    #[cfg(not(target_arch = "wasm32"))]
    #[must_use]
    pub fn with_pool(mut self, pool: Arc<rayon::ThreadPool>) -> Self {
        self.pool = PoolConfig::Shared(pool);
        self
    }
}

impl Default for WorkerConfig {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Clone)]
pub(crate) enum PoolConfig {
    Disabled,
    #[cfg(not(target_arch = "wasm32"))]
    OwnedLazy(RayonConfig),
    #[cfg(not(target_arch = "wasm32"))]
    Shared(Arc<rayon::ThreadPool>),
}

/// What a document can say about the compute pool. `Shared` is absent on
/// purpose: it carries a live `rayon::ThreadPool` only code can hand over.
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields, tag = "mode")]
#[non_exhaustive]
pub enum ComputePool {
    /// An empty struct variant, not a unit one: serde checks
    /// `deny_unknown_fields` against a variant's own field list, and a unit
    /// variant has none, so `mode: disabled` would swallow any key beside it.
    Disabled {},
    #[cfg(not(target_arch = "wasm32"))]
    Owned { name: String, threads: NonZeroUsize },
}

/// Configuration for a Rayon pool built on first admitted compute work.
#[cfg(not(target_arch = "wasm32"))]
#[non_exhaustive]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RayonConfig {
    pub(crate) threads: NonZeroUsize,
    pub(crate) name: String,
}

#[cfg(not(target_arch = "wasm32"))]
impl RayonConfig {
    /// Configure the thread count and thread-name prefix.
    #[must_use]
    pub fn new<N: Into<String>>(threads: NonZeroUsize, name: N) -> Self {
        Self {
            threads,
            name: name.into(),
        }
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use std::num::NonZeroUsize;

    use kithara_test_utils::kithara;

    use super::{ComputePool, PoolConfig, RayonConfig, WorkerConfig, WorkerConfigPatch};

    #[kithara::test(native, flash(false))]
    fn a_patch_writes_only_the_field_it_names() {
        let seeded_pool = RayonConfig::new(NonZeroUsize::new(3).expect("nonzero"), "seed");
        let mut config = WorkerConfig::new()
            .with_max_compute_tasks(NonZeroUsize::new(2).expect("nonzero"))
            .with_owned_pool(seeded_pool.clone());

        let patch: WorkerConfigPatch =
            serde_yaml_ng::from_str("max_compute_tasks: 4\n").expect("valid patch document");
        config.apply(patch);

        assert_eq!(config.max_compute_tasks.get(), 4);
        match &config.pool {
            PoolConfig::OwnedLazy(pool) => assert_eq!(
                *pool, seeded_pool,
                "an unnamed field keeps its seeded value"
            ),
            PoolConfig::Disabled | PoolConfig::Shared(_) => {
                panic!("pool must keep the seeded OwnedLazy variant")
            }
        }
    }

    #[kithara::test(native, flash(false))]
    fn compute_pool_owned_parses_name_and_threads() {
        let pool: ComputePool =
            serde_yaml_ng::from_str("mode: owned\nname: analysis\nthreads: 2\n")
                .expect("a valid owned-pool document parses");

        match pool {
            ComputePool::Owned { name, threads } => {
                assert_eq!(name, "analysis");
                assert_eq!(threads.get(), 2);
            }
            ComputePool::Disabled {} => panic!("expected the owned variant"),
        }
    }

    #[kithara::test(native, flash(false))]
    fn compute_pool_rejects_a_shared_mode() {
        let error = serde_yaml_ng::from_str::<ComputePool>("mode: shared\n")
            .expect_err("a document cannot name a live pool it does not own");

        assert!(error.to_string().contains("shared"), "{error}");
    }

    #[kithara::test(native, flash(false))]
    fn a_disabled_pool_refuses_a_key_it_cannot_use() {
        let error = serde_yaml_ng::from_str::<ComputePool>("mode: disabled\nthreads: 4\n")
            .expect_err("a key the disabled mode cannot use must not be dropped in silence");

        assert!(error.to_string().contains("threads"), "{error}");
    }

    #[kithara::test(native, flash(false))]
    fn with_compute_pool_carries_the_documents_thread_count_and_name() {
        let owned: ComputePool =
            serde_yaml_ng::from_str("mode: owned\nname: analysis\nthreads: 2\n")
                .expect("a valid owned-pool document parses");
        let config = WorkerConfig::new()
            .with_owned_pool(RayonConfig::new(
                NonZeroUsize::new(5).expect("nonzero"),
                "seed",
            ))
            .with_compute_pool(owned);

        match &config.pool {
            PoolConfig::OwnedLazy(pool) => {
                assert_eq!(pool.name, "analysis");
                assert_eq!(pool.threads.get(), 2);
            }
            PoolConfig::Disabled | PoolConfig::Shared(_) => {
                panic!("expected an owned pool carrying the document's values")
            }
        }
    }

    #[kithara::test(native, flash(false))]
    fn a_disabled_document_replaces_the_pool_the_builder_installed() {
        let disabled: ComputePool = serde_yaml_ng::from_str("mode: disabled\n")
            .expect("a valid disabled-pool document parses");
        let config = WorkerConfig::new()
            .with_owned_pool(RayonConfig::new(
                NonZeroUsize::new(5).expect("nonzero"),
                "seed",
            ))
            .with_compute_pool(disabled);

        match &config.pool {
            PoolConfig::Disabled => {}
            PoolConfig::OwnedLazy(_) | PoolConfig::Shared(_) => {
                panic!("a disabled document must replace the seeded pool, not keep it")
            }
        }
    }
}
