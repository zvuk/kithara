#![forbid(unsafe_code)]

//! Configuration for [`Resource`](crate::Resource).

#[cfg(any(feature = "file", feature = "hls"))]
use kithara_assets::StoreOptions;
use kithara_bufpool::SharedPool;
use kithara_decode::{DecodeError, DecodeOptions, DecoderConfig};
#[cfg(any(feature = "file", feature = "hls"))]
use kithara_net::NetOptions;
use tokio_util::sync::CancellationToken;
use url::Url;

/// Unified configuration for creating a [`Resource`](crate::Resource).
///
/// Wraps URL, decode options, and protocol-specific settings into a single
/// builder. `Resource::new(config)` auto-detects the stream type from the URL.
///
/// # Example
///
/// ```ignore
/// use kithara::ResourceConfig;
///
/// // Minimal
/// let config = ResourceConfig::new("https://example.com/song.mp3")?;
///
/// // With options
/// let config = ResourceConfig::new("https://example.com/playlist.m3u8")?
///     .with_decode(DecodeOptions::new().with_hint("mp3"))
///     .with_look_ahead_bytes(1_000_000);
/// ```
pub struct ResourceConfig {
    /// Parsed URL of the audio resource.
    pub url: Url,
    /// Cancellation token for graceful shutdown.
    pub cancel: Option<CancellationToken>,
    /// Decode-specific options (buffer sizes, format hints, etc.)
    pub decode: DecodeOptions,
    /// Max bytes the downloader may be ahead of the reader before it pauses.
    pub look_ahead_bytes: u64,
    /// Storage configuration (cache directory, eviction limits).
    #[cfg(any(feature = "file", feature = "hls"))]
    pub store: StoreOptions,
    /// Network configuration (timeouts, retries).
    #[cfg(any(feature = "file", feature = "hls"))]
    pub net: NetOptions,
    /// ABR (Adaptive Bitrate) configuration.
    #[cfg(feature = "hls")]
    pub abr: kithara_abr::AbrOptions,
    /// Encryption key handling configuration.
    #[cfg(feature = "hls")]
    pub keys: kithara_hls::KeyOptions,
    /// Base URL for resolving relative HLS playlist/segment URLs.
    #[cfg(feature = "hls")]
    pub hls_base_url: Option<Url>,
}

impl ResourceConfig {
    /// Create a new config from a URL string.
    ///
    /// Parses the URL and applies default settings for all options.
    pub fn new(url: impl AsRef<str>) -> Result<Self, DecodeError> {
        let parsed = Url::parse(url.as_ref().trim())
            .map_err(|e| DecodeError::DecodeError(format!("invalid URL: {e}")))?;

        Ok(Self {
            url: parsed,
            cancel: None,
            decode: DecodeOptions::new(),
            look_ahead_bytes: 500_000,
            #[cfg(any(feature = "file", feature = "hls"))]
            store: StoreOptions::default(),
            #[cfg(any(feature = "file", feature = "hls"))]
            net: NetOptions::default(),
            #[cfg(feature = "hls")]
            abr: kithara_abr::AbrOptions::default(),
            #[cfg(feature = "hls")]
            keys: kithara_hls::KeyOptions::default(),
            #[cfg(feature = "hls")]
            hls_base_url: None,
        })
    }

    /// Set cancellation token.
    pub fn with_cancel(mut self, cancel: CancellationToken) -> Self {
        self.cancel = Some(cancel);
        self
    }

    /// Set decode options.
    pub fn with_decode(mut self, decode: DecodeOptions) -> Self {
        self.decode = decode;
        self
    }

    /// Set format hint (file extension like "mp3", "wav").
    pub fn with_hint(mut self, hint: impl Into<String>) -> Self {
        self.decode = self.decode.with_hint(hint);
        self
    }

    /// Set shared PCM pool for temporary buffers.
    ///
    /// The pool is shared across the entire decode chain, eliminating
    /// per-call allocations in `read_planar` and internal decode buffers.
    pub fn with_pcm_pool(mut self, pool: SharedPool<32, Vec<f32>>) -> Self {
        self.decode = self.decode.with_pcm_pool(pool);
        self
    }

    /// Set max bytes the downloader may be ahead of the reader before it pauses.
    pub fn with_look_ahead_bytes(mut self, bytes: u64) -> Self {
        self.look_ahead_bytes = bytes;
        self
    }

    /// Set storage options.
    #[cfg(any(feature = "file", feature = "hls"))]
    pub fn with_store(mut self, store: StoreOptions) -> Self {
        self.store = store;
        self
    }

    /// Set network options.
    #[cfg(any(feature = "file", feature = "hls"))]
    pub fn with_net(mut self, net: NetOptions) -> Self {
        self.net = net;
        self
    }

    /// Set ABR options.
    #[cfg(feature = "hls")]
    pub fn with_abr(mut self, abr: kithara_abr::AbrOptions) -> Self {
        self.abr = abr;
        self
    }

    /// Set encryption key options.
    #[cfg(feature = "hls")]
    pub fn with_keys(mut self, keys: kithara_hls::KeyOptions) -> Self {
        self.keys = keys;
        self
    }

    /// Set base URL for resolving relative HLS URLs.
    #[cfg(feature = "hls")]
    pub fn with_hls_base_url(mut self, base_url: Url) -> Self {
        self.hls_base_url = Some(base_url);
        self
    }

    // -- Internal conversions -------------------------------------------------

    /// Convert into a `DecoderConfig<File>`.
    #[cfg(feature = "file")]
    pub(crate) fn into_file_config(self) -> DecoderConfig<kithara_file::File> {
        let hint = self
            .url
            .path()
            .rsplit('.')
            .next()
            .map(|ext| ext.to_lowercase());

        let mut file_config = kithara_file::FileConfig::new(self.url)
            .with_store(self.store)
            .with_net(self.net)
            .with_look_ahead_bytes(self.look_ahead_bytes);

        if let Some(cancel) = self.cancel {
            file_config = file_config.with_cancel(cancel);
        }

        let mut config =
            DecoderConfig::<kithara_file::File>::new(file_config).with_decode(self.decode);

        if let Some(ext) = hint
            && config.decode.hint.is_none()
        {
            config = config.with_hint(ext);
        }

        config
    }

    /// Convert into a `DecoderConfig<Hls>`.
    #[cfg(feature = "hls")]
    pub(crate) fn into_hls_config(self) -> DecoderConfig<kithara_hls::Hls> {
        let mut hls_config = kithara_hls::HlsConfig::new(self.url)
            .with_store(self.store)
            .with_net(self.net)
            .with_abr(self.abr)
            .with_keys(self.keys)
            .with_look_ahead_bytes(self.look_ahead_bytes);

        if let Some(base_url) = self.hls_base_url {
            hls_config = hls_config.with_base_url(base_url);
        }
        if let Some(cancel) = self.cancel {
            hls_config = hls_config.with_cancel(cancel);
        }

        DecoderConfig::<kithara_hls::Hls>::new(hls_config).with_decode(self.decode)
    }
}
