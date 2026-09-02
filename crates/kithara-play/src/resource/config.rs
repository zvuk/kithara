use std::num::{NonZeroU32, NonZeroUsize};

use bon::Builder;
use kithara_abr::AbrMode;
use kithara_assets::AssetStore;
use kithara_audio::{AudioDecoderConfig, ConsumerWakeMode};
use kithara_bufpool::HasPool;
use kithara_decode::DecodeError;
use kithara_events::EventBus;
use kithara_hls::{KeyOptions, SizeProbeMethod};
use kithara_net::Headers;
use kithara_platform::{CancelToken, sync::Arc};
use kithara_stream::dl::Downloader;
use kithara_warp::WarpConfig;
use url::Url;

use super::{ResourceSrc, resampler::PlaybackResamplerBackend};
use crate::{EngineLoad, PlayWorker};

struct Defaults;

impl Defaults {
    #[cfg(not(target_arch = "wasm32"))]
    const AUDIO_BUFFER_CHUNKS: NonZeroUsize = match NonZeroUsize::new(6) {
        Some(chunks) => chunks,
        None => unreachable!(),
    };
    #[cfg(target_arch = "wasm32")]
    const AUDIO_BUFFER_CHUNKS: NonZeroUsize = match NonZeroUsize::new(32) {
        Some(chunks) => chunks,
        None => unreachable!(),
    };
    #[cfg(not(target_arch = "wasm32"))]
    const PRELOAD_CHUNKS: NonZeroUsize = match NonZeroUsize::new(1) {
        Some(chunks) => chunks,
        None => unreachable!(),
    };
    #[cfg(target_arch = "wasm32")]
    const PRELOAD_CHUNKS: NonZeroUsize = match NonZeroUsize::new(3) {
        Some(chunks) => chunks,
        None => unreachable!(),
    };
}

