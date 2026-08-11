use std::num::{NonZeroU32, NonZeroUsize};

use bon::Builder;
use kithara_abr::AbrMode;
use kithara_assets::AssetStore;
use kithara_audio::{AudioDecoderConfig, AudioWorkerHandle, EngineLoad, TempoSlot};
use kithara_bufpool::{BytePool, PcmPool};
use kithara_events::EventBus;
use kithara_hls::{KeyOptions, SizeProbeMethod};
use kithara_net::Headers;
use kithara_platform::{CancelToken, sync::Arc};
use kithara_stream::dl::Downloader;
use portable_atomic::AtomicF32;
use url::Url;

use super::{ResourceSrc, resampler::PlaybackResamplerBackend};

/// Default number of preload chunks.
const DEFAULT_PRELOAD_CHUNKS: NonZeroUsize = NonZeroUsize::new(3).unwrap();

/// Unified configuration for opening an audio resource.
#[derive(Clone, Builder)]
#[builder(on(String, into), start_fn = for_src)]
#[non_exhaustive]
pub struct ResourceConfig<B: Default = PlaybackResamplerBackend> {
    /// Audio resource source (URL or local path).
    #[builder(start_fn)]
    pub(crate) src: ResourceSrc,
    /// Initial ABR mode passed to the HLS stream.
    #[builder(default)]
    pub(crate) initial_abr_mode: AbrMode,
    /// Shared asset store used by playback and derived resources.
    pub(crate) store: AssetStore,
    /// Decoder construction settings: backend selection, gapless mode, and
    /// decoder-side resampling.
    #[builder(default)]
    pub(crate) decoder: AudioDecoderConfig<B>,
    /// Shared byte pool for temporary buffers (probe, etc.).
    pub(crate) byte_pool: BytePool,
    /// Encryption key handling configuration.
    #[builder(default)]
    pub(crate) keys: KeyOptions,
    /// Number of chunks to buffer before signaling preload readiness.
    #[builder(default = DEFAULT_PRELOAD_CHUNKS)]
    pub(crate) preload_chunks: NonZeroUsize,
    /// Unified event bus for streaming, decode, and audio events.
    #[builder(name = events)]
    pub(crate) bus: Option<EventBus>,
    /// Per-track parent cancel. The atomic flag reaches the HLS coord's
    /// lock-free `is_cancelled()` read; downloader / file / decode paths derive
    /// children via [`CancelToken::child`]. `None` lets each subsystem own a
    /// standalone scope (see [`CancelScope::new`](kithara_platform::CancelScope)).
    pub(crate) cancel: Option<CancelToken>,
    /// Optional cache discriminator mixed into the asset root.
    pub(crate) discriminator: Option<String>,
    /// Shared downloader instance.
    pub(crate) downloader: Option<Downloader>,
    /// Shared live audio-engine cost meter (decode + effects).
    pub(crate) engine_load: Option<Arc<EngineLoad>>,
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
    /// Shared playback rate atomic for the audio pipeline resampler in the
    /// non-tempo (no-`stretch`) chain.
    pub(crate) playback_rate: Option<Arc<AtomicF32>>,
    /// What times this deck: live stretch controls, or a binding to the
    /// session grid. `Some` selects tempo mode; the same slot must flow to
    /// every track so live changes reach the running effect chain. `None`
    /// keeps the resampler-first chain.
    pub(crate) tempo: Option<TempoSlot>,
    /// Shared audio worker handle for cooperative multi-track decoding.
    pub(crate) worker: Option<AudioWorkerHandle>,
    /// Shared PCM pool for temporary buffers.
    pub(crate) pcm_pool: PcmPool,
    /// Method used by HLS size estimation to probe segment lengths.
    /// Default is [`SizeProbeMethod::Head`]; switch to
    /// [`SizeProbeMethod::RangeGet`] for upstreams that reject
    /// `HEAD` (zvuk stage `/drm/`).
    #[builder(default)]
    pub(crate) size_probe_method: SizeProbeMethod,
    /// Maximum peak bitrate in bits per second for ABR variant selection.
    #[builder(default = 0.0)]
    pub(crate) preferred_peak_bitrate: f64,
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use kithara_assets::AssetStore;
    use kithara_audio::{DecoderResamplerSettings, ResamplerBackend, ResamplerOptions};
    use kithara_decode::DecodeError;
    use kithara_test_utils::kithara;

