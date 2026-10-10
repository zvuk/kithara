#![forbid(unsafe_code)]
#![cfg_attr(all(), allow(clippy::missing_errors_doc))]
#![cfg_attr(all(rtsan, not(rtsan_standalone)), feature(sanitize))]

mod error;
mod guard;
#[cfg(test)]
pub(crate) use kithara_test_utils::bufpool as test_pools;

pub mod api;
pub mod player;
pub mod policy;
pub mod resource;
pub mod session;

#[cfg(target_arch = "wasm32")]
pub mod wasm;

#[cfg(any(test, feature = "mock"))]
pub mod mock;

pub use api::{
    BpmInfo, DjEvent, EngineEvent, Equalizer, InterruptionKind, ItemRole, ItemStatus, MediaTime,
    PlaybackDirection, PlayerEvent, PlayerStatus, PortDescription, PortType, RouteChangeReason,
    RouteDescription, SelectionPlayback, SessionBeat, SessionDuckingMode, SessionEvent,
    SessionTransportSnapshot, SlotId, StretchBackendKind, SyncUnavailable, Tempo, TempoError,
    TimeControlStatus, TimeRange, TrackBinding, TrackRef, TransportRevision, WaitingReason,
};
pub use error::PlayError;
pub use kithara_assets::{AssetLayout, DefaultLayout};
pub use kithara_audio::SeekOutcome;
pub use kithara_effects::{GainDb, eq::EqBandConfig};
pub use kithara_net::Headers;
pub use kithara_render::{
    CrossfadeCurve, CrossfadeSettings, CrossfadeSettingsPatch, CrossfadeSettingsPatchError,
    DispatcherProtocol, EngineLoad, EngineLoadSnapshot, InvalidCrossfade, LoadRefusal, PlayWorker,
    PlayWorkerConfig, PlayWorkerConfigPatch, ServiceClass, TrackConfig,
    bridge::{
        DeckEqChange, DeckEvent, DeckMixSettings, DeckMixSettingsChange, DeckPart, DeckProtocol,
        DeckRefusal, DeckSnapshot, EqSnapshot, FadeDir, MixTapWriter, PlaybackFault,
        RtMetricsSnapshot, Slot, SlotSnapshot,
    },
    dispatch,
    rt::{
        BufferGeometryError, DeckMixerConfig, DeckMixerConfigPatch, DeckMixerConfigPatchError,
        PlayerNode, StreamShape,
    },
};
pub use kithara_warp::{BeatGrid, BeatGridId, BeatGridSnapshot, MIN_SPEED};
pub use player::{
    Bound, DeckControl, DeckPass, HostedDeck, Outbox, Player, PlayerConfig, PlayerFactory,
    PlayerImpl, Position, Settled, Track, TrackCommand, TrackFactory, TrackReceipt, TrackSettings,
    TrackSettingsChange, TrackSettingsPatch, TrackSettingsPatchError, TrackSnapshot, TrackStatus,
};
pub use resource::{
    ArtifactDocument, ArtifactFetch, ArtifactLoadError, ArtifactSource, Cover, MAX_ARTIFACT_BYTES,
    OpenedTrack, PlaybackResamplerBackend, Resource, ResourceConfig, ResourceLane, ResourceLoad,
    ResourcePrep, ResourcePrepPatch, ResourceSrc, SourceType,
};
pub use session::{OutputSnapshot, PlayerId, SessionError, SessionOutputView, SessionSampleRate};
mod consts;
