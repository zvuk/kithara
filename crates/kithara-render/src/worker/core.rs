use std::{
    fmt,
    num::NonZeroUsize,
    sync::atomic::{AtomicU64, Ordering},
};

use kithara_audio::{Audio, AudioReadError, ResamplerBackend, SeekOutcome, TrackFailureKind};
use kithara_bufpool::{HasPool, PoolError, PoolRegion};
use kithara_command::{ChannelConfig, Inbox, Sender, channel};
use kithara_decode::DecodeError;
use kithara_effects::EffectDrain;
use kithara_events::EventBus;
use kithara_platform::{
    sync::Arc, thread::ThreadClass, time::Duration, tokio::task::spawn_blocking,
};
use kithara_signal::{AudioChunkInfo, FrameCount};
use kithara_stream::{Stream, StreamType};
use kithara_warp::Warp;
use kithara_worker::{
    Dispatcher, DispatcherConfig, TaskConfig, TaskError, TaskHandle, Worker, WorkerConfig,
};

use super::{
    DecoderNode, PcmReceiver, PlayWorkerConfig, TrackConfig,
    scheduler::{PlaybackObserver, StreamWake},
};
use crate::{
    LaneProtocol, ServiceClass, WarpSource,
    dispatcher::{DispatcherProtocol, DispatcherTask, LaneStart, LaneTask, Open},
};

static WORKER_ID: AtomicU64 = AtomicU64::new(1);

