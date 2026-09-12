use std::fmt;

use kithara_events::TrackId;
use kithara_platform::sync::Arc;
use kithara_warp::{SessionFrame, WarpMapRevision};

use crate::rt::track::PlayerResource;

/// Commands sent from the main thread to the processor.
pub enum PlayerCmd {
    /// Load a track into the processor arena.
    LoadTrack {
        resource: Box<PlayerResource>,
        item_id: TrackId,
    },
    /// Unload a track by its queue-item identity.
    UnloadTrack { item_id: TrackId },
    /// Unload every track from the arena and reset the position/duration
    /// snapshot to zero. Sent when the queue is explicitly cleared.
    Clear,
    /// Add a track transition (fade in / fade out).
    Transition(TrackTransition),
    /// Seek active tracks to the given position in seconds.
    Seek { seconds: f64, seek_epoch: u64 },
    /// Present a decoder seek for one track at its installed Warp activation.
    ScheduleSeek {
        item_id: TrackId,
        seek_epoch: u64,
        disposition: ScheduledSeekDisposition,
        armed: bool,
    },
    /// Cancel the installed prepared launch for one track.
    CancelPreparedLaunch {
        item_id: TrackId,
        prepared_seek_epoch: u64,
        replacement_seek_epoch: u64,
        transport_seek_epoch: u64,
        target: std::time::Duration,
        resume: bool,
    },
    /// Set the paused state.
    SetPaused {
        paused: bool,
        item_id: Option<TrackId>,
    },
    /// Update the fade duration.
    SetFadeDuration(f32),
    /// Update the prefetch lead time.
    SetPrefetchDuration(f32),
}

impl fmt::Debug for PlayerCmd {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::LoadTrack { item_id, resource } => f
                .debug_struct("LoadTrack")
                .field("item_id", item_id)
                .field("src", resource.src())
                .finish_non_exhaustive(),
            Self::UnloadTrack { item_id } => f
                .debug_struct("UnloadTrack")
                .field("item_id", item_id)
                .finish(),
            Self::Clear => f.write_str("Clear"),
            Self::Transition(t) => f.debug_tuple("Transition").field(t).finish(),
            Self::Seek {
                seconds,
                seek_epoch,
            } => f
                .debug_struct("Seek")
                .field("seconds", seconds)
                .field("seek_epoch", seek_epoch)
                .finish(),
            Self::ScheduleSeek {
                item_id,
                seek_epoch,
                disposition,
                armed,
            } => f
                .debug_struct("ScheduleSeek")
                .field("item_id", item_id)
                .field("seek_epoch", seek_epoch)
                .field("disposition", disposition)
                .field("armed", armed)
                .finish(),
            Self::CancelPreparedLaunch {
                item_id,
                prepared_seek_epoch,
                replacement_seek_epoch,
                transport_seek_epoch,
                target,
                resume,
            } => f
                .debug_struct("CancelPreparedLaunch")
                .field("item_id", item_id)
                .field("prepared_seek_epoch", prepared_seek_epoch)
                .field("replacement_seek_epoch", replacement_seek_epoch)
                .field("transport_seek_epoch", transport_seek_epoch)
                .field("target", target)
                .field("resume", resume)
                .finish(),
            Self::SetPaused { paused, item_id } => f
                .debug_struct("SetPaused")
                .field("paused", paused)
                .field("item_id", item_id)
                .finish(),
            Self::SetFadeDuration(d) => f.debug_tuple("SetFadeDuration").field(d).finish(),
            Self::SetPrefetchDuration(d) => f.debug_tuple("SetPrefetchDuration").field(d).finish(),
        }
    }
}

/// Immutable reason and identity for a scheduled decoder presentation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScheduledSeekDisposition {
    SeekOnly { activation: SessionFrame },
    PreparedLaunch(PreparedLaunchIdentity),
}

impl ScheduledSeekDisposition {
    #[must_use]
    pub const fn is_prepared_launch(self) -> bool {
        matches!(self, Self::PreparedLaunch(_))
    }

    #[must_use]
    pub const fn activation(self) -> SessionFrame {
        match self {
            Self::SeekOnly { activation } => activation,
            Self::PreparedLaunch(identity) => identity.activation,
        }
    }
}

/// The exact synchronized relation that may release a prepared launch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PreparedLaunchIdentity {
    pub activation: SessionFrame,
    pub warp_map: WarpMapRevision,
}

/// State machine for a single track's lifecycle.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum TrackState {
    /// Track is loaded but not yet playing.
    #[default]
    Preloading,
    /// Track is actively playing at full volume.
    Playing,
    /// Track is fading in (volume ramping up).
    FadingIn,
    /// Track is fading out (volume ramping down).
    FadingOut,
    /// Track has finished playback (EOF or stopped).
    Finished,
}

impl TrackState {
    /// Whether the track is the "leading" track (playing or fading in).
    pub(crate) const fn is_leading(self) -> bool {
        matches!(self, Self::Playing | Self::FadingIn)
    }

    /// Whether the track is producing audible audio.
    pub(crate) const fn is_playing(self) -> bool {
        matches!(self, Self::Playing | Self::FadingIn | Self::FadingOut)
    }
}

