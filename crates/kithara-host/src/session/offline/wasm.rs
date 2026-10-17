use std::{
    panic::{AssertUnwindSafe, catch_unwind},
    task::Wake,
};

use kithara_platform::{
    CancelGroup,
    maybe_send::MaybeSend,
    sync::{Arc, Mutex, Notify},
    time::sleep,
    tokio::{select, task::spawn},
};
use kithara_worker::{PendingTask, Task, TaskContext, TaskError, TickResult};

use crate::consts::SESSION_PUMP_INTERVAL;

#[derive(Clone)]
pub(crate) struct OfflineTaskRoute {
    task: Arc<Mutex<Option<Box<dyn Task>>>>,
    notify: Arc<Notify>,
    cancel: CancelGroup,
}

pub(crate) struct OfflineTaskHandle {
    pending: PendingTask,
    route: OfflineTaskRoute,
}

impl OfflineTaskRoute {
    pub(super) fn new(pending: &PendingTask) -> Self {
        Self {
            task: Arc::new(Mutex::new(None)),
            notify: Arc::new(Notify::new()),
            cancel: pending.context().cancel_group().clone(),
        }
    }

    pub(super) fn start<T: Task>(
        self,
        pending: PendingTask,
        factory: impl FnOnce(TaskContext) -> T + MaybeSend + 'static,
    ) -> Result<OfflineTaskHandle, TaskError> {
        if self.cancel.is_cancelled() {
            return Err(TaskError::Cancelled);
        }
        let mut task = catch_unwind(AssertUnwindSafe(|| factory(pending.context().clone())))
            .map_err(|_| TaskError::Stopped)?;
        task.warm_up();
        *self.task.lock() = Some(Box::new(task));
        let route = self.clone();
        drop(spawn(async move {
            loop {
                select! {
                    _ = route.cancel.cancelled() => {},
                    _ = route.notify.notified() => {},
                    _ = sleep(SESSION_PUMP_INTERVAL) => {},
                }
                if !route.tick() {
                    break;
                }
            }
        }));
        Ok(OfflineTaskHandle {
            pending,
            route: self,
        })
    }

    pub(super) fn wake(&self) {
        self.notify.notify_one();
        self.tick();
    }

    fn tick(&self) -> bool {
        let Some(mut task) = self.task.lock().take() else {
            return false;
        };
        if self.cancel.is_cancelled() {
            task.on_cancel();
            return false;
        }
        let result = catch_unwind(AssertUnwindSafe(|| {
            task.recycle();
            task.tick()
        }));
        match result {
            Ok(TickResult::Done) => false,
            Ok(_) => {
                *self.task.lock() = Some(task);
                true
            }
            Err(_) => {
                task.on_cancel();
                tracing::error!("offline session task panicked");
                false
            }
        }
    }
}

impl Wake for OfflineTaskRoute {
    fn wake(self: Arc<Self>) {
        self.wake_by_ref();
    }

    fn wake_by_ref(self: &Arc<Self>) {
        self.notify.notify_one();
    }
}

impl Drop for OfflineTaskHandle {
    fn drop(&mut self) {
        self.pending.context().control().cancel();
        self.route.tick();
    }
}

#[cfg(test)]
mod tests {
    use std::{cell::Cell, rc::Rc};

    use kithara_test_utils::{cancel::cancel_token, kithara};

    use super::*;

    struct LocalTask {
        ticks: Rc<Cell<usize>>,
        cancelled: Rc<Cell<bool>>,
    }

    impl Task for LocalTask {
        fn tick(&mut self) -> TickResult {
            self.ticks.set(self.ticks.get() + 1);
            TickResult::Waiting
        }

        fn on_cancel(&mut self) {
            self.cancelled.set(true);
        }
    }

    #[kithara::test(wasm)]
    fn synchronous_wake_drives_and_cancels_a_non_send_task() {
        let token = cancel_token();
        let ticks = Rc::new(Cell::new(0));
        let cancelled = Rc::new(Cell::new(false));
        let route = OfflineTaskRoute {
            task: Arc::new(Mutex::new(Some(Box::new(LocalTask {
                ticks: ticks.clone(),
                cancelled: cancelled.clone(),
            })))),
            notify: Arc::new(Notify::new()),
            cancel: CancelGroup::from(token.clone()),
        };

        route.wake();
        assert_eq!(ticks.get(), 1);
        assert!(route.task.lock().is_some());

        token.cancel();
        route.wake();
        assert!(cancelled.get());
        assert_eq!(ticks.get(), 1);
        assert!(route.task.lock().is_none());
    }
}
