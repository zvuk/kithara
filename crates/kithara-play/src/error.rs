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

    /// Bound-track elastic preparation failed.
    #[error("bound-track elastic preparation failed: {reason}")]
    ElasticPreparation {
        /// Backend or source preparation failure.
        reason: String,
    },

    /// The target cannot provide the elastic rendering backend.
    #[error("bound-track elastic rendering is unavailable on this target")]
    ElasticBackendUnavailable,

    /// Bound-track preparation was cancelled before publication.
    #[error("bound-track preparation was cancelled")]
    BindingPreparationCancelled,

    /// The session preparation snapshot changed before publication.
    #[error("session transport or stream shape changed during bound-track preparation")]
    BindingPreparationContextChanged,

    /// The analysed track axis does not match the session stream rate.
    #[error(
        "track binding sample rate {binding_sample_rate} does not match stream sample rate {stream_sample_rate}"
    )]
    BindingSampleRateMismatch {
        /// Host rate used to build the track binding.
        binding_sample_rate: u32,
        /// Active session stream rate.
        stream_sample_rate: u32,
    },

    /// A bound queue item has no prepared elastic state.
    #[error("item {index} requires bound-track preparation before activation")]
    BindingPreparationRequired {
        /// Queue index of the unprepared item.
        index: usize,
    },

    /// A bound queue item was prepared for an obsolete session snapshot.
    #[error("item {index} was prepared for a different session transport or stream shape")]
    BindingPreparationStale {
        /// Queue index of the stale item.
        index: usize,
    },

    /// The resource cannot open an independent preparation reader.
    #[error("bound item cannot open an independent preparation reader")]
    BindingSourceNotReopenable,

    #[error("seek failed to position {position:?}")]
    SeekFailed { position: Duration },

    /// Bound playback can only seek through a session transport transaction.
    #[error("bound-track seek requires a transactional session transport operation")]
    BoundTrackSeekRequiresSessionTransport,

    #[error("engine not running")]
    EngineNotRunning,

    #[error("engine already running")]
    EngineAlreadyRunning,

    /// Players in one transport operation must use the same session.
    #[error("players do not share one session transport")]
    SessionMismatch,

    /// A bound player cannot render the requested session tempo.
    #[error(
        "session tempo requires source rate {rate}, outside the renderer envelope [{minimum}, {maximum}]"
    )]
    SessionTempoUnsupported {
        /// Required source frames per output frame.
        rate: f64,
        /// Minimum supported source frames per output frame.
        minimum: f64,
        /// Maximum supported source frames per output frame.
        maximum: f64,
    },

    /// A bound player has not decoded far enough for the common tempo boundary.
    #[error(
        "session tempo requires source frame {required}, but the prepared frontier is {available}"
    )]
    SessionTempoLookAheadUnavailable {
        /// Furthest source frame needed through the prepared boundary.
        required: f64,
        /// Furthest source frame currently available to the renderer.
        available: f64,
    },

    /// A player has more than one audible track during a local handover.
    #[error("session tempo cannot change during a player track handover")]
    SessionTempoHandoverActive,

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

    #[error("end of resource")]
    Eof,

    #[error(transparent)]
    Session(#[from] SessionError),

    #[error("{0}")]
    Internal(String),
}
