//! Shared per-player ABR controller — registry, tick orchestration,
//! event throttling, and the incoherence watcher.
//!
//! Layout:
//! - `core` — [`AbrController`] struct + lifecycle/registration/peer-state
//!   callbacks + the [`AbrSettings`] config and [`AbrPeerId`] identifier.
//! - `tick` — `record_bandwidth` + `tick`. The `tick` body uses a
//!   `TickContext::resolve` Option-resolver to collapse the previous
//!   homogeneous let-else cascade — see `xtask/src/idioms/checks/guard_cascade.rs`
//!   module docs for the rationale.
//! - `throttle` — `EventThrottleCache` + `emit_throttled` + the
//!   `bytes_per_second` / `relative_delta` / `duration_delta` helpers.
//! - `incoherence` — `schedule_incoherence_watch` + `check_incoherence`.
//!   `check_incoherence` uses a single `resolve_incoherence_ctx`
//!   Option-resolver instead of the previous 4-guard cascade.
//! - `peer` — internal `PeerEntry` struct.

mod core;
mod incoherence;
mod peer;
mod throttle;
mod tick;

pub use core::{AbrController, AbrPeerId, AbrSettings};
