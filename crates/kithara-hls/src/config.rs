#![forbid(unsafe_code)]

use std::fmt;

use bon::Builder;
pub use kithara_abr::AbrMode;
use kithara_assets::{BytePool, StoreOptions};
use kithara_drm::KeyProcessorRegistry;
use kithara_events::EventBus;
use kithara_net::Headers;
use kithara_stream::dl::Downloader;
use tokio_util::sync::CancellationToken;
use url::Url;

/// Encryption key handling configuration.
///
/// DRM key processing is routed through [`KeyProcessorRegistry`] so
/// different providers (zvuk, custom in-house DRM, etc.) can coexist
/// with different processors, headers, and query params — all scoped
/// by URL domain.
#[derive(Clone, Debug, Default, Builder)]
#[builder(state_mod(vis = "pub"))]
#[non_exhaustive]
pub struct KeyOptions {
    /// Domain-scoped processor registry. Key URLs whose host matches
    /// a rule get that rule's processor + headers + query params;
    /// unmatched URLs use the raw key as-is.
    pub key_registry: Option<KeyProcessorRegistry>,
}

impl KeyOptions {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }
}

/// Configuration for HLS streaming.
///
/// Used with `Stream::<Hls>::new(config)`.
#[derive(Clone, Builder)]
#[builder(state_mod(vis = "pub"))]
#[non_exhaustive]
pub struct HlsConfig {
    /// Initial ABR mode.
    #[builder(default)]
    pub initial_abr_mode: AbrMode,
    /// Encryption key handling configuration.
    #[builder(default)]
    pub keys: KeyOptions,
    /// Base URL for resolving relative playlist/segment URLs.
    pub base_url: Option<Url>,
    /// Event bus (optional - if not provided, one is created internally).
    #[builder(name = events)]
    pub bus: Option<EventBus>,
    /// Cancellation token for graceful shutdown.
    pub cancel: Option<CancellationToken>,
    /// Shared downloader (created lazily if not provided).
    pub downloader: Option<Downloader>,
    /// Additional HTTP headers to include in all requests.
    pub headers: Option<Headers>,
    /// Max bytes the downloader may be ahead of the reader before it pauses.
    /// `None` falls back to [`HlsConfig::DEFAULT_LOOK_AHEAD_BYTES`] (~2 `MiB`)
    /// at the consumer site — production HLS streams need a downloader
    /// backpressure cap. Pass `Some(0)` to disable the cap explicitly.
    pub look_ahead_bytes: Option<u64>,
    /// Optional name for cache disambiguation.
    pub name: Option<String>,
    /// Buffer pool (shared across all components, created if not provided).
    pub pool: Option<BytePool>,
    /// Storage configuration.
    #[builder(default)]
    pub store: StoreOptions,
    /// Master playlist URL.
    pub url: Url,
    /// Max segments to download per step.
    #[builder(default = 3)]
    pub download_batch_size: usize,
    /// Capacity of the event bus channel (used when `bus` is not provided).
    #[builder(default = kithara_events::DEFAULT_EVENT_BUS_CAPACITY)]
    pub event_channel_capacity: usize,
}

impl fmt::Debug for HlsConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HlsConfig")
            .field("initial_abr_mode", &self.initial_abr_mode)
            .field("keys", &self.keys)
            .field("base_url", &self.base_url)
            .field("bus", &self.bus)
            .field("cancel", &self.cancel)
            .field("headers", &self.headers)
            .field("look_ahead_bytes", &self.look_ahead_bytes)
            .field("name", &self.name)
            .field("pool", &self.pool)
            .field("store", &self.store)
            .field("url", &self.url)
            .field("download_batch_size", &self.download_batch_size)
            .field("event_channel_capacity", &self.event_channel_capacity)
            .finish_non_exhaustive()
    }
}

impl Default for HlsConfig {
    fn default() -> Self {
        let url = Url::parse("http://localhost/stream.m3u8").expect("valid default URL");
        Self::new(url)
    }
}

impl HlsConfig {
    /// Default `look_ahead_bytes` cap (~2 `MiB`). Production HLS streams
    /// need a downloader backpressure cap so an idle reader does not
    /// drain the whole playlist into cache.
    pub const DEFAULT_LOOK_AHEAD_BYTES: u64 = 2 * 1024 * 1024;

    /// Create new HLS config with URL.
    #[must_use]
    pub fn new(url: Url) -> Self {
        Self::for_url(url).build()
    }

    /// Chainable counterpart to [`HlsConfig::new`].
    pub fn for_url(url: Url) -> HlsConfigBuilder<hls_config_builder::SetUrl> {
        Self::builder().url(url)
    }
}
