use std::num::{NonZeroU32, NonZeroUsize};

use kithara_audio::{
    AudioDecoderConfig, AudioObserver, DecoderResamplerSettings, ResamplerBackend, ResamplerOptions,
};
use kithara_bufpool::HasPool;
use kithara_config::bon::Builder;
use kithara_decode::GaplessMode;
use kithara_derive::Patch;
use kithara_events::EventBus;
use kithara_platform::{CancelScope, CancelToken, sync::Arc};
use kithara_render::{TrackConfig, TrackConfigPatch, rt::BufferGeometryError};
use kithara_warp::{WarpConfig, WarpConfigPatch};

use crate::{
    EngineLoad, OutputSnapshot, PlayError, PlayWorker, ResourceLoad, resource::ResourceConfig,
};

/// What every track a deck loads opens with: the worker it renders on, the
/// deck's own playback policy.
///
/// The deck holds one and prepares each track's config with it just before
/// the track loads, using the output snapshot lent by that owner pass.
#[derive(Builder, Patch)]
#[builder(crate = ::kithara_config::bon)]
#[derive_where::derive_where(Clone)]
pub struct ResourcePrep<S> {
    #[patch(skip)]
    pub worker: PlayWorker<S>,
    #[builder(default)]
    #[patch(skip)]
    pub bus: EventBus,
    #[patch(skip)]
    pub cancel: Option<CancelToken>,
    /// The renderer every track starts from; a track starts it at its own
    /// settings.
    #[builder(default = WarpConfig::builder().build())]
    #[patch(skip)]
    pub warp: WarpConfig,
    #[patch(skip)]
    pub response_budget_frames: Option<NonZeroUsize>,
    #[builder(default)]
    pub gapless_mode: GaplessMode,
    #[builder(default)]
    #[patch(skip)]
    pub block_on_underrun: bool,
    #[builder(default)]
    #[patch(skip)]
    pub engine_load: Arc<EngineLoad>,
}

