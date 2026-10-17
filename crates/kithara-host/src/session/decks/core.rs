use kithara_command::{Live, ScopeId, Seq};
use kithara_play::{HostedDeck, PlayError};
use kithara_render::bridge::{
    DeckEvents, DeckMixSettings, DeckProtocol, DeckSnapshot, MixerInputs, scope_channels,
};
use triple_buffer::Output;

use crate::{DeckId, session::SessionError};

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
