//! FFI-compatible type conversions between `kithara-play` and Swift.

use std::time::Duration;

use kithara::play::{ItemStatus, PlayError, PlayerStatus, TimeControlStatus, TimeRange};

/// FFI-friendly error type bridging [`PlayError`] and [`DecodeError`].
#[derive(Clone, Debug, thiserror::Error)]
#[cfg_attr(feature = "backend-uniffi", derive(uniffi::Error))]
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

    #[error("{message}")]
    Internal { message: String },
}

pub type FfiResult<T> = Result<T, FfiError>;

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
                message: err.to_string(),
            },
        }
    }
}

#[cfg(feature = "backend-uniffi")]
impl From<uniffi::UnexpectedUniFFICallbackError> for FfiError {
    fn from(e: uniffi::UnexpectedUniFFICallbackError) -> Self {
        Self::Internal { message: e.reason }
    }
}

/// Convert seconds (`f64`) to [`Duration`], rejecting invalid values.
///
/// # Errors
///
/// Returns [`FfiError::InvalidArgument`] for `NaN`, infinity, or negative values.
pub fn seconds_to_duration(seconds: f64) -> FfiResult<Duration> {
    if seconds.is_nan() || seconds.is_infinite() {
        return Err(FfiError::InvalidArgument {
            reason: format!("invalid seconds value: {seconds}"),
        });
    }
    if seconds < 0.0 {
        return Err(FfiError::InvalidArgument {
            reason: format!("negative seconds: {seconds}"),
        });
    }
    Ok(Duration::from_secs_f64(seconds))
}

/// Convert [`Duration`] to seconds (`f64`).
#[must_use]
pub fn duration_to_seconds(d: Duration) -> f64 {
    d.as_secs_f64()
}

/// Parse a URL string, rejecting empty and invalid values.
///
/// # Errors
///
/// Returns [`FfiError::InvalidArgument`] for empty or malformed URLs.
pub fn parse_url(s: &str) -> FfiResult<url::Url> {
    let trimmed = s.trim();
    if trimmed.is_empty() {
        return Err(FfiError::InvalidArgument {
            reason: "empty URL".to_string(),
        });
    }
    url::Url::parse(trimmed).map_err(|e| FfiError::InvalidArgument {
        reason: format!("invalid URL: {e}"),
    })
}

/// FFI-friendly mirror of [`PlayerStatus`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "backend-uniffi", derive(uniffi::Enum))]
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
#[cfg_attr(feature = "backend-uniffi", derive(uniffi::Enum))]
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
#[cfg_attr(feature = "backend-uniffi", derive(uniffi::Enum))]
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

/// FFI-friendly time range (seconds-based).
#[derive(Clone, Copy, Debug, PartialEq)]
#[cfg_attr(feature = "backend-uniffi", derive(uniffi::Record))]
pub struct FfiTimeRange {
    pub start_seconds: f64,
    pub duration_seconds: f64,
}

impl From<TimeRange> for FfiTimeRange {
    fn from(tr: TimeRange) -> Self {
        Self {
            start_seconds: duration_to_seconds(tr.start),
            duration_seconds: duration_to_seconds(tr.duration),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[kithara::test]
    fn seconds_to_duration_valid() {
        let d = seconds_to_duration(1.5).expect("valid");
        assert_eq!(d, Duration::from_secs_f64(1.5));
    }

    #[kithara::test]
    fn seconds_to_duration_zero() {
        let d = seconds_to_duration(0.0).expect("valid");
        assert_eq!(d, Duration::ZERO);
    }

    #[kithara::test]
    fn seconds_to_duration_nan() {
        assert!(seconds_to_duration(f64::NAN).is_err());
    }

    #[kithara::test]
    fn seconds_to_duration_infinity() {
        assert!(seconds_to_duration(f64::INFINITY).is_err());
        assert!(seconds_to_duration(f64::NEG_INFINITY).is_err());
    }

    #[kithara::test]
    fn seconds_to_duration_negative() {
        assert!(seconds_to_duration(-1.0).is_err());
    }

    #[kithara::test]
    fn duration_roundtrip() {
        let secs = 42.123_456;
        let d = seconds_to_duration(secs).expect("valid");
        let back = duration_to_seconds(d);
        assert!((back - secs).abs() < 1e-9);
    }

    #[kithara::test]
    fn parse_url_valid() {
        let u = parse_url("https://example.com/song.mp3").expect("valid");
        assert_eq!(u.scheme(), "https");
    }

    #[kithara::test]
    fn parse_url_trimmed() {
        let u = parse_url("  https://example.com/  ").expect("valid");
        assert_eq!(u.host_str(), Some("example.com"));
    }

    #[kithara::test]
    fn parse_url_empty() {
        assert!(parse_url("").is_err());
        assert!(parse_url("   ").is_err());
    }

    #[kithara::test]
    fn parse_url_invalid() {
        assert!(parse_url("not a url").is_err());
    }

    #[kithara::test]
    fn play_error_not_ready() {
        let ffi: FfiError = PlayError::NotReady.into();
        assert!(matches!(ffi, FfiError::NotReady));
    }

    #[kithara::test]
    fn play_error_item_failed() {
        let ffi: FfiError = PlayError::ItemFailed {
            reason: "bad codec".into(),
        }
        .into();
        assert!(matches!(ffi, FfiError::ItemFailed { .. }));
    }

    #[kithara::test]
    fn play_error_internal_fallback() {
        let ffi: FfiError = PlayError::ArenaFull.into();
        assert!(matches!(ffi, FfiError::Internal { .. }));
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
