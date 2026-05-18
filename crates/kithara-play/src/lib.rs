#![forbid(unsafe_code)]
#![cfg_attr(all(), allow(clippy::missing_errors_doc))]

#[cfg(all(target_arch = "wasm32", not(feature = "backend-web-audio")))]
compile_error!("kithara-play: wasm32 build requires `backend-web-audio`");

#[cfg(all(target_arch = "wasm32", not(feature = "wasm-bindgen")))]
compile_error!("kithara-play: wasm32 build requires `wasm-bindgen`");

#[cfg(all(not(target_arch = "wasm32"), not(feature = "backend-cpal")))]
compile_error!("kithara-play: non-wasm build requires `backend-cpal`");

mod error;
mod events;
mod metadata;
mod time;
mod types;

#[cfg(any(test, feature = "probe"))]
pub mod test_helpers;

pub mod impls;
pub mod traits;

#[cfg(target_arch = "wasm32")]
pub mod wasm_support;

#[cfg(any(test, feature = "mock"))]
pub mod mock;

pub use error::PlayError;
pub use events::{
    DjEvent, EngineEvent, InterruptionKind, ItemEvent, PlayerEvent, RouteChangeReason, SessionEvent,
};
pub use impls::{
    config::{ResourceConfig, ResourceSrc},
    engine::{EngineConfig, EngineImpl},
    player::{PlayerConfig, PlayerImpl},
    player_node::PlayerNode,
    resource::Resource,
    session_engine::{
        AllocatedSlot, Cmd, CmdMsg, PlayerId, Reply, SessionDispatcher, SessionState,
        StartStreamFn, run_cmd,
    },
    shared_eq::SharedEq,
    source_type::SourceType,
};
pub use kithara_audio::{AudioWorkerHandle, SeekOutcome, ServiceClass};
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
        eq::Equalizer,
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
    ActionAtItemEnd, ItemStatus, ObserverId, PlayerStatus, SessionDuckingMode, SlotId,
    TimeControlStatus, TimeRange, WaitingReason,
};
