use kithara::{
    abr::AbrMode,
    play::{CrossfadeCurve, CrossfadeSettings, PlayError, TimeRange},
    queue::{ActionAtItemEnd, PlaybackOrder, QueueError, RepeatMode, Transition},
};

use super::{
    FfiAbrMode, FfiActionAtItemEnd, FfiCrossfadeCurve, FfiCrossfadeSettings, FfiError,
    FfiPlaybackOrder, FfiRepeatMode, FfiTimeRange, FfiTransition, duration_to_seconds,
};

impl From<PlayError> for FfiError {
    fn from(err: PlayError) -> Self {
        match err {
            PlayError::NotReady | PlayError::NoActiveSlot => Self::NotReady,
            PlayError::ItemFailed { reason } => Self::ItemFailed { reason },
            PlayError::SeekFailed { position } => Self::SeekFailed {
                reason: format!("position {position:?}"),
            },
            PlayError::EngineNotRunning => Self::EngineNotRunning,
            err @ (PlayError::ItemConsumed { .. }
            | PlayError::ArmedItemMismatch { .. }
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

impl From<QueueError> for FfiError {
    fn from(err: QueueError) -> Self {
        match err {
            QueueError::Play(err) => err.into(),
            QueueError::NotReady(_) => Self::NotReady,
            err => Self::Internal {
                description: err.to_string(),
            },
        }
    }
}

#[cfg(all(feature = "uniffi", not(target_arch = "wasm32")))]
impl From<uniffi::UnexpectedUniFFICallbackError> for FfiError {
    fn from(e: uniffi::UnexpectedUniFFICallbackError) -> Self {
        Self::Internal {
            description: e.reason,
        }
    }
}

impl From<CrossfadeSettings> for FfiCrossfadeSettings {
    fn from(value: CrossfadeSettings) -> Self {
        Self {
            duration: value.duration,
            curve: match value.curve {
                CrossfadeCurve::Linear => FfiCrossfadeCurve::Linear,
                CrossfadeCurve::EqualPower => FfiCrossfadeCurve::EqualPower,
                _ => FfiCrossfadeCurve::Unknown,
            },
            depth: value.depth,
            position: value.position,
        }
    }
}

impl TryFrom<FfiCrossfadeSettings> for CrossfadeSettings {
    type Error = FfiError;
    fn try_from(value: FfiCrossfadeSettings) -> Result<Self, Self::Error> {
        let curve = match value.curve {
            FfiCrossfadeCurve::Linear => CrossfadeCurve::Linear,
            FfiCrossfadeCurve::EqualPower => CrossfadeCurve::EqualPower,
            FfiCrossfadeCurve::Unknown => {
                return Err(FfiError::InvalidArgument {
                    reason: "unknown crossfade curve".into(),
                });
            }
        };
        Self::new(value.duration, curve, value.depth, value.position)
            .map_err(|error| FfiError::from(PlayError::from(error)))
    }
}

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

impl From<TimeRange> for FfiTimeRange {
    fn from(tr: TimeRange) -> Self {
        Self {
            start_seconds: duration_to_seconds(tr.start),
            duration_seconds: duration_to_seconds(tr.duration),
        }
    }
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

impl From<FfiAbrMode> for AbrMode {
    fn from(mode: FfiAbrMode) -> Self {
        match mode {
            FfiAbrMode::Auto => Self::Auto(None),
            FfiAbrMode::Manual { variant_index } => Self::manual(variant_index as usize),
        }
    }
}

/// ABR bandwidth cap, in bits/sec, from the caller's Wi-Fi and cellular
/// ceilings: the tighter of the non-zero ones. `None` when both are unset
/// (`0.0`), letting ABR consider every variant. Saturates at [`u64::MAX`]:
/// FFI callers pass any `f64`.
pub(crate) fn peak_bitrate_cap(wifi_bps: f64, cellular_bps: f64) -> Option<u64> {
    let cap = [wifi_bps, cellular_bps]
        .into_iter()
        .filter(|v| *v > 0.0)
        .reduce(f64::min)
        .filter(|v| v.is_finite())?;
    Some(num_traits::cast(cap.trunc()).unwrap_or(u64::MAX))
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::peak_bitrate_cap;

    #[kithara::test]
    fn peak_bitrate_cap_picks_the_tighter_non_zero_ceiling() {
        assert_eq!(peak_bitrate_cap(2_000_000.0, 500_000.0), Some(500_000));
        assert_eq!(peak_bitrate_cap(0.0, 750_000.0), Some(750_000));
        assert_eq!(peak_bitrate_cap(0.0, 0.0), None);
        assert_eq!(peak_bitrate_cap(f64::INFINITY, 0.0), None);
        assert_eq!(peak_bitrate_cap(1e30, 0.0), Some(u64::MAX));
    }
}
