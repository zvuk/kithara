//! Selection state machine: explicit `select`, navigation-driven
//! `advance_to_next` / `return_to_previous`, pending-select bookkeeping,
//! and the `spawn_apply_after_load` post-load hookup.

use std::sync::{Arc, PoisonError};

use kithara_events::{QueueEvent, TrackId, TrackStatus};
use tracing::{debug, warn};

use super::{
    Queue,
    types::{PendingSelect, Transition},
};
use crate::{error::QueueError, track::TrackSource};

impl Queue {
    /// Advance to the next track per navigation rules. Returns the newly
    /// selected id, or `None` when the queue has ended (and
    /// [`RepeatMode::Off`](crate::navigation::RepeatMode::Off) is active).
    pub fn advance_to_next(&self, transition: Transition) -> Option<TrackId> {
        let len = self.len();
        // `TrackStatus::Cancelled` slots stay populated in the queue
        // but their resource was never planted (load was overridden
        // by a later explicit select), so auto-advance must not flip
        // `current()` onto them. Loop until we find a playable slot
        // or run off the end of the queue. An explicit
        // [`Self::select`] of a Cancelled track is honoured via the
        // `Cancelled | Consumed | Failed` branch in `select`.
        loop {
            let Some(idx) = self.lock_navigation_mut().next(len) else {
                self.bus.publish(QueueEvent::QueueEnded);
                self.player.finish_queue();
                return None;
            };
            let Some((id, status)) = self
                .lock_tracks()
                .get(idx)
                .map(|e| (e.id, e.status.clone()))
            else {
                continue;
            };
            if matches!(status, TrackStatus::Cancelled) {
                debug!(
                    id = id.as_u64(),
                    "advance_to_next: skipping cancelled track"
                );
                continue;
            }
            if let Err(e) = self.select(id, transition) {
                warn!(id = id.as_u64(), error = %e, "advance_to_next: select failed");
            }
            return Some(id);
        }
    }

