#![forbid(unsafe_code)]

use std::sync::Arc;

use kithara_assets::{AssetStoreBuilder, asset_root_for_url};
use kithara_net::{HttpClient, NetOptions, RetryPolicy};
use url::Url;

use crate::{
    abr::{AbrConfig, AbrController},
    error::HlsResult,
    events::EventEmitter,
    fetch::FetchManager,
    keys::KeyManager,
    options::HlsOptions,
    playlist::PlaylistManager,
    session::HlsSession,
    stream::SegmentStream,
};

/// Opens HLS sessions from URLs.
#[derive(Clone, Copy, Debug, Default)]
pub struct Hls;

impl Hls {
    pub async fn open(url: Url, opts: HlsOptions) -> HlsResult<HlsSession> {
        let asset_root = asset_root_for_url(&url);
        let cancel = opts.cancel.clone().unwrap_or_default();

        // Build asset store.
        let mut builder = AssetStoreBuilder::new()
            .asset_root(&asset_root)
            .cancel(cancel.clone());

        if let Some(cache_dir) = &opts.cache.cache_dir {
            builder = builder.root_dir(cache_dir);
        }
        if let Some(evict_config) = &opts.cache.evict_config {
            builder = builder.evict_config(evict_config.clone());
        }

        let assets = builder.build();

        // Build HTTP client.
        let net = HttpClient::new(NetOptions {
            request_timeout: opts.network.request_timeout,
            retry_policy: RetryPolicy::new(
                opts.network.max_retries,
                opts.network.retry_base_delay,
                opts.network.max_retry_delay,
            ),
            ..Default::default()
        });

        // Build managers.
        let fetch_manager = Arc::new(FetchManager::new(assets, net));

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

        // Build SegmentStream directly (no HlsDriver).
        let base_stream = SegmentStream::new(
            url,
            Arc::clone(&fetch_manager),
            playlist_manager,
            Some(key_manager),
            abr_controller,
            cancel,
        );

        let events = Arc::new(EventEmitter::new());

        Ok(HlsSession::new(base_stream, fetch_manager, events))
    }
}
