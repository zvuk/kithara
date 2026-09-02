use std::num::{NonZeroU32, NonZeroUsize};

use kithara_audio::{AudioDecoderConfig, DecoderResamplerSettings, ResamplerOptions};
use kithara_bufpool::HasPool;
use kithara_platform::sync::Arc;

#[cfg(test)]
use super::super::core::PlayerImpl;
use super::super::core::PlayerRuntime;
use crate::{PlayError, resource::ResourceConfig};

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
        let output_buffer_frames =
            NonZeroUsize::try_from(self.player.core.engine.stream_shape()?.max_block_frames)
                .map_err(|_| PlayError::Internal("session output block exceeds usize".into()))?;
        let resampler = config.decoder.resampler().cloned().unwrap_or_else(|| {
            DecoderResamplerSettings::builder()
                .backend(B::default())
                .options(
                    ResamplerOptions::builder()
                        .chunk_size(output_buffer_frames.get())
                        .build(),
                )
                .build()
        });
        let decoder = AudioDecoderConfig::builder()
            .backend(config.decoder.backend())
            .gapless_mode(self.player.core.gapless_mode)
            .resampler(resampler)
            .build();
        // Web Warp is an identity stage and does not emit bounded render quanta;
        // keep the browser's chunk-based preload and ring geometry.
        Ok(ResourceConfig {
            bus,
            cancel,
            worker: Some(self.player.core.worker.clone()),
            output_buffer_frames: (!cfg!(target_arch = "wasm32")).then_some(output_buffer_frames),
            response_budget_frames: (!cfg!(target_arch = "wasm32"))
                .then_some(self.player.core.response_budget_frames),
            consumer_wake_mode: Some(self.player.core.engine.consumer_wake_mode()),
            block_on_underrun: self.player.core.block_on_underrun,
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
    /// Returns an error when the player has no Host session or its stream
    /// geometry cannot be queried.
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
    use kithara_audio::ConsumerWakeMode;
    use kithara_platform::sync::Arc;
    use kithara_test_utils::kithara;

    use super::*;
    use crate::{
        PlayWorker, PlayWorkerConfig,
        player::PlayerConfig,
        resource::ResourceSrc,
        session::{Cmd, Reply, SessionDispatcher, testing},
        test_pools::{TestPools, pools},
    };

    struct ImmediateSession(Arc<dyn SessionDispatcher<TestPools>>);

    impl SessionDispatcher<TestPools> for ImmediateSession {
        fn exec(&self, cmd: Cmd<TestPools>) -> Result<Reply, PlayError> {
            self.0.exec(cmd)
        }

        fn consumer_wake_mode(&self) -> ConsumerWakeMode {
            ConsumerWakeMode::ImmediateOffRt
        }
    }

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

    #[kithara::test]
    fn prepare_config_propagates_session_consumer_wake_mode_to_audio() {
        let session: Arc<dyn SessionDispatcher<TestPools>> =
            Arc::new(ImmediateSession(testing::test_session()));
        let player = PlayerImpl::new(
            PlayerConfig::builder()
                .worker(worker())
                .session(session)
                .build(),
        );

        let prepared = player
            .prepare_config(resource_config("https://example.com/song.mp3"))
            .expect("bound player reads its session stream shape");
        assert_eq!(
            prepared.consumer_wake_mode,
            Some(ConsumerWakeMode::ImmediateOffRt)
        );
        let audio = prepared.build_file_config(player.worker(), None);
        assert_eq!(audio.consumer_wake_mode(), ConsumerWakeMode::ImmediateOffRt);

        let prepared = player
            .prepare_config(resource_config("https://example.com/live.m3u8"))
            .expect("bound player reads its session stream shape");
        let audio = prepared
            .build_hls_config(player.worker(), None)
            .expect("valid HLS config");
        assert_eq!(audio.consumer_wake_mode(), ConsumerWakeMode::ImmediateOffRt);
    }

    #[kithara::test]
    fn prepare_config_sizes_default_resampling_work_to_the_output_block() {
        let player = PlayerImpl::new(
            PlayerConfig::builder()
                .worker(worker())
                .session(testing::test_session())
                .build(),
        );

        let prepared = player
            .prepare_config(resource_config("https://example.com/song.mp3"))
            .expect("bound player reads its session stream shape");

        assert_eq!(
            prepared
                .decoder
                .resampler()
                .expect("player installs decoder resampling settings")
                .options()
                .chunk_size,
            128,
        );
    }
}
