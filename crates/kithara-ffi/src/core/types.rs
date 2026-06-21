use kithara_events::{TrackId, TrackStatus as TS};
use kithara_platform::time::Duration;
use kithara_play::{ItemStatus, PlayError, PlayerStatus, TimeControlStatus, TimeRange};

/// FFI-friendly error type bridging playback failures into platform bindings.
#[derive(Clone, Debug, thiserror::Error)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Error))]
pub enum FfiError {
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
            PlayError::NotReady => Self::NotReady,
            PlayError::ItemFailed { reason } => Self::ItemFailed { reason },
            PlayError::SeekFailed { position } => Self::SeekFailed {
                reason: format!("position {position:?}"),
            },
            PlayError::EngineNotRunning => Self::EngineNotRunning,
            _ => Self::Internal {
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
pub fn duration_to_seconds(d: Duration) -> f64 {
    d.as_secs_f64()
}

/// FFI-friendly player configuration.
#[derive(Clone, Debug)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct FfiPlayerConfig {
    /// DRM key handling. Pass an empty [`FfiKeyOptions`] (default) when
    /// no DRM is needed.
    pub key_options: FfiKeyOptions,
    /// Storage options shared by all items (cache directory, etc.).
    pub store: crate::config::StoreOptions,
    /// Number of EQ bands (log-spaced). Default: 10.
    pub eq_band_count: u32,
}

/// Default number of log-spaced EQ bands.
pub const DEFAULT_EQ_BAND_COUNT: u32 = 10;

impl Default for FfiPlayerConfig {
    fn default() -> Self {
        Self {
            eq_band_count: DEFAULT_EQ_BAND_COUNT,
            key_options: FfiKeyOptions::default(),
            store: crate::config::StoreOptions::default(),
        }
    }
}

/// FFI-friendly mirror of [`kithara::hls::KeyOptions`].
///
/// Holds domain-scoped DRM rules — providers with different key
/// processors and headers can coexist.
#[derive(Clone, Default)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct FfiKeyOptions {
    pub rules: Vec<FfiKeyRule>,
}

impl std::fmt::Debug for FfiKeyOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FfiKeyOptions")
            .field("rules", &self.rules.len())
            .finish()
    }
}

/// A single DRM rule: domain patterns + key processor + optional
/// per-provider headers / query params.
#[derive(Clone)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct FfiKeyRule {
    pub processor: std::sync::Arc<dyn crate::observer::FfiKeyProcessor>,
    pub headers: Option<std::collections::HashMap<String, String>>,
    pub query_params: Option<std::collections::HashMap<String, String>>,
    /// Salt forwarded to [`crate::observer::FfiKeyProcessor::process_key`]
    /// on every decrypt. `None` is treated as an empty string.
    ///
    /// `setup_hls_aes` populates this automatically with a freshly
    /// generated 16-character alphanumeric value and mirrors it into
    /// [`crate::observer::SALT_HEADER`] in the player-wide header map.
    pub salt: Option<String>,
    /// Domain patterns — exact (`"example.com"`), wildcard subdomain
    /// (`"*.example.com"`), or match-any (`"*"`).
    pub domains: Vec<String>,
}

impl std::fmt::Debug for FfiKeyRule {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FfiKeyRule")
            .field("domains", &self.domains)
            .field("headers", &self.headers)
            .field("query_params", &self.query_params)
            .field("salt", &self.salt.as_ref().map(|_| "<set>"))
            .finish_non_exhaustive()
    }
}

/// FFI-friendly per-item configuration. All fields immutable after
/// [`crate::item::AudioPlayerItem::new`].
#[derive(Clone, Debug)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
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
    /// [`kithara::play::ResourceConfig::for_src`] at insert time, so the
    /// same string flows untouched into the player core.
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

/// FFI-friendly mirror of [`PlayerStatus`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
pub enum FfiPlayerStatus {
    Unknown,
    ReadyToPlay,
    Failed,
}

impl From<PlayerStatus> for FfiPlayerStatus {
    fn from(s: PlayerStatus) -> Self {
        match s {
            PlayerStatus::ReadyToPlay => Self::ReadyToPlay,
            PlayerStatus::Failed => Self::Failed,
            _ => Self::Unknown,
        }
    }
}

