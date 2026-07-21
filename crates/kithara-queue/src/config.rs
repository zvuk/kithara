use std::{fmt, num::NonZeroUsize};

use bon::Builder;
use kithara_assets::AssetStore;
use kithara_platform::CancelToken;

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
/// Holds queue-level defaults. A queue that decorates an existing player is
/// constructed through [`FromWithParams`](kithara_platform::traits::FromWithParams)
/// instead of storing a component inside configuration.
///
/// [`TrackSource::Uri`](crate::TrackSource::Uri) resources share this queue's
/// store. A caller-supplied [`ResourceConfig`](kithara_play::ResourceConfig)
/// retains its own store.
#[derive(Clone, Builder, fieldwork::Fieldwork)]
#[builder(state_mod(vis = "pub"))]
#[non_exhaustive]
#[fieldwork(opt_in, with)]
pub struct QueueConfig {
    /// Max concurrent background prefetch loads. Default: 3.
    #[builder(default = DEFAULT_MAX_CONCURRENT_LOADS)]
    pub max_concurrent_loads: NonZeroUsize,

    /// Master cancel for the queue. `Some` threads the app master so the
    /// queue subtree cascades from one app-wide owner; `None` falls back
    /// to a fresh standalone token (test / library use). Must never be
    /// `None` on the production app path.
    #[field(with, option_set_some)]
    pub cancel: Option<CancelToken>,

    /// Shared store used for bare URI track sources.
    #[field(with, option_set_some)]
    pub store: Option<AssetStore>,

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
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::*;

    #[kithara::test]
    fn default_config_has_reasonable_loader_cap() {
        let cfg = QueueConfig::default();
        assert_eq!(cfg.max_concurrent_loads.get(), 3);
        assert!(cfg.store.is_none());
        assert!((cfg.prefetch_duration - 3.5).abs() < f32::EPSILON);
    }
}
