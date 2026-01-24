#![forbid(unsafe_code)]

use std::{collections::HashMap, sync::Arc};

use bytes::Bytes;
use kithara_assets::StoreOptions;
use kithara_net::NetOptions;
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;
use url::Url;

use crate::error::HlsResult;

#[derive(Clone, Debug)]
pub struct KeyContext {
    pub url: Url,
    pub iv: Option<[u8; 16]>,
}

/// Callback for processing encryption keys.
pub type KeyProcessor = Arc<dyn Fn(Bytes, KeyContext) -> HlsResult<Bytes> + Send + Sync>;

// Re-export ABR types from kithara-abr crate
pub use kithara_abr::{AbrMode, AbrOptions};

/// Encryption key handling configuration.
#[derive(Clone, Default)]
pub struct KeyOptions {
    /// Query parameters to append to key URLs.
    pub query_params: Option<HashMap<String, String>>,
    /// Headers to include in key requests.
    pub request_headers: Option<HashMap<String, String>>,
}

/// Unified parameters for HLS streaming.
///
/// Used with `Hls::open(url, params)` for the unified API.
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
    /// Events broadcast sender (optional - if not provided, events are not sent).
    pub events_tx: Option<broadcast::Sender<crate::HlsEvent>>,
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
            events_tx: None,
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

    /// Set events broadcast sender.
    ///
    /// If provided, HLS events will be sent to this channel.
    /// If not provided, events will not be sent (no overhead).
    pub fn with_events(mut self, events_tx: broadcast::Sender<crate::HlsEvent>) -> Self {
        self.events_tx = Some(events_tx);
        self
    }

    /// Set command channel capacity.
    pub fn with_command_capacity(mut self, capacity: usize) -> Self {
        self.command_capacity = capacity;
        self
    }
}
