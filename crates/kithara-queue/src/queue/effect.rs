use kithara_bufpool::HasPool;
use kithara_command::When;
use kithara_config::{Configure, LiveConfig};
use kithara_events::TrackId;
use kithara_play::{
    EffectChange, EffectConfigView, EffectSnapshot, EffectTarget, EqBandConfig, FilterSettings,
    LaneId, PlayError,
};
use kithara_signal::SessionFrame;

use super::{QueueCommand, QueueControl};
use crate::QueueError;

/// Typed controls bound to one prepared effect in one loaded track.
#[derive_where::derive_where(Clone)]
#[non_exhaustive]
pub struct EffectControl<S, C: Copy>
where
    S: HasPool<u8> + Send + Sync + 'static,
{
    queue: QueueControl<S>,
    item: TrackId,
    lane: LaneId,
    target: EffectTarget,
    view: EffectConfigView<C>,
}

impl<S> QueueControl<S>
where
    S: HasPool<u8> + Send + Sync + 'static,
{
    /// Bind to the fixed filter in the current track's effect chain.
    /// # Errors
    /// Refuses an unloaded track or an entry that is not a filter.
    pub fn filter(&self, effect: usize) -> Result<EffectControl<S, FilterSettings>, QueueError> {
        let snapshot = self.view.read();
        let track = snapshot.track.as_ref().ok_or(PlayError::NotReady)?;
        let Some(EffectSnapshot::Filter { lane, settings }) = track.effects.get(effect) else {
            return Err(PlayError::InvalidConfiguration {
                reason: "filter is not installed".into(),
            }
            .into());
        };
        Ok(EffectControl {
            queue: self.clone(),
            item: track.item,
            lane: *lane,
            target: EffectTarget::Filter { effect },
            view: settings.clone(),
        })
    }

    /// Bind to a band of the current track without changing its settings.
    /// # Errors
    /// Refuses an unloaded track or an absent effect or band.
    pub fn eq_band(
        &self,
        effect: usize,
        band: usize,
    ) -> Result<EffectControl<S, EqBandConfig>, QueueError> {
        let snapshot = self.view.read();
        let track = snapshot.track.as_ref().ok_or(PlayError::NotReady)?;
        let Some(EffectSnapshot::Equalizer { lane, bands }) = track.effects.get(effect) else {
            return Err(PlayError::InvalidConfiguration {
                reason: "equalizer is not installed".into(),
            }
            .into());
        };
        let view = bands
            .get(band)
            .ok_or(PlayError::EqBandOutOfRange {
                band,
                bands: bands.len(),
            })?
            .clone();
        Ok(EffectControl {
            queue: self.clone(),
            item: track.item,
            lane: *lane,
            target: EffectTarget::EqBand { effect, band },
            view,
        })
    }
}

impl<S, C> Configure<C::Change> for EffectControl<S, C>
where
    S: HasPool<u8> + Send + Sync + 'static,
    C: LiveConfig,
    C::Change: Into<EffectChange>,
    C::Error: std::fmt::Display,
{
    type At = When<SessionFrame>;
    type Config = C;
    type Error = QueueError;
    type Output = ();

    fn configure(&self, change: C::Change, at: Self::At) -> Result<(), QueueError> {
        let change = C::check(change).map_err(|error| PlayError::InvalidConfiguration {
            reason: error.to_string(),
        })?;
        self.queue.call(QueueCommand::ConfigureEffect {
            item: self.item,
            lane: self.lane,
            target: self.target,
            change: change.into(),
            at,
        })
    }

    fn settings(&self) -> C {
        self.view.config()
    }
}
