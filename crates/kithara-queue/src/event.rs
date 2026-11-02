use kithara_events::{Event, TrackId};

/// Why queue navigation advanced away from the previous current track.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AdvanceReason {
    InitialLoad,
    NaturalEof,
    CrossfadePreArm,
    UserSelect,
    UserNext,
    UserPrev,
    TrackFailed,
    RemovedCurrent,
    Repeat,
    Cancelled,
}

/// Queue repeat mode mirrored into the event surface.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum QueueRepeatMode {
    Off,
    One,
    All,
}

/// Loading lifecycle of a track in the queue.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TrackStatus {
    /// Queued but loading has not started.
    Pending,
    /// Currently loading.
    Loading,
    /// Loading longer than the soft timeout.
    Slow,
    /// Loaded and ready for playback.
    Loaded,
    /// Loading failed. The string renders the underlying error.
    Failed(String),
    /// Consumed by the engine after playback — needs a fresh load before
    /// it can be selected again.
    Consumed,
    /// In-flight load was overridden by a later [`Queue::select`] of a
    /// different track. The slot is intentionally left unpopulated so
    /// auto-advance does not flip onto a track the user explicitly
    /// walked away from. An explicit `select(id)` from the user
    /// re-engages this state and triggers a fresh load.
    Cancelled,
}

/// Queue-level events emitted by `kithara-queue::Queue`.
#[derive(Clone, Debug, Event)]
pub enum QueueEvent {
    /// A new track was appended / inserted at `index`.
    TrackAdded {
        id: TrackId,
        index: usize,
    },
    /// A track was removed from the queue.
    TrackRemoved {
        id: TrackId,
    },
    /// A track's loading status changed.
    TrackStatusChanged {
        id: TrackId,
        status: TrackStatus,
    },
    /// A track's metadata changed without a status change: the cover read
    /// with its load arrived.
    TrackMetadataChanged {
        id: TrackId,
    },
    /// The currently playing track changed.
    CurrentTrackChanged {
        id: Option<TrackId>,
    },
    /// Why the current track advanced.
    CurrentTrackAdvance {
        id: Option<TrackId>,
        reason: AdvanceReason,
    },
    /// Queue reached the end and is not repeating.
    QueueEnded,
    /// The current track failed to load or continue, and the queue may have skipped it.
    TrackLoadFailed {
        id: TrackId,
        reason: String,
        auto_skipped: bool,
    },
    CrossfadeSettingsChanged {
        settings: crate::CrossfadeSettings,
    },
    PlaybackOrderChanged {
        order: crate::PlaybackOrder,
    },
    ActionAtItemEndChanged {
        action: crate::ActionAtItemEnd,
    },
    /// Repeat mode changed.
    RepeatModeChanged {
        mode: QueueRepeatMode,
    },
    /// A successor finished loading and is ready for navigation / handover.
    NextTrackReady {
        id: TrackId,
        index: usize,
    },
    /// The queue chose another track to enter, or no longer enters one.
    TransitionTargetChanged,
    /// A crossfade between tracks just started. Emitted when
    /// [`Queue::select`](https://docs.rs/kithara-queue) triggers the engine
    /// to fade from a currently-playing track to the newly selected one.
    /// UIs can inspect the exact profile captured for this transition.
    CrossfadeStarted {
        settings: crate::CrossfadeSettings,
    },
}

#[derive(Clone, Debug, Event)]
pub enum ItemEvent {
    PlaybackLikelyToKeepUp {
        id: TrackId,
        position: kithara_platform::time::Duration,
        frames: u32,
    },
    PlaybackStalled {
        id: TrackId,
        position: kithara_platform::time::Duration,
    },
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::*;

    #[kithara::test]
    fn queue_and_item_events_are_owned_by_kithara_queue() {
        assert_eq!(
            ::core::any::type_name::<QueueEvent>(),
            "kithara_queue::event::QueueEvent"
        );
        assert_eq!(
            ::core::any::type_name::<ItemEvent>(),
            "kithara_queue::event::ItemEvent"
        );
    }
}
