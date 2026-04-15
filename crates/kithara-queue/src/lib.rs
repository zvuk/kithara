//! AVQueuePlayer-analogue orchestration layer on top of `kithara-play`.
//!
//! See `README.md` for the public API contract and migration notes.

mod config;
mod error;
mod events;
mod loader;
mod navigation;
mod queue;
mod track;

pub use config::QueueConfig;
pub use error::QueueError;
pub use events::QueueEvent;
pub use navigation::{NavigationState, RepeatMode};
pub use queue::Queue;
pub use track::{TrackEntry, TrackId, TrackSource, TrackStatus};
