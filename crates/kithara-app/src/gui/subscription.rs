/// Which iced subscriptions should be active, and at what rate.
///
/// Lowering the tick frequency while paused cuts Main Thread redraws by ~5×
/// without breaking user-input handling that relies on iced's periodic event
/// pump (volume slider, tab switching) — the dominant Main Thread cost
/// observed in Instruments traces (512 ms / 30 s).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SubscriptionConfig {
    /// Global keyboard listener for Delete/Backspace shortcuts.
    pub is_keyboard_enabled: bool,
    /// Time tick interval in milliseconds that drives position/state sync
    /// with the queue. Lower interval = more responsive but more CPU.
    pub tick_interval_ms: u64,
}

/// Time-tick interval while a track is actively playing.
///
/// 100 ms (10 Hz) matches the previous fixed cadence — position slider needs
/// this rate to appear smooth during playback.
pub(crate) const TICK_INTERVAL_ACTIVE_MS: u64 = 100;

/// Time-tick interval while playback is paused or stopped.
///
/// 500 ms (2 Hz) is 5× less CPU than active playback, yet still pumps iced's
/// message loop often enough that user-driven updates (volume slider, EQ
/// bands, background variant discovery) propagate promptly to the view.
pub(crate) const TICK_INTERVAL_IDLE_MS: u64 = 500;

/// Decide subscription cadence based on playback state.
///
/// Keyboard shortcuts must always work so the user can delete a highlighted
/// row even while paused.
pub(crate) const fn subscription_config(playing: bool) -> SubscriptionConfig {
    SubscriptionConfig {
        tick_interval_ms: if playing {
            TICK_INTERVAL_ACTIVE_MS
        } else {
            TICK_INTERVAL_IDLE_MS
        },
        is_keyboard_enabled: true,
    }
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::*;

    #[kithara::test]
    #[case::paused(false, TICK_INTERVAL_IDLE_MS)]
    #[case::playing(true, TICK_INTERVAL_ACTIVE_MS)]
    fn subscription_tick_matches_playback_state(
        #[case] playing: bool,
        #[case] expected_tick_ms: u64,
    ) {
        let cfg = subscription_config(playing);
        assert_eq!(cfg.tick_interval_ms, expected_tick_ms);
        assert!(
            cfg.is_keyboard_enabled,
            "keyboard listener must stay active in both states"
        );
    }

    #[kithara::test]
    fn idle_tick_is_slower_than_active_tick() {
        assert!(
            TICK_INTERVAL_IDLE_MS > TICK_INTERVAL_ACTIVE_MS,
            "idle tick must be slower than active tick to save CPU"
        );
        assert!(
            TICK_INTERVAL_IDLE_MS <= 1000,
            "idle tick must stay fast enough that user-driven state changes \
             (volume, EQ) propagate to the view within a second"
        );
    }
}
