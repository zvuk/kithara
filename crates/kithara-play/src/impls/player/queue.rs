use std::sync::atomic::Ordering;

use kithara_platform::sync::{Arc, Mutex};
use ringbuf::traits::Consumer;
use tracing::debug;

use super::{
    core::PlayerImpl,
    phase::{PendingNext, PendingNextState},
};
use crate::{
    error::PlayError,
    events::PlayerEvent,
    impls::{
        player_notification::{PlayerNotification, TrackPlaybackStopReason},
        player_processor::PlayerCmd,
        player_resource::PlayerResource,
    },
    traits::engine::Engine,
};

/// Outcome of resolving an arm request under the short phase lock, acted on
/// outside the lock to avoid holding it across `send_to_slot`.
enum ArmDecision {
    /// The same index is already armed; return its src verbatim.
    AlreadyArmed(Arc<str>),
    /// The slot was cleared; optionally unload the previous src.
    Clear(Option<Arc<str>>),
}

struct ActivatedPending {
    src: Arc<str>,
    duration_seconds: f64,
}

impl PlayerImpl {
    /// Mark the armed-next slot at `index` activated under a short lock,
    /// returning its src. `Ok(None)` when it was already activated.
    fn activate_pending(&self, index: usize) -> Result<Option<ActivatedPending>, PlayError> {
        let mut phase = self.phase.lock();
        let pending = phase
            .pending_mut()
            .and_then(|slot| slot.as_mut())
            .ok_or(PlayError::NotReady)?;
        if pending.index != index {
            return Err(PlayError::Internal(format!(
                "commit_next index mismatch: requested {index}, armed {}",
                pending.index
            )));
        }
        let outcome = if pending.state.activated() {
            None
        } else {
            let src = Arc::clone(&pending.src);
            let duration_seconds = pending.duration_seconds;
            pending.state = PendingNextState::ActivatedReady;
            Some(ActivatedPending {
                src,
                duration_seconds,
            })
        };
        drop(phase);
        Ok(outcome)
    }

    /// Load `items[index]` into the audio-thread arena in `Preloading`
    /// state, ready for sample-accurate gapless stitch (cf=0) or parallel
    /// fade (cf>0).
    ///
    /// If a different next is already armed, it is unloaded first.
    /// Idempotent for the same index. Returns `Some(src)` on success;
    /// `None` if `items[index]` is empty (loader hasn't filled it yet) or
    /// `index` is out of range.
    pub fn arm_next(&self, index: usize) -> Option<Arc<str>> {
        let current_index = self.core.current_index.load(Ordering::Relaxed);
        if index >= self.item_count() {
            return None;
        }

        // Resolve any already-armed slot under a short phase lock: either the
        // same index is already armed (return early), or it must be cleared
        // and possibly unloaded outside the lock.
        let mut phase = self.phase.lock();
        let existing = phase.pending_mut().and_then(|slot| slot.as_ref());
        let decision = match existing {
            Some(existing) if existing.index == index => {
                ArmDecision::AlreadyArmed(Arc::clone(&existing.src))
            }
            Some(existing) => {
                let unload = (!(existing.state.activated() && existing.index == current_index))
                    .then(|| Arc::clone(&existing.src));
                if let Some(slot) = phase.pending_mut() {
                    *slot = None;
                }
                ArmDecision::Clear(unload)
            }
            None => ArmDecision::Clear(None),
        };
        drop(phase);

        let to_unload = match decision {
            ArmDecision::AlreadyArmed(src) => return Some(src),
            ArmDecision::Clear(unload) => unload,
        };
        if let Some(prev_src) = to_unload {
            let _ = self.send_to_slot(PlayerCmd::UnloadTrack { src: prev_src });
        }

        let (src, duration_seconds) = self.enqueue_to_processor(index)?;
        if let Some(pending_slot) = self.phase.lock().pending_mut() {
            *pending_slot = Some(PendingNext {
                index,
                duration_seconds,
                src: Arc::clone(&src),
                state: PendingNextState::Armed,
            });
        }
        Some(src)
    }

