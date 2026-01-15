#![forbid(unsafe_code)]

use std::sync::Arc;

use kithara_assets::{AssetStoreBuilder, ProcessFn, asset_root_for_url};
use kithara_net::{HttpClient, NetOptions, RetryPolicy};
use tokio::sync::broadcast;
use url::Url;

use crate::{
    abr::{AbrConfig, AbrController},
    adapter::{DecryptContext, HlsSource},
    error::HlsResult,
    events::HlsEvent,
    fetch::FetchManager,
    keys::KeyManager,
    options::HlsOptions,
    playlist::PlaylistManager,
    stream::SegmentStream,
};

/// Opens HLS sources from URLs.
#[derive(Clone, Copy, Debug, Default)]
pub struct Hls;

impl Hls {
    /// Open an HLS stream from a master playlist URL.
    ///
    /// Returns `HlsSource` which implements `Source` trait for random-access reading
    /// and provides event subscription via `events()`.
    ///
    /// # Example
    ///
    /// ```ignore
    /// use kithara_hls::{Hls, HlsOptions};
    ///
    /// let source = Hls::open(url, HlsOptions::default()).await?;
    /// let events_rx = source.events();
    /// ```
    pub async fn open(url: Url, opts: HlsOptions) -> HlsResult<HlsSource> {
        let asset_root = asset_root_for_url(&url);
        let cancel = opts.cancel.clone().unwrap_or_default();

        // Build HTTP client first (needed for KeyManager which is needed for decrypt callback).
        let net = HttpClient::new(NetOptions {
            request_timeout: opts.network.request_timeout,
            retry_policy: RetryPolicy::new(
                opts.network.max_retries,
                opts.network.retry_base_delay,
                opts.network.max_retry_delay,
            ),
            ..Default::default()
        });

        // Build base asset store without processing (needed for FetchManager/KeyManager).
        let mut base_builder = AssetStoreBuilder::new()
            .asset_root(&asset_root)
            .cancel(cancel.clone());

        if let Some(cache_dir) = &opts.cache.cache_dir {
            base_builder = base_builder.root_dir(cache_dir.clone());
        }
        if let Some(evict_config) = &opts.cache.evict_config {
            base_builder = base_builder.evict_config(evict_config.clone());
        }

        let base_assets = base_builder.build();

        // Build FetchManager and KeyManager with base assets.
        let fetch_manager = Arc::new(FetchManager::new(base_assets, net));

        let key_manager = Arc::new(KeyManager::new(
            Arc::clone(&fetch_manager),
            opts.keys.processor.clone(),
            opts.keys.query_params.clone(),
            opts.keys.request_headers.clone(),
        ));

        let playlist_manager = Arc::new(PlaylistManager::new(
            Arc::clone(&fetch_manager),
            opts.base_url.clone(),
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

        let mut assets_builder = AssetStoreBuilder::new()
            .asset_root(&asset_root)
            .cancel(cancel.clone())
            .process_fn(decrypt_fn);

        if let Some(cache_dir) = &opts.cache.cache_dir {
            assets_builder = assets_builder.root_dir(cache_dir.clone());
        }
        if let Some(evict_config) = &opts.cache.evict_config {
            assets_builder = assets_builder.evict_config(evict_config.clone());
        }

        let assets = assets_builder.build();

        // Build ABR controller.
        let abr_config = AbrConfig {
            mode: opts.abr.mode,
            min_buffer_for_up_switch_secs: opts.abr.min_buffer_for_up_switch as f64,
            down_switch_buffer_secs: opts.abr.down_switch_buffer as f64,
            throughput_safety_factor: opts.abr.throughput_safety_factor as f64,
            up_hysteresis_ratio: opts.abr.up_hysteresis_ratio as f64,
            down_hysteresis_ratio: opts.abr.down_hysteresis_ratio as f64,
            min_switch_interval: opts.abr.min_switch_interval,
            ..AbrConfig::default()
        };

        let abr_controller = AbrController::new(abr_config, opts.variant_selector.clone());

        // Create events channel.
        let (events_tx, _) = broadcast::channel::<HlsEvent>(opts.event_capacity);

        // Build SegmentStream.
        let base_stream = SegmentStream::new(
            url,
            Arc::clone(&fetch_manager),
            playlist_manager,
            Some(key_manager),
            abr_controller,
            events_tx.clone(),
            cancel,
            opts.command_capacity,
        );

        Ok(HlsSource::new(base_stream, assets, events_tx))
    }
}
