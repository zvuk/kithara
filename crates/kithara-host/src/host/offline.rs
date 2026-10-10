use std::num::{NonZeroU16, NonZeroU32, NonZeroUsize};

use kithara_bufpool::{HasPool, PoolRegion};
use kithara_command::Live;
use kithara_effects::LimiterConfig;
use kithara_output::{
    OfflineRenderError, OfflineRenderReport, OfflineRenderRequest, OfflineRenderer, RenderSink,
};
use kithara_platform::{CancelToken, maybe_send::MaybeSend, sync::Arc, time::Duration};
use kithara_play::PlayError;
use kithara_render::rt::DeckMixerConfig;
use kithara_signal::AudioSpec;
use kithara_worker::{DispatcherConfig, TaskConfig, Worker, WorkerConfig};

use super::{Host, HostConfig, platform::Platform};
use crate::{
    HostCore, HostOwner, HostSettings,
    rt::SessionOutput,
    session::{
        HostDispatcher, HostRoot, RootView,
        offline::{OfflineSessionClient, OfflineTaskConfig, OfflineTaskHandle},
    },
};

mod consts {
    use super::NonZeroU32;

    pub(super) const BLOCK_FRAMES: NonZeroU32 = match NonZeroU32::new(512) {
        Some(value) => value,
        None => unreachable!(),
    };
    pub(super) const CHANNELS: u16 = 2;
    #[cfg(test)]
    pub(super) const SAMPLE_RATE: NonZeroU32 = match NonZeroU32::new(44_100) {
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
        #[builder(default = consts::BLOCK_FRAMES)] max_block_frames: NonZeroU32,
        #[builder(default = consts::BLOCK_FRAMES)] declick_frames: NonZeroU32,
        #[builder(default = Duration::ZERO)] declared_latency: Duration,
        #[builder(default = crate::consts::MAX_DECKS)] max_decks: NonZeroU16,
        #[builder(default = crate::consts::DECK_CAPACITY)] deck_capacity: NonZeroUsize,
        #[builder(default = DeckMixerConfig::default().slots())] max_deck_slots: NonZeroUsize,
        #[builder(default)] limiter: LimiterConfig,
        #[builder(default)] settings: HostSettings,
        #[builder(default = WorkerConfig::new())] worker: WorkerConfig,
        #[builder(default = default_dispatcher_config())] dispatcher: DispatcherConfig,
        #[builder(default = TaskConfig::new())] task: TaskConfig,
    ) -> Self {
        Self::Offline {
            pools,
            max_block_frames,
            declick_frames,
            declared_latency,
            max_decks,
            deck_capacity,
            max_deck_slots,
            limiter,
            settings,
            worker,
            task,
            dispatcher: Box::new(dispatcher),
        }
    }
}

pub(super) struct OfflineRuntime<S, O: HostOwner<S>> {
    client: Arc<OfflineSessionClient<O::Command>>,
    _dispatcher: kithara_worker::Dispatcher,
    max_block_frames: NonZeroU32,
    _task: OfflineTaskHandle,
    _worker: Worker,
}

type StartedOfflineRuntime<S, O> = (
    Arc<dyn HostDispatcher<<O as HostOwner<S>>::Command>>,
    OfflineRuntime<S, O>,
);
/// An offline session started beside the platform that holds its decks.
pub(super) type StartedOffline<S, O> = (
    Arc<dyn HostDispatcher<<O as HostOwner<S>>::Command>>,
    Platform<S, O>,
    OfflineRuntime<S, O>,
);

