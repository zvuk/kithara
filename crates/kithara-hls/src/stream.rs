use std::{
    collections::HashSet,
    sync::{Arc, Mutex as StdMutex},
};

use futures::future::{join, join_all};
use kithara_assets::{
    AssetStore, AssetStoreBuilder, OnInvalidatedFn, ProcessChunkFn, ResourceKey, asset_root_for_url,
};
use kithara_drm::{DecryptContext, aes128_cbc_process_chunk};
use kithara_events::{EventBus, HlsEvent};
use kithara_platform::{Mutex, time::Instant};
use kithara_stream::{
    StreamContext, StreamType, Timeline,
    dl::{Downloader, DownloaderConfig, Peer},
};

use crate::{
    HlsStreamContext,
    config::HlsConfig,
    coord::HlsCoord,
    error::HlsError,
    loading::{KeyManager, PlaylistCache, SegmentLoader},
    parsing::{EncryptionMethod, variant_info_from_master},
    peer::HlsPeer,
    playlist::PlaylistState,
    source::{HlsSource, build_pair},
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
    type Source = HlsSource;
    type Error = HlsError;
    type Events = EventBus;

    fn event_bus(config: &Self::Config) -> Option<Self::Events> {
        config.bus.clone()
    }

    async fn create(config: Self::Config) -> Result<Self::Source, Self::Error> {
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

        // Build DRM process function for ProcessingAssets
        let drm_process_fn: ProcessChunkFn<DecryptContext> =
            Arc::new(|input, output, ctx: &mut DecryptContext, is_last| {
                aes128_cbc_process_chunk(input, output, ctx, is_last)
            });
        let invalidation_target = Arc::new(StdMutex::new(None));
        let on_invalidated = make_invalidation_callback(
            Arc::clone(&invalidation_target),
            config.store.on_invalidated.clone(),
        );

        let mut builder = AssetStoreBuilder::new()
            .process_fn(drm_process_fn)
            .asset_root(Some(asset_root.as_str()))
            .cancel(cancel.clone())
            .on_invalidated(on_invalidated)
            .root_dir(&config.store.cache_dir)
            .evict_config(config.store.to_evict_config())
            .ephemeral(config.store.ephemeral);
        if let Some(ref pool) = config.pool {
            builder = builder.pool(pool.clone());
        }
        if let Some(cap) = config.store.cache_capacity {
            builder = builder.cache_capacity(cap);
        }
        if let Some(n) = config.store.checkpoint_every {
            builder = builder.checkpoint_every(n);
        }
        let backend: AssetStore<DecryptContext> = builder.build();

        let timeline = Timeline::new();
        let hls_peer = Arc::new(HlsPeer::new(timeline.clone()));
        let peer_handle = downloader
            .register(Arc::clone(&hls_peer) as Arc<dyn Peer>)
            .with_bus(bus.clone());

        let playlist_cache = PlaylistCache::new(backend.clone(), peer_handle.clone());
        playlist_cache.set_master_url(config.url.clone());
        playlist_cache.set_base_url(config.base_url.clone());
        playlist_cache.set_headers(config.headers.clone());

        let key_manager = Arc::new(KeyManager::from_options(
            peer_handle.clone(),
            backend.clone(),
            config.headers.clone(),
            config.keys.clone(),
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

        let initial_variant = config.abr.as_ref().map_or(0, |abr| {
            let now = Instant::now();
            let decision = abr.decide(now);
            if decision.changed {
                abr.apply(&decision, now);
            }
            abr.get_current_variant_index()
        });

        prefetch_init_and_keys(&loader, &key_manager, &media_playlists).await;

        crate::loading::size_estimation::estimate_size_maps(
            &peer_handle,
            &playlist_state,
            &loader,
            &media_playlists,
            config.headers.as_ref(),
        )
        .await;

        let variant_info = variant_info_from_master(&master);
        bus.publish(HlsEvent::VariantsDiscovered {
            variants: variant_info,
            initial_variant,
        });

        let (hls_downloader, mut source) = build_pair(
            backend,
            peer_handle.clone(),
            &master.variants,
            &config,
            Arc::clone(&playlist_state),
            bus,
            timeline,
        );
        *invalidation_target
            .lock()
            .expect("HLS invalidation target lock poisoned") =
            Some((Arc::clone(&source.segments), Arc::clone(&source.coord)));

        hls_peer.activate(hls_downloader, Arc::clone(&loader));
        source.set_peer_handle(peer_handle);
        source.set_hls_peer(hls_peer);

        Ok(source)
    }

    fn build_stream_context(source: &Self::Source, timeline: Timeline) -> Arc<dyn StreamContext> {
        Arc::new(HlsStreamContext::new(
            timeline,
            Arc::clone(&source.segments),
            Arc::clone(&source.coord.abr_variant_index),
        ))
    }
}

async fn prefetch_init_and_keys(
    loader: &Arc<SegmentLoader>,
    key_manager: &Arc<KeyManager>,
    media_playlists: &[(url::Url, crate::parsing::MediaPlaylist)],
) {
    let num_variants = media_playlists.len();

    let init_futs: Vec<_> = (0..num_variants)
        .filter(|&v| {
            media_playlists
                .get(v)
                .and_then(|(_, pl)| pl.init_segment.as_ref())
                .is_some()
        })
        .map(|variant| {
            let loader = Arc::clone(loader);
            async move { (variant, loader.load_init_segment(variant).await) }
        })
        .collect();

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

    let key_futs: Vec<_> = key_urls
        .into_iter()
        .map(|url| {
            let km = Arc::clone(key_manager);
            async move { (url.clone(), km.get_raw_key(&url, None).await) }
        })
        .collect();

    let (init_results, key_results) = join(join_all(init_futs), join_all(key_futs)).await;

    for (variant, result) in init_results {
        if let Err(e) = result {
            tracing::warn!(variant, error = %e, "failed to pre-fetch init segment");
        }
    }
    for (url, result) in key_results {
        if let Err(e) = result {
            tracing::warn!(url = %url, error = %e, "failed to pre-fetch DRM key");
        }
    }
}
