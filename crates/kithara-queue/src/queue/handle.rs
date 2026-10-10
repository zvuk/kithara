use kithara_bufpool::HasPool;
use kithara_command::{Refused, When};
use kithara_events::TrackId;
use kithara_play::{
    CrossfadeSettings, EqBandConfig, InterruptionKind, PlayError, Position, TrackSettingsChange,
};
use num_traits::ToPrimitive;

use super::{QueueCommand, QueueControl, Transition};
use crate::{
    ActionAtItemEnd, PlaybackOrder, QueueError, QueueSettingsChange, RepeatMode, TrackSource,
};

impl<S> QueueControl<S>
where
    S: HasPool<u8> + Send + Sync + 'static,
{
    /// Appends an item; only a transition target or automatic preload is opened.
    /// # Errors
    /// Returns the queue's refusal or a closed mailbox.
    pub fn append<T: Into<TrackSource<S>>>(&self, source: T) -> Result<TrackId, QueueError> {
        self.append_with_id(TrackId::allocate(), source)
    }

    /// Appends an item using an identity allocated by the caller.
    /// # Errors
    /// Returns the queue's refusal or a closed mailbox.
    pub fn append_with_id<T: Into<TrackSource<S>>>(
        &self,
        id: TrackId,
        source: T,
    ) -> Result<TrackId, QueueError> {
        self.call(QueueCommand::Append {
            id,
            source: source.into(),
        })?;
        Ok(id)
    }

    /// Inserts an item after an identity, or at the head.
    /// # Errors
    /// Returns an unknown identity, the queue's refusal or a closed mailbox.
    pub fn insert<T: Into<TrackSource<S>>>(
        &self,
        source: T,
        after: Option<TrackId>,
    ) -> Result<TrackId, QueueError> {
        self.insert_with_id(TrackId::allocate(), source, after)
    }

    /// Inserts an item using an identity allocated by the caller.
    /// # Errors
    /// Returns an unknown identity, the queue's refusal or a closed mailbox.
    pub fn insert_with_id<T: Into<TrackSource<S>>>(
        &self,
        id: TrackId,
        source: T,
        after: Option<TrackId>,
    ) -> Result<TrackId, QueueError> {
        self.call(QueueCommand::Insert {
            id,
            source: source.into(),
            after,
        })?;
        Ok(id)
    }

    pub fn play(&self) {
        let _ = self.call(QueueCommand::Play { at: When::Next });
    }
    pub fn pause(&self) {
        let _ = self.call(QueueCommand::Pause { at: When::Next });
    }

    /// Seeks the sounding track in media seconds.
    /// # Errors
    /// Returns an invalid position, the track's refusal or a closed mailbox.
    pub fn seek(&self, seconds: f64) -> Result<(), QueueError> {
        let value = seconds.to_f32().ok_or_else(|| {
            PlayError::Internal("position diagnostic cannot be represented as f32".into())
        })?;
        let to = Position::try_from_secs_f64(seconds).map_err(|_| PlayError::InvalidParameter {
            name: "position".into(),
            value,
        })?;
        self.call(QueueCommand::Seek { to })
    }

    /// Selects an identity; the published current item changes on its receipt.
    /// Answers once the owner accepted and sent it, not when RT applies it.
    /// # Errors
    /// Returns an unknown identity, an admission refusal or a closed mailbox.
    pub fn select(&self, id: TrackId, transition: Transition) -> Result<(), QueueError> {
        self.call(QueueCommand::Select { id, transition })
    }

    /// Moves after the last requested target.
    /// Answers once the owner accepted and sent it; effects publish on receipts.
    /// # Errors
    /// Returns an admission refusal or a closed mailbox.
    pub fn next(&self, transition: Transition) -> Result<(), QueueError> {
        self.call(QueueCommand::Next(transition))
    }

    /// Selects the previous identity in navigation history.
    /// Answers once the owner accepted and sent it; effects publish on receipts.
    /// # Errors
    /// Returns an admission refusal or a closed mailbox.
    pub fn previous(&self, transition: Transition) -> Result<(), QueueError> {
        self.call(QueueCommand::Previous(transition))
    }

    /// Removes an item.
    /// # Errors
    /// Returns an unknown identity, a release refusal or a closed mailbox.
    pub fn remove(&self, id: TrackId) -> Result<(), QueueError> {
        self.call(QueueCommand::Remove(id))
    }

    /// Removes all items and releases every active track.
    /// # Errors
    /// Returns a release refusal or a closed mailbox.
    pub fn clear(&self) -> Result<(), QueueError> {
        self.call(QueueCommand::RemoveAll)
    }

    /// Replaces the queue's items.
    /// # Errors
    /// Returns a release refusal or a closed mailbox.
    pub fn set_tracks<I, T>(&self, sources: I) -> Result<(), QueueError>
    where
        I: IntoIterator<Item = T>,
        T: Into<TrackSource<S>>,
    {
        self.call(QueueCommand::SetTracks(
            sources.into_iter().map(Into::into).collect(),
        ))
    }

    /// Requests an owner pass.
    /// # Errors
    /// Returns the queue's refusal or a closed mailbox.
    pub fn tick(&self) -> Result<(), QueueError> {
        self.call(QueueCommand::Tick)
    }

    /// Releases the tracks and cancels queue-owned work.
    /// # Errors
    /// Returns a release refusal or a closed mailbox.
    pub fn close(&self) -> Result<(), QueueError> {
        self.call(QueueCommand::Close)
    }

    /// Changes the track speed across every active track.
    /// # Errors
    /// Returns the broadcast's refusal or a closed mailbox.
    pub fn set_rate(&self, rate: f32) -> Result<(), QueueError> {
        self.call(QueueCommand::ConfigureTrack(
            TrackSettingsChange::Speed(rate),
            When::Next,
        ))
    }

    /// Changes the speed inherited by subsequent tracks.
    /// # Errors
    /// Returns the broadcast's refusal or a closed mailbox.
    pub fn set_default_rate(&self, rate: f32) -> Result<(), QueueError> {
        self.set_rate(rate)
    }

    /// Changes the next transition's crossfade profile.
    /// # Errors
    /// Returns an invalid profile or a closed mailbox.
    pub fn set_crossfade_settings(&self, settings: CrossfadeSettings) -> Result<(), QueueError> {
        self.call(QueueCommand::ConfigureQueue(
            QueueSettingsChange::Crossfade(settings),
            When::Next,
        ))
    }

    pub fn set_repeat(&self, mode: RepeatMode) {
        let _ = self.call(QueueCommand::SetRepeat(mode));
    }
    pub fn set_playback_order(&self, order: PlaybackOrder) {
        let _ = self.call(QueueCommand::SetPlaybackOrder(order));
    }
    pub fn set_action_at_item_end(&self, action: ActionAtItemEnd) {
        let _ = self.call(QueueCommand::SetActionAtItemEnd(action));
    }

    /// Forwards volume to the host owner.
    /// # Errors
    /// Returns the host's refusal or a closed mailbox.
    pub fn set_volume(&self, volume: f32) -> Result<(), QueueError> {
        self.call(QueueCommand::SetVolume(volume))
    }

    /// Forwards mix level to the host owner.
    /// # Errors
    /// Returns the host's refusal or a closed mailbox.
    pub fn set_level(&self, level: f32) -> Result<(), QueueError> {
        self.call(QueueCommand::SetLevel(level))
    }

    /// Forwards mute to the host owner.
    /// # Errors
    /// Returns the host's refusal or a closed mailbox.
    pub fn set_muted(&self, muted: bool) -> Result<(), QueueError> {
        self.call(QueueCommand::SetMuted(muted))
    }

    /// Forwards an EQ gain to the host owner.
    /// # Errors
    /// Returns the host's refusal or a closed mailbox.
    pub fn set_eq_gain(&self, band: usize, gain_db: f32) -> Result<(), QueueError> {
        self.call(QueueCommand::SetEqGain { band, gain_db })
    }

    /// Forwards an EQ layout to the host owner.
    /// # Errors
    /// Returns the host's refusal or a closed mailbox.
    pub fn set_eq_layout(&self, layout: Vec<EqBandConfig>) -> Result<(), QueueError> {
        self.call(QueueCommand::SetEqLayout(layout))
    }

    /// Requests flat host EQ gains.
    /// # Errors
    /// Returns the host's refusal or a closed mailbox.
    pub fn reset_eq(&self) -> Result<(), QueueError> {
        self.call(QueueCommand::ResetEq)
    }

    /// Posts an interruption to the deck owner.
    ///
    /// # Errors
    /// Returns [`QueueError::Play`] with [`PlayError::Closed`] if the queue or
    /// mailbox is closed, or with [`PlayError::NotReady`] if the outbox has no
    /// host session binding.
    pub fn notify_interruption(&self, kind: InterruptionKind) -> Result<(), QueueError> {
        self.call(QueueCommand::NotifyInterruption(kind))
    }

    /// Whether the queue is gone, so no command reaches it.
    #[must_use]
    pub fn is_closed(&self) -> bool {
        self.postbox.is_closed()
    }

    fn call(&self, command: QueueCommand<S>) -> Result<(), QueueError> {
        let ticket = self.postbox.post(command).map_err(|_| PlayError::Closed)?;
        ticket.wait().map_err(|refused| match refused {
            Refused::Owner(error) => error,
            Refused::Unanswered => PlayError::Closed.into(),
        })
    }
}
