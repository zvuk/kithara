mod dispatcher;
mod task;
mod worker;

pub use dispatcher::{DispatcherConfig, DispatcherConfigPatch};
pub use task::TaskConfig;
pub(crate) use worker::PoolConfig;
#[cfg(not(target_arch = "wasm32"))]
pub use worker::RayonConfig;
pub use worker::{ComputePool, WorkerConfig, WorkerConfigPatch};
