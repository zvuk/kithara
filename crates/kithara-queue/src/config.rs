use std::{
    num::NonZeroUsize,
    sync::{Mutex, PoisonError},
};

use bon::Builder;
use kithara_assets::AssetStore;
use kithara_bufpool::HasPool;
use kithara_derive::Patch;
use kithara_platform::{CancelToken, sync::Arc, tokio::runtime::Handle as RuntimeHandle};
use kithara_play::{CrossfadeSettings, PlayerImpl};

use crate::{ActionAtItemEnd, PlaybackOrder, navigation::NavigationState};

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
#[kithara_config::config(builder = false)]
#[derive(Builder, derive_more::Debug, Patch)]
#[builder(state_mod(vis = "pub"))]
#[non_exhaustive]
pub struct QueueConfig<S>
where
    S: HasPool<u8> + HasPool<f32> + Send + Sync + 'static,
{
    /// The navigation owner attached when the queue is constructed.
    #[config(skip = "navigation owns the live traversal order", builder(field = None), patch(skip))]
    pub(crate) navigation: Option<Arc<Mutex<NavigationState>>>,

    /// Max concurrent background prefetch loads. Default: 3.
    #[config(value, sdk, builder(default = DEFAULT_MAX_CONCURRENT_LOADS))]
    pub max_concurrent_loads: NonZeroUsize,

    /// Master cancel for the queue. `Some` threads the app master so the
    /// queue subtree cascades from one app-wide owner; `None` falls back
    /// to a fresh standalone token (test / library use). Must never be
    /// `None` on the production app path.
    #[config(skip = "injected cancellation resource", patch(skip))]
    #[debug(skip)]
    pub cancel: Option<CancelToken>,

    /// Shared store used for bare URI track sources.
    #[config(skip = "injected asset store", patch(skip))]
    #[debug(skip)]
    pub store: Option<AssetStore<S>>,

    /// Runtime the queue runs its loads and load completions on. `None`
    /// takes the runtime current where the queue is built; an embedding
    /// that drives the queue from threads without one (FFI hosts) passes
    /// its own.
    #[config(skip = "injected runtime", patch(skip))]
    #[debug(skip)]
    pub runtime: Option<RuntimeHandle>,

    /// Player owned and decorated by this queue.
    #[config(skip = "player moves to the queue owner", builder(required, with = |value: PlayerImpl<S>| Some(value)), patch(skip))]
    #[debug(skip)]
    pub(crate) player: Option<PlayerImpl<S>>,

    /// Lead time in seconds before EOF at which the next queued track is
    /// preloaded into the audio processor. Default: 3.5.
    #[config(value, sdk, builder(default = DEFAULT_PREFETCH_DURATION))]
    pub prefetch_duration: f32,

    /// Whether the queue starts playback by itself once the first track
    /// appended to a queue with nothing selected finishes loading. Off by
    /// default: the embedding decides when playback starts. A document cannot
    /// name it, because starting playback is the embedding's choice.
    #[config(value, sdk, builder(default = false), patch(skip))]
    pub should_autoplay: bool,

    /// Entries the navigation history keeps. Only explicit selections and
    /// auto-advances land there, so the default is a listening session's
    /// worth of back-steps; the queue's own track list is unbounded.
    #[config(value, sdk, builder(default = 100))]
    pub max_history_size: usize,

    /// Initial queue traversal order; subsequent changes belong to navigation.
    #[config(value(PlaybackOrder, self.live_playback_order()), sdk, builder(default))]
    pub playback_order: PlaybackOrder,

    /// Initial action when the current item ends.
    #[config(
        value(ActionAtItemEnd, self.action_at_item_end()),
        sdk,
        builder(default = Mutex::new(ActionAtItemEnd::default()), with = |value: ActionAtItemEnd| Mutex::new(value)),
        patch(wire = ActionAtItemEnd, from = Mutex::new)
    )]
    pub(crate) action_at_item_end: Mutex<ActionAtItemEnd>,

    /// Initial transition settings for the next item.
    #[config(
        value(CrossfadeSettings, self.crossfade_settings()),
        sdk,
        builder(default = Mutex::new(CrossfadeSettings::default()), with = |value: CrossfadeSettings| Mutex::new(value)),
        patch(wire = CrossfadeSettings, from = Mutex::new)
    )]
    pub(crate) crossfade_settings: Mutex<CrossfadeSettings>,
}

impl<S> QueueConfig<S>
where
    S: HasPool<u8> + HasPool<f32> + Send + Sync + 'static,
{
    pub(crate) fn action_at_item_end(&self) -> ActionAtItemEnd {
        *self
            .action_at_item_end
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    pub(crate) fn crossfade_settings(&self) -> CrossfadeSettings {
        *self
            .crossfade_settings
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    fn live_playback_order(&self) -> PlaybackOrder {
        self.navigation
            .as_ref()
            .map_or(self.playback_order, |navigation| {
                navigation
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .playback_order()
            })
    }

    pub(crate) fn set_action_at_item_end(&self, action: ActionAtItemEnd) {
        *self
            .action_at_item_end
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = action;
    }

    pub(crate) fn set_crossfade_settings(&self, settings: CrossfadeSettings) {
        *self
            .crossfade_settings
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = settings;
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
}
