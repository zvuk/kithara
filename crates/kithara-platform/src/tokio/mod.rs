//! Cross-platform tokio — real `tokio` on native, WASM shims on `wasm32`.
//!
//! Re-exports everything from `tokio_with_wasm::alias` (which itself gates
//! per platform), with a custom `task` module for Web Worker lifecycle.
//!
//! Usage: `use kithara_platform::tokio;`

pub use tokio_with_wasm::alias::*;

pub mod task;