impl<S, O: HostOwner<S>> OfflineRuntime<S, O>
where
    S: HasPool<f32> + Send + Sync + 'static,
{
    pub(super) fn new(
        config: HostConfig<S>,
        root: HostRoot,
        root_view: RootView,
        layer: impl FnOnce(HostCore<S, O::Deck>) -> O + MaybeSend + 'static,
    ) -> Result<StartedOfflineRuntime<S, O>, PlayError> {
        let channel_config = config.channel_config();
        let HostConfig::Offline {
            pools,
            max_block_frames,
            declick_frames,
            declared_latency,
            limiter,
            settings,
            worker,
            dispatcher,
            task,
            ..
        } = config
        else {
            unreachable!("offline runtime requires offline Host config");
        };
        let settings = Live::new(settings)?;
        let worker = Worker::new(worker);
        let dispatcher = worker.dispatcher(*dispatcher);
        let (client, task_handle) = crate::session::offline::spawn(
            &dispatcher,
            task,
            root,
            root_view,
            OfflineTaskConfig::builder()
                .declared_latency(declared_latency)
                .output(SessionOutput::new(limiter))
                .settings(settings)
                .channel_config(channel_config)
                .declick_frames(declick_frames)
                .max_block_frames(max_block_frames)
                .pools(pools)
                .build(),
            layer,
        )?;
        let host_dispatcher: Arc<dyn HostDispatcher<O::Command>> = client.clone();
        Ok((
            host_dispatcher,
            Self {
                client,
                max_block_frames,
                _worker: worker,
                _dispatcher: dispatcher,
                _task: task_handle,
            },
        ))
    }

    delegate::delegate! {
        to self.client {
            #[expr($.map_err(OfflineRenderError::backend))]
            fn position(&self) -> Result<u64, OfflineRenderError>;
        }
    }

    /// Renders `request` block by block, the Host's decks ticking ahead of
    /// each block.
    fn render(
        &mut self,
        request: &OfflineRenderRequest,
        spec: AudioSpec,
        cancel: &CancelToken,
        sink: &mut dyn RenderSink,
    ) -> Result<OfflineRenderReport, OfflineRenderError> {
        let requested_frames = request.frame_count()?;
        if request.spec() != spec {
            return Err(OfflineRenderError::SpecMismatch {
                expected: spec,
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

impl<S, O: HostOwner<S>> OfflineRenderer for Host<S, O>
where
    S: HasPool<f32> + Send + Sync + 'static,
{
    fn render(
        &mut self,
        request: &OfflineRenderRequest,
        cancel: &CancelToken,
        sink: &mut dyn RenderSink,
    ) -> Result<OfflineRenderReport, OfflineRenderError> {
        let rate = self.output_sample_rate().output();
        let rate = NonZeroU32::new(rate).ok_or_else(|| {
            OfflineRenderError::backend(PlayError::Internal(
                "offline session reported a zero output rate".into(),
            ))
        })?;
        let spec = AudioSpec::new(consts::CHANNELS, rate);
        let (_platform, runtime) = self
            ._session
            .offline_mut()
            .ok_or(OfflineRenderError::SessionModeUnavailable)?;
        runtime.render(request, spec, cancel, sink)
    }
}

#[derive(Debug, thiserror::Error)]
#[error("offline Host timeline overflow")]
struct TimelineOverflow;

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use kithara_command::When;
    use kithara_config::Configure;
    use kithara_output::{OfflineRenderRequest, OfflineRenderer, RenderSinkError};
    use kithara_platform::CancelScope;
    use kithara_signal::SessionFrame;
    use kithara_test_utils::{
        bufpool::{TestPools, pools},
        kithara,
    };
    use kithara_warp::{BeatGrid, BeatGridQuery, MapPoint, MapPosition};

    use super::*;
    use crate::{
        HostConfig, HostSettingsChange, HostSettingsControl, MetronomeConfigControl, api::Tempo,
    };

    struct Discard;

    impl RenderSink for Discard {
        fn write(&mut self, _samples: &[f32]) -> Result<(), RenderSinkError> {
            Ok(())
        }
    }

    #[derive(Default)]
    struct Capture(Vec<f32>);

    impl RenderSink for Capture {
        fn write(&mut self, samples: &[f32]) -> Result<(), RenderSinkError> {
            self.0.extend_from_slice(samples);
            Ok(())
        }
    }

    #[kithara::test(native, flash(false))]
    fn offline_builder_configures_the_host_directly() {
        let sample_rate = NonZeroU32::new(48_000).expect("test sample rate is non-zero");
        let block_frames = NonZeroU32::new(128).expect("test block size is non-zero");
        let config = HostConfig::offline(pools())
            .settings(HostSettings::builder().sample_rate(sample_rate).build())
            .max_block_frames(block_frames)
            .build();

        assert_eq!(config.settings().sample_rate(), sample_rate);
        assert_eq!(config.max_block_frames(), Some(block_frames));

        let host = Host::<TestPools>::new(config).expect("fixture offline Host");
        assert_eq!(host.sample_rate(), sample_rate);
    }

    #[kithara::test(native, flash(false))]
    fn realtime_host_rejects_offline_rendering() {
        let mut host =
            Host::<TestPools>::new(HostConfig::builder().build()).expect("fixture realtime Host");
        let request = OfflineRenderRequest::builder()
            .spec(AudioSpec::new(consts::CHANNELS, consts::SAMPLE_RATE))
            .frames(0..1)
            .build();
        let cancel = CancelScope::new(None);

        assert!(matches!(
            host.render(&request, &cancel.token(), &mut Discard),
            Err(OfflineRenderError::SessionModeUnavailable)
        ));
    }

    fn tempo_host() -> Host<TestPools> {
        let block_frames = NonZeroU32::new(128).expect("test block size is non-zero");
        let config = HostConfig::offline(pools())
            .max_block_frames(block_frames)
            .build();
        Host::<TestPools>::new(config).expect("fixture offline Host")
    }

    fn render_frames(host: &mut Host<TestPools>, frames: std::ops::Range<u64>) {
        render_into(host, frames, &mut Discard);
    }

    /// The interleaved samples `frames` render to.
    fn render_samples(host: &mut Host<TestPools>, frames: std::ops::Range<u64>) -> Vec<f32> {
        let mut capture = Capture::default();
        render_into(host, frames, &mut capture);
        capture.0
    }

    fn render_into(
        host: &mut Host<TestPools>,
        frames: std::ops::Range<u64>,
        sink: &mut dyn RenderSink,
    ) {
        let request = OfflineRenderRequest::builder()
            .spec(AudioSpec::new(consts::CHANNELS, consts::SAMPLE_RATE))
            .frames(frames)
            .build();
        let cancel = CancelScope::new(None);
        host.render(&request, &cancel.token(), sink)
            .expect("offline render");
    }

    fn tempo(beats_per_minute: f64) -> Tempo {
        Tempo::new(beats_per_minute).expect("fixture tempo is in range")
    }

    /// Tempo the Host grid resolves at session frame `frame`.
    fn grid_bpm(host: &Host<TestPools>, frame: i64) -> f64 {
        let grid = host.snapshot();
        let position = MapPoint::new(grid.stamp(), MapPosition::Session(SessionFrame::new(frame)));
        let BeatGridQuery::Resolved(estimate) = grid.tempo_at(position) else {
            panic!("the Host grid resolves its tempo at frame {frame}");
        };
        f64::from(*estimate.value())
    }

    #[kithara::test(native)]
    fn a_tempo_change_at_a_frame_reanchors_the_transport_on_that_frame() {
        let mut host = tempo_host();
        render_frames(&mut host, 0..256);
        assert_eq!(host.tempo(), tempo(120.0), "a Host starts at 120 BPM");
        assert_eq!(grid_bpm(&host, 200), 120.0, "the transport counts 120 BPM");

        host.configure(
            HostSettingsChange::Tempo(tempo(128.0)),
            When::At(SessionFrame::new(1_000)),
        )
        .expect("a frame two blocks ahead is reachable");
        render_frames(&mut host, 256..896);
        assert_eq!(
            host.tempo(),
            tempo(120.0),
            "the getter keeps 120 BPM until the change applies"
        );

        render_frames(&mut host, 896..1_024);
        assert_eq!(
            host.tempo(),
            tempo(128.0),
            "the getter shows the change once its receipt arrives"
        );
        assert_eq!(
            grid_bpm(&host, 999),
            120.0,
            "the frame before the change keeps 120 BPM"
        );
        let settled = grid_bpm(&host, 1_000 + i64::from(consts::SAMPLE_RATE.get()));
        assert!(
            (settled - 128.0).abs() < 1e-9,
            "a second after the change the transport counts 128 BPM, got {settled}"
        );
    }

    #[kithara::test(native)]
    fn a_tempo_change_at_a_rendered_frame_is_late() {
        let mut host = tempo_host();
        render_frames(&mut host, 0..2_048);

        assert!(matches!(
            host.configure(
                HostSettingsChange::Tempo(tempo(128.0)),
                When::At(SessionFrame::new(1_000)),
            ),
            Err(PlayError::Late)
        ));
        assert_eq!(host.tempo(), tempo(120.0), "a late change changes nothing");
    }

    #[kithara::test(native)]
    fn a_change_one_block_ahead_is_accepted_offline() {
        let mut host = tempo_host();
        render_frames(&mut host, 0..128);

        host.configure(
            HostSettingsChange::Tempo(tempo(128.0)),
            When::At(SessionFrame::new(256)),
        )
        .expect("one block gives the offline owner enough delivery lead");
        assert_eq!(host.tempo(), tempo(120.0));
        render_frames(&mut host, 128..384);
        assert_eq!(host.tempo(), tempo(128.0));
    }

    #[kithara::test(native)]
    fn without_a_render_context_a_frame_has_no_clock_and_the_next_moment_applies_at_once() {
        let mut host = tempo_host();

        assert!(matches!(
            host.configure(
                HostSettingsChange::Tempo(tempo(128.0)),
                When::At(SessionFrame::new(1_000)),
            ),
            Err(PlayError::Untimed)
        ));
        host.set_tempo(tempo(128.0))
            .expect("the next moment needs no clock");
        assert_eq!(host.tempo(), tempo(128.0), "the change applies at once");

        render_frames(&mut host, 0..128);
        assert_eq!(
            grid_bpm(&host, 64),
            128.0,
            "the transport starts at the configured tempo"
        );
    }

    #[kithara::test(native)]
    fn an_offline_metronome_level_change_sounds_in_the_block_it_is_set_before() {
        // Beat 1 at 120 BPM falls on frame 22 050, inside the block from 22 016.
        let beat_block = 22_016..22_144;
        let mut full_host = tempo_host();
        let mut quiet_host = tempo_host();
        for host in [&mut full_host, &mut quiet_host] {
            host.metronome()
                .set_enabled(true)
                .expect("the metronome switches on");
            render_frames(host, 0..beat_block.start);
        }

        quiet_host
            .metronome()
            .set_level(0.5)
            .expect("half level is in bounds");
        let full = render_samples(&mut full_host, beat_block.clone());
        let quiet = render_samples(&mut quiet_host, beat_block);

        assert_eq!(
            quiet_host.metronome().level(),
            0.5,
            "the getter shows the level once its receipt arrives"
        );
        assert!(
            full.iter().any(|sample| sample.abs() > 0.1),
            "the block carries the click of beat 1"
        );
        for (index, (full, quiet)) in full.iter().zip(&quiet).enumerate() {
            assert!(
                (quiet - full * 0.5).abs() < 1e-6,
                "sample {index} sounds at half level: full {full}, quiet {quiet}"
            );
        }
    }
}
