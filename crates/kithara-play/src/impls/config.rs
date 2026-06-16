use std::{
    fmt,
    num::{NonZeroU32, NonZeroUsize},
    path::PathBuf,
    sync::Arc,
};

use bon::Builder;
use kithara_abr::AbrMode;
use kithara_assets::{AssetStore, FlushHub, StoreOptions};
use kithara_audio::{
    AudioConfig, AudioWorkerHandle, EngineLoad, ResamplerQuality, StretchControls,
};
use kithara_bufpool::{BytePool, PcmPool};
use kithara_decode::{DecodeError, DecoderBackend};
use kithara_events::EventBus;
use kithara_file::{FileConfig, FileSrc};
use kithara_hls::{HlsConfig, HlsStore, KeyOptions, SizeProbeMethod};
use kithara_net::Headers;
use kithara_platform::CancellationToken;
use kithara_stream::dl::{Downloader, DownloaderConfig};
use portable_atomic::AtomicF32;
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
/// See the crate `README.md` "Usage".
#[derive(Clone, Builder)]
#[builder(state_mod(vis = "pub"))]
#[non_exhaustive]
pub struct ResourceConfig {
    /// Initial ABR mode passed to the HLS stream.
    #[builder(default)]
    pub initial_abr_mode: AbrMode,
    /// Selects the decoder backend explicitly.
    #[builder(default)]
    pub decoder_backend: DecoderBackend,
    /// How leading/trailing PCM is trimmed after decode.
    #[builder(default)]
    pub gapless_mode: kithara_decode::GaplessMode,
    /// Encryption key handling configuration.
    #[builder(default)]
    pub keys: KeyOptions,
    /// Number of chunks to buffer before signaling preload readiness.
    #[builder(default = DEFAULT_PRELOAD_CHUNKS)]
    pub preload_chunks: NonZeroUsize,
    /// Unified event bus for streaming, decode, and audio events.
    #[builder(name = events)]
    pub bus: Option<EventBus>,
    /// Shared byte pool for temporary buffers (probe, etc.).
    pub byte_pool: Option<BytePool>,
    /// Cancellation token for graceful shutdown. The master `CancellationToken` whose
    /// shared atomic mirror reaches the HLS coord's lock-free `is_cancelled()`
    /// read; the async-only downloader / file / decode paths take its inner
    /// `CancellationToken` via [`CancellationToken::token`].
    pub cancel: Option<CancellationToken>,
    /// Shared downloader instance.
    pub downloader: Option<Downloader>,
    /// Shared flush coordinator for `AssetStore` on-disk indexes.
    pub flush_hub: Option<Arc<FlushHub>>,
    /// App-wide shared file store. When present, file resources for the
    /// same URL share one download and cached byte surface (player +
    /// waveform dedup). `None` builds a private per-resource store.
    pub file_asset_store: Option<Arc<AssetStore>>,
    /// App-wide shared HLS store (shared cache + DRM `process_fn` +
    /// per-`asset_root` eviction routing). `None` builds a private
    /// per-resource store.
    pub hls_asset_store: Option<HlsStore>,
    /// Additional HTTP headers to include in all network requests.
    pub headers: Option<Headers>,
    /// Optional format hint (file extension like "mp3", "wav").
    pub hint: Option<String>,
    /// Base URL for resolving relative HLS playlist/segment URLs.
    pub hls_base_url: Option<Url>,
    /// Target sample rate of the audio host (for resampling).
    pub host_sample_rate: Option<NonZeroU32>,
    /// Max bytes the downloader may be ahead of the reader before it pauses.
    pub look_ahead_bytes: Option<u64>,
    /// Optional name for cache disambiguation.
    pub name: Option<String>,
    /// Shared PCM pool for temporary buffers.
    pub pcm_pool: Option<PcmPool>,
    /// Shared playback rate atomic for the audio pipeline resampler in the
    /// non-tempo (no-`stretch`) chain.
    pub playback_rate: Option<Arc<AtomicF32>>,
    /// Live time-stretch controls (speed + key-lock + backend). `Some` selects
    /// tempo mode; the same `Arc` must flow to every track so live changes
    /// reach the running effect chain. `None` keeps the resampler-first chain.
    pub stretch: Option<Arc<StretchControls>>,
    /// Shared live audio-engine cost meter (decode + effects).
    pub engine_load: Option<Arc<EngineLoad>>,
    /// Shared audio worker handle for cooperative multi-track decoding.
    pub worker: Option<AudioWorkerHandle>,
    /// Resampling quality preset.
    #[builder(default)]
    pub resampler_quality: ResamplerQuality,
    /// Audio resource source (URL or local path).
    pub src: ResourceSrc,
    /// Method used by HLS size estimation to probe segment lengths.
    /// Default is [`SizeProbeMethod::Head`]; switch to
    /// [`SizeProbeMethod::RangeGet`] for upstreams that reject
    /// `HEAD` (zvuk stage `/drm/`).
    #[builder(default)]
    pub size_probe_method: SizeProbeMethod,
    /// Storage configuration (cache directory, eviction limits).
    #[builder(default)]
    pub store: StoreOptions,
    /// Maximum peak bitrate in bits per second for ABR variant selection.
    #[builder(default = 0.0)]
    pub preferred_peak_bitrate: f64,
    /// Maximum peak bitrate for expensive networks (e.g., cellular).
    #[builder(default = 0.0)]
    pub preferred_peak_bitrate_for_expensive_networks: f64,
}

