use kithara_platform::{CancelScope, sync::Arc};

use crate::{Dispatcher, DispatcherConfig, WorkerConfig, compute::ComputeRuntime};

/// Cloneable owner of shared worker runtime resources.
#[derive(Clone)]
pub struct Worker {
    inner: Arc<WorkerInner>,
}

impl Worker {
    /// Create a worker from one complete configuration.
    #[must_use]
    pub fn new(config: WorkerConfig) -> Self {
        Self {
            inner: Arc::new(WorkerInner {
                compute: Arc::new(ComputeRuntime::new(config.pool, config.max_compute_tasks)),
                runtime: config.runtime,
                scope: CancelScope::new(config.cancel),
            }),
        }
    }

    /// Start a dedicated scheduler thread sharing this worker's resources.
    #[must_use]
    pub fn dispatcher(&self, config: DispatcherConfig) -> Dispatcher {
        Dispatcher::start(
            config,
            self.inner.scope.token().child(),
            Arc::clone(&self.inner.compute),
            self.inner.runtime.clone(),
        )
    }

    delegate::delegate! {
        to self.inner.scope {
            /// Cancel every dispatcher and task in this worker subtree.
            pub fn cancel(&self);
            /// Return whether the worker subtree has been cancelled.
            #[must_use]
            pub fn is_cancelled(&self) -> bool;
        }
    }
}

struct WorkerInner {
    compute: Arc<ComputeRuntime>,
    scope: CancelScope,
    runtime: Option<kithara_platform::tokio::runtime::Handle>,
}

impl Drop for WorkerInner {
    fn drop(&mut self) {
        self.scope.cancel();
    }
}

#[cfg(test)]
mod tests {
    #[cfg(not(target_arch = "wasm32"))]
    use std::num::NonZeroUsize;

    #[cfg(not(target_arch = "wasm32"))]
    use kithara_platform::{
        CancelScope,
        sync::{Arc, ThreadGate, WaitGate, mpsc},
        thread,
        time::{Duration, Instant},
    };
    use kithara_test_utils::kithara;

    use super::*;
    #[cfg(not(target_arch = "wasm32"))]
    use crate::{ComputeSubmitError, RayonConfig, compute::ComputePool};
    use crate::{DispatcherConfig, TaskConfig};

