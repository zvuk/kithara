use kithara_command::{
    Batch, Live, LiveError, Outcome, Port, Receipt, Rejection, SendError, Sender, Seq, When,
};
use kithara_render::{
    DispatcherCommand, DispatcherProtocol, LaneId, LoadRequest, ServiceClass,
    bridge::{DeckEvent, DeckPart, DeckProtocol, Slot},
};
use kithara_signal::SessionFrame;
pub use kithara_sync::Bound;

use crate::{
    DeckEqChange, DeckMixSettings, DeckMixSettingsChange, DeckPass, InterruptionKind, PlayError,
    ResourceLoad, SessionEvent,
};

/// One loaded track or a deck built of them: it changes its own state on the
/// owner's thread and reaches the executors only through the [`Outbox`] it is
/// lent.
pub trait Player<S> {
    /// What the player is told to do.
    type Command;
    /// What the player shows of itself.
    type Snapshot;

    /// The proven entry nearest to `bound` on its requested side, if known.
    fn entry(&self, bound: Bound) -> Option<SessionFrame>;

    /// Applies `command` and returns the number of the batch it became, if one
    /// went out on its own.
    ///
    /// # Errors
    ///
    /// Returns why nothing was sent: a refused change, or a queue with no
    /// room.
    fn apply(
        &mut self,
        command: Self::Command,
        out: &mut Outbox<'_, S>,
    ) -> Result<Option<Seq>, PlayError>;

    /// Takes what an executor answered: what the player holds from it moves in,
    /// and the outcome goes up.
    fn settle(&mut self, receipt: TrackReceipt<'_, S>, out: &mut Outbox<'_, S>) -> Settled;

    /// One step of session time: deadlines and the receipts of the player's
    /// own lane.
    fn tick(&mut self, now: SessionFrame, out: &mut Outbox<'_, S>);

    fn snapshot(&self) -> Self::Snapshot;
}

/// The queues a player sends to: the mixer of its deck and the dispatcher that
/// opens sources. The owner lends it for one pass.
///
/// Inside [`Outbox::together`] every deck part goes into one batch, so the
/// halves of a transition two players send apply on one frame or not at all.
pub struct Outbox<'a, S> {
    deck: &'a mut dyn Port<DeckProtocol>,
    dispatcher: &'a mut Sender<DispatcherProtocol<ResourceLoad<S>>>,
    group: Option<Group>,
    pass: Option<DeckPass<'a>>,
    dispatches: Option<&'a mut Vec<Seq>>,
    mix: Option<&'a mut Live<DeckMixSettings, DeckProtocol>>,
    session: Option<(&'a mut Option<u64>, &'a kithara_events::EventBus, u64)>,
}

/// The batch [`Outbox::together`] is collecting.
struct Group {
    at: When<SessionFrame>,
    basis: Vec<(Slot, Option<Seq>)>,
    parts: Vec<DeckPart>,
}

type GroupResult<R> = Result<(R, Option<Seq>), (PlayError, Vec<DeckPart>)>;

impl<'a, S> Outbox<'a, S> {
    #[must_use]
    pub fn new(
        deck: &'a mut dyn Port<DeckProtocol>,
        dispatcher: &'a mut Sender<DispatcherProtocol<ResourceLoad<S>>>,
    ) -> Self {
        Self {
            deck,
            dispatcher,
            group: None,
            pass: None,
            dispatches: None,
            mix: None,
            session: None,
        }
    }

    /// Borrows the observations and clock of the owner's current iteration.
    #[must_use]
    pub fn in_pass(mut self, pass: DeckPass<'a>) -> Self {
        self.pass = Some(pass);
        self
    }

    /// Records dispatcher batches in the deck record that owns their answers.
    pub fn track_dispatches(mut self, dispatches: &'a mut Vec<Seq>) -> Self {
        self.dispatches = Some(dispatches);
        self
    }

    pub fn lend_mix(mut self, mix: &'a mut Live<DeckMixSettings, DeckProtocol>) -> Self {
        self.mix = Some(mix);
        self
    }

    pub fn lend_session(
        mut self,
        suspended_at: &'a mut Option<u64>,
        bus: &'a kithara_events::EventBus,
        blocks: u64,
    ) -> Self {
        self.session = Some((suspended_at, bus, blocks));
        self
    }

