#![forbid(unsafe_code)]

use std::{collections::HashMap, sync::Arc, time::Duration};

use bytes::Bytes;
use kithara_assets::StoreOptions;
use kithara_net::NetOptions;
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
#[derive(Clone)]
pub struct AbrOptions {
    /// ABR mode: Auto (adaptive) or Manual (fixed variant).
    pub mode: AbrMode,
    /// Custom variant selector callback.
    pub variant_selector: Option<VariantSelector>,
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
    /// Minimum bytes to accumulate before pushing a throughput sample.
    pub min_sample_bytes: u64,
}

impl Default for AbrOptions {
    fn default() -> Self {
        Self {
            mode: AbrMode::default(),
            variant_selector: None,
            min_buffer_for_up_switch: 10.0,
            down_switch_buffer: 5.0,
            throughput_safety_factor: 1.5,
            up_hysteresis_ratio: 1.3,
            down_hysteresis_ratio: 0.8,
            min_switch_interval: Duration::from_secs(30),
            min_sample_bytes: 32_000, // 32KB
        }
    }
}

/// Encryption key handling configuration.
#[derive(Clone, Default)]
pub struct KeyOptions {
    /// Custom key processor callback (internal, for DRM).
    pub(crate) processor: Option<KeyProcessor>,
    /// Query parameters to append to key URLs.
    pub query_params: Option<HashMap<String, String>>,
    /// Headers to include in key requests.
    pub request_headers: Option<HashMap<String, String>>,
}

/// Unified parameters for HLS streaming.
///
/// Used with `StreamSource::<Hls>::open(url, params)` for the unified API.
#[derive(Clone)]
pub struct HlsParams {
    /// Storage configuration (required).
    pub store: StoreOptions,
    /// Network configuration (from kithara-net).
    pub net: NetOptions,
    /// ABR (Adaptive Bitrate) configuration.
    pub abr: AbrOptions,
    /// Encryption key handling configuration.
    pub keys: KeyOptions,
    /// Base URL for resolving relative playlist/segment URLs.
    pub base_url: Option<Url>,
    /// Cancellation token for graceful shutdown.
    pub cancel: Option<CancellationToken>,
    /// Capacity of the events broadcast channel.
    pub event_capacity: usize,
    /// Capacity of the command mpsc channel.
    pub command_capacity: usize,
}

impl Default for HlsParams {
    fn default() -> Self {
        Self::new(StoreOptions::default())
    }
}

impl HlsParams {
    /// Create new HLS params with the given store options.
    pub fn new(store: StoreOptions) -> Self {
        Self {
            store,
            net: NetOptions::default(),
            abr: AbrOptions::default(),
            keys: KeyOptions::default(),
            base_url: None,
            cancel: None,
            event_capacity: 32,
            command_capacity: 8,
        }
    }

    /// Set network options.
    pub fn with_net(mut self, net: NetOptions) -> Self {
        self.net = net;
        self
    }

    /// Set ABR options.
    pub fn with_abr(mut self, abr: AbrOptions) -> Self {
        self.abr = abr;
        self
    }

    /// Set key options.
    pub fn with_keys(mut self, keys: KeyOptions) -> Self {
        self.keys = keys;
        self
    }

    /// Set base URL.
    pub fn with_base_url(mut self, base_url: Url) -> Self {
        self.base_url = Some(base_url);
        self
    }

    /// Set cancellation token.
    pub fn with_cancel(mut self, cancel: CancellationToken) -> Self {
        self.cancel = Some(cancel);
        self
    }

    /// Set event channel capacity.
    pub fn with_event_capacity(mut self, capacity: usize) -> Self {
        self.event_capacity = capacity;
        self
    }

    /// Set command channel capacity.
    pub fn with_command_capacity(mut self, capacity: usize) -> Self {
        self.command_capacity = capacity;
        self
    }
}
