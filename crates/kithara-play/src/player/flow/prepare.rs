use std::num::{NonZeroU32, NonZeroUsize};

use kithara_audio::{AudioDecoderConfig, DecoderResamplerSettings, ResamplerOptions};
use kithara_bufpool::HasPool;
use kithara_platform::sync::Arc;

#[cfg(test)]
use super::super::core::PlayerImpl;
use super::super::core::PlayerRuntime;
use crate::{PlayError, resource::ResourceConfig, rt::StreamShape, session::SessionError};

fn playback_buffers(
    shape: StreamShape,
    quantum: NonZeroUsize,
    budget: NonZeroUsize,
) -> Result<(NonZeroUsize, NonZeroUsize), SessionError> {
    let output_frames = usize::try_from(shape.max_block_frames.get())
        .map_err(|_| SessionError::ResponseGeometryOverflow)?;
    let preload = output_frames.div_ceil(quantum.get());
    let ring = preload
        .checked_add(1)
        .ok_or(SessionError::ResponseGeometryOverflow)?;
    let required_frames = ring
        .checked_add(1)
        .and_then(|chunks| chunks.checked_mul(quantum.get()))
        .and_then(|frames| frames.checked_sub(1))
        .ok_or(SessionError::ResponseGeometryOverflow)?;
    if required_frames > budget.get() {
        return Err(SessionError::ResponseBudgetExceeded {
            required_frames,
            max_block_frames: shape.max_block_frames.get(),
            render_quantum_frames: quantum.get(),
            budget_frames: budget.get(),
        });
    }
    Ok((
        NonZeroUsize::new(preload).ok_or(SessionError::ResponseGeometryOverflow)?,
        NonZeroUsize::new(ring).ok_or(SessionError::ResponseGeometryOverflow)?,
    ))
}

struct ConfigPrep<'a, S> {
    player: &'a PlayerRuntime<S>,
}

