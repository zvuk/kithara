use kithara::{
    assets::EvictReason,
    download::CancelReason,
    events::TrackId,
    platform::{sync::Arc, time::Duration},
    play::{
        InterruptionKind, ItemStatus, PlayError, PlayerStatus, RouteChangeReason,
        SessionDuckingMode, StretchBackendKind, TimeControlStatus, TimeRange,
    },
    queue::{
        ActionAtItemEnd, AdvanceReason, PlaybackOrder, QueueRepeatMode, RepeatMode, Transition,
    },
    stream::{AudioCodec, ContainerFormat},
};
use kithara_audio::{
    DecodeErrorClass, DecodeErrorKind, DecoderBackend, DecoderChangeCause, FrameDomain,
    PlaybackResamplerKind, ResamplerKind, TrackFailureKind,
};
use kithara_file::TotalBytesSource;
use kithara_hls::{KeyFailureStage, KeySource};

/// FFI-friendly error type bridging playback failures into platform bindings.
#[derive(Clone, Debug, thiserror::Error)]
#[cfg_attr(any(feature = "uniffi", feature = "uniffi-web"), derive(uniffi::Error))]
pub enum FfiError {
    #[error("Kithara host is not initialized")]
    NotInitialized,

    #[error("Kithara host initialization is already in progress")]
    InitializationInProgress,

    #[error("Kithara host is already initialized")]
    AlreadyInitialized,

    #[error("player not ready")]
    NotReady,

    #[error("item failed: {reason}")]
    ItemFailed { reason: String },

    #[error("seek failed: {reason}")]
    SeekFailed { reason: String },

    #[error("engine not running")]
    EngineNotRunning,

    #[error("invalid argument: {reason}")]
    InvalidArgument { reason: String },

    #[error("{description}")]
    Internal { description: String },
}

impl From<PlayError> for FfiError {
    fn from(err: PlayError) -> Self {
        match err {
            PlayError::NotReady | PlayError::NoActiveSlot => Self::NotReady,
            PlayError::ItemFailed { reason } => Self::ItemFailed { reason },
            PlayError::SeekFailed { position } => Self::SeekFailed {
                reason: format!("position {position:?}"),
            },
            PlayError::EngineNotRunning => Self::EngineNotRunning,
            err @ (PlayError::IndexOutOfRange { .. }
            | PlayError::ItemConsumed { .. }
            | PlayError::ArmIndexMismatch { .. }
            | PlayError::EqBandOutOfRange { .. }
            | PlayError::InvalidParameter { .. }) => Self::InvalidArgument {
                reason: err.to_string(),
            },
            err => Self::Internal {
                description: err.to_string(),
            },
        }
    }
}

#[cfg(feature = "uniffi")]
impl From<uniffi::UnexpectedUniFFICallbackError> for FfiError {
    fn from(e: uniffi::UnexpectedUniFFICallbackError) -> Self {
        Self::Internal {
            description: e.reason,
        }
    }
}

/// Convert [`Duration`] to seconds (`f64`).
#[must_use]
pub const fn duration_to_seconds(d: Duration) -> f64 {
    d.as_secs_f64()
}

/// FFI-friendly mirror of [`kithara::hls::KeyOptions`].
///
/// Holds domain-scoped DRM rules — providers with different key
/// processors and headers can coexist.
#[derive(Clone, Default)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
#[derive(derive_more::Debug)]
pub struct FfiKeyOptions {
    #[debug("{:?}", self.rules.len())]
    pub rules: Vec<FfiKeyRule>,
}

/// A single DRM rule: domain patterns + key processor + optional
/// per-provider headers / query params.
#[derive(Clone, derive_more::Debug)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct FfiKeyRule {
    #[debug(skip)]
    pub processor: Arc<dyn crate::observer::FfiKeyProcessor>,
    pub headers: Option<std::collections::HashMap<String, String>>,
    pub query_params: Option<std::collections::HashMap<String, String>>,
    /// Salt forwarded to [`crate::observer::FfiKeyProcessor::process_key`]
    /// on every decrypt. `None` is treated as an empty string.
    ///
    /// A rule carrying a salt also mirrors it into
    /// [`crate::observer::SALT_HEADER`] in the player-wide header map.
    #[debug("{:?}", self.salt.as_ref().map(|_| "<set>"))]
    pub salt: Option<String>,
    /// Domain patterns — exact (`"example.com"`), wildcard subdomain
    /// (`"*.example.com"`), or match-any (`"*"`).
    pub domains: Vec<String>,
}

