use std::{num::NonZeroUsize, sync::Arc};

use derivative::Derivative;
use derive_setters::Setters;
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
/// Network and storage options are owned by
/// [`ResourceConfig`](kithara_play::ResourceConfig). Callers that need
/// non-default net/store behavior (custom timeouts, insecure certs,
/// alternative cache dir) build a configured [`ResourceConfig`] and
/// pass it via [`TrackSource::Config`](crate::TrackSource::Config).
/// [`TrackSource::Uri`](crate::TrackSource::Uri) uses the
/// [`ResourceConfig::new`](kithara_play::ResourceConfig::new) defaults.
#[derive(Clone, Derivative, Setters)]
#[derivative(Debug, Default)]
#[setters(prefix = "with_", strip_option)]
pub struct QueueConfig {
    /// Max concurrent `Loader` in-flight loads. Default: 3.
    #[derivative(Default(value = "DEFAULT_MAX_CONCURRENT_LOADS"))]
    pub max_concurrent_loads: NonZeroUsize,

    /// Externally-owned player. `None` means Queue builds a default.
    #[setters(skip)]
    #[derivative(Debug = "ignore")]
    pub player: Option<Arc<PlayerImpl>>,
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
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::*;

    #[kithara::test]
    fn default_config_has_reasonable_loader_cap() {
        let cfg = QueueConfig::default();
        assert_eq!(cfg.max_concurrent_loads.get(), 3);
        assert!(cfg.player.is_none());
    }
}
