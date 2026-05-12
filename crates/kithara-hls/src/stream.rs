use std::{
    collections::HashSet,
    sync::{Arc, Mutex as StdMutex},
};

use futures::future::{try_join, try_join_all};
use kithara_assets::{
    AssetStore, AssetStoreBuilder, OnInvalidatedFn, ProcessChunkFn, ResourceKey, asset_root_for_url,
};
use kithara_drm::{DecryptContext, aes128_cbc_process_chunk};
use kithara_events::EventBus;
use kithara_platform::Mutex;
use kithara_stream::{
    SourceError, StreamContext, StreamType, Timeline,
    dl::{Downloader, DownloaderConfig, Peer},
};

use crate::{
    HlsResult, HlsStreamContext,
    config::HlsConfig,
    coord::HlsCoord,
    loading::{KeyManager, PlaylistCache, SegmentLoader},
    parsing::{EncryptionMethod, variant_info_from_master},
    peer::HlsPeer,
    playlist::PlaylistState,
    scheduler::HlsScheduler,
    source::HlsSource,
    stream_index::StreamIndex,
};

/// Marker type for HLS streaming.
pub struct Hls;

type InvalidationTarget = (Arc<Mutex<StreamIndex>>, Arc<HlsCoord>);

fn make_invalidation_callback(
    target: Arc<StdMutex<Option<InvalidationTarget>>>,
    next: Option<OnInvalidatedFn>,
) -> OnInvalidatedFn {
    Arc::new(move |key: &ResourceKey| {
        if let Some((segments, coord)) = target
            .lock()
            .expect("HLS invalidation target lock poisoned")
            .as_ref()
            && segments.lock_sync().remove_resource(key)
        {
            coord.condvar.notify_all();
        }
        if let Some(ref callback) = next {
            callback(key);
        }
    })
}

impl StreamType for Hls {
    type Config = HlsConfig;
    type Events = EventBus;
    type Source = HlsSource;

    fn build_stream_context(source: &Self::Source) -> Arc<dyn StreamContext> {
        Arc::new(HlsStreamContext::new(
            source.coord.position_handle(),
            source.coord.timeline(),
            Arc::clone(&source.segments),
            Arc::clone(&source.coord.abr_state),
        ))
    }

    async fn create(config: Self::Config) -> Result<Self::Source, SourceError> {
        let asset_root = asset_root_for_url(&config.url, config.name.as_deref());
        let cancel = config.cancel.clone().unwrap_or_default();

        let bus = config
            .bus
            .clone()
            .unwrap_or_else(|| EventBus::new(config.event_channel_capacity));

        let downloader = config.downloader.clone().unwrap_or_else(|| {
            let dl_config = DownloaderConfig::default().with_cancel(cancel.child_token());
            Downloader::new(dl_config)
        });

        let invalidation_target = Arc::new(StdMutex::new(None));
        let backend = build_asset_store(&config, &asset_root, cancel.clone(), &invalidation_target);

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

        let key_manager = Arc::new(KeyManager::from_options(
            peer_handle.clone(),
            backend.clone(),
            config.headers.clone(),
            config.keys.clone(),
            byte_pool.clone(),
        ));

        let mut loader = SegmentLoader::new(
            peer_handle.clone(),
            backend.clone(),
            config.headers.clone(),
            playlist_cache.clone(),
        );
        loader.set_key_manager(Arc::clone(&key_manager));
        let loader = Arc::new(loader);

        let master = playlist_cache.master_playlist(&config.url).await?;

        let mut media_playlists = Vec::new();
        for variant in &master.variants {
            let media_url = playlist_cache.resolve_url(&config.url, &variant.uri)?;
            let playlist = playlist_cache
                .media_playlist(&media_url, crate::parsing::VariantId(variant.id.0))
                .await?;
            media_playlists.push((media_url, playlist));
        }

        let playlist_state = Arc::new(PlaylistState::from_parsed(
            &master.variants,
            &media_playlists,
        ));

        hls_peer.set_abr_variants(build_abr_variants(&master.variants, &media_playlists));
        let initial_variant = hls_peer
            .abr()
            .current_variant_index()
            .min(master.variants.len().saturating_sub(1));

        prefetch_init_and_keys(&loader, &key_manager, &media_playlists)
            .await
            .map_err(SourceError::from)?;

        crate::loading::size_estimation::estimate_size_maps(
            &peer_handle,
            &playlist_state,
            &loader,
            &media_playlists,
            config.headers.as_ref(),
            &byte_pool,
        )
        .await;

        let _ = initial_variant;
        let _ = variant_info_from_master;

        let hls_downloader = HlsScheduler::new(
            backend,
            Arc::clone(&playlist_state),
            Arc::clone(hls_peer.abr()),
            timeline,
            bus,
            &config,
            hls_peer.committed_segment_cursor(),
        );
        let mut source = hls_downloader.spawn_source(hls_peer.reader_segment_cursor());
        *invalidation_target
            .lock()
            .expect("HLS invalidation target lock poisoned") =
            Some((Arc::clone(&source.segments), Arc::clone(&source.coord)));

        hls_peer.activate(hls_downloader, Arc::clone(&loader));
        source.set_peer_handle(peer_handle);
        source.set_hls_peer(hls_peer);

        Ok(source)
    }