/// FFI-friendly per-item configuration. All fields immutable after
/// [`crate::item::AudioPlayerItem::new`].
#[derive(Clone, Debug)]
#[cfg_attr(
    any(feature = "uniffi", feature = "uniffi-web"),
    derive(uniffi::Record)
)]
pub struct FfiItemConfig {
    pub abr_mode: Option<FfiAbrMode>,
    /// Optional caller-facing content id. When absent, the item exposes
    /// its internally allocated queue id as `audioId` for the standalone
    /// Kithara API.
    pub audio_id: Option<TrackId>,
    pub headers: Option<std::collections::HashMap<String, String>>,
    /// Optional caller-facing queue-item uuid. When absent, the item
    /// exposes the legacy UUIDv5-derived handle.
    pub uuid_i64: Option<i64>,
    /// Audio source. Accepts a network URL (`https://example.com/song.mp3`,
    /// `https://…/master.m3u8`) **or** an absolute local file path
    /// (`/Users/…/song.flac`). Parsed via
    /// [`kithara::play::ResourceSrc::parse`] at insert time, then passed
    /// to [`kithara::play::ResourceConfig::for_src`].
    pub url: String,
    /// Caller-declared live-stream flag. `true` means the source is a
    /// live HLS feed (radio / broadcast); the player skips end-of-stream
    /// gating and `is_playable` always returns `true` for the item.
    /// Defaults to `false`. Auto-detection from the manifest is a
    /// future improvement.
    pub is_live_stream: bool,
    /// Peak bitrate ceiling in bits/sec. `0.0` means no cap.
    pub preferred_peak_bitrate: f64,
    /// Peak bitrate ceiling on expensive networks (cellular). `0.0`
    /// means no cap.
    pub preferred_peak_bitrate_expensive: f64,
}

#[cfg(test)]
impl FfiItemConfig {
    pub(crate) fn for_test(url: &str) -> Self {
        Self {
            abr_mode: None,
            audio_id: None,
            headers: None,
            uuid_i64: None,
            url: url.to_owned(),
            is_live_stream: false,
            preferred_peak_bitrate: 0.0,
            preferred_peak_bitrate_expensive: 0.0,
        }
    }
}

/// FFI-friendly mirror of [`PlayerStatus`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, kithara_derive::Mirror)]
#[mirror(from = PlayerStatus)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
pub enum FfiPlayerStatus {
    Unknown,
    ReadyToPlay,
    Failed,
}

/// Snapshot of everything an item knows about itself. One getter so a
/// caller reads a consistent set instead of three independently locked
/// values.
#[derive(Clone, Debug, PartialEq)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct FfiItemState {
    pub status: FfiItemStatus,
    /// Playable duration once the metadata layer answers.
    pub duration_seconds: Option<f64>,
    pub error: Option<String>,
    pub loaded_ranges: Vec<FfiTimeRange>,
}
/// FFI-friendly mirror of [`ItemStatus`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, kithara_derive::Mirror)]
#[mirror(from = ItemStatus)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
pub enum FfiItemStatus {
    Unknown,
    ReadyToPlay,
    Failed,
}

/// FFI-friendly mirror of [`TimeControlStatus`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, kithara_derive::Mirror)]
#[mirror(from = TimeControlStatus)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
pub enum FfiTimeControlStatus {
    Paused,
    WaitingToPlay,
    Playing,
}

/// Track lifecycle state for a queued item.
///
/// Emitted by the native engine as the queue loads, plays, consumes,
/// fails, or cancels an item.
#[derive(Clone, Debug, PartialEq, Eq, kithara_derive::Mirror)]
#[mirror(from = kithara::queue::TrackStatus)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
pub enum FfiTrackStatus {
    /// The item is known to the queue but loading has not started.
    Pending,
    /// The item is actively loading.
    Loading,
    /// Loading is progressing slowly enough to be user-visible.
    Slow,
    /// The item is loaded and ready for playback.
    Loaded,
    /// Loading or playback failed with a native error message.
    #[mirror(tuple)]
    Failed { reason: String },
    /// The item has already been consumed by playback.
    Consumed,
    /// Loading was cancelled by a newer queue selection.
    Cancelled,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, kithara_derive::Mirror)]
#[mirror(from = AdvanceReason)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
pub enum FfiAdvanceReason {
    InitialLoad,
    NaturalEof,
    CrossfadePreArm,
    UserSelect,
    UserNext,
    UserPrev,
    TrackFailed,
    RemovedCurrent,
    Repeat,
    Cancelled,
    #[mirror(skip)]
    Unknown,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, kithara_derive::Mirror)]
#[mirror(from = QueueRepeatMode)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
pub enum FfiRepeatMode {
    Off,
    One,
    All,
    #[mirror(skip)]
    Unknown,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(any(feature = "uniffi", feature = "uniffi-web"), derive(uniffi::Enum))]
pub enum FfiPlaybackOrder {
    Sequential,
    Shuffle,
    Unknown,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(any(feature = "uniffi", feature = "uniffi-web"), derive(uniffi::Enum))]
pub enum FfiActionAtItemEnd {
    Advance,
    Pause,
    None,
    Unknown,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(any(feature = "uniffi", feature = "uniffi-web"), derive(uniffi::Enum))]
pub enum FfiCrossfadeCurve {
    Linear,
    EqualPower,
    Unknown,
}

pub use super::config::FfiCrossfadeSettings;

impl TryFrom<FfiPlaybackOrder> for PlaybackOrder {
    type Error = FfiError;
    fn try_from(value: FfiPlaybackOrder) -> Result<Self, Self::Error> {
        match value {
            FfiPlaybackOrder::Sequential => Ok(Self::Sequential),
            FfiPlaybackOrder::Shuffle => Ok(Self::Shuffle),
            FfiPlaybackOrder::Unknown => Err(FfiError::InvalidArgument {
                reason: "unknown playback order".into(),
            }),
        }
    }
}

