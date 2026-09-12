#![forbid(unsafe_code)]
#![cfg_attr(all(), allow(clippy::missing_errors_doc))]
#![cfg_attr(all(rtsan, not(rtsan_standalone)), feature(sanitize))]

mod error;
mod guard;
#[cfg(test)]
pub(crate) use kithara_bufpool::testing as test_pools;

pub mod api;
pub mod bridge;
pub mod effects;
pub mod engine;
pub mod player;
pub mod policy;
pub mod resource;
pub mod rt;
pub mod session;
pub mod sync;
pub mod worker;

#[cfg(target_arch = "wasm32")]
pub mod wasm;

#[cfg(any(test, feature = "mock"))]
pub mod mock;

pub use api::{
    BpmInfo, DjEvent, EngineEvent, Equalizer, InterruptionKind, ItemRole, ItemStatus, MediaTime,
    PlaybackDirection, PlayerEvent, PlayerStatus, PortDescription, PortType, RouteChangeReason,
    RouteDescription, SessionBeat, SessionEvent, SessionTransportSnapshot, SlotId,
    StretchBackendKind, SyncUnavailable, Tempo, TempoError, TimeControlStatus, TimeRange,
    TrackBinding, TrackRef, TransportRevision, WaitingReason,
};
pub use bridge::{
    AllocatedSlot, Cmd, MixTapWriter, NodeInputs, PlaybackShared, PlaybackSnapshot, PlayerId,
    PlayerLevel, PlayerNotification, Reply, SessionBinding, SessionDispatcher, SessionError,
    SessionHandle, SessionSampleRate, SharedEq, SlotControl, TrackPlaybackStopReason, TrackState,
    TrackTransition,
};
pub use effects::eq::EqBandConfig;
pub use engine::{DEFAULT_GATE_SMOOTHING, EngineConfig, EngineImpl, apply_mix};
pub use error::PlayError;
pub use kithara_assets::{AssetLayout, DefaultLayout};
pub use kithara_audio::SeekOutcome;
pub use kithara_net::Headers;
pub use kithara_warp::{
    BeatGrid, BeatGridId, BeatGridSnapshot, StretchControls, SyncAdmission, SyncApplied, SyncError,
    SyncGroup, SyncGroupSnapshot, SyncOperation, SyncRejected, SyncStatusSnapshot,
};
pub use player::{PlayerConfig, PlayerConfigPatch, PlayerImpl, SelectTransition};
pub use resource::{PlaybackResamplerBackend, Resource, ResourceConfig, ResourceSrc, SourceType};
pub use rt::{PlayerNode, StreamShape};
pub use sync::GroupState;
pub use worker::{
    EngineLoad, EngineLoadSnapshot, PlayWorker, PlayWorkerConfig, PlayWorkerConfigPatch,
    RegisteredAudio, ServiceClass, TrackConfig,
};
