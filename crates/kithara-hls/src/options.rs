#![forbid(unsafe_code)]

use std::{collections::HashMap, sync::Arc};

use bytes::Bytes;
use kithara_assets::{BytePool, StoreOptions};
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

/// Configuration for HLS streaming.
///
/// Used with `Stream::<Hls>::new(config)`.
#[derive(Clone)]
pub struct HlsConfig {
    /// Master playlist URL.
    pub url: Url,
    /// Storage configuration.
    pub store: StoreOptions,
    /// Network configuration.
    pub net: NetOptions,
    /// ABR (Adaptive Bitrate) configuration.
    pub abr: AbrOptions,
    /// Encryption key handling configuration.
    pub keys: KeyOptions,
    /// Base URL for resolving relative playlist/segment URLs.
    pub base_url: Option<Url>,
    /// Cancellation token for graceful shutdown.
    pub cancel: Option<CancellationToken>,
    /// Events broadcast sender (optional - if not provided, one is created internally).
    pub events_tx: Option<broadcast::Sender<crate::HlsEvent>>,
    /// Capacity of the command channel.
    pub command_channel_capacity: usize,
    /// Capacity of the chunk/data channel.
    pub chunk_channel_capacity: usize,
    /// Capacity of the events broadcast channel (used when events_tx is not provided).
    pub events_channel_capacity: usize,
    /// Buffer pool (shared across all components, created if not provided).
    pub pool: Option<BytePool>,
}

impl Default for HlsConfig {
    fn default() -> Self {
        Self {
            url: Url::parse("http://localhost/stream.m3u8").expect("valid default URL"),
            store: StoreOptions::default(),
            net: NetOptions::default(),
            abr: AbrOptions::default(),
            keys: KeyOptions::default(),
            base_url: None,
            cancel: None,
            events_tx: None,
            command_channel_capacity: 16,
            chunk_channel_capacity: 8,
            events_channel_capacity: 32,
            pool: None,
        }
    }
}

impl HlsConfig {
    /// Create new HLS config with URL.
    pub fn new(url: Url) -> Self {
        Self {
            url,
            store: StoreOptions::default(),
            net: NetOptions::default(),
            abr: AbrOptions::default(),
            keys: KeyOptions::default(),
            base_url: None,
            cancel: None,
            events_tx: None,
            command_channel_capacity: 16,
            chunk_channel_capacity: 8,
            events_channel_capacity: 32,
            pool: None,
        }
    }

    /// Set storage options.
    pub fn with_store(mut self, store: StoreOptions) -> Self {
        self.store = store;
        self
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
    pub fn with_events(mut self, events_tx: broadcast::Sender<crate::HlsEvent>) -> Self {
        self.events_tx = Some(events_tx);
        self
    }

    /// Set command channel capacity.
    pub fn with_command_channel_capacity(mut self, capacity: usize) -> Self {
        self.command_channel_capacity = capacity;
        self
    }

    /// Set chunk/data channel capacity.
    pub fn with_chunk_channel_capacity(mut self, capacity: usize) -> Self {
        self.chunk_channel_capacity = capacity;
        self
    }

    /// Set events broadcast channel capacity.
    pub fn with_events_channel_capacity(mut self, capacity: usize) -> Self {
        self.events_channel_capacity = capacity;
        self
    }

    /// Set buffer pool (shared across all components).
    pub fn with_pool(mut self, pool: BytePool) -> Self {
        self.pool = Some(pool);
        self
    }
}
