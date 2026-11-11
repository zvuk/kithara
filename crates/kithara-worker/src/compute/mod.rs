#[cfg(not(target_arch = "wasm32"))]
mod native;
mod runtime;
#[cfg(target_arch = "wasm32")]
mod wasm;

#[cfg(not(target_arch = "wasm32"))]
pub(crate) use native::ComputePool;
#[cfg(all(test, not(target_arch = "wasm32")))]
pub(crate) use native::PoolState;
pub(crate) use runtime::{Budget, ComputeRuntime};
pub use runtime::{ComputeContext, ComputeRejected, ComputeSubmitError};
#[cfg(target_arch = "wasm32")]
pub(crate) use wasm::ComputePool;
