use std::num::{NonZeroU32, NonZeroUsize};

use kithara_bufpool::{HasPool, PoolRegion};
use kithara_output::{
    OfflineRenderError, OfflineRenderReport, OfflineRenderRequest, OfflineRenderer, RenderSink,
};
use kithara_platform::{CancelToken, sync::Arc, time::Duration};
use kithara_play::{GroupState, PlayError, player::PlayerMember};
use kithara_signal::AudioSpec;
use kithara_worker::{DispatcherConfig, TaskConfig, Worker, WorkerConfig};

use super::{Host, HostConfig};
use crate::session::{
    HostDispatcher, RootView,
    offline::{OfflineSessionClient, OfflineTaskConfig},
};

struct Defaults;

impl Defaults {
    const BLOCK_FRAMES: NonZeroU32 = match NonZeroU32::new(512) {
        Some(value) => value,
        None => unreachable!(),
    };
    const RENDER_FRAMES: NonZeroU32 = match NonZeroU32::new(128) {
        Some(value) => value,
        None => unreachable!(),
    };
    const CHANNELS: u16 = 2;
    const SAMPLE_RATE: NonZeroU32 = match NonZeroU32::new(44_100) {
        Some(value) => value,
        None => unreachable!(),
    };
}

fn default_dispatcher_config() -> DispatcherConfig {
    DispatcherConfig::builder()
        .name("kithara-engine-offline")
        .capacity(NonZeroUsize::MIN)
        .build()
}

#[bon::bon]
impl<S> HostConfig<S> {
    /// Maximum frames processed by one backend/task quantum.
    #[must_use]
    pub const fn max_block_frames(&self) -> Option<NonZeroU32> {
        match self {
            Self::Offline {
                max_block_frames, ..
            } => Some(*max_block_frames),
            Self::Realtime { .. } => None,
        }
    }

    /// Configure a device-free offline session.
    #[builder(finish_fn = build)]
    pub fn offline(
        #[builder(start_fn)] pools: PoolRegion<S>,
        #[builder(default = Defaults::SAMPLE_RATE)] sample_rate: NonZeroU32,
        #[builder(default = Defaults::RENDER_FRAMES)] max_block_frames: NonZeroU32,
        #[builder(default = Defaults::BLOCK_FRAMES)] declick_frames: NonZeroU32,
        #[builder(default = Duration::ZERO)] declared_latency: Duration,
        #[builder(default = WorkerConfig::new())] worker: WorkerConfig,
        #[builder(default = default_dispatcher_config())] dispatcher: DispatcherConfig,
        #[builder(default = TaskConfig::new())] task: TaskConfig,
    ) -> Self {
        Self::Offline {
            pools,
            sample_rate,
            max_block_frames,
            declick_frames,
            declared_latency,
            worker,
            task,
            dispatcher: Box::new(dispatcher),
        }
    }
}

pub(super) struct OfflineRuntime<S> {
    client: Arc<OfflineSessionClient<S>>,
    spec: AudioSpec,
    _dispatcher: kithara_worker::Dispatcher,
    max_block_frames: NonZeroU32,
    _task: kithara_worker::TaskHandle,
    _worker: Worker,
}

type StartedOfflineRuntime<S> = (Arc<dyn HostDispatcher<S>>, OfflineRuntime<S>);

