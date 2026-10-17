use std::num::NonZeroU32;

use kithara_command::{Seq, When};
use kithara_render::bridge::DeckPart;
use kithara_signal::SessionFrame;
use kithara_sync::SpeedCurve;

use super::{
    Player, PlayerConfig, PlayerImpl, Position, Settled, TrackCommand, TrackSettings,
    TrackSettingsChange, TrackSnapshot,
};
use crate::PlayError;

/// A player of one track: what a queue drives and builds the next track from.
pub trait Track<S>: Player<S, Command = TrackCommand<S>, Snapshot: AsRef<TrackSnapshot>> {
    /// Checks a configuration change without sending it. Receipt intake may
    /// change admission before a subsequent apply. Some contains the number of
    /// deck credits its immediate batch needs; None means no immediate batch,
    /// including unchanged settings and preparation deferred to a load receipt.
    ///
    /// # Errors
    /// Returns a checked change's refusal, Untimed, Late or a full lane or deck.
    fn admit(
        &mut self,
        change: TrackSettingsChange,
        at: When<SessionFrame>,
        out: &super::Outbox<'_, S>,
    ) -> Result<Option<usize>, PlayError>;

    /// The settings a track built after this one starts with: the applied ones and the changes still in flight.
    fn projected(&self) -> TrackSettings;

    /// The accepted speed law at the current segment's origin, if still intact.
    fn prepared_speed(&self) -> Option<&SpeedCurve>;

    /// Prepares the actual entry before its owner's timed batch is assembled.
    /// Returns false until the current segment's readiness receipt arrives.
    ///
    /// # Errors
    /// Returns a cue or admission refusal without starting the track.
    fn prepare_entry(
        &mut self,
        at: SessionFrame,
        out: &mut super::Outbox<'_, S>,
    ) -> Result<bool, PlayError>;

    /// Projects media position and instantaneous speed on the output frame.
    ///
    /// # Errors
    /// Returns when no uninterrupted slot mark or effective lane clock is known.
    fn planned(
        &self,
        at: SessionFrame,
        sample_rate: NonZeroU32,
    ) -> Result<(Position, f64), PlayError>;

    /// Projects the first media end through accepted speed and jump history.
    ///
    /// # Errors
    /// Returns when the playing segment has no usable mark or effective clock.
    fn planned_end(&self, sample_rate: NonZeroU32) -> Result<Option<SessionFrame>, PlayError>;

    /// Takes a final speed verdict already drained by the track's lane owner.
    fn speed_receipt(&mut self) -> Option<Settled>;

    /// Whether the lane accepted a particular speed batch, once it answered.
    fn speed_applied(&mut self, seq: Seq) -> Option<bool>;

    /// Binds a staged attachment to its shared batch or restores returned parts.
    fn finish_group(&mut self, result: Result<Seq, &mut Vec<DeckPart>>);

    /// Opens a silent cue segment with its synchronized speed in one operation.
    ///
    /// # Errors
    /// Returns a checked speed or an admission refusal before either send.
    fn cue(
        &mut self,
        position: Position,
        speed: SpeedCurve,
        out: &mut super::Outbox<'_, S>,
    ) -> Result<Option<Seq>, PlayError>;
}

/// How a queue builds the track that plays one item.
pub trait TrackFactory<S> {
    type Track: Track<S>;

    /// # Errors
    ///
    /// Returns the config's refusal.
    fn track(&self, config: PlayerConfig) -> Result<Self::Track, PlayError>;
}

/// Builds a bare `PlayerImpl`.
#[derive(Clone, Copy, Debug, Default)]
pub struct PlayerFactory;

impl<S> TrackFactory<S> for PlayerFactory {
    type Track = PlayerImpl<S>;

    fn track(&self, config: PlayerConfig) -> Result<Self::Track, PlayError> {
        PlayerImpl::new(config)
    }
}
