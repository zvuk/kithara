use std::{
    error::Error,
    fmt,
    num::{NonZeroU64, NonZeroUsize},
};

use kithara::{
    analysis::{
        AnalysisFile, AnalysisFileError, AnalysisFileSpec, AnalysisFileUpdate, AnalysisProgress,
    },
    assets::{AcquisitionResult, AssetReader, AssetWriter, AssetsError, ReadSide, WriteSide},
    platform::{
        CancelGroup,
        time::Duration,
        tokio::{
            self,
            runtime::Handle,
            sync::{mpsc, oneshot},
            task::{JoinError, JoinHandle, spawn_blocking_on, spawn_on},
        },
    },
    worker::{
        Dispatcher, DispatcherConfig, Task, TaskConfig, TaskContext, TaskError, TaskHandle,
        TickResult, Worker,
    },
};
use kithara_test_utils::kithara as probe;

use super::AnalysisTarget;
use crate::pools::{AppPools, AppStore, Pools};

/// Complete configuration for the app-owned analysis persistence actor.
pub(crate) struct AnalysisPersistenceConfig {
    worker: Worker,
    pools: Pools,
    queue_capacity: NonZeroUsize,
    chunk_duration: Duration,
    dispatcher: DispatcherConfig,
    task: TaskConfig,
}

impl AnalysisPersistenceConfig {
    pub(crate) fn new(
        worker: Worker,
        pools: Pools,
        queue_capacity: NonZeroUsize,
        chunk_duration: Duration,
        dispatcher: DispatcherConfig,
        task: TaskConfig,
    ) -> Self {
        Self {
            worker,
            pools,
            queue_capacity,
            chunk_duration,
            dispatcher,
            task,
        }
    }
}

/// Cloneable handle to one ordered, bounded analysis persistence actor.
#[derive(Clone)]
pub(crate) struct AnalysisPersistence {
    inner: kithara::platform::sync::Arc<AnalysisPersistenceInner>,
}

impl AnalysisPersistence {
    /// Start one persistence task on the configured base worker.
    pub(crate) fn new(config: AnalysisPersistenceConfig) -> Result<Self, AnalysisPersistenceError> {
        let AnalysisPersistenceConfig {
            worker,
            pools,
            queue_capacity,
            chunk_duration,
            dispatcher: dispatcher_config,
            task: task_config,
        } = config;
        let dispatcher = worker.dispatcher(dispatcher_config);
        let pending = dispatcher.reserve(task_config)?;
        let runtime = pending
            .context()
            .runtime()
            .cloned()
            .ok_or(AnalysisPersistenceError::RuntimeUnavailable)?;
        let (tx, rx) = mpsc::channel(queue_capacity.get());
        let task = pending.start(move |context| {
            PersistenceTask::new(&context, &runtime, rx, pools, chunk_duration)
        })?;

        Ok(Self {
            inner: kithara::platform::sync::Arc::new(AnalysisPersistenceInner {
                tx,
                _owner: PersistenceOwner {
                    task,
                    dispatcher,
                    _worker: worker,
                },
            }),
        })
    }

    /// Enqueue a final publication and wait until its commit is acknowledged.
    pub(crate) async fn store(
        &self,
        target: AnalysisTarget,
        progress: AnalysisProgress,
    ) -> Result<(), AnalysisPersistenceError> {
        let (ack, done) = oneshot::channel();
        self.inner
            .tx
            .send(StoreRequest {
                target,
                progress,
                ack,
            })
            .await
            .map_err(|_| AnalysisPersistenceError::QueueClosed)?;
        done.await
            .map_err(|_| AnalysisPersistenceError::AcknowledgementClosed)?
    }

    /// Try to enqueue an intermediate publication without waiting or blocking.
    #[must_use]
    pub(crate) fn try_store(&self, target: AnalysisTarget, progress: AnalysisProgress) -> bool {
        let (ack, done) = oneshot::channel();
        drop(done);
        self.inner
            .tx
            .try_send(StoreRequest {
                target,
                progress,
                ack,
            })
            .is_ok()
    }
}

