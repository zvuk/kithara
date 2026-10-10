use kithara_bufpool::PoolError;
use kithara_platform::time::Duration;
use kithara_render::{
    InvalidCrossfade,
    bridge::{DeckRefusal, InvalidMixLevel},
    rt::BufferGeometryError,
};

use crate::{
    api::{SlotId, TrackId},
    session::SessionError,
};

#[derive(Clone, Debug, thiserror::Error)]
#[non_exhaustive]
pub enum PlayError {
    #[error("player is closed")]
    Closed,

    #[error("player not ready")]
    NotReady,

    #[error("no active slot")]
    NoActiveSlot,

    #[error("the {0} queue has no room")]
    Full(&'static str),

    #[error("a later consecutive tempo change superseded this one")]
    Superseded,

    #[error("the deck refused a batch: {0:?}")]
    Deck(DeckRefusal),

    #[error("item {item:?} is not on the deck and came without a resource")]
    ItemConsumed { item: TrackId },

    #[error("commit mismatch: requested {requested:?}, armed {armed:?}")]
    ArmedItemMismatch { requested: TrackId, armed: TrackId },

    #[error("eq band out of range: {band} (bands: {bands})")]
    EqBandOutOfRange { band: usize, bands: usize },

    #[error("item failed to load: {reason}")]
    ItemFailed { reason: String },

    #[error("seek failed to position {position:?}")]
    SeekFailed { position: Duration },

    #[error("engine not running")]
    EngineNotRunning,

    #[error("engine already running")]
    EngineAlreadyRunning,

    #[error("slot not found: {0:?}")]
    SlotNotFound(SlotId),

    #[error("slot already occupied: {0:?}")]
    SlotOccupied(SlotId),

    #[error("crossfade already in progress")]
    CrossfadeActive,

    #[error("no active crossfade to cancel")]
    NoCrossfade,

    #[error("BPM analysis failed: {reason}")]
    BpmAnalysisFailed { reason: String },

    #[error("BPM sync requires detected BPM on both slots")]
    BpmUnknown,

    #[error("session activation failed: {reason}")]
    SessionActivationFailed { reason: String },

    #[error("session category not supported: {reason}")]
    SessionCategoryUnsupported { reason: String },

    #[error("audio route unavailable: {reason}")]
    RouteUnavailable { reason: String },

    #[error("effect parameter not found: {name}")]
    EffectParameterNotFound { name: String },

    #[error("invalid parameter value: {name}={value}")]
    InvalidParameter { name: String, value: f32 },

    #[error("the frame a change was asked for is already rendered or about to be")]
    Late,

    #[error("a change at a frame needs a running render clock")]
    Untimed,

    #[error("invalid player configuration: {reason}")]
    InvalidConfiguration { reason: String },

    #[error("mix level {level} is not a finite value in 0.0..=1.0")]
    MixLevel { level: f32 },

    #[error("crossfader position {position} is not a finite value in 0.0..=1.0")]
    MixPosition { position: f32 },

    #[error("player belongs to a different audio session")]
    ForeignSession,

    #[error("player is already attached to an audio session")]
    SessionAlreadyBound,

    #[error("player sample rate {player} does not match audio session sample rate {session}")]
    SessionSampleRateMismatch { player: u32, session: u32 },

    #[error("an audio session is already active on this thread")]
    SessionAlreadyActive,

    #[error("end of resource")]
    Eof,

    #[error("playback buffer allocation failed: {0}")]
    Pool(#[from] PoolError),

    #[error("audio session is gone: {reason}")]
    SessionGone { reason: &'static str },

    #[error(transparent)]
    Session(#[from] SessionError),

    #[error("{0}")]
    Internal(String),
}

impl From<BufferGeometryError> for PlayError {
    fn from(error: BufferGeometryError) -> Self {
        Self::Session(error.into())
    }
}

impl From<InvalidCrossfade> for PlayError {
    fn from(InvalidCrossfade { name, value }: InvalidCrossfade) -> Self {
        Self::InvalidParameter {
            name: name.into(),
            value,
        }
    }
}

impl From<InvalidMixLevel> for PlayError {
    fn from(InvalidMixLevel { level }: InvalidMixLevel) -> Self {
        Self::MixLevel { level }
    }
}
