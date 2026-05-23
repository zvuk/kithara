#![forbid(unsafe_code)]

mod core;
#[cfg(not(target_arch = "wasm32"))]
mod worker;
#[cfg(target_arch = "wasm32")]
#[path = "worker_stub.rs"]
mod worker;

pub use core::{FlushHub, FlushPolicy};
pub(crate) use core::{Flushable, signal_or_flush_sync};