    /// Snapshot of the armed-next index. `None` when no slot is armed
    /// (or after `commit_next` has consumed it for the current handover).
    #[must_use]
    pub fn armed_next(&self) -> Option<usize> {
        self.phase
            .lock()
            .pending()
            .filter(|pending| !pending.state.activated())
            .map(|pending| pending.index)
    }

    /// Commit the previously armed next track and start the cross-fade.
    ///
    /// Sends `FadeIn` to the audio thread for the armed slot, updates
    /// `current_index`, and publishes `CurrentItemChanged`.
    ///
    /// # Errors
    /// - [`PlayError::NotReady`] if no slot is armed.
    /// - [`PlayError::Internal`] if `index` does not match
    ///   [`Self::armed_next`].
    pub fn commit_next(&self, index: usize) -> Result<(), PlayError> {
        // `None` ⇒ the slot was already activated (idempotent no-op).
        let Some(activated) = self.activate_pending(index)? else {
            return Ok(());
        };

        self.start_playback(activated.src);
        self.publish_current_track_snapshot(activated.duration_seconds);
        let current_index = self.core.current_index.load(Ordering::Relaxed);
        if index != current_index {
            self.core.current_index.store(index, Ordering::Relaxed);
            self.announce_current_item(index);
        }
        Ok(())
    }

    pub(crate) fn dispatch_notification(&self, notification: PlayerNotification) {
        match notification.clone() {
            PlayerNotification::Requested => {
                self.handle_track_requested();
            }
            PlayerNotification::HandoverRequested => {
                self.handle_handover_requested();
            }
            PlayerNotification::PlaybackStopped {
                reason: TrackPlaybackStopReason::Eof,
                ..
            } => {
                self.handle_track_playback_stopped(notification);
            }
            _ => {
                if let Some(event) = player_event_from_notification(notification.clone()) {
                    self.core.bus.publish(event);
                } else {
                    tracing::trace!(
                        src = ?notification.src(),
                        ?notification,
                        "unhandled player notification"
                    );
                }
            }
        }
    }

    /// Drain audio-thread notifications for every active slot.
    pub fn drain_notifications(&self) -> Vec<String> {
        let mut out = Vec::new();
        for slot_id in self.core.engine.active_slots() {
            let Some(state) = self.core.engine.slot_shared_state(slot_id) else {
                continue;
            };
            while let Some(notification) = state.notification_rx.lock().try_pop() {
                out.push(format!("{notification:?}"));
            }
            state.drain_trash();
        }
        out
    }

    pub(crate) fn enqueue_to_processor(&self, index: usize) -> Option<(Arc<str>, f64)> {
        let mut items = self.core.items.lock();
        if index >= items.len() {
            return None;
        }

        let queued = items[index].take()?;
        let (item_id, resource) = (queued.item_id, queued.resource);

        let duration_seconds = resource
            .duration()
            .map_or(0.0, |duration| duration.as_secs_f64());
        let abr_handle = resource.abr_handle();
        self.phase.lock().set_abr_handle(abr_handle);

        let current_rate = self.core.config.timestretch.speed();
        resource.set_playback_rate(current_rate);

        let host_sr = self.core.engine.master_sample_rate();
        if let Some(sr) = std::num::NonZeroU32::new(host_sr) {
            resource.set_host_sample_rate(sr);
        }

        let src = Arc::clone(resource.src());
        let player_resource = PlayerResource::new(resource, Arc::clone(&src), &self.core.pcm_pool);
        let arc_resource = Arc::new(Mutex::new(player_resource));
        drop(items);

        let _ = self.send_to_slot(PlayerCmd::LoadTrack {
            item_id,
            resource: arc_resource,
            src: Arc::clone(&src),
        });

        Some((src, duration_seconds))
    }

