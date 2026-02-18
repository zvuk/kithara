#![forbid(unsafe_code)]

use std::{collections::HashMap, sync::Arc};

use bytes::Bytes;
use derivative::Derivative;
use derive_setters::Setters;
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
#[derive(Clone, Default, Derivative, Setters)]
#[derivative(Debug)]
#[setters(prefix = "with_", strip_option)]
pub struct KeyOptions {
    /// Callback for processing (e.g. decrypting) raw key bytes after fetch.
    #[derivative(Debug(format_with = "fmt_key_processor"))]
    pub key_processor: Option<KeyProcessor>,
    /// Query parameters to append to key URLs.
    pub query_params: Option<HashMap<String, String>>,
    /// Headers to include in key requests.
    pub request_headers: Option<HashMap<String, String>>,
}

fn fmt_key_processor(val: &Option<KeyProcessor>, f: &mut std::fmt::Formatter) -> std::fmt::Result {
    std::fmt::Debug::fmt(&val.as_ref().map(|_| "KeyProcessor"), f)
}

impl KeyOptions {
    /// Create default key options.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }
}

/// Configuration for HLS streaming.
///
/// Used with `Stream::<Hls>::new(config)`.
#[derive(Clone, Derivative, Setters)]
#[derivative(Default, Debug)]
#[setters(prefix = "with_", strip_option)]
pub struct HlsConfig {
    /// ABR (Adaptive Bitrate) configuration.
    pub abr: AbrOptions,
    /// Base URL for resolving relative playlist/segment URLs.
    pub base_url: Option<Url>,
    /// Cancellation token for graceful shutdown.
    pub cancel: Option<CancellationToken>,
    /// Capacity of the event bus channel (used when `bus` is not provided).
    #[derivative(Default(value = "32"))]
    pub event_channel_capacity: usize,
    /// Event bus (optional - if not provided, one is created internally).
    #[setters(rename = "with_events")]
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
    #[derivative(Default(value = "3"))]
    pub download_batch_size: usize,
    /// Optional name for cache disambiguation.
    ///
    /// When multiple URLs share the same canonical form (e.g. differ only in
    /// query parameters), setting a unique `name` ensures each gets its own
    /// cache directory.
    #[setters(skip)]
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
    /// Shared across all components. When `None`, defaults to the global rayon pool.
    pub thread_pool: Option<ThreadPool>,
    /// Master playlist URL.
    #[derivative(Default(
        value = "Url::parse(\"http://localhost/stream.m3u8\").expect(\"valid default URL\")"
    ))]
    pub url: Url,
}

impl HlsConfig {
    /// Create new HLS config with URL.
    #[must_use]
    pub fn new(url: Url) -> Self {
        Self {
            url,
            ..Self::default()
        }
    }

    /// Set name for cache disambiguation.
    pub fn with_name<S: Into<String>>(mut self, name: S) -> Self {
        self.name = Some(name.into());
        self
    }
}
