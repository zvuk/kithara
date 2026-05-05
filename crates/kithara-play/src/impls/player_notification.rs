//! Notification types emitted by the audio-thread processor.
//!
//! These are sent from `PlayerNodeProcessor` to the main thread via a
//! bounded channel inside `SharedPlayerState`.

use std::sync::Arc;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TrackPlaybackStopReason {
    /// Playback stopped because the track naturally reached EOF.
    Eof,
    /// Playback stopped because the track was explicitly stopped or interrupted.
    Stop,
}

#[derive(Debug, Clone)]
pub(crate) enum PlayerNotification {
    /// A track was successfully loaded into the processor arena.
    Loaded,
    /// A track was removed from the processor arena.
    Unloaded,
    /// A track started audible playback (fade-in completed or `play()`).
    PlaybackStarted,
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
    Changed,
    /// A track started fading in.
    FadingIn,
    /// A track started fading out.
    FadingOut,
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use kithara_test_utils::kithara;

    use super::*;

    #[kithara::test]
    #[case(PlayerNotification::Loaded, "Loaded")]
    #[case(PlayerNotification::Requested, "Requested")]
    #[case(PlayerNotification::HandoverRequested, "HandoverRequested")]
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
}
