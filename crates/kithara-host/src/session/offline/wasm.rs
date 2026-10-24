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

#[derive(Clone, fieldwork::Fieldwork)]
#[fieldwork(opt_in, vis = "pub(super)")]
pub(crate) struct OfflineTaskRoute {
    task: Arc<Mutex<Option<Box<dyn Task>>>>,
    #[field(get, deref = false)]
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
    use std::{
        cell::Cell,
        rc::Rc,
        sync::atomic::{AtomicBool, Ordering},
    };

    use kithara_platform::{sync::mpsc, thread, time::Duration};
    use kithara_play::PlayError;
    use kithara_test_utils::{cancel::cancel_token, kithara};
    use kithara_worker::{DispatcherConfig, TaskConfig, Worker, WorkerConfig};

    use super::*;
    use crate::session::{
        decks::{DeckInbox, DeckMsg},
        offline::{OfflineSessionClient, task::OfflineMsg},
    };

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

    struct WaitingTask;

    impl Task for WaitingTask {
        fn tick(&mut self) -> TickResult {
            TickResult::Waiting
        }
    }

    #[kithara::test(wasm, timeout(Duration::from_secs(2)))]
    async fn a_deck_wake_delivers_its_message() {
        let worker = Worker::new(WorkerConfig::new());
        let dispatcher = worker.dispatcher(
            DispatcherConfig::builder()
                .name("offline-deck-wake-test")
                .build(),
        );
        let pending = dispatcher.reserve(TaskConfig::new()).expect("reserve task");
        let route = OfflineTaskRoute::new(&pending);
        let _task = route
            .clone()
            .start(pending, |_| WaitingTask)
            .expect("start task");
        let (postbox, _mailbox) = kithara_command::mailbox::<(), PlayError>();
        let (sender, receiver) = mpsc::channel();
        let client = Arc::new(OfflineSessionClient::new(postbox, sender, route));

        client.waker(DeckMsg::Receipts).wake();

        assert!(matches!(
            receiver.try_recv(),
            Ok(OfflineMsg::Deck(DeckMsg::Receipts))
        ));
    }

    struct OwnerTask {
        owner: thread::ThreadId,
        wrong_thread: Arc<AtomicBool>,
    }

    impl Task for OwnerTask {
        fn tick(&mut self) -> TickResult {
            if thread::current().id() != self.owner {
                self.wrong_thread.store(true, Ordering::Release);
            }
            TickResult::Waiting
        }
    }

    #[kithara::test(wasm, timeout(Duration::from_secs(10)))]
    async fn a_deck_wake_on_a_worker_runs_the_task_on_its_owner() {
        let worker = Worker::new(WorkerConfig::new());
        let dispatcher = worker.dispatcher(
            DispatcherConfig::builder()
                .name("offline-owner-test")
                .build(),
        );
        let pending = dispatcher.reserve(TaskConfig::new()).expect("reserve task");
        let route = OfflineTaskRoute::new(&pending);
        let wrong_thread = Arc::new(AtomicBool::new(false));
        let owner = thread::current().id();
        let task_flag = wrong_thread.clone();
        let _task = route
            .clone()
            .start(pending, move |_| OwnerTask {
                owner,
                wrong_thread: task_flag,
            })
            .expect("start task");
        let (postbox, _mailbox) = kithara_command::mailbox::<(), PlayError>();
        let (sender, _receiver) = mpsc::channel();
        let client = Arc::new(OfflineSessionClient::new(postbox, sender, route));
        let waker = client.waker(DeckMsg::Receipts);
        let finished = Arc::new(AtomicBool::new(false));
        let wake_finished = finished.clone();

        drop(thread::spawn(move || {
            waker.wake();
            wake_finished.store(true, Ordering::Release);
        }));
        while !finished.load(Ordering::Acquire) {
            sleep(SESSION_PUMP_INTERVAL).await;
        }

        assert!(
            !wrong_thread.load(Ordering::Acquire),
            "offline task ran on the waking worker"
        );
    }
}
