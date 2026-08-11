use kithara_platform::{
    CancelToken,
    sync::{Arc, mpsc},
    tokio::sync::watch,
};
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
    scheduler: SchedulerHandle<AnalysisNode<B>>,
    jobs: mpsc::Sender<Job>,
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
            scheduler,
            jobs,
        }
    }

    pub fn analyze(
        &self,
        reader: Box<dyn PcmReader>,
        cancel: CancelToken,
        track: Arc<str>,
    ) -> watch::Receiver<Option<TrackAnalysis>> {
        let (tx, rx) = watch::channel(None);
        match self.jobs.send(Job {
            reader,
            cancel,
            tx,
            track,
        }) {
            Ok(()) => self.scheduler.wake(),
            // The channel hands the job back, so the track it was for is still
            // there to name.
            Err(returned) => {
                warn!(track = %returned.0.track, "analysis worker stopped; job dropped");
            }
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
