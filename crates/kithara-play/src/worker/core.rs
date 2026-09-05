use std::{
    fmt,
    sync::atomic::{AtomicU64, Ordering},
};

use kithara_audio::{Audio, AudioSource, PreparedAudio, ResamplerBackend};
use kithara_bufpool::{HasPool, PoolRegion};
use kithara_decode::{DecodeError, DecodeResult};
use kithara_events::EventBus;
use kithara_platform::{CancelGroup, CancelToken, sync::Arc};
use kithara_stream::{Stream, StreamType};
use kithara_warp::Warp;
use kithara_worker::{Dispatcher, DispatcherConfig, TaskConfig, Worker, WorkerConfig};

use super::{
    DecoderNode, EngineLoad, PlayWorkerConfig, RegisteredAudio, TrackConfig, TrackLease,
    WarpSource,
    scheduler::{PlaybackObserver, ServiceClass, Wake},
};
use crate::effects::EffectDrain;

static WORKER_ID: AtomicU64 = AtomicU64::new(1);

struct WorkerOwner<S> {
    dispatcher: Dispatcher,
    base: Worker,
    pools: PoolRegion<S>,
}

/// Explicit owner of the playback dispatcher.
///
/// Clones share one OS thread and one scheduler loop. Dropping a Player only
/// releases that clone; the final owner shuts down its dispatcher and releases
/// its base-worker clone.
pub struct PlayWorker<S>(Arc<WorkerOwner<S>>);

impl<S> PlayWorker<S> {
    /// Construct the sole playback-worker implementation.
    #[must_use]
    pub fn new(config: PlayWorkerConfig<S>) -> Self {
        let PlayWorkerConfig {
            backpressure_poll_interval,
            cancel,
            capacity,
            fairness_yield_interval,
            idle_timeout,
            pools,
            slow_tick_threshold,
            task_burst,
            wait_timeout,
            worker,
        } = config;
        let (base, dispatcher_cancel) = if let Some(worker) = worker {
            (worker, cancel.map(CancelGroup::from))
        } else {
            let worker_config = cancel.map_or_else(WorkerConfig::new, |cancel| {
                WorkerConfig::new().with_cancel(cancel)
            });
            (Worker::new(worker_config), None)
        };
        let id = WORKER_ID.fetch_add(1, Ordering::Relaxed);
        let dispatcher_config = DispatcherConfig::builder()
            .name(format!("kithara-play-worker-{id}"))
            .backpressure_poll_interval(backpressure_poll_interval)
            .capacity(capacity)
            .fairness_yield_interval(fairness_yield_interval)
            .idle_timeout(idle_timeout)
            .observer(PlaybackObserver::default())
            .slow_tick_threshold(slow_tick_threshold)
            .task_burst(task_burst)
            .wait_timeout(wait_timeout)
            .maybe_cancel(dispatcher_cancel)
            .build();
        let dispatcher = base.dispatcher(dispatcher_config);
        Self(Arc::new(WorkerOwner {
            dispatcher,
            base,
            pools,
        }))
    }

    /// Shared typed pool facade used by every registered Player/resource.
    #[must_use]
    pub fn pools(&self) -> &PoolRegion<S> {
        &self.0.pools
    }
}

impl<S> Clone for PlayWorker<S> {
    fn clone(&self) -> Self {
        Self(Arc::clone(&self.0))
    }
}

impl<S> PlayWorker<S>
where
    S: HasPool<u8> + HasPool<f32> + Send + Sync + 'static,
{
    /// Prepare and register a stream-backed audio reader on this worker.
    ///
    /// # Errors
    ///
    /// Returns decode/setup errors or a typed worker registration failure.
    pub async fn open<T, B, C>(&self, config: C) -> DecodeResult<RegisteredAudio<Stream<T>, S>>
    where
        T: StreamType<Events = EventBus>,
        B: Default + ResamplerBackend,
        C: Into<TrackConfig<T, B>>,
    {
        let config = config.into();
        let audio_buffer_chunks = config.audio().audio_buffer_chunks();
        let TrackConfig {
            audio,
            effects,
            engine_load,
            warp,
        } = config;
        let task_cancel = audio.cancel().cloned();
        let wake = Wake::new(self.0.dispatcher.wake_handle());
        let prepared = Audio::<Stream<T>>::prepare(
            audio,
            audio_buffer_chunks,
            Arc::new(wake),
            self.pools().clone(),
        )
        .await?;
        let drain = EffectDrain::new(effects.len(), self.pools())?;
        let prepared = prepared.map(|audio, source| {
            let spec = audio.spec();
            let warp = Warp::new(audio, &warp);
            let source = WarpSource::new(
                source,
                warp.renderer(spec, self.pools().clone()),
                effects,
                drain,
                spec,
                self.pools().clone(),
            );
            (warp, source)
        });
        self.register(prepared, engine_load, task_cancel)
    }

    fn register<T, P>(
        &self,
        prepared: PreparedAudio<Warp<Audio<T>>, P>,
        engine_load: Option<Arc<EngineLoad>>,
        cancel: Option<CancelToken>,
    ) -> DecodeResult<RegisteredAudio<T, S>>
    where
        P: AudioSource<Chunk = kithara_signal::AudioChunk>,
    {
        let (audio, lane) = prepared.into();
        let mut task_config = TaskConfig::new().with_priority(ServiceClass::default().into());
        if let Some(cancel) = cancel {
            task_config = task_config.with_cancel(CancelGroup::from(cancel));
        }
        let task = self
            .0
            .dispatcher
            .register(task_config, |_| DecoderNode::new(lane, engine_load))
            .map_err(|error| DecodeError::audio_stream("play worker registration", error))?;
        Ok(RegisteredAudio::new(
            audio,
            TrackLease::new(self.clone(), task),
        ))
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
    use kithara_platform::CancelScope;
    use kithara_test_utils::kithara;

    use super::*;
    use crate::test_pools::pools;

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
