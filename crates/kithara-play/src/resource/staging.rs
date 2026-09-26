use kithara_audio::{ReadOutcome, ResamplerBackend};
use kithara_bufpool::HasPool;
use kithara_decode::DecodeError;
use kithara_events::TrackId;
use kithara_platform::{
    CancelToken,
    maybe_send::{BoxFuture, MaybeSendFuture},
    sync::Arc,
    tokio::{runtime::Handle, sync::oneshot},
};
use kithara_signal::SessionEpoch;
use kithara_sync::{ArmPermit, LoadGeneration, StagePort, SyncExecutionReject, SyncGateBinding};
use kithara_warp::{MapAxis, WarpCursor, WarpMapRevision, WarpPlan, supports_playback_rate};
use kithara_worker::TaskError;
use tracing::warn;

use super::{Resource, ResourceConfig, SourceType};
use crate::{
    PlayWorker, TrackConfig,
    bridge::sync::{PreparedFirst, SyncTicket},
    rt::track::PlayerResource,
    worker::{Readiness, ReadinessProbe, StagedSlot},
};

/// One staged lane to open: the plan it enters, the cancel it answers to,
/// and the probe that proves its prepared PCM.
pub(crate) struct StageRequest {
    pub(crate) plan: WarpPlan,
    pub(crate) cancel: CancelToken,
    pub(crate) probe: ReadinessProbe,
    pub(crate) verdict: oneshot::Receiver<Readiness>,
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
    /// The entered map or its first decoded source interval is inconsistent.
    Geometry,
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
    resource: Box<PlayerResource>,
    first: PreparedFirst,
    activation: WarpCursor,
    epoch: SessionEpoch,
    output_rate: std::num::NonZeroU32,
}

type OpenStaged =
    dyn Fn(StageRequest) -> BoxFuture<'static, Result<StagedLane, StagingError>> + Send + Sync;
type Handoff = dyn Fn(SyncTicket) -> Result<(), SyncExecutionReject> + Send + Sync;