impl From<PlaybackOrder> for FfiPlaybackOrder {
    fn from(value: PlaybackOrder) -> Self {
        match value {
            PlaybackOrder::Sequential => Self::Sequential,
            PlaybackOrder::Shuffle => Self::Shuffle,
            _ => Self::Unknown,
        }
    }
}
impl TryFrom<FfiActionAtItemEnd> for ActionAtItemEnd {
    type Error = FfiError;
    fn try_from(value: FfiActionAtItemEnd) -> Result<Self, Self::Error> {
        match value {
            FfiActionAtItemEnd::Advance => Ok(Self::Advance),
            FfiActionAtItemEnd::Pause => Ok(Self::Pause),
            FfiActionAtItemEnd::None => Ok(Self::None),
            FfiActionAtItemEnd::Unknown => Err(FfiError::InvalidArgument {
                reason: "unknown terminal action".into(),
            }),
        }
    }
}

impl From<ActionAtItemEnd> for FfiActionAtItemEnd {
    fn from(value: ActionAtItemEnd) -> Self {
        match value {
            ActionAtItemEnd::Advance => Self::Advance,
            ActionAtItemEnd::Pause => Self::Pause,
            ActionAtItemEnd::None => Self::None,
            _ => Self::Unknown,
        }
    }
}
impl From<RepeatMode> for FfiRepeatMode {
    fn from(value: RepeatMode) -> Self {
        match value {
            RepeatMode::Off => Self::Off,
            RepeatMode::One => Self::One,
            RepeatMode::All => Self::All,
            _ => Self::Unknown,
        }
    }
}

/// How far the whole session output drops under a competing sound.
#[derive(Clone, Copy, Debug, PartialEq, Eq, kithara_derive::Mirror)]
#[mirror(into = SessionDuckingMode)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
pub enum FfiDuckingMode {
    /// Full level.
    Off,
    /// Lowered to 40%.
    Soft,
    /// Lowered to 20%.
    Hard,
}

/// What one platform audio-interruption notification reports.
#[derive(Clone, Copy, Debug, PartialEq, Eq, kithara_derive::Mirror)]
#[mirror(into = InterruptionKind)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
pub enum FfiInterruptionKind {
    /// The system took the output away.
    Began,
    /// The system released the output, telling whether playback may resume.
    Ended { should_resume: bool },
}

impl TryFrom<FfiRepeatMode> for RepeatMode {
    type Error = FfiRepeatMode;

    fn try_from(value: FfiRepeatMode) -> Result<Self, Self::Error> {
        match value {
            FfiRepeatMode::Off => Ok(Self::Off),
            FfiRepeatMode::One => Ok(Self::One),
            FfiRepeatMode::All => Ok(Self::All),
            FfiRepeatMode::Unknown => Err(FfiRepeatMode::Unknown),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, kithara_derive::Mirror)]
#[mirror(from = RouteChangeReason)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
pub enum FfiRouteChangeReason {
    Unknown,
    NewDeviceAvailable,
    OldDeviceUnavailable,
    CategoryChange,
    Override,
    WakeFromSleep,
    NoSuitableRouteForCategory,
    RouteConfigurationChange,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, kithara_derive::Mirror)]
#[mirror(from = StretchBackendKind)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
pub enum FfiStretchBackendKind {
    Signalsmith,
    Bungee,
    Unknown,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, kithara_derive::Mirror)]
#[mirror(from = EvictReason)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
pub enum FfiEvictReason {
    QuotaBytes,
    QuotaAssets,
    Displaced,
    #[mirror(skip)]
    Unknown,
}

/// FFI-friendly time range (seconds-based).
#[derive(Clone, Copy, Debug, PartialEq)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct FfiTimeRange {
    pub duration_seconds: f64,
    pub start_seconds: f64,
}

impl From<TimeRange> for FfiTimeRange {
    fn from(tr: TimeRange) -> Self {
        Self {
            start_seconds: duration_to_seconds(tr.start),
            duration_seconds: duration_to_seconds(tr.duration),
        }
    }
}

