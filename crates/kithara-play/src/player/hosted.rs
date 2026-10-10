use std::task::Waker;

use kithara_platform::maybe_send::MaybeSend;
use kithara_render::{bridge::DeckSnapshot, rt::DeckMixerConfig};
use kithara_signal::{FrameCount, SessionFrame};

use super::{Outbox, TrackReceipt};
use crate::{OutputSnapshot, PlayError, PlayWorker, ResourcePrep};

/// A deck's public control endpoint before the owner registers it.
pub trait DeckControl {
    /// The handle retained while the owner holds the deck.
    type Control;
    /// Hands out a control handle without binding or seating the deck.
    fn control(&self) -> Self::Control;
}

/// A deck as the engine owner holds it: the owner builds the deck's mixer from
/// [`HostedDeck::mixer_config`], lends the deck an [`Outbox`] over that mixer
/// and the dispatcher for each pass, and hands it every receipt and event of
/// its slots.
pub trait HostedDeck<S>: MaybeSend + 'static {
    /// The worker and typed pools shared by the deck's resource loads.
    fn worker(&self) -> Option<&PlayWorker<S>>;

    /// Resource-loading policy checked against the session before registration.
    /// Decks that do not load resources have no preparation policy.
    fn resource_prep(&self) -> Option<&ResourcePrep<S>> {
        None
    }

    /// The mixer the owner builds for this deck when it registers it.
    fn mixer_config(&self) -> DeckMixerConfig;

    /// Runs every command the deck's handles posted since the last drain.
    fn drain(&mut self, pass: DeckPass<'_>, out: &mut Outbox<'_, S>);

    /// Takes a receipt or an event of the deck's slots.
    fn settle(&mut self, receipt: TrackReceipt<'_, S>, pass: DeckPass<'_>, out: &mut Outbox<'_, S>);

    /// One step of session time.
    fn tick(&mut self, pass: DeckPass<'_>, out: &mut Outbox<'_, S>);

    /// Stops the deck's tracks and lets their slots go.
    ///
    /// # Errors
    ///
    /// Returns why the release did not go out.
    fn close(&mut self, out: &mut Outbox<'_, S>) -> Result<(), PlayError>;

    /// An executor took the deck: `waker` tells it when the deck's handles
    /// post a command.
    fn hold(&mut self, waker: Waker);

    /// The executor let the deck go: later commands wait for the next one.
    fn release(&mut self);
}

/// What the owner tells its deck on one pass.
#[derive(Clone, Copy, Debug)]
pub struct DeckPass<'a> {
    /// Mix confirmed by the Host's Applied receipts.
    pub mix: crate::DeckMixSettings,
    /// The platform holds the output until the mixer has published past suspension.
    pub suspended: bool,
    /// The session frame the pass stands on.
    pub now: SessionFrame,
    /// Frames a batch sent now takes to reach the mixer: the session thread,
    /// the worker's wake and one block. The earliest frame a press lands on is
    /// `now + delivery`.
    pub delivery: FrameCount,
    /// The session output observed by the owner for this pass.
    pub output: &'a OutputSnapshot,
    /// What the mixer last published of the deck's slots.
    pub deck: &'a DeckSnapshot,
}

impl DeckPass<'_> {
    /// The earliest frame a batch sent on this pass can apply on.
    #[must_use]
    pub fn earliest(&self) -> SessionFrame {
        self.now + self.delivery
    }
}
