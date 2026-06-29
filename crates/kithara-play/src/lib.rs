#![forbid(unsafe_code)]
#![cfg_attr(all(), allow(clippy::missing_errors_doc))]
#![cfg_attr(rtsan, feature(sanitize))]

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
    player::{PlayerConfig, PlayerImpl, SelectTransition},
    player_node::PlayerNode,
    resource::Resource,
    session::{
        AllocatedSlot, Cmd, CmdMsg, PlayerId, Reply, SessionDispatcher, SessionState,
        StartStreamFn, run_cmd,
    },
    shared_eq::SharedEq,
    shared_player_state::PlaybackSnapshot,
    source_type::SourceType,
};
pub use kithara_audio::{
    AudioWorkerHandle, EngineLoadSnapshot, SeekOutcome, ServiceClass, StretchControls,
};
pub use kithara_net::Headers;
pub use metadata::{Artwork, Metadata};
pub use time::MediaTime;
pub use traits::{
    dj,
    dj::{
        bpm::{BeatGrid, BpmInfo, GridSegment},
        crossfade::{CrossfadeConfig, CrossfadeCurve},
        eq::Equalizer,
    },
    engine::Engine,
    item::PlayerItem,
    player::Player,
    queue::QueuePlayer,
    session::{PortDescription, PortType, RouteDescription},
};
pub use types::{
    ItemStatus, ObserverId, PlayerStatus, SessionDuckingMode, SlotId, TimeControlStatus, TimeRange,
    WaitingReason,
};
