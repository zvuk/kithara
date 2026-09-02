use std::{ops::Deref, sync::atomic::Ordering};

use kithara_bufpool::HasPool;
use kithara_events::Event;
use kithara_platform::sync::Arc;

use super::super::core::PlayerRuntime;
use crate::{
    api::{EngineEvent, ItemRole, PlayerEvent, SlotId, TrackRef},
    bridge::{PlayerNotification, TrackPlaybackStopReason},
};

struct Notifier<'a, S> {
    player: &'a PlayerRuntime<S>,
}

impl<'a, S> Notifier<'a, S> {
    const fn new(player: &'a PlayerRuntime<S>) -> Self {
        Self { player }
    }
}

impl<S> Deref for Notifier<'_, S> {
    type Target = PlayerRuntime<S>;

    fn deref(&self) -> &Self::Target {
        self.player
    }
}

impl<S> Notifier<'_, S>
where
    S: HasPool<f32>,
{
    fn dispatch_notification(&self, slot_id: SlotId, notification: &PlayerNotification) {
        if self.natural_end_outranked_by_seek(slot_id, notification) {
            tracing::debug!(
                src = ?notification.src(),
                "dropping a natural end outranked by a published seek"
            );
            return;
        }
        let item = self.item_role(slot_id, notification);
        let emitted = item
            .as_ref()
            .map(|item| player_events_from_notification(self, notification, item))
            .unwrap_or_default();
        let emitted_any = !emitted.is_empty();
        for event in emitted {
            self.core.engine.bus().publish(event);
        }

        match notification {
            PlayerNotification::Requested => {
                self.handle_track_requested();
            }
            PlayerNotification::HandoverRequested => {
                self.handle_handover_requested();
            }
            PlayerNotification::RateChanged { rate } => {
                self.core
                    .engine
                    .bus()
                    .publish(PlayerEvent::RateChanged { rate: *rate });
            }
            PlayerNotification::PlaybackStopped {
                reason: TrackPlaybackStopReason::Eof,
                ..
            } => {
                self.handle_track_playback_stopped(item, notification);
            }
            _ => {
                if let Some(event) =
                    item.and_then(|item| player_event_from_notification(notification, item))
                {
                    self.core.engine.bus().publish(event);
                } else if !emitted_any {
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

    /// The role is read before `finalize_handover_if_armed` runs, so the
    /// phase still describes the arena as it was when the track stopped.
    fn handle_track_playback_stopped(
        &self,
        item: Option<ItemRole>,
        notification: &PlayerNotification,
    ) {
        if let Some(event) =
            item.and_then(|item| player_event_from_notification(notification, item))
        {
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

    /// Name the item a start or stop notification is about, together with
    /// its role in the arena. `None` for notifications that do not name an
    /// item at all.
    ///
    /// Slot identity answers only half of the role: a slot is a processor
    /// holding an arena, and `commit_next` promotes the successor *inside*
    /// the current slot, leaving it in the phase as the activated
    /// `PendingNext`. So an item in the held slot carrying an identity other
    /// than the promoted one is the item promoted over — it sits inside the
    /// leading slot, but it is not what the listener is hearing.
    fn item_role(&self, slot_id: SlotId, notification: &PlayerNotification) -> Option<ItemRole> {
        let (src, id) = match notification {
            PlayerNotification::PlaybackStarted { src, item_id }
            | PlayerNotification::PlaybackStopped { src, item_id, .. } => (src, *item_id),
            _ => return None,
        };
        let track = TrackRef::new(id, slot_id, Arc::clone(src));
        if self.slot() != Some(slot_id) {
            return Some(ItemRole::Background(track));
        }
        let promoted = self
            .phase
            .lock()
            .pending()
            .filter(|pending| pending.state.activated())
            .map(|pending| pending.item_id);
        Some(match promoted {
            Some(promoted) if promoted != track.id => ItemRole::Outgoing(track),
            _ => ItemRole::Leading(track),
        })
    }

    /// Bug #5's dispatch-side sibling: a natural end minted at an older
    /// epoch describes a position the user has already left — a newer
    /// published seek revives the track (`apply_seek`), and delivering the
    /// stale end would hand the queue an `ItemDidPlayToEnd` it answers with
    /// an auto-advance out from under the accepted seek. Compared with `!=`,
    /// not `<`: epochs wrap, and `withdraw_seek_epoch` legally steps the
    /// published value back. `Stop` and `Failed` are never fenced — a broken
    /// source stays broken across a seek, and a slot without playback state
    /// has nothing to outrank the end.
    fn natural_end_outranked_by_seek(
        &self,
        slot_id: SlotId,
        notification: &PlayerNotification,
    ) -> bool {
        let PlayerNotification::PlaybackStopped {
            reason: TrackPlaybackStopReason::Eof,
            seek_epoch,
            ..
        } = notification
        else {
            return false;
        };
        self.core
            .engine
            .slot_playback(slot_id)
            .is_some_and(|playback| playback.seek_epoch.load(Ordering::SeqCst) != *seek_epoch)
    }

    /// Process audio-thread notifications, emitting `ItemDidPlayToEnd`
    /// only when a track finishes via natural EOF.
    fn process_notifications(&self) {
        for slot_id in self.core.engine.active_slots() {
            let mut saw_slot = false;
            while let Some(notification) = self.core.engine.pop_slot_notification(slot_id) {
                saw_slot = true;
                tracing::debug!(?notification, "process_notifications: handle");
                self.dispatch_notification(slot_id, &notification);
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

impl<S> PlayerRuntime<S>
where
    S: HasPool<f32>,
{
    pub fn process_notifications(&self) {
        self.core.worker.flush_deferred();
        Notifier::new(self).process_notifications();
    }

    pub(crate) fn publish_current_track_snapshot(&self, duration_seconds: f64) {
        Notifier::new(self).publish_current_track_snapshot(duration_seconds);
    }
}

pub(crate) fn player_event_from_notification(
    notification: &PlayerNotification,
    item: ItemRole,
) -> Option<PlayerEvent> {
    match notification {
        PlayerNotification::PlaybackStopped {
            reason: TrackPlaybackStopReason::Eof,
            ..
        } => Some(PlayerEvent::ItemDidPlayToEnd { item }),
        PlayerNotification::PlaybackStopped {
            reason: TrackPlaybackStopReason::Failed,
            ..
        } => Some(PlayerEvent::ItemDidFail { item }),
        _ => None,
    }
}

fn player_events_from_notification<S>(
    player: &PlayerRuntime<S>,
    notification: &PlayerNotification,
    item: &ItemRole,
) -> Vec<Event> {
    let mut events: Vec<Event> = Vec::new();
    match notification {
        PlayerNotification::PlaybackStarted { .. } => {
            events.push(PlayerEvent::PlaybackStarted { item: item.clone() }.into());
        }
        PlayerNotification::PlaybackStopped {
            reason: TrackPlaybackStopReason::Stop,
            ..
        } => {
            let phase = player.phase.lock();
            if phase
                .pending()
                .is_some_and(|pending| pending.state.activated())
                && let Some(slot) = phase.slot()
            {
                events.push(
                    EngineEvent::CrossfadeCompleted {
                        from: slot,
                        to: slot,
                    }
                    .into(),
                );
            }
        }
        _ => {}
    }
    events
}

#[cfg(test)]
mod tests {
    use kithara_events::{Envelope, Event, EventReceiver, TrackId};
    use kithara_platform::sync::Arc;
    use kithara_test_utils::kithara;

    use super::*;
    use crate::{
        PlayWorker, PlayWorkerConfig,
        player::{
            PlayerConfig, PlayerImpl,
            state::{PendingNext, PendingNextState},
        },
        session::testing,
        test_pools::{TestPools, pools},
    };

    struct Items;

    impl Items {
        const BACKGROUND: TrackId = TrackId(9);
        const OUTGOING: TrackId = TrackId(7);
        const PROMOTED: TrackId = TrackId(8);
    }

    /// A started player holding one slot — the slot the phase calls current.
    fn player_with_slot() -> (PlayerImpl<TestPools>, SlotId) {
        let worker = PlayWorker::new(PlayWorkerConfig::builder(pools()).build());
        let player = PlayerImpl::new(
            PlayerConfig::builder()
                .worker(worker)
                .session(testing::test_session())
                .build(),
        );
        player
            .ensure_engine_started()
            .expect("engine start must succeed");
        let slot = player.ensure_slot().expect("slot allocation must succeed");
        (player, slot)
    }

    /// Put the slot mid-crossfade: the successor is loaded into this same
    /// slot's arena and already promoted over the outgoing track, exactly
    /// as `commit_next` leaves it.
    fn activate_pending(player: &PlayerImpl<TestPools>, item_id: TrackId, src: &str) {
        let mut phase = player.phase.lock();
        let Some(pending) = phase.pending_mut() else {
            panic!("BUG: an active phase must carry a pending slot");
        };
        *pending = Some(PendingNext {
            item_id,
            src: Arc::from(src),
            state: PendingNextState::ActivatedReady,
            index: 1,
            duration_seconds: 60.0,
        });
        drop(phase);
    }

    fn stop_notification(
        item_id: TrackId,
        src: &str,
        reason: TrackPlaybackStopReason,
    ) -> PlayerNotification {
        PlayerNotification::PlaybackStopped {
            item_id,
            reason,
            src: Arc::from(src),
            seek_epoch: 0,
        }
    }

    fn eof_notification() -> PlayerNotification {
        stop_notification(Items::OUTGOING, "leading.mp3", TrackPlaybackStopReason::Eof)
    }

    /// Each test dispatches exactly one notification, so the first role a
    /// start or an end carries is the one under test.
    fn published_role(rx: &mut EventReceiver) -> Option<ItemRole> {
        while let Ok(Envelope { event, .. }) = rx.try_recv() {
            if let Event::Player(
                PlayerEvent::PlaybackStarted { item } | PlayerEvent::ItemDidPlayToEnd { item },
            ) = event
            {
                return Some(item);
            }
        }
        None
    }

    fn track(item_id: TrackId, slot: SlotId, src: &str) -> TrackRef {
        TrackRef::new(item_id, slot, Arc::from(src))
    }

    #[kithara::test]
    fn eof_playback_stopped_notification_maps_to_item_end_event() {
        let event = player_event_from_notification(
            &eof_notification(),
            ItemRole::Leading(track(Items::OUTGOING, SlotId::new(0), "leading.mp3")),
        );
        assert!(matches!(
            event,
            Some(PlayerEvent::ItemDidPlayToEnd {
                item: ItemRole::Leading(_)
            })
        ));
    }

    #[kithara::test]
    fn failed_playback_stopped_notification_carries_the_role() {
        let event = player_event_from_notification(
            &stop_notification(
                Items::OUTGOING,
                "leading.mp3",
                TrackPlaybackStopReason::Failed,
            ),
            ItemRole::Background(track(Items::OUTGOING, SlotId::new(0), "leading.mp3")),
        );
        assert!(matches!(
            event,
            Some(PlayerEvent::ItemDidFail {
                item: ItemRole::Background(_)
            })
        ));
    }

    #[kithara::test]
    fn playback_stopped_notification_does_not_map_to_item_end_event() {
        let event = player_event_from_notification(
            &stop_notification(
                Items::OUTGOING,
                "leading.mp3",
                TrackPlaybackStopReason::Stop,
            ),
            ItemRole::Leading(track(Items::OUTGOING, SlotId::new(0), "leading.mp3")),
        );
        assert!(event.is_none());
    }

    #[kithara::test]
    fn end_of_the_held_slot_is_the_leading_track() {
        let (player, slot) = player_with_slot();
        let mut rx = player.subscribe();

        Notifier::new(&player).dispatch_notification(slot, &eof_notification());

        assert_eq!(
            published_role(&mut rx),
            Some(ItemRole::Leading(track(
                Items::OUTGOING,
                slot,
                "leading.mp3",
            ))),
            "with nothing promoted over it, the held slot's track is the one being heard"
        );
    }

    /// `process_notifications` drains every active slot. A slot the phase
    /// no longer holds is an orphan still emptying its notification ring
    /// while a different track plays; acting on its end cuts that track.
    #[kithara::test]
    fn end_of_a_slot_the_phase_does_not_hold_is_a_background_track() {
        let (player, slot) = player_with_slot();
        let background = SlotId::new(slot.value() + 1);
        let mut rx = player.subscribe();

        Notifier::new(&player).dispatch_notification(
            background,
            &stop_notification(
                Items::BACKGROUND,
                "leading.mp3",
                TrackPlaybackStopReason::Eof,
            ),
        );

        assert_eq!(
            published_role(&mut rx),
            Some(ItemRole::Background(track(
                Items::BACKGROUND,
                background,
                "leading.mp3",
            ))),
            "an end from a slot the phase does not hold is not the track being heard"
        );
    }

    /// A slot is a processor holding an arena, not one track: `commit_next`
    /// promotes the successor *inside* the current slot. So the outgoing
    /// half of a crossfade ends while its own slot is still the held one —
    /// slot identity alone calls it leading, and the queue would advance a
    /// second time off an end that was already accounted for.
    #[kithara::test]
    fn the_outgoing_half_of_a_crossfade_is_not_the_leading_track() {
        let (player, slot) = player_with_slot();
        activate_pending(&player, Items::PROMOTED, "same.mp3");
        let mut rx = player.subscribe();

        Notifier::new(&player).dispatch_notification(
            slot,
            &stop_notification(Items::OUTGOING, "same.mp3", TrackPlaybackStopReason::Eof),
        );

        assert_eq!(
            published_role(&mut rx),
            Some(ItemRole::Outgoing(
                track(Items::OUTGOING, slot, "same.mp3",)
            )),
            "the track promoted over is not the one the listener is hearing"
        );
    }

    /// The other half of the same moment: the promoted track is the one
    /// being heard, so its own end must still drive the advance.
    #[kithara::test]
    fn the_promoted_half_of_a_crossfade_is_the_leading_track() {
        let (player, slot) = player_with_slot();
        activate_pending(&player, Items::PROMOTED, "same.mp3");
        let mut rx = player.subscribe();

        Notifier::new(&player).dispatch_notification(
            slot,
            &stop_notification(Items::PROMOTED, "same.mp3", TrackPlaybackStopReason::Eof),
        );

        assert_eq!(
            published_role(&mut rx),
            Some(ItemRole::Leading(track(Items::PROMOTED, slot, "same.mp3",))),
            "the promoted track is the one being heard"
        );
    }

    /// A start is drained from every active slot exactly as a stop is, so
    /// it needs the same answer: an orphan whose item reaches `Playing`
    /// would otherwise announce itself as the item being heard.
    #[kithara::test]
    fn start_of_a_slot_the_phase_does_not_hold_is_a_background_item() {
        let (player, slot) = player_with_slot();
        let background = SlotId::new(slot.value() + 1);
        let mut rx = player.subscribe();

        Notifier::new(&player).dispatch_notification(
            background,
            &PlayerNotification::PlaybackStarted {
                src: Arc::from("background.mp3"),
                item_id: Items::BACKGROUND,
            },
        );

        assert_eq!(
            published_role(&mut rx),
            Some(ItemRole::Background(track(
                Items::BACKGROUND,
                background,
                "background.mp3"
            ))),
            "a start from a slot the phase does not hold is not the item being heard"
        );
    }
}
