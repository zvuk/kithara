#![forbid(unsafe_code)]
#![cfg_attr(all(), allow(clippy::missing_errors_doc))]
#![cfg_attr(rtsan, feature(sanitize))]

mod error;
mod guard;

pub mod api;
pub mod bridge;
pub mod engine;
pub mod player;
pub mod policy;
pub mod resource;
pub mod rt;
pub mod session;

#[cfg(target_arch = "wasm32")]
pub mod wasm;

#[cfg(any(test, feature = "mock"))]
pub mod mock;

pub use api::{
    BeatQuantum, BeatQuantumError, CrossfaderBus, DjEvent, EngineEvent, Equalizer,
    InterruptionKind, ItemEvent, ItemStatus, PlaybackDirection, PlayerEvent, PlayerStatus,
    RouteChangeReason, SessionDuckingMode, SessionEvent, SessionTransportSnapshot, SlotId,
    SyncUnavailable, Tempo, TempoError, TimeControlStatus, TimeRange, TrackBinding, TransportEvent,
    TransportRevision, WaitingReason, crossfader_gain,
};
pub use bridge::{
    AllocatedSlot, Cmd, CmdMsg, NodeInputs, PlaybackShared, PlaybackSnapshot, PlayerId,
    PlayerLevel, PlayerNotification, Reply, SessionDispatcher, SessionError, SessionHandle,
    SessionState, SharedEq, SlotControl, StartStreamFn, TrackPlaybackStopReason, TrackState,
    TrackTransition, run_cmd,
};
pub use engine::{EngineConfig, EngineImpl, apply_mix};
pub use error::PlayError;
pub use kithara_assets::{AssetLayout, DefaultLayout};
pub use kithara_audio::{
    AudioWorkerHandle, CoordinateError, EngineLoadSnapshot, EqBandConfig, SeekOutcome,
    ServiceClass, SessionBeat, StretchControls, TrackBeat, analysis::TrackAnalysis,
};
pub use kithara_net::Headers;
pub use player::{BeatStart, PlayerConfig, PlayerImpl, SelectTransition};
pub use resource::{PlaybackResamplerBackend, Resource, ResourceConfig, ResourceSrc, SourceType};
pub use rt::PlayerNode;
