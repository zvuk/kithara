pub use kithara::audio::analysis::TrackAnalysis;
use kithara::{
    audio::{
        PcmReader,
        analysis::{AnalysisWorker, AnalyzerBuilder, BeatAnalysisConfig},
    },
    bufpool::PcmPool,
    prelude::{PlaybackResamplerBackend, Resource, ResourceConfig},
};
use kithara_platform::{
    CancelToken,
    sync::Arc,
    tokio::{
        sync::watch,
        task::{self, JoinHandle},
    },
};
use tracing::warn;

type AppAnalysisWorker = AnalysisWorker<PlaybackResamplerBackend>;
type AppBeatAnalysisConfig = BeatAnalysisConfig<PlaybackResamplerBackend>;
type AppResourceConfig = ResourceConfig<PlaybackResamplerBackend>;

/// App-side handle over the shared [`AnalysisWorker`]: opens the resource
/// off the player runtime, hands the opened reader to the worker thread,
/// and keeps at most one run in flight. Dropping it cancels the run and
/// stops the worker.
#[derive(fieldwork::Fieldwork)]
#[fieldwork(opt_in, get)]
pub struct TrackAnalysisRunner {
    worker: Arc<AppAnalysisWorker>,
    current: Option<RunHandle>,
    /// Whether any analyzer is compiled in; without one a decode pass would
    /// produce nothing, so the driver skips analysis entirely.
    /// `false` when no analyzer is configured (`builder.is_empty()`) — the
    /// runtime signal to skip analysis scheduling.
    #[field(get = is_active)]
    active: bool,
}

/// An in-flight run: its child token and the spawned open/forward task.
/// Teardown is cooperative — cancelling the token exits the worker's decode
/// loop at its next per-chunk check.
struct RunHandle {
    cancel: CancelToken,
    task: JoinHandle<()>,
}

impl TrackAnalysisRunner {
    /// `master` must be a child of the app master cancel; the worker thread
    /// and every run scope live under it. `buckets` caps the waveform output;
    /// the native window count is the real resolution.
    #[must_use]
    pub fn new(master: &CancelToken, _buckets: usize, beat_config: AppBeatAnalysisConfig) -> Self {
        let builder = AnalyzerBuilder::default()
            .with_pcm_pool(PcmPool::default())
            .with_beat_config(beat_config);
        #[cfg(feature = "analysis-waveform")]
        let builder = builder.with_waveform(_buckets);
        let builder = builder.with_beat();
        let active = !builder.is_empty();
        let worker = Arc::new(AnalysisWorker::new(master, builder));
        Self {
            worker,
            active,
            current: None,
        }
    }

    /// Cancel any prior run and queue `config` for analysis.
    /// Staged results arrive on the returned receiver,
    /// which closes when the run ends; nothing arrives on failure/cancel.
    pub fn analyze(&mut self, config: AppResourceConfig) -> watch::Receiver<Option<TrackAnalysis>> {
        self.clear();

        let run = self.worker.child_token();
        let (tx, rx) = watch::channel(None);
        let task = task::spawn(run_analysis(
            Arc::clone(&self.worker),
            config,
            run.clone(),
            tx,
        ));
        self.current = Some(RunHandle { cancel: run, task });
        rx
    }

    /// Cancel the in-flight run.
    pub fn clear(&mut self) {
        if let Some(prev) = self.current.take() {
            prev.cancel.cancel();
            prev.task.abort();
        }
    }
}

impl Drop for TrackAnalysisRunner {
    fn drop(&mut self) {
        self.clear();
    }
}

/// Open `config` and run it through the shared worker, forwarding every
/// staged update to `tx`.
async fn run_analysis(
    worker: Arc<AppAnalysisWorker>,
    config: AppResourceConfig,
    cancel: CancelToken,
    tx: watch::Sender<Option<TrackAnalysis>>,
) {
    let Some(reader) = open_reader(config, &cancel).await else {
        return;
    };
    let mut rx = worker.analyze(reader, cancel);

    while rx.changed().await.is_ok() {
        let analysis = rx.borrow().clone();
        if let Some(analysis) = analysis {
            // The receiver may be gone (deck swapped).
            let _ = tx.send(Some(analysis));
        }
    }
}

/// Open the resource under the run's cancel scope (so preemption and app
/// shutdown tear the standalone audio worker down top-down) and unwrap the
/// reader for the analysis worker.
async fn open_reader(
    mut config: AppResourceConfig,
    cancel: &CancelToken,
) -> Option<Box<dyn PcmReader>> {
    if cancel.is_cancelled() {
        return None;
    }
    config.cancel = Some(cancel.child());
    let mut resource = match Resource::new(config).await {
        Ok(r) => r,
        Err(e) => {
            warn!(?e, "analysis: resource open failed");
            return None;
        }
    };
    if let Err(e) = resource.preload().await {
        warn!(?e, "analysis: preload failed");
        return None;
    }
    Some(resource.into())
}
