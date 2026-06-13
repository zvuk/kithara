//! Enumerated async-channel surface (flash). The glob is dead by design — it is
//! how flash-blind primitives leak in unnoticed (design §2). Every consumed item
//! is a sim-participating sibling wrapper: [`broadcast`], [`mpsc`], [`oneshot`],
//! [`OnceCell`](oncecell::OnceCell), [`Semaphore`](semaphore::Semaphore). The
//! modules live as flat siblings (not a `sync/` directory) to stay within the
//! `max_nesting` arch limit. `Notify` is intentionally absent — it lives in the
//! `sync` facade (also flash-aware).
pub use super::{broadcast, mpsc, oncecell::OnceCell, oneshot, semaphore::Semaphore};
