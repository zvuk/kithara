use std::{marker::PhantomData, num::NonZeroU32};

use kithara_command::{Rejection, Seq, When};
use kithara_host::{
    DeckId, HostCommand, HostOwner, HostSettingsChange, HostSettingsExec, HostSettled, api::Tempo,
};
use kithara_play::{DeckPass, Outbox, PlayError, SessionTransportSnapshot};
use kithara_signal::{FrameCount, SessionEpoch, SessionFrame};

use crate::{GridAnswer, LinkedDeck, TempoTrajectory};

/// A tempo batch whose projected step is retained until the Host answers it.
#[derive(Clone, Copy, Debug)]
pub struct PendingTempo {
    pub seq: Seq,
    pub frame: SessionFrame,
    pub value: Tempo,
    pub at: When<SessionFrame>,
}

struct TempoOperation {
    pending: PendingTempo,
    caller: Seq,
    epoch: Option<SessionEpoch>,
    retry: bool,
}

/// Owner commands for a linked Host and the decks it holds.
pub enum LinkedHostCommand<S> {
    /// A Host command: registration and tempo go through the decorator,
    /// the rest to the owner it wraps.
    Host(HostCommand<S, dyn LinkedDeck<S>>),
    Sync {
        deck: DeckId,
        on: bool,
    },
    Grid {
        deck: DeckId,
        answer: GridAnswer,
    },
}

impl<S> From<HostCommand<S, dyn LinkedDeck<S>>> for LinkedHostCommand<S> {
    fn from(command: HostCommand<S, dyn LinkedDeck<S>>) -> Self {
        Self::Host(command)
    }
}

/// Decorates the session owner with projected tempo and synchronized deck retiming.
pub struct LinkedHost<S, H> {
    inner: H,
    trajectory: TempoTrajectory,
    tempo: Vec<TempoOperation>,
    answers: Vec<HostSettled>,
    settled: Vec<HostSettled>,
    epoch: Option<SessionEpoch>,
    schema: PhantomData<fn() -> S>,
}

impl<S, H> LinkedHost<S, H> {
    /// Starts with the Host's configured tempo anchor and no tempo batches in flight.
    #[must_use]
    pub fn new(inner: H, trajectory: TempoTrajectory) -> Self {
        Self {
            inner,
            trajectory,
            tempo: Vec::new(),
            answers: Vec::new(),
            settled: Vec::new(),
            epoch: None,
            schema: PhantomData,
        }
    }
}

