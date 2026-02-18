//! Configuration for [`Item`](crate::impls::item::Item).

use std::{
    num::{NonZeroU32, NonZeroUsize},
    path::PathBuf,
};

use derive_setters::Setters;
#[cfg(any(feature = "file", feature = "hls"))]
use kithara_assets::StoreOptions;
use kithara_audio::{AudioConfig, ResamplerQuality};
use kithara_bufpool::{BytePool, PcmPool};
use kithara_decode::DecodeError;
use kithara_events::EventBus;
#[cfg(any(feature = "file", feature = "hls"))]
use kithara_net::{Headers, NetOptions};
use kithara_platform::ThreadPool;
use tokio_util::sync::CancellationToken;
use url::Url;

/// Source of an audio resource: either a URL or a local file path.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ResourceSrc {
    /// Remote resource accessed via URL (HTTP/HTTPS, or other schemes).
    Url(Url),
    /// Local file accessed directly from disk.
    Path(PathBuf),
}

impl From<Url> for ResourceSrc {
    fn from(url: Url) -> Self {
        Self::Url(url)
    }
}

impl From<PathBuf> for ResourceSrc {
    fn from(path: PathBuf) -> Self {
        Self::Path(path)
    }
}

impl std::fmt::Display for ResourceSrc {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Url(url) => write!(f, "{url}"),
            Self::Path(path) => write!(f, "{}", path.display()),
        }
    }
}

/// Default number of preload chunks.
const DEFAULT_PRELOAD_CHUNKS: NonZeroUsize = NonZeroUsize::new(3).unwrap();

/// Unified configuration for creating an [`Item`](crate::impls::item::Item).
///
/// Wraps source, audio options, and protocol-specific settings into a single
/// builder. `Item::new(config)` auto-detects the stream type from the input.
///
/// # Example
///
/// ```ignore
/// use kithara_play::ResourceConfig;
///
/// // From URL
/// let config = ResourceConfig::new("https://example.com/song.mp3")?;
///
/// // From local path
/// let config = ResourceConfig::new("/path/to/song.mp3")?;
///
/// // With options
/// let config = ResourceConfig::new("https://example.com/playlist.m3u8")?
///     .with_hint("mp3")
///     .with_look_ahead_bytes(1_000_000);
/// ```
#[derive(Setters)]
#[setters(prefix = "with_", strip_option)]
pub struct ResourceConfig {
    /// ABR (Adaptive Bitrate) configuration.
    #[cfg(feature = "hls")]
    pub abr: kithara_abr::AbrOptions,
    /// Unified event bus for streaming, decode, and audio events.
    ///
    /// When set, the bus is propagated to the underlying stream and audio
    /// pipeline. All events (`FileEvent`, `HlsEvent`, `AudioEvent`) are
    /// published to this bus, enabling a single subscription point.
    /// When `None`, a fresh bus is created per resource.
    #[setters(rename = "with_events")]
    pub bus: Option<EventBus>,
    /// Shared byte pool for temporary buffers (probe, etc.).
    pub byte_pool: Option<BytePool>,
    /// Cancellation token for graceful shutdown.
    pub cancel: Option<CancellationToken>,
    /// Optional format hint (file extension like "mp3", "wav").
    #[setters(skip)]
    pub hint: Option<String>,
    /// Base URL for resolving relative HLS playlist/segment URLs.
    #[cfg(feature = "hls")]
    pub hls_base_url: Option<Url>,
    /// Target sample rate of the audio host (for resampling).
    pub host_sample_rate: Option<NonZeroU32>,
    /// Encryption key handling configuration.
    #[cfg(feature = "hls")]
    pub keys: kithara_hls::KeyOptions,
    /// Max bytes the downloader may be ahead of the reader before it pauses.
    ///
    /// - `Some(n)` — pause when downloaded - read > n bytes (backpressure)
    /// - `None` — no backpressure, download as fast as possible
    pub look_ahead_bytes: Option<u64>,
    /// Optional name for cache disambiguation.
    ///
    /// When multiple URLs share the same canonical form (e.g. differ only in
    /// query parameters), setting a unique `name` ensures each gets its own
    /// cache directory.
    #[setters(skip)]
    pub name: Option<String>,
    /// Additional HTTP headers to include in all network requests.
    #[cfg(any(feature = "file", feature = "hls"))]
    pub headers: Option<Headers>,
    /// Network configuration (timeouts, retries).
    #[cfg(any(feature = "file", feature = "hls"))]
    pub net: NetOptions,
    /// Shared PCM pool for temporary buffers.
    pub pcm_pool: Option<PcmPool>,
    /// Number of chunks to buffer before signaling preload readiness.
    ///
    /// Higher values reduce the chance of the audio thread blocking on `recv()`
    /// after preload, but increase initial latency. Default: 3.
    pub preload_chunks: NonZeroUsize,
    /// Resampling quality preset.
    pub resampler_quality: ResamplerQuality,
    /// Audio resource source (URL or local path).
    pub src: ResourceSrc,
    /// Storage configuration (cache directory, eviction limits).
    #[cfg(any(feature = "file", feature = "hls"))]
    pub store: StoreOptions,
    /// Thread pool for background work (decode, probe, downloads).
    ///
    /// Shared across all components. When `None`, defaults to the global rayon pool.
    pub thread_pool: Option<ThreadPool>,
}

