use kithara_command::Seq;
use kithara_play::{HostedDeck, Outbox, PlayError};
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
    /// Delivers analysis to the track owning the exact item and load.
    fn grid(&mut self, answer: GridAnswer, out: &mut Outbox<'_, S>);
    /// Whether this deck follows the Host's tempo.
    fn synced(&self) -> bool;
    /// Maximum lane lead among sounding synchronized tracks.
    fn lead(&self, delivery: FrameCount) -> Option<FrameCount>;
    /// Minimum available room among lanes that would receive a retime.
    fn lane_room(&self) -> usize;
    /// Scope batches needed to reopen silent tracks during a retime.
    fn scope_parts(&self) -> usize;
    /// Whether any lane applied the speed batch of this retime.
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
