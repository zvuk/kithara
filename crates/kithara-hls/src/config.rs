#![forbid(unsafe_code)]

use std::{collections::HashMap, sync::Arc};

use bytes::Bytes;
use kithara_assets::{BytePool, StoreOptions};
use kithara_events::EventBus;
use kithara_net::{Headers, NetOptions};
use kithara_platform::ThreadPool;
use tokio_util::sync::CancellationToken;
use url::Url;

use crate::error::HlsResult;

#[derive(Clone, Debug)]
pub struct KeyContext {
    pub iv: Option<[u8; 16]>,
    pub url: Url,
}

/// Callback for processing encryption keys.
pub type KeyProcessor = Arc<dyn Fn(Bytes, KeyContext) -> HlsResult<Bytes> + Send + Sync>;

// Re-export ABR types from kithara-abr crate
pub use kithara_abr::{AbrMode, AbrOptions};

/// Encryption key handling configuration.
#[derive(Clone, Default)]
pub struct KeyOptions {
    /// Callback for processing (e.g. decrypting) raw key bytes after fetch.
    pub key_processor: Option<KeyProcessor>,
    /// Query parameters to append to key URLs.
    pub query_params: Option<HashMap<String, String>>,
    /// Headers to include in key requests.
    pub request_headers: Option<HashMap<String, String>>,
}

impl std::fmt::Debug for KeyOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("KeyOptions")
            .field(
                "key_processor",
                &self.key_processor.as_ref().map(|_| "KeyProcessor"),
            )
            .field("query_params", &self.query_params)
            .field("request_headers", &self.request_headers)
            .finish()
    }
}

impl KeyOptions {
    /// Create default key options.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Set query parameters to append to key URLs.
    #[must_use]
    pub fn with_query_params(mut self, params: HashMap<String, String>) -> Self {
        self.query_params = Some(params);
        self
    }

    /// Set headers to include in key requests.
    #[must_use]
    pub fn with_request_headers(mut self, headers: HashMap<String, String>) -> Self {
        self.request_headers = Some(headers);
        self
    }

    /// Set callback for processing raw key bytes after fetch.
    pub fn with_key_processor(mut self, processor: KeyProcessor) -> Self {
        self.key_processor = Some(processor);
        self
    }
}

/// Configuration for HLS streaming.
///
/// Used with `Stream::<Hls>::new(config)`.
#[derive(Clone)]
pub struct HlsConfig {
    /// ABR (Adaptive Bitrate) configuration.
    pub abr: AbrOptions,
    /// Base URL for resolving relative playlist/segment URLs.
    pub base_url: Option<Url>,
    /// Cancellation token for graceful shutdown.
    pub cancel: Option<CancellationToken>,
    /// Capacity of the event bus channel (used when `bus` is not provided).
    pub event_channel_capacity: usize,
    /// Event bus (optional - if not provided, one is created internally).
    pub bus: Option<EventBus>,
    /// Encryption key handling configuration.
    pub keys: KeyOptions,
    /// Max bytes the downloader may be ahead of the reader before it pauses.
    ///
    /// - `Some(n)` — pause when downloaded - read > n bytes (backpressure)
    /// - `None` — no backpressure, download as fast as possible
    pub look_ahead_bytes: Option<u64>,
    /// Max segments to download per step.
    ///
    /// Higher values reduce per-step overhead (ABR decisions happen per-batch)
    /// but reduce ABR reactivity. Default: 3.
    pub download_batch_size: usize,
    /// Optional name for cache disambiguation.
    ///
    /// When multiple URLs share the same canonical form (e.g. differ only in
    /// query parameters), setting a unique `name` ensures each gets its own
    /// cache directory.
    pub name: Option<String>,
    /// Network configuration.
    pub net: NetOptions,
    /// Additional HTTP headers to include in all requests.
    pub headers: Option<Headers>,
    /// Buffer pool (shared across all components, created if not provided).
    pub pool: Option<BytePool>,
    /// Storage configuration.
    pub store: StoreOptions,
    /// Thread pool for background work.
    ///
    /// Shared across all components. Defaults to the global rayon pool.
    pub thread_pool: ThreadPool,
    /// Master playlist URL.
    pub url: Url,
    /// Force in-memory storage (no disk caching).
    ///
    /// When `true`, segments are stored in memory and never written to disk.
    /// Useful for content marked with `#EXT-X-ALLOW-CACHE:NO` or when disk
    /// caching is not desired.
    pub ephemeral: bool,
}