struct AnalysisPersistenceInner {
    tx: mpsc::Sender<StoreRequest>,
    _owner: PersistenceOwner,
}

struct PersistenceOwner {
    task: TaskHandle,
    dispatcher: Dispatcher,
    _worker: Worker,
}

impl Drop for PersistenceOwner {
    fn drop(&mut self) {
        self.task.cancel();
        self.dispatcher.shutdown();
    }
}

struct StoreRequest {
    target: AnalysisTarget,
    progress: AnalysisProgress,
    ack: oneshot::Sender<Result<(), AnalysisPersistenceError>>,
}

struct PersistenceTask {
    actor: Option<JoinHandle<()>>,
}

impl PersistenceTask {
    fn new(
        context: &TaskContext,
        runtime: &Handle,
        rx: mpsc::Receiver<StoreRequest>,
        pools: Pools,
        chunk_duration: Duration,
    ) -> Self {
        let cancel = context.cancel_group().clone();
        let actor = spawn_on(
            runtime,
            run_actor(rx, runtime.clone(), pools, chunk_duration, cancel),
        );
        Self { actor: Some(actor) }
    }

    fn abort(&mut self) {
        let Some(actor) = self.actor.take() else {
            return;
        };
        abort_join(&actor);
    }
}

impl Task for PersistenceTask {
    fn tick(&mut self) -> TickResult {
        TickResult::Waiting
    }

    fn on_cancel(&mut self) {
        self.abort();
    }
}

impl Drop for PersistenceTask {
    fn drop(&mut self) {
        self.abort();
    }
}

async fn run_actor(
    mut rx: mpsc::Receiver<StoreRequest>,
    runtime: Handle,
    pools: Pools,
    chunk_duration: Duration,
    cancel: CancelGroup,
) {
    loop {
        let request = tokio::select! {
            biased;
            () = cancel.cancelled() => return,
            request = rx.recv() => match request {
                Some(request) => request,
                None => return,
            },
        };
        let revision = request.progress.analysis().revision();
        let key = request.target.key.clone();
        let result = tokio::select! {
            biased;
            () = cancel.cancelled() => return,
            result = persist(
                request.target,
                request.progress,
                runtime.clone(),
                pools.clone(),
                chunk_duration,
                cancel.clone(),
            ) => result,
        };

        let ack = match result {
            Ok(WriteOutcome::Committed { final_len }) => {
                analysis_persistence_committed(revision, final_len);
                Ok(())
            }
            Ok(WriteOutcome::AlreadyCommitted) => Ok(()),
            Err(error) => {
                tracing::warn!(?key, %error, "analysis persistence failed");
                Err(error)
            }
        };
        drop(request.ack.send(ack));
    }
}

async fn persist(
    target: AnalysisTarget,
    progress: AnalysisProgress,
    runtime: Handle,
    pools: Pools,
    chunk_duration: Duration,
    cancel: CancelGroup,
) -> Result<WriteOutcome, AnalysisPersistenceError> {
    let store = target.store;
    let key = target.key;
    let operation_store = store.clone();
    let operation_key = key.clone();
    store
        .with_resource_transaction(&key, move || {
            let job = spawn_blocking_on(&runtime, move || {
                write_request(
                    &operation_store,
                    &operation_key,
                    &progress,
                    &pools,
                    chunk_duration,
                    &cancel,
                )
            });
            AbortOnDrop::new(job).join()
        })
        .await?
}

fn write_request(
    store: &AppStore,
    key: &kithara::assets::ResourceKey,
    progress: &AnalysisProgress,
    pools: &Pools,
    chunk_duration: Duration,
    cancel: &CancelGroup,
) -> Result<WriteOutcome, AnalysisPersistenceError> {
    ensure_active(cancel)?;
    let spec = file_spec(progress, chunk_duration)?;
    match store.acquire_resource(key, None)? {
        AcquisitionResult::Pending(writer) => {
            let update = AnalysisFile::create(&spec, progress)?;
            commit_generation(writer, &update, None, cancel)
        }
        AcquisitionResult::Ready(reader) => {
            write_existing(reader, &spec, progress, pools, chunk_duration, cancel)
        }
        _ => Err(AnalysisPersistenceError::InvalidResourceState),
    }
}

