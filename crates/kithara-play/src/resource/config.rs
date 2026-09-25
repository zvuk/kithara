use std::num::NonZeroU32;

use bon::Builder;
use kithara_abr::AbrMode;
use kithara_assets::AssetStore;
use kithara_audio::{AudioConfigPatch, AudioDecoderConfig};
use kithara_beat::BeatGridModel;
use kithara_bufpool::HasPool;
use kithara_download::Downloader;
use kithara_events::EventBus;
use kithara_file::FileConfigPatch;
use kithara_hls::{HlsConfigPatch, KeyOptions};
use kithara_net::Headers;
use kithara_platform::{CancelToken, sync::Arc};
use kithara_warp::WarpConfig;
use kithara_waveform::Waveform;
use url::Url;

use super::{ArtifactSource, ResourceSrc, resampler::PlaybackResamplerBackend};
use crate::{EngineLoad, PlayWorker};

/// Unified configuration for opening an audio resource.
///
/// Holds the per-call wiring and per-stream input this resource is opened
/// with, the runtime-owned audio capabilities a player overwrites, and what a
/// configuration document says about each of the three configurations this
/// resource builds — carried as patches, because none of those configurations
/// can exist before the track does.
#[kithara_config::config(construction, builder = false)]
#[derive(Builder)]
#[builder(on(String, into), start_fn = for_src)]
#[non_exhaustive]
#[derive_where::derive_where(Clone; B: Clone + Default, S: HasPool<u8> + Send + Sync + 'static)]
pub struct ResourceConfig<S, B: Default = PlaybackResamplerBackend>
where
    S: HasPool<u8> + Send + Sync + 'static,
{
    /// Audio resource source (URL or local path).
    #[config(value, builder(start_fn))]
    pub(crate) src: ResourceSrc,
    /// A beat grid this track already has, as a structure the caller holds or
    /// a source its bytes are read from. Analysis fills in only what no
    /// prepared artifact covers, so a track opened with a grid here is never
    /// re-analysed for one.
    #[config(skip = "injected beat-grid artifact")]
    pub(crate) beat_grid: Option<ArtifactSource<BeatGridModel>>,
    /// A waveform this track already has, on the same terms as
    /// [`Self::beat_grid`].
    #[config(skip = "injected waveform artifact")]
    pub(crate) waveform: Option<ArtifactSource<Waveform>>,
    /// Initial ABR mode passed to the HLS stream.
    #[config(value, builder(default))]
    pub(crate) initial_abr_mode: AbrMode,
    /// Shared asset store used by playback and derived resources.
    #[config(skip = "injected asset store")]
    pub(crate) store: AssetStore<S>,
    /// What a configuration document says about the [`AudioConfig`] this
    /// resource builds, carried as a patch for the same reason [`Self::hls`]
    /// is. A document's live spelling is its own top-level `audio:` section.
    ///
    /// [`AudioConfig`]: kithara_audio::AudioConfig
    #[config(value, builder(default))]
    pub(crate) audio: AudioConfigPatch,
    /// Decoder construction settings: backend selection, gapless mode, and
    /// decoder-side resampling.
    #[config(value, builder(default))]
    pub(crate) decoder: AudioDecoderConfig<B>,
    /// What a configuration document says about the [`FileConfig`] this
    /// resource builds, carried as a patch for the same reason [`Self::hls`]
    /// is. A document's live spelling is its own top-level `file:` section.
    ///
    /// [`FileConfig`]: kithara_file::FileConfig
    #[config(value, builder(default))]
    pub(crate) file: FileConfigPatch,
    /// What a configuration document says about the [`HlsConfig`] this
    /// resource builds, carried as a patch because that configuration cannot
    /// exist before the track's URL and store do. A document's live spelling
    /// is its own top-level `hls:` section.
    ///
    /// [`HlsConfig`]: kithara_hls::HlsConfig
    #[config(value, builder(default))]
    pub(crate) hls: HlsConfigPatch,
    /// Encryption key handling configuration.
    #[config(value, builder(default))]
    pub(crate) keys: KeyOptions,
    /// Unified event bus for streaming, decode, and audio events.
    #[config(skip = "injected event bus", builder(name = events))]
    pub(crate) bus: Option<EventBus>,
    /// Per-track parent cancel. The atomic flag reaches the HLS coord's
    /// lock-free `is_cancelled()` read; downloader / file / decode paths derive
    /// children via [`CancelToken::child`]. `None` lets each subsystem own a
    /// standalone scope (see [`CancelScope::new`](kithara_platform::CancelScope)).
    #[config(skip = "injected cancellation resource")]
    pub(crate) cancel: Option<CancelToken>,
    /// Optional cache discriminator mixed into the asset root.
    #[config(value)]
    pub(crate) discriminator: Option<String>,
    /// Shared downloader instance.
    #[config(skip = "injected downloader resource")]
    pub(crate) downloader: Option<Downloader>,
    /// Shared live audio-engine cost meter (decode + effects).
    #[config(skip = "injected engine cost meter")]
    pub(crate) engine_load: Option<Arc<EngineLoad>>,
    /// Additional HTTP headers to include in all network requests.
    #[config(value)]
    pub(crate) headers: Option<Headers>,
    /// Optional format hint (file extension like "mp3", "wav"). Per-call input
    /// read twice: the file branch maps it into `FileConfig::extension`,
    /// and both branches pass it to the decoder as a format hint.
    #[config(value)]
    pub(crate) hint: Option<String>,
    /// Base URL for resolving relative HLS playlist/segment URLs.
    #[config(value)]
    pub(crate) hls_base_url: Option<Url>,
    /// Rate the audio host actually opened, handed to the built audio
    /// pipeline. The player overwrites it from its engine, so it is not a
    /// document key.
    #[config(skip = "host-owned sample rate")]
    pub(crate) host_sample_rate: Option<NonZeroU32>,
    /// Explicit playback worker. Player preparation fills this field; direct
    /// Resource callers must configure it themselves.
    #[config(skip = "injected playback worker")]
    pub(crate) worker: Option<PlayWorker<S>>,
    /// Resident Warp resources and live temporal controls. Not a document
    /// key: a player-managed resource has this overwritten with the player's
    /// own `warp`, which is where a document's `player.warp:` section lands.
    #[config(skip = "resident Warp resource", builder(default = WarpConfig::builder().build()))]
    pub(crate) warp: WarpConfig,
    /// Whether audio-thread reads block on a producer-ring underrun. Only an
    /// offline harness or a player's own policy sets this, so it is not a
    /// document key.
    #[config(skip = "offline-only blocking policy", builder(default))]
    pub(crate) block_on_underrun: bool,
    /// Initial HLS ABR bitrate ceiling in bits per second; zero means no cap.
    /// Passed to the per-stream ABR handle before variant selection. The
    /// file branch ignores it. This per-call input is not a document key.
    #[config(value, builder(default = 0.0))]
    pub(crate) preferred_peak_bitrate: f64,
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroUsize;

    use kithara_assets::AssetStore;
    use kithara_audio::{
        AudioConfigPatch, ConsumerWakeMode, DecoderResamplerSettings, ResamplerBackend,
        ResamplerOptions,
    };
    use kithara_decode::DecodeError;
    use kithara_test_utils::kithara;

    use super::*;
    use crate::{
        PlayWorkerConfig,
        test_pools::{TestPools, pools},
    };

    fn preload_chunks(count: usize) -> AudioConfigPatch {
        let mut patch = AudioConfigPatch::default();
        patch.preload_chunks = NonZeroUsize::new(count);
        patch
    }

    fn store() -> AssetStore<TestPools> {
        AssetStore::builder(pools()).build()
    }

    fn valid_src(input: &str) -> ResourceSrc {
        ResourceSrc::parse(input).expect("valid test source")
    }

    fn test_config<S: AsRef<str>>(input: S) -> Result<ResourceConfig<TestPools>, DecodeError> {
        Ok(ResourceConfig::for_src(ResourceSrc::parse(input)?)
            .store(store())
            .build())
    }

    fn worker() -> PlayWorker<TestPools> {
        PlayWorker::new(PlayWorkerConfig::builder(pools()).build())
    }

    #[kithara::test]
    fn config_source_parsing_url() {
        let config = test_config("https://example.com/song.mp3").unwrap();
        assert!(matches!(&config.src, ResourceSrc::Url(url) if url.scheme() == "https"));
    }

    #[kithara::test]
    fn config_file_url_derives_extension_hint_from_last_path_segment() {
        let worker = worker();
        let config = test_config("https://example.com/audio/get-mp3/song.MP3?sign=test")
            .unwrap()
            .build_file_config(&worker, None);

        assert_eq!(config.hint(), Some("mp3"));
    }

    #[kithara::test]
    fn config_file_url_without_extension_does_not_derive_hint() {
        let worker = worker();
        let config = test_config("https://example.com/get-mp3/42?sign=test")
            .unwrap()
            .build_file_config(&worker, None);

        assert_eq!(config.hint(), None);
    }

    #[kithara::test(native)]
    #[case(false)]
    #[case(true)]
    fn config_source_parsing_file_path(#[case] as_file_url: bool) {
        let expected = std::env::temp_dir().join("song.mp3");
        let input = if as_file_url {
            Url::from_file_path(&expected)
                .expect("temp dir is absolute")
                .to_string()
        } else {
            expected.to_str().expect("utf-8 temp dir").to_string()
        };
        let config = test_config(&input).unwrap();
        assert!(matches!(
            &config.src,
            ResourceSrc::Path(path) if *path == expected
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
        let config: ResourceConfig<TestPools> =
            ResourceConfig::for_src(valid_src("https://example.com/song.mp3"))
                .store(store())
                .maybe_events(with_events.then(|| EventBus::new(32)))
                .build();
        assert_eq!(config.bus.is_some(), with_events);
    }

    #[kithara::test]
    fn config_bus_propagates_to_file_config() {
        let worker = worker();
        let config: ResourceConfig<TestPools> =
            ResourceConfig::for_src(valid_src("https://example.com/song.mp3"))
                .store(store())
                .events(EventBus::new(32))
                .build();
        let audio_config = config.build_file_config(&worker, None);
        assert!(audio_config.stream().bus.is_some());
    }

    #[kithara::test]
    fn config_bus_propagates_to_hls_config() {
        let worker = worker();
        let config: ResourceConfig<TestPools> =
            ResourceConfig::for_src(valid_src("https://example.com/live.m3u8"))
                .store(store())
                .events(EventBus::new(32))
                .build();
        let audio_config = config.build_hls_config(&worker, None).unwrap();
        assert!(audio_config.stream().bus.is_some());
    }

    #[kithara::test]
    fn direct_resources_wake_the_worker_off_rt() {
        let worker = worker();
        let file: ResourceConfig<TestPools> =
            ResourceConfig::for_src(valid_src("https://example.com/a.mp3"))
                .store(store())
                .build();
        assert_eq!(
            file.build_file_config(&worker, None).consumer_wake_mode(),
            ConsumerWakeMode::ImmediateOffRt
        );

        let hls: ResourceConfig<TestPools> =
            ResourceConfig::for_src(valid_src("https://example.com/a.m3u8"))
                .store(store())
                .build();
        assert_eq!(
            hls.build_hls_config(&worker, None)
                .expect("valid HLS config")
                .consumer_wake_mode(),
            ConsumerWakeMode::ImmediateOffRt
        );
    }

    #[kithara::test]
    fn config_resampler_options_propagate_to_file_config() {
        let worker = worker();
        let decoder = AudioDecoderConfig::builder()
            .resampler(
                DecoderResamplerSettings::builder()
                    .backend(PlaybackResamplerBackend::default())
                    .options(ResamplerOptions::builder().chunk_size(2_048).build())
                    .build(),
            )
            .build();
        let config: ResourceConfig<TestPools> =
            ResourceConfig::for_src(valid_src("https://example.com/song.mp3"))
                .store(store())
                .decoder(decoder)
                .build();
        let audio_config = config.build_file_config(&worker, None);

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
        let worker = worker();
        let decoder = AudioDecoderConfig::builder()
            .resampler(
                DecoderResamplerSettings::builder()
                    .backend(PlaybackResamplerBackend::default())
                    .build(),
            )
            .build();
        let config: ResourceConfig<TestPools> =
            ResourceConfig::for_src(valid_src("https://example.com/live.m3u8"))
                .store(store())
                .decoder(decoder)
                .build();
        let audio_config = config.build_hls_config(&worker, None).unwrap();

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
        let config: ResourceConfig<TestPools> =
            ResourceConfig::for_src(valid_src("https://example.com/song.mp3"))
                .store(store())
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
        let config: ResourceConfig<TestPools> =
            ResourceConfig::for_src(valid_src("https://example.com/song.mp3"))
                .store(store())
                .events(EventBus::new(32))
                .hint("mp3")
                .discriminator("test")
                .audio(preload_chunks(5))
                .build();
        assert!(config.bus.is_some());
        assert_eq!(config.hint.as_deref(), Some("mp3"));
        assert_eq!(config.discriminator.as_deref(), Some("test"));
        assert_eq!(config.audio.preload_chunks, NonZeroUsize::new(5));
    }

    #[kithara::test]
    fn config_bitrate_fields_default_zero() {
        let config = test_config("https://example.com/live.m3u8").unwrap();
        assert!((config.preferred_peak_bitrate - 0.0).abs() < f64::EPSILON);
    }

    #[kithara::test]
    fn config_worker_default_none() {
        let config = test_config("https://example.com/song.mp3").unwrap();
        assert!(config.worker.is_none());
    }

    #[kithara::test]
    fn config_stretch_defaults_to_unity() {
        let config = test_config("https://example.com/song.mp3").unwrap();
        assert!((config.warp.stretch().speed() - 1.0).abs() < f32::EPSILON);
    }

    #[kithara::test]
    fn config_with_worker_sets_field() {
        let worker = worker();
        let config: ResourceConfig<TestPools> =
            ResourceConfig::for_src(valid_src("https://example.com/song.mp3"))
                .store(store())
                .worker(worker.clone())
                .build();
        let configured = config.worker.as_ref().expect("worker must be configured");
        assert!(std::ptr::eq(configured.pools(), worker.pools()));
    }

    #[kithara::test]
    fn file_hint_none_for_url_without_extension() {
        let worker = worker();
        let config = test_config("https://cdn-edge.zvq.me/track/streamhq?id=125475417").unwrap();
        let audio_config = config.build_file_config(&worker, None);
        assert_eq!(
            audio_config.hint(),
            None,
            "URL without file extension must produce hint=None"
        );
    }

    /// The defaults a resource opened without a document carries: its own
    /// runtime-owned audio capabilities, and three empty patches that leave
    /// each built configuration on its crate's own defaults.
    #[kithara::test]
    fn defaults_match_the_documented_values() {
        let config = test_config("https://example.com/song.mp3").expect("valid config");

        assert!((config.preferred_peak_bitrate - 0.0).abs() < f64::EPSILON);
        assert!(!config.block_on_underrun);
        assert!(config.host_sample_rate.is_none());
        assert!(
            config.hls.download_batch_size.is_none(),
            "an unnamed HLS key must leave kithara-hls's own default standing"
        );
        assert!(
            config.file.reader_event_capacity.is_none(),
            "an unnamed file key must leave kithara-file's own default standing"
        );
        assert!(
            config.audio.preload_chunks.is_none(),
            "an unnamed audio key must leave kithara-audio's own default standing"
        );
        assert!(
            config.audio.audio_buffer_chunks.is_none(),
            "a direct resource names no output-ring depth, so the platform default stands"
        );
    }

    #[kithara::test]
    #[case("https://example.com/song.mp3", Some("mp3"))]
    #[case("https://example.com/audio.flac", Some("flac"))]
    #[case("https://example.com/track/stream", None)]
    #[case("https://example.com/track/streamhq?id=123", None)]
    #[case("https://example.com/audio", None)]
    fn file_hint_from_url_extension(#[case] url: &str, #[case] expected: Option<&str>) {
        let worker = worker();
        let config = test_config(url).unwrap();
        let audio_config = config.build_file_config(&worker, None);
        assert_eq!(
            audio_config.hint(),
            expected,
            "hint mismatch for URL: {url}"
        );
    }
}
