use std::{num::NonZeroU32, sync::Arc};

use super::super::core::PlayerImpl;
use crate::resource::ResourceConfig;

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
        let bus = config.bus.or_else(|| Some(self.core.engine.bus().scoped()));
        let cancel = config
            .cancel
            .or_else(|| self.core.engine.cancel_token())
            .map(|parent| parent.child());
        let stretch = Some(Arc::clone(&self.core.timestretch));
        let host_sample_rate = NonZeroU32::new(self.core.engine.master_sample_rate())
            .or_else(|| NonZeroU32::new(self.core.engine.configured_sample_rate()));
        ResourceConfig {
            bus,
            cancel,
            worker: Some(self.core.engine.worker().clone()),
            host_sample_rate,
            gapless_mode: self.core.gapless_mode,
            stretch,
            engine_load: Some(Arc::clone(&self.core.engine_load)),
            ..config
        }
    }
}