    /// Synchronous-select counterpart to [`Self::override_pending_select`]:
    /// the user picked a `Loaded` track, so any other in-flight load is
    /// stale. Drop pending and mark the stale track [`TrackStatus::Cancelled`]
    /// so its `spawn_apply_after_load` completion path does not barge
    /// in on top of the just-selected track.
    pub(super) fn cancel_stale_pending(&self, applying_id: TrackId) {
        let stale = {
            let mut p = self.lock_pending_select_mut();
            let result = match *p {
                Some(prev) if prev.id != applying_id => Some(prev.id),
                _ => None,
            };
            *p = None;
            result
        };
        if let Some(stale_id) = stale {
            self.set_status(stale_id, TrackStatus::Cancelled);
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
    pub(super) fn override_pending_select(&self, new: PendingSelect) {
        let mut p = self.lock_pending_select_mut();
        let prev_id = match *p {
            Some(prev) if prev.id != new.id => Some(prev.id),
            _ => None,
        };
        *p = Some(new);
        drop(p);
        if let Some(prev_id) = prev_id {
            self.set_status(prev_id, TrackStatus::Cancelled);
        }
    }

    /// Go back to the previous track. Returns the newly selected id, or
    /// `None` at index 0.
    pub fn return_to_previous(&self, transition: Transition) -> Option<TrackId> {
        let prev_idx = self.lock_navigation_mut().prev()?;
        let id = self.lock_tracks().get(prev_idx).map(|e| e.id)?;
        if let Err(e) = self.select(id, transition) {
            warn!(id = id.as_u64(), error = %e, "return_to_previous: select failed");
        }
        Some(id)
    }

    /// Select a track by id, applying the given [`Transition`]. If the
    /// track is still loading or pending, both the id and the
    /// transition are stashed and applied when loading finishes.
    ///
    /// # Errors
    /// Returns [`QueueError::UnknownTrackId`] if `id` is not in the queue,
    /// [`QueueError::NotReady`] if the track is in a terminal failed state,
    /// or [`QueueError::Play`] if the underlying `select_item` call fails.
    pub fn select(&self, id: TrackId, transition: Transition) -> Result<(), QueueError> {
        let (index, status) = {
            let guard = self.lock_tracks();
            guard
                .iter()
                .enumerate()
                .find(|(_, e)| e.id == id)
                .map(|(i, e)| (i, e.status.clone()))
                .ok_or(QueueError::UnknownTrackId(id))?
        };

        // Idempotency: selecting the track that's already loaded & active
        // is a no-op. Autoplay in `spawn_apply_after_load` issues an
        // implicit select; an explicit user select on the same track must
        // not re-issue LoadTrack — that wipes the decoder timeline and
        // resets position to zero mid-playback. We check `player.current_index`
        // (updated synchronously inside `select_item_with_crossfade`) rather
        // than `navigation.current_index` (which `advance_to_next` moves
        // before calling select, so it would mistakenly match).
        // `rate > 0` distinguishes "player holds this slot" from the
        // default `current_index = 0` on a fresh, never-selected player;
        // `select_item_with_crossfade` with `autoplay=true` sets `rate`
        // synchronously, while `is_playing()` depends on the processor
        // draining `SetPaused(false)` — racy.
        if self.player.current_index() == index && self.player.rate() > 0.0 {
            return Ok(());
        }

        match status {
            TrackStatus::Loaded => {
                // Cancel any other in-flight pending — otherwise its
                // spawn_apply_after_load completion would still plant
                // its resource and `select_item_with_crossfade` it,
                // barging in over the track the user just selected.
                self.cancel_stale_pending(id);
                let was_playing = self.player.is_playing();
                let crossfade = transition.crossfade_seconds(self.player.crossfade_duration());
                self.player
                    .select_item_with_crossfade(index, true, crossfade)?;
                self.lock_navigation_mut().select(index);
                if was_playing && crossfade > 0.0 {
                    self.bus.publish(QueueEvent::CrossfadeStarted {
                        duration_seconds: crossfade,
                    });
                }
                // Engine took items[index] inside select_item — if user
                // re-selects the same track later, we must reload.
                self.set_status(id, TrackStatus::Consumed);
                Ok(())
            }
            TrackStatus::Pending | TrackStatus::Loading | TrackStatus::Slow => {
                self.override_pending_select(PendingSelect { id, transition });
                Ok(())
            }
            TrackStatus::Cancelled | TrackStatus::Consumed | TrackStatus::Failed(_) => {
                let source = self
                    .sources
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .get(&id)
                    .cloned()
                    .ok_or(QueueError::NotReady(id))?;
                self.override_pending_select(PendingSelect { id, transition });
                self.set_status(id, TrackStatus::Pending);
                self.spawn_apply_after_load(id, source);
                Ok(())
            }
            _ => Err(QueueError::NotReady(id)),
        }
    }

    pub(super) fn spawn_apply_after_load(&self, id: TrackId, source: TrackSource) {
        let handle = self.loader.spawn_load(id, source);
        let player = Arc::clone(&self.player);
        let tracks = Arc::clone(&self.tracks);
        let pending_select = Arc::clone(&self.pending_select);
        let navigation = Arc::clone(&self.navigation);
        let bus = self.bus.clone();
        tokio::spawn(async move {
            let resource = match handle.await {
                Ok(Ok(resource)) => resource,
                Ok(Err(_)) => return,
                Err(join_err) => {
                    warn!(id = id.as_u64(), error = %join_err, "loader join failed");
                    return;
                }
            };

            // Bug B (barge-in): if a later `select` overrode this
            // load's `pending_select`, the previous track was marked
            // `TrackStatus::Cancelled` synchronously by
            // `override_pending_select`. Skip `replace_item` so the
            // slot stays empty and auto-advance falls through; the
            // status remains `Cancelled` so an explicit user
            // `select(id)` still re-engages via the
            // `Cancelled | Consumed | Failed` branch in `select`.
            let was_cancelled = tracks
                .lock()
                .iter()
                .find(|e| e.id == id)
                .is_some_and(|e| matches!(e.status, TrackStatus::Cancelled));
            if was_cancelled {
                debug!(
                    id = id.as_u64(),
                    "load was overridden by a later select; skipping replace_item"
                );
                return;
            }

            let index = {
                let guard = tracks.lock();
                guard.iter().position(|e| e.id == id)
            };
            let Some(index) = index else {
                debug!(
                    id = id.as_u64(),
                    "load completed but track no longer in queue"
                );
                return;
            };

            player.replace_item(index, resource);
            tracks.set_status(id, TrackStatus::Loaded);

            let pending_transition = {
                let mut p = pending_select
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner);
                let result = match *p {
                    Some(pending) if pending.id == id => {
                        *p = None;
                        Some(pending.transition)
                    }
                    _ => None,
                };
                drop(p);
                result
            };
            let mark_consumed = || {
                tracks.set_status(id, TrackStatus::Consumed);
            };

            if let Some(transition) = pending_transition {
                let was_playing = player.is_playing();
                let crossfade = transition.crossfade_seconds(player.crossfade_duration());
                if let Err(e) = player.select_item_with_crossfade(index, true, crossfade) {
                    warn!(id = id.as_u64(), error = %e, "pending select failed");
                } else {
                    navigation
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .select(index);
                    if was_playing && crossfade > 0.0 {
                        bus.publish(QueueEvent::CrossfadeStarted {
                            duration_seconds: crossfade,
                        });
                    }
                    mark_consumed();
                }
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use kithara_events::QueueEvent;
    use kithara_test_utils::kithara;

    use super::*;
    use crate::queue::state::tests::{make_queue, wait_for_queue_event};

    #[kithara::test(tokio)]
    async fn select_unknown_id_errors() {
        let queue = make_queue();
        let err = queue
            .select(TrackId(999), Transition::None)
            .expect_err("unknown id should error");
        assert!(matches!(err, QueueError::UnknownTrackId(_)));
    }

    #[kithara::test(tokio)]
    async fn select_pending_track_stashes_pending_select() {
        let queue = make_queue();
        let id = queue.append("https://example.com/a.mp3");
        let _ = queue.select(id, Transition::None);
        let pending = queue
            .pending_select
            .lock()
            .expect("pending_select lock")
            .to_owned();
        let pending = pending.expect("pending set");
        assert_eq!(pending.id, id);
        assert_eq!(pending.transition, Transition::None);
    }

    #[kithara::test(tokio)]
    async fn advance_to_next_on_empty_emits_queue_ended() {
        let queue = make_queue();
        let mut rx = queue.subscribe();
        assert!(queue.advance_to_next(Transition::Crossfade).is_none());
        let saw_ended =
            wait_for_queue_event(&mut rx, |ev| matches!(ev, QueueEvent::QueueEnded), 200).await;
        assert!(saw_ended);
    }

    #[kithara::test(tokio)]
    async fn advance_to_next_cycles_then_emits_queue_ended() {
        let queue = make_queue();
        let _a = queue.append("https://example.com/a.mp3");
        let _b = queue.append("https://example.com/b.mp3");
        let mut rx = queue.subscribe();

        assert!(queue.advance_to_next(Transition::Crossfade).is_some());
        assert!(queue.advance_to_next(Transition::Crossfade).is_some());
        assert!(queue.advance_to_next(Transition::Crossfade).is_none());

        let saw_ended =
            wait_for_queue_event(&mut rx, |ev| matches!(ev, QueueEvent::QueueEnded), 400).await;
        assert!(saw_ended, "QueueEnded should be broadcast at end-of-queue");
    }
}
