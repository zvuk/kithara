use std::{
    num::{NonZeroU32, NonZeroUsize},
    sync::Arc,
};

use bon::Builder;
use kithara_abr::AbrMode;
use kithara_assets::{AssetStore, FlushHub, StoreOptions};
use kithara_audio::{AudioWorkerHandle, EngineLoad, ResamplerQuality, StretchControls};
use kithara_bufpool::{BytePool, PcmPool};
use kithara_decode::{DecodeError, DecoderBackend};
use kithara_events::EventBus;
use kithara_hls::{KeyOptions, SizeProbeMethod};
use kithara_net::Headers;
use kithara_platform::CancelToken;
use kithara_stream::dl::Downloader;
use portable_atomic::AtomicF32;
use url::Url;

use super::{ResourceSrc, source::parse_src};

/// Default number of preload chunks.
const DEFAULT_PRELOAD_CHUNKS: NonZeroUsize = NonZeroUsize::new(3).unwrap();

/// Unified configuration for opening an audio resource.
#[derive(Clone, Builder)]
#[builder(state_mod(vis = "pub"))]
#[non_exhaustive]
pub struct ResourceConfig {
    /// Initial ABR mode passed to the HLS stream.
    #[builder(default)]
    pub(crate) initial_abr_mode: AbrMode,
    /// Selects the decoder backend explicitly.
    #[builder(default)]
    pub(crate) decoder_backend: DecoderBackend,
    /// How leading/trailing PCM is trimmed after decode.
    #[builder(default)]
    pub(crate) gapless_mode: kithara_decode::GaplessMode,
    /// Encryption key handling configuration.
    #[builder(default)]
    pub(crate) keys: KeyOptions,
    /// Number of chunks to buffer before signaling preload readiness.
    #[builder(default = DEFAULT_PRELOAD_CHUNKS)]
    pub(crate) preload_chunks: NonZeroUsize,
    /// App-wide shared asset store. When present, resources for the same
    /// URL share one download and cached byte surface. `None` builds a private
    /// per-resource store.
    pub(crate) asset_store: Option<Arc<AssetStore>>,
    /// Unified event bus for streaming, decode, and audio events.
    #[builder(name = events)]
    pub(crate) bus: Option<EventBus>,
    /// Shared byte pool for temporary buffers (probe, etc.).
    pub(crate) byte_pool: Option<BytePool>,
    /// Per-track parent cancel. The atomic flag reaches the HLS coord's
    /// lock-free `is_cancelled()` read; downloader / file / decode paths derive
    /// children via [`CancelToken::child`]. `None` lets each subsystem own a
    /// standalone scope (see [`CancelScope::new`](kithara_platform::CancelScope)).
    pub(crate) cancel: Option<CancelToken>,
    /// Shared downloader instance.
    pub(crate) downloader: Option<Downloader>,
    /// Shared live audio-engine cost meter (decode + effects).
    pub(crate) engine_load: Option<Arc<EngineLoad>>,
    /// Shared flush coordinator for `AssetStore` on-disk indexes.
    pub(crate) flush_hub: Option<Arc<FlushHub>>,
    /// Additional HTTP headers to include in all network requests.
    pub(crate) headers: Option<Headers>,
    /// Optional format hint (file extension like "mp3", "wav").
    pub(crate) hint: Option<String>,
    /// Base URL for resolving relative HLS playlist/segment URLs.
    pub(crate) hls_base_url: Option<Url>,
    /// Target sample rate of the audio host (for resampling).
    pub(crate) host_sample_rate: Option<NonZeroU32>,
    /// Max bytes the downloader may be ahead of the reader before it pauses.
    pub(crate) look_ahead_bytes: Option<u64>,
    /// Optional name for cache disambiguation.
    pub(crate) name: Option<String>,
    /// Shared PCM pool for temporary buffers.
    pub(crate) pcm_pool: Option<PcmPool>,
    /// Shared playback rate atomic for the audio pipeline resampler in the
    /// non-tempo (no-`stretch`) chain.
    pub(crate) playback_rate: Option<Arc<AtomicF32>>,
    /// Live time-stretch controls (speed + key-lock + backend). `Some` selects
    /// tempo mode; the same `Arc` must flow to every track so live changes
    /// reach the running effect chain. `None` keeps the resampler-first chain.
    pub(crate) stretch: Option<Arc<StretchControls>>,
    /// Shared audio worker handle for cooperative multi-track decoding.
    pub(crate) worker: Option<AudioWorkerHandle>,
    /// Resampling quality preset.
    #[builder(default)]
    pub(crate) resampler_quality: ResamplerQuality,
    /// Audio resource source (URL or local path).
    pub(crate) src: ResourceSrc,
    /// Method used by HLS size estimation to probe segment lengths.
    /// Default is [`SizeProbeMethod::Head`]; switch to
    /// [`SizeProbeMethod::RangeGet`] for upstreams that reject
    /// `HEAD` (zvuk stage `/drm/`).
    #[builder(default)]
    pub(crate) size_probe_method: SizeProbeMethod,
    /// Storage configuration (cache directory, eviction limits).
    #[builder(default)]
    pub(crate) store: StoreOptions,
    /// Maximum peak bitrate in bits per second for ABR variant selection.
    #[builder(default = 0.0)]
    pub(crate) preferred_peak_bitrate: f64,
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
        parse_src(input).map(|src| Self::builder().src(src))
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
        let worker = AudioWorkerHandle::with_cancel(CancelToken::never());
        let config = ResourceConfig::for_src("https://example.com/song.mp3")
            .unwrap()
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