impl<S: 'static, H: HostOwner<S, Deck = dyn LinkedDeck<S>>> LinkedHost<S, H> {
    fn observe_axis(&mut self) {
        let Some(transport) = self.inner.transport() else {
            return;
        };
        let epoch = transport.session_epoch();
        if self.epoch == Some(epoch) {
            return;
        }
        self.trajectory
            .reaxis_observed(transport.anchor(), transport.tempo());
        self.epoch = Some(epoch);
        if let Some((now, delivery)) = self.inner.clock() {
            let at = now + self.lead(delivery);
            let trajectory = &self.trajectory;
            self.inner.each_deck(&mut |_id, deck, out, _pass| {
                deck.realign(trajectory, at, out);
            });
        }
    }

    fn lead(&mut self, delivery: FrameCount) -> FrameCount {
        let mut lead = delivery;
        self.inner.each_deck(&mut |_id, deck, _out, _pass| {
            if deck.synced()
                && let Some(lane) = deck.lead(delivery)
            {
                lead = lead.max(lane);
            }
        });
        lead
    }

    fn retime(&mut self, frame: SessionFrame) {
        let trajectory = &self.trajectory;
        self.inner.each_deck(&mut |_id, deck, out, _pass| {
            if deck.synced() {
                deck.retime(trajectory, frame, out);
            }
        });
    }

    fn admit_tempo(
        &mut self,
        now: SessionFrame,
        delivery: FrameCount,
        at: When<SessionFrame>,
    ) -> Result<SessionFrame, PlayError> {
        let bound = now + self.lead(delivery);
        let frame = match at {
            When::Next => bound,
            When::At(frame) if frame < bound => return Err(PlayError::Late),
            When::At(frame) => frame,
            When::Deferred => {
                return Err(PlayError::Internal(
                    "a tempo has no deferred executor moment".into(),
                ));
            }
        };
        if self.inner.host_room() == 0 {
            return Err(PlayError::Full("host"));
        }
        let mut lanes_full = false;
        let mut scopes_full = false;
        self.inner.each_deck(&mut |_id, deck, out, _pass| {
            if deck.synced() && deck.lane_room() == 0 {
                lanes_full = true;
            }
            if out.deck_available() < deck.scope_parts() {
                scopes_full = true;
            }
        });
        if lanes_full {
            return Err(PlayError::Full("lane"));
        }
        if scopes_full {
            return Err(PlayError::Full("deck"));
        }
        Ok(frame)
    }

    fn retry_tempo(&mut self, operation: &mut TempoOperation) -> Result<bool, PlayError> {
        let Some((now, delivery)) = self.inner.clock() else {
            return Ok(false);
        };
        let frame = match self.admit_tempo(now, delivery, When::Next) {
            Ok(frame) => frame,
            Err(PlayError::Full(_)) => return Ok(false),
            Err(error) => return Err(error),
        };
        let mut trajectory = self.trajectory.clone();
        if operation.epoch == self.epoch {
            trajectory.withdraw(operation.pending.frame);
        }
        trajectory
            .push(frame, operation.pending.value)
            .map_err(|error| PlayError::Internal(error.to_string()))?;
        let Some(seq) = self
            .inner
            .exec_tempo(operation.pending.value, When::At(frame), &mut ())?
        else {
            return Err(PlayError::Internal(
                "a timed tempo resend did not produce an executor receipt".into(),
            ));
        };
        self.trajectory = trajectory;
        operation.pending.seq = seq;
        operation.pending.frame = frame;
        operation.epoch = self.epoch;
        operation.retry = false;
        self.retime(frame);
        Ok(true)
    }

    fn settle_tempos(&mut self, settled: Vec<HostSettled>) -> Vec<HostSettled> {
        let mut answers: Vec<HostSettled> = Vec::with_capacity(settled.len());
        for answer in settled {
            let (seq, value, outcome) = match answer {
                HostSettled::Settings {
                    seq,
                    change: HostSettingsChange::Tempo(value),
                    outcome,
                } => (seq, value, outcome),
                answer => {
                    answers.push(answer);
                    continue;
                }
            };
            let Some(index) = self
                .tempo
                .iter()
                .position(|operation| operation.pending.seq == seq && !operation.retry)
            else {
                answers.push(HostSettled::Settings {
                    seq,
                    change: HostSettingsChange::Tempo(value),
                    outcome,
                });
                continue;
            };
            let mut operation = self.tempo.remove(index);
            if matches!(&outcome, Err(Rejection::Late | Rejection::Refused(_))) {
                if operation.epoch == self.epoch {
                    self.trajectory.withdraw(operation.pending.frame);
                }
                let newer = self.tempo[index..]
                    .iter()
                    .any(|pending| pending.pending.at == When::Next);
                if operation.pending.at == When::Next && !newer {
                    operation.retry = true;
                    self.tempo.insert(index, operation);
                    continue;
                }
                if let Some((now, delivery)) = self.inner.clock() {
                    let frame = now + self.lead(delivery);
                    self.retime(frame);
                }
            }
            answers.push(HostSettled::Settings {
                seq: operation.caller,
                change: HostSettingsChange::Tempo(value),
                outcome,
            });
        }
        answers
    }

    fn retry_tempos(&mut self) {
        let mut index = 0;
        while index < self.tempo.len() {
            if !self.tempo[index].retry {
                index += 1;
                continue;
            }
            let mut operation = self.tempo.remove(index);
            if let Err(error) = self.retry_tempo(&mut operation) {
                tracing::warn!(%error, seq = ?operation.caller, "tempo replan refused");
                self.answers.push(HostSettled::Settings {
                    seq: operation.caller,
                    change: HostSettingsChange::Tempo(operation.pending.value),
                    outcome: Err(Rejection::Refused(error)),
                });
                continue;
            }
            self.tempo.insert(index, operation);
            index += 1;
        }
    }
}

