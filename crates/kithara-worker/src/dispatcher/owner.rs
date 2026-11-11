use kithara_platform::{
    CancelGroup, CancelToken,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
        mpsc,
    },
    tokio::runtime::Handle,
};

use super::{
    handle::TaskError,
    pending::PendingTask,
    state::{Capacity, Command, Registration},
};
use crate::{
    DispatcherConfig, TaskConfig, TaskContext, TaskControl, TaskId, Wake,
    compute::{Budget, ComputeRuntime},
};

pub(super) struct DispatcherInner {
    pub(super) config: Arc<DispatcherConfig>,
    pub(super) capacity: Arc<Capacity>,
    pub(super) compute: Arc<ComputeRuntime>,
    pub(super) next_id: AtomicU64,
    pub(super) cancel: CancelToken,
    pub(super) admission: Mutex<Admission>,
    pub(super) runtime: Option<Handle>,
    pub(super) cmd_tx: mpsc::Sender<Command>,
    pub(super) wake: Wake,
}

#[derive(Clone, Copy, Eq, PartialEq)]
pub(super) enum Admission {
    Open,
    Closed,
}

impl DispatcherInner {
    pub(super) fn register(&self, registration: Registration) -> Result<(), TaskError> {
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

    pub(super) fn reserve(self: &Arc<Self>, config: TaskConfig) -> Result<PendingTask, TaskError> {
        let admission = self.admission.lock();
        if *admission == Admission::Closed || self.cancel.is_cancelled() {
            return Err(TaskError::Stopped);
        }
        let limit = self.config.capacity.get();
        let reservation = Capacity::reserve(&self.capacity, limit)
            .ok_or(TaskError::Capacity { capacity: limit })?;
        drop(admission);
        let id = TaskId::new(self.next_id.fetch_add(1, Ordering::Relaxed));
        let token = self.cancel.child();
        let cancel = config.cancel.clone().map_or_else(
            || CancelGroup::from(token.clone()),
            |domain| CancelGroup::from(token.clone()) | domain,
        );
        let control = TaskControl::new(Arc::new(config), token.clone(), self.wake.clone());
        let context = TaskContext::new(
            cancel.clone(),
            Arc::clone(&self.compute),
            Arc::new(Budget::default()),
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

    pub(super) fn shutdown(&self) {
        let mut admission = self.admission.lock();
        if *admission == Admission::Open {
            *admission = Admission::Closed;
            self.cmd_tx.send(Command::Shutdown).ok();
        }
        drop(admission);
        self.cancel.cancel();
        self.wake.wake();
    }

    pub(super) fn unregister(&self, id: TaskId) {
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
