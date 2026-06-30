#![forbid(unsafe_code)]

use std::sync::{Arc, OnceLock};

use kithara_assets::{
    AssetScopeDelegate, AssetStore, AssetStoreBuilder, BytePool, EvictConfig, ResourceKey,
};
use kithara_events::EventBus;
use kithara_net::HttpClient;
use kithara_platform::{CancelScope, CancelToken, sync::CondvarGate, tokio::sync::mpsc};
use kithara_stream::{
    Activity, PlayheadState, PlayheadWrite, SeekObserve, SeekState, SourceError, StreamType,
    dl::{Downloader, DownloaderConfig, Peer},
};
use url::Url;

use crate::{
    config::HlsConfig,
    coord::{HlsCoord, HlsCoordEnv},
    loading::{KeyStore, PlaylistCache},
    naming::HlsAssetScopeDelegate,
    parsing::{MediaPlaylist, VariantStream, variant_info_from_master},
    peer::HlsPeer,
    playlist::PlaylistState,
    source::HlsSource,
    variant::{HlsVariant, PlanCtx},
};

/// Marker type for HLS streaming.
pub struct Hls;

impl StreamType for Hls {
    type Config = HlsConfig;
    type Events = EventBus;
    type Source = HlsSource;

    async fn create(config: Self::Config) -> Result<Self::Source, SourceError> {
        let naming: Arc<dyn AssetScopeDelegate> = Arc::new(HlsAssetScopeDelegate);
        let asset_root = naming.asset_root_for_url(&config.url, config.name.as_deref());
        let asset_root_arc: Arc<str> = Arc::from(asset_root.as_str());
        let stream_scope = CancelScope::new(config.cancel.clone());
        let cancel = stream_scope.token();

        let bus = config
            .bus
            .clone()
            .unwrap_or_else(|| EventBus::new(config.event_channel_capacity));

        let downloader = config
            .downloader
            .clone()
            .unwrap_or_else(|| default_downloader(&config, &cancel));

        let (evict_tx, evict_rx) = mpsc::unbounded_channel::<ResourceKey>();
       
        let store = config
            .asset_store
            .clone()
            .unwrap_or_else(|| Arc::new(build_asset_store(&config, cancel.clone())));
        let invalidation_guard = store.subscribe_eviction(Arc::clone(&asset_root_arc), evict_tx);
        let scope = store.scope_with_delegate(asset_root_arc, naming);

        let byte_pool = config
            .pool
            .clone()
            .unwrap_or_else(|| BytePool::default().clone());

        let playhead = Arc::new(PlayheadState::new());
        let seek = Arc::new(SeekState::new());
        let seek_obs = Arc::clone(&seek) as Arc<dyn SeekObserve>;
        let hls_peer = Arc::new(HlsPeer::new(
            Arc::clone(&seek_obs),
            Arc::clone(&seek) as Arc<dyn Activity>,
            config.initial_abr_mode,
        ));
        let peer_handle = downloader
            .register(Arc::clone(&hls_peer) as Arc<dyn Peer>)
            .with_bus(bus.clone());

        let playlist_cache =
            PlaylistCache::new(scope.clone(), peer_handle.clone(), byte_pool.clone());
        playlist_cache.set_master_url(config.url.clone());
        playlist_cache.set_base_url(config.base_url.clone());
        playlist_cache.set_headers(config.headers.clone());

        let key_store = KeyStore::with_options(
            peer_handle.clone(),
            scope.clone(),
            config.headers.clone(),
            config.keys.clone(),
            byte_pool.clone(),
        );

        let master = playlist_cache.master_playlist(&config.url).await?;

        let media_playlists =
            fetch_media_playlists(&playlist_cache, &config.url, &master.variants).await?;

        let playlist_state = Arc::new(PlaylistState::assemble(&master.variants, &media_playlists));

        hls_peer.set_abr_variants(variant_info_from_master(&master, &media_playlists));

        key_store
            .prefetch_aes128_keys(&media_playlists)
            .await
            .map_err(SourceError::from)?;

        let estimation = crate::loading::size_estimation::SizeEstimator::new(
            peer_handle.clone(),
            scope.clone(),
            Arc::clone(&playlist_state),
            media_playlists,
            config.headers.clone(),
            config.head_estimation_concurrency,
            config.size_probe_method,
        )
        .estimate()
        .await;
        let media_playlists = estimation.media_playlists;
        estimation
            .size_maps
            .into_iter()
            .enumerate()
            .filter(|(_, map)| !map.is_empty())
            .for_each(|(variant, map)| playlist_state.set_size_map(variant, map));

        playhead.set_duration(playlist_state.track_duration());

        // Shared readiness gate for the off-RT `wait_range(_, None)` park (CONTEXT.md "Seek and wait_range Contract").
        let ready = Arc::new(CondvarGate::<u64>::default());
        // Late-bound audio-worker wake, filled by `HlsSource::set_worker_wake`
        // once the worker exists; fired alongside `ready` on the two
        // downloader write/settle sites so the RT decoder re-ticks on data
        // arrival, not on its 10 ms scheduler poll. `None` until set.
        let worker_wake = Arc::new(OnceLock::new());

        let plan_ctx = PlanCtx {
            master_cancel: cancel.clone(),
            scope: scope.clone(),
            headers: config.headers.clone(),
            prefetch_budget: config.download_batch_size.max(1),
            seek_epoch: seek_obs.epoch(),
            look_ahead_bytes: Some(
                config
                    .look_ahead_bytes
                    .unwrap_or(HlsConfig::DEFAULT_LOOK_AHEAD_BYTES),
            ),
            ready: Arc::clone(&ready),
            worker_wake: Arc::clone(&worker_wake),
        };

        let variants: Vec<Arc<HlsVariant>> = media_playlists
            .iter()
            .enumerate()
            .map(|(idx, mp)| {
                let init_decrypt_ctx = key_store.resolve_init_decrypt_ctx(mp);
                let decrypt_contexts = key_store.resolve_variant_decrypt_contexts(mp);
                HlsVariant::new(
                    idx,
                    &playlist_state,
                    Arc::clone(&seek_obs),
                    init_decrypt_ctx,
                    &decrypt_contexts,
                    &plan_ctx,
                )
            })
            .collect();
        let variants = Arc::new(variants);

        let coord = Arc::new(HlsCoord::new(
            HlsCoordEnv {
                ready,
                worker_wake,
                cancel: cancel.clone(),
                scope: scope.clone(),
                headers: config.headers.clone(),
            },
            playhead,
            seek,
            peer_handle.abr().clone(),
            Arc::clone(&variants),
            Arc::clone(&playlist_state),
        ));

        let mut source = HlsSource::new(Arc::clone(&coord), bus.clone(), stream_scope);

        hls_peer.activate(
            coord,
            evict_rx,
            config.download_batch_size.max(1),
            Some(
                config
                    .look_ahead_bytes
                    .unwrap_or(HlsConfig::DEFAULT_LOOK_AHEAD_BYTES),
            ),
        );

        source.set_peer_handle(peer_handle);
        source.set_hls_peer(hls_peer);
        source.set_invalidation_guard(invalidation_guard);

        Ok(source)
    }

