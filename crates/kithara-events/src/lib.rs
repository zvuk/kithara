#![forbid(unsafe_code)]

//! Unified event bus for the kithara audio pipeline.

mod bus;
mod bus_event;
mod deferred;
mod event;
mod ids;
mod meta;
mod receiver;
mod scope;
mod seek;

#[cfg(feature = "abr")]
mod abr;
#[cfg(feature = "app")]
mod app;
#[cfg(feature = "asset")]
mod asset;
#[cfg(feature = "audio")]
mod audio;
#[cfg(feature = "decoder")]
mod decoder;
#[cfg(feature = "downloader")]
mod downloader;
#[cfg(feature = "drm")]
mod drm;
#[cfg(feature = "file")]
mod file;
#[cfg(feature = "hls")]
mod hls;
#[cfg(feature = "player")]
mod play;
#[cfg(feature = "queue")]
mod queue;

#[cfg(feature = "abr")]
pub use abr::{
    AbrEvent, AbrMode, AbrProgressSnapshot, AbrReason, BandwidthSource, BoundsError,
    VariantDuration, VariantIndex, VariantInfo,
};
#[cfg(feature = "app")]
pub use app::AppEvent;
#[cfg(feature = "asset")]
pub use asset::{AssetEvent, EvictReason};
#[cfg(feature = "audio")]
pub use audio::{
    AudioEvent, AudioFormat, PlaybackResamplerKind, SeekLifecycleStage, SegmentLocation,
    TrackFailureKind,
};
pub use bus::{DEFAULT_EVENT_BUS_CAPACITY, EventBus};
pub use bus_event::BusEvent;
#[cfg(feature = "decoder")]
pub use decoder::{
    AudioCodecKind, ContainerKind, DecodeErrorClass, DecodeErrorKind, DecoderBackend,
    DecoderChangeCause, DecoderEvent, FrameDomain, GaplessSpan, ResamplerKind,
};
pub use deferred::DeferredBus;
#[cfg(feature = "downloader")]
pub use downloader::{CancelReason, DownloaderEvent, RequestId, RequestMethod, RequestPriority};
#[cfg(feature = "drm")]
pub use drm::{DrmEvent, KeyFailureStage, KeySource};
pub use event::Event;
#[cfg(feature = "file")]
pub use file::{FileError, FileEvent, TotalBytesSource};
#[cfg(feature = "hls")]
pub use hls::{HlsError, HlsEvent};
pub use ids::{SlotId, TrackId};
pub use meta::{Envelope, EventMeta, ScopeLabel};
#[cfg(feature = "player")]
pub use play::{
    BpmInfo, DjEvent, EngineEvent, InterruptionKind, ItemEvent, ItemStatus, MediaTime, PlayerEvent,
    PlayerStatus, PortDescription, PortType, RouteChangeReason, RouteDescription, SessionEvent,
    TimeControlStatus, TimeRange, WaitingReason,
};
#[cfg(feature = "queue")]
pub use queue::{QueueEvent, TrackStatus};
pub use receiver::EventReceiver;
pub use scope::BusScope;
pub use seek::SeekEpoch;
