#![forbid(unsafe_code)]

//! Unified event bus for the kithara audio pipeline.

mod bus;
mod event;
mod receiver;
mod scope;
mod seek;

#[cfg(feature = "app")]
mod app;
#[cfg(feature = "audio")]
mod audio;
#[cfg(feature = "downloader")]
mod downloader;
#[cfg(feature = "file")]
mod file;
#[cfg(feature = "hls")]
mod hls;
#[cfg(feature = "internal")]
pub mod internal;
#[cfg(feature = "player")]
mod play;
#[cfg(feature = "queue")]
mod queue;

#[cfg(feature = "app")]
pub use app::AppEvent;
#[cfg(feature = "audio")]
pub use audio::{AudioEvent, AudioFormat, SeekLifecycleStage};
pub use bus::{DEFAULT_EVENT_BUS_CAPACITY, EventBus};
#[cfg(feature = "downloader")]
pub use downloader::DownloaderEvent;
pub use event::Event;
#[cfg(feature = "file")]
pub use file::FileEvent;
#[cfg(feature = "hls")]
pub use hls::HlsEvent;
#[cfg(feature = "player")]
pub use play::{
    BpmInfo, DjEvent, EngineEvent, InterruptionKind, ItemEvent, ItemStatus, MediaTime, PlayerEvent,
    PlayerStatus, PortDescription, PortType, RouteChangeReason, RouteDescription, SessionEvent,
    SlotId, TimeControlStatus, TimeRange, WaitingReason,
};
#[cfg(feature = "queue")]
pub use queue::{QueueEvent, TrackId, TrackStatus};
pub use receiver::EventReceiver;
pub use scope::BusScope;
pub use seek::{SeekEpoch, SeekTaskId};
