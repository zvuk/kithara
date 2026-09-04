use std::num::{NonZeroU32, NonZeroU64};

use kithara_audio::AudioReader;
use kithara_bufpool::HasPool;
use kithara_platform::{
    CancelGroup, CancelScope, CancelToken,
    sync::{Mutex, mpsc},
    tokio::sync::watch,
};
use kithara_resampler::ResamplerBackend;
use kithara_worker::{
    Dispatcher, DispatcherConfig, RayonConfig, TaskConfig, TaskError, TaskHandle, Worker,
    WorkerConfig,
};
use tracing::warn;

use super::{AnalysisNode, AnalysisObserver, AnalysisWorkerConfig, Job};
use crate::{
    AnalysisFileError, AnalysisProgress,
    analyzer::{AnalysisFingerprint, AnalysisToken},
    producer::{AnalysisProducer, ring},
    worker::schedule::extent_frames,
};

pub struct AnalysisWorker {
    resume_shape: (bool, bool),
    fingerprint: AnalysisFingerprint,
    dispatcher: Dispatcher,
    chunk_seconds: NonZeroU32,
    scope: CancelScope,
    start_job: StartJob,
    tasks: Mutex<Vec<ActiveTask>>,
    _base: Worker,
    active: bool,
}

type StartJob =
    Box<dyn Fn(Job, mpsc::Sender<()>) -> Result<TaskHandle, TaskError> + Send + Sync + 'static>;

struct ActiveTask {
    completion: mpsc::Receiver<()>,
    _handle: TaskHandle,
}

/// An analysis pass opened before either decoder starts.
///
/// Opening creates the bounded producer transport synchronously. The app can
/// therefore attach the producer to playback before it asynchronously opens
/// the pass's fallback reader, then hand this value back to [`AnalysisWorker::start`].
pub struct AnalysisPass {
    token: AnalysisToken,
    revision: u64,
    cancel: CancelToken,
    rate: NonZeroU32,
    resume: Option<AnalysisProgress>,
    ingest: ring::Reader,
    tx: watch::Sender<Option<AnalysisProgress>>,
}

/// Output of opening an analysis pass before its fallback reader starts.
pub type AnalysisOpen = (
    watch::Receiver<Option<AnalysisProgress>>,
    AnalysisProducer,
    AnalysisPass,
);

impl AnalysisPass {
    /// Returns the job-scoped cancellation token for opening this pass's reader.
    #[must_use]
    pub const fn cancel_token(&self) -> &CancelToken {
        &self.cancel
    }
}

impl AnalysisWorker {
    /// Construct the analysis dispatcher used by independently admitted jobs.
    ///
    pub fn new<B, S>(config: AnalysisWorkerConfig<B, S>) -> Self
    where
        B: ResamplerBackend,
        S: HasPool<f32> + Send + Sync + 'static,
    {
        let AnalysisWorkerConfig {
            mut builder,
            cancel,
            capacity,
            chunk_seconds,
            fairness_yield_interval,
            idle_timeout,
            max_compute_tasks,
            priority,
            producer_drain_limit,
            publish_seconds,
            slow_tick_threshold,
            task_burst,
            wait_timeout,
            worker,
        } = config;
        let scope = CancelScope::new(cancel.clone());
        let (base, dispatcher_cancel) = if let Some(worker) = worker {
            (worker, cancel.clone().map(CancelGroup::from))
        } else {
            let worker_config = cancel
                .map_or_else(WorkerConfig::new, |cancel| {
                    WorkerConfig::new().with_cancel(cancel)
                })
                .with_max_compute_tasks(max_compute_tasks)
                .with_owned_pool(RayonConfig::new(
                    max_compute_tasks,
                    "kithara-analysis-compute",
                ));
            (Worker::new(worker_config), None)
        };
        let dispatcher_config = DispatcherConfig::builder()
            .name("kithara-analysis")
            .capacity(capacity)
            .fairness_yield_interval(fairness_yield_interval)
            .idle_timeout(idle_timeout)
            .observer(AnalysisObserver::default())
            .slow_tick_threshold(slow_tick_threshold)
            .task_burst(task_burst)
            .wait_timeout(wait_timeout)
            .maybe_cancel(dispatcher_cancel)
            .build();
        let dispatcher = base.dispatcher(dispatcher_config);
        let _ = builder.take_detector();
        let fingerprint = builder.fingerprint();
        let active = !builder.is_empty();
        let resume_shape = builder.resume_shape();
        let task_config = TaskConfig::new()
            .with_max_compute_tasks(max_compute_tasks)
            .with_priority(priority);
        let job_dispatcher = dispatcher.clone();
        let start_job: StartJob = Box::new(move |job, completion| {
            let pending = job_dispatcher.reserve(task_config.clone())?;
            let (jobs, receiver) = mpsc::channel();
            if jobs.send(job).is_err() {
                return Err(TaskError::Stopped);
            }
            let job_builder = builder.clone();
            pending.start(move |context| {
                AnalysisNode::with_completion(
                    job_builder,
                    receiver,
                    context,
                    chunk_seconds,
                    producer_drain_limit,
                    publish_seconds,
                    Some(completion),
                )
            })
        });

        Self {
            active,
            chunk_seconds,
            dispatcher,
            fingerprint,
            resume_shape,
            scope,
            start_job,
            tasks: Mutex::new(Vec::new()),
            _base: base,
        }
    }

