use std::mem;

use kithara_command::{Seq, When};
use kithara_play::{
    Bound, FadeDir, Outbox, PlayError, Position, SeekBatch, Track, TrackCommand,
    TrackSettingsChange, TrackStatus,
};
use kithara_signal::SessionFrame;
use tracing::warn;

use super::{
    Linked, LinkedPlayer, SyncMode, Waiting,
    geometry::{cue_position, is_final, last_beat, placement},
    state::{Cue, SpeedOperation},
};
use crate::{covers, entry, jump_target};

impl<P> Linked<P> {
    pub(super) fn synced_speed(&self, change: TrackSettingsChange) -> Result<(), PlayError> {
        if self.mode == SyncMode::On
            && let TrackSettingsChange::Speed(speed) = change
        {
            return Err(PlayError::InvalidParameter {
                name: "speed while synchronized".into(),
                value: speed,
            });
        }
        Ok(())
    }

    pub(super) fn check_synced_command<S>(
        &self,
        command: &TrackCommand<S>,
        out: &Outbox<'_, S>,
    ) -> Result<(), PlayError>
    where
        P: Track<S>,
    {
        if self.synced() && matches!(command, TrackCommand::Play { at: When::Deferred }) {
            return Err(PlayError::Internal(
                "a linked Play has no deferred executor moment".into(),
            ));
        }
        if self.synced()
            && let TrackCommand::Play {
                at: When::At(frame),
            } = command
            && *frame < self.earliest_start(out)?
        {
            return Err(PlayError::Late);
        }
        Ok(())
    }

    pub(super) fn load_item<S>(
        &mut self,
        item: kithara_play::ResourceLoad<S>,
        position: Position,
        out: &mut Outbox<'_, S>,
    ) -> Result<Option<Seq>, PlayError>
    where
        P: Track<S>,
    {
        let seq = self
            .inner
            .apply(TrackCommand::Load { item, position }, out)?;
        self.grid = None;
        self.correction = None;
        self.alignment = None;
        self.speeds.clear();
        self.relocation = None;
        self.paused_in_place = false;
        self.start = None;
        self.start_caller = None;
        self.withdrawn.clear();
        self.retimes.clear();
        self.cue = self.synced().then_some(Cue::Beat(position));
        self.waiting = self
            .synced()
            .then_some(Waiting::Load { required: position });
        Ok(seq)
    }

    pub(super) fn seek_synced<S>(
        &mut self,
        to: Position,
        out: &mut Outbox<'_, S>,
    ) -> Result<Option<SeekBatch>, PlayError>
    where
        P: Track<S>,
    {
        let status = self.inner.snapshot().as_ref().status;
        if status == TrackStatus::Loading && self.cue.is_some() {
            self.cue = Some(Cue::Beat(to));
            if let Some(Waiting::Load { required } | Waiting::Play { required, .. }) =
                self.waiting.as_mut()
            {
                *required = to;
            }
            return Ok(None);
        }
        if !matches!(status, TrackStatus::Playing { .. }) {
            let sent = self.inner.seek(to, out)?;
            if let Some(start) = self.start {
                self.start_caller.get_or_insert(start);
            }
            self.withdraw_start(to);
            if self.cue.is_some() {
                self.cue = Some(Cue::Beat(to));
            }
            return Ok(sent);
        }
        self.seek_playing(to, out)
    }

    fn seek_playing<S>(
        &mut self,
        to: Position,
        out: &mut Outbox<'_, S>,
    ) -> Result<Option<SeekBatch>, PlayError>
    where
        P: Track<S>,
    {
        let Some(grid) = self.grid.as_ref() else {
            self.waiting = Some(Waiting::Seek { required: to });
            return Ok(None);
        };
        let Some(to) = placement(grid, to).or_else(|| last_beat(grid)) else {
            if is_final(grid) {
                warn!(?to, "the final grid holds no beat for this seek");
                self.waiting = None;
                self.refuse_sync(out);
                return self.inner.seek(to, out);
            }
            self.waiting = Some(Waiting::Seek { required: to });
            return Ok(None);
        };
        let at = self.jump_frame(out)?;
        let source = self.source(grid)?;
        let target = jump_target(&self.host, &source, to, at).ok_or(PlayError::NotReady)?;
        let start = at - self.inner.snapshot().as_ref().declick;
        let (position, _) = self.planned::<S>(start)?;
        let speed = self.curve(&source, position, start, Some((target, at)))?;
        let synchronized = matches!(&speed, kithara_sync::SpeedCurve::Constant(value) if self.inner.projected().speed() == *value);
        let command = if synchronized {
            TrackCommand::Jump { to: target, at }
        } else {
            TrackCommand::Align {
                to: target,
                speed,
                at,
            }
        };
        let sent = self.inner.apply(command, out)?;
        if !synchronized && let Some(seq) = sent {
            self.speeds.push((seq, SpeedOperation::Relocation { to }));
        }
        self.waiting = None;
        Ok(sent.map(SeekBatch::Lane))
    }