impl ResourceConfig {
    /// Terminal constructor — parses input and returns a fully-built config.
    ///
    /// # Errors
    ///
    /// Returns `DecodeError::InvalidData` if input is not a valid URL or
    /// absolute path. See [`Self::parse_src`].
    pub fn new<S: AsRef<str>>(input: S) -> Result<Self, DecodeError> {
        Self::for_src(input).map(ResourceConfigBuilder::build)
    }

    /// Build an `AudioConfig<File>` from this resource configuration.
    pub(crate) fn build_file_config(self) -> AudioConfig<kithara_file::File> {
        let (file_src, derived_hint) = match self.src {
            ResourceSrc::Url(ref url) => {
                (FileSrc::Remote(url.clone()), derive_remote_file_hint(url))
            }
            ResourceSrc::Path(ref path) => (
                FileSrc::Local(path.clone()),
                path.extension()
                    .and_then(|e| e.to_str())
                    .map(str::to_lowercase),
            ),
        };
        let store = StoreOptions::builder()
            .cache_dir(self.store.cache_dir.clone())
            .maybe_flush_hub(
                self.flush_hub
                    .clone()
                    .or_else(|| self.store.flush_hub.clone()),
            )
            .build();
        let downloader = self.downloader.clone().unwrap_or_else(|| {
            let dl_cancel = self
                .cancel
                .as_ref()
                .map_or_else(CancellationToken::default, CancellationToken::child_token);
            let client = kithara_net::HttpClient::new(
                kithara_net::NetOptions::default(),
                dl_cancel.child_token(),
            );
            Downloader::new(
                DownloaderConfig::for_client(client)
                    .cancel(dl_cancel)
                    .build(),
            )
        });
        let file_config = FileConfig::for_src(file_src)
            .store(store)
            .downloader(downloader)
            .maybe_asset_store(self.file_asset_store.clone())
            .maybe_look_ahead_bytes(self.look_ahead_bytes)
            .maybe_headers(self.headers.clone())
            .maybe_name(self.name.clone())
            .maybe_events(self.bus.clone())
            .maybe_cancel(self.cancel.clone())
            .build();
        AudioConfig::<kithara_file::File>::for_stream(file_config)
            .maybe_cancel(self.cancel.clone())
            .maybe_hint(self.hint.or(derived_hint))
            .maybe_byte_pool(self.byte_pool)
            .maybe_pcm_pool(self.pcm_pool)
            .maybe_host_sample_rate(self.host_sample_rate)
            .resampler_quality(self.resampler_quality)
            .preload_chunks(self.preload_chunks)
            .decoder_backend(self.decoder_backend)
            .maybe_playback_rate(self.playback_rate)
            .maybe_stretch(self.stretch)
            .maybe_engine_load(self.engine_load)
            .maybe_worker(self.worker)
            .gapless_mode(self.gapless_mode)
            .build()
    }

