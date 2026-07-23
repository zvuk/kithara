#[cfg(not(target_arch = "wasm32"))]
mod atomic;
mod flush;
pub mod schema;
#[cfg(not(target_arch = "wasm32"))]
mod worker;
#[cfg(target_arch = "wasm32")]
#[path = "worker_stub.rs"]
mod worker;

#[cfg(not(target_arch = "wasm32"))]
pub(crate) use atomic::{init_atomic, open_existing};
pub use flush::{FlushHub, FlushPolicy};
pub(crate) use flush::{Flushable, flush_sync};
