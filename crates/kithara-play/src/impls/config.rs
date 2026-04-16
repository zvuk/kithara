//! Configuration for [`Item`](crate::impls::item::Item).

use std::{
    fmt,
    num::{NonZeroU32, NonZeroUsize},
    path::PathBuf,
    sync::Arc,
};

use derive_setters::Setters;
#[cfg(feature = "hls")]
use kithara_abr::{AbrController, AbrOptions, ThroughputEstimator};
#[cfg(any(feature = "file", feature = "hls"))]
use kithara_assets::StoreOptions;
use kithara_audio::{AudioConfig, AudioWorkerHandle, ResamplerQuality};
use kithara_bufpool::{BytePool, PcmPool};
use kithara_decode::DecodeError;
use kithara_events::EventBus;
#[cfg(feature = "file")]
use kithara_file::{FileConfig, FileSrc};
#[cfg(feature = "hls")]
use kithara_hls::{HlsConfig, KeyOptions};
#[cfg(any(feature = "file", feature = "hls"))]
use kithara_net::Headers;
#[cfg(any(feature = "file", feature = "hls"))]
use kithara_stream::dl::{Downloader, DownloaderConfig};
use portable_atomic::AtomicF32;
use tokio_util::sync::CancellationToken;
use url::Url;

#[cfg(feature = "file")]
fn derive_remote_file_hint(url: &Url) -> Option<String> {
    url.path_segments()
        .and_then(|mut segments| segments.next_back())
        .and_then(derive_extension_hint)
}

#[cfg(feature = "file")]
fn derive_extension_hint(segment: &str) -> Option<String> {
    let (_, extension) = segment.rsplit_once('.')?;
    if extension.is_empty() || !extension.chars().all(|ch| ch.is_ascii_alphanumeric()) {
        return None;
    }
    Some(extension.to_lowercase())
}

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