fn write_existing(
    reader: AssetReader<AppPools>,
    spec: &AnalysisFileSpec,
    progress: &AnalysisProgress,
    pools: &Pools,
    chunk_duration: Duration,
    cancel: &CancelGroup,
) -> Result<WriteOutcome, AnalysisPersistenceError> {
    let mut bytes = pools.get::<u8>();
    reader.read_into(&mut bytes).map_err(AssetsError::from)?;
    ensure_active(cancel)?;

    let update = match AnalysisFile::parse(&bytes, progress.analysis().fingerprint()) {
        Ok(file) if file_matches(&file, spec, chunk_duration) => {
            let stored = file.latest().analysis().revision();
            let incoming = progress.analysis().revision();
            if stored == incoming {
                return Ok(WriteOutcome::AlreadyCommitted);
            }
            file.update(progress)?
        }
        Ok(_) | Err(_) => AnalysisFile::create(spec, progress)?,
    };
    let prefix_len = usize::try_from(update.payload().offset())
        .map_err(|_| AnalysisPersistenceError::InvalidWritePlan)?;
    let prefix = if update.initial_bytes().is_none() {
        Some(
            bytes
                .get(..prefix_len)
                .ok_or(AnalysisPersistenceError::InvalidWritePlan)?,
        )
    } else {
        None
    };
    ensure_active(cancel)?;
    let writer = reader.reactivate().map_err(AssetsError::from)?;
    commit_generation(writer, &update, prefix, cancel)
}

fn file_spec(
    progress: &AnalysisProgress,
    chunk_duration: Duration,
) -> Result<AnalysisFileSpec, AnalysisPersistenceError> {
    const NANOS_PER_SECOND: u128 = 1_000_000_000;

    let rate = progress.analysis().source_sample_rate();
    let scaled = u128::from(rate.get())
        .checked_mul(chunk_duration.as_nanos())
        .ok_or(AnalysisPersistenceError::InvalidChunkDuration)?;
    if scaled % NANOS_PER_SECOND != 0 {
        return Err(AnalysisPersistenceError::InvalidChunkDuration);
    }
    let frames = u64::try_from(scaled / NANOS_PER_SECOND)
        .ok()
        .and_then(NonZeroU64::new)
        .ok_or(AnalysisPersistenceError::InvalidChunkDuration)?;
    Ok(AnalysisFileSpec::for_analysis(progress.analysis(), frames)?)
}

fn file_matches(file: &AnalysisFile, spec: &AnalysisFileSpec, chunk_duration: Duration) -> bool {
    file.spec().source_sample_rate() == spec.source_sample_rate()
        && file.spec().extent() == spec.extent()
        && file.spec().fingerprint() == spec.fingerprint()
        && file.spec().matches_chunk_duration(chunk_duration)
}

fn commit_generation(
    writer: AssetWriter<AppPools>,
    update: &AnalysisFileUpdate,
    prior_prefix: Option<&[u8]>,
    cancel: &CancelGroup,
) -> Result<WriteOutcome, AnalysisPersistenceError> {
    ensure_active(cancel)?;
    match update.initial_bytes() {
        Some(initial) => writer.write_at(0, initial).map_err(AssetsError::from)?,
        None => writer
            .write_at(
                0,
                prior_prefix.ok_or(AnalysisPersistenceError::InvalidWritePlan)?,
            )
            .map_err(AssetsError::from)?,
    }
    ensure_active(cancel)?;
    writer
        .write_at(update.payload().offset(), update.payload().bytes())
        .map_err(AssetsError::from)?;
    for patch in update.patches() {
        ensure_active(cancel)?;
        writer
            .write_at(patch.offset(), patch.bytes())
            .map_err(AssetsError::from)?;
    }
    ensure_active(cancel)?;
    drop(
        writer
            .commit(Some(update.final_len()))
            .map_err(AssetsError::from)?,
    );
    Ok(WriteOutcome::Committed {
        final_len: update.final_len(),
    })
}

