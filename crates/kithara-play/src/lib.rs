#![forbid(unsafe_code)]
#![cfg_attr(all(), allow(clippy::missing_errors_doc))]
#![cfg_attr(rtsan, feature(sanitize))]

mod error;
mod events;
mod guard;
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
pub use kithara_assets::{AssetLayout, DefaultLayout};
pub use kithara_audio::{
    AudioWorkerHandle, EngineLoadSnapshot, SeekOutcome, ServiceClass, StretchControls,
};
pub use kithara_net::Headers;
pub use traits::dj::eq::Equalizer;
pub use types::{
    ItemStatus, PlayerStatus, SessionDuckingMode, SlotId, TimeControlStatus, TimeRange,
    WaitingReason,
};