/// FFI-friendly mirror of [`ItemStatus`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
pub enum FfiItemStatus {
    Unknown,
    ReadyToPlay,
    Failed,
}

impl From<ItemStatus> for FfiItemStatus {
    fn from(s: ItemStatus) -> Self {
        match s {
            ItemStatus::ReadyToPlay => Self::ReadyToPlay,
            ItemStatus::Failed => Self::Failed,
            _ => Self::Unknown,
        }
    }
}

/// FFI-friendly mirror of [`TimeControlStatus`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
pub enum FfiTimeControlStatus {
    Paused,
    WaitingToPlay,
    Playing,
}

impl From<TimeControlStatus> for FfiTimeControlStatus {
    fn from(s: TimeControlStatus) -> Self {
        match s {
            TimeControlStatus::WaitingToPlay => Self::WaitingToPlay,
            TimeControlStatus::Playing => Self::Playing,
            _ => Self::Paused,
        }
    }
}

/// FFI-friendly mirror of [`kithara_events::TrackStatus`].
///
/// Mirrors the Queue-side track lifecycle: pending -> loading -> slow ->
/// loaded -> consumed, or `failed` on error. `Cancelled` covers
/// loads that were overridden by a later `select` of a different track.
#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
pub enum FfiTrackStatus {
    Pending,
    Loading,
    Slow,
    Loaded,
    Failed { reason: String },
    Consumed,
    Cancelled,
}

impl From<kithara_events::TrackStatus> for FfiTrackStatus {
    fn from(s: kithara_events::TrackStatus) -> Self {
        match s {
            TS::Loading => Self::Loading,
            TS::Slow => Self::Slow,
            TS::Loaded => Self::Loaded,
            TS::Failed(reason) => Self::Failed { reason },
            TS::Consumed => Self::Consumed,
            TS::Cancelled => Self::Cancelled,
            _ => Self::Pending,
        }
    }
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

/// Typed player event dispatched through [`PlayerObserver::on_event`].
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
    CrossfadeStarted { duration_seconds: f32 },
    /// The configured crossfade window changed at runtime.
    CrossfadeDurationChanged { seconds: f32 },
}

/// Transition style for a track switch.
///
/// Mirrors [`kithara_queue::Transition`]. Use [`FfiTransition::None`]
/// for immediate cuts (`AVQueuePlayer` user-initiated-selection idiom),
/// [`FfiTransition::Crossfade`] to use the player's configured
/// duration (typical for auto-advance and Next/Prev buttons), or
/// [`FfiTransition::CrossfadeWith`] to override per-call.
#[derive(Clone, Copy, Debug, PartialEq)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
pub enum FfiTransition {
    None,
    Crossfade,
    CrossfadeWith { seconds: f32 },
}

impl From<FfiTransition> for kithara_queue::Transition {
    fn from(t: FfiTransition) -> Self {
        match t {
            FfiTransition::None => Self::None,
            FfiTransition::Crossfade => Self::Crossfade,
            FfiTransition::CrossfadeWith { seconds } => Self::CrossfadeWith { seconds },
        }
    }
}

/// Typed item event dispatched through [`ItemObserver::on_event`].
#[derive(Debug)]
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
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
pub enum FfiAbrMode {
    Auto,
    Manual { variant_index: u32 },
}

/// Snapshot of the player's current state, returned by [`AudioPlayer::snapshot`].
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
    use super::*;

    #[kithara::test]
    fn duration_to_seconds_roundtrips() {
        let secs = 42.123_456;
        let back = duration_to_seconds(Duration::from_secs_f64(secs));
        assert!((back - secs).abs() < 1e-9);
    }

    #[kithara::test]
    #[case::not_ready(PlayError::NotReady, (|f: &FfiError| matches!(f, FfiError::NotReady)) as fn(&FfiError) -> bool)]
    #[case::item_failed(
        PlayError::ItemFailed { reason: "bad codec".into() },
        (|f: &FfiError| matches!(f, FfiError::ItemFailed { .. })) as fn(&FfiError) -> bool
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
    fn time_range_conversion() {
        let tr = TimeRange::new(Duration::from_secs(10), Duration::from_secs(5));
        let ffi = FfiTimeRange::from(tr);
        assert!((ffi.start_seconds - 10.0).abs() < 1e-9);
        assert!((ffi.duration_seconds - 5.0).abs() < 1e-9);
    }
}
