use std::num::NonZeroU32;

use kithara_bufpool::HasPool;
use kithara_command::{Seq, When};
use kithara_events::TrackId;
use kithara_platform::time::Duration;
use kithara_play::{
    Bound, Outbox, OutputSnapshot, PlayError, Player, Track, TrackCommand, TrackFactory,
    TrackStatus as PlayingStatus,
};
use kithara_signal::{AudioSpec, FrameCount, SessionFrame};

use super::{
    super::{
        Queue, Transition,
        command::play_error,
        slots::{LoadState, Role},
        types::Target,
    },
    TransitionRequest,
};
use crate::{ActionAtItemEnd, AdvanceReason, QueueError, RepeatMode, TrackStatus};

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
        if target.repeat.is_some() {
            self.cancel_repeat(out)?;
            self.target = None;
            return Ok(());
        }
        if let Some(index) = self.incoming_index(target.to) {
            self.release_track(index, out)?;
        }
        self.tracks.set_status(target.to, TrackStatus::Cancelled);
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
            self.tracks.set_status(id, TrackStatus::Cancelled);
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
        if let Some(batch) = self.active.get(index).and_then(|active| match active.role {
            Role::Incoming { batch } => batch,
            _ => None,
        }) {
            self.active
                .get_mut(index)
                .ok_or(PlayError::NoActiveSlot)?
                .track
                .apply(TrackCommand::Pause { at: When::Next }, out)?;
            self.target.as_mut().ok_or(PlayError::NotReady)?.stale = Some(batch);
        } else {
            let settings = target.transition.settings(self.config.settings.crossfade());
            self.target.as_mut().ok_or(PlayError::NotReady)?.settings = settings;
        }
        Ok(())
    }

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
        let Some(end) = self.current_end() else {
            return Ok(());
        };
        if let Some(target) = self.target {
            if target.auto && self.single_slot_repeat(target.to) {
                if target.repeat.is_none() {
                    self.repeat_one(end, out)?;
                }
                return Ok(());
            }
            if target.auto && self.auto_bound(target.settings)? != Some(target.bound) {
                self.withdraw_auto(out)?;
            }
            return Ok(());
        }
        let ids = self.track_ids();
        let wrap = self.navigation.repeat_mode() == RepeatMode::All;
        let Some(id) = self.navigation.next(&ids, true, wrap) else {
            return Ok(());
        };
        if now >= end - self.frames(self.config.preload_lead)? {
            if self.single_slot_repeat(id) {
                self.repeat_one(end, out)?;
                return Ok(());
            }
            self.load_track(id, Role::Preloaded, output, out)?;
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

    pub(in crate::queue) fn incoming_index(&self, id: TrackId) -> Option<usize> {
        self.active
            .position(|active| active.item == id && matches!(active.role, Role::Incoming { .. }))
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
            bound: Bound::AtOrBefore(end),
            settings: Transition::None.settings(self.config.settings.crossfade()),
            transition: Transition::None,
            reason: AdvanceReason::NaturalEof,
            auto: true,
            stale: None,
            retry: None,
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

    pub(in crate::queue) fn current_end(&self) -> Option<SessionFrame> {
        let track = self.current_track()?;
        let snapshot = track.snapshot();
        let snapshot = snapshot.as_ref();
        if let PlayingStatus::Ended { at } | PlayingStatus::Failed { at, .. } = snapshot.status {
            return Some(at);
        }
        if !matches!(snapshot.status, PlayingStatus::Playing { .. }) {
            return None;
        }
        snapshot.duration?;
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
