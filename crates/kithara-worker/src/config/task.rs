use std::num::NonZeroUsize;

use kithara_config::Config;
use kithara_platform::CancelGroup;

use crate::Priority;

/// Admission, cancellation, initial priority, and compute budget for one task.
#[non_exhaustive]
#[derive(Clone, Config, fieldwork::Fieldwork)]
#[config(builder(existing))]
#[fieldwork(opt_in, with)]
pub struct TaskConfig {
    #[config(value)]
    #[field(with)]
    pub(crate) max_compute_tasks: NonZeroUsize,
    #[config(skip = "composed into the task cancel group")]
    #[field(with, option_set_some)]
    pub(crate) cancel: Option<CancelGroup>,
    #[config(value)]
    priority: Priority,
}

impl TaskConfig {
    /// Create a task with no additional cancel source and priority zero.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    delegate::delegate! {
        to self {
            #[expr({ self.priority = priority; self })]
            #[must_use]
            pub fn with_priority(mut self, priority: Priority) -> Self;
        }
    }
}

impl Default for TaskConfig {
    fn default() -> Self {
        Self {
            cancel: None,
            max_compute_tasks: NonZeroUsize::MIN,
            priority: Priority::default(),
        }
    }
}
