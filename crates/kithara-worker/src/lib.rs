mod compute;
mod config;
mod dispatcher;
mod observer;
mod task;
#[cfg(not(target_arch = "wasm32"))]
mod threads;
mod wake;

#[cfg(any(test, feature = "mock"))]
pub mod mock;
mod worker;

pub use compute::{ComputeContext, ComputeRejected, ComputeSubmitError};
pub use config::{
    ComputePool, DispatcherConfig, DispatcherConfigPatch, OwnedPoolConfig, TaskConfig,
    WorkerConfig, WorkerConfigPatch,
};
pub use dispatcher::{Dispatcher, PendingTask, TaskError, TaskHandle};
use humantime_serde as _;
pub use observer::{Event, Observer, PassOutcome, PassReport};
pub use task::{Priority, Task, TaskContext, TaskControl, TaskId, TickResult};
#[cfg(not(target_arch = "wasm32"))]
pub use threads::ShutdownError;
pub use wake::Wake;
pub use worker::Worker;
mod consts;