/// Typed player event dispatched through [`crate::observer::PlayerObserver::on_event`].
///
/// Replaces raw integer status codes with typed enums. Swift receives
/// a single callback with a discriminated union instead of 7 separate methods.
///
/// **Concurrency**: events may arrive from multiple threads concurrently
/// (async broadcast task + OS polling thread). Swift must handle
/// thread-safe delivery internally.
#[derive(Debug)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
#[rustfmt::skip]
pub enum FfiPlayerEvent {
    TimeChanged { seconds: f64 },
    RateChanged { rate: f32 },
    CurrentItemChanged { item_id: Option<TrackId> },
    StatusChanged { status: FfiPlayerStatus },
    TimeControlStatusChanged { status: FfiTimeControlStatus },
    Error { error: String },
    DurationChanged { seconds: f64 },
    BufferedDurationChanged { seconds: f64 },
    VolumeChanged { volume: f32 },
    MuteChanged { muted: bool },
    ItemDidPlayToEnd,
    /// A track aborted mid-stream because the decoder / source
    /// reported a non-recoverable error. Distinct from
    /// [`Self::ItemDidPlayToEnd`]: the track did NOT reach its
    /// natural end. UI clients should surface this as a track
    /// failure (skip-and-flag), not treat it as completion.
    ItemDidFail { item_id: Option<TrackId> },
    /// Queue-level: the loading/playback status of an item changed.
    /// `item_id` is the private queue id used by the player wrapper to
    /// route back to the Swift-owned item.
    TrackStatusChanged { item_id: TrackId, status: FfiTrackStatus },
    /// Queue reached the end with `RepeatMode::Off` active.
    QueueEnded,
    /// A crossfade between tracks just started. `duration_seconds` is
    /// the configured crossfade window — UIs can drive progress from it.
    CrossfadeStarted { settings: FfiCrossfadeSettings },
    /// The configured crossfade window changed at runtime.
    CrossfadeSettingsChanged { settings: FfiCrossfadeSettings },
    PlaybackOrderChanged { order: FfiPlaybackOrder },
    ActionAtItemEndChanged { action: FfiActionAtItemEnd },
    TrackAdded { item_id: TrackId, index: u64 },
    TrackRemoved { item_id: TrackId },
    TrackLoadFailed { item_id: TrackId, reason: String, auto_skipped: bool },
    RepeatModeChanged { mode: FfiRepeatMode },
    NextTrackReady { item_id: TrackId, index: u64 },
    CurrentItemAdvanced { item_id: Option<TrackId>, reason: FfiAdvanceReason },
    EngineStarted,
    EngineStopped,
    CrossfadeCompleted,
    CrossfadeCancelled,
    MasterVolumeChanged { volume: f32 },
    AudioRouteChanged { reason: FfiRouteChangeReason },
    DjBpmDetected {
        slot: u64,
        bpm: f64,
        confidence: Option<f32>,
        first_beat_offset_seconds: f64,
    },
    DjKeylockChanged { on: bool },
    DjStretchBackendChanged { kind: FfiStretchBackendKind },
    AssetCommitted { asset_root: String, rel_path: String, final_len: Option<u64> },
    AssetFailed { asset_root: String, rel_path: String, reason: String },
    AssetEvicted { asset_root: String, reason: FfiEvictReason },
}

/// Transition style for a track switch.
///
/// Mirrors [`kithara::queue::Transition`]. Use [`FfiTransition::None`]
/// for immediate cuts (`AVQueuePlayer` user-initiated-selection idiom),
/// [`FfiTransition::Crossfade`] to use the player's configured
/// duration (typical for auto-advance and Next/Prev buttons), or
/// [`FfiTransition::CrossfadeWith`] to override per-call.
#[derive(Clone, Copy, Debug, PartialEq)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
pub enum FfiTransition {
    None,
    Crossfade,
    CrossfadeWith { settings: FfiCrossfadeSettings },
}

impl TryFrom<FfiTransition> for Transition {
    type Error = FfiError;
    fn try_from(t: FfiTransition) -> Result<Self, Self::Error> {
        Ok(match t {
            FfiTransition::None => Self::None,
            FfiTransition::Crossfade => Self::Crossfade,
            FfiTransition::CrossfadeWith { settings } => Self::CrossfadeWith {
                settings: settings.try_into()?,
            },
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, kithara_derive::Mirror)]
#[mirror(from = AudioCodec)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
pub enum FfiAudioCodecKind {
    AacLc,
    AacHe,
    AacHeV2,
    Mp3,
    Flac,
    Vorbis,
    Opus,
    Alac,
    Pcm,
    Adpcm,
    #[mirror(skip)]
    Unknown,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, kithara_derive::Mirror)]
#[mirror(from = ContainerFormat)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
pub enum FfiContainerKind {
    Mp4,
    Fmp4,
    MpegTs,
    MpegAudio,
    Adts,
    Flac,
    Wav,
    Ogg,
    Caf,
    Mkv,
    #[mirror(skip)]
    Unknown,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, kithara_derive::Mirror)]
#[mirror(from = DecoderBackend)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
pub enum FfiDecoderBackend {
    Symphonia,
    Apple,
    Android,
    #[mirror(skip)]
    Unknown,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, kithara_derive::Mirror)]
#[mirror(from = DecoderChangeCause)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
pub enum FfiDecoderChangeCause {
    Initial,
    VariantSwitch,
    FormatBoundary,
    SeekRecreate,
    Recovery,
    HostRateChange,
    #[mirror(skip)]
    Unknown,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, kithara_derive::Mirror)]
#[mirror(from = DecodeErrorClass)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
pub enum FfiDecodeErrorClass {
    Interrupted,
    VariantChange,
    Other,
    #[mirror(skip)]
    Unknown,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, kithara_derive::Mirror)]
#[mirror(from = DecodeErrorKind)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
pub enum FfiDecodeErrorKind {
    Io,
    UnsupportedCodec,
    UnsupportedContainer,
    InvalidData,
    SeekFailed,
    SeekOutOfRange,
    Parse,
    ProbeFailed,
    BackendUnavailable,
    InvalidSampleRate,
    BackendStatus,
    Interrupted,
    Backend,
    #[mirror(skip)]
    Unknown,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, kithara_derive::Mirror)]
