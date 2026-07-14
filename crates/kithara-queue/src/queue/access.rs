use kithara_events::{EventReceiver, QueueEvent, QueueRepeatMode, TrackId};

use super::Queue;
use crate::{
    navigation::RepeatMode,
    track::{TrackEntry, TrackRecord, TrackSource},
};

impl Queue {
    /// The currently playing track entry, if any.
    ///
    /// Sourced from the navigation cursor (not the player) so the queue
    /// reports `None` after `advance_to_next` runs off the end of the
    /// queue (`RepeatMode::Off` exhaustion). The player's own
    /// `current_index` stays parked at the last-played slot — read it
    /// via [`Self::current_index`] when the call site needs the
    /// last-played index even after queue-end.
    #[must_use]
    pub fn current(&self) -> Option<TrackEntry> {
        let idx = self.lock_navigation().current_index()?;
        self.lock_tracks().get(idx).map(TrackRecord::entry)
    }

    /// ABR handle of the currently playing adaptive item, if any.
    ///
    /// Returned handle drives runtime variant/bandwidth control — FFI and
    /// GUI use it for `set_abr_mode` / `set_preferred_peak_bitrate`.
    #[must_use]
    pub fn current_abr_handle(&self) -> Option<kithara_abr::AbrHandle> {
        self.player.current_abr_handle()
    }

    /// The currently playing track's queue index (player-reported).
    #[must_use]
    pub fn current_index(&self) -> Option<usize> {
        let idx = self.player.current_index();
        if idx < self.len() { Some(idx) } else { None }
    }

    delegate::delegate! {
        to self {
            /// Live variant metadata of the currently playing adaptive item.
            /// Pulled from the player's stashed ABR handle on every call so a
            /// renderer can poll for the up-to-date label after every frame
            /// without depending on event delivery.
            #[must_use]
            #[expr($?.current_variant())]
            #[call(current_abr_handle)]
            pub fn current_variant(&self) -> Option<kithara_events::VariantInfo>;
            /// Whether the queue is empty.
            #[must_use]
            #[expr($.is_empty())]
            #[call(lock_tracks)]
            pub fn is_empty(&self) -> bool;
            /// Current shuffle state.
            #[must_use]
            #[expr($.is_shuffle_enabled())]
            #[call(lock_navigation)]
            pub fn is_shuffle_enabled(&self) -> bool;
            /// Number of tracks currently in the queue.
            #[must_use]
            #[expr($.len())]
            #[call(lock_tracks)]
            pub fn len(&self) -> usize;
            /// Current repeat mode.
            #[must_use]
            #[expr($.repeat_mode())]
            #[call(lock_navigation)]
            pub fn repeat_mode(&self) -> RepeatMode;
            /// Snapshot of all track entries, in queue order.
            #[must_use]
            #[expr($.iter().map(TrackRecord::entry).collect())]
            #[call(lock_tracks)]
            pub fn tracks(&self) -> Vec<TrackEntry>;
        }
    }

    /// Set repeat mode.
    pub fn set_repeat(&self, mode: RepeatMode) {
        self.lock_navigation_mut().set_repeat(mode);
        self.bus.publish(QueueEvent::RepeatModeChanged {
            mode: map_repeat_mode(mode),
        });
    }

    /// Enable or disable shuffle.
    pub fn set_shuffle(&self, on: bool) {
        self.lock_navigation_mut().set_shuffle(on);
    }

    /// Subscribe to the unified event stream:
    /// [`QueueEvent`](kithara_events::QueueEvent) + underlying player /
    /// audio / hls / file events.
    #[must_use]
    pub fn subscribe(&self) -> EventReceiver {
        self.bus.subscribe()
    }

    /// Lookup a track entry by id.
    #[must_use]
    pub fn track(&self, id: TrackId) -> Option<TrackEntry> {
        self.lock_tracks()
            .iter()
            .find(|r| r.id == id)
            .map(TrackRecord::entry)
    }

    /// The original [`TrackSource`] for `id`, if still queued. Lets callers
    /// rebuild a resource by track identity rather than by queue position.
    #[must_use]
    pub fn track_source(&self, id: TrackId) -> Option<TrackSource> {
        self.tracks.source(id)
    }
}

fn map_repeat_mode(mode: RepeatMode) -> QueueRepeatMode {
    match mode {
        RepeatMode::Off => QueueRepeatMode::Off,
        RepeatMode::One => QueueRepeatMode::One,
        RepeatMode::All => QueueRepeatMode::All,
    }
}
