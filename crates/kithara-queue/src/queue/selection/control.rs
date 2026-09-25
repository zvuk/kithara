use kithara_bufpool::HasPool;
use kithara_events::TrackId;
use kithara_play::{SelectTransition, SelectionPlayback};
use smallvec::SmallVec;

use super::super::{
    QueueControl,
    types::{CrossfadeArm, PendingSelect, Transition},
};
use crate::{
    attempts::LoadClass,
    error::QueueError,
    event::{AdvanceReason, QueueEvent, TrackStatus},
};

impl<S> QueueControl<S>
where
    S: HasPool<u8> + HasPool<f32> + Send + Sync + 'static,
{
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
            queue.select_with(
                id,
                transition,
                AdvanceReason::UserSelect,
                SelectionPlayback::Play,
            )
        })
    }

    pub(in crate::queue) fn select_loaded_item(
        &self,
        index: usize,
        id: TrackId,
        crossfade: kithara_play::CrossfadeSettings,
        reason: AdvanceReason,
        playback: SelectionPlayback,
    ) -> Result<(), QueueError> {
        let was_playing = self.player.is_playing();
        if was_playing && crossfade.duration > 0.0 {
            self.bus.publish(QueueEvent::CrossfadeStarted {
                settings: crossfade,
            });
        }
        self.player.select_item_with_crossfade(
            index,
            SelectTransition {
                playback,
                crossfade,
            },
        )?;
        let ids = self
            .tracks()
            .into_iter()
            .map(|track| track.id)
            .collect::<SmallVec<[_; 16]>>();
        self.lock_navigation_mut().select(id, &ids);
        self.bus.publish(QueueEvent::CurrentTrackAdvance {
            reason,
            id: Some(id),
        });
        self.set_status(id, TrackStatus::Consumed);
        Ok(())
    }

    /// Serializes the whole select against a concurrent `spawn_apply_after_load` completion so
    /// marking the prior pending attempt `Cancelled` and a loading track's apply never interleave,
    /// which would let the superseded track barge in.
    pub(in crate::queue) fn select_with(
        &self,
        id: TrackId,
        transition: Transition,
        reason: AdvanceReason,
        playback: SelectionPlayback,
    ) -> Result<(), QueueError> {
        if matches!(
            reason,
            AdvanceReason::UserSelect
                | AdvanceReason::UserNext
                | AdvanceReason::UserPrev
                | AdvanceReason::RemovedCurrent
        ) {
            self.autoplay_target.store(CrossfadeArm::Disarmed);
        }
        let default = self.config.crossfade_settings();
        let settings = transition.settings(default).validate()?;
        let _apply = self.lock_select_apply();
        self.select_with_reason_locked(id, settings, reason, playback)
    }

    pub(in crate::queue) fn select_with_reason(
        &self,
        id: TrackId,
        transition: Transition,
        reason: AdvanceReason,
    ) -> Result<(), QueueError> {
        let playback = if matches!(
            reason,
            AdvanceReason::NaturalEof
                | AdvanceReason::TrackFailed
                | AdvanceReason::CrossfadePreArm
                | AdvanceReason::Repeat
        ) || self.player.is_playing()
        {
            SelectionPlayback::Play
        } else {
            SelectionPlayback::Pause
        };
        self.select_with(id, transition, reason, playback)
    }

    /// `is_playing` is a session flag, not a verdict on the current item: the render thread queues
    /// the natural end but clears the flag only at the next `process`, so a repeat-one advance must
    /// re-select the item that just ended despite the flag.
    pub(in crate::queue) fn select_with_reason_locked(
        &self,
        id: TrackId,
        settings: kithara_play::CrossfadeSettings,
        reason: AdvanceReason,
        playback: SelectionPlayback,
    ) -> Result<(), QueueError> {
        let (index, status) = {
            let guard = self.lock_tracks();
            guard
                .iter()
                .enumerate()
                .find(|(_, e)| e.id == id)
                .map(|(i, e)| (i, e.status.clone()))
                .ok_or(QueueError::UnknownTrackId(id))?
        };

        if self.player.current_index() == index
            && matches!(status, TrackStatus::Consumed)
            && matches!(
                reason,
                AdvanceReason::UserSelect
                    | AdvanceReason::UserNext
                    | AdvanceReason::UserPrev
                    | AdvanceReason::RemovedCurrent
                    | AdvanceReason::NaturalEof
            )
        {
            self.cancel_stale_pending(id);
            if self.player.is_playing() && reason != AdvanceReason::NaturalEof {
                return Ok(());
            }
            let finished = self.player.playback_snapshot().is_some_and(|snapshot| {
                snapshot.duration() > 0.0 && snapshot.position() >= snapshot.duration()
            });
            if reason == AdvanceReason::NaturalEof || finished {
                self.player.seek_seconds(0.0)?;
            }
            if playback == SelectionPlayback::Play {
                self.player.play();
            } else {
                self.player.pause();
            }
            let ids = self
                .tracks()
                .into_iter()
                .map(|track| track.id)
                .collect::<SmallVec<[_; 16]>>();
            self.lock_navigation_mut().select(id, &ids);
            self.bus.publish(QueueEvent::CurrentTrackAdvance {
                reason,
                id: Some(id),
            });
            return Ok(());
        }

        match status {
            TrackStatus::Loaded => {
                self.cancel_stale_pending(id);
                self.select_loaded_item(index, id, settings, reason, playback)?;
                Ok(())
            }
            TrackStatus::Pending | TrackStatus::Loading | TrackStatus::Slow => {
                self.override_pending_select(PendingSelect {
                    reason,
                    settings,
                    playback,
                    id,
                });
                self.promote_pending_load(id);
                Ok(())
            }
            TrackStatus::Cancelled | TrackStatus::Consumed | TrackStatus::Failed(_) => {
                let source = self.tracks.source(id).ok_or(QueueError::NotReady(id))?;
                self.override_pending_select(PendingSelect {
                    reason,
                    settings,
                    playback,
                    id,
                });
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

    use super::{super::super::types::SelectPhase, *};
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
                assert_eq!(pending.settings.duration, 0.0);
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
                .advance_to_next_inner(Transition::Crossfade, AdvanceReason::NaturalEof)
                .expect("BUG: open queue advance must be admitted")
                .is_none()
        );
        let saw_ended =
            wait_for_queue_event(&mut rx, |ev| matches!(ev, QueueEvent::QueueEnded), 200).await;
        assert!(saw_ended);
    }

    #[kithara::test(tokio)]
    async fn manual_next_at_exhaustion_does_not_emit_queue_ended() {
        let queue = make_queue();
        let mut rx = queue.subscribe();
        assert_eq!(queue.next(Transition::None).expect("manual next"), None);
        assert!(
            !wait_for_queue_event(&mut rx, |ev| matches!(ev, QueueEvent::QueueEnded), 50).await
        );
    }

    #[kithara::test(tokio)]
    async fn advance_to_next_cycles_then_emits_queue_ended() {
        let queue = make_queue();
        let a = append(&queue, "https://example.com/a.mp3");
        let b = append(&queue, "https://example.com/b.mp3");
        queue.lock_navigation_mut().select(b, &[a, b]);
        let mut rx = queue.subscribe();

        assert!(
            queue
                .advance_to_next_inner(Transition::Crossfade, AdvanceReason::NaturalEof)
                .expect("BUG: open queue advance must be admitted")
                .is_none()
        );

        let saw_ended =
            wait_for_queue_event(&mut rx, |ev| matches!(ev, QueueEvent::QueueEnded), 400).await;
        assert!(saw_ended, "QueueEnded should be broadcast at end-of-queue");
    }

    #[kithara::test(tokio)]
    async fn admitted_pending_successor_becomes_navigation_authority() {
        let queue = make_queue();
        let first = append(&queue, "https://example.com/a.mp3");
        let second = append(&queue, "https://example.com/b.mp3");
        queue.lock_navigation_mut().select(first, &[first, second]);
        queue.set_status(first, TrackStatus::Consumed);
        queue.set_status(second, TrackStatus::Pending);

        assert_eq!(
            queue
                .advance_to_next_inner(Transition::Crossfade, AdvanceReason::NaturalEof)
                .expect("BUG: open queue advance must be admitted"),
            Some(second)
        );
        assert_eq!(
            queue.lock_navigation().current(),
            Some(second),
            "admitted automatic successor must remain authoritative while loading"
        );
        let SelectPhase::Pending(pending) = *queue.lock_pending_select_mut() else {
            panic!("successor selection must remain pending")
        };
        assert_eq!(pending.playback, SelectionPlayback::Play);
        assert_eq!(pending.reason, AdvanceReason::NaturalEof);
    }

    #[kithara::test(tokio)]
    async fn pending_override_latches_profile_without_mutating_default() {
        let queue = make_queue();
        let id = append(&queue, "https://example.com/a.mp3");
        let configured = kithara_play::CrossfadeSettings::new(
            2.0,
            kithara_play::CrossfadeCurve::EqualPower,
            1.0,
            0.5,
        )
        .expect("valid settings");
        let override_settings = kithara_play::CrossfadeSettings::new(
            4.0,
            kithara_play::CrossfadeCurve::Linear,
            0.25,
            0.3,
        )
        .expect("valid settings");
        queue
            .set_crossfade_settings(configured)
            .expect("valid settings");
        queue
            .select(
                id,
                Transition::CrossfadeWith {
                    settings: override_settings,
                },
            )
            .expect("pending selection admitted");
        queue
            .set_crossfade_settings(kithara_play::CrossfadeSettings::default())
            .expect("valid settings");

        let SelectPhase::Pending(pending) = *queue.lock_pending_select_mut() else {
            panic!("selection must remain pending")
        };
        assert_eq!(pending.settings, override_settings);
        assert_eq!(
            queue.crossfade_settings(),
            kithara_play::CrossfadeSettings::default()
        );
    }
}
