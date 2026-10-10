//! AVQueuePlayer-analogue orchestration layer on top of `kithara-play`.

mod config;
mod error;
mod event;
mod loader;
mod loading;
mod navigation;
mod queue;
#[cfg(test)]
pub(crate) use kithara_test_utils::bufpool as test_pools;
mod track;

pub use config::{QueueConfig, QueueConfigPatch, QueueSettings, QueueSettingsChange};
pub use error::QueueError;
pub use event::{AdvanceReason, ItemEvent, QueueEvent, QueueRepeatMode, TrackStatus};
pub use kithara_events::TrackId;
pub use kithara_play::{CrossfadeCurve, CrossfadeSettings, SelectionPlayback};
#[doc(hidden)]
pub use loading::LoadReport;
pub use navigation::{ActionAtItemEnd, NavigationState, PlaybackOrder, RepeatMode};
pub use queue::{PlaybackView, Queue, QueueCommand, QueueControl, QueueSnapshot, Transition};
pub use track::{TrackEntry, TrackSource};
mod consts;