    #[cfg(not(target_arch = "wasm32"))]
    #[kithara::test(native, flash(false))]
    fn worker_keeps_supplied_pool_and_never_creates_one_when_absent() {
        let absent = Worker::new(WorkerConfig::new());
        assert!(matches!(absent.inner.compute.pool(), ComputePool::Disabled));

        let pool = Arc::new(
            rayon::ThreadPoolBuilder::new()
                .num_threads(1)
                .build()
                .expect("test Rayon pool must build"),
        );
        let shared = Worker::new(WorkerConfig::new().with_pool(Arc::clone(&pool)));
        let configured = shared
            .inner
            .compute
            .pool()
            .shared()
            .expect("configured pool must be retained");

        assert!(Arc::ptr_eq(configured, &pool));
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[kithara::test(native, flash(false))]
    fn owned_pool_is_lazy_and_executes_the_payload_once() {
        let worker = Worker::new(
            WorkerConfig::new()
                .with_owned_pool(RayonConfig::new(NonZeroUsize::MIN, "owned-compute-test")),
        );
        assert!(!worker.inner.compute.pool().owned_is_initialized());
        let dispatcher =
            worker.dispatcher(DispatcherConfig::builder().name("owned-pool-test").build());
        let pending = dispatcher
            .reserve(TaskConfig::new())
            .expect("task reservation");
        let (completed, received) = mpsc::channel();

        pending
            .context()
            .submit_compute(String::from("compute-payload"), move |_, payload| {
                let name = thread::current().name().unwrap_or_default().to_owned();
                completed.send((name, payload)).ok();
            })
            .expect("first compute job must build the owned pool");

        assert!(worker.inner.compute.pool().owned_is_initialized());
        assert_eq!(
            received
                .recv_timeout(Instant::now() + Duration::from_secs(2))
                .expect("compute job completion"),
            (
                String::from("owned-compute-test-0"),
                String::from("compute-payload")
            )
        );
        assert!(received.try_recv().is_err());
        let ComputePool::OwnedLazy { pool, .. } = worker.inner.compute.pool() else {
            panic!("expected owned lazy pool");
        };
        let built = pool
            .get()
            .expect("pool must be initialized")
            .as_ref()
            .expect("pool build must succeed");
        assert_eq!(built.current_num_threads(), 1);
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[kithara::test(native, flash(false))]
    fn accepted_payload_executes_once_when_cancelled_before_the_pool_runs_it() {
        let pool = Arc::new(
            rayon::ThreadPoolBuilder::new()
                .num_threads(1)
                .build()
                .expect("test Rayon pool must build"),
        );
        let (blocked, blocked_rx) = mpsc::channel();
        let (release, release_rx) = mpsc::channel();
        pool.spawn(move || {
            blocked.send(()).ok();
            release_rx.recv().ok();
        });
        blocked_rx
            .recv_timeout(Instant::now() + Duration::from_secs(2))
            .expect("pool thread must be occupied");
        let worker = Worker::new(WorkerConfig::new().with_pool(pool));
        let dispatcher = worker.dispatcher(
            DispatcherConfig::builder()
                .name("accepted-payload-test")
                .build(),
        );
        let pending = dispatcher
            .reserve(TaskConfig::new())
            .expect("task reservation");
        let context = pending.context().clone();
        let (completed, completed_rx) = mpsc::channel();
        context
            .submit_compute(String::from("owned-detector"), move |compute, payload| {
                completed
                    .send((compute.cancel_group().is_cancelled(), payload))
                    .ok();
            })
            .expect("compute admission");

        context.control().cancel();
        release.send(()).expect("release pool thread");

        assert_eq!(
            completed_rx
                .recv_timeout(Instant::now() + Duration::from_secs(2))
                .expect("accepted payload must execute"),
            (true, String::from("owned-detector"))
        );
        assert!(completed_rx.try_recv().is_err());
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[kithara::test(native, flash(false))]
    fn disabled_compute_is_unavailable_and_budgets_saturate_without_queueing() {
        let disabled = Worker::new(WorkerConfig::new());
        let disabled_dispatcher =
            disabled.dispatcher(DispatcherConfig::builder().name("disabled-test").build());
        let disabled_task = disabled_dispatcher
            .reserve(TaskConfig::new())
            .expect("disabled task reservation");
        let rejected = disabled_task
            .context()
            .submit_compute(11, |_, _| {})
            .expect_err("disabled pool must reject compute");
        assert_eq!(rejected.reason(), ComputeSubmitError::Unavailable);
        assert_eq!(rejected.recover_payload(), 11);

        let pool = Arc::new(
            rayon::ThreadPoolBuilder::new()
                .num_threads(2)
                .build()
                .expect("test Rayon pool must build"),
        );
        let worker = Worker::new(
            WorkerConfig::new()
                .with_pool(pool)
                .with_max_compute_tasks(NonZeroUsize::MIN),
        );
        let dispatcher = worker.dispatcher(
            DispatcherConfig::builder()
                .name("compute-budget-test")
                .build(),
        );
        let first = dispatcher
            .reserve(TaskConfig::new().with_max_compute_tasks(NonZeroUsize::MIN))
            .expect("first task reservation");
        let second = dispatcher
            .reserve(TaskConfig::new().with_max_compute_tasks(NonZeroUsize::MIN))
            .expect("second task reservation");
        let (started, started_rx) = mpsc::channel();
        let (release, release_rx) = mpsc::channel();
        first
            .context()
            .submit_compute((), move |_, ()| {
                started.send(()).ok();
                release_rx.recv().ok();
            })
            .expect("first compute admission");
        started_rx
            .recv_timeout(Instant::now() + Duration::from_secs(2))
            .expect("first compute started");

        let rejected = first
            .context()
            .submit_compute("task-budget", |_, _| {})
            .expect_err("task budget must reject compute");
        assert_eq!(rejected.reason(), ComputeSubmitError::Saturated);
        assert_eq!(rejected.recover_payload(), "task-budget");
        let rejected = second
            .context()
            .submit_compute("worker-budget", |_, _| {})
            .expect_err("worker budget must reject compute");
        assert_eq!(rejected.reason(), ComputeSubmitError::Saturated);
        assert_eq!(rejected.recover_payload(), "worker-budget");

        release.send(()).expect("release first compute");
        let deadline = Instant::now() + Duration::from_secs(2);
        let mut payload = String::from("retry-payload");
        loop {
            match second.context().submit_compute(payload, |_, _| {}) {
                Ok(()) => break,
                Err(rejected)
                    if rejected.reason() == ComputeSubmitError::Saturated
                        && Instant::now() < deadline =>
                {
                    payload = rejected.recover_payload();
                    thread::yield_now();
                }
                Err(rejected) => panic!("compute budget did not release: {rejected:?}"),
            }
        }
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[kithara::test(native, flash(false))]
    fn cancelled_task_does_not_initialize_owned_pool() {
        let worker = Worker::new(WorkerConfig::new().with_owned_pool(RayonConfig::new(
            NonZeroUsize::MIN,
            "cancelled-compute-test",
        )));
        let dispatcher = worker.dispatcher(
            DispatcherConfig::builder()
                .name("cancelled-compute-test")
                .build(),
        );
        let pending = dispatcher
            .reserve(TaskConfig::new())
            .expect("task reservation");
        let context = pending.context().clone();

        context.control().cancel();

        let rejected = context
            .submit_compute(String::from("cancelled-payload"), |_, _| {})
            .expect_err("cancelled task must reject compute");
        assert_eq!(rejected.reason(), ComputeSubmitError::Cancelled);
        assert_eq!(rejected.recover_payload(), "cancelled-payload");
        assert!(!worker.inner.compute.pool().owned_is_initialized());
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[kithara::test(native, flash(false))]
    fn parent_cancel_reaches_worker_dispatcher_task_and_compute_job() {
        let parent = CancelScope::new(None);
        let parent_token = parent.token();
        let worker = Worker::new(
            WorkerConfig::new()
                .with_cancel(parent_token.clone())
                .with_owned_pool(RayonConfig::new(NonZeroUsize::MIN, "cancel-compute-test")),
        );
        let dispatcher = worker.dispatcher(
            DispatcherConfig::builder()
                .name("cancel-lineage-test")
                .build(),
        );
        let pending = dispatcher
            .reserve(TaskConfig::new())
            .expect("task reservation");
        let context = pending.context().clone();
        let (started, started_rx) = mpsc::channel();
        let (cancelled, cancelled_rx) = mpsc::channel();
        let task_cancelled = Arc::new(ThreadGate::default());
        let task_cancelled_since = task_cancelled.current();
        let release_cancel = Arc::new(ThreadGate::default());
        let release_cancel_since = release_cancel.current();
        let cancel_seen = Arc::clone(&task_cancelled);
        let cancel_release = Arc::clone(&release_cancel);
        let _cancel_guard = context.token().on_cancel(move || {
            cancel_seen.signal();
            assert!(
                cancel_release.wait_timeout(release_cancel_since, Duration::from_secs(2)),
                "task cancellation propagation was not released"
            );
        });
        context
            .submit_compute((), move |compute, ()| {
                started.send(()).ok();
                while !compute.cancel_group().is_cancelled() {
                    thread::yield_now();
                }
                cancelled.send(compute.token().is_cancelled()).ok();
            })
            .expect("compute admission");
        started_rx
            .recv_timeout(Instant::now() + Duration::from_secs(2))
            .expect("compute started");

        let cancel_thread = thread::spawn(move || parent_token.cancel());
        assert!(
            task_cancelled.wait_timeout(task_cancelled_since, Duration::from_secs(2)),
            "task cancellation did not reach the propagation barrier"
        );

        assert!(worker.is_cancelled());
        assert!(dispatcher.is_cancelled());
        assert!(context.token().is_cancelled());
        release_cancel.signal();
        cancel_thread
            .join()
            .expect("parent cancellation must finish");
        assert!(
            cancelled_rx
                .recv_timeout(Instant::now() + Duration::from_secs(2))
                .expect("compute cancellation propagation")
        );
    }

    #[kithara::test(native, flash(false))]
    fn final_worker_drop_cancels_derived_dispatchers_and_tasks() {
        let worker = Worker::new(WorkerConfig::new());
        let dispatcher =
            worker.dispatcher(DispatcherConfig::builder().name("worker-drop-test").build());
        let pending = dispatcher
            .reserve(TaskConfig::new())
            .expect("task reservation");
        let context = pending.context().clone();

        drop(worker);

        assert!(dispatcher.is_cancelled());
        assert!(context.token().is_cancelled());
    }
}
