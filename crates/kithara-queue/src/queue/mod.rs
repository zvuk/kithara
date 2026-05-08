//! AVQueuePlayer-analogue orchestration facade.
//!
//! See `crates/kithara-queue/README.md` for the public API contract.
//! This module groups the implementation by responsibility:
//!
//! - [`mod@state`] — the [`Queue`] struct, its constructor, and the
//!   inherent helpers shared by the impl-block split (lock helpers,
//!   atomic accessors).
//! - [`mod@types`] — shared free items (`Transition`, helpers, internal shapes).
//! - [`mod@access`] — read-only API (`len`, `current`, `subscribe`, navigation getters).
//! - [`mod@lifecycle`] — track creation/removal (`append`, `insert`, `remove`, …).
//! - [`mod@selection`] — selection state machine (`select`, `advance_to_next`, …).
//! - [`mod@playback`] — runtime tick (`tick`, `position_seconds`, crossfade arming, event drain).
//! - [`mod@passthrough`] — `delegate!`-forwarded `PlayerImpl` controls.

mod access;
mod lifecycle;
mod passthrough;
mod playback;
mod selection;
mod state;
mod types;

pub use self::{state::Queue, types::Transition};
