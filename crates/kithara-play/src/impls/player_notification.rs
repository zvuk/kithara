//! Notification types emitted by the audio-thread processor.
//!
//! These are sent from `PlayerNodeProcessor` to the main thread via a
//! bounded channel inside `SharedPlayerState`.

use std::sync::Arc;

use crate::error::PlayError;

/// Notifications emitted by the player processor on the audio thread.
///
/// All variants carry an `Arc<str>` identifier for the track they refer to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TrackPlaybackStopReason {
    /// Playback stopped because the track naturally reached EOF.
    Eof,
    /// Playback stopped because the track was explicitly stopped or interrupted.
    Stop,
}

#[expect(dead_code, reason = "fields read when player polls notification_rx")]
#[expect(
    clippy::enum_variant_names,
    reason = "Track prefix is intentional domain naming"
)]
#[derive(Debug, Clone)]
pub(crate) enum PlayerNotification {
    /// A track encountered an unrecoverable error during playback.
    TrackError(Arc<str>, PlayError),
    /// A track was successfully loaded into the processor arena.
    TrackLoaded(Arc<str>),
    /// A track was removed from the processor arena.
    TrackUnloaded(Arc<str>),
    /// A track started audible playback (fade-in completed or `play()`).
    TrackPlaybackStarted(Arc<str>),
    /// A track stopped playback.
    TrackPlaybackStopped {
        src: Arc<str>,
        item_id: Option<Arc<str>>,
        reason: TrackPlaybackStopReason,
    },
    /// A track was paused (fade-out completed).
    TrackPlaybackPaused(Arc<str>),
    /// The next track should be loaded into the processor (position
    /// reached the prefetch lead window before EOF). Preload-only —
    /// handlers must not start fade-in or change the current item.
    TrackRequested(Arc<str>),
    /// Time to hand over to the next track (position reached
    /// `crossfade_duration + block_seconds` before EOF, or natural EOF was
    /// observed). Handlers may activate the already-preloaded successor;
    /// when `crossfade_duration == 0` the activation defers to the
    /// playback-stopped path instead.
    TrackHandoverRequested(Arc<str>),
    /// A track change occurred: old track fading out, new track fading in.
    TrackChanged { old: Arc<str>, new: Arc<str> },
    /// A track started fading in.
    TrackFadingIn(Arc<str>),
    /// A track started fading out.
    TrackFadingOut(Arc<str>),
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use kithara_test_utils::kithara;

    use super::*;

    #[kithara::test]
    #[case(
        PlayerNotification::TrackLoaded(Arc::from("test.mp3")),
        "TrackLoaded",
        "test.mp3"
    )]
    #[case(
        PlayerNotification::TrackRequested(Arc::from("queued.mp3")),
        "TrackRequested",
        "queued.mp3"
    )]
    #[case(
        PlayerNotification::TrackHandoverRequested(Arc::from("handover.mp3")),
        "TrackHandoverRequested",
        "handover.mp3"
    )]
    #[case(
        PlayerNotification::TrackPlaybackStopped {
            src: Arc::from("ended.mp3"),
            item_id: Some(Arc::from("item-1")),
            reason: TrackPlaybackStopReason::Eof,
        },
        "TrackPlaybackStopped",
        "ended.mp3"
    )]
    fn notification_debug_format(
        #[case] n: PlayerNotification,
        #[case] variant_name: &str,
        #[case] src_hint: &str,
    ) {
        let debug = format!("{n:?}");
        assert!(debug.contains(variant_name));
        assert!(debug.contains(src_hint));
    }

    #[kithara::test]
    fn notification_clone() {
        let n = PlayerNotification::TrackChanged {
            old: Arc::from("old.mp3"),
            new: Arc::from("new.mp3"),
        };
        let cloned = n.clone();
        assert!(
            matches!(cloned, PlayerNotification::TrackChanged { old, new } if &*old == "old.mp3" && &*new == "new.mp3")
        );
    }

    #[kithara::test]
    #[case("fail.mp3")]
    #[case("broken.mp3")]
    fn notification_error_variant(#[case] src: &str) {
        let err = PlayError::Internal("test error".into());
        let n = PlayerNotification::TrackError(Arc::from(src), err);
        assert!(matches!(n, PlayerNotification::TrackError(track, _) if &*track == src));
    }
}