/// How to open another lane of the recording a resource plays, with the
/// resource's own source settings, cache, and worker, entering a plan.
#[derive(Clone, fieldwork::Fieldwork)]
#[fieldwork(opt_in, get)]
pub(crate) struct StagingRecipe {
    /// Runtime the opening and its receipts run on.
    #[field(get, vis = "pub(crate)")]
    handle: Handle,
    open: Arc<OpenStaged>,
    handoff: Option<Arc<Handoff>>,
    gate: Option<SyncGateBinding>,
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
        Some(Self {
            handle,
            open,
            handoff: None,
            gate: None,
        })
    }

    /// Bind this load's exact slot and Host gate before the executor sees it.
    pub(crate) fn with_handoff(
        mut self,
        gate: SyncGateBinding,
        handoff: impl Fn(SyncTicket) -> Result<(), SyncExecutionReject> + Send + Sync + 'static,
    ) -> Self {
        self.gate = Some(gate);
        self.handoff = Some(Arc::new(handoff));
        self
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
                verdict,
            };
            match self.open(request).await {
                Ok(lane) => Ok(lane),
                Err(StagingError::Capacity) => Err(SyncExecutionReject::Capacity),
                Err(StagingError::Cancelled) => Err(SyncExecutionReject::Cancelled),
                Err(StagingError::Geometry) => Err(SyncExecutionReject::Geometry),
                Err(StagingError::Media(error)) => {
                    warn!(%error, "sync: the staged lane could not be opened");
                    Err(SyncExecutionReject::Media)
                }
            }
        }
    }

    fn handoff(
        self,
        media: Self::Media,
        lane: Self::Lane,
        cancel: CancelToken,
        permit: ArmPermit,
    ) -> Result<(), SyncExecutionReject> {
        let Some(gate) = self.gate else {
            cancel.cancel();
            return Err(SyncExecutionReject::Cancelled);
        };
        let Some(handoff) = self.handoff else {
            cancel.cancel();
            return Err(SyncExecutionReject::Cancelled);
        };
        let (item_id, load) = media;
        if permit.stamp().load() != load {
            cancel.cancel();
            return Err(SyncExecutionReject::Cancelled);
        }
        handoff(SyncTicket {
            item_id,
            load,
            resource: lane.resource,
            first: lane.first,
            permit,
            gate,
            activation: lane.activation.output(),
            source_start: lane.activation.source(),
            epoch: lane.epoch,
            output_rate: lane.output_rate,
            map: lane.activation.revision(),
        })
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
        verdict,
    } = request;
    let activation = plan.activation();
    let (epoch, output_rate) = match plan.output_axis() {
        Some(MapAxis::Session(axis)) => (axis.epoch(), axis.sample_rate()),
        _ => return Err(StagingError::Geometry),
    };
    let source_rate = match plan.source_axis() {
        Some(MapAxis::Asset(axis)) => axis.sample_rate(),
        _ => return Err(StagingError::Geometry),
    };
    let slot: StagedSlot = worker.reserve_staged(cancel.clone())?;
    let warp = config.warp.entering(Arc::new(plan));
    let stretch = Arc::clone(warp.stretch());
    config.cancel = Some(cancel.clone());
    config.bus = None;
    let src: Arc<str> = Arc::from(config.src.to_string());
    let mut resource = match SourceType::detect(&config.src)? {
        SourceType::RemoteFile(_) | SourceType::LocalFile(_) => {
            let audio = config.build_file_config(&worker, None);
            let track = TrackConfig::for_audio(audio).warp(warp).build();
            let mut reader = worker.open_staged(track, slot, probe).await?;
            let publisher = reader.take_publisher().ok_or(StagingError::Geometry)?;
            let priority = reader.priority();
            Resource::from_staged_reader(
                reader,
                Arc::clone(&src),
                publisher,
                priority,
                cancel.clone(),
                Arc::clone(&stretch),
            )
        }
        SourceType::HlsStream(_) => {
            let audio = config.build_hls_config(&worker, None)?;
            let track = TrackConfig::for_audio(audio).warp(warp).build();
            let mut reader = worker.open_staged(track, slot, probe).await?;
            let publisher = reader.take_publisher().ok_or(StagingError::Geometry)?;
            let priority = reader.priority();
            Resource::from_staged_reader(
                reader,
                Arc::clone(&src),
                publisher,
                priority,
                cancel.clone(),
                Arc::clone(&stretch),
            )
        }
    };
    match verdict.await {
        Ok(Readiness::Ready) => {}
        Ok(Readiness::Failed) => return Err(StagingError::Geometry),
        Err(_) => return Err(StagingError::Cancelled),
    }
    let first = prepare_first(&mut resource, activation, output_rate, source_rate)?;
    let resource =
        PlayerResource::new(resource, src, worker.pools()).map_err(|_| StagingError::Capacity)?;
    Ok(StagedLane {
        resource: Box::new(resource),
        first,
        activation,
        epoch,
        output_rate,
    })
}

fn prepare_first(
    resource: &mut Resource,
    activation: WarpCursor,
    output_rate: std::num::NonZeroU32,
    source_rate: std::num::NonZeroU32,
) -> Result<PreparedFirst, StagingError> {
    if resource.spec().sample_rate != output_rate {
        return Err(StagingError::Geometry);
    }
    let mut left = [0.0];
    let mut right = [0.0];
    let outcome = {
        let mut planes = [&mut left[..], &mut right[..]];
        resource.read_planar(&mut planes)?
    };
    let ReadOutcome::Frames {
        count,
        source_span: Some(source),
        ..
    } = outcome
    else {
        return Err(StagingError::Geometry);
    };
    let count = count.get();
    if count != 1
        || source.output_frames() != 1
        || source.start() != activation.source()
        || source.sample_rate() != source_rate
        || source.mapping_revision().map(WarpMapRevision::from) != Some(activation.revision())
    {
        return Err(StagingError::Geometry);
    }
    Ok(PreparedFirst {
        stereo: [left[0], right[0]],
        source,
    })
}