/// Why a worker refused to load a track.
#[derive(Debug, thiserror::Error)]
pub enum LoadRefusal {
    #[error("play worker holds its capacity of {capacity} tracks")]
    Capacity { capacity: usize },
    #[error("the track's load was cancelled before it opened")]
    Cancelled,
    #[error("play worker has no runtime for opening a track")]
    NoRuntime,
    #[error(transparent)]
    Open(#[from] DecodeError),
    #[error(transparent)]
    Source(TrackFailureKind),
    #[error(transparent)]
    Pool(#[from] PoolError),
}

impl From<LoadRefusal> for DecodeError {
    fn from(refusal: LoadRefusal) -> Self {
        match refusal {
            LoadRefusal::Open(error) => error,
            refusal @ (LoadRefusal::Capacity { .. }
            | LoadRefusal::Cancelled
            | LoadRefusal::NoRuntime
            | LoadRefusal::Pool(_)
            | LoadRefusal::Source(_)) => Self::audio_stream("play worker load", refusal),
        }
    }
}

struct WorkerOwner<S> {
    lane: ChannelConfig,
    capacity: NonZeroUsize,
    dispatcher: Dispatcher,
    pools: PoolRegion<S>,
    base: Worker,
}

/// Shared scheduler and pools for the sole render dispatcher task.
#[derive_where::derive_where(Clone)]
pub struct PlayWorker<S>(Arc<WorkerOwner<S>>);

impl<S> PlayWorker<S> {
    #[must_use]
    pub fn new(config: PlayWorkerConfig<S>) -> Self {
        let PlayWorkerConfig {
            backpressure_poll_interval,
            cancel,
            capacity,
            fairness_yield_interval,
            idle_timeout,
            lane_capacity,
            pools,
            runtime,
            slow_tick_threshold,
            task_burst,
            wait_timeout,
            worker,
        } = config;
        let (base, dispatcher_cancel) = if let Some(worker) = worker {
            (worker, cancel.map(kithara_platform::CancelGroup::from))
        } else {
            let worker_config = cancel.map_or_else(WorkerConfig::new, |cancel| {
                WorkerConfig::new().with_cancel(cancel)
            });
            let worker_config = if let Some(runtime) = runtime {
                worker_config.with_runtime(runtime)
            } else {
                worker_config
            };
            (Worker::new(worker_config), None)
        };
        let id = WORKER_ID.fetch_add(1, Ordering::Relaxed);
        let dispatcher_config = DispatcherConfig::builder()
            .name(format!("kithara-play-worker-{id}"))
            .backpressure_poll_interval(backpressure_poll_interval)
            .capacity(NonZeroUsize::MIN)
            .fairness_yield_interval(fairness_yield_interval)
            .idle_timeout(idle_timeout)
            .observer(PlaybackObserver::default())
            .slow_tick_threshold(slow_tick_threshold)
            .task_burst(task_burst)
            .thread_class(ThreadClass::AudioFeed)
            .wait_timeout(wait_timeout)
            .maybe_cancel(dispatcher_cancel)
            .build();
        let dispatcher = base.dispatcher(dispatcher_config);
        Self(Arc::new(WorkerOwner {
            lane: ChannelConfig::builder().capacity(lane_capacity).build(),
            capacity,
            dispatcher,
            pools,
            base,
        }))
    }

    #[must_use]
    pub fn pools(&self) -> &PoolRegion<S> {
        &self.0.pools
    }

    /// Longest park duration before the render dispatcher checks for new work.
    #[must_use]
    pub fn wake_allowance(&self) -> Duration {
        self.0.dispatcher.wake_allowance()
    }

    pub fn wake(&self) {
        self.0.dispatcher.wake_handle().wake();
    }

    /// Allocate the lane channel whose sender stays with the requesting owner.
    #[must_use]
    pub fn lane_channel(&self) -> (Sender<LaneProtocol>, Inbox<LaneProtocol>) {
        channel(self.0.lane)
    }

    /// Register the actual dispatcher owner on this worker's scheduler thread.
    ///
    /// # Errors
    /// Returns a refusal if another dispatcher is registered or the worker stopped.
    pub fn start_dispatcher<I>(
        &self,
        inbox: Inbox<DispatcherProtocol<I>>,
    ) -> Result<TaskHandle, LoadRefusal>
    where
        I: Open + Send + 'static,
        I::Opened: Send + 'static,
        I::Lane: LaneTask,
    {
        let capacity = self.0.capacity;
        let wake = self.0.dispatcher.wake_handle();
        self.0
            .dispatcher
            .reserve(TaskConfig::new().with_priority(ServiceClass::Warm.into()))
            .map_err(task_refusal)?
            .start_local(move |context| {
                DispatcherTask::new(inbox, capacity, wake, context.runtime().cloned())
            })
            .map_err(task_refusal)
    }
}

impl<S> PlayWorker<S>
where
    S: HasPool<u8> + HasPool<f32> + Send + Sync + 'static,
{
    /// Open once, accept initial positioning, and assemble the worker-owned lane.
    ///
    /// # Errors
    /// Returns cancellation, source/decoder, invalid ring geometry, or pool failures.
    pub async fn load<T, B, C>(
        &self,
        config: C,
        position: Duration,
        start: LaneStart,
        inbox: Inbox<LaneProtocol>,
    ) -> Result<(PcmReceiver, DecoderNode<Audio<Stream<T>>, S>, FrameCount), LoadRefusal>
    where
        T: StreamType<Events = EventBus>,
        B: Default + ResamplerBackend,
        C: Into<TrackConfig<T, B>>,
    {
        let TrackConfig {
            audio,
            effects,
            engine_load,
            warp,
            preload_chunks,
            declick,
            audio_buffer_chunks,
            block_on_underrun,
        } = config.into();
        let cancel = audio.cancel().cloned();
        self.check_load_cancelled(cancel.as_ref())?;
        if preload_chunks > audio_buffer_chunks {
            return Err(LoadRefusal::Open(DecodeError::InvalidData {
                detail: "lane preload quota exceeds PCM ring capacity",
            }));
        }
        let (receiver, lane) = {
            let wake = StreamWake::new(self.0.dispatcher.wake_handle());
            // Keep cold source preparation out of the callers' inline future state.
            let prepared = Box::pin(Audio::<Stream<T>>::prepare(
                audio,
                Arc::new(wake.clone()),
                self.pools().clone(),
            ))
            .await;
            self.check_load_cancelled(cancel.as_ref())?;
            let mut audio = prepared.map_err(decode_refusal)?;
            let position = initial_position(&mut audio, position)?;
            let spec = audio.spec();
            let initial = AudioChunkInfo {
                spec,
                media_origin: audio.media_origin(),
                visible_origin: audio.visible_origin(),
                media_end: audio.media_end(),
                timestamp: position,
                end_timestamp: position,
                ..AudioChunkInfo::default()
            };
            let (mut receiver, port) = PcmReceiver::new(
                audio_buffer_chunks,
                block_on_underrun,
                wake,
                &audio,
                position,
            );
            let activity = audio.take_activity_writer();
            let mut warp = Warp::new(
                (),
                &warp.starting_at(warp.speed(), start.keylock, start.backend),
            );
            receiver.render_publisher = warp.take_publisher();
            let pools = self.pools().clone();
            let dispatch = tracing::dispatcher::get_default(Clone::clone);
            let span = tracing::Span::current();
            // Cold engine allocation must not stop service of resident lanes.
            let prepared = spawn_blocking(move || {
                tracing::dispatcher::with_default(&dispatch, || {
                    let _entered = span.enter();
                    let mut renderer = warp.renderer(spec, pools);
                    renderer.set_speed(start.speed, 0).map_err(|error| {
                        DecodeError::audio_stream("initial lane speed curve", error)
                    })?;
                    renderer.prepare_engine_latency(spec).map_err(|error| {
                        DecodeError::audio_stream("lane engine preparation", error)
                    })?;
                    Ok::<_, DecodeError>(renderer)
                })
            })
            .await;
            self.check_load_cancelled(cancel.as_ref())?;
            let renderer = prepared
                .map_err(|error| DecodeError::audio_stream("lane renderer preparation task", error))
                .map_err(decode_refusal)?
                .map_err(decode_refusal)?;
            let drain = EffectDrain::new(effects.len(), self.pools())?;
            let source = WarpSource::new(
                audio,
                renderer,
                effects,
                drain,
                spec,
                self.pools().clone(),
                crate::LaneSetup {
                    inbox,
                    preload_chunks,
                    declick,
                },
            );
            let lane = Box::new(DecoderNode::new(
                source,
                port,
                activity,
                initial,
                engine_load,
                self.pools().clone(),
                cancel.clone(),
            ));
            (receiver, lane)
        };
        self.check_load_cancelled(cancel.as_ref())?;
        let latency = lane.engine_latency();
        Ok((receiver, *lane, latency))
    }

    fn check_load_cancelled(
        &self,
        cancel: Option<&kithara_platform::CancelToken>,
    ) -> Result<(), LoadRefusal> {
        if self.0.dispatcher.is_cancelled()
            || cancel.is_some_and(kithara_platform::CancelToken::is_cancelled)
        {
            return Err(LoadRefusal::Cancelled);
        }
        Ok(())
    }
}

fn initial_position<S>(audio: &mut Audio<S>, position: Duration) -> Result<Duration, LoadRefusal> {
    Ok(if position.is_zero() {
        position
    } else {
        let raw_position = audio
            .media_origin()
            .and_then(|origin| origin.checked_add(position))
            .ok_or(LoadRefusal::Open(DecodeError::InvalidData {
                detail: "initial media target has no raw source translation",
            }))?;
        match audio.seek(raw_position).map_err(|error| match error {
            AudioReadError::Decode(error) => decode_refusal(error),
            error => LoadRefusal::Source(TrackFailureKind::from(&error)),
        })? {
            SeekOutcome::Pending { .. } => audio.position(),
            SeekOutcome::Landed { landed_at, .. } => landed_at,
            SeekOutcome::PastEof { duration, .. } => duration,
        }
    })
}

fn decode_refusal(error: DecodeError) -> LoadRefusal {
    match error {
        DecodeError::Pool { source } => LoadRefusal::Pool(source),
        error => LoadRefusal::Open(error),
    }
}

fn task_refusal(error: TaskError) -> LoadRefusal {
    match error {
        TaskError::Capacity { capacity } => LoadRefusal::Capacity { capacity },
        TaskError::Cancelled => LoadRefusal::Cancelled,
        error => LoadRefusal::Open(DecodeError::audio_stream("play worker dispatcher", error)),
    }
}

impl<S> fmt::Debug for PlayWorker<S> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PlayWorker")
            .field("base_cancelled", &self.0.base.is_cancelled())
            .field("pools", self.pools())
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use std::mem::size_of_val;

    use kithara_assets::{AssetStore, StorageBackend};
    use kithara_audio::{AudioConfig, NoResamplerBackend};
    use kithara_file::{File, FileConfig, FileSrc};
    use kithara_platform::CancelScope;
    use kithara_sync::SpeedCurve;
    use kithara_test_utils::kithara;
    use kithara_warp::StretchKind;

    use super::*;
    use crate::test_pools::pools;

    #[kithara::test]
    fn source_preparation_future_is_bounded() {
        let play = PlayWorker::new(PlayWorkerConfig::builder(pools()).build());
        let audio = || {
            let file = FileConfig::for_src(FileSrc::Local("unused.wav".into()))
                .store(
                    AssetStore::builder(play.pools().clone())
                        .backend(StorageBackend::Memory)
                        .build(),
                )
                .pools(play.pools().clone())
                .build();
            AudioConfig::<File<_>, NoResamplerBackend>::for_stream(file).build()
        };
        let (_sender, inbox) = play.lane_channel();
        let start = LaneStart {
            speed: SpeedCurve::Constant(1.0),
            keylock: false,
            backend: StretchKind::default(),
        };
        let preparation =
            play.load::<File<_>, NoResamplerBackend, _>(audio(), Duration::ZERO, start, inbox);
        let source = Audio::<Stream<File<_>>>::prepare(
            audio(),
            Arc::new(StreamWake::new(play.0.dispatcher.wake_handle())),
            play.pools().clone(),
        );
        let bytes = size_of_val(&preparation);
        let source_bytes = size_of_val(&source);

        assert!(
            bytes < source_bytes,
            "lane holds {bytes} bytes inline versus {source_bytes} bytes of cold preparation"
        );
    }

    #[kithara::test]
    fn shared_base_outlives_play_dispatcher_and_play_cancel_stays_local() {
        let base = Worker::new(WorkerConfig::new());
        let cancel = CancelScope::new(None);
        let play = PlayWorker::new(
            PlayWorkerConfig::builder(pools())
                .worker(base.clone())
                .cancel(cancel.token())
                .build(),
        );

        cancel.cancel();

        assert!(play.0.dispatcher.is_cancelled());
        assert!(!base.is_cancelled());
        drop(play);
        assert!(!base.is_cancelled());
    }

    struct GatedFailure;

    impl StreamType for GatedFailure {
        type Config = futures::channel::oneshot::Receiver<()>;
        type Events = EventBus;
        type Source = unimock::Unimock;

        async fn create(config: Self::Config) -> Result<Self::Source, kithara_stream::SourceError> {
            config.await.expect("the test releases source preparation");
            Err(kithara_stream::SourceError::InvalidPath(
                "gated source failure".to_owned(),
            ))
        }
    }

    #[kithara::test(native, tokio)]
    #[case::live_error(false, false)]
    #[case::cancelled_audio(true, false)]
    #[case::cancelled_dispatcher(false, true)]
    async fn cancellation_during_failed_preparation_wins_over_its_source_error(
        #[case] cancel_audio: bool,
        #[case] cancel_dispatcher: bool,
    ) {
        let audio_cancel = CancelScope::new(None);
        let dispatcher_cancel = CancelScope::new(None);
        let play = PlayWorker::new(
            PlayWorkerConfig::builder(pools())
                .cancel(dispatcher_cancel.token())
                .build(),
        );
        let (release, gate) = futures::channel::oneshot::channel();
        let audio = AudioConfig::<GatedFailure, NoResamplerBackend>::for_stream(gate)
            .cancel(audio_cancel.token())
            .build();
        let (_sender, inbox) = play.lane_channel();
        let start = LaneStart {
            speed: SpeedCurve::Constant(1.0),
            keylock: false,
            backend: StretchKind::default(),
        };
        let mut opening = Box::pin(play.load::<GatedFailure, NoResamplerBackend, _>(
            audio,
            Duration::ZERO,
            start,
            inbox,
        ));
        assert!(futures::poll!(opening.as_mut()).is_pending());
        if cancel_audio {
            audio_cancel.cancel();
        }
        if cancel_dispatcher {
            dispatcher_cancel.cancel();
        }
        release.send(()).expect("release the pending source");
        let Err(refusal) = opening.await else {
            panic!("the gated source cannot open");
        };
        if cancel_audio || cancel_dispatcher {
            assert!(matches!(&refusal, LoadRefusal::Cancelled), "{refusal:?}");
        } else {
            let LoadRefusal::Open(DecodeError::Io { source }) = refusal else {
                panic!("live preparation keeps its source error: {refusal:?}");
            };
            let error = source
                .get_ref()
                .and_then(|error| error.downcast_ref::<kithara_stream::SourceError>())
                .expect("the original stream error remains typed");
            assert!(matches!(
                error,
                kithara_stream::SourceError::InvalidPath(path)
                    if path == "gated source failure"
            ));
        }
    }
}
