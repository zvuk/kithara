use std::num::NonZeroU32;

use kithara_command::{Batch, Outcome, Rejection, Seq, When};
use kithara_render::{
    LaneApplied, LaneCommand, LaneFrame,
    bridge::{DeckPart, DeckProtocol, DeckRefusal, Returned, SlotMark},
};
use kithara_signal::SessionFrame;
use kithara_sync::SpeedCurve;
use tracing::warn;

use super::{
    super::{
        outbox::{Outbox, Settled, rejection},
        settings::TrackSettingsChange,
    },
    core::PlayerImpl,
    geometry::session_at,
    state::*,
};
use crate::PlayError;

impl<S> PlayerImpl<S> {
    pub(super) fn segment(
        &mut self,
        from: Position,
        rate: Option<NonZeroU32>,
        out: &mut Outbox<'_, S>,
    ) -> Result<Option<Seq>, PlayError> {
        self.segment_with(
            from,
            rate,
            SpeedCurve::Constant(self.settings.projected().speed()),
            out,
        )
    }

    pub(super) fn segment_with(
        &mut self,
        from: Position,
        rate: Option<NonZeroU32>,
        mut speed: SpeedCurve,
        out: &mut Outbox<'_, S>,
    ) -> Result<Option<Seq>, PlayError> {
        let final_speed = super::geometry::check_curve(&mut speed)?;
        let change = TrackSettingsChange::Speed(final_speed);
        let starting_speed = self.segment_start_speed(&speed)?;
        self.check_segment(out)?;
        let attached = self.attached();
        let origin = if rate.is_some() {
            self.speed_position().ok()
        } else {
            Some(from)
        };
        let segment = self.issued_segment.next();
        let command = rate.map_or(
            LaneCommand::Segment {
                id: segment,
                from,
                speed,
            },
            |rate| LaneCommand::SetHostRate { id: segment, rate },
        );
        let commands = match starting_speed {
            Some(speed) => vec![LaneCommand::SetSpeed(SpeedCurve::Constant(speed)), command],
            None => vec![command],
        };
        let seq = self.send_lane_batch(commands, When::Next)?;
        self.issued_segment = segment;
        if rate.is_none() {
            self.settings.track(seq, When::Next, change);
        }
        self.segment = segment;
        self.segment_origin = origin;
        self.observed_source = None;
        self.segment_speed = starting_speed.unwrap_or(final_speed);
        self.ready = None;
        self.mark = None;
        self.resume = None;
        if self.slot.is_none() && self.loading.is_some() {
            self.status = TrackStatus::Loading;
        }
        if attached {
            let adopt = out
                .deck_owned(
                    When::Next,
                    vec![DeckPart::Adopt {
                        slot: self.seat()?,
                        segment,
                    }],
                )
                .map_err(|(error, _parts)| error)?;
            if let Some(adopt) = adopt {
                self.adopting.push(Adoption {
                    seq: adopt,
                    caller: adopt,
                    segment,
                    parked: None,
                    cancelled: false,
                });
            }
            return Ok(adopt);
        }
        Ok(Some(seq))
    }

    fn segment_start_speed(&self, speed: &SpeedCurve) -> Result<Option<f32>, PlayError> {
        Ok(match speed {
            SpeedCurve::Ramp { .. } => Some(self.consumed_speed()?),
            SpeedCurve::Steps(steps) if steps.first().is_some_and(|(at, _)| *at > 0) => {
                Some(self.consumed_speed()?)
            }
            _ => None,
        })
    }

    pub(super) fn check_segment(&self, out: &Outbox<'_, S>) -> Result<(), PlayError> {
        let attached = self.attached();
        if attached && out.is_grouped() {
            return Err(PlayError::Internal(
                "a segment adoption cannot join a timed group".into(),
            ));
        }
        let lane = self.lane.as_ref().ok_or(PlayError::NotReady)?;
        if lane.available() == 0 {
            return Err(PlayError::Full("lane"));
        }
        if attached && out.deck_available() == 0 {
            return Err(PlayError::Full("deck"));
        }
        Ok(())
    }

