#![forbid(unsafe_code)]

//! Configuration for [`Resource`](crate::Resource).

use std::{num::NonZeroU32, path::PathBuf};

#[cfg(any(feature = "file", feature = "hls"))]
use kithara_assets::StoreOptions;
use kithara_audio::{AudioConfig, ResamplerQuality};
use kithara_bufpool::{BytePool, PcmPool};
use kithara_decode::DecodeError;
#[cfg(any(feature = "file", feature = "hls"))]
use kithara_net::NetOptions;
use kithara_stream::ThreadPool;
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

/// Unified configuration for creating a [`Resource`](crate::Resource).
///
/// Wraps source, audio options, and protocol-specific settings into a single
/// builder. `Resource::new(config)` auto-detects the stream type from the input.
///
/// # Example
///
/// ```ignore
/// use kithara::ResourceConfig;
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
///     .with_look_ahead_bytes(Some(1_000_000));
/// ```
pub struct ResourceConfig {
    /// ABR (Adaptive Bitrate) configuration.
    #[cfg(feature = "hls")]
    pub abr: kithara_abr::AbrOptions,
    /// Shared byte pool for temporary buffers (probe, etc.).
    pub byte_pool: Option<BytePool>,
    /// Cancellation token for graceful shutdown.
    pub cancel: Option<CancellationToken>,
    /// Optional format hint (file extension like "mp3", "wav").
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
    pub name: Option<String>,
    /// Network configuration (timeouts, retries).
    #[cfg(any(feature = "file", feature = "hls"))]
    pub net: NetOptions,
    /// Shared PCM pool for temporary buffers.
    pub pcm_pool: Option<PcmPool>,
    /// Number of chunks to buffer before signaling preload readiness.
    ///
    /// Higher values reduce the chance of the audio thread blocking on `recv()`
    /// after preload, but increase initial latency. Default: 3.
    pub preload_chunks: usize,
    /// Resampling quality preset.
    pub resampler_quality: ResamplerQuality,
    /// Audio resource source (URL or local path).
    pub src: ResourceSrc,
    /// Storage configuration (cache directory, eviction limits).
    #[cfg(any(feature = "file", feature = "hls"))]
    pub store: StoreOptions,
    /// Thread pool for background work (decode, probe, downloads).
    ///
    /// Shared across all components. Defaults to the global rayon pool.
    pub thread_pool: ThreadPool,
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
            byte_pool: None,
            cancel: None,
            hint: None,
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
            preload_chunks: 3,
            resampler_quality: ResamplerQuality::default(),
            src,
            #[cfg(any(feature = "file", feature = "hls"))]
            store: StoreOptions::default(),
            thread_pool: ThreadPool::default(),
        })
    }

    /// Set name for cache disambiguation.
    ///
    /// When multiple URLs share the same canonical form (differ only in query
    /// parameters), a unique name ensures each gets its own cache directory.
    pub fn with_name<N: Into<String>>(mut self, name: N) -> Self {
        self.name = Some(name.into());
        self
    }

    /// Set cancellation token.
    #[must_use]
    pub fn with_cancel(mut self, cancel: CancellationToken) -> Self {
        self.cancel = Some(cancel);
        self
    }

    /// Set format hint (file extension like "mp3", "wav").
    pub fn with_hint<H: Into<String>>(mut self, hint: H) -> Self {
        self.hint = Some(hint.into());
        self
    }

    /// Set shared byte pool for temporary buffers (probe, etc.).
    #[must_use]
    pub fn with_byte_pool(mut self, pool: BytePool) -> Self {
        self.byte_pool = Some(pool);
        self
    }

    /// Set shared PCM pool for temporary buffers.
    ///
    /// The pool is shared across the entire audio chain, eliminating
    /// per-call allocations in `read_planar` and internal decode buffers.
    #[must_use]
    pub fn with_pcm_pool(mut self, pool: PcmPool) -> Self {
        self.pcm_pool = Some(pool);
        self
    }

    /// Set target sample rate of the audio host (for resampling).
    #[must_use]
    pub fn with_host_sample_rate(mut self, sample_rate: NonZeroU32) -> Self {
        self.host_sample_rate = Some(sample_rate);
        self
    }

    /// Set resampling quality preset.
    #[must_use]
    pub fn with_resampler_quality(mut self, quality: ResamplerQuality) -> Self {
        self.resampler_quality = quality;
        self
    }

    /// Set number of chunks to buffer before signaling preload readiness.
    #[must_use]
    pub fn with_preload_chunks(mut self, chunks: usize) -> Self {
        self.preload_chunks = chunks.max(1);
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

    /// Set storage options.
    #[cfg(any(feature = "file", feature = "hls"))]
    #[must_use]
    pub fn with_store(mut self, store: StoreOptions) -> Self {
        self.store = store;
        self
    }

    /// Set network options.
    #[cfg(any(feature = "file", feature = "hls"))]
    #[must_use]
    pub fn with_net(mut self, net: NetOptions) -> Self {
        self.net = net;
        self
    }

    /// Set ABR options.
    #[cfg(feature = "hls")]
    #[must_use]
    pub fn with_abr(mut self, abr: kithara_abr::AbrOptions) -> Self {
        self.abr = abr;
        self
    }

    /// Set encryption key options.
    #[cfg(feature = "hls")]
    #[must_use]
    pub fn with_keys(mut self, keys: kithara_hls::KeyOptions) -> Self {
        self.keys = keys;
        self
    }

    /// Set base URL for resolving relative HLS URLs.
    #[cfg(feature = "hls")]
    #[must_use]
    pub fn with_hls_base_url(mut self, base_url: Url) -> Self {
        self.hls_base_url = Some(base_url);
        self
    }

    /// Set thread pool for background work (decode, probe, downloads).
    ///
    /// The pool is shared across all components. Defaults to the global rayon pool.
    #[must_use]
    pub fn with_thread_pool(mut self, pool: ThreadPool) -> Self {
        self.thread_pool = pool;
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
            .with_net(self.net)
            .with_look_ahead_bytes(self.look_ahead_bytes)
            .with_thread_pool(self.thread_pool.clone());

        if let Some(name) = self.name {
            file_config = file_config.with_name(name);
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
            .with_keys(self.keys)
            .with_look_ahead_bytes(self.look_ahead_bytes)
            .with_thread_pool(self.thread_pool.clone());

        if let Some(name) = self.name {
            hls_config = hls_config.with_name(name);
        }

        if let Some(base_url) = self.hls_base_url {
            hls_config = hls_config.with_base_url(base_url);
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
    use super::*;

    #[test]
    fn config_from_http_url() {
        let config = ResourceConfig::new("https://example.com/song.mp3").unwrap();
        assert!(matches!(&config.src, ResourceSrc::Url(url) if url.scheme() == "https"));
    }

    #[test]
    fn config_from_absolute_path() {
        let config = ResourceConfig::new("/tmp/song.mp3").unwrap();
        assert!(
            matches!(&config.src, ResourceSrc::Path(p) if p == std::path::Path::new("/tmp/song.mp3"))
        );
    }

    #[test]
    fn config_from_file_url_normalizes_to_path() {
        let config = ResourceConfig::new("file:///tmp/song.mp3").unwrap();
        assert!(
            matches!(&config.src, ResourceSrc::Path(p) if p == std::path::Path::new("/tmp/song.mp3"))
        );
    }

    #[test]
    fn config_relative_path_fails() {
        let config = ResourceConfig::new("relative/path.mp3");
        assert!(config.is_err());
    }
}