impl<S> OfflineRuntime<S>
where
    S: HasPool<f32> + Send + Sync + 'static,
{
    pub(super) fn new(
        config: HostConfig<S>,
        root: GroupState<PlayerMember>,
        root_view: RootView,
    ) -> Result<StartedOfflineRuntime<S>, PlayError> {
        let HostConfig::Offline {
            pools,
            sample_rate,
            max_block_frames,
            declick_frames,
            declared_latency,
            worker,
            dispatcher,
            task,
        } = config
        else {
            unreachable!("offline runtime requires offline Host config");
        };
        let spec = AudioSpec::new(Defaults::CHANNELS, sample_rate);
        let worker = Worker::new(worker);
        let dispatcher = worker.dispatcher(*dispatcher);
        let (client, task_handle) = crate::session::offline::spawn(
            &dispatcher,
            task,
            root,
            root_view,
            OfflineTaskConfig {
                pools,
                sample_rate,
                max_block_frames,
                declick_frames,
                declared_latency,
            },
        )?;
        let host_dispatcher: Arc<dyn HostDispatcher<S>> = client.clone();
        Ok((
            host_dispatcher,
            Self {
                client,
                max_block_frames,
                spec,
                _worker: worker,
                _dispatcher: dispatcher,
                _task: task_handle,
            },
        ))
    }

    fn position(&self) -> Result<u64, OfflineRenderError> {
        self.client.position().map_err(OfflineRenderError::backend)
    }

    fn render(
        &mut self,
        request: &OfflineRenderRequest,
        cancel: &CancelToken,
        sink: &mut dyn RenderSink,
    ) -> Result<OfflineRenderReport, OfflineRenderError> {
        let requested_frames = request.frame_count()?;
        if request.spec() != self.spec {
            return Err(OfflineRenderError::SpecMismatch {
                expected: self.spec,
                actual: request.spec(),
            });
        }
        let mut position = self.position()?;
        if request.frames().start < position {
            return Err(OfflineRenderError::RangeUnavailable {
                requested: request.frames().start,
                current: position,
            });
        }

        while position < request.frames().start {
            if cancel.is_cancelled() {
                return Err(OfflineRenderError::Cancelled { rendered_frames: 0 });
            }
            let remaining = request.frames().start - position;
            let frames = remaining.min(u64::from(self.max_block_frames.get()));
            let frames = u32::try_from(frames).map_err(OfflineRenderError::backend)?;
            let _ = self.render_at(position, frames)?;
            position = position
                .checked_add(u64::from(frames))
                .ok_or_else(|| OfflineRenderError::backend(TimelineOverflow))?;
        }

        let mut rendered_frames = 0;
        while position < request.frames().end {
            if cancel.is_cancelled() {
                return Err(OfflineRenderError::Cancelled { rendered_frames });
            }
            let remaining = request.frames().end - position;
            let frames = remaining.min(u64::from(self.max_block_frames.get()));
            let frames = u32::try_from(frames).map_err(OfflineRenderError::backend)?;
            let block = self.render_at(position, frames)?;
            if cancel.is_cancelled() {
                return Err(OfflineRenderError::Cancelled { rendered_frames });
            }
            sink.write(&block)
                .map_err(|error| OfflineRenderError::sink(rendered_frames, error))?;
            position = position
                .checked_add(u64::from(frames))
                .ok_or_else(|| OfflineRenderError::backend(TimelineOverflow))?;
            rendered_frames = rendered_frames
                .checked_add(u64::from(frames))
                .ok_or_else(|| OfflineRenderError::backend(TimelineOverflow))?;
        }
        if cancel.is_cancelled() {
            return Err(OfflineRenderError::Cancelled { rendered_frames });
        }
        debug_assert_eq!(rendered_frames, requested_frames);
        Ok(OfflineRenderReport::new(rendered_frames))
    }

    fn render_at(
        &self,
        position: u64,
        frames: u32,
    ) -> Result<kithara_bufpool::SampleBuffer, OfflineRenderError> {
        self.client
            .render(position, frames)
            .map_err(|error| match error {
                crate::session::offline::OfflineSessionError::CursorChanged { actual, .. } => {
                    OfflineRenderError::RangeUnavailable {
                        requested: position,
                        current: actual,
                    }
                }
                error => OfflineRenderError::backend(error),
            })
    }
}

impl<S> OfflineRenderer for Host<S>
where
    S: HasPool<f32> + Send + Sync + 'static,
{
    fn render(
        &mut self,
        request: &OfflineRenderRequest,
        cancel: &CancelToken,
        sink: &mut dyn RenderSink,
    ) -> Result<OfflineRenderReport, OfflineRenderError> {
        self.session
            .offline_runtime_mut()
            .ok_or(OfflineRenderError::SessionModeUnavailable)?
            .render(request, cancel, sink)
    }
}

#[derive(Debug, thiserror::Error)]
#[error("offline Host timeline overflow")]
struct TimelineOverflow;

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use kithara_bufpool::testing::{TestPools, pools};
    use kithara_output::{OfflineRenderRequest, OfflineRenderer, RenderSinkError};
    use kithara_platform::CancelScope;
    use kithara_test_utils::kithara;

    use super::*;
    use crate::HostConfig;

    struct Discard;

    impl RenderSink for Discard {
        fn write(&mut self, _samples: &[f32]) -> Result<(), RenderSinkError> {
            Ok(())
        }
    }

    #[kithara::test(native, flash(false))]
    fn offline_builder_configures_the_host_directly() {
        let sample_rate = NonZeroU32::new(48_000).expect("test sample rate is non-zero");
        let block_frames = NonZeroU32::new(128).expect("test block size is non-zero");
        let config = HostConfig::offline(pools())
            .sample_rate(sample_rate)
            .max_block_frames(block_frames)
            .build();

        assert_eq!(config.sample_rate(), sample_rate);
        assert_eq!(config.max_block_frames(), Some(block_frames));

        let host = Host::<TestPools>::new(config).expect("fixture offline Host");
        assert_eq!(host.requested_sample_rate(), sample_rate);
    }

    #[kithara::test(native, flash(false))]
    fn realtime_host_rejects_offline_rendering() {
        let mut host =
            Host::<TestPools>::new(HostConfig::builder().build()).expect("fixture realtime Host");
        let request = OfflineRenderRequest::builder()
            .spec(AudioSpec::new(Defaults::CHANNELS, Defaults::SAMPLE_RATE))
            .frames(0..1)
            .build();
        let cancel = CancelScope::new(None);

        assert!(matches!(
            host.render(&request, &cancel.token(), &mut Discard),
            Err(OfflineRenderError::SessionModeUnavailable)
        ));
    }
}
