use std::{marker::PhantomData, num::NonZeroU32};

use kithara_bufpool::HasPool;
use kithara_command::{
    Batch, ChannelConfig, Outcome, Port, Rejection, ScopedReceipt, Sender, Seq, When, channel,
};
use kithara_config::ConfigOwner;
use kithara_platform::maybe_send::MaybeSend;
pub use kithara_play::DeckControl;
use kithara_play::{DeckPass, HostedDeck, Outbox, PlayError, ResourceLoad, TrackReceipt};
use kithara_render::{DispatcherProtocol, bridge::DeckPart};
use kithara_signal::{FrameCount, SessionFrame};
use kithara_worker::TaskHandle;

use crate::{
    HostSettingsChange, HostSettingsExec,
    session::{
        SessionError,
        decks::{Deck, DeckInbox, DeckMsg, DeckWake, Decks},
        dispatch::tick_session,
        graph,
        state::{SessionState, SessionStream},
        transport::TransportState,
    },
};

/// The existing identity used for decks and their beat grids.
pub type DeckId = kithara_warp::BeatGridId;

/// A visit to one held deck during its owner's current pass.
type DeckVisit<'a, S, D> = &'a mut (dyn FnMut(&mut D, &mut Outbox<'_, S>, DeckPass<'_>) + 'a);
/// A visit that also receives the held deck's identity.
type EachDeckVisit<'a, S, D> =
    &'a mut (dyn FnMut(DeckId, &mut D, &mut Outbox<'_, S>, DeckPass<'_>) + 'a);