    /// Open a pass on `rate`, the axis its ranges are measured on; a chunk on
    /// another axis is refused. Returns where its snapshots arrive and the
    /// producer another component may contribute decoded ranges through.
    #[must_use]
    pub fn analyze(
        &self,
        reader: Box<dyn AudioReader>,
        token: AnalysisToken,
        rate: NonZeroU32,
        revision: u64,
    ) -> (watch::Receiver<Option<AnalysisProgress>>, AnalysisProducer) {
        let (rx, producer, pass) = self.open(token, rate, revision);
        self.start(pass, reader);
        (rx, producer)
    }

    /// Identity of the analyzers that survived worker initialization.
    #[must_use]
    pub const fn fingerprint(&self) -> &AnalysisFingerprint {
        &self.fingerprint
    }

    /// Whether detector initialization left at least one effective analyzer.
    #[must_use]
    pub const fn is_active(&self) -> bool {
        self.active
    }

    /// Open a pass and its bounded playback producer without waiting for the
    /// fallback reader to open or preload. `revision` is the one the caller
    /// already holds for `token`; every publication outranks it.
    #[must_use]
    pub fn open(
        &self,
        token: AnalysisToken,
        rate: NonZeroU32,
        revision: u64,
    ) -> (
        watch::Receiver<Option<AnalysisProgress>>,
        AnalysisProducer,
        AnalysisPass,
    ) {
        let (tx, rx) = watch::channel(None);
        let (writer, ingest) = ring::open_for(rate);
        let producer = AnalysisProducer::new(writer, rate, token.clone());
        let pass = AnalysisPass {
            cancel: self.scope.token().child(),
            ingest,
            rate,
            token,
            revision,
            tx,
            resume: None,
        };
        (rx, producer, pass)
    }

    /// Open a validated partial publication before its fallback reader is
    /// available, returning the producer synchronously for playback ingress.
    ///
    /// # Errors
    ///
    /// Rejects a settled or malformed checkpoint, analyzer/config drift, an
    /// unknown source extent, and a different configured chunk size.
    pub fn open_resume(
        &self,
        progress: AnalysisProgress,
    ) -> Result<AnalysisOpen, AnalysisFileError> {
        progress.validate_resume()?;
        let analysis = progress.analysis();
        let rate = analysis.source_sample_rate();
        let extent = analysis.extent().ok_or(AnalysisFileError::UnknownExtent)?;
        let (chunk_frames, shape) = progress.resume_meta().ok_or(AnalysisFileError::Config)?;
        let expected_chunk = NonZeroU64::new(
            u64::from(rate.get()).saturating_mul(u64::from(self.chunk_seconds.get())),
        )
        .ok_or(AnalysisFileError::Config)?;
        if analysis.is_settled()
            || analysis.fingerprint() != &self.fingerprint
            || chunk_frames != expected_chunk
            || shape != self.resume_shape
            || analysis
                .coverage()
                .runs()
                .iter()
                .any(|range| range.end() > extent)
        {
            return Err(AnalysisFileError::Config);
        }

        let token = analysis.token().clone();
        let revision = analysis.revision();
        let (tx, rx) = watch::channel(Some(progress.clone()));
        let (writer, ingest) = ring::open_for(rate);
        let producer = AnalysisProducer::new(writer, rate, token.clone());
        let pass = AnalysisPass {
            cancel: self.scope.token().child(),
            ingest,
            rate,
            token,
            revision,
            tx,
            resume: Some(progress),
        };
        Ok((rx, producer, pass))
    }

