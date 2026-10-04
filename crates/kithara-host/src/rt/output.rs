use kithara_config::{CheckedConfig, LiveConfig};
use kithara_effects::LimiterConfig;

use super::{
    LimiterNode, MetronomeNode,
    metronome::{MetronomeConfig, MetronomeConfigChange},
};
use crate::PlayError;

/// The session output chain after the mix: the limiter, then the metronome
/// whose click ducks the limited signal under it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct SessionOutput {
    limiter: LimiterConfig,
    metronome: MetronomeConfig,
}

impl SessionOutput {
    /// The output chain a session starts with.
    ///
    /// # Errors
    ///
    /// Returns [`PlayError::InvalidParameter`] naming the first metronome
    /// field out of its bounds.
    pub(crate) fn new(
        limiter: LimiterConfig,
        metronome: MetronomeConfig,
    ) -> Result<Self, PlayError> {
        Ok(Self {
            limiter,
            metronome: metronome.validated()?,
        })
    }

    pub(crate) fn limiter(&self) -> LimiterNode {
        LimiterNode::new(self.limiter)
    }

    pub(crate) fn metronome(&self, enabled: bool) -> MetronomeNode {
        MetronomeNode::new(enabled, self.metronome, self.limiter.ceiling())
    }

    /// Keeps `level` for every metronome node built from here on.
    pub(crate) fn set_metronome_level(&mut self, level: f32) -> Result<(), PlayError> {
        let change = MetronomeConfig::check(MetronomeConfigChange::Level(level))?;
        self.metronome.apply_change(change);
        Ok(())
    }
}
