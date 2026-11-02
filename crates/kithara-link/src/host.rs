use std::{marker::PhantomData, num::NonZeroU32};

use kithara_command::{Rejection, Seq, When};
use kithara_host::{
    DeckId, HostCommand, HostOwner, HostRegistration, HostSettingsChange, HostSettingsExec,
    HostSettled, api::Tempo,
};
use kithara_play::{
    DeckControl, DeckPass, HostedDeck, Outbox, PlayError, SessionTransportSnapshot,
};
use kithara_signal::{FrameCount, SessionEpoch, SessionFrame};

use crate::{GridAnswer, LinkedDeck, LinkedHostConfig, LinkedHostDeck, TempoTrajectory, Unlinked};

/// Where a tempo stands between its admission and its answer.
#[derive(Clone, Copy)]
enum TempoState {
    /// Its step is on the trajectory until `seq` is answered.
    Sent {
        seq: Seq,
        frame: SessionFrame,
        epoch: Option<SessionEpoch>,
    },
    /// Waiting for room, with no step on the trajectory.
    Parked,
    /// A newer tempo withdrew its step; `seq` is still unanswered.
    Replaced { seq: Seq },
}

struct TempoOperation {
    caller: Seq,
    value: Tempo,
    at: When<SessionFrame>,
    state: TempoState,
}

impl TempoOperation {
    fn answers(&self, answer: Seq) -> bool {
        matches!(
            self.state,
            TempoState::Sent { seq, .. } | TempoState::Replaced { seq } if seq == answer
        )
    }

    /// The step of a `When::Next` tempo that the commit bound has not passed.
    fn unreached_step(
        &self,
        bound: SessionFrame,
        axis: Option<SessionEpoch>,
    ) -> Option<SessionFrame> {
        match self.state {
            TempoState::Sent { frame, epoch, .. }
                if self.at == When::Next && epoch == axis && frame >= bound =>
            {
                Some(frame)
            }
            _ => None,
        }
    }

    fn settled(&self, outcome: Result<SessionFrame, Rejection<PlayError>>) -> HostSettled {
        HostSettled::Settings {
            seq: self.caller,
            change: HostSettingsChange::Tempo(self.value),
            outcome,
        }
    }
}

fn superseded() -> Result<SessionFrame, Rejection<PlayError>> {
    Err(Rejection::Refused(PlayError::Superseded))
}

/// Owner commands for a linked Host and the decks it holds.
pub enum LinkedHostCommand<S> {
    /// A Host command: registration and tempo go through the decorator,
    /// the rest to the owner it wraps.
    Host(HostCommand<S, LinkedHostDeck<S>>),
    Sync {
        deck: DeckId,
        on: bool,
    },
    Grid {
        deck: DeckId,
        answer: GridAnswer,
    },
}

