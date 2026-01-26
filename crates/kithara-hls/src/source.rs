#![forbid(unsafe_code)]

use std::sync::Arc;

use kithara_assets::{AssetStoreBuilder, asset_root_for_url};
use kithara_net::HttpClient;
use kithara_stream::{OpenedSource, SourceFactory, StreamError};
use kithara_worker::Worker;
use tokio::sync::broadcast;
use url::Url;

use crate::{
    cache::{FetchLoader, Loader},
    error::{HlsError, HlsResult},
    events::HlsEvent,
    fetch::FetchManager,
    options::HlsParams,
    playlist::{PlaylistManager, variant_info_from_master},
    segment_source::HlsSegmentSource,
    worker::{HlsSourceAdapter, HlsWorkerSource, VariantMetadata},
};

/// Marker type for HLS streaming with the unified `StreamSource<S>` API.
///
/// ## Usage
///
/// ```ignore
/// use kithara_stream::{StreamSource, SyncReader, SyncReaderParams};
/// use kithara_hls::{Hls, HlsParams};
///
/// // Async source with events
/// let source = StreamSource::<Hls>::open(url, HlsParams::default()).await?;
/// let events = source.events();  // Receiver<HlsEvent>
///
/// // Sync reader for decoders (Read + Seek)
/// let reader = SyncReader::<StreamSource<Hls>>::open(
///     url,
///     HlsParams::default(),
///     SyncReaderParams::default()
/// ).await?;
/// let events = reader.events();
/// ```
#[derive(Clone, Copy, Debug, Default)]
pub struct Hls;

impl Hls {
    /// Open an HLS stream and return a SegmentSource for zero-copy decoding.
    ///
    /// This is the recommended way to use HLS with kithara-decode for efficient
    /// memory usage. The decoder reads segment bytes directly from disk.
    ///
    /// ## Example
    ///
    /// ```ignore
    /// use kithara_decode::SegmentStreamDecoder;
    /// use kithara_hls::{Hls, HlsParams};
    ///
    /// let source = Hls::open_segment_source(url, HlsParams::default()).await?;
    /// let mut decoder = SegmentStreamDecoder::new(Arc::new(source));
    ///
    /// while let Some(chunk) = decoder.decode_next()? {
    ///     play_audio(chunk);
    /// }
    /// ```
    pub async fn open_segment_source(url: Url, params: HlsParams) -> HlsResult<HlsSegmentSource> {
        let asset_root = asset_root_for_url(&url);
        let cancel = params.cancel.clone().unwrap_or_default();
        let net = HttpClient::new(params.net.clone());

        // Build base asset store
        let base_assets = AssetStoreBuilder::new()
            .asset_root(&asset_root)
            .cancel(cancel.clone())
            .root_dir(&params.store.cache_dir)
            .evict_config(params.store.to_evict_config())
            .build();

        // Build FetchManager
        let fetch_manager = Arc::new(FetchManager::new(base_assets.clone(), net));

        // Build PlaylistManager
        let playlist_manager = Arc::new(PlaylistManager::new(
            Arc::clone(&fetch_manager),
            params.base_url.clone(),
        ));

        // Load master playlist
        let master = playlist_manager.master_playlist(&url).await?;

        // Determine initial variant from ABR mode
        let initial_variant = params.abr.initial_variant();

        // Emit VariantsDiscovered event
        if let Some(ref events_tx) = params.events_tx {
            let variant_info = variant_info_from_master(&master);
            let _ = events_tx.send(HlsEvent::VariantsDiscovered {
                variants: variant_info,
                initial_variant,
            });
        }

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
            url.clone(),
            Arc::clone(&fetch_manager),
            Arc::clone(&playlist_manager),
        ));

        // Create and return HlsSegmentSource
        Ok(HlsSegmentSource::new(
            fetch_loader as Arc<dyn Loader>,
            base_assets,
            variant_metadata,
            initial_variant,
            Some(params.abr.clone()),
            params.events_tx.clone(),
            cancel,
        ))
    }

    /// Open an HLS stream from a master playlist URL.
    ///
    /// Returns `HlsWorkerSource` which can be used with `AsyncWorker` for
    /// push-based streaming.
    ///
    /// **Note:** For new code, prefer `open_segment_source()` which provides
    /// better memory efficiency through zero-copy decoding.
    pub async fn open(url: Url, params: HlsParams) -> HlsResult<HlsWorkerSource> {
        let asset_root = asset_root_for_url(&url);
        let cancel = params.cancel.clone().unwrap_or_default();
        let net = HttpClient::new(params.net.clone());

        // Build base asset store
        let base_assets = AssetStoreBuilder::new()
            .asset_root(&asset_root)
            .cancel(cancel.clone())
            .root_dir(&params.store.cache_dir)
            .evict_config(params.store.to_evict_config())
            .build();

        // Build FetchManager
        let fetch_manager = Arc::new(FetchManager::new(base_assets, net));

        // Build PlaylistManager
        let playlist_manager = Arc::new(PlaylistManager::new(
            Arc::clone(&fetch_manager),
            params.base_url.clone(),
        ));

        // Load master playlist
        let master = playlist_manager.master_playlist(&url).await?;

        // Determine initial variant from ABR mode
        let initial_variant = params.abr.initial_variant();

        // Emit VariantsDiscovered event
        if let Some(ref events_tx) = params.events_tx {
            let variant_info = variant_info_from_master(&master);
            let _ = events_tx.send(HlsEvent::VariantsDiscovered {
                variants: variant_info,
                initial_variant,
            });
        }

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
            url.clone(),
            Arc::clone(&fetch_manager),
            Arc::clone(&playlist_manager),
        ));

        // Create and return HlsWorkerSource
        Ok(HlsWorkerSource::new(
            fetch_loader as Arc<dyn Loader>,
            Arc::clone(&fetch_manager),
            variant_metadata,
            initial_variant,
            Some(params.abr.clone()),
            params.events_tx.clone(),
            cancel,
        ))
    }
}

impl SourceFactory for Hls {
    type Params = HlsParams;
    type Event = HlsEvent;
    type SourceImpl = HlsSourceAdapter;

    async fn open(
        url: Url,
        mut params: Self::Params,
    ) -> Result<OpenedSource<Self::SourceImpl, Self::Event>, StreamError<HlsError>> {
        // Create events channel if not provided
        let events_tx = if let Some(tx) = params.events_tx.clone() {
            tx
        } else {
            let (tx, _) = broadcast::channel(32);
            params.events_tx = Some(tx.clone());
            tx
        };

        // Open HLS worker source
        let worker_source = Hls::open(url, params).await.map_err(StreamError::Source)?;

        // Get assets before passing worker_source to AsyncWorker
        let assets = worker_source.assets();

        // Create channels for worker
        let (cmd_tx, cmd_rx) = kanal::bounded_async(16);
        const HLS_CHUNK_CHANNEL_CAPACITY: usize = 8;
        let (chunk_tx, chunk_rx) = kanal::bounded_async(HLS_CHUNK_CHANNEL_CAPACITY);

        // Create AsyncWorker and spawn it
        let worker = kithara_worker::AsyncWorker::new(worker_source, cmd_rx, chunk_tx);
        tokio::spawn(worker.run());

        // Create HlsSourceAdapter with assets for direct disk access
        let adapter = HlsSourceAdapter::new(chunk_rx, cmd_tx, assets, events_tx.clone());

        Ok(OpenedSource {
            source: Arc::new(adapter),
            events_tx,
        })
    }
}
