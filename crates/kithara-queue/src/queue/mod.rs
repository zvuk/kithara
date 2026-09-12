//! AVQueuePlayer-analogue orchestration facade.
//!
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
mod engine_events;
mod lifecycle;
mod owner;
mod passthrough;
mod playback;
mod player;
mod selection;
mod state;
mod types;

#[cfg(test)]
pub(crate) use state::tests::{TEST_SAMPLE_RATE, test_session};

pub use self::{
    state::{Queue, QueueControl},
    types::{PlaybackView, Transition},
};
