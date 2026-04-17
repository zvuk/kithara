#![forbid(unsafe_code)]

use derivative::Derivative;
use derive_setters::Setters;
// Re-export ABR types from kithara-abr crate
pub use kithara_abr::{AbrController, AbrMode, AbrOptions, ThroughputEstimator};
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
#[derive(Clone, Default, Derivative, Setters)]
#[derivative(Debug)]
#[setters(prefix = "with_", strip_option)]
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
#[derive(Clone, Derivative, Setters)]
#[derivative(Default, Debug)]
#[setters(prefix = "with_", strip_option)]
#[non_exhaustive]
pub struct HlsConfig {
    /// Pre-created ABR controller. When `None`, `build_pair()` creates
    /// one from default `AbrOptions`.
    #[derivative(Debug = "ignore")]
    pub abr: Option<AbrController<ThroughputEstimator>>,
    /// Base URL for resolving relative playlist/segment URLs.
    pub base_url: Option<Url>,
    /// Cancellation token for graceful shutdown.
    pub cancel: Option<CancellationToken>,
    /// Capacity of the event bus channel (used when `bus` is not provided).
    #[derivative(Default(value = "kithara_events::DEFAULT_EVENT_BUS_CAPACITY"))]
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
    /// Additional HTTP headers to include in all requests.
    pub headers: Option<Headers>,
    /// Buffer pool (shared across all components, created if not provided).
    pub pool: Option<BytePool>,
    /// Storage configuration.
    pub store: StoreOptions,
    /// Master playlist URL.
    #[derivative(Default(
        value = "Url::parse(\"http://localhost/stream.m3u8\").expect(\"valid default URL\")"
    ))]
    pub url: Url,
    /// Shared downloader (created lazily if not provided).
    ///
    /// Currently unread — added ahead of the phase02 HLS migration that
    /// routes fetches through the unified downloader. Mirrors the
    /// `FileConfig::downloader` field so that once the HLS path consumes
    /// this field it can share a `Downloader` across multiple tracks.
    #[setters(skip)]
    #[derivative(Debug = "ignore")]
    pub downloader: Option<Downloader>,
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

    /// Create ABR controller from options and set it.
    #[must_use]
    pub fn with_abr_options(mut self, opts: AbrOptions) -> Self {
        self.abr = Some(AbrController::new(opts));
        self
    }

    /// Set name for cache disambiguation.
    pub fn with_name<S: Into<String>>(mut self, name: S) -> Self {
        self.name = Some(name.into());
        self
    }

    /// Set shared downloader.
    ///
    /// When not provided, the HLS path will create a private downloader
    /// during `Hls::create`. Supply a shared instance here to run
    /// multiple HLS tracks on one download pool (and to share it with
    /// `File` tracks if desired).
    ///
    /// Currently the field is not yet consumed by the HLS path — it is
    /// added ahead of the phase02 migration commits that wire the HLS
    /// fetchers through the unified downloader.
    #[must_use]
    pub fn with_downloader(mut self, dl: Downloader) -> Self {
        self.downloader = Some(dl);
        self
    }
}