    pub fn mix(
        &mut self,
        at: When<SessionFrame>,
        change: DeckMixSettingsChange,
    ) -> Result<Seq, PlayError> {
        self.check_when(at)?;
        let mix = self.mix.as_mut().ok_or_else(|| {
            PlayError::Internal("deck mix change outside its owner's pass".into())
        })?;
        mix.send(&mut *self.deck, at, change, DeckPart::Mix)
            .map_err(|error| match error {
                LiveError::Invalid(error) => PlayError::from(error),
                LiveError::Send(SendError::Full(_)) => PlayError::Full("deck"),
                LiveError::Send(SendError::Closed(_)) => PlayError::Closed,
                LiveError::Send(SendError::Target(_)) => {
                    PlayError::Internal("a deck mix batch names a slot".into())
                }
            })
    }

    pub fn eq(&mut self, parts: Vec<DeckEqChange>) -> Result<Option<Seq>, PlayError> {
        self.deck(When::Next, parts.into_iter().map(DeckPart::Eq).collect())
    }

    pub fn eq_layout(
        &mut self,
        pools: &kithara_bufpool::PoolRegion<S>,
        bands: &[crate::EqBandConfig],
        rate: std::num::NonZeroU32,
    ) -> Result<Option<Seq>, PlayError>
    where
        S: kithara_bufpool::HasPool<f32>,
    {
        let config = kithara_effects::eq::EqConfig::builder(pools.clone()).build();
        let layout = kithara_effects::eq::EqLayout::new(&config, bands, rate)?;
        self.eq(vec![DeckEqChange::Layout(Box::new(layout))])
    }

    pub fn notify_interruption(&mut self, kind: InterruptionKind) -> Result<bool, PlayError> {
        let (suspended_at, bus, blocks) = self.session.as_mut().ok_or(PlayError::NotReady)?;
        if matches!(kind, InterruptionKind::Began) {
            **suspended_at = Some(*blocks);
        }
        bus.publish(SessionEvent::Interruption { kind });
        Ok(suspended_at.is_some_and(|at| *blocks <= at.saturating_add(1)))
    }

