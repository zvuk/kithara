pub(crate) mod backend;
mod client;
#[cfg(all(test, not(target_arch = "wasm32")))]
pub(crate) mod mock;
#[cfg(not(target_arch = "wasm32"))]
mod native;
mod task;
#[cfg(all(test, not(target_arch = "wasm32")))]
pub(crate) mod tests;
#[cfg(target_arch = "wasm32")]
mod wasm;

pub(crate) use client::OfflineSessionClient;
#[cfg(not(target_arch = "wasm32"))]
pub(crate) use native::{OfflineTaskHandle, OfflineTaskRoute};
pub(crate) use task::{OfflineSessionError, OfflineTaskConfig, spawn};
#[cfg(target_arch = "wasm32")]
pub(crate) use wasm::{OfflineTaskHandle, OfflineTaskRoute};
