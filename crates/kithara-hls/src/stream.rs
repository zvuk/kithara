#![forbid(unsafe_code)]

use std::sync::Arc;

use dashmap::DashMap;
use kithara_assets::{
    AssetStore, AssetStoreBuilder, BytePool, EvictConfig, OnInvalidatedFn, ProcessChunkFn,
    ResourceKey, StoreOptions, asset_root_for_url,
};
use kithara_drm::{DecryptContext, aes128_cbc_process_chunk};
use kithara_events::EventBus;
use kithara_net::HttpClient;
use kithara_platform::{CancelScope, CancelToken, tokio::sync::mpsc};
use kithara_stream::{
    Activity, PlayheadState, PlayheadWrite, SeekObserve, SeekState, SourceError, StreamType,
    dl::{Downloader, DownloaderConfig, Peer},
};

use crate::{
    config::HlsConfig,
    coord::{HlsCoord, HlsCoordEnv},
    invalidation::{HlsInvalidationGuard, HlsInvalidationRegistry, HlsStore},
    loading::{KeyManager, PlaylistCache},
    parsing::{MediaPlaylist, variant_info_from_master},
    peer::HlsPeer,
    playlist::PlaylistState,
    source::HlsSource,
    variant::{HlsVariant, PlanCtx},
};

/// Marker type for HLS streaming.
pub struct Hls;

fn eviction_callback(
    evict_tx: mpsc::UnboundedSender<ResourceKey>,
    next: Option<OnInvalidatedFn>,
) -> OnInvalidatedFn {
    Arc::new(move |key: &ResourceKey| {
        let _ = evict_tx.send(key.clone());
        if let Some(ref callback) = next {
            callback(key);
        }
    })
}

impl StreamType for Hls {
    type Config = HlsConfig;
    type Events = EventBus;
    type Source = HlsSource;

