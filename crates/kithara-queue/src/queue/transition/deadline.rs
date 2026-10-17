use std::num::NonZeroU32;

use kithara_bufpool::HasPool;
use kithara_events::TrackId;
use kithara_platform::time::Duration;
use kithara_play::{
    Bound, Outbox, OutputSnapshot, PlayError, Player, Track, TrackCommand, TrackFactory,
    TrackStatus as PlayingStatus,
};
use kithara_signal::{AudioSpec, FrameCount, SessionFrame};

use super::{
    super::{Queue, Transition, slots::Role, types::Target},
    TransitionRequest,
};
use crate::{
    ActionAtItemEnd, AdvanceReason, NavigationState, PlaybackOrder, QueueError, RepeatMode,
    TrackStatus,
};

impl<S, F> Queue<S, F>
where
    S: HasPool<u8> + HasPool<f32> + Send + Sync + 'static,
    F: TrackFactory<S>,
{
    pub(in crate::queue) fn tick_deadlines(
        &mut self,
        now: SessionFrame,
        output: Option<&OutputSnapshot>,
        out: &mut Outbox<'_, S>,
    ) -> Result<(), QueueError> {
        if self.shutdown.is_cancelled() {
            return Ok(());
        }
        self.release_tails(out)?;
        self.reap_released();
        self.loaded_tracks(out)?;
        if self.config.action_at_item_end != ActionAtItemEnd::Advance {
            return self.cancel_auto(out);
        }
        if let Some(target) = self.target {
            return self.refresh_deadline(target, now, out);
        }
        self.arm_next_deadline(now, output, out)
    }

    fn refresh_deadline(
        &mut self,
        target: Target,
        now: SessionFrame,
        out: &mut Outbox<'_, S>,
    ) -> Result<(), QueueError> {
        let Some(end) = self.current_end() else {
            return Ok(());
        };
        if target.auto && self.single_slot_repeat(target.to) {
            if target.repeat.is_none() {
                self.repeat_one(end, out)?;
            }
            return Ok(());
        }
        if target.auto && target.reason == AdvanceReason::NaturalEof && !target.chained {
            let new_bound = self.auto_bound(target.settings)?;
            if new_bound != target.bound {
                let old_bound_frame = target.bound.map(|bound| match bound {
                    Bound::AtOrBefore(frame) | Bound::AtOrAfter(frame) => i64::from(frame),
                });
                let new_bound_frame = new_bound.map(|bound| match bound {
                    Bound::AtOrBefore(frame) | Bound::AtOrAfter(frame) => i64::from(frame),
                });
                let pending_seq = self
                    .pending_handover
                    .filter(|pending| pending.item == target.to)
                    .map(|pending| pending.seq.get());
                let source_mark = self
                    .active_current_index()
                    .and_then(|index| self.active.get(index))
                    .and_then(|active| active.track.snapshot().as_ref().mark);
                kithara_test_utils::kithara::probe_event!(
                    automatic_handover_withdrawal,
                    old_bound_frame,
                    new_bound_frame,
                    chained = target.chained,
                    pending_seq,
                    source_segment = source_mark.map(|mark| mark.lane.segment.get())
                );
                tracing::debug!(
                    ?old_bound_frame,
                    ?new_bound_frame,
                    chained = target.chained,
                    ?pending_seq,
                    ?source_mark,
                    ?now,
                    "queue automatic handover deadline changed"
                );
                self.withdraw_auto(out)?;
            }
        }
        Ok(())
    }

    fn playing_last(&self, ids: &[TrackId], wrap: bool) -> bool {
        self.navigation.playback_order() == PlaybackOrder::Sequential
            && self.navigation.repeat_mode() != RepeatMode::One
            && NavigationState::next_sequential(ids, self.current, wrap).is_none()
            && self
                .active_current_index()
                .and_then(|index| self.active.get(index))
                .is_some_and(|active| {
                    matches!(
                        active.track.snapshot().as_ref().status,
                        PlayingStatus::Playing { .. }
                    )
                })
    }

    fn arm_next_deadline(
        &mut self,
        now: SessionFrame,
        output: Option<&OutputSnapshot>,
        out: &mut Outbox<'_, S>,
    ) -> Result<(), QueueError> {
        let ids = self.selectable_ids();
        let wrap = self.navigation.repeat_mode() == RepeatMode::All;
        if self.playing_last(&ids, wrap) {
            return Ok(());
        }
        let Some(end) = self.current_end() else {
            return Ok(());
        };
        let Some(id) = self.navigation.next(&ids, self.current, true, wrap) else {
            if self
                .active_current_index()
                .and_then(|index| self.active.get(index))
                .is_some_and(|active| {
                    matches!(
                        active.track.snapshot().as_ref().status,
                        PlayingStatus::Ended { .. }
                    )
                })
            {
                self.current = None;
                self.held_position = None;
                self.announce(crate::QueueEvent::QueueEnded);
            }
            return Ok(());
        };
        if now >= end - self.frames(self.config.preload_lead)? {
            if self.single_slot_repeat(id) {
                self.repeat_one(end, out)?;
                return Ok(());
            }
            self.preload_deadline_track(id, output, out)?;
        }
        let duration = if self.config.settings.gapless() {
            FrameCount::new(0)
        } else {
            self.fade_frames(self.config.settings.crossfade().duration)?
        };
        let ready = self.active.iter().any(|active| {
            active.item == id && active.role == Role::Preloaded && active.load.is_none()
        });
        if ready || self.earliest()? >= end - duration {
            self.request_transition(
                TransitionRequest {
                    id,
                    transition: Transition::Crossfade,
                    reason: AdvanceReason::NaturalEof,
                    auto: true,
                    playing: true,
                },
                output,
                out,
            )?;
        }
        Ok(())
    }

    fn preload_deadline_track(
        &mut self,
        id: TrackId,
        output: Option<&OutputSnapshot>,
        out: &mut Outbox<'_, S>,
    ) -> Result<(), QueueError> {
        if let Err(error) = self.load_track(id, Role::Preloaded, output, out) {
            if matches!(error, QueueError::Play(PlayError::Full("deck"))) {
                for parked in self
                    .active
                    .parked_iter_mut()
                    .filter(|parked| parked.item == id)
                {
                    parked.track.apply(TrackCommand::Release, out)?;
                }
                self.tracks.set_status(id, TrackStatus::Consumed);
            }
            return Err(error);
        }
        Ok(())
    }

    pub(in crate::queue) fn current_end(&self) -> Option<SessionFrame> {
        self.current?;
        let track = &self.active.get(self.active_current_index()?)?.track;
        let snapshot = track.snapshot();
        let snapshot = snapshot.as_ref();
        if let PlayingStatus::Ended { at } | PlayingStatus::Failed { at, .. } = snapshot.status {
            return Some(at);
        }
        if !matches!(snapshot.status, PlayingStatus::Playing { .. }) {
            return None;
        }
        snapshot.duration.filter(|duration| !duration.is_zero())?;
        let sample_rate = NonZeroU32::new(self.deck.mixer.sample_rate)?;
        track.planned_end(sample_rate).ok().flatten()
    }

    pub(in crate::queue) fn frames(&self, duration: Duration) -> Result<FrameCount, PlayError> {
        let sample_rate = NonZeroU32::new(self.deck.mixer.sample_rate).ok_or(PlayError::Untimed)?;
        let spec = AudioSpec::new(1, sample_rate);
        spec.frames_for(duration)
            .map_err(|error| PlayError::Internal(error.to_string()))
    }

    pub(in crate::queue) fn fade_frames(&self, duration: f32) -> Result<FrameCount, PlayError> {
        let time =
            Duration::try_from_secs_f32(duration).map_err(|_| PlayError::InvalidParameter {
                name: "crossfade duration".into(),
                value: duration,
            })?;
        self.frames(time)
    }
}
