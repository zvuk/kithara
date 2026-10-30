use kithara_effects::LimiterConfig;

use super::{LimiterNode, MetronomeNode, metronome::MetronomeConfig};

/// The session output chain after the mix: the limiter, then the metronome
/// whose click ducks the limited signal under it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct SessionOutput {
    limiter: LimiterConfig,
}

impl SessionOutput {
    /// The output chain a session starts with.
    pub(crate) const fn new(limiter: LimiterConfig) -> Self {
        Self { limiter }
    }

    pub(crate) fn limiter(self) -> LimiterNode {
        LimiterNode::new(self.limiter)
    }

    /// The metronome whose click `config` shapes, under the limiter's ceiling.
    pub(crate) fn metronome(self, config: MetronomeConfig) -> MetronomeNode {
        MetronomeNode::new(config, f32::from(self.limiter.ceiling()))
    }
}
