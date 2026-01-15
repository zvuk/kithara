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
    pipeline::BaseStream,
    playlist::PlaylistManager,
    session::HlsSession,
};

/// Opens HLS sessions from URLs.
#[derive(Clone, Copy, Debug, Default)]
pub struct HlsSource;

impl HlsSource {
    pub async fn open(url: Url, opts: HlsOptions) -> HlsResult<HlsSession> {
        let asset_root = asset_root_for_url(&url);
        let cancel = opts.cancel.clone().unwrap_or_default();

        // Build asset store.
        let mut builder = AssetStoreBuilder::new()
            .asset_root(&asset_root)
            .cancel(cancel.clone());

        if let Some(cache_dir) = &opts.cache_dir {
            builder = builder.root_dir(cache_dir);
        }
        if let Some(evict_config) = &opts.evict_config {
            builder = builder.evict_config(evict_config.clone());
        }

        let assets = builder.build();

        // Build HTTP client.
        let net = HttpClient::new(NetOptions {
            request_timeout: opts.request_timeout,
            retry_policy: RetryPolicy::new(
                opts.max_retries,
                opts.retry_base_delay,
                opts.max_retry_delay,
            ),
        });

        // Build managers.
        let fetch_manager = Arc::new(FetchManager::new_with_read_chunk(
            assets,
            net,
            opts.read_chunk_bytes,
        ));

        let key_manager = Arc::new(KeyManager::new(
            Arc::clone(&fetch_manager),
            opts.key_processor_cb.clone(),
            opts.key_query_params.clone(),
            opts.key_request_headers.clone(),
        ));

        let playlist_manager = Arc::new(PlaylistManager::new(
            Arc::clone(&fetch_manager),
            opts.base_url.clone(),
        ));

        // Build ABR controller.
        let abr_config = AbrConfig {
            min_buffer_for_up_switch_secs: opts.abr_min_buffer_for_up_switch as f64,
            down_switch_buffer_secs: opts.abr_down_switch_buffer as f64,
            throughput_safety_factor: opts.abr_throughput_safety_factor as f64,
            up_hysteresis_ratio: opts.abr_up_hysteresis_ratio as f64,
            down_hysteresis_ratio: opts.abr_down_switch_buffer as f64,
            min_switch_interval: opts.abr_min_switch_interval,
            initial_variant_index: opts.abr_initial_variant_index,
            ..AbrConfig::default()
        };

        let abr_controller = AbrController::new(abr_config, opts.variant_stream_selector.clone());

        // Build BaseStream directly (no HlsDriver).
        let base_stream = BaseStream::new(
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
