//! Notification types emitted by the audio-thread processor.
//!
//! These are sent from `PlayerNodeProcessor` to the main thread via a
//! `kanal` channel inside `SharedPlayerState`.

use std::sync::Arc;

use crate::error::PlayError;

/// Notifications emitted by the player processor on the audio thread.
///
/// All variants carry an `Arc<str>` identifier for the track they refer to.
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
    /// A track is approaching its end (threshold-based).
    TrackAboutToEnd(Arc<str>),
    /// A track started audible playback (fade-in completed or `play()`).
    TrackPlaybackStarted(Arc<str>),
    /// A track stopped playback (EOF or `stop()`).
    TrackPlaybackStopped(Arc<str>),
    /// A track was paused (fade-out completed).
    TrackPlaybackPaused(Arc<str>),
    /// The next track should be queued (position reached fade duration before end).
    TrackRequested(Arc<str>),
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

    use super::*;

    #[test]
    fn notification_debug_format() {
        let n = PlayerNotification::TrackLoaded(Arc::from("test.mp3"));
        let debug = format!("{n:?}");
        assert!(debug.contains("TrackLoaded"));
        assert!(debug.contains("test.mp3"));
    }

    #[test]
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

    #[test]
    fn notification_error_variant() {
        let err = PlayError::Internal("test error".into());
        let n = PlayerNotification::TrackError(Arc::from("fail.mp3"), err);
        assert!(matches!(n, PlayerNotification::TrackError(src, _) if &*src == "fail.mp3"));
    }
}