    /// The current iteration's observations, when a host owns this outbox.
    #[must_use]
    pub fn pass(&self) -> Option<DeckPass<'_>> {
        self.pass
    }

    pub(crate) fn is_grouped(&self) -> bool {
        self.group.is_some()
    }

    delegate::delegate! {
        to self.deck {
            #[must_use]
            #[call(available)]
            pub fn deck_available(&self) -> usize;
            #[call(basis)]
            pub(crate) fn deck_basis(&self, slot: Slot, at: When<SessionFrame>) -> Option<Seq>;
        }
    }

    #[must_use]
    pub fn dispatcher_available(&self) -> usize {
        self.dispatcher.available()
    }

    fn check_when(&self, at: When<SessionFrame>) -> Result<(), PlayError> {
        if let When::At(frame) = at {
            let pass = self.pass.ok_or(PlayError::Untimed)?;
            if frame < pass.earliest() {
                return Err(PlayError::Late);
            }
        }
        Ok(())
    }

    /// Supersedes scheduled and parked batches without changing the slot's playback.
    pub(crate) fn supersede(&mut self, slot: Slot) -> Result<(), PlayError> {
        let basis = (slot, self.deck.basis(slot, When::Next));
        if let Some(group) = self.group.as_mut() {
            if group.at != When::Next {
                return Err(PlayError::Internal("supersession requires Next".into()));
            }
            if !group.basis.iter().any(|entry| entry.0 == slot) {
                group.basis.push(basis);
            }
            return Ok(());
        }
        deck_sent(self.deck.send(
            When::Next,
            Batch {
                basis: vec![basis],
                commands: Vec::new(),
            },
        ))
        .map(drop)
    }

    /// Runs `send` with every deck part it sends collected into one batch
    /// that applies at `at`, and sends that batch once `send` returns. A
    /// refusal from `send` sends nothing.
    ///
    /// # Errors
    ///
    /// Returns the refusal of `send`, or the deck's when it has no room for the
    /// batch; nothing was sent then.
    pub fn together<R, F>(
        &mut self,
        at: When<SessionFrame>,
        send: F,
    ) -> Result<(R, Option<Seq>), PlayError>
    where
        F: FnOnce(&mut Self) -> Result<R, PlayError>,
    {
        self.together_owned(at, send)
            .map_err(|(error, _parts)| error)
    }

    /// Collects one batch and returns its original parts if staging or sending fails.
    pub fn together_owned<R, F>(&mut self, at: When<SessionFrame>, send: F) -> GroupResult<R>
    where
        F: FnOnce(&mut Self) -> Result<R, PlayError>,
    {
        if self.group.is_some() || matches!(at, When::Deferred) {
            return Err((
                PlayError::Internal("a deferred or nested group is not a timed batch".into()),
                Vec::new(),
            ));
        }
        self.check_when(at).map_err(|error| (error, Vec::new()))?;
        if self.deck.available() == 0 {
            return Err((PlayError::Full("deck"), Vec::new()));
        }
        self.group = Some(Group {
            at,
            basis: Vec::new(),
            parts: Vec::new(),
        });
        let sent = send(self);
        let group = self.group.take();
        let value = match sent {
            Ok(value) => value,
            Err(error) => return Err((error, group.map_or_else(Vec::new, |group| group.parts))),
        };
        let Some(Group { at, basis, parts }) =
            group.filter(|group| !group.parts.is_empty() || !group.basis.is_empty())
        else {
            return Ok((value, None));
        };
        let seq = deck_sent_owned(self.deck.send(
            at,
            Batch {
                basis,
                commands: parts,
            },
        ))?;
        Ok((value, Some(seq)))
    }

    /// Sends `parts` to the deck to apply at `at`, each slot they name on the
    /// basis of the last batch sent for it; inside [`Outbox::together`] they
    /// join its batch and no number comes back.
    ///
    /// # Errors
    ///
    /// Returns [`PlayError::Full`] when the deck has no room, and
    /// [`PlayError::Internal`] for a part of a group at another moment.
    pub(crate) fn deck(
        &mut self,
        at: When<SessionFrame>,
        parts: Vec<DeckPart>,
    ) -> Result<Option<Seq>, PlayError> {
        self.deck_owned(at, parts).map_err(|(error, _parts)| error)
    }

    pub(crate) fn deck_owned(
        &mut self,
        at: When<SessionFrame>,
        parts: Vec<DeckPart>,
    ) -> Result<Option<Seq>, (PlayError, Vec<DeckPart>)> {
        if matches!(at, When::Deferred) {
            return Err((
                PlayError::Internal("deferred parts require an end-marker operation".into()),
                parts,
            ));
        }
        if let Err(error) = self.check_when(at) {
            return Err((error, parts));
        }
        let deck = &*self.deck;
        if let Some(group) = &mut self.group {
            if group.at != at {
                return Err((
                    PlayError::Internal(format!(
                        "a part at {at:?} joined a batch at {:?}",
                        group.at
                    )),
                    parts,
                ));
            }
            for slot in parts.iter().flat_map(slots) {
                if !group.basis.iter().any(|&(named, _)| named == slot) {
                    group.basis.push((slot, deck.basis(slot, at)));
                }
            }
            group.parts.extend(parts);
            return Ok(None);
        }
        let mut basis: Vec<(Slot, Option<Seq>)> = Vec::new();
        for slot in parts.iter().flat_map(slots) {
            if !basis.iter().any(|&(named, _)| named == slot) {
                basis.push((slot, deck.basis(slot, at)));
            }
        }
        deck_sent_owned(self.deck.send(
            at,
            Batch {
                basis,
                commands: parts,
            },
        ))
        .map(Some)
    }

    /// Asks the dispatcher to open `item`.
    ///
    /// # Errors
    ///
    /// Returns [`PlayError::Full`] when the dispatcher has no room.
    pub(crate) fn load(&mut self, request: LoadRequest<ResourceLoad<S>>) -> Result<Seq, PlayError> {
        self.dispatch(DispatcherCommand::Load(Box::new(request)))
    }

    pub(crate) fn release(&mut self, lane: LaneId) -> Result<Seq, PlayError> {
        self.dispatch(DispatcherCommand::Release(lane))
    }

    /// Asks the dispatcher to move a resident lane to another service class.
    pub(crate) fn prioritize(
        &mut self,
        lane: LaneId,
        class: ServiceClass,
    ) -> Result<Seq, PlayError> {
        self.dispatch(DispatcherCommand::SetPriority(lane, class))
    }

    fn dispatch(&mut self, command: DispatcherCommand<ResourceLoad<S>>) -> Result<Seq, PlayError> {
        let batch = Batch {
            basis: Vec::new(),
            commands: vec![command],
        };
        let seq = self
            .dispatcher
            .send(When::Next, batch)
            .map_err(|error| match error {
                SendError::Full(_) => PlayError::Full("dispatcher"),
                SendError::Target(_) | SendError::Closed(_) => PlayError::Closed,
            })?;
        if let Some(dispatches) = &mut self.dispatches {
            dispatches.push(seq);
        }
        Ok(seq)
    }

    pub fn chain(&mut self, from: Slot, to: Slot) -> Result<Seq, PlayError> {
        self.deferred(vec![DeckPart::Chain { from, to }])
    }

    pub(crate) fn deferred(&mut self, parts: Vec<DeckPart>) -> Result<Seq, PlayError> {
        if self.group.is_some() {
            return Err(PlayError::Internal(
                "an end-marker operation cannot join a timed batch".into(),
            ));
        }
        let mut basis: Vec<(Slot, Option<Seq>)> = Vec::new();
        for slot in parts.iter().flat_map(slots) {
            if !basis.iter().any(|&(named, _)| named == slot) {
                basis.push((slot, self.deck.basis(slot, When::Deferred)));
            }
        }
        deck_sent(self.deck.send(
            When::Deferred,
            Batch {
                basis,
                commands: parts,
            },
        ))
    }
}