/// What the session thread drives, including decorators over its base owner.
pub trait HostOwner<S>:
    HostSettingsExec<(), At = When<SessionFrame>, Output = Result<Option<Seq>, PlayError>> + 'static
{
    /// Commands posted by the Host handle.
    type Command: From<HostCommand<S, Self::Deck>> + MaybeSend;
    /// The deck objects held by this owner.
    type Deck: ?Sized + HostedDeck<S>;

    /// Runs a command and returns the batch it sent, if any.
    ///
    /// # Errors
    /// Propagates registration and deck-close refusals; returns
    /// [`PlayError::Late`] or [`PlayError::Untimed`] for inadmissible timing,
    /// [`PlayError::InvalidParameter`] for invalid settings, and
    /// [`PlayError::Session`] for graph, tap, backend, or
    /// [`SessionError::HostQueueFull`] admission failures.
    /// Returns [`PlayError::Internal`] for an invalid marker command or owner
    /// setup failure, and [`PlayError::Closed`] for a retired command channel.
    fn apply(&mut self, command: Self::Command) -> Result<Option<Seq>, PlayError>;
    /// Builds and holds a deck's mixer and owner record.
    ///
    /// # Errors
    /// Returns [`PlayError::Session`] for duplicate deck identities, missing
    /// context, invalid buffer geometry, graph edits, or backend startup failure.
    /// Returns [`PlayError::Internal`] for a missing worker, dispatcher startup,
    /// scope allocation failure, or invalid mixer settings. Returns
    /// [`PlayError::Closed`] for a retired channel or deck scope.
    fn register(&mut self, id: DeckId, deck: Box<Self::Deck>) -> Result<(), PlayError>;
    /// Lends each deck its outbox and current pass.
    fn each_deck(&mut self, visit: EachDeckVisit<'_, S, Self::Deck>);
    /// Lends the named deck its outbox and current pass.
    ///
    /// # Errors
    /// Returns [`PlayError::Session`] with [`SessionError::DeckNotFound`] if the
    /// deck is absent, or [`PlayError::Closed`] if its channel or scope is retired.
    fn with_deck(
        &mut self,
        id: DeckId,
        visit: DeckVisit<'_, S, Self::Deck>,
    ) -> Result<(), PlayError>;
    /// Current frame and delivery lead, absent before a render graph exists.
    fn clock(&self) -> Option<(SessionFrame, FrameCount)>;
    /// Prepares the offline stream before the block's owner publication.
    ///
    /// # Errors
    /// Returns [`PlayError::Session`] on graph or backend startup failure, or
    /// [`PlayError::Internal`] when offline support is absent or the host is realtime.
    fn prepare_offline(&mut self) -> Result<(), PlayError>;
    /// Processes one offline block after the owner has published its commands.
    ///
    /// # Errors
    /// Returns [`PlayError::Internal`] if offline support or the prepared stream
    /// is absent, or if the timeline or output buffer geometry is invalid.
    fn render_offline(
        &mut self,
        position: u64,
        frames: usize,
        output: &mut [f32],
    ) -> Result<(), PlayError>;
    /// Reads the latest applied transport anchor from its processor observation.
    fn transport(&mut self) -> Option<crate::api::SessionTransportSnapshot>;
    /// Available batches in the host ring.
    fn host_room(&self) -> usize;
    /// Reads the iteration clock and settles receipts before handling posts.
    fn begin_pass(&mut self);
    /// A release post waits for this deck's scope retirement.
    fn release_id(command: &Self::Command) -> Option<DeckId>;
    /// Identifies consecutive next-block tempo posts that share one winner.
    fn is_next_tempo(command: &Self::Command) -> bool;
    /// Settles executor receipts and publishes the owner snapshot.
    fn pass(&mut self) -> Vec<HostSettled>;
}

/// One operation on the canonical owner, answered after checks and sending.
pub enum HostCommand<S, D: ?Sized> {
    Register {
        id: DeckId,
        deck: Box<D>,
    },
    Configure(HostSettingsChange, When<SessionFrame>),
    Close(DeckId),
    Release(DeckId),
    AttachOutputs {
        tap: crate::api::Tap,
        outputs: kithara_output::OutputGroup,
    },
    DetachOutputs {
        tap: crate::api::Tap,
    },
    Restart,
    #[doc(hidden)]
    _Marker(PhantomData<fn() -> S>),
}

/// The final outcome of a host-settings batch.
pub enum HostSettled {
    Settings {
        seq: Seq,
        change: HostSettingsChange,
        outcome: Result<SessionFrame, Rejection<PlayError>>,
    },
    Batch {
        seq: Seq,
        outcome: Result<SessionFrame, Rejection<PlayError>>,
    },
    Closed {
        deck: DeckId,
    },
}

/// The base owner of a session, its deck records and its single load dispatcher.
pub struct HostCore<S, D: ?Sized + HostedDeck<S> = dyn HostedDeck<S>> {
    pub(crate) session: SessionState<SessionStream, S>,
    decks: Decks<S, D>,
    dispatcher: Sender<DispatcherProtocol<ResourceLoad<S>>>,
    dispatcher_inbox: Option<kithara_command::Inbox<DispatcherProtocol<ResourceLoad<S>>>>,
    dispatcher_task: Option<TaskHandle>,
    retired_dispatches: Vec<Seq>,
    retired_lanes: Vec<kithara_render::LaneId>,
    inbox: kithara_platform::sync::Arc<dyn DeckInbox>,
}

impl<S, D: ?Sized + HostedDeck<S>> HostCore<S, D>
where
    S: HasPool<f32> + Send + Sync + 'static,
{
    pub(crate) fn new(
        session: SessionState<SessionStream, S>,
        inbox: kithara_platform::sync::Arc<dyn DeckInbox>,
    ) -> Self {
        let (dispatcher, dispatcher_inbox) = channel(ChannelConfig::builder().build());
        Self {
            session,
            decks: Decks::default(),
            dispatcher,
            dispatcher_inbox: Some(dispatcher_inbox),
            dispatcher_task: None,
            retired_dispatches: Vec::new(),
            retired_lanes: Vec::new(),
            inbox,
        }
    }

    fn close(&mut self, id: DeckId) -> Result<(), PlayError> {
        let index = self.decks.index(id)?;
        if self.decks.0[index].1.releasing {
            return Ok(());
        }
        let mut result = Ok(());
        self.with_deck(id, &mut |deck, out, _pass| {
            result = deck.close(out);
        })?;
        result?;
        let record = &mut self.decks.0[index].1;
        self.session
            .channel
            .as_mut()
            .ok_or(PlayError::Closed)?
            .close(record.scope)
            .map_err(|error| PlayError::Internal(error.to_string()))?;
        record.releasing = true;
        if self.decks.0.iter().all(|(_, record)| record.releasing) {
            graph::idle(&mut self.session)?;
        }
        Ok(())
    }

    fn release(&mut self, id: DeckId) -> Result<(), PlayError> {
        self.close(id)
    }

    fn restart(&mut self) -> Result<(), PlayError> {
        crate::session::dispatch::invalidate_audio_route(&mut self.session, "host restart")
            .map_err(Into::into)
    }

    fn publish_root(&self) {
        self.session
            .root_view
            .publish_decks(self.decks.0.iter().map(|(id, _)| *id).collect());
        self.session.publish_root();
    }
}

impl<S, D> HostSettingsExec<()> for HostCore<S, D>
where
    S: HasPool<f32> + Send + Sync + 'static,
    D: ?Sized + HostedDeck<S>,
{
    type At = When<SessionFrame>;
    type Output = Result<Option<Seq>, PlayError>;

    delegate::delegate! {
        to self.session {
            fn exec_sample_rate(&mut self, value: NonZeroU32, at: Self::At, cx: &mut ()) -> Self::Output;
            fn exec_tempo(&mut self, value: crate::api::Tempo, at: Self::At, cx: &mut ()) -> Self::Output;
            fn exec_live(&mut self, change: HostSettingsChange, at: Self::At, cx: &mut ()) -> Self::Output;
        }
    }
}

impl<S, D> HostOwner<S> for HostCore<S, D>
where
    S: HasPool<f32> + Send + Sync + 'static,
    D: ?Sized + HostedDeck<S>,
{
    type Command = HostCommand<S, D>;
    type Deck = D;

    fn apply(&mut self, command: Self::Command) -> Result<Option<Seq>, PlayError> {
        match command {
            HostCommand::Register { id, deck } => self.register(id, deck).map(|()| None),
            HostCommand::Configure(change, at) => self.exec(change, at, &mut ()),
            HostCommand::Close(id) => self.close(id).map(|()| None),
            HostCommand::Release(id) => self.release(id).map(|()| None),
            HostCommand::AttachOutputs { tap, outputs } => {
                graph::tap::attach(&mut self.session, tap, outputs)
                    .map(|()| None)
                    .map_err(Into::into)
            }
            HostCommand::DetachOutputs { tap } => {
                graph::tap::detach(&mut self.session, tap);
                Ok(None)
            }
            HostCommand::Restart => self.restart().map(|()| None),
            HostCommand::_Marker(_) => Err(PlayError::Internal(
                "a marker is not an owner command".to_owned(),
            )),
        }
    }

    fn register(&mut self, id: DeckId, deck: Box<D>) -> Result<(), PlayError> {
        if self.decks.0.iter().any(|(held, _)| *held == id) {
            return Err(SessionError::DeckAttached(id).into());
        }
        crate::session::state::ensure_ctx(&mut self.session)?;
        let prep = deck.resource_prep();
        if let Some(prep) = prep
            && let Some(quantum) = prep.warp.render_quantum_frames()
        {
            let shape = self
                .session
                .root_view
                .output
                .get()
                .stream_shape
                .ok_or(SessionError::NoContext)?;
            if let Err(error) = shape.playback_buffers(quantum, prep.response_budget_frames) {
                if self.decks.0.is_empty() {
                    graph::idle(&mut self.session)?;
                    graph::drop_idle_context(&mut self.session)?;
                }
                return Err(error.into());
            }
        }
        let bus = prep.map(|prep| prep.bus.clone());
        let worker = deck.worker().ok_or_else(|| {
            PlayError::Internal("a hosted deck requires its resource worker".into())
        })?;
        if let Some(delay) = &mut self.session.delivery_delay {
            *delay = (*delay)
                .max(crate::consts::SESSION_PUMP_INTERVAL.saturating_add(worker.wake_allowance()));
        }
        let pools = worker.pools().clone();
        if let Some(inbox) = self.dispatcher_inbox.take() {
            self.dispatcher_task = Some(
                worker
                    .start_dispatcher(inbox)
                    .map_err(|error| PlayError::Internal(error.to_string()))?,
            );
        }
        let scope = self
            .session
            .channel
            .as_mut()
            .ok_or(PlayError::Closed)?
            .open(deck.mixer_config().slots().get());
        let scope = match scope {
            Ok(scope) => scope,
            Err(error) => {
                if self.decks.0.is_empty() {
                    graph::idle(&mut self.session)?;
                    graph::drop_idle_context(&mut self.session)?;
                }
                return Err(PlayError::Internal(error.to_string()));
            }
        };
        let (mut record, inputs) = Deck::new(deck, scope)?;
        if let Err(error) = graph::install_deck(&mut self.session, id, inputs, pools, bus) {
            if let Some(channel) = &mut self.session.channel {
                let _ = channel.close(scope);
            }
            record.releasing = true;
            self.decks.0.push((id, record));
            return Err(error.into());
        }
        record
            .deck
            .hold(DeckWake::waker(&self.inbox, DeckMsg::Drain(id)));
        self.dispatcher
            .hold(DeckWake::waker(&self.inbox, DeckMsg::Receipts));
        self.decks.0.push((id, record));
        self.with_deck(id, &mut |deck, out, pass| deck.drain(pass, out))?;
        Ok(())
    }

    fn each_deck(
        &mut self,
        visit: &mut dyn FnMut(DeckId, &mut D, &mut Outbox<'_, S>, DeckPass<'_>),
    ) {
        let clock = self.clock();
        let (now, delivery) = clock.unwrap_or((SessionFrame::new(0), FrameCount::new(0)));
        let output = self.session.root_view.output.get();
        let Some(channel) = &mut self.session.channel else {
            return;
        };
        for (id, record) in &mut self.decks.0 {
            let suspended = record
                .suspended_at
                .is_some_and(|at| record.snapshot.read().blocks <= at.saturating_add(1));
            let pass = DeckPass {
                mix: *record.mix.config(),
                suspended,
                now,
                delivery,
                output: &output,
                deck: record.snapshot.read(),
            };
            let Some(mut port) = channel.scope(record.scope) else {
                continue;
            };
            let mut out = Outbox::new(&mut port, &mut self.dispatcher)
                .lend_mix(&mut record.mix)
                .lend_session(
                    &mut record.suspended_at,
                    &record.session_bus,
                    pass.deck.blocks,
                )
                .track_dispatches(&mut record.dispatches);
            if clock.is_some() {
                out = out.in_pass(pass);
            }
            visit(*id, &mut record.deck, &mut out, pass);
        }
    }

    fn with_deck(
        &mut self,
        id: DeckId,
        visit: &mut dyn FnMut(&mut D, &mut Outbox<'_, S>, DeckPass<'_>),
    ) -> Result<(), PlayError> {
        let index = self.decks.index(id)?;
        let clock = self.clock();
        let (now, delivery) = clock.unwrap_or((SessionFrame::new(0), FrameCount::new(0)));
        let output = self.session.root_view.output.get();
        let record = &mut self.decks.0[index].1;
        let suspended = record
            .suspended_at
            .is_some_and(|at| record.snapshot.read().blocks <= at.saturating_add(1));
        let pass = DeckPass {
            mix: *record.mix.config(),
            suspended,
            now,
            delivery,
            output: &output,
            deck: record.snapshot.read(),
        };
        let mut port = self
            .session
            .channel
            .as_mut()
            .ok_or(PlayError::Closed)?
            .scope(record.scope)
            .ok_or(PlayError::Closed)?;
        let mut out = Outbox::new(&mut port, &mut self.dispatcher)
            .lend_mix(&mut record.mix)
            .lend_session(
                &mut record.suspended_at,
                &record.session_bus,
                pass.deck.blocks,
            )
            .track_dispatches(&mut record.dispatches);
        if clock.is_some() {
            out = out.in_pass(pass);
        }
        visit(&mut record.deck, &mut out, pass);
        Ok(())
    }

    fn clock(&self) -> Option<(SessionFrame, FrameCount)> {
        self.session.iteration_clock
    }

    #[cfg(feature = "offline")]
    fn prepare_offline(&mut self) -> Result<(), PlayError> {
        crate::session::state::ensure_ctx(&mut self.session)?;
        if matches!(self.session.stream, Some(SessionStream::Offline(_))) {
            Ok(())
        } else {
            Err(PlayError::Internal(
                "host is not configured for offline rendering".to_owned(),
            ))
        }
    }

    #[cfg(not(feature = "offline"))]
    fn prepare_offline(&mut self) -> Result<(), PlayError> {
        Err(PlayError::Internal(
            "offline rendering requires the offline feature".to_owned(),
        ))
    }

    #[cfg(feature = "offline")]
    fn render_offline(
        &mut self,
        position: u64,
        frames: usize,
        output: &mut [f32],
    ) -> Result<(), PlayError> {
        let Some(SessionStream::Offline(stream)) = &mut self.session.stream else {
            return Err(PlayError::Internal(
                "offline stream is not prepared".to_owned(),
            ));
        };
        stream
            .render(position, frames, output)
            .map_err(|error| PlayError::Internal(error.to_string()))
    }

    #[cfg(not(feature = "offline"))]
    fn render_offline(
        &mut self,
        _position: u64,
        _frames: usize,
        _output: &mut [f32],
    ) -> Result<(), PlayError> {
        Err(PlayError::Internal(
            "offline rendering requires the offline feature".to_owned(),
        ))
    }

    fn transport(&mut self) -> Option<crate::api::SessionTransportSnapshot> {
        self.session
            .transport_observation
            .as_mut()?
            .read()
            .snapshot()
    }

    fn host_room(&self) -> usize {
        self.session.channel.as_ref().map_or(0, Port::available)
    }

    fn release_id(command: &Self::Command) -> Option<DeckId> {
        if let HostCommand::Release(id) = command {
            Some(*id)
        } else {
            None
        }
    }

    fn is_next_tempo(command: &Self::Command) -> bool {
        matches!(
            command,
            HostCommand::Configure(HostSettingsChange::Tempo(_), When::Next)
        )
    }

    fn begin_pass(&mut self) {
        self.session.begin_iteration();
        let (now, delivery) = self
            .clock()
            .unwrap_or((SessionFrame::new(0), FrameCount::new(0)));
        let output = self.session.root_view.output.get();
        loop {
            let receipt = {
                let mut receipts = self.dispatcher.receipts();
                receipts.next()
            };
            let Some(receipt) = receipt else {
                break;
            };
            let owner = self
                .decks
                .0
                .iter()
                .position(|(_, record)| record.dispatches.contains(&receipt.seq()));
            let Some(index) = owner else {
                if let Some(index) = self
                    .retired_dispatches
                    .iter()
                    .position(|seq| *seq == receipt.seq())
                {
                    self.retired_dispatches.remove(index);
                    if let Outcome::Applied {
                        data: kithara_render::Dispatched::Loaded(loaded),
                        ..
                    } = receipt.outcome()
                    {
                        self.retired_lanes.push(loaded.lane);
                    }
                    continue;
                }
                tracing::error!(seq = ?receipt.seq(), "dispatcher receipt has no owning deck");
                continue;
            };
            let record = &mut self.decks.0[index].1;
            record.dispatches.retain(|seq| *seq != receipt.seq());
            let suspended = record
                .suspended_at
                .is_some_and(|at| record.snapshot.read().blocks <= at.saturating_add(1));
            let pass = DeckPass {
                mix: *record.mix.config(),
                suspended,
                now,
                delivery,
                output: &output,
                deck: record.snapshot.read(),
            };
            if let Some(mut port) = self
                .session
                .channel
                .as_mut()
                .and_then(|channel| channel.scope(record.scope))
            {
                let mut out = Outbox::new(&mut port, &mut self.dispatcher)
                    .lend_mix(&mut record.mix)
                    .lend_session(
                        &mut record.suspended_at,
                        &record.session_bus,
                        pass.deck.blocks,
                    )
                    .track_dispatches(&mut record.dispatches);
                if self.session.iteration_clock.is_some() {
                    out = out.in_pass(pass);
                }
                record
                    .deck
                    .settle(TrackReceipt::Loaded(receipt), pass, &mut out);
            }
        }
        while self.dispatcher.available() != 0 {
            let Some(lane) = self.retired_lanes.last().copied() else {
                break;
            };
            match self.dispatcher.send(
                When::Next,
                Batch {
                    basis: Vec::new(),
                    commands: vec![kithara_render::DispatcherCommand::Release(lane)],
                },
            ) {
                Ok(seq) => {
                    self.retired_lanes.pop();
                    self.retired_dispatches.push(seq);
                }
                Err(error) => {
                    tracing::error!(?error, "a retired load's lane could not be released");
                    break;
                }
            }
        }
        self.retire_stopped_scopes();
        self.route_receipts(now, delivery);
        self.poll_events();
    }

    fn pass(&mut self) -> Vec<HostSettled> {
        if let Err(error) = tick_session(&mut self.session) {
            tracing::warn!(%error, "host graph pass failed");
        }
        if let Some(channel) = &mut self.session.channel
            && let Err(error) = channel.publish()
        {
            tracing::warn!(%error, "host publication gate closed");
        }
        self.retire_stopped_scopes();
        let (now, delivery) = self
            .clock()
            .unwrap_or((SessionFrame::new(0), FrameCount::new(0)));
        self.route_receipts(now, delivery);
        if self.session.stream.is_none()
            && self.decks.0.is_empty()
            && let Err(error) = graph::drop_idle_context(&mut self.session)
        {
            tracing::warn!(%error, "empty stopped host context could not retire");
        }
        self.publish_root();
        std::mem::take(&mut self.session.settled)
    }
}

impl<S, D> HostCore<S, D>
where
    D: ?Sized + HostedDeck<S>,
{
    fn retire_stopped_scopes(&mut self) {
        #[cfg(feature = "offline")]
        if let Some(SessionStream::Offline(stream)) = &mut self.session.stream {
            if let Err(error) = stream.retire_closing() {
                tracing::error!(%error, "parked offline transport could not retire its scopes");
            }
            return;
        }
        if self.session.stream.is_some() {
            return;
        }
        if let Some(store) = self
            .session
            .ctx
            .as_mut()
            .and_then(|ctx| ctx.proc_store_mut())
            && let Some(transport) = store.try_get_mut::<TransportState>()
            && let Err(error) = transport.retire_closing()
        {
            tracing::error!(%error, "stopped transport could not retire its scopes");
        }
    }

    fn route_receipts(&mut self, now: SessionFrame, delivery: FrameCount) {
        let output = self.session.root_view.output.get();
        loop {
            let Some(receipt) = self
                .session
                .channel
                .as_mut()
                .and_then(kithara_command::ScopedSender::receipt)
            else {
                break;
            };
            match receipt {
                ScopedReceipt::Root(receipt) => {
                    crate::session::queue::settle_receipt(&mut self.session, &receipt);
                }
                ScopedReceipt::Scope(scope, receipt) => {
                    let Some(index) = self
                        .decks
                        .0
                        .iter()
                        .position(|(_, record)| record.scope == scope)
                    else {
                        tracing::error!(?scope, "scope receipt has no owning deck");
                        continue;
                    };
                    let record = &mut self.decks.0[index].1;
                    let mix = record.mix.settle(&receipt).is_some();
                    let eq = receipt.batch().commands.iter().any(|part| {
                        matches!(
                            part,
                            DeckPart::Eq(_)
                                | DeckPart::Returned(kithara_render::bridge::Returned::Eq(_))
                        )
                    });
                    if mix || eq {
                        let outcome = match receipt.outcome() {
                            Outcome::Applied { at, .. } => Ok(*at),
                            Outcome::Rejected(reason) => Err(map_deck_rejection(*reason)),
                        };
                        self.session.settled.push(HostSettled::Batch {
                            seq: receipt.seq(),
                            outcome,
                        });
                    }
                    if let Some(mut port) = self
                        .session
                        .channel
                        .as_mut()
                        .and_then(|channel| channel.scope(scope))
                    {
                        let seq = receipt.seq();
                        let (outcome, mut batch) = receipt.into();
                        let suspended = record.suspended_at.is_some_and(|at| {
                            record.snapshot.read().blocks <= at.saturating_add(1)
                        });
                        let pass = DeckPass {
                            mix: *record.mix.config(),
                            suspended,
                            now,
                            delivery,
                            output: &output,
                            deck: record.snapshot.read(),
                        };
                        let mut out = Outbox::new(&mut port, &mut self.dispatcher)
                            .lend_mix(&mut record.mix)
                            .lend_session(
                                &mut record.suspended_at,
                                &record.session_bus,
                                pass.deck.blocks,
                            )
                            .track_dispatches(&mut record.dispatches);
                        if self.session.iteration_clock.is_some() {
                            out = out.in_pass(pass);
                        }
                        record.deck.settle(
                            TrackReceipt::Deck {
                                seq,
                                outcome: &outcome,
                                batch: &mut batch,
                            },
                            pass,
                            &mut out,
                        );
                    }
                }
                ScopedReceipt::Closed(scope) => {
                    let Some(index) = self
                        .decks
                        .0
                        .iter()
                        .position(|(_, record)| record.scope == scope)
                    else {
                        continue;
                    };
                    let id = self.decks.0[index].0;
                    if self.session.deck_nodes.iter().any(|deck| deck.id == id)
                        && let Err(error) = graph::remove_deck(&mut self.session, id)
                    {
                        tracing::error!(%error, ?id, "closed deck node could not be reclaimed");
                        continue;
                    }
                    let (_, mut record) = self.decks.0.remove(index);
                    self.retired_dispatches.append(&mut record.dispatches);
                    record.deck.release();
                    self.session.settled.push(HostSettled::Closed { deck: id });
                }
            }
        }
    }

    fn poll_events(&mut self) {
        let output = self.session.root_view.output.get();
        if let Some((now, delivery)) = self.session.iteration_clock
            && let Some(channel) = &mut self.session.channel
        {
            for (_, record) in &mut self.decks.0 {
                let suspended = record
                    .suspended_at
                    .is_some_and(|at| record.snapshot.read().blocks <= at.saturating_add(1));
                let pass = DeckPass {
                    mix: *record.mix.config(),
                    suspended,
                    now,
                    delivery,
                    output: &output,
                    deck: record.snapshot.read(),
                };
                let Some(mut port) = channel.scope(record.scope) else {
                    continue;
                };
                let mut out = Outbox::new(&mut port, &mut self.dispatcher)
                    .in_pass(pass)
                    .lend_mix(&mut record.mix)
                    .lend_session(
                        &mut record.suspended_at,
                        &record.session_bus,
                        pass.deck.blocks,
                    )
                    .track_dispatches(&mut record.dispatches);
                for event in record.receipts.drain() {
                    record
                        .deck
                        .settle(TrackReceipt::Event(event), pass, &mut out);
                }
            }
        }
    }
}

fn map_deck_rejection(
    reason: Rejection<kithara_render::bridge::DeckRefusal>,
) -> Rejection<PlayError> {
    match reason {
        Rejection::Late => Rejection::Late,
        Rejection::Stale => Rejection::Stale,
        Rejection::Unanswered => Rejection::Unanswered,
        Rejection::Refused(reason) => Rejection::Refused(PlayError::Deck(reason)),
    }
}

impl<S, D: ?Sized + HostedDeck<S>> Drop for HostCore<S, D> {
    fn drop(&mut self) {
        for (_, record) in &mut self.decks.0 {
            if let Some(channel) = &mut self.session.channel {
                if let Some(mut port) = channel.scope(record.scope) {
                    let mut out =
                        Outbox::new(&mut port, &mut self.dispatcher).lend_mix(&mut record.mix);
                    if let Err(error) = record.deck.close(&mut out) {
                        tracing::warn!(%error, "host deck close failed during shutdown");
                    }
                }
                if !record.releasing {
                    let _ = channel.close(record.scope);
                }
            }
        }
        if let Some(channel) = &mut self.session.channel {
            let _ = channel.publish();
        }
        self.session.stream = None;
        if let Some(ctx) = &mut self.session.ctx {
            #[cfg(not(target_arch = "wasm32"))]
            if let Err(error) =
                ctx.deactivate_blocking(kithara_platform::time::Duration::from_secs(3))
            {
                tracing::error!(?error, "host processor did not return during shutdown");
                return;
            }
            #[cfg(target_arch = "wasm32")]
            {
                ctx.request_deactivate();
                if let Some(mut context) = self.session.ctx.take() {
                    let mut channel = self.session.channel.take();
                    let mut decks: Vec<_> = self
                        .decks
                        .0
                        .drain(..)
                        .map(|(_, record)| (record.scope, record.deck))
                        .collect();
                    let dispatcher_task = self.dispatcher_task.take();
                    let root_view = self.session.root_view.clone();
                    drop(kithara_platform::tokio::task::spawn(async move {
                        let mut retired = false;
                        loop {
                            if let Err(error) = context.update() {
                                tracing::warn!(?error, "browser shutdown graph update failed");
                            }
                            if !retired && let Some(store) = context.proc_store_mut() {
                                if let Some(transport) = store.try_get_mut::<TransportState>() {
                                    if let Err(error) = transport.retire_closing() {
                                        tracing::error!(%error, "stopped transport could not retire its scopes");
                                    }
                                }
                                if let std::collections::hash_map::Entry::Occupied(entry) =
                                    store.entry::<TransportState>().boxed_entry
                                {
                                    drop(entry.remove());
                                }
                                retired = true;
                            }
                            if let Some(channel) = &mut channel {
                                while let Some(receipt) = channel.receipt() {
                                    if let ScopedReceipt::Closed(scope) = receipt
                                        && let Some(index) =
                                            decks.iter().position(|(held, _)| *held == scope)
                                    {
                                        let (_, mut deck) = decks.remove(index);
                                        deck.release();
                                    }
                                }
                            }
                            if retired && decks.is_empty() {
                                break;
                            }
                            kithara_platform::time::sleep(crate::consts::SESSION_PUMP_INTERVAL)
                                .await;
                        }
                        root_view.publish_decks(Box::default());
                        drop(dispatcher_task);
                        drop(context);
                        drop(channel);
                    }));
                }
                return;
            }
        }
        self.retire_stopped_scopes();
        self.route_receipts(SessionFrame::new(0), FrameCount::new(0));
    }
}
