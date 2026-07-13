use std::{ops::Deref, sync::atomic::Ordering};

use super::super::core::PlayerImpl;
use crate::{
    api::PlayerEvent,
    bridge::{PlayerNotification, TrackPlaybackStopReason},
};

struct Notifier<'a> {
    player: &'a PlayerImpl,
}

impl<'a> Notifier<'a> {
    fn new(player: &'a PlayerImpl) -> Self {
        Self { player }
    }
}

impl Deref for Notifier<'_> {
    type Target = PlayerImpl;

    fn deref(&self) -> &Self::Target {
        self.player
    }
}

impl Notifier<'_> {
    fn dispatch_notification(&self, notification: PlayerNotification) {
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
                    self.core.engine.bus().publish(event);
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
        self.core.items.set_current(index);
        self.announce_current_item(index);
    }

    fn handle_handover_requested(&self) {
        if self.crossfade_duration() <= 0.0 {
            return;
        }
        self.core
            .engine
            .bus()
            .publish(PlayerEvent::HandoverRequested);
        if self.auto_advance_enabled()
            && let Some(idx) = self.armed_next()
        {
            let _ = self.commit_next(idx);
        }
    }

    fn handle_track_playback_stopped(&self, notification: PlayerNotification) {
        if let Some(event) = player_event_from_notification(notification) {
            self.core.engine.bus().publish(event);
        }

        self.finalize_handover_if_armed();
    }

    fn handle_track_requested(&self) {
        self.core
            .engine
            .bus()
            .publish(PlayerEvent::PrefetchRequested);
        if self.auto_advance_enabled() {
            let next_index = self.current_index() + 1;
            if next_index < self.item_count() {
                let _ = self.arm_next(next_index);
            }
        }
    }

    /// Process audio-thread notifications, emitting `ItemDidPlayToEnd`
    /// only when a track finishes via natural EOF.
    fn process_notifications(&self) {
        for slot_id in self.core.engine.active_slots() {
            let mut saw_slot = false;
            while let Some(notification) = self.core.engine.pop_slot_notification(slot_id) {
                saw_slot = true;
                tracing::debug!(?notification, "process_notifications: handle");
                self.dispatch_notification(notification);
            }
            if !self.core.engine.drain_slot_trash(slot_id) && !saw_slot {
                tracing::warn!(?slot_id, "process_notifications: slot has no control state");
            }
        }
    }

    fn publish_current_track_snapshot(&self, duration_seconds: f64) {
        let Some(slot_id) = self.slot() else {
            return;
        };
        let Some(playback) = self.core.engine.slot_playback(slot_id) else {
            return;
        };
        playback.position.store(0.0, Ordering::Relaxed);
        playback
            .duration
            .store(duration_seconds.max(0.0), Ordering::Relaxed);
    }
}

impl PlayerImpl {
    pub fn process_notifications(&self) {
        Notifier::new(self).process_notifications();
    }

    pub(crate) fn publish_current_track_snapshot(&self, duration_seconds: f64) {
        Notifier::new(self).publish_current_track_snapshot(duration_seconds);
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
    use kithara_platform::sync::Arc;
    use kithara_test_utils::kithara;

    use super::*;

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
}
