use std::task::Waker;

use kithara_command::Seq;
use kithara_play::{
    DeckControl, DeckPass, HostedDeck, Outbox, PlayError, PlayWorker, ResourcePrep, TrackReceipt,
};
use kithara_render::rt::DeckMixerConfig;
use kithara_signal::{FrameCount, SessionFrame};

use crate::{GridAnswer, LinkedPlayer, TempoTrajectory};

/// Object-safe synchronization face of a deck held by the Host owner.
pub trait LinkedDeck<S>: HostedDeck<S> {
    /// Enables explicit phase alignment or disables future Host retimes.
    ///
    /// # Errors
    /// Returns the refusal of an alignment batch.
    fn sync(&mut self, on: bool, out: &mut Outbox<'_, S>) -> Result<Option<Seq>, PlayError>;
    /// Delivers the planned Host trajectory to every active synchronized track.
    fn retime(&mut self, trajectory: &TempoTrajectory, at: SessionFrame, out: &mut Outbox<'_, S>);
    /// Delivers analysis to the track owning the item.
    fn grid(&mut self, answer: GridAnswer, out: &mut Outbox<'_, S>);
    /// Whether this deck follows the Host's tempo.
    fn synced(&self) -> bool;
    /// Maximum lane lead among sounding synchronized tracks.
    fn lead(&self, delivery: FrameCount) -> Option<FrameCount>;
    /// Minimum available room among lanes that would receive a retime.
    fn lane_room(&self) -> usize;
    /// Scope batches needed to reopen silent tracks during a retime.
    fn scope_parts(&self) -> usize;
    /// Whether any lane applied its current native representation of this retime.
    fn retime_applied(&mut self, at: SessionFrame) -> Option<bool>;
    /// Closes phase against the new trajectory without a commanded speed step.
    fn realign(&mut self, trajectory: &TempoTrajectory, at: SessionFrame, out: &mut Outbox<'_, S>);
}

impl<S, D: HostedDeck<S> + LinkedPlayer<S>> LinkedDeck<S> for D {
    fn sync(&mut self, on: bool, out: &mut Outbox<'_, S>) -> Result<Option<Seq>, PlayError> {
        LinkedPlayer::sync(self, on, out)
    }

    fn retime(&mut self, trajectory: &TempoTrajectory, at: SessionFrame, out: &mut Outbox<'_, S>) {
        LinkedPlayer::retime(self, trajectory, at, out);
    }

    fn grid(&mut self, answer: GridAnswer, out: &mut Outbox<'_, S>) {
        LinkedPlayer::grid(self, answer, out);
    }

    fn synced(&self) -> bool {
        LinkedPlayer::synced(self)
    }

    fn lead(&self, delivery: FrameCount) -> Option<FrameCount> {
        LinkedPlayer::lead(self, delivery)
    }

    fn lane_room(&self) -> usize {
        LinkedPlayer::lane_room(self)
    }

    fn scope_parts(&self) -> usize {
        LinkedPlayer::scope_parts(self)
    }

    fn retime_applied(&mut self, at: SessionFrame) -> Option<bool> {
        LinkedPlayer::retime_applied(self, at)
    }

    fn realign(&mut self, trajectory: &TempoTrajectory, at: SessionFrame, out: &mut Outbox<'_, S>) {
        LinkedPlayer::realign(self, trajectory, at, out);
    }
}

/// An ordinary deck registered in a Host decorated with synchronization.
pub struct Unlinked<D>(pub(crate) D);

impl<D> Unlinked<D> {
    /// Marks this registration as an ordinary hosted deck.
    #[must_use]
    pub const fn new(deck: D) -> Self {
        Self(deck)
    }
}

impl<D: DeckControl> DeckControl for Unlinked<D> {
    type Control = D::Control;

    fn control(&self) -> Self::Control {
        self.0.control()
    }
}

/// The ordinary and synchronized members of one Host registry.
#[non_exhaustive]
pub enum LinkedHostDeck<S> {
    /// A deck whose playback is independent of the Host tempo.
    Ordinary(Box<dyn HostedDeck<S>>),
    /// A deck that can receive synchronization commands and Host trajectories.
    Linked(Box<dyn LinkedDeck<S>>),
}

impl<S: 'static> LinkedHostDeck<S> {
    fn hosted(&self) -> &dyn HostedDeck<S> {
        match self {
            Self::Ordinary(deck) => &**deck,
            Self::Linked(deck) => &**deck,
        }
    }

    fn hosted_mut(&mut self) -> &mut dyn HostedDeck<S> {
        match self {
            Self::Ordinary(deck) => &mut **deck,
            Self::Linked(deck) => &mut **deck,
        }
    }
}

impl<S: 'static> HostedDeck<S> for LinkedHostDeck<S> {
    delegate::delegate! {
        to self.hosted() {
            fn worker(&self) -> Option<&PlayWorker<S>>;
            fn resource_prep(&self) -> Option<&ResourcePrep<S>>;
            fn mixer_config(&self) -> DeckMixerConfig;
        }
        to self.hosted_mut() {
            fn drain(&mut self, pass: DeckPass<'_>, out: &mut Outbox<'_, S>);
            fn settle(&mut self, receipt: TrackReceipt<'_, S>, pass: DeckPass<'_>, out: &mut Outbox<'_, S>);
            fn tick(&mut self, pass: DeckPass<'_>, out: &mut Outbox<'_, S>);
            fn close(&mut self, out: &mut Outbox<'_, S>) -> Result<(), PlayError>;
            fn hold(&mut self, waker: Waker);
            fn release(&mut self);
        }
    }
}
