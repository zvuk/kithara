use std::sync::PoisonError;
#[cfg(any(test, feature = "probe"))]
use std::sync::atomic::Ordering;

use kithara_events::{QueueEvent, TrackId, TrackStatus};

use super::{
    Queue,
    types::{Placement, Transition, extract_track_name},
};
use crate::{
    error::QueueError,
    track::{TrackEntry, TrackSource},
};

impl Queue {
    /// Append a track. Loading starts immediately in the background.
    /// The id is allocated from the global counter via
    /// [`TrackId::allocate`]; use [`Self::append_with_id`] when the
    /// caller owns the id (FFI item pre-allocation).
    pub fn append<S: Into<TrackSource>>(&self, source: S) -> TrackId {
        self.insert_entry(TrackId::allocate(), source.into(), Placement::Append)
    }

    /// Append a track with a caller-supplied id. The id MUST come from
    /// [`TrackId::allocate`] so it stays inside the process-wide
    /// monotonic address space. Used by the FFI layer where the item
    /// reserves its id at construction and surfaces it as `audioId`
    /// before insert.
    pub fn append_with_id<S: Into<TrackSource>>(&self, id: TrackId, source: S) -> TrackId {
        self.insert_entry(id, source.into(), Placement::Append)
    }

    /// Remove all tracks from the queue.
    pub fn clear(&self) {
        let ids: Vec<TrackId> = {
            let mut guard = self.lock_tracks_mut();
            let ids = guard.iter().map(|e| e.id).collect();
            guard.clear();
            ids
        };
        self.sources
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clear();
        self.player.remove_all_items();
        for id in ids {
            self.bus.publish(QueueEvent::TrackRemoved { id });
        }
    }

    /// Test helper: drive a pre-built [`kithara_play::Resource`] into the
    /// player slot for an id previously created via
    /// [`Self::register_for_test`]. Mirrors the synchronous portion of
    /// the loader's `apply_after_load` callback.
    #[cfg(any(test, feature = "probe"))]
    pub fn complete_load_for_test(&self, id: TrackId, resource: kithara_play::Resource) {
        let index = {
            let guard = self.lock_tracks();
            guard.iter().position(|e| e.id == id)
        };
        if let Some(index) = index {
            self.player.replace_item(index, resource);
            self.set_status(id, TrackStatus::Loaded);
            if self.should_autoplay
                && self
                    .autoplay_target
                    .compare_exchange(
                        id.as_u64(),
                        Self::NO_ARMED_TRACK,
                        Ordering::AcqRel,
                        Ordering::Acquire,
                    )
                    .is_ok()
                && let Err(err) = self.select(id, Transition::None)
            {
                tracing::warn!(id = id.as_u64(), %err, "autoplay select failed");
            }
        }
    }

    /// Insert a track after the given id, or at the head when `after` is
    /// `None`. Loading starts immediately.
    ///
    /// # Errors
    /// Returns [`QueueError::UnknownTrackId`] if `after` does not match any
    /// track.
    pub fn insert<S: Into<TrackSource>>(
        &self,
        source: S,
        after: Option<TrackId>,
    ) -> Result<TrackId, QueueError> {
        self.insert_with_id(TrackId::allocate(), source, after)
    }

    /// Shared insertion path for [`Self::append`] and [`Self::insert`].
    ///
    /// Builds the [`TrackEntry`] with `id`, places it per `placement`,
    /// then mirrors the track into the player, bus, and background
    /// loader. Position resolution — including fallible `after_id`
    /// lookup — happens in the caller, so this helper is infallible.
    pub(super) fn insert_entry(
        &self,
        id: TrackId,
        source: TrackSource,
        placement: Placement,
    ) -> TrackId {
        let entry = TrackEntry {
            id,
            name: extract_track_name(&source),
            url: source.uri().map(str::to_string),
            status: TrackStatus::Pending,
        };

        let index = {
            let mut guard = self.lock_tracks_mut();
            match placement {
                Placement::Append => {
                    guard.push(entry);
                    guard.len() - 1
                }
                Placement::At(pos) => {
                    guard.insert(pos, entry);
                    pos
                }
            }
        };
        self.sources
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(id, source.clone());
        self.player.reserve_slots(self.len());
        self.bus.publish(QueueEvent::TrackAdded { id, index });
        self.spawn_apply_after_load(id, source);
        id
    }

