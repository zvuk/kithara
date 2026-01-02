#![forbid(unsafe_code)]

//! # kithara-hls
//!
//! HLS VOD orchestration with caching, ABR and offline playback.
//!
//! ## Architecture (new)
//!
//! This crate provides a `HlsSource::open(...) -> HlsSession` API.
//! Internally, it uses `kithara-stream` to orchestrate fetching and expose an async byte stream.
//!
//! - No explicit stop command: stopping is done by dropping the stream.
//! - Seek is not supported yet (contract is fixed; implementation will be added via TDD).

use bytes::Bytes;
use futures::Stream;
use kithara_cache::AssetCache;
use kithara_core::AssetId;
use kithara_net::HttpClient;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;
use thiserror::Error;
use url::Url;

// Public modules
pub mod abr;
pub mod events;
pub mod fetch;
pub mod keys;
pub mod playlist;

// Private modules
mod driver;

// Re-export key types
pub use abr::{AbrConfig, AbrController, ThroughputSample};
pub use driver::{DriverError, SourceError};
pub use events::{EventEmitter, EventError, HlsEvent, VariantChangeReason};
pub use fetch::{FetchError, FetchManager, SegmentStream};
pub use keys::{KeyError, KeyManager};
pub use playlist::{PlaylistError, PlaylistManager};

#[derive(Debug, Error)]
pub enum HlsError {
    #[error("Network error: {0}")]
    Net(#[from] kithara_net::NetError),

    #[error("Cache error: {0}")]
    Cache(#[from] kithara_cache::CacheError),

    #[error("Core error: {0}")]
    Core(#[from] kithara_core::CoreError),

    #[error("Playlist parsing error: {0}")]
    PlaylistParse(String),

    #[error("Variant not found: {0}")]
    VariantNotFound(String),

    #[error("Segment not found: {0}")]
    SegmentNotFound(String),

    #[error("No suitable variant found")]
    NoSuitableVariant,

    #[error("Key processing failed: {0}")]
    KeyProcessing(String),

    #[error("ABR error: {0}")]
    Abr(String),

    #[error("Offline mode: resource not cached")]
    OfflineMiss,

    #[error("Invalid URL: {0}")]
    InvalidUrl(String),

    #[error("not implemented")]
    Unimplemented,

    #[error("Driver error: {0}")]
    Driver(String),
}

pub type HlsResult<T> = Result<T, HlsError>;

#[derive(Clone)]
pub struct HlsOptions {
    pub base_url: Option<Url>,
    pub variant_stream_selector:
        Option<Arc<dyn Fn(&hls_m3u8::MasterPlaylist) -> Option<usize> + Send + Sync>>,
    pub abr_initial_variant_index: Option<usize>,
    pub abr_min_buffer_for_up_switch: f32,
    pub abr_down_switch_buffer: f32,
    pub abr_throughput_safety_factor: f32,
    pub abr_up_hysteresis_ratio: f32,
    pub abr_down_hysteresis_ratio: f32,
    pub abr_min_switch_interval: Duration,

    // networking/retry/prefetch knobs (kept for now)
    pub request_timeout: Duration,
    pub max_retries: u32,
    pub retry_base_delay: Duration,
    pub max_retry_delay: Duration,
    pub retry_timeout: Duration,
    pub prefetch_buffer_size: usize,
    pub live_refresh_interval: Option<Duration>,

    // key processing
    pub key_processor_cb: Option<Arc<dyn Fn(Bytes, KeyContext) -> HlsResult<Bytes> + Send + Sync>>,
    pub key_query_params: Option<std::collections::HashMap<String, String>>,
    pub key_request_headers: Option<std::collections::HashMap<String, String>>,

    pub offline_mode: bool,
}

impl Default for HlsOptions {
    fn default() -> Self {
        Self {
            base_url: None,
            variant_stream_selector: None,
            abr_initial_variant_index: Some(0),
            abr_min_buffer_for_up_switch: 10.0,
            abr_down_switch_buffer: 5.0,
            abr_throughput_safety_factor: 1.5,
            abr_up_hysteresis_ratio: 1.3,
            abr_down_hysteresis_ratio: 0.8,
            abr_min_switch_interval: Duration::from_secs(30),

            request_timeout: Duration::from_secs(30),
            max_retries: 3,
            retry_base_delay: Duration::from_millis(100),
            max_retry_delay: Duration::from_secs(5),
            retry_timeout: Duration::from_secs(60),
            prefetch_buffer_size: 3,
            live_refresh_interval: None,

            key_processor_cb: None,
            key_query_params: None,
            key_request_headers: None,

            offline_mode: false,
        }
    }
}

#[derive(Clone, Debug)]
pub struct KeyContext {
    pub url: Url,
    pub iv: Option<[u8; 16]>,
}

#[derive(Clone, Debug)]
pub struct HlsSource;

impl HlsSource {
    pub async fn open(url: Url, opts: HlsOptions, cache: AssetCache) -> HlsResult<HlsSession> {
        let asset_id = AssetId::from_url(&url)?;
        let net = HttpClient::new(kithara_net::NetOptions::default());

        // Create managers (kept as-is; internals will be iterated via TDD).
        let playlist_manager =
            playlist::PlaylistManager::new(cache.clone(), net.clone(), opts.base_url.clone());
        let fetch_manager = fetch::FetchManager::new(cache.clone(), net.clone());
        let key_processor = opts.key_processor_cb.clone().map(|arc| {
            Box::new(move |bytes, ctx| arc(bytes, ctx))
                as Box<dyn Fn(Bytes, KeyContext) -> HlsResult<Bytes> + Send + Sync>
        });

        let key_manager = keys::KeyManager::new(
            cache.clone(),
            net.clone(),
            key_processor,
            opts.key_query_params.clone(),
            opts.key_request_headers.clone(),
        );

        let abr_config = abr::AbrConfig {
            min_buffer_for_up_switch: opts.abr_min_buffer_for_up_switch,
            down_switch_buffer: opts.abr_down_switch_buffer,
            throughput_safety_factor: opts.abr_throughput_safety_factor,
            up_hysteresis_ratio: opts.abr_up_hysteresis_ratio,
            down_hysteresis_ratio: opts.abr_down_hysteresis_ratio,
            min_switch_interval: opts.abr_min_switch_interval,
            initial_variant_index: opts.abr_initial_variant_index,
        };

        let abr_controller = abr::AbrController::new(
            abr_config,
            opts.variant_stream_selector.clone(),
            opts.abr_initial_variant_index.unwrap_or(0),
        );

        let event_emitter = events::EventEmitter::new();

        let driver = driver::HlsDriver::new(
            url.clone(),
            opts,
            playlist_manager,
            fetch_manager,
            key_manager,
            abr_controller,
            event_emitter,
        );

        Ok(HlsSession { asset_id, driver })
    }
}

pub struct HlsSession {
    asset_id: AssetId,
    driver: driver::HlsDriver,
}

impl HlsSession {
    pub fn asset_id(&self) -> AssetId {
        self.asset_id
    }

    pub fn stream(&self) -> Pin<Box<dyn Stream<Item = HlsResult<Bytes>> + Send + '_>> {
        Box::pin(self.driver.stream())
    }
}
