use std::sync::PoisonError;

use kithara_audio::AudioEvent;
use kithara_bufpool::HasPool;
use kithara_events::{Envelope, EventSet, TrackId};
use kithara_platform::tokio::sync::broadcast::error::TryRecvError;
use kithara_play::{ItemRole, PlayerEvent};
use tracing::debug;

use super::{
    QueueControl,
    types::{CachedPosition, CrossfadeArm, Transition},
};
use crate::{
    attempts::LoadClass,
    event::{AdvanceReason, ItemEvent, QueueEvent, TrackStatus},
};

impl<S> QueueControl<S>
where
    S: HasPool<u8> + HasPool<f32> + Send + Sync + 'static,
{
    pub(super) fn advance_loaded_successor(&self, current_id: TrackId, transition: Transition) {
        let Some(next) = self.next_selectable_entry() else {
            return;
        };
        if !matches!(next.status, TrackStatus::Loaded) {
            return;
        }

        let before_index = self.player.current_index();
        if self
            .select_with_reason(next.id, transition, AdvanceReason::CrossfadePreArm)
            .is_err()
        {
            return;
        }
        if self.player.current_index() != before_index {
            self.write_armed_for(CrossfadeArm::armed(current_id));
        }
    }

    fn handle_prefetch_requested(&self) {
        let Some(next) = self.next_selectable_entry() else {
            return;
        };
        if !matches!(next.status, TrackStatus::Consumed) {
            return;
        }
        let Some(source) = self.tracks.source(next.id) else {
            return;
        };
        self.set_status(next.id, TrackStatus::Pending);
        self.spawn_apply_after_load(next.id, source, LoadClass::Prefetch);
    }

    /// If an advance was already armed from `tick()`, consume it and
    /// return `true` — the engine's trailing `ItemDidPlayToEnd` for
    /// the same track must not advance again.
    pub(super) fn consume_armed_advance(&self, ended_id: TrackId, pos: f64, dur: f64) -> bool {
        if self.take_armed_for_if_matches(ended_id) {
            debug!(
                track_id = ended_id.as_u64(),
                pos, dur, "consumed ItemDidPlayToEnd (armed pre-end)"
            );
            true
        } else {
            false
        }
    }

    pub(super) fn drain_player_events(&self) {
        let mut lagged = false;
        {
            let mut rx = self
                .player_rx
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            loop {
                match rx.try_recv() {
                    Ok(Envelope { event: ev, .. }) => self.process_player_event(&ev),
                    Err(TryRecvError::Empty | TryRecvError::Closed) => break,
                    Err(TryRecvError::Lagged(_)) => lagged = true,
                }
            }
        }
        if lagged {
            // WHY: `CurrentItemChanged` is edge-triggered and de-duplicated by `ItemQueue::announce_current_item`, so waiting cannot recover a
            // drop.
            self.handle_current_item_changed();
        }
    }

    pub(super) fn handle_current_item_changed(&self) {
        let idx = self.player.current_index();
        let id = self.lock_tracks().get(idx).map(|e| e.id);
        self.write_cached_position(CachedPosition::Unknown);
        self.bus.publish(QueueEvent::CurrentTrackChanged { id });
    }

    /// Gated on `item` for the same reason as
    /// [`Self::handle_item_did_play_to_end`]: the request names the track
    /// that is running out, not the one the queue is on. Committing an
    /// advance moves the queue's cursor at once while the outgoing track
    /// keeps rendering and keeps its own triggers armed, so its handover
    /// can still arrive after the queue has left it — and applied to the
    /// successor it reads as "this track is about to end" before a single
    /// block of the successor has been heard.
    pub(super) fn handle_handover_requested(&self, item: &ItemRole) {
        if self.is_paused() {
            return;
        }
        let Some(entry) = self.current() else {
            return;
        };
        if entry.id != item.track().id {
            return;
        }
        self.advance_loaded_successor(entry.id, Transition::Crossfade);
    }

    /// Gated on `item` for the same reason as
    /// [`Self::handle_item_did_play_to_end`]: the player reports the item
    /// that aborted, not the one being heard. Only a leading item's
    /// failure may skip and flag, and it flags the entry the event names —
    /// never one merely sharing its source, which a playlist repeating a
    /// track would take out of selection for the rest of the session.
    pub(super) fn handle_item_did_fail(&self, item: &ItemRole) {
        let track = item.track();
        let snap = self.player.playback_snapshot();
        let pos = snap.map_or(0.0, |s| s.position());
        let dur = snap.map_or(0.0, |s| s.duration());
        debug!(%track, pos, dur, "ItemDidFail received — track aborted mid-stream");
        if self.current().is_none() {
            self.bus.publish(QueueEvent::QueueEnded);
            return;
        }
        if self.is_paused() {
            debug!(%track, "paused: not auto-advancing on ItemDidFail");
            return;
        }
        if self.consume_armed_advance(track.id, pos, dur) {
            return;
        }
        if !item.is_leading() {
            debug!(%track, pos, dur, ?item, "not the leading item: not failing the queue entry");
            return;
        }
        self.set_status(
            track.id,
            TrackStatus::Failed("mid-stream engine failure".to_string()),
        );
        self.bus.publish(QueueEvent::TrackLoadFailed {
            id: track.id,
            reason: "mid-stream engine failure".to_string(),
            auto_skipped: true,
        });
        let _ = self.advance_to_next_inner(Transition::None, AdvanceReason::TrackFailed);
    }

    /// `item` is the player's verdict on which item in its arena ended.
    /// The player drains every active slot, and one slot holds more than
    /// one item, so an end says nothing on its own: an orphaned slot or
    /// the outgoing half of a crossfade reports its own end while the
    /// item being heard has minutes left. Only `Leading` advances.
    pub(super) fn handle_item_did_play_to_end(&self, item: &ItemRole) {
        let track = item.track();
        let snap = self.player.playback_snapshot();
        let pos = snap.map_or(0.0, |s| s.position());
        let dur = snap.map_or(0.0, |s| s.duration());
        debug!(%track, pos, dur, "ItemDidPlayToEnd received");
        if self.current().is_none() {
            self.bus.publish(QueueEvent::QueueEnded);
            return;
        }
        if self.is_paused() {
            debug!(%track, pos, dur, "paused: not auto-advancing on ItemDidPlayToEnd");
            return;
        }
        if self.consume_armed_advance(track.id, pos, dur) {
            return;
        }
        if !item.is_leading() {
            debug!(%track, pos, dur, ?item, "not the leading item: not advancing");
            return;
        }
        let _ = self.advance_to_next_inner(Transition::Crossfade, AdvanceReason::NaturalEof);
    }

    pub(super) fn process_player_event(&self, ev: &PlayerBusEvent) {
        match ev {
            PlayerBusEvent::Player(PlayerEvent::ItemDidPlayToEnd { item }) => {
                self.handle_item_did_play_to_end(item);
            }
            PlayerBusEvent::Player(PlayerEvent::ItemDidFail { item }) => {
                self.handle_item_did_fail(item);
            }
            PlayerBusEvent::Player(PlayerEvent::CurrentItemChanged { .. }) => {
                self.handle_current_item_changed();
            }
            PlayerBusEvent::Player(PlayerEvent::PrefetchRequested) => {
                self.handle_prefetch_requested();
            }
            PlayerBusEvent::Player(PlayerEvent::HandoverRequested { item }) => {
                self.handle_handover_requested(item);
            }
            PlayerBusEvent::Audio(AudioEvent::UnderrunStarted { .. }) => {
                self.bus.publish(ItemEvent::PlaybackStalled);
            }
            PlayerBusEvent::Audio(AudioEvent::UnderrunEnded { .. }) => {
                self.bus.publish(ItemEvent::PlaybackLikelyToKeepUp);
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use kithara_events::{DEFAULT_EVENT_BUS_CAPACITY, SlotId, TrackId};
    use kithara_platform::sync::Arc;
    use kithara_play::{ItemRole, PlayerEvent, TrackRef};
    use kithara_test_utils::kithara;

    use crate::{
        QueueControl,
        event::{QueueEvent, TrackStatus},
        queue::state::tests::{make_queue, wait_for_queue_event},
        test_pools::TestPools,
    };

    fn selected_second(queue: &QueueControl<TestPools>) -> (TrackId, TrackId) {
        let first = queue
            .append("https://example.com/repeated.mp3")
            .expect("open queue accepts first repeated source");
        let second = queue
            .append("https://example.com/repeated.mp3")
            .expect("open queue accepts second repeated source");
        queue.lock_navigation_mut().select(1);
        queue.player.set_rate(1.0);
        (first, second)
    }

    #[kithara::test(tokio)]
    async fn leading_failure_marks_the_played_entry_when_sources_repeat() {
        let queue = make_queue();
        let (first, second) = selected_second(&queue);

        queue.handle_item_did_fail(&ItemRole::Leading(TrackRef::new(
            second,
            SlotId::new(0),
            Arc::from("https://example.com/repeated.mp3"),
        )));

        assert!(
            !matches!(
                queue.track(first).map(|entry| entry.status),
                Some(TrackStatus::Failed(_))
            ),
            "an event for the second repeated source must not fail the first entry"
        );
        assert!(
            matches!(
                queue.track(second).map(|entry| entry.status),
                Some(TrackStatus::Failed(_))
            ),
            "the entry named by the player event must be failed"
        );
    }

    #[kithara::test(tokio)]
    async fn background_end_and_failure_leave_the_current_entry_untouched() {
        let queue = make_queue();
        let (background, current) = selected_second(&queue);
        let item = ItemRole::Background(TrackRef::new(
            background,
            SlotId::new(1),
            Arc::from("https://example.com/repeated.mp3"),
        ));

        queue.handle_item_did_play_to_end(&item);
        queue.handle_item_did_fail(&item);

        assert_eq!(queue.current().map(|entry| entry.id), Some(current));
        assert!(
            !matches!(
                queue.track(background).map(|entry| entry.status),
                Some(TrackStatus::Failed(_))
            ),
            "a background failure must not fail its queue entry"
        );
    }

    #[kithara::test(tokio)]
    async fn lagged_player_events_resynchronize_current_track() {
        let queue = make_queue();
        let id = queue
            .append("https://example.com/lagged-events.mp3")
            .expect("open queue accepts a track");

        for _ in 0..=DEFAULT_EVENT_BUS_CAPACITY {
            queue
                .player
                .bus()
                .publish(PlayerEvent::RateChanged { rate: 1.0 });
        }

        let mut events = queue.subscribe();
        queue
            .tick()
            .expect("BUG: tick returned error in test setup");

        let saw_current_track = wait_for_queue_event(
            &mut events,
            |event| {
                matches!(
                    event,
                    QueueEvent::CurrentTrackChanged {
                        id: Some(current_id)
                    } if *current_id == id
                )
            },
            200,
        )
        .await;
        assert!(
            saw_current_track,
            "lag recovery should re-announce the current track"
        );
    }
}

#[derive(Clone, Debug, EventSet)]
#[non_exhaustive]
pub(crate) enum PlayerBusEvent {
    Player(PlayerEvent),
    Audio(AudioEvent),
}
