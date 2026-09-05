use std::any::Any;

use kithara_platform::{
    CancelGroup, CancelToken, CancelWakerGuard,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};
use kithara_test_macros as kithara;
#[cfg(feature = "probe")]
use tracing as _;

use crate::{Priority, Task, TaskControl, TaskId};

pub(super) type TaskFactory =
    Box<dyn FnOnce() -> Result<Box<dyn Task>, Box<dyn Any + Send>> + Send>;

pub(super) struct Reservation {
    capacity: Arc<Capacity>,
}

pub(super) struct Capacity {
    pub(super) limit: usize,
    active: AtomicUsize,
}

impl Capacity {
    pub(super) fn new(limit: usize) -> Self {
        Self {
            limit,
            active: AtomicUsize::new(0),
        }
    }

    pub(super) fn reserve(capacity: &Arc<Self>) -> Option<Reservation> {
        capacity
            .active
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |current| {
                (current < capacity.limit).then_some(current + 1)
            })
            .ok()
            .map(|_| Reservation {
                capacity: Arc::clone(capacity),
            })
    }
}

impl Drop for Reservation {
    fn drop(&mut self) {
        self.capacity.active.fetch_sub(1, Ordering::AcqRel);
    }
}

pub(super) enum Command {
    Register(Registration),
    Unregister(TaskId),
    Shutdown,
}

pub(super) struct Registration {
    pub(super) factory: TaskFactory,
    pub(super) cancel: CancelGroup,
    pub(super) token: CancelToken,
    pub(super) priority: Priority,
    pub(super) control: TaskControl,
    pub(super) id: TaskId,
    pub(super) cancel_guards: Vec<CancelWakerGuard>,
}

impl Registration {
    pub(super) fn build_slot(self) -> Result<Slot, TaskId> {
        let Self {
            factory,
            cancel,
            token,
            priority,
            control,
            id,
            cancel_guards,
        } = self;
        let task = factory().map_err(|_| id)?;
        Ok(Slot {
            task,
            cancel,
            token,
            priority,
            control,
            id,
            _cancel_guards: cancel_guards,
            is_terminal: false,
        })
    }
}

pub(super) struct Slot {
    pub(super) task: Box<dyn Task>,
    pub(super) cancel: CancelGroup,
    pub(super) token: CancelToken,
    pub(super) priority: Priority,
    pub(super) control: TaskControl,
    pub(super) id: TaskId,
    pub(super) _cancel_guards: Vec<CancelWakerGuard>,
    pub(super) is_terminal: bool,
}

impl Slot {
    #[kithara::probe(task_id = self.id.get(), already_terminal = self.is_terminal)]
    pub(super) fn cancel(&mut self) {
        if self.is_terminal {
            return;
        }
        self.is_terminal = true;
        self.token.cancel();
        self.task.on_cancel();
        self.task.recycle();
    }
}

impl Drop for Slot {
    fn drop(&mut self) {
        self.cancel();
    }
}

#[derive(Clone, Copy)]
pub(super) struct SchedulerBudgets {
    pub(super) backpressure_poll_interval: Duration,
    pub(super) idle_timeout: Duration,
    pub(super) slow_tick_threshold: Duration,
    pub(super) wait_timeout: Duration,
    pub(super) fairness_yield_interval: u32,
    pub(super) task_burst: u32,
}
