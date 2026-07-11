use std::{fmt, num::NonZeroUsize};

use bon::Builder;
use kithara_platform::{CancelToken, sync::Arc};
use kithara_play::PlayerImpl;

/// Default parallelism cap for async track loads.
pub(crate) const DEFAULT_MAX_CONCURRENT_LOADS: NonZeroUsize = match NonZeroUsize::new(3) {
    Some(n) => n,
    None => unreachable!(),
};

/// Default prefetch lead time before EOF, in seconds.
///
/// Mirrors `kithara_play::PlayerConfig::prefetch_duration` default.
pub(crate) const DEFAULT_PREFETCH_DURATION: f32 = 3.5;

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
#[derive(Clone, Builder)]
#[builder(state_mod(vis = "pub"))]
#[non_exhaustive]
pub struct QueueConfig {
    /// Max concurrent `Loader` in-flight loads. Default: 3.
    #[builder(default = DEFAULT_MAX_CONCURRENT_LOADS)]
    pub max_concurrent_loads: NonZeroUsize,

    /// Master cancel for the queue. `Some` threads the app master so the
    /// queue subtree cascades from one app-wide owner; `None` falls back
    /// to a fresh standalone token (test / library use). Must never be
    /// `None` on the production app path.
    pub cancel: Option<CancelToken>,

    /// Externally-owned player. `None` means Queue builds a default.
    pub player: Option<Arc<PlayerImpl>>,

    /// Whether the queue auto-advances to the next track at EOF.
    #[builder(default = true)]
    pub should_autoplay: bool,

    /// Lead time in seconds before EOF at which the next queued track
    /// is preloaded into the audio processor. Default: 3.5.
    #[builder(default = DEFAULT_PREFETCH_DURATION)]
    pub prefetch_duration: f32,
}

impl fmt::Debug for QueueConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("QueueConfig")
            .field("max_concurrent_loads", &self.max_concurrent_loads)
            .field("should_autoplay", &self.should_autoplay)
            .field("prefetch_duration", &self.prefetch_duration)
            .finish_non_exhaustive()
    }
}

impl Default for QueueConfig {
    fn default() -> Self {
        Self::builder().build()
    }
}

impl QueueConfig {
    /// Create a new [`QueueConfig`] with all defaults.
    #[must_use]
    // ast-grep-ignore: style.prefer-default-derive
    pub fn new() -> Self {
        Self::default()
    }

    /// Thread an app-wide master cancel so the queue subtree derives from
    /// a single owner instead of minting its own root.
    #[must_use]
    pub fn with_cancel(mut self, cancel: CancelToken) -> Self {
        self.cancel = Some(cancel);
        self
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
        assert!((cfg.prefetch_duration - 3.5).abs() < f32::EPSILON);
    }
}