impl<S> ConfigPrep<'_, S>
where
    S: HasPool<u8> + HasPool<f32> + Send + Sync + 'static,
{
    fn prepare<B>(&self, config: ResourceConfig<S, B>) -> Result<ResourceConfig<S, B>, PlayError>
    where
        B: Clone + Default,
    {
        let bus = config
            .bus
            .or_else(|| Some(self.player.core.engine.bus().scoped()));
        let cancel = config
            .cancel
            .or_else(|| self.player.core.engine.cancel_token())
            .map(|parent| parent.child());
        let warp = self.player.core.warp.clone();
        let host_sample_rate = NonZeroU32::new(self.player.core.engine.master_sample_rate())
            .or_else(|| NonZeroU32::new(self.player.core.engine.configured_sample_rate()));
        let stream_shape = self.player.core.engine.stream_shape()?;
        // A resident render quantum turns the two buffer depths into geometry
        // the response budget admits rather than a preference, so the computed
        // pair overwrites whatever the document said under `audio:`.
        let mut audio = config.audio;
        if let Some(quantum) = warp.render_quantum_frames() {
            let shape = stream_shape.ok_or(SessionError::NoContext)?;
            let (preload, ring) =
                playback_buffers(shape, quantum, self.player.core.response_budget_frames)?;
            audio.preload_chunks = Some(preload);
            audio.audio_buffer_chunks = Some(ring.get());
        }
        let resampler = match config.decoder.resampler().cloned() {
            Some(settings) => Some(settings),
            None => stream_shape
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
            .gapless_mode(self.player.core.gapless_mode)
            .maybe_resampler(resampler)
            .build();
        Ok(ResourceConfig {
            bus,
            cancel,
            worker: Some(self.player.core.worker.clone()),
            block_on_underrun: self.player.core.block_on_underrun,
            audio,
            host_sample_rate,
            decoder,
            warp,
            engine_load: Some(Arc::clone(&self.player.core.engine_load)),
            ..config
        })
    }
}

impl<S> PlayerRuntime<S>
where
    S: HasPool<u8> + HasPool<f32> + Send + Sync + 'static,
{
    /// Apply shared worker, host sample rate, ABR, and bus to a resource
    /// config so the resource integrates with this player's engine.
    ///
    /// Call this before [`Resource::new`](crate::resource::Resource::new) to
    /// ensure the resource shares the player's playback worker and resampler is
    /// pre-initialised with the correct ratio. Callers that want a shared HTTP
    /// pool / tokio runtime must build their own downloader and attach it via
    /// [`ResourceConfig::with_downloader`] before passing the config in.
    /// # Errors
    ///
    /// Returns an error when the bound session cannot report its output shape.
    pub fn prepare_config<B>(
        &self,
        config: ResourceConfig<S, B>,
    ) -> Result<ResourceConfig<S, B>, PlayError>
    where
        B: Clone + Default,
    {
        ConfigPrep { player: self }.prepare(config)
    }
}

#[cfg(test)]
mod tests {
    use kithara_assets::AssetStore;
    use kithara_test_utils::kithara;
    use kithara_warp::WarpConfig;

    use super::*;
    use crate::{
        PlayError, PlayWorker, PlayWorkerConfig, PlaybackResamplerBackend, mock,
        player::PlayerConfig,
        resource::ResourceSrc,
        test_pools::{TestPools, pools},
    };

    fn resource_config(source: &str) -> ResourceConfig<TestPools> {
        let pools = pools();
        let src = ResourceSrc::parse(source).expect("valid test source");
        ResourceConfig::for_src(src)
            .store(AssetStore::builder(pools).build())
            .build()
    }

    fn worker() -> PlayWorker<TestPools> {
        PlayWorker::new(PlayWorkerConfig::builder(pools()).build())
    }

    fn player_with_geometry(
        quantum: usize,
        output_buffer: u32,
        response_budget: usize,
    ) -> PlayerImpl<TestPools> {
        let shape = StreamShape::new(
            NonZeroU32::new(output_buffer).expect("fixture output block is non-zero"),
            mock::SAMPLE_RATE,
        );
        let warp = WarpConfig::builder()
            .render_quantum_frames(NonZeroUsize::new(quantum).expect("fixture quantum is non-zero"))
            .build();
        PlayerImpl::new(
            PlayerConfig::builder()
                .sample_rate(mock::SAMPLE_RATE)
                .worker(worker())
                .session(mock::session_with_shape(Some(shape)))
                .warp(warp)
                .response_budget_frames(
                    NonZeroUsize::new(response_budget).expect("fixture budget is non-zero"),
                )
                .build(),
        )
    }

    #[kithara::test]
    fn prepare_config_sizes_default_resampling_work_to_the_output_block() {
        let shape = StreamShape::new(
            NonZeroU32::new(128).expect("test block is non-zero"),
            mock::SAMPLE_RATE,
        );
        let player = PlayerImpl::new(
            PlayerConfig::builder()
                .sample_rate(mock::SAMPLE_RATE)
                .worker(worker())
                .session(mock::session_with_shape(Some(shape)))
                .build(),
        );

        let prepared = player
            .prepare_config(resource_config("https://example.com/song.mp3"))
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
        let player = PlayerImpl::new(
            PlayerConfig::builder()
                .sample_rate(mock::SAMPLE_RATE)
                .worker(worker())
                .build(),
        );

        let prepared = player
            .prepare_config(resource_config("https://example.com/song.mp3"))
            .expect("resources may be prepared before host insertion");

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
        let player = PlayerImpl::new(
            PlayerConfig::builder()
                .sample_rate(mock::SAMPLE_RATE)
                .worker(worker())
                .session(mock::session_with_shape(Some(shape)))
                .build(),
        );

        let prepared = player
            .prepare_config(config)
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
        let player = player_with_geometry(quantum, output_buffer, response_budget);

        let prepared = player
            .prepare_config(resource_config("https://example.com/song.mp3"))
            .expect("fixture geometry fits the response budget");

        assert_eq!(
            prepared.audio.preload_chunks.map(NonZeroUsize::get),
            Some(expected_preload)
        );
        assert_eq!(prepared.audio.audio_buffer_chunks, Some(expected_ring));
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
        let player = player_with_geometry(quantum, output_buffer, response_budget);

        assert!(matches!(
            player.prepare_config(resource_config("https://example.com/song.mp3")),
            Err(PlayError::Session(SessionError::ResponseBudgetExceeded {
                max_block_frames,
                render_quantum_frames,
                required_frames: actual_required_frames,
                budget_frames,
            })) if max_block_frames == output_buffer
                && render_quantum_frames == quantum
                && actual_required_frames == required_frames
                && budget_frames == response_budget
        ));
    }
}
