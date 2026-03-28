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
#[cfg(feature = "file")]
mod file;
#[cfg(feature = "hls")]
mod hls;
#[cfg(feature = "internal")]
pub mod internal;
#[cfg(feature = "player")]
mod play;

#[cfg(feature = "app")]
pub use app::AppEvent;
#[cfg(feature = "audio")]
pub use audio::{AudioEvent, SeekLifecycleStage};
pub use bus::{DEFAULT_EVENT_BUS_CAPACITY, EventBus};
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
pub use receiver::EventReceiver;
pub use scope::BusScope;
pub use seek::{SeekEpoch, SeekTaskId};