    /// Build an `AudioConfig<Hls>` from this resource configuration.
    pub(crate) fn build_hls_config(self) -> Result<AudioConfig<kithara_hls::Hls>, DecodeError> {
        let url = match self.src {
            ResourceSrc::Url(ref url) => url.clone(),
            ResourceSrc::Path(ref p) => {
                return Err(DecodeError::InvalidData(format!(
                    "HLS requires a URL, got local path: {}",
                    p.display()
                )));
            }
        };
        let store = StoreOptions::builder()
            .cache_dir(self.store.cache_dir.clone())
            .maybe_flush_hub(
                self.flush_hub
                    .clone()
                    .or_else(|| self.store.flush_hub.clone()),
            )
            .build();
        let hls_config = HlsConfig::for_url(url)
            .store(store)
            .maybe_asset_store(self.hls_asset_store)
            .keys(self.keys)
            .maybe_downloader(self.downloader)
            .initial_abr_mode(self.initial_abr_mode)
            .maybe_look_ahead_bytes(self.look_ahead_bytes)
            .maybe_headers(self.headers)
            .maybe_name(self.name)
            .maybe_base_url(self.hls_base_url)
            .maybe_events(self.bus.clone())
            .maybe_cancel(self.cancel.clone())
            .size_probe_method(self.size_probe_method)
            .build();
        Ok(AudioConfig::<kithara_hls::Hls>::for_stream(hls_config)
            .maybe_cancel(self.cancel.clone())
            .maybe_hint(self.hint)
            .maybe_byte_pool(self.byte_pool)
            .maybe_pcm_pool(self.pcm_pool)
            .maybe_host_sample_rate(self.host_sample_rate)
            .resampler_quality(self.resampler_quality)
            .preload_chunks(self.preload_chunks)
            .decoder_backend(self.decoder_backend)
            .maybe_playback_rate(self.playback_rate)
            .maybe_stretch(self.stretch)
            .maybe_engine_load(self.engine_load)
            .maybe_worker(self.worker)
            .gapless_mode(self.gapless_mode)
            .build())
    }

    /// Chainable counterpart to [`Self::new`]: parses input and returns a
    /// builder with `src` already populated so call-sites can add options
    /// before `.build()`.
    ///
    /// # Errors
    ///
    /// Returns `DecodeError::InvalidData` if input is not a valid URL or
    /// absolute path. See [`Self::parse_src`].
    pub fn for_src<S: AsRef<str>>(
        input: S,
    ) -> Result<ResourceConfigBuilder<resource_config_builder::SetSrc>, DecodeError> {
        Self::parse_src(input).map(|src| Self::builder().src(src))
    }

    /// Parse a URL string or local file path into a [`ResourceSrc`].
    ///
    /// Tries URL parsing first. On failure, falls back to absolute file path.
    /// A `file://` URL is normalized to a `Path`.
    ///
    /// # Errors
    ///
    /// Returns `DecodeError::InvalidData` if the input is an invalid `file://`
    /// URL or a non-absolute file path.
    pub fn parse_src<S: AsRef<str>>(input: S) -> Result<ResourceSrc, DecodeError> {
        let trimmed = input.as_ref().trim();

        match Url::parse(trimmed) {
            #[cfg(not(target_arch = "wasm32"))]
            Ok(url) if url.scheme() == "file" => {
                let path = url.to_file_path().map_err(|()| {
                    DecodeError::InvalidData(format!("invalid file URL: {trimmed}"))
                })?;
                Ok(ResourceSrc::Path(path))
            }
            #[cfg(target_arch = "wasm32")]
            Ok(url) if url.scheme() == "file" => Err(DecodeError::InvalidData(format!(
                "file:// URL is not supported on wasm: {trimmed}"
            ))),
            Ok(url) => Ok(ResourceSrc::Url(url)),
            Err(_) => {
                let path = PathBuf::from(trimmed);
                if !path.is_absolute() {
                    return Err(DecodeError::InvalidData(format!(
                        "invalid URL or file path (must be absolute): {trimmed}"
                    )));
                }
                Ok(ResourceSrc::Path(path))
            }
        }
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
            .build_file_config();

        assert_eq!(config.hint.as_deref(), Some("mp3"));
    }

