//! Flash control surface. Engine under `feature = "flash"` (native only),
//! inert ZST/identity forms otherwise. See README "Flash layering".
#[cfg(all(not(target_arch = "wasm32"), feature = "flash"))]
mod api;
#[cfg(all(not(target_arch = "wasm32"), feature = "flash"))]
pub use api::*;

#[cfg(not(all(not(target_arch = "wasm32"), feature = "flash")))]
mod inert;
#[cfg(not(all(not(target_arch = "wasm32"), feature = "flash")))]
pub use inert::*;

/// Quiescence-driven virtual-clock engine. The engine drives `SIM_NANOS`
/// forward at quiescent points. Its consumers are the platform wait primitives
/// (`thread::park_timeout`, `sync::Condvar`, async `FlashSleep`/`Notify`) plus
/// the harness, so it compiles whenever `flash` is on. The engine API stays
/// `pub(crate)`.
#[cfg(all(not(target_arch = "wasm32"), feature = "flash"))]
pub(crate) mod sched;

/// Participant credit accounting (dedicated pacers, bridged waits, blocking
/// pacer bracket) split out of `sched` — see `credit.rs`.
#[cfg(all(not(target_arch = "wasm32"), feature = "flash"))]
pub(crate) mod credit;
/// Real-I/O pacing (the `real_io` count, pace anchor maintenance and the
/// pacer thread) — see `pace.rs` and [`RealIoScope`].
#[cfg(all(not(target_arch = "wasm32"), feature = "flash"))]
mod pace;
#[cfg(all(not(target_arch = "wasm32"), feature = "flash"))]
mod participant;
/// Flash-side mirror of the `sync` facade tree: re-import stubs where flash
/// adds no semantics, flash-aware primitives where it does.
#[cfg(all(not(target_arch = "wasm32"), feature = "flash"))]
pub(crate) mod sync;
#[cfg(all(not(target_arch = "wasm32"), feature = "flash"))]
pub(crate) mod time;
#[cfg(all(not(target_arch = "wasm32"), feature = "flash"))]
mod wake;

#[cfg(all(not(target_arch = "wasm32"), feature = "flash"))]
#[cfg(test)]
mod tests;