impl<S: 'static, H: HostOwner<S, Deck = dyn LinkedDeck<S>>> HostOwner<S> for LinkedHost<S, H> {
    type Command = LinkedHostCommand<S>;
    type Deck = dyn LinkedDeck<S>;

    fn apply(&mut self, command: Self::Command) -> Result<Option<Seq>, PlayError> {
        match command {
            LinkedHostCommand::Host(HostCommand::Register { id, deck }) => {
                self.register(id, deck).map(|()| None)
            }
            LinkedHostCommand::Host(HostCommand::Configure(change, at)) => {
                self.exec(change, at, &mut ())
            }
            LinkedHostCommand::Host(command) => self.inner.apply(H::Command::from(command)),
            LinkedHostCommand::Sync { deck, on } => {
                let mut sent = Ok(None);
                self.inner.with_deck(deck, &mut |deck, out, _pass| {
                    sent = deck.sync(on, out);
                })?;
                sent
            }
            LinkedHostCommand::Grid { deck, answer } => {
                self.inner.with_deck(deck, &mut |deck, out, _pass| {
                    deck.grid(answer.clone(), out);
                })?;
                Ok(None)
            }
        }
    }

    fn register(&mut self, id: DeckId, deck: Box<Self::Deck>) -> Result<(), PlayError> {
        self.inner.register(id, deck)?;
        let trajectory = &self.trajectory;
        self.inner.with_deck(id, &mut |deck, out, pass| {
            deck.retime(trajectory, pass.now, out);
        })
    }

    delegate::delegate! {
        to self.inner {
            fn each_deck(
                &mut self,
                visit: &mut dyn FnMut(DeckId, &mut Self::Deck, &mut Outbox<'_, S>, DeckPass<'_>),
            );
            fn with_deck(
                &mut self,
                id: DeckId,
                visit: &mut dyn FnMut(&mut Self::Deck, &mut Outbox<'_, S>, DeckPass<'_>),
            ) -> Result<(), PlayError>;
            fn clock(&self) -> Option<(SessionFrame, FrameCount)>;
            fn prepare_offline(&mut self) -> Result<(), PlayError>;
            fn render_offline(
                &mut self,
                position: u64,
                frames: usize,
                output: &mut [f32],
            ) -> Result<(), PlayError>;
            fn transport(&mut self) -> Option<SessionTransportSnapshot>;
            fn host_room(&self) -> usize;
        }
    }

    fn begin_pass(&mut self) {
        self.inner.begin_pass();
        self.observe_axis();
        let settled = std::mem::take(&mut self.settled);
        let answers = self.settle_tempos(settled);
        self.answers.extend(answers);
        self.retry_tempos();
    }

    fn release_id(command: &Self::Command) -> Option<DeckId> {
        match command {
            LinkedHostCommand::Host(HostCommand::Release(deck)) => Some(*deck),
            _ => None,
        }
    }

    fn is_next_tempo(command: &Self::Command) -> bool {
        matches!(
            command,
            LinkedHostCommand::Host(HostCommand::Configure(
                HostSettingsChange::Tempo(_),
                When::Next,
            ))
        )
    }

    fn pass(&mut self) -> Vec<HostSettled> {
        let settled = self.inner.pass();
        let mut answers = std::mem::take(&mut self.answers);
        for answer in settled {
            if matches!(&answer, HostSettled::Settings {
                seq, change: HostSettingsChange::Tempo(_), ..
            } if self.tempo.iter().any(|operation| operation.pending.seq == *seq && !operation.retry))
            {
                self.settled.push(answer);
            } else {
                answers.push(answer);
            }
        }
        answers
    }
}

impl<S: 'static, H: HostOwner<S, Deck = dyn LinkedDeck<S>>> HostSettingsExec<()>
    for LinkedHost<S, H>
{
    type At = When<SessionFrame>;
    type Output = Result<Option<Seq>, PlayError>;

    delegate::delegate! {
        to self.inner {
            fn exec_sample_rate(&mut self, value: NonZeroU32, at: Self::At, cx: &mut ()) -> Self::Output;
            fn exec_live(&mut self, change: HostSettingsChange, at: Self::At, cx: &mut ()) -> Self::Output;
        }
    }

    fn exec_tempo(&mut self, value: Tempo, at: Self::At, cx: &mut ()) -> Self::Output {
        if at == When::Deferred {
            return Err(PlayError::Internal(
                "a tempo has no deferred executor moment".into(),
            ));
        }
        let Some((now, delivery)) = self.inner.clock() else {
            if matches!(at, When::At(_)) {
                return Err(PlayError::Untimed);
            }
            let sent = self.inner.exec_tempo(value, When::Next, cx)?;
            self.trajectory.initial_tempo(value);
            return Ok(sent);
        };
        let frame = self.admit_tempo(now, delivery, at)?;
        let mut trajectory = self.trajectory.clone();
        trajectory
            .push(frame, value)
            .map_err(|error| PlayError::Internal(error.to_string()))?;
        let seq = self
            .inner
            .exec_tempo(value, When::At(frame), cx)?
            .ok_or_else(|| {
                PlayError::Internal("a timed tempo did not produce an executor receipt".into())
            })?;
        self.trajectory = trajectory;
        self.retime(frame);
        self.tempo.push(TempoOperation {
            pending: PendingTempo {
                seq,
                frame,
                value,
                at,
            },
            caller: seq,
            epoch: self.epoch,
            retry: false,
        });
        Ok(Some(seq))
    }
}
