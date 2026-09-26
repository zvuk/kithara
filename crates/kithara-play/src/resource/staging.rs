use kithara_audio::{AudioReader, ResamplerBackend};
use kithara_bufpool::HasPool;
use kithara_decode::DecodeError;
use kithara_events::TrackId;
use kithara_platform::{
    CancelToken,
    maybe_send::{BoxFuture, MaybeSendFuture},
    sync::Arc,
    tokio::runtime::Handle,
};
use kithara_sync::{LoadGeneration, StagePort, SyncExecutionReject};
use kithara_warp::{WarpPlan, supports_playback_rate};
use kithara_worker::TaskError;
use tracing::warn;

use super::{ResourceConfig, SourceType};
use crate::{
    PlayWorker, TrackConfig,
    worker::{Readiness, ReadinessProbe, StagedSlot},
};

/// One staged lane to open: the plan it enters, the cancel it answers to,
/// and the probe that proves its prepared PCM.
pub(crate) struct StageRequest {
    pub(crate) plan: WarpPlan,
    pub(crate) cancel: CancelToken,
    pub(crate) probe: ReadinessProbe,
}

/// Why a staged lane was not opened.
#[derive(Debug)]
pub(crate) enum StagingError {
    /// The worker holds no slot for another lane; nothing was opened.
    Capacity,
    /// The lane's cancel fired or its worker stopped before it started.
    Cancelled,
    /// The recording could not be opened or positioned for the plan.
    Media(DecodeError),
}

impl From<TaskError> for StagingError {
    fn from(error: TaskError) -> Self {
        match error {
            TaskError::Capacity { .. } => Self::Capacity,
            _ => Self::Cancelled,
        }
    }
}

impl From<DecodeError> for StagingError {
    fn from(error: DecodeError) -> Self {
        Self::Media(error)
    }
}

/// An opened staged lane: its reader keeps the worker lease and the ring of
/// prepared PCM until the lane is dropped.
pub(crate) struct StagedLane {
    _reader: Box<dyn AudioReader>,
}

type OpenStaged =
    dyn Fn(StageRequest) -> BoxFuture<'static, Result<StagedLane, StagingError>> + Send + Sync;

/// How to open another lane of the recording a resource plays, with the
/// resource's own source settings, cache, and worker, entering a plan.
#[derive(Clone, fieldwork::Fieldwork)]
#[fieldwork(opt_in, get)]
pub(crate) struct StagingRecipe {
    /// Runtime the opening and its receipts run on.
    #[field(get, vis = "pub(crate)")]
    handle: Handle,
    open: Arc<OpenStaged>,
}

impl StagingRecipe {
    /// The recipe for a resource opened from `config`, or `None` where no
    /// renderer can enter a plan or no runtime can run the opening.
    pub(super) fn new<S, B>(config: &ResourceConfig<S, B>, worker: &PlayWorker<S>) -> Option<Self>
    where
        B: Default + ResamplerBackend,
        S: HasPool<u8> + HasPool<f32> + Send + Sync + 'static,
    {
        if !supports_playback_rate() {
            return None;
        }
        let handle = Handle::try_current().ok()?;
        let config = config.clone();
        let worker = worker.clone();
        let open: Arc<OpenStaged> =
            Arc::new(move |request| Box::pin(open_staged(config.clone(), worker.clone(), request)));
        Some(Self { handle, open })
    }

    /// Reserves a worker slot, then opens and positions the lane in it.
    pub(crate) fn open(
        &self,
        request: StageRequest,
    ) -> BoxFuture<'static, Result<StagedLane, StagingError>> {
        (self.open)(request)
    }
}

impl StagePort for StagingRecipe {
    type Media = (TrackId, LoadGeneration);
    type Lane = StagedLane;

    fn runtime(&self) -> &Handle {
        self.handle()
    }

    /// Opens the lane, then holds it only once its probe proves the plan's
    /// prepared PCM.
    fn stage(
        self,
        plan: WarpPlan,
        cancel: CancelToken,
    ) -> impl MaybeSendFuture<Output = Result<StagedLane, SyncExecutionReject>> + 'static {
        async move {
            let (probe, verdict) = ReadinessProbe::new(&plan);
            let request = StageRequest {
                plan,
                cancel,
                probe,
            };
            match self.open(request).await {
                Ok(lane) => match verdict.await {
                    Ok(Readiness::Ready) => Ok(lane),
                    Ok(Readiness::Failed) => Err(SyncExecutionReject::Media),
                    Err(_) => Err(SyncExecutionReject::Cancelled),
                },
                Err(StagingError::Capacity) => Err(SyncExecutionReject::Capacity),
                Err(StagingError::Cancelled) => Err(SyncExecutionReject::Cancelled),
                Err(StagingError::Media(error)) => {
                    warn!(%error, "sync: the staged lane could not be opened");
                    Err(SyncExecutionReject::Media)
                }
            }
        }
    }
}

async fn open_staged<S, B>(
    mut config: ResourceConfig<S, B>,
    worker: PlayWorker<S>,
    request: StageRequest,
) -> Result<StagedLane, StagingError>
where
    B: Default + ResamplerBackend,
    S: HasPool<u8> + HasPool<f32> + Send + Sync + 'static,
{
    let StageRequest {
        plan,
        cancel,
        probe,
    } = request;
    let slot: StagedSlot = worker.reserve_staged(cancel.clone())?;
    let warp = config.warp.entering(Arc::new(plan));
    config.cancel = Some(cancel);
    config.bus = None;
    let reader: Box<dyn AudioReader> = match SourceType::detect(&config.src)? {
        SourceType::RemoteFile(_) | SourceType::LocalFile(_) => {
            let audio = config.build_file_config(&worker, None);
            let track = TrackConfig::for_audio(audio).warp(warp).build();
            Box::new(worker.open_staged(track, slot, probe).await?)
        }
        SourceType::HlsStream(_) => {
            let audio = config.build_hls_config(&worker, None)?;
            let track = TrackConfig::for_audio(audio).warp(warp).build();
            Box::new(worker.open_staged(track, slot, probe).await?)
        }
    };
    Ok(StagedLane { _reader: reader })
}
