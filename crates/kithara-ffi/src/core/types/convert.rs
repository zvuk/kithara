use kithara::{
    play::{CrossfadeCurve, CrossfadeSettings, PlayError, TimeRange},
    queue::{ActionAtItemEnd, PlaybackOrder, QueueError, RepeatMode, Transition},
};

use super::{
    FfiActionAtItemEnd, FfiCrossfadeCurve, FfiCrossfadeSettings, FfiError, FfiPlaybackOrder,
    FfiRepeatMode, FfiTimeRange, FfiTransition, duration_to_seconds,
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
