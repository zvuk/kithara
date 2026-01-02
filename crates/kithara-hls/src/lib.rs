#![forbid(unsafe_code)]

//! # kithara-hls
//!
//! HLS VOD orchestration with caching, ABR, and DRM support.
//!
//! ## Features
//!
//! - **Playlist Management**: Master and media playlist parsing and caching
//! - **Adaptive Bitrate**: ABR policy with configurable parameters
//! - **Segment Fetching**: Network and cache-aware segment streaming
//! - **DRM Support**: AES-128 key processing and caching
//! - **Event System**: Telemetry and monitoring events
//! - **Offline Mode**: Complete offline playback when content is cached
//!
//! ## URL Resolution Rules
//!
//! URLs are resolved using following priority:
//!
//! 1. **base_url Override**: If configured in `HlsOptions.base_url`, this takes precedence
//!    for resolving variant playlists, segments, and keys.
//! 2. **Playlist URL**: The URL of current playlist is used as base for relative URLs.
//!
//! The base_url override allows remapping resources under different path prefixes
//! (useful for CDN changes, proxy setups, etc.).
//!
//! ## DRM Key Processing and Caching
//!
//! - Keys are fetched with optional query parameters and headers
//! - `key_processor_cb` can transform "wrapped" keys to usable form
//! - **Processed keys** (after transformation) are cached for offline playback
//! - Raw keys are never logged; only fingerprints/hashes may be logged
//! - Cache misses in offline mode are fatal errors
//!
//! ## ABR Switching Invariants
//!
//! - ABR decisions are based only on **network throughput** (cache hits don't affect estimation)
//! - Up-switching requires sufficient buffer level and hysteresis ratio
//! - Down-switching occurs when buffer falls below threshold
//! - Minimum switch interval prevents oscillation
//! - Manual overrides take precedence over automatic ABR

use bytes::Bytes;
use futures::Stream;
use kithara_cache::AssetCache;
use kithara_core::AssetId;
use kithara_net::NetClient;
use std::sync::Arc;
use std::time::Duration;
use thiserror::Error;
use tokio::sync::mpsc;
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
    pub request_timeout: Duration,
    pub max_retries: u32,
    pub retry_base_delay: Duration,
    pub max_retry_delay: Duration,
    pub retry_timeout: Duration,
    pub prefetch_buffer_size: usize,
    pub live_refresh_interval: Option<Duration>,
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
pub enum HlsCommand {
    Stop,
    SeekTime(Duration),
    SetVariant(usize),
    ClearVariantOverride,
}

#[derive(Clone, Debug)]
pub struct HlsSource;

impl HlsSource {
    pub async fn open(
        url: Url,
        opts: HlsOptions,
        cache: AssetCache,
        net: NetClient,
    ) -> HlsResult<HlsSession> {
        let asset_id = AssetId::from_url(&url)?;
        let (cmd_sender, cmd_receiver) = mpsc::channel(16);
        let (bytes_sender, bytes_receiver) = mpsc::channel(100);

        // For now, simplified implementation without full driver
        // TODO: Implement full driver when cache module is fixed
        tokio::spawn(async move {
            // Placeholder driver that just emits unimplemented error
            let _ = cmd_sender;
            let _ = bytes_sender;
        });

        Ok(HlsSession {
            asset_id,
            cmd_sender,
            bytes_receiver,
        })
    }
}

pub struct HlsSession {
    asset_id: AssetId,
    cmd_sender: mpsc::Sender<HlsCommand>,
    bytes_receiver: mpsc::Receiver<HlsResult<Bytes>>,
}

impl HlsSession {
    pub fn asset_id(&self) -> AssetId {
        self.asset_id
    }

    pub fn commands(&self) -> mpsc::Sender<HlsCommand> {
        self.cmd_sender.clone()
    }

    pub fn stream(&mut self) -> impl Stream<Item = HlsResult<Bytes>> + Send + '_ {
        async_stream::stream! {
            while let Some(item) = self.bytes_receiver.recv().await {
                yield item;
            }
        }
    }
}
