use kithara_bufpool::HasPool;
use kithara_events::TrackId;

use crate::{
    attempts::LoadClass,
    event::TrackStatus,
    queue::{
        QueueControl,
        types::{PendingSelect, SelectPhase},
    },
    track::TrackSource,
};

impl<S> QueueControl<S>
where
    S: HasPool<u8> + HasPool<f32> + Send + Sync + 'static,
{
    /// Synchronous-select counterpart to [`Self::override_pending_select`]:
    /// the user picked a `Loaded` track, so any other in-flight load is
    /// stale. Drop pending and mark the stale track [`TrackStatus::Cancelled`]
    /// so its `spawn_apply_after_load` completion path does not barge
    /// in on top of the just-selected track.
    pub(in crate::queue) fn cancel_stale_pending(&self, applying_id: TrackId) {
        let stale = {
            let mut p = self.lock_pending_select_mut();
            let result = match *p {
                SelectPhase::Pending(prev) if prev.id != applying_id => Some(prev.id),
                _ => None,
            };
            *p = SelectPhase::Idle;
            result
        };
        if let Some(stale_id) = stale {
            self.set_status(stale_id, TrackStatus::Cancelled);
            self.evict_player_item(stale_id);
        }
    }

    /// Drop a cancelled track's resource from the player so the
    /// near-EOF `arm_next` prefetch cannot plant it for handover. The
    /// `spawn_apply_after_load` completion path already skips
    /// `replace_item` on a cancelled status, but a fast loader can
    /// finish *before* the override runs and leave the resource in
    /// `items[index]`. This evict closes that race.
    fn evict_player_item(&self, id: TrackId) {
        let index = {
            let guard = self.lock_tracks();
            guard.iter().position(|entry| entry.id == id)
        };
        if let Some(index) = index {
            self.player.clear_item(index);
        }
    }

    /// Replace `pending_select` with a new selection. If the previous
    /// pending track is different from `new`, mark it
    /// [`TrackStatus::Cancelled`] so the in-flight load — when it
    /// finishes — does not silently plant its resource into the queue
    /// and "barge in" via auto-advance. `TrackStatus::Cancelled` is the
    /// single source of truth for this: `spawn_apply_after_load` reads
    /// it on completion and `advance_to_next` reads it when iterating.
    /// See Bug B reproducer (`tests/.../track_switch_race.rs`).
    pub(in crate::queue) fn override_pending_select(&self, new: PendingSelect) {
        let mut pending = self.lock_pending_select_mut();
        let prev_id = match *pending {
            SelectPhase::Pending(prev) if prev.id != new.id => Some(prev.id),
            _ => None,
        };
        *pending = SelectPhase::Pending(new);
        drop(pending);
        if let Some(prev_id) = prev_id {
            self.set_status(prev_id, TrackStatus::Cancelled);
            self.evict_player_item(prev_id);
        }
    }

    pub(in crate::queue) fn promote_pending_load(&self, id: TrackId) {
        if let Some(source) = self.tracks.source(id) {
            let handle = self.loader.promote_load(id, source);
            self.watch_apply(id, handle);
        }
    }

    pub(in crate::queue) fn spawn_apply_after_load(
        &self,
        id: TrackId,
        source: TrackSource<S>,
        class: LoadClass,
    ) {
        let handle = self.loader.spawn_load(id, source, class);
        self.watch_apply(id, handle);
    }
}
