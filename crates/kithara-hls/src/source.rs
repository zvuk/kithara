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
    media_source::HlsMediaSource,
    options::HlsParams,
    playlist::{PlaylistManager, variant_info_from_master},
    worker::{HlsSourceAdapter, HlsWorkerSource, VariantMetadata},
};

/// Marker type for HLS streaming.
///
/// ## Usage
///
/// ```ignore
/// use kithara_decode::{StreamDecoder, MediaSource};
/// use kithara_hls::{Hls, HlsParams};
///
/// // Open media source
/// let source = Hls::open_media_source(url, HlsParams::default()).await?;
/// let stream = source.open()?;
///
/// // Create decoder
/// let mut decoder = StreamDecoder::new(stream)?;
///
/// // Decode - bytes read on-demand
/// while let Some(chunk) = decoder.decode_next()? {
///     play_audio(chunk);
/// }
/// ```
#[derive(Clone, Copy, Debug, Default)]
pub struct Hls;

impl Hls {
    /// Open an HLS stream and return a MediaSource for streaming decode.
    ///
    /// This is the recommended way to use HLS with kithara-decode.
    /// Bytes are read incrementally, allowing decode to start before full download.
    ///
    /// ## Example
    ///
    /// ```ignore
    /// use kithara_decode::{StreamDecoder, MediaSource};
    /// use kithara_hls::{Hls, HlsParams};
    ///
    /// let source = Hls::open_media_source(url, HlsParams::default()).await?;
    /// let stream = source.open()?;
    /// let mut decoder = StreamDecoder::new(stream)?;
    ///
    /// while let Some(chunk) = decoder.decode_next()? {
    ///     play_audio(chunk);
    /// }
    /// ```
    pub async fn open_media_source(url: Url, params: HlsParams) -> HlsResult<HlsMediaSource> {
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

        // Create and return HlsMediaSource
        Ok(HlsMediaSource::new(
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
    /// Returns `HlsWorkerSource` for push-based streaming with AsyncWorker.
    ///
    /// **Note:** For new code, prefer `open_media_source()` which provides
    /// better latency through streaming decode.
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