#[mirror(from = FrameDomain)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
pub enum FfiFrameDomain {
    Source,
    Output,
    #[mirror(skip)]
    Unknown,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, kithara_derive::Mirror)]
#[mirror(from = ResamplerKind)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
pub enum FfiResamplerKind {
    Rubato,
    Apple,
    Glide,
    None,
    #[mirror(skip)]
    Unknown,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, kithara_derive::Mirror)]
#[mirror(from = PlaybackResamplerKind)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
pub enum FfiPlaybackResamplerKind {
    Rubato,
    Glide,
    None,
    #[mirror(skip)]
    Unknown,
}

#[derive(Clone, Debug, PartialEq, Eq, kithara_derive::Mirror)]
#[mirror(from = TrackFailureKind)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
pub enum FfiTrackFailureKind {
    Decode,
    RecreateFailed {
        offset: u64,
    },
    SourceCancelled,
    #[mirror(skip)]
    Unknown,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, kithara_derive::Mirror)]
#[mirror(from = CancelReason)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
pub enum FfiCancelReason {
    EpochCancel,
    PeerCancel,
    DownloaderShutdown,
    BeforeStart,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, kithara_derive::Mirror)]
#[mirror(from = TotalBytesSource)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
pub enum FfiTotalBytesSource {
    CommittedLen,
    ContentLength,
    #[mirror(skip)]
    Unknown,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, kithara_derive::Mirror)]
#[mirror(from = KeyFailureStage)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
pub enum FfiKeyFailureStage {
    Network,
    BodyCollect,
    Processor,
    Missing,
    #[mirror(skip)]
    Unknown,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, kithara_derive::Mirror)]
#[mirror(from = KeySource)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
pub enum FfiKeySource {
    Network,
    DiskCache,
    MemCache,
    #[mirror(skip)]
    Unknown,
}

/// Typed item event dispatched through [`crate::observer::ItemObserver::on_event`].
#[derive(Debug, Clone)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
pub enum FfiItemEvent {
    DurationChanged {
        seconds: f64,
    },
    /// Buffered byte ranges, expressed as `[start, start + duration)` in
    /// seconds. Replaces the older scalar `BufferedDurationChanged` —
    /// the total buffered time is the sum of `range.duration_seconds`.
    /// Mirrors the iOS `AudioPlayerItemProtocol.rxLoadedRanges` shape.
    LoadedRangesChanged {
        ranges: Vec<FfiTimeRange>,
    },
    StatusChanged {
        status: FfiItemStatus,
    },
    VariantsDiscovered {
        variants: Vec<FfiVariant>,
    },
    /// User selected a variant in the picker (may not be applied yet).
    VariantSelected {
        variant: FfiVariant,
    },
    /// Stream actually switched to a new variant.
    VariantApplied {
        variant: FfiVariant,
    },
    /// The item reached natural end-of-stream. Mirrors the iOS
    /// `AudioPlayerItemProtocol.rxDidReachEnd`.
    DidReachEnd,
    /// The item aborted mid-stream because the decoder / source
    /// reported a non-recoverable error. Distinct from
    /// [`Self::DidReachEnd`]: the item did NOT play to its
    /// natural end. UI clients should surface a failure marker
    /// instead of treating this as completion.
    DidFail,
    /// Playback stalled (the player is waiting for more data).
    /// Mirrors the iOS `AudioPlayerItemProtocol.rxDidStall`.
    DidStall,
    Error {
        error: String,
    },
    /// Decoder configuration changed for the current item.
    DecoderChanged {
        backend: FfiDecoderBackend,
        codec: Option<FfiAudioCodecKind>,
        container: Option<FfiContainerKind>,
        sample_rate: u32,
        channels: u16,
        bit_depth: Option<u16>,
        bitrate: Option<u32>,
        epoch: u64,
        cause: FfiDecoderChangeCause,
        variant: Option<u32>,
        base_offset: u64,
        duration_seconds: Option<f64>,
        gapless_leading: u64,
        gapless_trailing: u64,
        has_gapless: bool,
    },
    /// Decoder reported a non-fatal or fatal decode error.
    DecodeError {
        class: FfiDecodeErrorClass,
        kind: FfiDecodeErrorKind,
        codec: Option<FfiAudioCodecKind>,
        detail: String,
    },
    /// Decoder resolved gapless trim values for the current item.
    GaplessResolved {
        leading_frames: u64,
        trailing_frames: u64,
        domain: FfiFrameDomain,
        codec: Option<FfiAudioCodecKind>,
        sample_rate: u32,
    },
    /// Decoder-side resampler configuration changed for the current item.
    ResamplerConfigured {
        backend: FfiResamplerKind,
        input_rate: u32,
        output_rate: u32,
        channels: u16,
        bypassed: bool,
    },
    AudioFormatDetected {
        channels: u16,
        sample_rate: u32,
    },
    AudioFormatChanged {
        old_channels: u16,
        old_sample_rate: u32,
        new_channels: u16,
        new_sample_rate: u32,
    },
    SeekComplete {
        position_seconds: f64,
        epoch: u64,
    },
    SeekRejected {
        epoch: u64,
        target_seconds: f64,
    },
    DecoderReady {
        base_offset: u64,
        variant: Option<u32>,
    },
    TrackFailed {
        reason: FfiTrackFailureKind,
        epoch: u64,
    },
    UnderrunStarted {
        position_ms: u64,
        epoch: u64,
    },
    UnderrunEnded {
        position_ms: u64,
        epoch: u64,
    },
    BufferHealth {
        buffered_ms: u64,
        decoded_frontier_ms: u64,
        epoch: u64,
    },
    EngineLoad {
        load: f32,
        ms_per_chunk: f32,
        realtime_factor: f32,
    },
    PlaybackResamplerConfigured {
        backend: FfiPlaybackResamplerKind,
        host_sample_rate: u32,
        source_sample_rate: u32,
        active: bool,
    },
    HlsCacheComplete {
        total_bytes: Option<u64>,
    },
    DownloadStarted {
        request_id: u64,
        wait_in_queue_seconds: f64,
    },
    DownloadSlow {
        request_id: u64,
        elapsed_seconds: f64,
    },
    DownloadCompleted {
        request_id: u64,
        bytes_transferred: u64,
        duration_seconds: f64,
        bandwidth_bps: u64,
    },
    DownloadRetrying {
        request_id: u64,
        attempt: u32,
        max_retries: u32,
        error: String,
        backoff_seconds: f64,
    },
    DownloadBodyStalled {
        request_id: u64,
        consumed: u64,
        expected: Option<u64>,
        stall_seconds: f64,
    },
    DownloadBodyResumed {
        request_id: u64,
        resume_number: u32,
        from_offset: u64,
        honoured_range: bool,
    },
    DownloadRetryExhausted {
        request_id: u64,
        max_retries: u32,
        consumed: u64,
        error: String,
    },
    DownloadFirstByte {
        request_id: u64,
        ttfb_seconds: f64,
        status: u16,
        partial: bool,
    },
    DownloadCancelled {
        request_id: u64,
        reason: FfiCancelReason,
        bytes_transferred: u64,
    },
    FileOpened {
        codec: Option<FfiAudioCodecKind>,
        container: Option<FfiContainerKind>,
        total_bytes: Option<u64>,
        cached: bool,
    },
    FileTotalBytesResolved {
        total_bytes: u64,
        source: FfiTotalBytesSource,
    },
    FileCacheComplete {
        total_bytes: u64,
    },
    DrmKeyFetchFailed {
        key_host: Option<String>,
        stage: FfiKeyFailureStage,
        detail: String,
    },
    DrmKeyAcquired {
        key_host: Option<String>,
        source: FfiKeySource,
        bytes: u64,
        latency_ms: Option<u64>,
    },
    DrmSegmentDecryptFailed {
        variant: u32,
        segment_index: u32,
        detail: String,
    },
}