impl ResourceConfig {
    /// Create a new config from a URL string or local file path.
    ///
    /// Parses the input as a URL first. If parsing fails, treats it as a local
    /// file path (must be absolute). A `file://` URL is normalized to a `Path`.
    ///
    /// # Errors
    ///
    /// Returns `DecodeError::InvalidData` if the input is an invalid `file://` URL
    /// or a non-absolute file path.
    pub fn new<S: AsRef<str>>(input: S) -> Result<Self, DecodeError> {
        let trimmed = input.as_ref().trim();

        let src = match Url::parse(trimmed) {
            Ok(url) if url.scheme() == "file" => {
                let path = url.to_file_path().map_err(|()| {
                    DecodeError::InvalidData(format!("invalid file URL: {trimmed}"))
                })?;
                ResourceSrc::Path(path)
            }
            Ok(url) => ResourceSrc::Url(url),
            Err(_) => {
                let path = PathBuf::from(trimmed);
                if !path.is_absolute() {
                    return Err(DecodeError::InvalidData(format!(
                        "invalid URL or file path (must be absolute): {trimmed}"
                    )));
                }
                ResourceSrc::Path(path)
            }
        };

        Ok(Self {
            #[cfg(feature = "hls")]
            abr: kithara_abr::AbrOptions::default(),
            bus: None,
            byte_pool: None,
            cancel: None,
            hint: None,
            #[cfg(any(feature = "file", feature = "hls"))]
            headers: None,
            #[cfg(feature = "hls")]
            hls_base_url: None,
            host_sample_rate: None,
            #[cfg(feature = "hls")]
            keys: kithara_hls::KeyOptions::default(),
            look_ahead_bytes: None,
            name: None,
            #[cfg(any(feature = "file", feature = "hls"))]
            net: NetOptions::default(),
            pcm_pool: None,
            preload_chunks: DEFAULT_PRELOAD_CHUNKS,
            resampler_quality: ResamplerQuality::default(),
            src,
            #[cfg(any(feature = "file", feature = "hls"))]
            store: StoreOptions::default(),
            thread_pool: None,
        })
    }

    /// Set name for cache disambiguation.
    #[must_use]
    pub fn with_name<N: Into<String>>(mut self, name: N) -> Self {
        self.name = Some(name.into());
        self
    }

    /// Set format hint (file extension like "mp3", "wav").
    #[must_use]
    pub fn with_hint<H: Into<String>>(mut self, hint: H) -> Self {
        self.hint = Some(hint.into());
        self
    }

    // -- Internal conversions -------------------------------------------------

    /// Convert into an `AudioConfig<File>`.
    #[cfg(feature = "file")]
    pub(crate) fn into_file_config(self) -> AudioConfig<kithara_file::File> {
        let (file_src, hint) = match self.src {
            ResourceSrc::Url(url) => {
                let h = url.path().rsplit('.').next().map(str::to_lowercase);
                (kithara_file::FileSrc::Remote(url), h)
            }
            ResourceSrc::Path(path) => {
                let h = path
                    .extension()
                    .and_then(|e| e.to_str())
                    .map(str::to_lowercase);
                (kithara_file::FileSrc::Local(path), h)
            }
        };

        let mut file_config = kithara_file::FileConfig::new(file_src)
            .with_store(self.store)
            .with_net(self.net);

        if let Some(bytes) = self.look_ahead_bytes {
            file_config = file_config.with_look_ahead_bytes(bytes);
        }

        if let Some(pool) = self.thread_pool.clone() {
            file_config = file_config.with_thread_pool(pool);
        }

        if let Some(headers) = self.headers {
            file_config = file_config.with_headers(headers);
        }

        if let Some(name) = self.name {
            file_config = file_config.with_name(name);
        }

        if let Some(ref bus) = self.bus {
            file_config = file_config.with_events(bus.clone());
        }
        if let Some(cancel) = self.cancel {
            file_config = file_config.with_cancel(cancel);
        }

        let mut config = AudioConfig::<kithara_file::File>::new(file_config);

        // Apply audio settings from ResourceConfig.
        if let Some(h) = self.hint {
            config = config.with_hint(h);
        } else if let Some(ext) = hint {
            config = config.with_hint(ext);
        }
        if let Some(pool) = self.byte_pool {
            config = config.with_byte_pool(pool);
        }
        if let Some(pool) = self.pcm_pool {
            config = config.with_pcm_pool(pool);
        }
        if let Some(sr) = self.host_sample_rate {
            config = config.with_host_sample_rate(sr);
        }
        config = config.with_resampler_quality(self.resampler_quality);
        config = config.with_preload_chunks(self.preload_chunks);

        config
    }

