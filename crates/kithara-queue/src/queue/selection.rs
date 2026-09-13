use kithara_bufpool::HasPool;
use kithara_events::TrackId;
use kithara_play::SelectTransition;
use kithara_warp::AssetFrame;

#[cfg(test)]
use super::types::SelectPhase;
use super::{
    QueueControl,
    types::{PendingSelect, Transition},
};
use crate::{
    attempts::LoadClass,
    error::QueueError,
    event::{AdvanceReason, QueueEvent, TrackStatus},
};

mod apply;
mod navigation;
mod pending;

impl<S> QueueControl<S>
where
    S: HasPool<u8> + HasPool<f32> + Send + Sync + 'static,
{
    pub(in crate::queue) fn select_player_item(
        &self,
        index: usize,
        transition: SelectTransition,
    ) -> Result<(), QueueError> {
        let source_cue = matches!(self.cue_in, crate::CueIn::TrackStart)
            .then(|| AssetFrame::new(0.0).expect("asset start is valid"));
        self.player
            .select_item_with_crossfade_from_source_cue(index, transition, source_cue)
            .map_err(QueueError::from)
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
        self.with_open_result(|queue| {
            queue.select_with_reason(id, transition, AdvanceReason::UserSelect)
        })
    }

    pub(super) fn select_with_reason(
        &self,
        id: TrackId,
        transition: Transition,
        reason: AdvanceReason,
    ) -> Result<(), QueueError> {
        // WHY: Serialise the whole select against a concurrent `spawn_apply_after_load` completion (see `select_apply`): the supersede
        // (marking the prior pending `Cancelled`) and a loading track's apply must not interleave, or the superseded track barges in.
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
                let was_playing = self.player.is_playing();
                let crossfade = transition.crossfade_seconds(self.player.crossfade_duration());
                if was_playing && crossfade > 0.0 {
                    self.bus.publish(QueueEvent::CrossfadeStarted {
                        duration_seconds: crossfade,
                    });
                }
                self.select_player_item(
                    index,
                    SelectTransition {
                        autoplay: true,
                        crossfade_seconds: crossfade,
                    },
                )?;
                self.lock_navigation_mut().select(index);
                self.bus.publish(QueueEvent::CurrentTrackAdvance {
                    reason,
                    id: Some(id),
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
        }
    }
}

#[cfg(test)]
mod tests {
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
        let id = append(&queue, "https://example.com/a.mp3");
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
                .expect("BUG: open queue advance must be admitted")
                .is_none()
        );
        let saw_ended =
            wait_for_queue_event(&mut rx, |ev| matches!(ev, QueueEvent::QueueEnded), 200).await;
        assert!(saw_ended);
    }

    #[kithara::test(tokio)]
    async fn advance_to_next_cycles_then_emits_queue_ended() {
        let queue = make_queue();
        let _a = append(&queue, "https://example.com/a.mp3");
        let _b = append(&queue, "https://example.com/b.mp3");
        queue.lock_navigation_mut().select(1);
        let mut rx = queue.subscribe();

        assert!(
            queue
                .advance_to_next(Transition::Crossfade, AdvanceReason::NaturalEof)
                .expect("BUG: open queue advance must be admitted")
                .is_none()
        );

        let saw_ended =
            wait_for_queue_event(&mut rx, |ev| matches!(ev, QueueEvent::QueueEnded), 400).await;
        assert!(saw_ended, "QueueEnded should be broadcast at end-of-queue");
    }

    #[kithara::test(tokio)]
    async fn advance_to_pending_next_does_not_move_navigation_before_select_commit() {
        let queue = make_queue();
        let first = append(&queue, "https://example.com/a.mp3");
        let second = append(&queue, "https://example.com/b.mp3");
        queue.lock_navigation_mut().select(0);
        queue.set_status(first, TrackStatus::Consumed);
        queue.set_status(second, TrackStatus::Pending);

        assert_eq!(
            queue
                .advance_to_next(Transition::Crossfade, AdvanceReason::NaturalEof)
                .expect("BUG: open queue advance must be admitted"),
            Some(second)
        );
        assert_eq!(
            queue.lock_navigation().current_index(),
            Some(0),
            "navigation must stay on the audible item until pending select commits"
        );
    }
}