/// FFI-friendly HLS variant descriptor.
#[derive(Clone, Debug)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct FfiVariant {
    pub name: Option<String>,
    pub index: u32,
    pub bandwidth_bps: u64,
}

/// Outcome reported by [`crate::observer::ItemLoadCallback::on_complete`]
/// when [`crate::item::AudioPlayerItem::load`] resolves.
#[derive(Clone, Copy, Debug)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct FfiItemLoadResult {
    /// `true` once the metadata layer recognises encrypted segments.
    pub has_protected_content: bool,
    /// `true` when the item has enough metadata to start playback.
    pub is_playable: bool,
}

/// FFI-friendly ABR mode.
#[derive(Clone, Copy, Debug)]
#[cfg_attr(any(feature = "uniffi", feature = "uniffi-web"), derive(uniffi::Enum))]
pub enum FfiAbrMode {
    Auto,
    Manual { variant_index: u32 },
}

/// Snapshot of the player's current state, returned by [`crate::player::AudioPlayer::snapshot`].
///
/// Fields are `Option` when no current item is loaded — callers should
/// not assume defaults.
#[derive(Clone, Debug)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct FfiPlayerSnapshot {
    pub status: FfiPlayerStatus,
    pub current_time: Option<f64>,
    pub duration: Option<f64>,
    pub is_muted: bool,
    /// Target playback speed (the value used by `play()`). Live `rate`
    /// equals this while playing and `0.0` while paused.
    pub playing_rate: f32,
    pub rate: f32,
    pub volume: f32,
}

#[cfg(test)]
mod tests {
    use ::kithara::play::CrossfadeSettings;

    use super::*;

    #[kithara::test]
    fn duration_to_seconds_roundtrips() {
        let secs = 42.123_456;
        let back = duration_to_seconds(Duration::from_secs_f64(secs));
        assert!((back - secs).abs() < 1e-9);
    }

