use std::mem;

use kithara_platform::sync::{Arc, Mutex};
use rayon::ThreadPoolBuilder;

use super::ComputeSubmitError;
use crate::{OwnedPoolConfig, config::PoolConfig, threads::Threads};

pub(crate) struct ComputePool {
    pub(crate) owned: Mutex<PoolState>,
    threads: Arc<Threads>,
}

pub(crate) enum PoolState {
    Uninitialized,
    Running(Arc<rayon::ThreadPool>),
    Failed,
    Stopped,
}

impl ComputePool {
    pub(super) fn new(threads: Arc<Threads>) -> Self {
        Self {
            owned: Mutex::new(PoolState::Uninitialized),
            threads,
        }
    }

    #[cfg(test)]
    pub(crate) fn owned_is_initialized(&self) -> bool {
        !matches!(*self.owned.lock(), PoolState::Uninitialized)
    }

    pub(super) fn spawner(&self, config: &PoolConfig) -> Result<Spawner, ComputeSubmitError> {
        match config {
            PoolConfig::Disabled => Err(ComputeSubmitError::Unavailable),
            PoolConfig::Shared(pool) => Ok(Spawner(Arc::clone(pool))),
            PoolConfig::OwnedLazy(config) => {
                let mut owned = self.owned.lock();
                if matches!(*owned, PoolState::Uninitialized) {
                    *owned = build_pool(config, &self.threads)
                        .map_or(PoolState::Failed, PoolState::Running);
                }
                match &*owned {
                    PoolState::Running(pool) => Ok(Spawner(Arc::clone(pool))),
                    _ => Err(ComputeSubmitError::Unavailable),
                }
            }
        }
    }

    pub(crate) fn stop(&self) {
        let owned = mem::replace(&mut *self.owned.lock(), PoolState::Stopped);
        drop(owned);
    }
}

/// Pool threads live in the worker's thread registry, which the worker joins.
impl Drop for ComputePool {
    fn drop(&mut self) {
        self.stop();
    }
}

pub(super) struct Spawner(Arc<rayon::ThreadPool>);

impl Spawner {
    pub(super) fn spawn<F: FnOnce() + Send + 'static>(self, job: F) {
        self.0.spawn(kithara_platform::thread::wrap_pool_task(job));
    }
}

fn build_pool(
    config: &OwnedPoolConfig,
    threads: &Threads,
) -> Result<Arc<rayon::ThreadPool>, String> {
    ThreadPoolBuilder::new()
        .num_threads(config.threads.get())
        .spawn_handler(|thread| {
            let name = format!("{}-{}", config.name, thread.index());
            threads.spawn_pool(name, move || thread.run())
        })
        .build()
        .map(Arc::new)
        .map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroUsize;

    use kithara_platform::{
        CancelScope,
        sync::{Arc, mpsc},
        time::{Duration, Instant},
    };
    use kithara_test_utils::kithara;

    use super::PoolState;
    use crate::{
        OwnedPoolConfig, Wake, WorkerConfig,
        compute::{Budget, ComputeRuntime, ComputeSubmitError},
    };

    #[kithara::test(native, flash(false))]
    fn owned_pool_failure_returns_payload_and_releases_both_permits() {
        let runtime = ComputeRuntime::new(
            WorkerConfig::new()
                .with_owned_pool(OwnedPoolConfig::new(NonZeroUsize::MIN, "failed-pool-test")),
            Arc::default(),
        );
        *runtime.pool.owned.lock() = PoolState::Failed;
        let task_budget = Arc::new(Budget::default());
        let scope = CancelScope::new(None);
        let token = scope.token().child();
        let rejected = runtime
            .submit(
                &task_budget,
                NonZeroUsize::MIN,
                &token,
                Wake::default(),
                String::from("detector"),
                |_, _| {},
            )
            .expect_err("cached pool build failure must reject compute");

        assert_eq!(rejected.reason(), ComputeSubmitError::Unavailable);
        assert_eq!(rejected.recover_payload(), "detector");
        assert_eq!(task_budget.active(), 0);
        assert_eq!(runtime.budget.active(), 0);
    }

    #[kithara::test(native, flash(false))]
    fn worker_budget_reads_the_retained_config_on_each_submission() {
        let pool = Arc::new(
            rayon::ThreadPoolBuilder::new()
                .num_threads(2)
                .build()
                .expect("test pool"),
        );
        let mut runtime = ComputeRuntime::new(WorkerConfig::new().with_pool(pool), Arc::default());
        let budget = Arc::new(Budget::default());
        let scope = CancelScope::new(None);
        let token = scope.token().child();
        let (started, started_rx) = mpsc::channel();
        let (release, release_rx) = mpsc::channel();
        runtime
            .submit(
                &budget,
                NonZeroUsize::MIN,
                &token,
                Wake::default(),
                (),
                move |_, ()| {
                    started.send(()).ok();
                    release_rx.recv().ok();
                },
            )
            .expect("first job admitted");
        started_rx
            .recv_timeout(Instant::now() + Duration::from_secs(2))
            .expect("first job started");

        runtime.config.max_compute_tasks = NonZeroUsize::new(2).expect("nonzero");
        let second_budget = Arc::new(Budget::default());
        let (done, done_rx) = mpsc::channel();
        runtime
            .submit(
                &second_budget,
                NonZeroUsize::MIN,
                &token,
                Wake::default(),
                (),
                move |_, ()| {
                    done.send(()).ok();
                },
            )
            .expect("new config limit admits another job");
        done_rx
            .recv_timeout(Instant::now() + Duration::from_secs(2))
            .expect("second job ran");
        release.send(()).expect("release first job");
    }
}
