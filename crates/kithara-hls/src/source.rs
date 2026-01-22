#![forbid(unsafe_code)]

use std::sync::Arc;

use kithara_assets::{AssetStoreBuilder, ProcessFn, asset_root_for_url};
use kithara_net::HttpClient;
use kithara_stream::{OpenedSource, SourceFactory, StreamError};
use kithara_worker::Worker;
use tokio::sync::broadcast;
use url::Url;

use crate::{
    abr::{AbrConfig, AbrController},
    cache::{EncryptionInfo, FetchLoader, Loader},
    error::{HlsError, HlsResult},
    events::HlsEvent,
    fetch::FetchManager,
    keys::KeyManager,
    options::HlsParams,
    playlist::PlaylistManager,
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
    /// Open HLS stream and return worker source for stream-based decoding.
    ///
    /// Returns components for building StreamPipeline:
    /// - `HlsWorkerSource`: implements AsyncWorkerSource
    /// - `broadcast::Receiver<HlsEvent>`: for monitoring events
    ///
    /// # Usage with StreamPipeline
    ///
    /// ```ignore
    /// use kithara_hls::Hls;
    /// use kithara_decode::{HlsStreamDecoder, StreamPipeline};
    ///
    /// let (worker_source, mut events_rx) = Hls::open_stream(url, params).await?;
    /// let decoder = HlsStreamDecoder::new();
    /// let pipeline = StreamPipeline::new(worker_source, decoder).await?;
    ///
    /// // Consume PCM chunks
    /// let pcm_rx = pipeline.pcm_rx();
    /// while let Ok(chunk) = pcm_rx.recv() {
    ///     play_audio(chunk);
    /// }
    /// ```
    pub async fn open_stream(
        url: Url,
        params: HlsParams,
    ) -> HlsResult<(HlsWorkerSource, broadcast::Receiver<HlsEvent>)> {
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

        // Create events channel
        let (events_tx, events_rx) = broadcast::channel::<HlsEvent>(params.event_capacity);

        // Determine initial variant
        let initial_variant = params
            .abr
            .variant_selector
            .as_ref()
            .and_then(|selector| (selector)(&master))
            .unwrap_or(0);

        // Create HlsWorkerSource
        let worker_source = HlsWorkerSource::new(
            fetch_loader as Arc<dyn Loader>,
            Arc::clone(&fetch_manager),
            master,
            variant_metadata,
            initial_variant,
            Some(params.abr.clone()),
            Some(events_tx),
            cancel,
        );

        Ok((worker_source, events_rx))
    }

    /// Open an HLS stream from a master playlist URL.
    ///
    /// Returns `HlsSourceAdapter` which implements `Source` trait for random-access reading
    /// and provides event subscription via `events()`.
    ///
    /// # Note
    /// This method is internal. Use `StreamSource::<Hls>::open(url, params)` instead.
    ///
    /// NEW ARCHITECTURE: Uses worker-based streaming with HlsSourceAdapter.
    pub(crate) async fn open(url: Url, params: HlsParams) -> HlsResult<HlsSourceAdapter> {
        let asset_root = asset_root_for_url(&url);
        let cancel = params.cancel.clone().unwrap_or_default();
        let net = HttpClient::new(params.net.clone());

        // Build base asset store (for FetchManager to use)
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

        // Extract variant metadata from master playlist
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

        // Create events channel
        let (events_tx, _) = broadcast::channel::<HlsEvent>(params.event_capacity);

        // Determine initial variant
        let initial_variant = params
            .abr
            .variant_selector
            .as_ref()
            .and_then(|selector| (selector)(&master))
            .unwrap_or(0);

        // Create HlsWorkerSource
        let worker_source = HlsWorkerSource::new(
            fetch_loader as Arc<dyn Loader>,
            Arc::clone(&fetch_manager),
            master,
            variant_metadata,
            initial_variant,
            Some(params.abr.clone()),
            Some(events_tx.clone()),
            cancel,
        );

        // Create channels for worker
        // Keep small for low memory usage (backpressure)
        let (cmd_tx, cmd_rx) = kanal::bounded_async(16);
        let (chunk_tx, chunk_rx) = kanal::bounded_async(2);

        // Create AsyncWorker and spawn it
        let worker = kithara_worker::AsyncWorker::new(worker_source, cmd_rx, chunk_tx);
        tokio::spawn(worker.run());

        // Create and return HlsSourceAdapter
        Ok(HlsSourceAdapter::new(chunk_rx, cmd_tx, events_tx))
    }
}

impl SourceFactory for Hls {
    type Params = HlsParams;
    type Event = HlsEvent;
    type SourceImpl = HlsSourceAdapter;

    async fn open(
        url: Url,
        params: Self::Params,
    ) -> Result<OpenedSource<Self::SourceImpl, Self::Event>, StreamError<HlsError>> {
        let hls_source = Hls::open(url, params.clone())
            .await
            .map_err(StreamError::Source)?;

        let events_tx = hls_source.events_tx().clone();

        Ok(OpenedSource {
            source: Arc::new(hls_source),
            events_tx,
        })
    }
}
