use std::num::{NonZeroU32, NonZeroUsize};

use kithara_audio::{AudioDecoderConfig, DecoderResamplerSettings, ResamplerOptions};
use kithara_bufpool::HasPool;
use kithara_config::bon::Builder;
use kithara_decode::GaplessMode;
use kithara_derive::Patch;
use kithara_events::EventBus;
use kithara_platform::{CancelScope, CancelToken, sync::Arc};
use kithara_warp::WarpConfig;

use crate::{EngineLoad, OutputSnapshot, PlayError, PlayWorker, resource::ResourceConfig};

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
    /// Prepares `config` to play on the deck's worker into its session.
    /// Before the session measures its output no deadline can be checked, so
    /// buffer depths sized to the render quantum and response budget overwrite
    /// whatever `audio:` configured only once the output shape is known.
    ///
    /// # Errors
    ///
    /// Returns the session output's refusal of the buffer geometry.
    pub fn prepare<B>(
        &self,
        config: ResourceConfig<S, B>,
        output: &OutputSnapshot,
    ) -> Result<ResourceConfig<S, B>, PlayError>
    where
        B: Clone + Default,
    {
        let bus = config.bus.or_else(|| Some(self.bus.scoped()));
        let cancel =
            CancelScope::new(config.cancel.clone().or_else(|| self.cancel.clone())).token();
        let cancel_link = self.cancel.as_ref().map(|parent| {
            let track = cancel.clone();
            Arc::new(parent.on_cancel(move || track.cancel()))
        });
        let (preload_chunks, audio_buffer_chunks) =
            match (self.warp.render_quantum_frames(), output.stream_shape) {
                (Some(quantum), Some(shape)) => {
                    let (preload, ring) =
                        shape.playback_buffers(quantum, self.response_budget_frames)?;
                    (Some(preload), Some(ring))
                }
                _ => (config.preload_chunks, config.audio_buffer_chunks),
            };
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
        let decoder = AudioDecoderConfig::builder()
            .backend(config.decoder.backend())
            .gapless_mode(self.gapless_mode)
            .maybe_resampler(resampler)
            .build();
        Ok(ResourceConfig {
            bus,
            cancel: Some(cancel),
            cancel_link,
            worker: Some(self.worker.clone()),
            block_on_underrun: self.block_on_underrun,
            preload_chunks,
            audio_buffer_chunks,
            host_sample_rate: NonZeroU32::new(output.sample_rate.output()),
            decoder,
            warp: self.warp.clone(),
            engine_load: Some(Arc::clone(&self.engine_load)),
            ..config
        })
    }
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroUsize;

    use kithara_assets::AssetStore;
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
    fn default_response_budget_leaves_the_deadline_to_the_application() {
        let config = prep(WarpConfig::builder().build());

        assert_eq!(config.response_budget_frames, None);
    }

    #[kithara::test]
    fn prepare_config_applies_player_gapless_mode() {
        let prep = ResourcePrep {
            gapless_mode: GaplessMode::Disabled,
            ..prep(WarpConfig::builder().build())
        };
        let config = prep
            .prepare(
                resource_config("https://example.com/song.mp3"),
                &mock::output(None).get(),
            )
            .expect("test output answers stream-shape queries");

        assert_eq!(config.decoder.gapless_mode(), GaplessMode::Disabled);
        assert!(
            config.cancel.is_some(),
            "prepare_config must inject a per-track cancel child"
        );
    }

    #[kithara::test(native)]
    fn a_document_named_gapless_mode_reaches_the_prepared_decoder() {
        let patch: ResourcePrepPatch = serde_yaml_ng::from_str("gapless_mode:\n  mode: disabled\n")
            .expect("the document types");
        let mut prep = prep(WarpConfig::builder().build());
        prep.apply(patch);
        let config = prep
            .prepare(
                resource_config("https://example.com/song.mp3"),
                &mock::output(None).get(),
            )
            .expect("test output answers stream-shape queries");

        assert_eq!(config.decoder.gapless_mode(), GaplessMode::Disabled);
    }

    #[kithara::test(native)]
    fn a_gapless_mode_patch_reaches_the_player() {
        let patch: ResourcePrepPatch = serde_yaml_ng::from_str("gapless_mode:\n  mode: disabled\n")
            .expect("the document types");
        let mut prep = prep(WarpConfig::builder().build());
        prep.warp = WarpConfig::builder().speed(2.5).build();
        prep.apply(patch);
        let config = prep
            .prepare(
                resource_config("https://example.com/song.mp3"),
                &mock::output(None).get(),
            )
            .expect("test output answers stream-shape queries");

        assert_eq!(prep.gapless_mode, GaplessMode::Disabled);
        assert_eq!(config.decoder.gapless_mode(), GaplessMode::Disabled);
        assert!(
            (prep.warp.speed() - 2.5).abs() < f32::EPSILON,
            "a sibling field must survive the patch"
        );
        assert!((config.warp.speed() - 2.5).abs() < f32::EPSILON);
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
        let prepared = prep(WarpConfig::builder().build())
            .prepare(
                resource_config("https://example.com/song.mp3"),
                &mock::output(Some(shape)).get(),
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
        let prepared = prep(WarpConfig::builder().build())
            .prepare(
                resource_config("https://example.com/song.mp3"),
                &mock::output(None).get(),
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
        let prepared = prep
            .prepare(config, &mock::output(None).get())
            .expect("unmeasured preparation");
        assert_eq!(
            prepared.warp.render_quantum_frames().map(NonZeroUsize::get),
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

        let prepared = prep(WarpConfig::builder().build())
            .prepare(config, &mock::output(Some(shape)).get())
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
        let prepared = prep
            .prepare(
                resource_config("https://example.com/song.mp3"),
                &mock::output(None).get(),
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
        let prepared = prep
            .prepare(resource_config("https://example.com/song.mp3"), &output)
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
            prep.prepare(resource_config("https://example.com/song.mp3"), &output),
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
}