    /// Convert into an `AudioConfig<Hls>`.
    #[cfg(feature = "hls")]
    pub(crate) fn into_hls_config(self) -> Result<AudioConfig<kithara_hls::Hls>, DecodeError> {
        let url = match self.src {
            ResourceSrc::Url(url) => url,
            ResourceSrc::Path(p) => {
                return Err(DecodeError::InvalidData(format!(
                    "HLS requires a URL, got local path: {}",
                    p.display()
                )));
            }
        };

        let mut hls_config = kithara_hls::HlsConfig::new(url)
            .with_store(self.store)
            .with_net(self.net)
            .with_abr(self.abr)
            .with_keys(self.keys);

        if let Some(bytes) = self.look_ahead_bytes {
            hls_config = hls_config.with_look_ahead_bytes(bytes);
        }

        if let Some(pool) = self.thread_pool.clone() {
            hls_config = hls_config.with_thread_pool(pool);
        }

        if let Some(headers) = self.headers {
            hls_config = hls_config.with_headers(headers);
        }

        if let Some(name) = self.name {
            hls_config = hls_config.with_name(name);
        }

        if let Some(base_url) = self.hls_base_url {
            hls_config = hls_config.with_base_url(base_url);
        }
        if let Some(ref bus) = self.bus {
            hls_config = hls_config.with_events(bus.clone());
        }
        if let Some(cancel) = self.cancel {
            hls_config = hls_config.with_cancel(cancel);
        }

        let mut config = AudioConfig::<kithara_hls::Hls>::new(hls_config);

        // Apply audio settings from ResourceConfig.
        if let Some(h) = self.hint {
            config = config.with_hint(h);
        }
        if let Some(pool) = self.byte_pool {
            config = config.with_byte_pool(pool);
        }
        if let Some(pool) = self.pcm_pool {
            config = config.with_pcm_pool(pool);
        }
        if let Some(sr) = self.host_sample_rate {
            config = config.with_host_sample_rate(sr);
        }
        config = config.with_resampler_quality(self.resampler_quality);
        config = config.with_preload_chunks(self.preload_chunks);

        Ok(config)
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use rstest::rstest;

    use super::*;

    #[rstest]
    #[case("https://example.com/song.mp3", true, "https")]
    #[case("/tmp/song.mp3", false, "/tmp/song.mp3")]
    #[case("file:///tmp/song.mp3", false, "/tmp/song.mp3")]
    fn config_source_parsing_success(
        #[case] input: &str,
        #[case] expect_url: bool,
        #[case] expected: &str,
    ) {
        let config = ResourceConfig::new(input).unwrap();
        if expect_url {
            assert!(matches!(&config.src, ResourceSrc::Url(url) if url.scheme() == expected));
        } else {
            assert!(matches!(&config.src, ResourceSrc::Path(path) if path == Path::new(expected)));
        }
    }

    #[rstest]
    #[case("relative/path.mp3")]
    fn config_source_parsing_error(#[case] input: &str) {
        assert!(ResourceConfig::new(input).is_err());
    }

    #[rstest]
    #[case(false)]
    #[case(true)]
    fn config_bus_presence(#[case] with_events: bool) {
        let mut config = ResourceConfig::new("https://example.com/song.mp3").unwrap();
        if with_events {
            config = config.with_events(EventBus::new(32));
        }
        assert_eq!(config.bus.is_some(), with_events);
    }

    #[cfg(feature = "file")]
    #[test]
    fn config_bus_propagates_to_file_config() {
        let bus = EventBus::new(32);
        let config = ResourceConfig::new("https://example.com/song.mp3")
            .unwrap()
            .with_events(bus);
        let audio_config = config.into_file_config();
        assert!(audio_config.stream.bus.is_some());
    }

    #[cfg(feature = "hls")]
    #[test]
    fn config_bus_propagates_to_hls_config() {
        let bus = EventBus::new(32);
        let config = ResourceConfig::new("https://example.com/live.m3u8")
            .unwrap()
            .with_events(bus);
        let audio_config = config.into_hls_config().unwrap();
        assert!(audio_config.stream.bus.is_some());
    }

    #[cfg(any(feature = "file", feature = "hls"))]
    #[test]
    fn config_with_headers() {
        let mut headers = Headers::new();
        headers.insert("Authorization", "Bearer test");
        let config = ResourceConfig::new("https://example.com/song.mp3")
            .unwrap()
            .with_headers(headers);

        assert!(config.headers.is_some());
        assert_eq!(
            config.headers.as_ref().and_then(|h| h.get("Authorization")),
            Some("Bearer test")
        );
    }

    #[test]
    fn config_builder_chain() {
        let bus = EventBus::new(32);
        let config = ResourceConfig::new("https://example.com/song.mp3")
            .unwrap()
            .with_events(bus)
            .with_hint("mp3")
            .with_name("test")
            .with_thread_pool(ThreadPool::default())
            .with_preload_chunks(NonZeroUsize::new(5).expect("5 > 0"));
        assert!(config.bus.is_some());
        assert_eq!(config.hint.as_deref(), Some("mp3"));
        assert_eq!(config.name.as_deref(), Some("test"));
        assert_eq!(config.preload_chunks.get(), 5);
    }
}
