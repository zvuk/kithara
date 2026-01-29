//! HLS stream type implementation.
//!
//! Provides `Hls` marker type implementing `StreamType` trait.

use std::sync::Arc;

use kithara_assets::{AssetStoreBuilder, asset_root_for_url};
use kithara_net::HttpClient;
use kithara_stream::StreamType;
use tokio::sync::broadcast;

use crate::{
    error::HlsError,
    events::HlsEvent,
    fetch::FetchManager,
    options::HlsConfig,
    playlist::variant_info_from_master,
    source::{VariantMetadata, build_pair},
};

/// Marker type for HLS streaming.
pub struct Hls;

impl StreamType for Hls {
    type Config = HlsConfig;
    type Source = crate::source::HlsSource;
    type Error = HlsError;
    type Event = HlsEvent;

    async fn create(mut config: Self::Config) -> Result<Self::Source, Self::Error> {
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

        // Build FetchManager (unified: fetch + playlist cache + Loader)
        let fetch_manager = Arc::new(
            FetchManager::new(base_assets.clone(), net, cancel.clone())
                .with_master_url(config.url.clone())
                .with_base_url(config.base_url.clone()),
        );

        // Load master playlist
        let master = fetch_manager.master_playlist(&config.url).await?;

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

        // Create HlsDownloader + HlsSource pair
        let (downloader, source) =
            build_pair(Arc::clone(&fetch_manager), variant_metadata, &config);

        // Spawn downloader as background task
        std::mem::forget(kithara_stream::Backend::new(downloader, cancel));

        Ok(source)
    }
}
