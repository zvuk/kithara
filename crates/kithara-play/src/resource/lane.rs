use std::task::{Context, Poll};

use kithara_platform::{CancelToken, CancelWakerGuard, sync::Arc};
use kithara_render::{LaneTask, ServiceClass};
use kithara_worker::{Priority, Task, TickResult};

/// The actual decoder lane retained by the dispatcher until release.
pub struct ResourceLane {
    task: Box<dyn LaneTask>,
    cancel: Option<CancelToken>,
    _cancel_link: Option<Arc<CancelWakerGuard>>,
}

impl ResourceLane {
    pub(crate) fn new(
        lane: impl LaneTask,
        cancel: Option<CancelToken>,
        cancel_link: Option<Arc<CancelWakerGuard>>,
    ) -> Self {
        Self {
            task: Box::new(lane),
            cancel,
            _cancel_link: cancel_link,
        }
    }
}

impl Drop for ResourceLane {
    fn drop(&mut self) {
        if let Some(cancel) = &self.cancel {
            cancel.cancel();
        }
    }
}

impl Task for ResourceLane {
    fn on_cancel(&mut self) {
        if let Some(cancel) = &self.cancel {
            cancel.cancel();
        }
        self.task.on_cancel();
    }

    delegate::delegate! {
        to self.task {
            fn priority(&self) -> Option<Priority>;
            fn recycle(&mut self);
            fn tick(&mut self) -> TickResult;
            fn warm_up(&mut self);
        }
    }
}

impl LaneTask for ResourceLane {
    delegate::delegate! {
        to self.task {
            fn preload_status(&mut self) -> Result<bool, kithara_render::LoadRefusal>;
            fn set_priority(&mut self, class: ServiceClass);
            fn poll_commands(&mut self, context: &mut Context<'_>) -> Poll<()>;
        }
    }
}
