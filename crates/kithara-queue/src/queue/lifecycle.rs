use std::sync::PoisonError;

use kithara_bufpool::HasPool;
use kithara_events::TrackId;

use super::{
    QueueControl,
    types::{CachedPosition, CrossfadeArm, Placement, SelectPhase, Transition, extract_track_name},
};
use crate::{
    attempts::LoadClass,
    error::QueueError,
    event::{AdvanceReason, QueueEvent},
    navigation::NavigationState,
    track::{TrackRecord, TrackSource},
};

impl<S> QueueControl<S>
where
    S: HasPool<u8> + HasPool<f32> + Send + Sync + 'static,
{
    /// Append a track. Loading starts immediately in the background.
    /// The id is allocated from the global counter via
    /// [`TrackId::allocate`]; use [`Self::append_with_id`] when the
    /// caller owns the id (FFI item pre-allocation).
    ///
    /// # Errors
    ///
    /// Returns [`QueueError::Play`] after the resident player is closed.
    pub fn append<T: Into<TrackSource<S>>>(&self, source: T) -> Result<TrackId, QueueError> {
        let source = source.into();
        self.with_open(|queue| queue.insert_entry(TrackId::allocate(), source, Placement::Append))
            .map_err(QueueError::from)
    }

    /// Append a track with a caller-supplied id. The id MUST come from
    /// [`TrackId::allocate`] so it stays inside the process-wide
    /// monotonic address space. Used by the FFI layer where the item
    /// reserves its id at construction and surfaces it as `audioId`
    /// before insert.
    ///
    /// # Errors
    ///
    /// Returns [`QueueError::Play`] after the resident player is closed.
    pub fn append_with_id<T: Into<TrackSource<S>>>(
        &self,
        id: TrackId,
        source: T,
    ) -> Result<TrackId, QueueError> {
        let source = source.into();
        self.with_open(|queue| queue.insert_entry(id, source, Placement::Append))
            .map_err(QueueError::from)
    }

    /// Remove all tracks from the queue. Dropping the records aborts
    /// their in-flight loads.
    pub fn clear(&self) {
        self.command(Self::clear_inner);
    }

    fn clear_inner(&self) {
        let ids: Vec<TrackId> = {
            let _apply = self.lock_select_apply();
            let mut guard = self.lock_tracks_mut();
            let ids = guard.iter().map(|r| r.id).collect();
            guard.clear();
            drop(guard);

            *self.lock_pending_select_mut() = SelectPhase::Idle;
            let mut navigation = self.lock_navigation_mut();
            let repeat = navigation.repeat_mode();
            let shuffle = navigation.is_shuffle_enabled();
            *navigation = NavigationState::new(navigation.history_limit());
            navigation.set_repeat(repeat);
            navigation.set_shuffle(shuffle);
            drop(navigation);
            self.write_armed_for(CrossfadeArm::Disarmed);
            self.write_cached_position(CachedPosition::Unknown);
            self.player.remove_all_items();
            ids
        };
        *self
            .player_rx
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = self.bus.subscribe();
        for id in ids {
            self.bus.publish(QueueEvent::TrackRemoved { id });
        }
    }

    /// Insert a track after the given id, or at the head when `after` is
    /// `None`. Loading starts immediately.
    ///
    /// # Errors
    /// Returns [`QueueError::UnknownTrackId`] if `after` does not match any
    /// track.
    pub fn insert<T: Into<TrackSource<S>>>(
        &self,
        source: T,
        after: Option<TrackId>,
    ) -> Result<TrackId, QueueError> {
        let source = source.into();
        self.with_open_result(|queue| {
            queue.insert_with_id_inner(TrackId::allocate(), source, after)
        })
    }

    /// Inserts a resolved track placement into queue state and starts loading.
    pub(super) fn insert_entry(
        &self,
        id: TrackId,
        source: TrackSource<S>,
        placement: Placement,
    ) -> TrackId {
        let record = TrackRecord::new(id, extract_track_name(&source), source.clone());

        let index = {
            let mut guard = self.lock_tracks_mut();
            match placement {
                Placement::Append => {
                    guard.push(record);
                    guard.len() - 1
                }
                Placement::At(pos) => {
                    guard.insert(pos, record);
                    pos
                }
            }
        };
        self.player.reserve_slots(self.len());
        self.bus.publish(QueueEvent::TrackAdded { id, index });
        self.spawn_apply_after_load(id, source, LoadClass::Prefetch);
        id
    }

    /// Insert a track with a caller-supplied id. See
    /// [`Self::append_with_id`] for why the id MUST come from
    /// [`TrackId::allocate`].
    ///
    /// # Errors
    /// Returns [`QueueError::UnknownTrackId`] if `after` does not match
    /// any track.
    pub fn insert_with_id<T: Into<TrackSource<S>>>(
        &self,
        id: TrackId,
        source: T,
        after: Option<TrackId>,
    ) -> Result<TrackId, QueueError> {
        let source = source.into();
        self.with_open_result(|queue| queue.insert_with_id_inner(id, source, after))
    }

    fn insert_with_id_inner(
        &self,
        id: TrackId,
        source: TrackSource<S>,
        after: Option<TrackId>,
    ) -> Result<TrackId, QueueError> {
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
        self.with_open_result(|queue| queue.remove_inner(id))
    }

    fn remove_inner(&self, id: TrackId) -> Result<(), QueueError> {
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
        let _ = self.player.remove_at(index)?;
        self.bus.publish(QueueEvent::TrackRemoved { id });

        if was_current {
            if let Some(next) = successor_id {
                let _ =
                    self.select_with_reason(next, Transition::None, AdvanceReason::RemovedCurrent);
            } else {
                self.player.pause();
            }
        }
        Ok(())
    }

    /// Replace the entire queue with the given sources.
    pub fn set_tracks<I, T>(&self, sources: I)
    where
        I: IntoIterator<Item = T>,
        T: Into<TrackSource<S>>,
    {
        self.command(|queue| {
            queue.clear_inner();
            for source in sources {
                queue.insert_entry(TrackId::allocate(), source.into(), Placement::Append);
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use kithara_platform::sync::Arc;
    use kithara_play::{ItemRole, PlayerEvent, SlotId, TrackRef};
    use kithara_test_utils::kithara;

    use super::*;
    use crate::{
        event::QueueEvent,
        queue::state::tests::{make_queue, wait_for_queue_event},
    };

    fn append(queue: &crate::Queue<crate::test_pools::TestPools>, source: &str) -> TrackId {
        queue
            .append(source)
            .expect("BUG: open queue must accept a track")
    }

    #[kithara::test(tokio)]
    async fn len_is_empty_reflect_append() {
        let queue = make_queue();
        assert!(queue.is_empty());
        let _ = append(&queue, "https://example.com/a.mp3");
        let _ = append(&queue, "https://example.com/b.mp3");
        assert_eq!(queue.len(), 2);
    }

    #[kithara::test(tokio)]
    async fn append_returns_monotonic_ids_and_emits_track_added() {
        let queue = make_queue();
        let mut rx = queue.subscribe();
        let a = append(&queue, "https://example.com/a.mp3");
        let b = append(&queue, "https://example.com/b.mp3");
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
        let a = append(&queue, "https://example.com/a.mp3");
        let _b = append(&queue, "https://example.com/b.mp3");
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
        let _a = append(&queue, "https://example.com/a.mp3");
        let _b = append(&queue, "https://example.com/b.mp3");
        assert_eq!(queue.len(), 2);
        queue.clear();
        assert_eq!(queue.len(), 0);
    }

    #[kithara::test(tokio)]
    async fn clear_discards_old_eof_before_reinsert() {
        let queue = make_queue();
        let old = queue
            .append("https://example.com/old.mp3")
            .expect("open queue accepts a track");
        queue.lock_navigation_mut().select(0);
        queue.player.bus().publish(PlayerEvent::ItemDidPlayToEnd {
            item: ItemRole::Leading(TrackRef::new(
                old,
                SlotId::new(0),
                Arc::from(format!("test://memory/{}", old.as_u64())),
            )),
        });

        queue.clear();
        let replacement = queue
            .append("https://example.com/replacement.mp3")
            .expect("open queue accepts a replacement track");
        queue.lock_navigation_mut().select(0);
        queue.player.set_rate(1.0);

        queue
            .tick()
            .expect("tick must accept a freshly reinserted queue");

        assert_eq!(
            queue.current().map(|entry| entry.id),
            Some(replacement),
            "an EOF queued before clear must not end the replacement queue"
        );
    }

    #[kithara::test(tokio)]
    async fn set_tracks_replaces_queue() {
        let queue = make_queue();
        let _a = append(&queue, "https://example.com/a.mp3");
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
        let a = append(&queue, "https://example.com/a.mp3");
        let b = append(&queue, "https://example.com/b.mp3");
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
        let a = append(&queue, "https://example.com/a.mp3");
        let b = append(&queue, "https://example.com/b.mp3");

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
