use std::sync::{Arc, atomic::Ordering};

use kithara_platform::sync::Mutex;
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
};

/// Outcome of resolving an arm request under the short phase lock, acted on
/// outside the lock to avoid holding it across `send_to_slot`.
enum ArmDecision {
    /// The same index is already armed; return its src verbatim.
    AlreadyArmed(Arc<str>),
    /// The slot was cleared; optionally unload the previous src.
    Clear(Option<Arc<str>>),
}

impl PlayerImpl {
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

        let src = self.enqueue_to_processor(index)?;
        if let Some(pending_slot) = self.phase.lock().pending_mut() {
            *pending_slot = Some(PendingNext {
                index,
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
        let Some(src) = self.activate_pending(index)? else {
            return Ok(());
        };

        self.start_playback(src);
        let current_index = self.core.current_index.load(Ordering::Relaxed);
        if index != current_index {
            self.core.current_index.store(index, Ordering::Relaxed);
            self.core.bus.publish(PlayerEvent::CurrentItemChanged);
        }
        Ok(())
    }

    /// Mark the armed-next slot at `index` activated under a short lock,
    /// returning its src. `Ok(None)` when it was already activated.
    fn activate_pending(&self, index: usize) -> Result<Option<Arc<str>>, PlayError> {
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
            pending.state = PendingNextState::ActivatedReady;
            Some(src)
        };
        drop(phase);
        Ok(outcome)
    }

    pub(crate) fn enqueue_to_processor(&self, index: usize) -> Option<Arc<str>> {
        let mut items = self.core.items.lock();
        if index >= items.len() {
            return None;
        }

        let queued = items[index].take()?;
        let (item_id, resource) = (queued.item_id, queued.resource);

        let abr_handle = resource.abr_handle();
        self.phase.lock().set_abr_handle(abr_handle);

        let current_rate = self.core.playback_rate_shared.load(Ordering::Relaxed);
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

        Some(src)
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

    /// Drain audio-thread notifications for the active slot.
    pub fn drain_notifications(&self) -> Vec<String> {
        let Some(slot_id) = self.slot() else {
            return Vec::new();
        };
        let Some(state) = self.core.engine.slot_shared_state(slot_id) else {
            return Vec::new();
        };

        let mut out = Vec::new();
        while let Some(notification) = state.notification_rx.lock().try_pop() {
            out.push(format!("{notification:?}"));
        }
        state.drain_trash();
        out
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

        let current_index = self.core.current_index.load(Ordering::Relaxed);
        if pending.index == current_index || pending.index >= self.item_count() {
            return;
        }
        self.core
            .current_index
            .store(pending.index, Ordering::Relaxed);
        self.core.bus.publish(PlayerEvent::CurrentItemChanged);
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
        let Some(slot_id) = self.slot() else {
            tracing::trace!("process_notifications: no current slot");
            return;
        };
        let Some(state) = self.core.engine.slot_shared_state(slot_id) else {
            tracing::warn!(?slot_id, "process_notifications: slot has no shared state");
            return;
        };

        let mut notifications = Vec::new();
        while let Some(notification) = state.notification_rx.lock().try_pop() {
            notifications.push(notification);
        }
        state.drain_trash();

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

    /// Remove all items from the queue.
    pub fn remove_all_items(&self) {
        self.unarm_next();
        self.core.items.lock().clear();
        self.core.current_index.store(0, Ordering::Relaxed);
        self.set_status(crate::types::PlayerStatus::Unknown);
        // Drop the actively playing track from the audio-thread arena and
        // reset the position/duration snapshot; otherwise the stale snapshot
        // outlives the cleared queue. No-op when no slot is allocated.
        let _ = self.send_to_slot(PlayerCmd::Clear);
        self.enter_stopped();
        debug!("all items removed");
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
    use std::sync::Arc;

    use kithara_test_utils::kithara;

    use super::*;
    use crate::impls::{
        player::PlayerConfig,
        player_notification::{PlayerNotification, TrackPlaybackStopReason},
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
}
