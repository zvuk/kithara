use std::marker::PhantomData;

use kithara_command::{Rejection, Seq, When};
use kithara_platform::maybe_send::MaybeSend;
use kithara_play::{DeckControl, DeckPass, HostedDeck, Outbox, PlayError};
use kithara_signal::{FrameCount, SessionFrame};

use crate::{HostSettingsChange, HostSettingsExec};

/// The existing identity used for decks and their beat grids.
pub type DeckId = kithara_warp::BeatGridId;

/// A visit to one held deck during its owner's current pass.
type DeckVisit<'a, S, D> = &'a mut (dyn FnMut(&mut D, &mut Outbox<'_, S>, DeckPass<'_>) + 'a);
/// A visit that also receives the held deck's identity.
type EachDeckVisit<'a, S, D> =
    &'a mut (dyn FnMut(DeckId, &mut D, &mut Outbox<'_, S>, DeckPass<'_>) + 'a);

/// The clock one owner pass reads once: its frame and delivery leads.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PassClock {
    /// The session frame read at the start of the pass.
    pub now: SessionFrame,
    /// Frames a batch that wakes the worker takes to reach the mixer.
    pub delivery: FrameCount,
    /// Frames a batch of deck parts takes to reach the mixer.
    pub deck_delivery: FrameCount,
}

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
    /// [`SessionError::HostQueueFull`](crate::session::SessionError::HostQueueFull)
    /// admission failures.
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
    /// Returns [`PlayError::Session`] with
    /// [`SessionError::DeckNotFound`](crate::session::SessionError::DeckNotFound) if the
    /// deck is absent, or [`PlayError::Closed`] if its channel or scope is retired.
    fn with_deck(
        &mut self,
        id: DeckId,
        visit: DeckVisit<'_, S, Self::Deck>,
    ) -> Result<(), PlayError>;
    /// The current pass's clock, absent before a render graph exists.
    fn clock(&self) -> Option<PassClock>;
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

/// Converts a concrete control-bearing deck into the owning layer's held face.
/// `Face` selects the source capability converted into [`HostOwner::Deck`].
pub trait HostRegistration<S, D: DeckControl, Face: ?Sized>: HostOwner<S> {
    /// Transfers the deck without duplicating its owner or control endpoint.
    fn into_deck(deck: D) -> Box<Self::Deck>;
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
