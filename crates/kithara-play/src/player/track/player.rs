use kithara_command::{Sender, Seq, Target, When};
use kithara_config::ConfigOwner;
use kithara_render::{
    LaneCommand,
    bridge::{DeckEvent, DeckPart, Fade, FadeDir, Returned},
};
use kithara_signal::SessionFrame;
use kithara_warp::SpeedCurve;
use tracing::warn;

use super::{
    super::outbox::{Bound, Outbox, Player, Settled, TrackReceipt},
    core::PlayerImpl,
    state::*,
};
use crate::PlayError;

impl<S> Player<S> for PlayerImpl<S> {
    type Command = TrackCommand<S>;
    type Snapshot = TrackSnapshot;

    fn entry(&self, bound: Bound) -> Option<SessionFrame> {
        match bound {
            Bound::AtOrAfter(frame) | Bound::AtOrBefore(frame) => Some(frame),
        }
    }

    fn apply(
        &mut self,
        command: TrackCommand<S>,
        out: &mut Outbox<'_, S>,
    ) -> Result<Option<Seq>, PlayError> {
        self.observe(out);
        self.settle_lane();
        match command {
            TrackCommand::Load { item, position } => self.load(item, position, out),
            TrackCommand::Play { at } => self.play(at, out),
            TrackCommand::Pause { at } => self.pause(at, out),
            TrackCommand::Seek { to } => {
                if self.status == TrackStatus::Idle {
                    self.position = to;
                    return Ok(None);
                }
                self.segment(to, None, out)
            }
            TrackCommand::SetHostRate { rate } => self.segment(self.position, Some(rate), out),
            TrackCommand::Jump { to, at } => {
                let start = i64::from(at)
                    .checked_sub(
                        i64::try_from(self.declick.get())
                            .map_err(|error| PlayError::Internal(error.to_string()))?,
                    )
                    .map(SessionFrame::new)
                    .ok_or(PlayError::Late)?;
                let When::At(frame) = self.lane_when(When::At(start), out)? else {
                    return Err(PlayError::Untimed);
                };
                self.send_lane(LaneCommand::Jump { to }, When::At(frame))
                    .map(Some)
            }
            TrackCommand::Configure(change, at) => self.configure(change, at, out),
            TrackCommand::SetSpeed { speed, at } => self.set_speed(speed, at, out),
            TrackCommand::Fade { at, settings, dir } => {
                let slot = self.seat()?;
                let staged = dir == FadeDir::In
                    && out.is_grouped()
                    && self.attaching.is_some_and(|attach| {
                        attach.seq.is_none() && attach.replacement && attach.at == at
                    });
                if !staged {
                    self.check_when(at, out)?;
                }
                let part = match (self.status, dir) {
                    (TrackStatus::Playing { .. }, _) => DeckPart::Fade {
                        slot,
                        settings,
                        dir,
                    },
                    (_, FadeDir::In) => DeckPart::Start {
                        slot,
                        fade: Fade::Crossfade(settings),
                    },
                    (_, FadeDir::Out) => return Ok(None),
                };
                self.send_playback(at, vec![part], out)
            }
            TrackCommand::PlayAfter { track } if Some(track) == self.slot => {
                let slot = self.seat()?;
                if out.is_grouped() {
                    return Err(PlayError::Internal(
                        "a repeat adoption cannot join a timed group".into(),
                    ));
                }
                if self.lane.as_ref().is_none_or(|lane| lane.available() == 0) {
                    return Err(PlayError::Full("lane"));
                }
                if out.deck_available() == 0 {
                    return Err(PlayError::Full("deck"));
                }
                let parked = (self.segment, self.position);
                let segment = self.issued_segment.next();
                self.send_lane(
                    LaneCommand::Segment {
                        id: segment,
                        from: Position::ZERO,
                        speed: SpeedCurve::Constant(self.settings.projected().speed()),
                    },
                    When::Deferred,
                )?;
                self.issued_segment = segment;
                let seq = out.deferred(vec![DeckPart::Adopt { slot, segment }])?;
                self.segment = segment;
                self.playback_commands.clear();
                self.segment_speed = self.settings.projected().speed();
                self.ready = None;
                self.repeat = true;
                self.adopting.push(Adoption {
                    seq,
                    caller: seq,
                    segment,
                    parked: Some(parked),
                    cancelled: false,
                });
                Ok(Some(seq))
            }
            TrackCommand::PlayAfter { track } => self.send_playback(
                When::Deferred,
                vec![DeckPart::Chain {
                    from: track,
                    to: self.seat()?,
                }],
                out,
            ),
            TrackCommand::Supersede => {
                if self
                    .adopting
                    .iter()
                    .any(|adoption| adoption.parked.is_some())
                    && self.lane.as_ref().is_none_or(|lane| lane.available() == 0)
                {
                    return Err(PlayError::Full("lane"));
                }
                out.supersede(self.seat()?)?;
                for adoption in &mut self.adopting {
                    if adoption.parked.is_some() {
                        adoption.cancelled = true;
                    }
                }
                self.adopt_retry = None;
                self.repeat = false;
                Ok(None)
            }
            TrackCommand::Release => self.release_track(out),
            TrackCommand::Seat { slot, at } => self.seat_at(slot, at, out),
        }
    }

