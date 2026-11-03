use std::mem;

use kithara_command::{Seq, When};
use kithara_play::{
    Bound, CrossfadeSettings, FadeDir, Outbox, PlayError, Position, SeekBatch, Track, TrackCommand,
    TrackSettingsChange, TrackStatus,
};
use kithara_signal::SessionFrame;
use kithara_sync::{SourceGrid, SpeedCurve};
use tracing::warn;

use super::{
    Linked, LinkedPlayer, Waiting,
    geometry::{PastEnd, Placement, cue_position, place},
    state::{Cue, SpeedOperation},
};
use crate::{covers, entry, jump_target};

impl<P> Linked<P> {
    pub(super) fn synced_speed<S>(&self, change: TrackSettingsChange) -> Result<(), PlayError>
    where
        P: Track<S>,
    {
        if self.synced()
            && let TrackSettingsChange::Speed(speed) = change
        {
            return Err(PlayError::InvalidParameter {
                name: "speed while synchronized".into(),
                value: speed,
            });
        }
        Ok(())
    }

    /// The frame a synchronized Play starts from: its own, or the earliest start for Next.
    fn play_bound<S>(
        &self,
        at: When<SessionFrame>,
        out: &Outbox<'_, S>,
    ) -> Result<SessionFrame, PlayError>
    where
        P: Track<S>,
    {
        let frame = match at {
            When::Next => return self.earliest(out),
            When::At(frame) => frame,
            When::Deferred => {
                return Err(PlayError::Internal(
                    "a linked Play has no deferred executor moment".into(),
                ));
            }
        };
        if frame < self.earliest(out)? {
            return Err(PlayError::Late);
        }
        Ok(frame)
    }

    /// A fade out drops a waiting Play; a fade in clears a silent deck's owed correction.
    pub(super) fn fade_synced<S>(
        &mut self,
        at: When<SessionFrame>,
        settings: CrossfadeSettings,
        dir: FadeDir,
        out: &mut Outbox<'_, S>,
    ) -> Result<Option<Seq>, PlayError>
    where
        P: Track<S>,
    {
        self.check_synced_fade::<S>(at, dir)?;
        let sent = self
            .inner
            .apply(TrackCommand::Fade { at, settings, dir }, out)?;
        if dir == FadeDir::Out && matches!(self.waiting, Some(Waiting::Play { .. })) {
            self.waiting = None;
        }
        if dir == FadeDir::In {
            self.entry_planned::<S>();
        }
        Ok(sent)
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
        self.clear_grid();
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
        let to = match place(grid, Cue::Covered(to), PastEnd::LastBeat) {
            Placement::At(position) => position,
            Placement::Wait => {
                self.waiting = Some(Waiting::Seek { required: to });
                return Ok(None);
            }
            Placement::Refused => {
                warn!(?to, "the final grid holds no beat for this seek");
                self.waiting = None;
                self.refuse_sync(out);
                return self.inner.seek(to, out);
            }
        };
        let at = self.jump_frame(out)?;
        let source = self.source(grid)?;
        let sent = self.jump_to(&source, to, at, out)?;
        self.waiting = None;
        Ok(sent.map(SeekBatch::Lane))
    }

    /// Jumps to the in-phase position nearest `to`; a speed change rides an Align.
    fn jump_to<S>(
        &mut self,
        source: &SourceGrid,
        to: Position,
        at: SessionFrame,
        out: &mut Outbox<'_, S>,
    ) -> Result<Option<Seq>, PlayError>
    where
        P: Track<S>,
    {
        let target = jump_target(&self.host, source, to, at).ok_or(PlayError::NotReady)?;
        let start = at - self.inner.snapshot().as_ref().declick;
        let (position, _) = self.planned::<S>(start)?;
        let speed = self.curve(source, position, start, Some((target, at)))?;
        if matches!(&speed, SpeedCurve::Constant(value) if self.inner.projected().speed() == *value)
        {
            return self.inner.apply(TrackCommand::Jump { to: target, at }, out);
        }

        let sent = self.inner.apply(
            TrackCommand::Align {
                to: target,
                speed,
                at,
            },
            out,
        )?;
        if let Some(seq) = sent {
            self.speeds.push((seq, SpeedOperation::Relocation { to }));
        }
        Ok(sent)
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
        let cue = Cue::Beat(required);
        match place(grid, cue, PastEnd::Refused) {
            Placement::At(_) if self.cue.is_none() => {
                self.cue = Some(cue);
                self.prepare_cue(out);
            }
            Placement::Refused => {
                warn!(?required, "the final grid holds no beat for this Play");
                self.refuse_sync(out);
            }
            Placement::At(_) | Placement::Wait => {}
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
        if at != When::Next {
            self.play_bound(at, out)?;
        }
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

        let bound = self.play_bound(at, out)?;
        let source = self.source(grid)?;
        self.start_synced(&source, required, bound, at, out)
    }

    /// Starts on the first in-phase entry from `bound`; a deck paused in place
    /// first cues the in-phase position nearest its cue.
    fn start_synced<S>(
        &mut self,
        source: &SourceGrid,
        required: Position,
        bound: SessionFrame,
        at: When<SessionFrame>,
        out: &mut Outbox<'_, S>,
    ) -> Result<Option<Seq>, PlayError>
    where
        P: Track<S>,
    {
        if let Some(target) = self.in_place_target(source, required, bound)? {
            self.cue = Some(Cue::Covered(target));
            self.waiting = Some(Waiting::Play {
                required: target,
                at,
            });
            self.prepare_cue(out);
            return Ok(None);
        }

        let frame = entry(&self.host, source, required, Bound::AtOrAfter(bound))
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

    /// The in-phase position a deck paused in place resumes on, once ready after `bound`.
    fn in_place_target<S>(
        &mut self,
        source: &SourceGrid,
        required: Position,
        bound: SessionFrame,
    ) -> Result<Option<Position>, PlayError>
    where
        P: Track<S>,
    {
        let status = self.inner.snapshot().as_ref().status;
        if !matches!(
            status,
            TrackStatus::Paused { .. } | TrackStatus::Faded { .. }
        ) || !mem::take(&mut self.paused_in_place)
        {
            return Ok(None);
        }
        let target = jump_target(&self.host, source, required, self.prepared(bound)?);
        Ok(target.filter(|target| {
            self.grid
                .as_ref()
                .is_some_and(|grid| covers(grid.geometry(), *target))
        }))
    }
}
