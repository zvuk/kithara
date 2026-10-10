use std::num::NonZeroUsize;

use kithara_assets::AssetStore;
use kithara_bufpool::HasPool;
use kithara_config::Config;
use kithara_derive::Patch;
use kithara_platform::{CancelToken, time::Duration, tokio::runtime::Handle as RuntimeHandle};
use kithara_play::{
    CrossfadeSettings, CrossfadeSettingsPatch, CrossfadeSettingsPatchError, DeckMixerConfig,
    DeckMixerConfigPatch, DeckMixerConfigPatchError, PlayerFactory, ResourcePrep, TrackFactory,
    TrackSettings, TrackSettingsPatch, TrackSettingsPatchError,
};

use crate::{ActionAtItemEnd, PlaybackOrder, consts};

/// What a [`Queue`](crate::Queue) runs with that changes while it runs. The
/// queue executes both fields itself; a change at a session frame is refused
/// as untimed.
#[derive(Clone, Copy, Debug, PartialEq, Config, Patch)]
#[config(default, fields(value, get(copy)), patch(fallible))]
pub struct QueueSettings {
    /// How one track hands over to the next.
    #[config(live, builder(default), patch(nested, fallible))]
    crossfade: CrossfadeSettings,
    /// Whether the next track starts on the frame after the current one ends
    /// instead of crossfading into it.
    #[config(live, builder(default = false))]
    gapless: bool,
}

/// The one parameter a [`Queue`](crate::Queue) is built from.
///
/// [`TrackSource::Uri`](crate::TrackSource::Uri) resources share this queue's
/// store. A caller-supplied [`ResourceConfig`](kithara_play::ResourceConfig)
/// retains its own store.
#[derive(Patch, Config)]
#[config(debug, builder(start_fn(name = with_factory), state_mod(vis = "pub")), fields(value))]
#[patch(fallible)]
#[non_exhaustive]
pub struct QueueConfig<S, F = PlayerFactory>
where
    S: HasPool<u8> + Send + Sync + 'static,
    F: TrackFactory<S>,
{
    /// Builds every track this queue plays.
    #[config(
        skip = "injected track factory",
        builder(start_fn),
        patch(skip),
        debug(skip)
    )]
    pub(crate) factory: F,

    /// The deck's mixer: its owner builds it from this when it registers the
    /// queue; the queue reads its slot count and fade lengths here.
    #[config(builder(default), patch(nested, fallible), debug(skip))]
    pub(crate) mixer: DeckMixerConfig,

    /// The live fields the queue executes itself.
    #[config(builder(default), patch(nested, fallible), debug(skip))]
    pub(crate) settings: QueueSettings,

    /// Session time before the current track ends at which the queue loads
    /// its successor. Fixed for the queue's lifetime. Default: 3.5 s.
    #[config(builder(default = consts::DEFAULT_PRELOAD_LEAD), patch(skip), debug(skip))]
    pub(crate) preload_lead: Duration,

    /// The settings a new track starts with; a track change moves them.
    #[config(builder(default), patch(nested, fallible), debug(skip))]
    pub(crate) track: TrackSettings,

    /// What every track the queue loads opens with: the worker and playback policy.
    #[config(skip = "injected by the deck's owner", builder(required, with = Some), patch(skip), debug(skip))]
    pub(crate) prep: Option<ResourcePrep<S>>,

    /// Master cancel for the queue. `Some` threads the app master so the
    /// queue subtree cascades from one app-wide owner; `None` falls back
    /// to a fresh standalone token (test / library use). Must never be
    /// `None` on the production app path.
    #[config(skip = "injected cancellation resource", patch(skip), debug(skip))]
    pub(crate) cancel: Option<CancelToken>,

    /// Shared store used for bare URI track sources; unset, the queue builds one
    /// from its prep worker's pools.
    #[config(skip = "injected asset store", patch(skip), debug(skip))]
    pub(crate) store: Option<AssetStore<S>>,

    /// Background loads the queue keeps open at once: appended tracks opened
    /// ahead of a selection and held off the deck. Tracks beyond it stay
    /// `Pending` until a place frees. The bound counts parked tracks even after
    /// their open is answered, not just in-flight opens. Default: 3.
    #[config(sdk, builder(default = consts::DEFAULT_MAX_CONCURRENT_LOADS))]
    pub(crate) max_concurrent_loads: NonZeroUsize,

    /// Runtime the tasks beside each load run on: the cover read and the
    /// slow-transfer watch. `None` takes the runtime current where the queue
    /// is built; a queue built outside one with none passed fails every load
    /// with [`QueueError::NoRuntime`](crate::QueueError::NoRuntime).
    #[config(skip = "injected runtime", patch(skip), debug(skip))]
    pub(crate) runtime: Option<RuntimeHandle>,

    /// Whether the initial target (the first track appended to a queue with
    /// nothing selected) starts playing once it loads; when off, it loads and
    /// enters paused. A document cannot
    /// name it, because starting playback is the embedding's choice.
    #[config(sdk, builder(default = false), patch(skip))]
    pub(crate) should_autoplay: bool,

    /// Entries the navigation history keeps. Only explicit selections and
    /// auto-advances land there, so the default is a listening session's
    /// worth of back-steps; the queue's own track list is unbounded.
    #[config(sdk, builder(default = 100))]
    pub(crate) max_history_size: usize,

    /// Initial queue traversal order; subsequent changes belong to navigation.
    #[config(sdk, builder(default))]
    pub(crate) playback_order: PlaybackOrder,

    /// Initial action when the current item ends; subsequent changes belong
    /// to the queue.
    #[config(sdk, builder(default))]
    pub(crate) action_at_item_end: ActionAtItemEnd,
}

