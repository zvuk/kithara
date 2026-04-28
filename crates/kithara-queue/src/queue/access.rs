//! Read-only API + navigation passthroughs (`subscribe`, `len`, `current`,
//! `tracks`, `repeat_mode`, …). No state mutation lives here.

use std::sync::Arc;

use kithara_events::{EventReceiver, TrackId};
use kithara_play::PlayerImpl;

use super::Queue;
use crate::{navigation::RepeatMode, track::TrackEntry};

impl Queue {
    /// Escape hatch — direct access to the underlying [`PlayerImpl`]. iOS /
    /// Android SDK bindings should not need this.
    #[must_use]
    pub fn player(&self) -> &Arc<PlayerImpl> {
        &self.player
    }

    /// ABR handle of the currently playing adaptive item, if any.
    ///
    /// Returned handle drives runtime variant/bandwidth control — FFI and
    /// GUI use it for `set_abr_mode` / `set_preferred_peak_bitrate`.
    #[must_use]
    pub fn current_abr_handle(&self) -> Option<kithara_abr::AbrHandle> {
        self.player.current_abr_handle()
    }

    /// Subscribe to the unified event stream:
    /// [`QueueEvent`](kithara_events::QueueEvent) + underlying player /
    /// audio / hls / file events.
    #[must_use]
    pub fn subscribe(&self) -> EventReceiver {
        self.bus.subscribe()
    }

    /// Number of tracks currently in the queue.
    #[must_use]
    pub fn len(&self) -> usize {
        self.lock_tracks().len()
    }

    /// Whether the queue is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.lock_tracks().is_empty()
    }

    /// Snapshot of all track entries, in queue order.
    #[must_use]
    pub fn tracks(&self) -> Vec<TrackEntry> {
        self.lock_tracks().clone()
    }

    /// Lookup a track entry by id.
    #[must_use]
    pub fn track(&self, id: TrackId) -> Option<TrackEntry> {
        self.lock_tracks().iter().find(|e| e.id == id).cloned()
    }

    /// The currently playing track entry, if any.
    #[must_use]
    pub fn current(&self) -> Option<TrackEntry> {
        let idx = self.player.current_index();
        self.lock_tracks().get(idx).cloned()
    }

    /// The currently playing track's queue index (player-reported).
    #[must_use]
    pub fn current_index(&self) -> Option<usize> {
        let idx = self.player.current_index();
        if idx < self.len() { Some(idx) } else { None }
    }

    /// Enable or disable shuffle.
    pub fn set_shuffle(&self, on: bool) {
        self.lock_navigation_mut().set_shuffle(on);
    }

    /// Current shuffle state.
    #[must_use]
    pub fn is_shuffle_enabled(&self) -> bool {
        self.lock_navigation().is_shuffle_enabled()
    }

    /// Set repeat mode.
    pub fn set_repeat(&self, mode: RepeatMode) {
        self.lock_navigation_mut().set_repeat(mode);
    }

    /// Current repeat mode.
    #[must_use]
    pub fn repeat_mode(&self) -> RepeatMode {
        self.lock_navigation().repeat_mode()
    }
}
