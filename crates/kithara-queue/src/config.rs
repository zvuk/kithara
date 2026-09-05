use std::{fmt, num::NonZeroUsize};

use bon::Builder;
use kithara_assets::AssetStore;
use kithara_bufpool::HasPool;
use kithara_macros::Patch;
use kithara_platform::CancelToken;
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
/// Holds queue-level defaults plus the owned [`PlayerImpl`] instance whose
/// item list the queue coordinates.
///
/// [`TrackSource::Uri`](crate::TrackSource::Uri) resources share this queue's
/// store. A caller-supplied [`ResourceConfig`](kithara_play::ResourceConfig)
/// retains its own store.
#[derive(Builder, Patch)]
#[builder(state_mod(vis = "pub"))]
#[non_exhaustive]
pub struct QueueConfig<S>
where
    S: HasPool<u8> + HasPool<f32> + Send + Sync + 'static,
{
    /// Master cancel for the queue. `Some` threads the app master so the
    /// queue subtree cascades from one app-wide owner; `None` falls back
    /// to a fresh standalone token (test / library use). Must never be
    /// `None` on the production app path.
    #[patch(skip)]
    pub cancel: Option<CancelToken>,

    /// Player owned and decorated by this queue.
    #[patch(skip)]
    pub player: PlayerImpl<S>,

    /// Shared store used for bare URI track sources.
    #[patch(skip)]
    pub store: Option<AssetStore<S>>,

    /// Max concurrent background prefetch loads. Default: 3.
    #[builder(default = DEFAULT_MAX_CONCURRENT_LOADS)]
    pub max_concurrent_loads: NonZeroUsize,

    /// Whether the queue auto-starts playback once the first registered track
    /// finishes loading. A document cannot name this: the field is read only
    /// under `cfg(any(test, feature = "probe"))` (`queue/lifecycle.rs`), and
    /// `kithara-app` ships without `probe`, so a document key would configure
    /// nothing in the binary. It carries `#[patch(skip)]` for that reason, and
    /// naming it is refused rather than silently dropped.
    #[builder(default = true)]
    #[patch(skip)]
    pub should_autoplay: bool,

    /// Lead time in seconds before EOF at which the next queued track
    /// is preloaded into the audio processor. Default: 3.5. Stays `f32`
    /// seconds rather than the campaign's `humantime` duration convention:
    /// the value already reaches 10 setter and 14 read call sites as a bare
    /// `f32`, and converting the type would only churn those for a
    /// formatting preference.
    #[builder(default = DEFAULT_PREFETCH_DURATION)]
    pub prefetch_duration: f32,

    /// Entries the navigation history keeps. Only explicit selections and
    /// auto-advances land there, so the default is a listening session's
    /// worth of back-steps; the queue's own track list is unbounded.
    #[builder(default = 100)]
    pub max_history_size: usize,
}

impl<S> fmt::Debug for QueueConfig<S>
where
    S: HasPool<u8> + HasPool<f32> + Send + Sync + 'static,
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("QueueConfig")
            .field("max_concurrent_loads", &self.max_concurrent_loads)
            .field("prefetch_duration", &self.prefetch_duration)
            .field("max_history_size", &self.max_history_size)
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use kithara_play::{PlayWorker, PlayWorkerConfig, PlayerConfig};
    use kithara_test_utils::kithara;

    use super::*;
    use crate::{
        queue::{TEST_SAMPLE_RATE, test_session},
        test_pools::pools,
    };

    pub(super) fn config() -> QueueConfig<crate::test_pools::TestPools> {
        let worker = PlayWorker::new(PlayWorkerConfig::builder(pools()).build());
        let player = PlayerImpl::new(
            PlayerConfig::builder()
                .sample_rate(TEST_SAMPLE_RATE)
                .worker(worker)
                .session(test_session())
                .build(),
        );
        QueueConfig::builder().player(player).build()
    }

    #[kithara::test]
    fn default_config_has_reasonable_loader_cap() {
        let cfg = config();

        assert_eq!(cfg.max_concurrent_loads.get(), 3);
        assert!(cfg.store.is_none());
        assert!((cfg.prefetch_duration - 3.5).abs() < f32::EPSILON);
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod document_tests {
    use kithara_test_utils::kithara;

    use super::{QueueConfigPatch, tests::config};

    #[kithara::test(native, flash(false))]
    fn a_document_sets_the_load_cap_and_leaves_the_history_size() {
        let patch: QueueConfigPatch =
            serde_yaml_ng::from_str("max_concurrent_loads: 5\n").expect("the document types");
        // Seeded off the crate default so a merge that reset every unnamed
        // field could not pass this by coincidence.
        let mut config = config();
        config.max_history_size = 37;

        config.apply(patch);

        assert_eq!(config.max_concurrent_loads.get(), 5);
        assert_eq!(
            config.max_history_size, 37,
            "a key the document does not name must keep its seeded value"
        );
    }

    /// `concurrent_load_cap` is neither a real key nor a substring of one,
    /// so the refusal cannot pass off serde's list of valid names.
    #[kithara::test(native, flash(false))]
    fn an_unknown_field_is_rejected_and_named() {
        let error = serde_yaml_ng::from_str::<QueueConfigPatch>("concurrent_load_cap: 5\n")
            .expect_err("a typo must not be silently ignored");

        assert!(error.to_string().contains("concurrent_load_cap"), "{error}");
    }

    /// `should_autoplay` is read only under `cfg(any(test, feature = "probe"))`
    /// and `kithara-app` ships without `probe`, so a document key would
    /// configure nothing in the binary. Naming it is refused rather than
    /// silently dropped.
    #[kithara::test(native, flash(false))]
    fn the_probe_only_autoplay_flag_is_not_a_document_key() {
        let error = serde_yaml_ng::from_str::<QueueConfigPatch>("should_autoplay: false\n")
            .expect_err("a flag the shipped binary never reads must not be document-settable");

        assert!(error.to_string().contains("should_autoplay"), "{error}");
    }
}
