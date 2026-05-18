use std::{
    fmt,
    num::{NonZeroU32, NonZeroUsize},
    path::PathBuf,
    sync::Arc,
};

use bon::Builder;
use kithara_abr::AbrMode;
use kithara_assets::{FlushHub, StoreOptions};
use kithara_audio::{AudioConfig, AudioWorkerHandle, ResamplerQuality};
use kithara_bufpool::{BytePool, PcmPool};
use kithara_decode::{DecodeError, DecoderBackend};
use kithara_events::EventBus;
use kithara_file::{FileConfig, FileSrc};
use kithara_hls::{HlsConfig, KeyOptions};
use kithara_net::Headers;
use kithara_stream::dl::{Downloader, DownloaderConfig};
use portable_atomic::AtomicF32;
use tokio_util::sync::CancellationToken;
use url::Url;

fn derive_remote_file_hint(url: &Url) -> Option<String> {
    url.path_segments()
        .and_then(|mut segments| segments.next_back())
        .and_then(derive_extension_hint)
}

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
///     .hint("mp3")
///     .look_ahead_bytes(1_000_000);
/// ```
#[derive(Clone, Builder)]
#[builder(state_mod(vis = "pub"))]
#[non_exhaustive]
pub struct ResourceConfig {
    /// Initial ABR mode passed to the HLS stream.
    #[builder(default)]
    pub initial_abr_mode: AbrMode,
    /// Unified event bus for streaming, decode, and audio events.
    #[builder(name = events)]
    pub bus: Option<EventBus>,
    /// Shared byte pool for temporary buffers (probe, etc.).
    pub byte_pool: Option<BytePool>,
    /// Cancellation token for graceful shutdown.
    pub cancel: Option<CancellationToken>,
    /// Optional format hint (file extension like "mp3", "wav").
    pub hint: Option<String>,
    /// Base URL for resolving relative HLS playlist/segment URLs.
    pub hls_base_url: Option<Url>,
    /// Target sample rate of the audio host (for resampling).
    pub host_sample_rate: Option<NonZeroU32>,
    /// Encryption key handling configuration.
    #[builder(default)]
    pub keys: KeyOptions,
    /// Max bytes the downloader may be ahead of the reader before it pauses.
    pub look_ahead_bytes: Option<u64>,
    /// Optional name for cache disambiguation.
    pub name: Option<String>,
    /// Additional HTTP headers to include in all network requests.
    pub headers: Option<Headers>,
    /// Shared PCM pool for temporary buffers.
    pub pcm_pool: Option<PcmPool>,
    /// Shared playback rate atomic for the audio pipeline resampler.
    pub playback_rate: Option<Arc<AtomicF32>>,
    /// Maximum peak bitrate in bits per second for ABR variant selection.
    #[builder(default = 0.0)]
    pub preferred_peak_bitrate: f64,
    /// Maximum peak bitrate for expensive networks (e.g., cellular).
    #[builder(default = 0.0)]
    pub preferred_peak_bitrate_for_expensive_networks: f64,
    /// Number of chunks to buffer before signaling preload readiness.
    #[builder(default = DEFAULT_PRELOAD_CHUNKS)]
    pub preload_chunks: NonZeroUsize,
    /// Selects the decoder backend explicitly.
    #[builder(default)]
    pub decoder_backend: DecoderBackend,
    /// Resampling quality preset.
    #[builder(default)]
    pub resampler_quality: ResamplerQuality,
    /// Audio resource source (URL or local path).
    pub src: ResourceSrc,
    /// Storage configuration (cache directory, eviction limits).
    #[builder(default)]
    pub store: StoreOptions,
    /// Shared downloader instance.
    pub downloader: Option<Downloader>,
    /// Shared flush coordinator for `AssetStore` on-disk indexes.
    pub flush_hub: Option<Arc<FlushHub>>,
    /// Shared audio worker handle for cooperative multi-track decoding.
    pub worker: Option<AudioWorkerHandle>,
    /// How leading/trailing PCM is trimmed after decode.
    #[builder(default)]
    pub gapless_mode: kithara_decode::GaplessMode,
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
    /// Backwards-compat fluent setters. Mirror the old `with_*` API so
    /// existing call-sites compile while the workspace migrates to the
    /// canonical `ResourceConfig::builder()` chain. To be removed in
    /// follow-up cleanup PR.
    #[must_use]
    pub fn with_events(mut self, bus: EventBus) -> Self {
        self.bus = Some(bus);
        self
    }
    #[must_use]
    pub fn with_headers(mut self, headers: Headers) -> Self {
        self.headers = Some(headers);
        self
    }
    #[must_use]
    pub fn with_hint<S: Into<String>>(mut self, hint: S) -> Self {
        self.hint = Some(hint.into());
        self
    }
    #[must_use]
    pub fn with_name<S: Into<String>>(mut self, name: S) -> Self {
        self.name = Some(name.into());
        self
    }
    #[must_use]
    pub fn with_store(mut self, store: StoreOptions) -> Self {
        self.store = store;
        self
    }
    #[must_use]
    pub fn with_downloader(mut self, dl: Downloader) -> Self {
        self.downloader = Some(dl);
        self
    }
    #[must_use]
    pub fn with_flush_hub(mut self, hub: Arc<FlushHub>) -> Self {
        self.flush_hub = Some(hub);
        self
    }
    #[must_use]
    pub fn with_worker(mut self, worker: AudioWorkerHandle) -> Self {
        self.worker = Some(worker);
        self
    }
    #[must_use]
    pub fn with_cancel(mut self, cancel: CancellationToken) -> Self {
        self.cancel = Some(cancel);
        self
    }
    #[must_use]
    pub fn with_initial_abr_mode(mut self, mode: AbrMode) -> Self {
        self.initial_abr_mode = mode;
        self
    }
    #[must_use]
    pub fn with_keys(mut self, keys: KeyOptions) -> Self {
        self.keys = keys;
        self
    }
    #[must_use]
    pub fn with_preferred_peak_bitrate(mut self, bps: f64) -> Self {
        self.preferred_peak_bitrate = bps;
        self
    }
    #[must_use]
    pub fn with_preferred_peak_bitrate_for_expensive_networks(mut self, bps: f64) -> Self {
        self.preferred_peak_bitrate_for_expensive_networks = bps;
        self
    }
    #[must_use]
    pub fn with_preload_chunks(mut self, n: NonZeroUsize) -> Self {
        self.preload_chunks = n;
        self
    }
    #[must_use]
    pub fn with_decoder_backend(mut self, backend: DecoderBackend) -> Self {
        self.decoder_backend = backend;
        self
    }
    #[must_use]
    pub fn with_resampler_quality(mut self, q: ResamplerQuality) -> Self {
        self.resampler_quality = q;
        self
    }
    #[must_use]
    pub fn with_byte_pool(mut self, pool: BytePool) -> Self {
        self.byte_pool = Some(pool);
        self
    }
    #[must_use]
    pub fn with_pcm_pool(mut self, pool: PcmPool) -> Self {
        self.pcm_pool = Some(pool);
        self
    }
    #[must_use]
    pub fn with_host_sample_rate(mut self, sr: NonZeroU32) -> Self {
        self.host_sample_rate = Some(sr);
        self
    }
    #[must_use]
    pub fn with_playback_rate(mut self, rate: Arc<AtomicF32>) -> Self {
        self.playback_rate = Some(rate);
        self
    }
    #[must_use]
    pub fn with_look_ahead_bytes(mut self, bytes: u64) -> Self {
        self.look_ahead_bytes = Some(bytes);
        self
    }
    #[must_use]
    pub fn with_hls_base_url(mut self, url: Url) -> Self {
        self.hls_base_url = Some(url);
        self
    }
    #[must_use]
    pub fn with_gapless_mode(mut self, mode: kithara_decode::GaplessMode) -> Self {
        self.gapless_mode = mode;
        self
    }