    pub(super) fn applied(
        &mut self,
        seq: Seq,
        outcome: &Outcome<DeckProtocol>,
        batch: &mut Batch<DeckProtocol>,
        transport_current: bool,
        out: &mut Outbox<'_, S>,
    ) -> Settled {
        let attaching = self.attaching.filter(|attach| attach.seq == Some(seq));
        let mut answered = attaching.map_or(seq, |attach| attach.caller);
        if let Some(index) = self.adopting.iter().position(|adopt| adopt.seq == seq) {
            let adoption = self.adopting.remove(index);
            answered = adoption.caller;
            if let Some(settled) = self.apply_adoption(&adoption, outcome, out) {
                return settled;
            }
        }
        let at = match outcome {
            Outcome::Applied { at, .. } => *at,
            Outcome::Rejected(reason) => {
                if attaching.is_some() {
                    let returned = self.reject_attachment(batch, out);
                    if !returned {
                        return Settled::Rejected {
                            seq: answered,
                            reason: Rejection::Refused(PlayError::Internal(
                                "rejected attachment did not return its original PCM".into(),
                            )),
                        };
                    }
                }
                return Settled::Rejected {
                    seq: answered,
                    reason: rejection(reason, |refusal| PlayError::Deck(*refusal)),
                };
            }
        };
        if attaching.is_some() {
            self.attaching = None;
            self.loading = None;
            self.attach_at = Some(at);
            if transport_current {
                self.status = TrackStatus::Loaded;
            }
        }
        self.settle_parts(batch, attaching.is_some(), transport_current, at, out);
        self.promote(out);
        Settled::Applied { seq: answered, at }
    }

    fn apply_adoption(
        &mut self,
        adoption: &Adoption,
        outcome: &Outcome<DeckProtocol>,
        out: &mut Outbox<'_, S>,
    ) -> Option<Settled> {
        if self.preparation_closed() {
            self.repeat = false;
            return Some(Settled::Pending);
        }
        if adoption.cancelled {
            if adoption.segment == self.segment
                && matches!(outcome, Outcome::Rejected(Rejection::Stale))
                && let Some((segment, position)) = adoption.parked
            {
                if let Err(error) = self.send_lane(
                    LaneCommand::Segment {
                        id: segment,
                        from: position,
                        speed: SpeedCurve::Constant(self.settings.projected().speed()),
                    },
                    When::Next,
                ) {
                    return Some(Settled::Rejected {
                        seq: adoption.caller,
                        reason: Rejection::Refused(error),
                    });
                }
                self.segment = segment;
                self.segment_origin = Some(position);
                self.observed_source = None;
                self.ready = None;
            }
            self.repeat = false;
            return Some(Settled::Pending);
        }
        if matches!(
            outcome,
            Outcome::Rejected(Rejection::Stale | Rejection::Refused(DeckRefusal::Outdated { .. }))
        ) {
            self.newest_epoch_pending = Some(adoption.caller);
            self.adopt_newest_epoch(out);
            return Some(Settled::Pending);
        }
        if matches!(outcome, Outcome::Applied { .. }) && adoption.segment == self.segment {
            self.repeat = false;
            if matches!(self.status, TrackStatus::Ended { .. }) {
                self.status = TrackStatus::Loaded;
            }
        }
        None
    }

    fn reject_attachment(
        &mut self,
        batch: &mut Batch<DeckProtocol>,
        out: &mut Outbox<'_, S>,
    ) -> bool {
        self.attaching = None;
        let returned = self.restore_attachment(&mut batch.commands);
        self.loading = None;
        self.attach_at = None;
        if self.releasing
            || !matches!(self.status, TrackStatus::Failed { .. })
                && self.preparation_failure().is_none()
        {
            self.status = TrackStatus::Released;
        }
        self.releasing = false;
        self.ready = None;
        self.play = None;
        self.playback_commands.clear();
        self.adopting.clear();
        self.newest_epoch_pending = None;
        if let Err(error) = self.release_lane(out) {
            warn!(%error, "refused attachment waits for lane release room");
        }
        returned
    }

    fn settle_parts(
        &mut self,
        batch: &Batch<DeckProtocol>,
        attaching: bool,
        transport_current: bool,
        at: SessionFrame,
        out: &mut Outbox<'_, S>,
    ) {
        for part in &batch.commands {
            match part {
                DeckPart::Start { slot, .. } | DeckPart::Chain { to: slot, .. }
                    if Some(*slot) == self.slot && transport_current =>
                {
                    self.status = TrackStatus::Playing { since: at };
                    self.resume = None;
                }
                DeckPart::Returned(Returned::Stopped { slot, resume })
                    if Some(*slot) == self.slot && transport_current =>
                {
                    self.stopped(*resume);
                }
                DeckPart::Returned(Returned::Pcm { slot, .. })
                    if Some(*slot) == self.slot && !attaching =>
                {
                    self.released(out);
                }
                _ => {}
            }
        }
    }

    fn stopped(&mut self, resume: Option<SlotMark>) {
        self.resume = resume.filter(|mark| mark.lane.segment == self.segment);
        if let Some(position) = self
            .resume
            .and_then(|mark| self.content_mark_position(mark))
        {
            self.position = position;
        }
        self.status = TrackStatus::Paused { at: self.position };
    }

    fn released(&mut self, out: &mut Outbox<'_, S>) {
        self.status = TrackStatus::Released;
        self.attach_at = None;
        self.releasing = false;
        self.mark = None;
        self.play = None;
        self.ready = None;
        self.playback_commands.clear();
        self.adopting.clear();
        self.newest_epoch_pending = None;
        if let Err(error) = self.release_lane(out) {
            warn!(%error, "lane release waits for dispatcher room");
        }
    }

