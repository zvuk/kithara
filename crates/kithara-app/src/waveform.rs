use std::sync::Arc;

pub use kithara::audio::analysis::TrackAnalysis;
use kithara::{
    audio::{
        PcmReader,
        analysis::{AnalysisWorker, AnalyzerBuilder},
    },
    prelude::{Resource, ResourceConfig},
};
use kithara_platform::{
    CancelToken,
    tokio::{
        sync::watch,
        task::{self, JoinHandle},
    },
};
use tracing::warn;

/// App-side handle over the shared [`AnalysisWorker`]: opens the resource
/// off the player runtime, hands the opened reader to the worker thread,
/// and keeps at most one run in flight. Dropping it cancels the run and
/// stops the worker.
pub struct TrackAnalysisRunner {
    worker: Arc<AnalysisWorker>,
    cancel: CancelToken,
    current: Option<RunHandle>,
    /// Whether any analyzer is compiled in; without one a decode pass would
    /// produce nothing, so the driver skips analysis entirely.
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
    /// and every run scope live under it. `buckets` is the waveform
    /// resolution registered with the worker.
    #[must_use]
    pub fn new(master: &CancelToken, buckets: usize) -> Self {
        let cancel = master.child();
        let builder = AnalyzerBuilder::default()
            .with_waveform(buckets)
            .with_beat();
        let active = !builder.is_empty();
        let worker = Arc::new(AnalysisWorker::new(&cancel, builder));
        Self {
            cancel,
            worker,
            active,
            current: None,
        }
    }

    /// Cancel any prior run and queue `config` for analysis. The latest
    /// result (or none on failure/cancel) arrives on the returned receiver.
    ///
    /// Two watch channels bridge here on purpose: this receiver must exist
    /// before the async `Resource` open completes, while the worker hands
    /// out its own channel only once the opened reader is queued.
    pub fn analyze(&mut self, config: ResourceConfig) -> watch::Receiver<Option<TrackAnalysis>> {
        self.clear();

        let run = self.cancel.child();
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

    /// Cancel the in-flight run, if any, without starting a new one. The
    /// worker skips or aborts the job at its next cancel check.
    pub fn clear(&mut self) {
        if let Some(prev) = self.current.take() {
            prev.cancel.cancel();
            prev.task.abort();
        }
    }

    /// `false` when no analyzer is configured (`builder.is_empty()`) — the
    /// runtime signal to skip analysis scheduling.
    #[must_use]
    pub fn is_active(&self) -> bool {
        self.active
    }
}

impl Drop for TrackAnalysisRunner {
    fn drop(&mut self) {
        self.clear();
        self.cancel.cancel();
    }
}

/// Open `config` and run it through the shared worker, forwarding the
/// result to `tx`. Nothing is sent on failure or cancel.
async fn run_analysis(
    worker: Arc<AnalysisWorker>,
    config: ResourceConfig,
    cancel: CancelToken,
    tx: watch::Sender<Option<TrackAnalysis>>,
) {
    let Some(reader) = open_reader(config, &cancel).await else {
        return;
    };
    let mut rx = worker.analyze(reader, cancel);
    if rx.changed().await.is_err() {
        return;
    }
    let analysis = rx.borrow().clone();
    if let Some(analysis) = analysis {
        // The receiver may be gone (deck swapped); a failed send is fine.
        let _ = tx.send(Some(analysis));
    }
}

/// Open the resource under the run's cancel scope (so preemption and app
/// shutdown tear the standalone audio worker down top-down) and unwrap the
/// reader for the analysis worker.
async fn open_reader(
    mut config: ResourceConfig,
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
