use std::{num::NonZeroUsize, sync::Arc};

use derivative::Derivative;
use derive_setters::Setters;
use kithara_assets::StoreOptions;
use kithara_net::NetOptions;
use kithara_play::PlayerImpl;

/// Default parallelism cap for async track loads.
const DEFAULT_MAX_CONCURRENT_LOADS: NonZeroUsize = match NonZeroUsize::new(3) {
    Some(n) => n,
    None => unreachable!(),
};

/// Configuration for a [`Queue`](crate::Queue).
///
/// Holds queue-level defaults plus an optional externally-owned
/// [`PlayerImpl`] instance. Matches the project-wide pattern where
/// config structs accept optional built instances (see
/// [`ResourceConfig::worker`](kithara_play::ResourceConfig::worker) /
/// [`runtime`](kithara_play::ResourceConfig::runtime) /
/// [`bus`](kithara_play::ResourceConfig::bus)) rather than re-taking
/// their own construction parameters.
///
/// - [`QueueConfig::player`] — if `None`, [`Queue::new`](crate::Queue::new)
///   builds a default [`PlayerImpl`].
/// - [`QueueConfig::net`] + [`QueueConfig::store`] — used as templates
///   for [`TrackSource::Uri`](crate::TrackSource::Uri) entries.
///   Caller-built [`TrackSource::Config`](crate::TrackSource::Config)
///   values are left intact (DRM keys, headers, hints).
#[derive(Clone, Derivative, Setters)]
#[derivative(Debug, Default)]
#[setters(prefix = "with_", strip_option)]
pub struct QueueConfig {
    /// Externally-owned player. `None` means Queue builds a default.
    #[setters(skip)]
    #[derivative(Debug = "ignore")]
    pub player: Option<Arc<PlayerImpl>>,

    /// Default network options for `Uri`-sourced tracks. Timeouts, retries.
    #[setters(skip)]
    pub net: NetOptions,

    /// Default storage options for `Uri`-sourced tracks. Cache dir, eviction.
    #[setters(skip)]
    pub store: StoreOptions,

    /// Max concurrent `Loader` in-flight loads. Default: 3.
    #[derivative(Default(value = "DEFAULT_MAX_CONCURRENT_LOADS"))]
    pub max_concurrent_loads: NonZeroUsize,

    /// Whether the Queue should start playing as soon as the first track
    /// enters [`TrackStatus::Loaded`](crate::TrackStatus::Loaded).
    /// Default: `false`.
    pub autoplay: bool,
}

impl QueueConfig {
    /// Create a new [`QueueConfig`] with all defaults. Equivalent to
    /// [`QueueConfig::default`].
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Replace the [`PlayerImpl`] instance.
    #[must_use]
    pub fn with_player(mut self, player: Arc<PlayerImpl>) -> Self {
        self.player = Some(player);
        self
    }

    /// Replace the [`NetOptions`] template.
    #[must_use]
    pub fn with_net(mut self, net: NetOptions) -> Self {
        self.net = net;
        self
    }

    /// Replace the [`StoreOptions`] template.
    #[must_use]
    pub fn with_store(mut self, store: StoreOptions) -> Self {
        self.store = store;
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_has_reasonable_loader_cap() {
        let cfg = QueueConfig::default();
        assert_eq!(cfg.max_concurrent_loads.get(), 3);
        assert!(!cfg.autoplay);
        assert!(cfg.player.is_none());
    }

    #[test]
    fn with_autoplay_sets_field() {
        let cfg = QueueConfig::default().with_autoplay(true);
        assert!(cfg.autoplay);
    }
}