impl<S> From<HostCommand<S, LinkedHostDeck<S>>> for LinkedHostCommand<S> {
    fn from(command: HostCommand<S, LinkedHostDeck<S>>) -> Self {
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
    pub fn new(config: LinkedHostConfig<H>) -> Self {
        let LinkedHostConfig { inner, trajectory } = config;
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

impl<S: 'static, H: HostOwner<S, Deck = LinkedHostDeck<S>>> LinkedHost<S, H> {
    fn observe_axis(&mut self) {
        let Some(transport) = self.inner.transport() else {
            return;
        };
        let epoch = transport.session_epoch();
        if self.epoch == Some(epoch) {
            return;
        }
        self.trajectory.reaxis_observed(transport.anchor());
        self.epoch = Some(epoch);
        if let Some((now, delivery)) = self.inner.clock() {
            let at = now + self.lead(delivery);
            let trajectory = &self.trajectory;
            self.inner.each_deck(&mut |_id, deck, out, _pass| {
                if let LinkedHostDeck::Linked(deck) = deck {
                    deck.realign(trajectory, at, out);
                }
            });
        }
    }

    fn lead(&mut self, delivery: FrameCount) -> FrameCount {
        let mut lead = delivery;
        self.inner.each_deck(&mut |_id, deck, _out, _pass| {
            if let LinkedHostDeck::Linked(deck) = deck
                && deck.synced()
                && let Some(lane) = deck.lead(delivery)
            {
                lead = lead.max(lane);
            }
        });
        lead
    }

    fn retime(&mut self, frame: SessionFrame) {
        let trajectory = &self.trajectory;
        let frames: Vec<_> = std::iter::once(frame)
            .chain(trajectory.changes_after(frame))
            .collect();
        self.inner.each_deck(&mut |_id, deck, out, _pass| {
            if let LinkedHostDeck::Linked(deck) = deck
                && deck.synced()
            {
                for &frame in &frames {
                    deck.retime(trajectory, frame, out);
                }
            }
        });
    }

    fn admit_tempo(
        &mut self,
        bound: SessionFrame,
        at: When<SessionFrame>,
    ) -> Result<SessionFrame, PlayError> {
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
            if let LinkedHostDeck::Linked(deck) = deck {
                if deck.synced() && deck.lane_room() == 0 {
                    lanes_full = true;
                }
                if out.deck_available() < deck.scope_parts() {
                    scopes_full = true;
                }
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

    /// Puts `value` on the trajectory at `frame`, in place of the `replaced`
    /// step on that frame, and sends it to the Host.
    fn send_step(
        &mut self,
        value: Tempo,
        frame: SessionFrame,
        replaced: Option<SessionFrame>,
        cx: &mut (),
    ) -> Result<Seq, PlayError> {
        let mut trajectory = self.trajectory.clone();
        if let Some(step) = replaced {
            trajectory.withdraw(step);
        }
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
        Ok(seq)
    }

    fn reschedule_tempo(&mut self, index: usize) -> Result<(), PlayError> {
        let Some((now, delivery)) = self.inner.clock() else {
            return Ok(());
        };
        let bound = now + self.lead(delivery);
        let frame = match self.admit_tempo(bound, When::Next) {
            Ok(frame) => frame,
            Err(PlayError::Full(_)) => return Ok(()),
            Err(error) => return Err(error),
        };
        let seq = self.send_step(self.tempo[index].value, frame, None, &mut ())?;
        self.tempo[index].state = TempoState::Sent {
            seq,
            frame,
            epoch: self.epoch,
        };
        Ok(())
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
            let Some(index) = self.tempo.iter().position(|op| op.answers(seq)) else {
                answers.push(HostSettled::Settings {
                    seq,
                    change: HostSettingsChange::Tempo(value),
                    outcome,
                });
                continue;
            };
            let outcome = match self.tempo[index].state {
                TempoState::Replaced { .. } => superseded(),
                TempoState::Sent { frame, epoch, .. }
                    if matches!(&outcome, Err(Rejection::Late | Rejection::Refused(_))) =>
                {
                    if epoch == self.epoch {
                        self.trajectory.withdraw(frame);
                    }
                    let for_room = self.tempo[index].at == When::Next
                        && matches!(
                            &outcome,
                            Err(Rejection::Late | Rejection::Refused(PlayError::Full(_)))
                        );
                    if for_room && index + 1 == self.tempo.len() {
                        self.tempo[index].state = TempoState::Parked;
                        continue;
                    }
                    if let Some((now, delivery)) = self.inner.clock() {
                        let frame = now + self.lead(delivery);
                        self.retime(frame);
                    }
                    if for_room { superseded() } else { outcome }
                }
                _ => outcome,
            };
            let operation = self.tempo.remove(index);
            answers.push(operation.settled(outcome));
        }
        answers
    }

    /// Admission retires a parked tempo, so at most one waits, and it is the newest.
    fn resume_tempos(&mut self) {
        let Some(index) = self
            .tempo
            .iter()
            .position(|op| matches!(op.state, TempoState::Parked))
        else {
            return;
        };
        if let Err(error) = self.reschedule_tempo(index) {
            let operation = self.tempo.remove(index);
            tracing::warn!(%error, seq = ?operation.caller, "tempo replan refused");
            self.answers
                .push(operation.settled(Err(Rejection::Refused(error))));
        }
    }
}

impl<S, H, D> HostRegistration<S, D, dyn LinkedDeck<S>> for LinkedHost<S, H>
where
    S: 'static,
    H: HostOwner<S, Deck = LinkedHostDeck<S>>,
    D: LinkedDeck<S> + DeckControl,
{
    fn into_deck(deck: D) -> Box<Self::Deck> {
        Box::new(LinkedHostDeck::Linked(Box::new(deck)))
    }
}

impl<S, H, D> HostRegistration<S, Unlinked<D>, dyn HostedDeck<S>> for LinkedHost<S, H>
where
    S: 'static,
    H: HostOwner<S, Deck = LinkedHostDeck<S>>,
    D: HostedDeck<S> + DeckControl,
{
    fn into_deck(deck: Unlinked<D>) -> Box<Self::Deck> {
        Box::new(LinkedHostDeck::Ordinary(Box::new(deck.0)))
    }
}

impl<S: 'static, H: HostOwner<S, Deck = LinkedHostDeck<S>>> HostOwner<S> for LinkedHost<S, H> {
    type Command = LinkedHostCommand<S>;
    type Deck = LinkedHostDeck<S>;

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
                let trajectory = &self.trajectory;
                self.inner.with_deck(deck, &mut |deck, out, pass| {
                    sent = match deck {
                        LinkedHostDeck::Linked(deck) => {
                            if on && !deck.synced() {
                                deck.retime(trajectory, pass.now, out);
                            }
                            deck.sync(on, out)
                        }
                        LinkedHostDeck::Ordinary(_) => Err(PlayError::InvalidParameter {
                            name: "sync".into(),
                            value: if on { 1.0 } else { 0.0 },
                        }),
                    };
                })?;
                sent
            }
            LinkedHostCommand::Grid { deck, answer } => {
                let mut delivered = Ok(None);
                self.inner
                    .with_deck(deck, &mut |deck, out, _pass| match deck {
                        LinkedHostDeck::Linked(deck) => deck.grid(answer.clone(), out),
                        LinkedHostDeck::Ordinary(_) => {
                            delivered = Err(PlayError::InvalidParameter {
                                name: "grid".into(),
                                value: 0.0,
                            });
                        }
                    })?;
                delivered
            }
        }
    }

    fn register(&mut self, id: DeckId, deck: Box<Self::Deck>) -> Result<(), PlayError> {
        let linked = matches!(&*deck, LinkedHostDeck::Linked(_));
        self.inner.register(id, deck)?;
        if !linked {
            return Ok(());
        }
        let trajectory = &self.trajectory;
        self.inner.with_deck(id, &mut |deck, out, pass| {
            if let LinkedHostDeck::Linked(deck) = deck {
                deck.retime(trajectory, pass.now, out);
            }
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
        self.resume_tempos();
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
            } if self.tempo.iter().any(|op| op.answers(*seq)))
            {
                self.settled.push(answer);
            } else {
                answers.push(answer);
            }
        }
        answers
    }
}

impl<S: 'static, H: HostOwner<S, Deck = LinkedHostDeck<S>>> HostSettingsExec<()>
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
        let bound = now + self.lead(delivery);
        let planned = self.admit_tempo(bound, at)?;
        let axis = self.epoch;
        let replaced = self
            .tempo
            .iter()
            .rev()
            .filter_map(|op| op.unreached_step(bound, axis))
            .find(|&step| at == When::Next || step == planned);
        let frame = replaced.unwrap_or(planned);
        let seq = self.send_step(value, frame, replaced, cx)?;
        let answers = &mut self.answers;
        self.tempo.retain_mut(|op| match op.state {
            TempoState::Parked => {
                answers.push(op.settled(superseded()));
                false
            }
            TempoState::Sent { seq, .. }
                if replaced.is_some() && op.unreached_step(bound, axis) == replaced =>
            {
                op.state = TempoState::Replaced { seq };
                true
            }
            TempoState::Sent { .. } | TempoState::Replaced { .. } => true,
        });
        self.tempo.push(TempoOperation {
            caller: seq,
            value,
            at,
            state: TempoState::Sent {
                seq,
                frame,
                epoch: self.epoch,
            },
        });
        Ok(Some(seq))
    }
}