    /// Backwards-compat fluent setters mirroring [`Self::with_*`] without
    /// the `with_` prefix. Removed alongside the with_* aliases once
    /// all call-sites migrate to `ResourceConfig::builder()`.
    #[must_use]
    pub fn events(self, bus: EventBus) -> Self {
        self.with_events(bus)
    }
    #[must_use]
    pub fn headers(self, headers: Headers) -> Self {
        self.with_headers(headers)
    }
    #[must_use]
    pub fn hint<S: Into<String>>(self, hint: S) -> Self {
        self.with_hint(hint)
    }
    #[must_use]
    pub fn name<S: Into<String>>(self, name: S) -> Self {
        self.with_name(name)
    }
    #[must_use]
    pub fn store(self, store: StoreOptions) -> Self {
        self.with_store(store)
    }
    #[must_use]
    pub fn downloader(self, dl: Downloader) -> Self {
        self.with_downloader(dl)
    }
    #[must_use]
    pub fn flush_hub(self, hub: Arc<FlushHub>) -> Self {
        self.with_flush_hub(hub)
    }
    #[must_use]
    pub fn worker(self, worker: AudioWorkerHandle) -> Self {
        self.with_worker(worker)
    }
    #[must_use]
    pub fn cancel(self, cancel: CancellationToken) -> Self {
        self.with_cancel(cancel)
    }
    #[must_use]
    pub fn initial_abr_mode(self, mode: AbrMode) -> Self {
        self.with_initial_abr_mode(mode)
    }
    #[must_use]
    pub fn keys(self, keys: KeyOptions) -> Self {
        self.with_keys(keys)
    }
    #[must_use]
    pub fn preferred_peak_bitrate(self, bps: f64) -> Self {
        self.with_preferred_peak_bitrate(bps)
    }
    #[must_use]
    pub fn preferred_peak_bitrate_for_expensive_networks(self, bps: f64) -> Self {
        self.with_preferred_peak_bitrate_for_expensive_networks(bps)
    }
    #[must_use]
    pub fn preload_chunks(self, n: NonZeroUsize) -> Self {
        self.with_preload_chunks(n)
    }
    #[must_use]
    pub fn decoder_backend(self, backend: DecoderBackend) -> Self {
        self.with_decoder_backend(backend)
    }
    #[must_use]
    pub fn resampler_quality(self, q: ResamplerQuality) -> Self {
        self.with_resampler_quality(q)
    }
    #[must_use]
    pub fn byte_pool(self, pool: BytePool) -> Self {
        self.with_byte_pool(pool)
    }
    #[must_use]
    pub fn pcm_pool(self, pool: PcmPool) -> Self {
        self.with_pcm_pool(pool)
    }
    #[must_use]
    pub fn host_sample_rate(self, sr: NonZeroU32) -> Self {
        self.with_host_sample_rate(sr)
    }
    #[must_use]
    pub fn playback_rate(self, rate: Arc<AtomicF32>) -> Self {
        self.with_playback_rate(rate)
    }
    #[must_use]
    pub fn look_ahead_bytes(self, bytes: u64) -> Self {
        self.with_look_ahead_bytes(bytes)
    }
    #[must_use]
    pub fn hls_base_url(self, url: Url) -> Self {
        self.with_hls_base_url(url)
    }
    #[must_use]
    pub fn gapless_mode(self, mode: kithara_decode::GaplessMode) -> Self {
        self.with_gapless_mode(mode)
    }

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

