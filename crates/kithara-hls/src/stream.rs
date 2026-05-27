#![forbid(unsafe_code)]

use std::sync::Arc;

use kithara_assets::{
    AssetStore, AssetStoreBuilder, EvictConfig, OnInvalidatedFn, ProcessChunkFn, ResourceKey,
    asset_root_for_url,
};
use kithara_drm::{DecryptContext, aes128_cbc_process_chunk};
use kithara_events::EventBus;
use kithara_platform::tokio::sync::mpsc;
use kithara_stream::{
    SourceError, StreamType, Timeline,
    dl::{Downloader, DownloaderConfig, Peer},
};
use tokio_util::sync::CancellationToken;

use crate::{
    config::HlsConfig,
    coord::{HlsCoord, HlsCoordEnv},
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
        let cancel = config.cancel.clone().unwrap_or_default();

        let bus = config
            .bus
            .clone()
            .unwrap_or_else(|| EventBus::new(config.event_channel_capacity));

        let downloader = config.downloader.clone().unwrap_or_else(|| {
            let dl_cancel = cancel.child_token();
            let client = kithara_net::HttpClient::new(
                kithara_net::NetOptions::default(),
                dl_cancel.child_token(),
            );
            let dl_config = DownloaderConfig::for_client(client)
                .cancel(dl_cancel)
                .build();
            Downloader::new(dl_config)
        });

        let (evict_tx, evict_rx) = mpsc::unbounded_channel::<ResourceKey>();
        let backend = build_asset_store(&config, &asset_root, cancel.clone(), evict_tx);

        let byte_pool = config
            .pool
            .clone()
            .unwrap_or_else(|| kithara_bufpool::BytePool::default().clone());

        let timeline = Timeline::new();
        let hls_peer = Arc::new(HlsPeer::new(timeline.clone(), config.initial_abr_mode));
        let peer_handle = downloader
            .register(Arc::clone(&hls_peer) as Arc<dyn Peer>)
            .with_bus(bus.clone());

        let playlist_cache =
            PlaylistCache::new(backend.clone(), peer_handle.clone(), byte_pool.clone());
        playlist_cache.set_master_url(config.url.clone());
        playlist_cache.set_base_url(config.base_url.clone());
        playlist_cache.set_headers(config.headers.clone());

        let key_manager = KeyManager::with_options(
            peer_handle.clone(),
            backend.clone(),
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
        for (variant, map) in estimation.size_maps.into_iter().enumerate() {
            if !map.is_empty() {
                playlist_state.set_size_map(variant, map);
            }
        }

        timeline.set_total_duration(playlist_state.track_duration());

        let asset_store = Arc::new(backend);
        let plan_ctx = PlanCtx {
            master_cancel: cancel.clone(),
            asset_store: Arc::clone(&asset_store),
            headers: config.headers.clone(),
            prefetch_budget: config.download_batch_size.max(1),
            seek_epoch: timeline.seek_epoch(),
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
                    &timeline,
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
                asset_store: Arc::clone(&asset_store),
                headers: config.headers.clone(),
            },
            timeline,
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

        Ok(source)
    }

    fn event_bus(config: &Self::Config) -> Option<Self::Events> {
        config.bus.clone()
    }
}

fn build_asset_store(
    config: &HlsConfig,
    asset_root: &str,
    cancel: CancellationToken,
    evict_tx: mpsc::UnboundedSender<ResourceKey>,
) -> AssetStore<DecryptContext> {
    let drm_process_fn: ProcessChunkFn<DecryptContext> =
        Arc::new(|input, output, ctx: &mut DecryptContext, is_last| {
            aes128_cbc_process_chunk(input, output, ctx, is_last)
        });
    let on_invalidated = eviction_callback(evict_tx, config.store.on_invalidated.clone());

    let mut builder = AssetStoreBuilder::new()
        .process_fn(drm_process_fn)
        .asset_root(Some(asset_root))
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
