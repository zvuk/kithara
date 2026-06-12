//! Flash control surface + quiescence-driven virtual-clock engine. Compiled
//! only under `all(not(wasm32), feature = "flash")` (gated in `lib.rs`), so
//! the tree itself carries no cfg; the inert OFF/wasm forms live in
//! `crate::common::flash_inert`. See README "Flash layering".

mod api;
mod participant;
/// Flash-side mirror of the platform tree: re-import stubs where flash
/// adds no semantics, flash-aware primitives where it does.
pub mod sync;
/// Quiescence engine internals (scheduler, credit, pacing, wake handles,
/// task gate) — see `system/mod.rs`.
mod system;
pub mod thread;
pub mod time;
pub mod tokio;

pub use api::*;

pub use crate::native::{env, logging, maybe_send};

#[cfg(test)]
mod tests;
