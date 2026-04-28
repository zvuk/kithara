//! Track lifecycle: `append`, `insert`, `remove`, `clear`, `set_tracks`
//! and the shared `insert_entry` placement path.

use std::sync::{PoisonError, atomic::Ordering};

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
    pub fn append<S: Into<TrackSource>>(&self, source: S) -> TrackId {
        self.insert_entry(source.into(), Placement::Append)
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
        Ok(self.insert_entry(source, Placement::At(pos)))
    }

    /// Shared insertion path for [`Self::append`] and [`Self::insert`].
    ///
    /// Allocates a fresh [`TrackId`], builds the [`TrackEntry`], places it
    /// per `placement`, then mirrors the track into the player, bus, and
    /// background loader. Position resolution — including fallible
    /// `after_id` lookup — happens in the caller, so this helper is
    /// infallible.
    pub(super) fn insert_entry(&self, source: TrackSource, placement: Placement) -> TrackId {
        let id = TrackId(self.next_id.fetch_add(1, Ordering::Relaxed));
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
                guard
                    .get(p + 1)
                    .or_else(|| if p > 0 { guard.get(p - 1) } else { None })
                    .map(|e| e.id)
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

        queue.remove(a).expect("removed");
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
            .expect("insert");
        let snapshot = queue.tracks();
        let ids: Vec<TrackId> = snapshot.iter().map(|e| e.id).collect();
        assert_eq!(ids, vec![a, mid, b]);
    }
}
