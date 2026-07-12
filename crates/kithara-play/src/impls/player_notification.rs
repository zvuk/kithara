use kithara_platform::sync::Arc;

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
    Unloaded { src: Arc<str> },
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
    Changed { src: Arc<str> },
    /// A track started fading in.
    FadingIn,
    /// A track started fading out.
    FadingOut,
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
            | Self::PlaybackStopped { src, .. } => Some(src),
            Self::PlaybackStarted
            | Self::Requested
            | Self::HandoverRequested
            | Self::FadingIn
            | Self::FadingOut => None,
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