/// Transition command for a track.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TrackTransition {
    /// Start fading in the track with the given queue-item identity.
    FadeIn(TrackId),
    /// Start fading out the track with the given queue-item identity.
    FadeOut(TrackId),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrackPlaybackStopReason {
    /// Playback stopped because the track naturally reached EOF.
    Eof,
    /// Playback stopped because the track was explicitly stopped or interrupted.
    Stop,
    /// Playback stopped because the underlying decoder / source reported
    /// a non-recoverable error mid-stream. Distinct from `Eof`: the
    /// track did NOT play to its natural end. Queue consumers must
    /// treat this as a track-failed signal, NOT as an auto-advance
    /// trigger.
    Failed,
}

#[derive(Debug, Clone)]
pub enum PlayerNotification {
    /// A track was successfully loaded into the processor arena.
    Loaded { src: Arc<str> },
    /// A track was removed from the processor arena.
    Unloaded { src: Arc<str>, item_id: TrackId },
    /// A track started audible playback (fade-in completed or `play()`).
    PlaybackStarted { src: Arc<str>, item_id: TrackId },
    /// A track stopped playback. `src` and `item_id` are read by the
    /// player to construct the `ItemRole` on `ItemDidPlayToEnd`.
    PlaybackStopped {
        src: Arc<str>,
        item_id: TrackId,
        reason: TrackPlaybackStopReason,
        /// The slot seek epoch the track sat at when this stop was minted.
        /// An `Eof` stop is delivered only while this is still the published
        /// epoch: a newer published seek revives the track, and the end the
        /// user left behind must not reach the queue. `Stop` and `Failed`
        /// carry the epoch too but are never fenced on it.
        seek_epoch: u64,
    },
    /// The next track should be loaded into the processor (position
    /// reached the prefetch lead window before EOF). Preload-only —
    /// handlers must not start fade-in or change the current item.
    Requested,
    /// Time to hand over to the next track (position reached
    /// `crossfade_duration + block_seconds` before EOF, or natural EOF was
    /// observed). Handlers may activate the already-preloaded successor;
    /// when `crossfade_duration == 0` the activation defers to the
    /// playback-stopped path instead.
    ///
    /// `src` and `item_id` name the track that is running out, for the same
    /// reason [`PlaybackStopped`](Self::PlaybackStopped) carries them: the
    /// request is minted by one track while any number of others render, and
    /// a consumer that has already advanced past it must be able to tell
    /// that this handover is not about the track it now holds.
    HandoverRequested { src: Arc<str>, item_id: TrackId },
    /// A track change occurred: old track fading out, new track fading in.
    Changed { src: Arc<str> },
    /// A track started fading in.
    FadingIn { src: Arc<str> },
    /// A track started fading out.
    FadingOut { src: Arc<str> },
    /// The processor applied a new effective live playback rate.
    RateChanged { rate: f32 },
}

impl PlayerNotification {
    /// Returns the track src for variants that carry it.
    ///
    /// Used by the offline test harness (`take_notification_kinds`) and by
    /// tracing call-sites that need to discriminate between concurrent
    /// tracks beyond what the variant tag alone can express.
    #[must_use]
    pub const fn src(&self) -> Option<&Arc<str>> {
        match self {
            Self::Loaded { src }
            | Self::Unloaded { src, .. }
            | Self::Changed { src }
            | Self::FadingIn { src }
            | Self::FadingOut { src }
            | Self::HandoverRequested { src, .. }
            | Self::PlaybackStopped { src, .. } => Some(src),
            Self::PlaybackStarted { .. } | Self::Requested | Self::RateChanged { .. } => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use kithara_platform::sync::Arc;
    use kithara_test_utils::kithara;

    use super::*;

    #[kithara::test]
    #[case(PlayerNotification::Loaded { src: Arc::from("a.mp3") }, "Loaded")]
    #[case(PlayerNotification::Requested, "Requested")]
    #[case(
        PlayerNotification::HandoverRequested {
            src: Arc::from("ending.mp3"),
            item_id: TrackId::allocate(),
        },
        "HandoverRequested"
    )]
    #[case(PlayerNotification::FadingIn { src: Arc::from("a.mp3") }, "FadingIn")]
    #[case(PlayerNotification::RateChanged { rate: 1.25 }, "RateChanged")]
    #[case(
        PlayerNotification::PlaybackStopped {
            src: Arc::from("ended.mp3"),
            item_id: TrackId::allocate(),
            reason: TrackPlaybackStopReason::Eof,
            seek_epoch: 0,
        },
        "PlaybackStopped"
    )]
    fn notification_debug_format(#[case] n: PlayerNotification, #[case] variant_name: &str) {
        let debug = format!("{n:?}");
        assert!(debug.contains(variant_name));
    }

    #[kithara::test]
    fn notification_clone() {
        let n = PlayerNotification::PlaybackStopped {
            src: Arc::from("ended.mp3"),
            item_id: TrackId::allocate(),
            reason: TrackPlaybackStopReason::Stop,
            seek_epoch: 0,
        };
        let cloned = n.clone();
        assert!(matches!(
            &n,
            PlayerNotification::PlaybackStopped { src, .. } if &**src == "ended.mp3"
        ));
        assert!(matches!(
            cloned,
            PlayerNotification::PlaybackStopped { ref src, .. } if &**src == "ended.mp3"
        ));
    }

    #[kithara::test]
    fn notification_changed_carries_src() {
        let n = PlayerNotification::Changed {
            src: Arc::from("next.mp3"),
        };
        let PlayerNotification::Changed { src } = n else {
            panic!("expected Changed");
        };
        assert_eq!(&*src, "next.mp3");
    }
}
