//! HLS stream type implementation.
//!
//! Provides `Hls` marker type implementing `StreamType` trait.

use std::sync::{Arc, atomic::AtomicU64};

use kithara_assets::{
    AssetStoreBuilder, Assets, AssetsBackend, CoverageIndex, ProcessChunkFn, asset_root_for_url,
};
use kithara_drm::{DecryptContext, aes128_cbc_process_chunk};
use kithara_net::HttpClient;
use kithara_stream::{StreamContext, StreamType};
use tokio::sync::broadcast;

use crate::{
    HlsStreamContext, config::HlsConfig, error::HlsError, events::HlsEvent, fetch::FetchManager,
    keys::KeyManager, playlist::variant_info_from_master, source::build_pair,
};

/// Marker type for HLS streaming.
pub struct Hls;

impl StreamType for Hls {
    type Config = HlsConfig;
    type Source = crate::source::HlsSource;
    type Error = HlsError;
    type Event = HlsEvent;

    fn ensure_events(config: &mut Self::Config) -> broadcast::Receiver<Self::Event> {
        if config.events_tx.is_none() {
            let capacity = config.events_channel_capacity.max(1);
            config.events_tx = Some(broadcast::channel(capacity).0);
        }
        match config.events_tx {
            Some(ref tx) => tx.subscribe(),
            None => broadcast::channel(1).1,
        }
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
        let fetch_manager = Arc::new(
            FetchManager::new(backend, net, cancel.clone())
                .with_master_url(config.url.clone())
                .with_base_url(config.base_url.clone())
                .with_key_manager(key_manager),
        );

        // Load master playlist
        let master = fetch_manager.master_playlist(&config.url).await?;

        // Determine initial variant
        let initial_variant = config.abr.initial_variant();

        // events_tx is guaranteed to exist (ensure_events was called by Stream::new).
        let events_tx = match config.events_tx {
            Some(ref tx) => tx.clone(),
            None => broadcast::channel(config.events_channel_capacity.max(1)).0,
        };

        // Emit VariantsDiscovered event
        let variant_info = variant_info_from_master(&master);
        let _ = events_tx.send(HlsEvent::VariantsDiscovered {
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
        let (downloader, mut source) = build_pair(
            Arc::clone(&fetch_manager),
            master.variants.clone(),
            &config,
            coverage_index,
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
