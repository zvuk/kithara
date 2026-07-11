//! Flash control surface and quiescence-driven virtual clock.
//! Off-feature and wasm builds expose its inert twin.

mod api;
/// The single flash thread-local (`ThreadCtx`: mode + credit + poll depth +
/// dedicated flag) and its accessors.
mod ctx;
/// Sync-primitive diagnostics registry (provenance + holder/waiter state) for
/// the hang dump. Runtime-gated by `KITHARA_FLASH_SYNC_TRACE`.
mod diag;
/// Typed engine ids (`CvId` / `WaiterId` / `ThreadKey`) plus the
/// stateful-primitive `Backend` construction latch.
mod ids;
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
pub(crate) use ctx::{flash_ambient, flash_enabled};

pub use crate::backend::{env, logging, maybe_send};

#[cfg(test)]
mod tests;
