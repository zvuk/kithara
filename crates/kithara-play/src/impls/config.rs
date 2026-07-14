use std::{
    fmt,
    num::{NonZeroU32, NonZeroUsize},
    path::PathBuf,
};

use bon::Builder;
use kithara_abr::AbrMode;
use kithara_assets::{AssetStore, FlushHub, StoreOptions};
use kithara_audio::{
    AudioConfig, AudioDecoderConfig, AudioWorkerHandle, EngineLoad, ResamplerBackend,
    StretchControls,
};
use kithara_bufpool::{BytePool, PcmPool};
use kithara_decode::DecodeError;
use kithara_events::EventBus;
use kithara_file::{FileConfig, FileSrc};
use kithara_hls::{HlsConfig, KeyOptions, SizeProbeMethod};
use kithara_net::{Headers, HttpClient, NetOptions};
use kithara_platform::{CancelScope, CancelToken, sync::Arc};
use kithara_stream::dl::{Downloader, DownloaderConfig};
use portable_atomic::AtomicF32;
use url::Url;

use super::playback_resampler::PlaybackResamplerBackend;

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

fn store_options_with_flush_hub(
    store: &StoreOptions,
    flush_hub: Option<Arc<FlushHub>>,
) -> StoreOptions {
    StoreOptions::builder()
        .backend(store.backend.clone())
        .maybe_cache_capacity(store.cache_capacity)
        .maybe_flush_hub(flush_hub.or_else(|| store.flush_hub.clone()))
        .maybe_layout(store.layout.clone())
        .maybe_max_assets(store.max_assets)
        .maybe_max_bytes(store.max_bytes)
        .build()
}