    use super::*;
    use crate::resource::source::parse_src;

    fn store() -> AssetStore {
        AssetStore::builder().build()
    }

    fn valid_src(input: &str) -> ResourceSrc {
        parse_src(input).expect("valid test source")
    }

    fn test_config<S: AsRef<str>>(input: S) -> Result<ResourceConfig, DecodeError> {
        Ok(ResourceConfig::for_src(parse_src(input)?)
            .store(store())
            .byte_pool(BytePool::default())
            .pcm_pool(PcmPool::default())
            .build())
    }

    #[kithara::test]
    fn config_source_parsing_url() {
        let config = test_config("https://example.com/song.mp3").unwrap();
        assert!(matches!(&config.src, ResourceSrc::Url(url) if url.scheme() == "https"));
    }

    #[kithara::test]
    fn config_file_url_derives_extension_hint_from_last_path_segment() {
        let config = test_config("https://example.com/audio/get-mp3/song.MP3?sign=test")
            .unwrap()
            .build_file_config();

        assert_eq!(config.hint(), Some("mp3"));
    }

    #[kithara::test]
    fn config_file_url_without_extension_does_not_derive_hint() {
        let config = test_config("https://example.com/get-mp3/42?sign=test")
            .unwrap()
            .build_file_config();

        assert_eq!(config.hint(), None);
    }

    #[kithara::test(native)]
    #[case("/tmp/song.mp3", "/tmp/song.mp3")]
    #[case("file:///tmp/song.mp3", "/tmp/song.mp3")]
    fn config_source_parsing_file_path(#[case] input: &str, #[case] expected: &str) {
        let config = test_config(input).unwrap();
        assert!(matches!(
            &config.src,
            ResourceSrc::Path(path) if path == Path::new(expected)
        ));
    }

    #[kithara::test]
    #[case("relative/path.mp3")]
    fn config_source_parsing_error(#[case] input: &str) {
        assert!(test_config(input).is_err());
    }

    #[kithara::test]
    #[case(false)]
    #[case(true)]
    fn config_bus_presence(#[case] with_events: bool) {
        let config: ResourceConfig =
            ResourceConfig::for_src(valid_src("https://example.com/song.mp3"))
                .store(store())
                .byte_pool(BytePool::default())
                .pcm_pool(PcmPool::default())
                .maybe_events(with_events.then(|| EventBus::new(32)))
                .build();
        assert_eq!(config.bus.is_some(), with_events);
    }

    #[kithara::test]
    fn config_bus_propagates_to_file_config() {
        let config: ResourceConfig =
            ResourceConfig::for_src(valid_src("https://example.com/song.mp3"))
                .store(store())
                .byte_pool(BytePool::default())
                .pcm_pool(PcmPool::default())
                .events(EventBus::new(32))
                .build();
        let audio_config = config.build_file_config();
        assert!(audio_config.stream().bus.is_some());
    }