fn ensure_active(cancel: &CancelGroup) -> Result<(), AnalysisPersistenceError> {
    if cancel.is_cancelled() {
        Err(AnalysisPersistenceError::Cancelled)
    } else {
        Ok(())
    }
}

enum WriteOutcome {
    Committed { final_len: u64 },
    AlreadyCommitted,
}

struct AbortOnDrop<T> {
    handle: JoinHandle<T>,
}

impl<T> AbortOnDrop<T> {
    const fn new(handle: JoinHandle<T>) -> Self {
        Self { handle }
    }

    async fn join(mut self) -> Result<T, AnalysisPersistenceError> {
        (&mut self.handle)
            .await
            .map_err(AnalysisPersistenceError::Join)
    }
}

impl<T> Drop for AbortOnDrop<T> {
    fn drop(&mut self) {
        abort_handle(&self.handle);
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn abort_handle<T>(handle: &JoinHandle<T>) {
    handle.abort();
}

#[cfg(target_arch = "wasm32")]
fn abort_handle<T>(_handle: &JoinHandle<T>) {}

#[cfg(not(target_arch = "wasm32"))]
fn abort_join<T>(handle: &JoinHandle<T>) {
    handle.abort();
}

#[cfg(target_arch = "wasm32")]
fn abort_join<T>(_handle: &JoinHandle<T>) {}

#[probe::probe(revision, bytes = final_len)]
fn analysis_persistence_committed(revision: u64, final_len: u64) {}

/// Analysis persistence startup, queue, storage, or archive failure.
#[derive(Debug)]
pub(crate) enum AnalysisPersistenceError {
    RuntimeUnavailable,
    QueueClosed,
    AcknowledgementClosed,
    Cancelled,
    InvalidChunkDuration,
    InvalidResourceState,
    InvalidWritePlan,
    Task(TaskError),
    Analysis(AnalysisFileError),
    Assets(AssetsError),
    Join(JoinError),
}

impl fmt::Display for AnalysisPersistenceError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::RuntimeUnavailable => formatter.write_str("persistence worker has no runtime"),
            Self::QueueClosed => formatter.write_str("persistence queue is closed"),
            Self::AcknowledgementClosed => {
                formatter.write_str("persistence acknowledgement closed")
            }
            Self::Cancelled => formatter.write_str("persistence task was cancelled"),
            Self::InvalidChunkDuration => {
                formatter.write_str("analysis chunk duration does not map to whole source frames")
            }
            Self::InvalidResourceState => {
                formatter.write_str("asset store returned an unsupported resource state")
            }
            Self::InvalidWritePlan => formatter.write_str("analysis file write plan is invalid"),
            Self::Task(error) => write!(formatter, "persistence task failed: {error}"),
            Self::Analysis(error) => write!(formatter, "analysis file failed: {error}"),
            Self::Assets(error) => write!(formatter, "analysis asset failed: {error}"),
            Self::Join(error) => write!(formatter, "persistence job failed: {error}"),
        }
    }
}

impl Error for AnalysisPersistenceError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Task(error) => Some(error),
            Self::Analysis(error) => Some(error),
            Self::Assets(error) => Some(error),
            Self::Join(error) => Some(error),
            Self::RuntimeUnavailable
            | Self::QueueClosed
            | Self::AcknowledgementClosed
            | Self::Cancelled
            | Self::InvalidChunkDuration
            | Self::InvalidResourceState
            | Self::InvalidWritePlan => None,
        }
    }
}

impl From<TaskError> for AnalysisPersistenceError {
    fn from(error: TaskError) -> Self {
        Self::Task(error)
    }
}

impl From<AnalysisFileError> for AnalysisPersistenceError {
    fn from(error: AnalysisFileError) -> Self {
        Self::Analysis(error)
    }
}

