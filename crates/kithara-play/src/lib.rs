#![forbid(unsafe_code)]
#![cfg_attr(all(), allow(clippy::missing_errors_doc))]
#![cfg_attr(rtsan, feature(sanitize))]

mod error;
mod guard;
#[cfg(test)]
mod test_support;

pub mod api;
pub mod bridge;
pub mod engine;
pub mod player;
pub mod resource;
pub mod session;

#[cfg(target_arch = "wasm32")]
pub mod wasm;

#[cfg(any(test, feature = "mock"))]
pub mod mock;

pub use api::{
    DjEvent, EngineEvent, Equalizer, InterruptionKind, ItemEvent, ItemStatus, PlaybackDirection,
    PlayerEvent, PlayerStatus, RouteChangeReason, SessionBeat, SessionBeatError,
    SessionDuckingMode, SessionEvent, SessionTransportSnapshot, SlotId, SyncUnavailable, Tempo,
    TempoError, TimeControlStatus, TimeRange, TrackBinding, WaitingReason,
};
pub use bridge::{
    AllocatedSlot, Cmd, CmdMsg, NodeInputs, PlaybackShared, PlaybackSnapshot, PlayerId,
    PlayerNotification, Reply, SessionDispatcher, SessionError, SessionHandle, SessionState,
    SharedEq, SlotControl, StartStreamFn, TrackPlaybackStopReason, TrackState, TrackTransition,
    run_cmd,
};
pub use engine::{EngineConfig, EngineImpl};
pub use error::PlayError;
pub use kithara_assets::{AssetLayout, DefaultLayout};
pub use kithara_audio::{
    AudioWorkerHandle, EngineLoadSnapshot, SeekOutcome, ServiceClass, StretchControls,
};
pub use kithara_net::Headers;
pub use player::{
    PlayerConfig, PlayerImpl, PlayerNode, PlayerNodeProcessor, SelectTransition, StreamShape,
};
pub use resource::{
    PlaybackResamplerBackend, Resource, ResourceBlueprint, ResourceConfig, ResourceSrc, SourceType,
    default_resource_decoder_config,
};
