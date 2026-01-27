//! HLS inner stream implementation.
//!
//! Provides `HlsInner` - a sync `Read + Seek` adapter for HLS streams.

use std::{io::{Read, Seek, SeekFrom}, sync::Arc};

use kithara_assets::{AssetStoreBuilder, asset_root_for_url};
use kithara_net::HttpClient;
use kithara_stream::{StreamType, SyncReader};
use kithara_worker::Worker;
use tokio::sync::broadcast;

use crate::{
    cache::FetchLoader,
    error::HlsError,
    events::HlsEvent,
    fetch::FetchManager,
    options::HlsConfig,
    playlist::{PlaylistManager, variant_info_from_master},
    worker::{HlsSourceAdapter, HlsWorkerSource, VariantMetadata},
};

/// HLS inner stream implementing `Read + Seek`.
///
/// This wraps `SyncReader<HlsSourceAdapter>` to provide sync access to HLS streams.
pub struct HlsInner {
    reader: SyncReader<HlsSourceAdapter>,
}

impl HlsInner {
    /// Create new HLS inner stream.
    pub async fn new(mut config: HlsConfig) -> Result<Self, HlsError> {
        let asset_root = asset_root_for_url(&config.url);
        let cancel = config.cancel.clone().unwrap_or_default();
        let net = HttpClient::new(config.net.clone());

        // Build asset store
        let base_assets = AssetStoreBuilder::new()
            .asset_root(&asset_root)
            .cancel(cancel.clone())
            .root_dir(&config.store.cache_dir)
            .evict_config(config.store.to_evict_config())
            .build();

        // Build FetchManager
        let fetch_manager = Arc::new(FetchManager::new(base_assets.clone(), net));

        // Build PlaylistManager
        let playlist_manager = Arc::new(PlaylistManager::new(
            Arc::clone(&fetch_manager),
            config.base_url.clone(),
        ));

        // Load master playlist
        let master = playlist_manager.master_playlist(&config.url).await?;

        // Determine initial variant
        let initial_variant = config.abr.initial_variant();

        // Create events channel if not provided
        let events_tx = if let Some(tx) = config.events_tx.clone() {
            tx
        } else {
            let capacity = config.events_channel_capacity.max(1);
            let (tx, _) = broadcast::channel(capacity);
            config.events_tx = Some(tx.clone());
            tx
        };

        // Emit VariantsDiscovered event
        let variant_info = variant_info_from_master(&master);
        let _ = events_tx.send(HlsEvent::VariantsDiscovered {
            variants: variant_info,
            initial_variant,
        });

        // Extract variant metadata
        let variant_metadata: Vec<VariantMetadata> = master
            .variants
            .iter()
            .enumerate()
            .map(|(index, v)| VariantMetadata {
                index,
                codec: v.codec.as_ref().and_then(|c| c.audio_codec),
                container: v.codec.as_ref().and_then(|c| c.container),
                bitrate: v.bandwidth,
            })
            .collect();

        // Create FetchLoader
        let fetch_loader = Arc::new(FetchLoader::new(
            config.url.clone(),
            Arc::clone(&fetch_manager),
            Arc::clone(&playlist_manager),
        ));

        // Create HlsWorkerSource
        let worker_source = HlsWorkerSource::new(
            fetch_loader,
            Arc::clone(&fetch_manager),
            variant_metadata,
            initial_variant,
            Some(config.abr.clone()),
            config.events_tx.clone(),
            cancel,
        );

        // Get assets for adapter
        let assets = worker_source.assets();

        // Create channels for worker (use config values)
        let cmd_capacity = config.command_channel_capacity.max(1);
        let chunk_capacity = config.chunk_channel_capacity.max(1);
        let (cmd_tx, cmd_rx) = kanal::bounded_async(cmd_capacity);
        let (chunk_tx, chunk_rx) = kanal::bounded_async(chunk_capacity);

        // Create AsyncWorker and spawn it
        let worker = kithara_worker::AsyncWorker::new(worker_source, cmd_rx, chunk_tx);
        tokio::spawn(worker.run());

        // Create HlsSourceAdapter
        let adapter = HlsSourceAdapter::new(chunk_rx, cmd_tx, assets, events_tx);

        let reader = SyncReader::new(adapter);

        Ok(Self { reader })
    }

    /// Get current read position.
    pub fn position(&self) -> u64 {
        self.reader.position()
    }
}

impl Read for HlsInner {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        self.reader.read(buf)
    }
}

impl Seek for HlsInner {
    fn seek(&mut self, pos: SeekFrom) -> std::io::Result<u64> {
        self.reader.seek(pos)
    }
}

/// Marker type for HLS streaming.
pub struct Hls;

impl StreamType for Hls {
    type Config = HlsConfig;
    type Inner = HlsInner;
    type Error = HlsError;

    async fn create(config: Self::Config) -> Result<Self::Inner, Self::Error> {
        HlsInner::new(config).await
    }
}
