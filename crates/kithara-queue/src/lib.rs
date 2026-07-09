//! AVQueuePlayer-analogue orchestration layer on top of `kithara-play`.
//!
//! See `CONTEXT.md` for the public API contract and migration notes.

mod attempts;
mod config;
mod error;
mod loader;
mod navigation;
mod queue;
mod track;

pub use config::QueueConfig;
pub use error::QueueError;
pub use kithara_events::{QueueEvent, TrackId, TrackStatus};
pub use navigation::{NavigationState, RepeatMode};
pub use queue::{PlaybackView, Queue, Transition};
pub use track::{TrackEntry, TrackSource};
