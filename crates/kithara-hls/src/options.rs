#![forbid(unsafe_code)]

use std::{collections::HashMap, path::PathBuf, sync::Arc, time::Duration};

use bytes::Bytes;
use kithara_assets::EvictConfig;
use tokio_util::sync::CancellationToken;
use url::Url;

use crate::{error::HlsResult, playlist::MasterPlaylist};

#[derive(Clone, Debug)]
pub struct KeyContext {
    pub url: Url,
    pub iv: Option<[u8; 16]>,
}

/// Callback for selecting variant stream index from master playlist.
pub type VariantSelector = Arc<dyn Fn(&MasterPlaylist) -> Option<usize> + Send + Sync>;

/// Callback for processing encryption keys.
pub type KeyProcessor = Arc<dyn Fn(Bytes, KeyContext) -> HlsResult<Bytes> + Send + Sync>;

/// ABR mode selection.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AbrMode {
    /// Automatic bitrate adaptation (ABR enabled).
    /// Optionally specify initial variant index (defaults to 0).
    Auto(Option<usize>),
    /// Manual variant selection (ABR disabled).
    /// Always use the specified variant index.
    Manual(usize),
}

impl Default for AbrMode {
    fn default() -> Self {
        Self::Auto(None)
    }
}

/// ABR (Adaptive Bitrate) configuration.
#[derive(Clone, Debug)]
pub struct AbrOptions {
    /// ABR mode: Auto (adaptive) or Manual (fixed variant).
    pub mode: AbrMode,
    /// Minimum buffer level (seconds) required for up-switch.
    pub min_buffer_for_up_switch: f32,
    /// Buffer level (seconds) that triggers down-switch.
    pub down_switch_buffer: f32,
    /// Safety factor for throughput estimation (e.g., 1.5 means use 66% of estimated throughput).
    pub throughput_safety_factor: f32,
    /// Hysteresis ratio for up-switch (bandwidth must exceed target by this factor).
    pub up_hysteresis_ratio: f32,
    /// Hysteresis ratio for down-switch.
    pub down_hysteresis_ratio: f32,
    /// Minimum interval between variant switches.
    pub min_switch_interval: Duration,
}

impl Default for AbrOptions {
    fn default() -> Self {
        Self {
            mode: AbrMode::default(),
            min_buffer_for_up_switch: 10.0,
            down_switch_buffer: 5.0,
            throughput_safety_factor: 1.5,
            up_hysteresis_ratio: 1.3,
            down_hysteresis_ratio: 0.8,
            min_switch_interval: Duration::from_secs(30),
        }
    }
}

/// Network and retry configuration.
#[derive(Clone, Debug)]
pub struct NetworkOptions {
    /// Timeout for individual HTTP requests.
    pub request_timeout: Duration,
    /// Maximum number of retries for failed requests.
    pub max_retries: u32,
    /// Base delay between retries (exponential backoff).
    pub retry_base_delay: Duration,
    /// Maximum delay between retries.
    pub max_retry_delay: Duration,
}

impl Default for NetworkOptions {
    fn default() -> Self {
        Self {
            request_timeout: Duration::from_secs(30),
            max_retries: 3,
            retry_base_delay: Duration::from_millis(100),
            max_retry_delay: Duration::from_secs(5),
        }
    }
}

/// Cache and storage configuration.
#[derive(Clone, Debug, Default)]
pub struct CacheOptions {
    /// Directory for persistent cache storage.
    pub cache_dir: Option<PathBuf>,
    /// Eviction configuration for cache management.
    pub evict_config: Option<EvictConfig>,
}

/// Encryption key handling configuration.
#[derive(Clone, Default)]
pub struct KeyOptions {
    /// Custom key processor callback.
    pub processor: Option<KeyProcessor>,
    /// Query parameters to append to key URLs.
    pub query_params: Option<HashMap<String, String>>,
    /// Headers to include in key requests.
    pub request_headers: Option<HashMap<String, String>>,
}

/// Configuration options for HLS playback.
#[derive(Clone)]
pub struct HlsOptions {
    /// Base URL for resolving relative playlist/segment URLs.
    pub base_url: Option<Url>,
    /// Custom variant selector callback.
    pub variant_selector: Option<VariantSelector>,
    /// ABR (Adaptive Bitrate) configuration.
    pub abr: AbrOptions,
    /// Network and retry configuration.
    pub network: NetworkOptions,
    /// Cache and storage configuration.
    pub cache: CacheOptions,
    /// Encryption key handling configuration.
    pub keys: KeyOptions,
    /// Cancellation token for graceful shutdown.
    pub cancel: Option<CancellationToken>,
    /// Capacity of the events broadcast channel.
    pub event_capacity: usize,
    /// Capacity of the command mpsc channel.
    pub command_capacity: usize,
}

impl Default for HlsOptions {
    fn default() -> Self {
        Self {
            base_url: None,
            variant_selector: None,
            abr: AbrOptions::default(),
            network: NetworkOptions::default(),
            cache: CacheOptions::default(),
            keys: KeyOptions::default(),
            cancel: None,
            event_capacity: 32,
            command_capacity: 8,
        }
    }
}
