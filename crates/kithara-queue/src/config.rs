use std::num::NonZeroUsize;

use derivative::Derivative;
use derive_setters::Setters;
use kithara_assets::StoreOptions;
use kithara_net::NetOptions;
use kithara_play::PlayerConfig;

/// Default parallelism cap for async track loads.
const DEFAULT_MAX_CONCURRENT_LOADS: NonZeroUsize = match NonZeroUsize::new(3) {
    Some(n) => n,
    None => unreachable!(),
};

/// Configuration for a [`Queue`](crate::Queue).
///
/// Composes existing `kithara-play`, `kithara-net`, and `kithara-assets`
/// configs rather than duplicating their fields. Forward-composition keeps
/// the Queue layer thin: bumps to `PlayerConfig` / `NetOptions` /
/// `StoreOptions` show up for free.
///
/// - [`QueueConfig::player`] is passed to `PlayerImpl::new`.
/// - [`QueueConfig::net`] + [`QueueConfig::store`] are used as templates for
///   [`TrackSource::Uri`](crate::TrackSource::Uri) entries. Caller-built
///   [`TrackSource::Config`](crate::TrackSource::Config) values are left
///   intact (DRM keys, headers, hints).
#[derive(Clone, Derivative, Setters)]
#[derivative(Debug, Default)]
#[setters(prefix = "with_", strip_option)]
pub struct QueueConfig {
    /// Forwarded to `PlayerImpl::new`.
    #[setters(skip)]
    pub player: PlayerConfig,

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
    /// Create a new [`QueueConfig`] with the given [`PlayerConfig`] and
    /// defaults for everything else.
    #[must_use]
    pub fn new(player: PlayerConfig) -> Self {
        Self {
            player,
            net: NetOptions::default(),
            store: StoreOptions::default(),
            max_concurrent_loads: DEFAULT_MAX_CONCURRENT_LOADS,
            autoplay: false,
        }
    }

    /// Replace the [`PlayerConfig`] wholesale.
    #[must_use]
    pub fn with_player(mut self, player: PlayerConfig) -> Self {
        self.player = player;
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
    }

    #[test]
    fn with_autoplay_sets_field() {
        let cfg = QueueConfig::default().with_autoplay(true);
        assert!(cfg.autoplay);
    }
}