    #[kithara::test]
    #[case::not_ready(PlayError::NotReady, (|f: &FfiError| matches!(f, FfiError::NotReady)) as fn(&FfiError) -> bool)]
    #[case::no_active_slot(PlayError::NoActiveSlot, (|f: &FfiError| matches!(f, FfiError::NotReady)) as fn(&FfiError) -> bool)]
    #[case::item_failed(
        PlayError::ItemFailed { reason: "bad codec".into() },
        (|f: &FfiError| matches!(f, FfiError::ItemFailed { .. })) as fn(&FfiError) -> bool
    )]
    #[case::index_out_of_range(
        PlayError::IndexOutOfRange { index: 3, len: 2 },
        (|f: &FfiError| matches!(f, FfiError::InvalidArgument { .. })) as fn(&FfiError) -> bool
    )]
    #[case::internal_fallback(PlayError::ArenaFull, (|f: &FfiError| matches!(f, FfiError::Internal { .. })) as fn(&FfiError) -> bool)]
    fn play_error_maps_to_expected_ffi_variant(
        #[case] input: PlayError,
        #[case] matches_variant: fn(&FfiError) -> bool,
    ) {
        let ffi: FfiError = input.into();
        assert!(matches_variant(&ffi));
    }

    #[kithara::test]
    fn player_status_conversion() {
        assert_eq!(
            FfiPlayerStatus::from(PlayerStatus::ReadyToPlay),
            FfiPlayerStatus::ReadyToPlay
        );
        assert_eq!(
            FfiPlayerStatus::from(PlayerStatus::Failed),
            FfiPlayerStatus::Failed
        );
        assert_eq!(
            FfiPlayerStatus::from(PlayerStatus::Unknown),
            FfiPlayerStatus::Unknown
        );
    }

    #[kithara::test]
    fn item_status_conversion() {
        assert_eq!(
            FfiItemStatus::from(ItemStatus::ReadyToPlay),
            FfiItemStatus::ReadyToPlay
        );
    }

    #[kithara::test]
    fn time_control_status_conversion() {
        assert_eq!(
            FfiTimeControlStatus::from(TimeControlStatus::Playing),
            FfiTimeControlStatus::Playing
        );
    }

    #[kithara::test]
    fn advance_reason_conversion_preserves_known_variants() {
        for (source, expected) in [
            (AdvanceReason::InitialLoad, FfiAdvanceReason::InitialLoad),
            (AdvanceReason::NaturalEof, FfiAdvanceReason::NaturalEof),
            (
                AdvanceReason::CrossfadePreArm,
                FfiAdvanceReason::CrossfadePreArm,
            ),
            (AdvanceReason::UserSelect, FfiAdvanceReason::UserSelect),
            (AdvanceReason::UserNext, FfiAdvanceReason::UserNext),
            (AdvanceReason::UserPrev, FfiAdvanceReason::UserPrev),
            (AdvanceReason::TrackFailed, FfiAdvanceReason::TrackFailed),
            (
                AdvanceReason::RemovedCurrent,
                FfiAdvanceReason::RemovedCurrent,
            ),
            (AdvanceReason::Repeat, FfiAdvanceReason::Repeat),
            (AdvanceReason::Cancelled, FfiAdvanceReason::Cancelled),
        ] {
            assert_eq!(FfiAdvanceReason::from(source), expected);
        }
    }

    #[kithara::test]
    fn audio_codec_conversion_preserves_known_variants() {
        for (source, expected) in [
            (AudioCodec::AacLc, FfiAudioCodecKind::AacLc),
            (AudioCodec::AacHe, FfiAudioCodecKind::AacHe),
            (AudioCodec::AacHeV2, FfiAudioCodecKind::AacHeV2),
            (AudioCodec::Mp3, FfiAudioCodecKind::Mp3),
            (AudioCodec::Flac, FfiAudioCodecKind::Flac),
            (AudioCodec::Vorbis, FfiAudioCodecKind::Vorbis),
            (AudioCodec::Opus, FfiAudioCodecKind::Opus),
            (AudioCodec::Alac, FfiAudioCodecKind::Alac),
            (AudioCodec::Pcm, FfiAudioCodecKind::Pcm),
            (AudioCodec::Adpcm, FfiAudioCodecKind::Adpcm),
        ] {
            assert_eq!(FfiAudioCodecKind::from(source), expected);
        }
    }

    #[kithara::test]
    fn container_conversion_preserves_known_variants() {
        for (source, expected) in [
            (ContainerFormat::Mp4, FfiContainerKind::Mp4),
            (ContainerFormat::Fmp4, FfiContainerKind::Fmp4),
            (ContainerFormat::MpegTs, FfiContainerKind::MpegTs),
            (ContainerFormat::MpegAudio, FfiContainerKind::MpegAudio),
            (ContainerFormat::Adts, FfiContainerKind::Adts),
            (ContainerFormat::Flac, FfiContainerKind::Flac),
            (ContainerFormat::Wav, FfiContainerKind::Wav),
            (ContainerFormat::Ogg, FfiContainerKind::Ogg),
            (ContainerFormat::Caf, FfiContainerKind::Caf),
            (ContainerFormat::Mkv, FfiContainerKind::Mkv),
        ] {
            assert_eq!(FfiContainerKind::from(source), expected);
        }
    }

    #[kithara::test]
    fn decoder_change_cause_conversion_preserves_known_variants() {
        for (source, expected) in [
            (DecoderChangeCause::Initial, FfiDecoderChangeCause::Initial),
            (
                DecoderChangeCause::VariantSwitch,
                FfiDecoderChangeCause::VariantSwitch,
            ),
            (
                DecoderChangeCause::FormatBoundary,
                FfiDecoderChangeCause::FormatBoundary,
            ),
            (
                DecoderChangeCause::SeekRecreate,
                FfiDecoderChangeCause::SeekRecreate,
            ),
            (
                DecoderChangeCause::Recovery,
                FfiDecoderChangeCause::Recovery,
            ),
            (
                DecoderChangeCause::HostRateChange,
                FfiDecoderChangeCause::HostRateChange,
            ),
        ] {
            assert_eq!(FfiDecoderChangeCause::from(source), expected);
        }
    }

    #[kithara::test]
    fn decode_error_kind_conversion_preserves_known_variants() {
        for (source, expected) in [
            (DecodeErrorKind::Io, FfiDecodeErrorKind::Io),
            (
                DecodeErrorKind::UnsupportedCodec,
                FfiDecodeErrorKind::UnsupportedCodec,
            ),
            (
                DecodeErrorKind::UnsupportedContainer,
                FfiDecodeErrorKind::UnsupportedContainer,
            ),
            (
                DecodeErrorKind::InvalidData,
                FfiDecodeErrorKind::InvalidData,
            ),
            (DecodeErrorKind::SeekFailed, FfiDecodeErrorKind::SeekFailed),
            (
                DecodeErrorKind::SeekOutOfRange,
                FfiDecodeErrorKind::SeekOutOfRange,
            ),
            (DecodeErrorKind::Parse, FfiDecodeErrorKind::Parse),
            (
                DecodeErrorKind::ProbeFailed,
                FfiDecodeErrorKind::ProbeFailed,
            ),
            (
                DecodeErrorKind::BackendUnavailable,
                FfiDecodeErrorKind::BackendUnavailable,
            ),
            (
                DecodeErrorKind::InvalidSampleRate,
                FfiDecodeErrorKind::InvalidSampleRate,
            ),
            (
                DecodeErrorKind::BackendStatus,
                FfiDecodeErrorKind::BackendStatus,
            ),
            (
                DecodeErrorKind::Interrupted,
                FfiDecodeErrorKind::Interrupted,
            ),
            (DecodeErrorKind::Backend, FfiDecodeErrorKind::Backend),
        ] {
            assert_eq!(FfiDecodeErrorKind::from(source), expected);
        }
    }

    #[kithara::test]
    fn resampler_kind_conversion_preserves_known_variants() {
        for (source, expected) in [
            (ResamplerKind::Rubato, FfiResamplerKind::Rubato),
            (ResamplerKind::Apple, FfiResamplerKind::Apple),
            (ResamplerKind::Glide, FfiResamplerKind::Glide),
            (ResamplerKind::None, FfiResamplerKind::None),
        ] {
            assert_eq!(FfiResamplerKind::from(source), expected);
        }
    }

    #[kithara::test]
    fn queue_repeat_mode_round_trips_through_ffi() {
        for expected in [RepeatMode::Off, RepeatMode::One, RepeatMode::All] {
            let ffi = FfiRepeatMode::from(expected);
            assert_eq!(RepeatMode::try_from(ffi), Ok(expected));
        }
    }

    #[kithara::test]
    fn unknown_ffi_repeat_mode_is_rejected() {
        assert_eq!(
            RepeatMode::try_from(FfiRepeatMode::Unknown),
            Err(FfiRepeatMode::Unknown)
        );
    }

    #[kithara::test]
    fn queue_policy_rejects_unknown_ffi_variants() {
        assert!(PlaybackOrder::try_from(FfiPlaybackOrder::Unknown).is_err());
        assert!(ActionAtItemEnd::try_from(FfiActionAtItemEnd::Unknown).is_err());
        assert!(
            CrossfadeSettings::try_from(FfiCrossfadeSettings {
                duration: 1.0,
                curve: FfiCrossfadeCurve::Unknown,
                depth: 1.0,
                position: 0.5,
            })
            .is_err()
        );
    }

    #[kithara::test]
    fn ffi_crossfade_default_matches_domain_and_round_trips() {
        let domain = CrossfadeSettings::default();
        let wire = FfiCrossfadeSettings::default();
        assert_eq!(wire, FfiCrossfadeSettings::from(domain));
        assert_eq!(CrossfadeSettings::try_from(wire).unwrap(), domain);
    }

    #[kithara::test]
    fn ffi_crossfade_settings_reject_every_invalid_float_class() {
        for settings in [
            FfiCrossfadeSettings {
                duration: f32::NAN,
                curve: FfiCrossfadeCurve::Linear,
                depth: 1.0,
                position: 0.5,
            },
            FfiCrossfadeSettings {
                duration: 1.0,
                curve: FfiCrossfadeCurve::Linear,
                depth: f32::INFINITY,
                position: 0.5,
            },
            FfiCrossfadeSettings {
                duration: 1.0,
                curve: FfiCrossfadeCurve::Linear,
                depth: 1.0,
                position: 0.0,
            },
        ] {
            assert!(CrossfadeSettings::try_from(settings).is_err());
        }
    }

    #[kithara::test]
    fn time_range_conversion() {
        let tr = TimeRange::new(Duration::from_secs(10), Duration::from_secs(5));
        let ffi = FfiTimeRange::from(tr);
        assert!((ffi.start_seconds - 10.0).abs() < 1e-9);
        assert!((ffi.duration_seconds - 5.0).abs() < 1e-9);
    }
}
