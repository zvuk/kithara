#![forbid(unsafe_code)]
#![allow(clippy::missing_errors_doc, clippy::ignored_unit_patterns)]

#[cfg(all(feature = "backend-cpal", feature = "backend-web-audio"))]
compile_error!(
    "kithara-play: select exactly one backend feature: `backend-cpal` or `backend-web-audio`"
);

#[cfg(all(target_arch = "wasm32", feature = "backend-cpal"))]
compile_error!("kithara-play: `backend-cpal` is forbidden on wasm32; use `backend-web-audio`");

#[cfg(all(target_arch = "wasm32", not(feature = "backend-web-audio")))]
compile_error!("kithara-play: wasm32 build requires `backend-web-audio`");

#[cfg(all(not(target_arch = "wasm32"), feature = "backend-web-audio"))]
compile_error!("kithara-play: `backend-web-audio` is wasm32-only");

#[cfg(all(not(target_arch = "wasm32"), not(feature = "backend-cpal")))]
compile_error!("kithara-play: non-wasm build requires `backend-cpal`");

mod error;
mod events;
mod metadata;
mod time;
mod types;

pub mod impls;
pub mod traits;

#[cfg(any(test, feature = "test-utils"))]
pub mod mock;

pub use error::PlayError;
pub use events::{
    DjEvent, EngineEvent, InterruptionKind, ItemEvent, PlayerEvent, RouteChangeReason, SessionEvent,
};
// Concrete implementations
pub use impls::config::{ResourceConfig, ResourceSrc};
pub use impls::{
    engine::{EngineConfig, EngineImpl},
    player::{PlayerConfig, PlayerImpl},
    resource::Resource,
    source_type::SourceType,
};
#[cfg(any(feature = "file", feature = "hls"))]
pub use kithara_net::Headers;
pub use metadata::{Artwork, Metadata};
pub use time::MediaTime;
pub use traits::{
    asset::Asset,
    dj,
    dj::{
        bpm::{BeatGrid, BpmAnalyzer, BpmInfo, BpmSync},
        crossfade::{CrossfadeConfig, CrossfadeController, CrossfadeCurve},
        effects::{DjEffect, DjEffectKind},
        eq::{EqConfig, Equalizer},
    },
    engine::Engine,
    item::PlayerItem,
    mixer::Mixer,
    player::Player,
    queue::QueuePlayer,
    session::{
        AudioSession, PortDescription, PortType, RouteDescription, SessionCategory, SessionMode,
        SessionOptions,
    },
};
pub use types::{
    ActionAtItemEnd, EqBand, ItemStatus, ObserverId, PlayerStatus, SlotId, TimeControlStatus,
    TimeRange, WaitingReason,
};