impl Default for HlsConfig {
    fn default() -> Self {
        Self {
            abr: AbrOptions::default(),
            base_url: None,
            cancel: None,
            event_channel_capacity: 32,
            bus: None,
            keys: KeyOptions::default(),
            look_ahead_bytes: None,
            name: None,
            net: NetOptions::default(),
            headers: None,
            pool: None,
            download_batch_size: 3,
            store: StoreOptions::default(),
            thread_pool: ThreadPool::default(),
            url: Url::parse("http://localhost/stream.m3u8").expect("valid default URL"),
            ephemeral: false,
        }
    }
}

impl HlsConfig {
    /// Create new HLS config with URL.
    #[must_use]
    pub fn new(url: Url) -> Self {
        Self {
            abr: AbrOptions::default(),
            base_url: None,
            cancel: None,
            event_channel_capacity: 32,
            bus: None,
            keys: KeyOptions::default(),
            look_ahead_bytes: None,
            name: None,
            net: NetOptions::default(),
            headers: None,
            pool: None,
            download_batch_size: 3,
            store: StoreOptions::default(),
            thread_pool: ThreadPool::default(),
            url,
            ephemeral: false,
        }
    }

    /// Set name for cache disambiguation.
    ///
    /// When multiple URLs share the same canonical form (differ only in query
    /// parameters), a unique name ensures each gets its own cache directory.
    pub fn with_name<S: Into<String>>(mut self, name: S) -> Self {
        self.name = Some(name.into());
        self
    }

    /// Set storage options.
    #[must_use]
    pub fn with_store(mut self, store: StoreOptions) -> Self {
        self.store = store;
        self
    }

    /// Set network options.
    #[must_use]
    pub fn with_net(mut self, net: NetOptions) -> Self {
        self.net = net;
        self
    }

    /// Set additional HTTP headers for all requests.
    #[must_use]
    pub fn with_headers(mut self, headers: Headers) -> Self {
        self.headers = Some(headers);
        self
    }

    /// Set ABR options.
    #[must_use]
    pub fn with_abr(mut self, abr: AbrOptions) -> Self {
        self.abr = abr;
        self
    }

    /// Set key options.
    #[must_use]
    pub fn with_keys(mut self, keys: KeyOptions) -> Self {
        self.keys = keys;
        self
    }

    /// Set base URL.
    #[must_use]
    pub fn with_base_url(mut self, base_url: Url) -> Self {
        self.base_url = Some(base_url);
        self
    }

    /// Set cancellation token.
    #[must_use]
    pub fn with_cancel(mut self, cancel: CancellationToken) -> Self {
        self.cancel = Some(cancel);
        self
    }

    /// Set event bus for subscribing to HLS events.
    #[must_use]
    pub fn with_events(mut self, bus: EventBus) -> Self {
        self.bus = Some(bus);
        self
    }

    /// Set event bus channel capacity.
    #[must_use]
    pub fn with_event_channel_capacity(mut self, capacity: usize) -> Self {
        self.event_channel_capacity = capacity;
        self
    }

    /// Set buffer pool (shared across all components).
    #[must_use]
    pub fn with_pool(mut self, pool: BytePool) -> Self {
        self.pool = Some(pool);
        self
    }

    /// Set max bytes the downloader may be ahead of the reader before it pauses.
    ///
    /// - `Some(n)` — enable backpressure, pause when ahead by n bytes
    /// - `None` — disable backpressure, download as fast as possible
    #[must_use]
    pub fn with_look_ahead_bytes(mut self, bytes: Option<u64>) -> Self {
        self.look_ahead_bytes = bytes;
        self
    }

    /// Set thread pool for background work.
    ///
    /// The pool is shared across all components. Defaults to the global rayon pool.
    #[must_use]
    pub fn with_thread_pool(mut self, pool: ThreadPool) -> Self {
        self.thread_pool = pool;
        self
    }

    /// Force in-memory storage (no disk caching).
    #[must_use]
    pub fn with_ephemeral(mut self, ephemeral: bool) -> Self {
        self.ephemeral = ephemeral;
        self
    }
}