    /// Test helper: convenience for the common case where load order
    /// matches register order. Equivalent to
    /// [`Self::register_for_test`] + [`Self::complete_load_for_test`].
    #[cfg(any(test, feature = "probe"))]
    pub fn insert_loaded_for_test(&self, resource: kithara_play::Resource) -> TrackId {
        let id = self.register_for_test();
        self.complete_load_for_test(id, resource);
        id
    }

    /// Insert a track with a caller-supplied id. See
    /// [`Self::append_with_id`] for why the id MUST come from
    /// [`TrackId::allocate`].
    ///
    /// # Errors
    /// Returns [`QueueError::UnknownTrackId`] if `after` does not match
    /// any track.
    pub fn insert_with_id<S: Into<TrackSource>>(
        &self,
        id: TrackId,
        source: S,
        after: Option<TrackId>,
    ) -> Result<TrackId, QueueError> {
        let source = source.into();
        let pos = {
            let guard = self.lock_tracks();
            match after {
                None => 0,
                Some(after_id) => guard
                    .iter()
                    .position(|e| e.id == after_id)
                    .map(|i| i + 1)
                    .ok_or(QueueError::UnknownTrackId(after_id))?,
            }
        };
        Ok(self.insert_entry(id, source, Placement::At(pos)))
    }

    /// Test helper: register a placeholder track entry without starting
    /// a real loader. Pair with [`Self::complete_load_for_test`] to
    /// drive the loaded resource into the player on demand.
    #[cfg(any(test, feature = "probe"))]
    pub fn register_for_test(&self) -> TrackId {
        let id = TrackId::allocate();
        let url = format!("test://memory/{}", id.as_u64());
        let entry = TrackEntry {
            id,
            name: format!("test-{}", id.as_u64()),
            url: Some(url.clone()),
            status: TrackStatus::Pending,
        };
        let index = {
            let mut guard = self.lock_tracks_mut();
            guard.push(entry);
            guard.len() - 1
        };
        self.sources
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(id, TrackSource::Uri(url));
        self.player.reserve_slots(self.len());
        if self.should_autoplay {
            let _ = self.autoplay_target.compare_exchange(
                Self::NO_ARMED_TRACK,
                id.as_u64(),
                Ordering::AcqRel,
                Ordering::Acquire,
            );
        }
        self.bus.publish(QueueEvent::TrackAdded { id, index });
        id
    }

    /// Remove a track from the queue by id.
    ///
    /// If the removed track is currently playing:
    /// - with tracks remaining → switches to the next (or previous if
    ///   we were at the tail) with an immediate cut.
    /// - with no tracks remaining → pauses the player.
    ///
    /// # Errors
    /// Returns [`QueueError::UnknownTrackId`] if `id` is not in the queue.
    pub fn remove(&self, id: TrackId) -> Result<(), QueueError> {
        let was_current = self.current().map(|e| e.id) == Some(id);
        let successor_id = if was_current {
            let guard = self.lock_tracks();
            let pos = guard.iter().position(|e| e.id == id);
            let result = pos.and_then(|p| {
                let next = guard.get(p + 1);
                let prev = if p > 0 { guard.get(p - 1) } else { None };
                next.or(prev).map(|e| e.id)
            });
            drop(guard);
            result
        } else {
            None
        };

        let index = {
            let mut guard = self.lock_tracks_mut();
            let pos = guard
                .iter()
                .position(|e| e.id == id)
                .ok_or(QueueError::UnknownTrackId(id))?;
            guard.remove(pos);
            pos
        };
        self.sources
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(&id);
        let _ = self.player.remove_at(index);
        self.bus.publish(QueueEvent::TrackRemoved { id });

        if was_current {
            if let Some(next) = successor_id {
                let _ = self.select(next, Transition::None);
            } else {
                self.player.pause();
            }
        }
        Ok(())
    }

    /// Replace the entire queue with the given sources.
    pub fn set_tracks<I, S>(&self, sources: I)
    where
        I: IntoIterator<Item = S>,
        S: Into<TrackSource>,
    {
        self.clear();
        for s in sources {
            let _ = self.append(s);
        }
    }

