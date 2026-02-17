//! HLS stream type implementation.
//!
//! Provides `Hls` marker type implementing `StreamType` trait.

use std::sync::{Arc, atomic::AtomicU64};

use kithara_assets::{
    AssetStoreBuilder, Assets, AssetsBackend, CoverageIndex, ProcessChunkFn, asset_root_for_url,
};
use kithara_drm::{DecryptContext, aes128_cbc_process_chunk};
use kithara_events::{EventBus, HlsEvent};
use kithara_net::HttpClient;
use kithara_stream::{StreamContext, StreamType, ThreadPool};

use crate::{
    HlsStreamContext,
    config::HlsConfig,
    error::HlsError,
    fetch::FetchManager,
    keys::KeyManager,
    parsing::variant_info_from_master,
    source::{HlsSource, build_pair},
};

/// Marker type for HLS streaming.
pub struct Hls;

impl StreamType for Hls {
    type Config = HlsConfig;
    type Source = HlsSource;
    type Error = HlsError;
    type Events = EventBus;

    fn thread_pool(config: &Self::Config) -> ThreadPool {
        config.thread_pool.clone()
    }

    fn event_bus(config: &Self::Config) -> Option<Self::Events> {
        config.bus.clone()
    }

    async fn create(config: Self::Config) -> Result<Self::Source, Self::Error> {
        let asset_root = asset_root_for_url(&config.url, config.name.as_deref());
        let cancel = config.cancel.clone().unwrap_or_default();
        let net = HttpClient::new(config.net.clone());

        // Build DRM process function for ProcessingAssets
        let drm_process_fn: ProcessChunkFn<DecryptContext> =
            Arc::new(|input, output, ctx: &mut DecryptContext, is_last| {
                aes128_cbc_process_chunk(input, output, ctx, is_last)
            });

        // Build storage backend with DRM processing support
        let mut builder = AssetStoreBuilder::new()
            .process_fn(drm_process_fn)
            .asset_root(Some(asset_root.as_str()))
            .cancel(cancel.clone())
            .root_dir(&config.store.cache_dir)
            .evict_config(config.store.to_evict_config())
            .ephemeral(config.ephemeral);
        if let Some(ref pool) = config.pool {
            builder = builder.pool(pool.clone());
        }
        if let Some(cap) = config.store.cache_capacity {
            builder = builder.cache_capacity(cap);
        }
        let backend: AssetsBackend<DecryptContext> = builder.build();

        // Build KeyManager for DRM key resolution
        let key_manager = Arc::new(KeyManager::from_options(
            // We pass a temporary Arc; the real FetchManager will be used below.
            // KeyManager needs its own FetchManager for key fetching.
            // We'll set it up after creating the FetchManager.
            Arc::new(
                FetchManager::new(backend.clone(), net.clone(), cancel.clone())
                    .with_master_url(config.url.clone())
                    .with_base_url(config.base_url.clone()),
            ),
            config.keys.clone(),
        ));

        // Build FetchManager (unified: fetch + playlist cache + Loader)
        let mut fetch_manager = FetchManager::new(backend, net, cancel.clone())
            .with_master_url(config.url.clone())
            .with_base_url(config.base_url.clone())
            .with_key_manager(key_manager);

        // Load master playlist
        let master = fetch_manager.master_playlist(&config.url).await?;

        // Load all media playlists eagerly for PlaylistState
        let mut media_playlists = Vec::new();
        for variant in &master.variants {
            let media_url = fetch_manager.resolve_url(&config.url, &variant.uri)?;
            let playlist = fetch_manager
                .media_playlist(&media_url, crate::parsing::VariantId(variant.id.0))
                .await?;
            media_playlists.push((media_url, playlist));
        }

        let playlist_state = Arc::new(crate::playlist::PlaylistState::from_parsed(
            &master.variants,
            &media_playlists,
        ));
        fetch_manager.set_playlist_state(playlist_state);

        let fetch_manager = Arc::new(fetch_manager);

        // Determine initial variant
        let initial_variant = config.abr.initial_variant();

        // Create or reuse event bus.
        let bus = config
            .bus
            .clone()
            .unwrap_or_else(|| EventBus::new(config.event_channel_capacity));

        // Emit VariantsDiscovered event
        let variant_info = variant_info_from_master(&master);
        bus.publish(HlsEvent::VariantsDiscovered {
            variants: variant_info,
            initial_variant,
        });

        // Create coverage index for crash-safe segment tracking (disk only).
        let coverage_index = match fetch_manager.backend() {
            AssetsBackend::Disk(store) => store
                .open_coverage_index_resource()
                .ok()
                .map(|res| Arc::new(CoverageIndex::new(res))),
            AssetsBackend::Mem(_) => None,
        };

        // Create HlsDownloader + HlsSource pair
        let playlist_state = fetch_manager
            .playlist_state()
            .cloned()
            .ok_or_else(|| HlsError::PlaylistParse("playlist state not initialized".to_string()))?;
        let (downloader, mut source) = build_pair(
            Arc::clone(&fetch_manager),
            &master.variants,
            &config,
            coverage_index,
            playlist_state,
            bus,
        );

        // Spawn downloader on the thread pool.
        // Backend is stored in HlsSource — dropping the source cancels the downloader.
        let backend = kithara_stream::Backend::new(downloader, &cancel, &config.thread_pool);
        source.set_backend(backend);

        Ok(source)
    }

    fn build_stream_context(
        source: &Self::Source,
        position: Arc<AtomicU64>,
    ) -> Arc<dyn StreamContext> {
        Arc::new(HlsStreamContext::new(
            position,
            source.segment_index_handle(),
            source.variant_index_handle(),
        ))
    }
}
