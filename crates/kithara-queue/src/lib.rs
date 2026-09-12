//! AVQueuePlayer-analogue orchestration layer on top of `kithara-play`.

mod attempts;
mod config;
mod error;
mod event;
mod loader;
mod navigation;
mod queue;
#[cfg(test)]
pub(crate) use kithara_bufpool::testing as test_pools;
mod track;

pub use config::{QueueConfig, QueueConfigPatch};
pub use error::QueueError;
pub use event::{AdvanceReason, ItemEvent, QueueEvent, QueueRepeatMode, TrackStatus};
pub use kithara_events::TrackId;
pub use navigation::{NavigationState, RepeatMode};
pub use queue::{PlaybackView, Queue, QueueControl, Transition};
pub use track::{TrackEntry, TrackSource};