    fn event_bus(config: &Self::Config) -> Option<Self::Events> {
        config.bus.clone()
    }
}

/// Fetch each variant's media playlist for `master`, in master order.
async fn fetch_media_playlists(
    cache: &PlaylistCache,
    master_url: &Url,
    variants: &[VariantStream],
) -> Result<Vec<MediaPlaylist>, SourceError> {
    let mut playlists = Vec::with_capacity(variants.len());
    for variant in variants {
        let media_url = cache.resolve_url(master_url, &variant.uri)?;
        let playlist = cache
            .media_playlist(&media_url, crate::parsing::VariantId(variant.id.0))
            .await?;
        playlists.push(playlist);
    }
    Ok(playlists)
}

/// Default transport when the caller injects none: a private `Downloader`
/// rooted at a child of the stream's cancel token.
fn default_downloader(config: &HlsConfig, cancel: &CancelToken) -> Downloader {
    let dl_cancel = cancel.child();
    let client = HttpClient::new(config.net_options.clone(), dl_cancel.child());
    let dl_config = DownloaderConfig::for_client(client)
        .cancel(dl_cancel)
        .build();
    Downloader::new(dl_config)
}

/// Build a private per-stream `AssetStore` (cache, flush hub; AES-128
/// decryption travels per-acquire as a `ProcessCtx`). The caller
/// subscribes its eviction channel via
/// [`AssetStore::subscribe_eviction`](kithara_assets::AssetStore::subscribe_eviction).
fn build_asset_store(config: &HlsConfig, cancel: CancelToken) -> AssetStore {
    let mut builder = AssetStoreBuilder::new()
        .cancel(cancel)
        .root_dir(&config.store.cache_dir)
        .evict_config(EvictConfig::from(&config.store))
        .ephemeral(config.store.is_ephemeral);
    if let Some(ref pool) = config.pool {
        builder = builder.pool(pool.clone());
    }
    if let Some(cap) = config.store.cache_capacity {
        builder = builder.cache_capacity(cap);
    }
    if let Some(ref hub) = config.store.flush_hub {
        builder = builder.flush_hub(Arc::clone(hub));
    }
    builder.build()
}