impl From<AssetsError> for AnalysisPersistenceError {
    fn from(error: AssetsError) -> Self {
        Self::Assets(error)
    }
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroU32;

    use ::kithara::{
        analysis::{AnalysisFingerprint, Coverage, FrameRange, TrackAnalysis},
        assets::{ReadSide, StorageBackend},
        platform::{time::Duration, tokio::runtime::Handle},
        prelude::ResourceSrc,
        worker::{DispatcherConfig, TaskConfig, WorkerConfig},
    };
    use kithara_test_utils::kithara;

    use super::*;
    use crate::pools::{self, AppResourceConfig};

    struct Consts;

    impl Consts {
        const CHUNK_FRAMES: u64 = 16;
        const EXTENT: u64 = 64;
    }

    fn analysis(revision: u64, ranges: &[(u64, u64)]) -> TrackAnalysis {
        let mut coverage = Coverage::default();
        for &(start, frames) in ranges {
            coverage.insert(FrameRange::new(start, frames));
        }
        TrackAnalysis::builder()
            .token("persistence-test".into())
            .revision(revision)
            .source_sample_rate(NonZeroU32::MIN)
            .extent(Consts::EXTENT)
            .settled(true)
            .coverage(coverage)
            .fingerprint(AnalysisFingerprint::default())
            .build()
    }

    async fn round_trip(backend: StorageBackend) {
        let pools = pools::build().expect("valid app pool policy");
        let store = AppStore::builder(pools.clone()).backend(backend).build();
        let resource = AppResourceConfig::for_src(
            ResourceSrc::parse("https://analysis.test.invalid/persistence.mp3")
                .expect("fixture source is valid"),
        )
        .store(store.clone())
        .discriminator("persistence-test")
        .build();
        let target = AnalysisTarget::for_config(&resource).expect("fixture target is valid");
        let worker = Worker::new(WorkerConfig::new().with_runtime(Handle::current()));
        let persistence = AnalysisPersistence::new(AnalysisPersistenceConfig::new(
            worker,
            pools.clone(),
            NonZeroUsize::MIN,
            Duration::from_secs(Consts::CHUNK_FRAMES),
            DispatcherConfig::builder()
                .name("analysis-persistence-test")
                .build(),
            TaskConfig::new(),
        ))
        .expect("persistence actor starts");
        let first = AnalysisProgress::try_from(analysis(
            1,
            &[
                (0, Consts::CHUNK_FRAMES),
                (2 * Consts::CHUNK_FRAMES, Consts::CHUNK_FRAMES),
            ],
        ))
        .expect("settled progress is persistable");
        let second = AnalysisProgress::try_from(analysis(2, &[(0, 3 * Consts::CHUNK_FRAMES)]))
            .expect("settled progress is persistable");

        persistence
            .store(target.clone(), first.clone())
            .await
            .expect("first generation commits");
        persistence
            .store(target.clone(), first)
            .await
            .expect("equal revision is idempotent");
        persistence
            .store(target.clone(), second)
            .await
            .expect("replacement generation commits");

        let reader = store
            .open_resource(target.key(), None)
            .expect("committed analysis opens");
        let mut bytes = pools.get::<u8>();
        reader
            .read_into(&mut bytes)
            .expect("committed analysis reads");
        let file = AnalysisFile::parse(&bytes, &AnalysisFingerprint::default())
            .expect("replacement prefix and payload form a valid file");
        assert_eq!(file.latest().analysis().revision(), 2);
        assert_eq!(
            file.latest().analysis().coverage().frames(),
            3 * Consts::CHUNK_FRAMES
        );
    }

    #[kithara::test(native, tokio)]
    async fn ordered_replacement_round_trips_on_memory_and_disk() {
        round_trip(StorageBackend::Memory).await;
        let directory = tempfile::tempdir().expect("temporary disk store");
        round_trip(StorageBackend::Disk {
            root: directory.path().into(),
        })
        .await;
    }
}
