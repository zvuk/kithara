use std::num::NonZeroUsize;

use bon::Builder;
use kithara_assets::AssetStore;
use kithara_bufpool::HasPool;
use kithara_derive::Patch;
use kithara_platform::{CancelToken, tokio::runtime::Handle as RuntimeHandle};
use kithara_play::{CrossfadeSettings, PlayerImpl};

use crate::{ActionAtItemEnd, PlaybackOrder, consts};

/// Configuration for a [`Queue`](crate::Queue).
///
/// Holds queue-level defaults plus the owned [`PlayerImpl`] instance whose
/// item list the queue coordinates.
///
/// [`TrackSource::Uri`](crate::TrackSource::Uri) resources share this queue's
/// store. A caller-supplied [`ResourceConfig`](kithara_play::ResourceConfig)
/// retains its own store.
#[derive(Builder, derive_more::Debug, Patch)]
#[builder(state_mod(vis = "pub"))]
#[non_exhaustive]
pub struct QueueConfig<S>
where
    S: HasPool<u8> + HasPool<f32> + Send + Sync + 'static,
{
    #[builder(default)]
    pub action_at_item_end: ActionAtItemEnd,

    #[builder(default)]
    pub crossfade_settings: CrossfadeSettings,

    /// Max concurrent background prefetch loads. Default: 3.
    #[builder(default = consts::DEFAULT_MAX_CONCURRENT_LOADS)]
    pub max_concurrent_loads: NonZeroUsize,

    /// Master cancel for the queue. `Some` threads the app master so the
    /// queue subtree cascades from one app-wide owner; `None` falls back
    /// to a fresh standalone token (test / library use). Must never be
    /// `None` on the production app path.
    #[patch(skip)]
    #[debug(skip)]
    pub cancel: Option<CancelToken>,

    /// Shared store used for bare URI track sources.
    #[patch(skip)]
    #[debug(skip)]
    pub store: Option<AssetStore<S>>,

    #[builder(default)]
    pub playback_order: PlaybackOrder,

    /// Runtime the queue runs its loads and load completions on. `None`
    /// takes the runtime current where the queue is built; an embedding
    /// that drives the queue from threads without one (FFI hosts) passes
    /// its own.
    #[patch(skip)]
    #[debug(skip)]
    pub runtime: Option<RuntimeHandle>,

    /// Player owned and decorated by this queue.
    #[patch(skip)]
    #[debug(skip)]
    pub player: PlayerImpl<S>,

    /// Whether the queue starts playback by itself once the first track
    /// appended to a queue with nothing selected finishes loading. Off by
    /// default: the embedding decides when playback starts. A document cannot
    /// name it, because starting playback is the embedding's choice.
    #[builder(default = false)]
    #[patch(skip)]
    pub should_autoplay: bool,

    /// Lead time in seconds before EOF at which the next queued track
    /// is preloaded into the audio processor. Default: 3.5. Stays `f32`
    /// seconds rather than the campaign's `humantime` duration convention:
    /// the value already reaches 10 setter and 14 read call sites as a bare
    /// `f32`, and converting the type would only churn those for a
    /// formatting preference.
    #[builder(default = consts::DEFAULT_PREFETCH_DURATION)]
    pub prefetch_duration: f32,

    /// Entries the navigation history keeps. Only explicit selections and
    /// auto-advances land there, so the default is a listening session's
    /// worth of back-steps; the queue's own track list is unbounded.
    #[builder(default = 100)]
    pub max_history_size: usize,
}

#[cfg(test)]
mod tests {
    use kithara_play::{PlayWorker, PlayWorkerConfig, PlayerConfig};
    use kithara_test_utils::kithara;

    use super::*;
    use crate::{queue::test_session, test_pools::pools};

    pub(super) fn config() -> QueueConfig<crate::test_pools::TestPools> {
        let worker = PlayWorker::new(PlayWorkerConfig::builder(pools()).build());
        let player = PlayerImpl::new(
            PlayerConfig::builder()
                .sample_rate(consts::TEST_SAMPLE_RATE)
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
}