impl<S> QueueConfig<S, PlayerFactory>
where
    S: HasPool<u8> + Send + Sync + 'static,
{
    /// Starts a queue configuration with the bare-track factory.
    pub fn builder() -> QueueConfigBuilder<S, PlayerFactory> {
        Self::with_factory(PlayerFactory)
    }
}
#[cfg(test)]
mod tests {
    use kithara_play::{PlayWorker, PlayWorkerConfig};
    use kithara_test_utils::kithara;

    use super::*;
    use crate::test_pools::pools;

    pub(super) fn config() -> QueueConfig<crate::test_pools::TestPools> {
        let prep = ResourcePrep::builder()
            .worker(PlayWorker::new(PlayWorkerConfig::builder(pools()).build()))
            .build();
        QueueConfig::builder().prep(prep).build()
    }

    #[kithara::test]
    fn default_config_preloads_ahead_of_the_end() {
        let cfg = config();

        assert!(cfg.store.is_none());
        assert_eq!(cfg.preload_lead, Duration::from_millis(3_500));
        assert!(!cfg.settings.gapless());
    }

    #[kithara::test]
    fn default_config_has_reasonable_loader_cap() {
        let cfg = config();
        assert_eq!(cfg.max_concurrent_loads.get(), 3);
        assert!(cfg.store.is_none());
        assert_eq!(cfg.preload_lead, Duration::from_millis(3_500));
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod document_tests {
    use kithara_test_utils::kithara;

    use super::{QueueConfigPatch, tests::config};
    use crate::PlaybackOrder;

    #[kithara::test(native, flash(false))]
    fn a_document_sets_the_load_cap_and_leaves_the_history_size() {
        let patch: QueueConfigPatch =
            serde_yaml_ng::from_str("max_concurrent_loads: 5\n").expect("the document types");
        // Seeded off the crate default so a merge that reset every unnamed
        // field could not pass this by coincidence.
        let mut config = config();
        config.max_history_size = 37;

        config.apply(patch).expect("the document patch is valid");

        assert_eq!(config.max_concurrent_loads.get(), 5);
        assert_eq!(
            config.max_history_size, 37,
            "a key the document does not name must keep its seeded value"
        );
    }

    #[kithara::test(native, flash(false))]
    fn a_document_sets_the_order_and_leaves_the_history_size() {
        let patch: QueueConfigPatch =
            serde_yaml_ng::from_str("playback_order: Shuffle\n").expect("the document types");
        // Seeded off the crate default so a merge that reset every unnamed
        // field could not pass this by coincidence.
        let mut config = config();
        config.max_history_size = 37;

        config.apply(patch).expect("the document patch is valid");

        assert_eq!(config.playback_order, PlaybackOrder::Shuffle);
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
