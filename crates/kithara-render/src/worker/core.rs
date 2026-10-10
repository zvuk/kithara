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
use kithara_platform::{sync::Arc, thread::ThreadClass, time::Duration};
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
        I: Open + kithara_platform::maybe_send::MaybeSend + 'static,
        I::Opened: kithara_platform::maybe_send::MaybeSend + 'static,
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
    /// Open once, position synchronously, and assemble the worker-owned lane.
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
        if self.0.dispatcher.is_cancelled()
            || cancel
                .as_ref()
                .is_some_and(kithara_platform::CancelToken::is_cancelled)
        {
            return Err(LoadRefusal::Cancelled);
        }
        if preload_chunks > audio_buffer_chunks {
            return Err(LoadRefusal::Open(DecodeError::InvalidData {
                detail: "lane preload quota exceeds PCM ring capacity",
            }));
        }
        let (receiver, lane) = {
            let wake = StreamWake::new(self.0.dispatcher.wake_handle());
            // Keep cold source preparation out of the callers' inline future state.
            let mut audio = Box::pin(Audio::<Stream<T>>::prepare(
                audio,
                Arc::new(wake.clone()),
                self.pools().clone(),
            ))
            .await
            .map_err(decode_refusal)?;
            if self.0.dispatcher.is_cancelled()
                || cancel
                    .as_ref()
                    .is_some_and(kithara_platform::CancelToken::is_cancelled)
            {
                return Err(LoadRefusal::Cancelled);
            }
            let position = match audio.seek(position).map_err(|error| match error {
                AudioReadError::Decode(error) => decode_refusal(error),
                error => LoadRefusal::Source(TrackFailureKind::from(&error)),
            })? {
                SeekOutcome::Landed { landed_at, .. } => landed_at,
                SeekOutcome::PastEof { duration, .. } => duration,
            };
            let spec = audio.spec();
            let initial = AudioChunkInfo {
                spec,
                timestamp: position,
                end_timestamp: position,
                ..AudioChunkInfo::default()
            };
            let (receiver, port) = PcmReceiver::new(
                audio_buffer_chunks,
                block_on_underrun,
                wake,
                &audio,
                position,
            );
            let activity = audio.take_activity_writer();
            let warp = Warp::new(
                (),
                &warp.starting_at(warp.speed(), start.keylock, start.backend),
            );
            let mut renderer = warp.renderer(spec, self.pools().clone());
            renderer
                .set_speed(start.speed, 0)
                .map_err(|error| DecodeError::audio_stream("initial lane speed curve", error))
                .map_err(decode_refusal)?;
            renderer
                .prepare_engine_latency(spec)
                .map_err(|error| DecodeError::audio_stream("lane engine preparation", error))
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
        if self.0.dispatcher.is_cancelled()
            || cancel
                .as_ref()
                .is_some_and(kithara_platform::CancelToken::is_cancelled)
        {
            return Err(LoadRefusal::Cancelled);
        }
        let latency = lane.engine_latency();
        Ok((receiver, *lane, latency))
    }
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
    use kithara_test_utils::kithara;
    use kithara_warp::{SpeedCurve, StretchKind};

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
}