    async fn create(config: Self::Config) -> Result<Self::Source, SourceError> {
        let asset_root = asset_root_for_url(&config.url, config.name.as_deref());
        let asset_root_arc: Arc<str> = Arc::from(asset_root.as_str());
        let cancel = CancelScope::new(config.cancel.clone()).token();

        let bus = config
            .bus
            .clone()
            .unwrap_or_else(|| EventBus::new(config.event_channel_capacity));

        let downloader = config.downloader.clone().unwrap_or_else(|| {
            let dl_cancel = cancel.child();
            let client = HttpClient::new(config.net_options.clone(), dl_cancel.child());
            let dl_config = DownloaderConfig::for_client(client)
                .cancel(dl_cancel)
                .build();
            Downloader::new(dl_config)
        });

        let (evict_tx, evict_rx) = mpsc::unbounded_channel::<ResourceKey>();
        // App-wide shared store: reuse the injected backend and register
        // this stream's eviction channel in the routing registry. Without
        // injection, build a private per-stream store whose
        // `on_invalidated` feeds this single eviction channel directly.
        let (scope, invalidation_guard) = if let Some(shared) = config.asset_store.as_ref() {
            let guard = HlsInvalidationGuard::install(
                Arc::clone(&shared.registry),
                Arc::clone(&asset_root_arc),
                evict_tx,
            );
            (
                shared.backend.scope(Arc::clone(&asset_root_arc)),
                Some(guard),
            )
        } else {
            let backend = build_asset_store(&config, cancel.clone(), evict_tx);
            (backend.scope(Arc::clone(&asset_root_arc)), None)
        };

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

        let key_manager = KeyManager::with_options(
            peer_handle.clone(),
            scope.clone(),
            config.headers.clone(),
            config.keys.clone(),
            byte_pool.clone(),
        );

        let master = playlist_cache.master_playlist(&config.url).await?;

        let mut media_playlists: Vec<MediaPlaylist> = Vec::new();
        for variant in &master.variants {
            let media_url = playlist_cache.resolve_url(&config.url, &variant.uri)?;
            let playlist = playlist_cache
                .media_playlist(&media_url, crate::parsing::VariantId(variant.id.0))
                .await?;
            media_playlists.push(playlist);
        }

        let playlist_state = Arc::new(PlaylistState::assemble(&master.variants, &media_playlists));

        hls_peer.set_abr_variants(variant_info_from_master(&master, &media_playlists));

        key_manager
            .prefetch_aes128_keys(&media_playlists)
            .await
            .map_err(SourceError::from)?;

        let estimation = crate::loading::size_estimation::SizeEstimator::new(
            peer_handle.clone(),
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
        };

        let variants: Vec<Arc<HlsVariant>> = media_playlists
            .iter()
            .enumerate()
            .map(|(idx, mp)| {
                let init_decrypt_ctx = key_manager.resolve_init_decrypt_ctx(mp);
                let decrypt_contexts = key_manager.resolve_variant_decrypt_contexts(mp);
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

        let mut source = HlsSource::new(Arc::clone(&coord), bus.clone());

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

fn build_asset_store(
    config: &HlsConfig,
    cancel: CancelToken,
    evict_tx: mpsc::UnboundedSender<ResourceKey>,
) -> AssetStore<DecryptContext> {
    let drm_process_fn: ProcessChunkFn<DecryptContext> =
        Arc::new(|input, output, ctx: &mut DecryptContext, is_last| {
            aes128_cbc_process_chunk(input, output, ctx, is_last)
        });
    let on_invalidated = eviction_callback(evict_tx, config.store.on_invalidated.clone());

    let mut builder = AssetStoreBuilder::new()
        .process_fn(drm_process_fn)
        .cancel(cancel)
        .on_invalidated(on_invalidated)
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

/// Build an app-wide shared HLS asset store: one
/// `AssetStore<DecryptContext>` (DRM `process_fn`, cache, flush hub) plus
/// a routing registry that steers per-`asset_root` invalidations to the
/// owning stream. Inject the result into every [`HlsConfig::asset_store`]
/// that should cooperate on a single cache. `cancel` must be a child of
/// the app master so a shutdown cascades through the store.
#[must_use]
pub fn build_shared_asset_store(
    store: &StoreOptions,
    pool: Option<BytePool>,
    cancel: CancelToken,
) -> HlsStore {
    let registry: HlsInvalidationRegistry = Arc::new(DashMap::new());
    let drm_process_fn: ProcessChunkFn<DecryptContext> =
        Arc::new(|input, output, ctx: &mut DecryptContext, is_last| {
            aes128_cbc_process_chunk(input, output, ctx, is_last)
        });
    let on_invalidated =
        registry_eviction_callback(Arc::clone(&registry), store.on_invalidated.clone());

    let mut builder = AssetStoreBuilder::new()
        .process_fn(drm_process_fn)
        .cancel(cancel)
        .on_invalidated(on_invalidated)
        .root_dir(&store.cache_dir)
        .evict_config(EvictConfig::from(store))
        .ephemeral(store.is_ephemeral);
    if let Some(pool) = pool {
        builder = builder.pool(pool);
    }
    if let Some(cap) = store.cache_capacity {
        builder = builder.cache_capacity(cap);
    }
    if let Some(ref hub) = store.flush_hub {
        builder = builder.flush_hub(Arc::clone(hub));
    }

    HlsStore {
        backend: Arc::new(builder.build()),
        registry,
    }
}

/// Shared-store invalidation callback: route an evicted key to the live
/// stream that owns its `asset_root`, then chain any caller callback.
fn registry_eviction_callback(
    registry: HlsInvalidationRegistry,
    next: Option<OnInvalidatedFn>,
) -> OnInvalidatedFn {
    Arc::new(move |key: &ResourceKey| {
        if let Some(root) = key.asset_root()
            && let Some(tx) = registry.get(root)
        {
            let _ = tx.send(key.clone());
        }
        if let Some(ref callback) = next {
            callback(key);
        }
    })
}
