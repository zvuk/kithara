use kithara_platform::{CancelToken, sync::mpsc, tokio::sync::watch};
use kithara_resampler::ResamplerBackend;
use tracing::warn;

use super::{AnalysisNode, AnalysisObserver, Job};
use crate::{
    PcmReader,
    analysis::analyzer::{AnalyzerBuilder, TrackAnalysis},
    runtime::{Scheduler, SchedulerHandle},
};

const ANALYSIS_NODE_ID: u64 = 1;

pub struct AnalysisWorker<B>
where
    B: ResamplerBackend,
{
    job_scope: JobScope,
    jobs: mpsc::Sender<Job>,
    scheduler: SchedulerHandle<AnalysisNode<B>>,
}

struct JobScope(CancelToken);

impl JobScope {
    fn child(&self) -> CancelToken {
        self.0.child()
    }
}

impl<B> AnalysisWorker<B>
where
    B: ResamplerBackend,
{
    #[must_use]
    pub fn new(parent: &CancelToken, builder: AnalyzerBuilder<B>) -> Self {
        let cancel = parent.child();
        let job_scope = JobScope(cancel.child());
        let (jobs, receiver) = mpsc::channel();
        let node = AnalysisNode::new(builder, receiver);
        let scheduler = Scheduler::<AnalysisNode<B>, AnalysisObserver>::start(
            "kithara-analysis".into(),
            AnalysisObserver::default(),
            cancel,
        );
        scheduler.register(ANALYSIS_NODE_ID, node);
        Self {
            job_scope,
            jobs,
            scheduler,
        }
    }

    pub fn analyze(
        &self,
        reader: Box<dyn PcmReader>,
        cancel: CancelToken,
    ) -> watch::Receiver<Option<TrackAnalysis>> {
        let (tx, rx) = watch::channel(None);
        if self.jobs.send(Job { cancel, reader, tx }).is_err() {
            warn!("analysis worker stopped; job dropped");
        } else {
            self.scheduler.wake();
        }
        rx
    }

    #[must_use]
    pub fn child_token(&self) -> CancelToken {
        self.job_scope.child()
    }
}

impl<B> Drop for AnalysisWorker<B>
where
    B: ResamplerBackend,
{
    fn drop(&mut self) {
        self.scheduler.shutdown();
    }
}
