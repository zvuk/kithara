use kithara_platform::time::Duration;

use crate::{api::SlotId, session::SessionError};

#[derive(Clone, Debug, thiserror::Error)]
#[non_exhaustive]
pub enum PlayError {
    #[error("player not ready")]
    NotReady,

    #[error("no active slot")]
    NoActiveSlot,

    #[error("slot command channel full: {slot:?}")]
    SlotChannelFull { slot: SlotId },

    #[error("item {index} has no resource (already consumed)")]
    ItemConsumed { index: usize },

    #[error("item index out of range: {index} (len {len})")]
    IndexOutOfRange { index: usize, len: usize },

    #[error("commit index mismatch: requested {requested}, armed {armed}")]
    ArmIndexMismatch { requested: usize, armed: usize },

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

    #[error("no available slots in arena")]
    ArenaFull,

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

    #[error("mix level {level} is not a finite value in 0.0..=1.0")]
    MixLevel { level: f32 },

    #[error("crossfader position {position} is not a finite value in 0.0..=1.0")]
    MixPosition { position: f32 },

    #[error("mix input player belongs to a different audio session")]
    MixForeignSession,

    #[error("mix input lists the same player more than once")]
    MixDuplicatePlayer,

    #[error("end of resource")]
    Eof,

    #[error("audio session is gone: {reason}")]
    SessionGone { reason: &'static str },

    #[error(transparent)]
    Session(#[from] SessionError),

    #[error("{0}")]
    Internal(String),
}
