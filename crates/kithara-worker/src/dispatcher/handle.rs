use kithara_platform::{
    CancelGroup, CancelToken,
    sync::{Arc, Mutex, Weak, atomic::AtomicU64, mpsc},
    thread::spawn_named,
    time::Duration,
    tokio::runtime::Handle,
};

use super::{
    core::run_loop,
    owner::{Admission, DispatcherInner},
    pending::PendingTask,
    state::{Capacity, Reservation},
};
use crate::{
    DispatcherConfig, Task, TaskConfig, TaskContext, TaskControl, TaskId, Wake,
    compute::ComputeRuntime,
};

/// Task admission or dispatcher lifecycle failure.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum TaskError {
    /// The configured dispatcher task capacity has been reached.
    #[error("dispatcher capacity {capacity} reached")]
    Capacity { capacity: usize },
    /// A task cancellation source fired before submission completed.
    #[error("task was cancelled before submission")]
    Cancelled,
    /// The dispatcher thread has stopped.
    #[error("dispatcher stopped")]
    Stopped,
}

/// Cloneable handle to one dedicated scheduler thread.
#[derive(Clone)]
pub struct Dispatcher {
    inner: Arc<DispatcherInner>,
}

impl Dispatcher {
    /// Longest park duration before the dispatcher checks for new work.
    #[must_use]
    pub fn wake_allowance(&self) -> Duration {
        let config = &self.inner.config;
        config
            .wait_timeout
            .max(config.idle_timeout)
            .max(config.backpressure_poll_interval)
    }

    /// Return whether this dispatcher subtree has been cancelled.
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.inner.cancel.is_cancelled()
    }

    /// Reserve and submit a task in one call.
    ///
    /// # Errors
    ///
    /// Returns a task admission or dispatcher lifecycle failure.
    pub fn register<T, F>(&self, config: TaskConfig, factory: F) -> Result<TaskHandle, TaskError>
    where
        T: Task + Send,
        F: FnOnce(TaskContext) -> T,
    {
        self.reserve(config)?.start(factory)
    }

    pub(crate) fn start(
        config: DispatcherConfig,
        cancel: CancelToken,
        compute: Arc<ComputeRuntime>,
        runtime: Option<Handle>,
    ) -> Self {
        let (cmd_tx, cmd_rx) = mpsc::channel();
        let wake = Wake::default();
        let observer = config
            .observer
            .lock()
            .take()
            .expect("dispatcher config always starts with an observer");
        let domain_cancel = config.cancel.clone();
        let name = config.name.clone();
        let config = Arc::new(config);
        let inner = Arc::new(DispatcherInner {
            config: Arc::clone(&config),
            cmd_tx,
            compute,
            runtime,
            admission: Mutex::new(Admission::Open),
            cancel: cancel.clone(),
            capacity: Arc::new(Capacity::default()),
            next_id: AtomicU64::new(1),
            wake: wake.clone(),
        });
        let cancel_group = domain_cancel.map_or_else(
            || CancelGroup::from(cancel.clone()),
            |domain| CancelGroup::from(cancel.clone()) | domain,
        );
        let cancel_wake = wake.clone();
        let cancel_token = cancel.clone();
        let cancel_guards = cancel_group.on_cancel(move || {
            cancel_token.cancel();
            cancel_wake.wake();
        });

        spawn_named(name, move || {
            let _cancel_guards = cancel_guards;
            run_loop(&cmd_rx, &wake, &cancel, &config, observer);
        });

        Self { inner }
    }

    /// Restricted immediate and deferred wake capability.
    #[must_use]
    pub fn wake_handle(&self) -> Wake {
        self.inner.wake.clone()
    }

    delegate::delegate! {
        to self.inner {
            /// Reserve capacity and derive a task context before constructing the task.
            ///
            /// # Errors
            ///
            /// Returns [`TaskError::Capacity`] when admission is full,
            /// [`TaskError::Cancelled`] when a cancellation source already fired, or
            /// [`TaskError::Stopped`] after dispatcher shutdown.
            pub fn reserve(&self, config: TaskConfig) -> Result<PendingTask, TaskError>;
            /// Cancel this dispatcher subtree and wake its scheduler thread.
            pub fn shutdown(&self);
        }
    }
}

/// Non-cloneable ownership handle for one admitted task.
pub struct TaskHandle {
    pub(super) token: CancelToken,
    pub(super) _reservation: Reservation,
    pub(super) control: TaskControl,
    pub(super) id: TaskId,
    pub(super) inner: Weak<DispatcherInner>,
}

impl TaskHandle {
    /// Stable task identifier.
    #[must_use]
    pub const fn id(&self) -> TaskId {
        self.id
    }

    delegate::delegate! {
        to self.control {
            /// Clone the restricted cancellation and wake control.
            #[must_use]
            #[call(clone)]
            pub fn control(&self) -> TaskControl;
            /// Cancel only this task subtree.
            pub fn cancel(&self);
        }
    }
}

impl Drop for TaskHandle {
    fn drop(&mut self) {
        self.token.cancel();
        if let Some(inner) = self.inner.upgrade() {
            inner.unregister(self.id);
        }
    }
}
