use std::collections::BTreeMap;

#[cfg(feature = "broadcast")]
use kithara::broadcast::BroadcastConfigPatch;
#[cfg(feature = "gui")]
use kithara::ui::source::{DrawPoolLimitsPatch, UiConfigPatch};
use kithara::{
    analysis::BeatAnalysisConfigPatch,
    assets::{AssetStoreConfigPatch, FlushPolicyPatch},
    audio::AudioConfigPatch,
    file::FileConfigPatch,
    hls::HlsConfigPatch,
    net::NetOptionsPatch,
    play::{PlayWorkerConfigPatch, PlayerConfigPatch},
    queue::QueueConfigPatch,
    stream::dl::DownloaderConfigPatch,
    worker::{ComputePool, DispatcherConfigPatch, WorkerConfigPatch},
};
use serde::Deserialize;

use crate::{config::AppConfigPatch, pools::PoolsSection};

/// Everything one configuration document can say. Sections default to empty, so
/// a document names only what it changes.
///
/// Deserialize-only on purpose: by the time a document is typed its references
/// are resolved, so this tree holds cipher keys and header secrets in the clear.
/// Rendering the configuration is [`Config::dump`]'s job, and it prints the
/// pre-expansion source instead.
///
/// [`Config::dump`]: crate::document::Config::dump
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct Document {
    pub(crate) app: AppConfigPatch,
    pub(crate) assets: Assets,
    pub(crate) assets_store: AssetStoreConfigPatch,
    pub(crate) audio: AudioConfigPatch,
    pub(crate) beat: BeatAnalysisConfigPatch,
    #[cfg(feature = "broadcast")]
    pub(crate) broadcast: BroadcastConfigPatch,
    /// Thread budgets the app builds its background dispatchers with. The
    /// thread name is not among them: a dispatcher is named where it is
    /// built.
    pub(crate) dispatcher: DispatcherConfigPatch,
    pub(crate) downloader: DownloaderConfigPatch,
    /// Draw-pool retention limits `UiConfig::draw_buffers` is built from.
    /// Read before `DrawBuffers` is constructed, never patched onto
    /// `UiConfig` afterwards -- see `Config::ui`.
    #[cfg(feature = "gui")]
    pub(crate) draw_pool: DrawPoolLimitsPatch,
    pub(crate) drm: Drm,
    pub(crate) file: FileConfigPatch,
    pub(crate) flush: FlushPolicyPatch,
    pub(crate) hls: HlsConfigPatch,
    pub(crate) net: NetOptionsPatch,
    /// Thread budgets of the one playback worker every deck shares.
    pub(crate) play_worker: PlayWorkerConfigPatch,
    pub(crate) player: PlayerConfigPatch,
    pub(crate) playlist: Playlist,
    pub(crate) pools: PoolsSection,
    pub(crate) queue: QueueConfigPatch,
    /// The toolkit's own tunables -- arena bytes and node limits, read by
    /// both hosts, plus `screen_cache`, which only the retained host reads
    /// (see `UiConfig::screen_cache` in `kithara-ui`). `draw_buffers` is not
    /// among them: it carries `UiConfig`'s own `#[patch(skip)]` because it is
    /// a *built* value, and `draw_pool` above is the document's way of naming
    /// what it is built from -- see `Config::ui`.
    #[cfg(feature = "gui")]
    pub(crate) ui: UiConfigPatch,
    pub(crate) worker: WorkerConfigPatch,
    pub(crate) worker_pool: Option<ComputePool>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct Assets {
    pub(crate) cache_identity: Vec<CacheIdentityRule>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CacheIdentityRule {
    pub(crate) domains: Vec<String>,
    pub(crate) query_parameters: Vec<String>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct Playlist {
    pub(crate) tracks: Vec<String>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct Drm {
    pub(crate) providers: Vec<DrmProvider>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct DrmProvider {
    pub(crate) name: String,
    pub(crate) domains: Vec<String>,
    /// Cipher key for this provider. A document references a secret through
    /// `$KITHARA_...`; expansion has already run by the time this parses.
    pub(crate) cipher_key: String,
    #[serde(default)]
    pub(crate) headers: BTreeMap<String, String>,
    #[serde(default)]
    pub(crate) seed: SeedSpec,
}

/// Shape of the per-request `X-Encrypted-Key` salt.
#[derive(Clone, Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct SeedSpec {
    pub(crate) alphabet: SeedAlphabet,
    pub(crate) length: usize,
}

impl Default for SeedSpec {
    fn default() -> Self {
        Self {
            alphabet: SeedAlphabet::Hex,
            length: 8,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub(crate) enum SeedAlphabet {
    #[default]
    Hex,
    Alphanumeric,
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroUsize;

    use kithara::platform::time::Duration;

    use super::{ComputePool, Document};
    use crate::baked::BAKED_DOCUMENT;

    #[kithara::test(native, flash(false))]
    fn the_baked_document_parses_under_the_schema() {
        let document: Document =
            serde_yaml_ng::from_str(BAKED_DOCUMENT).expect("the baked document matches the schema");

        assert!(
            !document.playlist.tracks.is_empty(),
            "the baked document ships a playlist"
        );
        assert!(
            !document.drm.providers.is_empty(),
            "the baked document ships DRM providers"
        );
        assert!(
            !document.assets.cache_identity.is_empty(),
            "the baked document ships cache-identity rules"
        );
    }

    #[kithara::test(native, flash(false))]
    fn an_unknown_field_is_refused_and_named() {
        let error = serde_yaml_ng::from_str::<Document>("player:\n  fade_style: dj\n")
            .expect_err("a typo must not pass silently");

        assert!(error.to_string().contains("fade_style"), "{error}");
    }

    #[kithara::test(native, flash(false))]
    fn an_empty_document_is_all_defaults() {
        let document: Document = serde_yaml_ng::from_str("{}").expect("an empty document is valid");

        assert!(
            document.hls.size_probe_method.is_none(),
            "a document naming no hls section leaves the crate default standing"
        );
        assert!(document.playlist.tracks.is_empty());
        assert!(
            document.worker_pool.is_none(),
            "a document naming no compute pool leaves the crate default standing"
        );
        assert!(
            document.worker.max_compute_tasks.is_none(),
            "a document naming no worker section leaves the crate default standing"
        );
        assert!(
            document.pools.budget_bytes.is_none(),
            "a document naming no pools section leaves the region budget standing"
        );
        assert!(
            document.assets_store.cache_capacity.is_none(),
            "a document naming no assets_store section leaves the crate default standing"
        );
        assert!(
            document.queue.max_concurrent_loads.is_none(),
            "a document naming no queue section leaves the crate default standing"
        );
        assert!(
            document.audio.preload_chunks.is_none(),
            "a document naming no audio section leaves the crate default standing"
        );
        assert!(
            document.file.reader_event_capacity.is_none(),
            "a document naming no file section leaves the crate default standing"
        );
        assert!(
            document.dispatcher.wait_timeout.is_none(),
            "a document naming no dispatcher section leaves the crate default standing"
        );
        assert!(
            document.play_worker.wait_timeout.is_none(),
            "a document naming no play_worker section leaves the crate default standing"
        );
    }

    #[kithara::test(native, flash(false))]
    fn an_assets_store_section_names_the_cache_capacity() {
        let document: Document = serde_yaml_ng::from_str("assets_store:\n  cache_capacity: 32\n")
            .expect("a valid document");

        assert_eq!(
            document.assets_store.cache_capacity.map(NonZeroUsize::get),
            Some(32)
        );
        assert!(
            document.assets_store.max_bytes.is_none(),
            "naming the cache capacity does not name the byte cap"
        );
    }

    #[kithara::test(native, flash(false))]
    fn a_pools_section_names_one_pool_without_touching_the_other() {
        let document: Document = serde_yaml_ng::from_str("pools:\n  bytes:\n    max_buffers: 64\n")
            .expect("a valid document");

        assert_eq!(document.pools.bytes.max_buffers, Some(64));
        assert!(
            document.pools.samples.max_buffers.is_none(),
            "naming one pool leaves the other empty"
        );
    }

    #[kithara::test(native, flash(false))]
    fn a_dispatcher_section_names_the_wait_budget() {
        let document: Document = serde_yaml_ng::from_str("dispatcher:\n  wait_timeout: 4ms\n")
            .expect("a valid document");

        assert_eq!(
            document.dispatcher.wait_timeout,
            Some(Duration::from_millis(4))
        );
    }

    #[kithara::test(native, flash(false))]
    fn the_dispatcher_thread_name_is_not_a_document_key() {
        let error = serde_yaml_ng::from_str::<Document>("dispatcher:\n  name: renamed\n")
            .expect_err("one document key must not rename every dispatcher");

        assert!(error.to_string().contains("name"), "{error}");
    }

    #[kithara::test(native, flash(false))]
    fn a_play_worker_section_names_the_track_ceiling() {
        let document: Document =
            serde_yaml_ng::from_str("play_worker:\n  capacity: 4\n").expect("a valid document");

        assert_eq!(
            document.play_worker.capacity.map(NonZeroUsize::get),
            Some(4)
        );
    }

    #[kithara::test(native, flash(false))]
    fn a_worker_section_names_the_compute_task_ceiling() {
        let document: Document =
            serde_yaml_ng::from_str("worker:\n  max_compute_tasks: 4\n").expect("a valid document");

        assert_eq!(
            document.worker.max_compute_tasks.map(NonZeroUsize::get),
            Some(4)
        );
        assert!(
            document.worker_pool.is_none(),
            "naming the worker section does not name a pool"
        );
    }

    #[kithara::test(native, flash(false))]
    fn a_worker_pool_document_names_the_owned_mode() {
        let document: Document = serde_yaml_ng::from_str(
            "worker_pool:\n  mode: owned\n  name: analysis\n  threads: 2\n",
        )
        .expect("a valid compute-pool document parses");

        match document.worker_pool {
            Some(ComputePool::Owned { name, threads }) => {
                assert_eq!(name, "analysis");
                assert_eq!(threads.get(), 2);
            }
            other => panic!("expected an owned compute pool, got {other:?}"),
        }
    }

    #[kithara::test(native, flash(false))]
    fn a_network_section_is_rejected() {
        let error = serde_yaml_ng::from_str::<Document>("network:\n  size_probe_method: head\n")
            .expect_err("network was renamed to hls");

        assert!(error.to_string().contains("network"), "{error}");
    }

    #[kithara::test(native, flash(false))]
    fn a_playback_section_is_rejected() {
        let error = serde_yaml_ng::from_str::<Document>("playback:\n  crossfade_seconds: 5.0\n")
            .expect_err("playback was folded into player.crossfade_duration");

        assert!(error.to_string().contains("playback"), "{error}");
    }

    #[kithara::test(native, flash(false))]
    fn a_resource_section_is_rejected() {
        let error = serde_yaml_ng::from_str::<Document>("resource:\n  preload_chunks: 5\n")
            .expect_err("resource was split into its own audio, hls, and file sections");

        assert!(error.to_string().contains("resource"), "{error}");
    }
}