    /// Start an already-open pass with its fallback reader.
    pub fn start(&self, pass: AnalysisPass, reader: Box<dyn AudioReader>) {
        if pass.resume.is_some() {
            warn!("analysis resume pass requires extent validation");
            return;
        }
        self.submit_pass(pass, reader);
    }

    /// Start a resume pass only after its opened reader confirms the persisted
    /// source extent.
    ///
    /// # Errors
    ///
    /// Rejects a fresh pass, an unknown reader duration, or an extent that no
    /// longer matches the validated checkpoint.
    pub fn start_resume(
        &self,
        pass: AnalysisPass,
        reader: Box<dyn AudioReader>,
    ) -> Result<(), AnalysisFileError> {
        let progress = pass.resume.as_ref().ok_or(AnalysisFileError::Config)?;
        let extent = progress
            .analysis()
            .extent()
            .ok_or(AnalysisFileError::UnknownExtent)?;
        if extent_frames(reader.duration(), pass.rate) != Some(extent) {
            return Err(AnalysisFileError::Config);
        }
        self.submit_pass(pass, reader);
        Ok(())
    }

    fn submit(&self, job: Job) {
        self.tasks.lock().retain(|task| {
            !matches!(
                task.completion.try_recv(),
                Err(mpsc::TryRecvError::Disconnected)
            )
        });
        let (completion, completed) = mpsc::channel();
        match (self.start_job)(job, completion) {
            Ok(handle) => self.tasks.lock().push(ActiveTask {
                completion: completed,
                _handle: handle,
            }),
            Err(error) => {
                warn!(?error, "analysis job was not admitted");
            }
        }
    }

    fn submit_pass(&self, pass: AnalysisPass, reader: Box<dyn AudioReader>) {
        let AnalysisPass {
            cancel,
            ingest,
            rate,
            token,
            revision,
            tx,
            resume,
        } = pass;
        self.submit(Job {
            reader,
            cancel,
            ingest,
            rate,
            token,
            revision,
            tx,
            resume,
        });
    }
}

impl Drop for AnalysisWorker {
    fn drop(&mut self) {
        self.scope.cancel();
        self.dispatcher.shutdown();
    }
}

#[cfg(all(
    test,
    feature = "analysis-beat",
    not(feature = "beat-nn"),
    not(feature = "beat-dsp")
))]
mod tests {
    use kithara_platform::CancelToken;
    use kithara_resampler::NoResamplerBackend;
    use kithara_test_utils::kithara;

    use super::{AnalysisWorker, AnalysisWorkerConfig};
    use crate::{AnalyzerBuilder, test_pools::pools};

    #[kithara::test(native, flash(false))]
    fn beat_without_a_detector_is_not_an_effective_analyzer() {
        let cancel = CancelToken::never();
        let worker = AnalysisWorker::new(
            AnalysisWorkerConfig::for_builder(
                AnalyzerBuilder::<NoResamplerBackend, _>::new(pools()).with_beat(),
            )
            .cancel(cancel)
            .build(),
        );

        assert!(!worker.is_active());
        assert_eq!(worker.fingerprint().beat(), None);
        assert_eq!(worker.fingerprint().waveform(), None);
    }
}
