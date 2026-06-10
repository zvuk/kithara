use std::sync::mpsc;

use kithara_platform::{CancellationToken, thread, tokio::sync::watch};
use tracing::warn;

use super::analyzer::{AnalyzerRegistry, TrackAnalysis};
use super::run::analyze_reader;
use crate::traits::PcmReader;

/// Long-lived analysis thread: decodes each queued track once and feeds
/// every analyzer registered at construction.
///
/// Jobs are gated by caller-owned cancel tokens that must be children of
/// the same scope that owns this worker, so preemption and shutdown stay
/// inside one cancel hierarchy. The caller keeps at most one job in
/// flight and cancels the previous token before queueing the next.
pub struct AnalysisWorker {
    jobs: mpsc::Sender<Job>,
    cancel: CancellationToken,
}

struct Job {
    reader: Box<dyn PcmReader>,
    cancel: CancellationToken,
    tx: watch::Sender<Option<TrackAnalysis>>,
}

impl AnalysisWorker {
    /// `parent` must be a child of the consumer-crate master cancel; the
    /// worker thread stops on parent cancel or drop.
    #[must_use]
    pub fn new(parent: &CancellationToken, registry: AnalyzerRegistry) -> Self {
        let cancel = parent.child_token();
        let thread_cancel = cancel.clone();
        let (jobs, rx) = mpsc::channel();
        thread::spawn_named("kithara-analysis", move || {
            run_jobs(&rx, &registry, &thread_cancel);
        });
        Self { jobs, cancel }
    }

    /// Queue one opened track. On success the result arrives on the
    /// returned receiver; on failure or cancel the sender drops without a
    /// value (`changed()` errs). Cancel `cancel` to preempt the job.
    pub fn analyze(
        &self,
        reader: Box<dyn PcmReader>,
        cancel: CancellationToken,
    ) -> watch::Receiver<Option<TrackAnalysis>> {
        let (tx, rx) = watch::channel(None);
        if self.jobs.send(Job { reader, cancel, tx }).is_err() {
            warn!("analysis worker stopped; job dropped");
        }
        rx
    }
}

impl Drop for AnalysisWorker {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}

fn run_jobs(jobs: &mpsc::Receiver<Job>, registry: &AnalyzerRegistry, cancel: &CancellationToken) {
    while let Ok(mut job) = jobs.recv() {
        if cancel.is_cancelled() {
            break;
        }
        if job.cancel.is_cancelled() {
            continue;
        }
        if let Some(out) = analyze_reader(job.reader.as_mut(), registry, &job.cancel) {
            let _ = job.tx.send(Some(out));
        }
    }
}

#[cfg(all(test, feature = "analysis"))]
mod tests {
    use kithara_test_utils::kithara;

    use super::super::analyzer::waveform_analyzer;
    use super::super::fake::{FakeReader, sine};
    use super::*;

    fn registry() -> AnalyzerRegistry {
        let mut registry = AnalyzerRegistry::default();
        registry.register(waveform_analyzer(16).expect("analysis feature is on in tests"));
        registry
    }

    #[kithara::test(tokio)]
    async fn delivers_result_on_its_own_thread() {
        let master = CancellationToken::default();
        let worker = AnalysisWorker::new(&master, registry());
        let mut rx = worker.analyze(
            Box::new(FakeReader::chunked(&sine(8192), 3)),
            master.child_token(),
        );
        rx.changed().await.expect("worker sends a result");
        assert!(rx.borrow().as_ref().is_some_and(|a| a.waveform.is_some()));
    }

    #[kithara::test(tokio)]
    async fn preempted_job_sends_nothing_and_next_job_runs() {
        let master = CancellationToken::default();
        let worker = AnalysisWorker::new(&master, registry());

        let stale = master.child_token();
        stale.cancel();
        let mut stale_rx = worker.analyze(Box::new(FakeReader::chunked(&sine(8192), 3)), stale);

        let mut live_rx = worker.analyze(
            Box::new(FakeReader::chunked(&sine(8192), 3)),
            master.child_token(),
        );
        live_rx.changed().await.expect("live job completes");
        assert!(live_rx.borrow().is_some());
        assert!(
            stale_rx.changed().await.is_err(),
            "preempted job must drop its sender without a result"
        );
        assert!(stale_rx.borrow().is_none());
    }
}