    /// Test helper: pre-supply a fresh [`kithara_play::Resource`] that
    /// `Queue::select` should plant when a `Consumed` / `Cancelled` /
    /// `Failed` track is re-selected. This emulates the loader-respawn
    /// path the production code uses without dispatching the real
    /// loader, so harness tests can exercise replay-after-EOF.
    #[cfg(any(test, feature = "probe"))]
    pub fn supply_test_resource_for_respawn(&self, id: TrackId, resource: kithara_play::Resource) {
        self.test_resources
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(id, resource);
    }
}

#[cfg(test)]
mod tests {
    use kithara_events::QueueEvent;
    use kithara_test_utils::kithara;

    use super::*;
    use crate::queue::state::tests::{make_queue, wait_for_queue_event};

    #[kithara::test(tokio)]
    async fn len_is_empty_reflect_append() {
        let queue = make_queue();
        assert!(queue.is_empty());
        let _ = queue.append("https://example.com/a.mp3");
        let _ = queue.append("https://example.com/b.mp3");
        assert_eq!(queue.len(), 2);
    }

    #[kithara::test(tokio)]
    async fn append_returns_monotonic_ids_and_emits_track_added() {
        let queue = make_queue();
        let mut rx = queue.subscribe();
        let a = queue.append("https://example.com/a.mp3");
        let b = queue.append("https://example.com/b.mp3");
        assert_ne!(a, b);
        assert!(a.as_u64() < b.as_u64());

        let mut seen = 0;
        while wait_for_queue_event(
            &mut rx,
            |ev| matches!(ev, QueueEvent::TrackAdded { .. }),
            200,
        )
        .await
        {
            seen += 1;
            if seen == 2 {
                break;
            }
        }
        assert_eq!(seen, 2);
    }

    #[kithara::test(tokio)]
    async fn remove_drops_from_queue_and_emits() {
        let queue = make_queue();
        let a = queue.append("https://example.com/a.mp3");
        let _b = queue.append("https://example.com/b.mp3");
        let mut rx = queue.subscribe();

        queue
            .remove(a)
            .expect("BUG: just-appended track must be removable");
        assert_eq!(queue.len(), 1);
        let saw_removed = wait_for_queue_event(
            &mut rx,
            |ev| matches!(ev, QueueEvent::TrackRemoved { id } if id == &a),
            300,
        )
        .await;
        assert!(saw_removed);
    }

    #[kithara::test(tokio)]
    async fn clear_empties_queue() {
        let queue = make_queue();
        let _a = queue.append("https://example.com/a.mp3");
        let _b = queue.append("https://example.com/b.mp3");
        assert_eq!(queue.len(), 2);
        queue.clear();
        assert_eq!(queue.len(), 0);
    }

    #[kithara::test(tokio)]
    async fn set_tracks_replaces_queue() {
        let queue = make_queue();
        let _a = queue.append("https://example.com/a.mp3");
        queue.set_tracks([
            "https://example.com/1.mp3",
            "https://example.com/2.mp3",
            "https://example.com/3.mp3",
        ]);
        assert_eq!(queue.len(), 3);
    }

    #[kithara::test(tokio)]
    async fn insert_after_id_places_next() {
        let queue = make_queue();
        let a = queue.append("https://example.com/a.mp3");
        let b = queue.append("https://example.com/b.mp3");
        let mid = queue
            .insert("https://example.com/mid.mp3", Some(a))
            .expect("BUG: insert relative to existing track");
        let snapshot = queue.tracks();
        let ids: Vec<TrackId> = snapshot.iter().map(|e| e.id).collect();
        assert_eq!(ids, vec![a, mid, b]);
    }

    #[kithara::test(tokio)]
    async fn track_source_is_keyed_by_id_across_removal() {
        let queue = make_queue();
        let a = queue.append("https://example.com/a.mp3");
        let b = queue.append("https://example.com/b.mp3");

        assert_eq!(
            queue
                .track_source(a)
                .and_then(|s| s.uri().map(str::to_string)),
            Some("https://example.com/a.mp3".to_string()),
            "source resolves by identity"
        );

        // Removing an earlier track must not shift which source `b` resolves
        // to, and the removed id must no longer have a source.
        queue.remove(a).expect("BUG: remove existing track");
        assert!(
            queue.track_source(a).is_none(),
            "removed track has no source"
        );
        assert_eq!(
            queue
                .track_source(b)
                .and_then(|s| s.uri().map(str::to_string)),
            Some("https://example.com/b.mp3".to_string()),
            "surviving track still resolves to its own source by id"
        );
    }
}
