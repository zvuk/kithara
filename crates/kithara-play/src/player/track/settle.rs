use std::num::NonZeroU32;

use kithara_command::{Batch, Outcome, Rejection, Seq, When};
use kithara_render::{
    LaneCommand,
    bridge::{DeckPart, DeckProtocol, DeckRefusal, Returned},
};
use kithara_warp::SpeedCurve;
use tracing::warn;

use super::{
    super::{
        outbox::{Outbox, Settled, rejection},
        settings::{TrackSettingsChange, check_speed},
    },
    core::PlayerImpl,
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
        self.segment_with(from, rate, self.settings.projected().speed(), out)
    }

    pub(super) fn segment_with(
        &mut self,
        from: Position,
        rate: Option<NonZeroU32>,
        speed: f32,
        out: &mut Outbox<'_, S>,
    ) -> Result<Option<Seq>, PlayError> {
        let speed = check_speed(speed)?;
        let change = TrackSettingsChange::Speed(speed);
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
        let segment = self.issued_segment.next();
        let command = rate.map_or(
            LaneCommand::Segment {
                id: segment,
                from,
                speed: SpeedCurve::Constant(speed),
            },
            |rate| LaneCommand::SetHostRate { id: segment, rate },
        );
        let seq = self.send_lane(command, When::Next)?;
        self.issued_segment = segment;
        if rate.is_none() {
            self.settings.track(seq, When::Next, change);
        }
        self.segment = segment;
        self.playback_commands.clear();
        self.segment_speed = speed;
        self.ready = None;
        self.mark = None;
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

    pub(super) fn applied(
        &mut self,
        seq: Seq,
        outcome: &Outcome<DeckProtocol>,
        batch: &mut Batch<DeckProtocol>,
        out: &mut Outbox<'_, S>,
    ) -> Settled {
        let attaching = self.attaching.filter(|attach| attach.seq == Some(seq));
        let mut answered = attaching.map_or(seq, |attach| attach.caller);
        if let Some(index) = self.adopting.iter().position(|adopt| adopt.seq == seq) {
            let adoption = self.adopting.remove(index);
            answered = adoption.caller;
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
                        return Settled::Rejected {
                            seq: answered,
                            reason: Rejection::Refused(error),
                        };
                    }
                    self.segment = segment;
                    self.ready = None;
                }
                self.repeat = false;
                return Settled::Pending;
            }
            if matches!(
                outcome,
                Outcome::Rejected(
                    Rejection::Stale | Rejection::Refused(DeckRefusal::Outdated { .. })
                )
            ) {
                self.adopt_retry = Some(adoption.caller);
                self.retry_adopt(out);
                return Settled::Pending;
            }
            if matches!(outcome, Outcome::Applied { .. }) && adoption.segment == self.segment {
                self.repeat = false;
            }
        }
        let at = match outcome {
            Outcome::Applied { at, .. } => *at,
            Outcome::Rejected(reason) => {
                if attaching.is_some() {
                    self.attaching = None;
                    let returned = self.restore_attachment(&mut batch.commands);
                    self.loading = None;
                    self.status = TrackStatus::Released;
                    self.ready = None;
                    self.play = None;
                    self.playback_commands.clear();
                    self.adopting.clear();
                    self.adopt_retry = None;
                    if let Err(error) = self.release_lane(out) {
                        warn!(%error, "refused attachment waits for lane release room");
                    }
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
            self.status = TrackStatus::Loaded;
        }
        for part in &batch.commands {
            match part {
                DeckPart::Start { slot, .. } | DeckPart::Chain { to: slot, .. }
                    if Some(*slot) == self.slot =>
                {
                    self.status = TrackStatus::Playing { since: at };
                    self.resume = None;
                }
                DeckPart::Returned(Returned::Stopped { slot, resume })
                    if Some(*slot) == self.slot =>
                {
                    self.position = resume.position;
                    self.resume = Some(*resume);
                    self.status = TrackStatus::Paused { at: self.position };
                }
                DeckPart::Returned(Returned::Pcm { slot, .. })
                    if Some(*slot) == self.slot && attaching.is_none() =>
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
                _ => {}
            }
        }
        Settled::Applied { seq: answered, at }
    }

    pub(super) fn retry_adopt(&mut self, out: &mut Outbox<'_, S>) {
        let Some(caller) = self.adopt_retry else {
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
                self.adopt_retry = None;
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
