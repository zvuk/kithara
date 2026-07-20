use std::fmt;

use kithara_platform::sync::Arc;

use crate::{
    api::{PlaybackDirection, TrackBinding},
    error::PlayError,
    player::track::PlayerResource,
};

/// Commands sent from the main thread to the processor.
#[non_exhaustive]
pub enum PlayerCmd {
    /// Load a track into the processor arena.
    LoadTrack {
        /// Immutable session-grid binding for synchronized rendering.
        binding: Option<TrackBinding>,
        resource: Box<PlayerResource>,
        item_id: Option<Arc<str>>,
    },
    /// Unload a track by its source identifier.
    UnloadTrack { src: Arc<str> },
    /// Unload every track from the arena and reset the position/duration
    /// snapshot to zero. Sent when the queue is explicitly cleared.
    Clear,
    /// Add a track transition (fade in / fade out).
    Transition(TrackTransition),
    /// Seek active tracks to the given position in seconds.
    Seek { seconds: f64, seek_epoch: u64 },
    /// Set the paused state.
    SetPaused(bool),
    /// Update the fade duration.
    SetFadeDuration(f32),
    /// Update the prefetch lead time.
    SetPrefetchDuration(f32),
    /// Update the playback rate for all active tracks.
    SetPlaybackRate(f32),
    /// Update the transport pitch-bend multiplier for all active tracks.
    SetPitchBend(f32),
}

pub(crate) struct RejectedPlayerCmd {
    pub(crate) command: Box<PlayerCmd>,
    pub(crate) error: PlayError,
}

impl fmt::Debug for PlayerCmd {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::LoadTrack {
                binding,
                item_id,
                resource,
            } => f
                .debug_struct("LoadTrack")
                .field("bound", &binding.is_some())
                .field("item_id", item_id)
                .field("src", resource.src())
                .finish_non_exhaustive(),
            Self::UnloadTrack { src } => f.debug_struct("UnloadTrack").field("src", src).finish(),
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
            Self::SetPaused(p) => f.debug_tuple("SetPaused").field(p).finish(),
            Self::SetFadeDuration(d) => f.debug_tuple("SetFadeDuration").field(d).finish(),
            Self::SetPrefetchDuration(d) => f.debug_tuple("SetPrefetchDuration").field(d).finish(),
            Self::SetPlaybackRate(r) => f.debug_tuple("SetPlaybackRate").field(r).finish(),
            Self::SetPitchBend(b) => f.debug_tuple("SetPitchBend").field(b).finish(),
        }
    }
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
    pub(crate) fn is_leading(self) -> bool {
        matches!(self, Self::Playing | Self::FadingIn)
    }

    /// Whether the track is producing audible audio.
    pub(crate) fn is_playing(self) -> bool {
        matches!(self, Self::Playing | Self::FadingIn | Self::FadingOut)
    }
}

/// Transition command for a track.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TrackTransition {
    /// Start fading in the track with the given source identifier.
    FadeIn(Arc<str>),
    /// Start fading out the track with the given source identifier.
    FadeOut(Arc<str>),
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
    /// A bound track was accepted by the audio node.
    BindingCommitted {
        direction: PlaybackDirection,
        session_anchor_beats: f64,
        track_anchor_beats: f64,
    },
    /// A track was removed from the processor arena.
    Unloaded { src: Arc<str> },
    /// A track started audible playback (fade-in completed or `play()`).
    PlaybackStarted {
        src: Arc<str>,
        item_id: Option<Arc<str>>,
    },
    /// A track stopped playback. `src` and `item_id` are read by the
    /// player to construct `PlayerEvent::ItemDidPlayToEnd`.
    PlaybackStopped {
        src: Arc<str>,
        item_id: Option<Arc<str>>,
        reason: TrackPlaybackStopReason,
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
    HandoverRequested,
    /// A track change occurred: old track fading out, new track fading in.
    Changed { src: Arc<str> },
    /// A track started fading in.
    FadingIn { src: Arc<str> },
    /// A track started fading out.
    FadingOut { src: Arc<str> },
}

impl PlayerNotification {
    /// Returns the track src for variants that carry it.
    ///
    /// Used by the offline test harness (`take_notification_kinds`) and by
    /// tracing call-sites that need to discriminate between concurrent
    /// tracks beyond what the variant tag alone can express.
    #[must_use]
    pub fn src(&self) -> Option<&Arc<str>> {
        match self {
            Self::Loaded { src }
            | Self::Unloaded { src }
            | Self::Changed { src }
            | Self::FadingIn { src }
            | Self::FadingOut { src }
            | Self::PlaybackStopped { src, .. } => Some(src),
            Self::BindingCommitted { .. }
            | Self::PlaybackStarted { .. }
            | Self::Requested
            | Self::HandoverRequested => None,
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
    #[case(PlayerNotification::HandoverRequested, "HandoverRequested")]
    #[case(PlayerNotification::FadingIn { src: Arc::from("a.mp3") }, "FadingIn")]
    #[case(
        PlayerNotification::PlaybackStopped {
            src: Arc::from("ended.mp3"),
            item_id: Some(Arc::from("item-1")),
            reason: TrackPlaybackStopReason::Eof,
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
            item_id: None,
            reason: TrackPlaybackStopReason::Stop,
        };
        let cloned = n.clone();
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