    #[kithara::test]
    fn config_file_url_without_extension_does_not_derive_hint() {
        let config = ResourceConfig::new("https://example.com/get-mp3/42?sign=test")
            .unwrap()
            .build_file_config();

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
        let config = ResourceConfig::for_src("https://example.com/song.mp3")
            .unwrap()
            .maybe_events(with_events.then(|| EventBus::new(32)))
            .build();
        assert_eq!(config.bus.is_some(), with_events);
    }

    #[kithara::test]
    fn config_bus_propagates_to_file_config() {
        let config = ResourceConfig::for_src("https://example.com/song.mp3")
            .unwrap()
            .events(EventBus::new(32))
            .build();
        let audio_config = config.build_file_config();
        assert!(audio_config.stream.bus.is_some());
    }

    #[kithara::test]
    fn config_bus_propagates_to_hls_config() {
        let config = ResourceConfig::for_src("https://example.com/live.m3u8")
            .unwrap()
            .events(EventBus::new(32))
            .build();
        let audio_config = config.build_hls_config().unwrap();
        assert!(audio_config.stream.bus.is_some());
    }

    #[kithara::test]
    fn config_with_headers() {
        let mut headers = Headers::new();
        headers.insert("Authorization", "Bearer test");
        let config = ResourceConfig::for_src("https://example.com/song.mp3")
            .unwrap()
            .headers(headers)
            .build();

        assert!(config.headers.is_some());
        assert_eq!(
            config.headers.as_ref().and_then(|h| h.get("Authorization")),
            Some("Bearer test")
        );
    }

    #[kithara::test]
    fn config_builder_chain() {
        let config = ResourceConfig::for_src("https://example.com/song.mp3")
            .unwrap()
            .events(EventBus::new(32))
            .hint("mp3".to_string())
            .name("test".to_string())
            .preload_chunks(NonZeroUsize::new(5).expect("BUG: 5 > 0"))
            .build();
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
        let config = ResourceConfig::for_src("https://example.com/live.m3u8")
            .unwrap()
            .preferred_peak_bitrate(512_000.0)
            .build();
        let _audio_config = config.build_hls_config().unwrap();
    }

    #[kithara::test]
    fn config_worker_default_none() {
        let config = ResourceConfig::new("https://example.com/song.mp3").unwrap();
        assert!(config.worker.is_none());
    }

    #[kithara::test]
    fn config_with_worker_sets_field() {
        let worker = AudioWorkerHandle::with_cancel(CancellationToken::default());
        let config = ResourceConfig::for_src("https://example.com/song.mp3")
            .unwrap()
            .worker(worker.clone())
            .build();
        assert!(config.worker.is_some());
        worker.shutdown();
    }

    #[kithara::test]
    fn config_worker_propagates_to_file_config() {
        let worker = AudioWorkerHandle::with_cancel(CancellationToken::default());
        let config = ResourceConfig::for_src("https://example.com/song.mp3")
            .unwrap()
            .worker(worker.clone())
            .build();
        let audio_config = config.build_file_config();
        assert!(audio_config.worker.is_some());
        worker.shutdown();
    }

    #[kithara::test]
    fn config_worker_propagates_to_hls_config() {
        let worker = AudioWorkerHandle::with_cancel(CancellationToken::default());
        let config = ResourceConfig::for_src("https://example.com/live.m3u8")
            .unwrap()
            .worker(worker.clone())
            .build();
        let audio_config = config.build_hls_config().unwrap();
        assert!(audio_config.worker.is_some());
        worker.shutdown();
    }

    #[kithara::test]
    fn file_hint_none_for_url_without_extension() {
        let config =
            ResourceConfig::new("https://cdn-edge.zvq.me/track/streamhq?id=125475417").unwrap();
        let audio_config = config.build_file_config();
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
        let audio_config = config.build_file_config();
        assert_eq!(
            audio_config.hint.as_deref(),
            expected,
            "hint mismatch for URL: {url}"
        );
    }
}
