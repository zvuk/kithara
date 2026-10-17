use kithara_bufpool::HasPool;
use kithara_command::{Seq, When};
use kithara_events::TrackId;
use kithara_play::{
    Bound, Outbox, PlayError, Player, TrackCommand, TrackFactory, TrackStatus as PlayingStatus,
};
use kithara_signal::SessionFrame;

use super::super::{
    Queue, Transition,
    command::play_error,
    slots::{LoadState, Role},
    types::Target,
};
use crate::{AdvanceReason, QueueError, RepeatMode, TrackStatus};

impl<S, F> Queue<S, F>
where
    S: HasPool<u8> + HasPool<f32> + Send + Sync + 'static,
    F: TrackFactory<S>,
{
    /// Step 6: outgoing tracks keep their original tails until RT stops them.
    pub(in crate::queue) fn release_tails(
        &mut self,
        out: &mut Outbox<'_, S>,
    ) -> Result<(), PlayError> {
        let ended = self.active.indices(|active| {
            active.role == Role::Outgoing
                && !matches!(
                    active.track.snapshot().as_ref().status,
                    PlayingStatus::Playing { .. }
                )
        });
        for index in ended {
            self.release_track(index, out)?;
        }
        Ok(())
    }

    pub(in crate::queue) fn cancel_target(
        &mut self,
        out: &mut Outbox<'_, S>,
    ) -> Result<(), QueueError> {
        let Some(target) = self.target else {
            return Ok(());
        };
        let consumed =
            target.auto
                && self
                    .incoming_index(target.to)
                    .is_some_and(|index| !self.active.is_replacement(index))
                || target.reason == AdvanceReason::InitialLoad
                    && self.tracks.records().iter().any(|record| {
                        record.id == target.to && record.status == TrackStatus::Loaded
                    });
        if target.repeat.is_some() {
            self.cancel_repeat(out)?;
            self.target = None;
            return Ok(());
        }
        let incoming = self.incoming_index(target.to);
        if let Some(index) = incoming {
            if target.chained {
                self.active
                    .get_mut(index)
                    .ok_or(PlayError::NoActiveSlot)?
                    .track
                    .apply(TrackCommand::Pause { at: When::Next }, out)?;
            } else {
                self.release_track(index, out)?;
            }
        }
        if (incoming.is_some() || self.current != Some(target.to))
            && !self.tracks.records().iter().any(|record| {
                record.id == target.to && matches!(record.status, TrackStatus::Failed(_))
            })
        {
            self.tracks.set_status(
                target.to,
                if consumed {
                    TrackStatus::Consumed
                } else {
                    TrackStatus::Cancelled
                },
            );
        }
        self.target = None;
        self.reap_released();
        Ok(())
    }

    pub(in crate::queue) fn cancel_auto(
        &mut self,
        out: &mut Outbox<'_, S>,
    ) -> Result<(), QueueError> {
        if self.target.is_some_and(|target| target.auto) {
            self.cancel_target(out)?;
        }
        for index in self.active.indices(|active| active.role == Role::Preloaded) {
            let id = self.active.get(index).ok_or(PlayError::NoActiveSlot)?.item;
            self.release_track(index, out)?;
            self.tracks.set_status(id, TrackStatus::Consumed);
        }
        self.reap_released();
        Ok(())
    }

    pub(in crate::queue) fn withdraw_auto(
        &mut self,
        out: &mut Outbox<'_, S>,
    ) -> Result<(), QueueError> {
        if self.target.is_some_and(|target| target.auto) {
            self.withdraw_transition(out)?;
        }
        Ok(())
    }

    /// A Stop supersedes B's slot basis; only Stale allows the new entry.
    pub(in crate::queue) fn withdraw_transition(
        &mut self,
        out: &mut Outbox<'_, S>,
    ) -> Result<(), QueueError> {
        let Some(target) = self.target.filter(|target| target.stale.is_none()) else {
            return Ok(());
        };
        let Some(index) = self.incoming_index(target.to) else {
            return Ok(());
        };
        if let Some(pending) = self
            .pending_handover
            .filter(|pending| pending.item == target.to)
        {
            let pause_seq = self
                .active
                .get_mut(index)
                .ok_or(PlayError::NoActiveSlot)?
                .track
                .apply(TrackCommand::Pause { at: When::Next }, out)?;
            kithara_test_utils::kithara::probe_event!(
                handover_withdrawal_submitted,
                pending_seq = pending.seq.get(),
                pause_seq = pause_seq.map(Seq::get),
                chained = target.chained
            );
            tracing::debug!(
                pending_seq = pending.seq.get(),
                ?pause_seq,
                chained = target.chained,
                "queue handover withdrawal submitted"
            );
            self.target.as_mut().ok_or(PlayError::NotReady)?.stale = Some(pending.seq);
        } else {
            let settings = target.transition.settings(self.config.settings.crossfade());
            self.target.as_mut().ok_or(PlayError::NotReady)?.settings = settings;
        }
        Ok(())
    }

    pub(in crate::queue) fn incoming_index(&self, id: TrackId) -> Option<usize> {
        self.active
            .position(|active| active.item == id && matches!(active.role, Role::Incoming))
    }

    pub(in crate::queue) fn single_slot_repeat(&self, id: TrackId) -> bool {
        self.config.mixer.slots().get() == 1
            && self.navigation.repeat_mode() == RepeatMode::One
            && self.current == Some(id)
    }

    pub(in crate::queue) fn repeat_one(
        &mut self,
        end: SessionFrame,
        out: &mut Outbox<'_, S>,
    ) -> Result<(), PlayError> {
        if out.deck_available() == 0 {
            return Err(PlayError::Full("deck"));
        }
        let seq = self.repeat_segment(out)?;
        let to = self.current.ok_or(PlayError::NoActiveSlot)?;
        self.target = Some(Target {
            playing: true,
            to,
            bound: Some(Bound::AtOrBefore(end)),
            settings: Transition::None.settings(self.config.settings.crossfade()),
            transition: Transition::None,
            reason: AdvanceReason::NaturalEof,
            auto: true,
            stale: None,
            continued: None,
            repeat: Some(seq),
            chained: false,
        });
        Ok(())
    }

    pub(in crate::queue) fn repeat_segment(
        &mut self,
        out: &mut Outbox<'_, S>,
    ) -> Result<Seq, PlayError> {
        let index = self.active_current_index().ok_or(PlayError::NoActiveSlot)?;
        let active = self.active.get_mut(index).ok_or(PlayError::NoActiveSlot)?;
        if active.track.snapshot().as_ref().lane_room == 0 {
            return Err(PlayError::Full("lane"));
        }
        active
            .track
            .apply(TrackCommand::PlayAfter { track: active.slot }, out)?
            .ok_or_else(|| {
                PlayError::Internal("a repeated segment must name its Adopt batch".into())
            })
    }

    pub(in crate::queue) fn cancel_repeat(
        &mut self,
        out: &mut Outbox<'_, S>,
    ) -> Result<(), PlayError> {
        let index = self.active_current_index().ok_or(PlayError::NoActiveSlot)?;
        self.active
            .get_mut(index)
            .ok_or(PlayError::NoActiveSlot)?
            .track
            .apply(TrackCommand::Supersede, out)?;
        Ok(())
    }

    pub(in crate::queue) fn loaded_tracks(
        &mut self,
        out: &mut Outbox<'_, S>,
    ) -> Result<(), PlayError> {
        let ready = self.active.indices(|active| {
            matches!(active.load, Some(LoadState::Attaching(_)))
                && matches!(
                    active.track.snapshot().as_ref().status,
                    PlayingStatus::Loaded | PlayingStatus::Paused { .. }
                )
        });
        for index in ready {
            if self.finish_load(index)? {
                self.transition_loaded(out)?;
            } else {
                self.cancel_target(out).map_err(play_error)?;
            }
        }
        Ok(())
    }
}
