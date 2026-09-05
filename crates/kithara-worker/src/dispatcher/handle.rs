use std::{
    mem,
    panic::{AssertUnwindSafe, catch_unwind},
};

use kithara_platform::{
    CancelGroup, CancelToken, CancelWakerGuard,
    sync::{
        Arc, Mutex, Weak,
        atomic::{AtomicU64, Ordering},
        mpsc::{self},
    },
    thread::spawn_named,
    tokio::runtime::Handle,
};

use super::{
    core::run_loop,
    state::{Capacity, Command, Registration, Reservation, SchedulerBudgets, TaskFactory},
};
use crate::{
    DispatcherConfig, Task, TaskConfig, TaskContext, TaskControl, TaskId, Wake,
    compute::{Budget, ComputeRuntime},
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
        let DispatcherConfig {
            backpressure_poll_interval,
            cancel: domain_cancel,
            capacity,
            fairness_yield_interval,
            idle_timeout,
            name,
            observer,
            slow_tick_threshold,
            task_burst,
            wait_timeout,
        } = config;
        let budgets = SchedulerBudgets {
            backpressure_poll_interval,
            idle_timeout,
            slow_tick_threshold,
            wait_timeout,
            fairness_yield_interval: fairness_yield_interval.get(),
            task_burst: task_burst.get(),
        };
        let inner = Arc::new(DispatcherInner {
            cmd_tx,
            compute,
            runtime,
            admission: Mutex::new(Admission::Open),
            cancel: cancel.clone(),
            capacity: Arc::new(Capacity::new(capacity.get())),
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
            run_loop(&cmd_rx, &wake, &cancel, budgets, observer);
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

struct DispatcherInner {
    capacity: Arc<Capacity>,
    compute: Arc<ComputeRuntime>,
    next_id: AtomicU64,
    cancel: CancelToken,
    admission: Mutex<Admission>,
    runtime: Option<Handle>,
    cmd_tx: mpsc::Sender<Command>,
    wake: Wake,
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum Admission {
    Open,
    Closed,
}

impl DispatcherInner {
    fn register(&self, registration: Registration) -> Result<(), TaskError> {
        let mut admission = self.admission.lock();
        if *admission == Admission::Closed || self.cancel.is_cancelled() {
            drop(admission);
            drop(registration);
            return Err(TaskError::Stopped);
        }
        let sent = self.cmd_tx.send(Command::Register(registration));
        if sent.is_err() {
            *admission = Admission::Closed;
        }
        drop(admission);
        sent.map_err(|_| TaskError::Stopped)
    }

    fn reserve(self: &Arc<Self>, config: TaskConfig) -> Result<PendingTask, TaskError> {
        let admission = self.admission.lock();
        if *admission == Admission::Closed || self.cancel.is_cancelled() {
            return Err(TaskError::Stopped);
        }
        let reservation = Capacity::reserve(&self.capacity).ok_or(TaskError::Capacity {
            capacity: self.capacity.limit,
        })?;
        drop(admission);
        let id = TaskId::new(self.next_id.fetch_add(1, Ordering::Relaxed));
        let token = self.cancel.child();
        let cancel = config.cancel.map_or_else(
            || CancelGroup::from(token.clone()),
            |domain| CancelGroup::from(token.clone()) | domain,
        );
        let control = TaskControl::new(config.priority, token.clone(), self.wake.clone());
        let context = TaskContext::new(
            cancel.clone(),
            Arc::clone(&self.compute),
            Arc::new(Budget::new(config.max_compute_tasks)),
            control,
            self.runtime.clone(),
            token.clone(),
        );
        let task_wake = self.wake.clone();
        let task_token = token.clone();
        let cancel_guards = cancel.on_cancel(move || {
            task_token.cancel();
            task_wake.wake();
        });

        if cancel.is_cancelled() {
            return Err(TaskError::Cancelled);
        }

        Ok(PendingTask {
            cancel_guards,
            context,
            id,
            token,
            inner: Arc::clone(self),
            reservation: Some(reservation),
            submitted: false,
        })
    }

    fn shutdown(&self) {
        let mut admission = self.admission.lock();
        if *admission == Admission::Open {
            *admission = Admission::Closed;
            self.cmd_tx.send(Command::Shutdown).ok();
        }
        drop(admission);
        self.cancel.cancel();
        self.wake.wake();
    }

    fn unregister(&self, id: TaskId) {
        let admission = self.admission.lock();
        if *admission == Admission::Open {
            self.cmd_tx.send(Command::Unregister(id)).ok();
        }
        drop(admission);
        self.wake.wake();
    }
}

impl Drop for DispatcherInner {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// Capacity reservation with a derived task context not yet submitted.
pub struct PendingTask {
    inner: Arc<DispatcherInner>,
    token: CancelToken,
    reservation: Option<Reservation>,
    context: TaskContext,
    id: TaskId,
    cancel_guards: Vec<CancelWakerGuard>,
    submitted: bool,
}

impl PendingTask {
    /// Context available before task construction and submission.
    #[must_use]
    pub const fn context(&self) -> &TaskContext {
        &self.context
    }

    /// Stable task identifier assigned with the reservation.
    #[must_use]
    pub const fn id(&self) -> TaskId {
        self.id
    }

    /// Construct and submit the task while preserving the reserved context.
    ///
    /// # Errors
    ///
    /// Returns [`TaskError::Cancelled`] if cancellation raced construction or
    /// [`TaskError::Stopped`] if the dispatcher stopped before submission.
    pub fn start<T, F>(self, factory: F) -> Result<TaskHandle, TaskError>
    where
        T: Task + Send,
        F: FnOnce(TaskContext) -> T,
    {
        if self.context.cancel_group().is_cancelled() {
            return Err(TaskError::Cancelled);
        }
        let task = Box::new(factory(self.context.clone()));
        if self.context.cancel_group().is_cancelled() {
            return Err(TaskError::Cancelled);
        }
        let factory: TaskFactory = Box::new(move || Ok(task));
        self.submit(factory)
    }

    /// Construct and submit a thread-bound task on the dispatcher thread.
    ///
    /// # Errors
    ///
    /// Returns [`TaskError::Cancelled`] if cancellation raced submission or
    /// [`TaskError::Stopped`] if the dispatcher stopped before submission.
    pub fn start_local<T, F>(self, factory: F) -> Result<TaskHandle, TaskError>
    where
        T: Task,
        F: FnOnce(TaskContext) -> T + Send + 'static,
    {
        let context = self.context.clone();
        let factory: TaskFactory = Box::new(move || {
            catch_unwind(AssertUnwindSafe(|| {
                Box::new(factory(context)) as Box<dyn Task>
            }))
        });
        self.submit(factory)
    }

    fn submit(mut self, factory: TaskFactory) -> Result<TaskHandle, TaskError> {
        if self.context.cancel_group().is_cancelled() {
            return Err(TaskError::Cancelled);
        }
        let Some(reservation) = self.reservation.take() else {
            return Err(TaskError::Stopped);
        };
        let registration = Registration {
            factory,
            cancel_guards: mem::take(&mut self.cancel_guards),
            cancel: self.context.cancel_group().clone(),
            control: self.context.control(),
            id: self.id,
            priority: self.context.control().priority(),
            token: self.token.clone(),
        };
        self.inner.register(registration)?;
        let handle = TaskHandle {
            _reservation: reservation,
            control: self.context.control(),
            id: self.id,
            inner: Arc::downgrade(&self.inner),
            token: self.token.clone(),
        };
        self.submitted = true;
        self.inner.wake.wake();
        Ok(handle)
    }
}

impl Drop for PendingTask {
    fn drop(&mut self) {
        if !self.submitted {
            self.token.cancel();
        }
    }
}

/// Non-cloneable ownership handle for one admitted task.
pub struct TaskHandle {
    token: CancelToken,
    _reservation: Reservation,
    control: TaskControl,
    id: TaskId,
    inner: Weak<DispatcherInner>,
}

impl TaskHandle {
    /// Stable task identifier.
    #[must_use]
    pub const fn id(&self) -> TaskId {
        self.id
    }

    delegate::delegate! {
        to self.control {
            /// Clone the restricted priority and wake control.
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
