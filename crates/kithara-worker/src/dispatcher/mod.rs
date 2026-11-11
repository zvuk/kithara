#[cfg(not(target_arch = "wasm32"))]
mod completion;
mod core;
mod handle;
mod owner;
mod pending;
mod state;

#[cfg(test)]
mod tests;

pub use handle::{Dispatcher, TaskError, TaskHandle};
pub use pending::PendingTask;

#[cfg(all(test, not(target_arch = "wasm32")))]
mod completion_tests;

#[cfg(not(target_arch = "wasm32"))]
pub use completion::ShutdownError;
