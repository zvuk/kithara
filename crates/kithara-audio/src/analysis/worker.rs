use std::sync::mpsc;

use kithara_platform::{CancellationToken, thread, tokio::sync::watch};
use tracing::warn;

use super::{
    analyzer::{AnalyzerBuilder, TrackAnalysis},
    run::analyze_reader,
};
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
    pub fn new(parent: &CancellationToken, builder: AnalyzerBuilder) -> Self {
        let cancel = parent.child_token();
        let thread_cancel = cancel.clone();
        let (jobs, rx) = mpsc::channel();
        thread::spawn_named("kithara-analysis", move || {
            run_jobs(&rx, &builder, &thread_cancel);
        });
        Self { jobs, cancel }
    }

    /// Create a job-scoped token owned by this worker.
    #[must_use]
    pub fn child_token(&self) -> CancellationToken {
        self.cancel.child_token()
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

fn run_jobs(jobs: &mpsc::Receiver<Job>, builder: &AnalyzerBuilder, cancel: &CancellationToken) {
    while let Ok(mut job) = jobs.recv() {
        if cancel.is_cancelled() {
            break;
        }
        if job.cancel.is_cancelled() {
            continue;
        }
        analyze_reader(job.reader.as_mut(), builder, &job.cancel, |a| {
            let _ = job.tx.send(Some(a));
        });
    }
}
