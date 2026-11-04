use kithara_bufpool::PoolRegion;
use kithara_effects::LimiterConfig;

use super::{LimiterNode, MetronomeNode, metronome::MetronomeConfig};
use crate::consts;

/// The session output chain after the mix: the limiter, then the metronome
/// whose click ducks the limited signal under it. Its processors take their
/// storage from the one region the chain holds.
#[derive_where::derive_where(Debug)]
pub(crate) struct SessionOutput<S> {
    limiter: LimiterConfig,
    pools: PoolRegion<S>,
}

impl<S> SessionOutput<S> {
    /// The output chain a session starts with, storing into `pools`.
    pub(crate) const fn new(limiter: LimiterConfig, pools: PoolRegion<S>) -> Self {
        Self { limiter, pools }
    }

    /// The limiter, its lookahead storage for every rate a session serves taken
    /// once when its processor is built.
    pub(crate) fn limiter(&self) -> LimiterNode<S> {
        LimiterNode::new(self.limiter, consts::MAX_SAMPLE_RATE, self.pools.clone())
    }

    /// The metronome whose click `config` shapes, under the limiter's ceiling.
    pub(crate) fn metronome(&self, config: MetronomeConfig) -> MetronomeNode {
        MetronomeNode::new(config, f32::from(self.limiter.ceiling()))
    }
}
