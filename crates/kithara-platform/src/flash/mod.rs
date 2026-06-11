//! Flash control surface + quiescence-driven virtual-clock engine. Compiled
//! only under `all(not(wasm32), feature = "flash")` (gated in `lib.rs`), so
//! the tree itself carries no cfg; the inert OFF/wasm forms live in
//! `crate::common::flash_inert`. See README "Flash layering".

mod api;
/// Participant credit accounting (dedicated pacers, bridged waits, blocking
/// pacer bracket) split out of `sched` — see `credit.rs`.
pub(crate) mod credit;
/// Real-I/O pacing (the `real_io` count, pace anchor maintenance and the
/// pacer thread) — see `pace.rs` and [`RealIoScope`].
mod pace;
mod participant;
/// Quiescence-driven virtual-clock engine. The engine drives `SIM_NANOS`
/// forward at quiescent points. Its consumers are the platform wait primitives
/// (`thread::park_timeout`, `sync::Condvar`, async `FlashSleep`/`Notify`) plus
/// the harness. The engine API stays `pub(crate)`.
pub(crate) mod sched;
/// Flash-side mirror of the platform tree: re-import stubs where flash
/// adds no semantics, flash-aware primitives where it does.
pub mod sync;
pub mod thread;
pub mod time;
pub mod tokio;
mod wake;

pub use api::*;

pub use crate::native::{env, logging, maybe_send};

#[cfg(test)]
mod tests;
