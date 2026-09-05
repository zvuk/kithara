//! AVQueuePlayer-analogue orchestration layer on top of `kithara-play`.
//!
//! See `CONTEXT.md` for the public API contract and migration notes.

mod attempts;
mod config;
mod error;
mod loader;
mod navigation;
mod queue;
#[cfg(test)]
pub(crate) use kithara_bufpool::testing as test_pools;
mod track;

pub use config::{QueueConfig, QueueConfigPatch};
pub use error::QueueError;
pub use kithara_events::{QueueEvent, TrackId, TrackStatus};
pub use navigation::{NavigationState, RepeatMode};
#[cfg(any(test, feature = "probe"))]
pub use queue::test_utils;
pub use queue::{PlaybackView, Queue, QueueControl, Transition};
pub use track::{TrackEntry, TrackSource};
