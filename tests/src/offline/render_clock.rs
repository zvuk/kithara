use std::num::NonZeroU32;

use kithara::{
    host::{
        DeckControl, DeckId, HostOwner, HostRegistration, HostSettingsChange, HostSettingsExec,
        HostSettled, PassClock,
        api::{SessionTransportSnapshot, Tempo},
    },
    platform::{time::Instant, tokio::sync::watch},
    play::{DeckPass, Outbox, PlayError},
};
use kithara_command::Seq;

/// Observes callback starts on the existing offline session owner.
#[non_exhaustive]
pub struct RenderClock<O> {
    inner: O,
    started: watch::Sender<Option<(u64, Instant)>>,
}

impl<O> RenderClock<O> {
    pub(super) fn new(inner: O, started: watch::Sender<Option<(u64, Instant)>>) -> Self {
        Self { inner, started }
    }
}

impl<O: HostSettingsExec<()>> HostSettingsExec<()> for RenderClock<O> {
    type At = O::At;
    type Output = O::Output;

    delegate::delegate! {
        to self.inner {
            fn exec_sample_rate(&mut self, value: NonZeroU32, at: Self::At, cx: &mut ()) -> Self::Output;
            fn exec_tempo(&mut self, value: Tempo, at: Self::At, cx: &mut ()) -> Self::Output;
            fn exec_live(&mut self, change: HostSettingsChange, at: Self::At, cx: &mut ()) -> Self::Output;
        }
    }
}

impl<S, O: HostOwner<S>> HostOwner<S> for RenderClock<O> {
    type Command = O::Command;
    type Deck = O::Deck;

    fn render_offline(
        &mut self,
        position: u64,
        frames: usize,
        output: &mut [f32],
    ) -> Result<(), PlayError> {
        self.started.send_replace(Some((position, Instant::now())));
        self.inner.render_offline(position, frames, output)
    }

    delegate::delegate! {
        to self.inner {
            fn apply(&mut self, command: Self::Command) -> Result<Option<Seq>, PlayError>;
            fn register(&mut self, id: DeckId, deck: Box<Self::Deck>) -> Result<(), PlayError>;
            fn each_deck(
                &mut self,
                visit: &mut dyn FnMut(DeckId, &mut Self::Deck, &mut Outbox<'_, S>, DeckPass<'_>),
            );
            fn with_deck(
                &mut self,
                id: DeckId,
                visit: &mut dyn FnMut(&mut Self::Deck, &mut Outbox<'_, S>, DeckPass<'_>),
            ) -> Result<(), PlayError>;
            fn clock(&self) -> Option<PassClock>;
            fn prepare_offline(&mut self) -> Result<(), PlayError>;
            fn transport(&mut self) -> Option<SessionTransportSnapshot>;
            fn host_room(&self) -> usize;
            fn begin_pass(&mut self);
            fn pass(&mut self) -> Vec<HostSettled>;
        }
    }

    fn release_id(command: &Self::Command) -> Option<DeckId> {
        O::release_id(command)
    }

    fn is_next_tempo(command: &Self::Command) -> bool {
        O::is_next_tempo(command)
    }
}

impl<S, O, D, Face: ?Sized> HostRegistration<S, D, Face> for RenderClock<O>
where
    O: HostRegistration<S, D, Face>,
    D: DeckControl,
{
    fn into_deck(deck: D) -> Box<Self::Deck> {
        O::into_deck(deck)
    }
}
