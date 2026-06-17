//! Flash control surface + quiescence-driven virtual-clock engine. Compiled
//! only under `all(not(wasm32), feature = "flash")` (gated in `lib.rs`), so
//! the tree itself carries no cfg; the inert OFF/wasm forms live in
//! `crate::common::flash_inert`. See CONTEXT.md "Virtual time (`flash`)".

mod api;
/// The single flash thread-local (`ThreadCtx`: mode + credit + poll depth +
/// dedicated flag) and its accessors.
mod ctx;
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

pub use crate::native::{env, logging, maybe_send};

#[cfg(test)]
mod tests;