    #[kithara::test]
    fn config_bus_propagates_to_hls_config() {
        let config: ResourceConfig =
            ResourceConfig::for_src(valid_src("https://example.com/live.m3u8"))
                .store(store())
                .byte_pool(BytePool::default())
                .pcm_pool(PcmPool::default())
                .events(EventBus::new(32))
                .build();
        let audio_config = config.build_hls_config().unwrap();
        assert!(audio_config.stream().bus.is_some());
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
        let config: ResourceConfig =
            ResourceConfig::for_src(valid_src("https://example.com/song.mp3"))
                .store(store())
                .byte_pool(BytePool::default())
                .pcm_pool(PcmPool::default())
                .decoder(decoder)
                .build();
        let audio_config = config.build_file_config();

        assert_eq!(
            audio_config
                .decoder()
                .resampler()
                .expect("resampler config")
                .options()
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
        let config: ResourceConfig =
            ResourceConfig::for_src(valid_src("https://example.com/live.m3u8"))
                .store(store())
                .byte_pool(BytePool::default())
                .pcm_pool(PcmPool::default())
                .decoder(decoder)
                .build();
        let audio_config = config.build_hls_config().unwrap();

        assert_eq!(
            audio_config
                .decoder()
                .resampler()
                .expect("resampler config")
                .backend()
                .name(),
            PlaybackResamplerBackend::default().name()
        );
    }

    #[kithara::test]
    fn config_with_headers() {
        let mut headers = Headers::default();
        headers.insert("Authorization", "Bearer test");
        let config: ResourceConfig =
            ResourceConfig::for_src(valid_src("https://example.com/song.mp3"))
                .store(store())
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
        let config: ResourceConfig =
            ResourceConfig::for_src(valid_src("https://example.com/song.mp3"))
                .store(store())
                .byte_pool(BytePool::default())
                .pcm_pool(PcmPool::default())
                .events(EventBus::new(32))
                .hint("mp3")
                .discriminator("test")
                .preload_chunks(NonZeroUsize::new(5).expect("BUG: 5 > 0"))
                .build();
        assert!(config.bus.is_some());
        assert_eq!(config.hint.as_deref(), Some("mp3"));
        assert_eq!(config.discriminator.as_deref(), Some("test"));
        assert_eq!(config.preload_chunks.get(), 5);
    }

    #[kithara::test]
    fn config_bitrate_fields_default_zero() {
        let config = test_config("https://example.com/live.m3u8").unwrap();
        assert!((config.preferred_peak_bitrate - 0.0).abs() < f64::EPSILON);
    }

    #[kithara::test]
    fn config_bitrate_propagates_to_hls_abr() {
        let config: ResourceConfig =
            ResourceConfig::for_src(valid_src("https://example.com/live.m3u8"))
                .store(store())
                .byte_pool(BytePool::default())
                .pcm_pool(PcmPool::default())
                .preferred_peak_bitrate(512_000.0)
                .build();
        let _audio_config = config.build_hls_config().unwrap();
    }

    #[kithara::test]
    fn config_worker_default_none() {
        let config = test_config("https://example.com/song.mp3").unwrap();
        assert!(config.worker.is_none());
    }

    #[kithara::test]
    fn config_with_worker_sets_field() {
        let worker = AudioWorkerHandle::with_cancel(CancelToken::never());
        let config: ResourceConfig =
            ResourceConfig::for_src(valid_src("https://example.com/song.mp3"))
                .store(store())
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
        let config: ResourceConfig =
            ResourceConfig::for_src(valid_src("https://example.com/song.mp3"))
                .store(store())
                .byte_pool(BytePool::default())
                .pcm_pool(PcmPool::default())
                .worker(worker.clone())
                .build();
        let audio_config = config.build_file_config();
        assert!(audio_config.worker().is_some());
        worker.shutdown();
    }

    #[kithara::test]
    fn config_worker_propagates_to_hls_config() {
        let worker = AudioWorkerHandle::with_cancel(CancelToken::never());
        let config: ResourceConfig =
            ResourceConfig::for_src(valid_src("https://example.com/live.m3u8"))
                .store(store())
                .byte_pool(BytePool::default())
                .pcm_pool(PcmPool::default())
                .worker(worker.clone())
                .build();
        let audio_config = config.build_hls_config().unwrap();
        assert!(audio_config.worker().is_some());
        worker.shutdown();
    }

    #[kithara::test]
    fn file_hint_none_for_url_without_extension() {
        let config = test_config("https://cdn-edge.zvq.me/track/streamhq?id=125475417").unwrap();
        let audio_config = config.build_file_config();
        assert_eq!(
            audio_config.hint(),
            None,
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
        let config = test_config(url).unwrap();
        let audio_config = config.build_file_config();
        assert_eq!(
            audio_config.hint(),
            expected,
            "hint mismatch for URL: {url}"
        );
    }
}
