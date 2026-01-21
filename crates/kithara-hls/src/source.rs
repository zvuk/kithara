#![forbid(unsafe_code)]

use std::sync::Arc;

use kithara_assets::{AssetStoreBuilder, ProcessFn, asset_root_for_url};
use kithara_net::HttpClient;
use kithara_stream::{OpenedSource, SourceFactory, StreamError};
use tokio::sync::broadcast;
use url::Url;

use crate::{
    abr::{AbrConfig, AbrController},
    adapter::HlsSource,
    cache::{CachedLoader, FetchLoader, EncryptionInfo},
    error::{HlsError, HlsResult},
    events::HlsEvent,
    fetch::FetchManager,
    keys::KeyManager,
    options::HlsParams,
    playlist::PlaylistManager,
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

    /// Open an HLS stream from a master playlist URL.
    ///
    /// Returns `HlsSource` which implements `Source` trait for random-access reading
    /// and provides event subscription via `events()`.
    ///
    /// # Note
    /// This method is internal. Use `StreamSource::<Hls>::open(url, params)` instead.
    ///
    /// NEW ARCHITECTURE: Uses CachedLoader instead of SegmentStream.
    pub(crate) async fn open(url: Url, params: HlsParams) -> HlsResult<HlsSource> {
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

        // Build KeyManager for decryption
        let key_manager = Arc::new(KeyManager::new(
            Arc::clone(&fetch_manager),
            params.keys.processor.clone(),
            params.keys.query_params.clone(),
            params.keys.request_headers.clone(),
        ));

        // Build PlaylistManager
        let playlist_manager = Arc::new(PlaylistManager::new(
            Arc::clone(&fetch_manager),
            params.base_url.clone(),
        ));

        // Build asset store WITH decryption callback (for CachedLoader)
        let decrypt_fn: ProcessFn<EncryptionInfo> = {
            let km = Arc::clone(&key_manager);
            Arc::new(move |bytes, ctx: EncryptionInfo| {
                let km = Arc::clone(&km);
                Box::pin(async move {
                    km.decrypt(&ctx.key_url, Some(ctx.iv), bytes)
                        .await
                        .map_err(|e| e.to_string())
                })
            })
        };

        let assets = AssetStoreBuilder::new()
            .asset_root(&asset_root)
            .cancel(cancel)
            .root_dir(&params.store.cache_dir)
            .evict_config(params.store.to_evict_config())
            .process_fn(decrypt_fn)
            .build();

        // Create events channel
        let (events_tx, _) = broadcast::channel::<HlsEvent>(params.event_capacity);

        // Load master playlist to extract variant codec info
        let master = playlist_manager.master_playlist(&url).await?;
        let variant_codecs: Vec<Option<_>> =
            master.variants.iter().map(|v| v.codec.clone()).collect();

        // Build ABR controller
        let abr_config = AbrConfig {
            mode: params.abr.mode,
            min_buffer_for_up_switch_secs: params.abr.min_buffer_for_up_switch as f64,
            down_switch_buffer_secs: params.abr.down_switch_buffer as f64,
            throughput_safety_factor: params.abr.throughput_safety_factor as f64,
            up_hysteresis_ratio: params.abr.up_hysteresis_ratio as f64,
            down_hysteresis_ratio: params.abr.down_hysteresis_ratio as f64,
            min_switch_interval: params.abr.min_switch_interval,
            ..AbrConfig::default()
        };
        let abr_controller = AbrController::new(abr_config, params.abr.variant_selector.clone());

        // Create FetchLoader
        let fetch_loader = Arc::new(FetchLoader::new(
            url.clone(),
            fetch_manager,
            playlist_manager,
        ));

        // Create CachedLoader (NEW architecture!)
        let cached_loader = CachedLoader::new(fetch_loader, assets);

        // Create HlsSource wrapper
        Ok(HlsSource::new(
            cached_loader,
            abr_controller,
            events_tx,
            variant_codecs,
        ))
    }
}

impl SourceFactory for Hls {
    type Params = HlsParams;
    type Event = HlsEvent;
    type SourceImpl = HlsSource;

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
