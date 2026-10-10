use std::{
    mem,
    panic::{AssertUnwindSafe, catch_unwind},
};

use kithara_config::{Config, ConfigOwner};
use kithara_platform::{CancelToken, CancelWakerGuard, sync::Arc};

use super::{
    handle::{TaskError, TaskHandle},
    owner::DispatcherInner,
    state::{Registration, Reservation, TaskFactory},
};
use crate::{Task, TaskContext, TaskId};

/// Capacity reservation with a derived task context not yet submitted.
pub struct PendingTask {
    pub(super) inner: Arc<DispatcherInner>,
    pub(super) token: CancelToken,
    pub(super) reservation: Option<Reservation>,
    pub(super) context: TaskContext,
    pub(super) id: TaskId,
    pub(super) cancel_guards: Vec<CancelWakerGuard>,
    pub(super) submitted: bool,
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
            priority: self.context.control().config().values().priority,
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
