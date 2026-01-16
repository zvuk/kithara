#![forbid(unsafe_code)]

use std::sync::Arc;

use kithara_assets::{AssetStoreBuilder, ProcessFn, asset_root_for_url};
use kithara_net::HttpClient;
use kithara_stream::{OpenedSource, StreamError, StreamSource};
use tokio::sync::broadcast;
use url::Url;

use crate::{
    abr::{AbrConfig, AbrController},
    adapter::{DecryptContext, HlsSource},
    error::{HlsError, HlsResult},
    events::HlsEvent,
    fetch::FetchManager,
    keys::KeyManager,
    options::HlsParams,
    playlist::PlaylistManager,
    stream::{SegmentStream, SegmentStreamParams},
};

/// Marker type for HLS streaming with the unified `Stream<S>` API.
///
/// ## Usage
///
/// ```ignore
/// use kithara_stream::Stream;
/// use kithara_hls::{Hls, HlsParams};
/// use kithara_assets::StoreOptions;
///
/// let params = HlsParams::new(StoreOptions::new("/tmp/cache"));
/// let stream = Stream::<Hls>::open(url, params).await?;
/// let events = stream.events();  // Receiver<HlsEvent>
/// ```
#[derive(Clone, Copy, Debug, Default)]
pub struct Hls;

impl Hls {
    /// Open an HLS stream from a master playlist URL.
    ///
    /// Returns `HlsSource` which implements `Source` trait for random-access reading
    /// and provides event subscription via `events()`.
    pub async fn open(url: Url, params: HlsParams) -> HlsResult<HlsSource> {
        let asset_root = asset_root_for_url(&url);
        let cancel = params.cancel.clone().unwrap_or_default();

        let net = HttpClient::new(params.net.clone());

        // Build base asset store without processing (needed for FetchManager/KeyManager).
        let base_assets = AssetStoreBuilder::new()
            .asset_root(&asset_root)
            .cancel(cancel.clone())
            .root_dir(&params.store.cache_dir)
            .evict_config(params.store.to_evict_config())
            .build();

        // Build FetchManager and KeyManager with base assets.
        let fetch_manager = Arc::new(FetchManager::new(base_assets, net));

        let key_manager = Arc::new(KeyManager::new(
            Arc::clone(&fetch_manager),
            params.keys.processor.clone(),
            params.keys.query_params.clone(),
            params.keys.request_headers.clone(),
        ));

        let playlist_manager = Arc::new(PlaylistManager::new(
            Arc::clone(&fetch_manager),
            params.base_url.clone(),
        ));

        // Build asset store with decryption callback.
        let decrypt_fn: ProcessFn<DecryptContext> = {
            let km = Arc::clone(&key_manager);
            Arc::new(move |bytes, ctx: DecryptContext| {
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
            .cancel(cancel.clone())
            .root_dir(&params.store.cache_dir)
            .evict_config(params.store.to_evict_config())
            .process_fn(decrypt_fn)
            .build();

        // Build ABR controller.
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

        // Create events channel.
        let (events_tx, _) = broadcast::channel::<HlsEvent>(params.event_capacity);

        // Build SegmentStream.
        let base_stream = SegmentStream::new(SegmentStreamParams {
            master_url: url,
            fetch: Arc::clone(&fetch_manager),
            playlist_manager,
            key_manager: Some(key_manager),
            abr_controller,
            events_tx: events_tx.clone(),
            cancel,
            command_capacity: params.command_capacity,
            min_sample_bytes: params.abr.min_sample_bytes,
        });

        Ok(HlsSource::new(base_stream, assets, events_tx))
    }
}

impl StreamSource for Hls {
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