    /// Promote the armed slot to current after an audio-thread handover.
    ///
    /// Called on `TrackPlaybackStopped { Eof }` for the outgoing track.
    /// Two paths:
    /// - `activated == true`: `commit_next` already moved the bookkeeping at
    ///   the handover threshold (cf>0). Just clear.
    /// - `activated == false`: the audio thread performed the in-block arena
    ///   handover (cf=0). Sync `current_index` and emit `CurrentItemChanged`;
    ///   do **not** dispatch a fresh `FadeIn` — the arena promotion already
    ///   advanced the track to `Playing`.
    fn finalize_handover_if_armed(&self) {
        let pending = self.phase.lock().pending_mut().and_then(Option::take);
        let Some(pending) = pending else {
            return;
        };

        if pending.state.activated() {
            return;
        }

        if pending.index >= self.item_count() {
            return;
        }
        let index = pending.index;
        self.publish_current_track_snapshot(pending.duration_seconds);
        self.core.current_index.store(index, Ordering::Relaxed);
        self.announce_current_item(index);
    }

    fn handle_handover_requested(&self) {
        if self.crossfade_duration() <= 0.0 {
            return;
        }
        self.core.bus.publish(PlayerEvent::HandoverRequested);
        if self.auto_advance_enabled()
            && let Some(idx) = self.armed_next()
        {
            let _ = self.commit_next(idx);
        }
    }

    fn handle_track_playback_stopped(&self, notification: PlayerNotification) {
        if let Some(event) = player_event_from_notification(notification) {
            self.core.bus.publish(event);
        }

        self.finalize_handover_if_armed();
    }

    fn handle_track_requested(&self) {
        self.core.bus.publish(PlayerEvent::PrefetchRequested);
        if self.auto_advance_enabled() {
            let next_index = self.core.current_index.load(Ordering::Relaxed) + 1;
            if next_index < self.item_count() {
                let _ = self.arm_next(next_index);
            }
        }
    }

    /// Process audio-thread notifications, emitting `ItemDidPlayToEnd`
    /// only when a track finishes via natural EOF.
    pub fn process_notifications(&self) {
        let mut notifications = Vec::new();
        for slot_id in self.core.engine.active_slots() {
            let Some(state) = self.core.engine.slot_shared_state(slot_id) else {
                tracing::warn!(?slot_id, "process_notifications: slot has no shared state");
                continue;
            };

            while let Some(notification) = state.notification_rx.lock().try_pop() {
                notifications.push(notification);
            }
            state.drain_trash();
        }

        if !notifications.is_empty() {
            tracing::debug!(
                count = notifications.len(),
                "process_notifications draining"
            );
        }
        for notification in notifications {
            tracing::debug!(?notification, "process_notifications: handle");
            self.dispatch_notification(notification);
        }
    }

    pub(crate) fn publish_current_track_snapshot(&self, duration_seconds: f64) {
        let Some(slot_id) = self.slot() else {
            return;
        };
        let Some(shared_state) = self.core.engine.slot_shared_state(slot_id) else {
            return;
        };
        shared_state.position.store(0.0, Ordering::Relaxed);
        shared_state
            .duration
            .store(duration_seconds.max(0.0), Ordering::Relaxed);
    }

    /// Remove all items from the queue.
    pub fn remove_all_items(&self) {
        self.unarm_next();
        self.core.items.lock().clear();
        self.core.current_index.store(0, Ordering::Relaxed);
        // Whatever is inserted next is a new identity, so re-open announce.
        self.core
            .last_announced_index
            .store(usize::MAX, Ordering::Relaxed);
        self.set_status(crate::types::PlayerStatus::Unknown);
        // Drop the actively playing track from the audio-thread arena and
        // reset the position/duration snapshot; otherwise the stale snapshot
        // outlives the cleared queue. No-op when no slot is allocated.
        let _ = self.send_to_slot(PlayerCmd::Clear);
        self.enter_stopped();
        debug!("all items removed");
    }

