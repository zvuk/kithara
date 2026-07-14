use std::num::NonZeroU32;

use kithara_platform::sync::Arc;

use super::super::core::PlayerImpl;
use crate::resource::ResourceConfig;

struct ConfigPrep<'a> {
    player: &'a PlayerImpl,
}

impl ConfigPrep<'_> {
    fn prepare(&self, config: ResourceConfig) -> ResourceConfig {
        let bus = config
            .bus
            .or_else(|| Some(self.player.core.engine.bus().scoped()));
        let cancel = config
            .cancel
            .or_else(|| self.player.core.engine.cancel_token())
            .map(|parent| parent.child());
        let stretch = Some(Arc::clone(&self.player.core.timestretch));
        let host_sample_rate = NonZeroU32::new(self.player.core.engine.master_sample_rate())
            .or_else(|| NonZeroU32::new(self.player.core.engine.configured_sample_rate()));
        let mut decoder = config.decoder.clone();
        decoder.gapless_mode = self.player.core.gapless_mode;
        ResourceConfig {
            bus,
            cancel,
            pcm_pool: self.player.core.engine.pcm_pool().clone(),
            worker: Some(self.player.core.engine.worker().clone()),
            host_sample_rate,
            decoder,
            stretch,
            engine_load: Some(Arc::clone(&self.player.core.engine_load)),
            ..config
        }
    }
}

impl PlayerImpl {
    /// Apply shared worker, host sample rate, ABR, and bus to a resource
    /// config so the resource integrates with this player's engine.
    ///
    /// Call this before [`Resource::new`](crate::resource::Resource::new) to
    /// ensure the resource shares the player's decode thread and resampler is
    /// pre-initialised with the correct ratio. Callers that want a shared HTTP
    /// pool / tokio runtime must build their own downloader and attach it via
    /// [`ResourceConfig::with_downloader`] before passing the config in.
    #[must_use]
    pub fn prepare_config(&self, config: ResourceConfig) -> ResourceConfig {
        ConfigPrep { player: self }.prepare(config)
    }
}