    fn event_bus(config: &Self::Config) -> Option<Self::Events> {
        config.bus.clone()
    }
}

/// Pre-fetch init segments and DRM keys before the scheduler comes up.
///
/// Both pieces are required by `prepare_media_sync` — without them the
/// scheduler's first segment commit fails synchronously and the reader
/// hangs in `wait_range` until the hang detector panics. Surfacing the
/// failure here lets `Hls::create` return `Err` immediately, so callers
/// (player UI, FFI) get an actionable error instead of a 5-second deadlock.
async fn prefetch_init_and_keys(
    loader: &Arc<SegmentLoader>,
    key_manager: &Arc<KeyManager>,
    media_playlists: &[(url::Url, crate::parsing::MediaPlaylist)],
) -> HlsResult<()> {
    let init_futs = media_playlists
        .iter()
        .enumerate()
        .filter(|(_, (_, pl))| pl.init_segment.is_some())
        .map(|(variant, _)| {
            let loader = Arc::clone(loader);
            async move { loader.load_init_segment(variant).await.map(drop) }
        });

    let key_futs = collect_aes128_key_urls(media_playlists)
        .into_iter()
        .map(|url| {
            let km = Arc::clone(key_manager);
            async move { km.get_raw_key(&url, None).await.map(drop) }
        });

    try_join(try_join_all(init_futs), try_join_all(key_futs)).await?;
    Ok(())
}

fn collect_aes128_key_urls(
    media_playlists: &[(url::Url, crate::parsing::MediaPlaylist)],
) -> HashSet<url::Url> {
    let mut key_urls: HashSet<url::Url> = HashSet::new();
    for (media_url, playlist) in media_playlists {
        for segment in &playlist.segments {
            if let Some(ref seg_key) = segment.key
                && matches!(seg_key.method, EncryptionMethod::Aes128)
                && let Some(ref key_info) = seg_key.key_info
                && let Ok(seg_url) = media_url.join(&segment.uri)
                && let Ok(key_url) = KeyManager::resolve_key_url(key_info, &seg_url)
            {
                key_urls.insert(key_url);
            }
        }
    }
    key_urls
}

fn build_asset_store(
    config: &HlsConfig,
    asset_root: &str,
    cancel: tokio_util::sync::CancellationToken,
    invalidation_target: &Arc<StdMutex<Option<InvalidationTarget>>>,
) -> AssetStore<DecryptContext> {
    let drm_process_fn: ProcessChunkFn<DecryptContext> =
        Arc::new(|input, output, ctx: &mut DecryptContext, is_last| {
            aes128_cbc_process_chunk(input, output, ctx, is_last)
        });
    let on_invalidated = make_invalidation_callback(
        Arc::clone(invalidation_target),
        config.store.on_invalidated.clone(),
    );

    let mut builder = AssetStoreBuilder::new()
        .process_fn(drm_process_fn)
        .asset_root(Some(asset_root))
        .cancel(cancel)
        .on_invalidated(on_invalidated)
        .root_dir(&config.store.cache_dir)
        .evict_config(config.store.to_evict_config())
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

fn build_abr_variants(
    master_variants: &[crate::parsing::VariantStream],
    media_playlists: &[(url::Url, crate::parsing::MediaPlaylist)],
) -> Vec<kithara_events::AbrVariant> {
    master_variants
        .iter()
        .zip(media_playlists.iter())
        .map(|(v, (_, playlist))| {
            let duration = if playlist.segments.is_empty() {
                kithara_events::VariantDuration::Unknown
            } else {
                kithara_events::VariantDuration::Segmented(
                    playlist.segments.iter().map(|s| s.duration).collect(),
                )
            };
            kithara_events::AbrVariant {
                duration,
                variant_index: v.id.0,
                bandwidth_bps: v.bandwidth.unwrap_or(0),
            }
        })
        .collect()
}