/// Source of an audio resource: either a URL or a local file path.
#[derive(Clone, Debug, derive_more::From, PartialEq, Eq)]
pub enum ResourceSrc {
    /// Remote resource accessed via URL (HTTP/HTTPS, or other schemes).
    Url(Url),
    /// Local file accessed directly from disk.
    Path(PathBuf),
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

fn default_resource_decoder_config<B>() -> AudioDecoderConfig<B>
where
    B: Default,
{
    AudioDecoderConfig::builder()
        .resampler(
            kithara_audio::DecoderResamplerSettings::builder()
                .backend(B::default())
                .build(),
        )
        .build()
}

/// Unified configuration for creating an [`Item`](crate::impls::item::Item).
///
/// Wraps source, audio options, and protocol-specific settings into a single
/// builder. `Item::new(config)` auto-detects the stream type from the input.
/// See the crate `README.md` "Usage".
#[derive(Clone, Builder)]
#[builder(state_mod(vis = "pub"))]
#[non_exhaustive]
pub struct ResourceConfig<B: Default = PlaybackResamplerBackend> {
    /// Initial ABR mode passed to the HLS stream.
    #[builder(default)]
    pub initial_abr_mode: AbrMode,
    /// Decoder construction settings, including decoder-side resampling.
    #[builder(default = default_resource_decoder_config())]
    pub decoder: AudioDecoderConfig<B>,
    /// Encryption key handling configuration.
    #[builder(default)]
    pub keys: KeyOptions,
    /// Number of chunks to buffer before signaling preload readiness.
    #[builder(default = DEFAULT_PRELOAD_CHUNKS)]
    pub preload_chunks: NonZeroUsize,
    /// App-wide shared asset store. When present, resources for the same
    /// URL share one download and cached byte surface. `None` builds a private
    /// per-resource store.
    pub asset_store: Option<Arc<AssetStore>>,
    /// Unified event bus for streaming, decode, and audio events.
    #[builder(name = events)]
    pub bus: Option<EventBus>,
    /// Shared byte pool for temporary buffers (probe, etc.).
    pub byte_pool: BytePool,
    /// Per-track parent cancel. The atomic flag reaches the HLS coord's
    /// lock-free `is_cancelled()` read; downloader / file / decode paths derive
    /// children via [`CancelToken::child`]. `None` lets each subsystem own a
    /// standalone scope (see [`CancelScope::new`](kithara_platform::CancelScope)).
    pub cancel: Option<CancelToken>,
    /// Shared downloader instance.
    pub downloader: Option<Downloader>,
    /// Shared live audio-engine cost meter (decode + effects).
    pub engine_load: Option<Arc<EngineLoad>>,
    /// Shared flush coordinator for `AssetStore` on-disk indexes.
    pub flush_hub: Option<Arc<FlushHub>>,
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
    pub pcm_pool: PcmPool,
    /// Legacy shared playback-rate state for direct audio configuration. The
    /// resampler is fixed-ratio; live speed DSP uses `stretch`.
    pub playback_rate: Option<Arc<AtomicF32>>,
    /// Live time-stretch controls (speed + key-lock + backend). `Some` selects
    /// tempo mode; the same `Arc` must flow to every track so live changes
    /// reach the running effect chain. `None` leaves playback speed pinned.
    pub stretch: Option<Arc<StretchControls>>,
    /// Shared audio worker handle for cooperative multi-track decoding.
    pub worker: Option<AudioWorkerHandle>,
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

impl ResourceConfig<PlaybackResamplerBackend> {
    /// Terminal constructor — parses input and returns a fully-built config.
    ///
    /// # Errors
    ///
    /// Returns `DecodeError::InvalidData` if input is not a valid URL or
    /// absolute path. See [`Self::parse_src`].
    pub fn new<S: AsRef<str>>(
        input: S,
        byte_pool: BytePool,
        pcm_pool: PcmPool,
    ) -> Result<Self, DecodeError> {
        Self::for_src(input).map(|builder| builder.byte_pool(byte_pool).pcm_pool(pcm_pool).build())
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
    ) -> Result<
        ResourceConfigBuilder<PlaybackResamplerBackend, resource_config_builder::SetSrc>,
        DecodeError,
    > {
        Self::parse_src(input).map(|src| Self::builder().src(src))
    }
}

impl<B> ResourceConfig<B>
where
    B: Default + ResamplerBackend,
{
    /// Build an `AudioConfig<File>` from this resource configuration.
    pub(crate) fn build_file_config(self) -> AudioConfig<kithara_file::File, B> {
        let byte_pool = self.byte_pool.clone();
        let pcm_pool = self.pcm_pool.clone();
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
        let store = store_options_with_flush_hub(&self.store, self.flush_hub.clone());
        let downloader = self.downloader.clone().unwrap_or_else(|| {
            let dl_cancel = CancelScope::new(self.cancel.clone()).token();
            let net_options = NetOptions::builder().byte_pool(byte_pool.clone()).build();
            let client = HttpClient::new(net_options, dl_cancel.child());
            Downloader::new(
                DownloaderConfig::for_client(client)
                    .cancel(dl_cancel)
                    .build(),
            )
        });
        let file_config = FileConfig::for_src(file_src)
            .store(store)
            .downloader(downloader)
            .maybe_asset_store(self.asset_store.clone())
            .maybe_look_ahead_bytes(self.look_ahead_bytes)
            .maybe_headers(self.headers.clone())
            .maybe_name(self.name.clone())
            .pool(byte_pool.clone())
            .maybe_events(self.bus.clone())
            .maybe_cancel(self.cancel.clone())
            .build();
        AudioConfig::<kithara_file::File, B>::for_stream(file_config)
            .maybe_cancel(self.cancel.clone())
            .maybe_hint(self.hint.or(derived_hint))
            .byte_pool(byte_pool)
            .pcm_pool(pcm_pool)
            .maybe_host_sample_rate(self.host_sample_rate)
            .preload_chunks(self.preload_chunks)
            .decoder(self.decoder)
            .maybe_playback_rate(self.playback_rate)
            .maybe_stretch(self.stretch)
            .maybe_engine_load(self.engine_load)
            .maybe_worker(self.worker)
            .build()
    }

    /// Build an `AudioConfig<Hls>` from this resource configuration.
    pub(crate) fn build_hls_config(self) -> Result<AudioConfig<kithara_hls::Hls, B>, DecodeError> {
        let byte_pool = self.byte_pool.clone();
        let pcm_pool = self.pcm_pool.clone();
        let url = match self.src {
            ResourceSrc::Url(ref url) => url.clone(),
            ResourceSrc::Path(_) => {
                return Err(DecodeError::InvalidData {
                    detail: "HLS requires a URL, got a local path",
                });
            }
        };
        let store = store_options_with_flush_hub(&self.store, self.flush_hub.clone());
        let hls_config = HlsConfig::for_url(url)
            .store(store)
            .maybe_asset_store(self.asset_store)
            .keys(self.keys)
            .maybe_downloader(self.downloader)
            .initial_abr_mode(self.initial_abr_mode)
            .maybe_look_ahead_bytes(self.look_ahead_bytes)
            .maybe_headers(self.headers)
            .maybe_name(self.name)
            .maybe_base_url(self.hls_base_url)
            .pool(byte_pool.clone())
            .maybe_events(self.bus.clone())
            .maybe_cancel(self.cancel.clone())
            .size_probe_method(self.size_probe_method)
            .build();
        Ok(AudioConfig::<kithara_hls::Hls, B>::for_stream(hls_config)
            .maybe_cancel(self.cancel.clone())
            .maybe_hint(self.hint)
            .byte_pool(byte_pool)
            .pcm_pool(pcm_pool)
            .maybe_host_sample_rate(self.host_sample_rate)
            .preload_chunks(self.preload_chunks)
            .decoder(self.decoder)
            .maybe_playback_rate(self.playback_rate)
            .maybe_stretch(self.stretch)
            .maybe_engine_load(self.engine_load)
            .maybe_worker(self.worker)
            .build())
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
                let path = url.to_file_path().map_err(|()| DecodeError::InvalidData {
                    detail: "invalid file URL",
                })?;
                Ok(ResourceSrc::Path(path))
            }
            #[cfg(target_arch = "wasm32")]
            Ok(url) if url.scheme() == "file" => Err(DecodeError::InvalidData {
                detail: "file:// URL is not supported on wasm",
            }),
            Ok(url) => Ok(ResourceSrc::Url(url)),
            Err(_) => {
                let path = PathBuf::from(trimmed);
                if !path.is_absolute() {
                    return Err(DecodeError::InvalidData {
                        detail: "invalid URL or file path (must be absolute)",
                    });
                }
                Ok(ResourceSrc::Path(path))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use kithara_audio::{DecoderResamplerSettings, ResamplerBackend, ResamplerOptions};
    use kithara_test_utils::kithara;

    use super::*;

    fn test_resource_config(input: &str) -> Result<ResourceConfig, DecodeError> {
        ResourceConfig::new(input, BytePool::default(), PcmPool::default())
    }

    #[kithara::test]
    fn config_source_parsing_url() {
        let config = test_resource_config("https://example.com/song.mp3").unwrap();
        assert!(matches!(&config.src, ResourceSrc::Url(url) if url.scheme() == "https"));
    }

    #[kithara::test]
    fn config_file_url_derives_extension_hint_from_last_path_segment() {
        let config = test_resource_config("https://example.com/audio/get-mp3/song.MP3?sign=test")
            .unwrap()
            .build_file_config();

        assert_eq!(config.hint.as_deref(), Some("mp3"));
    }

    #[kithara::test]
    fn config_file_url_without_extension_does_not_derive_hint() {
        let config = test_resource_config("https://example.com/get-mp3/42?sign=test")
            .unwrap()
            .build_file_config();

        assert_eq!(config.hint, None);
    }

    #[kithara::test(native)]
    #[case("/tmp/song.mp3", "/tmp/song.mp3")]
    #[case("file:///tmp/song.mp3", "/tmp/song.mp3")]
    fn config_source_parsing_file_path(#[case] input: &str, #[case] expected: &str) {
        let config = test_resource_config(input).unwrap();
        assert!(matches!(
            &config.src,
            ResourceSrc::Path(path) if path == Path::new(expected)
        ));
    }

    #[kithara::test]
    #[case("relative/path.mp3")]
    fn config_source_parsing_error(#[case] input: &str) {
        assert!(test_resource_config(input).is_err());
    }

    #[kithara::test]
    #[case(false)]
    #[case(true)]
    fn config_bus_presence(#[case] with_events: bool) {
        let config = ResourceConfig::for_src("https://example.com/song.mp3")
            .unwrap()
            .byte_pool(BytePool::default())
            .pcm_pool(PcmPool::default())
            .maybe_events(with_events.then(|| EventBus::new(32)))
            .build();
        assert_eq!(config.bus.is_some(), with_events);
    }

    #[kithara::test]
    fn config_bus_propagates_to_file_config() {
        let config = ResourceConfig::for_src("https://example.com/song.mp3")
            .unwrap()
            .byte_pool(BytePool::default())
            .pcm_pool(PcmPool::default())
            .events(EventBus::new(32))
            .build();
        let audio_config = config.build_file_config();
        assert!(audio_config.stream.bus.is_some());
    }

    #[kithara::test]
    fn config_bus_propagates_to_hls_config() {
        let config = ResourceConfig::for_src("https://example.com/live.m3u8")
            .unwrap()
            .byte_pool(BytePool::default())
            .pcm_pool(PcmPool::default())
            .events(EventBus::new(32))
            .build();
        let audio_config = config.build_hls_config().unwrap();
        assert!(audio_config.stream.bus.is_some());
    }

    #[kithara::test]
    fn config_resampler_options_propagate_to_file_config() {
        let decoder = AudioDecoderConfig::builder()
            .resampler(
                DecoderResamplerSettings::builder()
                    .backend(PlaybackResamplerBackend::default())
                    .options(ResamplerOptions::builder().chunk_size(2_048).build())
                    .build(),
            )
            .build();
        let config = ResourceConfig::for_src("https://example.com/song.mp3")
            .unwrap()
            .byte_pool(BytePool::default())
            .pcm_pool(PcmPool::default())
            .decoder(decoder)
            .build();
        let audio_config = config.build_file_config();

        assert_eq!(
            audio_config
                .decoder
                .resampler
                .as_ref()
                .expect("resampler config")
                .options
                .chunk_size,
            2_048
        );
    }

    #[kithara::test]
    fn config_explicit_resampler_backend_propagates_to_hls_config() {
        let decoder = AudioDecoderConfig::builder()
            .resampler(
                DecoderResamplerSettings::builder()
                    .backend(PlaybackResamplerBackend::default())
                    .build(),
            )
            .build();
        let config = ResourceConfig::for_src("https://example.com/live.m3u8")
            .unwrap()
            .byte_pool(BytePool::default())
            .pcm_pool(PcmPool::default())
            .decoder(decoder)
            .build();
        let audio_config = config.build_hls_config().unwrap();

        assert_eq!(
            audio_config
                .decoder
                .resampler
                .as_ref()
                .expect("resampler config")
                .backend
                .name(),
            PlaybackResamplerBackend::default().name()
        );
    }

    #[kithara::test]
    fn config_with_headers() {
        let mut headers = Headers::new();
        headers.insert("Authorization", "Bearer test");
        let config = ResourceConfig::for_src("https://example.com/song.mp3")
            .unwrap()
            .byte_pool(BytePool::default())
            .pcm_pool(PcmPool::default())
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
            .byte_pool(BytePool::default())
            .pcm_pool(PcmPool::default())
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
        let config = test_resource_config("https://example.com/live.m3u8").unwrap();
        assert!((config.preferred_peak_bitrate - 0.0).abs() < f64::EPSILON);
        assert!((config.preferred_peak_bitrate_for_expensive_networks - 0.0).abs() < f64::EPSILON);
    }

    #[kithara::test]
    fn config_bitrate_propagates_to_hls_abr() {
        let config = ResourceConfig::for_src("https://example.com/live.m3u8")
            .unwrap()
            .byte_pool(BytePool::default())
            .pcm_pool(PcmPool::default())
            .preferred_peak_bitrate(512_000.0)
            .build();
        let _audio_config = config.build_hls_config().unwrap();
    }

    #[kithara::test]
    fn config_worker_default_none() {
        let config = test_resource_config("https://example.com/song.mp3").unwrap();
        assert!(config.worker.is_none());
    }

    #[kithara::test]
    fn config_with_worker_sets_field() {
        let worker = AudioWorkerHandle::with_cancel(CancelToken::never());
        let config = ResourceConfig::for_src("https://example.com/song.mp3")
            .unwrap()
            .byte_pool(BytePool::default())
            .pcm_pool(PcmPool::default())
            .worker(worker.clone())
            .build();
        assert!(config.worker.is_some());
        worker.shutdown();
    }

    #[kithara::test]
    fn config_worker_propagates_to_file_config() {
        let worker = AudioWorkerHandle::with_cancel(CancelToken::never());
        let config = ResourceConfig::for_src("https://example.com/song.mp3")
            .unwrap()
            .byte_pool(BytePool::default())
            .pcm_pool(PcmPool::default())
            .worker(worker.clone())
            .build();
        let audio_config = config.build_file_config();
        assert!(audio_config.worker.is_some());
        worker.shutdown();
    }

    #[kithara::test]
    fn config_worker_propagates_to_hls_config() {
        let worker = AudioWorkerHandle::with_cancel(CancelToken::never());
        let config = ResourceConfig::for_src("https://example.com/live.m3u8")
            .unwrap()
            .byte_pool(BytePool::default())
            .pcm_pool(PcmPool::default())
            .worker(worker.clone())
            .build();
        let audio_config = config.build_hls_config().unwrap();
        assert!(audio_config.worker.is_some());
        worker.shutdown();
    }

    #[kithara::test]
    fn file_hint_none_for_url_without_extension() {
        let config =
            test_resource_config("https://cdn-edge.zvq.me/track/streamhq?id=125475417").unwrap();
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
        let config = test_resource_config(url).unwrap();
        let audio_config = config.build_file_config();
        assert_eq!(
            audio_config.hint.as_deref(),
            expected,
            "hint mismatch for URL: {url}"
        );
    }
}
