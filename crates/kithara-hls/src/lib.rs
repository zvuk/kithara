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
//! - Seek is supported for rodio playback via `HlsSession::source()` + `kithara-io::Reader`.
//!
//! ## Public contracts (explicit)
//!
//! The explicit public contracts are:
//! - [`HlsSourceContract`] — open an HLS session for a URL + options.
//!
//! Concrete types (like [`HlsSource`]) implement these traits.

use std::{pin::Pin, sync::Arc, time::Duration};

use async_trait::async_trait;
use bytes::Bytes;
use futures::Stream;
use kithara_assets::{AssetStore, ResourceKey};
use kithara_core::AssetId;
use kithara_net::HttpClient;
use thiserror::Error;
use tokio::sync::broadcast;
use url::Url;

pub mod abr;
mod driver;
pub mod events;
pub mod fetch;
pub mod keys;
pub mod playlist;
pub mod session;

// Internal modules (exposed for crate tests and internal plumbing).
pub mod cursor;

// Re-export key types
pub use abr::{
    AbrConfig, AbrController, AbrDecision, AbrReason, ThroughputSample, ThroughputSampleSource,
};
pub use driver::{DriverError, SourceError};
pub use events::{EventEmitter, HlsEvent};
pub use fetch::{FetchManager, SegmentStream};
pub use keys::{KeyError, KeyManager};
pub use playlist::{PlaylistError, PlaylistManager};
pub use session::HlsSessionSource;

#[derive(Debug, Error)]
pub enum HlsError {
    #[error("Network error: {0}")]
    Net(#[from] kithara_net::NetError),

    #[error("Assets error: {0}")]
    Assets(#[from] kithara_assets::AssetsError),

    #[error("Storage error: {0}")]
    Storage(#[from] kithara_storage::StorageError),

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
        Option<Arc<dyn Fn(&crate::playlist::MasterPlaylist) -> Option<usize> + Send + Sync>>,
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
        }
    }
}

#[derive(Clone, Debug)]
pub struct KeyContext {
    pub url: Url,
    pub iv: Option<[u8; 16]>,
}

#[async_trait]
pub trait HlsSourceContract: Send + Sync + 'static {
    async fn open(&self, url: Url, opts: HlsOptions, assets: AssetStore) -> HlsResult<HlsSession>;
}

#[derive(Clone, Copy, Debug, Default)]
pub struct HlsSource;

#[async_trait]
impl HlsSourceContract for HlsSource {
    async fn open(&self, url: Url, opts: HlsOptions, assets: AssetStore) -> HlsResult<HlsSession> {
        let asset_id = AssetId::from_url(&url)?;
        let asset_root = ResourceKey::asset_root_for_url(&url);
        let net = HttpClient::new(kithara_net::NetOptions::default());

        // Create managers (kept as-is; internals will be iterated via TDD).
        let playlist_manager = playlist::PlaylistManager::new(
            asset_root.clone(),
            assets.clone(),
            net.clone(),
            opts.base_url.clone(),
        );
        let fetch_manager =
            fetch::FetchManager::new(asset_root.clone(), assets.clone(), net.clone());
        let key_processor = opts.key_processor_cb.clone().map(|arc| {
            Box::new(move |bytes, ctx| arc(bytes, ctx))
                as Box<dyn Fn(Bytes, KeyContext) -> HlsResult<Bytes> + Send + Sync>
        });

        let key_manager = keys::KeyManager::new(
            asset_root.clone(),
            assets.clone(),
            net.clone(),
            key_processor,
            opts.key_query_params.clone(),
            opts.key_request_headers.clone(),
        );

        let abr_config = abr::AbrConfig {
            min_buffer_for_up_switch_secs: f64::from(opts.abr_min_buffer_for_up_switch),
            down_switch_buffer_secs: f64::from(opts.abr_down_switch_buffer),
            throughput_safety_factor: f64::from(opts.abr_throughput_safety_factor),
            up_hysteresis_ratio: f64::from(opts.abr_up_hysteresis_ratio),
            down_hysteresis_ratio: f64::from(opts.abr_down_hysteresis_ratio),
            min_switch_interval: opts.abr_min_switch_interval,
            initial_variant_index: opts.abr_initial_variant_index,
            sample_window: std::time::Duration::from_secs(30),
        };

        let abr_controller = abr::AbrController::new(
            abr_config,
            opts.variant_stream_selector.clone(),
            opts.abr_initial_variant_index.unwrap_or(0),
        );

        let event_emitter = events::EventEmitter::new();

        let driver = driver::HlsDriver::new(
            url.clone(),
            opts.clone(),
            playlist_manager,
            fetch_manager,
            key_manager,
            abr_controller,
            event_emitter,
        );

        Ok(HlsSession {
            asset_id,
            master_url: url,
            opts,
            assets,
            driver,
        })
    }
}

impl HlsSource {
    /// Convenience associated constructor matching the historical API.
    pub async fn open(url: Url, opts: HlsOptions, assets: AssetStore) -> HlsResult<HlsSession> {
        HlsSource.open(url, opts, assets).await
    }
}

pub struct HlsSession {
    asset_id: AssetId,
    master_url: Url,
    opts: HlsOptions,
    assets: AssetStore,
    driver: driver::HlsDriver,
}

impl HlsSession {
    pub fn asset_id(&self) -> AssetId {
        self.asset_id
    }

    pub fn stream(&self) -> Pin<Box<dyn Stream<Item = HlsResult<Bytes>> + Send + '_>> {
        Box::pin(self.driver.stream())
    }

    pub fn events(&self) -> broadcast::Receiver<HlsEvent> {
        self.driver.events()
    }

    /// Build an I/O `Source` adapter for this session, suitable for wrapping into
    /// `kithara-io::Reader` (`Read + Seek`) for `rodio::Decoder`.
    ///
    /// This mirrors `kithara-file` example structure:
    /// - `session.source().await?`
    /// - `Reader::new(Arc::new(source))`
    pub async fn source(&self) -> HlsResult<HlsSessionSource> {
        let asset_root = ResourceKey::asset_root_for_url(&self.master_url);
        let net = HttpClient::new(kithara_net::NetOptions::default());

        let playlist_manager = playlist::PlaylistManager::new(
            asset_root.clone(),
            self.assets.clone(),
            net.clone(),
            self.opts.base_url.clone(),
        );

        let fetch_manager = fetch::FetchManager::new(asset_root.clone(), self.assets.clone(), net);

        Ok(HlsSessionSource::new(
            self.master_url.clone(),
            self.opts.clone(),
            self.assets.clone(),
            playlist_manager,
            fetch_manager,
        ))
    }
}
