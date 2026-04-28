//! Per-peer ABR state, decision logic, and supporting types.
//!
//! Layout:
//! - `core` — `AbrState` struct with accessors and commands (the bag of
//!   atomics + a mutex for the variant list).
//! - `decision` — pure decision function (`evaluate`) that turns an
//!   `(AbrState, AbrView, Instant)` triple into an `AbrDecision`. Refactored
//!   from a heterogeneous guard cascade into parallel compute + a single
//!   tuple-match — see file-level docs there for the rationale.
//! - `view` — `AbrView<'a>` snapshot of the inputs the decision needs.
//! - `error` — `AbrError` for the few state mutations that can fail.
//! - `tests` — unit + property tests for `AbrState` / `evaluate`
//!   (cfg-test only). Includes the `test_variants_3` fixture as a
//!   private helper — no public test surface leaks out of the crate.

mod core;
mod decision;
mod error;
mod view;

#[cfg(test)]
mod tests;

pub use core::AbrState;

pub use decision::AbrDecision;
pub use error::AbrError;
pub use view::AbrView;