impl<S> ResourcePrep<S>
where
    S: HasPool<u8> + HasPool<f32> + Send + Sync + 'static,
{
    /// Binds source inputs to the deck's worker and playback policy.
    /// The returned inputs share the load's cancellation scope and event bus
    /// so artifacts and owner observations follow the same track lifetime.
    ///
    /// # Errors
    /// Returns the session output's refusal of the buffer geometry.
    pub fn prepare<B>(
        &self,
        mut config: ResourceConfig<S, B>,
        output: &OutputSnapshot,
        observer: Box<dyn AudioObserver>,
    ) -> Result<(ResourceConfig<S, B>, ResourceLoad), PlayError>
    where
        B: Clone + Default + ResamplerBackend,
    {
        let bus = config.bus.take().or_else(|| Some(self.bus.scoped()));
        let cancel =
            CancelScope::new(config.cancel.clone().or_else(|| self.cancel.clone())).token();
        let cancel_link = self.cancel.as_ref().map(|parent| {
            let track = cancel.clone();
            parent.on_cancel(move || track.cancel())
        });
        let track = self.track_config(&mut config, output)?;
        let resampler = match config.decoder.resampler().cloned() {
            Some(settings) => Some(settings),
            None => output
                .stream_shape
                .map(|shape| {
                    let chunk_size =
                        usize::try_from(shape.max_block_frames.get()).map_err(|_| {
                            PlayError::Internal("session output block exceeds usize".into())
                        })?;
                    Ok::<_, PlayError>(
                        DecoderResamplerSettings::builder()
                            .backend(B::default())
                            .options(ResamplerOptions::builder().chunk_size(chunk_size).build())
                            .build(),
                    )
                })
                .transpose()?,
        };
        config.decoder = AudioDecoderConfig::builder()
            .backend(config.decoder.backend())
            .gapless_mode(self.gapless_mode)
            .maybe_resampler(resampler)
            .build();
        config.bus = bus;
        config.cancel = Some(cancel);
        config.host_sample_rate = NonZeroU32::new(output.sample_rate.output());
        let load = ResourceLoad::prepared(
            config.clone(),
            track,
            self.worker.clone(),
            observer,
            cancel_link,
        );
        Ok((config, load))
    }

    fn track_config<B: Default>(
        &self,
        config: &mut ResourceConfig<S, B>,
        output: &OutputSnapshot,
    ) -> Result<TrackConfig, PlayError> {
        let mut warp = self.warp.clone();
        if warp.render_quantum_frames().is_none()
            && let Some(shape) = output.stream_shape
        {
            let quantum = NonZeroUsize::try_from(shape.max_block_frames)
                .map_err(|_| BufferGeometryError::Overflow)?;
            let mut patch = WarpConfigPatch::default();
            patch.render_quantum_frames = Some(quantum);
            warp.apply(patch)
                .map_err(|error| PlayError::Internal(error.to_string()))?;
        }
        let derived = match (warp.render_quantum_frames(), output.stream_shape) {
            (Some(quantum), Some(shape)) => {
                Some(shape.playback_buffers(quantum, self.response_budget_frames)?)
            }
            _ => None,
        };
        let (preload_chunks, audio_buffer_chunks) = match (derived, self.response_budget_frames) {
            (Some((preload, ring)), Some(_)) => (Some(preload), Some(ring)),
            (Some((preload, ring)), None) => (
                config.preload_chunks.or(Some(preload)),
                config.audio_buffer_chunks.or(Some(ring)),
            ),
            (None, _) => (config.preload_chunks, config.audio_buffer_chunks),
        };
        config.preload_chunks = preload_chunks;
        config.audio_buffer_chunks = audio_buffer_chunks;
        let mut track = TrackConfig::builder()
            .block_on_underrun(self.block_on_underrun)
            .engine_load(Arc::clone(&self.engine_load))
            .warp(warp)
            .build();
        let mut patch = TrackConfigPatch::default();
        patch.preload_chunks = preload_chunks;
        patch.audio_buffer_chunks = audio_buffer_chunks;
        track.apply(patch);
        Ok(track)
    }
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroUsize;

    use kithara_assets::AssetStore;
    use kithara_audio::AudioObserverSlot;
    use kithara_render::rt::{BufferGeometryError, StreamShape};
    use kithara_test_utils::{TestTempDir, kithara};
    use kithara_warp::WarpConfig;

    use super::*;
    use crate::{
        PlayWorkerConfig, PlaybackResamplerBackend, mock,
        resource::ResourceSrc,
        session::SessionError,
        test_pools::{TestPools, pools},
    };

    fn resource_config(source: &str) -> ResourceConfig<TestPools> {
        let pools = pools();
        let src = ResourceSrc::parse(source).expect("valid test source");
        ResourceConfig::for_src(src)
            .store(AssetStore::builder(pools).build())
            .build()
    }

    fn prep(warp: WarpConfig) -> ResourcePrep<TestPools> {
        ResourcePrep::builder()
            .worker(PlayWorker::new(PlayWorkerConfig::builder(pools()).build()))
            .bus(EventBus::new(16))
            .warp(warp)
            .build()
    }

    #[kithara::test]
    fn preparation_stretch_defaults_to_unity() {
        let prep = ResourcePrep::builder()
            .worker(PlayWorker::new(PlayWorkerConfig::builder(pools()).build()))
            .build();
        assert!((prep.warp.speed() - 1.0).abs() < f32::EPSILON);
    }

    #[kithara::test]
    fn default_response_budget_leaves_the_deadline_to_the_application() {
        let config = prep(WarpConfig::builder().build());

        assert_eq!(config.response_budget_frames, None);
        assert!(!config.block_on_underrun);
        let mut source = resource_config("https://example.com/song.mp3");
        let prepared = config
            .track_config(&mut source, &mock::output(None).get())
            .expect("default playback policy prepares the resource");
        assert!(!prepared.block_on_underrun());
    }

    #[kithara::test]
    fn prepare_config_applies_player_gapless_mode() {
        let prep = ResourcePrep {
            gapless_mode: GaplessMode::Disabled,
            ..prep(WarpConfig::builder().build())
        };
        let (config, load) = prep
            .prepare(
                resource_config("https://example.com/song.mp3"),
                &mock::output(None).get(),
                Box::new(AudioObserverSlot::default().relay()),
            )
            .expect("test output answers stream-shape queries");

        assert_eq!(config.decoder.gapless_mode(), GaplessMode::Disabled);
        assert!(
            config.cancel.is_some(),
            "prepare_config must inject a per-track cancel child"
        );
        assert!(load.lane_geometry().is_ok());
    }

    #[kithara::test(native)]
    fn a_document_named_gapless_mode_reaches_the_prepared_decoder() {
        let patch: ResourcePrepPatch = serde_yaml_ng::from_str("gapless_mode:\n  mode: disabled\n")
            .expect("the document types");
        let mut prep = prep(WarpConfig::builder().build());
        prep.apply(patch);
        let (config, _load) = prep
            .prepare(
                resource_config("https://example.com/song.mp3"),
                &mock::output(None).get(),
                Box::new(AudioObserverSlot::default().relay()),
            )
            .expect("test output answers stream-shape queries");

        assert_eq!(config.decoder.gapless_mode(), GaplessMode::Disabled);

        let error = serde_yaml_ng::from_str::<ResourcePrepPatch>("block_on_underrun: true\n")
            .expect_err("a document cannot enable blocking audio reads");
        assert!(error.to_string().contains("block_on_underrun"), "{error}");
    }

    #[kithara::test(native)]
    fn a_gapless_mode_patch_reaches_the_player() {
        let patch: ResourcePrepPatch = serde_yaml_ng::from_str("gapless_mode:\n  mode: disabled\n")
            .expect("the document types");
        let mut prep = prep(WarpConfig::builder().build());
        prep.warp = WarpConfig::builder().speed(2.5).build();
        prep.apply(patch);
        let (mut config, _load) = prep
            .prepare(
                resource_config("https://example.com/song.mp3"),
                &mock::output(None).get(),
                Box::new(AudioObserverSlot::default().relay()),
            )
            .expect("test output answers stream-shape queries");

        assert_eq!(prep.gapless_mode, GaplessMode::Disabled);
        assert_eq!(config.decoder.gapless_mode(), GaplessMode::Disabled);
        assert!(
            (prep.warp.speed() - 2.5).abs() < f32::EPSILON,
            "a sibling field must survive the patch"
        );
        assert!(
            (prep
                .track_config(&mut config, &mock::output(None).get())
                .expect("producer config")
                .warp()
                .speed()
                - 2.5)
                .abs()
                < f32::EPSILON
        );
    }

    fn prep_with_geometry(
        quantum: usize,
        output_buffer: u32,
        response_budget: usize,
    ) -> (ResourcePrep<TestPools>, OutputSnapshot) {
        let shape = StreamShape::new(
            NonZeroU32::new(output_buffer).expect("fixture output block is non-zero"),
            mock::SAMPLE_RATE,
        );
        let warp = WarpConfig::builder()
            .render_quantum_frames(NonZeroUsize::new(quantum).expect("fixture quantum is non-zero"))
            .build();
        let prep = ResourcePrep {
            response_budget_frames: Some(
                NonZeroUsize::new(response_budget).expect("fixture budget is non-zero"),
            ),
            ..prep(warp)
        };
        (prep, mock::output(Some(shape)).get())
    }

    #[kithara::test]
    fn prepare_config_sizes_default_resampling_work_to_the_output_block() {
        let shape = StreamShape::new(
            NonZeroU32::new(128).expect("test block is non-zero"),
            mock::SAMPLE_RATE,
        );
        let (prepared, _load) = prep(WarpConfig::builder().build())
            .prepare(
                resource_config("https://example.com/song.mp3"),
                &mock::output(Some(shape)).get(),
                Box::new(AudioObserverSlot::default().relay()),
            )
            .expect("test session answers stream-shape queries");

        assert_eq!(
            prepared
                .decoder
                .resampler()
                .expect("known output shape installs decoder resampling settings")
                .options()
                .chunk_size,
            128
        );
    }

    #[kithara::test]
    fn prepare_config_without_a_session_keeps_default_resampling_work() {
        let (prepared, _load) = prep(WarpConfig::builder().build())
            .prepare(
                resource_config("https://example.com/song.mp3"),
                &mock::output(None).get(),
                Box::new(AudioObserverSlot::default().relay()),
            )
            .expect("resources may be prepared before the session measures its output");

        assert!(prepared.decoder.resampler().is_none());
    }

    #[kithara::test]
    #[case::default(None, None)]
    #[case::explicit(Some(64), Some(64))]
    fn unbound_preparation_preserves_audio_settings_and_resolves_player_quantum(
        #[case] configured: Option<usize>,
        #[case] expected: Option<usize>,
    ) {
        let prep = prep(
            WarpConfig::builder()
                .maybe_render_quantum_frames(configured.and_then(NonZeroUsize::new))
                .build(),
        );
        let mut config = resource_config("https://example.com/song.mp3");
        config.preload_chunks = NonZeroUsize::new(7);
        config.audio_buffer_chunks = NonZeroUsize::new(11);
        let (mut prepared, _load) = prep
            .prepare(
                config,
                &mock::output(None).get(),
                Box::new(AudioObserverSlot::default().relay()),
            )
            .expect("unmeasured preparation");
        assert_eq!(
            prep.track_config(&mut prepared, &mock::output(None).get())
                .expect("producer config")
                .warp()
                .render_quantum_frames()
                .map(NonZeroUsize::get),
            expected
        );
        assert_eq!(prepared.preload_chunks.map(NonZeroUsize::get), Some(7));
        assert_eq!(
            prepared.audio_buffer_chunks.map(NonZeroUsize::get),
            Some(11)
        );
        assert!(prepared.decoder.resampler().is_none());
    }

    #[kithara::test]
    fn prepare_config_preserves_explicit_resampling_work() {
        let explicit = DecoderResamplerSettings::builder()
            .backend(PlaybackResamplerBackend::default())
            .options(ResamplerOptions::builder().chunk_size(256).build())
            .build();
        let mut config = resource_config("https://example.com/song.mp3");
        config.decoder = AudioDecoderConfig::builder().resampler(explicit).build();
        let shape = StreamShape::new(
            NonZeroU32::new(128).expect("test block is non-zero"),
            mock::SAMPLE_RATE,
        );

        let (prepared, _load) = prep(WarpConfig::builder().build())
            .prepare(
                config,
                &mock::output(Some(shape)).get(),
                Box::new(AudioObserverSlot::default().relay()),
            )
            .expect("test session answers stream-shape queries");

        assert_eq!(
            prepared
                .decoder
                .resampler()
                .expect("explicit resampling settings remain installed")
                .options()
                .chunk_size,
            256
        );
    }

    #[kithara::test(native, tokio)]
    async fn prepare_config_carries_the_sessions_rate_and_wake_mode() {
        let prep = prep(WarpConfig::builder().build());
        let (prepared, _load) = prep
            .prepare(
                resource_config("https://example.com/song.mp3"),
                &mock::output(None).get(),
                Box::new(AudioObserverSlot::default().relay()),
            )
            .expect("unmeasured preparation");

        assert_eq!(
            prepared.host_sample_rate.map(NonZeroU32::get),
            Some(mock::SAMPLE_RATE.get())
        );
        let dir = TestTempDir::new();
        mock::assert_prepared_render_off_bus(
            &prep,
            &mock::output(None).get(),
            &pools(),
            &dir.path().join("prepared.wav"),
        )
        .await
        .expect("player-prepared lane renders off the bus");
    }

    #[kithara::test]
    #[case::industry_budget(32, 128, 441, 4, 5)]
    #[case::large_continuity_buffer(64, 512, 639, 8, 9)]
    fn prepare_config_derives_playback_buffering(
        #[case] quantum: usize,
        #[case] output_buffer: u32,
        #[case] response_budget: usize,
        #[case] expected_preload: usize,
        #[case] expected_ring: usize,
    ) {
        let (prep, output) = prep_with_geometry(quantum, output_buffer, response_budget);
        let (prepared, _load) = prep
            .prepare(
                resource_config("https://example.com/song.mp3"),
                &output,
                Box::new(AudioObserverSlot::default().relay()),
            )
            .expect("fixture geometry fits the response budget");

        assert_eq!(
            prepared.preload_chunks.map(NonZeroUsize::get),
            Some(expected_preload)
        );
        assert_eq!(
            prepared.audio_buffer_chunks.map(NonZeroUsize::get),
            Some(expected_ring)
        );
    }

    #[kithara::test]
    fn a_measured_output_block_is_the_default_render_quantum() {
        let shape = StreamShape::new(
            NonZeroU32::new(512).expect("test block is non-zero"),
            mock::SAMPLE_RATE,
        );
        let mut config = resource_config("https://example.com/song.mp3");
        config.audio_buffer_chunks = NonZeroUsize::new(1);
        let prep = prep(WarpConfig::builder().build());
        let output = mock::output(Some(shape)).get();
        let (mut prepared, _load) = prep
            .prepare(
                config,
                &output,
                Box::new(AudioObserverSlot::default().relay()),
            )
            .expect("a measured output prepares the resource");

        assert_eq!(
            prep.track_config(&mut prepared, &output)
                .expect("producer config")
                .warp()
                .render_quantum_frames()
                .map(NonZeroUsize::get),
            Some(512)
        );
        assert_eq!(
            prepared.audio_buffer_chunks.map(NonZeroUsize::get),
            Some(1),
            "without a response budget the track's ring depth stands"
        );
        assert!(prepared.preload_chunks.is_some());
    }

    #[kithara::test]
    fn response_budget_bounds_the_default_render_quantum() {
        let (mut prep, output) = prep_with_geometry(32, 128, 441);
        prep.warp = WarpConfig::builder().build();
        let (mut prepared, _load) = prep
            .prepare(
                resource_config("https://example.com/song.mp3"),
                &output,
                Box::new(AudioObserverSlot::default().relay()),
            )
            .expect("the session block fits the response budget");

        assert_eq!(
            prep.track_config(&mut prepared, &output)
                .expect("producer config")
                .warp()
                .render_quantum_frames()
                .map(NonZeroUsize::get),
            Some(128)
        );
        assert_eq!(prepared.preload_chunks.map(NonZeroUsize::get), Some(1));
        assert_eq!(prepared.audio_buffer_chunks.map(NonZeroUsize::get), Some(2));
        assert_eq!(prep.warp.render_quantum_frames(), None);

        prep.response_budget_frames = NonZeroUsize::new(382);
        assert!(matches!(
            prep.prepare(
                resource_config("https://example.com/song.mp3"),
                &output,
                Box::new(AudioObserverSlot::default().relay())
            ),
            Err(PlayError::Session(SessionError::BufferGeometry(
                BufferGeometryError::BudgetExceeded {
                    required_frames: 383,
                    budget_frames: 382,
                    ..
                }
            )))
        ));
    }

    #[kithara::test]
    #[case::one_frame_over_budget(64, 128, 254, 255)]
    #[case::large_buffer_over_industry_budget(64, 512, 441, 639)]
    fn prepare_config_rejects_buffering_over_budget(
        #[case] quantum: usize,
        #[case] output_buffer: u32,
        #[case] response_budget: usize,
        #[case] required_frames: usize,
    ) {
        let (prep, output) = prep_with_geometry(quantum, output_buffer, response_budget);

        assert!(matches!(
            prep.prepare(resource_config("https://example.com/song.mp3"), &output, Box::new(AudioObserverSlot::default().relay())),
            Err(PlayError::Session(SessionError::BufferGeometry(
                BufferGeometryError::BudgetExceeded {
                    max_block_frames,
                    render_quantum_frames,
                    required_frames: actual_required_frames,
                    budget_frames,
                }
            ))) if max_block_frames == output_buffer
                && render_quantum_frames == quantum
                && actual_required_frames == required_frames
                && budget_frames == response_budget
        ));
    }
    #[kithara::test(tokio)]
    #[case::hls("https://example.com/live.m3u8")]
    #[case::file("https://example.com/song.mp3")]
    async fn preload_count_reaches_producer_config_without_opening_the_source(
        #[case] source: &str,
    ) {
        let mut config = resource_config(source);
        config.preload_chunks = NonZeroUsize::new(9);

        let built = prep(WarpConfig::builder().build())
            .track_config(&mut config, &mock::output(None).get())
            .expect("producer geometry");

        assert_eq!(built.preload_chunks().get(), 9);
    }

    #[kithara::test(tokio)]
    async fn ring_depth_reaches_producer_config_without_opening_the_source() {
        let mut config = resource_config("https://example.com/live.m3u8");
        config.audio_buffer_chunks = NonZeroUsize::new(24);

        let built = prep(WarpConfig::builder().build())
            .track_config(&mut config, &mock::output(None).get())
            .expect("producer geometry");

        assert_eq!(built.audio_buffer_chunks().get(), 24);
    }

    #[kithara::test(tokio)]
    #[case::hls("https://example.com/live.m3u8")]
    #[case::file("https://example.com/song.mp3")]
    async fn underrun_policy_reaches_producer_config_without_opening_the_source(
        #[case] source: &str,
    ) {
        let mut config = resource_config(source);
        let prep = ResourcePrep {
            block_on_underrun: true,
            ..prep(WarpConfig::builder().build())
        };
        let built = prep
            .track_config(&mut config, &mock::output(None).get())
            .expect("producer geometry");

        assert!(built.block_on_underrun());
    }
}