    /// Drop the armed next slot without committing.
    ///
    /// Sends `UnloadTrack` to the audio thread for the armed src and
    /// clears the pending slot. Skips the unload if the armed slot has
    /// already been activated for the current index (the activated track
    /// is now the leading one — unloading would silence playback).
    pub fn unarm_next(&self) {
        self.unarm_next_internal(Some(self.core.current_index.load(Ordering::Relaxed)));
    }

    pub(crate) fn unarm_next_internal(&self, current_index_hint: Option<usize>) {
        let pending = self.phase.lock().pending_mut().and_then(Option::take);
        let Some(pending) = pending else {
            return;
        };
        let preserve_active_current = current_index_hint
            .is_some_and(|index| pending.state.activated() && pending.index == index);
        if !preserve_active_current {
            let _ = self.send_to_slot(PlayerCmd::UnloadTrack { src: pending.src });
        }
    }
}

pub(crate) fn player_event_from_notification(
    notification: PlayerNotification,
) -> Option<PlayerEvent> {
    match notification {
        PlayerNotification::PlaybackStopped {
            reason: TrackPlaybackStopReason::Eof,
            src,
            item_id,
        } => Some(PlayerEvent::ItemDidPlayToEnd { src, item_id }),
        PlayerNotification::PlaybackStopped {
            reason: TrackPlaybackStopReason::Failed,
            src,
            item_id,
        } => Some(PlayerEvent::ItemDidFail { src, item_id }),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use kithara_events::Event;
    use kithara_platform::sync::Arc;
    use kithara_test_utils::kithara;

    use super::*;
    use crate::impls::{
        player::PlayerConfig,
        player_notification::{PlayerNotification, TrackPlaybackStopReason},
        session::testing,
    };

    #[kithara::test]
    fn eof_playback_stopped_notification_maps_to_item_end_event() {
        let event = player_event_from_notification(PlayerNotification::PlaybackStopped {
            src: Arc::from("track.mp3"),
            item_id: Some(Arc::from("item-1")),
            reason: TrackPlaybackStopReason::Eof,
        });
        assert!(matches!(event, Some(PlayerEvent::ItemDidPlayToEnd { .. })));
    }

    #[kithara::test]
    fn playback_stopped_notification_does_not_map_to_item_end_event() {
        let event = player_event_from_notification(PlayerNotification::PlaybackStopped {
            src: Arc::from("track.mp3"),
            item_id: Some(Arc::from("item-1")),
            reason: TrackPlaybackStopReason::Stop,
        });
        assert!(event.is_none());
    }

    #[kithara::test]
    fn commit_next_without_arm_returns_not_ready() {
        let player = PlayerImpl::new(PlayerConfig::default());
        let err = player.commit_next(1).expect_err("must error");
        assert!(matches!(err, PlayError::NotReady));
    }

    #[kithara::test]
    fn commit_next_publishes_snapshot_before_current_item_changed() {
        let player = PlayerImpl::new(PlayerConfig {
            session: Some(testing::test_session()),
            ..PlayerConfig::default()
        });
        player
            .ensure_engine_started()
            .expect("engine start must succeed");
        player.ensure_slot().expect("slot allocation must succeed");
        let mut rx = player.subscribe();

        if let Some(pending_slot) = player.phase.lock().pending_mut() {
            *pending_slot = Some(PendingNext {
                src: Arc::from("next.mp3"),
                state: PendingNextState::Armed,
                index: 1,
                duration_seconds: 162.0,
            });
        }

        player.commit_next(1).expect("commit_next must succeed");

        assert!(matches!(
            rx.try_recv(),
            Ok(Event::Player(PlayerEvent::CurrentItemChanged))
        ));
        assert_eq!(player.duration_seconds(), Some(162.0));
    }
}
