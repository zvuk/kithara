//! Async synchronization primitives.
//!
//! Re-exports everything from `tokio_with_wasm::alias::sync`, with
//! sim-participating [`mpsc`] and [`oneshot`] under `sim-time` (native): those
//! shadow the real `tokio` channels so the download/peer stack's send→recv
//! handoffs run through the quiescence engine and the virtual clock cannot race
//! past a channel handoff. Off the sim path the glob re-export provides the real
//! `tokio` channels unchanged.

pub use tokio_with_wasm::alias::sync::*;

#[cfg(all(not(target_arch = "wasm32"), feature = "sim-time"))]
pub mod mpsc;
#[cfg(all(not(target_arch = "wasm32"), feature = "sim-time"))]
pub mod oneshot;