/// Unified configuration for opening an audio resource.
#[derive(Builder)]
#[builder(on(String, into), start_fn = for_src)]
#[non_exhaustive]
pub struct ResourceConfig<S, B: Default = PlaybackResamplerBackend>
where
    S: HasPool<u8> + Send + Sync + 'static,
{
    /// Audio resource source (URL or local path).
    #[builder(start_fn)]
    pub(crate) src: ResourceSrc,
    /// Initial ABR mode passed to the HLS stream.
    #[builder(default)]
    pub(crate) initial_abr_mode: AbrMode,
    /// Shared asset store used by playback and derived resources.
    pub(crate) store: AssetStore<S>,
    /// Decoder construction settings: backend selection, gapless mode, and
    /// decoder-side resampling.
    #[builder(default)]
    pub(crate) decoder: AudioDecoderConfig<B>,
    /// Encryption key handling configuration.
    #[builder(default)]
    pub(crate) keys: KeyOptions,
    /// Number of chunks buffered before preload readiness for direct resources.
    /// Player preparation replaces this with its frame-derived geometry.
    #[builder(default = Defaults::PRELOAD_CHUNKS)]
    pub(crate) preload_chunks: NonZeroUsize,
    /// Final PCM ring depth for direct resources. Player preparation replaces
    /// this with its frame-derived geometry.
    #[builder(default = Defaults::AUDIO_BUFFER_CHUNKS)]
    pub(crate) audio_buffer_chunks: NonZeroUsize,
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
    /// Resident Warp resources and live temporal controls.
    #[builder(default = WarpConfig::builder().build())]
    pub(crate) warp: WarpConfig,
    /// Session-owned output block, copied by `ConfigPrep` for ring sizing.
    #[builder(skip)]
    pub(crate) output_buffer_frames: Option<NonZeroUsize>,
    /// Player-owned native elastic response budget, copied by `ConfigPrep`.
    #[builder(skip)]
    pub(crate) response_budget_frames: Option<NonZeroUsize>,
    /// Explicit playback worker. Player preparation fills this field; direct
    /// Resource callers must configure it themselves.
    pub(crate) worker: Option<PlayWorker<S>>,
    /// Session-owned audio-consumer wake capability. This is populated by
    /// `PlayerImpl::prepare_config`, not by callers, so resource configuration
    /// does not become a second public source of session policy. `None` means
    /// the resource is consumed directly off RT.
    #[builder(skip)]
    pub(crate) consumer_wake_mode: Option<ConsumerWakeMode>,
    /// Make audio-thread reads block on a producer-ring underrun instead of
    /// zero-filling. `PlayerImpl::prepare_config` copies the player's policy
    /// here; a direct reader off the real-time thread may opt in itself.
    /// Never set on a resource consumed by a real-time callback.
    #[builder(default)]
    pub(crate) block_on_underrun: bool,
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

impl<S, B> Clone for ResourceConfig<S, B>
where
    B: Clone + Default,
    S: HasPool<u8> + Send + Sync + 'static,
{
    fn clone(&self) -> Self {
        Self {
            src: self.src.clone(),
            initial_abr_mode: self.initial_abr_mode,
            store: self.store.clone(),
            decoder: self.decoder.clone(),
            keys: self.keys.clone(),
            preload_chunks: self.preload_chunks,
            audio_buffer_chunks: self.audio_buffer_chunks,
            bus: self.bus.clone(),
            cancel: self.cancel.clone(),
            discriminator: self.discriminator.clone(),
            downloader: self.downloader.clone(),
            engine_load: self.engine_load.clone(),
            headers: self.headers.clone(),
            hint: self.hint.clone(),
            hls_base_url: self.hls_base_url.clone(),
            host_sample_rate: self.host_sample_rate,
            look_ahead_bytes: self.look_ahead_bytes,
            warp: self.warp.clone(),
            output_buffer_frames: self.output_buffer_frames,
            response_budget_frames: self.response_budget_frames,
            worker: self.worker.clone(),
            consumer_wake_mode: self.consumer_wake_mode,
            block_on_underrun: self.block_on_underrun,
            size_probe_method: self.size_probe_method,
            preferred_peak_bitrate: self.preferred_peak_bitrate,
        }
    }
}

impl<S, B> ResourceConfig<S, B>
where
    B: Default,
    S: HasPool<u8> + Send + Sync + 'static,
{
    pub(crate) fn resolve_output_geometry(&mut self) -> Result<(), DecodeError> {
        let (Some(output_buffer_frames), Some(response_budget_frames)) =
            (self.output_buffer_frames, self.response_budget_frames)
        else {
            if self.output_buffer_frames.is_some() || self.response_budget_frames.is_some() {
                return Err(DecodeError::InvalidData {
                    detail: "incomplete playback response contract",
                });
            }
            return Ok(());
        };
        let quantum = self.warp.render_quantum_frames().get();
        let preload_chunks = output_buffer_frames.get().div_ceil(quantum);
        let ring_chunks = preload_chunks
            .checked_add(1)
            .ok_or(DecodeError::InvalidData {
                detail: "playback output geometry overflow",
            })?;
        let stale_frames = ring_chunks
            .checked_add(1)
            .and_then(|chunks| chunks.checked_mul(quantum))
            .and_then(|frames| frames.checked_sub(1))
            .ok_or(DecodeError::InvalidData {
                detail: "playback output geometry overflow",
            })?;
        if stale_frames > response_budget_frames.get() {
            return Err(DecodeError::InvalidData {
                detail: "playback output buffer exceeds the configured response budget",
            });
        }
        self.preload_chunks =
            NonZeroUsize::new(preload_chunks).ok_or(DecodeError::InvalidData {
                detail: "playback preload geometry is empty",
            })?;
        self.audio_buffer_chunks =
            NonZeroUsize::new(ring_chunks).ok_or(DecodeError::InvalidData {
                detail: "playback output geometry is empty",
            })?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use kithara_assets::AssetStore;
    use kithara_audio::{DecoderResamplerSettings, ResamplerBackend, ResamplerOptions};
    use kithara_decode::DecodeError;
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

    #[kithara::test]
    fn direct_config_has_no_session_wake_policy() {
        let config = test_config("https://example.com/track.mp3").expect("valid config");
        assert_eq!(config.consumer_wake_mode, None);
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
                .preload_chunks(NonZeroUsize::new(5).expect("test preload is non-zero"))
                .audio_buffer_chunks(NonZeroUsize::new(2).expect("test ring is non-zero"))
                .build();
        assert!(config.bus.is_some());
        assert_eq!(config.hint.as_deref(), Some("mp3"));
        assert_eq!(config.discriminator.as_deref(), Some("test"));
        assert_eq!(config.preload_chunks.get(), 5);
        assert_eq!(config.audio_buffer_chunks.get(), 2);
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[kithara::test]
    fn direct_config_preserves_native_chunk_defaults() {
        let config = test_config("https://example.com/song.mp3").expect("valid config");

        assert_eq!(config.preload_chunks.get(), 1);
        assert_eq!(config.audio_buffer_chunks.get(), 6);
    }

    #[cfg(target_arch = "wasm32")]
    #[kithara::test]
    fn direct_config_preserves_wasm_chunk_defaults() {
        let config = test_config("https://example.com/song.mp3").expect("valid config");

        assert_eq!(config.preload_chunks.get(), 3);
        assert_eq!(config.audio_buffer_chunks.get(), 32);
    }

    fn frame_contract_config(
        quantum: usize,
        output_buffer: usize,
        response_budget: usize,
    ) -> ResourceConfig<TestPools> {
        let warp = WarpConfig::builder()
            .render_quantum_frames(NonZeroUsize::new(quantum).expect("test quantum is non-zero"))
            .build();
        let mut config = ResourceConfig::for_src(valid_src("https://example.com/song.mp3"))
            .store(store())
            .warp(warp)
            .build();
        config.output_buffer_frames =
            Some(NonZeroUsize::new(output_buffer).expect("test output buffer is non-zero"));
        config.response_budget_frames =
            Some(NonZeroUsize::new(response_budget).expect("test response budget is non-zero"));
        config
    }

    #[kithara::test]
    #[case::industry_budget(64, 128, 255, 2, 3)]
    #[case::large_continuity_buffer(64, 512, 639, 8, 9)]
    fn valid_frame_contract_resolves_exact_quantum_slots(
        #[case] quantum: usize,
        #[case] buffer: usize,
        #[case] budget: usize,
        #[case] expected_preload: usize,
        #[case] expected_ring: usize,
    ) {
        let mut config = frame_contract_config(quantum, buffer, budget);

        config.resolve_output_geometry().expect("valid geometry");

        assert_eq!(config.preload_chunks.get(), expected_preload);
        assert_eq!(config.audio_buffer_chunks.get(), expected_ring);
    }

    #[kithara::test]
    #[case::one_frame_over_budget(64, 128, 254)]
    #[case::large_buffer_over_industry_budget(64, 512, 441)]
    fn invalid_frame_contract_is_rejected(
        #[case] quantum: usize,
        #[case] buffer: usize,
        #[case] budget: usize,
    ) {
        let mut config = frame_contract_config(quantum, buffer, budget);

        assert!(matches!(
            config.resolve_output_geometry(),
            Err(DecodeError::InvalidData {
                detail: "playback output buffer exceeds the configured response budget"
            })
        ));
    }

    #[kithara::test]
    fn config_bitrate_fields_default_zero() {
        let config = test_config("https://example.com/live.m3u8").unwrap();
        assert!((config.preferred_peak_bitrate - 0.0).abs() < f64::EPSILON);
    }

    #[kithara::test]
    fn config_bitrate_propagates_to_hls_abr() {
        let worker = worker();
        let config: ResourceConfig<TestPools> =
            ResourceConfig::for_src(valid_src("https://example.com/live.m3u8"))
                .store(store())
                .preferred_peak_bitrate(512_000.0)
                .build();
        let _audio_config = config.build_hls_config(&worker, None).unwrap();
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
