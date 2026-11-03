use kithara_command::{Live, Port, ScopeId, Sender, Seq};
use kithara_config::ConfigOwner;
use kithara_play::{DeckPass, HostedDeck, Outbox, OutputSnapshot, PlayError, ResourceLoad};
use kithara_render::{
    DispatcherProtocol,
    bridge::{
        DeckEvents, DeckMixSettings, DeckProtocol, DeckSnapshot, MixerInputs, scope_channels,
    },
};
use triple_buffer::Output;

use crate::{DeckId, PassClock, session::SessionError};

/// The sole owner record for a held deck and its mixer endpoints.
pub(crate) struct Deck<S, D: ?Sized> {
    pub(crate) deck: Box<D>,
    pub(crate) scope: ScopeId,
    pub(crate) releasing: bool,
    pub(crate) dispatches: Vec<Seq>,
    pub(crate) receipts: DeckEvents,
    pub(crate) mix: Live<DeckMixSettings, DeckProtocol>,
    pub(crate) snapshot: Output<DeckSnapshot>,
    pub(crate) suspended_at: Option<u64>,
    pub(crate) session_bus: kithara_events::EventBus,
    marker: std::marker::PhantomData<fn() -> S>,
}

impl<S, D: ?Sized + HostedDeck<S>> Deck<S, D> {
    pub(crate) fn new(deck: Box<D>, scope: ScopeId) -> Result<(Self, MixerInputs), PlayError> {
        let config = deck.mixer_config();
        let mix =
            Live::new(config.mix()).map_err(|error| PlayError::Internal(error.to_string()))?;
        let (ends, inputs) = scope_channels(scope, config);
        let session_bus = deck
            .resource_prep()
            .map_or_else(kithara_events::EventBus::default, |prep| prep.bus.clone());
        Ok((
            Self {
                deck,
                scope,
                releasing: false,
                dispatches: Vec::new(),
                receipts: ends.events,
                snapshot: ends.snapshot,
                mix,
                suspended_at: None,
                session_bus,
                marker: std::marker::PhantomData,
            },
            inputs,
        ))
    }

    /// Opens this deck for one owner pass on `port`.
    ///
    /// The pass reads the snapshot once, so its suspension agrees with the
    /// blocks it reports. The outbox borrows the record's mix, suspension and
    /// dispatch list, and it carries the pass only when the pass is timed.
    pub(crate) fn open<'a>(
        &'a mut self,
        port: &'a mut dyn Port<DeckProtocol>,
        dispatcher: &'a mut Sender<DispatcherProtocol<ResourceLoad<S>>>,
        clock: PassClock,
        timed: bool,
        output: &'a OutputSnapshot,
    ) -> OpenDeck<'a, S, D> {
        let deck = self.snapshot.read();
        let pass = DeckPass {
            mix: *self.mix.config(),
            suspended: self
                .suspended_at
                .is_some_and(|at| deck.blocks <= at.saturating_add(1)),
            now: clock.now,
            delivery: clock.delivery,
            deck_delivery: clock.deck_delivery,
            output,
            deck,
        };
        let out = Outbox::new(port, dispatcher)
            .lend_mix(&mut self.mix)
            .lend_session(&mut self.suspended_at, &self.session_bus, deck.blocks)
            .track_dispatches(&mut self.dispatches);
        OpenDeck {
            deck: &mut self.deck,
            receipts: &mut self.receipts,
            out: if timed { out.in_pass(pass) } else { out },
            pass,
        }
    }
}

/// A held deck opened for one owner pass.
pub(crate) struct OpenDeck<'a, S, D: ?Sized> {
    pub(crate) deck: &'a mut D,
    pub(crate) receipts: &'a mut DeckEvents,
    pub(crate) out: Outbox<'a, S>,
    pub(crate) pass: DeckPass<'a>,
}

/// Decks held only on the owner's session thread.
#[derive_where::derive_where(Default)]
pub(crate) struct Decks<S, D: ?Sized>(pub(crate) Vec<(DeckId, Deck<S, D>)>);

impl<S, D: ?Sized> Decks<S, D> {
    pub(crate) fn index(&self, id: DeckId) -> Result<usize, PlayError> {
        self.0
            .iter()
            .position(|(held, _)| *held == id)
            .ok_or_else(|| SessionError::DeckNotFound(id).into())
    }
}