        Ok(Self::builder().src(src).build())
    }

    /// Convert into an `AudioConfig<File>`.
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
        let mut store = self.store;
        if let Some(hub) = self.flush_hub {
            store.flush_hub = Some(hub);
        }
        let cancel_for_audio = self.cancel.clone();
        let file_config = FileConfig::for_src(file_src)
            .store(store)
            .downloader(dl)
            .maybe_look_ahead_bytes(self.look_ahead_bytes)
            .maybe_headers(self.headers)
            .maybe_name(self.name)
            .maybe_events(self.bus.clone())
            .maybe_cancel(self.cancel)
            .build();
        let mut config = AudioConfig::<kithara_file::File>::new(file_config);
        config.cancel = cancel_for_audio;
        config.hint = self.hint.or(hint);
        config.byte_pool = self.byte_pool;
        config.pcm_pool = self.pcm_pool;
        config.host_sample_rate = self.host_sample_rate;
        config.resampler_quality = self.resampler_quality;
        config.preload_chunks = self.preload_chunks;
        config.decoder_backend = self.decoder_backend;
        config.playback_rate = self.playback_rate;
        config.worker = self.worker;
        config.gapless_mode = self.gapless_mode;

        config
    }

    /// Convert into an `AudioConfig<Hls>`.
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

        let mut store = self.store;
        if let Some(hub) = self.flush_hub {
            store.flush_hub = Some(hub);
        }
        let cancel_for_audio = self.cancel.clone();
        let hls_config = HlsConfig::for_url(url)
            .store(store)
            .keys(self.keys)
            .maybe_downloader(self.downloader)
            .initial_abr_mode(self.initial_abr_mode)
            .maybe_look_ahead_bytes(self.look_ahead_bytes)
            .maybe_headers(self.headers)
            .maybe_name(self.name)
            .maybe_base_url(self.hls_base_url)
            .maybe_events(self.bus.clone())
            .maybe_cancel(self.cancel)
            .build();

        let mut config = AudioConfig::<kithara_hls::Hls>::new(hls_config);
        config.cancel = cancel_for_audio;
        config.hint = self.hint;
        config.byte_pool = self.byte_pool;
        config.pcm_pool = self.pcm_pool;
        config.host_sample_rate = self.host_sample_rate;
        config.resampler_quality = self.resampler_quality;
        config.preload_chunks = self.preload_chunks;
        config.decoder_backend = self.decoder_backend;
        config.playback_rate = self.playback_rate;
        config.worker = self.worker;
        config.gapless_mode = self.gapless_mode;

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

    #[kithara::test]
    fn config_file_url_derives_extension_hint_from_last_path_segment() {
        let config = ResourceConfig::new("https://example.com/audio/get-mp3/song.MP3?sign=test")
            .unwrap()
            .into_file_config();

        assert_eq!(config.hint.as_deref(), Some("mp3"));
    }

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

    #[kithara::test]
    fn config_bus_propagates_to_file_config() {
        let bus = EventBus::new(32);
        let config = ResourceConfig::new("https://example.com/song.mp3")
            .unwrap()
            .events(bus);
        let audio_config = config.into_file_config();
        assert!(audio_config.stream.bus.is_some());
    }

    #[kithara::test]
    fn config_bus_propagates_to_hls_config() {
        let bus = EventBus::new(32);
        let config = ResourceConfig::new("https://example.com/live.m3u8")
            .unwrap()
            .events(bus);
        let audio_config = config.into_hls_config().unwrap();
        assert!(audio_config.stream.bus.is_some());
    }

    #[kithara::test]
    fn config_with_headers() {
        let mut headers = Headers::new();
        headers.insert("Authorization", "Bearer test");
        let config = ResourceConfig::new("https://example.com/song.mp3")
            .unwrap()
            .headers(headers);

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
            .events(bus)
            .hint("mp3")
            .name("test")
            .preload_chunks(NonZeroUsize::new(5).expect("BUG: 5 > 0"));
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

    #[kithara::test]
    fn config_bitrate_propagates_to_hls_abr() {
        let config = ResourceConfig::new("https://example.com/live.m3u8")
            .unwrap()
            .preferred_peak_bitrate(512_000.0);
        let _audio_config = config.into_hls_config().unwrap();
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
            .worker(worker.clone());
        assert!(config.worker.is_some());
        worker.shutdown();
    }

    #[kithara::test]
    fn config_worker_propagates_to_file_config() {
        let worker = AudioWorkerHandle::new();
        let config = ResourceConfig::new("https://example.com/song.mp3")
            .unwrap()
            .worker(worker.clone());
        let audio_config = config.into_file_config();
        assert!(audio_config.worker.is_some());
        worker.shutdown();
    }

    #[kithara::test]
    fn config_worker_propagates_to_hls_config() {
        let worker = AudioWorkerHandle::new();
        let config = ResourceConfig::new("https://example.com/live.m3u8")
            .unwrap()
            .worker(worker.clone());
        let audio_config = config.into_hls_config().unwrap();
        assert!(audio_config.worker.is_some());
        worker.shutdown();
    }

    #[kithara::test]
    fn file_hint_none_for_url_without_extension() {
        let config =
            ResourceConfig::new("https://cdn-edge.zvq.me/track/streamhq?id=125475417").unwrap();
        let audio_config = config.into_file_config();
        assert_eq!(
            audio_config.hint, None,
            "URL without file extension must produce hint=None"
        );
    }

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