    fn settle(&mut self, receipt: TrackReceipt<'_, S>, out: &mut Outbox<'_, S>) -> Settled {
        match receipt {
            TrackReceipt::Loaded(receipt) => {
                self.observe(out);
                self.opened(receipt, out)
            }
            TrackReceipt::Deck {
                seq,
                outcome,
                batch,
            } if batch.basis.iter().any(|&(slot, _)| Some(slot) == self.slot) => {
                let attaching = self.attaching.is_some_and(|attach| attach.seq == Some(seq));
                let adopting = self.adopting.iter().any(|adopt| adopt.seq == seq);
                let playback = self.playback_commands.iter().position(|operation| {
                    operation.seq == Some(seq)
                        && operation.segment == self.segment
                        && operation
                            .basis
                            .iter()
                            .all(|basis| batch.basis.contains(basis))
                });
                if !attaching && !adopting && playback.is_none() {
                    if self.attached()
                        && self.attaching.is_none()
                        && self.status != TrackStatus::Released
                        && batch.commands.iter().any(|part| {
                            matches!(part, DeckPart::Returned(Returned::Pcm { slot, .. })
                                if Some(*slot) == self.slot)
                        })
                    {
                        self.status = TrackStatus::Released;
                        self.mark = None;
                        self.play = None;
                        self.ready = None;
                        self.playback_commands.clear();
                        self.adopting.clear();
                        self.adopt_retry = None;
                        if let Err(error) = self.release_lane(out) {
                            warn!(%error, "lane release waits for dispatcher room");
                        }
                    }
                    return Settled::Pending;
                }
                if let Some(index) = playback {
                    self.playback_commands.remove(index);
                }
                self.observe(out);
                self.applied(seq, outcome, batch, out)
            }
            TrackReceipt::Deck { .. } => Settled::Pending,
            TrackReceipt::Event(event) => {
                self.observe(out);
                if !self.attached() || matches!(self.status, TrackStatus::Failed { .. }) {
                    return Settled::Pending;
                }
                match event {
                    DeckEvent::Failed { slot, at, fault } if Some(slot) == self.slot => {
                        if let TrackStatus::Playing { since } = self.status
                            && at.frames_since(since).is_some()
                        {
                            self.status = TrackStatus::Failed { at, fault };
                        }
                    }
                    DeckEvent::Ended { slot, at } if Some(slot) == self.slot && !self.repeat => {
                        self.status = TrackStatus::Ended { at }
                    }
                    DeckEvent::Faded { slot, at } if Some(slot) == self.slot => {
                        self.status = TrackStatus::Faded { at }
                    }
                    DeckEvent::Underrun { slot, .. }
                        if Some(slot) == self.slot && self.loading.is_none() =>
                    {
                        self.mark = out
                            .pass()
                            .and_then(|pass| pass.deck.slots.get(slot.index()))
                            .and_then(|slot| slot.mark);
                    }
                    _ => {}
                }
                Settled::Pending
            }
        }
    }

    fn tick(&mut self, _now: SessionFrame, out: &mut Outbox<'_, S>) {
        self.observe(out);
        self.settle_lane();
        self.retry_adopt(out);
        if let Err(error) = self.promote(out) {
            warn!(%error, "a seated lane waits to leave the background class");
        }
        if self.status == TrackStatus::Released {
            if let Err(error) = self.release_lane(out) {
                warn!(%error, "lane release waits for room");
            }
        } else if self.releasing && self.attaching.is_none() && self.playback_commands.is_empty() {
            self.retry_detach(out);
        } else if self.attached()
            && self.ready == Some(self.segment)
            && let Some(at) = self.play
        {
            match self.play(at, out) {
                Ok(Some(_)) => self.play = None,
                Ok(None) => {}
                Err(error) => warn!(%error, "ready start was rejected"),
            }
        }
    }

    fn snapshot(&self) -> TrackSnapshot {
        TrackSnapshot {
            item: self.item,
            slot: self.slot,
            status: self.status,
            speed: self.settings.config().speed(),
            position: self.position,
            duration: self.duration,
            abr: self.abr.clone(),
            metadata: self.metadata.clone(),
            mark: self.mark,
            engine_latency: self.engine_latency,
            ring_depth: self.ring_depth,
            lane_room: self.lane.as_ref().map_or(0, Sender::available),
            pending_lane: self
                .lane_commands
                .iter()
                .any(|operation| operation.applied.is_none()),
            attached: self.attached(),
            declick: self.declick,
        }
    }
}