    pub(super) fn check_synced_fade<S>(
        &self,
        at: When<SessionFrame>,
        dir: FadeDir,
    ) -> Result<(), PlayError>
    where
        P: Track<S>,
    {
        if dir == FadeDir::In
            && !matches!(
                self.inner.snapshot().as_ref().status,
                TrackStatus::Playing { .. }
            )
        {
            let When::At(frame) = at else {
                return Err(PlayError::Untimed);
            };
            let grid = self.grid.as_ref().ok_or(PlayError::NotReady)?;
            let source = self.source(grid)?;
            if self.inner.entry(Bound::AtOrAfter(frame)).is_none()
                || self.inner.prepared_speed()
                    != Some(&self.curve(
                        &source,
                        cue_position(self.inner.snapshot().as_ref()),
                        frame,
                        None,
                    )?)
            {
                return Err(PlayError::NotReady);
            }
        }
        Ok(())
    }

    /// A Play the grid does not cover waits, and a stopped track moves to the
    /// grid's boundary or refuses SYNC when a final grid holds none.
    fn park_play<S>(&mut self, required: Position, at: When<SessionFrame>, out: &mut Outbox<'_, S>)
    where
        P: Track<S>,
    {
        self.waiting = Some(Waiting::Play { required, at });
        let snapshot = self.inner.snapshot();
        let track = snapshot.as_ref();
        if matches!(track.status, TrackStatus::Playing { .. }) || track.pending_lane {
            return;
        }
        let Some(grid) = self.grid.as_ref() else {
            return;
        };
        match placement(grid, required) {
            Some(_) if self.cue.is_none() => {
                self.cue = Some(Cue::Beat(required));
                self.prepare_cue(out);
            }
            None if is_final(grid) => {
                warn!(?required, "the final grid holds no beat for this Play");
                self.refuse_sync(out);
            }
            Some(_) | None => {}
        }
    }

    pub(super) fn play_synced<S>(
        &mut self,
        at: When<SessionFrame>,
        out: &mut Outbox<'_, S>,
    ) -> Result<Option<Seq>, PlayError>
    where
        P: Track<S>,
    {
        let track = self.inner.snapshot();
        let required = cue_position(track.as_ref());
        let Some(grid) = self
            .grid
            .as_ref()
            .filter(|grid| covers(grid.geometry(), required))
        else {
            self.park_play(required, at, out);
            return Ok(None);
        };
        if matches!(
            track.as_ref().status,
            TrackStatus::Idle | TrackStatus::Loading
        ) || self.cue.is_some()
        {
            self.waiting = Some(Waiting::Play { required, at });
            return Ok(None);
        }
        let earliest = self.earliest_start(out)?;
        let bound = match at {
            When::Next => earliest,
            When::At(frame) if frame < earliest => return Err(PlayError::Late),
            When::At(frame) => frame,
            When::Deferred => {
                return Err(PlayError::Internal(
                    "a linked Play has no deferred executor moment".into(),
                ));
            }
        };
        let source = self.source(grid)?;
        if matches!(
            track.as_ref().status,
            TrackStatus::Paused { .. } | TrackStatus::Faded { .. }
        ) && mem::take(&mut self.paused_in_place)
            && let Some(target) = jump_target(&self.host, &source, required, self.prepared(bound)?)
                .filter(|target| covers(grid.geometry(), *target))
        {
            self.cue = Some(Cue::Covered(target));
            self.waiting = Some(Waiting::Play {
                required: target,
                at,
            });
            self.prepare_cue(out);
            return Ok(None);
        }
        let frame = entry(&self.host, &source, required, Bound::AtOrAfter(bound))
            .ok_or(PlayError::NotReady)?;
        if !self.prepare_entry(frame, out)? {
            self.waiting = Some(Waiting::Play { required, at });
            return Ok(None);
        }
        let sent = self.inner.apply(
            TrackCommand::Play {
                at: When::At(frame),
            },
            out,
        )?;
        self.waiting = None;
        self.start = sent;
        self.entry_planned::<S>();
        Ok(sent)
    }
}