impl fmt::Display for ResourceSrc {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
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
#[derive(Clone, Setters)]
#[setters(prefix = "with_", strip_option)]
pub struct ResourceConfig {
    /// ABR controller (shared across tracks in a player).
    #[cfg(feature = "hls")]
    pub abr: Option<AbrController<ThroughputEstimator>>,
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
    pub keys: KeyOptions,
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
    /// Shared PCM pool for temporary buffers.
    pub pcm_pool: Option<PcmPool>,
    /// Shared playback rate atomic for the audio pipeline resampler.
    ///
    /// When set, propagated to `AudioConfig` so the resampler can dynamically
    /// adjust its ratio based on the current playback rate.
    pub playback_rate: Option<Arc<AtomicF32>>,
    /// Maximum peak bitrate in bits per second for ABR variant selection.
    ///
    /// When greater than zero, variants with bandwidth exceeding this value
    /// are excluded from ABR decisions. Maps to `AVPlayer`'s
    /// `preferredPeakBitRate`. Default: `0.0` (no limit).
    pub preferred_peak_bitrate: f64,
    /// Maximum peak bitrate for expensive networks (e.g., cellular).
    ///
    /// Stored for use by the FFI layer which determines network type.
    /// Not consumed by `into_hls_config()` directly. Default: `0.0` (no limit).
    pub preferred_peak_bitrate_for_expensive_networks: f64,
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
    /// Shared downloader instance.
    ///
    /// When set, the underlying `FileConfig` / `HlsConfig` reuses this
    /// downloader instead of spawning a private one. Lets multiple
    /// resources share a single HTTP pool and runtime handle.
    #[cfg(any(feature = "file", feature = "hls"))]
    #[setters(skip)]
    pub downloader: Option<Downloader>,
    /// Shared audio worker handle for cooperative multi-track decoding.
    ///
    /// When set, all resources sharing the same worker decode on a single
    /// OS thread. When `None`, each `Audio` pipeline creates its own
    /// standalone worker thread (backward-compatible default).
    #[setters(skip)]
    pub worker: Option<AudioWorkerHandle>,
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
            #[cfg(not(target_arch = "wasm32"))]
            Ok(url) if url.scheme() == "file" => {
                let path = url.to_file_path().map_err(|()| {
                    DecodeError::InvalidData(format!("invalid file URL: {trimmed}"))
                })?;
                ResourceSrc::Path(path)
            }
            #[cfg(target_arch = "wasm32")]
            Ok(url) if url.scheme() == "file" => {
                return Err(DecodeError::InvalidData(format!(
                    "file:// URL is not supported on wasm: {trimmed}"
                )));
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
            abr: None,
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
            keys: KeyOptions::default(),
            look_ahead_bytes: None,
            name: None,
            pcm_pool: None,
            playback_rate: None,
            preferred_peak_bitrate: 0.0,
            preferred_peak_bitrate_for_expensive_networks: 0.0,
            preload_chunks: DEFAULT_PRELOAD_CHUNKS,
            resampler_quality: ResamplerQuality::default(),
            src,
            #[cfg(any(feature = "file", feature = "hls"))]
            store: StoreOptions::default(),
            #[cfg(any(feature = "file", feature = "hls"))]
            downloader: None,
            worker: None,
        })
    }

    /// Set a shared downloader for the underlying stream.
    #[cfg(any(feature = "file", feature = "hls"))]
    #[must_use]
    pub fn with_downloader(mut self, dl: Downloader) -> Self {
        self.downloader = Some(dl);
        self
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

    /// Set shared audio worker for cooperative multi-track decoding.
    #[must_use]
    pub fn with_worker(mut self, worker: AudioWorkerHandle) -> Self {
        self.worker = Some(worker);
        self
    }

    /// Convert into an `AudioConfig<File>`.
    #[cfg(feature = "file")]
    pub(crate) fn into_file_config(self) -> AudioConfig<kithara_file::File> {
        let (file_src, hint) = match self.src {
            ResourceSrc::Url(url) => {
                let h = derive_remote_file_hint(&url);
                (FileSrc::Remote(url), h)
            }
            ResourceSrc::Path(path) => {
                let h = path
                    .extension()
                    .and_then(|e| e.to_str())
                    .map(str::to_lowercase);
                (FileSrc::Local(path), h)
            }
        };

        let dl = self
            .downloader
            .unwrap_or_else(|| Downloader::new(DownloaderConfig::default()));
        let mut file_config = FileConfig::new(file_src)
            .with_store(self.store)
            .with_downloader(dl);

        if let Some(bytes) = self.look_ahead_bytes {
            file_config = file_config.with_look_ahead_bytes(bytes);
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
        if let Some(rate) = self.playback_rate {
            config = config.with_playback_rate(rate);
        }
        if let Some(worker) = self.worker {
            config = config.with_worker(worker);
        }

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

        let mut hls_config = HlsConfig::new(url)
            .with_store(self.store)
            .with_keys(self.keys);
        if let Some(dl) = self.downloader {
            hls_config = hls_config.with_downloader(dl);
        }

        hls_config.abr = self.abr;

        if self.preferred_peak_bitrate.is_finite() && self.preferred_peak_bitrate >= 1.0 {
            #[expect(clippy::cast_sign_loss)]
            #[expect(clippy::cast_possible_truncation)]
            let cap = self.preferred_peak_bitrate as u64;
            let ctrl = hls_config
                .abr
                .get_or_insert_with(|| AbrController::new(AbrOptions::default()));
            ctrl.set_max_bandwidth_bps(Some(cap));
        }

        if let Some(bytes) = self.look_ahead_bytes {
            hls_config = hls_config.with_look_ahead_bytes(bytes);
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
        if let Some(rate) = self.playback_rate {
            config = config.with_playback_rate(rate);
        }
        if let Some(worker) = self.worker {
            config = config.with_worker(worker);
        }

        Ok(config)
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use kithara_test_utils::kithara;

    use super::*;

    #[kithara::test]
    fn config_source_parsing_url() {
        let config = ResourceConfig::new("https://example.com/song.mp3").unwrap();
        assert!(matches!(&config.src, ResourceSrc::Url(url) if url.scheme() == "https"));
    }

    #[cfg(feature = "file")]
    #[kithara::test]
    fn config_file_url_derives_extension_hint_from_last_path_segment() {
        let config = ResourceConfig::new("https://example.com/audio/get-mp3/song.MP3?sign=test")
            .unwrap()
            .into_file_config();

        assert_eq!(config.hint.as_deref(), Some("mp3"));
    }

    #[cfg(feature = "file")]
    #[kithara::test]
    fn config_file_url_without_extension_does_not_derive_hint() {
        let config = ResourceConfig::new("https://example.com/get-mp3/42?sign=test")
            .unwrap()
            .into_file_config();

        assert_eq!(config.hint, None);
    }

    #[kithara::test(native)]
    #[case("/tmp/song.mp3", "/tmp/song.mp3")]
    #[case("file:///tmp/song.mp3", "/tmp/song.mp3")]
    fn config_source_parsing_file_path(#[case] input: &str, #[case] expected: &str) {
        let config = ResourceConfig::new(input).unwrap();
        assert!(matches!(
            &config.src,
            ResourceSrc::Path(path) if path == Path::new(expected)
        ));
    }

    #[kithara::test]
    #[case("relative/path.mp3")]
    fn config_source_parsing_error(#[case] input: &str) {
        assert!(ResourceConfig::new(input).is_err());
    }

    #[kithara::test]
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
    #[kithara::test]
    fn config_bus_propagates_to_file_config() {
        let bus = EventBus::new(32);
        let config = ResourceConfig::new("https://example.com/song.mp3")
            .unwrap()
            .with_events(bus);
        let audio_config = config.into_file_config();
        assert!(audio_config.stream.bus.is_some());
    }

    #[cfg(feature = "hls")]
    #[kithara::test]
    fn config_bus_propagates_to_hls_config() {
        let bus = EventBus::new(32);
        let config = ResourceConfig::new("https://example.com/live.m3u8")
            .unwrap()
            .with_events(bus);
        let audio_config = config.into_hls_config().unwrap();
        assert!(audio_config.stream.bus.is_some());
    }

    #[cfg(any(feature = "file", feature = "hls"))]
    #[kithara::test]
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

    #[kithara::test]
    fn config_builder_chain() {
        let bus = EventBus::new(32);
        let config = ResourceConfig::new("https://example.com/song.mp3")
            .unwrap()
            .with_events(bus)
            .with_hint("mp3")
            .with_name("test")
            .with_preload_chunks(NonZeroUsize::new(5).expect("5 > 0"));
        assert!(config.bus.is_some());
        assert_eq!(config.hint.as_deref(), Some("mp3"));
        assert_eq!(config.name.as_deref(), Some("test"));
        assert_eq!(config.preload_chunks.get(), 5);
    }

    #[kithara::test]
    fn config_bitrate_fields_default_zero() {
        let config = ResourceConfig::new("https://example.com/live.m3u8").unwrap();
        assert!((config.preferred_peak_bitrate - 0.0).abs() < f64::EPSILON);
        assert!((config.preferred_peak_bitrate_for_expensive_networks - 0.0).abs() < f64::EPSILON);
    }

    #[cfg(feature = "hls")]
    #[kithara::test]
    fn config_bitrate_propagates_to_hls_abr() {
        let config = ResourceConfig::new("https://example.com/live.m3u8")
            .unwrap()
            .with_preferred_peak_bitrate(512_000.0);
        let audio_config = config.into_hls_config().unwrap();
        let abr = audio_config
            .stream
            .abr
            .as_ref()
            .expect("controller should be set");
        assert_eq!(abr.max_bandwidth_bps(), Some(512_000));
    }

    #[kithara::test]
    fn config_worker_default_none() {
        let config = ResourceConfig::new("https://example.com/song.mp3").unwrap();
        assert!(config.worker.is_none());
    }

    #[kithara::test]
    fn config_with_worker_sets_field() {
        let worker = AudioWorkerHandle::new();
        let config = ResourceConfig::new("https://example.com/song.mp3")
            .unwrap()
            .with_worker(worker.clone());
        assert!(config.worker.is_some());
        worker.shutdown();
    }

    #[cfg(feature = "file")]
    #[kithara::test]
    fn config_worker_propagates_to_file_config() {
        let worker = AudioWorkerHandle::new();
        let config = ResourceConfig::new("https://example.com/song.mp3")
            .unwrap()
            .with_worker(worker.clone());
        let audio_config = config.into_file_config();
        assert!(audio_config.worker.is_some());
        worker.shutdown();
    }

    #[cfg(feature = "hls")]
    #[kithara::test]
    fn config_worker_propagates_to_hls_config() {
        let worker = AudioWorkerHandle::new();
        let config = ResourceConfig::new("https://example.com/live.m3u8")
            .unwrap()
            .with_worker(worker.clone());
        let audio_config = config.into_hls_config().unwrap();
        assert!(audio_config.worker.is_some());
        worker.shutdown();
    }

    #[cfg(feature = "file")]
    #[kithara::test]
    fn file_hint_none_for_url_without_extension() {
        // URL path has no file extension — hint must be None, not garbage.
        let config =
            ResourceConfig::new("https://cdn-edge.zvq.me/track/streamhq?id=125475417").unwrap();
        let audio_config = config.into_file_config();
        assert_eq!(
            audio_config.hint, None,
            "URL without file extension must produce hint=None"
        );
    }

    #[cfg(feature = "file")]
    #[kithara::test]
    #[case("https://example.com/song.mp3", Some("mp3"))]
    #[case("https://example.com/audio.flac", Some("flac"))]
    #[case("https://example.com/track/stream", None)]
    #[case("https://example.com/track/streamhq?id=123", None)]
    #[case("https://example.com/audio", None)]
    fn file_hint_from_url_extension(#[case] url: &str, #[case] expected: Option<&str>) {
        let config = ResourceConfig::new(url).unwrap();
        let audio_config = config.into_file_config();
        assert_eq!(
            audio_config.hint.as_deref(),
            expected,
            "hint mismatch for URL: {url}"
        );
    }
}