/// The slots a part shifts the time of.
pub(crate) fn slots(part: &DeckPart) -> impl Iterator<Item = Slot> {
    let (first, second) = match *part {
        DeckPart::Attach { slot, .. }
        | DeckPart::Detach { slot }
        | DeckPart::Start { slot, .. }
        | DeckPart::Stop { slot, .. }
        | DeckPart::Fade { slot, .. }
        | DeckPart::Adopt { slot, .. }
        | DeckPart::Replace { slot, .. } => (Some(slot), None),
        DeckPart::Chain { from, to } => (Some(from), Some(to)),
        DeckPart::Mix(_) | DeckPart::Eq(_) | DeckPart::Returned(_) => (None, None),
    };
    first.into_iter().chain(second)
}

fn deck_sent(sent: Result<Seq, SendError<DeckProtocol>>) -> Result<Seq, PlayError> {
    deck_sent_owned(sent).map_err(|(error, _parts)| error)
}

fn deck_sent_owned(
    sent: Result<Seq, SendError<DeckProtocol>>,
) -> Result<Seq, (PlayError, Vec<DeckPart>)> {
    sent.map_err(|error| {
        let (error, batch) = match error {
            SendError::Full(batch) => (PlayError::Full("deck"), batch),
            SendError::Target(batch) => (
                PlayError::Internal(format!(
                    "a deck batch names a slot outside the mixer: {:?}",
                    batch.basis
                )),
                batch,
            ),
            SendError::Closed(batch) => (PlayError::Closed, batch),
        };
        (error, batch.commands)
    })
}

/// What became of a batch, as a player reports it up.
#[derive(Clone, Debug)]
pub enum Settled {
    /// The receipt is not about this player, or its outcome has not come yet.
    Pending,
    Applied {
        seq: Seq,
        at: SessionFrame,
    },
    Rejected {
        seq: Seq,
        reason: Rejection<PlayError>,
    },
}

/// What comes back to a player: a receipt of its deck's mixer, shared by every
/// player whose slot the batch names; a receipt of the dispatcher; or an event
/// of its slot. Its own lane's receipts it reads itself.
pub enum TrackReceipt<'r, S> {
    Deck {
        seq: Seq,
        outcome: &'r Outcome<DeckProtocol>,
        batch: &'r mut Batch<DeckProtocol>,
    },
    Loaded(Receipt<DispatcherProtocol<ResourceLoad<S>>>),
    Event(DeckEvent),
}

impl<S> TrackReceipt<'_, S> {
    /// Whether this is about `slot`: a deck batch whose basis names it, or an
    /// event of it.
    #[must_use]
    pub fn names(&self, slot: Slot) -> bool {
        match self {
            Self::Deck { batch, .. } => batch.basis.iter().any(|&(named, _)| named == slot),
            Self::Event(
                DeckEvent::Ended { slot: named, .. }
                | DeckEvent::Failed { slot: named, .. }
                | DeckEvent::Faded { slot: named, .. }
                | DeckEvent::Underrun { slot: named, .. },
            ) => *named == slot,
            Self::Loaded(_) => false,
        }
    }
}

/// `rejection` as it goes up, the executor's own reason made a [`PlayError`].
pub(crate) fn rejection<R>(
    rejection: &Rejection<R>,
    refusal: impl FnOnce(&R) -> PlayError,
) -> Rejection<PlayError> {
    match rejection {
        Rejection::Late => Rejection::Late,
        Rejection::Stale => Rejection::Stale,
        Rejection::Unanswered => Rejection::Unanswered,
        Rejection::Refused(reason) => Rejection::Refused(refusal(reason)),
    }
}
