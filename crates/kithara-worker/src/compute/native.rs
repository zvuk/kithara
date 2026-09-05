use std::num::NonZeroUsize;

use kithara_platform::{
    CancelGroup, CancelToken,
    sync::{
        Arc, OnceLock,
        atomic::{AtomicUsize, Ordering},
    },
};

use super::{ComputeContext, ComputeRejected, ComputeSubmitError};
use crate::{RayonConfig, Wake, config::PoolConfig};

pub(crate) struct ComputeRuntime {
    budget: Arc<Budget>,
    pool: ComputePool,
}

impl ComputeRuntime {
    pub(crate) fn new(pool: PoolConfig, max_in_flight: NonZeroUsize) -> Self {
        Self {
            budget: Arc::new(Budget::new(max_in_flight)),
            pool: match pool {
                PoolConfig::Disabled => ComputePool::Disabled,
                PoolConfig::OwnedLazy(config) => ComputePool::OwnedLazy {
                    config,
                    pool: OnceLock::new(),
                },
                PoolConfig::Shared(pool) => ComputePool::Shared(pool),
            },
        }
    }

    #[cfg(test)]
    pub(crate) const fn pool(&self) -> &ComputePool {
        &self.pool
    }

    pub(crate) fn submit<T, F>(
        &self,
        task_budget: &Arc<Budget>,
        task_token: &CancelToken,
        wake: Wake,
        payload: T,
        job: F,
    ) -> Result<(), ComputeRejected<T>>
    where
        T: Send + 'static,
        F: FnOnce(ComputeContext, T) + Send + 'static,
    {
        if task_token.is_cancelled() {
            return Err(ComputeRejected::new(ComputeSubmitError::Cancelled, payload));
        }
        if matches!(self.pool, ComputePool::Disabled) {
            return Err(ComputeRejected::new(
                ComputeSubmitError::Unavailable,
                payload,
            ));
        }
        let Some(task_permit) = Budget::try_acquire(task_budget) else {
            return Err(ComputeRejected::new(ComputeSubmitError::Saturated, payload));
        };
        let Some(worker_permit) = Budget::try_acquire(&self.budget) else {
            return Err(ComputeRejected::new(ComputeSubmitError::Saturated, payload));
        };
        if task_token.is_cancelled() {
            return Err(ComputeRejected::new(ComputeSubmitError::Cancelled, payload));
        }
        let pool = match self.pool.get() {
            Ok(pool) => pool,
            Err(reason) => return Err(ComputeRejected::new(reason, payload)),
        };
        if task_token.is_cancelled() {
            return Err(ComputeRejected::new(ComputeSubmitError::Cancelled, payload));
        }
        let token = task_token.child();
        let context = ComputeContext {
            cancel: CancelGroup::from(token.clone()),
            token,
        };
        let permit = ComputePermit {
            wake,
            task: Some(task_permit),
            worker: Some(worker_permit),
        };

        pool.spawn(move || {
            let _permit = permit;
            job(context, payload);
        });
        Ok(())
    }
}

pub(crate) enum ComputePool {
    Disabled,
    OwnedLazy {
        config: RayonConfig,
        pool: OnceLock<Result<Arc<rayon::ThreadPool>, String>>,
    },
    Shared(Arc<rayon::ThreadPool>),
}

impl ComputePool {
    fn get(&self) -> Result<Arc<rayon::ThreadPool>, ComputeSubmitError> {
        if matches!(self, Self::Disabled) {
            return Err(ComputeSubmitError::Unavailable);
        }
        if let Self::Shared(pool) = self {
            return Ok(Arc::clone(pool));
        }
        let Self::OwnedLazy { config, pool } = self else {
            return Err(ComputeSubmitError::Unavailable);
        };
        pool.get_or_init(|| build_pool(config))
            .as_ref()
            .map(Arc::clone)
            .map_err(|_| ComputeSubmitError::Unavailable)
    }

    #[cfg(test)]
    pub(crate) fn owned_is_initialized(&self) -> bool {
        matches!(self, Self::OwnedLazy { pool, .. } if pool.get().is_some())
    }

    #[cfg(test)]
    pub(crate) fn shared(&self) -> Option<&Arc<rayon::ThreadPool>> {
        let Self::Shared(pool) = self else {
            return None;
        };
        Some(pool)
    }
}

fn build_pool(config: &RayonConfig) -> Result<Arc<rayon::ThreadPool>, String> {
    let prefix = config.name.clone();
    rayon::ThreadPoolBuilder::new()
        .num_threads(config.threads.get())
        .thread_name(move |index| format!("{prefix}-{index}"))
        .build()
        .map(Arc::new)
        .map_err(|error| error.to_string())
}

pub(crate) struct Budget {
    active: AtomicUsize,
    limit: NonZeroUsize,
}

impl Budget {
    pub(crate) fn new(limit: NonZeroUsize) -> Self {
        Self {
            limit,
            active: AtomicUsize::new(0),
        }
    }

    fn try_acquire(budget: &Arc<Self>) -> Option<BudgetPermit> {
        let mut active = budget.active.load(Ordering::Acquire);
        while active < budget.limit.get() {
            match budget.active.compare_exchange_weak(
                active,
                active + 1,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => {
                    return Some(BudgetPermit {
                        budget: Arc::clone(budget),
                    });
                }
                Err(current) => active = current,
            }
        }
        None
    }
}

struct BudgetPermit {
    budget: Arc<Budget>,
}

impl Drop for BudgetPermit {
    fn drop(&mut self) {
        self.budget.active.fetch_sub(1, Ordering::AcqRel);
    }
}

struct ComputePermit {
    task: Option<BudgetPermit>,
    worker: Option<BudgetPermit>,
    wake: Wake,
}

impl Drop for ComputePermit {
    fn drop(&mut self) {
        drop(self.task.take());
        drop(self.worker.take());
        self.wake.wake();
    }
}

#[cfg(test)]
mod tests {
    use kithara_platform::{
        CancelScope,
        sync::{Arc, OnceLock, atomic::Ordering},
    };
    use kithara_test_utils::kithara;

    use super::{Budget, ComputePool, ComputeRuntime, ComputeSubmitError};
    use crate::{RayonConfig, Wake};

    #[kithara::test(native, flash(false))]
    fn owned_pool_failure_returns_payload_and_releases_both_permits() {
        let failed = OnceLock::new();
        assert!(failed.set(Err(String::from("pool build failed"))).is_ok());
        let runtime = ComputeRuntime {
            budget: Arc::new(Budget::new(std::num::NonZeroUsize::MIN)),
            pool: ComputePool::OwnedLazy {
                config: RayonConfig::new(std::num::NonZeroUsize::MIN, "failed-pool-test"),
                pool: failed,
            },
        };
        let task_budget = Arc::new(Budget::new(std::num::NonZeroUsize::MIN));
        let scope = CancelScope::new(None);
        let token = scope.token().child();
        let rejected = runtime
            .submit(
                &task_budget,
                &token,
                Wake::default(),
                String::from("detector"),
                |_, _| {},
            )
            .expect_err("cached pool build failure must reject compute");

        assert_eq!(rejected.reason(), ComputeSubmitError::Unavailable);
        assert_eq!(rejected.recover_payload(), "detector");
        assert_eq!(task_budget.active.load(Ordering::Acquire), 0);
        assert_eq!(runtime.budget.active.load(Ordering::Acquire), 0);
    }
}