    pub(super) fn adopt_newest_epoch(&mut self, out: &mut Outbox<'_, S>) {
        if self.preparation_closed() {
            self.newest_epoch_pending = None;
            return;
        }
        let Some(caller) = self.newest_epoch_pending else {
            return;
        };
        if out.deck_available() == 0 {
            return;
        }
        let Some(slot) = self.slot else {
            return;
        };
        match out.deck_owned(
            When::Next,
            vec![DeckPart::Adopt {
                slot,
                segment: self.segment,
            }],
        ) {
            Ok(Some(seq)) => {
                self.adopting.push(Adoption {
                    seq,
                    caller,
                    segment: self.segment,
                    parked: None,
                    cancelled: false,
                });
                self.newest_epoch_pending = None;
            }
            Ok(None) => {}
            Err((error, _parts)) => warn!(%error, "the latest segment's adoption waits for room"),
        }
    }

    pub(super) fn release_lane(&mut self, out: &mut Outbox<'_, S>) -> Result<(), PlayError> {
        if self.release.is_none()
            && let Some(lane) = self.lane_id
        {
            self.release = Some(out.release(lane)?);
        }
        Ok(())
    }
}

impl<S> PlayerImpl<S> {
    fn accept_ready(&mut self, seq: Seq, data: &LaneApplied) {
        self.engine_latency = data.engine_latency;
        if let Some(ready) = data.ready
            && !matches!(self.status, TrackStatus::Failed { .. })
        {
            self.ready = Some(ready);
            if ready == self.segment
                && let Some(basis) = data.source_basis
            {
                self.source_basis = Some(basis);
                kithara_test_macros::probe_event!(
                    lane_source_ready,
                    item = u64::from(self.item),
                    segment = ready.get(),
                    lane_seq = seq.get(),
                    source_revision = basis.source_revision,
                    source_first_frame = basis.frame_offset
                );
            }
        }
    }

    fn apply_lane(&mut self, seq: Seq, at: LaneFrame, data: &LaneApplied, segment: bool) {
        let operations = self
            .lane_commands
            .iter_mut()
            .filter(|operation| operation.seq == seq);
        let mut speed_session = None;
        for operation in operations {
            tracing::debug!(
                item = u64::from(self.item),
                seq = seq.get(),
                actual_apply_segment = at.segment.get(),
                actual_apply_frame = at.frame,
                current_segment = self.segment.get(),
                requested_when = ?operation.when,
                paused_resume = ?self.resume,
                mark = ?self.mark,
                ready_segment = ?data.ready,
                command = ?operation.command,
                "lane command settled"
            );
            let session = match operation.when {
                When::At(requested) if requested == at => operation.session,
                _ => self.mark.and_then(|mark| session_at(mark, at)),
            };
            operation.when = When::At(at);
            operation.verdict = Some(Ok(()));
            if matches!(&operation.command, LaneCommand::SetSpeed(_)) {
                speed_session = Some(session);
            }
            if let LaneCommand::Segment { id, from, .. } = operation.command
                && data.ready == Some(id)
                && id == self.segment
                && !matches!(self.status, TrackStatus::Playing { .. })
                && let Some(origin) = self.media_origin
            {
                self.position = from.saturating_sub(origin);
                if matches!(self.status, TrackStatus::Paused { .. }) {
                    self.status = TrackStatus::Paused { at: self.position };
                }
            }
        }
        if let Some(session) = speed_session
            && !segment
        {
            self.speed_answers.push_back((seq, Ok((at, session))));
        }
        self.accept_ready(seq, data);
    }

    pub(super) fn settle_lane(&mut self) {
        if self.lane.is_none() {
            return;
        }
        while let Some(receipt) = self.lane.as_mut().and_then(|lane| lane.receipts().next()) {
            self.settings.settle(&receipt);
            let segment = receipt
                .batch()
                .commands
                .iter()
                .any(|command| matches!(command, LaneCommand::Segment { .. }));
            if let Outcome::Applied { at, data } = receipt.outcome() {
                self.apply_lane(receipt.seq(), *at, data, segment);
            } else if let Outcome::Rejected(reason) = receipt.outcome() {
                let mut speed = false;
                for operation in self
                    .lane_commands
                    .iter_mut()
                    .filter(|operation| operation.seq == receipt.seq())
                {
                    operation.verdict = Some(Err(*reason));
                    speed |= matches!(operation.command, LaneCommand::SetSpeed(_));
                }
                if speed && !segment {
                    self.speed_answers.push_back((
                        receipt.seq(),
                        Err(rejection(reason, |failure| PlayError::Lane(*failure))),
                    ));
                }
                warn!(?reason, "lane command was rejected");
            }
        }
        self.reserved_ready();
    }
}
