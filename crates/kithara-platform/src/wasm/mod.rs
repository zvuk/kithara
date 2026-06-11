//! wasm32 backend. Mirrors the facade tree 1:1; cross-platform code is
//! re-imported from `crate::common`. Compiled only on wasm32 (gated in
//! `lib.rs`), so the tree itself carries no cfg.

pub mod logging;
pub mod maybe_send;
pub mod sync;
pub mod thread;
pub mod time;
pub mod tokio;
