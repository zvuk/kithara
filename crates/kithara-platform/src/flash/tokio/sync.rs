//! Enumerated async-channel surface (flash). The glob is dead by design — it is
//! how flash-blind primitives leak in unnoticed (design §2). Every consumed item
//! is a sim-participating sibling wrapper: [`broadcast`], [`mpsc`], [`oneshot`],
//! [`watch`], [`OnceCell`](oncecell::OnceCell), [`Semaphore`](semaphore::Semaphore).
//! The modules live as flat siblings (not a `sync/` directory) to stay within
//! the `max_nesting` arch limit. `Notify` is intentionally absent — it lives in
//! the `sync` facade (also flash-aware).
//!
//! `watch` was previously the lone non-sim re-export, routing the background
//! [`AnalysisWorker`] handoff through the real `tokio` reactor. That worker runs
//! decode on a real OS thread and delivers over `watch`, so under flash the
//! receiver parked invisibly to the engine and the virtual clock raced past the
//! real CPU work to the test timeout. The sibling [`watch`] wrapper now pulls
//! that handoff onto the engine's untimed channel waiters, so every consumed
//! item here is sim-participating.
pub use super::{broadcast, mpsc, oncecell::OnceCell, oneshot, semaphore::Semaphore, watch};
