use std::sync::PoisonError;

use kithara_events::{AdvanceReason, QueueEvent, TrackId, TrackStatus};
use kithara_platform::{sync::Arc, tokio::task};
use kithara_play::{Resource, SelectTransition};
use tracing::{debug, warn};

use super::{
    Queue,
    types::{PendingSelect, SelectPhase, Transition},
};
use crate::{
    attempts::LoadClass,
    error::QueueError,
    navigation::RepeatMode,
    track::{TrackEntry, TrackSource},
};

impl Queue {
    /// Advance to the next track per navigation rules. Returns the newly
    /// selected id, or `None` when the queue has ended (and
    /// [`RepeatMode::Off`](crate::navigation::RepeatMode::Off) is active).
    pub fn advance_to_next(
        &self,
        transition: Transition,
        reason: AdvanceReason,
    ) -> Option<TrackId> {
        let Some(next) = self.next_selectable_entry() else {
            self.lock_navigation_mut().finish();
            self.bus.publish(QueueEvent::QueueEnded);
            return None;
        };
        let id = next.id;
        if let Err(e) = self.select_with_reason(id, transition, reason) {
            warn!(id = id.as_u64(), error = %e, "advance_to_next: select failed");
        }
        Some(id)
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
            guard.iter().position(|e| e.id == id)
        };
        if let Some(index) = index {
            self.player.clear_item(index);
        }
    }

    /// Read the next selectable entry without mutating navigation. Selection
    /// commits navigation only when the player selection actually commits.
    pub(super) fn next_selectable_entry(&self) -> Option<TrackEntry> {
        let len = self.len();
        if len == 0 {
            return None;
        }
        let (current, repeat) = {
            let nav = self.lock_navigation();
            (nav.current_index(), nav.repeat_mode())
        };
        let mut indices = Vec::with_capacity(len);
        match (current, repeat) {
            (None, _) => indices.extend(0..len),
            (Some(idx), RepeatMode::One) => indices.push(idx),
            (Some(idx), RepeatMode::Off) => {
                indices.extend(idx.saturating_add(1)..len);
            }
            (Some(idx), RepeatMode::All) => {
                indices.extend(idx.saturating_add(1)..len);
                indices.extend(0..=idx);
            }
        }
        let tracks = self.lock_tracks();
        indices.into_iter().find_map(|idx| {
            let record = tracks.get(idx)?;
            if matches!(record.status, TrackStatus::Cancelled) {
                debug!(
                    id = record.id.as_u64(),
                    "advance_to_next: skipping cancelled track"
                );
                None
            } else {
                Some(record.entry())
            }
        })
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
            SelectPhase::Pending(prev) if prev.id != new.id => Some(prev.id),
            _ => None,
        };
        *p = SelectPhase::Pending(new);
        drop(p);
        if let Some(prev_id) = prev_id {
            self.set_status(prev_id, TrackStatus::Cancelled);
            self.evict_player_item(prev_id);
        }
    }

    /// Go back to the previous track. Returns the newly selected id, or
    /// `None` at index 0.
    pub fn return_to_previous(&self, transition: Transition) -> Option<TrackId> {
        let prev_idx = self.lock_navigation_mut().prev()?;
        let id = self.lock_tracks().get(prev_idx).map(|e| e.id)?;
        if let Err(e) = self.select_with_reason(id, transition, AdvanceReason::UserPrev) {
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
        self.select_with_reason(id, transition, AdvanceReason::UserSelect)
    }

    pub(super) fn select_with_reason(
        &self,
        id: TrackId,
        transition: Transition,
        reason: AdvanceReason,
    ) -> Result<(), QueueError> {
        // Serialise the whole select against a concurrent
        // `spawn_apply_after_load` completion (see `select_apply`): the
        // supersede (marking the prior pending `Cancelled`) and a loading
        // track's apply must not interleave, or the superseded track barges
        // in. Held across the synchronous body only.
        let _apply = self.lock_select_apply();
        let (index, status) = {
            let guard = self.lock_tracks();
            guard
                .iter()
                .enumerate()
                .find(|(_, e)| e.id == id)
                .map(|(i, e)| (i, e.status.clone()))
                .ok_or(QueueError::UnknownTrackId(id))?
        };

        if self.player.current_index() == index && self.player.is_playing() {
            self.cancel_stale_pending(id);
            return Ok(());
        }

        match status {
            TrackStatus::Loaded => {
                self.cancel_stale_pending(id);
                let was_playing = !self.is_paused();
                let crossfade = transition.crossfade_seconds(self.player.crossfade_duration());
                if was_playing && crossfade > 0.0 {
                    self.bus.publish(QueueEvent::CrossfadeStarted {
                        duration_seconds: crossfade,
                    });
                }
                self.player.select_item_with_crossfade(
                    index,
                    SelectTransition {
                        crossfade_seconds: crossfade,
                        should_autoplay: true,
                    },
                )?;
                self.lock_navigation_mut().select(index);
                self.bus.publish(QueueEvent::CurrentTrackAdvance {
                    id: Some(id),
                    reason,
                });
                self.set_status(id, TrackStatus::Consumed);
                Ok(())
            }
            TrackStatus::Pending | TrackStatus::Loading | TrackStatus::Slow => {
                self.override_pending_select(PendingSelect { id, transition });
                self.promote_pending_load(id);
                Ok(())
            }
            TrackStatus::Cancelled | TrackStatus::Consumed | TrackStatus::Failed(_) => {
                if let Some(result) = self.try_replant_test_resource(id, index, transition) {
                    return result;
                }
                let source = self.tracks.source(id).ok_or(QueueError::NotReady(id))?;
                self.override_pending_select(PendingSelect { id, transition });
                self.set_status(id, TrackStatus::Pending);
                self.spawn_apply_after_load(id, source, LoadClass::Interactive);
                Ok(())
            }
            _ => Err(QueueError::NotReady(id)),
        }
    }

    pub(super) fn spawn_apply_after_load(
        &self,
        id: TrackId,
        source: TrackSource,
        class: LoadClass,
    ) {
        let handle = self.loader.spawn_load(id, source, class);
        self.watch_apply(id, handle);
    }

    fn promote_pending_load(&self, id: TrackId) {
        if let Some(source) = self.tracks.source(id) {
            let handle = self.loader.promote_load(id, source);
            self.watch_apply(id, handle);
        }
    }

    fn watch_apply(
        &self,
        id: TrackId,
        handle: Option<task::JoinHandle<Result<Resource, QueueError>>>,
    ) {
        let Some(handle) = handle else {
            return;
        };
        let player = Arc::clone(&self.player);
        let tracks = Arc::clone(&self.tracks);
        let pending_select = Arc::clone(&self.pending_select);
        let navigation = Arc::clone(&self.navigation);
        let select_apply = Arc::clone(&self.select_apply);
        let bus = self.bus.clone();
        drop(task::spawn(async move {
            let resource = match handle.await {
                Ok(Ok(resource)) => resource,
                Ok(Err(_)) => return,
                Err(join_err) => {
                    warn!(id = id.as_u64(), error = %join_err, "loader join failed");
                    return;
                }
            };

            // Held across the whole synchronous block (never across .await):
            // the Cancelled re-check and select_item must be atomic w.r.t. a
            // superseding select.
            let _apply = select_apply.lock().unwrap_or_else(PoisonError::into_inner);

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
            if tracks.lock().get(index).is_some_and(|entry| entry.id == id) {
                bus.publish(QueueEvent::NextTrackReady { id, index });
            }

            let pending_transition = {
                let mut p = pending_select
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner);
                let result = match *p {
                    SelectPhase::Pending(pending) if pending.id == id => {
                        *p = SelectPhase::Idle;
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
                let was_playing = player.rate() > 0.0;
                let crossfade = transition.crossfade_seconds(player.crossfade_duration());
                if was_playing && crossfade > 0.0 {
                    bus.publish(QueueEvent::CrossfadeStarted {
                        duration_seconds: crossfade,
                    });
                }
                if let Err(e) = player.select_item_with_crossfade(
                    index,
                    SelectTransition {
                        crossfade_seconds: crossfade,
                        should_autoplay: true,
                    },
                ) {
                    warn!(id = id.as_u64(), error = %e, "pending select failed");
                } else {
                    navigation
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .select(index);
                    bus.publish(QueueEvent::CurrentTrackAdvance {
                        id: Some(id),
                        reason: AdvanceReason::UserSelect,
                    });
                    mark_consumed();
                }
            }
        }));
    }

    /// Test-only path: if a respawn resource was pre-supplied via
    /// `supply_test_resource_for_respawn`, plant it directly and select
    /// synchronously, bypassing the real loader. Returns `Some(result)`
    /// when the test path took the request, `None` to fall through to
    /// the production loader respawn.
    #[cfg(any(test, feature = "probe"))]
    fn try_replant_test_resource(
        &self,
        id: TrackId,
        index: usize,
        transition: Transition,
    ) -> Option<Result<(), QueueError>> {
        let cached = self
            .test_resources
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(&id);
        let resource = cached?;
        self.player.replace_item(index, resource);
        self.set_status(id, TrackStatus::Loaded);
        let was_playing = !self.is_paused();
        let crossfade = transition.crossfade_seconds(self.player.crossfade_duration());
        if was_playing && crossfade > 0.0 {
            self.bus.publish(QueueEvent::CrossfadeStarted {
                duration_seconds: crossfade,
            });
        }
        if let Err(err) = self.player.select_item_with_crossfade(
            index,
            SelectTransition {
                crossfade_seconds: crossfade,
                should_autoplay: true,
            },
        ) {
            return Some(Err(err.into()));
        }
        self.lock_navigation_mut().select(index);
        self.bus.publish(QueueEvent::CurrentTrackAdvance {
            id: Some(id),
            reason: AdvanceReason::UserSelect,
        });
        self.set_status(id, TrackStatus::Consumed);
        Some(Ok(()))
    }

    #[cfg(not(any(test, feature = "probe")))]
    fn try_replant_test_resource(
        &self,
        _id: TrackId,
        _index: usize,
        _transition: Transition,
    ) -> Option<Result<(), QueueError>> {
        let _ = self;
        None
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
        let phase = *queue
            .pending_select
            .lock()
            .expect("BUG: pending_select Mutex is not held across await");
        match phase {
            SelectPhase::Pending(pending) => {
                assert_eq!(pending.id, id);
                assert_eq!(pending.transition, Transition::None);
            }
            SelectPhase::Idle => panic!("BUG: select stashes pending entry"),
        }
    }

    #[kithara::test(tokio)]
    async fn advance_to_next_on_empty_emits_queue_ended() {
        let queue = make_queue();
        let mut rx = queue.subscribe();
        assert!(
            queue
                .advance_to_next(Transition::Crossfade, AdvanceReason::NaturalEof)
                .is_none()
        );
        let saw_ended =
            wait_for_queue_event(&mut rx, |ev| matches!(ev, QueueEvent::QueueEnded), 200).await;
        assert!(saw_ended);
    }

    #[kithara::test(tokio)]
    async fn advance_to_next_cycles_then_emits_queue_ended() {
        let queue = make_queue();
        let _a = queue.append("https://example.com/a.mp3");
        let _b = queue.append("https://example.com/b.mp3");
        queue.lock_navigation_mut().select(1);
        let mut rx = queue.subscribe();

        assert!(
            queue
                .advance_to_next(Transition::Crossfade, AdvanceReason::NaturalEof)
                .is_none()
        );

        let saw_ended =
            wait_for_queue_event(&mut rx, |ev| matches!(ev, QueueEvent::QueueEnded), 400).await;
        assert!(saw_ended, "QueueEnded should be broadcast at end-of-queue");
    }

    #[kithara::test(tokio)]
    async fn advance_to_pending_next_does_not_move_navigation_before_select_commit() {
        let queue = make_queue();
        let first = queue.append("https://example.com/a.mp3");
        let second = queue.append("https://example.com/b.mp3");
        queue.lock_navigation_mut().select(0);
        queue.set_status(first, TrackStatus::Consumed);
        queue.set_status(second, TrackStatus::Pending);

        assert_eq!(
            queue.advance_to_next(Transition::Crossfade, AdvanceReason::NaturalEof),
            Some(second)
        );
        assert_eq!(
            queue.lock_navigation().current_index(),
            Some(0),
            "navigation must stay on the audible item until pending select commits"
        );
    }
}
