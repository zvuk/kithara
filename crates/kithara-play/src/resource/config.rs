use std::num::{NonZeroU32, NonZeroUsize};

use kithara_abr::AbrMode;
use kithara_assets::AssetStore;
use kithara_audio::{AudioConfigPatch, AudioDecoderConfig};
use kithara_beat::BeatGridModel;
use kithara_bufpool::HasPool;
use kithara_config::Config;
use kithara_decode::TrackMetadata;
use kithara_download::Downloader;
use kithara_events::EventBus;
use kithara_file::FileConfigPatch;
use kithara_hls::{HlsConfigPatch, KeyOptions};
use kithara_net::Headers;
use kithara_platform::{CancelToken, CancelWakerGuard, sync::Arc};
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
#[derive(Config)]
#[config(construction, builder(on(String, into), start_fn = for_src))]
#[non_exhaustive]
#[derive_where::derive_where(Clone; B: Clone + Default, S: HasPool<u8> + Send + Sync + 'static)]
pub struct ResourceConfig<S, B: Default = PlaybackResamplerBackend>
where
    S: HasPool<u8> + Send + Sync + 'static,
{
    /// Audio resource source (URL or local path).
    #[config(skip = "consumed to select the resource", builder(start_fn))]
    pub(crate) src: ResourceSrc,
    /// Caller-known track metadata, held for the track's owner. The resource
    /// reports only its decoder tags; the owner keeps these fields ahead of
    /// them and takes from the tags only what these leave unset.
    #[config(skip = "kept by the track's owner ahead of decoder tags")]
    pub(crate) metadata: Option<TrackMetadata>,
    /// Initial ABR mode passed to the HLS stream.
    #[config(value, builder(default))]
    pub(crate) initial_abr_mode: AbrMode,
    /// Shared asset store used by playback and derived resources.
    #[config(skip = "transferred to the selected stream")]
    pub(crate) store: AssetStore<S>,
    /// What a configuration document says about the [`AudioConfig`] this
    /// resource builds, carried as a patch for the same reason [`Self::hls`]
    /// is. A document's live spelling is its own top-level `audio:` section.
    ///
    /// [`AudioConfig`]: kithara_audio::AudioConfig
    #[config(skip = "applied to the audio pipeline", builder(default))]
    pub(crate) audio: AudioConfigPatch,
    /// Decoder construction settings: backend selection, gapless mode, and
    /// decoder-side resampling.
    #[config(skip = "transferred to decoder dependencies", builder(default))]
    pub(crate) decoder: AudioDecoderConfig<B>,
    /// What a configuration document says about the [`FileConfig`] this
    /// resource builds, carried as a patch for the same reason [`Self::hls`]
    /// is. A document's live spelling is its own top-level `file:` section.
    ///
    /// [`FileConfig`]: kithara_file::FileConfig
    #[config(skip = "applied to the file stream", builder(default))]
    pub(crate) file: FileConfigPatch,
    /// What a configuration document says about the [`HlsConfig`] this
    /// resource builds, carried as a patch because that configuration cannot
    /// exist before the track's URL and store do. A document's live spelling
    /// is its own top-level `hls:` section.
    ///
    /// [`HlsConfig`]: kithara_hls::HlsConfig
    #[config(skip = "applied to the HLS stream", builder(default))]
    pub(crate) hls: HlsConfigPatch,
    /// Encryption key handling configuration.
    #[config(skip = "transferred to the key resolver", builder(default))]
    pub(crate) keys: KeyOptions,
    /// Where this track's encoded cover image is read from. The cover is an
    /// artifact of the track, read over [`Self::artifact_fetch`] beside the
    /// audio, which never waits for it.
    #[config(skip = "read as the track's cover artifact")]
    pub(crate) artwork: Option<ResourceSrc>,
    /// A beat grid this track already has, as a structure the caller holds or
    /// a source its bytes are read from. Analysis fills in only what no
    /// prepared artifact covers, so a track opened with a grid here is never
    /// re-analysed for one.
    #[config(skip = "transferred to the resource artifact source")]
    pub(crate) beat_grid: Option<ArtifactSource<BeatGridModel>>,
    /// Unified event bus for streaming, decode, and audio events.
    #[config(skip = "transferred to the event bus", builder(name = events))]
    pub(crate) bus: Option<EventBus>,
    /// Per-track parent cancel. The atomic flag reaches the HLS coord's
    /// lock-free `is_cancelled()` read; downloader / file / decode paths derive
    /// children via [`CancelToken::child`]. `None` lets each subsystem own a
    /// standalone scope (see [`CancelScope::new`](kithara_platform::CancelScope)).
    #[config(skip = "composed into the resource cancel scope")]
    pub(crate) cancel: Option<CancelToken>,
    /// Keeps the deck's cancellation connected to this track's subtree.
    #[config(skip = "retained until the resource lane is released", builder(skip))]
    pub(crate) cancel_link: Option<Arc<CancelWakerGuard>>,
    /// Optional cache discriminator mixed into the asset root.
    #[config(skip = "transferred to the asset key")]
    pub(crate) discriminator: Option<String>,
    /// Shared downloader instance.
    #[config(skip = "transferred to the selected stream")]
    pub(crate) downloader: Option<Downloader>,
    /// Shared live audio-engine cost meter (decode + effects).
    #[config(skip = "transferred to the play-owned producer")]
    pub(crate) engine_load: Option<Arc<EngineLoad>>,
    /// Additional HTTP headers to include in all network requests.
    #[config(skip = "transferred to network requests")]
    pub(crate) headers: Option<Headers>,
    /// Optional format hint (file extension like "mp3", "wav"). Per-call input
    /// read twice: the file branch maps it into `FileConfig::extension`,
    /// and both branches pass it to the decoder as a format hint.
    #[config(skip = "transferred to decoder and file construction")]
    pub(crate) hint: Option<String>,
    /// Base URL for resolving relative HLS playlist/segment URLs.
    #[config(skip = "transferred to HLS URL resolution")]
    pub(crate) hls_base_url: Option<Url>,
    /// Rate the audio host actually opened, handed to the built audio
    /// pipeline. The player overwrites it from its engine, so it is not a
    /// document key.
    #[config(skip = "transferred to the audio host-rate owner")]
    pub(crate) host_sample_rate: Option<NonZeroU32>,
    /// A waveform this track already has, on the same terms as
    /// [`Self::beat_grid`].
    #[config(skip = "transferred to the resource artifact source")]
    pub(crate) waveform: Option<ArtifactSource<Waveform>>,
    /// Final rendered chunks required before the lane reports readiness.
    /// An unset value uses the lane configuration's default.
    #[config(skip = "transferred to the render lane's preload quota")]
    pub(crate) preload_chunks: Option<NonZeroUsize>,
    /// Packet capacity of each lane ring. An unset value uses the lane
    /// configuration's default.
    #[config(skip = "transferred to the render lane's packet rings")]
    pub(crate) audio_buffer_chunks: Option<NonZeroUsize>,
    /// Explicit playback worker. Player preparation fills this field; direct
    /// Resource callers must configure it themselves.
    #[config(skip = "transferred to the playback worker")]
    pub(crate) worker: Option<PlayWorker<S>>,
    /// Resident Warp resources and live temporal controls. Not a document
    /// key: a player-managed resource has this overwritten with the player's
    /// own `warp`, which is where a document's `player.warp:` section lands.
    #[config(skip = "transferred to the play-owned warp lane", builder(default = WarpConfig::builder().build()))]
    pub(crate) warp: WarpConfig,
    /// Whether audio-thread reads block on a producer-ring underrun. Only an
    /// offline harness or a player's own policy sets this, so it is not a
    /// document key.
    #[config(value, builder(default))]
    pub(crate) block_on_underrun: bool,
    /// Requested peak-bitrate ceiling in bits per second, held for an ABR
    /// reader that does not exist yet. `resource/build.rs` forwards this to
    /// neither branch, so no value here changes variant selection today, and
    /// the one caller of [`ResourceConfig::preferred_peak_bitrate`] is a test
    /// asserting the value survives `Loader::build_config`. Not a document key
    /// for exactly that reason: a document knob the binary ignores is worse
    /// than no knob. Make it one when the ABR wiring lands.
    #[config(value, builder(default = 0.0))]
    pub(crate) preferred_peak_bitrate: f64,
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroUsize;

    use kithara_assets::AssetStore;
    use kithara_audio::{DecoderResamplerSettings, ResamplerBackend, ResamplerOptions};
    use kithara_decode::DecodeError;
    use kithara_stream::StreamType;
    use kithara_test_utils::kithara;

    use super::*;
    use crate::{
        PlayWorkerConfig,
        test_pools::{TestPools, pools},
    };

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
        assert!(kithara_file::File::<TestPools>::event_bus(audio_config.stream()).is_some());
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
        assert!(kithara_hls::Hls::<TestPools>::event_bus(audio_config.stream()).is_some());
    }

    #[kithara::test(native, tokio)]
    async fn direct_resources_wake_the_worker_off_rt() {
        use axum::{Router, routing::get};
        use kithara_audio::{Audio, DecoderChangeCause, DecoderEvent};
        use kithara_platform::sync::Arc;
        use kithara_stream::mock::NoopWorkerWake;
        use kithara_test_utils::{TestHttpServer, TestTempDir};

        let worker = worker();
        let dir = TestTempDir::new();
        let path = dir.path().join("direct.wav");
        crate::mock::write_pcm_wav(
            &path,
            &vec![0.5; 8_192],
            kithara_signal::AudioSpec::new(2, crate::mock::SAMPLE_RATE),
        )
        .expect("direct float WAV");
        let file_bus = EventBus::new(32);
        let mut file_events = file_bus.subscribe::<DecoderEvent>();
        let file: ResourceConfig<TestPools> =
            ResourceConfig::for_src(ResourceSrc::Path(path.clone()))
                .store(store())
                .events(file_bus)
                .build();
        let _file = Audio::prepare(
            file.build_file_config(&worker, None),
            Arc::new(NoopWorkerWake),
            pools(),
        )
        .await
        .expect("direct file builds");
        assert!(matches!(
            file_events
                .try_recv()
                .expect("build publishes inline")
                .event,
            DecoderEvent::DecoderChanged {
                cause: DecoderChangeCause::Initial,
                ..
            }
        ));

        let wav = std::fs::read(path).expect("generated segment");
        let server = TestHttpServer::new(Router::new()
            .route("/direct.m3u8", get(|| async {
                "#EXTM3U\n#EXT-X-VERSION:3\n#EXT-X-STREAM-INF:BANDWIDTH=2822400\n/direct-media.m3u8\n"
            }))
            .route("/direct-media.m3u8", get(|| async {
                "#EXTM3U\n#EXT-X-VERSION:3\n#EXT-X-TARGETDURATION:1\n#EXT-X-MEDIA-SEQUENCE:0\n#EXTINF:1,\n/direct.wav\n#EXT-X-ENDLIST\n"
            }))
            .route("/direct.wav", get(move || {
                let wav = wav.clone();
                async move { wav }
            }))).await;
        let hls_bus = EventBus::new(32);
        let mut hls_events = hls_bus.subscribe::<DecoderEvent>();
        let hls: ResourceConfig<TestPools> =
            ResourceConfig::for_src(ResourceSrc::Url(server.url("/direct.m3u8")))
                .store(store())
                .events(hls_bus)
                .hint("wav")
                .build();
        let _hls = Audio::prepare(
            hls.build_hls_config(&worker, None)
                .expect("valid HLS config"),
            Arc::new(NoopWorkerWake),
            pools(),
        )
        .await
        .expect("direct HLS builds");
        assert!(matches!(
            hls_events.try_recv().expect("build publishes inline").event,
            DecoderEvent::DecoderChanged {
                cause: DecoderChangeCause::Initial,
                ..
            }
        ));
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
                .maybe_preload_chunks(NonZeroUsize::new(5))
                .build();
        assert!(config.bus.is_some());
        assert_eq!(config.hint.as_deref(), Some("mp3"));
        assert_eq!(config.discriminator.as_deref(), Some("test"));
        assert_eq!(config.preload_chunks, NonZeroUsize::new(5));
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
        assert!((config.warp.speed() - 1.0).abs() < f32::EPSILON);
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
            config.preload_chunks.is_none(),
            "an unnamed audio key must leave kithara-audio's own default standing"
        );
        assert!(
            config.audio_buffer_chunks.is_none(),
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
