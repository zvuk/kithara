//! Pure native backend. Knows nothing about the other backends;
//! cross-platform code lives in `crate::common` and is re-imported here.
//! Compiled only off wasm32 (gated in `lib.rs`), so the tree itself
//! carries no cfg.

pub mod env;
pub mod logging;
pub mod maybe_send;
pub mod sync;
pub mod thread;
pub mod time;
pub mod tokio;
